# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import time
from pathlib import Path
from unittest.mock import MagicMock

from solstone_linux.config import Config
from solstone_linux.observer import HOST, PLATFORM, Observer
from solstone_linux.screencast import SilentStream


def _silent_stream() -> SilentStream:
    return SilentStream(
        node_id=42,
        connector="HDMI-1",
        position="right",
        file_path=Path("/x/right_HDMI-1_screen.webm"),
        file_bytes=418,
    )


def test_emits_with_full_fields(tmp_path: Path):
    observer = Observer(Config(base_dir=tmp_path))
    observer._client = MagicMock()
    observer.segment_dir = Path("/fake/093014.incomplete")
    observer.start_at = time.time() - 120

    observer._emit_stream_silent(_silent_stream())

    observer._client.enqueue_stream_silent.assert_called_once()
    args, _ = observer._client.enqueue_stream_silent.call_args
    kwargs = args[0]
    assert kwargs["connector"] == "HDMI-1"
    assert kwargs["position"] == "right"
    assert kwargs["node_id"] == 42
    assert kwargs["file_bytes"] == 418
    assert kwargs["segment_dir"] == "093014.incomplete"
    assert 118 <= kwargs["duration_seconds"] <= 122
    assert kwargs["host"] == HOST
    assert kwargs["platform"] == PLATFORM


def test_skips_when_client_none(tmp_path: Path):
    observer = Observer(Config(base_dir=tmp_path))
    observer._client = None

    observer._emit_stream_silent(_silent_stream())


def test_segment_dir_empty_when_none(tmp_path: Path):
    observer = Observer(Config(base_dir=tmp_path))
    observer._client = MagicMock()
    observer.segment_dir = None
    observer.start_at = time.time() - 10

    observer._emit_stream_silent(_silent_stream())

    args, _ = observer._client.enqueue_stream_silent.call_args
    kwargs = args[0]
    assert kwargs["segment_dir"] == ""
