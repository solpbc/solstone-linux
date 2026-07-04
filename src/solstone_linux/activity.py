# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

"""Activity detection using DBus APIs.

Detects screen lock and display power-save state via DBus, with ordered
fallback chains that cover GNOME and KDE desktops. Every function
degrades gracefully — returning a safe default — so the observer keeps
running regardless of desktop environment.
"""

import asyncio
import logging
import os
import re
import shutil
import subprocess

from dbus_fast import Variant
from dbus_fast.aio import MessageBus
from dbus_fast.errors import (
    DBusError,
    InvalidIntrospectionError,
    InvalidMemberNameError,
)

logger = logging.getLogger(__name__)

_DBUS_PROBE_TIMEOUT_SEC = 2.0
_POWER_SAVE_WARNED_BACKENDS: set[str] = set()
# One session bus lives for the whole process, so id(bus) is stable and
# never recycled — a plain dict needs no eviction. Keyed by bus identity
# because dbus-fast's cython MessageBus cannot be weak-referenced.
_PROXY_CACHE: dict = {}

_SERVICE_MISSING_ERRORS = (
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NameHasNoOwner",
)

# GTK4/GDK4 — optional, only needed for monitor geometry detection.
# On systems without GTK4, get_monitor_geometries() will raise RuntimeError
# but screencast recording still works (monitors labeled as "monitor-N").
try:
    import gi

    gi.require_version("Gdk", "4.0")
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gdk, Gtk

    _HAS_GTK = True
except (ImportError, ValueError):
    _HAS_GTK = False

# DBus service constants — screen lock
FDO_SCREENSAVER_BUS = "org.freedesktop.ScreenSaver"
FDO_SCREENSAVER_PATH = "/ScreenSaver"
FDO_SCREENSAVER_IFACE = "org.freedesktop.ScreenSaver"

GNOME_SCREENSAVER_BUS = "org.gnome.ScreenSaver"
GNOME_SCREENSAVER_PATH = "/org/gnome/ScreenSaver"
GNOME_SCREENSAVER_IFACE = "org.gnome.ScreenSaver"

# DBus service constants — power save
DISPLAY_CONFIG_BUS = "org.gnome.Mutter.DisplayConfig"
DISPLAY_CONFIG_PATH = "/org/gnome/Mutter/DisplayConfig"
DISPLAY_CONFIG_IFACE = "org.gnome.Mutter.DisplayConfig"

# DBus service constants — monitor geometry (KDE)
KSCREEN_BUS = "org.kde.KScreen"
KSCREEN_PATH = "/backend"
KSCREEN_IFACE = "org.kde.kscreen.Backend"


def _is_service_missing(exc: BaseException) -> bool:
    """True if exc is a DBusError meaning the bus name is not currently owned."""
    return (
        isinstance(exc, DBusError)
        and getattr(exc, "type", "") in _SERVICE_MISSING_ERRORS
    )


def _is_gnome_desktop() -> bool:
    """True if any XDG_CURRENT_DESKTOP token equals 'gnome' (case-insensitive)."""
    return any(
        token.strip().casefold() == "gnome"
        for token in os.environ.get("XDG_CURRENT_DESKTOP", "").split(":")
    )


async def _cached_interface(bus: MessageBus, service: str, path: str, iface_name: str):
    key = (id(bus), service, path, iface_name)
    if key in _PROXY_CACHE:
        return _PROXY_CACHE[key]
    intro = await bus.introspect(service, path)
    obj = bus.get_proxy_object(service, path, intro)
    iface = obj.get_interface(iface_name)
    _PROXY_CACHE[key] = iface
    return iface


def _invalidate_interface(
    bus: MessageBus, service: str, path: str, iface_name: str
) -> None:
    _PROXY_CACHE.pop((id(bus), service, path, iface_name), None)


async def _name_has_owner(bus: MessageBus, bus_name: str) -> bool:
    """Ask the bus daemon whether a well-known name is currently owned.

    Returns False on any probe failure (daemon unreachable, timeout, parser
    error) after logging a warning — the service is treated as absent.
    """

    async def _probe() -> bool:
        intro = await bus.introspect("org.freedesktop.DBus", "/org/freedesktop/DBus")
        obj = bus.get_proxy_object(
            "org.freedesktop.DBus", "/org/freedesktop/DBus", intro
        )
        iface = obj.get_interface("org.freedesktop.DBus")
        return bool(await iface.call_name_has_owner(bus_name))

    try:
        return await asyncio.wait_for(_probe(), timeout=_DBUS_PROBE_TIMEOUT_SEC)
    except (DBusError, InvalidMemberNameError, OSError, asyncio.TimeoutError) as exc:
        logger.warning(
            "NameHasOwner probe failed: service=%s path=%s: %s: %s",
            bus_name,
            "/org/freedesktop/DBus",
            type(exc).__name__,
            exc,
        )
        return False


def get_monitor_geometries_x11() -> list[dict]:
    """Get monitor geometry from xrandr (X11 only).

    Returns:
        List of dicts with format:
        [{"id": "connector-id", "box": [x1, y1, x2, y2], "position": "..."}, ...]
        Empty list if xrandr is unavailable or returns no connected monitors.
    """
    try:
        result = subprocess.run(
            ["xrandr"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return []

    if result.returncode != 0:
        return []

    from .monitor_positions import assign_monitor_positions

    monitors = []
    for line in result.stdout.splitlines():
        if " connected" not in line or "disconnected" in line:
            continue
        parts = line.split()
        name = parts[0]
        for part in parts:
            m = re.match(r"(\d+)x(\d+)\+(-?\d+)\+(-?\d+)", part)
            if m:
                w, h = int(m.group(1)), int(m.group(2))
                x, y = int(m.group(3)), int(m.group(4))
                if x < 0 or y < 0:
                    logger.warning(
                        "Skipping monitor %s with negative offset (%d, %d); "
                        "ximagesrc requires non-negative coordinates",
                        name,
                        x,
                        y,
                    )
                    break
                monitors.append({"id": name, "box": [x, y, x + w, y + h]})
                break

    return assign_monitor_positions(monitors)


async def is_dpms_active() -> bool:
    """Check if DPMS has powered off the display (X11 only).

    Runs xset q and parses the monitor state line.
    Returns True if the display is in standby/suspend/off state, False otherwise.
    Degrades gracefully to False when xset is unavailable or returns an error.
    """
    try:
        result = await asyncio.to_thread(
            subprocess.run,
            ["xset", "q"],
            capture_output=True,
            text=True,
            timeout=2,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return False

    if result.returncode != 0:
        return False

    for line in result.stdout.splitlines():
        stripped = line.strip()
        if stripped.startswith("Monitor is"):
            return stripped != "Monitor is On"
    return False


async def probe_activity_services(bus: MessageBus) -> dict[str, bool]:
    """Check which activity DBus services are reachable."""
    services = {
        "fdo_screensaver": FDO_SCREENSAVER_BUS,
        "gnome_screensaver": GNOME_SCREENSAVER_BUS,
        "gnome_display_config": DISPLAY_CONFIG_BUS,
        "kscreen": KSCREEN_BUS,
    }
    results = {}
    for name, bus_name in services.items():
        results[name] = await _name_has_owner(bus, bus_name)

    # DPMS is X11-only, checked via xset availability
    results["dpms"] = bool(shutil.which("xset"))
    results["gtk4"] = _HAS_GTK

    # Log grouped by function
    lock_backends = ["fdo_screensaver", "gnome_screensaver"]
    power_backends = ["gnome_display_config"]
    monitor_backends = ["kscreen"]

    def _status(keys):
        return ", ".join(f"{k} [{'ok' if results[k] else 'missing'}]" for k in keys)

    logger.info("Screen lock backends: %s", _status(lock_backends))
    logger.info(
        "Power save backends: %s, dpms [%s]",
        _status(power_backends),
        "ok" if results["dpms"] else "missing",
    )
    logger.info(
        "Monitor backends: %s, gtk4 [%s]",
        _status(monitor_backends),
        "ok" if results["gtk4"] else "missing",
    )

    any_lock = any(results[k] for k in lock_backends)
    any_power = any(results[k] for k in power_backends)
    if not any_lock and not any_power:
        logger.warning(
            "No activity backends available — running in always-capture mode"
        )

    return results


async def is_screen_locked(bus: MessageBus) -> bool:
    """Check if the screen is locked.

    On GNOME, probes only org.gnome.ScreenSaver — the FDO ScreenSaver bus
    on GNOME serves idle-inhibit endpoints only and does not implement
    GetActive. On non-GNOME desktops, tries FDO ScreenSaver first (KDE
    kwin and other compliant desktops), then falls back to GNOME
    ScreenSaver. Returns True if locked, False if unlocked or all
    backends unavailable.
    """
    if not _is_gnome_desktop():
        # Try freedesktop.org ScreenSaver first (KDE kwin and other non-GNOME desktops)
        try:
            iface = await _cached_interface(
                bus,
                FDO_SCREENSAVER_BUS,
                FDO_SCREENSAVER_PATH,
                FDO_SCREENSAVER_IFACE,
            )
            return bool(await iface.call_get_active())
        except (
            DBusError,
            InvalidMemberNameError,
            InvalidIntrospectionError,
            OSError,
        ) as exc:
            _invalidate_interface(
                bus,
                FDO_SCREENSAVER_BUS,
                FDO_SCREENSAVER_PATH,
                FDO_SCREENSAVER_IFACE,
            )
            if not _is_service_missing(exc):
                logger.warning(
                    "is_screen_locked FDO backend failed: service=%s path=%s: %s: %s",
                    FDO_SCREENSAVER_BUS,
                    FDO_SCREENSAVER_PATH,
                    type(exc).__name__,
                    exc,
                )

    # Fall back to GNOME ScreenSaver
    try:
        iface = await _cached_interface(
            bus,
            GNOME_SCREENSAVER_BUS,
            GNOME_SCREENSAVER_PATH,
            GNOME_SCREENSAVER_IFACE,
        )
        return bool(await iface.call_get_active())
    except (
        DBusError,
        InvalidMemberNameError,
        InvalidIntrospectionError,
        OSError,
    ) as exc:
        _invalidate_interface(
            bus,
            GNOME_SCREENSAVER_BUS,
            GNOME_SCREENSAVER_PATH,
            GNOME_SCREENSAVER_IFACE,
        )
        if not _is_service_missing(exc):
            logger.warning(
                "is_screen_locked GNOME backend failed: service=%s path=%s: %s: %s",
                GNOME_SCREENSAVER_BUS,
                GNOME_SCREENSAVER_PATH,
                type(exc).__name__,
                exc,
            )
        return False


async def is_power_save_active(bus: MessageBus) -> bool:
    """Return True when the session reports a power-saving/display-off state.

    Checks GNOME Mutter PowerSaveMode, then falls back to X11 DPMS when
    XDG_SESSION_TYPE is x11; degrades to False when no backend is available.
    """

    def log_backend_failure_once(backend: str, bus_name: str, path: str, exc) -> None:
        level = logger.warning
        if backend in _POWER_SAVE_WARNED_BACKENDS:
            level = logger.debug
        else:
            _POWER_SAVE_WARNED_BACKENDS.add(backend)
        level(
            "is_power_save_active %s backend failed: service=%s path=%s: %s: %s",
            backend,
            bus_name,
            path,
            type(exc).__name__,
            exc,
        )

    # Try GNOME Mutter DisplayConfig first
    try:
        iface = await _cached_interface(
            bus,
            DISPLAY_CONFIG_BUS,
            DISPLAY_CONFIG_PATH,
            "org.freedesktop.DBus.Properties",
        )
        mode_variant = await iface.call_get(DISPLAY_CONFIG_IFACE, "PowerSaveMode")
        mode = int(mode_variant.value)
        return mode != 0
    except (
        DBusError,
        InvalidMemberNameError,
        InvalidIntrospectionError,
        OSError,
    ) as exc:
        _invalidate_interface(
            bus,
            DISPLAY_CONFIG_BUS,
            DISPLAY_CONFIG_PATH,
            "org.freedesktop.DBus.Properties",
        )
        if not _is_service_missing(exc):
            log_backend_failure_once(
                "Mutter",
                DISPLAY_CONFIG_BUS,
                DISPLAY_CONFIG_PATH,
                exc,
            )

    # X11-only fallback: DPMS via xset
    if os.environ.get("XDG_SESSION_TYPE", "").lower() == "x11":
        return await is_dpms_active()

    return False


def get_monitor_geometries() -> list[dict]:
    """
    Get structured monitor information.

    Returns:
        List of dicts with format:
        [{"id": "connector-id", "box": [x1, y1, x2, y2], "position": "center|left|right|..."}, ...]
        where box contains [left, top, right, bottom] coordinates

    Raises:
        RuntimeError: If GTK4/GDK4 is not available.
    """
    if not _HAS_GTK:
        raise RuntimeError("GTK4 not available for monitor geometry detection")

    from .monitor_positions import assign_monitor_positions

    # Initialize GTK before using GDK functions
    Gtk.init()

    # Get the default display. If it is None, try opening one from the environment.
    display = Gdk.Display.get_default()
    if display is None:
        env_display = os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY")
        if env_display is not None:
            display = Gdk.Display.open(env_display)
        if display is None:
            raise RuntimeError("No display available")
    monitors = display.get_monitors()

    # Collect monitor geometries
    geometries = []
    for monitor in monitors:
        geom = monitor.get_geometry()
        connector = monitor.get_connector() or f"monitor-{len(geometries)}"
        geometries.append(
            {
                "id": connector,
                "box": [geom.x, geom.y, geom.x + geom.width, geom.y + geom.height],
            }
        )

    # Assign position labels using shared algorithm
    return assign_monitor_positions(geometries)


def _unwrap_variants(obj):
    """Recursively unwrap dbus-fast Variants in nested DBus structures."""
    if isinstance(obj, Variant):
        return _unwrap_variants(obj.value)
    if isinstance(obj, dict):
        return {key: _unwrap_variants(value) for key, value in obj.items()}
    if isinstance(obj, list):
        return [_unwrap_variants(value) for value in obj]
    if isinstance(obj, tuple):
        return tuple(_unwrap_variants(value) for value in obj)
    return obj


async def get_monitor_geometries_kscreen(bus: MessageBus) -> list[dict]:
    """
    Get monitor geometry information from KDE KScreen DBus.

    Returns:
        List of dicts with format:
        [{"id": "connector-id", "box": [x1, y1, x2, y2], "position": "center|left|right|..."}, ...]
    """
    try:
        from .monitor_positions import assign_monitor_positions

        intro = await bus.introspect(KSCREEN_BUS, KSCREEN_PATH)
        obj = bus.get_proxy_object(KSCREEN_BUS, KSCREEN_PATH, intro)
        iface = obj.get_interface(KSCREEN_IFACE)
        config = _unwrap_variants(await iface.call_get_config())
        outputs = config.get("outputs", {})
        output_values = outputs.values() if isinstance(outputs, dict) else outputs

        geometries = []
        for output in output_values:
            if not isinstance(output, dict):
                continue
            if not output.get("enabled") or not output.get("connected"):
                continue

            name = output.get("name")
            pos = output.get("pos", {})
            size = output.get("size", {})
            if not isinstance(name, str) or not isinstance(pos, dict):
                continue
            if not isinstance(size, dict):
                continue

            x = int(pos.get("x", 0))
            y = int(pos.get("y", 0))
            scale = float(output.get("scale", 1.0) or 1.0)
            width = int(size.get("width", 0))
            height = int(size.get("height", 0))
            logical_width = round(width / scale)
            logical_height = round(height / scale)
            geometries.append(
                {
                    "id": name,
                    "box": [x, y, x + logical_width, y + logical_height],
                }
            )

        monitors = assign_monitor_positions(geometries)
        logger.debug("KScreen monitor geometries found: %d", len(monitors))
        return monitors
    except (
        DBusError,
        InvalidMemberNameError,
        InvalidIntrospectionError,
        OSError,
    ) as exc:
        if not _is_service_missing(exc):
            logger.warning(
                "get_monitor_geometries_kscreen failed: service=%s path=%s: %s: %s",
                KSCREEN_BUS,
                KSCREEN_PATH,
                type(exc).__name__,
                exc,
            )
        return []
