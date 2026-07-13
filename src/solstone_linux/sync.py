# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Background sync service for uploading captured segments.

Modeled on solstone-macos's SyncService.swift. Runs as an asyncio
background task in the same event loop as capture. Walks cache days
newest-to-oldest, queries server for existing segments, uploads missing ones.

Refinements over tmux baseline:
- Owns long retry/backoff and the circuit breaker; per-upload immediate
  retries are bounded and interruptible in UploadClient
- Circuit breaker tuned by error type: auth=immediate, transient=5-10
- Transient circuit breaker recovers via half-open probe with exponential backoff
- Auth/revoked circuit breaker is permanent (requires restart)
- Synced-days pruning at 90 days to prevent unbounded cache growth
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import shutil
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable

from .config import Config
from .sync_health import (
    ErrorType,
    SyncFacts,
    SyncHealth,
    derive_health,
    load_facts,
    save_facts,
)
from .upload import UploadClient

logger = logging.getLogger(__name__)

# Circuit breaker thresholds by error type
CIRCUIT_THRESHOLD_AUTH = 1  # Auth failures open immediately
CIRCUIT_THRESHOLD_TRANSIENT = 5  # Transient failures need 5 consecutive

# Circuit breaker recovery cooldown
CIRCUIT_COOLDOWN_INITIAL = 30  # seconds before first probe
CIRCUIT_COOLDOWN_FACTOR = 2  # multiply cooldown on each failed probe
CIRCUIT_COOLDOWN_MAX = 300  # cap at 5 minutes

# Synced days older than this are pruned from the cache
SYNCED_DAYS_MAX_AGE = 90

# Quarantined (.failed) segments are dropped once this old (age from quarantine entry)
QUARANTINE_TTL_DAYS = 30

# Flush durable contact at most this often during long healthy drains.
CONTACT_FLUSH_INTERVAL = 30

SERVER_KEY_FILENAME = ".server_key"

# Per-file statuses that prove reconcile convergence after name and SHA match.
# "processed" means the journal intentionally consumed the raw byte after
# verified processing and deliberately does not keep that raw file on journal
# disk; it makes the segment eligible for configured local cache cleanup, but
# does not mean the raw byte is still stored.
TERMINAL_HELD_STATUSES = ("present", "relocated", "processed")


def _eligible_files(segment_dir: Path) -> list[Path]:
    """Files eligible for upload and reconcile."""
    return [
        f for f in segment_dir.iterdir() if f.is_file() and not f.name.startswith(".")
    ]


def _sha256_file(path: Path) -> str | None:
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError:
        return None
    return digest.hexdigest()


def _segment_proven_held(segment_dir: Path, entry: dict) -> bool:
    """Return True only when every local file has terminal journal proof.

    Each local file must match a listing entry by submitted_name (else name),
    carry a status in TERMINAL_HELD_STATUSES, and match its SHA-256 exactly.
    All three are required: a terminal status alone is never proof for this byte.
    Anything else -- absent entry, unknown status, hash mismatch, unreadable file
    -- needs upload and is never deletable.
    """
    local_files = _eligible_files(segment_dir)
    if not local_files:
        return False

    remote_by_name = {}
    for remote_file in entry.get("files", []):
        name = remote_file.get("submitted_name") or remote_file.get("name")
        if name:
            remote_by_name[name] = remote_file

    for local_file in local_files:
        remote_file = remote_by_name.get(local_file.name)
        if remote_file is None:
            return False
        if remote_file.get("status") not in TERMINAL_HELD_STATUSES:
            return False
        local_sha = _sha256_file(local_file)
        if local_sha is None or local_sha != remote_file.get("sha256"):
            return False

    return True


def _index_entries(items: list[dict]) -> dict[str, dict]:
    entries = {}
    for item in items:
        key = item.get("key")
        if key:
            entries[key] = item
        original_key = item.get("original_key")
        if original_key:
            entries[original_key] = item
    return entries


def _read_server_key(segment_dir: Path) -> str | None:
    try:
        key = (segment_dir / SERVER_KEY_FILENAME).read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return key or None


def _write_server_key(segment_dir: Path, key: str) -> None:
    try:
        (segment_dir / SERVER_KEY_FILENAME).write_text(f"{key}\n", encoding="utf-8")
    except OSError as e:
        logger.warning("Failed to write server key marker for %s: %s", segment_dir, e)


def _lookup_entry(entries_by_key: dict[str, dict], segment_dir: Path) -> dict | None:
    for key in (segment_dir.name, _read_server_key(segment_dir)):
        if key and key in entries_by_key:
            return entries_by_key[key]
    return None


class SyncService:
    """Background sync service that uploads completed segments to the server."""

    def __init__(
        self,
        config: Config,
        client: UploadClient,
        now: Callable[[], float] = time.time,
    ):
        self._config = config
        self._client = client
        self._now = now
        self._stale_threshold = config.sync_stale_threshold
        self._synced_days: set[str] = set()
        self._consecutive_failures = 0
        self._last_error_type: ErrorType | None = None
        self._circuit_open = False
        self._circuit_open_permanent = False
        self._circuit_open_since: float = 0.0
        self._circuit_cooldown: float = CIRCUIT_COOLDOWN_INITIAL
        self._last_full_sync: float = 0
        self._running = True
        self._trigger = asyncio.Event()
        self._dbus_service = None
        self._facts: SyncFacts = load_facts(self._config.state_dir)
        self._facts.in_progress = False
        self._facts.progress = ""
        self._last_contact_flush = 0.0
        self._last_emitted_health = ""
        self._save_health()

        # Load synced days cache
        self._load_synced_days()

    @property
    def health(self) -> SyncHealth:
        return derive_health(self._facts, self._now(), self._stale_threshold)

    @property
    def progress(self) -> str:
        return self._facts.progress

    def health_beacon_fields(self) -> dict[str, Any]:
        """Diagnostics-only sync fields: counts, epoch seconds, and error class."""
        last_successful_sync = self._facts.last_successful_sync
        last_error_class = self._facts.last_error_class
        last_error_code = self._facts.last_error_code

        if last_error_class is None:
            last_error_reason = None
        elif last_error_code is not None:
            last_error_reason = f"{last_error_class.value}:{last_error_code}"
        else:
            last_error_reason = last_error_class.value

        return {
            "last_successful_sync": int(last_successful_sync)
            if last_successful_sync is not None
            else None,
            "pending_queue_depth": self._facts.pending_confirmed,
            "recent_error_count": min(99, max(0, self._consecutive_failures)),
            "last_error_reason": last_error_reason,
        }

    def _synced_days_path(self) -> Path:
        return self._config.state_dir / "synced_days.json"

    def _load_synced_days(self) -> None:
        path = self._synced_days_path()
        if not path.exists():
            return
        try:
            with open(path, encoding="utf-8") as f:
                data = json.load(f)
            self._synced_days = set(data) if isinstance(data, list) else set()
        except (json.JSONDecodeError, OSError):
            self._synced_days = set()

    def _save_synced_days(self) -> None:
        self._config.state_dir.mkdir(parents=True, exist_ok=True)
        path = self._synced_days_path()
        tmp = path.with_suffix(f".{os.getpid()}.tmp")
        try:
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump(sorted(self._synced_days), f)
                f.write("\n")
            os.rename(str(tmp), str(path))
        except OSError as e:
            logger.warning(f"Failed to save synced days: {e}")

    def _prune_synced_days(self) -> None:
        """Remove synced-days entries older than 90 days."""
        if not self._synced_days:
            return
        cutoff = (datetime.now() - timedelta(days=SYNCED_DAYS_MAX_AGE)).strftime(
            "%Y%m%d"
        )
        before = len(self._synced_days)
        self._synced_days = {d for d in self._synced_days if d >= cutoff}
        pruned = before - len(self._synced_days)
        if pruned:
            logger.info(
                f"Pruned {pruned} synced-days entries older than {SYNCED_DAYS_MAX_AGE} days"
            )
            self._save_synced_days()

    def _quarantine_segment(self, segment_dir: Path, reason: str) -> bool:
        """Rename a segment directory to .failed so it's never retried."""
        failed_path = segment_dir.with_name(segment_dir.name + ".failed")
        try:
            segment_dir.rename(failed_path)
            try:
                os.utime(failed_path, (self._now(), self._now()))
            except OSError as e:
                logger.warning(
                    "Failed to stamp quarantine time for %s: %s", failed_path, e
                )
            logger.warning(
                "Quarantined %s/%s — %s",
                segment_dir.parent.parent.name,
                segment_dir.name,
                reason,
            )
            return True
        except OSError as e:
            logger.error("Failed to quarantine %s: %s", segment_dir, e)
            return False

    async def _cleanup_synced_segments(self) -> None:
        """Delete synced segments older than cache_retention_days.

        Triple-gated safety:
        1. Day must be in _synced_days (fully synced locally)
        2. Segment must be older than retention threshold (unless retention=0)
        3. Segment must be confirmed present on server (fresh query)
        """
        retention = self._config.cache_retention_days
        if retention < 0:
            return

        captures_dir = self._config.captures_dir
        if not captures_dir.exists():
            return

        today = datetime.now().strftime("%Y%m%d")
        if retention > 0:
            cutoff = (datetime.now() - timedelta(days=retention)).strftime("%Y%m%d")
        else:
            cutoff = today  # 0 means delete immediately — all days qualify

        deleted_total = 0

        for day_dir in sorted(captures_dir.iterdir()):
            if not day_dir.is_dir():
                continue

            day = day_dir.name

            if not self._running:
                break

            # Gate 1: day must be in synced_days
            if day not in self._synced_days:
                continue

            # Gate 2: day must be old enough (unless retention=0)
            if retention > 0 and day >= cutoff:
                continue

            # Don't clean today's segments
            if day == today:
                continue

            # Gate 3: fresh server confirmation
            query_result = await asyncio.to_thread(
                self._client.get_server_segments, day
            )
            if query_result.error_type is not None or query_result.segments is None:
                self._record_failure(query_result.error_type, query_result.status_code)
                logger.warning("Cleanup: skipping day %s — server unreachable", day)
                continue
            self._record_contact()

            proof_available = not query_result.legacy and not query_result.truncated
            entries_by_key = (
                _index_entries(query_result.segments) if proof_available else {}
            )

            deleted_day = 0

            for stream_dir in day_dir.iterdir():
                if not stream_dir.is_dir():
                    continue

                for seg_dir in sorted(stream_dir.iterdir()):
                    if not seg_dir.is_dir():
                        continue

                    name = seg_dir.name
                    # Never touch incomplete segments
                    if name.endswith(".incomplete"):
                        continue

                    # Quarantined (.failed) segments are handled by the age-based sweep, never here.
                    if name.endswith(".failed"):
                        continue

                    entry = _lookup_entry(entries_by_key, seg_dir)
                    if not (
                        proof_available
                        and entry is not None
                        and _segment_proven_held(seg_dir, entry)
                    ):
                        logger.warning(
                            "Cleanup: keeping %s/%s — not confirmed on server",
                            day,
                            name,
                        )
                        continue

                    shutil.rmtree(seg_dir)
                    logger.info("Cleanup: deleted %s/%s", day, name)
                    deleted_day += 1

                # Remove empty stream dir
                if stream_dir.is_dir() and not any(stream_dir.iterdir()):
                    stream_dir.rmdir()

            # Remove empty day dir
            if day_dir.is_dir() and not any(day_dir.iterdir()):
                day_dir.rmdir()

            if deleted_day:
                deleted_total += deleted_day

        if deleted_total:
            logger.info("Cleanup: deleted %d segment(s) total", deleted_total)

    def _sweep_expired_quarantine(self) -> None:
        """Drop quarantined (.failed) segments past the local TTL.

        Purely local and unconditional: independent of _synced_days, server
        reachability, cache_retention_days, and circuit state. Age is measured
        from quarantine-entry time (the .failed dir's stamped mtime), not
        capture time. Anything younger than the TTL is always kept.
        """
        captures_dir = self._config.captures_dir
        if not captures_dir.exists():
            return

        cutoff = self._now() - QUARANTINE_TTL_DAYS * 86400

        for day_dir in sorted(captures_dir.iterdir()):
            if not day_dir.is_dir():
                continue
            day = day_dir.name
            for stream_dir in day_dir.iterdir():
                if not stream_dir.is_dir():
                    continue
                for seg_dir in sorted(stream_dir.iterdir()):
                    if not seg_dir.is_dir() or not seg_dir.name.endswith(".failed"):
                        continue
                    try:
                        mtime = seg_dir.stat().st_mtime
                    except OSError:
                        continue
                    if mtime > cutoff:
                        continue
                    age_days = int((self._now() - mtime) // 86400)
                    try:
                        shutil.rmtree(seg_dir)
                    except OSError as e:
                        logger.error(
                            "Failed to drop quarantined %s/%s: %s",
                            day,
                            seg_dir.name,
                            e,
                        )
                        continue
                    logger.warning(
                        "Dropping quarantined %s/%s — held %dd, past %dd limit; "
                        "unrecovered quarantined data discarded",
                        day,
                        seg_dir.name,
                        age_days,
                        QUARANTINE_TTL_DAYS,
                    )

    def _save_health(self) -> None:
        try:
            save_facts(self._config.state_dir, self._facts)
        except OSError as e:
            logger.warning("Failed to save sync health: %s", e)

    def _emit_health_changed(self) -> None:
        health = self.health
        emitted = f"{health.state.value}:{self._facts.progress}"
        if emitted == self._last_emitted_health:
            return
        self._last_emitted_health = emitted
        if self._dbus_service:
            self._dbus_service.SyncProgressChanged(emitted)

    def _set_progress(self, progress: str, in_progress: bool = True) -> None:
        self._facts.in_progress = in_progress
        self._facts.progress = progress
        self._save_health()
        self._emit_health_changed()

    def _record_contact(self, force: bool = False) -> None:
        self._facts.last_successful_contact = self._now()
        now_mono = time.monotonic()
        if force or now_mono - self._last_contact_flush >= CONTACT_FLUSH_INTERVAL:
            self._last_contact_flush = now_mono
            self._save_health()
        self._emit_health_changed()

    def _record_failure(
        self, error_type: ErrorType | None, status_code: int | None = None
    ) -> None:
        if error_type is None:
            return

        self._last_error_type = error_type
        self._facts.last_error_class = error_type
        self._facts.last_error_code = status_code
        self._facts.pending_confirmed = None
        self._save_health()
        self._emit_health_changed()

        if error_type == ErrorType.CLIENT:
            return

        self._consecutive_failures += 1
        threshold = self._circuit_threshold()
        if self._consecutive_failures >= threshold:
            self._circuit_open = True
            self._circuit_open_permanent = error_type == ErrorType.AUTH
            self._circuit_open_since = time.monotonic()
            self._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
            logger.error(
                "Circuit breaker OPEN: %s consecutive %s failures (threshold: %s)",
                self._consecutive_failures,
                error_type.value,
                threshold,
            )

    def _commit_pass_result(
        self,
        success: bool,
        error_type: ErrorType | None = None,
        status_code: int | None = None,
    ) -> None:
        self._facts.in_progress = False
        self._facts.progress = ""
        if success:
            now = self._now()
            self._facts.last_successful_sync = now
            if self._facts.last_successful_contact is None:
                self._facts.last_successful_contact = now
            self._facts.last_error_class = None
            self._facts.last_error_code = None
            self._facts.pending_confirmed = 0
            self._consecutive_failures = 0
            self._last_error_type = None
        else:
            self._facts.pending_confirmed = None
            self._facts.last_error_class = error_type
            self._facts.last_error_code = status_code
        self._last_contact_flush = time.monotonic()
        self._save_health()
        self._emit_health_changed()

    def _circuit_threshold(self) -> int:
        """Get circuit breaker threshold based on last error type."""
        if self._last_error_type in (ErrorType.AUTH, ErrorType.INCOMPATIBLE):
            return CIRCUIT_THRESHOLD_AUTH
        if self._last_error_type == ErrorType.CLIENT:
            return 0
        return CIRCUIT_THRESHOLD_TRANSIENT

    def trigger(self) -> None:
        """Trigger a sync pass (called by observer on segment completion)."""
        self._trigger.set()

    def stop(self) -> None:
        """Stop the sync service."""
        self._running = False
        self._trigger.set()
        self._client.request_stop()

    async def run(self) -> None:
        """Main sync loop — waits for triggers, then syncs."""
        # Prune on startup
        self._prune_synced_days()

        while self._running:
            try:
                # Wait for trigger or periodic check (60s timeout)
                try:
                    await asyncio.wait_for(self._trigger.wait(), timeout=60)
                except asyncio.TimeoutError:
                    pass

                self._trigger.clear()

                if not self._running:
                    break

                if self._circuit_open:
                    if self._circuit_open_permanent:
                        self._facts.in_progress = False
                        self._facts.progress = ""
                        self._save_health()
                        self._emit_health_changed()
                        logger.warning(
                            "Circuit breaker open (permanent) — skipping sync"
                        )
                        continue

                    elapsed = time.monotonic() - self._circuit_open_since
                    if elapsed < self._circuit_cooldown:
                        remaining = self._circuit_cooldown - elapsed
                        self._facts.in_progress = False
                        self._facts.progress = f"{remaining:.0f}s until probe"
                        self._save_health()
                        self._emit_health_changed()
                        logger.warning(
                            f"Circuit breaker open — {remaining:.0f}s until probe"
                        )
                        continue

                    self._set_progress("probing journal...")
                    logger.info("Circuit breaker half-open — probing server")
                    today = datetime.now().strftime("%Y%m%d")
                    probe_result = await asyncio.to_thread(
                        self._client.get_server_segments, today
                    )
                    if probe_result.error_type is None:
                        self._record_contact(force=True)
                        logger.info("Circuit breaker probe succeeded — closing circuit")
                        self._circuit_open = False
                        self._circuit_open_permanent = False
                        self._circuit_open_since = 0.0
                        self._circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL
                        self._consecutive_failures = 0
                        self._last_error_type = None
                        self._facts.last_error_class = None
                        self._facts.last_error_code = None
                        self._set_progress("syncing...")
                    else:
                        self._record_failure(
                            probe_result.error_type, probe_result.status_code
                        )
                        self._circuit_cooldown = min(
                            self._circuit_cooldown * CIRCUIT_COOLDOWN_FACTOR,
                            CIRCUIT_COOLDOWN_MAX,
                        )
                        self._circuit_open_since = time.monotonic()
                        self._facts.in_progress = False
                        self._facts.progress = (
                            f"probe failed, next in {self._circuit_cooldown:.0f}s"
                        )
                        self._save_health()
                        self._emit_health_changed()
                        logger.warning(
                            f"Circuit breaker probe failed — next probe in {self._circuit_cooldown:.0f}s"
                        )
                        continue

                # Force full sync daily
                now = self._now()
                force_full = (now - self._last_full_sync) > 86400

                await self._sync(force_full=force_full)

                if force_full:
                    self._last_full_sync = now

            except Exception as e:
                logger.error(f"Sync error: {e}", exc_info=True)
                await asyncio.sleep(5)

    async def _sync(self, force_full: bool = False) -> None:
        """Walk days newest-to-oldest and upload missing segments."""
        captures_dir = self._config.captures_dir

        today = datetime.now().strftime("%Y%m%d")

        # Collect segments by day
        segments_by_day = (
            self._collect_segments(captures_dir) if captures_dir.exists() else {}
        )
        days = set(segments_by_day.keys())
        # Always query today so a caught-up/no-cache observer can earn connected.
        days.add(today)

        self._set_progress("checking journal...")
        pass_success = True
        pass_error_type: ErrorType | None = None
        pass_error_code: int | None = None
        legacy_logged = False

        for day in sorted(days, reverse=True):
            if not self._running:
                pass_success = False
                break

            if self._circuit_open:
                pass_success = False
                break

            # Skip past days already fully synced (unless forcing)
            if day != today and day in self._synced_days and not force_full:
                continue

            local_segments = segments_by_day.get(day, [])

            # Query server for existing segments
            self._set_progress(f"checking {day}...")
            query_result = await asyncio.to_thread(
                self._client.get_server_segments, day
            )
            if query_result.error_type is not None or query_result.segments is None:
                pass_success = False
                pass_error_type = query_result.error_type
                pass_error_code = query_result.status_code
                self._record_failure(query_result.error_type, query_result.status_code)
                logger.warning(f"Failed to query server for day {day}")
                if self._circuit_open:
                    break
                continue
            self._record_contact()

            if query_result.legacy:
                if not legacy_logged:
                    logger.warning(
                        "Journal listing is pre-v2 bare array; per-file reconcile "
                        "unavailable; syncing on key-membership; cleanup will not delete"
                    )
                    legacy_logged = True
                key_set = set(_index_entries(query_result.segments).keys())
                entries_by_key = {}
            else:
                key_set = set()
                entries_by_key = _index_entries(query_result.segments)

            any_needed_upload = False

            for segment_dir in local_segments:
                if not self._running or self._circuit_open:
                    break

                segment_key = segment_dir.name
                if query_result.legacy:
                    server_key = _read_server_key(segment_dir)
                    held = segment_key in key_set or (
                        server_key is not None and server_key in key_set
                    )
                elif query_result.truncated:
                    held = False
                else:
                    entry = _lookup_entry(entries_by_key, segment_dir)
                    held = entry is not None and _segment_proven_held(
                        segment_dir, entry
                    )

                if held:
                    continue

                # Quarantine segments where all files are zero-byte (corrupt)
                files = _eligible_files(segment_dir)
                if files and all(f.stat().st_size == 0 for f in files):
                    self._quarantine_segment(segment_dir, "all files zero-byte")
                    continue

                any_needed_upload = True
                self._set_progress(f"uploading {segment_key}")
                success = await self._upload_segment(day, segment_dir)

                if not success:
                    pass_success = False
                    pass_error_type = self._last_error_type
                    pass_error_code = None
                    if self._last_error_type == ErrorType.CLIENT:
                        # Non-retryable client error (e.g. 400) — quarantine, don't trip circuit
                        self._quarantine_segment(
                            segment_dir, "server rejected (client error)"
                        )
                        self._record_failure(self._last_error_type)
                        continue

                    self._record_failure(self._last_error_type)
                    if self._circuit_open:
                        break
                else:
                    self._consecutive_failures = 0
                    self._last_error_type = None

            # Mark past days as synced if nothing needed upload
            if day != today and not any_needed_upload:
                self._synced_days.add(day)
                self._save_synced_days()

        if pass_success and not self._circuit_open and self._running:
            self._commit_pass_result(True)
        else:
            self._commit_pass_result(
                False,
                pass_error_type or self._facts.last_error_class,
                pass_error_code or self._facts.last_error_code,
            )

        # Cleanup old synced segments
        if not self._circuit_open and self._running:
            try:
                await self._cleanup_synced_segments()
            except Exception as e:
                logger.error(f"Cleanup error: {e}", exc_info=True)

        # Expire quarantined segments past the local TTL — runs even when the
        # circuit is open / server is down / day was never synced / retention == -1.
        if self._running:
            try:
                self._sweep_expired_quarantine()
            except Exception as e:
                logger.error(f"Quarantine sweep error: {e}", exc_info=True)

    def _collect_segments(self, captures_dir: Path) -> dict[str, list[Path]]:
        """Collect completed segments grouped by day."""
        result: dict[str, list[Path]] = {}

        for day_dir in sorted(captures_dir.iterdir(), reverse=True):
            if not day_dir.is_dir():
                continue

            day = day_dir.name

            for stream_dir in day_dir.iterdir():
                if not stream_dir.is_dir():
                    continue

                segments = []
                for seg_dir in sorted(stream_dir.iterdir(), reverse=True):
                    if not seg_dir.is_dir():
                        continue
                    name = seg_dir.name
                    # Skip incomplete and failed
                    if name.endswith(".incomplete") or name.endswith(".failed"):
                        continue
                    segments.append(seg_dir)

                if segments:
                    result.setdefault(day, []).extend(segments)

        return result

    async def _upload_segment(self, day: str, segment_dir: Path) -> bool:
        """Upload a single segment with retry logic."""
        segment_key = segment_dir.name
        files = _eligible_files(segment_dir)
        if not files:
            return True  # Nothing to upload

        result = await asyncio.to_thread(
            self._client.upload_segment, day, segment_key, files
        )

        if result.success:
            if result.stored_key and result.stored_key != segment_key:
                _write_server_key(segment_dir, result.stored_key)
            self._record_contact()
            logger.info(f"Uploaded: {day}/{segment_key} ({len(files)} files)")
            return True

        # Track error type for circuit breaker
        self._last_error_type = result.error_type

        # Non-retryable errors
        if self._client.is_revoked:
            logger.error("Client revoked — disabling sync")
            self._circuit_open = True
            self._circuit_open_permanent = True
            return False

        logger.error(f"Upload failed: {day}/{segment_key}")
        return False
