# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for audio recording behavior."""

import logging
import signal
import threading
from unittest.mock import patch

import numpy as np

from solstone_linux.audio_recorder import AudioRecorder


class _FakeRecorder:
    def __init__(self, data: np.ndarray):
        self.data = data

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        return None

    def record(self, numframes):
        return self.data


class _FakeDevice:
    def __init__(self, data: np.ndarray):
        self.data = data
        self.id = "fake-device"

    def recorder(self, samplerate, channels, blocksize):
        return _FakeRecorder(self.data)


class _FakeDetectedDevice(_FakeDevice):
    def __init__(self, device_id: str, data: np.ndarray | None = None):
        if data is None:
            data = np.array([0.1, 0.2], dtype=np.float32)
        super().__init__(data)
        self.id = device_id


class _SetupFailDevice:
    id = "setup-fail"

    def recorder(self, samplerate, channels, blocksize):
        raise RuntimeError("setup failed")


class _SequenceRecorder:
    def __init__(self, values):
        self.values = list(values)

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        return None

    def record(self, numframes):
        value = self.values.pop(0)
        if isinstance(value, BaseException):
            raise value
        return value


class _SequenceDevice:
    id = "sequence-device"

    def __init__(self, values):
        self.values = values

    def recorder(self, samplerate, channels, blocksize):
        return _SequenceRecorder(self.values)


def test_record_both_stereo_layout_mic_left_sys_right():
    recorder = AudioRecorder()
    mic_data = np.array([0.1, 0.2, 0.3], dtype=np.float32)
    sys_data = np.array([0.4, 0.5, 0.6], dtype=np.float32)
    recorder.mic_device = _FakeDevice(mic_data)
    recorder.sys_device = _FakeDevice(sys_data)

    original_put = recorder.audio_queue.put

    def put_and_stop(chunk):
        original_put(chunk)
        recorder._running = False

    with patch.object(recorder.audio_queue, "put", side_effect=put_and_stop):
        recorder.record_both()

    chunk = recorder.audio_queue.get_nowait()
    np.testing.assert_allclose(chunk[:, 0], mic_data)
    np.testing.assert_allclose(chunk[:, 1], sys_data)


def test_create_flac_and_mono_flac_bytes_nonempty():
    recorder = AudioRecorder()
    stereo_data = np.array([[0.1, 0.2], [0.3, 0.4]], dtype=np.float32)
    mono_data = np.array([0.1, 0.2, 0.3], dtype=np.float32)
    empty_stereo = np.array([], dtype=np.float32).reshape(0, 2)
    empty_mono = np.array([], dtype=np.float32)

    assert recorder.create_flac_bytes(stereo_data).startswith(b"fLaC")
    assert recorder.create_mono_flac_bytes(mono_data).startswith(b"fLaC")
    assert recorder.create_flac_bytes(empty_stereo) == b""
    assert recorder.create_mono_flac_bytes(empty_mono) == b""


def test_set_audio_available_edge_logs_once(caplog):
    recorder = AudioRecorder()

    with caplog.at_level(logging.INFO):
        recorder._set_audio_available(False)
        recorder._set_audio_available(False)
        recorder._set_audio_available(False)
        recorder._set_audio_available(True)
        recorder._set_audio_available(True)

    warnings = [
        record for record in caplog.records if record.levelno == logging.WARNING
    ]
    infos = [record for record in caplog.records if record.levelno == logging.INFO]
    assert [record.message for record in warnings] == [
        "Audio devices unavailable — continuing with screen capture only"
    ]
    assert [record.message for record in infos] == [
        "Audio devices recovered — resuming audio capture"
    ]


def test_degraded_recorder_recovers_without_restart(caplog):
    recorder = AudioRecorder()
    mic = _FakeDetectedDevice("mic-id")
    loopback = _FakeDetectedDevice("loopback-id")
    original_put = recorder.audio_queue.put

    def put_and_stop(chunk):
        original_put(chunk)
        recorder._running = False

    with (
        caplog.at_level(logging.INFO),
        patch.object(recorder.audio_queue, "put", side_effect=put_and_stop),
        patch.object(recorder, "_sleep_interruptibly"),
        patch(
            "solstone_linux.audio_detect.input_detect",
            side_effect=[(None, None), (None, None), (mic, loopback)],
        ),
    ):
        recorder._set_audio_available(False)
        thread = threading.Thread(target=recorder.record_both)
        thread.start()
        thread.join(timeout=1.0)

    assert not thread.is_alive()
    assert recorder.audio_available is True
    assert recorder.mic_device is mic
    assert recorder.sys_device is loopback
    assert not recorder.audio_queue.empty()
    assert (
        sum(
            record.message
            == "Audio devices unavailable — continuing with screen capture only"
            for record in caplog.records
        )
        == 1
    )
    assert (
        sum(
            record.message == "Audio devices recovered — resuming audio capture"
            for record in caplog.records
        )
        == 1
    )


def test_detect_degrades_when_only_mic(caplog):
    recorder = AudioRecorder()
    mic = _FakeDetectedDevice("mic-id")

    with (
        caplog.at_level(logging.INFO),
        patch("solstone_linux.audio_detect.input_detect", return_value=(mic, None)),
    ):
        result = recorder.detect()

    assert result is False
    assert recorder.audio_available is False
    assert "Detection failed" not in caplog.text
    assert (
        caplog.text.count(
            "Audio devices unavailable — continuing with screen capture only"
        )
        == 1
    )


def test_detect_degrades_when_only_loopback(caplog):
    recorder = AudioRecorder()
    loopback = _FakeDetectedDevice("loopback-id")

    with (
        caplog.at_level(logging.INFO),
        patch(
            "solstone_linux.audio_detect.input_detect", return_value=(None, loopback)
        ),
    ):
        result = recorder.detect()

    assert result is False
    assert recorder.audio_available is False
    assert "Detection failed" not in caplog.text
    assert (
        caplog.text.count(
            "Audio devices unavailable — continuing with screen capture only"
        )
        == 1
    )


def test_record_both_setup_failures_trigger_redetect():
    recorder = AudioRecorder()
    recorder.mic_device = _SetupFailDevice()
    recorder.sys_device = _FakeDevice(np.array([0.4, 0.5], dtype=np.float32))
    working_mic = _FakeDetectedDevice("mic-id")
    working_loopback = _FakeDetectedDevice("loopback-id")
    original_put = recorder.audio_queue.put

    def recover():
        recorder.mic_device = working_mic
        recorder.sys_device = working_loopback
        recorder._set_audio_available(True)
        return True

    def put_and_stop(chunk):
        original_put(chunk)
        recorder._running = False

    with (
        patch.object(recorder, "_sleep_interruptibly"),
        patch.object(recorder, "detect", side_effect=recover) as detect_mock,
        patch.object(recorder.audio_queue, "put", side_effect=put_and_stop),
    ):
        recorder.record_both()

    detect_mock.assert_called_once()
    assert recorder._consecutive_failures == 0
    assert not recorder.audio_queue.empty()


def test_record_both_inner_record_failures_trigger_redetect():
    recorder = AudioRecorder()
    recorder.mic_device = _SequenceDevice(
        [
            RuntimeError("record failed 1"),
            RuntimeError("record failed 2"),
            RuntimeError("record failed 3"),
        ]
    )
    recorder.sys_device = _FakeDevice(np.array([0.4, 0.5], dtype=np.float32))
    working_mic = _FakeDetectedDevice("mic-id")
    working_loopback = _FakeDetectedDevice("loopback-id")
    original_put = recorder.audio_queue.put

    def recover():
        recorder.mic_device = working_mic
        recorder.sys_device = working_loopback
        recorder._set_audio_available(True)
        return True

    def put_and_stop(chunk):
        original_put(chunk)
        recorder._running = False

    with (
        patch("solstone_linux.audio_recorder.time.sleep"),
        patch.object(recorder, "detect", side_effect=recover) as detect_mock,
        patch.object(recorder.audio_queue, "put", side_effect=put_and_stop),
    ):
        recorder.record_both()

    detect_mock.assert_called_once()
    assert recorder._consecutive_failures == 0
    assert not recorder.audio_queue.empty()


def test_record_both_success_resets_counter():
    recorder = AudioRecorder()
    data = np.array([0.1, 0.2], dtype=np.float32)
    recorder.mic_device = _SequenceDevice(
        [
            RuntimeError("record failed before success"),
            data,
            RuntimeError("record failed after success 1"),
            RuntimeError("record failed after success 2"),
            data,
        ]
    )
    recorder.sys_device = _FakeDevice(np.array([0.4, 0.5], dtype=np.float32))
    original_put = recorder.audio_queue.put
    put_count = 0

    def put_and_stop_after_second_success(chunk):
        nonlocal put_count
        put_count += 1
        original_put(chunk)
        if put_count == 2:
            recorder._running = False

    with (
        patch("solstone_linux.audio_recorder.time.sleep"),
        patch.object(recorder, "detect") as detect_mock,
        patch.object(
            recorder.audio_queue, "put", side_effect=put_and_stop_after_second_success
        ),
    ):
        recorder.record_both()

    detect_mock.assert_not_called()
    assert recorder._consecutive_failures == 0
    assert put_count == 2


def test_sleep_interruptibly_exits_when_stopped():
    recorder = AudioRecorder()

    def stop_recording(_duration):
        recorder._running = False

    with (
        patch("solstone_linux.audio_recorder.time.monotonic", side_effect=[10.0, 10.0]),
        patch(
            "solstone_linux.audio_recorder.time.sleep", side_effect=stop_recording
        ) as sleep_mock,
    ):
        recorder._sleep_interruptibly(5)

    sleep_mock.assert_called_once_with(1.0)
    assert recorder._running is False


def test_fatal_format_error_untouched_by_counter():
    recorder = AudioRecorder()
    recorder.mic_device = _FakeDevice(np.array([0.1, 0.2], dtype=np.float32))
    recorder.sys_device = _FakeDevice(np.array([0.4, 0.5], dtype=np.float32))

    with (
        patch(
            "solstone_linux.audio_recorder.np.column_stack",
            side_effect=TypeError("bad format"),
        ),
        patch("solstone_linux.audio_recorder.os.kill") as kill_mock,
    ):
        recorder.record_both()

    assert recorder.fatal_error == "Fatal audio format error: bad format"
    assert recorder._running is False
    kill_mock.assert_called_once()
    assert kill_mock.call_args.args[1] == signal.SIGTERM
    assert recorder._consecutive_failures == 0
