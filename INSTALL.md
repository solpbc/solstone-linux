# Installing solstone-linux

These instructions are for a coding agent and human working together. The
solstone app on linux takes in what you share with it using PipeWire and
GStreamer, and all of it goes into your journal.

Your journal must already be available. If it is not, start there:
https://solstone.app/install. Create a pair link for this device in your journal
and save it as `pair-link.txt`.

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

## Install a release

**Verify first.** Download the package you will install, the release manifest,
and its adjacent `.minisig` file from the
[latest release](https://github.com/solpbc/solstone-linux/releases/latest).
Then fetch the published key, authenticate the manifest, and check the package
against the digest in that manifest:

```bash
curl -fLo solstone-linux-release.pub https://updates.solstone.app/solstone-linux/minisign.pub
minisign -Vm solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json -p solstone-linux-release.pub
jq -r --arg package '<downloaded-package>' \
  '.artifacts[] | select(.path == $package) | "\(.sha256)  \(.path)"' \
  solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json \
  | sha256sum -c -
```

Minisign authenticates the release manifest. The next command checks the
downloaded package against the authenticated SHA-256 digest. Replace
`<downloaded-package>` with its exact filename. You run these checks; `apt`,
`dnf`, and `scripts/install.sh` do not. If either command refuses the files,
stop.

Then install one of the release artifacts. Use the Debian or RPM package
published for your distribution when available.

**Debian / Ubuntu:** `apt` does not check our minisign signature, so complete
the verify-first step above before running:

```bash
sudo apt install ./solstone-linux_<VERSION>-1_amd64.deb
```

**Fedora / RHEL:** `dnf` does not check our minisign signature, so complete the
verify-first step above before running:

```bash
sudo dnf install ./solstone-linux-<VERSION>-1.x86_64.rpm
```

For the portable archive, `scripts/install.sh` does not check our minisign
signature either. Complete the verify-first step above before running it. The
installer is in the matching release source checkout, not inside the archive:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
solstone-linux install-service
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

If you downloaded only the tarball, obtain `scripts/install.sh` from the
matching release source. Alternatively, extract the archive, copy
`bin/solstone-linux` to a directory on `PATH`, and copy `share/icons/hicolor`
beneath the same installation prefix before running the service and setup
commands.

The archive includes `INSTALL-NOTES`, which is the canonical cross-distribution
runtime dependency list. Native packages install the same binary and icon set.
The service command writes the systemd user unit and desktop autostart entry,
enables the unit, and starts the solstone app.

Pairing is the only setup path:

```bash
solstone-linux setup < pair-link.txt
```

The pair link comes from your journal. A journal on the same machine uses the
same private network path as any other journal. There is no URL, key, local installation
of the journal or Python, or direct fallback to configure. The solstone app can
continue taking in what you share while unpaired or offline. That material goes
into your journal once the connection is available.

## Build from source

Install rustup, a C toolchain, CMake, pkg-config, GLib/GStreamer development headers, and PulseAudio development headers. Then:

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make bootstrap
make ci
make install-service
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

`rust-toolchain.toml` selects the exact compiler, components, and target. `make
install` explicitly establishes them and cargo-deny before installing the app.

## Update from source

```bash
git pull
make ci
make install-service
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

Setup and runtime deliberately share one private-state lock. Stop the solstone app before
pairing. If the solstone app is running, setup exits before consuming any input and leaves
intake state, configuration, and private state unchanged.

## Verify

```bash
systemctl --user status solstone-linux
solstone-linux status
```

## Desktop notes

Activity detection uses screen-lock and power-save signals to notice when you step away. GNOME provides both signals; KDE Wayland provides screen lock; X11 can also provide DPMS power save. Where neither signal is available, the solstone app on linux still takes in what you share and that material goes into your journal, but activity-based segment boundaries do not trigger.

The tray uses the StatusNotifierItem D-Bus protocol. KDE supports it directly.
GNOME requires an AppIndicator extension; without an SNI host, the solstone app continues
normally without a tray icon.

## Historical note: version 0.4.5

Version 0.4.5 was the final pre-native Python release. Current installation uses
the native Debian, RPM, or portable package described above.
