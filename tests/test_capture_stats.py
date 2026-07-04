# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import os

from solstone_linux.capture_stats import compute_quarantine_stats


def test_compute_quarantine_stats_counts_both_shapes_and_oldest_age(tmp_path):
    captures_dir = tmp_path / "captures"
    duration_shape = captures_dir / "20260101" / "archon" / "120000_300.failed"
    bare_shape = captures_dir / "20260101" / "archon" / "130000.failed"
    duration_shape.mkdir(parents=True)
    bare_shape.mkdir(parents=True)
    now = 1_000_000.0
    newer_mtime = now - 2 * 86400
    older_mtime = now - 5 * 86400
    os.utime(duration_shape, (newer_mtime, newer_mtime))
    os.utime(bare_shape, (older_mtime, older_mtime))

    stats = compute_quarantine_stats(captures_dir, now=now)

    assert stats["count"] == 2
    assert abs(stats["oldest_age_seconds"] - 5 * 86400) < 1


def test_compute_quarantine_stats_empty_or_missing_tree(tmp_path):
    now = 1_000_000.0

    assert compute_quarantine_stats(tmp_path / "missing", now=now) == {
        "count": 0,
        "oldest_age_seconds": None,
    }

    captures_dir = tmp_path / "captures"
    captures_dir.mkdir()
    assert compute_quarantine_stats(captures_dir, now=now) == {
        "count": 0,
        "oldest_age_seconds": None,
    }
