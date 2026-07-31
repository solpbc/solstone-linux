// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn live_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn fixture(mutate: impl FnOnce(&Path)) -> Fixture {
    let live = live_root();
    let archive = Command::new("git")
        .args(["archive", "--format=tar", "HEAD"])
        .current_dir(&live)
        .output()
        .unwrap();
    assert!(archive.status.success());
    let temp = tempfile::tempdir().unwrap();
    tar::Archive::new(Cursor::new(archive.stdout))
        .unpack(temp.path())
        .unwrap();
    for relative in [
        "Cargo.toml",
        "crates/solstone-linux/Cargo.toml",
        "Cargo.lock",
    ] {
        fs::copy(live.join(relative), temp.path().join(relative)).unwrap();
    }
    mutate(temp.path());
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "spl-pin-fixture@invalid.example"][..],
        &["config", "user.name", "SPL Pin Fixture"][..],
        &["add", "--all"][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    let root = temp.path().to_owned();
    Fixture { _temp: temp, root }
}

fn run(fixture: &Fixture) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rust-release-manifest"))
        .arg("validate-spl-pin")
        .current_dir(&fixture.root)
        .output()
        .unwrap()
}

fn rejected(mutate: impl FnOnce(&Path), repair: &str) {
    let fixture = fixture(mutate);
    let output = run(&fixture);
    assert!(
        !output.status.success(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains(repair),
        "expected {repair:?}, actual {stderr:?}"
    );
}

fn replace(path: &Path, from: &str, to: &str) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains(from), "missing replacement source {from:?}");
    fs::write(path, text.replacen(from, to, 1)).unwrap();
}

fn root_revision(root: &Path, package: &str) -> String {
    let value: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    value["workspace"]["dependencies"][package]["rev"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn root_version(root: &Path, package: &str) -> String {
    let value: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    value["workspace"]["dependencies"][package]["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn different_revision(revision: &str) -> String {
    if revision == "a".repeat(40) {
        "b".repeat(40)
    } else {
        "a".repeat(40)
    }
}

fn package_block(lock: &str, package: &str) -> String {
    let marker = format!("[[package]]\nname = \"{package}\"");
    let start = lock.find(&marker).unwrap();
    let end = lock[start + marker.len()..]
        .find("\n[[package]]")
        .map_or(lock.len(), |offset| start + marker.len() + offset);
    lock[start..end].to_owned()
}

#[test]
fn spl_pin_accepts_consistent_different_revision() {
    let fixture = fixture(|root| {
        let old = root_revision(root, "spl-core");
        let new = different_revision(&old);
        for relative in ["Cargo.toml", "Cargo.lock"] {
            let path = root.join(relative);
            let text = fs::read_to_string(&path).unwrap();
            fs::write(path, text.replace(&old, &new)).unwrap();
        }
    });
    let output = run(&fixture);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "SPL dependency pin verified.\n"
    );
}

#[test]
fn spl_pin_rejects_unapproved_workspace_source() {
    rejected(
        |root| {
            replace(
                &root.join("Cargo.toml"),
                "https://github.com/solpbc/spl-rust",
                "https://example.invalid/spl-rust",
            )
        },
        "repair: declare spl-core from the approved SPL Git source in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_root_workspace_path_dependency() {
    rejected(
        |root| {
            replace(
                &root.join("Cargo.toml"),
                "spl-core = {",
                "spl-core = { path = \"local\",",
            )
        },
        "repair: remove the local path route for spl-core from Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_missing_workspace_version() {
    rejected(
        |root| {
            let version = root_version(root, "spl-core");
            replace(
                &root.join("Cargo.toml"),
                &format!("version = \"{version}\", "),
                "",
            );
        },
        "repair: declare spl-core with the resolved version in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_workspace_version_disagreeing_with_lock() {
    rejected(
        |root| {
            let version = root_version(root, "spl-core");
            replace(
                &root.join("Cargo.toml"),
                &format!("version = \"{version}\""),
                &format!("version = \"{version}.different\""),
            );
        },
        "repair: declare spl-core at the resolved version in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_non_rev_workspace_selector() {
    rejected(
        |root| replace(&root.join("Cargo.toml"), "rev =", "branch ="),
        "repair: select spl-core with only rev in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_short_workspace_revision() {
    rejected(
        |root| {
            let revision = root_revision(root, "spl-core");
            replace(&root.join("Cargo.toml"), &revision, &revision[..39]);
        },
        "repair: declare the full approved spl-core commit in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_missing_workspace_declaration() {
    rejected(
        |root| {
            let path = root.join("Cargo.toml");
            let mut lines = fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            lines.retain(|line| !line.starts_with("spl-core ="));
            fs::write(path, lines.join("\n") + "\n").unwrap();
        },
        "repair: declare spl-core once in root [workspace.dependencies]",
    );
}

#[test]
fn spl_pin_rejects_aliased_duplicate_workspace_declaration() {
    rejected(
        |root| {
            let revision = root_revision(root, "spl-core");
            let version = root_version(root, "spl-core");
            let path = root.join("Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str(&format!("\n[workspace.dependencies.spl_alias]\npackage = \"spl-core\"\nversion = \"{version}\"\ngit = \"https://github.com/solpbc/spl-rust\"\nrev = \"{revision}\"\n"));
            fs::write(path, text).unwrap();
        },
        "repair: declare spl-core once in root [workspace.dependencies]",
    );
}

#[test]
fn spl_pin_rejects_different_workspace_revisions() {
    rejected(
        |root| {
            let old = root_revision(root, "spl-transport");
            let new = different_revision(&old);
            let path = root.join("Cargo.toml");
            let text = fs::read_to_string(&path).unwrap();
            let line = text
                .lines()
                .find(|line| line.starts_with("spl-transport ="))
                .unwrap();
            fs::write(&path, text.replacen(line, &line.replace(&old, &new), 1)).unwrap();
        },
        "repair: pin spl-core and spl-transport to the same revision in root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_leaf_keys_alongside_inheritance() {
    rejected(
        |root| {
            replace(
                &root.join("crates/solstone-linux/Cargo.toml"),
                "spl-core.workspace = true",
                "spl-core = { workspace = true, rev = \"bad\" }",
            )
        },
        "repair: remove local source and version keys from spl-core in crates/solstone-linux/Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_leaf_without_inheritance() {
    rejected(
        |root| {
            let version = root_version(root, "spl-core");
            replace(
                &root.join("crates/solstone-linux/Cargo.toml"),
                "spl-core.workspace = true",
                &format!("spl-core = \"{version}\""),
            )
        },
        "repair: inherit spl-core from root [workspace.dependencies] in crates/solstone-linux/Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_missing_workspace_consumer() {
    rejected(
        |root| {
            let path = root.join("crates/solstone-linux/Cargo.toml");
            let text = fs::read_to_string(&path).unwrap();
            fs::write(path, text.replace("spl-core.workspace = true\n", "")).unwrap();
        },
        "repair: inherit spl-core from root [workspace.dependencies] in a workspace member",
    );
}

#[test]
fn spl_pin_rejects_duplicate_workspace_consumers() {
    rejected(
        |root| {
            let path = root.join("crates/rust-release-manifest/Cargo.toml");
            let text = fs::read_to_string(&path).unwrap();
            fs::write(
                path,
                text.replace(
                    "[dev-dependencies]\n",
                    "[dev-dependencies]\nspl-core.workspace = true\n",
                ),
            )
            .unwrap();
        },
        "repair: inherit spl-core from root [workspace.dependencies] in only one workspace member",
    );
}

#[test]
fn spl_pin_rejects_missing_lock_record() {
    rejected(
        |root| {
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            fs::write(path, text.replacen(&block, "", 1)).unwrap();
        },
        "repair: regenerate Cargo.lock with the approved spl-core workspace pin",
    );
}

#[test]
fn spl_pin_rejects_duplicate_lock_record() {
    rejected(
        |root| {
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            fs::write(path, format!("{text}\n{block}\n")).unwrap();
        },
        "repair: regenerate Cargo.lock with one resolved spl-core package",
    );
}

#[test]
fn spl_pin_rejects_wrong_lock_source() {
    rejected(
        |root| {
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            fs::write(
                path,
                text.replacen(
                    &block,
                    &block.replacen(
                        "https://github.com/solpbc/spl-rust",
                        "https://example.invalid/spl-rust",
                        1,
                    ),
                    1,
                ),
            )
            .unwrap();
        },
        "repair: regenerate Cargo.lock from the approved spl-core workspace source",
    );
}

#[test]
fn spl_pin_rejects_non_rev_lock_selector() {
    rejected(
        |root| {
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            fs::write(
                path,
                text.replacen(&block, &block.replacen("?rev=", "?branch=", 1), 1),
            )
            .unwrap();
        },
        "repair: regenerate Cargo.lock from the rev-selected spl-core workspace declaration",
    );
}

#[test]
fn spl_pin_rejects_wrong_lock_query_revision() {
    rejected(
        |root| {
            let revision = root_revision(root, "spl-core");
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            let changed = block.replacen(
                &format!("rev={revision}"),
                &format!("rev={}", different_revision(&revision)),
                1,
            );
            fs::write(path, text.replacen(&block, &changed, 1)).unwrap();
        },
        "repair: regenerate Cargo.lock from the approved spl-core workspace revision",
    );
}

#[test]
fn spl_pin_rejects_wrong_lock_resolved_revision() {
    rejected(
        |root| {
            let revision = root_revision(root, "spl-core");
            let path = root.join("Cargo.lock");
            let text = fs::read_to_string(&path).unwrap();
            let block = package_block(&text, "spl-core");
            let changed = block.replacen(
                &format!("#{revision}"),
                &format!("#{}", different_revision(&revision)),
                1,
            );
            fs::write(path, text.replacen(&block, &changed, 1)).unwrap();
        },
        "repair: regenerate Cargo.lock so spl-core resolves to the approved workspace revision",
    );
}

#[test]
fn spl_pin_rejects_git_source_patch() {
    rejected(
        |root| {
            let path = root.join("Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("\n[patch.\"https://github.com/solpbc/spl-rust\"]\nspl-core = { path = \"local\" }\n");
            fs::write(path, text).unwrap();
        },
        "repair: remove the spl-core patch override from root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_crates_io_patch() {
    rejected(
        |root| {
            let path = root.join("Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("\n[patch.crates-io]\nspl-core = { path = \"local\" }\n");
            fs::write(path, text).unwrap();
        },
        "repair: remove the spl-core patch override from root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_replace() {
    rejected(
        |root| {
            let version = root_version(root, "spl-core");
            let path = root.join("Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str(&format!(
                "\n[replace]\n\"spl-core:{version}\" = {{ path = \"local\" }}\n"
            ));
            fs::write(path, text).unwrap();
        },
        "repair: remove the spl-core replacement from root Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_aliased_path_dependency() {
    rejected(
        |root| {
            let path = root.join("crates/solstone-linux/Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("\n[target.'cfg(any())'.dependencies]\nspl_alias = { package = \"spl-core\", path = \"../local\" }\n");
            fs::write(path, text).unwrap();
        },
        "repair: remove the local path route for spl-core from crates/solstone-linux/Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_root_cargo_source_replacement() {
    rejected(
        |root| {
            fs::create_dir(root.join(".cargo")).unwrap();
            fs::write(root.join(".cargo/config.toml"), "[source.crates-io]\nreplace-with = \"local\"\n[source.local]\ndirectory = \"vendor\"\n").unwrap();
        },
        "repair: remove the replace-with route so the SPL packages resolve from the declared workspace source",
    );
}

#[test]
fn spl_pin_rejects_tracked_in_tree_package() {
    rejected(
        |root| {
            let version = root_version(root, "spl-core");
            fs::create_dir(root.join("local-spl")).unwrap();
            fs::write(
                root.join("local-spl/Cargo.toml"),
                format!("[package]\nname = \"spl-core\"\nversion = \"{version}\"\n"),
            )
            .unwrap();
        },
        "repair: remove or rename the tracked in-tree crate implementing spl-core",
    );
}

#[test]
fn spl_pin_rejects_unapproved_member_git_dependency() {
    rejected(
        |root| {
            let path = root.join("crates/solstone-linux/Cargo.toml");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str("\n[target.'cfg(any())'.dependencies]\nother = { git = \"https://example.invalid/other\" }\n");
            fs::write(path, text).unwrap();
        },
        "repair: remove the unapproved Git dependency from crates/solstone-linux/Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_invalid_workspace_manifest_toml() {
    rejected(
        |root| fs::write(root.join("Cargo.toml"), "not = [valid").unwrap(),
        "repair: restore valid TOML in Cargo.toml",
    );
}

#[test]
fn spl_pin_rejects_invalid_lockfile_toml() {
    rejected(
        |root| fs::write(root.join("Cargo.lock"), "not = [valid").unwrap(),
        "repair: restore a valid Cargo.lock before validating the SPL pin",
    );
}
