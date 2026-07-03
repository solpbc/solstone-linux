# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""
Standalone Linux desktop observer — screen + audio capture.

Continuously captures audio and manages screencast recording based on activity.
Creates 5-minute segments in a local cache directory. The sync service handles
all uploads — the observer only writes locally.

Key architectural change from monorepo version:
- Capture writes completed segments to local cache only
- No ObserverClient usage in boundary handling — no network calls in capture loop
- Sync service picks up completed segments and uploads asynchronously

State machine:
    SCREENCAST: Screen is active, recording video
    IDLE: Screen is inactive
"""

import asyncio
import datetime
import logging
import os
import platform
import signal
import socket
import time
from pathlib import Path

import numpy as np
from dbus_next.aio import MessageBus
from dbus_next.constants import BusType

from . import __version__
from .activity import (
    is_power_save_active,
    is_screen_locked,
    probe_activity_services,
)
from .audio_mute import is_sink_muted
from .audio_recorder import AudioRecorder
from .chat_bridge import run_chat_bridge
from .config import Config
from .recovery import write_segment_metadata
from .screencast import Screencaster, SilentStream, StreamInfo, X11Screencaster
from .sync import SyncService
from .upload import STREAM_TYPE, UploadClient

logger = logging.getLogger(__name__)


def _create_screencaster(config) -> "Screencaster | X11Screencaster":
    """Return the appropriate screencaster for the current desktop session.

    Selection order:
    1. XDG_SESSION_TYPE=x11  → X11Screencaster
    2. WAYLAND_DISPLAY set   → Screencaster (portal/PipeWire)
    3. Only DISPLAY set      → X11Screencaster (no Wayland available)
    4. Fallback              → Screencaster (portal/PipeWire)
    """
    session_type = os.environ.get("XDG_SESSION_TYPE", "").lower()
    if session_type == "x11":
        logger.info("X11 session detected — using X11 screencaster")
        return X11Screencaster()
    if os.environ.get("WAYLAND_DISPLAY") or session_type == "wayland":
        logger.info("Wayland session detected — using portal/PipeWire screencaster")
        return Screencaster(config.restore_token_path)
    if os.environ.get("DISPLAY") and not os.environ.get("WAYLAND_DISPLAY"):
        logger.info("No Wayland display found — falling back to X11 screencaster")
        return X11Screencaster()
    logger.info("Using portal/PipeWire screencaster (default)")
    return Screencaster(config.restore_token_path)


# Host identification
HOST = socket.gethostname()
PLATFORM = platform.system().lower()

# Constants
RMS_THRESHOLD = 0.01
MIN_HITS_FOR_SAVE = 3
CHUNK_DURATION = 5  # seconds

# Capture modes
MODE_IDLE = "idle"
MODE_SCREENCAST = "screencast"

# Audio detection retry
DETECT_RETRIES = 3
DETECT_RETRY_DELAY = 5  # seconds


def _get_timestamp_parts(timestamp: float | None = None) -> tuple[str, str]:
    """Get date and time parts from timestamp."""
    if timestamp is None:
        timestamp = time.time()
    dt = datetime.datetime.fromtimestamp(timestamp)
    return dt.strftime("%Y%m%d"), dt.strftime("%H%M%S")


class Observer:
    """Unified audio and screencast observer with local cache + sync."""

    def __init__(self, config: Config):
        self.config = config
        self.interval = config.segment_interval
        self.audio_recorder = AudioRecorder()
        self.screencaster = _create_screencaster(config)
        self.bus: MessageBus | None = None
        self.running = True
        self.stream = config.stream

        self._client: UploadClient | None = None
        self._sync: SyncService | None = None

        # State tracking
        self.start_at = time.time()
        self.start_at_mono = time.monotonic()
        self._start_mono = time.monotonic()
        self.threshold_hits = 0
        self.accumulated_audio_buffer = np.array([], dtype=np.float32).reshape(0, 2)

        # Mode tracking
        self.current_mode = MODE_IDLE

        # Segment directory (HHMMSS.incomplete/)
        self.segment_dir: Path | None = None

        # Multi-file screencast tracking
        self.current_streams: list[StreamInfo] = []

        # Activity status cache (updated each loop)
        self.cached_is_active = False
        self.cached_screen_locked = False
        self.cached_is_muted = False
        self.cached_power_save = False

        # Mute state at segment start (determines save format)
        self.segment_is_muted = False

        # Pause state
        self._paused = False
        self._pause_until = 0.0

        # D-Bus service interface
        self._dbus_service = None
        self._tray = None

    async def setup(self) -> bool:
        """Initialize audio devices, DBus connection, and sync service."""
        # Detect audio devices with retry (devices may still be initializing)
        detected = False
        for attempt in range(DETECT_RETRIES):
            if self.audio_recorder.detect():
                detected = True
                break
            if attempt < DETECT_RETRIES - 1:
                logger.info(
                    "Audio detection attempt %d/%d failed, retrying in %ds",
                    attempt + 1,
                    DETECT_RETRIES,
                    DETECT_RETRY_DELAY,
                )
                await asyncio.sleep(DETECT_RETRY_DELAY)
        if not detected:
            logger.error("Failed to detect audio devices")
            return False

        self.audio_recorder.start_recording()
        logger.info("Audio recording started")

        # Connect to DBus for activity detection
        self.bus = await MessageBus(bus_type=BusType.SESSION).connect()
        logger.info("DBus connection established")

        # Probe which activity signals are available (logging only)
        await probe_activity_services(self.bus)

        # Verify capture backend is available (exit if not)
        if not await self.screencaster.connect():
            logger.error("Screencast capture backend not available")
            return False
        logger.info("Screencast capture backend connected")

        # Initialize upload client and sync service
        self._client = UploadClient(self.config)
        if self.config.server_url:
            self._client.ensure_registered(self.config)
        self.stream = self.config.stream
        self._sync = SyncService(self.config, self._client)

        from .dbus_service import BUS_NAME, OBJECT_PATH, ObserverService

        self._dbus_service = ObserverService(self)
        self.bus.export(OBJECT_PATH, self._dbus_service)
        await self.bus.request_name(BUS_NAME)
        self._sync._dbus_service = self._dbus_service
        logger.info("D-Bus service exported as %s", BUS_NAME)

        # Initialize system tray (graceful: skip if no StatusNotifierWatcher)
        try:
            from .tray import TrayApp

            tray = TrayApp(self, self.bus)
            started = await tray.start()
            if started:
                self._tray = tray
                logger.info("System tray active")
            else:
                logger.info("System tray unavailable (no StatusNotifierWatcher)")
        except Exception as e:
            logger.info("System tray disabled: %s", e)

        logger.info("Sync service initialized")

        return True

    async def check_activity_status(self) -> str:
        """Check system activity status and determine capture mode."""
        screen_locked = await is_screen_locked(self.bus)
        power_save = await is_power_save_active(self.bus)
        sink_muted = await is_sink_muted()

        # Cache values for status events
        self.cached_screen_locked = screen_locked
        self.cached_is_muted = sink_muted
        self.cached_power_save = power_save

        # Determine screen activity
        screen_idle = screen_locked or power_save
        screen_active = not screen_idle

        # Determine mode
        if screen_active:
            mode = MODE_SCREENCAST
        else:
            mode = MODE_IDLE

        # Cache legacy is_active for audio threshold logic
        has_audio_activity = self.threshold_hits >= MIN_HITS_FOR_SAVE
        self.cached_is_active = screen_active or has_audio_activity

        return mode

    def compute_rms(self, audio_buffer: np.ndarray) -> float:
        """Compute per-channel RMS and return maximum (stereo: mic=left, sys=right)."""
        if audio_buffer.size == 0:
            return 0.0
        rms_left = float(np.sqrt(np.mean(audio_buffer[:, 0] ** 2)))
        rms_right = float(np.sqrt(np.mean(audio_buffer[:, 1] ** 2)))
        return max(rms_left, rms_right)

    def _save_audio_segment(self, segment_dir: Path, is_muted: bool) -> list[str]:
        """Save accumulated audio buffer to segment directory."""
        if self.accumulated_audio_buffer.size == 0:
            logger.warning("No audio buffer to save")
            return []

        if is_muted:
            # Split mode: save mic and sys as separate mono files
            mic_data = self.accumulated_audio_buffer[:, 0]
            sys_data = self.accumulated_audio_buffer[:, 1]

            mic_bytes = self.audio_recorder.create_mono_flac_bytes(mic_data)
            sys_bytes = self.audio_recorder.create_mono_flac_bytes(sys_data)

            (segment_dir / "mic_audio.flac").write_bytes(mic_bytes)
            (segment_dir / "sys_audio.flac").write_bytes(sys_bytes)

            logger.info(f"Saved split audio (muted): {segment_dir}")
            return ["mic_audio.flac", "sys_audio.flac"]
        else:
            # Normal mode: save combined stereo file
            flac_bytes = self.audio_recorder.create_flac_bytes(
                self.accumulated_audio_buffer
            )
            (segment_dir / "audio.flac").write_bytes(flac_bytes)

            logger.info(f"Saved audio to {segment_dir}/audio.flac")
            return ["audio.flac"]

    def _start_segment(self) -> Path:
        """Start a new segment with .incomplete directory."""
        self.start_at = time.time()
        self.start_at_mono = time.monotonic()

        date_part, time_part = _get_timestamp_parts(self.start_at)
        captures_dir = self.config.captures_dir

        # Create YYYYMMDD/stream/HHMMSS.incomplete/
        segment_dir = captures_dir / date_part / self.stream / f"{time_part}.incomplete"
        segment_dir.mkdir(parents=True, exist_ok=True)
        self.segment_dir = segment_dir

        # Write metadata for recovery
        write_segment_metadata(segment_dir, self.start_at)

        return segment_dir

    def _finalize_segment(self) -> str | None:
        """Rename .incomplete to HHMMSS_DDD/ and return segment key."""
        if not self.segment_dir or not self.segment_dir.exists():
            return None

        # Remove .metadata before finalizing
        meta_path = self.segment_dir / ".metadata"
        if meta_path.exists():
            try:
                meta_path.unlink()
            except OSError:
                pass

        # Check if there are any actual files
        contents = [f for f in self.segment_dir.iterdir() if f.is_file()]
        if not contents:
            # Empty segment, remove it
            try:
                os.rmdir(str(self.segment_dir))
            except OSError:
                pass
            return None

        _, time_part = _get_timestamp_parts(self.start_at)
        duration = max(1, min(int(time.time() - self.start_at), self.interval))
        segment_key = f"{time_part}_{duration}"
        final_dir = self.segment_dir.parent / segment_key

        try:
            os.rename(str(self.segment_dir), str(final_dir))
            logger.info(f"Segment finalized: {segment_key}")
            return segment_key
        except OSError as e:
            logger.error(f"Failed to finalize segment: {e}")
            return None

    async def handle_boundary(self, new_mode: str):
        """Handle window boundary rollover.

        Closes the current segment, writes audio, finalizes to local cache,
        and triggers sync. No network calls in the capture loop.
        """
        # Stop screencast first (closes file handles)
        if self.current_mode == MODE_SCREENCAST:
            logger.info("Stopping previous screencast")
            healthy, silent = await self.screencaster.stop()
            for s in silent:
                self._emit_stream_silent(s)
            self.current_streams = []

        # Save audio if we have enough threshold hits
        did_save_audio = self.threshold_hits >= MIN_HITS_FOR_SAVE
        if did_save_audio and self.segment_dir:
            audio_files = self._save_audio_segment(
                self.segment_dir, self.segment_is_muted
            )
            if audio_files:
                logger.info(
                    f"Saved {len(audio_files)} audio file(s) ({self.threshold_hits} hits)"
                )
        else:
            logger.debug(
                f"Skipping audio save (only {self.threshold_hits}/{MIN_HITS_FOR_SAVE} hits)"
            )

        # Reset audio state
        self.accumulated_audio_buffer = np.array([], dtype=np.float32).reshape(0, 2)
        self.threshold_hits = 0

        # Finalize segment (rename .incomplete -> HHMMSS_DDD/)
        segment_key = self._finalize_segment()
        self.segment_dir = None

        # Trigger sync to upload the completed segment
        if segment_key and self._sync:
            self._sync.trigger()

        # Update segment mute state for new segment
        self.segment_is_muted = self.cached_is_muted

        # Update mode
        old_mode = self.current_mode
        self.current_mode = new_mode

        # Start new capture based on mode
        if new_mode == MODE_SCREENCAST and not self.cached_screen_locked:
            await self.initialize_screencast()
        else:
            self._start_segment()

        logger.info(f"Mode transition: {old_mode} -> {new_mode}")

    async def initialize_screencast(self) -> bool:
        """Start a new screencast recording.

        Creates a segment directory and starts GStreamer recording to it.
        """
        segment_dir = self._start_segment()

        try:
            streams = await self.screencaster.start(
                str(segment_dir),
                framerate=self.config.capture_framerate,
                draw_cursor=self.config.draw_cursor,
            )
        except RuntimeError as e:
            logger.error(f"Failed to start screencast: {e}")
            raise

        if not streams:
            logger.error("No streams returned from screencast start")
            raise RuntimeError("No streams available")

        self.current_streams = streams

        logger.info(f"Started screencast with {len(streams)} stream(s)")
        for stream in streams:
            logger.info(f"  {stream.position} ({stream.connector}): {stream.file_path}")

        return True

    def _emit_stream_silent(self, silent: SilentStream) -> None:
        if self._client is None:
            return
        segment_dir_basename = self.segment_dir.name if self.segment_dir else ""
        duration_seconds = int(time.time() - self.start_at) if self.start_at else 0
        self._client.relay_event(
            "observe",
            "stream_silent",
            connector=silent.connector,
            position=silent.position,
            node_id=silent.node_id,
            file_bytes=silent.file_bytes,
            segment_dir=segment_dir_basename,
            duration_seconds=duration_seconds,
            host=HOST,
            platform=PLATFORM,
        )

    def emit_status(self):
        """Emit observe.status event with current state (fire-and-forget)."""
        if not self._client:
            return

        elapsed = int(time.monotonic() - self.start_at_mono)

        # Screencast info
        if self.current_mode == MODE_SCREENCAST and self.current_streams:
            streams_info = [
                {
                    "position": stream.position,
                    "connector": stream.connector,
                    "file": stream.file_path,
                }
                for stream in self.current_streams
            ]
            screencast_info = {
                "recording": True,
                "streams": streams_info,
                "window_elapsed_seconds": elapsed,
            }
        else:
            screencast_info = {"recording": False}

        # Audio info
        audio_info = {
            "threshold_hits": self.threshold_hits,
            "will_save": self.threshold_hits >= MIN_HITS_FOR_SAVE,
        }

        # Activity info
        activity_info = {
            "active": self.cached_is_active,
            "screen_locked": self.cached_screen_locked,
            "sink_muted": self.cached_is_muted,
            "power_save": self.cached_power_save,
        }

        status_fields = {
            "mode": self.current_mode,
            "screencast": screencast_info,
            "audio": audio_info,
            "activity": activity_info,
            "host": HOST,
            "platform": PLATFORM,
        }
        if self._client.is_registered and self.stream:
            status_fields.update(
                {
                    "name": self.stream,
                    "stream_type": STREAM_TYPE,
                    "version": __version__,
                    "uptime": elapsed,
                }
            )
            if self._sync is not None:
                status_fields.update(self._sync.health_beacon_fields())

        self._client.relay_event("observe", "status", **status_fields)

    def _refresh_tray(self):
        """Refresh the SNI tray UI. Safe when tray is unavailable; disables on failure."""
        if self._tray is None:
            return
        try:
            self._tray.update()
        except Exception:
            logger.warning("Tray update failed, disabling tray", exc_info=True)
            self._tray = None

    def pause(self, duration_seconds: int):
        """Pause capture. duration_seconds=0 means indefinite."""
        self._paused = True
        if duration_seconds > 0:
            self._pause_until = time.monotonic() + duration_seconds
        else:
            self._pause_until = 0.0
        if self._dbus_service:
            self._dbus_service.StatusChanged("paused")
        logger.info("Paused for %ss", duration_seconds)
        self._refresh_tray()

    def resume(self):
        """Resume capture from pause."""
        self._paused = False
        self._pause_until = 0.0
        if self._dbus_service:
            self._dbus_service.StatusChanged(
                "recording" if self.current_mode == MODE_SCREENCAST else "idle"
            )
        logger.info("Resumed")
        self._refresh_tray()

    async def main_loop(self):
        """Run the main observer loop with background sync."""
        logger.info(f"Starting observer loop (interval={self.interval}s)")

        # Start sync service as background task
        bridge_stop_event = asyncio.Event()
        bridge_task = None
        sync_task = None
        if self._sync:
            sync_task = asyncio.create_task(self._sync.run())
        if self.config.chat_bridge_enabled:
            bridge_task = asyncio.create_task(
                run_chat_bridge(self.config, bridge_stop_event)
            )

        # Determine initial mode (default to screencast if check fails)
        try:
            new_mode = await self.check_activity_status()
        except Exception as e:
            logger.warning(
                "Initial activity check failed: %s — defaulting to screencast", e
            )
            new_mode = MODE_SCREENCAST
        self.segment_is_muted = self.cached_is_muted
        self.current_mode = new_mode

        if self.config.start_paused:
            self.pause(0)
            logger.info("Starting in paused mode (start_paused=true)")

        # Start initial capture based on mode (skipped when starting paused)
        if not self._paused:
            if new_mode == MODE_SCREENCAST and not self.cached_screen_locked:
                try:
                    await self.initialize_screencast()
                except RuntimeError:
                    self.running = False
                    if sync_task:
                        if self._sync:
                            self._sync.stop()
                        sync_task.cancel()
                        try:
                            await sync_task
                        except asyncio.CancelledError:
                            pass
                    bridge_stop_event.set()
                    if bridge_task:
                        bridge_task.cancel()
                        try:
                            await bridge_task
                        except (asyncio.CancelledError, Exception):
                            pass
                    return
            else:
                self._start_segment()

        logger.info(f"Initial mode: {self.current_mode}")

        try:
            while self.running:
                await asyncio.sleep(CHUNK_DURATION)

                # Check auto-resume from timed pause
                if (
                    self._paused
                    and self._pause_until > 0
                    and time.monotonic() >= self._pause_until
                ):
                    self._paused = False
                    self._pause_until = 0.0
                    if self._dbus_service:
                        self._dbus_service.StatusChanged(
                            "recording"
                            if self.current_mode == MODE_SCREENCAST
                            else "idle"
                        )
                    logger.info("Auto-resumed from timed pause")
                    self._refresh_tray()

                # Handle paused state
                if self._paused:
                    if self.segment_dir:
                        if self.current_mode == MODE_SCREENCAST:
                            healthy, silent = await self.screencaster.stop()
                            for s in silent:
                                self._emit_stream_silent(s)
                            self.current_streams = []
                        if self.threshold_hits >= MIN_HITS_FOR_SAVE:
                            self._save_audio_segment(
                                self.segment_dir, self.segment_is_muted
                            )
                        self.accumulated_audio_buffer = np.array(
                            [], dtype=np.float32
                        ).reshape(0, 2)
                        self.threshold_hits = 0
                        segment_key = self._finalize_segment()
                        self.segment_dir = None
                        if segment_key and self._sync:
                            self._sync.trigger()
                    self.audio_recorder.get_buffers()
                    self.emit_status()
                    self._refresh_tray()
                    continue

                # Resume: start new segment if needed (segment_dir is None after pause)
                if self.segment_dir is None:
                    try:
                        new_mode = await self.check_activity_status()
                    except Exception:
                        new_mode = self.current_mode
                    self.segment_is_muted = self.cached_is_muted
                    self.current_mode = new_mode
                    if new_mode == MODE_SCREENCAST and not self.cached_screen_locked:
                        try:
                            await self.initialize_screencast()
                        except RuntimeError:
                            self._start_segment()
                    else:
                        self._start_segment()
                    self.emit_status()
                    continue

                # Check activity status and determine new mode
                try:
                    new_mode = await self.check_activity_status()
                except Exception as e:
                    logger.warning(
                        "Activity check failed: %s — keeping current mode", e
                    )
                    new_mode = self.current_mode

                # Check for GStreamer failure mid-recording
                if (
                    self.current_mode == MODE_SCREENCAST
                    and not self.screencaster.is_healthy()
                ):
                    logger.warning("Screencast recording failed, stopping gracefully")
                    healthy, silent = await self.screencaster.stop()
                    for s in silent:
                        self._emit_stream_silent(s)
                    self.current_streams = []
                    self.current_mode = MODE_IDLE

                # Detect mode change
                mode_changed = new_mode != self.current_mode
                if mode_changed:
                    logger.info(f"Mode changing: {self.current_mode} -> {new_mode}")

                # Only trigger segment boundary on screencast transitions
                screencast_transition = mode_changed and (
                    self.current_mode == MODE_SCREENCAST or new_mode == MODE_SCREENCAST
                )

                # Detect mute state transition
                mute_transition = self.cached_is_muted != self.segment_is_muted
                if mute_transition:
                    logger.info(
                        f"Mute state changed: "
                        f"{'muted' if self.segment_is_muted else 'unmuted'} -> "
                        f"{'muted' if self.cached_is_muted else 'unmuted'}"
                    )

                # Capture audio buffer for this chunk
                audio_chunk = self.audio_recorder.get_buffers()

                if audio_chunk.size > 0:
                    self.accumulated_audio_buffer = np.vstack(
                        (self.accumulated_audio_buffer, audio_chunk)
                    )
                    rms = self.compute_rms(audio_chunk)
                    if rms > RMS_THRESHOLD:
                        self.threshold_hits += 1
                        logger.debug(
                            f"RMS {rms:.4f} > threshold (hit {self.threshold_hits})"
                        )
                    else:
                        logger.debug(f"RMS {rms:.4f} below threshold")
                else:
                    logger.debug("No audio data in chunk")

                # Check for window boundary (monotonic to avoid DST/clock jumps)
                elapsed = time.monotonic() - self.start_at_mono
                is_boundary = (
                    (elapsed >= self.interval)
                    or screencast_transition
                    or mute_transition
                )

                if is_boundary:
                    logger.info(
                        f"Boundary: elapsed={elapsed:.1f}s screencast_change={screencast_transition} "
                        f"mute_change={mute_transition} "
                        f"hits={self.threshold_hits}/{MIN_HITS_FOR_SAVE}"
                    )
                    await self.handle_boundary(new_mode)
                    if mode_changed and self._dbus_service:
                        status = "recording" if new_mode == MODE_SCREENCAST else "idle"
                        self._dbus_service.StatusChanged(status)
                        self._refresh_tray()

                # Emit status event
                self.emit_status()
                self._refresh_tray()
        finally:
            # Cleanup on exit
            logger.info("Observer loop stopped, cleaning up...")
            await self.shutdown()
            if sync_task:
                if self._sync:
                    self._sync.stop()
                sync_task.cancel()
                try:
                    await sync_task
                except asyncio.CancelledError:
                    pass
            bridge_stop_event.set()
            if bridge_task:
                bridge_task.cancel()
                try:
                    await bridge_task
                except (asyncio.CancelledError, Exception):
                    pass

    async def shutdown(self):
        """Clean shutdown of observer."""
        # Stop screencast first (closes file handles)
        if self.current_mode == MODE_SCREENCAST:
            logger.info("Stopping screencast for shutdown")
            healthy, silent = await self.screencaster.stop()
            for s in silent:
                self._emit_stream_silent(s)
            await asyncio.sleep(0.5)

        # Save final audio if threshold met
        if self.threshold_hits >= MIN_HITS_FOR_SAVE and self.segment_dir:
            audio_files = self._save_audio_segment(
                self.segment_dir, self.segment_is_muted
            )
            if audio_files:
                logger.info(f"Saved final audio: {len(audio_files)} file(s)")

        # Finalize segment locally
        segment_key = self._finalize_segment()
        self.segment_dir = None

        if segment_key:
            logger.info(f"Finalized segment locally: {segment_key} (shutdown)")

        # Stop audio recorder
        self.audio_recorder.stop_recording()
        logger.info("Audio recording stopped")

        if self._client:
            self._client.stop()
            self._client = None
        logger.info("Client stopped")


async def async_run(config: Config) -> int:
    """Async entry point for the observer."""
    from .session_env import check_session_ready

    # Pre-flight: check session prerequisites
    not_ready = check_session_ready()
    if not_ready:
        logger.warning("Session not ready: %s", not_ready)
        return 75  # EXIT_TEMPFAIL

    observer = Observer(config)

    loop = asyncio.get_running_loop()

    def signal_handler():
        logger.info("Received shutdown signal")
        observer.running = False

    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, signal_handler)

    if not await observer.setup():
        logger.error("Observer setup failed")
        return 1

    try:
        await observer.main_loop()
    except RuntimeError as e:
        logger.error(f"Observer runtime error: {e}")
        return 1
    except Exception as e:
        logger.error(f"Observer error: {e}", exc_info=True)
        return 1

    return 0
