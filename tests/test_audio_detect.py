# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for structural audio device detection."""

import threading
import time
from unittest.mock import Mock, patch

from solstone_linux.audio_detect import input_detect


class _FakeMic:
    def __init__(self, device_id: str, is_loopback: bool, block_event=None):
        self.id = device_id
        self._is_loopback = is_loopback
        self._block_event = block_event
        self.record = Mock()

    @property
    def isloopback(self):
        if self._block_event is not None:
            self._block_event.wait()
        return self._is_loopback


def test_input_detect_detects_both_legs_without_any_signal():
    mic = _FakeMic("mic-1", False)
    loopback = _FakeMic("loopback-1", True)
    ignored_mic = _FakeMic("mic-2", False)
    devices = [mic, loopback, ignored_mic]

    with (
        patch("solstone_linux.audio_detect.sc.all_microphones", return_value=devices),
        patch("solstone_linux.audio_detect.sc.default_speaker") as default_speaker,
    ):
        detected_mic, detected_loopback = input_detect(timeout=0.2)

    assert detected_mic is mic
    assert detected_loopback is loopback
    default_speaker.assert_not_called()
    for device in devices:
        device.record.assert_not_called()


def test_input_detect_never_plays_tone():
    devices = [_FakeMic("mic-1", False), _FakeMic("loopback-1", True)]

    with (
        patch("solstone_linux.audio_detect.sc.all_microphones", return_value=devices),
        patch("solstone_linux.audio_detect.sc.default_speaker") as default_speaker,
    ):
        input_detect(timeout=0.2)

    default_speaker.assert_not_called()


def test_input_detect_hung_device_treated_absent_within_bound():
    release = threading.Event()
    hung = _FakeMic("hung-mic", False, block_event=release)
    mic = _FakeMic("mic-1", False)
    loopback = _FakeMic("loopback-1", True)

    with patch(
        "solstone_linux.audio_detect.sc.all_microphones",
        return_value=[hung, mic, loopback],
    ):
        started = time.monotonic()
        detected_mic, detected_loopback = input_detect(timeout=0.3)
        elapsed = time.monotonic() - started

    release.set()
    assert elapsed < 0.9
    assert detected_mic is mic
    assert detected_loopback is loopback
