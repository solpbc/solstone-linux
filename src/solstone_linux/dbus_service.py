# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
# ruff: noqa: F722, F821

import logging
import time

from dbus_fast import PropertyAccess, Variant
from dbus_fast.service import (
    ServiceInterface,
    dbus_property,
    method,
    signal as dbus_signal,
)

logger = logging.getLogger(__name__)

BUS_NAME = "org.solpbc.solstone.Observer1"
OBJECT_PATH = "/org/solpbc/solstone/Observer1"


class ObserverService(ServiceInterface):
    """D-Bus service interface for the observer."""

    def __init__(self, observer):
        super().__init__("org.solpbc.solstone.Observer1")
        self._observer = observer

    @dbus_property(access=PropertyAccess.READ)
    def Status(self) -> "s":
        if self._observer._paused:
            return "paused"
        if self._observer.current_mode == "screencast":
            return "recording"
        return "idle"

    @dbus_property(access=PropertyAccess.READ)
    def SyncStatus(self) -> "s":
        if self._observer._sync:
            return self._observer._sync.health.state.value
        return "unknown"

    @dbus_property(access=PropertyAccess.READ)
    def SyncProgress(self) -> "s":
        if self._observer._sync:
            return self._observer._sync.progress
        return ""

    @dbus_property(access=PropertyAccess.READ)
    def CaptureDir(self) -> "s":
        return str(self._observer.config.captures_dir)

    @dbus_property(access=PropertyAccess.READ)
    def SegmentTimer(self) -> "i":
        if self._observer._paused or self._observer.segment_dir is None:
            return 0
        remaining = self._observer.interval - (
            time.monotonic() - self._observer.start_at_mono
        )
        return max(0, int(remaining))

    @dbus_property(access=PropertyAccess.READ)
    def PauseRemaining(self) -> "i":
        if not self._observer._paused or self._observer._pause_until <= 0:
            return 0
        return max(0, int(self._observer._pause_until - time.monotonic()))

    @dbus_property(access=PropertyAccess.READ)
    def Error(self) -> "s":
        return ""

    @dbus_property(access=PropertyAccess.READ)
    def ServerUrl(self) -> "s":
        return self._observer.config.server_url or ""

    @dbus_property(access=PropertyAccess.READ)
    def Stream(self) -> "s":
        return self._observer.stream

    @dbus_property(access=PropertyAccess.READ)
    def SegmentInterval(self) -> "i":
        return self._observer.interval

    @method()
    def Pause(self, duration_seconds: "i") -> "s":
        self._observer.pause(duration_seconds)
        return "ok"

    @method()
    def Resume(self) -> "s":
        self._observer.resume()
        return "ok"

    @method()
    def GetStats(self) -> "a{sv}":
        stats = self._observer.capture_stats
        uptime_seconds = int(time.monotonic() - self._observer._start_mono)

        return {
            "captures_today": Variant("i", stats["captures_today"]),
            "total_size_mb": Variant("i", stats["total_size_mb"]),
            "uptime_seconds": Variant("i", uptime_seconds),
        }

    @dbus_signal()
    def StatusChanged(self, status) -> "s":
        return status

    @dbus_signal()
    def SyncProgressChanged(self, progress) -> "s":
        return progress

    @dbus_signal()
    def ErrorOccurred(self, message) -> "s":
        return message
