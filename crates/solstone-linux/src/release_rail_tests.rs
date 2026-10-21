// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use toml::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn workspace_root() -> PathBuf {
    manifest_dir().join("../..").canonicalize().unwrap()
}

pub(crate) fn read_toml(path: &Path) -> Value {
    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn metadata() -> (Value, Value) {
    let root = workspace_root();
    (
        read_toml(&root.join("Cargo.toml")),
        read_toml(&manifest_dir().join("Cargo.toml")),
    )
}

fn deb_assets(member: &Value) -> Vec<(String, String, String)> {
    member["package"]["metadata"]["deb"]["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let row = row.as_array().unwrap();
            (
                row[0].as_str().unwrap().to_owned(),
                row[1].as_str().unwrap().to_owned(),
                row[2].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn rpm_assets(member: &Value) -> Vec<(String, String, String)> {
    member["package"]["metadata"]["generate-rpm"]["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["source"].as_str().unwrap().to_owned(),
                row["dest"].as_str().unwrap().to_owned(),
                row["mode"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn committed_asset_set() -> BTreeSet<PathBuf> {
    let root = workspace_root();
    let mut paths = BTreeSet::from([
        root.join("LICENSE").canonicalize().unwrap(),
        root.join("crates/solstone-linux-window/assets/Comfortaa-OFL.txt")
            .canonicalize()
            .unwrap(),
        root.join("packaging/INSTALL-NOTES").canonicalize().unwrap(),
        root.join("packaging/org.solpbc.solstone.Window.desktop")
            .canonicalize()
            .unwrap(),
        root.join("RUST_DEPENDENCY_NOTICES.txt")
            .canonicalize()
            .unwrap(),
        root.join("packaging/activity-helpers/solstone-activity@solpbc.org/metadata.json")
            .canonicalize()
            .unwrap(),
        root.join("packaging/activity-helpers/solstone-activity@solpbc.org/extension.js")
            .canonicalize()
            .unwrap(),
        root.join("packaging/activity-helpers/solstone-activity-kwin/metadata.json")
            .canonicalize()
            .unwrap(),
        root.join("packaging/activity-helpers/solstone-activity-kwin/contents/code/main.js")
            .canonicalize()
            .unwrap(),
    ]);
    collect_files(&root.join("contrib/icons/hicolor"), &mut paths);
    paths
}

fn collect_files(path: &Path, paths: &mut BTreeSet<PathBuf>) {
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            collect_files(&entry.path(), paths);
        } else {
            paths.insert(entry.path().canonicalize().unwrap());
        }
    }
}

// release metadata lives on the member crate and both package formats
// resolve to AGPL-3.0-only using their real schemas.
#[test]
fn package_metadata_and_resolved_licenses() {
    let (root, member) = metadata();
    assert!(root.get("package").is_none());
    assert!(root.get("metadata").is_none());
    assert_eq!(
        member["package"]["version"]["workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(
        member["package"]["license"]["workspace"].as_bool(),
        Some(true)
    );
    assert_eq!(
        root["workspace"]["package"]["license"].as_str(),
        Some("AGPL-3.0-only")
    );

    let deb = &member["package"]["metadata"]["deb"];
    assert!(deb.get("license").is_none());
    assert_eq!(deb["copyright"].as_str(), Some("2026 sol pbc"));
    assert_eq!(
        deb["license-file"].as_array().unwrap(),
        &[
            Value::String("../../LICENSE".into()),
            Value::String("0".into())
        ]
    );
    assert!(deb["depends"].as_str().unwrap().contains("$auto"));

    let rpm = &member["package"]["metadata"]["generate-rpm"];
    assert_eq!(rpm["license"].as_str(), Some("AGPL-3.0-only"));
    assert_eq!(rpm["auto-req"].as_str(), Some("auto"));

    let flac = &member["dependencies"]["flac-bound"];
    assert_eq!(flac["default-features"].as_bool(), Some(false));
    assert_eq!(
        flac["features"].as_array().unwrap(),
        &[Value::String("libflac-noogg".into())]
    );
}

// the committed proof policy names artifacts from the current workspace
// version so an ordinary CI run catches a stale policy before candidate build.
#[test]
fn release_proof_install_commands_track_workspace_version() {
    let policy = read_toml(&workspace_root().join("packaging/release-policy.toml"));
    let expected = [
        (
            "debian-amd64",
            format!("/input/solstone-linux_{VERSION}-1_amd64.deb"),
        ),
        (
            "rpm-x86_64",
            format!("/input/solstone-linux-{VERSION}-1.x86_64.rpm"),
        ),
        (
            "tar-x86_64",
            format!("/input/solstone-linux-{VERSION}-linux-x86_64.tar.gz"),
        ),
    ];

    for (platform, artifact) in expected {
        let command = policy[platform]["install_command"].as_array().unwrap();
        assert_eq!(
            command.last().and_then(Value::as_str),
            Some(artifact.as_str())
        );
    }
}

// both supported container engines share one ignore policy that excludes
// host build products without hiding the canonical icon sources.
#[test]
fn container_context_excludes_host_outputs() {
    let root = workspace_root();
    assert_eq!(
        fs::read_link(root.join(".dockerignore")).unwrap(),
        PathBuf::from(".containerignore")
    );
    let ignore = fs::read_to_string(root.join(".containerignore")).unwrap();
    for excluded in ["target/", "dist/", ".venv/", "**/__pycache__/"] {
        assert!(ignore.lines().any(|line| line == excluded));
    }
    assert!(!ignore.lines().any(|line| line.contains("contrib")));
}

// each tool's asset dialect resolves to the same committed files plus the
// workspace-aware release binary, and every committed source exists.
#[test]
fn package_assets_exist_and_match() {
    let (_, member) = metadata();
    let expected = committed_asset_set();
    assert_eq!(
        expected.len(),
        24,
        "LICENSE + Comfortaa-OFL.txt + INSTALL-NOTES + RUST_DEPENDENCY_NOTICES.txt + launcher + 15 icons + 4 activity helpers"
    );

    let deb = deb_assets(&member);
    let rpm = rpm_assets(&member);
    assert_eq!(deb.len(), 26);
    assert_eq!(rpm.len(), 26);
    assert_eq!(deb[0].0, "target/release/solstone-linux");
    assert_eq!(rpm[0].0, "target/release/solstone-linux");
    assert_eq!(deb[0].2, "755");
    assert_eq!(rpm[0].2, "755");

    let deb_paths = deb
        .iter()
        .skip(2)
        .map(|(source, _, _)| manifest_dir().join(source).canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    let rpm_paths = rpm
        .iter()
        .skip(2)
        .map(|(source, _, _)| workspace_root().join(source).canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(deb_paths, expected);
    assert_eq!(rpm_paths, expected);

    for (_, destination, mode) in deb.iter().chain(rpm.iter()) {
        assert!(!destination.ends_with(".service"));
        assert!(mode == "644" || mode == "755");
    }
    assert!(
        member["package"]["metadata"]["deb"]
            .get("systemd-units")
            .is_none()
    );
}

// Debian sealing must hash the release binary, every declared asset, and
// cargo-deb's generated copyright file. Keep the Containerfile bound coupled
// to the package manifest so an asset addition cannot strand the candidate.
#[test]
fn debian_sealing_enumerates_every_installed_payload() {
    let (_, member) = metadata();
    let payload_count = deb_assets(&member).len() + 1;
    assert_eq!(
        payload_count, 27,
        "26 declared assets + generated copyright"
    );

    let containerfile =
        fs::read_to_string(workspace_root().join("packaging/Containerfile")).unwrap();
    assert!(containerfile.contains(&format!("test \"${{md5_count}}\" -eq {payload_count}")));
    assert!(containerfile.contains("Debian md5 inventory mismatch"));
}

pub(crate) fn command_path(name: &str) -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}
