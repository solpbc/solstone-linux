# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

from unittest.mock import MagicMock

from dbus_fast import Variant

from solstone_linux.dbusmenu import DBusMenu, MenuItem


def test_default_emits_enabled_and_visible_true():
    props = MenuItem(label="foo").get_properties()

    assert props["enabled"] == Variant("b", True)
    assert props["visible"] == Variant("b", True)


def test_explicit_false_emits_false():
    props = MenuItem(label="x", enabled=False, visible=False).get_properties()

    assert props["enabled"] == Variant("b", False)
    assert props["visible"] == Variant("b", False)


def test_toggle_true_after_false_still_emits():
    item = MenuItem(label="x", enabled=False, visible=False)
    item.enabled = True
    item.visible = True

    props = item.get_properties()

    assert props["enabled"] == Variant("b", True)
    assert props["visible"] == Variant("b", True)


def test_other_keys_still_conditional():
    props = MenuItem().get_properties()

    assert "icon-name" not in props
    assert "toggle-type" not in props
    assert "children-display" not in props


def test_update_properties_emits_items_properties_updated():
    menu = DBusMenu()
    item = MenuItem(label="resume", visible=True)
    menu.set_menu([item])
    menu.ItemsPropertiesUpdated = MagicMock()
    menu.LayoutUpdated = MagicMock()
    revision = menu._revision
    item.visible = False

    menu.update_properties(item, "visible")

    menu.ItemsPropertiesUpdated.assert_called_once()
    menu.LayoutUpdated.assert_not_called()
    assert menu._revision == revision
    assert menu._props_emitted == 1

    updated_props, removed_props = menu.ItemsPropertiesUpdated.call_args.args
    assert removed_props == []
    assert len(updated_props) == 1
    item_id, props = updated_props[0]
    assert item_id == item.id
    assert props.keys() == {"visible"}
    assert props["visible"].signature == "b"
    assert props["visible"].value is False


def test_update_properties_noop_when_no_names():
    menu = DBusMenu()
    item = MenuItem(label="resume")
    menu.set_menu([item])
    menu.ItemsPropertiesUpdated = MagicMock()
    menu.LayoutUpdated = MagicMock()
    revision = menu._revision

    menu.update_properties(item)

    menu.ItemsPropertiesUpdated.assert_not_called()
    menu.LayoutUpdated.assert_not_called()
    assert menu._revision == revision
    assert menu._props_emitted == 0


def test_about_to_show_uses_optional_hook():
    menu = DBusMenu()

    assert DBusMenu.AboutToShow.__wrapped__(menu, 0) is False

    menu.on_about_to_show = lambda: True
    assert DBusMenu.AboutToShow.__wrapped__(menu, 0) is True

    menu.on_about_to_show = lambda: False
    assert DBusMenu.AboutToShow.__wrapped__(menu, 0) is False


def test_about_to_show_group_uses_optional_hook():
    menu = DBusMenu()
    ids = [1, 2, 3]

    assert DBusMenu.AboutToShowGroup.__wrapped__(menu, ids) == [[], []]

    menu.on_about_to_show = lambda: True
    assert DBusMenu.AboutToShowGroup.__wrapped__(menu, ids) == [ids, []]

    menu.on_about_to_show = lambda: False
    assert DBusMenu.AboutToShowGroup.__wrapped__(menu, ids) == [[], []]
