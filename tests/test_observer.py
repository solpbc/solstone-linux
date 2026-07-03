# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for the observer module — segment lifecycle and local cache."""

import logging
import threading
import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import numpy as np
import pytest
from dbus_next.constants import RequestNameReply

from solstone_linux.config import Config
from solstone_linux.observer import MODE_IDLE, Observer, async_run
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


class TestAudioCharacterization:
    def test_save_audio_segment_muted_writes_split_files(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.audio_recorder = MagicMock()
        observer.audio_recorder.create_mono_flac_bytes.side_effect = [
            b"mic-bytes",
            b"sys-bytes",
        ]
        observer.accumulated_audio_buffer = np.array(
            [[0.1, 0.2], [0.3, 0.4]], dtype=np.float32
        )
        segment_dir = tmp_path / "segment.incomplete"
        segment_dir.mkdir()

        files = observer._save_audio_segment(segment_dir, is_muted=True)

        assert files == ["mic_audio.flac", "sys_audio.flac"]
        assert (segment_dir / "mic_audio.flac").read_bytes() == b"mic-bytes"
        assert (segment_dir / "sys_audio.flac").read_bytes() == b"sys-bytes"
        mic_arg = observer.audio_recorder.create_mono_flac_bytes.call_args_list[0].args[
            0
        ]
        sys_arg = observer.audio_recorder.create_mono_flac_bytes.call_args_list[1].args[
            0
        ]
        np.testing.assert_allclose(mic_arg, np.array([0.1, 0.3], dtype=np.float32))
        np.testing.assert_allclose(sys_arg, np.array([0.2, 0.4], dtype=np.float32))

    def test_save_audio_segment_unmuted_writes_combined(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.audio_recorder = MagicMock()
        observer.audio_recorder.create_flac_bytes.return_value = b"combined-bytes"
        observer.accumulated_audio_buffer = np.array(
            [[0.1, 0.2], [0.3, 0.4]], dtype=np.float32
        )
        segment_dir = tmp_path / "segment.incomplete"
        segment_dir.mkdir()

        files = observer._save_audio_segment(segment_dir, is_muted=False)

        assert files == ["audio.flac"]
        assert (segment_dir / "audio.flac").read_bytes() == b"combined-bytes"
        arg = observer.audio_recorder.create_flac_bytes.call_args.args[0]
        np.testing.assert_allclose(arg, observer.accumulated_audio_buffer)

    def test_compute_rms_mic_left_sys_right(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        mic_only = np.array([[3.0, 0.0], [4.0, 0.0]], dtype=np.float32)
        sys_only = np.array([[0.0, 6.0], [0.0, 8.0]], dtype=np.float32)

        assert observer.compute_rms(mic_only) == pytest.approx(np.sqrt(12.5))
        assert observer.compute_rms(sys_only) == pytest.approx(np.sqrt(50.0))

    def test_emit_status_audio_available_reflects_recorder(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer._client = MagicMock()
        observer._client.is_registered = False
        observer.threshold_hits = 2

        observer.emit_status()
        status = observer._client.enqueue_status.call_args.args[0]
        assert status["audio"]["available"] is True
        assert status["audio"]["threshold_hits"] == 2
        assert status["audio"]["will_save"] is False

        observer.audio_recorder._set_audio_available(False)
        observer.emit_status()
        status = observer._client.enqueue_status.call_args.args[0]
        assert status["audio"]["available"] is False
        assert status["audio"]["threshold_hits"] == 2
        assert status["audio"]["will_save"] is False

    @pytest.mark.asyncio
    async def test_degraded_segment_finalizes_with_video_only(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.current_mode = MODE_IDLE
        observer.threshold_hits = 0
        observer.start_at = 100.0
        seg_dir = config.captures_dir / "19700101" / config.stream / "000140.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"video")
        observer.segment_dir = seg_dir

        with patch("solstone_linux.observer.time.time", return_value=105.0):
            await observer.handle_boundary(MODE_IDLE)

        final_dirs = [
            path
            for path in seg_dir.parent.iterdir()
            if path.is_dir() and not path.name.endswith(".incomplete")
        ]
        assert len(final_dirs) == 1
        final_dir = final_dirs[0]
        assert final_dir.exists()
        assert (final_dir / "screen.webm").read_bytes() == b"video"
        assert not (final_dir / "audio.flac").exists()
        assert not (final_dir / "mic_audio.flac").exists()
        assert not (final_dir / "sys_audio.flac").exists()

    def test_hanging_redetect_does_not_block_tick(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer._client = MagicMock()
        observer._client.is_registered = False
        entered = threading.Event()
        release = threading.Event()

        def blocking_detect():
            entered.set()
            release.wait(timeout=2.0)
            return None, None

        observer.audio_recorder._set_audio_available(False)
        with patch(
            "solstone_linux.audio_detect.input_detect", side_effect=blocking_detect
        ):
            observer.audio_recorder.start_recording()
            try:
                assert entered.wait(timeout=0.5)
                started = time.monotonic()
                observer.emit_status()
                elapsed = time.monotonic() - started
                status = observer._client.enqueue_status.call_args.args[0]
                assert elapsed < 0.2
                assert status["audio"]["available"] is False
            finally:
                release.set()
                observer.audio_recorder.stop_recording()


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
    async def test_async_run_returns_0_when_audio_degraded_no_fatal(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        observer = _fake_async_run_observer(config)
        observer.audio_recorder.audio_available = False
        observer.audio_recorder.fatal_error = None

        with (
            patch("solstone_linux.session_env.check_session_ready", return_value=None),
            patch("solstone_linux.observer.Observer", return_value=observer),
            patch(
                "solstone_linux.observer.recover_incomplete_segments", return_value=0
            ),
        ):
            result = await async_run(config)

        assert result == 0

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

    @pytest.mark.asyncio
    async def test_setup_starts_recording_when_detect_fails(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.audio_recorder = MagicMock()
        observer.audio_recorder.detect.return_value = False
        observer.screencaster.connect = AsyncMock(return_value=True)
        bus_mock = MagicMock()
        bus_mock.request_name = AsyncMock(return_value=RequestNameReply.PRIMARY_OWNER)
        bus_connection = MagicMock()
        bus_connection.connect = AsyncMock(return_value=bus_mock)

        with (
            patch("solstone_linux.observer.MessageBus", return_value=bus_connection),
            patch("solstone_linux.observer.probe_activity_services", AsyncMock()),
            patch("solstone_linux.observer.UploadClient"),
            patch("solstone_linux.observer.SyncService"),
            patch("solstone_linux.tray.TrayApp") as tray_cls,
        ):
            tray_cls.return_value.start = AsyncMock(return_value=False)
            result = await observer.setup()

        assert result is True
        observer.audio_recorder.detect.assert_called_once()
        observer.audio_recorder.start_recording.assert_called_once()
        observer.screencaster.connect.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_setup_returns_false_when_screencast_connect_fails(
        self, tmp_path: Path
    ):
        config = Config(base_dir=tmp_path)
        observer = Observer(config)
        observer.audio_recorder = MagicMock()
        observer.audio_recorder.detect.return_value = True
        observer.screencaster.connect = AsyncMock(return_value=False)
        bus_mock = MagicMock()
        bus_mock.request_name = AsyncMock(return_value=RequestNameReply.PRIMARY_OWNER)
        bus_connection = MagicMock()
        bus_connection.connect = AsyncMock(return_value=bus_mock)

        with (
            patch("solstone_linux.observer.MessageBus", return_value=bus_connection),
            patch("solstone_linux.observer.probe_activity_services", AsyncMock()),
            patch("solstone_linux.observer.UploadClient") as upload_client_cls,
        ):
            result = await observer.setup()

        assert result is False
        observer.audio_recorder.detect.assert_called_once()
        observer.audio_recorder.start_recording.assert_called_once()
        observer.screencaster.connect.assert_awaited_once()
        upload_client_cls.assert_not_called()
