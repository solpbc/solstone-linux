# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Background sender for observer event relay."""

from __future__ import annotations

import logging
import threading
from collections import deque
from collections.abc import Callable
from typing import Any

logger = logging.getLogger(__name__)

SILENT_QUEUE_MAX = 64


class EventSender:
    """Single background sender for observe.status and stream_silent events."""

    def __init__(self, relay: Callable[[str, str], bool]):
        self._relay = relay
        self._condition = threading.Condition()
        self._latest_status: dict[str, Any] | None = None
        self._silent: deque[dict[str, Any]] = deque()
        self._thread: threading.Thread | None = None
        self._stopping = False
        self._inflight_count = 0

    def submit_status(self, fields: dict[str, Any]) -> None:
        """Enqueue status fields, superseding any undelivered status."""
        with self._condition:
            if self._latest_status is not None:
                logger.debug("Superseding undelivered observe.status event")
            self._latest_status = dict(fields)
            self._condition.notify()

    def submit_stream_silent(self, fields: dict[str, Any]) -> None:
        """Enqueue a stream_silent event unless the bounded queue is full."""
        with self._condition:
            if len(self._silent) >= SILENT_QUEUE_MAX:
                logger.warning(
                    "Dropping stream_silent event because queue is full: "
                    "connector=%s position=%s",
                    fields.get("connector", ""),
                    fields.get("position", ""),
                )
                return
            self._silent.append(dict(fields))
            self._condition.notify()

    def start(self) -> None:
        """Start the sender thread once."""
        with self._condition:
            if self._thread is not None and self._thread.is_alive():
                return
            if self._stopping:
                return
            self._thread = threading.Thread(
                target=self._run,
                name="solstone-event-sender",
                daemon=True,
            )
            self._thread.start()

    def _run(self) -> None:
        while True:
            with self._condition:
                while (
                    not self._stopping
                    and self._latest_status is None
                    and not self._silent
                ):
                    self._condition.wait()

                if self._stopping and self._latest_status is None and not self._silent:
                    return

                status = self._latest_status
                self._latest_status = None
                silent_batch = list(self._silent)
                self._silent.clear()
                self._inflight_count = len(silent_batch) + (1 if status else 0)

            try:
                for fields in silent_batch:
                    self._relay_safely("observe", "stream_silent", fields)
                if status is not None:
                    self._relay_safely("observe", "status", status)
            finally:
                with self._condition:
                    self._inflight_count = 0

    def _relay_safely(self, tract: str, event: str, fields: dict[str, Any]) -> None:
        try:
            self._relay(tract, event, **fields)
        except Exception:
            logger.debug("Event relay failed: %s.%s", tract, event, exc_info=True)

    def stop(self, timeout: float) -> None:
        """Stop the sender without blocking beyond timeout."""
        with self._condition:
            self._stopping = True
            self._condition.notify_all()
            thread = self._thread

        if thread is None:
            return

        thread.join(timeout)
        if thread.is_alive():
            with self._condition:
                undelivered = (
                    self._inflight_count
                    + len(self._silent)
                    + (1 if self._latest_status else 0)
                )
            logger.warning(
                "Event sender did not stop within %.1fs; %d event(s) may be undelivered",
                timeout,
                undelivered,
            )
