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

ARCH=$(uname -m)
if [[ "$ARCH" != "x86_64" ]]; then
  echo "error: unsupported architecture '$ARCH'; this release is x86_64 only" >&2
  exit 1
fi

ARCHIVE_NAME=${ARCHIVE##*/}
if [[ ! "$ARCHIVE_NAME" =~ ^solstone-linux-([0-9][0-9A-Za-z.+-]*)-linux-x86_64\.tar\.gz$ ]]; then
  echo "error: unexpected archive name '$ARCHIVE_NAME'" >&2
  exit 1
fi
VERSION=${BASH_REMATCH[1]}
TOP="solstone-linux-${VERSION}-linux-x86_64"

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
  "$TOP/LICENSE" \
  "$TOP/INSTALL-NOTES" \
  "$TOP/share/icons/hicolor/scalable/apps/solstone-observer.svg"; do
  if ! grep -Fxq "$REQUIRED" <<< "$CONTENTS"; then
    echo "error: archive is missing $REQUIRED" >&2
    exit 1
  fi
done

echo "solstone-linux $VERSION install plan ($DISTRO_FAMILY, x86_64):"
echo "  install binary: $PREFIX/bin/solstone-linux"
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
  "$PREFIX/share/icons"
install -m 0755 "$SOURCE/bin/solstone-linux" "$PREFIX/bin/solstone-linux"
install -m 0644 "$SOURCE/LICENSE" "$PREFIX/share/doc/solstone-linux/LICENSE"
install -m 0644 "$SOURCE/INSTALL-NOTES" "$PREFIX/share/doc/solstone-linux/INSTALL-NOTES"
while IFS= read -r -d '' ICON; do
  RELATIVE_ICON=${ICON#"$SOURCE/share/icons/hicolor/"}
  install -D -m 0644 "$ICON" "$PREFIX/share/icons/hicolor/$RELATIVE_ICON"
done < <(find "$SOURCE/share/icons/hicolor" -type f -print0)

echo "installed solstone-linux $VERSION under $PREFIX"
echo "optional after the Rust command is implemented: solstone-linux install-service"
