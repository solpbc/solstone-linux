# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import asyncio
import hashlib
import json
import os
import time
from datetime import datetime
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import numpy as np
import pytest
import soundfile as sf

from solstone_linux.config import Config
from solstone_linux.recovery import (
    _mark_failed,
    _recover_segment,
    recover_incomplete_segments,
)
from solstone_linux.sync import (
    CIRCUIT_COOLDOWN_INITIAL,
    CIRCUIT_COOLDOWN_MAX,
    SyncService,
    _segment_proven_held,
)
from solstone_linux.sync_health import (
    ErrorType,
    HealthState,
    SyncFacts,
    load_facts,
    save_facts,
)
from solstone_linux.upload import QueryResult, UploadClient, UploadResult


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _server_file(
    path: Path,
    *,
    status: str = "present",
    name: str | None = None,
    submitted_name: str | None = None,
    sha256: str | None = None,
) -> dict:
    record = {
        "name": name or path.name,
        "size": path.stat().st_size,
        "sha256": sha256 or _sha256(path),
        "status": status,
    }
    if submitted_name is not None:
        record["submitted_name"] = submitted_name
    return record


def _server_item(
    key: str,
    segment_dir: Path,
    *,
    original_key: str | None = None,
    files: list[dict] | None = None,
    status: str = "present",
) -> dict:
    item = {
        "key": key,
        "observed": False,
        "files": files
        if files is not None
        else [
            _server_file(path, status=status)
            for path in segment_dir.iterdir()
            if path.is_file() and not path.name.startswith(".")
        ],
    }
    if original_key is not None:
        item["original_key"] = original_key
    return item


class TestRecovery:
    """Test crash recovery for incomplete segments."""

    def _make_incomplete(
        self,
        captures_dir: Path,
        day: str,
        stream: str,
        time_prefix: str,
        age: int = 300,
    ) -> Path:
        """Create an incomplete segment directory with a dummy file."""
        seg_dir = captures_dir / day / stream / f"{time_prefix}.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "center_DP-3_screen.webm").write_bytes(b"\x00" * 100)

        # Set timestamps to simulate age
        old_time = time.time() - age
        os.utime(seg_dir, (old_time, old_time))
        return seg_dir

    def test_recovers_old_incomplete(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        self._make_incomplete(captures_dir, "20260403", "archon", "140000", age=300)

        recovered = recover_incomplete_segments(captures_dir)
        assert recovered == 1

        stream_dir = captures_dir / "20260403" / "archon"
        dirs = [d.name for d in stream_dir.iterdir() if d.is_dir()]
        assert len(dirs) == 1
        assert dirs[0].startswith("140000_")
        assert not dirs[0].endswith(".incomplete")

    def test_recovers_with_metadata(self, tmp_path: Path):
        """Recovery uses .metadata start_timestamp for accurate duration."""
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "center_DP-3_screen.webm").write_bytes(b"\x00" * 100)

        # Write metadata with known start timestamp (60 seconds ago)
        start_ts = time.time() - 60
        meta = {"start_timestamp": start_ts}
        (seg_dir / ".metadata").write_text(json.dumps(meta))

        # Age the directory
        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recovered = recover_incomplete_segments(captures_dir)
        assert recovered == 1

        stream_dir = captures_dir / "20260403" / "archon"
        dirs = [d.name for d in stream_dir.iterdir() if d.is_dir()]
        assert len(dirs) == 1
        # Duration should be based on metadata start timestamp, not mtime-ctime
        duration = int(dirs[0].split("_")[1])
        assert 55 <= duration <= 65  # ~60 seconds

    def test_recovery_metadata_duration_clamps_to_window_ceiling(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        (seg_dir / ".metadata").write_text(
            json.dumps({"start_timestamp": time.time() - 1000})
        )
        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recovered = recover_incomplete_segments(captures_dir, window_ceiling=60)

        assert recovered == 1
        assert (captures_dir / "20260403" / "archon" / "140000_60").exists()

    def test_recovery_filesystem_fallback_duration_clamps_to_window_ceiling(
        self, tmp_path: Path
    ):
        seg_dir = tmp_path / "captures" / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        original_stat = Path.stat

        def fake_stat(path: Path, *args, **kwargs):
            if path == seg_dir:
                return SimpleNamespace(st_mtime=1000, st_ctime=0)
            return original_stat(path, *args, **kwargs)

        with patch.object(Path, "stat", fake_stat):
            assert _recover_segment(seg_dir, window_ceiling=60) is True

        assert (tmp_path / "captures" / "20260403" / "archon" / "140000_60").exists()

    def test_recovery_bounds_duration_by_readable_flac(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        frames = np.zeros(4 * 16000, dtype=np.float32)
        sf.write(seg_dir / "audio.flac", frames, 16000, format="FLAC")
        (seg_dir / ".metadata").write_text(
            json.dumps({"start_timestamp": time.time() - 1000})
        )
        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recovered = recover_incomplete_segments(captures_dir, window_ceiling=300)

        assert recovered == 1
        assert (captures_dir / "20260403" / "archon" / "140000_4").exists()

    def test_recovery_webm_only_uses_ceiling_clamped_elapsed(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        (seg_dir / ".metadata").write_text(
            json.dumps({"start_timestamp": time.time() - 1000})
        )
        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recovered = recover_incomplete_segments(captures_dir, window_ceiling=60)

        assert recovered == 1
        assert (captures_dir / "20260403" / "archon" / "140000_60").exists()

    def test_skips_recent_incomplete(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "test.webm").write_bytes(b"\x00")

        recovered = recover_incomplete_segments(captures_dir)
        assert recovered == 0
        assert seg_dir.exists()

    def test_marks_empty_as_failed(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        # No files inside — should fail

        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recovered = recover_incomplete_segments(captures_dir)
        assert recovered == 0

        failed_dir = captures_dir / "20260403" / "archon" / "140000.failed"
        assert failed_dir.exists()

    def test_mark_failed_stamps_quarantine_mtime(self, tmp_path: Path):
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        old_time = time.time() - 100 * 86400
        os.utime(seg_dir, (old_time, old_time))

        before = time.time()
        _mark_failed(seg_dir)
        after = time.time()

        failed_dir = captures_dir / "20260403" / "archon" / "140000.failed"
        assert failed_dir.exists()
        assert before - 5 <= failed_dir.stat().st_mtime <= after + 5

    def test_metadata_removed_on_recovery(self, tmp_path: Path):
        """The .metadata file should be removed during recovery."""
        captures_dir = tmp_path / "captures"
        seg_dir = captures_dir / "20260403" / "archon" / "140000.incomplete"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00")
        (seg_dir / ".metadata").write_text('{"start_timestamp": 1000}')

        old_time = time.time() - 300
        os.utime(seg_dir, (old_time, old_time))

        recover_incomplete_segments(captures_dir)

        stream_dir = captures_dir / "20260403" / "archon"
        for d in stream_dir.iterdir():
            if d.is_dir() and not d.name.endswith((".incomplete", ".failed")):
                # .metadata should not be in the recovered dir
                assert not (d / ".metadata").exists()

    def test_no_captures_dir(self, tmp_path: Path):
        assert recover_incomplete_segments(tmp_path / "nonexistent") == 0


class TestSyncServiceCollect:
    """Test segment collection logic."""

    def test_skips_incomplete_and_failed(self, tmp_path: Path):
        from solstone_linux.sync import SyncService

        config = Config(base_dir=tmp_path)
        config.ensure_dirs()

        captures = config.captures_dir
        stream_dir = captures / "20260403" / "archon"
        stream_dir.mkdir(parents=True)

        (stream_dir / "140000_300").mkdir()
        (stream_dir / "140000_300" / "screen.webm").write_bytes(b"\x00")
        (stream_dir / "145000.incomplete").mkdir()
        (stream_dir / "143000.failed").mkdir()
        (stream_dir / "150000_300").mkdir()
        (stream_dir / "150000_300" / "audio.flac").write_bytes(b"\x00")

        client = UploadClient(config)
        sync = SyncService(config, client)

        segments = sync._collect_segments(captures)
        assert "20260403" in segments
        names = [s.name for s in segments["20260403"]]
        assert "140000_300" in names
        assert "150000_300" in names
        assert "145000.incomplete" not in names
        assert "143000.failed" not in names


class TestSyncedDaysPruning:
    """Test that synced-days cache is pruned to 90 days."""

    def test_prunes_old_entries(self, tmp_path: Path):
        from solstone_linux.sync import SyncService

        config = Config(base_dir=tmp_path)
        config.ensure_dirs()

        client = UploadClient(config)
        sync = SyncService(config, client)

        # Add entries spanning 100 days
        from datetime import datetime, timedelta

        today = datetime.now()
        for i in range(100):
            day = (today - timedelta(days=i)).strftime("%Y%m%d")
            sync._synced_days.add(day)

        sync._prune_synced_days()

        # Should have ~90 entries (not 100)
        assert len(sync._synced_days) <= 91  # Allow 1 day tolerance


class TestErrorClassification:
    """Test HTTP error classification for circuit breaker tuning."""

    def test_auth_errors(self):
        assert UploadClient.classify_error(401) == ErrorType.AUTH
        assert UploadClient.classify_error(403) == ErrorType.AUTH

    def test_client_errors(self):
        assert UploadClient.classify_error(400) == ErrorType.CLIENT

    def test_incompatible_errors(self):
        assert UploadClient.classify_error(404) == ErrorType.INCOMPATIBLE

    def test_transient_errors(self):
        assert UploadClient.classify_error(500) == ErrorType.TRANSIENT
        assert UploadClient.classify_error(502) == ErrorType.TRANSIENT
        assert UploadClient.classify_error(503) == ErrorType.TRANSIENT

    def test_network_errors(self):
        assert (
            UploadClient.classify_error(None, is_network_error=True)
            == ErrorType.TRANSIENT
        )

    def test_unknown_status(self):
        assert UploadClient.classify_error(418) == ErrorType.TRANSIENT


class TestCircuitBreakerThresholds:
    """Test circuit breaker state transitions with error-type tuning."""

    def test_auth_opens_immediately(self, tmp_path: Path):
        from solstone_linux.sync import SyncService, CIRCUIT_THRESHOLD_AUTH

        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        sync = SyncService(config, client)

        sync._last_error_type = ErrorType.AUTH
        assert sync._circuit_threshold() == CIRCUIT_THRESHOLD_AUTH
        assert CIRCUIT_THRESHOLD_AUTH == 1

    def test_transient_allows_more_failures(self, tmp_path: Path):
        from solstone_linux.sync import SyncService, CIRCUIT_THRESHOLD_TRANSIENT

        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        sync = SyncService(config, client)

        sync._last_error_type = ErrorType.TRANSIENT
        assert sync._circuit_threshold() == CIRCUIT_THRESHOLD_TRANSIENT
        assert CIRCUIT_THRESHOLD_TRANSIENT >= 5

    def test_incompatible_opens_immediately(self, tmp_path: Path):
        from solstone_linux.sync import SyncService, CIRCUIT_THRESHOLD_AUTH

        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        sync = SyncService(config, client)

        sync._last_error_type = ErrorType.INCOMPATIBLE
        assert sync._circuit_threshold() == CIRCUIT_THRESHOLD_AUTH


class TestCircuitBreakerRecovery:
    """Test circuit breaker recovery for transient failures."""

    def _make_sync(self, tmp_path: Path) -> SyncService:
        """Create a SyncService with minimal config."""
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    async def _run_briefly(self, sync: SyncService) -> None:
        sync._trigger.set()
        task = asyncio.create_task(sync.run())
        await asyncio.sleep(0.01)
        sync.stop()
        await asyncio.wait_for(task, timeout=1)

    @pytest.mark.asyncio
    async def test_transient_circuit_recovers_after_cooldown(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = False
        sync._circuit_open_since = time.monotonic() - 31
        sync._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
        sync._consecutive_failures = 5
        sync._last_error_type = ErrorType.TRANSIENT
        sync._sync = AsyncMock(side_effect=lambda force_full=False: sync.stop())
        sync._trigger.set()

        with patch(
            "asyncio.to_thread", new_callable=AsyncMock, return_value=QueryResult([])
        ):
            await sync.run()

        assert not sync._circuit_open
        assert sync._consecutive_failures == 0
        assert sync._last_error_type is None
        assert sync._circuit_cooldown == CIRCUIT_COOLDOWN_INITIAL
        sync._sync.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_revoked_circuit_never_recovers(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = True
        sync._circuit_open_since = time.monotonic() - 600
        sync._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
        sync._sync = AsyncMock()

        with patch("asyncio.to_thread", new_callable=AsyncMock) as to_thread:
            await self._run_briefly(sync)

        assert sync._circuit_open
        assert sync._circuit_open_permanent
        to_thread.assert_not_called()
        sync._sync.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_backoff_increases_on_failed_probe(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = False
        sync._circuit_open_since = 70.0
        sync._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
        sync._sync = AsyncMock()
        before_probe = time.monotonic()

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(None, ErrorType.TRANSIENT),
        ):
            await self._run_briefly(sync)

        assert sync._circuit_open
        assert sync._circuit_cooldown == CIRCUIT_COOLDOWN_INITIAL * 2
        assert sync._circuit_open_since >= before_probe
        sync._sync.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_full_reset_after_successful_probe(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = False
        sync._circuit_open_since = time.monotonic() - 121
        sync._circuit_cooldown = 120
        sync._consecutive_failures = 5
        sync._last_error_type = ErrorType.TRANSIENT
        sync._sync = AsyncMock(side_effect=lambda force_full=False: sync.stop())
        sync._trigger.set()

        with patch(
            "asyncio.to_thread", new_callable=AsyncMock, return_value=QueryResult([])
        ):
            await sync.run()

        assert not sync._circuit_open
        assert not sync._circuit_open_permanent
        assert sync._circuit_open_since == 0.0
        assert sync._circuit_cooldown == CIRCUIT_COOLDOWN_INITIAL
        assert sync._consecutive_failures == 0
        assert sync._last_error_type is None

    @pytest.mark.asyncio
    async def test_cooldown_caps_at_max(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = False
        sync._circuit_open_since = 0.0
        sync._circuit_cooldown = CIRCUIT_COOLDOWN_MAX
        sync._sync = AsyncMock()
        before_probe = time.monotonic()

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(None, ErrorType.TRANSIENT),
        ):
            await self._run_briefly(sync)

        assert sync._circuit_open
        assert sync._circuit_cooldown == CIRCUIT_COOLDOWN_MAX
        assert sync._circuit_open_since >= before_probe
        sync._sync.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_skips_probe_before_cooldown_elapses(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._circuit_open = True
        sync._circuit_open_permanent = False
        sync._circuit_open_since = time.monotonic() - 10
        sync._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
        sync._sync = AsyncMock()

        with patch("asyncio.to_thread", new_callable=AsyncMock) as to_thread:
            await self._run_briefly(sync)

        assert sync._circuit_open
        to_thread.assert_not_called()
        sync._sync.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_query_failures_recover_to_connected(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult(None, ErrorType.TRANSIENT)
        )

        for _ in range(5):
            await sync._sync()

        assert sync._circuit_open
        assert sync.health.state == HealthState.OFFLINE

        sync._circuit_open_since = time.monotonic() - 31
        sync._client.get_server_segments = MagicMock(
            side_effect=[
                QueryResult([], None, 200),
                QueryResult([], None, 200),
            ]
        )

        await self._run_briefly(sync)

        assert not sync._circuit_open
        assert sync.health.state == HealthState.CONNECTED


class TestRetryCapRespected:
    """Test that upload respects configured retry cap (no hard min(config,3))."""

    def test_respects_configured_max_retries(self):
        """Upload client should use the configured max_retries, not cap at 3."""
        config = Config()
        config.sync_max_retries = 10
        client = UploadClient(config)
        assert client._max_retries == 10

    def test_low_max_retries_respected(self):
        config = Config()
        config.sync_max_retries = 1
        client = UploadClient(config)
        assert client._max_retries == 1


class TestSyncHealthFacts:
    """Test pass-level health fact aggregation."""

    def _make_sync(self, tmp_path: Path) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        return seg_dir

    def test_startup_forces_in_progress_false(self, tmp_path: Path):
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        save_facts(
            config.state_dir,
            SyncFacts(in_progress=True, progress="uploading 120000_300"),
        )

        SyncService(config, UploadClient(config))
        facts = load_facts(config.state_dir)

        assert facts.in_progress is False
        assert facts.progress == ""

    @pytest.mark.asyncio
    async def test_today_success_and_older_404_is_update_needed(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        older_day = "20260101"
        today = datetime.now().strftime("%Y%m%d")
        self._create_segment(captures, older_day, "archon", "120000_300")

        def fake_query(day):
            if day == today:
                return QueryResult([], None, 200)
            return QueryResult(None, ErrorType.INCOMPATIBLE, 404)

        sync._client.get_server_segments = MagicMock(side_effect=fake_query)

        await sync._sync()

        assert sync.health.state == HealthState.UPDATE_NEEDED
        facts = load_facts(sync._config.state_dir)
        assert facts.last_error_class == ErrorType.INCOMPATIBLE
        assert facts.last_error_code == 404
        assert facts.pending_confirmed is None

    @pytest.mark.asyncio
    async def test_failed_query_clears_prior_pending_zero(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        sync._facts.pending_confirmed = 0
        sync._save_health()
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult(None, ErrorType.TRANSIENT)
        )

        await sync._sync()

        assert sync.health.state == HealthState.OFFLINE
        assert sync.health.pending_display == "pending unconfirmed"
        assert "pending unconfirmed" in sync.health.cli
        assert load_facts(sync._config.state_dir).pending_confirmed is None

    @pytest.mark.asyncio
    async def test_successful_cleanup_after_clean_pass_keeps_connected(
        self, tmp_path: Path
    ):
        sync = self._make_sync(tmp_path)
        older_day = "20260101"
        today = datetime.now().strftime("%Y%m%d")
        segment = self._create_segment(
            sync._config.captures_dir, older_day, "archon", "120000_300"
        )
        sync._synced_days.add(older_day)
        sync._client.get_server_segments = MagicMock(
            side_effect=lambda day: QueryResult(
                [_server_item("120000_300", segment)] if day == older_day else [],
                None,
                200,
            )
        )

        await sync._sync()

        assert sync._client.get_server_segments.call_count == 2
        sync._client.get_server_segments.assert_any_call(today)
        sync._client.get_server_segments.assert_any_call(older_day)
        assert sync.health.state == HealthState.CONNECTED
        assert load_facts(sync._config.state_dir).pending_confirmed == 0


class TestQuarantineZeroByte:
    """Test that segments with all zero-byte files are quarantined before upload."""

    def _make_sync(self, tmp_path: Path) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_zero_byte_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"")
        (seg_dir / "audio.flac").write_bytes(b"")
        return seg_dir

    @pytest.mark.asyncio
    async def test_zero_byte_segment_quarantined(self, tmp_path: Path):
        """A segment with all zero-byte files is renamed to .failed before upload."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        fake_now = datetime(2026, 4, 11, 12, 0, 0)

        seg = self._create_zero_byte_segment(
            captures, "20260410", "archon", "120000_300"
        )
        server_response = []

        with patch("solstone_linux.sync.datetime", wraps=datetime) as mock_datetime:
            mock_datetime.now.return_value = fake_now
            with patch(
                "asyncio.to_thread",
                new_callable=AsyncMock,
                return_value=QueryResult(server_response),
            ):
                await sync._sync()

        assert not seg.exists()
        assert seg.with_name("120000_300.failed").exists()

    @pytest.mark.asyncio
    async def test_zero_byte_does_not_trigger_upload(self, tmp_path: Path):
        """Zero-byte segments should never call upload_segment."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        self._create_zero_byte_segment(captures, "20260410", "archon", "120000_300")
        server_response = []

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            with patch.object(
                sync, "_upload_segment", new_callable=AsyncMock
            ) as mock_upload:
                await sync._sync()
                mock_upload.assert_not_called()

    @pytest.mark.asyncio
    async def test_mixed_files_not_quarantined(self, tmp_path: Path):
        """A segment with some zero-byte and some non-zero files is NOT quarantined."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        seg_dir = captures / "20260410" / "archon" / "120000_300"
        seg_dir.mkdir(parents=True)
        (seg_dir / "screen.webm").write_bytes(b"")
        (seg_dir / "audio.flac").write_bytes(b"\x00" * 100)

        server_response = []

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            with patch.object(
                sync, "_upload_segment", new_callable=AsyncMock, return_value=True
            ) as mock_upload:
                await sync._sync()
                mock_upload.assert_called_once()

    @pytest.mark.asyncio
    async def test_zero_byte_day_marked_synced(self, tmp_path: Path):
        """A past day with only zero-byte segments gets marked synced after quarantine."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        self._create_zero_byte_segment(captures, "20260101", "archon", "120000_300")
        server_response = []

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._sync()

        assert "20260101" in sync._synced_days


class TestQuarantineClientError:
    """Test that CLIENT errors (HTTP 400) quarantine the segment."""

    def _make_sync(self, tmp_path: Path) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        return seg_dir

    @pytest.mark.asyncio
    async def test_client_error_quarantines_segment(self, tmp_path: Path):
        """HTTP 400 response quarantines the segment to .failed."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        seg = self._create_segment(captures, "20260410", "archon", "120000_300")
        server_response = []

        async def fake_upload(day, segment_dir):
            sync._last_error_type = ErrorType.CLIENT
            return False

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            with patch.object(sync, "_upload_segment", side_effect=fake_upload):
                await sync._sync()

        assert not seg.exists()
        assert seg.with_name("120000_300.failed").exists()

    def test_quarantine_segment_stamps_quarantine_mtime(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        seg = self._create_segment(captures, "20260410", "archon", "120000_300")
        old_time = time.time() - 100 * 86400
        os.utime(seg, (old_time, old_time))

        before = time.time()
        assert sync._quarantine_segment(seg, "test") is True
        after = time.time()

        failed_dir = seg.with_name("120000_300.failed")
        assert failed_dir.exists()
        assert before - 5 <= failed_dir.stat().st_mtime <= after + 5

    @pytest.mark.asyncio
    async def test_client_error_does_not_trip_circuit(self, tmp_path: Path):
        """CLIENT errors should not increment consecutive_failures or open circuit."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        for i in range(10):
            self._create_segment(captures, "20260410", "archon", f"12000{i}_300")

        server_response = []

        async def fake_upload(day, segment_dir):
            sync._last_error_type = ErrorType.CLIENT
            return False

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            with patch.object(sync, "_upload_segment", side_effect=fake_upload):
                await sync._sync()

        assert sync._consecutive_failures == 0
        assert not sync._circuit_open

    @pytest.mark.asyncio
    async def test_transient_error_still_trips_circuit(self, tmp_path: Path):
        """TRANSIENT errors should still increment failures and trip circuit."""
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir

        for i in range(6):
            self._create_segment(captures, "20260410", "archon", f"12000{i}_300")

        server_response = []

        async def fake_upload(day, segment_dir):
            sync._last_error_type = ErrorType.TRANSIENT
            return False

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            with patch.object(sync, "_upload_segment", side_effect=fake_upload):
                await sync._sync()

        assert sync._circuit_open
        assert sync._consecutive_failures >= 5


class TestReconcilePredicate:
    def _make_sync(self, tmp_path: Path, retention: int = 7) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.cache_retention_days = retention
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"screen")
        return seg_dir

    def _query_for(self, day: str, items: list[dict], **kwargs):
        def fake_query(query_day: str):
            if query_day == day:
                return QueryResult(items, None, 200, **kwargs)
            return QueryResult([], None, 200, **kwargs)

        return fake_query

    def test_present_status_with_mismatched_sha_is_not_proof(self, tmp_path: Path):
        segment = self._create_segment(
            tmp_path / "captures", "20260101", "archon", "120000_300"
        )
        item = _server_item(
            "120000_300",
            segment,
            files=[_server_file(segment / "screen.webm", sha256="0" * 64)],
        )

        assert not _segment_proven_held(segment, item)

    @pytest.mark.asyncio
    async def test_present_status_with_mismatched_sha_uploads(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        item = _server_item(
            "120000_300",
            segment,
            files=[_server_file(segment / "screen.webm", sha256="0" * 64)],
        )
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item])
        )

        with patch.object(
            sync, "_upload_segment", new_callable=AsyncMock, return_value=True
        ) as upload:
            await sync._sync()

        upload.assert_called_once()

    def test_swapped_sha_by_filename_is_not_proof(self, tmp_path: Path):
        segment = self._create_segment(
            tmp_path / "captures", "20260101", "archon", "120000_300"
        )
        audio = segment / "audio.flac"
        audio.write_bytes(b"audio")
        item = _server_item(
            "120000_300",
            segment,
            files=[
                _server_file(segment / "screen.webm", sha256=_sha256(audio)),
                _server_file(audio, sha256=_sha256(segment / "screen.webm")),
            ],
        )

        assert not _segment_proven_held(segment, item)

    def test_relocated_matching_sha_is_held(self, tmp_path: Path):
        segment = self._create_segment(
            tmp_path / "captures", "20260101", "archon", "120000_300"
        )
        item = _server_item("120000_300", segment, status="relocated")

        assert _segment_proven_held(segment, item)

    def test_submitted_name_matches_local_filename(self, tmp_path: Path):
        segment = tmp_path / "captures" / "20260101" / "archon" / "120000_300"
        segment.mkdir(parents=True)
        local_file = segment / "120000_300_audio.flac"
        local_file.write_bytes(b"audio")
        item = _server_item(
            "120000_300",
            segment,
            files=[
                _server_file(
                    local_file,
                    name="audio.flac",
                    submitted_name="120000_300_audio.flac",
                )
            ],
        )

        assert _segment_proven_held(segment, item)

    @pytest.mark.asyncio
    async def test_missing_status_uploads_and_cleanup_keeps(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        item = _server_item("120000_300", segment, status="missing")
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item])
        )

        with patch.object(
            sync, "_upload_segment", new_callable=AsyncMock, return_value=True
        ) as upload:
            await sync._sync()

        upload.assert_called_once()
        sync._synced_days.add(day)
        await sync._cleanup_synced_segments()

        assert segment.exists()

    @pytest.mark.asyncio
    async def test_all_present_sha_match_skips_and_cleanup_deletes(
        self, tmp_path: Path
    ):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        item = _server_item("120000_300", segment)
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item])
        )

        with patch.object(sync, "_upload_segment", new_callable=AsyncMock) as upload:
            await sync._sync()

        upload.assert_not_called()
        assert not segment.exists()

    @pytest.mark.asyncio
    async def test_unreadable_sha_cleanup_keeps(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        item = _server_item("120000_300", segment)
        sync._synced_days.add(day)
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult([item], None, 200)
        )

        with patch("solstone_linux.sync._sha256_file", return_value=None):
            await sync._cleanup_synced_segments()

        assert segment.exists()

    @pytest.mark.asyncio
    async def test_truncated_envelope_uploads_and_cleanup_keeps(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        item = _server_item("120000_300", segment)
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item], truncated=True)
        )

        with patch.object(
            sync, "_upload_segment", new_callable=AsyncMock, return_value=True
        ) as upload:
            await sync._sync()

        upload.assert_called_once()
        sync._synced_days.add(day)
        await sync._cleanup_synced_segments()

        assert segment.exists()

    @pytest.mark.asyncio
    async def test_legacy_listing_logs_once_and_cleanup_keeps(
        self, tmp_path: Path, caplog
    ):
        sync = self._make_sync(tmp_path)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        failed = self._create_segment(
            sync._config.captures_dir, day, "archon", "130000_300.failed"
        )
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult([{"key": "120000_300"}], None, 200, legacy=True)
        )

        with (
            caplog.at_level("WARNING", logger="solstone_linux.sync"),
            patch.object(sync, "_upload_segment", new_callable=AsyncMock) as upload,
        ):
            await sync._sync()

        upload.assert_not_called()
        assert segment.exists()
        assert failed.exists()
        degraded = [
            rec for rec in caplog.records if "pre-v2 bare array" in rec.getMessage()
        ]
        assert len(degraded) == 1

    @pytest.mark.asyncio
    async def test_duplicate_marker_stops_reupload(self, tmp_path: Path):
        sync = self._make_sync(tmp_path, retention=-1)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult([], None, 200)
        )
        sync._client.upload_segment = MagicMock(
            return_value=UploadResult(True, duplicate=True, stored_key="existing_300")
        )

        await sync._sync()

        assert (segment / ".server_key").read_text().strip() == "existing_300"
        item = _server_item("existing_300", segment)
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item])
        )
        sync._client.upload_segment.reset_mock()

        await sync._sync()

        sync._client.upload_segment.assert_not_called()
        assert day in sync._synced_days

    @pytest.mark.asyncio
    async def test_collision_marker_and_original_key_reconcile(self, tmp_path: Path):
        sync = self._make_sync(tmp_path, retention=-1)
        day = "20260101"
        segment = self._create_segment(
            sync._config.captures_dir, day, "archon", "120000_300"
        )
        sync._client.get_server_segments = MagicMock(
            return_value=QueryResult([], None, 200)
        )
        sync._client.upload_segment = MagicMock(
            return_value=UploadResult(True, stored_key="120000_301")
        )

        await sync._sync()

        assert (segment / ".server_key").read_text().strip() == "120000_301"
        item = _server_item("120000_301", segment, original_key="120000_300")
        sync._client.get_server_segments = MagicMock(
            side_effect=self._query_for(day, [item])
        )
        sync._client.upload_segment.reset_mock()

        await sync._sync()

        sync._client.upload_segment.assert_not_called()
        assert day in sync._synced_days


class TestCleanupSyncedSegments:
    """Test cache retention cleanup of synced segments."""

    def _make_sync(self, tmp_path: Path, retention: int = 7) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.cache_retention_days = retention
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        return seg_dir

    @pytest.mark.asyncio
    async def test_deletes_old_synced_confirmed(self, tmp_path: Path):
        """Segments in synced_days + confirmed on server + old enough -> deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        segment = self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        server_response = [_server_item("120000_300", segment)]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert not (captures / "20260101" / "archon" / "120000_300").exists()

    @pytest.mark.asyncio
    async def test_keeps_unconfirmed_on_server(self, tmp_path: Path):
        """Segments in synced_days + NOT on server -> not deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        server_response = [
            _server_item("999999_300", captures / "20260101" / "archon" / "120000_300")
        ]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000_300").exists()

    @pytest.mark.asyncio
    async def test_keeps_segments_not_in_synced_days(self, tmp_path: Path):
        """Segments NOT in synced_days -> not deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000_300")

        with patch("asyncio.to_thread", new_callable=AsyncMock) as mock_thread:
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000_300").exists()
        mock_thread.assert_not_called()

    @pytest.mark.asyncio
    async def test_keeps_when_server_unreachable(self, tmp_path: Path):
        """Server unreachable (returns None) -> nothing deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(None, ErrorType.TRANSIENT),
        ):
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000_300").exists()

    @pytest.mark.asyncio
    async def test_never_touches_incomplete(self, tmp_path: Path):
        """.incomplete segments are never deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000.incomplete")
        complete = self._create_segment(captures, "20260101", "archon", "140000_300")
        sync._synced_days.add("20260101")

        server_response = [_server_item("140000_300", complete)]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000.incomplete").exists()
        assert not (captures / "20260101" / "archon" / "140000_300").exists()

    @pytest.mark.asyncio
    async def test_retention_negative_one_keeps_forever(self, tmp_path: Path):
        """cache_retention_days = -1 -> nothing deleted."""
        sync = self._make_sync(tmp_path, retention=-1)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        with patch("asyncio.to_thread", new_callable=AsyncMock) as mock_thread:
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000_300").exists()
        mock_thread.assert_not_called()

    @pytest.mark.asyncio
    async def test_retention_zero_deletes_immediately(self, tmp_path: Path):
        """cache_retention_days = 0 -> deletes immediately (no age check)."""
        sync = self._make_sync(tmp_path, retention=0)
        captures = sync._config.captures_dir

        from datetime import datetime, timedelta

        yesterday = (datetime.now() - timedelta(days=1)).strftime("%Y%m%d")

        segment = self._create_segment(captures, yesterday, "archon", "120000_300")
        sync._synced_days.add(yesterday)

        server_response = [_server_item("120000_300", segment)]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert not (captures / yesterday / "archon" / "120000_300").exists()

    @pytest.mark.asyncio
    async def test_never_cleans_today(self, tmp_path: Path):
        """Today's segments are never cleaned, even with retention=0."""
        sync = self._make_sync(tmp_path, retention=0)
        captures = sync._config.captures_dir

        from datetime import datetime

        today = datetime.now().strftime("%Y%m%d")

        self._create_segment(captures, today, "archon", "120000_300")
        sync._synced_days.add(today)

        with patch("asyncio.to_thread", new_callable=AsyncMock) as mock_thread:
            await sync._cleanup_synced_segments()

        assert (captures / today / "archon" / "120000_300").exists()
        mock_thread.assert_not_called()

    @pytest.mark.asyncio
    async def test_cleans_empty_dirs(self, tmp_path: Path):
        """Empty stream and day dirs are removed after segment deletion."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        segment = self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        server_response = [_server_item("120000_300", segment)]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert not (captures / "20260101" / "archon").exists()
        assert not (captures / "20260101").exists()

    @pytest.mark.asyncio
    async def test_original_key_lookup(self, tmp_path: Path):
        """Server segment with original_key should match local segment."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        segment = self._create_segment(captures, "20260101", "archon", "120000_300")
        sync._synced_days.add("20260101")

        server_response = [
            _server_item("renamed_key", segment, original_key="120000_300")
        ]
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert not (captures / "20260101" / "archon" / "120000_300").exists()


class TestCleanupFailedSegments:
    """Test that retention cleanup skips .failed segments."""

    def _make_sync(self, tmp_path: Path, retention: int = 7) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.cache_retention_days = retention
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        return seg_dir

    @pytest.mark.asyncio
    async def test_failed_segments_kept_if_day_not_synced(self, tmp_path: Path):
        """.failed segments are kept if the day is not in synced_days."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000_300.failed")

        with patch("asyncio.to_thread", new_callable=AsyncMock) as mock_thread:
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000_300.failed").exists()
        mock_thread.assert_not_called()

    @pytest.mark.asyncio
    async def test_failed_segments_kept_within_retention(self, tmp_path: Path):
        """.failed segments are kept when the synced day is still within retention."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir
        fake_now = datetime(2026, 1, 8, 12, 0, 0)

        seg = self._create_segment(captures, "20260107", "archon", "120000_300.failed")
        sync._synced_days.add("20260107")

        server_response = []
        with patch("solstone_linux.sync.datetime", wraps=datetime) as mock_datetime:
            mock_datetime.now.return_value = fake_now
            with patch(
                "asyncio.to_thread",
                new_callable=AsyncMock,
                return_value=QueryResult(server_response),
            ):
                await sync._cleanup_synced_segments()

        assert seg.exists()

    @pytest.mark.asyncio
    async def test_incomplete_still_skipped(self, tmp_path: Path):
        """.incomplete segments are still never deleted."""
        sync = self._make_sync(tmp_path, retention=7)
        captures = sync._config.captures_dir

        self._create_segment(captures, "20260101", "archon", "120000.incomplete")
        sync._synced_days.add("20260101")

        server_response = []
        with patch(
            "asyncio.to_thread",
            new_callable=AsyncMock,
            return_value=QueryResult(server_response),
        ):
            await sync._cleanup_synced_segments()

        assert (captures / "20260101" / "archon" / "120000.incomplete").exists()


class TestSweepExpiredQuarantine:
    """Test local TTL sweep for quarantined .failed segments."""

    def _make_sync(self, tmp_path: Path, retention: int = 7) -> SyncService:
        config = Config(base_dir=tmp_path)
        config.cache_retention_days = retention
        config.ensure_dirs()
        client = UploadClient(config)
        return SyncService(config, client)

    def _create_segment(
        self, captures_dir: Path, day: str, stream: str, name: str
    ) -> Path:
        seg_dir = captures_dir / day / stream / name
        seg_dir.mkdir(parents=True, exist_ok=True)
        (seg_dir / "screen.webm").write_bytes(b"\x00" * 100)
        return seg_dir

    def test_fresh_quarantine_survives_repeated_sweeps(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        seg = self._create_segment(captures, "20260101", "archon", "120000_300.failed")
        now = time.time()
        os.utime(seg, (now, now))

        sync._sweep_expired_quarantine()
        sync._sweep_expired_quarantine()

        assert seg.exists()

    def test_fresh_dir_mtime_beats_ancient_day_and_file_mtimes(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        seg = self._create_segment(captures, "20200101", "archon", "120000_300.failed")
        ancient = time.time() - 100 * 86400
        os.utime(seg / "screen.webm", (ancient, ancient))
        now = time.time()
        os.utime(seg, (now, now))

        sync._sweep_expired_quarantine()

        assert seg.exists()

    def test_aged_quarantine_deleted_without_server_query(self, tmp_path: Path, caplog):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        seg = self._create_segment(captures, "20200101", "archon", "120000_300.failed")
        old_time = time.time() - 40 * 86400
        os.utime(seg, (old_time, old_time))
        assert "20200101" not in sync._synced_days

        with patch("asyncio.to_thread", new_callable=AsyncMock) as mock_thread:
            with caplog.at_level("WARNING", logger="solstone_linux.sync"):
                sync._sweep_expired_quarantine()

        assert not seg.exists()
        mock_thread.assert_not_called()
        warnings = [
            rec
            for rec in caplog.records
            if rec.name == "solstone_linux.sync" and rec.levelname == "WARNING"
        ]
        assert len(warnings) == 1
        assert "120000_300.failed" in warnings[0].getMessage()

    def test_aged_quarantine_deleted_with_retention_disabled(self, tmp_path: Path):
        sync = self._make_sync(tmp_path, retention=-1)
        captures = sync._config.captures_dir
        seg = self._create_segment(captures, "20260101", "archon", "120000_300.failed")
        old_time = time.time() - 40 * 86400
        os.utime(seg, (old_time, old_time))

        sync._sweep_expired_quarantine()

        assert not seg.exists()

    def test_aged_quarantine_deletes_both_name_shapes(self, tmp_path: Path):
        sync = self._make_sync(tmp_path)
        captures = sync._config.captures_dir
        duration_shape = self._create_segment(
            captures, "20260101", "archon", "120000_300.failed"
        )
        bare_shape = self._create_segment(
            captures, "20260101", "archon", "130000.failed"
        )
        old_time = time.time() - 40 * 86400
        os.utime(duration_shape, (old_time, old_time))
        os.utime(bare_shape, (old_time, old_time))

        sync._sweep_expired_quarantine()

        assert not duration_shape.exists()
        assert not bare_shape.exists()
