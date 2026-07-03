# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
# ruff: noqa: F722, F821
"""com.canonical.dbusmenu implementation over dbus-fast.

This implements the D-Bus menu protocol used by StatusNotifierItem
to export application menus to the desktop environment's tray host.
Both KDE Plasma and GNOME's AppIndicator extension consume this.

Reference: https://github.com/AyatanaIndicators/libdbusmenu/blob/master/libdbusmenu-glib/dbus-menu.xml
"""

import logging

from dbus_fast import PropertyAccess, Variant
from dbus_fast.service import (
    ServiceInterface,
    dbus_property,
    method,
    signal as dbus_signal,
)

log = logging.getLogger(__name__)


class MenuItem:
    """A menu item in the dbusmenu tree."""

    _next_id = 1

    def __init__(
        self,
        label="",
        icon_name="",
        enabled=True,
        visible=True,
        toggle_type="",
        toggle_state=-1,
        item_type="",
        children_display="",
        shortcut=None,
        callback=None,
    ):
        self.id = MenuItem._next_id
        MenuItem._next_id += 1
        self.label = label
        self.icon_name = icon_name
        self.enabled = enabled
        self.visible = visible
        self.toggle_type = toggle_type  # "", "checkmark", "radio"
        self.toggle_state = toggle_state  # -1 = none, 0 = off, 1 = on
        self.item_type = item_type  # "" = standard, "separator"
        self.children_display = children_display  # "" or "submenu"
        self.shortcut = shortcut
        self.callback = callback
        self.children: list["MenuItem"] = []

    def get_properties(self) -> dict:
        """Return non-default properties as a dict of Variants."""
        props = {}
        if self.label:
            props["label"] = Variant("s", self.label)
        if self.icon_name:
            props["icon-name"] = Variant("s", self.icon_name)
        # Some hosts cache booleans and won't default missing keys back to True.
        props["enabled"] = Variant("b", self.enabled)
        props["visible"] = Variant("b", self.visible)
        if self.toggle_type:
            props["toggle-type"] = Variant("s", self.toggle_type)
            props["toggle-state"] = Variant("i", self.toggle_state)
        if self.item_type:
            props["type"] = Variant("s", self.item_type)
        if self.children_display:
            props["children-display"] = Variant("s", self.children_display)
        return props


def _separator():
    """Create a separator menu item."""
    item = MenuItem(item_type="separator")
    return item


class DBusMenu(ServiceInterface):
    """com.canonical.dbusmenu service interface."""

    def __init__(self):
        super().__init__("com.canonical.dbusmenu")
        self.on_about_to_show = None
        self._props_emitted = 0
        self._revision = 1
        self._root = MenuItem()  # id 0 is root
        self._root.id = 0
        self._root.children_display = "submenu"
        self._items: dict[int, MenuItem] = {0: self._root}
        MenuItem._next_id = 1

    def set_menu(self, items: list[MenuItem]):
        """Replace the entire menu tree."""
        self._root.children = items
        self._items = {0: self._root}
        self._register_items(items)
        self._revision += 1
        self.LayoutUpdated(self._revision, 0)

    def update_properties(self, item: MenuItem, *names: str):
        if not names:
            return

        updated = {name: self._property_variant(item, name) for name in names}
        self._props_emitted += 1
        self.ItemsPropertiesUpdated([[item.id, updated]], [])

    def _register_items(self, items: list[MenuItem]):
        for item in items:
            self._items[item.id] = item
            if item.children:
                self._register_items(item.children)

    def _property_variant(self, item: MenuItem, name: str) -> Variant:
        if name == "label":
            return Variant("s", item.label)
        if name == "visible":
            return Variant("b", item.visible)
        if name == "enabled":
            return Variant("b", item.enabled)
        if name == "icon-name":
            return Variant("s", item.icon_name)
        if name == "toggle-state":
            return Variant("i", item.toggle_state)

        raise ValueError(f"unsupported menu property: {name}")

    def _build_layout(self, item: MenuItem, depth: int, props: list[str]):
        """Build the (ia{sv}av) layout tuple for GetLayout."""
        item_props = item.get_properties()
        if props:
            item_props = {k: v for k, v in item_props.items() if k in props}

        children_variants = []
        if depth != 0 and item.children:
            for child in item.children:
                child_layout = self._build_layout(
                    child,
                    depth - 1 if depth > 0 else -1,
                    props,
                )
                children_variants.append(Variant("(ia{sv}av)", child_layout))

        return [item.id, item_props, children_variants]

    # ── D-Bus Methods ──

    @method()
    def GetLayout(
        self, parent_id: "i", recursion_depth: "i", property_names: "as"
    ) -> "u(ia{sv}av)":
        parent = self._items.get(parent_id, self._root)
        layout = self._build_layout(parent, recursion_depth, property_names)
        return [self._revision, layout]

    @method()
    def GetGroupProperties(self, ids: "ai", property_names: "as") -> "a(ia{sv})":
        result = []
        for item_id in ids:
            item = self._items.get(item_id)
            if item:
                props = item.get_properties()
                if property_names:
                    props = {k: v for k, v in props.items() if k in property_names}
                result.append([item_id, props])
        return result

    @method()
    def GetProperty(self, item_id: "i", name: "s") -> "v":
        item = self._items.get(item_id)
        if item:
            props = item.get_properties()
            if name in props:
                return props[name]
        return Variant("s", "")

    @method()
    def Event(self, item_id: "i", event_id: "s", data: "v", timestamp: "u"):
        item = self._items.get(item_id)
        if item and event_id == "clicked" and item.callback:
            log.info(f"Menu item clicked: {item.label!r} (id={item_id})")
            item.callback()
        elif item:
            log.debug(f"Menu event: {event_id} on {item.label!r} (id={item_id})")

    @method()
    def EventGroup(self, events: "a(isvu)") -> "ai":
        errors = []
        for item_id, event_id, data, timestamp in events:
            item = self._items.get(item_id)
            if item and event_id == "clicked" and item.callback:
                log.info(f"Menu item clicked: {item.label!r} (id={item_id})")
                item.callback()
        return errors

    @method()
    def AboutToShow(self, item_id: "i") -> "b":
        if self.on_about_to_show is None:
            return False
        return bool(self.on_about_to_show())

    @method()
    def AboutToShowGroup(self, ids: "ai") -> "aiai":
        if self.on_about_to_show is None:
            return [[], []]
        changed = bool(self.on_about_to_show())
        return [list(ids), []] if changed else [[], []]

    # ── D-Bus Properties ──

    @dbus_property(access=PropertyAccess.READ)
    def Version(self) -> "u":
        return 3

    @dbus_property(access=PropertyAccess.READ)
    def TextDirection(self) -> "s":
        return "ltr"

    @dbus_property(access=PropertyAccess.READ)
    def Status(self) -> "s":
        return "normal"

    @dbus_property(access=PropertyAccess.READ)
    def IconThemePath(self) -> "as":
        return []

    # ── D-Bus Signals ──

    @dbus_signal()
    def ItemsPropertiesUpdated(self, updated_props, removed_props) -> "a(ia{sv})a(ias)":
        return [updated_props, removed_props]

    @dbus_signal()
    def LayoutUpdated(self, revision, parent) -> "ui":
        return [revision, parent]

    @dbus_signal()
    def ItemActivationRequested(self, item_id, timestamp) -> "iu":
        return [item_id, timestamp]


def separator():
    """Create a separator menu item."""
    return _separator()
