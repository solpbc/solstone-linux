# solstone-linux

sol for Linux experiences your screen and audio along with you on a GNOME
Wayland session, stores segments locally, and syncs them to your journal on
[solstone](https://solpbc.org).

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

Your journal must already be available. If it is not, start with the
[solstone install](https://solstone.app/install). In your journal, create a pair
link for this device and save it as `pair-link.txt`.

Install a native Debian/RPM package from the release. From a matching source
checkout, the portable archive installer is:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
solstone-linux install-service
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

The installer is distributed in the source repository, not inside the
tarball. If you downloaded only the tarball, obtain `scripts/install.sh` from
the matching release source, or extract it and manually copy `bin/solstone-linux`
to a directory on `PATH` and `share/icons/hicolor` beneath the same prefix.

The archive includes `packaging/INSTALL-NOTES`, the canonical runtime-dependency list. See `INSTALL.md` for package installation, tray notes, and troubleshooting.

Pairing is the only setup path. The pair link comes from your journal. A journal
on the same machine connects through the same private link as any other journal;
there is no URL, key, local Python installation, or direct fallback to configure.
Sol can continue capturing while unpaired or offline and will save segments
locally.

### Developers building from source

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make install-service
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

## Setup

```bash
solstone-linux setup < pair-link.txt
```

Setup and runtime deliberately share one private-state lock. Stop sol before
pairing. If sol is running, setup exits before consuming the pair link and leaves
capture, config, and private state unchanged.

For an upgrade that needs a new pair link:

```bash
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
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

Paired devices also include a diagnostics-only status beacon in your journal:
identity, version, uptime, and sync liveness counts only, with none of the
screen or audio sol experiences with you.

## Observer contract

The observer-client contract is owned by the solstone journal and frozen here as a byte-exact, language-neutral bundle at `vendor/observer-client-contract/`. This observer adopts bundle version 1.0.2 and verifies it offline with `make check-observer-contract`.

The bundle version, the solstone-linux application release, and observer wire-protocol version 2 are independent versions. When the observer and authority disagree, resolve the incompatibility at the journal authority first; do not rewrite the vendored contract or weaken consumer conformance.

See `contracts/README.md` for the verified import ritual and public provenance record.

## License

AGPL-3.0-only — Copyright (c) 2026 sol pbc
