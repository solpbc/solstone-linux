# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import asyncio
import errno
import logging
import os
import threading
from collections import OrderedDict
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest

from solstone_linux import chat_bridge
from solstone_linux.chat_bridge import (
    EVENT_OWNER_CHAT_DISMISSED,
    EVENT_OWNER_CHAT_OPEN,
    EVENT_SOL_CHAT_REQUEST,
    EVENT_SOL_CHAT_REQUEST_SUPERSEDED,
    HEALTHY_RUN_SECONDS,
    HEARTBEAT_STALE_SECONDS,
    NOTIFY_ACTION_KEY,
    PENDING_CAP,
    SSE_CONNECT_TIMEOUT_SECONDS,
    SSE_READ_TIMEOUT_SECONDS,
    PendingRequest,
    _chat_url,
    _dispatch_event,
    _handle_one_notification,
    _mark_live_frame,
    _mark_stale_if_needed,
    _sse_worker,
    _SseParser,
    _write_fifo,
    run_chat_bridge,
)
from solstone_linux.config import Config


class FakeResponse:
    def __init__(self, status_code=200, data=None, lines=None):
        self.status_code = status_code
        self._data = data if data is not None else {}
        self._lines = lines if lines is not None else []

    def json(self):
        return self._data

    def iter_lines(self, decode_unicode=True):
        yield from self._lines


class FakeProc:
    def __init__(self, returncode=0, stdout=b""):
        self.returncode = returncode
        self._stdout = stdout
        self.terminated = False
        self.killed = False

    async def communicate(self):
        return self._stdout, b""

    async def wait(self):
        return self.returncode

    def terminate(self):
        self.terminated = True

    def kill(self):
        self.killed = True


def _config(enabled=True) -> Config:
    config = Config()
    config.server_url = "https://server.test"
    config.key = "key-123"
    config.chat_bridge_enabled = enabled
    return config


def _payload(event=EVENT_SOL_CHAT_REQUEST, request_id="req-1", **extra):
    payload = {
        "tract": "chat",
        "event": event,
        "request_id": request_id,
        "summary": "hello",
        "day": "20260509",
        "event_index": 7,
    }
    payload.update(extra)
    return payload


async def _never_notify(req, server_url, key):
    await asyncio.Event().wait()


async def _never_poll(server_url, key, state):
    await asyncio.Event().wait()


def _terminal_worker(status):
    def worker(url, key, queue, loop, stop_event):
        loop.call_soon_threadsafe(
            queue.put_nowait, {"_terminal": True, "status": status}
        )

    return worker


def _transport_worker(url, key, queue, loop, thread_stop):
    loop.call_soon_threadsafe(
        queue.put_nowait, {"_transport_error": True, "error": "boom"}
    )


def test_sse_parser_data_only_frame():
    parser = _SseParser()

    assert parser.feed_line("data: hello") is None
    assert parser.feed_line("") == {"event": None, "data": "hello", "id": None}


def test_sse_parser_event_and_data_frame():
    parser = _SseParser()

    parser.feed_line("event: message")
    parser.feed_line("id: 42")
    parser.feed_line("data: hello")

    assert parser.feed_line("") == {"event": "message", "data": "hello", "id": "42"}


def test_sse_parser_multiline_data():
    parser = _SseParser()

    parser.feed_line("data: hello")
    parser.feed_line("data: world")

    assert parser.feed_line("")["data"] == "hello\nworld"


def test_sse_parser_ignores_comment():
    parser = _SseParser()

    assert parser.feed_line(": heartbeat") is None
    parser.feed_line("data: after")

    assert parser.feed_line("")["data"] == "after"


def test_sse_parser_partial_frame_without_terminator_returns_none():
    parser = _SseParser()

    assert parser.feed_line("event: message") is None
    assert parser.feed_line("data: partial") is None


@pytest.mark.asyncio
async def test_dispatch_drops_non_chat_tract():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        await _dispatch_event(
            {"tract": "other", "event": EVENT_SOL_CHAT_REQUEST},
            pending,
            True,
            False,
            _config(),
        )

    write_fifo.assert_not_called()
    assert not pending


@pytest.mark.asyncio
async def test_dispatch_drops_unrecognized_chat_event():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        await _dispatch_event(
            {"tract": "chat", "event": "unknown", "request_id": "req-1"},
            pending,
            True,
            False,
            _config(),
        )

    write_fifo.assert_not_called()
    assert not pending


@pytest.mark.asyncio
async def test_dispatch_recognized_events():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        await _dispatch_event(_payload(), pending, False, False, _config())
        await _dispatch_event(
            _payload(EVENT_SOL_CHAT_REQUEST_SUPERSEDED),
            pending,
            False,
            False,
            _config(),
        )
        await _dispatch_event(
            _payload(EVENT_OWNER_CHAT_OPEN), pending, False, False, _config()
        )
        await _dispatch_event(
            _payload(EVENT_OWNER_CHAT_DISMISSED), pending, False, False, _config()
        )

    assert write_fifo.call_count == 4


@pytest.mark.asyncio
async def test_request_opt_in_off_writes_fifo_without_notify():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        with patch("solstone_linux.chat_bridge._handle_one_notification") as notify:
            await _dispatch_event(_payload(), pending, False, False, _config())

    write_fifo.assert_called_once_with("sol-ping req-1 hello\n")
    notify.assert_not_called()
    assert not pending


def test_request_fifo_absent_no_error(tmp_path: Path):
    _write_fifo("sol-ping req hello\n", tmp_path / "missing")


@pytest.mark.asyncio
async def test_request_opt_in_on_not_stale_fires_notify():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo"):
        with patch(
            "solstone_linux.chat_bridge._handle_one_notification", new=_never_notify
        ):
            await _dispatch_event(_payload(), pending, True, False, _config())

    assert list(pending) == ["req-1"]
    assert pending["req-1"].notify_task is not None
    await chat_bridge._cancel_pending_notifications(pending)


@pytest.mark.asyncio
async def test_request_stale_skips_notify_but_writes_fifo():
    pending = OrderedDict()

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        with patch("solstone_linux.chat_bridge._handle_one_notification") as notify:
            await _dispatch_event(_payload(), pending, True, True, _config())

    write_fifo.assert_called_once()
    notify.assert_not_called()
    assert not pending


async def _assert_clear_event_cancels(event):
    pending = OrderedDict()
    task = asyncio.create_task(asyncio.Event().wait())
    pending["req-1"] = PendingRequest("req-1", "hello", "https://server.test", task)

    with patch("solstone_linux.chat_bridge._write_fifo") as write_fifo:
        await _dispatch_event(_payload(event), pending, True, False, _config())

    write_fifo.assert_called_once_with("clear req-1\n")
    assert not pending
    result = await asyncio.gather(task, return_exceptions=True)
    assert isinstance(result[0], asyncio.CancelledError)


@pytest.mark.asyncio
async def test_superseded_removes_pending_writes_clear_and_cancels_task():
    await _assert_clear_event_cancels(EVENT_SOL_CHAT_REQUEST_SUPERSEDED)


@pytest.mark.asyncio
async def test_owner_chat_open_removes_pending_writes_clear_and_cancels_task():
    await _assert_clear_event_cancels(EVENT_OWNER_CHAT_OPEN)


@pytest.mark.asyncio
async def test_owner_chat_dismissed_removes_pending_writes_clear_and_cancels_task():
    await _assert_clear_event_cancels(EVENT_OWNER_CHAT_DISMISSED)


def test_fifo_present_with_reader_succeeds(tmp_path: Path):
    fifo = tmp_path / "notify"
    os.mkfifo(fifo)
    reader = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
    try:
        _write_fifo("line one\n", fifo)
        assert os.read(reader, 1024) == b"line one\n"
    finally:
        os.close(reader)


def test_fifo_present_no_reader_enxio_swallowed(tmp_path: Path):
    fifo = tmp_path / "notify"
    os.mkfifo(fifo)

    _write_fifo("line one\n", fifo)


def test_fifo_missing_noop(tmp_path: Path):
    _write_fifo("line one\n", tmp_path / "missing")


def test_fifo_regular_file_noop(tmp_path: Path):
    regular = tmp_path / "notify"
    regular.write_text("")

    _write_fifo("line one\n", regular)

    assert regular.read_text() == ""


def test_fifo_eagain_swallowed(tmp_path: Path):
    fifo = tmp_path / "notify"
    os.mkfifo(fifo)

    with patch(
        "solstone_linux.chat_bridge.os.open",
        side_effect=OSError(errno.EAGAIN, "try again"),
    ):
        _write_fifo("line one\n", fifo)


def test_heartbeat_staleness_marks_stale_and_logs_once_after_60s(caplog):
    with patch("solstone_linux.chat_bridge.time.monotonic", return_value=1000):
        is_stale, stale_logged = _mark_stale_if_needed(
            1000 - HEARTBEAT_STALE_SECONDS - 1, False, False
        )
        assert is_stale
        assert stale_logged
        _mark_stale_if_needed(1000 - HEARTBEAT_STALE_SECONDS - 2, True, True)

    assert [r.message for r in caplog.records].count("Chat bridge heartbeat stale") == 1


def test_heartbeat_new_frame_recovers_from_stale(caplog):
    with caplog.at_level(logging.INFO):
        is_stale, stale_logged = _mark_live_frame(True, True)

    assert not is_stale
    assert not stale_logged
    assert "Chat bridge heartbeat recovered" in [r.message for r in caplog.records]


@pytest.mark.asyncio
async def test_sse_worker_uses_finite_read_timeout():
    queue = asyncio.Queue()
    loop = asyncio.get_running_loop()

    with patch(
        "solstone_linux.chat_bridge.requests.get",
        return_value=FakeResponse(status_code=200, lines=[]),
    ) as get:
        _sse_worker(
            "https://server.test/app/observer/callosum",
            "key-123",
            queue,
            loop,
            threading.Event(),
        )

    timeout = get.call_args.kwargs["timeout"]
    assert timeout == (SSE_CONNECT_TIMEOUT_SECONDS, SSE_READ_TIMEOUT_SECONDS)
    assert timeout != (10, None)


def test_read_timeout_exceeds_staleness_threshold():
    assert SSE_READ_TIMEOUT_SECONDS > HEARTBEAT_STALE_SECONDS


@pytest.mark.asyncio
async def test_reconnect_transport_error_backoff_sequence():
    stop_event = asyncio.Event()
    delays = []

    async def fake_sleep(delay):
        delays.append(delay)
        if len(delays) >= 7:
            stop_event.set()

    with patch("solstone_linux.chat_bridge._sse_worker", new=_transport_worker):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", side_effect=fake_sleep
            ):
                await run_chat_bridge(_config(), stop_event)

    assert delays == [1, 2, 4, 8, 16, 30, 30]


@pytest.mark.asyncio
async def test_read_timeout_reconnects_and_clears_stale():
    stop_event = asyncio.Event()
    attempts = 0
    delays = []

    def worker(url, key, queue, loop, thread_stop):
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            loop.call_soon_threadsafe(
                queue.put_nowait,
                {"_transport_error": True, "error": "read timed out"},
            )
            return
        loop.call_soon_threadsafe(queue.put_nowait, {"_heartbeat": True})
        loop.call_soon_threadsafe(
            queue.put_nowait, {"_transport_error": True, "error": "stop"}
        )

    async def fake_sleep(delay):
        delays.append(delay)
        if len(delays) >= 2:
            stop_event.set()

    with patch("solstone_linux.chat_bridge._sse_worker", new=worker):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", side_effect=fake_sleep
            ):
                await run_chat_bridge(_config(), stop_event)

    assert attempts == 2
    assert delays == [1, 1]


@pytest.mark.asyncio
async def test_reconnect_successful_frame_resets_backoff_index():
    stop_event = asyncio.Event()
    attempts = 0
    delays = []

    def worker(url, key, queue, loop, thread_stop):
        nonlocal attempts
        attempts += 1
        if attempts == 4:
            loop.call_soon_threadsafe(queue.put_nowait, {"_heartbeat": True})
        loop.call_soon_threadsafe(
            queue.put_nowait, {"_transport_error": True, "error": "boom"}
        )

    async def fake_sleep(delay):
        delays.append(delay)
        if len(delays) >= 4:
            stop_event.set()

    with patch("solstone_linux.chat_bridge._sse_worker", new=worker):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", side_effect=fake_sleep
            ):
                await run_chat_bridge(_config(), stop_event)

    assert delays == [1, 2, 4, 1]


@pytest.mark.asyncio
async def test_terminal_401_exits_without_reconnect(caplog):
    with patch("solstone_linux.chat_bridge._sse_worker", new=_terminal_worker(401)):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", new_callable=AsyncMock
            ) as sleep:
                await run_chat_bridge(_config(), asyncio.Event())

    sleep.assert_not_called()
    assert "status 401" in caplog.text


@pytest.mark.asyncio
async def test_terminal_403_exits_without_reconnect(caplog):
    with patch("solstone_linux.chat_bridge._sse_worker", new=_terminal_worker(403)):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", new_callable=AsyncMock
            ) as sleep:
                await run_chat_bridge(_config(), asyncio.Event())

    sleep.assert_not_called()
    assert "status 403" in caplog.text


@pytest.mark.asyncio
async def test_click_post_reachable_posts_then_xdg_open():
    proc = FakeProc(returncode=0, stdout=f"{NOTIFY_ACTION_KEY}\n".encode("utf-8"))
    response = FakeResponse(status_code=200)

    with patch(
        "asyncio.create_subprocess_exec", new_callable=AsyncMock, return_value=proc
    ):
        with patch(
            "solstone_linux.chat_bridge.requests.post", return_value=response
        ) as post:
            with patch("solstone_linux.chat_bridge.subprocess.Popen") as popen:
                await _handle_one_notification(
                    PendingRequest("req-1", "hello", "https://server.test/app/chat/x"),
                    "https://server.test",
                    "key-123",
                )

    post.assert_called_once_with(
        "https://server.test/api/chat/sol_chat_request/open",
        json={"request_id": "req-1"},
        headers={"Authorization": "Bearer key-123"},
        timeout=10,
    )
    popen.assert_called_once()


@pytest.mark.asyncio
async def test_click_post_unreachable_still_xdg_open():
    proc = FakeProc(returncode=0, stdout=f"{NOTIFY_ACTION_KEY}\n".encode("utf-8"))

    with patch(
        "asyncio.create_subprocess_exec", new_callable=AsyncMock, return_value=proc
    ):
        with patch(
            "solstone_linux.chat_bridge.requests.post",
            side_effect=chat_bridge.requests.RequestException("down"),
        ):
            with patch("solstone_linux.chat_bridge.subprocess.Popen") as popen:
                await _handle_one_notification(
                    PendingRequest("req-1", "hello", "https://server.test/app/chat/x"),
                    "https://server.test",
                    "key-123",
                )

    popen.assert_called_once()


@pytest.mark.asyncio
async def test_dismissal_empty_stdout_no_ack_no_open():
    proc = FakeProc(returncode=0, stdout=b"")

    with patch(
        "asyncio.create_subprocess_exec", new_callable=AsyncMock, return_value=proc
    ):
        with patch("solstone_linux.chat_bridge.requests.post") as post:
            with patch("solstone_linux.chat_bridge.subprocess.Popen") as popen:
                await _handle_one_notification(
                    PendingRequest("req-1", "hello", "https://server.test/app/chat/x"),
                    "https://server.test",
                    "key-123",
                )

    post.assert_not_called()
    popen.assert_not_called()


@pytest.mark.parametrize("stdout", [b"nope", b"op"])
@pytest.mark.asyncio
async def test_nonaction_stdout_treated_as_dismissal(stdout):
    proc = FakeProc(returncode=0, stdout=stdout)

    with patch(
        "asyncio.create_subprocess_exec", new_callable=AsyncMock, return_value=proc
    ):
        with patch("solstone_linux.chat_bridge.requests.post") as post:
            with patch("solstone_linux.chat_bridge.subprocess.Popen") as popen:
                await _handle_one_notification(
                    PendingRequest("req-1", "hello", "https://server.test/app/chat/x"),
                    "https://server.test",
                    "key-123",
                )

    post.assert_not_called()
    popen.assert_not_called()


@pytest.mark.asyncio
async def test_click_notify_nonzero_does_not_xdg_open():
    proc = FakeProc(returncode=1)

    with patch(
        "asyncio.create_subprocess_exec", new_callable=AsyncMock, return_value=proc
    ):
        with patch("solstone_linux.chat_bridge.requests.post") as post:
            with patch("solstone_linux.chat_bridge.subprocess.Popen") as popen:
                await _handle_one_notification(
                    PendingRequest("req-1", "hello", "https://server.test/app/chat/x"),
                    "https://server.test",
                    "key-123",
                )

    post.assert_not_called()
    popen.assert_not_called()


def test_chat_url_with_day_and_event_index():
    assert (
        _chat_url("https://server.test/", "20260509", 7)
        == "https://server.test/app/chat/20260509#event-7"
    )


def test_chat_url_missing_day_or_event_index_uses_today():
    with patch("solstone_linux.chat_bridge.datetime") as mock_datetime:
        mock_datetime.now.return_value.strftime.return_value = "20260509"
        assert _chat_url("https://server.test/", None, None) == (
            "https://server.test/app/chat/20260509"
        )


@pytest.mark.asyncio
async def test_bridge_crash_restarts_after_backoff(caplog):
    stop_event = asyncio.Event()
    delays = []
    worker_calls = 0

    def worker(url, key, queue, loop, thread_stop):
        nonlocal worker_calls
        worker_calls += 1
        loop.call_soon_threadsafe(
            queue.put_nowait,
            {
                "data": (
                    '{"tract": "chat", "event": "sol_chat_request", '
                    '"request_id": "req-1"}'
                )
            },
        )

    async def fake_sleep(delay):
        delays.append(delay)
        stop_event.set()

    with caplog.at_level(logging.ERROR):
        with patch("solstone_linux.chat_bridge._sse_worker", new=worker):
            with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
                with patch(
                    "solstone_linux.chat_bridge.asyncio.sleep",
                    side_effect=fake_sleep,
                ):
                    with patch(
                        "solstone_linux.chat_bridge._dispatch_event",
                        new_callable=AsyncMock,
                        side_effect=RuntimeError("boom"),
                    ):
                        await run_chat_bridge(_config(), stop_event)

    error_records = [r for r in caplog.records if r.levelno == logging.ERROR]
    assert any("Chat bridge crashed" in r.message for r in error_records)
    assert any(r.exc_info for r in error_records)
    assert delays == [1]
    assert worker_calls == 1


@pytest.mark.asyncio
async def test_supervision_backoff_climbs_then_healthy_reset():
    stop_event = asyncio.Event()
    delays = []
    durations = [1, 1, 1, 1, 1, 1, 1, HEALTHY_RUN_SECONDS]
    monotonic_values = []
    now = 1000.0

    for duration in durations:
        monotonic_values.extend([now, now + duration])
        now += duration + 1

    async def fake_sleep(delay):
        delays.append(delay)
        if len(delays) >= len(durations):
            stop_event.set()

    body = AsyncMock(side_effect=RuntimeError("boom"))

    with patch("solstone_linux.chat_bridge._run_bridge_body", body):
        with patch(
            "solstone_linux.chat_bridge.time.monotonic",
            side_effect=monotonic_values,
        ):
            with patch(
                "solstone_linux.chat_bridge.asyncio.sleep", side_effect=fake_sleep
            ):
                await run_chat_bridge(_config(), stop_event)

    assert body.await_count == len(durations)
    assert delays == [1, 2, 4, 8, 16, 30, 30, 1]


@pytest.mark.asyncio
async def test_supervision_no_task_leak_across_restarts():
    stop_event = asyncio.Event()
    delays = []
    worker_calls = 0
    opt_in_entries = 0
    opt_in_tasks = []
    opt_in_alive_before = []
    opt_in_cancelled = []
    notify_tasks = []
    notify_cancelled = []
    wait_failures = []
    condition = threading.Condition()
    real_sleep = asyncio.sleep

    async def fake_opt_in_poll_loop(server_url, key, state):
        nonlocal opt_in_entries
        task = asyncio.current_task()
        opt_in_alive_before.append(sum(1 for t in opt_in_tasks if not t.done()))
        opt_in_tasks.append(task)
        state["value"] = True
        with condition:
            opt_in_entries += 1
            condition.notify_all()
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            opt_in_cancelled.append(task)
            raise

    async def fake_notify(req, server_url, key):
        task = asyncio.current_task()
        notify_tasks.append(task)
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            notify_cancelled.append(task)
            raise

    async def crashing_dispatch(payload, pending, opt_in, is_stale, config):
        await _dispatch_event(payload, pending, opt_in, is_stale, config)
        await real_sleep(0)
        raise RuntimeError("boom")

    def worker(url, key, queue, loop, thread_stop):
        nonlocal worker_calls
        worker_calls += 1
        with condition:
            if not condition.wait_for(
                lambda: opt_in_entries >= worker_calls, timeout=1
            ):
                wait_failures.append(worker_calls)
        loop.call_soon_threadsafe(
            queue.put_nowait,
            {
                "data": (
                    '{"tract": "chat", "event": "sol_chat_request", '
                    '"request_id": "req-1", "summary": "hello"}'
                )
            },
        )

    async def fake_sleep(delay):
        delays.append(delay)
        if len(delays) >= 3:
            stop_event.set()

    with patch("solstone_linux.chat_bridge._sse_worker", new=worker):
        with patch(
            "solstone_linux.chat_bridge._opt_in_poll_loop",
            new=fake_opt_in_poll_loop,
        ):
            with patch(
                "solstone_linux.chat_bridge._handle_one_notification",
                new=fake_notify,
            ):
                with patch("solstone_linux.chat_bridge._write_fifo"):
                    with patch(
                        "solstone_linux.chat_bridge._dispatch_event",
                        new=crashing_dispatch,
                    ):
                        with patch(
                            "solstone_linux.chat_bridge.asyncio.sleep",
                            side_effect=fake_sleep,
                        ):
                            await run_chat_bridge(_config(), stop_event)

    assert wait_failures == []
    assert worker_calls == 3
    assert delays == [1, 2, 4]
    assert len(opt_in_tasks) == worker_calls
    assert opt_in_alive_before == [0, 0, 0]
    assert len(opt_in_cancelled) == worker_calls
    assert all(task.done() and task.cancelled() for task in opt_in_tasks)
    assert len(notify_tasks) == worker_calls
    assert len(notify_cancelled) == worker_calls
    assert all(task.done() and task.cancelled() for task in notify_tasks)


@pytest.mark.asyncio
async def test_stop_during_supervision_backoff_no_restart():
    stop_event = asyncio.Event()
    delays = []
    body = AsyncMock(side_effect=RuntimeError("boom"))

    async def fake_sleep(delay):
        delays.append(delay)
        stop_event.set()

    with patch("solstone_linux.chat_bridge._run_bridge_body", body):
        with patch("solstone_linux.chat_bridge.asyncio.sleep", side_effect=fake_sleep):
            await run_chat_bridge(_config(), stop_event)

    assert body.await_count == 1
    assert delays == [1]


@pytest.mark.asyncio
async def test_chat_bridge_enabled_false_no_sse_attempt():
    with patch("solstone_linux.chat_bridge.requests.get") as get:
        await run_chat_bridge(_config(enabled=False), asyncio.Event())

    get.assert_not_called()


@pytest.mark.asyncio
async def test_chat_bridge_uses_keyless_callosum_url_with_bearer():
    stop_event = asyncio.Event()
    seen = {}

    def worker(url, key, queue, loop, thread_stop):
        seen["url"] = url
        seen["key"] = key
        loop.call_soon_threadsafe(
            queue.put_nowait, {"_transport_error": True, "error": "stop"}
        )

    async def fake_sleep(_delay):
        stop_event.set()

    with patch("solstone_linux.chat_bridge._sse_worker", new=worker):
        with patch("solstone_linux.chat_bridge._opt_in_poll_loop", new=_never_poll):
            with patch("solstone_linux.chat_bridge.asyncio.sleep", new=fake_sleep):
                await run_chat_bridge(_config(), stop_event)

    assert seen == {
        "url": "https://server.test/app/observer/callosum",
        "key": "key-123",
    }


def test_observer_bridge_task_none_when_disabled():
    import inspect

    from solstone_linux.observer import Observer

    source = inspect.getsource(Observer.main_loop)
    assert "if self.config.chat_bridge_enabled:" in source
    assert "bridge_task = None" in source


@pytest.mark.asyncio
async def test_pending_cap_33rd_entry_evicts_oldest_and_cancels_task(caplog):
    pending = OrderedDict()
    tasks = []

    with caplog.at_level(logging.DEBUG):
        with patch("solstone_linux.chat_bridge._write_fifo"):
            with patch(
                "solstone_linux.chat_bridge._handle_one_notification", new=_never_notify
            ):
                for i in range(PENDING_CAP + 1):
                    await _dispatch_event(
                        _payload(request_id=f"req-{i}"),
                        pending,
                        True,
                        False,
                        _config(),
                    )
                    if i < PENDING_CAP:
                        tasks.append(pending[f"req-{i}"].notify_task)

    assert "req-0" not in pending
    assert len(pending) == PENDING_CAP
    assert "Evicted pending chat request: req-0" in caplog.text
    result = await asyncio.gather(tasks[0], return_exceptions=True)
    assert isinstance(result[0], asyncio.CancelledError)
    await chat_bridge._cancel_pending_notifications(pending)


def test_constants_forbidden_literals_appear_once_in_src_only_in_chat_bridge_module_level():
    src_dir = Path(__file__).resolve().parents[1] / "src" / "solstone_linux"
    files = list(src_dir.glob("*.py"))
    event_literals = [
        '"sol_chat_request"',
        '"sol_chat_request_superseded"',
        '"owner_chat_open"',
        '"owner_chat_dismissed"',
    ]

    for literal in event_literals:
        hits = []
        for path in files:
            for lineno, line in enumerate(path.read_text().splitlines(), 1):
                if literal in line:
                    hits.append((path.name, lineno, line.strip()))
        assert len(hits) == 1
        assert hits[0][0] == "chat_bridge.py"

    text = (src_dir / "chat_bridge.py").read_text()
    assert text.count('NOTIFY_TITLE = "sol"') == 1
    assert text.count('SURFACE = "linux"') == 1
