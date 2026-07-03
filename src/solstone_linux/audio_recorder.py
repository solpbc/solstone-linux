# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Audio recording for Linux desktop observer.

Extracted from solstone's observe/hear.py — AudioRecorder class only.
load_transcript() and format_audio() remain in solstone core (used by 15+ files).

Changes from monorepo version:
- Replaces `from observe.detect import input_detect` with local audio_detect
- Replaces conditional `think.callosum` import with local logging
- Defines SAMPLE_RATE locally (was from observe.utils)
"""

from __future__ import annotations

import gc
import io
import logging
import os
import signal
import threading
import time
from queue import Queue

import numpy as np
import soundfile as sf

logger = logging.getLogger(__name__)

# Standard sample rate for audio processing
SAMPLE_RATE = 16000
BLOCK_SIZE = 1024
MAX_CONSECUTIVE_FAILURES = 3
REDETECT_INTERVAL = 5


class AudioRecorder:
    """Records stereo audio from microphone and system audio."""

    def __init__(self):
        # Queue holds stereo chunks (mic=left, sys=right)
        self.audio_queue = Queue()
        self._running = True
        self.recording_thread = None
        self.fatal_error: str | None = None
        self.audio_available = True
        self.mic_device = None
        self.sys_device = None
        self._consecutive_failures = 0

    def _set_audio_available(self, available: bool):
        """Set degraded state with edge-triggered logging."""
        if self.audio_available == available:
            return
        self.audio_available = available
        if available:
            logger.info("Audio devices recovered — resuming audio capture")
        else:
            logger.warning(
                "Audio devices unavailable — continuing with screen capture only"
            )

    def detect(self):
        """Detect microphone and system audio devices."""
        from .audio_detect import input_detect

        mic, loopback = input_detect()
        if mic is None or loopback is None:
            # Partial availability is degraded, not mono capture; mono capture is future work.
            self._set_audio_available(False)
            return False
        self.mic_device = mic
        self.sys_device = loopback
        self._consecutive_failures = 0
        # Use id instead of name: soundcard name performs a fresh metadata query.
        logger.info(f"Detected microphone: {mic.id}")
        logger.info(f"Detected system audio: {loopback.id}")
        self._set_audio_available(True)
        return True

    def _sleep_interruptibly(self, seconds: float):
        """Sleep in short steps so stop_recording can interrupt re-detect waits."""
        deadline = time.monotonic() + seconds
        while self._running:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return
            time.sleep(min(1.0, remaining))

    def record_both(self):
        """Record from both mic and system audio in a loop."""
        while self._running:
            if not self.audio_available:
                self.detect()
                if not self.audio_available:
                    self._sleep_interruptibly(REDETECT_INTERVAL)
                continue

            if self._consecutive_failures >= MAX_CONSECUTIVE_FAILURES:
                logger.info(
                    "Re-detecting audio devices after %d consecutive recorder failures",
                    self._consecutive_failures,
                )
                self._consecutive_failures = 0
                self.detect()
                continue

            try:
                with (
                    self.mic_device.recorder(
                        samplerate=SAMPLE_RATE, channels=[-1], blocksize=BLOCK_SIZE
                    ) as mic_rec,
                    self.sys_device.recorder(
                        samplerate=SAMPLE_RATE, channels=[-1], blocksize=BLOCK_SIZE
                    ) as sys_rec,
                ):
                    block_count = 0
                    while self._running and block_count < 1000:
                        try:
                            mic_chunk = mic_rec.record(numframes=BLOCK_SIZE)
                            sys_chunk = sys_rec.record(numframes=BLOCK_SIZE)

                            # Basic validation
                            if mic_chunk is None or mic_chunk.size == 0:
                                logger.warning("Empty microphone buffer")
                                # Empty buffers are intentionally not recorder failures.
                                continue
                            if sys_chunk is None or sys_chunk.size == 0:
                                logger.warning("Empty system buffer")
                                # Empty buffers are intentionally not recorder failures.
                                continue

                            try:
                                stereo_chunk = np.column_stack((mic_chunk, sys_chunk))
                                self.audio_queue.put(stereo_chunk)
                                block_count += 1
                                self._consecutive_failures = 0
                            except (TypeError, ValueError, AttributeError) as e:
                                error_msg = f"Fatal audio format error: {e}"
                                logger.error(
                                    f"{error_msg} - triggering clean shutdown\n"
                                    f"  mic_chunk type={type(mic_chunk)}, "
                                    f"shape={getattr(mic_chunk, 'shape', 'N/A')}, "
                                    f"dtype={getattr(mic_chunk, 'dtype', 'N/A')}\n"
                                    f"  sys_chunk type={type(sys_chunk)}, "
                                    f"shape={getattr(sys_chunk, 'shape', 'N/A')}, "
                                    f"dtype={getattr(sys_chunk, 'dtype', 'N/A')}"
                                )
                                # Stop recording thread and trigger shutdown
                                self.fatal_error = error_msg
                                self._running = False
                                os.kill(os.getpid(), signal.SIGTERM)
                                return
                        except Exception as e:
                            logger.error(f"Error recording audio: {e}")
                            self._consecutive_failures += 1
                            if not self._running:
                                break
                            if self._consecutive_failures >= MAX_CONSECUTIVE_FAILURES:
                                break
                            time.sleep(0.5)
                del mic_rec, sys_rec
                gc.collect()
            except Exception as e:
                logger.error(f"Error setting up recorders: {e}")
                self._consecutive_failures += 1
                if self._running:
                    self._sleep_interruptibly(1)

    def get_buffers(self) -> np.ndarray:
        """Return concatenated stereo audio data from the queue."""
        stereo_buffer = np.array([], dtype=np.float32).reshape(0, 2)

        while not self.audio_queue.empty():
            stereo_chunk = self.audio_queue.get()

            if stereo_chunk is None or stereo_chunk.size == 0:
                logger.warning("Queue contained empty chunk")
                continue

            # Clean the data
            stereo_chunk = np.nan_to_num(
                stereo_chunk, nan=0.0, posinf=1e10, neginf=-1e10
            )
            stereo_buffer = np.vstack((stereo_buffer, stereo_chunk))

        if stereo_buffer.size == 0:
            logger.warning("No valid audio data retrieved from queue")

        return stereo_buffer

    def create_flac_bytes(self, stereo_data: np.ndarray) -> bytes:
        """Create FLAC bytes from stereo audio data."""
        if stereo_data is None or stereo_data.size == 0:
            logger.warning("Audio data is empty. Returning empty bytes.")
            return b""

        audio_data = (np.clip(stereo_data, -1.0, 1.0) * 32767).astype(np.int16)

        buf = io.BytesIO()
        try:
            sf.write(buf, audio_data, SAMPLE_RATE, format="FLAC")
        except Exception as e:
            logger.error(
                f"Error creating FLAC: {e}. Audio data shape: {audio_data.shape}, dtype: {audio_data.dtype}"
            )
            return b""

        return buf.getvalue()

    def create_mono_flac_bytes(self, mono_data: np.ndarray) -> bytes:
        """Create FLAC bytes from mono audio data."""
        if mono_data is None or mono_data.size == 0:
            logger.warning("Mono audio data is empty. Returning empty bytes.")
            return b""

        audio_data = (np.clip(mono_data, -1.0, 1.0) * 32767).astype(np.int16)

        buf = io.BytesIO()
        try:
            sf.write(buf, audio_data, SAMPLE_RATE, format="FLAC")
        except Exception as e:
            logger.error(
                f"Error creating mono FLAC: {e}. Audio shape: {audio_data.shape}"
            )
            return b""

        return buf.getvalue()

    def start_recording(self):
        """Start the recording thread."""
        self._running = True
        self.recording_thread = threading.Thread(target=self.record_both, daemon=True)
        self.recording_thread.start()

    def stop_recording(self):
        """Stop the recording thread."""
        self._running = False
        if self.recording_thread:
            self.recording_thread.join(timeout=2.0)
