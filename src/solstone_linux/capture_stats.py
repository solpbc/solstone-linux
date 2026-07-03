# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Shared capture cache statistics."""

from pathlib import Path


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
