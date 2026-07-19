# installing solstone-linux

these instructions are for a coding agent and human working together. solstone-linux is a standalone observer that experiences your screen and audio along with you on Linux desktops using PipeWire and GStreamer, and syncs to your solstone journal.

solstone must already be installed and running. if it isn't, start there: https://solstone.app/install

## system dependencies

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

## install a release

use the Debian or RPM package published for your distribution when available.
The portable installer is in the matching release source checkout, not inside
the archive:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
solstone-linux install-service
solstone-linux setup
```

If you downloaded only the tarball, obtain `scripts/install.sh` from the
matching release source. Alternatively, extract the archive, copy
`bin/solstone-linux` to a directory on `PATH`, and copy `share/icons/hicolor`
beneath the same installation prefix before running the service and setup
commands.

the archive includes `INSTALL-NOTES`, which is the canonical cross-distribution runtime dependency list. Native packages install the same observer binary and icon set. The service command writes the systemd user unit and desktop autostart entry, enables the unit, and starts sol.

`setup` registers the observer through the local `http://localhost:5015` journal link by default. For a journal reached directly, use `solstone-linux setup --server-url <journal-url>`.

## build from source

install rustup, a C toolchain, CMake, pkg-config, GLib/GStreamer development headers, and PulseAudio development headers. Then:

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make bootstrap
make ci
make install-service
solstone-linux setup
```

`rust-toolchain.toml` selects the exact compiler, components, and target. `make install` explicitly establishes them and cargo-deny before installing the observer.

## update from source

```bash
git pull
make ci
make install-service
```

## verify

```bash
systemctl --user status solstone-linux
solstone-linux status
```

## desktop notes

Activity detection uses screen-lock and power-save signals to notice when you step away. GNOME provides both signals; KDE Wayland provides screen lock; X11 can also provide DPMS power save. Where neither signal is available, solstone-linux still experiences your screen and audio, but activity-based segment boundaries do not trigger.

The tray uses the StatusNotifierItem D-Bus protocol. KDE supports it directly. GNOME requires an AppIndicator extension; without an SNI host, the observer continues normally without a tray icon.

## retained Python implementation

The Python source, tests, PyPI metadata, and `scripts/release.sh` remain for
maintenance and historical parity. This former rail remains functional and
can publish when credentials are present, but it is not part of the shipping
product rail. Its commands are `make legacy-python-bootstrap`,
`legacy-python-install`, `legacy-python-format`, `legacy-python-test`,
`legacy-python-test-only TEST=<selector>`, `legacy-python-ci`,
`legacy-python-release`, and `legacy-python-release-test`. They require uv and
the former system PyGObject environment. Canonical install, test, CI, service,
and release commands are Rust-native.
