# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Install prerequisite checks for solstone-linux.

Exit code rule: fail anywhere -> 1; otherwise 0. Warn does not flip exit code.
"""

from __future__ import annotations

import asyncio
import os
import shutil
import subprocess
import sys
import time
from typing import Callable, NamedTuple

from .capture_stats import compute_quarantine_stats, format_quarantine_line
from .config import load_config
from .sync_health import derive_health, load_facts

CheckResult = NamedTuple(
    "CheckResult",
    [("name", str), ("severity", str), ("detail", str)],
)

_PORTAL_CHECK_TIMEOUT_SEC: float = 2.0


def check_python_version() -> CheckResult:
    version = tuple(sys.version_info[:2])
    if version >= (3, 10):
        return CheckResult("python version", "ok", f"{version[0]}.{version[1]}")
    return CheckResult(
        "python version",
        "fail",
        f"need >=3.10, got {version[0]}.{version[1]}",
    )


def check_gtk4_typelib() -> CheckResult:
    try:
        import gi

        gi.require_version("Gtk", "4.0")
        from gi.repository import Gtk  # noqa: F401
    except (ImportError, ValueError):
        return CheckResult(
            "gtk4 typelib",
            "fail",
            "install gir1.2-gtk-4.0 (or distro equivalent)",
        )
    return CheckResult("gtk4 typelib", "ok", "Gtk 4.0 available")


def check_gstreamer() -> CheckResult:
    if shutil.which("gst-launch-1.0") is None:
        return CheckResult(
            "gstreamer",
            "fail",
            "gst-launch-1.0 not on PATH; install gstreamer1.0-tools or equivalent",
        )
    try:
        import gi

        gi.require_version("Gst", "1.0")
        from gi.repository import Gst  # noqa: F401
    except (ImportError, ValueError):
        return CheckResult("gstreamer", "fail", "gir1.2-gstreamer-1.0 missing")
    return CheckResult("gstreamer", "ok", "gst-launch-1.0 and Gst typelib available")


def check_cairo() -> CheckResult:
    try:
        import cairo  # noqa: F401
    except ImportError:
        return CheckResult(
            "cairo binding",
            "fail",
            "install python3-cairo (or distro equivalent)",
        )
    return CheckResult("cairo binding", "ok", "cairo import ok")


def check_session_type() -> CheckResult:
    session_type = os.environ.get("XDG_SESSION_TYPE", "").lower()
    if session_type == "wayland":
        return CheckResult("session type", "ok", "wayland")
    if session_type == "x11":
        return CheckResult("session type", "ok", "x11 (using ximagesrc capture)")
    if not session_type:
        return CheckResult(
            "session type",
            "warn",
            "XDG_SESSION_TYPE not set; Wayland or X11 required",
        )
    return CheckResult(
        "session type",
        "warn",
        f"unrecognized session type '{session_type}'; Wayland or X11 required",
    )


def check_pipewire() -> CheckResult:
    try:
        result = subprocess.run(
            ["pactl", "info"],
            capture_output=True,
            timeout=5,
            text=True,
        )
    except FileNotFoundError:
        return CheckResult(
            "pipewire (pactl)",
            "fail",
            "pactl missing; install pipewire-pulse or pulseaudio-utils",
        )
    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()[0] if result.stderr.strip() else ""
        return CheckResult("pipewire (pactl)", "fail", detail)
    detail = result.stdout.strip().splitlines()[0] if result.stdout.strip() else ""
    return CheckResult("pipewire (pactl)", "ok", detail)


async def check_portal() -> CheckResult:
    from dbus_fast.aio import MessageBus
    from dbus_fast.constants import BusType
    from dbus_fast.errors import AuthError, DBusError, InvalidAddressError

    async def _body() -> CheckResult:
        bus = None
        try:
            try:
                bus = await MessageBus(bus_type=BusType.SESSION).connect()
            except (OSError, AuthError, InvalidAddressError, DBusError) as e:
                return CheckResult(
                    "xdg-desktop-portal",
                    "fail",
                    f"session bus unreachable: {e}",
                )
            try:
                intro = await bus.introspect(
                    "org.freedesktop.DBus", "/org/freedesktop/DBus"
                )
                obj = bus.get_proxy_object(
                    "org.freedesktop.DBus", "/org/freedesktop/DBus", intro
                )
                iface = obj.get_interface("org.freedesktop.DBus")
                owned = await iface.call_name_has_owner(
                    "org.freedesktop.portal.Desktop"
                )
            except (DBusError, OSError) as e:
                return CheckResult(
                    "xdg-desktop-portal",
                    "fail",
                    f"session bus unreachable: {e}",
                )
            if owned:
                return CheckResult(
                    "xdg-desktop-portal",
                    "ok",
                    "org.freedesktop.portal.Desktop registered on session bus",
                )
            session_type = os.environ.get("XDG_SESSION_TYPE", "").lower()
            if session_type == "x11":
                return CheckResult(
                    "xdg-desktop-portal",
                    "warn",
                    "not registered — not needed on X11 (using ximagesrc)",
                )
            return CheckResult(
                "xdg-desktop-portal",
                "fail",
                "org.freedesktop.portal.Desktop not registered on session bus",
            )
        finally:
            if bus is not None:
                bus.disconnect()

    try:
        return await asyncio.wait_for(_body(), timeout=_PORTAL_CHECK_TIMEOUT_SEC)
    except asyncio.TimeoutError:
        return CheckResult(
            "xdg-desktop-portal",
            "fail",
            f"timed out after {_PORTAL_CHECK_TIMEOUT_SEC:g}s",
        )


def check_x11_capture() -> CheckResult:
    session_type = os.environ.get("XDG_SESSION_TYPE", "").lower()
    if session_type == "wayland":
        return CheckResult("x11 capture", "ok", "not applicable (wayland session)")
    if not os.environ.get("DISPLAY"):
        if session_type != "x11":
            return CheckResult("x11 capture", "ok", "not applicable (no X11 display)")
        return CheckResult("x11 capture", "fail", "DISPLAY not set")
    if shutil.which("xrandr") is None:
        return CheckResult(
            "x11 capture",
            "fail",
            "xrandr not on PATH; install x11-xserver-utils or equivalent",
        )
    try:
        result = subprocess.run(
            ["gst-inspect-1.0", "ximagesrc"],
            capture_output=True,
            timeout=5,
        )
        if result.returncode != 0:
            return CheckResult(
                "x11 capture",
                "fail",
                "ximagesrc plugin missing; install gstreamer1.0-plugins-good",
            )
    except FileNotFoundError:
        return CheckResult(
            "x11 capture",
            "warn",
            "could not verify ximagesrc (gst-inspect-1.0 not found)",
        )
    except subprocess.TimeoutExpired:
        return CheckResult(
            "x11 capture",
            "warn",
            "could not verify ximagesrc (gst-inspect-1.0 timed out)",
        )
    return CheckResult("x11 capture", "ok", "xrandr and ximagesrc available")


def check_user_systemd() -> CheckResult:
    try:
        result = subprocess.run(
            ["systemctl", "--user", "is-system-running"],
            capture_output=True,
            timeout=5,
            text=True,
        )
    except FileNotFoundError:
        return CheckResult(
            "systemd --user",
            "fail",
            "systemctl --user not reachable",
        )
    detail = result.stdout.strip().splitlines()[0] if result.stdout.strip() else ""
    if detail:
        return CheckResult("systemd --user", "ok", detail)
    return CheckResult("systemd --user", "fail", "systemctl --user not reachable")


def check_pipx() -> CheckResult:
    if shutil.which("pipx") is None:
        return CheckResult(
            "pipx",
            "fail",
            "pipx missing; install via 'python3 -m pip install --user pipx' or distro package",
        )
    return CheckResult("pipx", "ok", "pipx on PATH")


def check_appindicator_ext() -> CheckResult:
    desktop = os.environ.get("XDG_CURRENT_DESKTOP", "")
    if "GNOME" not in desktop:
        return CheckResult(
            "appindicator ext (soft)",
            "ok",
            "not applicable (non-GNOME desktop)",
        )
    try:
        result = subprocess.run(
            ["gnome-extensions", "list"],
            capture_output=True,
            timeout=5,
            text=True,
        )
    except FileNotFoundError:
        return CheckResult(
            "appindicator ext (soft)",
            "warn",
            "install gnome-shell-extension-appindicator",
        )
    if "appindicator" in result.stdout.lower():
        return CheckResult(
            "appindicator ext (soft)", "ok", "appindicator extension present"
        )
    return CheckResult(
        "appindicator ext (soft)",
        "warn",
        "install gnome-shell-extension-appindicator",
    )


def check_sync_health() -> CheckResult:
    config = load_config()
    facts = load_facts(config.state_dir)
    health = derive_health(facts, time.time(), config.sync_stale_threshold)
    return CheckResult("sync health", health.doctor_severity, health.doctor_detail)


def run_doctor() -> int:
    checks: list[tuple[str, Callable[[], CheckResult]]] = [
        ("python version", check_python_version),
        ("session type", check_session_type),
        ("gtk4 typelib", check_gtk4_typelib),
        ("gstreamer", check_gstreamer),
        ("cairo binding", check_cairo),
        ("pipewire (pactl)", check_pipewire),
        ("xdg-desktop-portal", lambda: asyncio.run(check_portal())),
        ("x11 capture", check_x11_capture),
        ("systemd --user", check_user_systemd),
        ("sync health", check_sync_health),
        ("pipx", check_pipx),
        ("appindicator ext (soft)", check_appindicator_ext),
    ]
    fail_count = 0
    warn_count = 0

    for name, fn in checks:
        try:
            result = fn()
        except Exception as e:
            result = CheckResult(name, "fail", repr(e))
        if not result.name or result.name != name:
            result = CheckResult(name, result.severity, result.detail)
        print(f"{result.severity:<4}  {result.name:<28}  {result.detail}")
        if result.severity == "fail":
            fail_count += 1
        elif result.severity == "warn":
            warn_count += 1

    try:
        q_line = format_quarantine_line(
            compute_quarantine_stats(load_config().captures_dir)
        )
    except Exception:
        q_line = None
    if q_line:
        print(q_line)

    print()
    print(f"doctor: {len(checks)} checks, {fail_count} failed, {warn_count} warnings")
    return 1 if fail_count else 0
