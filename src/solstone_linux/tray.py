# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
"""solstone tray app — in-process D-Bus SNI component.

Exports the tray icon, menu, and tooltip on the observer's existing
session bus connection. No separate tray process is required.
"""

import asyncio
import logging
import os
import subprocess
import time
from pathlib import Path

from dbus_next.aio import MessageBus

from . import __version__
from .dbusmenu import DBusMenu, MenuItem, separator
from .sni import StatusNotifierItem, register_with_watcher
from .sync_health import HealthState, SyncFacts, SyncHealth, derive_health

log = logging.getLogger(__name__)

# Icon names — these reference SVGs in our icon theme
ICONS = {
    "recording": "solstone-recording",
    "paused": "solstone-paused",
    "idle": "solstone-paused",
    "stopped": "solstone-error",
    "syncing": "solstone-syncing",
    "error": "solstone-error",
}

# Agent instructions template copied to clipboard
AGENT_INSTRUCTIONS = """solstone observer (Linux)
Source: {source_dir}
Read INSTALL.md in the source directory for setup and architecture.
Config: {config_path}
Captures: {captures_dir}
Logs: journalctl --user -u solstone-linux -f
Service: systemctl --user status solstone-linux"""

SOURCE_DIR = str(Path(__file__).resolve().parent)


def _compute_header_label(status: str, health: SyncHealth, pause_remaining: int) -> str:
    if status == "paused":
        if pause_remaining and pause_remaining > 0:
            mins = pause_remaining // 60
            return f"paused ({mins}m remaining)"
        return "paused"
    if status == "stopped":
        return "not running"
    if status == "recording":
        return health.header_recording
    if status == "idle":
        return health.header_idle
    return str(status)


def resolve_icon_theme_path() -> str:
    """Return the SNI icon-theme search path, or '' if none is available.

    Prefers the installed location, then the in-repo contrib dir for
    development. No index.theme is needed: the SNI host merges this path
    against the system hicolor theme, which already declares scalable/status.
    """
    installed_icon = (
        Path.home()
        / ".local/share/icons/hicolor/scalable/status/solstone-recording.svg"
    )
    if installed_icon.exists():
        return str(Path.home() / ".local/share/icons")
    contrib = Path(__file__).resolve().parent.parent.parent / "contrib" / "icons"
    if (contrib / "hicolor").is_dir():
        return str(contrib)
    return ""


class TrayApp:
    """In-process tray component — exports SNI on the observer's bus."""

    def __init__(self, observer, bus):
        self._observer = observer
        self.config = observer.config
        self.bus: MessageBus = bus
        self.sni = StatusNotifierItem("solstone-observer")
        self.menu = DBusMenu()

        # State cache (for change detection) — empty string forces first update()
        # call to always go through _update_status regardless of initial mode
        self.status = ""
        self.health = self._fallback_health()
        self.error = ""
        self.paused_remaining = 0
        self.stats = {}

        # Menu item references for dynamic updates
        self._status_header: MenuItem = None
        self._status_item: MenuItem = None
        self._sync_item: MenuItem = None
        self._segment_item: MenuItem = None
        self._cache_item: MenuItem = None
        self._captures_item: MenuItem = None
        self._uptime_item: MenuItem = None
        self._pause_submenu: MenuItem = None
        self._resume_item: MenuItem = None

    def _fallback_health(self) -> SyncHealth:
        return derive_health(
            SyncFacts(),
            time.time(),
            self.config.sync_stale_threshold,
        )

    def _current_health(self) -> SyncHealth:
        obs = self._observer
        if obs._sync:
            return obs._sync.health
        return self._fallback_health()

    async def start(self):
        pid = os.getpid()
        bus_name = f"org.kde.StatusNotifierItem-{pid}-1"
        await self.bus.request_name(bus_name)

        # Export interfaces
        self.bus.export("/StatusNotifierItem", self.sni)
        self.bus.export("/MenuBar", self.menu)

        # Resolve icon theme: installed location, then dev/contrib fallback
        self.sni._icon_theme_path = resolve_icon_theme_path()
        if self.sni._icon_theme_path:
            log.info(f"Icon theme path: {self.sni._icon_theme_path}")

        # Set initial icon
        self.sni.set_icon(ICONS["recording"])
        self._update_accessible_descriptions()
        self.sni.set_tooltip("solstone observer", "starting...")

        # Build menu
        self._build_menu()

        # Register with watcher (3 attempts)
        registered = False
        for attempt in range(3):
            registered = await register_with_watcher(self.bus, bus_name)
            if registered:
                break
            if attempt < 2:
                await asyncio.sleep(1)
                log.info(f"SNI watcher retry {attempt + 1}/2...")

        if not registered:
            log.info("No StatusNotifierWatcher available")
            return False

        return True

    def update(self):
        """Read observer state and update tray display."""
        obs = self._observer
        now = time.monotonic()

        # Determine status
        if obs._paused:
            status = "paused"
        elif obs.current_mode == "screencast":
            status = "recording"
        else:
            status = "idle"

        health = self._current_health()

        # Segment timer
        if obs._paused or obs.segment_dir is None:
            segment_timer = 0
        else:
            remaining = obs.interval - (now - obs.start_at_mono)
            segment_timer = max(0, int(remaining))

        # Pause remaining
        if not obs._paused or obs._pause_until <= 0:
            pause_remaining = 0
        else:
            pause_remaining = max(0, int(obs._pause_until - now))

        capture_stats = obs.capture_stats
        self.stats = {
            "captures_today": capture_stats["captures_today"],
            "total_size_mb": capture_stats["total_size_mb"],
            "uptime_seconds": int(now - obs._start_mono),
        }

        self._update_status(status, health)
        self._update_sync(health)
        self._update_header(pause_remaining, health)
        self._update_live_stats(segment_timer, pause_remaining)
        self.paused_remaining = pause_remaining

    def _on_about_to_show(self) -> bool:
        """Full recompute on menu open; returns True if any item changed.

        Runs outside _refresh_tray so a failure here never tears down the tray.
        """
        before = self.menu._props_emitted
        try:
            self.update()
        except Exception:
            log.warning("Tray on-open recompute failed", exc_info=True)
            return False
        return self.menu._props_emitted > before

    def _build_menu(self):
        """Build the full tray menu structure."""

        self._status_header = MenuItem(label="observing", enabled=False)

        # ── Status submenu (live data) ──
        self._status_item = MenuItem(label="observing", enabled=False)
        self._sync_item = MenuItem(label="sync: checking...", enabled=False)
        self._segment_item = MenuItem(label="segment: --:--", enabled=False)
        self._cache_item = MenuItem(label="cache: --", enabled=False)
        self._captures_item = MenuItem(label="captures today: --", enabled=False)
        self._uptime_item = MenuItem(label="uptime: --", enabled=False)

        status_submenu = MenuItem(
            label="status",
            children_display="submenu",
        )
        status_submenu.children = [
            self._status_item,
            self._sync_item,
            separator(),
            self._segment_item,
            self._cache_item,
            self._captures_item,
            self._uptime_item,
        ]

        # ── Pause / Resume ──
        pause_15m = MenuItem(label="15 minutes", callback=lambda: self._pause(900))
        pause_30m = MenuItem(label="30 minutes", callback=lambda: self._pause(1800))
        pause_1h = MenuItem(label="1 hour", callback=lambda: self._pause(3600))
        pause_indef = MenuItem(label="until I resume", callback=lambda: self._pause(0))

        self._pause_submenu = MenuItem(
            label="pause",
            children_display="submenu",
        )
        self._pause_submenu.children = [pause_15m, pause_30m, pause_1h, pause_indef]

        self._resume_item = MenuItem(
            label="resume",
            visible=False,
            callback=self._resume,
        )

        # ── Open journal / Show captures ──
        open_journal = MenuItem(
            label="open journal",
            callback=self._open_journal,
        )

        # ── Settings submenu ──
        settings_open_config = MenuItem(
            label="open config.json",
            callback=self._open_config,
        )
        settings_submenu = MenuItem(
            label="settings",
            children_display="submenu",
        )
        settings_submenu.children = [
            settings_open_config,
        ]

        # ── About submenu ──
        about_version = MenuItem(
            label=f"solstone observer v{__version__}",
            enabled=False,
        )
        about_website = MenuItem(
            label="solstone.app",
            callback=lambda: self._open_url("https://solstone.app/observers"),
        )
        about_source = MenuItem(
            label="source code",
            callback=lambda: self._open_url("https://github.com/solpbc/solstone-linux"),
        )
        about_privacy = MenuItem(
            label="privacy policy",
            callback=lambda: self._open_url("https://solpbc.org/privacy"),
        )
        about_copyright = MenuItem(
            label="\u00a9 2026 sol pbc \u2014 a public benefit corporation",
            enabled=False,
        )

        about_submenu = MenuItem(
            label="about",
            children_display="submenu",
        )
        about_copy_agent = MenuItem(
            label="copy help agent instructions",
            callback=self._copy_agent_instructions,
        )

        about_submenu.children = [
            about_version,
            about_website,
            about_source,
            about_privacy,
            about_copy_agent,
            separator(),
            about_copyright,
        ]

        # ── Service hint ──
        service_hint = MenuItem(
            label="managed via systemctl",
            enabled=False,
        )

        # ── Assemble full menu ──
        self.menu.set_menu(
            [
                self._status_header,
                separator(),
                self._pause_submenu,
                self._resume_item,
                separator(),
                status_submenu,
                open_journal,
                settings_submenu,
                about_submenu,
                separator(),
                service_hint,
            ]
        )
        self.menu.on_about_to_show = self._on_about_to_show

    def _icon_for_health(self, status: str, health: SyncHealth) -> str:
        if self.error:
            return ICONS["error"]
        if status == "stopped":
            return ICONS["stopped"]
        if health.icon == "error":
            return ICONS["error"]
        if status == "paused":
            return ICONS["paused"]
        if health.icon == "syncing":
            return ICONS["syncing"]
        if status == "idle" and health.state == HealthState.CONNECTED:
            return ICONS["idle"]
        return ICONS.get(health.icon, ICONS["recording"])

    def _update_status(self, status: str, health: SyncHealth):
        """Update tray icon and menu for observer status."""
        old_status = self.status
        old_health_state = self.health.state
        self.health = health
        if status == old_status and health.state == old_health_state:
            return
        self.status = status

        icon = self._icon_for_health(status, health)
        self.sni.set_icon(icon)

        # Update tooltip
        self.sni.set_tooltip("solstone observer", self._build_tooltip(health))

        # Toggle pause/resume
        is_paused = status == "paused"
        self._pause_submenu.visible = not is_paused
        self._resume_item.visible = is_paused
        if is_paused and self.paused_remaining > 0:
            mins = self.paused_remaining // 60
            self._resume_item.label = f"resume ({mins}m remaining)"
        else:
            self._resume_item.label = "resume"
        self.menu.update_properties(self._pause_submenu, "visible")
        self.menu.update_properties(self._resume_item, "visible", "label")

        # SNI status
        if status == "stopped" or self.error:
            self.sni.set_status("NeedsAttention")
        else:
            self.sni.set_status(health.sni_status)
        self._update_accessible_descriptions(health)

        log.info(f"Status -> {status} (icon: {icon})")

    def _update_header(self, pause_remaining: int, health: SyncHealth):
        label = _compute_header_label(self.status, health, pause_remaining)
        if label == self._status_header.label:
            return
        self._status_header.label = label
        self._status_item.label = label
        self.menu.update_properties(self._status_header, "label")
        self.menu.update_properties(self._status_item, "label")

    def _update_sync(self, health: SyncHealth):
        """Update sync status display."""
        if self._sync_item.label == health.sync_line:
            return
        self.health = health
        self._sync_item.label = health.sync_line
        self.menu.update_properties(self._sync_item, "label")

        if not self.error:
            self.sni.set_icon(self._icon_for_health(self.status, health))
            if self.status == "stopped":
                self.sni.set_status("NeedsAttention")
            else:
                self.sni.set_status(health.sni_status)

        self.sni.set_tooltip("solstone observer", self._build_tooltip(health))
        self._update_accessible_descriptions(health)

    def _update_live_stats(self, segment_timer: int, pause_remaining: int):
        """Update the live stats in the status submenu."""
        # Segment timer
        mins = segment_timer // 60
        secs = segment_timer % 60
        new_label = f"segment: {mins}:{secs:02d} remaining"
        if self._segment_item.label != new_label:
            self._segment_item.label = new_label
            self.menu.update_properties(self._segment_item, "label")

        # Stats (computed in update())
        if self.stats:
            captures = self.stats.get("captures_today", 0)
            size_mb = self.stats.get("total_size_mb", 0)
            uptime = self.stats.get("uptime_seconds", 0)

            new_cache = f"cache: {size_mb} MB"
            new_captures = f"captures today: {captures} segments"

            hours = uptime // 3600
            mins_up = (uptime % 3600) // 60
            new_uptime = f"uptime: {hours}h {mins_up}m"

            if self._cache_item.label != new_cache:
                self._cache_item.label = new_cache
                self.menu.update_properties(self._cache_item, "label")
            if self._captures_item.label != new_captures:
                self._captures_item.label = new_captures
                self.menu.update_properties(self._captures_item, "label")
            if self._uptime_item.label != new_uptime:
                self._uptime_item.label = new_uptime
                self.menu.update_properties(self._uptime_item, "label")

        # Update pause remaining in resume button
        if self.status == "paused" and pause_remaining > 0:
            pr_mins = pause_remaining // 60
            new_resume = f"resume ({pr_mins}m remaining)"
            if self._resume_item.label != new_resume:
                self._resume_item.label = new_resume
                self.menu.update_properties(self._resume_item, "label")

    def _build_tooltip(self, health: SyncHealth | None = None) -> str:
        """Build plain-text tooltip body (cross-DE compatible)."""
        if health is None:
            health = self.health
        parts = []

        status_labels = {
            "recording": "observing",
            "paused": "paused",
            "idle": "idle (screen inactive)",
            "stopped": "not running",
        }
        parts.append(status_labels.get(self.status, self.status))

        parts.append(health.tooltip)

        if self.error:
            parts.append(self.error)

        return "\n".join(parts)

    def _update_accessible_descriptions(self, health: SyncHealth | None = None):
        if health is None:
            health = self.health
        if self.error:
            desc = "Solstone observer — error"
        elif self.status == "paused":
            desc = "Solstone observer — paused"
        elif self.status == "idle":
            desc = health.accessible_idle
        elif self.status == "stopped":
            desc = "Solstone observer — stopped"
        else:
            desc = health.accessible_recording

        self.sni.set_icon_accessible_desc(desc)
        self.sni.set_attention_accessible_desc(desc)

    # ── Menu callbacks ──

    def _pause(self, seconds: int):
        log.info(f"Pause: {seconds}s")
        self._observer.pause(seconds)

    def _resume(self):
        log.info("Resume")
        self._observer.resume()

    def _open_journal(self):
        log.info("Opening journal")
        self._open_url(self.config.server_url or "https://journal.solstone.app")

    def _open_config(self):
        config_path = str(self.config.config_path)
        log.info(f"Opening config: {config_path}")
        try:
            subprocess.Popen(
                ["xdg-open", config_path],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except Exception as e:
            log.error(f"Failed to open config: {e}")

    def _copy_agent_instructions(self):
        """Copy coding agent instructions to clipboard."""
        text = AGENT_INSTRUCTIONS.format(
            source_dir=SOURCE_DIR,
            config_path=str(self.config.config_path),
            captures_dir=str(self.config.captures_dir),
        )
        log.info("Copying agent instructions to clipboard")
        try:
            # wl-copy for Wayland, xclip for X11
            session_type = os.environ.get("XDG_SESSION_TYPE", "")
            if session_type == "wayland" or os.environ.get("WAYLAND_DISPLAY"):
                proc = subprocess.Popen(["wl-copy"], stdin=subprocess.PIPE)
            else:
                proc = subprocess.Popen(
                    ["xclip", "-selection", "clipboard"], stdin=subprocess.PIPE
                )
            proc.communicate(text.encode())
            log.info("Copied to clipboard")
        except FileNotFoundError:
            # Fallback: try xsel
            try:
                proc = subprocess.Popen(
                    ["xsel", "--clipboard", "--input"], stdin=subprocess.PIPE
                )
                proc.communicate(text.encode())
                log.info("Copied to clipboard (xsel)")
            except FileNotFoundError:
                log.error("No clipboard tool found (wl-copy, xclip, or xsel)")

    def _open_url(self, url: str):
        log.info(f"Opening: {url}")
        try:
            subprocess.Popen(
                ["xdg-open", url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            )
        except Exception as e:
            log.error(f"Failed to open URL: {e}")
