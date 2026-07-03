#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Publish solstone-linux to PyPI (or TestPyPI with --test).
# Builds sdist + py3-none-any wheel, uploads with twine, tags the commit,
# and creates a GitHub release with the artifacts attached.
#
# Required env: PYPI_TOKEN (or TESTPYPI_TOKEN with --test).
# Optional env: RELEASE_DRY_RUN=1 — runs build + twine check, echoes the
#   upload/tag/push/release-create commands instead of executing them.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release.sh [--test]

Options:
  --test       Publish to TestPyPI.
  -h, --help   Show this help.
EOF
}

TARGET="PyPI"
TOKEN_VAR="PYPI_TOKEN"
REPOSITORY_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --test)
      TARGET="TestPyPI"
      TOKEN_VAR="TESTPYPI_TOKEN"
      REPOSITORY_ARGS=(--repository-url https://test.pypi.org/legacy/)
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

if [[ -z "${!TOKEN_VAR:-}" ]]; then
  echo "error: $TOKEN_VAR not set (required for $TARGET upload)" >&2
  exit 1
fi
TOKEN="${!TOKEN_VAR}"

if ! git diff --quiet HEAD || [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree dirty; commit or stash changes before releasing" >&2
  exit 1
fi

rm -rf dist/
uv build

shopt -s nullglob
SDISTS=(dist/solstone_linux-*.tar.gz)
shopt -u nullglob

if [[ ${#SDISTS[@]} -ne 1 ]]; then
  echo "error: expected exactly one solstone_linux sdist in dist/" >&2
  exit 1
fi

SDIST_NAME=$(basename "${SDISTS[0]}")
VERSION="${SDIST_NAME#solstone_linux-}"
VERSION="${VERSION%.tar.gz}"

INIT_VERSION=$(grep -E '^__version__ = "[^"]+"' src/solstone_linux/__init__.py | sed 's/^__version__ = "\([^"]*\)".*/\1/' || true)
if [[ -z "$INIT_VERSION" ]]; then
  echo "error: could not read __version__ from src/solstone_linux/__init__.py" >&2
  exit 1
fi
if [[ "$INIT_VERSION" != "$VERSION" ]]; then
  echo "error: version mismatch: sdist version ${VERSION}, src/solstone_linux/__init__.py __version__ ${INIT_VERSION}" >&2
  exit 1
fi

uvx twine check dist/*

# Pre-flight: verify the CHANGELOG block exists before publishing.
bash scripts/extract_changelog.sh "$VERSION" >/dev/null

if [[ -n "${RELEASE_DRY_RUN:-}" ]]; then
  RUN=(echo "[dry-run]")
else
  RUN=()
fi

TWINE_USERNAME=__token__ TWINE_PASSWORD="$TOKEN" \
  "${RUN[@]}" uvx twine upload "${REPOSITORY_ARGS[@]}" dist/*

# Tag + GitHub release only for production. A TestPyPI dry-run should not leave
# a git tag or a public release behind.
if [[ "$TARGET" != "PyPI" ]]; then
  echo "skipping git tag + GitHub release (TestPyPI run)"
  exit 0
fi

TAG="v${VERSION}"
"${RUN[@]}" git tag -a "$TAG" -m "solstone-linux ${VERSION}"
if ! "${RUN[@]}" git push origin "$TAG"; then
  echo "error: git push origin ${TAG} failed; the tag was created locally but not pushed." >&2
  echo "       ${TARGET} is published and immutable. Resolve the push and create the GitHub release manually:" >&2
  echo "       gh release create ${TAG} dist/solstone_linux-${VERSION}.tar.gz dist/solstone_linux-${VERSION}-py3-none-any.whl --title 'solstone-linux ${VERSION}' --notes-file <notes>" >&2
  exit 1
fi

NOTES_FILE=$(mktemp)
trap 'rm -f "$NOTES_FILE"' EXIT
scripts/extract_changelog.sh "$VERSION" > "$NOTES_FILE"

if ! "${RUN[@]}" gh release create "$TAG" \
    "dist/solstone_linux-${VERSION}.tar.gz" \
    "dist/solstone_linux-${VERSION}-py3-none-any.whl" \
    --title "solstone-linux ${VERSION}" \
    --notes-file "$NOTES_FILE"; then
  echo "error: gh release create failed." >&2
  echo "       ${TARGET} is published and immutable; the git tag ${TAG} is pushed." >&2
  echo "       Re-run manually:" >&2
  echo "       gh release create ${TAG} dist/solstone_linux-${VERSION}.tar.gz dist/solstone_linux-${VERSION}-py3-none-any.whl --title 'solstone-linux ${VERSION}' --notes-file <notes>" >&2
  exit 1
fi

echo "published solstone-linux ${VERSION} to ${TARGET}"
