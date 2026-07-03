# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Audio device detection.

Changes from monorepo version:
- Uses structural soundcard isloopback metadata instead of amplitude-thresholding
  on a played tone, so muted sinks and silent rooms no longer fail detection.
"""

import logging
import threading
import time

import soundcard as sc

logger = logging.getLogger(__name__)


def input_detect(timeout=3.0):
    try:
        # Fully wedged PulseAudio enumeration is a pre-existing out-of-scope hang.
        devices = sc.all_microphones(include_loopback=True)
    except Exception:
        logger.warning("Failed to enumerate audio devices")
        return None, None
    if not devices:
        logger.warning("No audio devices found")
        return None, None

    results = {}
    lock = threading.Lock()

    def classify(index, mic):
        try:
            is_loopback = bool(mic.isloopback)
        except Exception:
            is_loopback = None
        with lock:
            results[index] = is_loopback

    threads = []
    deadline = time.monotonic() + timeout
    for index, mic in enumerate(devices):
        thread = threading.Thread(target=classify, args=(index, mic), daemon=True)
        thread.start()
        threads.append(thread)

    for thread in threads:
        remaining = max(0.0, deadline - time.monotonic())
        thread.join(timeout=remaining)

    with lock:
        final_results = dict(results)

    mic_detected = None
    loopback_detected = None
    for index, mic in enumerate(devices):
        is_loopback = final_results.get(index)
        if is_loopback is None:
            continue
        if is_loopback and loopback_detected is None:
            loopback_detected = mic
        elif not is_loopback and mic_detected is None:
            mic_detected = mic
    return mic_detected, loopback_detected
