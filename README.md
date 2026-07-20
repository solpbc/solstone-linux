# solstone-linux

Standalone Linux desktop observer for [solstone](https://solpbc.org). Experiences your screen and audio along with you on a GNOME Wayland session, stores segments locally, and syncs to your solstone journal.

**Note:** Activity detection uses screen-lock and power-save signals to notice when you step away. Coverage varies by desktop: GNOME provides both signals; KDE (Wayland) provides screen lock only; any X11 session also provides DPMS power save; other Wayland desktops provide screen lock where the compositor exposes it. Where neither signal is available, solstone-linux still experiences your screen and audio, but activity-based segment boundaries won't trigger.

## System dependencies

**Fedora:**
```
sudo dnf install pulseaudio-libs gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good pipewire-gstreamer pipewire-pulseaudio xdg-desktop-portal xdg-utils
```

**Debian / Ubuntu:**
```
sudo apt install libpulse0 libgstreamer1.0-0 gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pipewire gstreamer1.0-x pipewire-pulse xdg-desktop-portal xdg-utils
```

**Arch:**
```
sudo pacman -S libpulse gstreamer gst-plugins-base gst-plugins-good gst-plugin-pipewire pipewire-pulse xdg-desktop-portal xdg-utils
```

**openSUSE:**
```
sudo zypper install libpulse0 gstreamer gstreamer-plugins-base gstreamer-plugins-good gstreamer-plugin-pipewire pipewire-pulseaudio xdg-desktop-portal xdg-utils
```

## Install

solstone (the journal) must already be installed and running on the host this observer reports to. If it isn't, start with the [journal install](https://solstone.app/install).

Install a native Debian/RPM package from the release. From a matching source
checkout, the portable archive installer is:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
solstone-linux install-service
solstone-linux setup
```

The installer is distributed in the source repository, not inside the
tarball. If you downloaded only the tarball, obtain `scripts/install.sh` from
the matching release source, or extract it and manually copy `bin/solstone-linux`
to a directory on `PATH` and `share/icons/hicolor` beneath the same prefix.

The archive includes `packaging/INSTALL-NOTES`, the canonical runtime-dependency list. See `INSTALL.md` for package installation, tray notes, and troubleshooting.

`setup` registers the observer against your journal over the local `http://localhost:5015` link, so there's no URL to type. If this machine reaches your solstone host directly instead, run `solstone-linux setup --server-url <journal-url>`. (Legacy fallback: mint a key on the journal host with `journal observer create <name>` and paste it during setup.)

### Developers building from source

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make install-service
solstone-linux setup
```

The former Python rail remains functional behind `legacy-python-*` targets,
including its publishing targets, but it is not part of the shipping product
rail.

## Setup

```bash
solstone-linux setup
```

## Run

```bash
# Foreground
solstone-linux run
```

## Status

```bash
solstone-linux status
```

Registered observers also include a diagnostics-only status beacon in the
journal: identity, version, uptime, and sync liveness counts only, with no
captured or experienced content.

## Observer contract

The observer-client contract is owned by the solstone journal and frozen here as a byte-exact, language-neutral bundle at `vendor/observer-client-contract/`. This observer adopts bundle version 1.0.2 and verifies it offline with `make check-observer-contract`.

The bundle version, the solstone-linux application release, and observer wire-protocol version 2 are independent versions. When the observer and authority disagree, resolve the incompatibility at the journal authority first; do not rewrite the vendored contract or weaken consumer conformance.

See `contracts/README.md` for the verified import ritual and public provenance record.

## License

AGPL-3.0-only — Copyright (c) 2026 sol pbc
