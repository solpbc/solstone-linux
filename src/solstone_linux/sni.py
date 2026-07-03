# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
# ruff: noqa: F722, F821
"""StatusNotifierItem (SNI) implementation over dbus-fast.

Implements the org.kde.StatusNotifierItem D-Bus interface for
registering a tray icon with KDE Plasma's system tray or GNOME's
AppIndicator extension. Both speak the same protocol.

The tray icon, menu, and tooltip are all rendered by the DE's
tray host — this code just exposes the data over D-Bus.
"""

import logging

from dbus_fast import PropertyAccess
from dbus_fast.aio import MessageBus
from dbus_fast.service import (
    ServiceInterface,
    dbus_property,
    method,
    signal as dbus_signal,
)

log = logging.getLogger(__name__)


class StatusNotifierItem(ServiceInterface):
    """org.kde.StatusNotifierItem D-Bus interface."""

    def __init__(self, app_id: str = "solstone-observer"):
        super().__init__("org.kde.StatusNotifierItem")
        self._id = app_id
        self._category = "ApplicationStatus"
        self._status = "Active"  # Passive, Active, NeedsAttention
        self._title = "solstone observer"
        self._icon_name = "solstone-recording"
        self._icon_accessible_desc = ""
        self._attention_icon_name = ""
        self._attention_accessible_desc = ""
        self._overlay_icon_name = ""
        self._tooltip_icon = ""
        self._tooltip_title = "solstone observer"
        self._tooltip_body = "recording"
        self._icon_theme_path = ""
        self._menu_path = "/MenuBar"
        self._item_is_menu = True

        # Callbacks
        self.on_activate = None
        self.on_secondary_activate = None
        self.on_scroll = None

    # ── Setters that emit change signals ──

    def set_icon(self, icon_name: str):
        if self._icon_name != icon_name:
            self._icon_name = icon_name
            self.NewIcon()

    def set_icon_accessible_desc(self, desc: str):
        if self._icon_accessible_desc != desc:
            self._icon_accessible_desc = desc
            self.emit_properties_changed({"IconAccessibleDesc": desc})

    def set_status(self, status: str):
        """Set Active, Passive, or NeedsAttention."""
        if self._status != status:
            self._status = status
            self.NewStatus(status)

    def set_tooltip(self, title: str, body: str, icon: str = ""):
        self._tooltip_title = title
        self._tooltip_body = body
        if icon:
            self._tooltip_icon = icon
        self.NewToolTip()

    def set_title(self, title: str):
        if self._title != title:
            self._title = title
            self.NewTitle()

    def set_attention_icon(self, icon_name: str):
        self._attention_icon_name = icon_name
        self.NewAttentionIcon()

    def set_attention_accessible_desc(self, desc: str):
        if self._attention_accessible_desc != desc:
            self._attention_accessible_desc = desc
            self.emit_properties_changed({"AttentionAccessibleDesc": desc})

    def set_overlay_icon(self, icon_name: str):
        self._overlay_icon_name = icon_name
        self.NewOverlayIcon()

    # ── D-Bus Properties ──

    @dbus_property(access=PropertyAccess.READ)
    def Category(self) -> "s":
        return self._category

    @dbus_property(access=PropertyAccess.READ)
    def Id(self) -> "s":
        return self._id

    @dbus_property(access=PropertyAccess.READ)
    def Title(self) -> "s":
        return self._title

    @dbus_property(access=PropertyAccess.READ)
    def Status(self) -> "s":
        return self._status

    @dbus_property(access=PropertyAccess.READ)
    def WindowId(self) -> "i":
        return 0

    @dbus_property(access=PropertyAccess.READ)
    def IconName(self) -> "s":
        return self._icon_name

    @dbus_property(access=PropertyAccess.READ)
    def IconAccessibleDesc(self) -> "s":
        return self._icon_accessible_desc

    @dbus_property(access=PropertyAccess.READ)
    def IconPixmap(self) -> "a(iiay)":
        return []

    @dbus_property(access=PropertyAccess.READ)
    def OverlayIconName(self) -> "s":
        return self._overlay_icon_name

    @dbus_property(access=PropertyAccess.READ)
    def OverlayIconPixmap(self) -> "a(iiay)":
        return []

    @dbus_property(access=PropertyAccess.READ)
    def AttentionIconName(self) -> "s":
        return self._attention_icon_name

    @dbus_property(access=PropertyAccess.READ)
    def AttentionAccessibleDesc(self) -> "s":
        return self._attention_accessible_desc

    @dbus_property(access=PropertyAccess.READ)
    def AttentionIconPixmap(self) -> "a(iiay)":
        return []

    @dbus_property(access=PropertyAccess.READ)
    def AttentionMovieName(self) -> "s":
        return ""

    @dbus_property(access=PropertyAccess.READ)
    def ToolTip(self) -> "(sa(iiay)ss)":
        return [
            self._tooltip_icon,  # icon name
            [],  # icon pixmaps
            self._tooltip_title,  # title
            self._tooltip_body,  # body (supports HTML on KDE)
        ]

    @dbus_property(access=PropertyAccess.READ)
    def IconThemePath(self) -> "s":
        return self._icon_theme_path

    @dbus_property(access=PropertyAccess.READ)
    def Menu(self) -> "o":
        return self._menu_path

    @dbus_property(access=PropertyAccess.READ)
    def ItemIsMenu(self) -> "b":
        return self._item_is_menu

    # ── D-Bus Methods ──

    @method()
    def ContextMenu(self, x: "i", y: "i"):
        log.debug(f"ContextMenu at ({x}, {y})")

    @method()
    def Activate(self, x: "i", y: "i"):
        log.debug(f"Activate at ({x}, {y})")
        if self.on_activate:
            self.on_activate()

    @method()
    def SecondaryActivate(self, x: "i", y: "i"):
        log.debug(f"SecondaryActivate at ({x}, {y})")
        if self.on_secondary_activate:
            self.on_secondary_activate()

    @method()
    def Scroll(self, delta: "i", orientation: "s"):
        log.debug(f"Scroll delta={delta} orientation={orientation}")
        if self.on_scroll:
            self.on_scroll(delta, orientation)

    @method()
    def ProvideXdgActivationToken(self, token: "s"):
        log.debug(f"XDG activation token: {token}")

    # ── D-Bus Signals ──

    @dbus_signal()
    def NewTitle(self):
        pass

    @dbus_signal()
    def NewIcon(self):
        pass

    @dbus_signal()
    def NewAttentionIcon(self):
        pass

    @dbus_signal()
    def NewOverlayIcon(self):
        pass

    @dbus_signal()
    def NewToolTip(self):
        pass

    @dbus_signal()
    def NewStatus(self, status) -> "s":
        return status


async def register_with_watcher(bus: MessageBus, bus_name: str):
    """Register our SNI with the StatusNotifierWatcher."""
    try:
        introspection = await bus.introspect(
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
        )
        proxy = bus.get_proxy_object(
            "org.kde.StatusNotifierWatcher",
            "/StatusNotifierWatcher",
            introspection,
        )
        watcher = proxy.get_interface("org.kde.StatusNotifierWatcher")
        await watcher.call_register_status_notifier_item(bus_name)
        log.info(f"Registered with StatusNotifierWatcher as {bus_name}")
        return True
    except Exception as e:
        log.warning(f"Failed to register with StatusNotifierWatcher: {e}")
        log.warning("Is KDE Plasma running, or the AppIndicator extension enabled?")
        return False
