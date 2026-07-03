# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for the observer module — segment lifecycle and local cache."""

import logging
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from dbus_next.constants import RequestNameReply

from solstone_linux.config import Config
from solstone_linux.observer import Observer, async_run
from solstone_linux.recovery import write_segment_metadata


def _fake_async_run_observer(config: Config) -> MagicMock:
    observer = MagicMock()
    observer.config = config
    observer.running = True
    observer.setup = AsyncMock(return_value=True)
    observer.main_loop = AsyncMock(return_value=None)
    observer.audio_recorder = MagicMock()
    observer.audio_recorder.fatal_error = None
    return observer


class TestSegmentMetadata:
    """Test .metadata file creation for recovery."""

    def test_writes_metadata(self, tmp_path: Path):
        import json

        seg_dir = tmp_path / "test.incomplete"
        seg_dir.mkdir()
        write_segment_metadata(seg_dir, 1712160000.0)

        meta_path = seg_dir / ".metadata"
        assert meta_path.exists()

        data = json.loads(meta_path.read_text())
        assert data["start_timestamp"] == 1712160000.0


class TestSegmentDirStructure:
    """Test that config directories follow the expected structure."""

    def test_captures_dir_path(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        assert str(config.captures_dir).endswith("captures")

    def test_restore_token_path(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        assert config.restore_token_path == config.config_dir / "restore_token"
        assert str(config.restore_token_path).endswith("restore_token")


class TestFinalizeSegment:
    def test_finalize_segment_clamps_duration_to_interval(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.segment_interval = 5
        observer = Observer(config)
        seg_dir = tmp_path / "captures" / "20260101" / "archon" / "120000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "audio.flac").write_bytes(b"audio")
        observer.segment_dir = seg_dir
        observer.start_at = 100.0

        with patch("solstone_linux.observer.time.time", return_value=200.0):
            segment_key = observer._finalize_segment()

        assert segment_key is not None
        assert segment_key.endswith("_5")

    def test_finalize_segment_floor_is_one(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        seg_dir = tmp_path / "captures" / "20260101" / "archon" / "120000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "audio.flac").write_bytes(b"audio")
        observer.segment_dir = seg_dir
        observer.start_at = 200.0

        with patch("solstone_linux.observer.time.time", return_value=199.0):
            segment_key = observer._finalize_segment()

        assert segment_key is not None
        assert segment_key.endswith("_1")


class TestPauseResumeState:
    def test_observer_init_not_paused(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)

        observer = Observer(config)

        assert observer._paused is False
        assert observer._pause_until == 0.0

    def test_pause_state_fields_exist(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)

        observer = Observer(config)

        assert hasattr(observer, "_paused")
        assert hasattr(observer, "_pause_until")

    def test_pause_refreshes_tray(self, tmp_path: Path):
        from unittest.mock import MagicMock

        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer._tray = MagicMock()

        observer.pause(900)

        assert observer._tray.update.called is True

    def test_resume_refreshes_tray(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer._tray = MagicMock()

        observer.resume()

        assert observer._tray.update.called is True


class TestStartPaused:
    @pytest.mark.asyncio
    async def test_start_paused_true_skips_initial_capture(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.start_paused = True
        config.chat_bridge_enabled = False
        observer = Observer(config)
        observer._sync = None

        capture_calls = []

        async def mock_check_activity():
            return "screencast"

        async def mock_initialize():
            capture_calls.append("initialize")

        async def mock_sleep(_duration):
            observer.running = False

        with (
            patch.object(observer, "check_activity_status", mock_check_activity),
            patch.object(observer, "initialize_screencast", mock_initialize),
            patch.object(
                observer, "_start_segment", lambda: capture_calls.append("segment")
            ),
            patch.object(observer, "emit_status"),
            patch.object(observer, "_refresh_tray"),
            patch.object(observer, "shutdown", AsyncMock()),
            patch("solstone_linux.observer.asyncio.sleep", mock_sleep),
        ):
            await observer.main_loop()

        assert observer._paused is True
        assert capture_calls == []

    @pytest.mark.asyncio
    async def test_start_paused_false_starts_capture(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.start_paused = False
        config.chat_bridge_enabled = False
        observer = Observer(config)
        observer._sync = None

        capture_calls = []

        async def mock_check_activity():
            return "idle"

        async def mock_sleep(_duration):
            observer.running = False

        with (
            patch.object(observer, "check_activity_status", mock_check_activity),
            patch.object(
                observer, "_start_segment", lambda: capture_calls.append("segment")
            ),
            patch.object(observer, "emit_status"),
            patch.object(observer, "_refresh_tray"),
            patch.object(observer, "shutdown", AsyncMock()),
            patch("solstone_linux.observer.asyncio.sleep", mock_sleep),
        ):
            await observer.main_loop()

        assert observer._paused is False
        assert "segment" in capture_calls


class TestServiceLifecycle:
    @pytest.mark.asyncio
    async def test_async_run_returns_1_when_main_loop_runtime_error(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        observer = _fake_async_run_observer(config)
        observer.main_loop.side_effect = RuntimeError("boom")

        with (
            patch("solstone_linux.session_env.check_session_ready", return_value=None),
            patch("solstone_linux.observer.Observer", return_value=observer),
            patch(
                "solstone_linux.observer.recover_incomplete_segments", return_value=0
            ),
        ):
            result = await async_run(config)

        assert result == 1

    @pytest.mark.asyncio
    async def test_async_run_returns_1_when_audio_recorder_has_fatal_error(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        observer = _fake_async_run_observer(config)

        async def mark_fatal_error():
            observer.audio_recorder.fatal_error = "Fatal audio format error"

        observer.main_loop.side_effect = mark_fatal_error

        with (
            patch("solstone_linux.session_env.check_session_ready", return_value=None),
            patch("solstone_linux.observer.Observer", return_value=observer),
            patch(
                "solstone_linux.observer.recover_incomplete_segments", return_value=0
            ),
        ):
            result = await async_run(config)

        assert result == 1

    @pytest.mark.asyncio
    async def test_async_run_returns_0_on_normal_main_loop_return(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = _fake_async_run_observer(config)

        with (
            patch("solstone_linux.session_env.check_session_ready", return_value=None),
            patch("solstone_linux.observer.Observer", return_value=observer),
            patch(
                "solstone_linux.observer.recover_incomplete_segments", return_value=0
            ) as recover_mock,
        ):
            result = await async_run(config)

        assert result == 0
        recover_mock.assert_called_once_with(
            config.captures_dir, config.segment_interval
        )

    @pytest.mark.asyncio
    async def test_async_run_returns_75_when_session_not_ready(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)

        with (
            patch(
                "solstone_linux.session_env.check_session_ready",
                return_value="missing display",
            ),
            patch("solstone_linux.observer.Observer") as observer_cls,
            patch(
                "solstone_linux.observer.recover_incomplete_segments"
            ) as recover_mock,
        ):
            result = await async_run(config)

        assert result == 75
        observer_cls.assert_not_called()
        recover_mock.assert_not_called()

    @pytest.mark.asyncio
    async def test_async_run_returns_1_and_skips_recovery_when_setup_fails(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        observer = _fake_async_run_observer(config)
        observer.setup.return_value = False

        with (
            patch("solstone_linux.session_env.check_session_ready", return_value=None),
            patch("solstone_linux.observer.Observer", return_value=observer),
            patch(
                "solstone_linux.observer.recover_incomplete_segments"
            ) as recover_mock,
        ):
            result = await async_run(config)

        assert result == 1
        recover_mock.assert_not_called()

    @pytest.mark.asyncio
    async def test_initial_screencast_failure_runs_shutdown_and_propagates(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        config.chat_bridge_enabled = False
        observer = Observer(config)
        observer._sync = None
        observer.audio_recorder = MagicMock()
        observer.screencaster.stop = AsyncMock(return_value=([], []))

        async def mock_check_activity():
            return "screencast"

        with (
            patch.object(observer, "check_activity_status", mock_check_activity),
            patch.object(
                observer,
                "initialize_screencast",
                AsyncMock(side_effect=RuntimeError("portal failed")),
            ),
            patch("solstone_linux.observer.asyncio.sleep", AsyncMock()),
        ):
            with pytest.raises(RuntimeError):
                await observer.main_loop()

        observer.audio_recorder.stop_recording.assert_called_once()

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        "reply",
        [RequestNameReply.IN_QUEUE, RequestNameReply.EXISTS],
    )
    async def test_setup_returns_false_when_dbus_name_taken(
        self,
        tmp_path: Path,
        caplog,
        reply: RequestNameReply,
    ):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.audio_recorder = MagicMock()
        bus_mock = MagicMock()
        bus_mock.request_name = AsyncMock(return_value=reply)
        bus_mock.export = MagicMock()
        bus_connection = MagicMock()
        bus_connection.connect = AsyncMock(return_value=bus_mock)

        with (
            caplog.at_level(logging.ERROR),
            patch("solstone_linux.observer.MessageBus", return_value=bus_connection),
            patch("solstone_linux.observer.UploadClient") as upload_client_cls,
        ):
            result = await observer.setup()

        assert result is False
        observer.audio_recorder.detect.assert_not_called()
        observer.audio_recorder.start_recording.assert_not_called()
        upload_client_cls.assert_not_called()
        bus_mock.export.assert_not_called()
        assert any(
            "Another solstone-linux observer is already running" in record.message
            for record in caplog.records
        )
