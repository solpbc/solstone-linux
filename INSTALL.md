# installing solstone-linux

these instructions are for a coding agent and human working together. sol for
Linux experiences your screen and audio along with you using PipeWire and
GStreamer, and syncs segments to your journal.

your journal must already be available. if it is not, start there:
https://solstone.app/install. create a pair link for this device in your journal
and save it as `pair-link.txt`.

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
systemctl --user stop solstone-linux
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

If you downloaded only the tarball, obtain `scripts/install.sh` from the
matching release source. Alternatively, extract the archive, copy
`bin/solstone-linux` to a directory on `PATH`, and copy `share/icons/hicolor`
beneath the same installation prefix before running the service and setup
commands.

the archive includes `INSTALL-NOTES`, which is the canonical cross-distribution
runtime dependency list. Native packages install the same binary and icon set.
The service command writes the systemd user unit and desktop autostart entry,
enables the unit, and starts sol.

pairing is the only setup path:

```bash
solstone-linux setup < pair-link.txt
```

the pair link comes from your journal. a journal on the same machine uses the
same private link as any other journal. there is no URL, key, local installation
of the journal or Python, or direct fallback to configure. sol can continue
capturing while unpaired or offline and saves segments locally.

## build from source

install rustup, a C toolchain, CMake, pkg-config, GLib/GStreamer development headers, and PulseAudio development headers. Then:

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
install` explicitly establishes them and cargo-deny before installing sol.

## update from source

```bash
systemctl --user stop solstone-linux
git pull
make ci
make install-service
solstone-linux setup < pair-link.txt
systemctl --user start solstone-linux
```

setup and runtime deliberately share one private-state lock. stop sol before
pairing. if sol is running, setup exits before consuming any input and leaves
capture, config, and private state unchanged.

## verify

```bash
systemctl --user status solstone-linux
solstone-linux status
```

## desktop notes

Activity detection uses screen-lock and power-save signals to notice when you step away. GNOME provides both signals; KDE Wayland provides screen lock; X11 can also provide DPMS power save. Where neither signal is available, solstone-linux still experiences your screen and audio, but activity-based segment boundaries do not trigger.

The tray uses the StatusNotifierItem D-Bus protocol. KDE supports it directly.
GNOME requires an AppIndicator extension; without an SNI host, sol continues
normally without a tray icon.

## historical note: version 0.4.5

version 0.4.5 was the final pre-native Python release. current installation uses
the native Debian, RPM, or portable package described above.
