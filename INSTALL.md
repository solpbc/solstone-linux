# installing solstone-linux

these instructions are for a coding agent and human working together. solstone-linux is a standalone observer that experiences your screen and audio along with you on linux desktops using PipeWire and GStreamer, and uploads to your solstone journal.

solstone must already be installed and running. if it isn't, start there: https://solstone.app/install

> **most users install solstone-linux from PyPI in three commands** on the machine that will host the observer: `pipx install --system-site-packages solstone-linux`, `solstone-linux install-service`, then `solstone-linux setup` (which registers against your journal over the local `http://localhost:5015` link — no URL to type). if the observer machine reaches your solstone host directly instead, run `solstone-linux setup --server-url <journal-url>`. the instructions below are for developers building from source or troubleshooting the install.
>
> the `--system-site-packages` flag is **required**: it lets pipx's virtualenv reuse your distro's system PyGObject, pycairo, and GStreamer bindings (the `python3-gi` / `python3-cairo` packages installed below) instead of rebuilding PyGObject from source. a plain `pipx install solstone-linux` rebuilds PyGObject in an isolated venv, which needs the full GObject-Introspection build toolchain (`libgirepository-2.0-dev`) — and that dev package isn't even available on every supported distro (Debian 12 stable doesn't ship it). `--system-site-packages` is the path that works on every distro and skips the compile entirely.

## before you begin

if `sol` is not in PATH, check `~/.local/bin/sol`.

check if solstone-linux is already installed and running:

```
systemctl --user status solstone-linux
journal observer list
```

if it's already active and connected, you're done.

## what to sort out together

- **system dependencies.** the observer needs PyGObject, GStreamer, and PipeWire bindings from system packages. installing these requires sudo.
- **stream name.** this identifies this observer's stream. the machine's hostname is the typical choice.

### journal reached directly (not over the local link)

By default the observer registers over the local `http://localhost:5015` link, so the journal and observer are colocated. If you reach your journal directly over HTTPS instead:

- clone anywhere; the `$(sol root)/observers` path in step 2 only applies when sol is installed locally.
- run `solstone-linux setup --server-url <journal-url>` to point at that journal and auto-register the observer over HTTP, persisting the returned key. No manual key handoff is needed if the journal's observer-registration endpoint is reachable.
- otherwise, the install sequence below is the same.

## install sequence

this is the developer/from-source path; most installs should use the `pipx install --system-site-packages solstone-linux` + `solstone-linux install-service` + `solstone-linux setup` flow described in the callout above.

1. install system dependencies for your distro, including `pipx`. if you need sudo, walk your human through it.

   **fedora:**
   ```
   sudo dnf install python3-gobject python3-cairo gtk4 gstreamer1-plugins-base pipewire-gstreamer alsa-lib-devel pulseaudio-utils pipewire-pulseaudio xdg-desktop-portal pipx gcc python3-devel pkgconf-pkg-config cairo-devel cairo-gobject-devel
   ```

   **debian / ubuntu:**
   ```
   sudo apt install python3-gi python3-cairo gir1.2-gtk-4.0 gstreamer1.0-pipewire gstreamer1.0-tools libasound2-dev pulseaudio-utils pipewire-pulse xdg-desktop-portal pipx gcc python3-dev pkg-config libcairo2-dev
   ```

   **arch:**
   ```
   sudo pacman -S python-gobject gtk4 gstreamer gst-plugin-pipewire libpulse alsa-lib xdg-desktop-portal pipx
   ```

   **opensuse:**
   ```
   sudo zypper install python3-gobject python3-gobject-Gdk typelib-1_0-Gtk-4_0 \
     gtk4-tools gstreamer-plugins-base gstreamer-plugin-pipewire \
     pipewire-pulseaudio pulseaudio-utils alsa-devel \
     xdg-desktop-portal python3-pipx
   ```
   note: package names diverge from Fedora — `typelib-1_0-Gtk-4_0` (not `gtk4`), `gstreamer-plugin-pipewire` (singular), and `alsa-devel` (not `alsa-lib-devel`).

   with the recommended `pipx install --system-site-packages solstone-linux`, the system `python3-gi` and `python3-cairo` packages above satisfy PyGObject and pycairo, so **neither builds from source**. the `cairo` headers + `gcc` + Python dev headers in the Fedora/Debian lines are kept as a fallback for any other pure-Python dependency that lacks a prebuilt wheel on your platform — and they're what an *isolated*-venv install (a plain `pipx install` without `--system-site-packages`) needs to compile pycairo from source. (an isolated-venv install also needs `libgirepository-2.0-dev`; see Troubleshooting.)

   `uv` / `pipx`: Fedora packages both (`sudo dnf install uv pipx`); Debian/Ubuntu package `pipx` but not `uv`. the PyPI install flow only needs `pipx` — `uv` is optional and used by the from-source dev workflow in the Makefile.

2. cloning into `$(sol root)/observers` is only a developer convenience for keeping observer checkouts colocated with a local solstone clone. for a journal you reach directly, clone anywhere — the observer runs independently of your journal at runtime:
   ```
   cd "$(sol root)/observers"
   git clone https://github.com/solpbc/solstone-linux.git
   cd solstone-linux
   make install-service
   ```
   `make install-service` is a smart install-or-upgrade: detects fresh-install vs upgrade via a marker file, runs CI in upgrade mode, guards against cross-repo contamination.

3. run setup:
   ```
   solstone-linux setup
   ```
   this registers the observer against your journal over the local `http://localhost:5015` link — no URL to type. pass `--server-url <journal-url>` for a journal you reach directly.

4. verify the service is running:
   ```
   systemctl --user status solstone-linux
   ```

## updating after a code change

```
git pull && make install-service
```

## notes

- activity detection (idle timeout, screen lock, power save) works on both GNOME and KDE. on other desktops the observer still experiences your screen and audio fine, but activity-based segment boundaries won't trigger.
- the tray icon uses the StatusNotifierItem (SNI) D-Bus protocol. it works on KDE natively and GNOME with the AppIndicator extension. if no SNI host is available, the observer runs normally without a tray icon.

## appendix: GNOME tray support

the system tray icon appears automatically when the observer starts in a graphical session. on KDE Plasma this works out of the box. on GNOME, the AppIndicator extension is required.

GNOME removed native system tray support. the AppIndicator extension restores it via the same StatusNotifierItem protocol KDE uses. without it, the observer runs fine but has no tray icon.

**ubuntu:** already installed and enabled by default — skip this step.

**fedora:**
```
sudo dnf install gnome-shell-extension-appindicator
```
then log out and back in, or restart GNOME Shell (Alt+F2, type `r`, enter). enable the extension in GNOME Extensions app if not auto-enabled.

**arch:**
```
sudo pacman -S gnome-shell-extension-appindicator
```

**other distros (openSUSE, etc.):**

if your distro doesn't ship an AppIndicator extension package, install it from extensions.gnome.org via the CLI:

```
curl -LO https://extensions.gnome.org/extension-data/appindicatorsupportrgcjonas.gmail.com.v64.shell-extension.zip
gnome-extensions install appindicatorsupportrgcjonas.gmail.com.v64.shell-extension.zip
gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com
```

then restart GNOME Shell — on Wayland, log out and back in; on X11, press Alt+F2 and type `r`. v64 supports GNOME Shell 45–50; check https://extensions.gnome.org/extension/615/appindicator-support/ for a newer build if you're on a later shell.

to check if it's working: `gnome-extensions list | grep appindicator` should show it. if the tray icon still doesn't appear, verify it's enabled: `gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com`

## Troubleshooting

Common install-time errors and their fixes:

- **`pkg-config: command not found` or `cairo` pkg-config failure**
  - fedora: `sudo dnf install pkgconf-pkg-config cairo-devel`
  - debian/ubuntu: `sudo apt install pkg-config libcairo2-dev`
  - arch: `sudo pacman -S pkgconf cairo`
  - opensuse: `sudo zypper install pkgconf-pkg-config cairo-devel`

- **`girepository-2.0` missing or `pygobject` build failure** — only hit when installing into an *isolated* venv (a plain `pipx install` **without** `--system-site-packages`), which rebuilds PyGObject from PyPI from source. the recommended `pipx install --system-site-packages solstone-linux` uses your system `python3-gi` and skips this build entirely.
  - **first, retry with `--system-site-packages`.** `pipx install --system-site-packages solstone-linux` needs no build toolchain. this is the fix on Debian 12 (stable) and other distros that don't package the `-2.0` dev headers at all.
  - if you must build from source: PyPI's PyGObject (3.50+, Sept 2024 onward) needs the girepository-**2.0** dev headers, not the old 1.0 package:
    - fedora: `sudo dnf install gobject-introspection-devel`
    - debian/ubuntu: `sudo apt install libgirepository-2.0-dev` (Debian 13 / Ubuntu 24.04+; older releases that ship PyGObject < 3.50 use `libgirepository1.0-dev`)
    - arch: `sudo pacman -S gobject-introspection`
    - opensuse: `sudo zypper install gobject-introspection-devel`

- **`Python.h: No such file or directory`**
  - fedora: `sudo dnf install python3-devel`
  - debian/ubuntu: `sudo apt install python3-dev`
  - arch: already bundled in `python` package
  - opensuse: `sudo zypper install python3-devel`

- **`pipx: command not found`**
  - fedora: `sudo dnf install pipx`
  - debian/ubuntu: `sudo apt install pipx`
  - arch: `sudo pacman -S python-pipx`
  - opensuse: `sudo zypper install python3-pipx`
