#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Build one non-candidate package lane for drift inspection.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-release.sh <deb|rpm> [x86_64]

Build one package lane into the fixed dist/rust-drift/ directory. This helper
produces drift evidence only; it cannot create or promote a release candidate.
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
    echo "error: package format mismatch: expected deb or rpm, actual '$FORMAT'" >&2
    echo "repair: scripts/build-release.sh <deb|rpm> [x86_64]" >&2
    exit 2
    ;;
esac
if [[ "$ARCH" != "x86_64" ]]; then
  echo "error: package architecture mismatch: expected x86_64, actual '$ARCH'" >&2
  echo "repair: run this drift helper on x86_64" >&2
  exit 2
fi

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "error: repository root mismatch: expected solstone-linux Git checkout, actual unavailable" >&2
  echo "repair: run from the solstone-linux checkout" >&2
  exit 1
}
cd "$REPO_ROOT"
if ! git diff --quiet HEAD || [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree mismatch: expected clean, actual dirty" >&2
  echo "repair: commit or restore changes before building drift evidence" >&2
  exit 1
fi

if command -v podman >/dev/null 2>&1; then
  ENGINE=(podman build --pull=never --network=none)
elif command -v docker >/dev/null 2>&1; then
  docker buildx version >/dev/null 2>&1 || {
    echo "error: Docker build capability mismatch: expected buildx, actual unavailable" >&2
    echo "repair: provision Docker buildx before building drift evidence" >&2
    exit 1
  }
  ENGINE=(docker buildx build --pull=false --network=none)
else
  echo "error: container engine mismatch: expected podman or docker, actual unavailable" >&2
  echo "repair: provision a supported local container engine" >&2
  exit 1
fi

policy_value() {
  sed -n "s/^$1 = \"\([^\"]*\)\"/\1/p" packaging/release-policy.toml
}
UBUNTU_TOOL_BASE=$(policy_value build_ubuntu)
FEDORA_TOOL_BASE=$(policy_value build_fedora)
if [[ -z "$UBUNTU_TOOL_BASE" || -z "$FEDORA_TOOL_BASE" ]]; then
  echo "error: release image policy mismatch: expected committed build digests, actual missing" >&2
  echo "repair: restore packaging/release-policy.toml from the release commit" >&2
  exit 1
fi

SOURCE_COMMIT=$(git rev-parse HEAD)
SOURCE_ARCHIVE_SHA256=$(git archive --format=tar HEAD | sha256sum | cut -d' ' -f1)
CARGO_LOCK_SHA256=$(sha256sum Cargo.lock | cut -d' ' -f1)
RELEASE_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
INVOCATION_ID=${SOURCE_COMMIT:0:32}
OUTPUT_TMP=$(mktemp -d)
trap 'rm -rf "$OUTPUT_TMP"' EXIT

"${ENGINE[@]}" \
  --file packaging/Containerfile \
  --target "$FORMAT" \
  --output "type=local,dest=$OUTPUT_TMP" \
  --build-arg "UBUNTU_TOOL_BASE=$UBUNTU_TOOL_BASE" \
  --build-arg "FEDORA_TOOL_BASE=$FEDORA_TOOL_BASE" \
  --build-arg "INVOCATION_ID=$INVOCATION_ID" \
  --build-arg "SOURCE_COMMIT=$SOURCE_COMMIT" \
  --build-arg "SOURCE_ARCHIVE_SHA256=$SOURCE_ARCHIVE_SHA256" \
  --build-arg "CARGO_LOCK_SHA256=$CARGO_LOCK_SHA256" \
  --build-arg "RELEASE_VERSION=$RELEASE_VERSION" \
  "$REPO_ROOT"

OUTPUT_DIR=dist/rust-drift
mkdir -p "$OUTPUT_DIR"
shopt -s nullglob
OUTPUTS=("$OUTPUT_TMP"/* "$OUTPUT_TMP"/.[!.]*)
shopt -u nullglob
if [[ ${#OUTPUTS[@]} -ne 3 ]]; then
  echo "error: drift output inventory mismatch: expected two artifacts and lane evidence, actual ${#OUTPUTS[@]}" >&2
  echo "repair: inspect the selected local build image and packaging/Containerfile" >&2
  exit 1
fi
for SOURCE in "${OUTPUTS[@]}"; do
  install -m 0644 "$SOURCE" "$OUTPUT_DIR/${SOURCE##*/}"
done
echo "built non-candidate $FORMAT drift evidence in $OUTPUT_DIR/"
echo "candidate status: unavailable; use make release-candidate for the atomic candidate transaction"
