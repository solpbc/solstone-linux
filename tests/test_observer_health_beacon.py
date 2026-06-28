# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import time
from unittest.mock import MagicMock

from solstone_linux import __version__
from solstone_linux.config import Config
from solstone_linux.observer import MODE_SCREENCAST, Observer
from solstone_linux.screencast import StreamInfo
from solstone_linux.sync import SyncService
from solstone_linux.sync_health import ErrorType
from solstone_linux.upload import STREAM_TYPE

FIXED_EPOCH = 1_798_888_123.5
HEALTH_KEYS = {
    "name",
    "stream_type",
    "version",
    "uptime",
    "last_successful_sync",
    "pending_queue_depth",
    "recent_error_count",
    "last_error_reason",
}
BASE_STATUS_KEYS = {
    "mode",
    "screencast",
    "audio",
    "activity",
    "host",
    "platform",
}


def _observer(tmp_path, registered: bool = True) -> Observer:
    config = Config(base_dir=tmp_path)
    observer = Observer(config)
    observer._client = MagicMock()
    observer._client.is_registered = registered
    observer.stream = "desk-host"
    observer.start_at_mono = time.monotonic() - 12
    observer._sync = SyncService(config, MagicMock(), now=lambda: FIXED_EPOCH)
    return observer


def _status_kwargs(observer: Observer) -> dict:
    observer.emit_status()
    args, kwargs = observer._client.relay_event.call_args
    assert args == ("observe", "status")
    return kwargs


def test_registered_first_emit_includes_all_health_fields_top_level(tmp_path):
    observer = _observer(tmp_path)

    kwargs = _status_kwargs(observer)

    assert HEALTH_KEYS.issubset(kwargs)
    assert "health" not in kwargs
    assert kwargs["name"] == "desk-host"
    assert kwargs["stream_type"] == STREAM_TYPE
    assert kwargs["version"] == __version__
    assert isinstance(kwargs["uptime"], int)
    assert kwargs["uptime"] >= 0
    assert kwargs["last_successful_sync"] is None
    assert kwargs["pending_queue_depth"] is None
    assert kwargs["recent_error_count"] == 0
    assert kwargs["last_error_reason"] is None


def test_periodic_reemit_carries_same_health_fields(tmp_path):
    observer = _observer(tmp_path)

    first = _status_kwargs(observer)
    second = _status_kwargs(observer)

    assert HEALTH_KEYS.issubset(first)
    assert HEALTH_KEYS.issubset(second)


def test_health_fields_exclude_captured_content_and_extra_health_keys(tmp_path):
    observer = _observer(tmp_path)
    observer.current_mode = MODE_SCREENCAST
    observer.current_streams = [
        StreamInfo(
            node_id=42,
            position="left",
            connector="HDMI-SECRET",
            x=0,
            y=0,
            width=1920,
            height=1080,
            file_path="/captured/private/window-title-meeting.webm",
        )
    ]
    observer.threshold_hits = 4
    observer.cached_is_active = True
    observer.cached_screen_locked = False
    observer.cached_is_muted = True
    observer.cached_power_save = False

    kwargs = _status_kwargs(observer)

    assert set(kwargs) - BASE_STATUS_KEYS == HEALTH_KEYS
    forbidden = (
        "/captured/private",
        "window-title",
        "meeting",
        "HDMI-SECRET",
        "threshold_hits",
        "sink_muted",
    )
    health_values = [kwargs[key] for key in HEALTH_KEYS]
    for value in health_values:
        assert not any(token in str(value) for token in forbidden)


def test_successful_no_work_sync_reflected_in_health_beacon(tmp_path):
    observer = _observer(tmp_path)
    observer._sync._commit_pass_result(True)

    kwargs = _status_kwargs(observer)

    assert kwargs["last_successful_sync"] == int(FIXED_EPOCH)
    assert kwargs["pending_queue_depth"] == 0
    assert kwargs["recent_error_count"] == 0
    assert kwargs["last_error_reason"] is None


def test_failed_delivery_return_false_is_nonfatal_for_status_emit(tmp_path):
    observer = _observer(tmp_path)
    observer._client.relay_event.return_value = False

    observer.emit_status()
    observer.emit_status()

    assert observer._client.relay_event.call_count == 2


def test_unregistered_observer_emits_base_status_without_health_fields(tmp_path):
    observer = _observer(tmp_path, registered=False)

    kwargs = _status_kwargs(observer)

    assert BASE_STATUS_KEYS.issubset(kwargs)
    assert HEALTH_KEYS.isdisjoint(kwargs)


def test_failure_count_clamps_and_last_error_reason_is_safe(tmp_path):
    config = Config(base_dir=tmp_path)
    sync = SyncService(config, MagicMock(), now=lambda: FIXED_EPOCH)

    for _ in range(150):
        sync._record_failure(ErrorType.TRANSIENT, 503)

    fields = sync.health_beacon_fields()
    assert fields["recent_error_count"] == 99
    assert fields["last_error_reason"] == "transient:503"
