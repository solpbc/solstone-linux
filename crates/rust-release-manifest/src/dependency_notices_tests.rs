// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    RepoRoot,
    dependency_notices::{
        DependencyNoticesIndex, DependencyNoticesPaths, NOTICES_HEADER, PrebuiltScanTarget,
        check_dependency_notices, scan_non_test_prebuilts, verify_prebuilts_allowlist,
    },
};
use std::{collections::BTreeSet, fs};
use tempfile::tempdir;

#[test]
fn real_workspace_dependency_notices_pass_check() {
    let root = RepoRoot::resolve().unwrap();
    let paths = DependencyNoticesPaths::from_workspace_root(root.path());
    check_dependency_notices(&paths).unwrap();
}

#[test]
fn notices_file_starts_with_exact_required_header() {
    let root = RepoRoot::resolve().unwrap();
    let notices = fs::read_to_string(root.path().join("RUST_DEPENDENCY_NOTICES.txt")).unwrap();
    let expected_prefix = format!("{NOTICES_HEADER}\n");
    assert!(
        notices.starts_with(&expected_prefix),
        "notices file must start with exact header and an additional blank line"
    );
}

#[test]
fn mutated_cargo_lock_fails_as_lock_digest_mismatch() {
    let root = RepoRoot::resolve().unwrap();
    let temp = tempdir().unwrap();
    let mutated_lock = temp.path().join("Cargo.lock");

    let mut lock_bytes = fs::read(root.path().join("Cargo.lock")).unwrap();
    lock_bytes.push(b'\n');
    fs::write(&mutated_lock, lock_bytes).unwrap();

    let mut paths = DependencyNoticesPaths::from_workspace_root(root.path());
    paths.cargo_lock = mutated_lock;

    let error = check_dependency_notices(&paths).unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("Cargo.lock digest mismatch"),
        "expected Cargo.lock mismatch error, actual: {msg}"
    );
}

#[test]
fn mutated_notices_file_fails_as_notices_digest_mismatch() {
    let root = RepoRoot::resolve().unwrap();
    let temp = tempdir().unwrap();
    let mutated_notices = temp.path().join("RUST_DEPENDENCY_NOTICES.txt");

    let mut notices_bytes = fs::read(root.path().join("RUST_DEPENDENCY_NOTICES.txt")).unwrap();
    notices_bytes.push(b'\n');
    fs::write(&mutated_notices, notices_bytes).unwrap();

    let mut paths = DependencyNoticesPaths::from_workspace_root(root.path());
    paths.notices = mutated_notices;

    let error = check_dependency_notices(&paths).unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("RUST_DEPENDENCY_NOTICES.txt digest mismatch"),
        "expected notices digest mismatch error, actual: {msg}"
    );
}

#[test]
fn forged_index_member_fails_validation() {
    let root = RepoRoot::resolve().unwrap();
    let temp = tempdir().unwrap();
    let forged_index_path = temp.path().join("index.json");

    let original_index = fs::read_to_string(
        root.path()
            .join("licensing/rust-dependency-notices.index.json"),
    )
    .unwrap();
    let mut index_data: DependencyNoticesIndex = serde_json::from_str(&original_index).unwrap();

    // Mutate a package member's origin or sha256 to create a mismatch
    if let Some(pkg) = index_data.packages.first_mut()
        && let Some(member) = pkg.members.first_mut()
    {
        member.sha256 =
            "0000000000000000000000000000000000000000000000000000000000000000".to_owned();
    }

    fs::write(
        &forged_index_path,
        serde_json::to_string(&index_data).unwrap(),
    )
    .unwrap();

    let mut paths = DependencyNoticesPaths::from_workspace_root(root.path());
    paths.index = forged_index_path;

    let error = check_dependency_notices(&paths).unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("sha256 mismatch") || msg.contains("does not match"),
        "expected forged index member error, actual: {msg}"
    );
}

#[test]
fn forged_index_member_origin_fails_validation() {
    let root = RepoRoot::resolve().unwrap();
    let temp = tempdir().unwrap();
    let forged_index_path = temp.path().join("index.json");

    let original_index = fs::read_to_string(
        root.path()
            .join("licensing/rust-dependency-notices.index.json"),
    )
    .unwrap();
    let mut index_data: DependencyNoticesIndex = serde_json::from_str(&original_index).unwrap();

    // Flip a crate-archive member's origin to committed-override while keeping SHA-256 unchanged
    let mut modified = false;
    for pkg in &mut index_data.packages {
        for member in &mut pkg.members {
            if member.origin == "crate-archive" {
                member.origin = "committed-override".to_owned();
                modified = true;
                break;
            }
        }
        if modified {
            break;
        }
    }
    assert!(modified);

    fs::write(
        &forged_index_path,
        serde_json::to_string(&index_data).unwrap(),
    )
    .unwrap();

    let mut paths = DependencyNoticesPaths::from_workspace_root(root.path());
    paths.index = forged_index_path;

    let error = check_dependency_notices(&paths).unwrap_err();
    let msg = error.to_string();
    assert!(
        msg.contains("origin mismatch") || msg.contains("origin") || msg.contains("override"),
        "expected forged index member origin error, actual: {msg}"
    );
}

#[test]
fn prebuilt_scanner_handles_allowlist_variations() {
    let temp = tempdir().unwrap();
    let allowlist_path = temp.path().join("vendored-prebuilts.toml");

    let pkg_crate_dir = temp.path().join("sample-pkg-0.1.0");
    fs::create_dir_all(pkg_crate_dir.join("lib")).unwrap();
    fs::create_dir_all(pkg_crate_dir.join("tests/lib")).unwrap();
    fs::create_dir_all(pkg_crate_dir.join("test/lib")).unwrap();

    // Create a non-test prebuilt object
    fs::write(pkg_crate_dir.join("lib/sample.a"), b"archive").unwrap();
    // Create prebuilts under test and tests directories (which must be skipped)
    fs::write(pkg_crate_dir.join("tests/lib/test.a"), b"archive").unwrap();
    fs::write(pkg_crate_dir.join("test/lib/test.a"), b"archive").unwrap();

    let target = PrebuiltScanTarget {
        name: "sample-pkg".to_owned(),
        version: "0.1.0".to_owned(),
        crate_dir: pkg_crate_dir.clone(),
    };

    let discovered = scan_non_test_prebuilts(std::slice::from_ref(&target)).unwrap();
    assert_eq!(discovered.len(), 1);
    let mut expected_suffixes = BTreeSet::new();
    expected_suffixes.insert(".a".to_owned());
    assert_eq!(
        discovered.get(&("sample-pkg".to_owned(), "0.1.0".to_owned())),
        Some(&expected_suffixes)
    );

    // 1. Correct allowlist passes
    let valid_toml = r#"
schema = "solstone-linux.vendored-prebuilts.v1"

[[package]]
name = "sample-pkg"
version = "0.1.0"
suffixes = [".a"]
"#;
    fs::write(&allowlist_path, valid_toml).unwrap();
    verify_prebuilts_allowlist(&discovered, &allowlist_path).unwrap();

    // 2. Unexpected package in scan
    let missing_pkg_toml = r#"
schema = "solstone-linux.vendored-prebuilts.v1"

[[package]]
name = "other-pkg"
version = "0.1.0"
suffixes = [".a"]
"#;
    fs::write(&allowlist_path, missing_pkg_toml).unwrap();
    let err = verify_prebuilts_allowlist(&discovered, &allowlist_path).unwrap_err();
    assert!(
        err.to_string()
            .contains("unexpected vendored prebuilt package: sample-pkg 0.1.0")
    );

    // 3. Suffix mismatch
    let wrong_suffix_toml = r#"
schema = "solstone-linux.vendored-prebuilts.v1"

[[package]]
name = "sample-pkg"
version = "0.1.0"
suffixes = [".so"]
"#;
    fs::write(&allowlist_path, wrong_suffix_toml).unwrap();
    let err = verify_prebuilts_allowlist(&discovered, &allowlist_path).unwrap_err();
    assert!(
        err.to_string()
            .contains("vendored prebuilt suffix mismatch for sample-pkg 0.1.0")
    );

    // 4. Stale allowlist entry
    let extra_pkg_toml = r#"
schema = "solstone-linux.vendored-prebuilts.v1"

[[package]]
name = "sample-pkg"
version = "0.1.0"
suffixes = [".a"]

[[package]]
name = "ghost-pkg"
version = "1.0.0"
suffixes = [".so"]
"#;
    fs::write(&allowlist_path, extra_pkg_toml).unwrap();
    let err = verify_prebuilts_allowlist(&discovered, &allowlist_path).unwrap_err();
    assert!(
        err.to_string()
            .contains("stale vendored prebuilt allowlist entry: ghost-pkg 1.0.0")
    );

    // 5. Empty scan fails
    let empty_discovered = std::collections::BTreeMap::new();
    let err = verify_prebuilts_allowlist(&empty_discovered, &allowlist_path).unwrap_err();
    assert!(
        err.to_string()
            .contains("vendored prebuilt scan found no prebuilts")
    );
}
