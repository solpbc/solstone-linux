# Installing solstone-linux

These instructions are for a coding agent and human working together. The
solstone app on linux takes in what you share with it using PipeWire and
GStreamer, and all of it goes into your journal.

Your journal must already be available. If it is not, start there:
https://solstone.app/install. Create a pairing link for this device in your journal.

## The quick way

On x86_64 and aarch64 (64-bit ARM), the solstone installer downloads the app,
checks its signature, starts it and adds solstone to your app menu:

```bash
curl -fsSL https://solstone.app/install.sh | sh -s -- --components desktop
```

Install `minisign` and the system dependencies below first if your desktop
doesn't have them. Then [open the window](#open-the-window) and paste your
pairing link. The next sections install a release by hand; Verify and Desktop
notes apply either way.

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

## Install a release

Releases are published to `updates.solstone.app`. Read
`https://updates.solstone.app/solstone-linux/release/latest` for the current
version:

```bash
curl -fsS https://updates.solstone.app/solstone-linux/release/latest
```

It prints one line, `version=<VERSION>`. Read it into a variable that the
commands below use, so you don't type the version by hand:

```bash
SOLSTONE_VERSION=$(curl -fsS https://updates.solstone.app/solstone-linux/release/latest | sed -n 's/^version=//p')
echo "installing solstone-linux $SOLSTONE_VERSION"
```

Run the rest of these steps in the same shell. In a new one, set
`SOLSTONE_VERSION` again first.

Download paths use the bare version, `release/<VERSION>/` with no `v`, even
though the GitHub tag is `v<VERSION>`. There is no directory listing, so fetch
each file by name. For any version the files are, on x86_64:

- `solstone-linux_<VERSION>-1_amd64.deb`
- `solstone-linux-<VERSION>-1.x86_64.rpm`
- `solstone-linux-<VERSION>-linux-x86_64.tar.gz`
- `solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json`
- `solstone-linux-<VERSION>-linux-x86_64.rust-release-manifest.json.minisig`
- `SHA256SUMS`

and on aarch64 (64-bit ARM), from 2.0.26:

- `solstone-linux_<VERSION>-1_arm64.deb`
- `solstone-linux-<VERSION>-1.aarch64.rpm`
- `solstone-linux-<VERSION>-linux-aarch64.tar.gz`
- `solstone-linux-<VERSION>-linux-aarch64.rust-release-manifest.json`
- `solstone-linux-<VERSION>-linux-aarch64.rust-release-manifest.json.minisig`
- `SHA256SUMS-aarch64`

`uname -m` prints which one your computer is. The commands below use the x86_64
names; on aarch64, use the aarch64 names above in their place.

**Verify first.** Download the package you will install, the release manifest,
and its adjacent `.minisig` file:

```bash
base="https://updates.solstone.app/solstone-linux/release/$SOLSTONE_VERSION"
curl -fLO "$base/<the-package-you-will-install>"
curl -fLO "$base/solstone-linux-$SOLSTONE_VERSION-linux-x86_64.rust-release-manifest.json"
curl -fLO "$base/solstone-linux-$SOLSTONE_VERSION-linux-x86_64.rust-release-manifest.json.minisig"
```

Then fetch the published key, authenticate the manifest, and check the package
against the digest in that manifest:

```bash
curl -fLo solstone-linux-release.pub https://updates.solstone.app/solstone-linux/minisign.pub
minisign -Vm "solstone-linux-$SOLSTONE_VERSION-linux-x86_64.rust-release-manifest.json" -p solstone-linux-release.pub
jq -r --arg package "<downloaded-package>" \
  '.artifacts[] | select(.path == $package) | "\(.sha256)  \(.path)"' \
  "solstone-linux-$SOLSTONE_VERSION-linux-x86_64.rust-release-manifest.json" \
  | sha256sum -c -
```

Minisign authenticates the release manifest. The next command checks the
downloaded package against the authenticated SHA-256 digest. Replace
`<downloaded-package>` with its exact filename, such as
`solstone-linux_${SOLSTONE_VERSION}-1_amd64.deb`. You run these checks; `apt`,
`dnf`, and `scripts/install.sh` do not. If either command refuses the files,
stop.

Then install one of the release artifacts. Use the Debian or RPM package
published for your distribution when available.

**Debian / Ubuntu:** `apt` does not check our minisign signature, so complete
the verify-first step above before running:

```bash
sudo apt install "./solstone-linux_${SOLSTONE_VERSION}-1_amd64.deb"
```

**Fedora / RHEL:** `dnf` does not check our minisign signature, so complete the
verify-first step above before running:

```bash
sudo dnf install "./solstone-linux-$SOLSTONE_VERSION-1.x86_64.rpm"
```

For the portable archive, `scripts/install.sh` does not check our minisign
signature either. Complete the verify-first step above before running it. The
installer is in the matching release source checkout, not inside the archive:

```bash
scripts/install.sh "solstone-linux-$SOLSTONE_VERSION-linux-x86_64.tar.gz"
```

If you downloaded only the tarball, obtain `scripts/install.sh` from the
matching release source. Alternatively, extract the archive, copy
the files from `bin/` to a directory on `PATH`, and copy `share/icons/hicolor`
beneath the same installation prefix. Copy `share/applications` there when it
is included. Copy the bundled license notices and `INSTALL-NOTES` to
`share/doc/solstone-linux` beneath that prefix.

The archive includes `INSTALL-NOTES`, which is the canonical cross-distribution
runtime dependency list, and `RUST_DEPENDENCY_NOTICES.txt`, reproducing third-party crate license texts. Native packages install the app under the system prefix.

### Update a release

If you installed the quick way, update with the same command and `--upgrade`.
It restarts the app on the new version:

```bash
curl -fsSL https://solstone.app/install.sh | sh -s -- --components desktop --upgrade
```

To update by hand, verify and install the newer package as above, then run:

```bash
solstone-linux install-service
```

Installing a package replaces the app on disk but doesn't restart it, so the
solstone app keeps running the old version until this step. It rewrites the
service files for the new version and restarts the app. If the window is open,
close it and open it again.

### Open the window

The window comes with version 2.0.24 and later. On an earlier version, use
[terminal pairing](#pair-from-a-terminal).

Open solstone from your app menu or the panel icon. No panel icon extension is
needed for the window. It has four tabs: `status`, `journal`, `settings` and `help`.
If the solstone app isn't running, choose `start the solstone app` when you're
ready. This sets it up to run now and when you sign in. Opening or closing the
window does not start or stop intake.

In `journal`, paste the pairing link from your journal. Compare the displayed mark
with your journal's mark before confirming it. You can pair again while intake
continues. `open your journal` opens it in your browser.

In `settings`, change frame rate, whether to show the cursor, whether to start
paused, or segment duration. The config file remains the source of truth:
`$XDG_CONFIG_HOME/solstone-linux/config.json`, normally
`~/.config/solstone-linux/config.json`. Saving keeps unknown and hand-added keys.
The window says when a saved change needs a restart and offers to restart the
solstone app when it can safely do so.

In `help`, read recent logs before choosing `save a copy…`. The saved file is
exactly the preview you read. Saving does not send it anywhere.

The window requires GTK 4.6 or newer. Debian and RPM declare the dependency;
the portable archive needs it installed separately. The intake process has no
GTK dependency and keeps running when the window closes or cannot start.

### Pair from a terminal

Save the pairing link as `pairing-link.txt`. The service command writes the
systemd user unit and desktop autostart entry, enables the unit, and starts the
solstone app:

```bash
solstone-linux install-service
systemctl --user stop solstone-linux
solstone-linux setup < pairing-link.txt
systemctl --user start solstone-linux
```

Pairing is the only setup path:

```bash
solstone-linux setup < pairing-link.txt
```

```bash
solstone-linux setup --mark "word word" < pairing-link.txt
```

The pairing link comes from your journal. A journal on the same machine uses the
same private network path as any other journal. There is no URL, key, local installation
of the journal or Python, or direct fallback to configure. The solstone app can
continue taking in what you share while unpaired or offline. Setup asks you to confirm your journal's mark, or takes `--mark` when there is no terminal. Nothing goes into your journal until you confirm. `solstone-linux confirm` confirms a mark while the solstone app is running. Setup still needs the app stopped, because setup takes the private-state lock.

## Build from source

Install rustup, a C toolchain, CMake, pkg-config, GTK4/GLib/GStreamer development headers (GTK4's are `gtk4-devel` on Fedora and `libgtk-4-dev` on Debian and Ubuntu), PulseAudio development headers, and the `minisign` and `shellcheck` commands, which `make ci` runs. Then:

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make bootstrap
make ci
make install
solstone-linux window
```

`rust-toolchain.toml` selects the exact compiler, components, and target. `make
install` explicitly establishes them and cargo-deny before installing the app.

A source install does not add solstone to your app menu. Open the window with
`solstone-linux window`.

## Update from source

```bash
git pull
make agent-setup
make ci
make install
solstone-linux window
```

`make agent-setup` downloads any crates the new version added, which `make ci`
needs because it runs offline. A new version can also need a new development
package; if the build stops on a missing system library, install the headers
listed under Build from source. `make install` builds both binaries before it
installs either, so a failed build leaves the installed app unchanged. If the
solstone app is running, `make install` restarts it on the new version. If the
window is open, close it and open it again.

Setup and runtime deliberately share one private-state lock. Confirm changes pairing state while the solstone app is running, and setup still needs the app stopped because it takes the lock. If the solstone app is running, setup exits before consuming any input and leaves intake state, configuration, and private state unchanged.

## Verify

```bash
systemctl --user status solstone-linux
solstone-linux status
```

## Desktop notes

On Wayland, the first time the solstone app starts, your desktop asks which screens to share. Nothing from the screen is taken in until you answer, and your choice is kept for later starts.

Activity detection uses screen-lock and power-save signals to notice when you step away. GNOME provides both signals; KDE Wayland provides screen lock; X11 can also provide DPMS power save. Where neither signal is available, the solstone app on linux still takes in what you share and that material goes into your journal, but activity-based segment boundaries do not trigger.

### Which desktop session it takes in

The solstone app takes in only your own desktop session, and only while it is the one in
front of the screen. What it takes in goes into your journal. It asks the computer's login
manager which session is in front, as it runs, so when you switch sessions it follows you
without a restart.

When your session is not the one in front of the screen, the solstone app takes in
nothing, neither screen nor sound, until it is again. That covers someone else's session,
the login screen, and the time after you log out. A separate session you log in to from
another computer, such as a remote login, is never taken in. Sharing the desktop that's in
front of the screen doesn't stop intake.

When your screen is locked, the solstone app stops taking in the screen until you unlock it.

If the panel icon says "can't reach your desktop", your session is in front but the
solstone app can't connect to it. Log out and back in to fix that. `solstone-linux doctor` shows which session it is following.

### The panel icon

The panel icon is the tray icon on KDE and an icon in the top panel on GNOME. It is
one way to reach pause and resume. It uses the StatusNotifierItem D-Bus protocol: KDE supports
that directly, GNOME does not, and GNOME needs the AppIndicator extension before an icon
can appear at all.

On GNOME, run:

```bash
solstone-linux panel-icon
```

What happens next depends on whether the extension is already on your machine.

If it is not, GNOME asks you to confirm, then installs the AppIndicator extension from
extensions.gnome.org and turns it on in your current session. There is no logout. The
solstone app picks up the panel icon within a few seconds.

If you installed the rpm, the extension came with it and is already on disk, but GNOME has
not loaded it yet. Log out and back in once, then run the command again to turn it on.

Either way, `solstone-linux doctor` reports what it found.

If the command cannot reach extensions.gnome.org, or your desktop does not allow
extensions to be installed, install your distribution's package instead, then log out and
back in, then run the command again:

```bash
sudo dnf install gnome-shell-extension-appindicator   # Fedora
# log out and back in
solstone-linux panel-icon
```

While the panel icon is missing, the solstone app offers to set it up once per run.
"don't ask again" writes `"panel_icon_offer": false` into
`~/.config/solstone-linux/config.json`. Set it back to `true` there and restart the app
to see the offer again.

Without a panel icon the solstone app keeps working normally, and everything you share
with it still reaches your journal. Use a terminal:

```bash
solstone-linux pause               # until you resume
solstone-linux pause --minutes 30  # ends on its own
solstone-linux resume
```

These are the same controls the panel icon menu uses, so they work whether or not a panel
icon is there. They also work over SSH, as long as you are signed in as the same user.

## Historical note: version 0.4.5

Version 0.4.5 was the final pre-native Python release. Current installation uses
the native Debian, RPM, or portable package described above.
