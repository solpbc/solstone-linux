# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""HTTP upload client for solstone ingest server.

Extracted from solstone's observe/remote_client.py. Accepts Config
as constructor parameter instead of reading config internally.

Refinements over tmux baseline:
- Respects configured sync_max_retries without hard cap
- Error classification: auth (401/403) vs transient (5xx/network)
"""

from __future__ import annotations

import logging
import platform
import socket
import time
from pathlib import Path
from typing import Any, NamedTuple

import requests

from . import __version__
from .config import Config
from .event_sender import EventSender
from .sync_health import ErrorType

logger = logging.getLogger(__name__)

UPLOAD_TIMEOUT = 300
EVENT_TIMEOUT = 30
EVENT_DRAIN_TIMEOUT = 3.0
STREAM_TYPE = "desktop"
OBSERVER_PROTOCOL_VERSION = 2
OBSERVER_PROTOCOL_VERSION_HEADER = "X-Solstone-Protocol-Version"
_CONTENT_TYPES = {".flac": "audio/flac", ".webm": "video/webm"}


def _auth_headers(key: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {key}"}


class UploadResult(NamedTuple):
    success: bool
    duplicate: bool = False
    error_type: ErrorType | None = None
    stored_key: str | None = None


class QueryResult(NamedTuple):
    segments: list[dict] | None
    error_type: ErrorType | None = None
    status_code: int | None = None
    legacy: bool = False
    truncated: bool = False


class UploadClient:
    """HTTP client for uploading observer segments to the ingest server."""

    def __init__(self, config: Config):
        self._url = config.server_url.rstrip("/") if config.server_url else ""
        self._key = config.key
        self._stream = config.stream
        self._revoked = False
        self._session = requests.Session()
        self._event_session = requests.Session()
        self._event_sender = EventSender(self.relay_event)
        self._retry_backoff = config.sync_retry_delays or [5, 30, 120, 300]
        # Respect configured retry cap — no hard min(config, 3)
        self._max_retries = config.sync_max_retries

    @property
    def is_revoked(self) -> bool:
        return self._revoked

    @property
    def is_registered(self) -> bool:
        return bool(self._key)

    def _persist_registration(self, config: Config, key: str, stream: str) -> None:
        """Persist the server-issued handle and locked stream back to config."""
        from .config import save_config

        config.key = key
        config.stream = stream
        save_config(config)

    def ensure_registered(self, config: Config) -> bool:
        """Register this observer over HTTP, persisting the handle + locked stream.

        Short-circuits if a key is already present. Returns True if a key is available.
        """
        if self._key:
            return True
        if not self._url:
            return False

        descriptor: dict[str, Any] = {
            "platform": platform.system().lower(),
            "hostname": socket.gethostname(),
            "stream_type": STREAM_TYPE,
            "version": __version__,
        }
        if self._stream:
            descriptor["label"] = self._stream

        url = f"{self._url}/app/observer/register"

        retries = min(3, len(self._retry_backoff))
        for attempt in range(retries):
            delay = self._retry_backoff[min(attempt, len(self._retry_backoff) - 1)]
            try:
                resp = self._session.post(url, json=descriptor, timeout=EVENT_TIMEOUT)
                if resp.status_code == 200:
                    data = resp.json()
                    self._key = data["key"]
                    self._stream = data["name"]
                    self._persist_registration(config, data["key"], data["name"])
                    logger.info(
                        f"Registered as '{data['name']}' (key: {self._key[:8]}...)"
                    )
                    return True
                elif resp.status_code == 403:
                    self._revoked = True
                    logger.error("Registration rejected (403)")
                    return False
                else:
                    logger.warning(
                        f"Registration attempt {attempt + 1} failed: {resp.status_code}"
                    )
            except requests.RequestException as e:
                logger.warning(f"Registration attempt {attempt + 1} failed: {e}")
            if attempt < retries - 1:
                time.sleep(delay)

        logger.error(f"Registration failed after {retries} attempts")
        return False

    @staticmethod
    def classify_error(
        status_code: int | None, is_network_error: bool = False
    ) -> ErrorType:
        """Classify an error for circuit breaker and retry decisions."""
        if is_network_error:
            return ErrorType.TRANSIENT
        if status_code is None:
            return ErrorType.TRANSIENT
        if status_code in (401, 403):
            return ErrorType.AUTH
        if status_code == 400:
            return ErrorType.CLIENT
        if status_code == 404:
            return ErrorType.INCOMPATIBLE
        # 5xx and anything else
        return ErrorType.TRANSIENT

    def upload_segment(
        self,
        day: str,
        segment: str,
        files: list[Path],
    ) -> UploadResult:
        """Upload a segment's files to the ingest server."""
        if self._revoked or not self._key or not self._url:
            return UploadResult(
                False, error_type=ErrorType.AUTH if self._revoked else ErrorType.CLIENT
            )

        url = f"{self._url}/app/observer/ingest"

        for attempt in range(self._max_retries):
            file_handles = []
            files_data = []
            error_type = None
            try:
                for path in files:
                    if not path.exists():
                        logger.warning(f"File not found, skipping: {path}")
                        continue
                    fh = open(path, "rb")
                    file_handles.append(fh)
                    content_type = _CONTENT_TYPES.get(
                        path.suffix.lower(), "application/octet-stream"
                    )
                    files_data.append(("files", (path.name, fh, content_type)))

                if not files_data:
                    return UploadResult(False)

                data = {"day": day, "segment": segment}

                response = self._session.post(
                    url,
                    data=data,
                    files=files_data,
                    headers=_auth_headers(self._key),
                    timeout=UPLOAD_TIMEOUT,
                )

                if response.status_code == 200:
                    resp_data = response.json()
                    status = resp_data.get("status")
                    is_duplicate = status == "duplicate"
                    stored_key = (
                        resp_data.get("existing_segment")
                        if is_duplicate
                        else resp_data.get("segment")
                    )
                    return UploadResult(
                        True, duplicate=is_duplicate, stored_key=stored_key
                    )

                error_type = self.classify_error(response.status_code)

                if error_type == ErrorType.AUTH:
                    if response.status_code == 403:
                        self._revoked = True
                    logger.error(
                        f"Upload rejected ({response.status_code}): {response.text}"
                    )
                    return UploadResult(False, error_type=error_type)

                if error_type in (ErrorType.CLIENT, ErrorType.INCOMPATIBLE):
                    logger.error(
                        f"Upload rejected ({response.status_code}): {response.text}"
                    )
                    return UploadResult(False, error_type=error_type)

                logger.warning(
                    f"Upload attempt {attempt + 1} failed: "
                    f"{response.status_code} {response.text}"
                )
            except requests.RequestException as e:
                error_type = ErrorType.TRANSIENT
                logger.warning(f"Upload attempt {attempt + 1} failed: {e}")
            finally:
                for fh in file_handles:
                    try:
                        fh.close()
                    except Exception:
                        pass

            if attempt < self._max_retries - 1:
                delay = self._retry_backoff[min(attempt, len(self._retry_backoff) - 1)]
                time.sleep(delay)

        logger.error(
            f"Upload failed after {self._max_retries} attempts: {day}/{segment}"
        )
        return UploadResult(False, error_type=error_type)

    def get_server_segments(self, day: str) -> QueryResult:
        """Query server for segments on a given day.

        Returns segment dicts on success, with error details on failure.
        """
        if self._revoked:
            return QueryResult(None, ErrorType.AUTH, None)
        if not self._key or not self._url:
            return QueryResult(None, ErrorType.CLIENT, None)

        url = f"{self._url}/app/observer/ingest/segments/{day}"
        headers = {
            **_auth_headers(self._key),
            OBSERVER_PROTOCOL_VERSION_HEADER: str(OBSERVER_PROTOCOL_VERSION),
        }

        try:
            resp = self._session.get(url, headers=headers, timeout=EVENT_TIMEOUT)
            if resp.status_code == 200:
                body = resp.json()
                if isinstance(body, list):
                    return QueryResult(body, None, resp.status_code, legacy=True)
                if isinstance(body, dict):
                    items = body.get("items", [])
                    total = body.get("total", len(items))
                    truncated = total != len(items)
                    return QueryResult(
                        items,
                        None,
                        resp.status_code,
                        legacy=False,
                        truncated=truncated,
                    )
                return QueryResult([], None, resp.status_code)
            error_type = self.classify_error(resp.status_code)
            if error_type == ErrorType.AUTH:
                if resp.status_code == 403:
                    self._revoked = True
                logger.error(f"Segments query rejected ({resp.status_code})")
            logger.warning(f"Segments query failed: {resp.status_code}")
            return QueryResult(None, error_type, resp.status_code)
        except requests.RequestException as e:
            logger.debug(f"Segments query failed: {e}")
            return QueryResult(None, ErrorType.TRANSIENT, None)

    def relay_event(self, tract: str, event: str, **fields: Any) -> bool:
        """Fire-and-forget event relay."""
        if self._revoked or not self._key or not self._url:
            return False

        url = f"{self._url}/app/observer/ingest/event"
        payload = {"tract": tract, "event": event, **fields}
        try:
            resp = self._event_session.post(
                url,
                json=payload,
                headers=_auth_headers(self._key),
                timeout=EVENT_TIMEOUT,
            )
            if resp.status_code == 200:
                return True
            if resp.status_code == 403:
                self._revoked = True
            return False
        except requests.RequestException:
            return False

    def enqueue_status(self, fields: dict[str, Any]) -> None:
        self._event_sender.submit_status(fields)
        self._event_sender.start()

    def enqueue_stream_silent(self, fields: dict[str, Any]) -> None:
        self._event_sender.submit_stream_silent(fields)
        self._event_sender.start()

    def stop(self) -> None:
        self._event_sender.stop(EVENT_DRAIN_TIMEOUT)
        self._event_session.close()
        self._session.close()
