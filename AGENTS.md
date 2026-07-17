# AGENTS.md

Development guidelines for solstone-linux, a standalone Linux desktop observer.

## Project Overview

solstone-linux is a companion app that runs alongside the main [solstone](https://solstone.app) journal. It is one of the owner's observers — it experiences screen and audio along with the owner on a Linux desktop using PipeWire and GStreamer, stores segments locally, and syncs them to your solstone journal. It runs as a systemd user service on GNOME Wayland sessions.

This is **not** part of the solstone monorepo. It is a standalone package with its own release lifecycle, installed via pipx alongside system-provided PyGObject/GStreamer bindings.

## Source Layout

```
src/solstone_linux/
    __init__.py             Package version
    cli.py                  CLI entry point (run, setup, settings, install-service, status)
    solstone-linux.service.in        Systemd unit template (rendered by install-service)
    config.py               Config loading/persistence (config under ~/.config/solstone-linux/)
    doctor.py               Install prerequisite checks for the doctor command
    install_guard.py        Install ownership guard for pipx-managed service installs
    observer.py             Main capture loop — state machine (idle/screencast), audio + video
    capture_stats.py        Shared capture cache statistics
    screencast.py           Portal-based multi-monitor recording (xdg-desktop-portal + GStreamer)
    audio_recorder.py       Stereo audio recording (mic + system via soundcard)
    audio_detect.py         Audio device detection via ultrasonic tone
    audio_mute.py           PulseAudio mute state detection
    activity.py             Cross-desktop activity detection (screen lock, power save) via DBus
    monitor_positions.py    Monitor position assignment from geometry
    session_env.py          Desktop session environment checks and recovery
    streams.py              Stream name derivation (hostname-based)
    event_sender.py         Background sender for observer event relay
    sync.py                 Background sync service — uploads completed segments to server
    sync_health.py          Sync health facts, derivation, persistence, and surface copy
    upload.py               HTTP upload client for solstone ingest server
    recovery.py             Crash recovery for orphaned .incomplete segments
    chat_bridge.py          Server-initiated chat event bridge to local notifications
    dbus_service.py         Observer status/control D-Bus service interface
    dbusmenu.py             D-Bus menu protocol implementation for tray menus
    sni.py                  StatusNotifierItem D-Bus interface for tray icons
    tray.py                 In-process D-Bus SNI tray icon, menu, and tooltip

tests/                      pytest test suite
contrib/                    Reference icons for development fallback
```

## Architecture

The observer runs a single asyncio event loop with three concurrent concerns:

1. **Capture loop** (`observer.py`) — Checks activity status every 5 seconds, records audio continuously, manages screencast recording via GStreamer. Creates 5-minute segments in `~/.local/share/solstone-linux/captures/YYYYMMDD/stream/HHMMSS_DDD/`. Segment directories start as `.incomplete` and are renamed on finalization.

2. **Sync service** (`sync.py`) — Background asyncio task that walks the captures directory, queries the server for existing segments, and uploads missing ones. Circuit breaker pattern with error-type-aware thresholds.

3. **Chat bridge** (`chat_bridge.py`) — Background asyncio task that consumes server-sent callosum chat events, mirrors request/clear messages to an optional local FIFO, and fires click-capturing `notify-send` subprocesses when server opt-in allows Linux desktop notifications.

State machine has two modes: `screencast` (screen active, recording video) and `idle` (screen inactive). Mode transitions, mute state changes, and 5-minute intervals all trigger segment boundaries.

The capture loop never makes network calls. It writes locally; sync handles all uploads.

The `observe/status` heartbeat carries top-level diagnostics-only health-beacon fields for registered observers; these contain no captured content, paths, URLs, tokens, titles, or labels. Missing or legacy beacons are liveness-only and not failures; journal-side ingest rejections (`health.ingest_rejection`) are separate and are not produced by the observer.

## Commands

```bash
make install        # Create venv, install package + dev tools (pytest, ruff) via uv
make test           # Run all tests
make test-only TEST=tests/test_config.py  # Run specific test
make format         # Auto-format with ruff
make ci             # Python + Rust lint, format, dependency, and test checks
make install-service  # Smart install-or-upgrade: guards against cross-repo contamination; runs CI in upgrade mode
make service-restart  # systemctl restart wrapper
make service-status   # systemctl status wrapper
make service-logs     # systemctl log tail wrapper
make uninstall-service  # Disable + remove unit + pipx uninstall
make clean          # Remove build artifacts and caches
make versions       # Show installed package versions
```

## Rust rebuild

The root Cargo workspace is workspace-only: `crates/solstone-linux/` contains the portable observer logic and Linux video-capture backends, plus a stub CLI. Run `make rust-fmt-check`, `make rust-lint`, `make rust-test`, and `make rust-deny` individually, or use `make ci` as the combined Python and Rust gate. Python remains the shipped pipx observer until an explicit cutover; Rust crates are not installed or released with it.
For the unexercised operator-run Rust packaging rail and its blocking first-release validation, see `RELEASING.md`.

## Releasing

solstone-linux ships to PyPI via `scripts/release.sh`. The operator runs the
release from a clean checkout; there is no CI publish path.

```bash
make release-test   # upload to TestPyPI (requires TESTPYPI_TOKEN)
make release        # upload to PyPI (requires PYPI_TOKEN)
```

The script refuses to run on a dirty tree, builds an sdist + a
`py3-none-any` wheel with `uv build`, runs `uvx twine check`, uploads,
tags the commit `vX.Y.Z`, pushes the tag, and creates a matching GitHub
Release with the artifacts attached and the CHANGELOG block as release
notes.

Before releasing, bump the version in BOTH `pyproject.toml` (`[project].version`)
and `src/solstone_linux/__init__.py` (`__version__`) — they must match — and add
a `## [X.Y.Z] - YYYY-MM-DD` block to `CHANGELOG.md`.

Set `RELEASE_DRY_RUN=1` to walk the full flow without uploading, tagging,
pushing, or publishing a GitHub Release; the build and `twine check` still
run for real.

## Development Principles

- **Simple code.** Prefer plain functions over classes. Use dataclasses for structured data. Only use classes when managing stateful lifecycle (Observer, Screencaster, SyncService, AudioRecorder).
- **Async by default.** The main loop is asyncio. DBus calls, subprocess management, and sync all use async. Audio recording uses a dedicated thread because soundcard is blocking.
- **No network in the capture loop.** The observer writes segments locally. The sync service uploads asynchronously. This keeps capture reliable even when the server is down.
- **Atomic directory operations.** Segments start as `HHMMSS.incomplete/`, are renamed to `HHMMSS_DDD/` on completion, or `HHMMSS.failed/` on recovery failure.
- **System site-packages required.** PyGObject and GStreamer bindings come from system packages. The venv (and pipx) must use `--system-site-packages`.

## File Headers

All `.py` source files must include this header as the first two lines:

```python
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
```

Add this header to new `.py` files in `src/solstone_linux/` and `tests/`. Do not add headers to markdown, TOML, or config files.

## Runtime Dependencies

System packages (not pip-installable):
- `python3-gobject` / `python3-gi` — PyGObject for GTK4 and GDK
- GStreamer with PipeWire plugin (`gst-launch-1.0 pipewiresrc`)
- PipeWire running
- `pactl` (PulseAudio utils) for mute detection
- xdg-desktop-portal with ScreenCast support

Python packages (in pyproject.toml):
- `requests` — HTTP upload client
- `numpy` — Audio buffer manipulation and RMS computation
- `soundfile` — FLAC encoding
- `soundcard` — Audio device enumeration and recording
- `dbus-fast` — Async DBus client for portal and activity detection
- `PyGObject` — GDK monitor geometry (installed from system)

## Data Paths

- Config: `~/.config/solstone-linux/config.json`
- Captures: `~/.local/share/solstone-linux/captures/`
- State: `~/.local/share/solstone-linux/state/`
- Restore token: `~/.config/solstone-linux/restore_token`
- Install source marker: `~/.config/solstone-linux/.install-source` (tracks which repo clone owns the pipx install)

## Key Patterns

- **Activity detection is cross-desktop.** Uses ordered DBus fallback chains for screen lock (freedesktop.org ScreenSaver → GNOME ScreenSaver) and power save (Mutter DisplayConfig → KDE Solid PowerManagement). All backends degrade gracefully to safe defaults.
- **Audio is stereo-interleaved.** Left channel = microphone, right channel = system audio. When muted, channels are split into separate mono FLAC files.
- **Screencast uses xdg-desktop-portal.** Session persistence via restore tokens avoids re-prompting the user. GStreamer subprocess (`gst-launch-1.0`) handles the actual PipeWire recording.
- **Crash recovery runs on startup.** `recovery.py` scans for orphaned `.incomplete` directories older than 2 minutes and finalizes or marks them as failed.

## Testing

Tests use pytest with standard mocking. No system dependencies required for tests — audio devices, DBus, and GStreamer are mocked. Run `make test` to execute the full suite.

## Brand canon

- **solstone-linux is an observer.** Owner-facing canon describes solstone as observers + journal; sol is the keeper who lives in and tends your journal. In engineering architecture, `observers + sol agent + journal` is the running software this repo's code talks to. This repo implements one of those observers.
- **Use co-experience language in branded prose.** In README, INSTALL, onboarding text, settings copy, and error messages, describe solstone-linux as something that experiences screen and audio along with the owner. Never describe it as watching, recording, monitoring, or tracking the owner.
- **Keep code language in code-only contexts.** Internal architecture terms such as `Capture loop`, the capture pipeline, module names, and data-path names are canon-permitted here and must not be renamed just to match branded prose.
- **Edit with the surface in mind.** If the owner sees the string, follow the canon. If the text is naming code, pipelines, modules, or storage artifacts for engineers, the existing internal vocabulary stays.

## License

AGPL-3.0-only -- Copyright (c) 2026 sol pbc
