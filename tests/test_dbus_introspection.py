# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

import time
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import MagicMock

import pytest
from dbus_fast import introspection

from solstone_linux.dbus_service import ObserverService
from solstone_linux.dbusmenu import DBusMenu
from solstone_linux.sni import StatusNotifierItem


HYPHEN_XML = """<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.portal.Settings">
    <property name="power-saver-enabled" type="b" access="read"/>
  </interface>
</node>"""


def _make_observer():
    observer = MagicMock()
    observer._paused = False
    observer._pause_until = 0.0
    observer.current_mode = "screencast"
    observer.config = MagicMock()
    observer.config.captures_dir = Path("/tmp/test-captures")
    observer.config.server_url = "https://test.example.com"
    observer.interval = 300
    observer.segment_dir = None
    observer.start_at_mono = time.monotonic()
    observer._start_mono = time.monotonic()
    observer.stream = "test-stream"
    observer._sync = None
    observer.capture_stats = {"captures_today": 0, "total_size_mb": 0}
    return observer


def normalize(xml_str: str):
    root = ET.fromstring(xml_str)
    interfaces = root.findall("interface") if root.tag == "node" else [root]
    assert len(interfaces) == 1
    interface = interfaces[0]

    def args_for(member):
        return [
            (arg.attrib.get("direction", ""), arg.attrib["type"])
            for arg in member.findall("arg")
        ]

    return {
        "interface": interface.attrib["name"],
        "methods": {
            method.attrib["name"]: args_for(method)
            for method in interface.findall("method")
        },
        "signals": {
            signal.attrib["name"]: args_for(signal)
            for signal in interface.findall("signal")
        },
        "properties": {
            prop.attrib["name"]: (prop.attrib["type"], prop.attrib["access"])
            for prop in interface.findall("property")
        },
    }


def test_hyphenated_portal_property_names_parse_without_monkeypatch():
    # dbus-fast tolerates hyphenated members natively; if a strict validator returns, this fails before screencast.py needs a monkeypatch.
    node = introspection.Node.parse(HYPHEN_XML)

    properties = [
        prop.name for interface in node.interfaces for prop in interface.properties
    ]
    assert "power-saver-enabled" in properties


@pytest.mark.parametrize(
    ("service_factory", "fixture_name"),
    [
        (lambda: ObserverService(_make_observer()), "observer1.xml"),
        (StatusNotifierItem, "status_notifier_item.xml"),
        (DBusMenu, "dbusmenu.xml"),
    ],
    ids=["observer1", "status-notifier-item", "dbusmenu"],
)
def test_served_introspection_matches_legacy_baseline(service_factory, fixture_name):
    fixture_path = Path(__file__).parent / "fixtures" / "introspection" / fixture_name
    baseline_xml = fixture_path.read_text(encoding="utf-8")
    service = service_factory()
    dbus_fast_xml = introspection.Node(interfaces=[service.introspect()]).tostring()

    assert normalize(dbus_fast_xml) == normalize(baseline_xml)
