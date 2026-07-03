# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import argparse
import time
from unittest.mock import MagicMock

import pytest
from dbus_fast.service import ServiceInterface

from solstone_linux import cli as cli_module
from solstone_linux import doctor
from solstone_linux.cli import cmd_status
from solstone_linux.config import Config
from solstone_linux.dbus_service import ObserverService
from solstone_linux.sync import SyncService
from solstone_linux.sync_health import (
    ErrorType,
    HealthState,
    SyncFacts,
    derive_health,
    save_facts,
)
from solstone_linux.tray import TrayApp
from solstone_linux.upload import QueryResult, UploadClient


class _FakeSync:
    def __init__(self, health):
        self.health = health
        self.progress = health.progress


def _get_prop(service, name):
    for prop in ServiceInterface._get_properties(service):
        if prop.name == name:
            return prop.prop_getter(service)
    raise KeyError(name)


def _make_observer(config: Config, health):
    observer = MagicMock()
    observer.config = config
    observer._paused = False
    observer._pause_until = 0.0
    observer.current_mode = "screencast"
    observer.segment_dir = None
    observer.interval = 300
    observer.start_at_mono = time.monotonic()
    observer._start_mono = time.monotonic()
    observer.stream = "test-stream"
    observer._sync = _FakeSync(health)
    observer._dbus_service = None
    observer.capture_stats = {"captures_today": 0, "total_size_mb": 0}
    return observer


@pytest.mark.parametrize(
    "facts,expected_header,expected_sni,expected_dbus,expected_cli,expected_doctor",
    [
        (
            SyncFacts(
                pending_confirmed=0,
                last_successful_sync=1_800_000_000.0,
                last_successful_contact=1_800_000_000.0,
            ),
            "observing — connected",
            "Active",
            "connected",
            "Sync: connected — up to date (0 pending)",
            "ok",
        ),
        (
            SyncFacts(last_error_class=ErrorType.INCOMPATIBLE, last_error_code=404),
            "observing — update needed",
            "NeedsAttention",
            "update-needed",
            "Sync: update needed — update solstone-linux; pending unconfirmed",
            "fail",
        ),
    ],
)
def test_health_facts_drive_all_surfaces_consistently(
    tmp_path,
    monkeypatch,
    capsys,
    facts,
    expected_header,
    expected_sni,
    expected_dbus,
    expected_cli,
    expected_doctor,
):
    config = Config(
        base_dir=tmp_path,
        server_url="https://test.example.com",
        key="K123456789",
        stream="test-stream",
    )
    config.ensure_dirs()
    save_facts(config.state_dir, facts)
    monkeypatch.setattr(cli_module, "load_config", lambda: config)
    monkeypatch.setattr(doctor, "load_config", lambda: config)
    monkeypatch.setattr(
        cli_module.subprocess,
        "run",
        MagicMock(return_value=MagicMock(stdout="active\n")),
    )

    health = derive_health(facts, time.time(), config.sync_stale_threshold)
    observer = _make_observer(config, health)

    app = TrayApp(observer, MagicMock())
    app._build_menu()
    app.update()

    assert app._status_header.label == expected_header
    assert app._sync_item.label == health.sync_line
    assert health.tooltip in app.sni._tooltip_body
    assert app.sni._icon_accessible_desc in (
        health.accessible_recording,
        health.accessible_idle,
    )
    assert app.sni._status == expected_sni

    service = ObserverService(observer)
    assert _get_prop(service, "SyncStatus") == expected_dbus

    assert cmd_status(argparse.Namespace()) == 0
    assert expected_cli in capsys.readouterr().out

    doctor_result = doctor.check_sync_health()
    assert doctor_result.severity == expected_doctor


@pytest.mark.asyncio
async def test_404_query_cycle_drives_failing_state_on_all_surfaces(
    tmp_path, monkeypatch, capsys
):
    config = Config(
        base_dir=tmp_path,
        server_url="https://test.example.com",
        key="K123456789",
        stream="test-stream",
    )
    config.ensure_dirs()
    client = UploadClient(config)
    client.get_server_segments = MagicMock(
        return_value=QueryResult(None, ErrorType.INCOMPATIBLE, 404)
    )
    client.upload_segment = MagicMock()
    sync = SyncService(config, client)

    await sync._sync()

    assert sync.health.state == HealthState.UPDATE_NEEDED
    assert sync.health.pending_display == "pending unconfirmed"

    monkeypatch.setattr(cli_module, "load_config", lambda: config)
    monkeypatch.setattr(doctor, "load_config", lambda: config)
    monkeypatch.setattr(
        cli_module.subprocess,
        "run",
        MagicMock(return_value=MagicMock(stdout="active\n")),
    )
    observer = _make_observer(config, sync.health)

    app = TrayApp(observer, MagicMock())
    app._build_menu()
    app.update()

    assert app._status_header.label == "observing — update needed"
    assert app._status_header.label != "observing — connected"
    assert app._sync_item.label == "sync: update solstone-linux"
    assert "sync: update needed; update solstone-linux" in app.sni._tooltip_body
    assert app.sni._icon_accessible_desc == (
        "Solstone observer — observing, update needed"
    )
    assert app.sni._status == "NeedsAttention"

    service = ObserverService(observer)
    assert _get_prop(service, "SyncStatus") == "update-needed"

    assert cmd_status(argparse.Namespace()) == 0
    assert (
        "Sync: update needed — update solstone-linux; pending unconfirmed"
        in capsys.readouterr().out
    )

    doctor_result = doctor.check_sync_health()
    assert doctor_result.severity == "fail"
    assert "update needed" in doctor_result.detail
