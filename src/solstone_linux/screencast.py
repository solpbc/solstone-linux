# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""
Portal-based multi-monitor screencast recording.

Uses xdg-desktop-portal ScreenCast API with PipeWire + GStreamer to record
each monitor as a separate file. This replaces the old GNOME Shell D-Bus approach.

Extracted from solstone's observe/linux/screencast.py.

Changes from monorepo version:
- Replaces `from think.utils import get_journal` with config-based restore token path
- Replaces `from observe.gnome.activity import get_monitor_geometries` with local activity module

Runtime deps:
  - xdg-desktop-portal with org.freedesktop.portal.ScreenCast
  - Portal backend: xdg-desktop-portal-gnome (or -kde, -wlr, etc.)
  - PipeWire running
  - GStreamer with PipeWire plugin: gst-launch-1.0 pipewiresrc
"""

import asyncio
import logging
import os
import shutil
import signal
import subprocess
import threading
import uuid
from dataclasses import dataclass
from pathlib import Path

from dbus_next import Variant, introspection
from dbus_next.aio import MessageBus
from dbus_next.constants import BusType
from dbus_next.errors import (
    DBusError,
    InvalidIntrospectionError,
    InvalidMemberNameError,
)

# Workaround for dbus-next issue #122: portal has properties with hyphens
# (e.g., "power-saver-enabled") which violate strict D-Bus naming validation.
introspection.assert_member_name_valid = lambda name: None

logger = logging.getLogger(__name__)

# Portal D-Bus constants
PORTAL_BUS = "org.freedesktop.portal.Desktop"
PORTAL_PATH = "/org/freedesktop/portal/desktop"
SC_IFACE = "org.freedesktop.portal.ScreenCast"
REQ_IFACE = "org.freedesktop.portal.Request"
SESSION_IFACE = "org.freedesktop.portal.Session"

MIN_HEALTHY_WEBM_BYTES = 2048
STDERR_DRAIN_LINE_CAP = 500
STDERR_DRAIN_JOIN_TIMEOUT = 2.0
PORTAL_CALL_TIMEOUT = 30
PORTAL_INTERACTIVE_TIMEOUT = 600


@dataclass
class StreamInfo:
    """Information about a single monitor's recording stream."""

    node_id: int
    position: str
    connector: str
    x: int
    y: int
    width: int
    height: int
    file_path: str  # Final path in segment directory

    @property
    def filename(self) -> str:
        """Return just the filename for event payloads."""
        return os.path.basename(self.file_path)


@dataclass
class SilentStream:
    node_id: int
    connector: str
    position: str
    file_path: Path
    file_bytes: int


def _load_restore_token(token_path: Path) -> str | None:
    """Load restore token from disk."""
    try:
        data = token_path.read_text(encoding="utf-8").strip()
        return data or None
    except (FileNotFoundError, OSError):
        return None


def _save_restore_token(token: str, token_path: Path) -> None:
    """Save restore token to disk."""
    try:
        token_path.parent.mkdir(parents=True, exist_ok=True)
        token_path.write_text(token.strip() + "\n", encoding="utf-8")
        logger.debug(f"Saved restore token to {token_path}")
    except OSError as e:
        logger.warning(f"Failed to save restore token: {e}")


def _make_request_handle(bus: MessageBus, token: str) -> str:
    """Compute expected Request object path for a handle_token."""
    sender = bus.unique_name.lstrip(":").replace(".", "_")
    return f"/org/freedesktop/portal/desktop/request/{sender}/{token}"


def _prepare_request_handler(
    bus: MessageBus, handle: str
) -> tuple[asyncio.Future, object]:
    """Set up signal handler for Request::Response before calling portal method."""
    loop = asyncio.get_running_loop()
    fut: asyncio.Future = loop.create_future()

    def _message_handler(msg):
        if (
            msg.message_type.name == "SIGNAL"
            and msg.path == handle
            and msg.interface == REQ_IFACE
            and msg.member == "Response"
        ):
            response = msg.body[0]
            results = msg.body[1] if len(msg.body) > 1 else {}
            if not fut.done():
                fut.set_result((int(response), results))

    bus.add_message_handler(_message_handler)
    return fut, _message_handler


def _variant_or_value(val):
    """Extract value from Variant if needed."""
    if isinstance(val, Variant):
        return val.value
    return val


def _match_streams_to_monitors(streams: list[dict], monitors: list[dict]) -> list[dict]:
    """
    Match portal stream geometries to monitor info.

    Portal streams have position (x, y) and size (width, height).
    Monitors (from GDK or KScreen) have connector IDs and box coordinates.

    Returns streams augmented with connector and position labels.
    """
    matched = []
    used_position_connectors = set()

    # Detect if all streams lack meaningful position data (KDE portal reports (0,0) for all)
    all_zero_position = True
    for stream in streams:
        props = stream.get("props", {})
        pos = _variant_or_value(props.get("position", (0, 0)))
        if isinstance(pos, (tuple, list)) and len(pos) >= 2:
            if int(pos[0]) != 0 or int(pos[1]) != 0:
                all_zero_position = False
                break

    for stream in streams:
        props = stream.get("props", {})

        # Extract stream geometry from portal properties
        stream_pos = _variant_or_value(props.get("position", (0, 0)))
        stream_size = _variant_or_value(props.get("size", (0, 0)))

        if isinstance(stream_pos, (tuple, list)) and len(stream_pos) >= 2:
            sx, sy = int(stream_pos[0]), int(stream_pos[1])
        else:
            sx, sy = 0, 0

        if isinstance(stream_size, (tuple, list)) and len(stream_size) >= 2:
            sw, sh = int(stream_size[0]), int(stream_size[1])
        else:
            sw, sh = 0, 0

        # Find matching monitor by geometry
        best_match = None
        best_overlap = 0

        if not all_zero_position:
            for monitor in monitors:
                if monitor["id"] in used_position_connectors:
                    continue

                mx1, my1, mx2, my2 = monitor["box"]
                mw, mh = mx2 - mx1, my2 - my1

                # Check if geometries match (within tolerance for scaling)
                if abs(sx - mx1) < 10 and abs(sy - my1) < 10:
                    overlap = min(sw, mw) * min(sh, mh)
                    if overlap > best_overlap:
                        best_overlap = overlap
                        best_match = monitor

        if best_match:
            used_position_connectors.add(best_match["id"])
            stream["connector"] = best_match["id"]
            stream["position_label"] = best_match.get("position", "unknown")
            stream["x"] = best_match["box"][0]
            stream["y"] = best_match["box"][1]
            stream["width"] = best_match["box"][2] - best_match["box"][0]
            stream["height"] = best_match["box"][3] - best_match["box"][1]
        else:
            # Fallback: use stream index as identifier
            stream["connector"] = f"monitor-{stream['idx']}"
            stream["position_label"] = "unknown"
            stream["x"] = sx
            stream["y"] = sy
            stream["width"] = sw
            stream["height"] = sh

        matched.append(stream)

    unmatched_streams = [
        stream
        for stream in matched
        if str(stream.get("connector", "")).startswith("monitor-")
    ]
    matched_connectors = {
        stream["connector"]
        for stream in matched
        if not str(stream.get("connector", "")).startswith("monitor-")
    }
    unmatched_monitors = [
        monitor for monitor in monitors if monitor["id"] not in matched_connectors
    ]

    for stream in unmatched_streams:
        if not unmatched_monitors:
            break

        best_match = None
        sw, sh = stream["width"], stream["height"]
        for monitor in unmatched_monitors:
            mx1, my1, mx2, my2 = monitor["box"]
            mw, mh = mx2 - mx1, my2 - my1
            if abs(sw - mw) <= 2 and abs(sh - mh) <= 2:
                best_match = monitor
                break

        if best_match:
            stream["connector"] = best_match["id"]
            stream["position_label"] = best_match.get("position", "unknown")
            stream["x"] = best_match["box"][0]
            stream["y"] = best_match["box"][1]
            stream["width"] = best_match["box"][2] - best_match["box"][0]
            stream["height"] = best_match["box"][3] - best_match["box"][1]
            unmatched_monitors.remove(best_match)

    return matched


class _StderrDrain:
    """Continuously drain a subprocess stderr pipe.

    A chatty GStreamer pipeline can fill the OS pipe buffer (~64 KB); once
    full, gst blocks on write(2) and stops producing frames while the
    process stays alive — so is_healthy() would stay green while capture
    silently stalls. Draining on a daemon thread (mirroring audio_recorder's
    thread lifecycle) keeps the pipe empty. The thread ends on EOF when the
    process exits; stop() reaps it via join().
    """

    def __init__(self, stderr, tag: str):
        self._stderr = stderr
        self._tag = tag
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def _run(self) -> None:
        try:
            for raw in iter(self._stderr.readline, b""):
                line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
                if not line:
                    continue
                if len(line) > STDERR_DRAIN_LINE_CAP:
                    line = line[:STDERR_DRAIN_LINE_CAP] + "…"
                logger.debug("%s stderr: %s", self._tag, line)
        except (ValueError, OSError):
            # stderr closed underneath us (e.g. during stop()); done.
            pass

    def join(self, timeout: float = STDERR_DRAIN_JOIN_TIMEOUT) -> None:
        self._thread.join(timeout=timeout)


class Screencaster:
    """Portal-based multi-monitor screencast manager."""

    def __init__(self, restore_token_path: Path):
        self.bus: MessageBus | None = None
        self.session_handle: str | None = None
        self.pw_fd: int | None = None
        self.gst_process: subprocess.Popen | None = None
        self.streams: list[StreamInfo] = []
        self._started = False
        self._stderr_drain: _StderrDrain | None = None
        self._restore_token_path = restore_token_path

    def _close_pw_fd(self) -> None:
        """Close the PipeWire fd exactly once, if held."""
        if self.pw_fd is not None:
            try:
                os.close(self.pw_fd)
            except OSError:
                pass
            self.pw_fd = None

    async def connect(self) -> bool:
        """
        Establish D-Bus connection and verify portal availability.

        Returns:
            True if portal is available, False otherwise.
        """
        if self.bus is not None:
            return True

        try:
            self.bus = await MessageBus(
                bus_type=BusType.SESSION,
                negotiate_unix_fd=True,
            ).connect()

            # Verify portal interface exists
            root_intro = await asyncio.wait_for(
                self.bus.introspect(PORTAL_BUS, PORTAL_PATH),
                PORTAL_CALL_TIMEOUT,
            )
            root_obj = self.bus.get_proxy_object(PORTAL_BUS, PORTAL_PATH, root_intro)
            root_obj.get_interface(SC_IFACE)
            return True

        except Exception as e:
            logger.error(f"Portal not available: {e}")
            self.bus = None
            return False

    async def start(
        self,
        output_dir: str,
        framerate: int = 1,
        draw_cursor: bool = True,
    ) -> list[StreamInfo]:
        """
        Start screencast recording for all monitors.

        Files are written directly to output_dir with final names (position_connector_screen.webm).
        The output_dir is typically a segment directory that will be renamed on completion.

        Args:
            output_dir: Directory for output files (e.g., YYYYMMDD/stream/HHMMSS.incomplete/)
            framerate: Frames per second (default: 1)
            draw_cursor: Whether to draw mouse cursor (default: True)

        Returns:
            List of StreamInfo for each monitor being recorded.

        Raises:
            RuntimeError: If recording fails to start.
        """
        if not await self.connect():
            raise RuntimeError("Portal not available")

        # Get monitor info from GDK for connector IDs
        from .activity import get_monitor_geometries

        try:
            monitors = get_monitor_geometries()
        except Exception as e:
            logger.warning(f"Failed to get monitor geometries: {e}")
            monitors = []

        # Fall back to KScreen on KDE when GDK is unavailable
        if not monitors and self.bus:
            from .activity import get_monitor_geometries_kscreen

            try:
                monitors = await get_monitor_geometries_kscreen(self.bus)
            except Exception as e:
                logger.warning(f"KScreen monitor fallback failed: {e}")
                monitors = []

        # Get portal interface
        try:
            root_intro = await asyncio.wait_for(
                self.bus.introspect(PORTAL_BUS, PORTAL_PATH),
                PORTAL_CALL_TIMEOUT,
            )
        except asyncio.TimeoutError:
            await self._close_session()
            raise RuntimeError("Portal introspect timed out")
        root_obj = self.bus.get_proxy_object(PORTAL_BUS, PORTAL_PATH, root_intro)
        screencast = root_obj.get_interface(SC_IFACE)

        # 1) CreateSession
        create_token = "h_" + uuid.uuid4().hex
        create_handle = _make_request_handle(self.bus, create_token)
        create_fut, create_handler = _prepare_request_handler(self.bus, create_handle)

        create_opts = {
            "handle_token": Variant("s", create_token),
            "session_handle_token": Variant("s", "s_" + uuid.uuid4().hex),
        }

        try:
            await asyncio.wait_for(
                screencast.call_create_session(create_opts),
                PORTAL_CALL_TIMEOUT,
            )
            resp, results = await asyncio.wait_for(create_fut, PORTAL_CALL_TIMEOUT)
        except asyncio.TimeoutError:
            await self._close_session()
            raise RuntimeError("CreateSession timed out")
        finally:
            self.bus.remove_message_handler(create_handler)
        if resp != 0:
            raise RuntimeError(f"CreateSession failed with code {resp}")

        self.session_handle = str(_variant_or_value(results.get("session_handle")))
        if not self.session_handle:
            raise RuntimeError("CreateSession returned no session_handle")

        logger.debug(f"Portal session: {self.session_handle}")

        # 2) SelectSources
        restore_token = _load_restore_token(self._restore_token_path)
        if restore_token:
            logger.debug("Using saved restore token")

        cursor_mode = 1 if draw_cursor else 0

        select_token = "h_" + uuid.uuid4().hex
        select_handle = _make_request_handle(self.bus, select_token)
        select_fut, select_handler = _prepare_request_handler(self.bus, select_handle)

        select_opts = {
            "handle_token": Variant("s", select_token),
            "types": Variant("u", 1),  # 1 = MONITOR
            "multiple": Variant("b", True),
            "cursor_mode": Variant("u", cursor_mode),
            "persist_mode": Variant("u", 2),  # Persist until revoked
        }
        if restore_token:
            select_opts["restore_token"] = Variant("s", restore_token)

        response_timeout = (
            PORTAL_INTERACTIVE_TIMEOUT if not restore_token else PORTAL_CALL_TIMEOUT
        )
        try:
            await asyncio.wait_for(
                screencast.call_select_sources(self.session_handle, select_opts),
                PORTAL_CALL_TIMEOUT,
            )
            resp, _ = await asyncio.wait_for(select_fut, response_timeout)
        except asyncio.TimeoutError:
            await self._close_session()
            raise RuntimeError("SelectSources timed out")
        finally:
            self.bus.remove_message_handler(select_handler)
        if resp != 0:
            await self._close_session()
            raise RuntimeError(f"SelectSources failed with code {resp}")

        # 3) Start
        start_token = "h_" + uuid.uuid4().hex
        start_handle = _make_request_handle(self.bus, start_token)
        start_fut, start_handler = _prepare_request_handler(self.bus, start_handle)

        start_opts = {"handle_token": Variant("s", start_token)}
        response_timeout = (
            PORTAL_INTERACTIVE_TIMEOUT if not restore_token else PORTAL_CALL_TIMEOUT
        )
        try:
            await asyncio.wait_for(
                screencast.call_start(self.session_handle, "", start_opts),
                PORTAL_CALL_TIMEOUT,
            )
            resp, results = await asyncio.wait_for(start_fut, response_timeout)
        except asyncio.TimeoutError:
            await self._close_session()
            raise RuntimeError("Start timed out")
        finally:
            self.bus.remove_message_handler(start_handler)
        if resp != 0:
            await self._close_session()
            raise RuntimeError(f"Start failed with code {resp}")

        portal_streams = _variant_or_value(results.get("streams")) or []
        if not portal_streams:
            await self._close_session()
            raise RuntimeError("Start returned no streams")

        # Save new restore token if provided
        new_token = _variant_or_value(results.get("restore_token"))
        if isinstance(new_token, str) and new_token.strip():
            _save_restore_token(new_token, self._restore_token_path)

        # Parse streams
        stream_info = []
        for idx, stream in enumerate(portal_streams):
            try:
                node_id = int(stream[0])
                props = stream[1] if len(stream) > 1 else {}
                stream_info.append({"idx": idx, "node_id": node_id, "props": props})
            except Exception as e:
                logger.warning(f"Could not parse stream {idx}: {e}")

        if not stream_info:
            await self._close_session()
            raise RuntimeError("No valid streams found")

        # Match streams to monitors
        stream_info = _match_streams_to_monitors(stream_info, monitors)

        logger.info(f"Portal returned {len(stream_info)} stream(s)")

        # 4) OpenPipeWireRemote
        try:
            fd_obj = await asyncio.wait_for(
                screencast.call_open_pipe_wire_remote(self.session_handle, {}),
                PORTAL_CALL_TIMEOUT,
            )
        except asyncio.TimeoutError:
            await self._close_session()
            raise RuntimeError("OpenPipeWireRemote timed out")
        if hasattr(fd_obj, "take"):
            self.pw_fd = fd_obj.take()
        else:
            self.pw_fd = int(fd_obj)

        # 5) Build GStreamer pipeline
        self.streams = []
        pipeline_parts = []

        for info in stream_info:
            node_id = info["node_id"]
            position = info["position_label"]
            connector = info["connector"]

            # Final file path: position_connector_screen.webm
            file_path = os.path.join(output_dir, f"{position}_{connector}_screen.webm")

            stream_obj = StreamInfo(
                node_id=node_id,
                position=position,
                connector=connector,
                x=info["x"],
                y=info["y"],
                width=info["width"],
                height=info["height"],
                file_path=file_path,
            )
            self.streams.append(stream_obj)

            # GStreamer branch for this stream
            branch = [
                "pipewiresrc",
                f"fd={self.pw_fd}",
                f"path={node_id}",
                "!",
                "videorate",
                "!",
                f"video/x-raw,framerate={framerate}/1",
                "!",
                "videoconvert",
                "!",
                "vp8enc",
                "end-usage=cq",
                "cq-level=4",
                "max-quantizer=15",
                "keyframe-max-dist=30",
                "static-threshold=100",
                "!",
                "webmmux",
                "!",
                "filesink",
                f"location={file_path}",
            ]
            pipeline_parts.append(branch)

            logger.info(f"  Stream {node_id}: {position} ({connector}) -> {file_path}")

        cmd = ["gst-launch-1.0", "-e"]
        for branch in pipeline_parts:
            cmd.extend(branch)

        try:
            self.gst_process = subprocess.Popen(
                cmd,
                pass_fds=(self.pw_fd,),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
        except FileNotFoundError:
            self._close_pw_fd()
            await self._close_session()
            raise RuntimeError("gst-launch-1.0 not found")
        except Exception as e:
            self._close_pw_fd()
            await self._close_session()
            raise RuntimeError(f"Failed to start GStreamer: {e}")

        # Brief delay to check for immediate failure
        await asyncio.sleep(0.2)
        if self.gst_process.poll() is not None:
            stderr = (
                self.gst_process.stderr.read().decode("utf-8", errors="replace")
                if self.gst_process.stderr
                else ""
            )
            self._close_pw_fd()
            await self._close_session()
            raise RuntimeError(f"GStreamer exited immediately: {stderr[:200]}")

        if self.gst_process.stderr is not None:
            self._stderr_drain = _StderrDrain(self.gst_process.stderr, "gst")
            self._stderr_drain.start()

        self._started = True
        return self.streams

    async def stop(self) -> tuple[list[StreamInfo], list[SilentStream]]:
        """
        Stop screencast recording gracefully.

        Returns:
            Healthy streams and silent streams that were dropped.
        """
        streams = self.streams.copy()

        # Stop GStreamer with SIGINT for clean EOS
        if self.gst_process and self.gst_process.poll() is None:
            try:
                self.gst_process.send_signal(signal.SIGINT)
                try:
                    await asyncio.wait_for(
                        asyncio.to_thread(self.gst_process.wait),
                        timeout=5.0,
                    )
                except asyncio.TimeoutError:
                    logger.warning("GStreamer did not exit cleanly, killing")
                    self.gst_process.kill()
                    self.gst_process.wait()
            except Exception as e:
                logger.warning(f"Error stopping GStreamer: {e}")

        self.gst_process = None

        if self._stderr_drain is not None:
            self._stderr_drain.join()
            self._stderr_drain = None

        healthy: list[StreamInfo] = []
        silent: list[SilentStream] = []
        for stream in streams:
            file_path = Path(stream.file_path)
            try:
                file_bytes = file_path.stat().st_size
            except FileNotFoundError:
                file_bytes = 0
            except OSError as exc:
                logger.warning("could not stat %s: %s", file_path, exc)
                file_bytes = 0

            if file_bytes >= MIN_HEALTHY_WEBM_BYTES:
                healthy.append(stream)
                continue

            silent.append(
                SilentStream(
                    node_id=stream.node_id,
                    connector=stream.connector,
                    position=stream.position,
                    file_path=file_path,
                    file_bytes=file_bytes,
                )
            )
            logger.warning(
                "silent stream dropped: connector=%s position=%s file_bytes=%d path=%s",
                stream.connector,
                stream.position,
                file_bytes,
                file_path,
            )
            try:
                file_path.unlink(missing_ok=True)
            except OSError as exc:
                logger.warning("could not unlink silent stream %s: %s", file_path, exc)

        # Close PipeWire fd
        self._close_pw_fd()

        # Close portal session
        await self._close_session()

        self.streams = []
        self._started = False

        return healthy, silent

    async def _close_session(self):
        """Close the portal session."""
        if self.session_handle and self.bus:
            try:
                session_intro = await asyncio.wait_for(
                    self.bus.introspect(PORTAL_BUS, self.session_handle),
                    PORTAL_CALL_TIMEOUT,
                )
                session_obj = self.bus.get_proxy_object(
                    PORTAL_BUS, self.session_handle, session_intro
                )
                session_iface = session_obj.get_interface(SESSION_IFACE)
                await asyncio.wait_for(
                    session_iface.call_close(),
                    PORTAL_CALL_TIMEOUT,
                )
            except (
                asyncio.TimeoutError,
                DBusError,
                InvalidMemberNameError,
                InvalidIntrospectionError,
                OSError,
            ) as exc:
                logger.warning(
                    "_close_session failed: service=%s path=%s: %s: %s",
                    PORTAL_BUS,
                    self.session_handle,
                    type(exc).__name__,
                    exc,
                )
        self.session_handle = None

    def is_healthy(self) -> bool:
        """Check if recording is still running."""
        if not self._started:
            return False
        if self.gst_process is None:
            return False
        return self.gst_process.poll() is None


class X11Screencaster:
    """X11 screen capture using GStreamer ximagesrc.

    Mirrors the Screencaster interface so the observer can use either
    backend interchangeably.  Each connected monitor becomes one independent
    GStreamer branch writing a VP8/WebM file at the configured framerate.
    """

    def __init__(self):
        self.gst_process: subprocess.Popen | None = None
        self.streams: list[StreamInfo] = []
        self._started = False
        self._stderr_drain: _StderrDrain | None = None

    async def connect(self) -> bool:
        """Verify the X11 display and GStreamer are available."""
        if not os.environ.get("DISPLAY"):
            logger.error("X11 capture: DISPLAY not set")
            return False
        if shutil.which("gst-launch-1.0") is None:
            logger.error("X11 capture: gst-launch-1.0 not found")
            return False
        return True

    async def start(
        self,
        output_dir: str,
        framerate: int = 1,
        draw_cursor: bool = True,
    ) -> list[StreamInfo]:
        """Start X11 screencast recording for all monitors.

        Files are written to output_dir with names position_connector_screen.webm,
        identical to the Wayland backend.

        Raises:
            RuntimeError: If no monitors are found or GStreamer fails to start.
        """
        display = os.environ.get("DISPLAY", ":0")

        from .activity import get_monitor_geometries, get_monitor_geometries_x11

        monitors = get_monitor_geometries_x11()
        if not monitors:
            try:
                monitors = get_monitor_geometries()
            except Exception as e:
                logger.warning("GDK monitor fallback failed: %s", e)

        if not monitors:
            raise RuntimeError("No monitors found for X11 capture")

        show_pointer = "true" if draw_cursor else "false"
        self.streams = []
        pipeline_parts = []

        for idx, monitor in enumerate(monitors):
            x1, y1, x2, y2 = monitor["box"]
            w, h = x2 - x1, y2 - y1
            position = monitor.get("position", "center")
            connector = monitor["id"]

            file_path = os.path.join(output_dir, f"{position}_{connector}_screen.webm")

            stream_obj = StreamInfo(
                node_id=idx,
                position=position,
                connector=connector,
                x=x1,
                y=y1,
                width=w,
                height=h,
                file_path=file_path,
            )
            self.streams.append(stream_obj)

            # ximagesrc endx/endy are inclusive pixel indices
            endx = x1 + w - 1
            endy = y1 + h - 1

            branch = [
                "ximagesrc",
                f"display-name={display}",
                f"startx={x1}",
                f"starty={y1}",
                f"endx={endx}",
                f"endy={endy}",
                "use-damage=false",
                f"show-pointer={show_pointer}",
                "!",
                "videorate",
                "!",
                f"video/x-raw,framerate={framerate}/1",
                "!",
                "videoconvert",
                "!",
                "vp8enc",
                "end-usage=cq",
                "cq-level=4",
                "max-quantizer=15",
                "keyframe-max-dist=30",
                "static-threshold=100",
                "!",
                "webmmux",
                "!",
                "filesink",
                f"location={file_path}",
            ]
            pipeline_parts.append(branch)

            logger.info(
                "  X11 stream %d: %s (%s) -> %s", idx, position, connector, file_path
            )

        cmd = ["gst-launch-1.0", "-e"]
        for branch in pipeline_parts:
            cmd.extend(branch)

        try:
            self.gst_process = subprocess.Popen(
                cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
        except FileNotFoundError:
            raise RuntimeError("gst-launch-1.0 not found")
        except Exception as e:
            raise RuntimeError(f"Failed to start GStreamer (X11): {e}")

        await asyncio.sleep(0.2)
        if self.gst_process.poll() is not None:
            stderr = (
                self.gst_process.stderr.read().decode("utf-8", errors="replace")
                if self.gst_process.stderr
                else ""
            )
            raise RuntimeError(f"GStreamer (X11) exited immediately: {stderr[:200]}")

        if self.gst_process.stderr is not None:
            self._stderr_drain = _StderrDrain(self.gst_process.stderr, "gst-x11")
            self._stderr_drain.start()

        self._started = True
        return self.streams

    async def stop(self) -> tuple[list[StreamInfo], list[SilentStream]]:
        """Stop X11 screencast recording gracefully."""
        streams = self.streams.copy()

        if self.gst_process and self.gst_process.poll() is None:
            try:
                self.gst_process.send_signal(signal.SIGINT)
                try:
                    await asyncio.wait_for(
                        asyncio.to_thread(self.gst_process.wait),
                        timeout=5.0,
                    )
                except asyncio.TimeoutError:
                    logger.warning("GStreamer (X11) did not exit cleanly, killing")
                    self.gst_process.kill()
                    self.gst_process.wait()
            except Exception as e:
                logger.warning("Error stopping GStreamer (X11): %s", e)

        self.gst_process = None

        if self._stderr_drain is not None:
            self._stderr_drain.join()
            self._stderr_drain = None

        healthy: list[StreamInfo] = []
        silent: list[SilentStream] = []
        for stream in streams:
            file_path = Path(stream.file_path)
            try:
                file_bytes = file_path.stat().st_size
            except FileNotFoundError:
                file_bytes = 0
            except OSError as exc:
                logger.warning("could not stat %s: %s", file_path, exc)
                file_bytes = 0

            if file_bytes >= MIN_HEALTHY_WEBM_BYTES:
                healthy.append(stream)
                continue

            silent.append(
                SilentStream(
                    node_id=stream.node_id,
                    connector=stream.connector,
                    position=stream.position,
                    file_path=file_path,
                    file_bytes=file_bytes,
                )
            )
            logger.warning(
                "silent stream dropped: connector=%s position=%s file_bytes=%d path=%s",
                stream.connector,
                stream.position,
                file_bytes,
                file_path,
            )
            try:
                file_path.unlink(missing_ok=True)
            except OSError as exc:
                logger.warning("could not unlink silent stream %s: %s", file_path, exc)

        self.streams = []
        self._started = False
        return healthy, silent

    def is_healthy(self) -> bool:
        """Check if recording is still running."""
        if not self._started:
            return False
        if self.gst_process is None:
            return False
        return self.gst_process.poll() is None
