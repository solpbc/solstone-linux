#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -euo pipefail

if (($# != 2)); then
    echo "usage: extract_changelog.sh <version> <changelog>" >&2
    exit 2
fi

version="$1"
changelog="$2"
[[ -r "$changelog" ]] || {
    echo "changelog is not readable: $changelog" >&2
    exit 1
}

output="$(
    awk -v version="$version" '
        $0 ~ "^## \\[" version "\\]" { seen = 1 }
        seen && /^## \[/ && $0 !~ "^## \\[" version "\\]" { exit }
        seen { print }
    ' "$changelog"
)"
[[ -n "$output" ]] || {
    echo "no changelog block found for version $version" >&2
    exit 1
}
printf '%s\n' "$output"
