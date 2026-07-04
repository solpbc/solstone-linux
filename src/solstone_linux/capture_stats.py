# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Shared capture cache statistics."""

from pathlib import Path
import time


def compute_capture_stats(captures_dir: Path, today: str) -> dict[str, int]:
    captures_today = 0
    total_size = 0

    try:
        if captures_dir.exists():
            for day_dir in captures_dir.iterdir():
                if not day_dir.is_dir():
                    continue
                for stream_dir in day_dir.iterdir():
                    if not stream_dir.is_dir():
                        continue
                    for seg_dir in stream_dir.iterdir():
                        if not seg_dir.is_dir():
                            continue
                        if seg_dir.name.endswith(".incomplete"):
                            continue
                        if seg_dir.name.endswith(".failed"):
                            continue
                        if day_dir.name == today:
                            captures_today += 1
                        for file_path in seg_dir.iterdir():
                            if file_path.is_file():
                                total_size += file_path.stat().st_size
    except OSError:
        pass

    return {
        "captures_today": captures_today,
        "total_size_mb": int(total_size / (1024 * 1024)),
    }


def compute_quarantine_stats(captures_dir: Path, now: float | None = None) -> dict:
    """Count quarantined (.failed) segments and the oldest quarantine-entry age.

    Both name shapes (HHMMSS_DDD.failed and bare HHMMSS.failed) count.
    Returns {"count": int, "oldest_age_seconds": float | None}.
    """
    if now is None:
        now = time.time()
    count = 0
    oldest_mtime: float | None = None
    try:
        if captures_dir.exists():
            for day_dir in captures_dir.iterdir():
                if not day_dir.is_dir():
                    continue
                for stream_dir in day_dir.iterdir():
                    if not stream_dir.is_dir():
                        continue
                    for seg_dir in stream_dir.iterdir():
                        if not seg_dir.is_dir():
                            continue
                        if not seg_dir.name.endswith(".failed"):
                            continue
                        count += 1
                        try:
                            mtime = seg_dir.stat().st_mtime
                        except OSError:
                            continue
                        if oldest_mtime is None or mtime < oldest_mtime:
                            oldest_mtime = mtime
    except OSError:
        pass
    oldest_age = None if oldest_mtime is None else max(0.0, now - oldest_mtime)
    return {"count": count, "oldest_age_seconds": oldest_age}


def format_quarantine_line(stats: dict) -> str | None:
    """One-line quarantine summary (count + oldest age), or None when empty."""
    count = stats.get("count", 0)
    if not count:
        return None
    oldest = stats.get("oldest_age_seconds")
    if oldest is None:
        return f"Quarantine: {count} rejected segment(s) held"
    days = int(oldest // 86400)
    return f"Quarantine: {count} rejected segment(s) held, oldest {days}d"
