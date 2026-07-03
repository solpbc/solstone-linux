# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Tests for cross-desktop activity detection backends."""

import logging
import subprocess
from unittest.mock import AsyncMock, MagicMock, call

import pytest
from dbus_fast.errors import DBusError, InvalidMemberNameError

from solstone_linux import activity


def _make_proxy_with_interface(interface: MagicMock) -> MagicMock:
    proxy = MagicMock()
    proxy.get_interface.return_value = interface
    return proxy


def _make_variant(value: int) -> MagicMock:
    variant = MagicMock()
    variant.value = value
    return variant


def _service_unknown(detail: str) -> DBusError:
    return DBusError("org.freedesktop.DBus.Error.ServiceUnknown", detail)


def _no_reply(detail: str) -> DBusError:
    return DBusError("org.freedesktop.DBus.Error.NoReply", detail)


def _make_name_has_owner_bus(
    *, return_value: bool | None = None, side_effect=None
) -> tuple[MagicMock, MagicMock]:
    bus = MagicMock()
    bus.introspect = AsyncMock(return_value=object())
    iface = MagicMock()
    if side_effect is not None:
        iface.call_name_has_owner = AsyncMock(side_effect=side_effect)
    else:
        iface.call_name_has_owner = AsyncMock(return_value=return_value)
    bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)
    return bus, iface


class TestIsScreenLocked:
    """Test screen lock fallback order."""

    @pytest.fixture(autouse=True)
    def _clear_xdg_desktop(self, monkeypatch):
        monkeypatch.delenv("XDG_CURRENT_DESKTOP", raising=False)

    @pytest.mark.asyncio
    async def test_fdo_backend_returns_true_without_gnome_fallback(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_screen_locked(bus)

        assert result is True
        assert bus.introspect.await_count == 1
        bus.introspect.assert_awaited_once_with(
            activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH
        )

    @pytest.mark.asyncio
    async def test_xdg_current_desktop_ubuntu_gnome_skips_fdo_and_returns_gnome_state(
        self, monkeypatch, caplog
    ):
        monkeypatch.setenv("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        with caplog.at_level(logging.WARNING):
            result = await activity.is_screen_locked(bus)

        assert result is True
        assert bus.introspect.await_args_list == [
            call(activity.GNOME_SCREENSAVER_BUS, activity.GNOME_SCREENSAVER_PATH)
        ]
        assert not any(
            "is_screen_locked FDO backend failed" in record.message
            for record in caplog.records
        )

    @pytest.mark.asyncio
    async def test_xdg_current_desktop_kde_still_probes_fdo_first(self, monkeypatch):
        monkeypatch.setenv("XDG_CURRENT_DESKTOP", "KDE")
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_screen_locked(bus)

        assert result is True
        assert bus.introspect.await_count == 1
        bus.introspect.assert_awaited_once_with(
            activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH
        )

    @pytest.mark.asyncio
    async def test_xdg_current_desktop_not_gnome_does_not_match_substring(
        self, monkeypatch
    ):
        monkeypatch.setenv("XDG_CURRENT_DESKTOP", "NOT-GNOME")
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_screen_locked(bus)

        assert result is True
        assert bus.introspect.await_count == 1
        bus.introspect.assert_awaited_once_with(
            activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH
        )

    @pytest.mark.asyncio
    async def test_fdo_backend_returns_false_without_gnome_fallback(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_active = AsyncMock(return_value=False)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_screen_locked(bus)

        assert result is False
        assert bus.introspect.await_count == 1
        bus.introspect.assert_awaited_once_with(
            activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH
        )

    @pytest.mark.asyncio
    async def test_fdo_failure_gnome_returns_true(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[_service_unknown("fdo unavailable"), object()]
        )
        gnome_iface = MagicMock()
        gnome_iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(gnome_iface)

        result = await activity.is_screen_locked(bus)

        assert result is True
        assert bus.introspect.await_args_list == [
            call(activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH),
            call(activity.GNOME_SCREENSAVER_BUS, activity.GNOME_SCREENSAVER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_fdo_failure_gnome_returns_false(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[_service_unknown("fdo unavailable"), object()]
        )
        gnome_iface = MagicMock()
        gnome_iface.call_get_active = AsyncMock(return_value=False)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(gnome_iface)

        result = await activity.is_screen_locked(bus)

        assert result is False
        assert bus.introspect.await_args_list == [
            call(activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH),
            call(activity.GNOME_SCREENSAVER_BUS, activity.GNOME_SCREENSAVER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_both_backends_fail_returns_false(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                _service_unknown("fdo unavailable"),
                _service_unknown("gnome unavailable"),
            ]
        )

        result = await activity.is_screen_locked(bus)

        assert result is False
        assert bus.introspect.await_args_list == [
            call(activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH),
            call(activity.GNOME_SCREENSAVER_BUS, activity.GNOME_SCREENSAVER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_is_screen_locked_fdo_parser_error_falls_through_to_gnome(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[InvalidMemberNameError("bad"), object()]
        )
        gnome_iface = MagicMock()
        gnome_iface.call_get_active = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(gnome_iface)

        with caplog.at_level(logging.WARNING):
            result = await activity.is_screen_locked(bus)

        assert result is True
        assert [record.message for record in caplog.records] == [
            "is_screen_locked FDO backend failed: "
            "service=org.freedesktop.ScreenSaver path=/ScreenSaver: "
            "InvalidMemberNameError: invalid member name: bad"
        ]

    @pytest.mark.parametrize(
        "error_name",
        [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ],
    )
    @pytest.mark.asyncio
    async def test_is_screen_locked_service_missing_does_not_log(
        self, caplog, error_name
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                DBusError(error_name, "missing"),
                DBusError(error_name, "missing"),
            ]
        )

        with caplog.at_level(logging.WARNING):
            result = await activity.is_screen_locked(bus)

        assert result is False
        assert caplog.records == []

    @pytest.mark.asyncio
    async def test_is_screen_locked_both_backends_broken_logs_both_warnings(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=[_no_reply("broke"), _no_reply("broke")])

        with caplog.at_level(logging.WARNING):
            result = await activity.is_screen_locked(bus)

        assert result is False
        assert [record.message for record in caplog.records] == [
            "is_screen_locked FDO backend failed: "
            "service=org.freedesktop.ScreenSaver path=/ScreenSaver: "
            "DBusError: broke",
            "is_screen_locked GNOME backend failed: "
            "service=org.gnome.ScreenSaver path=/org/gnome/ScreenSaver: "
            "DBusError: broke",
        ]

    @pytest.mark.asyncio
    async def test_is_screen_locked_caches_and_invalidates_same_bus(self):
        activity._PROXY_CACHE.clear()
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        fdo_iface = MagicMock()
        fdo_iface.call_get_active = AsyncMock(
            side_effect=[False, False, _no_reply("broke"), False]
        )
        gnome_iface = MagicMock()
        gnome_iface.call_get_active = AsyncMock(return_value=False)

        def get_proxy_object(service, _path, _intro):
            if service == activity.FDO_SCREENSAVER_BUS:
                return _make_proxy_with_interface(fdo_iface)
            return _make_proxy_with_interface(gnome_iface)

        bus.get_proxy_object.side_effect = get_proxy_object

        assert await activity.is_screen_locked(bus) is False
        assert await activity.is_screen_locked(bus) is False
        assert bus.introspect.await_count == 1

        assert await activity.is_screen_locked(bus) is False
        assert bus.introspect.await_count == 2
        assert bus.introspect.await_args_list[-1] == call(
            activity.GNOME_SCREENSAVER_BUS, activity.GNOME_SCREENSAVER_PATH
        )

        assert await activity.is_screen_locked(bus) is False
        assert bus.introspect.await_count == 3
        assert bus.introspect.await_args_list[-1] == call(
            activity.FDO_SCREENSAVER_BUS, activity.FDO_SCREENSAVER_PATH
        )


class TestIsPowerSaveActive:
    """Test power save fallback order."""

    @pytest.fixture(autouse=True)
    def _clear_warning_cache(self, monkeypatch):
        activity._POWER_SAVE_WARNED_BACKENDS.clear()
        # Prevent the DPMS path from being entered in existing tests
        monkeypatch.delenv("XDG_SESSION_TYPE", raising=False)

    @pytest.mark.asyncio
    async def test_gnome_backend_nonzero_mode_returns_true(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get = AsyncMock(return_value=_make_variant(2))
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_power_save_active(bus)

        assert result is True
        bus.introspect.assert_awaited_once_with(
            activity.DISPLAY_CONFIG_BUS, activity.DISPLAY_CONFIG_PATH
        )

    @pytest.mark.asyncio
    async def test_gnome_backend_zero_mode_returns_false(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get = AsyncMock(return_value=_make_variant(0))
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.is_power_save_active(bus)

        assert result is False
        bus.introspect.assert_awaited_once_with(
            activity.DISPLAY_CONFIG_BUS, activity.DISPLAY_CONFIG_PATH
        )

    @pytest.mark.asyncio
    async def test_gnome_failure_kde_lid_closed_returns_true(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[_service_unknown("gnome unavailable"), object()]
        )
        kde_iface = MagicMock()
        kde_iface.call_is_lid_closed = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(kde_iface)

        result = await activity.is_power_save_active(bus)

        assert result is True
        assert bus.introspect.await_args_list == [
            call(activity.DISPLAY_CONFIG_BUS, activity.DISPLAY_CONFIG_PATH),
            call(activity.KDE_POWER_BUS, activity.KDE_POWER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_gnome_failure_kde_lid_open_returns_false(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[_service_unknown("gnome unavailable"), object()]
        )
        kde_iface = MagicMock()
        kde_iface.call_is_lid_closed = AsyncMock(return_value=False)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(kde_iface)

        result = await activity.is_power_save_active(bus)

        assert result is False
        assert bus.introspect.await_args_list == [
            call(activity.DISPLAY_CONFIG_BUS, activity.DISPLAY_CONFIG_PATH),
            call(activity.KDE_POWER_BUS, activity.KDE_POWER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_both_backends_fail_returns_false(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                _service_unknown("gnome unavailable"),
                _service_unknown("kde unavailable"),
            ]
        )

        result = await activity.is_power_save_active(bus)

        assert result is False
        assert bus.introspect.await_args_list == [
            call(activity.DISPLAY_CONFIG_BUS, activity.DISPLAY_CONFIG_PATH),
            call(activity.KDE_POWER_BUS, activity.KDE_POWER_PATH),
        ]

    @pytest.mark.asyncio
    async def test_is_power_save_active_mutter_parser_error_falls_through_to_kde(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[InvalidMemberNameError("bad"), object()]
        )
        kde_iface = MagicMock()
        kde_iface.call_is_lid_closed = AsyncMock(return_value=True)
        bus.get_proxy_object.return_value = _make_proxy_with_interface(kde_iface)

        with caplog.at_level(logging.WARNING):
            result = await activity.is_power_save_active(bus)

        assert result is True
        assert [record.message for record in caplog.records] == [
            "is_power_save_active Mutter backend failed: "
            "service=org.gnome.Mutter.DisplayConfig "
            "path=/org/gnome/Mutter/DisplayConfig: "
            "InvalidMemberNameError: invalid member name: bad"
        ]

    @pytest.mark.parametrize(
        "error_name",
        [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ],
    )
    @pytest.mark.asyncio
    async def test_is_power_save_active_service_missing_does_not_log(
        self, caplog, error_name
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                DBusError(error_name, "missing"),
                DBusError(error_name, "missing"),
            ]
        )

        with caplog.at_level(logging.WARNING):
            result = await activity.is_power_save_active(bus)

        assert result is False
        assert caplog.records == []

    @pytest.mark.asyncio
    async def test_is_power_save_active_both_backends_broken_logs_both_warnings(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=[_no_reply("broke"), _no_reply("broke")])

        with caplog.at_level(logging.WARNING):
            result = await activity.is_power_save_active(bus)

        assert result is False
        assert [record.message for record in caplog.records] == [
            "is_power_save_active Mutter backend failed: "
            "service=org.gnome.Mutter.DisplayConfig "
            "path=/org/gnome/Mutter/DisplayConfig: DBusError: broke",
            "is_power_save_active KDE backend failed: "
            "service=org.kde.Solid.PowerManagement "
            "path=/org/kde/Solid/PowerManagement: DBusError: broke",
        ]

    @pytest.mark.asyncio
    async def test_is_power_save_active_repeated_backend_failures_log_debug_after_first(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=[_no_reply("broke")] * 4)

        with caplog.at_level(logging.DEBUG):
            assert await activity.is_power_save_active(bus) is False
            assert await activity.is_power_save_active(bus) is False

        warnings = [
            record.message
            for record in caplog.records
            if record.levelno == logging.WARNING
        ]
        debug = [
            record.message
            for record in caplog.records
            if record.levelno == logging.DEBUG
        ]
        assert warnings == [
            "is_power_save_active Mutter backend failed: "
            "service=org.gnome.Mutter.DisplayConfig "
            "path=/org/gnome/Mutter/DisplayConfig: DBusError: broke",
            "is_power_save_active KDE backend failed: "
            "service=org.kde.Solid.PowerManagement "
            "path=/org/kde/Solid/PowerManagement: DBusError: broke",
        ]
        assert debug == warnings

    @pytest.mark.asyncio
    async def test_both_backends_fail_x11_falls_back_to_dpms_active(self, monkeypatch):
        monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                _service_unknown("gnome unavailable"),
                _service_unknown("kde unavailable"),
            ]
        )
        monkeypatch.setattr(activity, "is_dpms_active", AsyncMock(return_value=True))

        result = await activity.is_power_save_active(bus)

        assert result is True

    @pytest.mark.asyncio
    async def test_both_backends_fail_x11_dpms_returns_false(self, monkeypatch):
        monkeypatch.setenv("XDG_SESSION_TYPE", "x11")
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                _service_unknown("gnome unavailable"),
                _service_unknown("kde unavailable"),
            ]
        )
        monkeypatch.setattr(activity, "is_dpms_active", AsyncMock(return_value=False))

        result = await activity.is_power_save_active(bus)

        assert result is False

    @pytest.mark.asyncio
    async def test_both_backends_fail_non_x11_skips_dpms(self, monkeypatch):
        monkeypatch.setenv("XDG_SESSION_TYPE", "wayland")
        bus = MagicMock()
        bus.introspect = AsyncMock(
            side_effect=[
                _service_unknown("gnome unavailable"),
                _service_unknown("kde unavailable"),
            ]
        )
        mock_dpms = AsyncMock(return_value=True)
        monkeypatch.setattr(activity, "is_dpms_active", mock_dpms)

        result = await activity.is_power_save_active(bus)

        assert result is False
        mock_dpms.assert_not_called()


class TestProbeActivityServices:
    """Test activity backend probing and logging."""

    @pytest.mark.asyncio
    async def test_all_services_available_returns_true_results(self):
        bus, _ = _make_name_has_owner_bus(return_value=True)

        results = await activity.probe_activity_services(bus)

        assert results["fdo_screensaver"] is True
        assert results["gnome_screensaver"] is True
        assert results["gnome_display_config"] is True
        assert results["kde_power"] is True
        assert results["kscreen"] is True
        assert results["gtk4"] is activity._HAS_GTK

    @pytest.mark.asyncio
    async def test_no_services_available_logs_warning(self, caplog):
        bus, _ = _make_name_has_owner_bus(return_value=False)

        with caplog.at_level(logging.WARNING):
            results = await activity.probe_activity_services(bus)

        assert results["fdo_screensaver"] is False
        assert results["gnome_screensaver"] is False
        assert results["gnome_display_config"] is False
        assert results["kde_power"] is False
        assert results["kscreen"] is False
        assert "No activity backends available" in caplog.text

    @pytest.mark.asyncio
    async def test_mixed_service_availability_returns_correct_results(self):
        bus, _ = _make_name_has_owner_bus(side_effect=[True, False, True, False, True])

        results = await activity.probe_activity_services(bus)

        assert results["fdo_screensaver"] is True
        assert results["gnome_screensaver"] is False
        assert results["gnome_display_config"] is True
        assert results["kde_power"] is False
        assert results["kscreen"] is True

    @pytest.mark.asyncio
    async def test_probe_activity_services_parser_error_on_one_service_logs_and_continues(
        self, caplog
    ):
        bus, _ = _make_name_has_owner_bus(
            side_effect=[True, InvalidMemberNameError("bad"), True, True, True]
        )

        with caplog.at_level(logging.INFO):
            results = await activity.probe_activity_services(bus)

        assert results["fdo_screensaver"] is True
        assert results["gnome_screensaver"] is False
        assert results["gnome_display_config"] is True
        assert results["kde_power"] is True
        assert results["kscreen"] is True
        assert results["gtk4"] is activity._HAS_GTK
        assert "dpms" in results
        messages = [record.message for record in caplog.records]
        assert (
            "NameHasOwner probe failed: service=org.gnome.ScreenSaver "
            "path=/org/freedesktop/DBus: "
            "InvalidMemberNameError: invalid member name: bad"
        ) in messages
        assert any(message.startswith("Screen lock backends:") for message in messages)
        assert any(message.startswith("Power save backends:") for message in messages)
        assert any(message.startswith("Monitor backends:") for message in messages)


class TestGetMonitorGeometriesKscreen:
    """Test KDE KScreen monitor geometry detection."""

    @pytest.mark.asyncio
    async def test_returns_monitors_from_kscreen_dbus(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_config = AsyncMock(
            return_value={
                "outputs": {
                    1: {
                        "enabled": True,
                        "connected": True,
                        "name": "DP-1",
                        "pos": {"x": 0, "y": 0},
                        "size": {"width": 1920, "height": 1080},
                        "scale": 1.0,
                    },
                    2: {
                        "enabled": True,
                        "connected": True,
                        "name": "DP-2",
                        "pos": {"x": 1920, "y": 0},
                        "size": {"width": 2560, "height": 1440},
                        "scale": 1.0,
                    },
                }
            }
        )
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "left"},
            {"id": "DP-2", "box": [1920, 0, 4480, 1440], "position": "right"},
        ]
        bus.introspect.assert_awaited_once_with(
            activity.KSCREEN_BUS, activity.KSCREEN_PATH
        )

    @pytest.mark.asyncio
    async def test_skips_disabled_outputs(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_config = AsyncMock(
            return_value={
                "outputs": {
                    1: {
                        "enabled": True,
                        "connected": True,
                        "name": "DP-1",
                        "pos": {"x": 0, "y": 0},
                        "size": {"width": 1920, "height": 1080},
                        "scale": 1.0,
                    },
                    2: {
                        "enabled": False,
                        "connected": True,
                        "name": "DP-2",
                        "pos": {"x": 1920, "y": 0},
                        "size": {"width": 2560, "height": 1440},
                        "scale": 1.0,
                    },
                }
            }
        )
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "center"}
        ]

    @pytest.mark.asyncio
    async def test_returns_empty_on_dbus_failure(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=_service_unknown("missing"))

        result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == []

    @pytest.mark.asyncio
    async def test_applies_scale_factor(self):
        bus = MagicMock()
        bus.introspect = AsyncMock(return_value=object())
        iface = MagicMock()
        iface.call_get_config = AsyncMock(
            return_value={
                "outputs": {
                    1: {
                        "enabled": True,
                        "connected": True,
                        "name": "DP-1",
                        "pos": {"x": 0, "y": 0},
                        "size": {"width": 3840, "height": 2160},
                        "scale": 2.0,
                    }
                }
            }
        )
        bus.get_proxy_object.return_value = _make_proxy_with_interface(iface)

        result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == [
            {"id": "DP-1", "box": [0, 0, 1920, 1080], "position": "center"}
        ]

    @pytest.mark.asyncio
    async def test_get_monitor_geometries_kscreen_dbus_error_logs_and_returns_empty(
        self, caplog
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=_no_reply("broke"))

        with caplog.at_level(logging.WARNING):
            result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == []
        assert [record.message for record in caplog.records] == [
            "get_monitor_geometries_kscreen failed: "
            "service=org.kde.KScreen path=/backend: DBusError: broke"
        ]

    @pytest.mark.parametrize(
        "error_name",
        [
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "org.freedesktop.DBus.Error.NameHasNoOwner",
        ],
    )
    @pytest.mark.asyncio
    async def test_get_monitor_geometries_kscreen_service_missing_does_not_log(
        self, caplog, error_name
    ):
        bus = MagicMock()
        bus.introspect = AsyncMock(side_effect=DBusError(error_name, "missing"))

        with caplog.at_level(logging.WARNING):
            result = await activity.get_monitor_geometries_kscreen(bus)

        assert result == []
        assert caplog.records == []


class TestGetMonitorGeometriesX11:
    """Test xrandr-based monitor geometry detection."""

    XRANDR_TWO_MONITORS = (
        "Screen 0: minimum 8 x 8, current 3840 x 1080, maximum 32767 x 32767\n"
        "DP-1 connected primary 1920x1080+0+0 (normal left inverted right) 527mm x 296mm\n"
        "   1920x1080     60.00*+\n"
        "DP-2 connected 1920x1080+1920+0 (normal left inverted right) 527mm x 296mm\n"
        "   1920x1080     60.00*+\n"
        "HDMI-1 disconnected (normal left inverted right)\n"
    )

    def _make_run(self, stdout, returncode=0):
        return lambda *a, **kw: subprocess.CompletedProcess(
            [], returncode=returncode, stdout=stdout, stderr=""
        )

    def test_parses_two_connected_monitors(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run(self.XRANDR_TWO_MONITORS)
        )

        result = activity.get_monitor_geometries_x11()

        assert len(result) == 2
        connectors = {m["id"] for m in result}
        assert connectors == {"DP-1", "DP-2"}

    def test_assigns_position_labels(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run(self.XRANDR_TWO_MONITORS)
        )

        result = activity.get_monitor_geometries_x11()

        positions = {m["id"]: m["position"] for m in result}
        assert positions["DP-1"] == "left"
        assert positions["DP-2"] == "right"

    def test_correct_box_coordinates(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run(self.XRANDR_TWO_MONITORS)
        )

        result = activity.get_monitor_geometries_x11()

        by_id = {m["id"]: m for m in result}
        assert by_id["DP-1"]["box"] == [0, 0, 1920, 1080]
        assert by_id["DP-2"]["box"] == [1920, 0, 3840, 1080]

    def test_skips_disconnected_monitors(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run(self.XRANDR_TWO_MONITORS)
        )

        result = activity.get_monitor_geometries_x11()

        assert all(m["id"] != "HDMI-1" for m in result)

    def test_xrandr_missing_returns_empty(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess,
            "run",
            lambda *a, **kw: (_ for _ in ()).throw(FileNotFoundError()),
        )

        result = activity.get_monitor_geometries_x11()

        assert result == []

    def test_xrandr_nonzero_returns_empty(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run("", returncode=1)
        )

        result = activity.get_monitor_geometries_x11()

        assert result == []

    def test_single_monitor_gets_center_position(self, monkeypatch):
        xrandr_out = (
            "Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767\n"
            "DP-1 connected primary 1920x1080+0+0 (normal left inverted right) 527mm x 296mm\n"
        )
        monkeypatch.setattr(activity.subprocess, "run", self._make_run(xrandr_out))

        result = activity.get_monitor_geometries_x11()

        assert len(result) == 1
        assert result[0]["position"] == "center"

    def test_negative_offset_skipped_with_warning(self, monkeypatch, caplog):
        xrandr_out = (
            "Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767\n"
            "DP-1 connected 1920x1080+-100+0 (normal left inverted right)\n"
        )
        monkeypatch.setattr(activity.subprocess, "run", self._make_run(xrandr_out))

        with caplog.at_level(logging.WARNING):
            result = activity.get_monitor_geometries_x11()

        assert result == []
        assert any("negative" in r.message for r in caplog.records)


class TestIsDpmsActive:
    """Test DPMS power-save state detection via xset."""

    def _make_run(self, stdout, returncode=0):
        return lambda *a, **kw: subprocess.CompletedProcess(
            [], returncode=returncode, stdout=stdout, stderr=""
        )

    XSET_ON = (
        "Keyboard Control:\n"
        "DPMS (Energy Star):\n"
        "  Standby: 600    Suspend: 600    Off: 600\n"
        "  DPMS is Enabled\n"
        "  Monitor is On\n"
    )
    XSET_STANDBY = "DPMS (Energy Star):\n  DPMS is Enabled\n  Monitor is Standby\n"
    XSET_OFF = "DPMS (Energy Star):\n  DPMS is Enabled\n  Monitor is Off\n"

    @pytest.mark.asyncio
    async def test_monitor_on_returns_false(self, monkeypatch):
        monkeypatch.setattr(activity.subprocess, "run", self._make_run(self.XSET_ON))

        result = await activity.is_dpms_active()

        assert result is False

    @pytest.mark.asyncio
    async def test_monitor_standby_returns_true(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run(self.XSET_STANDBY)
        )

        result = await activity.is_dpms_active()

        assert result is True

    @pytest.mark.asyncio
    async def test_monitor_off_returns_true(self, monkeypatch):
        monkeypatch.setattr(activity.subprocess, "run", self._make_run(self.XSET_OFF))

        result = await activity.is_dpms_active()

        assert result is True

    @pytest.mark.asyncio
    async def test_xset_missing_returns_false(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess,
            "run",
            lambda *a, **kw: (_ for _ in ()).throw(FileNotFoundError()),
        )

        result = await activity.is_dpms_active()

        assert result is False

    @pytest.mark.asyncio
    async def test_xset_nonzero_returns_false(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess, "run", self._make_run("", returncode=1)
        )

        result = await activity.is_dpms_active()

        assert result is False

    @pytest.mark.asyncio
    async def test_no_monitor_line_returns_false(self, monkeypatch):
        monkeypatch.setattr(
            activity.subprocess,
            "run",
            self._make_run("DPMS is Disabled\n"),
        )

        result = await activity.is_dpms_active()

        assert result is False
