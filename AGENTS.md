# AGENTS.md

Development guidelines for solstone-linux, a standalone Linux desktop observer.

## Project Overview

solstone-linux is a companion app that runs alongside the main [solstone](https://solstone.app) journal. It is one of the owner's observers — it experiences screen and audio along with the owner on a Linux desktop using PipeWire and GStreamer, stores segments locally, and syncs them to your solstone journal. It runs as a systemd user service on GNOME Wayland sessions.

This is **not** part of the solstone monorepo. It is a standalone Rust package with its own native release lifecycle.

## Source Layout

```
crates/solstone-linux/src/  Shipping Rust observer, CLI, service, sync, and capture code
crates/rust-release-manifest/src/transparency.rs  Operator release-transparency publisher
transparency-head-log.jsonl Tracked transparency head witness
packaging/                  Native package Containerfile and install notes
scripts/build-release.sh    Non-candidate native package drift helper
scripts/install.sh          Portable archive installer

contrib/                    Reference icons for development fallback
```

## Shipping Rust architecture

The shipping observer is the Rust workspace member under
`crates/solstone-linux/`. Its native CLI owns capture, sync, setup, status, and
the systemd user-service lifecycle. Use the Rust source and tests as the
authority for current product behavior.

Comments in the Rust source that cite `tests/test_*.py` or
`src/solstone_linux/` refer to the pre-1.0 Python implementation preserved in
this repository's git history.

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
make publish-release RELEASE_DIR=dist/rust  # Publish the exact retained candidate
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
```

## Rust rebuild

The root Cargo workspace is workspace-only: `crates/solstone-linux/` contains the shipping observer, native CLI, service lifecycle, and Linux video-capture backends. `rust-toolchain.toml` is the compiler authority. Use the canonical Make targets above. For the operator-run native packaging rail and its blocking release validation, see `RELEASING.md`.

## Releasing

The native candidate rail creates portable, Debian, and RPM artifacts plus three
package-bound local proofs. Publication is a separate operator step.

```bash
make release-candidate EXPECTED_RELEASE_COMMIT=<commit> ADVISORY_DESCRIPTOR=<descriptor>
```

`make release` enters the same transaction. The manifest validator, candidate
transaction, Debian/RPM/tar install proofs, and blocking live FLAC checkpoint are
distinct evidence activities. `candidate-proven` and
`retained-candidate-valid` are local evidence, not publication approval. The
individual `scripts/build-release.sh` lanes write only non-candidate drift evidence.
Follow `RELEASING.md` for image and advisory preconditions, stale-lock recovery,
proof resume, read-only recovery, the separate FLAC checkpoint, and publication.

## File Headers

All `.rs` source files under `crates/` must include this header as the first two
lines:

```rust
// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
```

Add this header to new `.rs` files under `crates/`. Do not add headers to
markdown, TOML, or config files.

## Runtime Dependencies

Shipping system packages:
- glibc 2.35 or newer
- libpulse and a PulseAudio-compatible server such as PipeWire Pulse
- GStreamer 1.0 core, base, good, PipeWire, and X11 plugins
- PipeWire and xdg-desktop-portal with ScreenCast support for Wayland capture
- xdg-utils for opening links
- a desktop notification service; an SNI host is optional for the tray icon

## Data Paths

- Config: `~/.config/solstone-linux/config.json`
- Captures: `~/.local/share/solstone-linux/captures/`
- State: `~/.local/share/solstone-linux/state/`
- Restore token: `~/.config/solstone-linux/restore_token`

## Testing

Run `make test` for the locked Rust suite or `make ci` for host evidence across
Rust formatting, lint, tests, shell scripts, and offline dependency policy.

On a fresh checkout, run `make install` (or `cargo fetch --locked`) before
`make ci`. Parts of the gate resolve the dependency graph offline, and that
resolve covers the whole lockfile, including crates that never build on this
platform, so a cache populated only by building here is not sufficient.

Repository configuration is test-pinned. `Makefile`, `Cargo.toml`,
`.containerignore`, `packaging/`, and `contrib/icons/` all have tests asserting
their contents, and the toolchain policy fails closed on any Cargo invocation it
does not recognise. Run `make ci` after editing any of them: an edit that looks
like housekeeping can break the gate, and removing an entry can violate a policy
the tests hold.

## Brand canon

- **solstone-linux is an observer, but "observer" is an engineering word.** Owner-facing: solstone is the platform, **sol** is the app that lives on your devices, and **the journal** is the memory it keeps. "Observer" and "keeper" are engineering-internal only — never use them in owner-facing prose, and never give sol a role-noun title. Say what sol does with verbs: *sol keeps your journal*. In engineering architecture, `observers + sol agent + journal` is the running software this repo's code talks to, and this repo implements one of those observers.
- **The journal is "the journal" or "your journal."** Never "journal host," "journal service," or "a server" in owner-facing prose. Those are backstage words; the package name is not the owner-facing name.
- **Use co-experience language in branded prose.** In README, INSTALL, onboarding text, settings copy, and error messages, describe solstone-linux as something that experiences screen and audio along with the owner. Never describe it as watching, recording, monitoring, or tracking the owner.
- **Keep code language in code-only contexts.** Internal architecture terms such as `Capture loop`, the capture pipeline, module names, and data-path names are canon-permitted here and must not be renamed just to match branded prose.
- **Edit with the surface in mind.** If the owner sees the string, follow the canon. If the text is naming code, pipelines, modules, or storage artifacts for engineers, the existing internal vocabulary stays.

## License

AGPL-3.0-only -- Copyright (c) 2026 sol pbc
