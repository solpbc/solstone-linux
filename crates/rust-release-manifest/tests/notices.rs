// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![cfg(feature = "full-tests")]

use rust_release_manifest::deb_copyright;
use std::{fs, process::Command};

#[test]
fn deb_copyright_cli_writes_complete_licences_without_git() {
    let source = tempfile::tempdir().unwrap();
    fs::write(
        source.path().join("Cargo.toml"),
        include_str!("../../../Cargo.toml"),
    )
    .unwrap();
    fs::create_dir(source.path().join("target")).unwrap();
    assert!(!source.path().join(".git").exists());

    // Use the packaging command and relative output path, outside a checkout.
    let output = Command::new(env!("CARGO_BIN_EXE_rust-release-manifest"))
        .args([
            "notices",
            "deb-copyright",
            "--output",
            "target/deb-copyright",
        ])
        .current_dir(source.path())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CEILING_DIRECTORIES", source.path().parent().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read(source.path().join("target/deb-copyright")).unwrap(),
        deb_copyright(),
    );
}
