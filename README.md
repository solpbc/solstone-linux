# solstone-linux

The solstone app on linux takes in what you share with it on a GNOME Wayland
session; all of it goes into your journal, which lives on a device you own. It takes in
only your own desktop session, and only while that session is the one in front of the
screen ([details](INSTALL.md#which-desktop-session-it-takes-in)). It can also take in your
tmux sessions, if you turn that on ([details](INSTALL.md#your-tmux-sessions)), and those go
into your journal too. They follow
your account, so they work over SSH and on a computer with no desktop.

**Note:** Activity detection uses screen-lock and power-save signals to notice when you step away. Coverage varies by desktop: GNOME provides both signals; KDE (Wayland) provides screen lock only; any X11 session also provides DPMS power save; other Wayland desktops provide screen lock where the compositor exposes it. Where neither signal is available, the solstone app on linux still takes in what you share and that material goes into your journal, but activity-based segment boundaries will not trigger.

## System dependencies

**Fedora:**
```
sudo dnf install gtk4 glib2 pulseaudio-libs gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good pipewire-gstreamer pipewire-pulseaudio xdg-desktop-portal xdg-utils
```

**Debian / Ubuntu:**
```
sudo apt install libgtk-4-1 libglib2.0-bin libpulse0 libgstreamer1.0-0 gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pipewire gstreamer1.0-x pipewire-pulse xdg-desktop-portal xdg-utils
```

**Arch:**
```
sudo pacman -S gtk4 glib2 libpulse gstreamer gst-plugins-base gst-plugins-good gst-plugin-pipewire pipewire-pulse xdg-desktop-portal xdg-utils
```

**openSUSE:**
```
sudo zypper install libgtk-4-1 glib2-tools libpulse0 gstreamer gstreamer-plugins-base gstreamer-plugins-good gstreamer-plugin-pipewire pipewire-pulseaudio xdg-desktop-portal xdg-utils
```

## Install

Your journal must already be available. If it is not, start with the
[solstone install](https://solstone.app/install). If your journal is on another
computer, create a pairing link for this device in your journal and save it as
`pairing-link.txt`.

Install a native Debian or RPM package from `updates.solstone.app`. Read
`https://updates.solstone.app/solstone-linux/release/latest` for the current
version, then download and verify the package before installing it; `INSTALL.md`
carries the exact commands and filenames. From a matching source checkout, the
portable archive installer is:

```bash
scripts/install.sh solstone-linux-<VERSION>-linux-x86_64.tar.gz
```

The installer is distributed in the source repository, not inside the
tarball. If you downloaded only the tarball, obtain `scripts/install.sh` from
the matching release source, or follow the manual-copy instructions in `INSTALL.md`.

Check the changelog entry for your installed version. If it does not include
the window, use [terminal setup](#setup-from-a-terminal).

Open solstone from your app menu or the panel icon. The window shows status,
pause and resume, journal pairing, settings and help. If the solstone app isn't
running, choose `start the solstone app` when you're ready. Opening or closing
the window does not start or stop intake.

If your journal is running on this computer under your account, the `journal` tab
says it found your journal. Press `connect to your journal`: the app verifies the
mark itself, so there's no link to handle and no mark to compare.

For a journal on another computer, paste its pairing link in the `journal` tab and
choose `pair a device`, or open the link and choose `open in the solstone app`,
which fills it in for you. Then compare the mark with the one in your journal
before confirming it. `open your journal` opens your browser.

The window needs GTK 4.6 or newer. The Debian and RPM packages recommend it;
install it yourself for the portable archive. Intake runs in a separate process
without GTK. Settings remain in
`$XDG_CONFIG_HOME/solstone-linux/config.json` (normally
`~/.config/solstone-linux/config.json`). The window keeps hand-added keys and
tells you when a saved setting needs a restart. Help lets you preview recent
logs and save exactly that preview to a file you choose.

The archive includes `INSTALL-NOTES`, the canonical runtime-dependency list, and `RUST_DEPENDENCY_NOTICES.txt`, reproducing third-party crate license texts. See `INSTALL.md` for package installation, tray notes, and troubleshooting.

Connecting from the `journal` tab and pairing with a link from your journal are the
only setup paths. A journal on the same machine connects through the same private
network path as any other journal. There is no URL, key, local Python installation,
or direct fallback to configure.
The solstone app can continue taking in what you share while unpaired or offline.
With a pairing link, setup asks you to confirm your journal's mark, or takes `--mark` when there is no terminal. Nothing goes into your journal until that's done. `solstone-linux confirm` confirms a mark while the solstone app is running. Setup still needs the app stopped, because setup takes the private-state lock.

### Developers building from source

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make install
solstone-linux window
```

## Setup from a terminal

```bash
solstone-linux install-service
systemctl --user stop solstone-linux
solstone-linux setup < pairing-link.txt
systemctl --user start solstone-linux
```

```bash
solstone-linux setup --mark "word word" < pairing-link.txt
```

Setup and runtime deliberately share one private-state lock. Confirm changes pairing state while the solstone app is running, and setup still needs the app stopped because it takes the lock. If the solstone app is running, setup exits before consuming the pairing link and leaves intake state, configuration, and private state unchanged.

For an upgrade that needs a new pairing link:

```bash
systemctl --user stop solstone-linux
solstone-linux setup < pairing-link.txt
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

## Protocol contract

The journal's `observer-client` contract is frozen here as a byte-exact, language-neutral bundle at `vendor/observer-client-contract/`. `contracts/observer-client-import.json` records its authority revision and bundle version; `make check-observer-contract` verifies the frozen bundle without a network connection.

The vendored `manifest.json` declares observer protocol version 3. That version is separate from the bundle and solstone-linux application-release versions. When the app and authority disagree, resolve the incompatibility at the journal authority first; do not rewrite the vendored contract or weaken consumer conformance.

See `contracts/README.md` for the verified import ritual and public provenance record.

## License

AGPL-3.0-only. Copyright (c) 2026 sol pbc.
