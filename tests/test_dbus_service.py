# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import time
from datetime import datetime, timedelta
from pathlib import Path
from unittest.mock import MagicMock

from dbus_next.service import ServiceInterface

from solstone_linux.capture_stats import compute_capture_stats
from solstone_linux.config import Config
from solstone_linux.dbus_service import ObserverService
from solstone_linux.sync import SyncService
from solstone_linux.sync_health import SyncFacts
from solstone_linux.upload import UploadClient


def _get_prop(service, name):
    for prop in ServiceInterface._get_properties(service):
        if prop.name == name:
            return prop.prop_getter(service)
    raise KeyError(name)


def _call_method(service, name, *args):
    for method in ServiceInterface._get_methods(service):
        if method.name == name:
            return method.fn(service, *args)
    raise KeyError(name)


def _make_observer(captures_dir: Path | None = None):
    observer = MagicMock()
    observer._paused = False
    observer._pause_until = 0.0
    observer.current_mode = "screencast"
    observer.config = MagicMock()
    observer.config.captures_dir = captures_dir or Path("/tmp/test-captures")
    observer.config.server_url = "https://test.example.com"
    observer.interval = 300
    observer.segment_dir = None
    observer.start_at_mono = time.monotonic()
    observer._start_mono = time.monotonic()
    observer.stream = "test-stream"
    observer._sync = None
    observer._dbus_service = None
    observer.capture_stats = {"captures_today": 0, "total_size_mb": 0}
    return observer


class TestObserverServiceStatus:
    def test_status_recording(self):
        observer = _make_observer()
        observer.current_mode = "screencast"

        service = ObserverService(observer)

        assert _get_prop(service, "Status") == "recording"

    def test_status_idle(self):
        observer = _make_observer()
        observer.current_mode = "idle"

        service = ObserverService(observer)

        assert _get_prop(service, "Status") == "idle"

    def test_status_paused(self):
        observer = _make_observer()
        observer._paused = True

        service = ObserverService(observer)

        assert _get_prop(service, "Status") == "paused"


class TestPauseResume:
    def test_pause_calls_observer(self):
        observer = _make_observer()
        service = ObserverService(observer)

        result = _call_method(service, "Pause", 30)

        assert result == "ok"
        observer.pause.assert_called_once_with(30)

    def test_pause_indefinite_calls_observer(self):
        observer = _make_observer()
        service = ObserverService(observer)

        _call_method(service, "Pause", 0)

        observer.pause.assert_called_once_with(0)

    def test_resume_calls_observer(self):
        observer = _make_observer()
        service = ObserverService(observer)

        result = _call_method(service, "Resume")

        assert result == "ok"
        observer.resume.assert_called_once()


class TestAutoResume:
    def test_auto_resume_expiry(self):
        observer = _make_observer()
        observer._paused = True
        observer._pause_until = time.monotonic() - 1

        if (
            observer._paused
            and observer._pause_until > 0
            and time.monotonic() >= observer._pause_until
        ):
            observer._paused = False
            observer._pause_until = 0.0

        assert observer._paused is False
        assert observer._pause_until == 0.0


class TestSegmentTimerAndPauseRemaining:
    def test_segment_timer_while_recording(self):
        observer = _make_observer()
        observer.segment_dir = Path("/tmp/test.incomplete")
        observer.start_at_mono = time.monotonic() - 60
        service = ObserverService(observer)

        timer = _get_prop(service, "SegmentTimer")

        assert 238 <= timer <= 242

    def test_segment_timer_zero_when_paused(self):
        observer = _make_observer()
        observer._paused = True
        service = ObserverService(observer)

        assert _get_prop(service, "SegmentTimer") == 0

    def test_segment_timer_zero_when_no_segment(self):
        observer = _make_observer()
        observer.segment_dir = None
        service = ObserverService(observer)

        assert _get_prop(service, "SegmentTimer") == 0

    def test_pause_remaining_during_timed_pause(self):
        observer = _make_observer()
        observer._paused = True
        observer._pause_until = time.monotonic() + 120
        service = ObserverService(observer)

        remaining = _get_prop(service, "PauseRemaining")

        assert 118 <= remaining <= 122

    def test_pause_remaining_zero_when_not_paused(self):
        observer = _make_observer()
        observer._paused = False
        service = ObserverService(observer)

        assert _get_prop(service, "PauseRemaining") == 0

    def test_pause_remaining_zero_for_indefinite_pause(self):
        observer = _make_observer()
        observer._paused = True
        observer._pause_until = 0.0
        service = ObserverService(observer)

        assert _get_prop(service, "PauseRemaining") == 0


class TestComputeCaptureStats:
    def test_returns_walk_counts(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        today = datetime.now().strftime("%Y%m%d")
        yesterday = (datetime.now() - timedelta(days=1)).strftime("%Y%m%d")
        segment_dir = captures_dir / today / "stream-a" / "120000_300"
        incomplete_dir = captures_dir / today / "stream-a" / "120500.incomplete"
        failed_dir = captures_dir / today / "stream-a" / "121000_300.failed"
        old_segment = captures_dir / yesterday / "stream-a" / "130000_300"
        segment_dir.mkdir(parents=True)
        incomplete_dir.mkdir(parents=True)
        failed_dir.mkdir(parents=True)
        old_segment.mkdir(parents=True)
        (segment_dir / "audio.flac").write_bytes(b"x" * (1024 * 1024))
        (incomplete_dir / "audio.flac").write_bytes(b"x" * (1024 * 1024))
        (failed_dir / "audio.flac").write_bytes(b"x" * (1024 * 1024))
        (old_segment / "audio.flac").write_bytes(b"x")

        stats = compute_capture_stats(captures_dir, today)

        assert stats == {"captures_today": 1, "total_size_mb": 1}

    def test_empty_captures(self, tmp_path: Path):
        stats = compute_capture_stats(tmp_path / "captures", "20260101")

        assert stats == {"captures_today": 0, "total_size_mb": 0}


class TestGetStats:
    def test_returns_cached_stats_dict(self, tmp_path: Path):
        observer = _make_observer(tmp_path / "captures")
        observer.capture_stats = {"captures_today": 7, "total_size_mb": 42}
        service = ObserverService(observer)

        stats = _call_method(service, "GetStats")

        assert stats["captures_today"].value == 7
        assert stats["total_size_mb"].value == 42
        assert "synced_days" not in stats
        assert stats["uptime_seconds"].value >= 0

    def test_empty_captures(self, tmp_path: Path):
        observer = _make_observer(tmp_path / "captures")
        service = ObserverService(observer)

        stats = _call_method(service, "GetStats")

        assert stats["captures_today"].value == 0
        assert stats["total_size_mb"].value == 0
        assert "synced_days" not in stats

    def test_uses_cached_today_count(self, tmp_path: Path):
        observer = _make_observer(tmp_path / "captures")
        observer.capture_stats = {"captures_today": 1, "total_size_mb": 0}
        service = ObserverService(observer)

        stats = _call_method(service, "GetStats")

        assert stats["captures_today"].value == 1


class TestSyncStatusTracking:
    def test_initial_status(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)

        sync = SyncService(config, client)

        assert sync.health.state.value == "unknown"
        assert sync.progress == ""

    def test_progress_drives_syncing_status(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)

        sync = SyncService(config, client)
        sync._facts = SyncFacts(in_progress=True, progress="uploading 120000_300")

        assert sync.health.state.value == "syncing"
        assert sync.progress == "uploading 120000_300"

    def test_progress_change_emits_signal(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)

        sync = SyncService(config, client)
        sync._dbus_service = MagicMock()

        sync._set_progress("30s until probe")

        sync._dbus_service.SyncProgressChanged.assert_called_once_with(
            "syncing:30s until probe"
        )


class TestObserverServiceConfig:
    def test_capture_dir(self, tmp_path: Path):
        observer = _make_observer(tmp_path / "captures")
        service = ObserverService(observer)

        assert _get_prop(service, "CaptureDir") == str(observer.config.captures_dir)

    def test_server_url(self):
        observer = _make_observer()
        service = ObserverService(observer)

        assert _get_prop(service, "ServerUrl") == "https://test.example.com"

    def test_stream(self):
        observer = _make_observer()
        service = ObserverService(observer)

        assert _get_prop(service, "Stream") == "test-stream"

    def test_segment_interval(self):
        observer = _make_observer()
        service = ObserverService(observer)

        assert _get_prop(service, "SegmentInterval") == 300
