# solstone-linux

Standalone Linux desktop observer for [solstone](https://solpbc.org). Experiences your screen and audio along with you on a GNOME Wayland session, stores segments locally, and syncs to your solstone journal.

**Note:** Activity detection (idle timeout, screen lock, power save) currently requires a GNOME desktop. On other desktops (KDE, Sway, Hyprland, XFCE), the observer still experiences your screen and audio, but activity-based segment boundaries won't trigger.

## System Dependencies

   **Fedora:**
   ```
   sudo dnf install python3-gobject python3-cairo gtk4 gstreamer1-plugins-base pipewire-gstreamer alsa-lib-devel pulseaudio-utils pipewire-pulseaudio xdg-desktop-portal pipx gcc python3-devel pkgconf-pkg-config cairo-devel cairo-gobject-devel
   ```

   **Debian / Ubuntu:**
   ```
   sudo apt install python3-gi python3-cairo gir1.2-gtk-4.0 gstreamer1.0-pipewire gstreamer1.0-tools libasound2-dev pulseaudio-utils pipewire-pulse xdg-desktop-portal pipx gcc python3-dev pkg-config libcairo2-dev
   ```

   **Arch:**
   ```
   sudo pacman -S python-gobject gtk4 gstreamer gst-plugin-pipewire libpulse alsa-lib xdg-desktop-portal pipx
   ```

   **openSUSE:**
   ```
   sudo zypper install python3-gobject python3-gobject-Gdk typelib-1_0-Gtk-4_0 gtk4-tools gstreamer-plugins-base gstreamer-plugin-pipewire pipewire-pulseaudio pulseaudio-utils alsa-devel xdg-desktop-portal python3-pipx
   ```

## Install

solstone (the journal) must already be installed and running on the host this observer reports to. If it isn't, start with the [journal install](https://solstone.app/install).

On the machine that will host the observer:

```bash
pipx install solstone-linux
solstone-linux install-service
solstone-linux setup
```

`setup` prompts for your journal URL and registers the observer for you. If this machine can't reach your solstone host directly, mint a key from there with `sol observer create <name>` and paste it during setup.

### Developers building from source

```bash
git clone https://github.com/solpbc/solstone-linux.git
cd solstone-linux
make install-service
solstone-linux setup
```

See `INSTALL.md` for distro packages, tray notes, and troubleshooting details.

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

## License

AGPL-3.0-only — Copyright (c) 2026 sol pbc
