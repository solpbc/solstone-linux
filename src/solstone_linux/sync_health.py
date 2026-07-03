# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Sync health facts, derivation, persistence, and surface copy."""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any

from .config import DEFAULT_SYNC_STALE_THRESHOLD

SCHEMA_VERSION = 1


class ErrorType(Enum):
    """Classification of sync errors for health and circuit decisions."""

    AUTH = "auth"
    CLIENT = "client"
    TRANSIENT = "transient"
    INCOMPATIBLE = "incompatible"


class HealthState(Enum):
    """User-facing sync health state."""

    CONNECTED = "connected"
    SYNCING = "syncing"
    OFFLINE = "offline"
    UPDATE_NEEDED = "update-needed"
    REVOKED = "revoked"
    STALE = "stale"
    UNKNOWN = "unknown"


@dataclass
class SyncFacts:
    """Durable facts used to derive sync health."""

    last_successful_sync: float | None = None
    last_successful_contact: float | None = None
    last_error_class: ErrorType | None = None
    last_error_code: int | None = None
    pending_confirmed: int | None = None
    in_progress: bool = False
    progress: str = ""


@dataclass(frozen=True)
class HealthSurface:
    """State-specific copy and presentation data."""

    header_recording: str
    header_idle: str
    sync_line: str
    tooltip: str
    accessible_recording: str
    accessible_idle: str
    icon: str
    sni: str
    cli: str
    doctor_severity: str
    doctor_detail: str
    dbus: str


@dataclass(frozen=True)
class SyncHealth:
    """Derived health state and fully resolved surface strings."""

    state: HealthState
    header_recording: str
    header_idle: str
    sync_line: str
    tooltip: str
    accessible_recording: str
    accessible_idle: str
    icon: str
    sni_status: str
    cli: str
    doctor_severity: str
    doctor_detail: str
    dbus: str
    pending_display: str
    last_success_age: float | None
    progress: str


SURFACE_BY_STATE: dict[HealthState, HealthSurface] = {
    HealthState.CONNECTED: HealthSurface(
        header_recording="on — connected",
        header_idle="idle — connected",
        sync_line="sync: up to date",
        tooltip="sync: up to date",
        accessible_recording="sol — on, sync up to date",
        accessible_idle="sol — idle, sync up to date",
        icon="recording",
        sni="Active",
        cli="Sync: connected — up to date (0 pending)",
        doctor_severity="ok",
        doctor_detail="sync health: up to date; 0 pending confirmed at {sync_ts}",
        dbus="connected",
    ),
    HealthState.SYNCING: HealthSurface(
        header_recording="on — syncing",
        header_idle="idle — syncing",
        sync_line="sync: {progress}",
        tooltip="sync: {progress}",
        accessible_recording="sol — on, syncing",
        accessible_idle="sol — idle, syncing",
        icon="syncing",
        sni="Active",
        cli="Sync: syncing — pending unconfirmed until this pass finishes",
        doctor_severity="ok",
        doctor_detail="sync health: sync pass active; pending unconfirmed until check completes",
        dbus="syncing",
    ),
    HealthState.OFFLINE: HealthSurface(
        header_recording="on — offline (saving locally)",
        header_idle="idle — offline (saving locally)",
        sync_line="sync: offline; will retry",
        tooltip="sync: offline; saving locally",
        accessible_recording="sol — on, offline, saving locally",
        accessible_idle="sol — idle, offline, saving locally",
        icon="syncing",
        sni="Active",
        cli="Sync: offline — saving locally; pending unconfirmed (will retry)",
        doctor_severity="warn",
        doctor_detail="sync health: offline; pending unconfirmed; will retry",
        dbus="offline",
    ),
    HealthState.UPDATE_NEEDED: HealthSurface(
        header_recording="on — update needed",
        header_idle="idle — update needed",
        sync_line="sync: update solstone-linux",
        tooltip="sync: update needed; update solstone-linux",
        accessible_recording="sol — on, update needed",
        accessible_idle="sol — idle, update needed",
        icon="error",
        sni="NeedsAttention",
        cli="Sync: update needed — update solstone-linux; pending unconfirmed",
        doctor_severity="fail",
        doctor_detail="sync health: update needed; server route returned 404",
        dbus="update-needed",
    ),
    HealthState.REVOKED: HealthSurface(
        header_recording="on — re-auth needed",
        header_idle="idle — re-auth needed",
        sync_line="sync: re-auth required",
        tooltip="sync: access revoked; re-auth required",
        accessible_recording="sol — on, re-auth required",
        accessible_idle="sol — idle, re-auth required",
        icon="error",
        sni="NeedsAttention",
        cli="Sync: revoked — re-auth required; pending unconfirmed",
        doctor_severity="fail",
        doctor_detail="sync health: access revoked; re-auth required",
        dbus="revoked",
    ),
    HealthState.STALE: HealthSurface(
        header_recording="on — sync stale",
        header_idle="idle — sync stale",
        sync_line="sync: stale; no journal response in {contact_age}",
        tooltip="sync: stale; last contact {contact_ts}",
        accessible_recording="sol — on, sync stale",
        accessible_idle="sol — idle, sync stale",
        icon="error",
        sni="NeedsAttention",
        cli="Sync: stale — no journal response in {contact_age}; check service and journal",
        doctor_severity="fail",
        doctor_detail="sync health: stale; last contact {contact_ts}, threshold {threshold}",
        dbus="stale",
    ),
    HealthState.UNKNOWN: HealthSurface(
        header_recording="on — sync unconfirmed",
        header_idle="idle — sync unconfirmed",
        sync_line="sync: checking...",
        tooltip="sync: not confirmed yet",
        accessible_recording="sol — on, sync unconfirmed",
        accessible_idle="sol — idle, sync unconfirmed",
        icon="syncing",
        sni="Active",
        cli="Sync: unconfirmed — waiting for first successful journal check; pending unconfirmed",
        doctor_severity="warn",
        doctor_detail="sync health: unconfirmed; no successful journal check yet",
        dbus="unknown",
    ),
}


def _format_age(seconds: float | None) -> str:
    if seconds is None:
        return "unknown"
    seconds = max(0, int(seconds))
    if seconds < 60:
        return f"{seconds}s"
    minutes = seconds // 60
    if minutes < 60:
        return f"{minutes}m"
    hours = minutes // 60
    if hours < 24:
        return f"{hours}h"
    days = hours // 24
    return f"{days}d"


def _format_ts(timestamp: float | None) -> str:
    if timestamp is None:
        return "unknown"
    return datetime.fromtimestamp(timestamp).strftime("%Y-%m-%d %H:%M")


def _fill(template: str, values: dict[str, str]) -> str:
    return template.format(**values)


def derive_health(
    facts: SyncFacts,
    now: float,
    stale_threshold: float = DEFAULT_SYNC_STALE_THRESHOLD,
) -> SyncHealth:
    """Derive the sync health state and resolved surface copy from facts."""
    if facts.last_error_class == ErrorType.AUTH:
        state = HealthState.REVOKED
    elif facts.last_error_class == ErrorType.INCOMPATIBLE:
        state = HealthState.UPDATE_NEEDED
    elif (
        facts.last_successful_contact is not None
        and now - facts.last_successful_contact > stale_threshold
    ):
        state = HealthState.STALE
    elif facts.in_progress:
        state = HealthState.SYNCING
    elif facts.pending_confirmed == 0:
        state = HealthState.CONNECTED
    elif facts.last_error_class == ErrorType.TRANSIENT:
        state = HealthState.OFFLINE
    else:
        state = HealthState.UNKNOWN

    surface = SURFACE_BY_STATE[state]
    progress = facts.progress.strip() or "syncing..."
    sync_age = (
        now - facts.last_successful_sync
        if facts.last_successful_sync is not None
        else None
    )
    contact_age = (
        now - facts.last_successful_contact
        if facts.last_successful_contact is not None
        else None
    )
    values = {
        "progress": progress,
        "sync_ts": _format_ts(facts.last_successful_sync),
        "contact_ts": _format_ts(facts.last_successful_contact),
        "contact_age": _format_age(contact_age),
        "threshold": _format_age(stale_threshold),
    }
    pending_display = (
        "0 pending" if state == HealthState.CONNECTED else "pending unconfirmed"
    )

    return SyncHealth(
        state=state,
        header_recording=_fill(surface.header_recording, values),
        header_idle=_fill(surface.header_idle, values),
        sync_line=_fill(surface.sync_line, values),
        tooltip=_fill(surface.tooltip, values),
        accessible_recording=_fill(surface.accessible_recording, values),
        accessible_idle=_fill(surface.accessible_idle, values),
        icon=surface.icon,
        sni_status=surface.sni,
        cli=_fill(surface.cli, values),
        doctor_severity=surface.doctor_severity,
        doctor_detail=_fill(surface.doctor_detail, values),
        dbus=surface.dbus,
        pending_display=pending_display,
        last_success_age=sync_age,
        progress=facts.progress,
    )


def sync_health_path(state_dir: Path) -> Path:
    return state_dir / "sync_health.json"


def _parse_error_type(value: Any) -> ErrorType | None:
    if not isinstance(value, str):
        return None
    try:
        return ErrorType(value)
    except ValueError:
        return None


def _parse_optional_float(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _parse_optional_int(value: Any) -> int | None:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def load_facts(state_dir: Path) -> SyncFacts:
    path = sync_health_path(state_dir)
    if not path.exists():
        return SyncFacts()
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError):
        return SyncFacts()
    if not isinstance(data, dict):
        return SyncFacts()
    return SyncFacts(
        last_successful_sync=_parse_optional_float(data.get("last_successful_sync")),
        last_successful_contact=_parse_optional_float(
            data.get("last_successful_contact")
        ),
        last_error_class=_parse_error_type(data.get("last_error_class")),
        last_error_code=_parse_optional_int(data.get("last_error_code")),
        pending_confirmed=_parse_optional_int(data.get("pending_confirmed")),
        in_progress=bool(data.get("in_progress", False)),
        progress=str(data.get("progress", "")),
    )


def save_facts(state_dir: Path, facts: SyncFacts) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    path = sync_health_path(state_dir)
    tmp = path.with_suffix(f".{os.getpid()}.tmp")
    data = {
        "schema_version": SCHEMA_VERSION,
        "last_successful_sync": facts.last_successful_sync,
        "last_successful_contact": facts.last_successful_contact,
        "last_error_class": (
            facts.last_error_class.value if facts.last_error_class is not None else None
        ),
        "last_error_code": facts.last_error_code,
        "pending_confirmed": facts.pending_confirmed,
        "in_progress": facts.in_progress,
        "progress": facts.progress,
    }
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f)
        f.write("\n")
    os.rename(str(tmp), str(path))
