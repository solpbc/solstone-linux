#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -euo pipefail

umask 077
export LC_ALL=C

die() {
    printf 'native release publisher: %s\n' "$1" >&2
    exit 1
}

if (($# != 1)); then
    echo "usage: publish-release.sh <release-directory>" >&2
    exit 2
fi

release_directory="$1"

required_tools=(awk find gh git grep install jq mkdir mktemp realpath rm sha256sum sort)
for tool in "${required_tools[@]}"; do
    command -v "$tool" >/dev/null 2>&1 ||
        die "required release tool is unavailable: $tool"
done

repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" ||
    die "current directory is not a Git worktree"
repo_root="$(realpath "$repo_root")"
[[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ]] ||
    die "source tree must be clean"
source_commit="$(git -C "$repo_root" rev-parse HEAD)"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] ||
    die "source commit must be lowercase 40-hex"

[[ -d "$release_directory" && ! -L "$release_directory" ]] ||
    die "release directory must be a real directory"
release_directory="$(realpath "$release_directory")" ||
    die "could not resolve the release directory"
[[ "$release_directory" == "$repo_root/dist/rust" ]] ||
    die "release directory must be the retained dist/rust candidate"

mapfile -d '' -t manifests < <(
    find "$release_directory" -mindepth 1 -maxdepth 1 -type f \
        -name 'solstone-linux-*-linux-x86_64.rust-release-manifest.json' \
        -print0
)
((${#manifests[@]} == 1)) ||
    die "release directory must contain exactly one release manifest"
manifest="${manifests[0]}"

version="$(jq -er '.version' "$manifest")" ||
    die "release manifest version is unavailable"
manifest_commit="$(jq -er '.source_commit' "$manifest")" ||
    die "release manifest source commit is unavailable"
jq -e \
    --arg version "$version" \
    --arg commit "$source_commit" \
    '.schema_version == 1 and
     .product == "solstone-linux" and
     .version == $version and
     .source_commit == $commit and
     .source_dirty == false' \
    "$manifest" >/dev/null ||
    die "release manifest does not bind the clean source commit"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    die "release version must be strict SemVer"
[[ "$manifest_commit" == "$source_commit" ]] ||
    die "release manifest source commit must equal HEAD"

manifest_name="solstone-linux-$version-linux-x86_64.rust-release-manifest.json"
[[ "${manifest##*/}" == "$manifest_name" ]] ||
    die "release manifest basename does not agree with its version"

metadata="$(
    CARGO_NET_OFFLINE=true cargo metadata --locked --offline --no-deps --format-version 1
)" || die "locked package metadata is unavailable"
package_version="$(
    jq -er '
        [.packages[] | select(.name == "solstone-linux") | .version]
        | if length == 1 then .[0] else error("package count") end
    ' <<<"$metadata"
)" || die "shipping package version is ambiguous"
[[ "$package_version" == "$version" ]] ||
    die "shipping package version does not agree with the candidate"

tag="v$version"
title="solstone-linux $version"
notes="$(bash "$repo_root/scripts/extract_changelog.sh" "$version" "$repo_root/CHANGELOG.md")" ||
    die "could not extract exact release notes"
[[ "$notes" == "## [$version] - "* ]] ||
    die "release notes do not start with the expected version heading"

expected_names=(
    "SHA256SUMS"
    "solstone-linux-$version-1.x86_64.rpm"
    "$manifest_name"
    "solstone-linux-$version-linux-x86_64.tar.gz"
    "solstone-linux_$version-1_amd64.deb"
)
mapfile -t expected_names < <(printf '%s\n' "${expected_names[@]}" | sort)

assert_exact_files() {
    local root="$1"
    local -a actual=()
    local path
    while IFS= read -r -d '' path; do
        [[ -f "$path" && ! -L "$path" ]] ||
            die "release entries must be regular files"
        actual+=("${path##*/}")
    done < <(find "$root" -mindepth 1 -maxdepth 1 -print0 | sort -z)
    ((${#actual[@]} == ${#expected_names[@]})) ||
        die "release file set is incomplete or unlisted"
    local index
    for index in "${!expected_names[@]}"; do
        [[ "${actual[$index]}" == "${expected_names[$index]}" ]] ||
            die "release file set is incomplete or unlisted"
    done
}

assert_exact_files "$release_directory"
(
    cd "$release_directory"
    sha256sum -c SHA256SUMS
) || die "release checksums do not validate"
(
    cd "$repo_root"
    CARGO_NET_OFFLINE=true cargo run --locked -p rust-release-manifest -- \
        candidate recover --version "$version"
) || die "retained candidate validation failed"
[[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ]] ||
    die "candidate validation changed the source tree"

stage_root="$(mktemp -d "$repo_root/dist/.solstone-linux-publish.XXXXXX")"
cleanup() {
    rm -rf -- "$stage_root"
}
trap cleanup EXIT
downloads="$stage_root/downloads"
publishable="$stage_root/publishable"
mkdir -m 0700 "$downloads" "$publishable"
for name in "${expected_names[@]}"; do
    install -m 0644 "$release_directory/$name" "$publishable/$name"
done
assert_exact_files "$publishable"
(
    cd "$publishable"
    sha256sum -c SHA256SUMS
) || die "staged release checksums do not validate"
(
    cd "$repo_root"
    CARGO_NET_OFFLINE=true cargo run --locked -p rust-release-manifest -- \
        validate --release-dir "$publishable"
) || die "staged release validation failed"

repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)" ||
    die "could not resolve the GitHub repository"
[[ "$repo" == "solpbc/solstone-linux" ]] ||
    die "GitHub repository identity does not match solpbc/solstone-linux"
[[ "$(git -C "$repo_root" remote get-url origin)" == \
    "https://github.com/solpbc/solstone-linux.git" ]] ||
    die "origin does not match the release download repository"

local_tag_present=false
if git -C "$repo_root" show-ref --verify --quiet "refs/tags/$tag"; then
    local_tag_present=true
    [[ "$(git -C "$repo_root" cat-file -t "refs/tags/$tag")" == "tag" ]] ||
        die "existing local release tag is not annotated"
    [[ "$(git -C "$repo_root" rev-parse "$tag^{commit}")" == "$source_commit" ]] ||
        die "existing local release tag does not peel to HEAD"
else
    tag_status="$?"
    [[ "$tag_status" == "1" ]] ||
        die "could not inspect the local release tag"
fi

inspect_remote_tag() {
    local require_present="$1"
    local remote_refs remote_ref_count remote_tag_type remote_tag_object remote_tag
    remote_refs="$(gh api "repos/$repo/git/matching-refs/tags/$tag")" ||
        die "could not inspect the remote release tag"
    remote_ref_count="$(
        jq --arg ref "refs/tags/$tag" '[.[] | select(.ref == $ref)] | length' \
            <<<"$remote_refs"
    )"
    [[ "$remote_ref_count" == "0" || "$remote_ref_count" == "1" ]] ||
        die "remote release tag state is ambiguous"
    remote_tag_present=false
    if [[ "$remote_ref_count" == "1" ]]; then
        remote_tag_present=true
        remote_tag_type="$(
            jq -r --arg ref "refs/tags/$tag" \
                '.[] | select(.ref == $ref) | .object.type' <<<"$remote_refs"
        )"
        remote_tag_object="$(
            jq -r --arg ref "refs/tags/$tag" \
                '.[] | select(.ref == $ref) | .object.sha' <<<"$remote_refs"
        )"
        [[ "$remote_tag_type" == "tag" && "$remote_tag_object" =~ ^[0-9a-f]{40}$ ]] ||
            die "remote release tag is not annotated"
        remote_tag="$(gh api "repos/$repo/git/tags/$remote_tag_object")" ||
            die "could not peel the remote release tag"
        [[ "$(jq -r '.object.type' <<<"$remote_tag")" == "commit" &&
            "$(jq -r '.object.sha' <<<"$remote_tag")" == "$source_commit" ]] ||
            die "remote release tag does not peel to the exact source commit"
    elif [[ "$require_present" == "true" ]]; then
        die "remote release tag disappeared before publication"
    fi
}

read_release() {
    local pages
    pages="$(
        gh api --paginate "repos/$repo/releases?per_page=100" --jq '.[]'
    )" ||
        die "could not inspect releases"
    jq -s \
        --arg tag "$tag" \
        --arg title "$title" \
        '[.[] | select(.tag_name == $tag or .name == $title)]' \
        <<<"$pages"
}

verify_release_metadata() {
    local release_json="$1"
    [[ "$(jq 'length' <<<"$release_json")" == "1" ]] ||
        die "release state is ambiguous"
    [[ "$(jq -r '.[0].tag_name' <<<"$release_json")" == "$tag" &&
        "$(jq -r '.[0].target_commitish' <<<"$release_json")" == "$source_commit" &&
        "$(jq -r '.[0].name' <<<"$release_json")" == "$title" &&
        "$(jq -r '.[0].body' <<<"$release_json")" == "$notes" ]] ||
        die "release metadata differs from the exact candidate"
}

verify_remote_assets() {
    local release_json="$1"
    local require_complete="$2"
    local -a remote_names=()
    local -A seen_names=()
    local name asset_count asset_id download local_digest remote_digest
    mapfile -t remote_names < <(jq -r '.[0].assets[].name' <<<"$release_json" | sort)
    for name in "${remote_names[@]}"; do
        [[ -z "${seen_names[$name]:-}" ]] ||
            die "release contains duplicate asset names"
        seen_names["$name"]=1
        if ! printf '%s\n' "${expected_names[@]}" | grep -Fxq "$name"; then
            die "release contains an unlisted asset"
        fi
        asset_count="$(
            jq --arg name "$name" '[.[0].assets[] | select(.name == $name)] | length' \
                <<<"$release_json"
        )"
        [[ "$asset_count" == "1" ]] ||
            die "release asset state is ambiguous"
        asset_id="$(
            jq -r --arg name "$name" \
                '.[0].assets[] | select(.name == $name) | .id' <<<"$release_json"
        )"
        [[ "$asset_id" =~ ^[1-9][0-9]*$ ]] ||
            die "release asset identifier is invalid"
        download="$downloads/$name"
        gh api "repos/$repo/releases/assets/$asset_id" \
            -H "Accept: application/octet-stream" >"$download" ||
            die "could not download an existing release asset"
        local_digest="$(sha256sum "$publishable/$name" | awk '{print $1}')"
        remote_digest="$(sha256sum "$download" | awk '{print $1}')"
        [[ "$local_digest" == "$remote_digest" ]] ||
            die "release asset bytes differ from the exact candidate"
    done
    if [[ "$require_complete" == "true" ]]; then
        ((${#remote_names[@]} == ${#expected_names[@]})) ||
            die "published release asset set is incomplete"
        local index
        for index in "${!expected_names[@]}"; do
            [[ "${remote_names[$index]}" == "${expected_names[$index]}" ]] ||
                die "published release asset set is incomplete"
        done
    fi
}

inspect_remote_tag false
release_json="$(read_release)"
release_count="$(jq 'length' <<<"$release_json")"
[[ "$release_count" == "0" || "$release_count" == "1" ]] ||
    die "release state is ambiguous"

release_present=false
release_id=""
release_draft=""
if [[ "$release_count" == "1" ]]; then
    release_present=true
    verify_release_metadata "$release_json"
    release_id="$(jq -r '.[0].id' <<<"$release_json")"
    [[ "$release_id" =~ ^[1-9][0-9]*$ ]] ||
        die "release identifier is invalid"
    release_draft="$(jq -r '.[0].draft' <<<"$release_json")"
    [[ "$release_draft" == "true" || "$release_draft" == "false" ]] ||
        die "release draft state is invalid"
    verify_remote_assets \
        "$release_json" \
        "$([[ "$release_draft" == "false" ]] && echo true || echo false)"
fi
if $release_present && ! $remote_tag_present; then
    die "release exists without the exact annotated remote tag"
fi
if $release_present && [[ "$release_draft" == "false" ]]; then
    printf 'release %s is already published and exact\n' "$tag"
    exit 0
fi

if ! $remote_tag_present; then
    if ! $local_tag_present; then
        git -C "$repo_root" tag -a "$tag" "$source_commit" -m "$title" ||
            die "could not create the exact annotated local tag"
    fi
    git -C "$repo_root" push origin "refs/tags/$tag:refs/tags/$tag" ||
        die "could not push the exact annotated release tag"
    inspect_remote_tag true
fi

if ! $release_present; then
    created_release="$(
        gh api -X POST "repos/$repo/releases" \
            -f "tag_name=$tag" \
            -f "target_commitish=$source_commit" \
            -f "name=$title" \
            -f "body=$notes" \
            -F draft=true \
            -F prerelease=false
    )" || die "could not create the exact draft release"
    release_id="$(jq -r '.id' <<<"$created_release")"
    [[ "$release_id" =~ ^[1-9][0-9]*$ ]] ||
        die "created draft release identifier is invalid"
    release_json="$(jq -n --argjson release "$created_release" '[$release]')"
fi

for name in "${expected_names[@]}"; do
    existing_count="$(
        jq --arg name "$name" '[.[0].assets[] | select(.name == $name)] | length' \
            <<<"$release_json"
    )"
    if [[ "$existing_count" == "0" ]]; then
        gh release upload "$tag" "$publishable/$name" --repo "$repo" ||
            die "could not upload release asset"
    elif [[ "$existing_count" != "1" ]]; then
        die "draft release asset state is ambiguous"
    fi
done

release_json="$(read_release)"
verify_release_metadata "$release_json"
[[ "$(jq -r '.[0].id' <<<"$release_json")" == "$release_id" &&
    "$(jq -r '.[0].draft' <<<"$release_json")" == "true" ]] ||
    die "completed draft metadata or state changed"
verify_remote_assets "$release_json" true
inspect_remote_tag true

prepublish_release="$(gh api "repos/$repo/releases/$release_id")" ||
    die "could not perform the final draft recheck"
prepublish_json="$(jq -n --argjson release "$prepublish_release" '[$release]')"
verify_release_metadata "$prepublish_json"
[[ "$(jq -r '.[0].draft' <<<"$prepublish_json")" == "true" ]] ||
    die "draft state changed immediately before publication"
verify_remote_assets "$prepublish_json" true

gh api -X PATCH "repos/$repo/releases/$release_id" -F draft=false >/dev/null ||
    die "could not publish the verified draft"
published="$(gh api "repos/$repo/releases/$release_id")" ||
    die "could not verify the published release"
published_json="$(jq -n --argjson release "$published" '[$release]')"
verify_release_metadata "$published_json"
[[ "$(jq -r '.[0].draft' <<<"$published_json")" == "false" ]] ||
    die "published release is still a draft"
verify_remote_assets "$published_json" true

printf 'published exact native release %s: %s\n' \
    "$tag" "$(jq -r '.[0].html_url' <<<"$published_json")"
