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

Install a native Debian/RPM package from the release, or install its portable archive:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
solstone-linux install-service
solstone-linux setup
```

The archive includes `packaging/INSTALL-NOTES`, the canonical runtime-dependency list. See `INSTALL.md` for package installation, tray notes, and troubleshooting.

`setup` registers the observer against your journal over the local `http://localhost:5015` link, so there's no URL to type. If this machine reaches your solstone host directly instead, run `solstone-linux setup --server-url <journal-url>`. (Legacy fallback: mint a key on the journal host with `journal observer create <name>` and paste it during setup.)

### Developers building from source

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make install-service
solstone-linux setup
```

The Python implementation and its `legacy-python-*` developer targets remain in the repository for reference and parity testing, but are non-shipping.

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

## License

AGPL-3.0-only — Copyright (c) 2026 sol pbc
