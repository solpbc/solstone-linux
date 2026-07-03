# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import time
from pathlib import Path
from unittest.mock import call
from unittest.mock import MagicMock
from unittest.mock import patch

import pytest

from solstone_linux.config import Config
from solstone_linux.dbusmenu import DBusMenu, MenuItem, separator
from solstone_linux.sni import StatusNotifierItem
from solstone_linux.sync_health import ErrorType, HealthState, SyncFacts, derive_health
from solstone_linux.tray import (
    AGENT_INSTRUCTIONS,
    ICONS,
    SOURCE_DIR,
    TrayApp,
    _compute_header_label,
    resolve_icon_theme_path,
)


def _make_app(tmp_path=None):
    config = Config()
    if tmp_path:
        config.base_dir = tmp_path
        config.config_dir = tmp_path / "config"
    config.server_url = "https://test.example.com"
    observer = MagicMock()
    observer.config = config
    observer._paused = False
    observer._pause_until = 0.0
    observer.current_mode = "screencast"
    observer.segment_dir = None
    observer.interval = 300
    observer.start_at_mono = time.monotonic()
    observer._start_mono = time.monotonic()
    observer._sync = None
    observer._dbus_service = None
    observer.capture_stats = {"captures_today": 0, "total_size_mb": 0}
    bus = MagicMock()
    app = TrayApp(observer, bus)
    return app


def _health(facts=None):
    return derive_health(facts or SyncFacts(), time.time())


def _connected_health():
    return _health(SyncFacts(pending_confirmed=0, last_successful_sync=time.time()))


def _syncing_health(progress="3/10 segments"):
    return _health(SyncFacts(in_progress=True, progress=progress))


def _offline_health():
    return _health(SyncFacts(last_error_class=ErrorType.TRANSIENT))


def _prepare_open_refresh_state(app, now):
    segment_dir = Path("/tmp/test.incomplete")
    app._observer.current_mode = "screencast"
    app._observer._paused = False
    app._observer.segment_dir = segment_dir
    app._observer.start_at_mono = now - 75
    app._observer._start_mono = now - 3661
    app._observer.interval = 300
    app._observer._sync = MagicMock()
    app._observer._sync.health = _connected_health()
    app._observer.capture_stats = {"captures_today": 1, "total_size_mb": 1}
    return segment_dir


class TestResolveIconThemePath:
    def test_resolve_icon_theme_path_prefers_installed(self, tmp_path):
        installed_icon = (
            tmp_path
            / ".local/share/icons/hicolor/scalable/status/solstone-recording.svg"
        )
        installed_icon.parent.mkdir(parents=True)
        installed_icon.touch()

        with patch("solstone_linux.tray.Path.home", return_value=tmp_path):
            assert resolve_icon_theme_path() == str(tmp_path / ".local/share/icons")

    def test_resolve_icon_theme_path_contrib_fallback(self, tmp_path):
        with patch("solstone_linux.tray.Path.home", return_value=tmp_path):
            result = resolve_icon_theme_path()

        assert result.endswith("contrib/icons")
        assert (Path(result) / "hicolor").is_dir() is True


class TestTrayInit:
    def test_make_app_uses_observer_config(self):
        app = _make_app()

        assert isinstance(app, TrayApp)
        assert app.config.server_url == "https://test.example.com"
        assert app.sni is not None
        assert app.menu is not None


class TestBuildMenu:
    def test_build_menu_creates_expected_items(self):
        app = _make_app()

        app._build_menu()

        assert isinstance(app._status_item, MenuItem)
        assert app._status_item.label == "on"
        assert app._status_item.enabled is False
        assert app._sync_item.label == "sync: checking..."
        assert app._pause_submenu.children_display == "submenu"
        assert len(app._pause_submenu.children) == 4
        assert app._resume_item.visible is False
        assert app.menu._root.children[0] is app._status_header
        assert app.menu._root.children[1].item_type == separator().item_type
        assert len(app.menu._root.children) == 11


class TestUpdateStatus:
    def test_update_status_paused(self):
        app = _make_app()
        app._build_menu()
        app.menu.update_properties = MagicMock()

        app._update_status("paused", app.health)

        assert app.status == "paused"
        assert app._pause_submenu.visible is False
        assert app._resume_item.visible is True
        assert app.menu.update_properties.call_count >= 2
        assert (
            call(app._pause_submenu, "visible")
            in app.menu.update_properties.call_args_list
        )
        assert (
            call(app._resume_item, "visible", "label")
            in app.menu.update_properties.call_args_list
        )

    def test_update_status_idle(self):
        app = _make_app()
        app._build_menu()

        app._update_status("idle", app.health)

        assert app.status == "idle"

    def test_update_status_stopped_sets_attention(self):
        app = _make_app()
        app._build_menu()

        app._update_status("stopped", app.health)

        assert app.status == "stopped"
        assert app.sni._status == "NeedsAttention"

    def test_update_status_recording_uses_error_icon_when_error_set(self):
        app = _make_app()
        app._build_menu()
        app._update_status("paused", app.health)
        app.error = "Auth failed"

        app._update_status("recording", app.health)

        assert app.sni._icon_name == ICONS["error"]


class TestUpdateSync:
    def test_update_sync_signals_label_change_only_once(self):
        app = _make_app()
        app._build_menu()
        app.menu.update_properties = MagicMock()
        health = _connected_health()

        app._update_sync(health)

        app.menu.update_properties.assert_called_once_with(app._sync_item, "label")

        app.menu.update_properties.reset_mock()

        app._update_sync(health)

        app.menu.update_properties.assert_not_called()

    def test_update_sync_synced(self):
        app = _make_app()
        app._build_menu()

        app._update_sync(_connected_health())

        assert app._sync_item.label == "sync: up to date"

    def test_update_sync_syncing(self):
        app = _make_app()
        app._build_menu()

        app._update_sync(_syncing_health("3/10 segments"))

        assert app._sync_item.label == "sync: 3/10 segments"

    def test_update_sync_offline(self):
        app = _make_app()
        app._build_menu()

        app._update_sync(_offline_health())

        assert app._sync_item.label == "sync: offline; will retry"

    def test_update_sync_update_needed_sets_attention(self):
        app = _make_app()
        app._build_menu()
        health = _health(SyncFacts(last_error_class=ErrorType.INCOMPATIBLE))

        app._update_status("recording", health)
        app._update_sync(health)

        assert health.state == HealthState.UPDATE_NEEDED
        assert app.sni._status == "NeedsAttention"
        assert app.sni._icon_name == ICONS["error"]


class TestUpdateLiveStats:
    def test_update_live_stats_updates_labels(self):
        app = _make_app()
        app._build_menu()
        app.stats = {
            "captures_today": 5,
            "total_size_mb": 42,
            "uptime_seconds": 7260,
        }

        app._update_live_stats(245, 0)

        assert app._segment_item.label == "segment: 4:05 remaining"
        assert app._cache_item.label == "cache: 42 MB"
        assert app._captures_item.label == "today: 5 segments"
        assert app._uptime_item.label == "uptime: 2h 1m"

    def test_update_live_stats_skips_unchanged_menu_updates(self):
        app = _make_app()
        app._build_menu()
        app.menu.update_properties = MagicMock()
        app.stats = {
            "captures_today": 5,
            "total_size_mb": 42,
            "uptime_seconds": 7260,
        }

        app._update_live_stats(245, 0)

        assert app.menu.update_properties.call_args_list == [
            call(app._segment_item, "label"),
            call(app._cache_item, "label"),
            call(app._captures_item, "label"),
            call(app._uptime_item, "label"),
        ]

        app.menu.update_properties.reset_mock()

        app._update_live_stats(245, 0)

        app.menu.update_properties.assert_not_called()

    def test_update_live_stats_signals_resume_countdown_change_only_once(self):
        app = _make_app()
        app._build_menu()
        app.status = "paused"
        app._segment_item.label = "segment: 0:00 remaining"
        app.menu.update_properties = MagicMock()

        app._update_live_stats(0, 600)

        app.menu.update_properties.assert_called_once_with(
            app._resume_item,
            "label",
        )

        app.menu.update_properties.reset_mock()

        app._update_live_stats(0, 600)

        app.menu.update_properties.assert_not_called()


class TestHeaderLabel:
    def test_update_header_emits_label_property_update(self):
        app = _make_app()
        app._build_menu()
        app.status = "recording"
        app.menu.update_properties = MagicMock()
        offline = _offline_health()

        app._update_header(0, offline)
        app.menu.update_properties.reset_mock()

        app._update_header(0, _connected_health())

        assert (
            call(app._status_header, "label")
            in app.menu.update_properties.call_args_list
        )
        assert (
            call(app._status_item, "label") in app.menu.update_properties.call_args_list
        )
        assert app._status_header.label == "on — connected"
        assert app._status_item.label == "on — connected"

        app.menu.update_properties.reset_mock()

        app._update_header(0, _connected_health())

        app.menu.update_properties.assert_not_called()

    def test_header_recording_connected(self):
        app = _make_app()
        app._build_menu()
        app._observer._sync = MagicMock()
        app._observer._sync.health = _connected_health()

        app.update()

        assert app._status_header.label == "on — connected"
        assert app._status_item.label == "on — connected"

    def test_header_paused_with_timer(self):
        app = _make_app()
        app._build_menu()
        app._observer._paused = True
        app._observer._pause_until = 1000.0

        with patch("solstone_linux.tray.time.monotonic", return_value=100.0):
            app.update()

        assert app._status_header.label == "paused (15m remaining)"
        assert app._status_item.label == "paused (15m remaining)"

    def test_header_recording_offline(self):
        app = _make_app()
        app._build_menu()
        app._observer._sync = MagicMock()
        app._observer._sync.health = _offline_health()

        app.update()

        assert app._status_header.label == "on — offline (saving locally)"
        assert app._status_item.label == "on — offline (saving locally)"


class TestComputeHeaderLabel:
    @pytest.mark.parametrize(
        "status,health_key,pause_remaining,expected",
        [
            ("recording", "connected", 0, "on — connected"),
            ("recording", "syncing", 0, "on — syncing"),
            ("recording", "offline", 0, "on — offline (saving locally)"),
            ("idle", "connected", 0, "idle — connected"),
            ("idle", "syncing", 0, "idle — syncing"),
            ("idle", "offline", 0, "idle — offline (saving locally)"),
            ("paused", "connected", 0, "paused"),
            ("paused", "connected", 900, "paused (15m remaining)"),
            ("paused", "offline", 59, "paused (0m remaining)"),
            ("stopped", "connected", 0, "not running"),
            ("weird", "connected", 0, "weird"),
        ],
    )
    def test_compute_header_label(self, status, health_key, pause_remaining, expected):
        health = {
            "connected": _connected_health(),
            "syncing": _syncing_health(),
            "offline": _offline_health(),
        }[health_key]
        assert _compute_header_label(status, health, pause_remaining) == expected


class TestBuildTooltip:
    def test_build_tooltip_default(self):
        app = _make_app()
        app.status = "recording"

        tooltip = app._build_tooltip()

        assert tooltip.startswith("on")
        assert "sync: not confirmed yet" in tooltip

    def test_build_tooltip_stopped(self):
        app = _make_app()
        app.status = "stopped"

        tooltip = app._build_tooltip()

        assert "not running" in tooltip

    def test_build_tooltip_error(self):
        app = _make_app()
        app.error = "Auth failed"

        tooltip = app._build_tooltip()

        assert "Auth failed" in tooltip

    def test_build_tooltip_sync_progress(self):
        app = _make_app()
        app.health = _syncing_health("2/5")

        tooltip = app._build_tooltip()

        assert "sync: 2/5" in tooltip


class TestStatusNotifierItem:
    def test_accessible_desc_properties(self):
        sni = StatusNotifierItem()

        sni.set_icon_accessible_desc("sol — on")
        sni.set_attention_accessible_desc("sol — on")

        assert sni.IconAccessibleDesc == "sol — on"
        assert sni.AttentionAccessibleDesc == "sol — on"


class TestUpdate:
    def test_on_about_to_show_forces_recompute(self, tmp_path):
        app = _make_app(tmp_path)
        app._build_menu()
        now = 10_000.0
        _prepare_open_refresh_state(app, now)

        with patch("solstone_linux.tray.time.monotonic", return_value=now):
            changed = app._on_about_to_show()

        assert changed is True
        assert app.stats == {
            "captures_today": 1,
            "total_size_mb": 1,
            "uptime_seconds": 3661,
        }
        assert app._segment_item.label == "segment: 3:45 remaining"
        assert app._cache_item.label == "cache: 1 MB"
        assert app._captures_item.label == "today: 1 segments"
        assert app._uptime_item.label == "uptime: 1h 1m"
        assert app._sync_item.label == "sync: up to date"
        assert app._status_item.label == "on — connected"

    def test_about_to_show_returns_true_and_layout_has_refreshed_labels(self, tmp_path):
        app = _make_app(tmp_path)
        app._build_menu()
        now = 10_000.0
        _prepare_open_refresh_state(app, now)

        with patch("solstone_linux.tray.time.monotonic", return_value=now):
            assert DBusMenu.AboutToShow.__wrapped__(app.menu, 0) is True

        row_items = [
            app._status_item,
            app._sync_item,
            app._segment_item,
            app._cache_item,
            app._captures_item,
            app._uptime_item,
        ]
        props_by_id = {
            item_id: props
            for item_id, props in DBusMenu.GetGroupProperties.__wrapped__(
                app.menu,
                [item.id for item in row_items],
                [],
            )
        }

        for item in row_items:
            assert props_by_id[item.id]["label"].value == item.label

    def test_on_about_to_show_failure_keeps_tray_and_last_known_layout(self):
        app = _make_app()
        app._build_menu()
        app._observer._tray = app
        app.update = MagicMock(side_effect=RuntimeError("boom"))

        assert app._on_about_to_show() is False
        assert app._observer._tray is app

        props = DBusMenu.GetGroupProperties.__wrapped__(
            app.menu,
            [app._status_item.id],
            [],
        )
        assert props[0][1]["label"].value == "on"

    def test_first_update_clears_starting_tooltip(self):
        """Tray tooltip must not stay on 'starting…' after first update."""
        app = _make_app()
        app._build_menu()
        # Simulate what TrayApp.start() sets before any update
        app.sni.set_tooltip("sol", "starting…")
        app._observer.current_mode = "screencast"
        app._observer._paused = False

        app.update()

        # Tooltip body should no longer be "starting…"
        assert app.sni._tooltip_body != "starting…"

    def test_update_reads_observer_state(self):
        app = _make_app()
        app._build_menu()
        app._observer.current_mode = "screencast"
        app._observer._paused = False
        app._observer.segment_dir = Path("/tmp/test.incomplete")
        app._observer.start_at_mono = time.monotonic() - 60
        app._observer.interval = 300

        app.update()

        assert app.status == "recording"
        assert app._segment_item.label.startswith(("segment: 4:", "segment: 3:"))

    def test_update_shows_paused(self):
        app = _make_app()
        app._build_menu()
        app._observer._paused = True
        app._observer._pause_until = time.monotonic() + 600

        app.update()

        assert app.status == "paused"
        assert app._resume_item.visible is True


class TestConfigIntegration:
    def test_config_paths_use_base_dir(self, tmp_path):
        app = _make_app(tmp_path)

        assert str(app.config.captures_dir).startswith(str(tmp_path))
        assert str(app.config.config_path).startswith(str(tmp_path))

    def test_agent_instructions_template_uses_config_values(self, tmp_path):
        app = _make_app(tmp_path)

        text = AGENT_INSTRUCTIONS.format(
            source_dir=SOURCE_DIR,
            config_path=str(app.config.config_path),
            captures_dir=str(app.config.captures_dir),
        )

        assert SOURCE_DIR in text
        assert str(app.config.config_path) in text
        assert str(app.config.captures_dir) in text
