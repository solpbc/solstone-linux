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
            Value::String("../../target/deb-copyright".into()),
            Value::String("0".into())
        ]
    );
    let depends = deb["depends"].as_str().unwrap();
    assert!(
        !depends.contains("gtk"),
        "the window's GTK is only recommended"
    );
    assert!(deb["recommends"].as_str().unwrap().contains("libgtk-4-1"));

    let rpm = &member["package"]["metadata"]["generate-rpm"];
    assert_eq!(rpm["license"].as_str(), Some("AGPL-3.0-only"));
    assert_eq!(rpm["auto-req"].as_str(), Some("disabled"));
    assert!(rpm["requires"].get("gtk4").is_none());
    assert!(rpm["recommends"].get("gtk4").is_some());

    let flac = &member["dependencies"]["flac-bound"];
    assert_eq!(flac["default-features"].as_bool(), Some(false));
    assert_eq!(
        flac["features"].as_array().unwrap(),
        &[Value::String("libflac-noogg".into())]
    );
}

// the committed proof policy names artifacts from the current workspace
// version, for every architecture, so an ordinary CI run catches a stale
// policy before candidate build.
#[test]
fn release_proof_install_commands_track_workspace_version() {
    let policy = read_toml(&workspace_root().join("packaging/release-policy.toml"));
    let expected = [
        (
            "x86_64",
            "debian-amd64",
            format!("/input/solstone-linux_{VERSION}-1_amd64.deb"),
        ),
        (
            "x86_64",
            "rpm-x86_64",
            format!("/input/solstone-linux-{VERSION}-1.x86_64.rpm"),
        ),
        (
            "x86_64",
            "tar-x86_64",
            format!("/input/solstone-linux-{VERSION}-linux-x86_64.tar.gz"),
        ),
        (
            "aarch64",
            "debian-arm64",
            format!("/input/solstone-linux_{VERSION}-1_arm64.deb"),
        ),
        (
            "aarch64",
            "rpm-aarch64",
            format!("/input/solstone-linux-{VERSION}-1.aarch64.rpm"),
        ),
        (
            "aarch64",
            "tar-aarch64",
            format!("/input/solstone-linux-{VERSION}-linux-aarch64.tar.gz"),
        ),
    ];

    for (arch, platform, artifact) in expected {
        let command = policy[arch][platform]["install_command"]
            .as_array()
            .unwrap();
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

// The build stages these files from the native SDK. A source checkout has
// their reviewed inventories, and the container checks the actual file digests.
fn journal_assets(arch: &str) -> BTreeSet<(String, String, String)> {
    let pin = workspace_root().join(format!("journal-viewer/engine/{arch}.json"));
    let pin: serde_json::Value = serde_json::from_slice(&fs::read(pin).unwrap()).unwrap();
    let mut files = vec![
        ("usr/bin/solstone-linux-journal".to_owned(), "755"),
        (
            "usr/share/solstone-linux/apparmor/solstone-linux-journal".to_owned(),
            "644",
        ),
        ("opt/solstone-webkit/runtime.json".to_owned(), "644"),
        ("opt/solstone-webkit/bin/journal".to_owned(), "755"),
    ];
    for name in pin["files"].as_object().unwrap().keys() {
        let mode = if name.starts_with("libexec/") {
            "755"
        } else {
            "644"
        };
        files.push((format!("opt/solstone-webkit/{name}"), mode));
    }
    files
        .into_iter()
        .map(|(dest, mode)| {
            (
                format!("packaging/journal-root/{dest}"),
                dest,
                mode.to_owned(),
            )
        })
        .collect()
}

// Resolve each tool's source and destination conventions, including cargo-deb's
// directory destinations. Compare sources, installed filenames and file modes.
#[test]
fn package_assets_exist_and_match() {
    let (_, member) = metadata();
    let committed = committed_asset_set();
    assert_eq!(
        committed.len(),
        24,
        "desktop, docs, icons and activity helpers"
    );
    let journal = journal_assets("x86_64");
    assert_eq!(journal, journal_assets("aarch64"));
    let bins = ["solstone-linux", "solstone-linux-window"].map(|name| {
        (
            format!("target/release/{name}"),
            format!("usr/bin/{name}"),
            "755".to_owned(),
        )
    });
    let deb = deb_assets(&member);
    let rpm = rpm_assets(&member);
    let count = committed.len() + bins.len() + journal.len();
    assert_eq!(deb.len(), count);
    assert_eq!(rpm.len(), count);
    let deb = deb
        .into_iter()
        .map(|(source, dest, mode)| {
            let source = if source.starts_with("target/release/") {
                source
            } else {
                source
                    .strip_prefix("../../")
                    .expect("cargo-deb resolves sources from the crate directory")
                    .to_owned()
            };
            let dest = if dest.ends_with('/') {
                format!(
                    "{dest}{}",
                    Path::new(&source).file_name().unwrap().to_str().unwrap()
                )
            } else {
                dest
            };
            (source, dest, mode)
        })
        .collect::<BTreeSet<_>>();
    let rpm = rpm
        .into_iter()
        .map(|(source, dest, mode)| (source, dest.strip_prefix('/').unwrap().to_owned(), mode))
        .collect::<BTreeSet<_>>();
    assert_eq!(deb, rpm);
    assert_eq!(deb.len(), count, "duplicate package rows");
    let destinations = deb.iter().map(|(_, dest, _)| dest).collect::<BTreeSet<_>>();
    assert_eq!(destinations.len(), count, "duplicate install paths");
    for dest in destinations {
        assert!(!dest.ends_with(".service"));
    }
    let mut remaining = deb;
    for asset in journal.into_iter().chain(bins) {
        assert!(remaining.remove(&asset), "missing or mismatched {asset:?}");
    }
    let root = workspace_root();
    let paths = remaining
        .into_iter()
        .map(|(source, _, mode)| {
            assert_eq!(mode, "644");
            root.join(source).canonicalize().unwrap()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(paths, committed);
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
    // Two Rust binaries and cargo-deb's generated copyright accompany the
    // committed desktop files and the generated journal runtime.
    assert_eq!(
        payload_count,
        committed_asset_set().len() + journal_assets("x86_64").len() + 3
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
