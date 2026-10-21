#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Install a portable solstone-linux Rust release archive.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [--dry-run] [--prefix PATH] <tarball>

The default prefix is $HOME/.local. A system prefix is an explicit operator
choice and may require running this script with suitable privileges.
EOF
}

DRY_RUN=0
PREFIX="${HOME:?HOME is required}/.local"
PREFIX_EXPLICIT=0
ARCHIVE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --prefix)
      if [[ $# -lt 2 ]]; then
        echo "error: --prefix requires a path" >&2
        exit 2
      fi
      PREFIX="$2"
      PREFIX_EXPLICIT=1
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "error: unknown option '$1'" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$ARCHIVE" ]]; then
        echo "error: expected exactly one release tarball" >&2
        exit 2
      fi
      ARCHIVE="$1"
      shift
      ;;
  esac
done

if [[ -z "$ARCHIVE" ]]; then
  echo "error: a release tarball is required" >&2
  usage >&2
  exit 2
fi
if [[ ! -f "$ARCHIVE" ]]; then
  echo "error: release tarball not found: $ARCHIVE" >&2
  exit 1
fi
if [[ "$PREFIX" != /* ]]; then
  PREFIX="$PWD/$PREFIX"
fi

ARCH=$(uname -m)
if [[ "$ARCH" != "x86_64" && "$ARCH" != "aarch64" ]]; then
  echo "error: unsupported architecture '$ARCH'; releases are x86_64 and aarch64 only" >&2
  exit 1
fi

ARCHIVE_NAME=${ARCHIVE##*/}
if [[ ! "$ARCHIVE_NAME" =~ ^solstone-linux-([0-9][0-9A-Za-z.+-]*)-linux-(x86_64|aarch64)\.tar\.gz$ ]]; then
  echo "error: unexpected archive name '$ARCHIVE_NAME'" >&2
  exit 1
fi
RELEASE_VERSION=${BASH_REMATCH[1]}
if [[ "${BASH_REMATCH[2]}" != "$ARCH" ]]; then
  echo "error: this archive is for ${BASH_REMATCH[2]}, but this computer is $ARCH" >&2
  exit 1
fi
TOP="solstone-linux-${RELEASE_VERSION}-linux-${ARCH}"

OS_RELEASE=${SOLSTONE_INSTALL_OS_RELEASE:-/etc/os-release}
if [[ ! -r "$OS_RELEASE" ]]; then
  echo "error: cannot identify this Linux distribution: $OS_RELEASE is unreadable" >&2
  exit 1
fi
ID=""
ID_LIKE=""
# os-release is shell-compatible and is the distro's canonical identity file.
# shellcheck disable=SC1090
. "$OS_RELEASE"
DISTRO_WORDS=" ${ID:-} ${ID_LIKE:-} "
case "$DISTRO_WORDS" in
  *" debian "*|*" ubuntu "*) DISTRO_FAMILY="debian" ;;
  *" fedora "*|*" rhel "*|*" centos "*) DISTRO_FAMILY="fedora" ;;
  *" opensuse "*|*" suse "*) DISTRO_FAMILY="opensuse" ;;
  *" arch "*) DISTRO_FAMILY="arch" ;;
  *)
    echo "error: unsupported Linux distribution '${ID:-unknown}'; install stops without changes" >&2
    exit 1
    ;;
esac

CONTENTS=$(tar -tzf "$ARCHIVE") || {
  echo "error: cannot read release tarball: $ARCHIVE" >&2
  exit 1
}
while IFS= read -r ENTRY; do
  case "$ENTRY" in
    "$TOP"|"$TOP"/*) ;;
    *)
      echo "error: archive contains an unexpected path: $ENTRY" >&2
      exit 1
      ;;
  esac
  case "/$ENTRY/" in
    */../*)
      echo "error: archive contains an unsafe path: $ENTRY" >&2
      exit 1
      ;;
  esac
done <<< "$CONTENTS"

for REQUIRED in \
  "$TOP/bin/solstone-linux" \
  "$TOP/bin/solstone-linux-window" \
  "$TOP/LICENSE" \
  "$TOP/INSTALL-NOTES" \
  "$TOP/Comfortaa-OFL.txt" \
  "$TOP/RUST_DEPENDENCY_NOTICES.txt" \
  "$TOP/share/icons/hicolor/scalable/apps/solstone-observer.svg" \
  "$TOP/share/applications/org.solpbc.solstone.Window.desktop" \
  "$TOP/share/solstone-linux/activity-helpers/solstone-activity@solpbc.org/metadata.json" \
  "$TOP/share/solstone-linux/activity-helpers/solstone-activity@solpbc.org/extension.js" \
  "$TOP/share/solstone-linux/activity-helpers/solstone-activity-kwin/metadata.json" \
  "$TOP/share/solstone-linux/activity-helpers/solstone-activity-kwin/contents/code/main.js"; do
  if ! grep -Fxq "$REQUIRED" <<< "$CONTENTS"; then
    echo "error: archive is missing $REQUIRED" >&2
    exit 1
  fi
done

echo "solstone-linux $RELEASE_VERSION install plan ($DISTRO_FAMILY, $ARCH):"
echo "  install binary: $PREFIX/bin/solstone-linux"
echo "  install binary: $PREFIX/bin/solstone-linux-window"
echo "  install icons:  $PREFIX/share/icons/hicolor/"
echo "  install docs:   $PREFIX/share/doc/solstone-linux/"
if [[ ":$PATH:" != *":$PREFIX/bin:"* ]]; then
  echo "  notice: add $PREFIX/bin to PATH"
fi
if [[ $DRY_RUN -eq 1 ]]; then
  echo "dry-run: no filesystem changes made"
  exit 0
fi

if [[ $PREFIX_EXPLICIT -eq 1 && "$PREFIX" != "$HOME/.local" ]]; then
  PREFIX_PARENT=${PREFIX%/*}
  [[ -n "$PREFIX_PARENT" ]] || PREFIX_PARENT="/"
  if { [[ -e "$PREFIX" ]] && [[ ! -w "$PREFIX" ]]; } || \
     { [[ ! -e "$PREFIX" ]] && [[ ! -w "$PREFIX_PARENT" ]]; }; then
    echo "error: prefix '$PREFIX' is not writable; explicitly rerun with suitable privileges" >&2
    exit 1
  fi
fi

EXTRACT_DIR=$(mktemp -d)
trap 'rm -rf "$EXTRACT_DIR"' EXIT
tar -xzf "$ARCHIVE" -C "$EXTRACT_DIR"
SOURCE="$EXTRACT_DIR/$TOP"

mkdir -p \
  "$PREFIX/bin" \
  "$PREFIX/share/doc/solstone-linux" \
  "$PREFIX/share/applications" \
  "$PREFIX/share/icons" \
  "$PREFIX/share/solstone-linux/activity-helpers"
install -m 0755 "$SOURCE/bin/solstone-linux" "$PREFIX/bin/solstone-linux"
install -m 0755 "$SOURCE/bin/solstone-linux-window" "$PREFIX/bin/solstone-linux-window"
install -m 0644 "$SOURCE/LICENSE" "$PREFIX/share/doc/solstone-linux/LICENSE"
install -m 0644 "$SOURCE/Comfortaa-OFL.txt" "$PREFIX/share/doc/solstone-linux/Comfortaa-OFL.txt"
install -m 0644 "$SOURCE/INSTALL-NOTES" "$PREFIX/share/doc/solstone-linux/INSTALL-NOTES"
install -m 0644 "$SOURCE/RUST_DEPENDENCY_NOTICES.txt" "$PREFIX/share/doc/solstone-linux/RUST_DEPENDENCY_NOTICES.txt"
while IFS= read -r -d '' ICON; do
  RELATIVE_ICON=${ICON#"$SOURCE/share/icons/hicolor/"}
  install -D -m 0644 "$ICON" "$PREFIX/share/icons/hicolor/$RELATIVE_ICON"
done < <(find "$SOURCE/share/icons/hicolor" -type f -print0)
while IFS= read -r -d '' HELPER; do
  RELATIVE_HELPER=${HELPER#"$SOURCE/share/solstone-linux/activity-helpers/"}
  install -D -m 0644 "$HELPER" "$PREFIX/share/solstone-linux/activity-helpers/$RELATIVE_HELPER"
done < <(find "$SOURCE/share/solstone-linux/activity-helpers" -type f -print0)

# Exec is parsed twice: first the desktop-entry string escaping, then the
# quoted argument. Use an absolute path so app-menu launch does not depend on
# the interactive shell's PATH. No prefix byte becomes shell source.
DESKTOP_PREFIX=""
for ((INDEX=0; INDEX<${#PREFIX}; INDEX++)); do
  CHARACTER=${PREFIX:INDEX:1}
  case "$CHARACTER" in
    $'\\') DESKTOP_PREFIX+=$'\\\\\\\\' ;;
    '"'|'`'|'$') DESKTOP_PREFIX+=$'\\\\'; DESKTOP_PREFIX+="$CHARACTER" ;;
    '%') DESKTOP_PREFIX+='%%' ;;
    $'\n') DESKTOP_PREFIX+='\n' ;;
    $'\r') DESKTOP_PREFIX+='\r' ;;
    $'\t') DESKTOP_PREFIX+='\t' ;;
    *) DESKTOP_PREFIX+="$CHARACTER" ;;
  esac
done
while IFS= read -r LINE; do
  case "$LINE" in
    Exec=*)
      # The desktop-launched shell expands $1, never this installer.
      # shellcheck disable=SC2016
      printf '%s%s%s\n' 'Exec=/bin/sh -c "exec \\"\\$1\\" window" sh "' "$DESKTOP_PREFIX" '/bin/solstone-linux"'
      ;;
    *) printf '%s\n' "$LINE" ;;
  esac
done < "$SOURCE/share/applications/org.solpbc.solstone.Window.desktop" > "$PREFIX/share/applications/org.solpbc.solstone.Window.desktop"
chmod 0644 "$PREFIX/share/applications/org.solpbc.solstone.Window.desktop"

echo "installed solstone-linux $RELEASE_VERSION under $PREFIX"
echo "next: solstone-linux window"
