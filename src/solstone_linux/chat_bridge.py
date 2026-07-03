# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Bridge server-initiated chat events into local notification surfaces.

The bridge consumes callosum SSE frames, mirrors requests into an optional FIFO,
and fires click-capturing desktop notifications when the server opt-in allows it.
"""

from __future__ import annotations

import asyncio
import errno
import json
import logging
import os
import stat
import subprocess
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

import requests

from .config import Config

logger = logging.getLogger(__name__)

# Keep these event names and owner-facing copy hand-synced with
# solstone/convey/sol_initiated/copy.py; this repo does not vendor that canon.
EVENT_SOL_CHAT_REQUEST = "sol_chat_request"
EVENT_SOL_CHAT_REQUEST_SUPERSEDED = "sol_chat_request_superseded"
EVENT_OWNER_CHAT_OPEN = "owner_chat_open"
EVENT_OWNER_CHAT_DISMISSED = "owner_chat_dismissed"

NOTIFY_TITLE = "sol"
SURFACE = "linux"
FIFO_PATH = Path.home() / ".solstone" / "notify"
_HANDLED_EVENTS = frozenset(
    {
        EVENT_SOL_CHAT_REQUEST,
        EVENT_SOL_CHAT_REQUEST_SUPERSEDED,
        EVENT_OWNER_CHAT_OPEN,
        EVENT_OWNER_CHAT_DISMISSED,
    }
)
RECONNECT_DELAYS = [1, 2, 4, 8, 16, 30]
HEARTBEAT_STALE_SECONDS = 60
SSE_CONNECT_TIMEOUT_SECONDS = 10
# Read must outlast the soft-stale window; deriving it keeps the invariant fixed.
SSE_READ_TIMEOUT_SECONDS = HEARTBEAT_STALE_SECONDS + 30
NOTIFY_ACTION_KEY = "open"
# A body that ran at least this long before crashing resets the restart ladder.
HEALTHY_RUN_SECONDS = 60
OPT_IN_POLL_SECONDS = 300
PENDING_CAP = 32


@dataclass
class PendingRequest:
    request_id: str
    summary: str
    chat_url: str
    notify_task: asyncio.Task | None = None


class _SseParser:
    def __init__(self) -> None:
        self._event: str | None = None
        self._data: list[str] = []
        self._id: str | None = None

    def feed_line(self, line: str) -> dict[str, str | None] | None:
        line = line.rstrip("\r\n")
        if line == "":
            if not self._data:
                self._event = None
                self._id = None
                return None
            frame = {
                "event": self._event,
                "data": "\n".join(self._data),
                "id": self._id,
            }
            self._event = None
            self._data = []
            self._id = None
            return frame

        if line.startswith(":"):
            return None

        field, sep, value = line.partition(":")
        if sep and value.startswith(" "):
            value = value[1:]

        if field == "data":
            self._data.append(value)
        elif field == "event":
            self._event = value
        elif field == "id":
            self._id = value

        return None


def _auth_headers(key: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {key}"}


def _write_fifo(line: str, path: Path = FIFO_PATH) -> None:
    try:
        if not path.exists():
            logger.debug("Chat bridge FIFO missing: %s", path)
            return
        if not stat.S_ISFIFO(path.stat().st_mode):
            logger.debug("Chat bridge path is not a FIFO: %s", path)
            return

        fd = os.open(path, os.O_WRONLY | os.O_NONBLOCK)
        try:
            os.write(fd, line.encode("utf-8"))
        finally:
            os.close(fd)
    except FileNotFoundError:
        logger.debug("Chat bridge FIFO missing: %s", path)
    except BlockingIOError:
        logger.debug("Chat bridge FIFO has no reader: %s", path)
    except OSError as e:
        if e.errno in (errno.ENXIO, errno.EAGAIN, errno.EWOULDBLOCK):
            logger.debug("Chat bridge FIFO unavailable: %s", e)
            return
        logger.warning("Chat bridge FIFO write failed: %s", e)


def _push_frame(
    queue: asyncio.Queue,
    loop: asyncio.AbstractEventLoop,
    frame: dict[str, Any],
) -> None:
    loop.call_soon_threadsafe(queue.put_nowait, frame)


def _sse_worker(
    url: str,
    key: str,
    queue: asyncio.Queue,
    loop: asyncio.AbstractEventLoop,
    stop_event: threading.Event,
) -> None:
    parser = _SseParser()
    try:
        response = requests.get(
            url,
            stream=True,
            headers=_auth_headers(key),
            timeout=(SSE_CONNECT_TIMEOUT_SECONDS, SSE_READ_TIMEOUT_SECONDS),
        )
        if response.status_code in (401, 403):
            _push_frame(
                queue, loop, {"_terminal": True, "status": response.status_code}
            )
            return
        if response.status_code != 200:
            _push_frame(
                queue,
                loop,
                {
                    "_transport_error": True,
                    "error": f"status {response.status_code}",
                },
            )
            return

        for raw_line in response.iter_lines(decode_unicode=True):
            if stop_event.is_set():
                return
            if raw_line is None:
                continue
            line = raw_line.decode("utf-8") if isinstance(raw_line, bytes) else raw_line
            if line.startswith(":"):
                _push_frame(queue, loop, {"_heartbeat": True})
            frame = parser.feed_line(line)
            if frame is not None:
                _push_frame(queue, loop, frame)
    except requests.RequestException as e:
        _push_frame(queue, loop, {"_transport_error": True, "error": str(e)})


async def _poll_opt_in(server_url: str, key: str) -> bool:
    url = f"{server_url.rstrip('/')}/api/sol_voice"

    try:
        response = await asyncio.to_thread(
            requests.get,
            url,
            headers=_auth_headers(key),
            timeout=10,
        )
        if response.status_code != 200:
            return False
        data = response.json()
    except (requests.RequestException, ValueError, TypeError):
        return False

    return bool(data.get("linux_notify_send", False))


def _chat_url(server_url: str, day: str | None, event_index: int | None) -> str:
    base = server_url.rstrip("/")
    if day and event_index is not None:
        return f"{base}/app/chat/{day}#event-{event_index}"
    today = datetime.now().strftime("%Y%m%d")
    return f"{base}/app/chat/{today}"


async def _handle_one_notification(
    req: PendingRequest, server_url: str, key: str
) -> None:
    proc = await asyncio.create_subprocess_exec(
        "notify-send",
        "--wait",
        "--app-name",
        "solstone",
        f"--action={NOTIFY_ACTION_KEY}=Open",
        NOTIFY_TITLE,
        req.summary,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
    )
    try:
        stdout, _ = await proc.communicate()
    except asyncio.CancelledError:
        proc.terminate()
        try:
            await asyncio.wait_for(proc.wait(), timeout=1)
        except asyncio.TimeoutError:
            proc.kill()
            await proc.wait()
        raise

    if proc.returncode != 0:
        logger.debug("notify-send exited with status %s", proc.returncode)
        return

    action = stdout.decode("utf-8", "replace").strip() if stdout else ""
    if action != NOTIFY_ACTION_KEY:
        logger.debug("notify-send dismissed without action")
        return

    logger.info("Opening chat request: %s", req.request_id)
    url = f"{server_url.rstrip('/')}/api/chat/{EVENT_SOL_CHAT_REQUEST}/open"
    try:
        response = await asyncio.to_thread(
            requests.post,
            url,
            json={"request_id": req.request_id},
            headers=_auth_headers(key),
            timeout=10,
        )
        if response.status_code >= 400:
            logger.debug("Chat open ack failed: status %s", response.status_code)
    except requests.RequestException as e:
        logger.debug("Chat open ack failed: %s", e)

    try:
        subprocess.Popen(
            ["xdg-open", req.chat_url],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as e:
        logger.debug("xdg-open failed: %s", e)


async def _opt_in_poll_loop(server_url: str, key: str, state: dict[str, bool]) -> None:
    while True:
        state["value"] = await _poll_opt_in(server_url, key)
        await asyncio.sleep(OPT_IN_POLL_SECONDS)


def _cancel_pending_task(task: asyncio.Task | None) -> None:
    if task is not None and not task.done():
        task.cancel()


def _enforce_pending_cap(pending: OrderedDict[str, PendingRequest]) -> None:
    while len(pending) > PENDING_CAP:
        request_id, old_req = pending.popitem(last=False)
        _cancel_pending_task(old_req.notify_task)
        logger.debug("Evicted pending chat request: %s", request_id)


def _mark_stale_if_needed(
    last_frame_at: float, is_stale: bool, stale_logged: bool
) -> tuple[bool, bool]:
    if time.monotonic() - last_frame_at > HEARTBEAT_STALE_SECONDS and not is_stale:
        logger.warning("Chat bridge heartbeat stale")
        return True, True
    return is_stale, stale_logged


def _mark_live_frame(is_stale: bool, stale_logged: bool) -> tuple[bool, bool]:
    if is_stale:
        if stale_logged:
            logger.info("Chat bridge heartbeat recovered")
        return False, False
    return is_stale, stale_logged


async def _dispatch_event(
    payload: dict[str, Any],
    pending: OrderedDict[str, PendingRequest],
    opt_in: bool,
    is_stale: bool,
    config: Config,
) -> None:
    if payload.get("tract") != "chat":
        return

    event = payload.get("event")
    if event not in _HANDLED_EVENTS:
        return

    request_id = payload.get("request_id")
    if not request_id:
        logger.debug("Chat event missing request_id: %s", event)
        return
    request_id = str(request_id)

    if event == EVENT_SOL_CHAT_REQUEST:
        summary = str(payload.get("summary") or "")
        _write_fifo(f"sol-ping {request_id} {summary}\n")

        old_req = pending.pop(request_id, None)
        if old_req is not None:
            _cancel_pending_task(old_req.notify_task)

        if opt_in and not is_stale:
            event_index = payload.get("event_index")
            if not isinstance(event_index, int):
                event_index = None
            req = PendingRequest(
                request_id=request_id,
                summary=summary,
                chat_url=_chat_url(config.server_url, payload.get("day"), event_index),
            )
            req.notify_task = asyncio.create_task(
                _handle_one_notification(req, config.server_url, config.key)
            )
            pending[request_id] = req
            _enforce_pending_cap(pending)
        return

    if event in (
        EVENT_SOL_CHAT_REQUEST_SUPERSEDED,
        EVENT_OWNER_CHAT_OPEN,
        EVENT_OWNER_CHAT_DISMISSED,
    ):
        old_req = pending.pop(request_id, None)
        if old_req is not None:
            _cancel_pending_task(old_req.notify_task)
        _write_fifo(f"clear {request_id}\n")


async def _cancel_pending_notifications(
    pending: OrderedDict[str, PendingRequest],
) -> None:
    tasks = [req.notify_task for req in pending.values() if req.notify_task is not None]
    for task in tasks:
        _cancel_pending_task(task)
    if tasks:
        await asyncio.gather(*tasks, return_exceptions=True)
    pending.clear()


async def _await_worker(worker_task: asyncio.Task | None) -> None:
    if worker_task is None:
        return
    try:
        await asyncio.wait_for(worker_task, timeout=1)
    except asyncio.TimeoutError:
        worker_task.cancel()
        await asyncio.gather(worker_task, return_exceptions=True)


async def _sleep_reconnect(delay: int, stop_event: asyncio.Event) -> None:
    if not stop_event.is_set():
        await asyncio.sleep(delay)


async def _run_bridge_body(config: Config, stop_event: asyncio.Event) -> None:
    server_url = config.server_url.rstrip("/")
    key = config.key
    sse_url = f"{server_url}/app/observer/callosum"
    pending: OrderedDict[str, PendingRequest] = OrderedDict()
    opt_in_state = {"value": False}
    opt_in_task = asyncio.create_task(_opt_in_poll_loop(server_url, key, opt_in_state))
    reconnect_index = 0
    is_stale = False
    stale_logged = False
    worker_task: asyncio.Task | None = None
    thread_stop: threading.Event | None = None

    try:
        while not stop_event.is_set():
            queue: asyncio.Queue = asyncio.Queue()
            thread_stop = threading.Event()
            loop = asyncio.get_running_loop()
            worker_task = asyncio.create_task(
                asyncio.to_thread(_sse_worker, sse_url, key, queue, loop, thread_stop)
            )
            last_frame_at = time.monotonic()
            reconnect = False

            while not stop_event.is_set():
                try:
                    frame = await asyncio.wait_for(queue.get(), timeout=5)
                except asyncio.TimeoutError:
                    is_stale, stale_logged = _mark_stale_if_needed(
                        last_frame_at, is_stale, stale_logged
                    )
                    if worker_task.done():
                        reconnect = True
                        break
                    continue

                if frame.get("_terminal"):
                    logger.error(
                        "Chat bridge SSE authorization failed: status %s",
                        frame.get("status"),
                    )
                    thread_stop.set()
                    return

                if frame.get("_transport_error"):
                    logger.debug("Chat bridge transport error: %s", frame.get("error"))
                    reconnect = True
                    break

                last_frame_at = time.monotonic()
                reconnect_index = 0
                is_stale, stale_logged = _mark_live_frame(is_stale, stale_logged)

                if frame.get("_heartbeat"):
                    continue

                data = frame.get("data")
                if not isinstance(data, str):
                    continue
                try:
                    payload = json.loads(data)
                except json.JSONDecodeError as e:
                    logger.debug("Chat bridge frame JSON decode failed: %s", e)
                    continue
                if not isinstance(payload, dict):
                    continue
                await _dispatch_event(
                    payload,
                    pending,
                    opt_in_state["value"],
                    is_stale,
                    config,
                )

            if thread_stop:
                thread_stop.set()
            await _await_worker(worker_task)
            worker_task = None
            if stop_event.is_set():
                break
            if reconnect:
                delay = RECONNECT_DELAYS[
                    min(reconnect_index, len(RECONNECT_DELAYS) - 1)
                ]
                reconnect_index += 1
                logger.info("Chat bridge reconnecting in %ss", delay)
                await _sleep_reconnect(delay, stop_event)
    finally:
        if thread_stop:
            thread_stop.set()
        opt_in_task.cancel()
        await asyncio.gather(opt_in_task, return_exceptions=True)
        await _cancel_pending_notifications(pending)
        await _await_worker(worker_task)


async def run_chat_bridge(config: Config, stop_event: asyncio.Event) -> None:
    if not config.chat_bridge_enabled:
        return
    if not config.server_url or not config.key:
        logger.debug("Chat bridge disabled: server_url or key missing")
        return

    supervise_index = 0
    while not stop_event.is_set():
        started = time.monotonic()
        try:
            await _run_bridge_body(config, stop_event)
            return
        except Exception as e:
            logger.error("Chat bridge crashed: %s", e, exc_info=True)
        if stop_event.is_set():
            break
        ran_for = time.monotonic() - started
        if ran_for >= HEALTHY_RUN_SECONDS:
            supervise_index = 0
        delay = RECONNECT_DELAYS[min(supervise_index, len(RECONNECT_DELAYS) - 1)]
        supervise_index += 1
        logger.info("Chat bridge restarting in %ss", delay)
        await _sleep_reconnect(delay, stop_event)
