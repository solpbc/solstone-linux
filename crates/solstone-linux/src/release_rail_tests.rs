// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        root.join("packaging/INSTALL-NOTES").canonicalize().unwrap(),
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

// AC: release metadata lives on the member crate and both package formats
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
    assert_eq!(deb["depends"].as_str(), Some("$auto"));

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

// AC: both supported container engines share one ignore policy that excludes
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

// AC: each tool's asset dialect resolves to the same committed files plus the
// workspace-aware release binary, and every committed source exists.
#[test]
fn package_assets_exist_and_match() {
    let (_, member) = metadata();
    let expected = committed_asset_set();
    assert_eq!(expected.len(), 15, "LICENSE + INSTALL-NOTES + 13 icons");

    let deb = deb_assets(&member);
    let rpm = rpm_assets(&member);
    assert_eq!(deb.len(), 16);
    assert_eq!(rpm.len(), 16);
    assert_eq!(deb[0].0, "target/release/solstone-linux");
    assert_eq!(rpm[0].0, "target/release/solstone-linux");
    assert_eq!(deb[0].2, "755");
    assert_eq!(rpm[0].2, "755");

    let deb_paths = deb
        .iter()
        .skip(1)
        .map(|(source, _, _)| manifest_dir().join(source).canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    let rpm_paths = rpm
        .iter()
        .skip(1)
        .map(|(source, _, _)| workspace_root().join(source).canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(deb_paths, expected);
    assert_eq!(rpm_paths, expected);

    for (_, destination, mode) in deb.iter().chain(rpm.iter()) {
        assert!(!destination.ends_with(".service"));
        assert!(!destination.ends_with(".desktop"));
        assert!(mode == "644" || mode == "755");
    }
    assert!(
        member["package"]["metadata"]["deb"]
            .get("systemd-units")
            .is_none()
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn release_fixture(temp: &Path) -> PathBuf {
    let root_name = format!("solstone-linux-{VERSION}-linux-x86_64");
    let root = temp.join(&root_name);
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/solstone-linux"), b"fixture-binary\n").unwrap();
    fs::set_permissions(
        root.join("bin/solstone-linux"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::copy(workspace_root().join("LICENSE"), root.join("LICENSE")).unwrap();
    fs::copy(
        workspace_root().join("packaging/INSTALL-NOTES"),
        root.join("INSTALL-NOTES"),
    )
    .unwrap();
    copy_tree(
        &workspace_root().join("contrib/icons/hicolor"),
        &root.join("share/icons/hicolor"),
    );

    let archive = temp.join(format!("{root_name}.tar.gz"));
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg(&root_name)
        .current_dir(temp)
        .status()
        .unwrap();
    assert!(status.success());
    archive
}

#[derive(Debug, Eq, PartialEq)]
struct SnapshotEntry {
    mode: u32,
    size: u64,
    modified_ns: i128,
    content_hash: u64,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let mut hasher = DefaultHasher::new();
        if metadata.is_file() {
            fs::read(path).unwrap().hash(&mut hasher);
        } else if metadata.file_type().is_symlink() {
            fs::read_link(path).unwrap().hash(&mut hasher);
        }
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            SnapshotEntry {
                mode: metadata.mode(),
                size: metadata.size(),
                modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                    + i128::from(metadata.mtime_nsec()),
                content_hash: hasher.finish(),
            },
        );
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn git_status() -> Vec<u8> {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root())
        .output()
        .unwrap()
        .stdout
}

fn write_os_release(path: &Path) {
    fs::write(path, "ID=ubuntu\nID_LIKE=debian\n").unwrap();
}

fn installer_command(archive: &Path, os_release: &Path) -> Command {
    let mut command = Command::new(command_path("bash"));
    command
        .arg(workspace_root().join("scripts/install.sh"))
        .env("SOLSTONE_INSTALL_OS_RELEASE", os_release)
        .arg(archive);
    command
}

pub(crate) fn command_path(name: &str) -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// AC: installer dry-run prints a complete plan while leaving repository and
// isolated user roots byte-, metadata-, and path-identical.
#[test]
fn installer_dry_run_is_write_free() {
    let temp = tempfile::tempdir().unwrap();
    let archive = release_fixture(temp.path());
    let home = temp.path().join("home");
    let xdg_config = temp.path().join("xdg-config");
    let xdg_data = temp.path().join("xdg-data");
    let task_tmp = temp.path().join("tmp");
    let bin = temp.path().join("path");
    for path in [&home, &xdg_config, &xdg_data, &task_tmp, &bin] {
        fs::create_dir_all(path).unwrap();
    }
    let os_release = temp.path().join("os-release");
    write_os_release(&os_release);

    for allowed in ["tar", "gzip", "uname", "grep"] {
        symlink(command_path(allowed), bin.join(allowed)).unwrap();
    }
    let tripwire = temp.path().join("tripwire");
    for forbidden in ["install", "mkdir", "cp", "mv", "chmod", "rm", "sudo"] {
        let stub = bin.join(forbidden);
        fs::write(
            &stub,
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$0\" > \"$TRIPWIRE\"\nexit 97\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let before = snapshot(temp.path());
    let git_before = git_status();
    let output = installer_command(&archive, &os_release)
        .arg("--dry-run")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("TMPDIR", &task_tmp)
        .env("TRIPWIRE", &tripwire)
        .env("PATH", &bin)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("install binary:"));
    assert!(stdout.contains("install icons:"));
    assert!(stdout.contains("install docs:"));
    assert!(stdout.contains("dry-run: no filesystem changes made"));
    assert!(!tripwire.exists());
    assert_eq!(snapshot(temp.path()), before);
    assert_eq!(git_status(), git_before);
}

// AC: a real portable install writes the expected bytes and executable/data
// modes into an isolated explicit prefix.
#[test]
fn installer_installs_archive_into_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let archive = release_fixture(temp.path());
    let home = temp.path().join("home");
    let prefix = temp.path().join("prefix");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&prefix).unwrap();
    let os_release = temp.path().join("os-release");
    write_os_release(&os_release);

    let output = installer_command(&archive, &os_release)
        .args(["--prefix"])
        .arg(&prefix)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(
        fs::read(prefix.join("bin/solstone-linux")).unwrap(),
        b"fixture-binary\n"
    );
    assert_eq!(
        fs::metadata(prefix.join("bin/solstone-linux"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let icon = prefix.join("share/icons/hicolor/scalable/apps/solstone-observer.svg");
    assert!(icon.is_file());
    assert_eq!(
        fs::metadata(icon).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert!(prefix.join("share/doc/solstone-linux/LICENSE").is_file());
    assert!(
        prefix
            .join("share/doc/solstone-linux/INSTALL-NOTES")
            .is_file()
    );
}

// AC: a real install merges its icons into the shared hicolor theme without
// modifying unrelated application icons or the theme index.
#[test]
fn installer_preserves_foreign_hicolor_files() {
    let temp = tempfile::tempdir().unwrap();
    let archive = release_fixture(temp.path());
    let home = temp.path().join("home");
    let prefix = temp.path().join("prefix");
    let hicolor = prefix.join("share/icons/hicolor");
    let foreign_icon = hicolor.join("48x48/apps/unrelated-app.png");
    let index_theme = hicolor.join("index.theme");
    fs::create_dir_all(foreign_icon.parent().unwrap()).unwrap();
    fs::write(&foreign_icon, b"foreign-icon-bytes\n").unwrap();
    fs::write(&index_theme, b"foreign-theme-index\n").unwrap();
    fs::create_dir_all(&home).unwrap();
    let os_release = temp.path().join("os-release");
    write_os_release(&os_release);

    let output = installer_command(&archive, &os_release)
        .args(["--prefix"])
        .arg(&prefix)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(fs::read(&foreign_icon).unwrap(), b"foreign-icon-bytes\n");
    assert_eq!(fs::read(&index_theme).unwrap(), b"foreign-theme-index\n");
    assert!(
        hicolor
            .join("scalable/apps/solstone-observer.svg")
            .is_file()
    );
    assert!(hicolor.join("48x48/apps/solstone-observer.png").is_file());
}
