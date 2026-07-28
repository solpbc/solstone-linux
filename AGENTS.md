# AGENTS.md

Development guidelines for solstone-linux, a standalone Linux desktop observer.

## Project Overview

solstone-linux is a companion app that runs alongside the main [solstone](https://solstone.app) journal. It is one of the owner's observers — it experiences screen and audio along with the owner on a Linux desktop using PipeWire and GStreamer, stores segments locally, and syncs them to your solstone journal. It runs as a systemd user service on GNOME Wayland sessions.

This is **not** part of the solstone monorepo. It is a standalone Rust package with its own native release lifecycle. The retained former Python rail remains functional but is not part of the shipping product rail.

## Source Layout

```
crates/solstone-linux/src/  Shipping Rust observer, CLI, service, sync, and capture code
crates/rust-release-manifest/src/transparency.rs  Operator release-transparency publisher
transparency-head-log.jsonl Tracked transparency head witness
packaging/                  Native package Containerfile and install notes
scripts/build-release.sh    Non-candidate native package drift helper
scripts/install.sh          Portable archive installer

src/solstone_linux/         Retained former Python implementation
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

tests/                      Legacy Python pytest suite
contrib/                    Reference icons for development fallback
```

## Shipping Rust architecture

The shipping observer is the Rust workspace member under
`crates/solstone-linux/`. Its native CLI owns capture, sync, setup, status, and
the systemd user-service lifecycle. Use the Rust source and tests as the
authority for current product behavior.

## Legacy Python architecture

The retained Python implementation runs a single asyncio event loop with three
concurrent concerns. This describes legacy code, not the shipping observer:

1. **Capture loop** (`observer.py`) — Checks activity status every 5 seconds, records audio continuously, manages screencast recording via GStreamer. Creates 5-minute segments in `~/.local/share/solstone-linux/captures/YYYYMMDD/stream/HHMMSS_DDD/`. Segment directories start as `.incomplete` and are renamed on finalization.

2. **Sync service** (`sync.py`) — Background asyncio task that walks the captures directory, queries the server for existing segments, and uploads missing ones. Circuit breaker pattern with error-type-aware thresholds.

3. **Chat bridge** (`chat_bridge.py`) — Background asyncio task that consumes server-sent callosum chat events, mirrors request/clear messages to an optional local FIFO, and fires click-capturing `notify-send` subprocesses when server opt-in allows Linux desktop notifications.

State machine has two modes: `screencast` (screen active, recording video) and `idle` (screen inactive). Mode transitions, mute state changes, and 5-minute intervals all trigger segment boundaries.

The capture loop never makes network calls. It writes locally; sync handles all uploads.

The `observe/status` heartbeat carries top-level diagnostics-only health-beacon fields for registered observers; these contain no captured content, paths, URLs, tokens, titles, or labels. Missing or legacy beacons are liveness-only and not failures; journal-side ingest rejections (`health.ingest_rejection`) are separate and are not produced by the observer.

## Commands

```bash
make bootstrap      # Install rustup if needed, then establish pinned tools
make install        # Establish pinned Rust/tools and install the observer
make format         # Format Rust source
make test           # Run locked Rust tests
make check-rust-release-manifest  # Validate release-manifest fixtures offline
make publish-transparency RELEASE_DIR=<candidate>  # Publish retained release evidence
make resign-transparency-pointer  # Verify the chain and renew its signed pointer
make release-candidate  # Create and locally prove one atomic candidate
make release-images  # Build the local Ubuntu and Fedora release build/proof images
make release-candidate-prove  # Resume only missing package proofs
make release-candidate-recover  # Read-only retained-candidate validation
make ci             # Host evidence: Rust format, lint, tests, offline policy
make audit          # Refresh RustSec data, then check advisories
make update-deps    # Sole unlocked Cargo dependency-update path
make shellcheck     # Check release and installer shell scripts
make install-service  # Install the native systemd user service
make service-restart  # systemctl restart wrapper
make service-status   # systemctl status wrapper
make service-logs     # systemctl log tail wrapper
make uninstall-service  # Remove the native systemd user service
make clean          # Remove build artifacts and caches
make clean-install  # Clean build artifacts, then reinstall the Rust observer
make versions       # Show installed package versions

make legacy-python-bootstrap     # Install uv if needed and set up retained Python code
make legacy-python-install       # Set up the retained Python environment
make legacy-python-format        # Format and lint retained Python code
make legacy-python-test          # Run all retained Python tests
make legacy-python-test-only TEST=<selector>  # Run selected retained Python tests
make legacy-python-ci            # Run the retained Python gate
```

## Rust rebuild

The root Cargo workspace is workspace-only: `crates/solstone-linux/` contains the shipping observer, native CLI, service lifecycle, and Linux video-capture backends. `rust-toolchain.toml` is the compiler authority. Use the canonical Make targets above; Python targets are retained for maintenance of the former rail and are not part of the shipping product rail. For the operator-run native packaging rail and its blocking release validation, see `RELEASING.md`.

## Releasing

The native rail creates portable, Debian, and RPM candidate artifacts plus three
package-bound local proofs. Publication is unavailable.

```bash
make release-candidate EXPECTED_RELEASE_COMMIT=<commit> ADVISORY_DESCRIPTOR=<descriptor>
```

`make release` enters the same transaction. The manifest validator, candidate
transaction, Debian/RPM/tar install proofs, and blocking live FLAC checkpoint are
distinct evidence activities. `candidate-proven` and
`retained-candidate-valid` are local evidence, not publication approval. The
individual `scripts/build-release.sh` lanes write only non-candidate drift evidence.
Follow `RELEASING.md` for image and advisory preconditions, stale-lock recovery,
proof resume, read-only recovery, and the separate FLAC checkpoint.

## Legacy Python development principles

- **Simple code.** Prefer plain functions over classes. Use dataclasses for structured data. Only use classes when managing stateful lifecycle (Observer, Screencaster, SyncService, AudioRecorder).
- **Async by default.** The main loop is asyncio. DBus calls, subprocess management, and sync all use async. Audio recording uses a dedicated thread because soundcard is blocking.
- **No network in the capture loop.** The observer writes segments locally. The sync service uploads asynchronously. This keeps capture reliable even when the server is down.
- **Atomic directory operations.** Segments start as `HHMMSS.incomplete/`, are renamed to `HHMMSS_DDD/` on completion, or `HHMMSS.failed/` on recovery failure.
- **System site-packages required for legacy Python.** PyGObject and GStreamer bindings come from system packages. Its venv must use `--system-site-packages`.

## File Headers

All `.py` source files must include this header as the first two lines:

```python
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
```

Add this header to new `.py` files in `src/solstone_linux/` and `tests/`. Do not add headers to markdown, TOML, or config files.

## Runtime Dependencies

Shipping system packages:
- glibc 2.35 or newer
- libpulse and a PulseAudio-compatible server such as PipeWire Pulse
- GStreamer 1.0 core, base, good, PipeWire, and X11 plugins
- PipeWire and xdg-desktop-portal with ScreenCast support for Wayland capture
- xdg-utils for opening links
- a desktop notification service; an SNI host is optional for the tray icon

Legacy Python packages (in `pyproject.toml`; not used by the shipping binary):
- `requests` — HTTP upload client
- `numpy` — Audio buffer manipulation and RMS computation
- `soundfile` — FLAC encoding
- `soundcard` — Audio device enumeration and recording
- `dbus-fast` — Async DBus client for portal and activity detection
- `PyGObject` — GDK monitor geometry (`python3-gobject` / `python3-gi`, installed from the system)

## Data Paths

- Config: `~/.config/solstone-linux/config.json`
- Captures: `~/.local/share/solstone-linux/captures/`
- State: `~/.local/share/solstone-linux/state/`
- Restore token: `~/.config/solstone-linux/restore_token`
- Legacy Python install source marker: `~/.config/solstone-linux/.install-source` (tracks which repo clone owns the former pipx install)

## Legacy Python implementation patterns

- **Activity detection is cross-desktop.** Uses ordered DBus fallback chains for screen lock (freedesktop.org ScreenSaver → GNOME ScreenSaver) and power save (Mutter DisplayConfig → KDE Solid PowerManagement). All backends degrade gracefully to safe defaults.
- **Audio is stereo-interleaved.** Left channel = microphone, right channel = system audio. When muted, channels are split into separate mono FLAC files.
- **Screencast uses xdg-desktop-portal.** Session persistence via restore tokens avoids re-prompting the user. GStreamer subprocess (`gst-launch-1.0`) handles the actual PipeWire recording.
- **Crash recovery runs on startup.** `recovery.py` scans for orphaned `.incomplete` directories older than 2 minutes and finalizes or marks them as failed.

## Testing

Run `make test` for the locked Rust suite or `make ci` for host evidence across
Rust formatting, lint, tests, shell scripts, and offline dependency policy.
The retained Python tests use pytest with mocked audio devices, D-Bus, and
GStreamer; run them separately with `make legacy-python-test`.

## Brand canon

- **solstone-linux is an observer.** Owner-facing canon describes solstone as observers + journal; sol is the keeper who lives in and tends your journal. In engineering architecture, `observers + sol agent + journal` is the running software this repo's code talks to. This repo implements one of those observers.
- **Use co-experience language in branded prose.** In README, INSTALL, onboarding text, settings copy, and error messages, describe solstone-linux as something that experiences screen and audio along with the owner. Never describe it as watching, recording, monitoring, or tracking the owner.
- **Keep code language in code-only contexts.** Internal architecture terms such as `Capture loop`, the capture pipeline, module names, and data-path names are canon-permitted here and must not be renamed just to match branded prose.
- **Edit with the surface in mind.** If the owner sees the string, follow the canon. If the text is naming code, pipelines, modules, or storage artifacts for engineers, the existing internal vocabulary stays.

## License

AGPL-3.0-only -- Copyright (c) 2026 sol pbc
