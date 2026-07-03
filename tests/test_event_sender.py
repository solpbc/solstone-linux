# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import logging
import time
from threading import Event

import solstone_linux.event_sender as event_sender
from solstone_linux.event_sender import EventSender


def _wait_for(predicate, timeout: float = 0.5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.01)
    return predicate()


def test_submit_status_is_nonblocking_while_relay_is_blocked():
    relay_started = Event()
    release_relay = Event()

    def relay(_tract, _event, **_fields):
        relay_started.set()
        release_relay.wait(timeout=1)
        return True

    sender = EventSender(relay)
    sender.submit_status({"seq": 1})
    sender.start()
    assert relay_started.wait(timeout=0.5)

    start = time.monotonic()
    sender.submit_status({"seq": 2})
    elapsed = time.monotonic() - start

    release_relay.set()
    sender.stop(0.5)
    assert elapsed < 0.1


def test_status_supersession_delivers_newest_after_blocked_relay_recovers():
    relay_started = Event()
    release_relay = Event()
    calls = []

    def relay(tract, event, **fields):
        calls.append((tract, event, fields))
        if fields["seq"] == 1:
            relay_started.set()
            release_relay.wait(timeout=1)
        return True

    sender = EventSender(relay)
    sender.submit_status({"seq": 1})
    sender.start()
    assert relay_started.wait(timeout=0.5)

    sender.submit_status({"seq": 2})
    sender.submit_status({"seq": 3})
    release_relay.set()

    assert _wait_for(lambda: len(calls) >= 2)
    sender.stop(0.5)
    assert calls[-1] == ("observe", "status", {"seq": 3})


def test_stream_silent_overflow_drop_and_bounded_stop(monkeypatch, caplog):
    delivered = []
    monkeypatch.setattr(event_sender, "SILENT_QUEUE_MAX", 1)
    sender = EventSender(
        lambda tract, event, **fields: delivered.append(fields) or True
    )

    sender.submit_stream_silent(
        {"connector": "HDMI-1", "position": "left", "node_id": 1}
    )
    with caplog.at_level(logging.WARNING):
        sender.submit_stream_silent(
            {"connector": "DP-1", "position": "right", "node_id": 2}
        )

    drop_warnings = [
        record.message
        for record in caplog.records
        if "Dropping stream_silent event" in record.message
    ]
    assert drop_warnings == [
        "Dropping stream_silent event because queue is full: "
        "connector=DP-1 position=right"
    ]

    sender.start()
    assert _wait_for(lambda: len(delivered) == 1)
    sender.stop(0.5)
    assert delivered == [{"connector": "HDMI-1", "position": "left", "node_id": 1}]

    relay_started = Event()
    release_relay = Event()

    def blocking_relay(_tract, _event, **_fields):
        relay_started.set()
        release_relay.wait(timeout=1)
        return True

    blocked_sender = EventSender(blocking_relay)
    blocked_sender.submit_stream_silent({"connector": "eDP-1", "position": "center"})
    blocked_sender.start()
    assert relay_started.wait(timeout=0.5)

    start = time.monotonic()
    with caplog.at_level(logging.WARNING):
        blocked_sender.stop(0.01)
    elapsed = time.monotonic() - start

    release_relay.set()
    blocked_sender.stop(0.5)
    assert elapsed < 0.2
    assert any("may be undelivered" in record.message for record in caplog.records)
