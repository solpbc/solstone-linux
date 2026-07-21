// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use chrono::Utc;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::process::Output;
use std::sync::OnceLock;

static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();

pub(super) struct TestRepo {
    _temp: tempfile::TempDir,
    pub root: RepoRoot,
    pub commit: String,
    pub cargo_lock_sha256: String,
    pub cargo_deny_version: String,
    pub exceptions: Vec<String>,
}

pub(super) fn fixture() -> TestRepo {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let archive = ARCHIVE.get_or_init(|| {
        Command::new("git")
            .args(["archive", "--format=tar", "HEAD"])
            .current_dir(&source)
            .output()
            .unwrap()
            .stdout
    });
    let temp = tempfile::tempdir().unwrap();
    Archive::new(Cursor::new(archive))
        .unpack(temp.path())
        .unwrap();
    let image_policy = Path::new("packaging/release-policy.toml");
    if !temp.path().join(image_policy).exists() {
        fs::copy(source.join(image_policy), temp.path().join(image_policy)).unwrap();
    }
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "release-fixture@invalid.example"][..],
        &["config", "user.name", "Release Fixture"][..],
        &["add", "--all"][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        let mut command = Command::new("git");
        command.args(args).current_dir(temp.path());
        if args.first() == Some(&"commit") {
            command
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
        }
        assert!(command.status().unwrap().success());
    }
    let root = RepoRoot::validate_path(temp.path()).unwrap();
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"]).unwrap();
    let cargo_lock_sha256 = digest(&fs::read(root.path().join("Cargo.lock")).unwrap());
    let makefile = fs::read_to_string(root.path().join("Makefile")).unwrap();
    let cargo_deny_version = makefile
        .lines()
        .find_map(|line| line.strip_prefix("CARGO_DENY_VERSION := "))
        .unwrap()
        .to_owned();
    let exceptions = ordered_exceptions(&root).unwrap();
    TestRepo {
        _temp: temp,
        root,
        commit,
        cargo_lock_sha256,
        cargo_deny_version,
        exceptions,
    }
}

pub(super) fn sha256_fixture() -> TestRepo {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let archive = Command::new("git")
        .args(["archive", "--format=tar", "HEAD"])
        .current_dir(&source)
        .output()
        .unwrap()
        .stdout;
    let temp = tempfile::tempdir().unwrap();
    Archive::new(Cursor::new(archive))
        .unpack(temp.path())
        .unwrap();
    let image_policy = Path::new("packaging/release-policy.toml");
    if !temp.path().join(image_policy).exists() {
        fs::copy(source.join(image_policy), temp.path().join(image_policy)).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "-q", "--object-format=sha256"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success()
    );
    for args in [
        &["config", "user.email", "release-fixture@invalid.example"][..],
        &["config", "user.name", "Release Fixture"][..],
        &["add", "--all"][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .status()
                .unwrap()
                .success()
        );
    }
    let root = RepoRoot::validate_path(temp.path()).unwrap();
    let commit = command(root.path(), &["git", "rev-parse", "HEAD"]).unwrap();
    let cargo_lock_sha256 = digest(&fs::read(root.path().join("Cargo.lock")).unwrap());
    let makefile = fs::read_to_string(root.path().join("Makefile")).unwrap();
    let cargo_deny_version = makefile
        .lines()
        .find_map(|line| line.strip_prefix("CARGO_DENY_VERSION := "))
        .unwrap()
        .into();
    let exceptions = ordered_exceptions(&root).unwrap();
    TestRepo {
        _temp: temp,
        root,
        commit,
        cargo_lock_sha256,
        cargo_deny_version,
        exceptions,
    }
}

pub(super) struct StubPath {
    _temp: tempfile::TempDir,
    pub bin: PathBuf,
    pub argv: PathBuf,
    pub tripwire: PathBuf,
    pub output: PathBuf,
}

impl StubPath {
    pub fn new(name: &str, output_fixture: Option<&Path>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let argv = temp.path().join("argv");
        let tripwire = temp.path().join("tripwire");
        let output = temp.path().join("output");
        let executable = bin.join(name);
        let fixture = output_fixture.map_or_else(String::new, |path| path.display().to_string());
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$PWD\" \"$@\" > \"$ARGV_RECORD\"\n\
                 if [ -n \"$TRIPWIRE\" ]; then printf called > \"$TRIPWIRE\"; fi\n\
                 if [ -n '{fixture}' ]; then cp '{fixture}' \"$STUB_OUTPUT\"; fi\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _temp: temp,
            bin,
            argv,
            tripwire,
            output,
        }
    }

    pub fn command(&self, name: &str) -> Command {
        let mut command = Command::new(self.bin.join(name));
        command
            .env("ARGV_RECORD", &self.argv)
            .env("TRIPWIRE", &self.tripwire)
            .env("STUB_OUTPUT", &self.output);
        command
    }

    pub fn run(&self, name: &str, args: &[&str]) -> Output {
        self.command(name).args(args).output().unwrap()
    }
}

#[test]
fn fixture_binds_real_repository_authorities() {
    let repo = fixture();
    assert_eq!(repo.commit.len(), 40);
    assert!(is_sha256(&repo.cargo_lock_sha256));
    assert_eq!(repo.cargo_deny_version, CARGO_DENY_VERSION);
    assert_eq!(repo.exceptions, TEST_EXCEPTIONS);
}

#[test]
fn lane_rejects_carried_source_that_disagrees_with_exported_context() {
    let repo = fixture();
    let temp = tempfile::tempdir().unwrap();
    let context_root = temp.path().join("context");
    fs::create_dir(&context_root).unwrap();
    let context = export_immutable_context(&repo.root, &context_root).unwrap();
    let error = emit_lane_handoff_in(
        &LaneEmitRequest {
            lane: Lane::Deb,
            invocation_id: "d",
            source_commit: &"e".repeat(40),
            source_archive_sha256: &context.archive_sha256,
            expected_cargo_lock_sha256: &context.cargo_lock_sha256,
            version: "1.0.0",
            target: TARGET_TRIPLE,
            profile: "release",
            features: Vec::new(),
            image_digest: &format!("sha256:{}", "f".repeat(64)),
            baseline_executable: Path::new("target/release/solstone-linux"),
            artifacts: Vec::new(),
            output: Path::new("unused"),
        },
        &context_root,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("immutable context binding mismatch")
    );
}

#[test]
fn path_stub_records_nul_safe_argv_and_cwd() {
    let stub = StubPath::new("tool", None);
    assert!(stub.run("tool", &["one", "two words"]).status.success());
    let bytes = fs::read(&stub.argv).unwrap();
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    assert_eq!(fields[1], b"one");
    assert_eq!(fields[2], b"two words");
    assert!(stub.tripwire.exists());
}

#[test]
fn exclusive_lock_loser_mutates_nothing() {
    let repo = fixture();
    fs::create_dir(repo.root.path().join("dist")).unwrap();
    let owner = CandidateLock::acquire(&repo.root).unwrap();
    let before = fs::read_dir(repo.root.path().join("dist")).unwrap().count();
    assert!(CandidateLock::acquire(&repo.root).is_err());
    assert_eq!(
        fs::read_dir(repo.root.path().join("dist")).unwrap().count(),
        before
    );
    drop(owner);
}

#[test]
fn explicit_lock_release_reports_replacement_and_preserves_foreign_file() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let path = lock.path().to_owned();
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"foreign").unwrap();
    let error = lock.release().unwrap_err();
    assert!(error.to_string().contains("repair:"));
    assert_eq!(fs::read(path).unwrap(), b"foreign");
}

#[test]
fn staging_paths_are_owner_scoped() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    assert!(
        staging.root.starts_with(
            repo.root
                .path()
                .join("dist/.rust-release-candidate-staging")
        )
    );
    for path in [
        &staging.context,
        &staging.deb_lane,
        &staging.rpm_lane,
        &staging.advisory_db,
        &staging.payload,
    ] {
        assert!(path.is_dir());
    }
}

#[test]
fn staging_construction_failure_cleans_only_owned_root_and_reports_residue() {
    let repo = fixture();
    let parent = repo
        .root
        .path()
        .join("dist/.rust-release-candidate-staging");
    fs::create_dir_all(&parent).unwrap();
    let sibling = parent.join("foreign");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("canary"), b"foreign").unwrap();

    let owned = parent.join("owned-mid-failure");
    fs::create_dir(&owned).unwrap();
    fs::write(owned.join("lane-rpm"), b"blocks directory creation").unwrap();
    assert!(StagingLayout::initialize_owned(owned.clone()).is_err());
    assert!(!owned.exists());
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
    assert!(!repo.root.path().join("dist/rust").exists());
    assert!(!repo.root.path().join("dist/rust-evidence").exists());

    let residue = parent.join("owned-cleanup-failure");
    fs::create_dir(&residue).unwrap();
    fs::write(residue.join("lane-rpm"), b"blocks directory creation").unwrap();
    fs::set_permissions(&residue, fs::Permissions::from_mode(0o555)).unwrap();
    let error = StagingLayout::initialize_owned(residue.clone()).unwrap_err();
    fs::set_permissions(&residue, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(error.to_string().contains("Permission denied"));
    assert!(error.to_string().contains("repair: remove"));
    assert!(residue.exists());
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
    fs::remove_dir_all(residue).unwrap();
}

#[test]
fn controlled_rollback_removes_only_owned_payload_and_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let payload = temp.path().join("dist/rust");
    let evidence = temp.path().join("dist/rust-evidence/1.0.0");
    let proofs = evidence.join("proofs");
    fs::create_dir_all(&payload).unwrap();
    fs::create_dir_all(&proofs).unwrap();
    fs::write(payload.join("candidate"), b"bytes").unwrap();
    fs::write(evidence.join("ledger.json"), b"ledger").unwrap();
    let owned = proofs.join("debian-amd64.json");
    fs::write(&owned, b"proof").unwrap();
    let unowned = temp.path().join("dist/rust-evidence/other/ledger.json");
    fs::create_dir_all(unowned.parent().unwrap()).unwrap();
    fs::write(&unowned, b"retain").unwrap();
    let error = rollback_error(
        Error::new("controlled failure"),
        &payload,
        &evidence,
        &[owned],
    );
    assert_eq!(error.to_string(), "controlled failure");
    assert!(!payload.exists());
    assert!(!evidence.exists());
    assert_eq!(fs::read(unowned).unwrap(), b"retain");
}

#[test]
fn controlled_rollback_reports_exact_residue() {
    let temp = tempfile::tempdir().unwrap();
    let payload = temp.path().join("dist/rust");
    fs::create_dir_all(&payload).unwrap();
    fs::set_permissions(payload.parent().unwrap(), fs::Permissions::from_mode(0o555)).unwrap();
    let evidence = temp.path().join("dist/rust-evidence/1.0.0");
    let error = rollback_error(Error::new("controlled failure"), &payload, &evidence, &[]);
    fs::set_permissions(payload.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        error
            .to_string()
            .contains(&format!("residue at {}", payload.display()))
    );
    assert!(error.to_string().contains("repair:"));
}

#[test]
fn clean_tree_accepts_an_absent_ignored_candidate_path() {
    let repo = fixture();
    let dist = repo.root.path().join("dist");
    fs::create_dir(&dist).unwrap();
    let payload = dist.join("rust");
    assert!(!payload.exists());
    require_clean_tree(repo.root.path(), &payload).unwrap();
    assert!(!payload.exists());
}

#[test]
fn immutable_context_uses_committed_archive() {
    let repo = fixture();
    fs::write(repo.root.path().join("UNTRACKED_CANARY"), b"ambient").unwrap();
    let destination = tempfile::tempdir().unwrap();
    let context = export_immutable_context(&repo.root, destination.path()).unwrap();
    assert_eq!(context.commit, repo.commit);
    assert_eq!(context.cargo_lock_sha256, repo.cargo_lock_sha256);
    assert!(!destination.path().join("UNTRACKED_CANARY").exists());
}

#[test]
fn release_image_policy_is_digest_only_strict_and_immutable() {
    let repo = fixture();
    let destination = tempfile::tempdir().unwrap();
    let context = export_immutable_context(&repo.root, destination.path()).unwrap();
    let expected = ReleaseImages::from_context(&context).unwrap();
    fs::write(
        repo.root.path().join("packaging/release-policy.toml"),
        "build_ubuntu = \"ubuntu:latest\"\n",
    )
    .unwrap();
    assert_eq!(ReleaseImages::from_context(&context).unwrap(), expected);

    let policy = context.path.join("packaging/release-policy.toml");
    let valid = fs::read(&policy).unwrap();
    for invalid in [
        "",
        "build_ubuntu = \"ubuntu:22.04\"\nbuild_fedora = \"fedora:42\"\nproof_debian = \"ubuntu:22.04\"\nproof_rpm = \"fedora:42\"\nproof_tar = \"ubuntu:22.04\"\n",
        "build_ubuntu = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
        "build_ubuntu = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nbuild_fedora = \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\nproof_debian = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nproof_rpm = \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\nproof_tar = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nunknown = \"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"\n",
        "build_ubuntu = \"--image\"\nbuild_fedora = \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\nproof_debian = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nproof_rpm = \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\nproof_tar = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
    ] {
        fs::write(&policy, invalid).unwrap();
        assert!(ReleaseImages::from_context(&context).is_err());
    }
    fs::write(&policy, valid).unwrap();
}

#[test]
fn release_image_resolution_rejects_absent_and_mismatched_local_ids() {
    let proof_policy = |image_digest: String| ProofPlatformPolicy {
        image_digest,
        os_release: "Ubuntu 22.04.5 LTS".into(),
        package_manager_version: "dpkg 1".into(),
        install_command: vec!["install".into()],
        version_command: vec!["version".into()],
        executable_path: "/usr/bin/solstone-linux".into(),
        executable_mode: 0o755,
    };
    let images = ReleaseImages {
        build_ubuntu: format!("sha256:{}", "a".repeat(64)),
        build_fedora: format!("sha256:{}", "b".repeat(64)),
        proof_debian: format!("sha256:{}", "a".repeat(64)),
        proof_rpm: format!("sha256:{}", "b".repeat(64)),
        proof_tar: format!("sha256:{}", "a".repeat(64)),
        debian_amd64: proof_policy(format!("sha256:{}", "a".repeat(64))),
        rpm_x86_64: proof_policy(format!("sha256:{}", "b".repeat(64))),
        tar_x86_64: proof_policy(format!("sha256:{}", "a".repeat(64))),
    };
    let mismatch = format!("#!/bin/sh\nprintf '%s' '{}'\n", image_json('c'));
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&mismatch));
    assert!(resolve_release_images(&processes, ContainerEngine::Podman, &images).is_err());
    let (_bin, absent) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 44\n"));
    assert!(resolve_release_images(&absent, ContainerEngine::Podman, &images).is_err());
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn process_bin(
    cargo_body: &str,
    podman_body: Option<&str>,
) -> (tempfile::TempDir, ProcessEnvironment) {
    let temp = tempfile::tempdir().unwrap();
    executable(&temp.path().join("cargo"), cargo_body);
    if let Some(body) = podman_body {
        executable(&temp.path().join("podman"), body);
    }
    let path = format!("{}:/usr/bin:/bin", temp.path().display());
    (temp, ProcessEnvironment::with_path(OsStr::new(&path)))
}

pub(super) fn git_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "advisory@invalid.example"][..],
        &["config", "user.name", "Advisory Fixture"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .status()
                .unwrap()
                .success()
        );
    }
    fs::create_dir_all(temp.path().join("crates/example")).unwrap();
    fs::write(
        temp.path().join("crates/example/RUSTSEC-2026-0001.md"),
        "fixture\n",
    )
    .unwrap();
    for args in [
        &["add", "--all"][..],
        &["commit", "-q", "-m", "advisories"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(temp.path())
                .status()
                .unwrap()
                .success()
        );
    }
    temp
}

pub(super) fn descriptor(
    root: &Path,
    db: &Path,
    extra: Option<(&str, Value)>,
    acquired_at: &str,
) -> PathBuf {
    let mut value = serde_json::json!({
        "schema_version": 1,
        "source_id": "rustsec snapshot 1",
        "db_path": db,
        "acquired_at": acquired_at,
    });
    if let Some((key, extra)) = extra {
        value[key] = extra;
    }
    let path = root.join("advisory-descriptor.json");
    fs::write(path.as_path(), serde_json::to_vec(&value).unwrap()).unwrap();
    path
}

pub(super) fn current_time() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

const CARGO_DENY_ASSERTIONS: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$PWD/cargo-deny-argv"
case " $* " in
  *" deny --locked --offline --config "*" check licenses bans sources "*) ;;
  *" deny --locked --offline --config "*" check advisories "*)
    config="$5"
    grep -F 'db-urls = ["file://localhost/advisory-db"]' "$config" >/dev/null || exit 91
    grep -F 'maximum-db-staleness = "1d"' "$config" >/dev/null || exit 92
    grep -F 'github.com/RustSec' "$config" >/dev/null && exit 93
    ;;
  *) exit 94 ;;
esac
exit 0
"#;

#[test]
fn advisory_cohort_is_clean_local_ordered_and_offline() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let db = git_repo();
    let descriptor = descriptor(&staging.root, db.path(), None, &current_time());
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, None);
    let before = fs::read(repo.root.path().join("deny.toml")).unwrap();
    let cohort = run_advisory_cohort(&context, &staging, &descriptor, &processes).unwrap();
    assert_eq!(cohort.deterministic_gate, "pass");
    assert_eq!(cohort.licenses_bans_sources, "pass");
    assert_eq!(cohort.advisories, "pass");
    assert_eq!(
        fs::read(repo.root.path().join("deny.toml")).unwrap(),
        before
    );
    let calls = fs::read_to_string(context.path.join("cargo-deny-argv")).unwrap();
    let calls = calls.lines().collect::<Vec<_>>();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].ends_with("check licenses bans sources"));
    assert!(calls[1].ends_with("check advisories"));
    let config = fs::read_to_string(staging.root.join("advisory-deny.toml")).unwrap();
    assert!(config.contains("db-path = \""));
    assert!(config.contains("db-urls = [\"file://localhost/advisory-db\"]"));
    assert!(!config.contains("github.com/RustSec"));
    assert!(
        !serde_json::to_string(&cohort.source_id)
            .unwrap()
            .contains(db.path().to_str().unwrap())
    );
}

#[test]
fn advisory_descriptor_rejects_absent_stale_dirty_and_fabricated_pass() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, None);
    for case in ["absent", "stale", "dirty", "fabricated"] {
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let db = git_repo();
        let missing = staging.root.join("missing-db");
        let db_path = if case == "absent" {
            missing.as_path()
        } else {
            db.path()
        };
        if case == "dirty" {
            fs::write(db.path().join("DIRTY"), b"dirty").unwrap();
        }
        let acquired = if case == "stale" {
            "2020-01-01T00:00:00Z".to_owned()
        } else {
            current_time()
        };
        let extra =
            (case == "fabricated").then(|| ("deterministic_gate", Value::String("pass".into())));
        let path = descriptor(&staging.root, db_path, extra, &acquired);
        assert!(
            run_advisory_cohort(&context, &staging, &path, &processes).is_err(),
            "accepted {case}"
        );
    }
}

#[test]
fn resume_accepts_aged_identity_matching_advisory_material() {
    let db = git_repo();
    let input = tempfile::tempdir().unwrap();
    let descriptor = descriptor(input.path(), db.path(), None, "2020-01-01T00:00:00Z");
    let processes = ProcessEnvironment::default();
    assert!(validate_advisory_descriptor_identity(&descriptor, &processes).is_err());
    let identity = validate_resume_advisory_identity(&descriptor, &processes).unwrap();
    assert_eq!(identity.source_id, "rustsec snapshot 1");
}

#[test]
fn advisory_paths_and_ambient_git_overrides_fail_closed() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    for db_path in [
        PathBuf::from("relative/db"),
        PathBuf::from("--git-dir=escape"),
    ] {
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let path = descriptor(&staging.root, &db_path, None, &current_time());
        let (bin, processes) = process_bin("#!/bin/sh\nexit 99\n", None);
        let tripwire = staging.root.join("git-tripwire");
        executable(
            &bin.path().join("git"),
            &format!(
                "#!/bin/sh\nprintf called > '{}'\nexit 99\n",
                tripwire.display()
            ),
        );
        assert!(run_advisory_cohort(&context, &staging, &path, &processes).is_err());
        assert!(!tripwire.exists());
    }

    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let db = git_repo();
    let path = descriptor(&staging.root, db.path(), None, &current_time());
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, None);
    let processes = processes.with_git_canaries(
        OsStr::new("/forbidden/git-dir"),
        OsStr::new("/forbidden/git-work-tree"),
    );
    run_advisory_cohort(&context, &staging, &path, &processes).unwrap();
}

#[test]
fn advisory_uses_exported_policy_and_lock_and_rechecks_snapshot() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let expected_lock = digest(&fs::read(context.path.join("Cargo.lock")).unwrap());
    fs::write(repo.root.path().join("Cargo.lock"), b"LIVE LOCK MUTATION").unwrap();
    fs::write(repo.root.path().join("deny.toml"), b"LIVE POLICY MUTATION").unwrap();
    let db = git_repo();
    let path = descriptor(&staging.root, db.path(), None, &current_time());
    let cargo = format!(
        "#!/bin/sh\n[ \"$(sha256sum Cargo.lock | cut -d' ' -f1)\" = '{expected_lock}' ] || exit 91\ncase \" $* \" in *' check licenses bans sources '*) [ \"$5\" = '{}/deny.toml' ] || exit 92;; *' check advisories '*) grep -F 'file://localhost/advisory-db' \"$5\" >/dev/null || exit 93;; *) exit 94;; esac\n",
        context.path.display()
    );
    let (_bin, processes) = process_bin(&cargo, None);
    let cohort = run_advisory_cohort(&context, &staging, &path, &processes).unwrap();
    recheck_advisory_cohort(&cohort, &processes, Utc::now()).unwrap();
    fs::write(db.path().join("DRIFT"), b"changed").unwrap();
    assert!(recheck_advisory_cohort(&cohort, &processes, Utc::now()).is_err());
}

#[test]
fn advisory_policy_failures_and_wrong_materialization_fail_closed() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    for failure in ["licenses", "bans", "sources"] {
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let db = git_repo();
        let path = descriptor(&staging.root, db.path(), None, &current_time());
        let script =
            format!("#!/bin/sh\ncase \" $* \" in *' {failure} '*) exit 97;; esac\nexit 0\n");
        let (_bin, processes) = process_bin(&script, None);
        assert!(run_advisory_cohort(&context, &staging, &path, &processes).is_err());
    }

    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    fs::create_dir(staging.advisory_db.join("advisory-db-wrong")).unwrap();
    let db = git_repo();
    let path = descriptor(&staging.root, db.path(), None, &current_time());
    let rejecting = "#!/bin/sh\nconfig=\"$5\"\ngrep -F 'file://localhost/advisory-db' \"$config\" >/dev/null || exit 98\nexit 99\n";
    let (_bin, processes) = process_bin(rejecting, None);
    assert!(run_advisory_cohort(&context, &staging, &path, &processes).is_err());

    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let db = git_repo();
    let path = descriptor(&staging.root, db.path(), None, &current_time());
    let (bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, None);
    executable(
        &bin.path().join("git"),
        "#!/bin/sh\n/usr/bin/git \"$@\" || exit $?\nif [ \"$1\" = clone ]; then dest=; for arg in \"$@\"; do dest=$arg; done; printf swapped > \"$dest/SWAPPED\"; fi\n",
    );
    assert!(run_advisory_cohort(&context, &staging, &path, &processes).is_err());
}

fn image_json(id: char) -> String {
    format!(
        "[{{\"Id\":\"{}\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}]",
        id.to_string().repeat(64)
    )
}

pub(super) fn lane_tools(lane: Lane, image_digest: &str) -> LaneNativeTools {
    let image = image_digest.strip_prefix("sha256:").unwrap().to_owned();
    match lane {
        Lane::Deb => LaneNativeTools::Ubuntu(UbuntuLaneTools {
            cargo_deb: "3.7.0".into(),
            dpkg_deb: "dpkg-deb 1.21.1".into(),
            signing_mode: "unsigned".into(),
            ubuntu_cargo: "cargo 1.97.1".into(),
            ubuntu_compiler: "gcc 11.4.0".into(),
            ubuntu_glibc: "glibc 2.35".into(),
            ubuntu_gzip: "gzip 1.10".into(),
            ubuntu_image_digest: image,
            ubuntu_linker: "GNU ld 2.38".into(),
            ubuntu_os: "Ubuntu 22.04".into(),
            ubuntu_rustc: "rustc 1.97.1".into(),
            ubuntu_tar: "GNU tar 1.34".into(),
        }),
        Lane::Rpm => LaneNativeTools::Fedora(FedoraLaneTools {
            cargo_generate_rpm: "0.21.0".into(),
            fedora_image_digest: image,
            fedora_os: "Fedora 42".into(),
            rpm: "RPM 4.20.0".into(),
            signing_mode: "unsigned".into(),
        }),
    }
}

#[test]
fn image_inspection_and_recheck_are_immutable_and_local() {
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in '--version') printf '%s' 'podman version 5.8.3' ;; 'image inspect sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa') printf '%s' '{}' ;; 'image inspect sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb') printf '%s' '{}' ;; *) exit 97;; esac\n",
        image_json('a'),
        image_json('b')
    );
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&script));
    let ubuntu = inspect_image(
        &processes,
        ContainerEngine::Podman,
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap();
    let fedora = inspect_image(
        &processes,
        ContainerEngine::Podman,
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap();
    assert_eq!(ubuntu.digest, format!("sha256:{}", "a".repeat(64)));
    assert_eq!(
        observe_container_engine(&processes, ContainerEngine::Podman).unwrap(),
        "podman version 5.8.3"
    );
    recheck_images(&processes, ContainerEngine::Podman, [&ubuntu, &fedora]).unwrap();

    let changed = format!("#!/bin/sh\nprintf '%s' '{}'\n", image_json('c'));
    let (_bin, changed_processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&changed));
    assert!(
        recheck_images(
            &changed_processes,
            ContainerEngine::Podman,
            [&ubuntu, &fedora]
        )
        .is_err()
    );
    let (_bin, missing) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 44\n"));
    assert!(
        inspect_image(
            &missing,
            ContainerEngine::Podman,
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        )
        .is_err()
    );
}

pub(super) fn lane_fixture(
    root: &Path,
    lane: Lane,
    context: &ImmutableContext,
    invocation: &str,
    image: &str,
    tar_bytes: &[u8],
) -> LaneEvidence {
    let version = "1.0.0";
    let tar_name = format!("solstone-linux-{version}-linux-x86_64.tar.gz");
    fs::write(root.join(&tar_name), tar_bytes).unwrap();
    let package_name = match lane {
        Lane::Deb => format!("solstone-linux_{version}-1_amd64.deb"),
        Lane::Rpm => format!("solstone-linux-{version}-1.x86_64.rpm"),
    };
    fs::write(root.join(&package_name), b"package").unwrap();
    let evidence = LaneEvidence {
        invocation_id: invocation.into(),
        lane,
        source_commit: context.commit.clone(),
        source_archive_sha256: context.archive_sha256.clone(),
        cargo_lock_sha256: context.cargo_lock_sha256.clone(),
        version: version.into(),
        target: TARGET_TRIPLE.into(),
        profile: "release".into(),
        features: vec![],
        rustc_verbose: "rustc 1.97.1 (abcdef012 2026-06-30)\nbinary: rustc\ncommit-hash: 0123456789abcdef0123456789abcdef01234567\ncommit-date: 2026-06-30\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\nLLVM version: 18.1.0".into(),
        cargo: "cargo 1.97.1 (abcdef012 2026-06-30)".into(),
        baseline_executable_sha256: "d".repeat(64),
        image_digest: image.into(),
        packaging_tool: match lane {
            Lane::Deb => "cargo-deb 3.7.0",
            Lane::Rpm => "cargo-generate-rpm 0.21.0",
        }
        .into(),
        native_tools: lane_tools(lane, image),
        artifacts: vec![
            artifact(&root.join(&tar_name)).unwrap(),
            artifact(&root.join(&package_name)).unwrap(),
        ],
    };
    fs::write(
        root.join(LANE_EVIDENCE_NAME),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    evidence
}

fn finalized_lane_fixture(
    output: &Path,
    lane: Lane,
    context: &ImmutableContext,
    image: &str,
) -> LaneEvidence {
    let products = crate::tests::release_fixture();
    let tar = "solstone-linux-1.0.0-linux-x86_64.tar.gz";
    let package = match lane {
        Lane::Deb => "solstone-linux_1.0.0-1_amd64.deb",
        Lane::Rpm => "solstone-linux-1.0.0-1.x86_64.rpm",
    };
    fs::copy(products.path().join(tar), output.join(tar)).unwrap();
    fs::copy(products.path().join(package), output.join(package)).unwrap();
    let mut evidence = lane_fixture(
        output,
        lane,
        context,
        "0123456789abcdef0123456789abcdef",
        image,
        &fs::read(products.path().join(tar)).unwrap(),
    );
    fs::copy(products.path().join(package), output.join(package)).unwrap();
    evidence.artifacts = [tar, package]
        .iter()
        .map(|name| artifact(&output.join(name)).unwrap())
        .collect();
    fs::write(
        output.join(LANE_EVIDENCE_NAME),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    evidence
}

#[test]
fn finalize_candidate_rolls_back_post_promotion_image_recheck_failure() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let policy = ReleaseImages::from_root(&context.path).unwrap();
    let ubuntu = proof_image_identity(&policy.build_ubuntu);
    let fedora = proof_image_identity(&policy.build_fedora);
    let images = ResolvedImages {
        build_ubuntu: ubuntu.clone(),
        build_fedora: fedora.clone(),
        proof_debian: proof_image_identity(&policy.proof_debian),
        proof_rpm: proof_image_identity(&policy.proof_rpm),
        proof_tar: proof_image_identity(&policy.proof_tar),
    };
    let deb = finalized_lane_fixture(&staging.deb_lane, Lane::Deb, &context, &ubuntu.digest);
    let rpm = finalized_lane_fixture(&staging.rpm_lane, Lane::Rpm, &context, &fedora.digest);
    let db = git_repo();
    let descriptor = descriptor(&staging.root, db.path(), None, &current_time());
    let counter = tempfile::tempdir().unwrap();
    let count = counter.path().join("inspect-count");
    let podman = format!(
        "#!/bin/sh\ncount=0\n[ -f '{0}' ] && count=$(cat '{0}')\ncount=$((count+1)); printf '%s' \"$count\" > '{0}'\nid=${{3##*sha256:}}\n[ \"$count\" -ge 3 ] && id={1}\nprintf '[{{\"Id\":\"sha256:%s\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}]' \"$id\"\n",
        count.display(),
        "c".repeat(64)
    );
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, Some(&podman));
    let cohort = run_advisory_cohort(&context, &staging, &descriptor, &processes).unwrap();
    let sibling = staging.root.parent().unwrap().join("foreign");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("canary"), b"foreign").unwrap();
    let canary = tempfile::NamedTempFile::new().unwrap();
    fs::write(canary.path(), b"outside allowlist").unwrap();
    let error = match finalize_candidate(FinalizeInput {
        root: &repo.root,
        staging: &staging,
        context: &context,
        version: "1.0.0",
        deb: &deb,
        rpm: &rpm,
        cohort: &cohort,
        images: &images,
        engine: ContainerEngine::Podman,
        engine_identity: "podman version 5.8.3".into(),
        processes: &processes,
    }) {
        Ok(_) => panic!("post-promotion image drift was accepted"),
        Err(error) => error,
    };
    let error =
        finish_candidate_staging::<()>(&repo.root, "1.0.0", &staging.root, Err(error)).unwrap_err();
    assert!(error.to_string().contains("image"));
    assert!(!error.to_string().contains("candidate-proven"));
    assert!(!staging.root.exists());
    assert!(!repo.root.path().join("dist/rust").exists());
    assert!(!repo.root.path().join("dist/rust-evidence/1.0.0").exists());
    assert_eq!(fs::read(canary.path()).unwrap(), b"outside allowlist");
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
}

#[test]
fn finalize_candidate_rolls_back_post_promotion_ledger_write_failure() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let policy = ReleaseImages::from_context(&context).unwrap();
    let images = ResolvedImages {
        build_ubuntu: proof_image_identity(&policy.build_ubuntu),
        build_fedora: proof_image_identity(&policy.build_fedora),
        proof_debian: proof_image_identity(&policy.proof_debian),
        proof_rpm: proof_image_identity(&policy.proof_rpm),
        proof_tar: proof_image_identity(&policy.proof_tar),
    };
    let deb = finalized_lane_fixture(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        &images.build_ubuntu.digest,
    );
    let rpm = finalized_lane_fixture(
        &staging.rpm_lane,
        Lane::Rpm,
        &context,
        &images.build_fedora.digest,
    );
    let db = git_repo();
    let descriptor = descriptor(&staging.root, db.path(), None, &current_time());
    let image_script = "#!/bin/sh\nid=${3##*sha256:}\nprintf '[{\"Id\":\"sha256:%s\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}]' \"$id\"\n";
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, Some(image_script));
    let cohort = run_advisory_cohort(&context, &staging, &descriptor, &processes).unwrap();
    let evidence = repo.root.path().join("dist/rust-evidence/1.0.0");
    fs::create_dir_all(&evidence).unwrap();
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o555)).unwrap();
    let sibling = staging.root.parent().unwrap().join("foreign");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("canary"), b"foreign").unwrap();
    let result = finalize_candidate(FinalizeInput {
        root: &repo.root,
        staging: &staging,
        context: &context,
        version: "1.0.0",
        deb: &deb,
        rpm: &rpm,
        cohort: &cohort,
        images: &images,
        engine: ContainerEngine::Podman,
        engine_identity: "podman version 5.8.3".into(),
        processes: &processes,
    });
    let error = match result {
        Ok(_) => panic!("read-only ledger directory was accepted"),
        Err(error) => error,
    };
    let error =
        finish_candidate_staging::<()>(&repo.root, "1.0.0", &staging.root, Err(error)).unwrap_err();
    assert!(!error.to_string().contains("candidate-proven"));
    assert!(!repo.root.path().join("dist/rust").exists());
    assert!(!repo.root.path().join("dist/rust-evidence/1.0.0").exists());
    assert!(!staging.root.exists());
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
}

#[test]
fn finalize_candidate_rolls_back_promoted_classification_failure() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let policy = ReleaseImages::from_context(&context).unwrap();
    let images = ResolvedImages {
        build_ubuntu: proof_image_identity(&policy.build_ubuntu),
        build_fedora: proof_image_identity(&policy.build_fedora),
        proof_debian: proof_image_identity(&policy.proof_debian),
        proof_rpm: proof_image_identity(&policy.proof_rpm),
        proof_tar: proof_image_identity(&policy.proof_tar),
    };
    let deb = finalized_lane_fixture(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        &images.build_ubuntu.digest,
    );
    let rpm = finalized_lane_fixture(
        &staging.rpm_lane,
        Lane::Rpm,
        &context,
        &images.build_fedora.digest,
    );
    let db = git_repo();
    let descriptor = descriptor(&staging.root, db.path(), None, &current_time());
    let counter = tempfile::tempdir().unwrap();
    let count = counter.path().join("count");
    let podman = format!(
        "#!/bin/sh\ncount=0\n[ -f '{count}' ] && count=$(cat '{count}')\ncount=$((count+1)); printf '%s' \"$count\" > '{count}'\n[ \"$count\" = 1 ] && printf corrupt >> '{checksum}'\nid=${{3##*sha256:}}\nprintf '[{{\"Id\":\"sha256:%s\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}]' \"$id\"\n",
        count = count.display(),
        checksum = staging.payload.join(CHECKSUM_NAME).display(),
    );
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, Some(&podman));
    let cohort = run_advisory_cohort(&context, &staging, &descriptor, &processes).unwrap();
    let sibling = staging
        .root
        .parent()
        .unwrap()
        .join("classification-foreign");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("canary"), b"foreign").unwrap();
    let result = finalize_candidate(FinalizeInput {
        root: &repo.root,
        staging: &staging,
        context: &context,
        version: "1.0.0",
        deb: &deb,
        rpm: &rpm,
        cohort: &cohort,
        images: &images,
        engine: ContainerEngine::Podman,
        engine_identity: "podman version 5.8.3".into(),
        processes: &processes,
    });
    let error = match result {
        Ok(_) => panic!("promoted checksum corruption was accepted"),
        Err(error) => error,
    };
    let error =
        finish_candidate_staging::<()>(&repo.root, "1.0.0", &staging.root, Err(error)).unwrap_err();
    assert!(!error.to_string().contains("candidate-proven"));
    assert!(!repo.root.path().join("dist/rust").exists());
    assert!(!repo.root.path().join("dist/rust-evidence/1.0.0").exists());
    assert!(!staging.root.exists());
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
}

#[test]
fn production_finalizer_is_deterministic_for_fixed_lane_bytes_and_evidence() {
    let first = fixture();
    let second = fixture();
    assert_eq!(first.commit, second.commit);
    let first_lock = CandidateLock::acquire(&first.root).unwrap();
    let second_lock = CandidateLock::acquire(&second.root).unwrap();
    let first_staging = StagingLayout::create(&first.root, &first_lock).unwrap();
    let second_staging = StagingLayout::create(&second.root, &second_lock).unwrap();
    let first_context = export_immutable_context(&first.root, &first_staging.context).unwrap();
    let second_context = export_immutable_context(&second.root, &second_staging.context).unwrap();
    assert_eq!(first_context.commit, second_context.commit);
    assert_eq!(first_context.archive_sha256, second_context.archive_sha256);
    let policy = ReleaseImages::from_context(&first_context).unwrap();
    let images = ResolvedImages {
        build_ubuntu: proof_image_identity(&policy.build_ubuntu),
        build_fedora: proof_image_identity(&policy.build_fedora),
        proof_debian: proof_image_identity(&policy.proof_debian),
        proof_rpm: proof_image_identity(&policy.proof_rpm),
        proof_tar: proof_image_identity(&policy.proof_tar),
    };
    let templates = tempfile::tempdir().unwrap();
    let deb_template = templates.path().join("deb");
    let rpm_template = templates.path().join("rpm");
    fs::create_dir(&deb_template).unwrap();
    fs::create_dir(&rpm_template).unwrap();
    let mut deb_evidence = finalized_lane_fixture(
        &deb_template,
        Lane::Deb,
        &first_context,
        &images.build_ubuntu.digest,
    );
    let mut rpm_evidence = finalized_lane_fixture(
        &rpm_template,
        Lane::Rpm,
        &first_context,
        &images.build_fedora.digest,
    );
    let products = crate::tests::release_fixture();
    let tar = "solstone-linux-1.0.0-linux-x86_64.tar.gz";
    for (output, evidence, package) in [
        (
            &deb_template,
            &mut deb_evidence,
            "solstone-linux_1.0.0-1_amd64.deb",
        ),
        (
            &rpm_template,
            &mut rpm_evidence,
            "solstone-linux-1.0.0-1.x86_64.rpm",
        ),
    ] {
        for name in [tar, package] {
            fs::copy(products.path().join(name), output.join(name)).unwrap();
        }
        evidence.artifacts = [tar, package]
            .iter()
            .map(|name| artifact(&output.join(name)).unwrap())
            .collect();
        fs::write(
            output.join(LANE_EVIDENCE_NAME),
            serde_json::to_vec(evidence).unwrap(),
        )
        .unwrap();
    }
    let image_script = format!(
        r#"#!/bin/sh
if [ "$1" = image ] && [ "$2" = inspect ]; then
  id=${{3##*sha256:}}
  printf '[{{"Id":"sha256:%s","Os":"linux","Architecture":"amd64"}}]' "$id"
  exit 0
fi
output=; target=; previous=
for argument in "$@"; do
  [ "$previous" = --output ] && output=${{argument#type=local,dest=}}
  [ "$previous" = --target ] && target=$argument
  previous=$argument
done
case "$target" in
  deb) source='{deb}'; native='solstone-linux_1.0.0-1_amd64.deb' ;;
  rpm) source='{rpm}'; native='solstone-linux-1.0.0-1.x86_64.rpm' ;;
  *) exit 91 ;;
esac
/bin/cp "$source/{tar}" "$source/$native" "$output/"
/bin/cp "$source/{evidence}" "$output/{handoff}"
"#,
        deb = deb_template.display(),
        rpm = rpm_template.display(),
        tar = tar,
        evidence = LANE_EVIDENCE_NAME,
        handoff = LANE_HANDOFF,
    );
    let (_bin, processes) = process_bin(CARGO_DENY_ASSERTIONS, Some(&image_script));
    let db = git_repo();
    let descriptor = descriptor(&first_staging.root, db.path(), None, &current_time());
    let cohort =
        run_advisory_cohort(&first_context, &first_staging, &descriptor, &processes).unwrap();

    let finalize = |repo: &TestRepo, staging: &StagingLayout, context: &ImmutableContext| {
        let deb = build_lane(&lane_request(
            repo,
            context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &images.build_ubuntu,
            &images.build_fedora,
        ))
        .unwrap();
        let rpm = build_lane(&lane_request(
            repo,
            context,
            Lane::Rpm,
            &staging.rpm_lane,
            &processes,
            &images.build_ubuntu,
            &images.build_fedora,
        ))
        .unwrap();
        finalize_candidate(FinalizeInput {
            root: &repo.root,
            staging,
            context,
            version: "1.0.0",
            deb: &deb,
            rpm: &rpm,
            cohort: &cohort,
            images: &images,
            engine: ContainerEngine::Podman,
            engine_identity: "podman version 5.8.3".into(),
            processes: &processes,
        })
        .unwrap()
    };
    let first_final = finalize(&first, &first_staging, &first_context);
    let second_final = finalize(&second, &second_staging, &second_context);
    let inventory = |root: &Path| {
        fs::read_dir(root)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        inventory(&first_final.payload_root),
        inventory(&second_final.payload_root)
    );
    assert_eq!(first_final.ledger_bytes, second_final.ledger_bytes);
    assert_eq!(
        fs::read(first_final.payload_root.join(CHECKSUM_NAME)).unwrap(),
        fs::read(second_final.payload_root.join(CHECKSUM_NAME)).unwrap()
    );
    assert_eq!(
        fs::read(first_final.payload_root.join(manifest_name("1.0.0"))).unwrap(),
        fs::read(second_final.payload_root.join(manifest_name("1.0.0"))).unwrap()
    );
}

fn lane_handoff(
    root: &Path,
    lane: Lane,
    context: &ImmutableContext,
    invocation_id: &str,
    image_digest: &str,
    tar_bytes: &[u8],
    rustc_verbose: &str,
) -> LaneEvidence {
    let version = "1.0.0";
    let names = [
        format!("solstone-linux-{version}-linux-x86_64.tar.gz"),
        match lane {
            Lane::Deb => format!("solstone-linux_{version}-1_amd64.deb"),
            Lane::Rpm => format!("solstone-linux-{version}-1.x86_64.rpm"),
        },
    ];
    for (name, bytes) in [
        (names[0].clone(), tar_bytes),
        (names[1].clone(), b"package".as_slice()),
    ] {
        fs::write(root.join(name), bytes).unwrap();
    }
    let evidence = LaneEvidence {
        invocation_id: invocation_id.into(),
        lane,
        source_commit: context.commit.clone(),
        source_archive_sha256: context.archive_sha256.clone(),
        cargo_lock_sha256: context.cargo_lock_sha256.clone(),
        version: version.into(),
        target: TARGET_TRIPLE.into(),
        profile: "release".into(),
        features: Vec::new(),
        rustc_verbose: rustc_verbose.trim_end().into(),
        cargo: "cargo 1.97.1 (abcdef012 2026-06-30)".into(),
        baseline_executable_sha256: "d".repeat(64),
        image_digest: image_digest.into(),
        packaging_tool: match lane {
            Lane::Deb => "cargo-deb 3.7.0\n",
            Lane::Rpm => "cargo-generate-rpm 0.21.0\n",
        }
        .trim()
        .into(),
        native_tools: lane_tools(lane, image_digest),
        artifacts: names
            .iter()
            .map(|name| artifact(&root.join(name)).unwrap())
            .collect(),
    };
    fs::write(
        root.join(LANE_HANDOFF),
        serde_json::to_vec(&evidence).unwrap(),
    )
    .unwrap();
    evidence
}

fn lane_request<'a>(
    repo: &'a TestRepo,
    context: &'a ImmutableContext,
    lane: Lane,
    output: &'a Path,
    processes: &'a ProcessEnvironment,
    ubuntu: &'a ImageIdentity,
    fedora: &'a ImageIdentity,
) -> LaneRequest<'a> {
    LaneRequest {
        repo: &repo.root,
        context,
        lane,
        engine: ContainerEngine::Podman,
        invocation_id: "0123456789abcdef0123456789abcdef",
        version: "1.0.0",
        ubuntu,
        fedora,
        output,
        processes,
    }
}

#[test]
fn lane_build_argv_is_offline_no_pull_and_uses_exported_context() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let ubuntu = ImageIdentity {
        configured_reference:
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        digest: format!("sha256:{}", "a".repeat(64)),
    };
    let fedora = ImageIdentity {
        configured_reference:
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        digest: format!("sha256:{}", "b".repeat(64)),
    };
    lane_handoff(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        "0123456789abcdef0123456789abcdef",
        &ubuntu.digest,
        b"same tar",
        "rustc 1.97.1 (abcdef012 2026-06-30)\nbinary: rustc\ncommit-hash: 0123456789abcdef0123456789abcdef01234567\ncommit-date: 2026-06-30\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\nLLVM version: 18.1.0\n",
    );
    fs::write(
        repo.root.path().join("packaging/Containerfile"),
        b"LIVE CONTAINERFILE MUTATION",
    )
    .unwrap();
    let record = staging.root.join("podman-argv");
    let script = format!(
        r#"#!/bin/sh
printf '%s\0' "$@" > '{}'
pull=0
network=0
last=
file=
next_file=0
for arg in "$@"; do
  [ "$arg" = '--pull=never' ] && pull=1
  [ "$arg" = '--network=none' ] && network=1
  if [ "$next_file" -eq 1 ]; then file="$arg"; next_file=0; fi
  [ "$arg" = '--file' ] && next_file=1
  last="$arg"
done
[ "$pull" -eq 1 ] && [ "$network" -eq 1 ] || exit 90
[ "$last" = '{}' ] || exit 91
[ "$file" = '{}/packaging/Containerfile' ] || exit 92
grep -F 'LIVE CONTAINERFILE MUTATION' "$file" >/dev/null && exit 93
exit 0
"#,
        record.display(),
        context.path.display(),
        context.path.display()
    );
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&script));
    let request = lane_request(
        &repo,
        &context,
        Lane::Deb,
        &staging.deb_lane,
        &processes,
        &ubuntu,
        &fedora,
    );
    build_lane(&request).unwrap();
    let argv = fs::read(record).unwrap();
    assert!(
        argv.windows(b"--pull=never".len())
            .any(|window| window == b"--pull=never")
    );
    assert!(
        argv.windows(b"--network=none".len())
            .any(|window| window == b"--network=none")
    );
    for binding in [
        "INVOCATION_ID=0123456789abcdef0123456789abcdef".to_owned(),
        format!("SOURCE_COMMIT={}", context.commit),
        format!("SOURCE_ARCHIVE_SHA256={}", context.archive_sha256),
        format!("CARGO_LOCK_SHA256={}", context.cargo_lock_sha256),
        format!("UBUNTU_TOOL_BASE={}", ubuntu.digest),
        "RELEASE_VERSION=1.0.0".to_owned(),
    ] {
        assert!(
            argv.windows(binding.len())
                .any(|window| window == binding.as_bytes()),
            "missing build binding {binding}"
        );
    }
    let containerfile = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packaging/Containerfile"),
    )
    .unwrap();
    for consumed in [
        "--invocation-id \"$INVOCATION_ID\"",
        "--source-commit \"$SOURCE_COMMIT\"",
        "--source-archive-sha256 \"$SOURCE_ARCHIVE_SHA256\"",
        "--cargo-lock-sha256 \"$CARGO_LOCK_SHA256\"",
        "--image-digest \"$UBUNTU_TOOL_BASE\"",
    ] {
        assert!(containerfile.contains(consumed), "unconsumed {consumed}");
    }

    let mut live = context.clone();
    live.path = repo.root.path().to_owned();
    let tripwire = staging.root.join("tripwire");
    let script = format!(
        "#!/bin/sh\nprintf called > '{}'\nexit 0\n",
        tripwire.display()
    );
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&script));
    let request = lane_request(
        &repo,
        &live,
        Lane::Deb,
        &staging.deb_lane,
        &processes,
        &ubuntu,
        &fedora,
    );
    assert!(build_lane(&request).is_err());
    assert!(!tripwire.exists());
}

#[test]
fn lane_build_rejects_extra_output_and_unsafe_rustc_evidence() {
    for case in ["extra", "secret", "control"] {
        let repo = fixture();
        let lock = CandidateLock::acquire(&repo.root).unwrap();
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let ubuntu = ImageIdentity {
            configured_reference:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
        };
        let fedora = ImageIdentity {
            configured_reference:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let rustc = match case {
            "secret" => {
                "rustc 1.97.1 (secret 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n"
            }
            "control" => {
                "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\0"
            }
            _ => {
                "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n"
            }
        };
        lane_handoff(
            &staging.deb_lane,
            Lane::Deb,
            &context,
            "0123456789abcdef0123456789abcdef",
            &ubuntu.digest,
            b"tar",
            rustc,
        );
        if case == "extra" {
            fs::write(staging.deb_lane.join("unexpected-output"), b"canary").unwrap();
        }
        let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));
        let request = lane_request(
            &repo,
            &context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &ubuntu,
            &fedora,
        );
        assert!(build_lane(&request).is_err(), "accepted {case}");
    }
}

#[test]
fn lane_build_rejects_malformed_image_digests_before_subprocess() {
    for digest in [
        "sha256:abcd".to_owned(),
        format!("sha256:{}", "A".repeat(64)),
        "not-a-digest".to_owned(),
    ] {
        let repo = fixture();
        let lock = CandidateLock::acquire(&repo.root).unwrap();
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let ubuntu = ImageIdentity {
            configured_reference:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            digest,
        };
        let fedora = ImageIdentity {
            configured_reference:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let tripwire = staging.root.join("container-tripwire");
        let script = format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 0\n",
            tripwire.display()
        );
        let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some(&script));
        let request = lane_request(
            &repo,
            &context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &ubuntu,
            &fedora,
        );
        assert!(build_lane(&request).is_err());
        assert!(!tripwire.exists());
        assert!(fs::read_dir(&staging.deb_lane).unwrap().next().is_none());
    }
}

#[test]
fn production_handoff_rejects_missing_duplicate_unknown_and_byte_drift() {
    let fields = [
        "invocation_id",
        "lane",
        "source_commit",
        "source_archive_sha256",
        "cargo_lock_sha256",
        "version",
        "target",
        "profile",
        "features",
        "rustc_verbose",
        "cargo",
        "baseline_executable_sha256",
        "image_digest",
        "packaging_tool",
        "artifacts",
    ];
    for mutation in fields
        .iter()
        .map(|field| format!("missing:{field}"))
        .chain([
            "duplicate:key".into(),
            "duplicate:artifact".into(),
            "unknown:key".into(),
            "artifact:bytes".into(),
        ])
    {
        let repo = fixture();
        let lock = CandidateLock::acquire(&repo.root).unwrap();
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let ubuntu = ImageIdentity {
            configured_reference:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
        };
        let fedora = ImageIdentity {
            configured_reference:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let evidence = lane_handoff(
            &staging.deb_lane,
            Lane::Deb,
            &context,
            "0123456789abcdef0123456789abcdef",
            &ubuntu.digest,
            b"tar",
            "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n",
        );
        let handoff = staging.deb_lane.join(LANE_HANDOFF);
        if let Some(field) = mutation.strip_prefix("missing:") {
            let mut value = serde_json::to_value(&evidence).unwrap();
            value.as_object_mut().unwrap().remove(field);
            fs::write(&handoff, serde_json::to_vec(&value).unwrap()).unwrap();
        } else if mutation == "duplicate:key" {
            let original = fs::read_to_string(&handoff).unwrap();
            fs::write(
                &handoff,
                format!(
                    "{{\"invocation_id\":\"ffffffffffffffffffffffffffffffff\",{}",
                    &original[1..]
                ),
            )
            .unwrap();
        } else if mutation == "duplicate:artifact" {
            let mut value = serde_json::to_value(&evidence).unwrap();
            let duplicate = value["artifacts"][0].clone();
            value["artifacts"].as_array_mut().unwrap().push(duplicate);
            fs::write(&handoff, serde_json::to_vec(&value).unwrap()).unwrap();
        } else if mutation == "unknown:key" {
            let mut value = serde_json::to_value(&evidence).unwrap();
            value["unallowlisted"] = Value::Bool(true);
            fs::write(&handoff, serde_json::to_vec(&value).unwrap()).unwrap();
        } else {
            fs::write(
                staging
                    .deb_lane
                    .join("solstone-linux-1.0.0-linux-x86_64.tar.gz"),
                b"changed",
            )
            .unwrap();
        }
        let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));
        let request = lane_request(
            &repo,
            &context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &ubuntu,
            &fedora,
        );
        assert!(build_lane(&request).is_err(), "accepted {mutation}");
    }
}

#[test]
fn production_handoff_rejects_stale_swapped_and_crosswired_documents() {
    for mutation in ["invocation_id", "image_digest", "lane"] {
        let repo = fixture();
        let lock = CandidateLock::acquire(&repo.root).unwrap();
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let ubuntu = ImageIdentity {
            configured_reference:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
        };
        let fedora = ImageIdentity {
            configured_reference:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let evidence = lane_handoff(
            &staging.deb_lane,
            Lane::Deb,
            &context,
            "0123456789abcdef0123456789abcdef",
            &ubuntu.digest,
            b"tar",
            "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n",
        );
        let mut value = serde_json::to_value(evidence).unwrap();
        value[mutation] = match mutation {
            "invocation_id" => Value::String("fedcba9876543210fedcba9876543210".into()),
            "image_digest" => Value::String(format!("sha256:{}", "b".repeat(64))),
            "lane" => Value::String("rpm".into()),
            _ => unreachable!(),
        };
        fs::write(
            staging.deb_lane.join(LANE_HANDOFF),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));
        let request = lane_request(
            &repo,
            &context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &ubuntu,
            &fedora,
        );
        assert!(build_lane(&request).is_err(), "accepted {mutation}");
    }

    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let ubuntu = ImageIdentity {
        configured_reference:
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        digest: format!("sha256:{}", "a".repeat(64)),
    };
    let fedora = ImageIdentity {
        configured_reference:
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        digest: format!("sha256:{}", "b".repeat(64)),
    };
    let rustc =
        "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n";
    lane_handoff(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        "0123456789abcdef0123456789abcdef",
        &ubuntu.digest,
        b"tar",
        rustc,
    );
    lane_handoff(
        &staging.rpm_lane,
        Lane::Rpm,
        &context,
        "0123456789abcdef0123456789abcdef",
        &fedora.digest,
        b"tar",
        rustc,
    );
    let deb_document = fs::read(staging.deb_lane.join(LANE_HANDOFF)).unwrap();
    let rpm_document = fs::read(staging.rpm_lane.join(LANE_HANDOFF)).unwrap();
    fs::write(staging.deb_lane.join(LANE_HANDOFF), rpm_document).unwrap();
    fs::write(staging.rpm_lane.join(LANE_HANDOFF), deb_document).unwrap();
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));
    for (lane, output) in [
        (Lane::Deb, staging.deb_lane.as_path()),
        (Lane::Rpm, staging.rpm_lane.as_path()),
    ] {
        let request = lane_request(&repo, &context, lane, output, &processes, &ubuntu, &fedora);
        assert!(
            build_lane(&request).is_err(),
            "accepted cross-wired {lane:?}"
        );
    }
}

#[test]
fn production_handoff_rejects_every_native_identity_mutation() {
    for (lane, keys) in [
        (
            Lane::Deb,
            &[
                "cargo_deb",
                "dpkg_deb",
                "signing_mode",
                "ubuntu_cargo",
                "ubuntu_compiler",
                "ubuntu_glibc",
                "ubuntu_gzip",
                "ubuntu_image_digest",
                "ubuntu_linker",
                "ubuntu_os",
                "ubuntu_rustc",
                "ubuntu_tar",
            ][..],
        ),
        (
            Lane::Rpm,
            &[
                "cargo_generate_rpm",
                "fedora_image_digest",
                "fedora_os",
                "rpm",
                "signing_mode",
            ][..],
        ),
    ] {
        let repo = fixture();
        let lock = CandidateLock::acquire(&repo.root).unwrap();
        let staging = StagingLayout::create(&repo.root, &lock).unwrap();
        let context = export_immutable_context(&repo.root, &staging.context).unwrap();
        let ubuntu = ImageIdentity {
            configured_reference:
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
        };
        let fedora = ImageIdentity {
            configured_reference:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let output = match lane {
            Lane::Deb => &staging.deb_lane,
            Lane::Rpm => &staging.rpm_lane,
        };
        let image = match lane {
            Lane::Deb => &ubuntu.digest,
            Lane::Rpm => &fedora.digest,
        };
        let evidence = lane_handoff(
            output,
            lane,
            &context,
            "0123456789abcdef0123456789abcdef",
            image,
            b"tar",
            "rustc 1.97.1 (abcdef012 2026-06-30)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\n",
        );
        let base = serde_json::to_value(evidence).unwrap();
        let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));
        let request = lane_request(&repo, &context, lane, output, &processes, &ubuntu, &fedora);
        for key in keys {
            for mutation in ["missing", "duplicate", "unknown", "wrong"] {
                let mut value = base.clone();
                let bytes = match mutation {
                    "missing" => {
                        value["native_tools"].as_object_mut().unwrap().remove(*key);
                        serde_json::to_vec(&value).unwrap()
                    }
                    "duplicate" => {
                        let text = serde_json::to_string(&value).unwrap();
                        text.replacen(
                            "\"native_tools\":{",
                            &format!("\"native_tools\":{{\"{key}\":\"duplicate\","),
                            1,
                        )
                        .into_bytes()
                    }
                    "unknown" => {
                        value["native_tools"]["unallowlisted_identity"] =
                            Value::String("tool 1.0".into());
                        serde_json::to_vec(&value).unwrap()
                    }
                    "wrong" => {
                        value["native_tools"][*key] = Value::String(if key.ends_with("_digest") {
                            "c".repeat(64)
                        } else {
                            "wrong-tool 1.0".into()
                        });
                        serde_json::to_vec(&value).unwrap()
                    }
                    _ => unreachable!(),
                };
                fs::write(output.join(LANE_HANDOFF), bytes).unwrap();
                assert!(
                    build_lane(&request).is_err(),
                    "accepted {lane:?} {key} {mutation}"
                );
            }
        }
    }
}

#[test]
fn lane_evidence_rejects_stale_swapped_crosswired_and_tar_mismatch() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let ubuntu = ImageIdentity {
        configured_reference:
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        digest: format!("sha256:{}", "a".repeat(64)),
    };
    let fedora = ImageIdentity {
        configured_reference:
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        digest: format!("sha256:{}", "b".repeat(64)),
    };
    let invocation = "0123456789abcdef0123456789abcdef";
    let deb = lane_fixture(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        invocation,
        &ubuntu.digest,
        b"deb tar",
    );
    let rpm = lane_fixture(
        &staging.rpm_lane,
        Lane::Rpm,
        &context,
        invocation,
        &fedora.digest,
        b"rpm tar",
    );
    let (_bin, processes) = process_bin("#!/bin/sh\nexit 97\n", Some("#!/bin/sh\nexit 0\n"));

    for field in [
        "invocation_id",
        "source_commit",
        "source_archive_sha256",
        "cargo_lock_sha256",
        "image_digest",
        "lane",
    ] {
        let mut value = serde_json::to_value(&deb).unwrap();
        value[field] = match field {
            "lane" => Value::String("rpm".into()),
            _ => Value::String("f".repeat(64)),
        };
        let mutated: LaneEvidence = serde_json::from_value(value).unwrap();
        let request = lane_request(
            &repo,
            &context,
            Lane::Deb,
            &staging.deb_lane,
            &processes,
            &ubuntu,
            &fedora,
        );
        assert!(
            validate_lane_evidence(&mutated, &request).is_err(),
            "accepted {field}"
        );
    }
    fs::write(
        staging.deb_lane.join(LANE_EVIDENCE_NAME),
        serde_json::to_vec(&deb).unwrap(),
    )
    .unwrap();
    assert!(reconcile_lanes(&deb, &rpm, &staging.deb_lane, &staging.rpm_lane).is_err());
}

#[test]
fn manifest_tool_map_is_derived_from_two_lanes_and_host_identity() {
    let repo = fixture();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    let deb = lane_fixture(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        "0123456789abcdef0123456789abcdef",
        &format!("sha256:{}", "a".repeat(64)),
        b"tar",
    );
    let rpm = lane_fixture(
        &staging.rpm_lane,
        Lane::Rpm,
        &context,
        "0123456789abcdef0123456789abcdef",
        &format!("sha256:{}", "b".repeat(64)),
        b"tar",
    );
    let tools =
        assemble_manifest_native_tools(&repo.root, &deb, &rpm, "podman version 5.8.3".into())
            .unwrap();
    assert_eq!(tools.len(), 18);
    assert_eq!(tools["container_engine"], "podman version 5.8.3");
    assert_eq!(tools["manifest_validator"], env!("CARGO_PKG_VERSION"));
    assert_eq!(tools["ubuntu_image_digest"], "a".repeat(64));
    assert_eq!(tools["fedora_image_digest"], "b".repeat(64));
}
pub const TEST_EXCEPTIONS: [&str; 2] = ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"];
