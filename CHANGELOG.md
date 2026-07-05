# Changelog

All notable changes to solstone-linux are documented here.
The format is based on Keep a Changelog (https://keepachangelog.com/),
and this project adheres to Semantic Versioning.

## [0.4.4] - 2026-07-04

### Fixed
- stopping or restarting sol now shuts down promptly, even with an upload mid-retry. that upload used to wait out its full retry delay first; now it ends the moment you quit sol or the machine powers off. a bad retry setting in your config no longer leaves the uploader stuck, either.

## [0.4.3] - 2026-07-03

### Changed
- the app now calls itself sol everywhere you see it — the launcher, tray, menus, status, and notifications. your journal is the memory it keeps, and solstone is the platform underneath. the command you run stays `solstone-linux`; nothing about what it does changed, only what it's called.
- segments your journal rejected or sol couldn't recover are now held for 30 days before they're removed, instead of being dropped silently. `status` and `doctor` show the count, so you can see when any are waiting.

### Fixed
- local cleanup now deletes a synced segment only after your journal confirms, file by file, that it holds everything in it. previously a segment could be cleaned up while your journal was missing part of it. if your journal can't yet confirm file by file, cleanup holds off and keeps the local copy.
- sol now recovers on its own from situations that used to leave it quietly stalled or stopped: the first screen-share dialog being dismissed, a journal that's slow to respond or offline, and speakers muted at startup (it goes on without audio and picks it back up when a device is available). an accidental second copy now declines to run rather than disturb the one already going. a round of smaller stability improvements rides along.
- closing the lid on a docked KDE laptop no longer makes sol go idle.
- chat notifications now come back on their own after a network drop, and dismissing a notification no longer counts as opening it.

## [0.4.2] - 2026-06-29

### Added
- this observer now sends your journal a small, diagnostics-only health note alongside its regular check-in, covering its name, version, how long it's been running, and whether syncing is keeping up. it carries none of what it experiences with you: no screen, audio, file paths, or titles. just enough for you to see at a glance that an observer is alive and in good health.

### Changed
- this observer now carries the sol mark across your desktop, in the app launcher and menus. the tray status icons are unchanged.

### Fixed
- installing solstone-linux now works cleanly on current debian and ubuntu. the earlier steps could fail while rebuilding the desktop graphics libraries from scratch; the updated install reuses the ones already on your system, so it goes through.

## [0.4.1] - 2026-06-17

### Added
- you can now check which version you're running. `solstone-linux --version` prints it, so when you're following along with the release notes or asking for help, you know exactly what you have.

## [0.4.0] - 2026-06-17

### Added
- a new `solstone-linux settings` command lets you adjust how this observer behaves after setup, from one place instead of hand-editing a config file. you can change how often it makes a segment, the framerate, whether it starts paused, the chat bridge, and how long it keeps local cache. setup itself stays prompt-free; your identity and pairing are left untouched.

### Changed
- this observer's settings now live under `~/.config/solstone-linux/`, where linux tools expect config to be. if you're upgrading, the move happens on its own the first time you run, with nothing to redo: no re-setup, no re-pairing. your segments stay exactly where they are.

## [0.3.3] - 2026-06-16

### Fixed
- the tray status submenu now refreshes its values every time you open it. the segment countdown, cache size, captures today, uptime, and sync line had been showing stale values on reopen on some desktops; they now reflect the current state each time you open the menu.

## [0.3.2] - 2026-06-16

### Changed
- the tray status now tells the truth about sync. it shows "connected" only when this observer has genuinely reached your journal with nothing left to send, and clearly says when it's offline, needs updating, or needs to re-authorize, instead of looking fine while quietly falling behind. the same honest status carries across the tray, `status`, and `doctor`.

### Fixed
- setup no longer asks for a journal url under any path. if you ran into a lingering "journal url" prompt during setup, that's gone — setup connects to your local journal automatically, and `solstone-linux setup --server-url <url>` still points at a journal you reach directly.

## [0.3.1] - 2026-06-15

### Changed
- chat notifications now use the journal's current callosum connection path,
  with the observer key still sent in the authorization header.

## [0.3.0] - 2026-06-14

setup is now zero-config: the observer connects to your journal automatically,
with no url to type.

### Changed
- setup no longer asks for a journal url. if your journal runs on another
  machine you reach directly, set its address with
  `solstone-linux setup --server-url <url>`.

## [0.2.0] - 2026-06-13

setup is now hands-off: the first time the observer runs, it connects itself
to your journal automatically, with no separate key step.

### Changed

- **first run sets itself up.** earlier versions asked you to create and paste
  a key to connect the observer to your journal. now the observer introduces
  itself to your journal on first run and remembers the connection on its own.
  you go straight from install to observing, with no manual key step.

## [0.1.1] - 2026-06-02

A focused maintenance release: two reliability fixes and a round of
install-instruction corrections from fresh-machine testing on Fedora,
Debian, and openSUSE.

### Fixed

- **Idle monitors no longer silently drop observations.** When a monitor
  produced no frames during a segment (a static screen with nothing
  changing on it), GStreamer still wrote a header-only WebM file. Those
  empty files were finalized, uploaded, and then failed to process in your
  journal — so that monitor's segment was lost without any signal. The
  observer now drops these empty recordings at the source and emits an
  `observe.stream_silent` event (logged at WARNING) so the gap is visible
  instead of silent.
- **Install no longer clobbers your system icon theme.** On GNOME,
  `install-service` was writing a stray `index.theme` into the shared
  hicolor icon directory, which shadowed the system index and caused
  unrelated app icons to render as the solstone diamond. The installer now
  drops only the solstone status icons (the system index already declares
  their directory) and self-heals any previously broken install on the next
  `install-service` run. A foreign or unreadable `index.theme` is left
  untouched.

### Documentation

- Corrected the Fedora and Debian system-dependency lines after fresh-box
  install testing: dropped packages that do not exist in their repos
  (`gstreamer1-plugin-pipewire` on Fedora, `gir1.2-gdk-4.0` on Debian) and
  hoisted the cairo / pycairo build toolchain onto the main install line so
  a fresh install succeeds in one shot. Added `gstreamer1.0-tools` to the
  Debian line — `gst-launch-1.0` is required for screen recording and is
  not pulled in transitively.
- Added a verified openSUSE dependency block and mirrored the corrected
  dependency lists between `README.md` and `INSTALL.md`.
- Updated the install path to lead with `pipx install solstone-linux`, then
  `solstone-linux install-service`, then `solstone-linux setup`.

### Internal

- The release script now tags the commit and cuts a GitHub release only on
  a production PyPI run; a TestPyPI run no longer leaves a tag or public
  release behind.

## [0.1.0] - 2026-05-19

First public release of solstone-linux — the Linux desktop observer
for your solstone journal.

solstone-linux runs as a systemd user service in your GNOME Wayland
session. It experiences screen and audio along with you, holds short
segments locally, and uploads them to your journal in the background.

### Install paths

- From PyPI: `pipx install --system-site-packages solstone-linux`,
  then `solstone-linux install-service` to register the systemd unit.
- From a clone: `git clone` this repo and run `make install-service`
  for development or unreleased changes.

Both paths rely on host packages for PyGObject, GStreamer with the
PipeWire plugin, PipeWire itself, `pactl`, and `xdg-desktop-portal`
with ScreenCast support. PyGObject and the GStreamer bindings ride
along from system site-packages — that is why `--system-site-packages`
matters.
