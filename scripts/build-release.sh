#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Build the Rust release rail in its distro-native packaging container.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-release.sh <deb|rpm> [x86_64]

Build the requested native package and the portable baseline tarball into
dist/rust/. Only x86_64 is currently supported.
EOF
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage >&2
  exit 2
fi

FORMAT="$1"
ARCH="${2:-$(uname -m)}"

case "$FORMAT" in
  deb|rpm) ;;
  *)
    echo "error: unsupported package format '$FORMAT'; expected deb or rpm" >&2
    usage >&2
    exit 2
    ;;
esac

if [[ "$ARCH" != "x86_64" ]]; then
  echo "error: unsupported architecture '$ARCH'; this release rail only builds x86_64" >&2
  exit 2
fi

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "error: run this script from a solstone-linux Git checkout" >&2
  exit 1
}
cd "$REPO_ROOT"

if ! git diff --quiet HEAD || [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree dirty; commit or stash changes before building release artifacts" >&2
  exit 1
fi

if command -v podman >/dev/null 2>&1; then
  ENGINE="podman"
elif command -v docker >/dev/null 2>&1; then
  ENGINE="docker"
else
  echo "error: podman or docker is required to build Rust release artifacts" >&2
  exit 1
fi

OUTPUT_TMP=$(mktemp -d)
trap 'rm -rf "$OUTPUT_TMP"' EXIT

# Keep the root CWD: cargo-generate-rpm resolves asset sources there first.
# Deb packaging stays inside Ubuntu because openSUSE hosts lack dpkg-shlibdeps;
# cargo-deb's depends="$auto" must be resolved in the Debian container.
"$ENGINE" build \
  --file packaging/Containerfile \
  --target "$FORMAT" \
  --output "type=local,dest=$OUTPUT_TMP" \
  "$REPO_ROOT"

shopt -s nullglob
TARBALLS=("$OUTPUT_TMP"/solstone-linux-*-linux-x86_64.tar.gz)
case "$FORMAT" in
  deb) PACKAGES=("$OUTPUT_TMP"/solstone-linux_*-1_amd64.deb) ;;
  rpm) PACKAGES=("$OUTPUT_TMP"/solstone-linux-*-1.x86_64.rpm) ;;
esac
shopt -u nullglob

if [[ ${#TARBALLS[@]} -ne 1 || ${#PACKAGES[@]} -ne 1 ]]; then
  echo "error: container output must contain exactly one baseline tarball and one $FORMAT package" >&2
  exit 1
fi

TARBALL_VERSION=${TARBALLS[0]##*/solstone-linux-}
TARBALL_VERSION=${TARBALL_VERSION%-linux-x86_64.tar.gz}
case "$FORMAT" in
  deb) EXPECTED_PACKAGE="solstone-linux_${TARBALL_VERSION}-1_amd64.deb" ;;
  rpm) EXPECTED_PACKAGE="solstone-linux-${TARBALL_VERSION}-1.x86_64.rpm" ;;
esac
if [[ "${PACKAGES[0]##*/}" != "$EXPECTED_PACKAGE" ]]; then
  echo "error: package and tarball versions do not match" >&2
  exit 1
fi

check_artifact() {
  local source="$1"
  local destination="dist/rust/${source##*/}"
  if [[ -e "$destination" ]] && ! cmp -s "$source" "$destination"; then
    echo "error: refusing to overwrite existing artifact with different bytes: $destination" >&2
    exit 1
  fi
}

mkdir -p dist/rust
ARTIFACTS=("${TARBALLS[0]}" "${PACKAGES[0]}")
for ARTIFACT in "${ARTIFACTS[@]}"; do
  check_artifact "$ARTIFACT"
done
for ARTIFACT in "${ARTIFACTS[@]}"; do
  DESTINATION="dist/rust/${ARTIFACT##*/}"
  if [[ -e "$DESTINATION" ]]; then
    echo "keeping byte-identical existing artifact: $DESTINATION"
  else
    install -m 0644 "$ARTIFACT" "$DESTINATION"
  fi
done
echo "built Rust $FORMAT artifacts for version $TARBALL_VERSION in dist/rust/"
