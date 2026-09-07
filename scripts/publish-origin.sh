#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

# Publishes an exact signed release candidate to the release origin,
# https://updates.solstone.app/solstone-linux/{lane}/{version}/{filename},
# writing the {lane}/latest pointer only after every artifact is published.
#
# The origin is where owners fetch bytes. This script never contacts GitHub and
# never requires gh: a GitHub outage cannot stop or delay an origin publish.

set -euo pipefail

umask 077
export LC_ALL=C

PRODUCT="solstone-linux"
BUCKET="${SOLSTONE_ORIGIN_BUCKET:-solstone-updates}"
ORIGIN_URL="https://updates.solstone.app"

die() {
    printf 'release origin publisher: %s\n' "$1" >&2
    exit 1
}

usage() {
    echo "usage: publish-origin.sh --lane <release|staging|dev> --release-dir <directory> [--dry-run]" >&2
    exit 2
}

lane=""
release_directory=""
dry_run=false
while (($# > 0)); do
    case "$1" in
        --lane)
            (($# >= 2)) || usage
            lane="$2"
            shift 2
            ;;
        --release-dir)
            (($# >= 2)) || usage
            release_directory="$2"
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        *)
            usage
            ;;
    esac
done
[[ -n "$lane" && -n "$release_directory" ]] || usage

# Lane vocabulary is the journal's: exactly release, staging, dev.
case "$lane" in
    release | staging | dev) ;;
    *) die "lane-invalid: $lane" ;;
esac

required_tools=(awk find git jq mkdir mktemp realpath rm sha256sum sort minisign)
$dry_run || required_tools+=(wrangler)
for tool in "${required_tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 ||
        die "required release tool is unavailable: $tool"
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
    die "current directory is not a Git worktree"
repo_root="$(realpath "$repo_root")"
public_key="$repo_root/packaging/keys/$PRODUCT-release.pub"
[[ -f "$public_key" && ! -L "$public_key" ]] ||
    die "release public key must be a regular file"

[[ -d "$release_directory" && ! -L "$release_directory" ]] ||
    die "release directory must be a real directory"
release_directory="$(realpath "$release_directory")" ||
    die "could not resolve the release directory"

mapfile -d '' -t manifests < <(
    find "$release_directory" -mindepth 1 -maxdepth 1 -type f \
        -name "$PRODUCT-*-linux-x86_64.rust-release-manifest.json" \
        -print0
)
((${#manifests[@]} == 1)) ||
    die "candidate-set-invalid: release directory must contain exactly one release manifest"
manifest="${manifests[0]}"

version="$(jq -er '.version' "$manifest")" ||
    die "candidate-set-invalid: release manifest version is unavailable"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    die "candidate-set-invalid: release version must be strict SemVer"

manifest_name="$PRODUCT-$version-linux-x86_64.rust-release-manifest.json"
[[ "${manifest##*/}" == "$manifest_name" ]] ||
    die "candidate-set-invalid: release manifest basename does not agree with its version"

# Exactly the names the producer emits today. No renames, ever.
expected_names=(
    "SHA256SUMS"
    "$PRODUCT-$version-1.x86_64.rpm"
    "$manifest_name"
    "$manifest_name.minisig"
    "$PRODUCT-$version-linux-x86_64.tar.gz"
    "${PRODUCT}_$version-1_amd64.deb"
)
mapfile -t expected_names < <(printf '%s\n' "${expected_names[@]}" | sort)

actual_names=()
while IFS= read -r -d '' path; do
    [[ -f "$path" && ! -L "$path" ]] ||
        die "candidate-set-invalid: release entries must be regular files"
    actual_names+=("${path##*/}")
done < <(find "$release_directory" -mindepth 1 -maxdepth 1 -print0 | sort -z)
((${#actual_names[@]} == ${#expected_names[@]})) ||
    die "candidate-set-invalid: release file set is incomplete or unlisted"
for index in "${!expected_names[@]}"; do
    [[ "${actual_names[$index]}" == "${expected_names[$index]}" ]] ||
        die "candidate-set-invalid: release file set is incomplete or unlisted"
done

# The always-on verification is the owner's verification: the exact set, the
# signature under the key this repository pins, and every declared digest.
(
    cd "$release_directory"
    sha256sum -c SHA256SUMS
) >/dev/null || die "digest-mismatch: release checksums do not validate"

minisign -V -q -p "$public_key" -m "$manifest" -x "$manifest.minisig" ||
    die "signature-invalid: release manifest signature did not verify"

jq -e \
    --arg version "$version" \
    '.schema_version == 1 and
     .product == "solstone-linux" and
     .version == $version and
     .source_dirty == false' \
    "$manifest" >/dev/null ||
    die "candidate-set-invalid: release manifest does not describe this candidate"

manifest_commit="$(jq -er '.source_commit' "$manifest")" ||
    die "candidate-set-invalid: release manifest source commit is unavailable"
[[ "$manifest_commit" =~ ^[0-9a-f]{40}$ ]] ||
    die "candidate-set-invalid: release manifest source commit must be lowercase 40-hex"

(
    cd "$release_directory"
    jq -r '.artifacts[] | "\(.sha256)  \(.path)"' "$manifest" | sha256sum -c -
) >/dev/null || die "digest-mismatch: an artifact differs from the signed manifest"

# The release lane additionally carries the full source binding and the
# repository's own release-model validator. The proof lanes deliberately do
# not, which is what lets a retained candidate be re-proved from main without
# re-cutting it.
if [[ "$lane" == "release" ]]; then
    [[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ]] ||
        die "source-unbound: source tree must be clean to publish to the release lane"
    [[ "$(git -C "$repo_root" rev-parse HEAD)" == "$manifest_commit" ]] ||
        die "source-unbound: HEAD must equal the release manifest source commit"
    (
        cd "$repo_root"
        CARGO_NET_OFFLINE=true cargo run --locked -p rust-release-manifest -- \
            validate --release-dir "$release_directory"
    ) || die "candidate-set-invalid: release-model validation failed"
fi

# Dot-separated numeric segments; non-numeric segments compare as strings.
# Same ordering the journal's publisher uses, so latest advances identically.
version_is_not_older() {
    local left="$1" right="$2"
    local -a left_parts right_parts
    IFS='.' read -r -a left_parts <<<"$left"
    IFS='.' read -r -a right_parts <<<"$right"
    local count=${#left_parts[@]}
    ((${#right_parts[@]} > count)) && count=${#right_parts[@]}
    local index l r
    for ((index = 0; index < count; index++)); do
        l="${left_parts[index]:-}"
        r="${right_parts[index]:-}"
        [[ "$l" == "$r" ]] && continue
        [[ -z "$l" ]] && return 1
        [[ -z "$r" ]] && return 0
        if [[ "$l" =~ ^[0-9]+$ && "$r" =~ ^[0-9]+$ ]]; then
            ((10#$l > 10#$r)) && return 0
            return 1
        fi
        [[ "$l" > "$r" ]] && return 0
        return 1
    done
    return 0
}

content_type_for() {
    case "$1" in
        *.tar.gz) echo "application/gzip" ;;
        *.deb) echo "application/vnd.debian.binary-package" ;;
        *.rpm) echo "application/x-rpm" ;;
        *.json) echo "application/json" ;;
        *.minisig | SHA256SUMS) echo "text/plain; charset=utf-8" ;;
        *) echo "application/octet-stream" ;;
    esac
}

stage_root="$(mktemp -d "${TMPDIR:-/tmp}/$PRODUCT-publish-origin.XXXXXX")"
cleanup() {
    rm -rf -- "$stage_root"
}
trap cleanup EXIT

# Returns 0 when the object is present (bytes land in $2), 1 when it is
# genuinely absent, and fails closed on every other outcome. An unreachable
# origin must never read as an empty one.
remote_get() {
    local key="$1" destination="$2" log status
    log="$stage_root/wrangler.log"
    set +e
    wrangler r2 object get "$BUCKET/$key" --remote --file "$destination" >"$log" 2>&1
    status=$?
    set -e
    if ((status == 0)); then
        return 0
    fi
    if grep -qF 'The specified key does not exist' "$log"; then
        rm -f "$destination"
        return 1
    fi
    cat "$log" >&2
    die "origin-unreachable: could not read $key"
}

remote_put() {
    local key="$1" file="$2" content_type="$3" cache_control="$4"
    wrangler r2 object put "$BUCKET/$key" \
        --file "$file" \
        --content-type "$content_type" \
        --cache-control "$cache_control" \
        --remote >/dev/null ||
        die "origin-unreachable: could not write $key"
}

checkpoint() {
    [[ "${SOLSTONE_ORIGIN_FAIL_AFTER:-}" == "$1" ]] || return 0
    die "injected-failure $1"
}

# The published key an owner fetches must be the key this repository pins, or
# the documented verify-first step authenticates against the wrong anchor.
# Nothing else places that object, so the publisher owns it: it restores the key
# when absent and refuses when a different one is already there.
if ! $dry_run; then
    published_key="$stage_root/published-minisign.pub"
    if remote_get "$PRODUCT/minisign.pub" "$published_key"; then
        cmp -s "$published_key" "$public_key" ||
            die "key-mismatch: the published minisign key differs from packaging/keys/$PRODUCT-release.pub"
    else
        remote_put "$PRODUCT/minisign.pub" "$public_key" \
            "text/plain; charset=utf-8" "no-cache"
        printf '  put      %s/minisign.pub (trust anchor was absent)\n' "$PRODUCT"
    fi
fi

if [[ "$lane" == "dev" ]]; then
    object_cache_control="no-cache"
else
    object_cache_control="public, max-age=31536000, immutable"
fi

printf 'publishing %s %s to the %s lane of %s\n' \
    "$PRODUCT" "$version" "$lane" "$ORIGIN_URL"

published=()
for name in "${expected_names[@]}"; do
    key="$PRODUCT/$lane/$version/$name"
    local_file="$release_directory/$name"
    if $dry_run; then
        printf '  would publish %s/%s\n' "$ORIGIN_URL" "$key"
        continue
    fi
    remote_file="$stage_root/remote-object"
    if remote_get "$key" "$remote_file"; then
        if cmp -s "$remote_file" "$local_file"; then
            printf '  present  %s\n' "$key"
            rm -f "$remote_file"
            checkpoint "object:$name"
            continue
        fi
        # release and staging versioned objects are immutable. No R2 bucket
        # lock rule covers these prefixes, so the store will not refuse for us
        # and wrangler overwrites silently. The refusal is the publisher's.
        [[ "$lane" == "dev" ]] ||
            die "object-immutable: $key already exists with different bytes"
        rm -f "$remote_file"
    fi
    remote_put "$key" "$local_file" "$(content_type_for "$name")" "$object_cache_control"
    printf '  put      %s\n' "$key"
    published+=("$key")
    checkpoint "object:$name"
done
checkpoint "objects"

latest_key="$PRODUCT/$lane/latest"
if $dry_run; then
    printf '  would advance %s/%s to version=%s\n' "$ORIGIN_URL" "$latest_key" "$version"
    exit 0
fi

latest_file="$stage_root/remote-latest"
advance=true
if remote_get "$latest_key" "$latest_file"; then
    existing_body="$(cat "$latest_file")"
    [[ "$existing_body" =~ ^version=[^[:space:]/]+$ ]] ||
        die "latest-invalid: $latest_key is not a single version= line"
    existing_version="${existing_body#version=}"
    if ! version_is_not_older "$version" "$existing_version"; then
        advance=false
    fi
fi

if $advance; then
    printf 'version=%s\n' "$version" >"$stage_root/latest"
    remote_put "$latest_key" "$stage_root/latest" "text/plain; charset=utf-8" "no-cache"
    printf '  put      %s (version=%s)\n' "$latest_key" "$version"
else
    printf '  held     %s (already at version=%s)\n' "$latest_key" "$existing_version"
fi

printf 'published %s %s to %s/%s/%s/%s/ (%d new objects)\n' \
    "$PRODUCT" "$version" "$ORIGIN_URL" "$PRODUCT" "$lane" "$version" "${#published[@]}"
