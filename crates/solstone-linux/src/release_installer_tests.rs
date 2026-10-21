// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// scripts/install.sh refuses any machine that is not x86_64, the one shipped
// release architecture, so these tests can only hold on an x86_64 host.
// lib.rs compiles this module there and nowhere else.

use std::collections::{BTreeMap, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::release_rail_tests::{command_path, workspace_root};

const VERSION: &str = env!("CARGO_PKG_VERSION");

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

fn add_window_payload(root: &Path) {
    fs::write(root.join("bin/solstone-linux-window"), b"fixture-window\n").unwrap();
    fs::set_permissions(
        root.join("bin/solstone-linux-window"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::create_dir_all(root.join("share/applications")).unwrap();
    fs::copy(
        workspace_root().join("packaging/org.solpbc.solstone.Window.desktop"),
        root.join("share/applications/org.solpbc.solstone.Window.desktop"),
    )
    .unwrap();
    fs::copy(
        workspace_root().join("crates/solstone-linux-window/assets/Comfortaa-OFL.txt"),
        root.join("Comfortaa-OFL.txt"),
    )
    .unwrap();
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
    add_window_payload(&root);
    fs::copy(workspace_root().join("LICENSE"), root.join("LICENSE")).unwrap();
    fs::copy(
        workspace_root().join("packaging/INSTALL-NOTES"),
        root.join("INSTALL-NOTES"),
    )
    .unwrap();
    fs::copy(
        workspace_root().join("RUST_DEPENDENCY_NOTICES.txt"),
        root.join("RUST_DEPENDENCY_NOTICES.txt"),
    )
    .unwrap();
    copy_tree(
        &workspace_root().join("contrib/icons/hicolor"),
        &root.join("share/icons/hicolor"),
    );
    copy_tree(
        &workspace_root().join("packaging/activity-helpers"),
        &root.join("share/solstone-linux/activity-helpers"),
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
    fs::write(
        path,
        "ID=ubuntu\nID_LIKE=debian\nVERSION=\"24.04.4 LTS (Noble Numbat)\"\n",
    )
    .unwrap();
}

fn installer_command(archive: &Path, os_release: &Path) -> Command {
    let mut command = Command::new(command_path("bash"));
    command
        .arg(workspace_root().join("scripts/install.sh"))
        .env("SOLSTONE_INSTALL_OS_RELEASE", os_release)
        .arg(archive);
    command
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// installer dry-run prints a complete plan while leaving repository and
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
    assert!(stdout.contains(&format!("solstone-linux {VERSION} install plan")));
    assert!(!stdout.contains("solstone-linux 24.04.4 LTS (Noble Numbat) install plan"));
    assert!(stdout.contains("install binary:"));
    assert!(stdout.contains("install icons:"));
    assert!(stdout.contains("install docs:"));
    assert!(stdout.contains("dry-run: no filesystem changes made"));
    assert!(!tripwire.exists());
    assert_eq!(snapshot(temp.path()), before);
    assert_eq!(git_status(), git_before);
}

// a real portable install writes the expected bytes and executable/data
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
    let notices_path = prefix.join("share/doc/solstone-linux/RUST_DEPENDENCY_NOTICES.txt");
    assert!(notices_path.is_file());
    assert_eq!(
        fs::read(&notices_path).unwrap(),
        fs::read(workspace_root().join("RUST_DEPENDENCY_NOTICES.txt")).unwrap()
    );
    assert_eq!(
        fs::metadata(&notices_path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let ofl_path = prefix.join("share/doc/solstone-linux/Comfortaa-OFL.txt");
    assert!(ofl_path.is_file());
    assert_eq!(
        fs::read(&ofl_path).unwrap(),
        fs::read(workspace_root().join("crates/solstone-linux-window/assets/Comfortaa-OFL.txt"))
            .unwrap()
    );
    assert_eq!(
        fs::metadata(&ofl_path).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

// a real install merges its icons into the shared hicolor theme without
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

#[test]
fn installer_rejects_archive_missing_dependency_notices() {
    let temp = tempfile::tempdir().unwrap();
    let root_name = format!("solstone-linux-{VERSION}-linux-x86_64");
    let root = temp.path().join(&root_name);
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/solstone-linux"), b"fixture-binary\n").unwrap();
    fs::set_permissions(
        root.join("bin/solstone-linux"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    add_window_payload(&root);
    fs::copy(workspace_root().join("LICENSE"), root.join("LICENSE")).unwrap();
    fs::copy(
        workspace_root().join("packaging/INSTALL-NOTES"),
        root.join("INSTALL-NOTES"),
    )
    .unwrap();
    // Omit RUST_DEPENDENCY_NOTICES.txt
    copy_tree(
        &workspace_root().join("contrib/icons/hicolor"),
        &root.join("share/icons/hicolor"),
    );

    let archive = temp.path().join(format!("{root_name}.tar.gz"));
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg(&root_name)
        .current_dir(temp.path())
        .status()
        .unwrap();
    assert!(status.success());

    let home = temp.path().join("home");
    let prefix = temp.path().join("prefix");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&prefix).unwrap();
    let os_release = temp.path().join("os-release");
    write_os_release(&os_release);

    let before = snapshot(temp.path());

    let output = installer_command(&archive, &os_release)
        .args(["--prefix"])
        .arg(&prefix)
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("archive is missing") && stderr.contains("RUST_DEPENDENCY_NOTICES.txt"),
        "expected missing notice error, actual: {stderr}"
    );
    assert_eq!(snapshot(temp.path()), before);
}
