// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use std::os::unix::fs::PermissionsExt;

const CANDIDATE_VECTOR: &str = "27e7dd62da4e0022b755f669dd00118a57715aaf088ff7f2a6c322951238494e";
const BUNDLE_VECTOR: &str = "cd214a005b2186a7eb25e9fd756561fb9c6e47e02004047c1cd5132106580a3e";

fn proof_ids() -> [&'static str; 3] {
    [PROOF_SPECS[0].id, PROOF_SPECS[1].id, PROOF_SPECS[2].id]
}

#[test]
fn failed_proof_attempt_removes_only_owned_attempt_and_publication() {
    let repo = crate::candidate_tests::fixture();
    let version = VersionComponent::new("1.0.0").unwrap();
    let proof = ProofId::new("debian-amd64").unwrap();
    let transaction = TransactionComponent::new("proof-cleanup-test").unwrap();
    let attempt_reserved = ReservedPath::ProofAttempt(version.clone(), proof.clone(), transaction);
    let published_reserved = ReservedPath::Proof(version, proof);
    let boundary = ReservedReleaseBoundary::new(&repo.root);
    fs::create_dir_all(repo.root.path().join("dist/rust-evidence/1.0.0/proofs")).unwrap();
    let attempt = boundary
        .resolve_for_create(attempt_reserved.clone(), ExpectedLeaf::Absent)
        .unwrap()
        .absolute;
    let published = boundary
        .resolve_for_create(published_reserved.clone(), ExpectedLeaf::Absent)
        .unwrap()
        .absolute;
    let foreign = repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs/foreign.tmp");
    fs::create_dir(&attempt).unwrap();
    fs::write(attempt.join("partial"), b"partial").unwrap();
    fs::write(&published, b"published").unwrap();
    fs::write(&foreign, b"foreign").unwrap();
    let attempt_identity = FileIdentity::from_metadata(&fs::symlink_metadata(&attempt).unwrap());
    let published_identity =
        FileIdentity::from_metadata(&fs::symlink_metadata(&published).unwrap());
    let error = finish_proof_attempt_cleanup(
        &repo.root,
        Error::new("primary"),
        attempt_reserved,
        attempt_identity,
        published_reserved,
        Some(published_identity),
    );
    assert_eq!(error.to_string(), "primary");
    assert!(!attempt.exists());
    assert!(!published.exists());
    assert_eq!(fs::read(foreign).unwrap(), b"foreign");
}

#[test]
fn candidate_schemas_are_digest_and_identity_pinned() {
    verify_candidate_schemas().unwrap();
    assert_eq!(digest(ledger_schema_bytes()), LEDGER_SCHEMA_SHA256);
    assert_eq!(digest(proof_schema_bytes()), PROOF_SCHEMA_SHA256);
    let ids = proof_ids().map(str::to_owned).to_vec();
    let proof_schema: Value = serde_json::from_slice(proof_schema_bytes()).unwrap();
    let ledger_schema: Value = serde_json::from_slice(ledger_schema_bytes()).unwrap();
    assert_eq!(
        proof_schema["properties"]["platform"]["enum"],
        serde_json::json!(ids)
    );
    assert_eq!(
        ledger_schema["properties"]["expected_proof_ids"]["const"],
        serde_json::json!(ids)
    );
    assert!(
        ledger_schema["required"]
            .as_array()
            .unwrap()
            .contains(&Value::String("baseline_executable".into()))
    );
    assert_eq!(
        ledger_schema["properties"]["baseline_executable"]["required"],
        serde_json::json!(["sha256", "bytes"])
    );
    assert_eq!(
        ledger_schema["properties"]["baseline_executable"]["additionalProperties"],
        false
    );
    assert_eq!(
        ledger_schema["properties"]["baseline_executable"]["properties"]["sha256"]["pattern"],
        "^[0-9a-f]{64}$"
    );
    assert_eq!(
        ledger_schema["properties"]["baseline_executable"]["properties"]["bytes"]["minimum"],
        1
    );
}

fn payload_vector() -> (tempfile::TempDir, Vec<Artifact>) {
    let temp = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("zeta", b"alpha\n".as_slice()),
        ("alpha", b"bravo".as_slice()),
        ("middle", b"charlie-data".as_slice()),
        ("beta", b"D".as_slice()),
        ("omega", b"echo echo".as_slice()),
    ] {
        fs::write(temp.path().join(name), bytes).unwrap();
    }
    let payload = ["zeta", "alpha", "middle", "beta", "omega"]
        .map(|name| artifact(&temp.path().join(name)).unwrap())
        .to_vec();
    (temp, payload)
}

#[test]
fn candidate_digest_has_exact_payload_line_formula() {
    let (_temp, payload) = payload_vector();
    let expected = concat!(
        "f144a6907dc4284d1f9fe6a7d9b9ff53c02c1d07ba68f24d413d7ff7f757a782  5  alpha\n",
        "3f39d5c348e5b79d06e842c114e6cc571583bbf44e4b0ebfda1a01ec05745d43  1  beta\n",
        "3a76f368f7ec3090e97437137f9fd6e8999fd4db5854adfff12248ebb005e521  12  middle\n",
        "51e04812dab5b72b9149da420a1528cb556d6795d6c5d7ce11ea94841313e595  9  omega\n",
        "b6a98d9ce9a2d9149288fa3df42d377c3e42737afdcdaf714e33c0a100b51060  6  zeta\n",
    );
    let stream = candidate_digest_input(&payload).unwrap();
    assert_eq!(stream, expected.as_bytes());
    assert_eq!(candidate_digest(&payload).unwrap(), CANDIDATE_VECTOR);
    let single_space = expected.replace("  ", " ");
    assert_ne!(digest(single_space.as_bytes()), CANDIDATE_VECTOR);

    let mut reversed = payload;
    reversed.reverse();
    assert_eq!(candidate_digest_input(&reversed).unwrap(), stream);
}

fn proof_map() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("tar-x86_64".into(), b"tar-proof\n".to_vec()),
        ("debian-amd64".into(), b"deb-proof\n".to_vec()),
        ("rpm-x86_64".into(), b"rpm-proof\n".to_vec()),
    ])
}

#[test]
fn bundle_digest_has_exact_compact_sorted_json_formula() {
    let candidate = "1".repeat(64);
    let ledger = b"{\"ledger\":\"fixed\"}\n";
    let proofs = proof_map();
    let expected = concat!(
        "{\"candidate_digest\":\"1111111111111111111111111111111111111111111111111111111111111111\",",
        "\"ledger_sha256\":\"4082c6007708fd77afd869d0624adc7d2c09dd6deb6505f7d5741cf7ed458524\",",
        "\"proofs\":{\"debian-amd64\":\"9689b99e6172887d286f44d6cbaef799b60d7687b0f8e0741494c7644e494412\",",
        "\"rpm-x86_64\":\"d1b8164dbc832ff48f7980309a7e5b505c92dca0108d8b6f030a61c1757ed93b\",",
        "\"tar-x86_64\":\"6eaea6b88a31cf0370426e36d9756aa6280c2d7ff1c8512203ab5be9ba82bb88\"}}",
    );
    let input = bundle_digest_input(&candidate, ledger, &proofs).unwrap();
    assert_eq!(input, expected.as_bytes());
    assert_eq!(
        bundle_digest(&candidate, ledger, &proofs).unwrap(),
        BUNDLE_VECTOR
    );

    let without_ledger = serde_json::json!({
        "candidate_digest": candidate,
        "proofs": serde_json::from_slice::<Value>(&input).unwrap()["proofs"].clone(),
    });
    assert_ne!(
        digest(serde_json::to_string(&without_ledger).unwrap().as_bytes()),
        BUNDLE_VECTOR
    );
}

fn proof_bindings() -> ProofBindings {
    ProofBindings {
        platform: "debian-amd64".into(),
        candidate_digest: "1".repeat(64),
        ledger_sha256: "2".repeat(64),
        source_commit: "3".repeat(40),
        cargo_lock_sha256: "4".repeat(64),
        artifact_basename: "solstone-linux_1.0.0-1_amd64.deb".into(),
        artifact_bytes: 123,
        artifact_sha256: "5".repeat(64),
        proof_image_digest: format!("sha256:{}", "6".repeat(64)),
        os_release: "Ubuntu 22.04.5 LTS".into(),
        package_manager_version: "dpkg 1.21.22".into(),
        install_command: vec!["dpkg".into(), "--install".into(), "package.deb".into()],
        install_exit_status: 0,
        version_command: vec!["/usr/bin/solstone-linux".into(), "--version".into()],
        version_exit_status: 0,
        executable_path: "/usr/bin/solstone-linux".into(),
        executable_mode: 0o755,
        executable_sha256: "7".repeat(64),
        version_output: "solstone-linux 1.0.0".into(),
        result: "pass".into(),
        policy_checked_at: "2026-07-20T12:00:00Z".into(),
        validation_time: "2026-07-20T14:00:00Z".into(),
    }
}

fn valid_proof() -> Value {
    serde_json::json!({
        "schema_version": 1,
        "platform": "debian-amd64",
        "candidate_digest": "1".repeat(64),
        "ledger_sha256": "2".repeat(64),
        "source_commit": "3".repeat(40),
        "cargo_lock_sha256": "4".repeat(64),
        "artifact_basename": "solstone-linux_1.0.0-1_amd64.deb",
        "artifact_bytes": 123,
        "artifact_sha256": "5".repeat(64),
        "proof_image_digest": format!("sha256:{}", "6".repeat(64)),
        "os_release": "Ubuntu 22.04.5 LTS",
        "package_manager_version": "dpkg 1.21.22",
        "install_command": ["dpkg", "--install", "package.deb"],
        "install_exit_status": 0,
        "version_command": ["/usr/bin/solstone-linux", "--version"],
        "version_exit_status": 0,
        "executable_path": "/usr/bin/solstone-linux",
        "executable_mode": 493,
        "executable_sha256": "7".repeat(64),
        "version_output": "solstone-linux 1.0.0",
        "result": "pass",
        "proof_time": "2026-07-20T13:00:00Z",
        "architecture": "amd64",
        "network": "none",
        "isolation": "fresh-container"
    })
}

fn mutated_value(field: &str) -> Value {
    match field {
        "platform" => Value::String("rpm-x86_64".into()),
        "candidate_digest" | "ledger_sha256" | "cargo_lock_sha256" | "artifact_sha256"
        | "executable_sha256" => Value::String("a".repeat(64)),
        "source_commit" => Value::String("a".repeat(40)),
        "artifact_basename" => Value::String("other.deb".into()),
        "artifact_bytes" => Value::from(124),
        "proof_image_digest" => Value::String(format!("sha256:{}", "a".repeat(64))),
        "os_release" => Value::String("debian 13".into()),
        "package_manager_version" => Value::String("apt 9.9".into()),
        "install_command" => serde_json::json!(["dpkg", "--unpack", "package.deb"]),
        "install_exit_status" | "version_exit_status" => Value::from(1),
        "version_command" => serde_json::json!(["/usr/bin/solstone-linux", "help"]),
        "executable_path" => Value::String("/usr/local/bin/solstone-linux".into()),
        "executable_mode" => Value::from(0o700),
        "version_output" => Value::String("solstone-linux 9.9.9".into()),
        "result" => Value::String("fail".into()),
        "proof_time" => Value::String("2026-07-20T11:59:59Z".into()),
        _ => unreachable!(),
    }
}

const BOUND_FIELDS: [&str; 21] = [
    "platform",
    "candidate_digest",
    "ledger_sha256",
    "source_commit",
    "cargo_lock_sha256",
    "artifact_basename",
    "artifact_bytes",
    "artifact_sha256",
    "proof_image_digest",
    "os_release",
    "package_manager_version",
    "install_command",
    "install_exit_status",
    "version_command",
    "version_exit_status",
    "executable_path",
    "executable_mode",
    "executable_sha256",
    "version_output",
    "result",
    "proof_time",
];

#[test]
fn proof_validator_rejects_each_mutated_binding() {
    let expected = proof_bindings();
    validate_candidate_proof(&valid_proof(), &expected).unwrap();
    for field in BOUND_FIELDS {
        let mut proof = valid_proof();
        proof[field] = mutated_value(field);
        assert!(
            validate_candidate_proof(&proof, &expected).is_err(),
            "accepted mutated {field}"
        );
    }

    let mut source = valid_proof();
    source["source_commit"] = Value::String("a".repeat(40));
    assert_eq!(
        source["candidate_digest"],
        valid_proof()["candidate_digest"]
    );
    assert!(validate_candidate_proof(&source, &expected).is_err());
    let mut lock = valid_proof();
    lock["cargo_lock_sha256"] = Value::String("a".repeat(64));
    assert_eq!(lock["candidate_digest"], valid_proof()["candidate_digest"]);
    assert!(validate_candidate_proof(&lock, &expected).is_err());
}

#[test]
fn proof_schema_rejects_each_missing_binding() {
    let expected = proof_bindings();
    for field in BOUND_FIELDS {
        let mut proof = valid_proof();
        proof.as_object_mut().unwrap().remove(field);
        assert!(
            validate_candidate_proof(&proof, &expected).is_err(),
            "accepted missing {field}"
        );
    }
}

#[test]
fn xxh64_matches_pinned_vectors_and_cargo_deny_layout() {
    assert_eq!(xxh64(0, b""), 0xef46_db37_51d8_e999);
    assert_eq!(xxh64(0, &[42]), 0x0a9e_dece_beb0_3ae4);
    assert_eq!(xxh64(0, b"Hello, world!\0"), 0x7b06_c531_ea43_e89f);
    let long = (0_u8..100).collect::<Vec<_>>();
    assert_eq!(xxh64(0, &long), 0x6ac1_e580_3216_6597);
    // This URL/directory pairing was observed from cargo-deny 0.20.2.
    let derived = advisory_db_directory("file://localhost.invalid/advisory-db").unwrap();
    assert_eq!(xxh64(0xca80_de71, b""), 0xecd8_91c6_6d7e_1845);
    assert_eq!(derived, "advisory-db-f8e6125c8c7da402");
}

#[test]
fn transaction_ids_are_lowercase_hex() {
    let first = transaction_id().unwrap();
    let second = transaction_id().unwrap();
    assert_eq!(first.len(), 32);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );
    assert_ne!(first, second);
}

#[test]
fn atomic_publish_failure_removes_only_the_owned_temporary() {
    let directory = tempfile::tempdir().unwrap();
    let owned = directory.path().join(".ledger.owned.tmp");
    let foreign = directory.path().join(".ledger.foreign.tmp");
    fs::write(&owned, b"owned").unwrap();
    fs::write(&foreign, b"foreign").unwrap();
    let error =
        finish_atomic_publish(&owned, Err(Error::new("synthetic publish failure"))).unwrap_err();
    assert!(error.to_string().contains("synthetic publish failure"));
    assert!(!owned.exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
}

#[test]
fn ledger_fsync_failure_uses_the_owned_publish_cleanup_invariant() {
    let directory = tempfile::tempdir().unwrap();
    let owned = directory.path().join(".ledger.fsync.tmp");
    let foreign = directory.path().join(".ledger.foreign.tmp");
    fs::write(&owned, b"owned").unwrap();
    fs::write(&foreign, b"foreign").unwrap();
    let error = finish_atomic_publish(&owned, Err(Error::new("ledger fsync failure"))).unwrap_err();
    assert!(error.to_string().contains("ledger fsync failure"));
    assert!(!owned.exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
}

#[test]
fn ledger_chmod_failure_uses_the_owned_publish_cleanup_invariant() {
    let directory = tempfile::tempdir().unwrap();
    let owned = directory.path().join(".ledger.chmod.tmp");
    let foreign = directory.path().join(".ledger.foreign.tmp");
    fs::write(&owned, b"owned").unwrap();
    fs::write(&foreign, b"foreign").unwrap();
    let error = finish_atomic_publish(&owned, Err(Error::new("ledger chmod failure"))).unwrap_err();
    assert!(error.to_string().contains("ledger chmod failure"));
    assert!(!owned.exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
}

#[test]
fn atomic_rename_failure_removes_owned_temp_and_preserves_foreign_temp() {
    let directory = tempfile::tempdir().unwrap();
    let owned = directory.path().join(".ledger.owned.tmp");
    let foreign = directory.path().join(".ledger.foreign.tmp");
    let destination = directory.path().join("ledger.json");
    fs::write(&owned, b"owned").unwrap();
    fs::write(&foreign, b"foreign").unwrap();
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep"), b"keep").unwrap();
    let publish = fs::rename(&owned, &destination).map_err(display_error);
    let error = finish_atomic_publish(&owned, publish).unwrap_err();
    assert!(!error.to_string().is_empty());
    assert!(!owned.exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(fs::read(destination.join("keep")).unwrap(), b"keep");
}

#[test]
fn atomic_temp_creation_failure_does_not_mutate_read_only_parent() {
    let directory = tempfile::tempdir().unwrap();
    let foreign = directory.path().join("foreign.tmp");
    fs::write(&foreign, b"foreign").unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = atomic_write_0644(&directory.path().join("ledger.json"), b"candidate\n");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert!(!directory.path().join("ledger.json").exists());
}

#[test]
fn post_rename_sync_failure_reclaims_owned_proof_and_preserves_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let proof = directory.path().join("proof.json");
    let result = atomic_write_0644_with_parent_sync(&proof, b"owned proof\n", |_| {
        Err(Error::new("injected parent fsync failure"))
    });
    assert!(result.is_err());
    assert!(!proof.exists());

    let result = atomic_write_0644_with_parent_sync(&proof, b"owned proof\n", |_| {
        fs::remove_file(&proof).unwrap();
        fs::write(&proof, b"foreign replacement\n").unwrap();
        Err(Error::new("injected parent fsync failure"))
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&proof).unwrap(), b"foreign replacement\n");
    assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn post_rename_metadata_failure_uses_precomputed_identity_and_preserves_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let proof = directory.path().join("proof.json");
    let result = atomic_write_0644_with_post_rename(
        &proof,
        b"owned proof\n",
        |_, _| Err(Error::new("injected post-rename metadata failure")),
        |_| Ok(()),
    );
    assert!(result.is_err());
    assert!(!proof.exists());

    let result = atomic_write_0644_with_post_rename(
        &proof,
        b"owned proof\n",
        |path, _| {
            fs::remove_file(path).unwrap();
            fs::write(path, b"foreign replacement\n").unwrap();
            Err(Error::new("injected post-rename metadata failure"))
        },
        |_| Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(fs::read(&proof).unwrap(), b"foreign replacement\n");
    assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn fixed_payload_and_ledger_serialization_are_reproducible() {
    let (_temp, mut payload) = payload_vector();
    payload.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let candidate = candidate_digest(&payload).unwrap();
    let ledger = CandidateLedger {
        schema_version: 1,
        product: PRODUCT.into(),
        version: "1.0.0".into(),
        source: LedgerSource {
            commit: "a".repeat(40),
            archive_sha256: "b".repeat(64),
            cargo_lock_sha256: "c".repeat(64),
        },
        validator: LedgerValidator {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        target: LedgerTarget {
            triple: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: Vec::new(),
        },
        policy: LedgerPolicy {
            cargo_deny_version: CARGO_DENY_VERSION.into(),
            deterministic_gate: "pass".into(),
            licenses_bans_sources: "pass".into(),
            advisories: "pass".into(),
            checked_at: "2026-07-20T12:00:00Z".into(),
            active_exceptions: vec!["RUSTSEC-2026-0194".into(), "RUSTSEC-2026-0195".into()],
        },
        advisory_cohort: LedgerAdvisory {
            source_id: "rustsec snapshot".into(),
            commit: "d".repeat(40),
            archive_sha256: "e".repeat(64),
            acquired_at: "2026-07-20T11:00:00Z".into(),
        },
        images: LedgerImages {
            engine: "podman".into(),
            engine_version: "podman version 5.8.3".into(),
            ubuntu_image_id: format!("sha256:{}", "f".repeat(64)),
            fedora_image_id: format!("sha256:{}", "1".repeat(64)),
        },
        tools: BTreeMap::new(),
        payload,
        package_members: Vec::new(),
        baseline_executable: ExecutableIdentity {
            sha256: "0".repeat(64),
            bytes: 1,
        },
        expected_proof_ids: proof_ids().map(str::to_owned).to_vec(),
        candidate_digest: candidate,
    };
    let first = canonical_json(&serde_json::to_value(&ledger).unwrap()).unwrap();
    let second = canonical_json(&serde_json::to_value(&ledger).unwrap()).unwrap();
    assert_eq!(first, second);
    assert_eq!(digest(&first), digest(&second));

    let mut foreign = serde_json::to_value(&ledger).unwrap();
    foreign.as_object_mut().unwrap().insert(
        "completion_time".into(),
        Value::String("2026-07-20T12:00:01Z".into()),
    );
    assert!(serde_json::from_value::<CandidateLedger>(foreign).is_err());
}

pub(super) struct RetainedFixture {
    pub(super) repo: crate::candidate_tests::TestRepo,
    advisory_db: tempfile::TempDir,
    _descriptor_dir: tempfile::TempDir,
    pub(super) descriptor: PathBuf,
    pub(super) ledger: CandidateLedger,
    pub(super) ledger_bytes: Vec<u8>,
}

pub(super) fn retained_fixture() -> RetainedFixture {
    retained_fixture_from(crate::candidate_tests::fixture())
}

fn retained_fixture_from(repo: crate::candidate_tests::TestRepo) -> RetainedFixture {
    retained_fixture_from_products(repo, crate::tests::release_fixture(), true)
}

fn retained_fixture_with(executables: [&[u8]; 3]) -> RetainedFixture {
    retained_fixture_from_products(
        crate::candidate_tests::fixture(),
        crate::tests::release_fixture_with(executables),
        false,
    )
}

fn retained_fixture_from_products(
    repo: crate::candidate_tests::TestRepo,
    products: tempfile::TempDir,
    validate: bool,
) -> RetainedFixture {
    let payload = repo.root.path().join("dist/rust");
    fs::create_dir_all(&payload).unwrap();
    for entry in fs::read_dir(products.path()).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), payload.join(entry.file_name())).unwrap();
    }
    crate::tests::write_rendered(crate::tests::evidence(), &payload).unwrap();

    let mut artifacts = fs::read_dir(&payload)
        .unwrap()
        .map(|entry| artifact(&entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut members = artifacts
        .iter()
        .filter(|item| item.path != CHECKSUM_NAME && !item.path.ends_with("-manifest.json"))
        .map(|item| package_member_evidence(&payload.join(&item.path), "1.0.0").unwrap())
        .collect::<Vec<_>>();
    members.sort_by(|left, right| {
        left.package_file
            .as_bytes()
            .cmp(right.package_file.as_bytes())
    });

    let advisory_db = crate::candidate_tests::git_repo();
    let advisory_commit = command(advisory_db.path(), &["git", "rev-parse", "HEAD"]).unwrap();
    let advisory_archive = command_bytes(
        advisory_db.path(),
        &["git", "archive", "--format=tar", "HEAD"],
    )
    .unwrap();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let descriptor_dir = tempfile::tempdir().unwrap();
    let descriptor = crate::candidate_tests::descriptor(
        descriptor_dir.path(),
        advisory_db.path(),
        None,
        "2026-01-01T00:00:00Z",
    );
    let policy = ReleaseImages::from_root(repo.root.path()).unwrap();
    let ubuntu = proof_image_identity(&policy.build_ubuntu).digest;
    let fedora = proof_image_identity(&policy.build_fedora).digest;
    let mut tools = crate::tests::tools();
    tools.insert("container_engine".into(), "podman version 5.8.3".into());
    tools.insert(
        "ubuntu_image_digest".into(),
        ubuntu.strip_prefix("sha256:").unwrap().into(),
    );
    tools.insert(
        "fedora_image_digest".into(),
        fedora.strip_prefix("sha256:").unwrap().into(),
    );
    let baseline_executable = ExecutableIdentity {
        sha256: digest(crate::tests::FIXTURE_EXECUTABLE_BYTES),
        bytes: crate::tests::FIXTURE_EXECUTABLE_BYTES.len() as u64,
    };
    let ledger = CandidateLedger {
        schema_version: 1,
        product: PRODUCT.into(),
        version: "1.0.0".into(),
        source: LedgerSource {
            commit: repo.commit.clone(),
            archive_sha256: digest(
                &command_bytes(
                    repo.root.path(),
                    &["git", "archive", "--format=tar", "HEAD"],
                )
                .unwrap(),
            ),
            cargo_lock_sha256: repo.cargo_lock_sha256.clone(),
        },
        validator: LedgerValidator {
            version: env!("CARGO_PKG_VERSION").into(),
        },
        target: LedgerTarget {
            triple: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: vec![],
        },
        policy: LedgerPolicy {
            cargo_deny_version: repo.cargo_deny_version.clone(),
            deterministic_gate: "pass".into(),
            licenses_bans_sources: "pass".into(),
            advisories: "pass".into(),
            checked_at: now.clone(),
            active_exceptions: repo.exceptions.clone(),
        },
        advisory_cohort: LedgerAdvisory {
            source_id: "rustsec snapshot 1".into(),
            commit: advisory_commit,
            archive_sha256: digest(&advisory_archive),
            acquired_at: "2026-01-01T00:00:00Z".into(),
        },
        images: LedgerImages {
            engine: "podman".into(),
            engine_version: "podman version 5.8.3".into(),
            ubuntu_image_id: ubuntu,
            fedora_image_id: fedora,
        },
        tools,
        payload: artifacts.clone(),
        package_members: members,
        baseline_executable,
        expected_proof_ids: proof_ids().map(str::to_owned).to_vec(),
        candidate_digest: candidate_digest(&artifacts).unwrap(),
    };
    let ledger_bytes = if validate {
        ledger_bytes(&repo.root, &payload, &ledger).unwrap()
    } else {
        canonical_json(&serde_json::to_value(&ledger).unwrap()).unwrap()
    };
    let evidence = repo.root.path().join("dist/rust-evidence/1.0.0");
    fs::create_dir_all(evidence.join("proofs")).unwrap();
    atomic_write_0644(&evidence.join("ledger.json"), &ledger_bytes).unwrap();
    for id in proof_ids() {
        write_valid_proof(&repo.root, &ledger, &ledger_bytes, id);
    }
    RetainedFixture {
        repo,
        advisory_db,
        _descriptor_dir: descriptor_dir,
        descriptor,
        ledger,
        ledger_bytes,
    }
}

const DIVERGENT_EXECUTABLE: &[u8] = b"plausible alternate executable";

fn divergent_executables(index: usize) -> [&'static [u8]; 3] {
    let mut executables = [crate::tests::FIXTURE_EXECUTABLE_BYTES; 3];
    executables[index] = DIVERGENT_EXECUTABLE;
    executables
}

fn creation_rejects_divergence(index: usize) {
    let fixture = retained_fixture_with(divergent_executables(index));
    let payload = fixture.repo.root.path().join("dist/rust");
    let lock = CandidateLock::acquire(&fixture.repo.root).unwrap();
    let staging = StagingLayout::create(&fixture.repo.root, &lock).unwrap();
    let context = export_immutable_context(&fixture.repo.root, &staging.context).unwrap();
    let descriptor_dir = tempfile::tempdir().unwrap();
    let descriptor = crate::candidate_tests::descriptor(
        descriptor_dir.path(),
        fixture.advisory_db.path(),
        None,
        &crate::candidate_tests::current_time(),
    );
    let (_bin, processes) =
        crate::candidate_tests::process_bin(crate::candidate_tests::CARGO_DENY_ASSERTIONS, None);
    let cohort = run_advisory_cohort(&context, &staging, &descriptor, &processes).unwrap();
    let policy = ReleaseImages::from_root(fixture.repo.root.path()).unwrap();
    let members = fixture.ledger.package_members.clone();
    let error = construct_ledger(LedgerInput {
        root: &fixture.repo.root,
        context: &context,
        version: "1.0.0",
        payload_root: &payload,
        package_members: members,
        baseline_executable: fixture.ledger.baseline_executable.clone(),
        cohort: &cohort,
        ubuntu: &proof_image_identity(&policy.build_ubuntu),
        fedora: &proof_image_identity(&policy.build_fedora),
        engine: ContainerEngine::Podman,
        engine_identity: "podman version 5.8.3".into(),
        tools: fixture.ledger.tools.clone(),
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("candidate executable identity mismatch")
    );
}

fn retained_rejects_divergence(index: usize) {
    let fixture = retained_fixture_with(divergent_executables(index));
    let payload = fixture.repo.root.path().join("dist/rust");
    let error = validate_ledger(&fixture.repo.root, &payload, &fixture.ledger).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("candidate executable identity mismatch")
    );
}

#[test]
fn creation_rejects_divergent_tar_executable() {
    creation_rejects_divergence(0);
}

#[test]
fn creation_rejects_divergent_deb_executable() {
    creation_rejects_divergence(1);
}

#[test]
fn creation_rejects_divergent_rpm_executable() {
    creation_rejects_divergence(2);
}

#[test]
fn retained_validation_rejects_divergent_tar_executable() {
    retained_rejects_divergence(0);
}

#[test]
fn retained_validation_rejects_divergent_deb_executable() {
    retained_rejects_divergence(1);
}

#[test]
fn retained_validation_rejects_divergent_rpm_executable() {
    retained_rejects_divergence(2);
}

#[test]
fn resume_rejects_each_divergent_package_executable() {
    for index in 0..3 {
        let fixture = retained_fixture_with(divergent_executables(index));
        assert!(
            prove_candidate(
                &fixture.repo.root,
                "1.0.0",
                &fixture.descriptor,
                &ProcessEnvironment::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("candidate executable identity mismatch")
        );
    }
}

#[test]
fn status_rejects_each_divergent_package_executable() {
    for index in 0..3 {
        let fixture = retained_fixture_with(divergent_executables(index));
        assert!(
            candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes,)
                .unwrap_err()
                .to_string()
                .contains("candidate executable identity mismatch")
        );
    }
}

#[test]
fn recovery_rejects_each_divergent_package_executable() {
    for index in 0..3 {
        let fixture = retained_fixture_with(divergent_executables(index));
        assert!(
            recover_candidate(&fixture.repo.root, "1.0.0")
                .unwrap_err()
                .to_string()
                .contains("candidate executable identity mismatch")
        );
    }
}

#[test]
fn ledger_rejects_baseline_digest_or_byte_count_drift() {
    for field in ["sha256", "bytes"] {
        let fixture = retained_fixture();
        let mut ledger = fixture.ledger;
        if field == "sha256" {
            ledger.baseline_executable.sha256 = "0".repeat(64);
        } else {
            ledger.baseline_executable.bytes += 1;
        }
        let payload = fixture.repo.root.path().join("dist/rust");
        assert!(
            validate_ledger(&fixture.repo.root, &payload, &ledger)
                .unwrap_err()
                .to_string()
                .contains("candidate executable baseline mismatch")
        );
    }
}

#[test]
fn lane_reconciliation_rejects_declared_baseline_not_matching_staged_tar() {
    let repo = crate::candidate_tests::fixture();
    let templates = tempfile::tempdir().unwrap();
    docker_create_templates(&repo.root, templates.path(), None);
    let mut deb: LaneEvidence =
        serde_json::from_slice(&fs::read(templates.path().join("deb-lane.json")).unwrap()).unwrap();
    let mut rpm: LaneEvidence =
        serde_json::from_slice(&fs::read(templates.path().join("rpm-lane.json")).unwrap()).unwrap();
    deb.baseline_executable_sha256 = "0".repeat(64);
    rpm.baseline_executable_sha256 = "0".repeat(64);
    assert!(
        reconcile_lanes(&deb, &rpm, templates.path(), templates.path(), "1.0.0",)
            .unwrap_err()
            .to_string()
            .contains("lane baseline executable mismatch")
    );
}

fn write_valid_proof(root: &RepoRoot, ledger: &CandidateLedger, ledger_bytes: &[u8], id: &str) {
    let policy = ReleaseImages::from_root(root.path()).unwrap();
    let platform = policy.proof_policy(id).unwrap();
    let artifact = proof_artifact(ledger, id).unwrap();
    let member = proof_member(ledger, id).unwrap();
    let proof = CandidateProof {
        schema_version: 1,
        platform: id.into(),
        candidate_digest: ledger.candidate_digest.clone(),
        ledger_sha256: digest(ledger_bytes),
        source_commit: ledger.source.commit.clone(),
        cargo_lock_sha256: ledger.source.cargo_lock_sha256.clone(),
        artifact_basename: artifact.path.clone(),
        artifact_bytes: artifact.bytes,
        artifact_sha256: artifact.sha256.clone(),
        proof_image_digest: platform.image_digest.clone(),
        os_release: platform.os_release.clone(),
        package_manager_version: platform.package_manager_version.clone(),
        install_command: platform.install_command.clone(),
        install_exit_status: 0,
        version_command: platform.version_command.clone(),
        version_exit_status: 0,
        executable_path: platform.executable_path.clone(),
        executable_mode: platform.executable_mode,
        executable_sha256: member.sha256.clone(),
        version_output: "solstone-linux 1.0.0".into(),
        result: "pass".into(),
        proof_time: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        architecture: if id == "debian-amd64" {
            "amd64"
        } else {
            "x86_64"
        }
        .into(),
        network: "none".into(),
        isolation: "fresh-container".into(),
        dry_run_passed: (id == "tar-x86_64").then_some(true),
        isolated_prefix_passed: (id == "tar-x86_64").then_some(true),
    };
    let bytes = canonical_json(&serde_json::to_value(proof).unwrap()).unwrap();
    atomic_write_0644(
        &root
            .path()
            .join("dist/rust-evidence/1.0.0/proofs")
            .join(format!("{id}.json")),
        &bytes,
    )
    .unwrap();
}

#[test]
fn candidate_status_requires_three_fully_valid_proofs() {
    let fixture = retained_fixture();
    let status =
        candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).unwrap();
    assert_eq!(status.status, "candidate-proven");
    assert!(status.local_evidence_only);
    assert!(!status.publication_approval);
    let proof = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs/tar-x86_64.json");
    let bytes = fs::read(&proof).unwrap();
    fs::remove_file(&proof).unwrap();
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err());
    fs::write(&proof, &bytes).unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["result"] = Value::String("fail".into());
    fs::write(&proof, canonical_json(&value).unwrap()).unwrap();
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err());
}

#[test]
fn bundle_digest_callsite_invariant_requires_validated_exact_inventory() {
    let fixture = retained_fixture();
    let status =
        candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).unwrap();
    assert_eq!(
        status.proofs.keys().cloned().collect::<Vec<_>>(),
        proof_ids()
    );
    assert_eq!(
        status.candidate_digest,
        candidate_digest(&fixture.ledger.payload).unwrap()
    );
    assert_eq!(status.ledger_sha256, digest(&fixture.ledger_bytes));
}

#[test]
fn promoted_package_member_reconciliation_is_stable_after_strict_classification() {
    let fixture = retained_fixture();
    validate_ledger(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &fixture.ledger,
    )
    .unwrap();
    let before = fixture.ledger.package_members.clone();
    validate_ledger(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &fixture.ledger,
    )
    .unwrap();
    assert_eq!(fixture.ledger.package_members, before);
}

#[test]
fn promoted_ledger_construction_has_no_unvalidated_fallible_input() {
    let fixture = retained_fixture();
    validate_ledger(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &fixture.ledger,
    )
    .unwrap();
    assert_eq!(
        candidate_digest(&fixture.ledger.payload).unwrap(),
        fixture.ledger.candidate_digest
    );
    assert_eq!(fixture.ledger.payload.len(), 5);
    assert_eq!(fixture.ledger.package_members.len(), 3);
}

#[test]
fn promoted_ledger_schema_and_privacy_precede_canonical_bytes() {
    let fixture = retained_fixture();
    let bytes = ledger_bytes(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &fixture.ledger,
    )
    .unwrap();
    assert_eq!(bytes, fixture.ledger_bytes);
    let parsed: CandidateLedger = serde_json::from_slice(&bytes).unwrap();
    validate_ledger(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &parsed,
    )
    .unwrap();
}

#[test]
fn status_and_recovery_reject_every_retained_binding_mutation() {
    let fixture = retained_fixture();
    assert_eq!(
        recover_candidate(&fixture.repo.root, "1.0.0").unwrap(),
        "retained-candidate-valid"
    );
    let fields = [
        "platform",
        "candidate_digest",
        "ledger_sha256",
        "source_commit",
        "cargo_lock_sha256",
        "artifact_basename",
        "artifact_bytes",
        "artifact_sha256",
        "proof_image_digest",
        "os_release",
        "package_manager_version",
        "install_command",
        "install_exit_status",
        "version_command",
        "version_exit_status",
        "executable_path",
        "executable_mode",
        "executable_sha256",
        "version_output",
        "result",
        "proof_time",
        "architecture",
        "network",
        "isolation",
    ];
    for id in proof_ids() {
        let path = fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0/proofs")
            .join(format!("{id}.json"));
        let original = fs::read(&path).unwrap();
        let applicable = fields
            .into_iter()
            .chain((id == "tar-x86_64").then_some("dry_run_passed"))
            .chain((id == "tar-x86_64").then_some("isolated_prefix_passed"));
        for field in applicable {
            let mut value: Value = serde_json::from_slice(&original).unwrap();
            let slot = value.get_mut(field).unwrap();
            *slot = match slot {
                Value::String(text) => Value::String(format!("{text}-skew")),
                Value::Number(number) => Value::Number((number.as_u64().unwrap() + 1).into()),
                Value::Array(argv) => {
                    let mut argv = argv.clone();
                    argv.push(Value::String("--skew".into()));
                    Value::Array(argv)
                }
                Value::Bool(value) => Value::Bool(!*value),
                other => panic!("unexpected mutation value {other:?}"),
            };
            fs::write(&path, canonical_json(&value).unwrap()).unwrap();
            assert!(
                candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes)
                    .is_err(),
                "status accepted {id} {field}"
            );
            assert!(
                recover_candidate(&fixture.repo.root, "1.0.0").is_err(),
                "recovery accepted {id} {field}"
            );
            let before = directory_digest_map(&fixture.repo.root.path().join("dist"));
            let tripwire_dir = tempfile::tempdir().unwrap();
            let tripwire = tripwire_dir.path().join("proof-run");
            let (_bin, processes) = proof_processes(&tripwire);
            assert!(
                prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &processes)
                    .is_err()
            );
            assert_eq!(
                directory_digest_map(&fixture.repo.root.path().join("dist")),
                before,
                "prove mutated retained state for {id} {field}"
            );
            assert!(!tripwire.exists(), "prove ran container for {id} {field}");
            fs::write(&path, &original).unwrap();
        }
    }

    let ledger_path = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/ledger.json");
    let original = fs::read(&ledger_path).unwrap();
    let mut ledger_pointers = vec![
        "/candidate_digest",
        "/images/engine",
        "/images/ubuntu_image_id",
        "/images/fedora_image_id",
        "/images/engine_version",
        "/target/triple",
        "/target/profile",
        "/target/features",
        "/source/commit",
        "/source/cargo_lock_sha256",
        "/source/archive_sha256",
        "/version",
        "/advisory_cohort/source_id",
        "/advisory_cohort/commit",
        "/advisory_cohort/archive_sha256",
        "/advisory_cohort/acquired_at",
        "/policy/checked_at",
        "/policy/deterministic_gate",
        "/policy/licenses_bans_sources",
        "/policy/advisories",
        "/policy/active_exceptions",
        "/expected_proof_ids",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    ledger_pointers.extend(TOOL_SPECS.iter().map(|spec| format!("/tools/{}", spec.key)));
    for index in 0..fixture.ledger.payload.len() {
        for field in ["path", "bytes", "sha256"] {
            ledger_pointers.push(format!("/payload/{index}/{field}"));
        }
    }
    for index in 0..fixture.ledger.package_members.len() {
        for field in [
            "package_file",
            "format",
            "installed_path",
            "mode",
            "bytes",
            "sha256",
        ] {
            ledger_pointers.push(format!("/package_members/{index}/{field}"));
        }
    }
    for pointer in ledger_pointers {
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        let slot = value.pointer_mut(&pointer).unwrap();
        *slot = match slot {
            Value::String(text) => Value::String(format!("{text}-skew")),
            Value::Array(values) => {
                let mut values = values.clone();
                values.push(Value::String("foreign".into()));
                Value::Array(values)
            }
            Value::Number(number) => Value::Number((number.as_u64().unwrap() + 1).into()),
            other => panic!("unexpected ledger mutation value {other:?}"),
        };
        let mutated = canonical_json(&value).unwrap();
        fs::write(&ledger_path, &mutated).unwrap();
        let parsed: CandidateLedger = serde_json::from_slice(&mutated).unwrap();
        assert!(
            candidate_status(&fixture.repo.root, &parsed, &mutated).is_err(),
            "status accepted ledger mutation {pointer}"
        );
        assert!(
            recover_candidate(&fixture.repo.root, "1.0.0").is_err(),
            "recovery accepted ledger mutation {pointer}"
        );
        fs::write(&ledger_path, &original).unwrap();
    }
    for pointer in ["/payload", "/package_members"] {
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        let values = value.pointer_mut(pointer).unwrap().as_array_mut().unwrap();
        values.push(values[0].clone());
        let mutated = canonical_json(&value).unwrap();
        fs::write(&ledger_path, &mutated).unwrap();
        let parsed: CandidateLedger = serde_json::from_slice(&mutated).unwrap();
        assert!(candidate_status(&fixture.repo.root, &parsed, &mutated).is_err());
        assert!(recover_candidate(&fixture.repo.root, "1.0.0").is_err());
        fs::write(&ledger_path, &original).unwrap();
    }
}

#[test]
fn recover_candidate_rejects_mismatched_checkout_without_writing() {
    let fixture = retained_fixture();
    let evidence = fixture.repo.root.path().join("dist/rust-evidence");
    let before = directory_digest_map(&evidence);
    fs::write(
        fixture.repo.root.path().join("Cargo.lock"),
        b"checkout skew\n",
    )
    .unwrap();
    assert!(recover_candidate(&fixture.repo.root, "1.0.0").is_err());
    assert_eq!(directory_digest_map(&evidence), before);
}

fn directory_digest_map(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(base: &Path, path: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                out.insert(
                    entry.path().strip_prefix(base).unwrap().into(),
                    "directory".into(),
                );
                walk(base, &entry.path(), out);
            } else {
                out.insert(
                    entry.path().strip_prefix(base).unwrap().into(),
                    digest(&fs::read(entry.path()).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

pub(super) fn proof_processes(tripwire: &Path) -> (tempfile::TempDir, ProcessEnvironment) {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("podman");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'podman version 5.8.3'; exit 0; fi\nif [ \"$1\" = image ] && [ \"$2\" = inspect ]; then id=${{3##*sha256:}}; printf '[{{\"Id\":\"sha256:%s\",\"Os\":\"linux\",\"Architecture\":\"amd64\"}}]' \"$id\"; exit 0; fi\nprintf called > '{}'\nexit 99\n",
            tripwire.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", temp.path().display());
    (
        temp,
        ProcessEnvironment::with_path(OsStr::new(path.as_str())),
    )
}

fn proof_output_processes(
    templates: &Path,
    fail_platform: Option<&str>,
    build_tripwire: &Path,
) -> (tempfile::TempDir, ProcessEnvironment) {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("podman");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'podman version 5.8.3'; exit 0; fi
if [ "$1" = image ] && [ "$2" = inspect ]; then id=${{3##*sha256:}}; printf '[{{"Id":"sha256:%s","Os":"linux","Architecture":"amd64"}}]' "$id"; exit 0; fi
if [ "$1" = build ] || [ "$1" = buildx ]; then printf called > '{tripwire}'; exit 98; fi
platform=
output=
previous=
for argument in "$@"; do
  [ "$previous" = --platform ] && platform=$argument
  case "$argument" in type=bind,src=*,dst=/evidence) output=${{argument#type=bind,src=}}; output=${{output%,dst=/evidence}};; esac
  previous=$argument
done
[ "$platform" = '{fail}' ] && exit 97
cp '{templates}/'$platform.json "$output/proof.json"
"#,
            tripwire = build_tripwire.display(),
            fail = fail_platform.unwrap_or("never"),
            templates = templates.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", temp.path().display());
    (
        temp,
        ProcessEnvironment::with_path(OsStr::new(path.as_str())),
    )
}

fn proof_output_processes_with_source_mutation(
    templates: &Path,
    cargo_lock: &Path,
    tripwire: &Path,
) -> (tempfile::TempDir, ProcessEnvironment) {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("podman");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'podman version 5.8.3'; exit 0; fi
if [ "$1" = image ] && [ "$2" = inspect ]; then id=${{3##*sha256:}}; printf '[{{"Id":"sha256:%s","Os":"linux","Architecture":"amd64"}}]' "$id"; exit 0; fi
if [ "$1" = build ] || [ "$1" = buildx ]; then printf called > '{tripwire}'; exit 98; fi
platform=; output=; previous=
for argument in "$@"; do
  [ "$previous" = --platform ] && platform=$argument
  case "$argument" in type=bind,src=*,dst=/evidence) output=${{argument#type=bind,src=}}; output=${{output%,dst=/evidence}};; esac
  previous=$argument
done
/bin/cp '{templates}/'$platform.json "$output/proof.json"
/usr/bin/printf 'mid-proof source change\n' > '{cargo_lock}'
"#,
            templates = templates.display(),
            cargo_lock = cargo_lock.display(),
            tripwire = tripwire.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", temp.path().display());
    (
        temp,
        ProcessEnvironment::with_path(OsStr::new(path.as_str())),
    )
}

#[test]
fn prove_candidate_validates_all_existing_proofs_before_any_write() {
    let fixture = retained_fixture();
    assert!(fixture.advisory_db.path().is_dir());
    let proofs = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs");
    let absent = proofs.join("debian-amd64.json");
    fs::remove_file(&absent).unwrap();
    let corrupt = proofs.join("tar-x86_64.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&corrupt).unwrap()).unwrap();
    value["os_release"] = Value::String("foreign platform".into());
    fs::write(&corrupt, canonical_json(&value).unwrap()).unwrap();
    let before = directory_digest_map(&fixture.repo.root.path().join("dist"));
    let tripwire_dir = tempfile::tempdir().unwrap();
    let tripwire = tripwire_dir.path().join("container-run");
    let (_bin, processes) = proof_processes(&tripwire);
    assert!(prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &processes).is_err());
    assert_eq!(
        directory_digest_map(&fixture.repo.root.path().join("dist")),
        before
    );
    assert!(!absent.exists());
    assert!(!tripwire.exists());
}

#[test]
fn prove_candidate_rejects_source_change_before_proof_writes() {
    let fixture = retained_fixture();
    let before = directory_digest_map(&fixture.repo.root.path().join("dist"));
    fs::write(
        fixture.repo.root.path().join("Cargo.lock"),
        b"source changed\n",
    )
    .unwrap();
    let tripwire_dir = tempfile::tempdir().unwrap();
    let tripwire = tripwire_dir.path().join("container-run");
    let (_bin, processes) = proof_processes(&tripwire);
    assert!(prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &processes).is_err());
    assert_eq!(
        directory_digest_map(&fixture.repo.root.path().join("dist")),
        before
    );
    assert!(!tripwire.exists());
}

#[test]
fn prove_candidate_rejects_source_change_during_first_proof_run() {
    let fixture = retained_fixture();
    let proofs = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs");
    let templates = tempfile::tempdir().unwrap();
    for id in proof_ids() {
        fs::copy(
            proofs.join(format!("{id}.json")),
            templates.path().join(format!("{id}.json")),
        )
        .unwrap();
    }
    fs::remove_file(proofs.join("debian-amd64.json")).unwrap();
    let payload_before = directory_digest_map(&fixture.repo.root.path().join("dist/rust"));
    let ledger_before = fixture.ledger_bytes.clone();
    let retained_before =
        ["rpm-x86_64", "tar-x86_64"].map(|id| fs::read(proofs.join(format!("{id}.json"))).unwrap());
    let tripwire_dir = tempfile::tempdir().unwrap();
    let tripwire = tripwire_dir.path().join("build");
    let (_bin, processes) = proof_output_processes_with_source_mutation(
        templates.path(),
        &fixture.repo.root.path().join("Cargo.lock"),
        &tripwire,
    );
    assert!(prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &processes).is_err());
    assert!(proofs.join("debian-amd64.json").exists());
    for (id, bytes) in ["rpm-x86_64", "tar-x86_64"]
        .into_iter()
        .zip(retained_before)
    {
        assert_eq!(fs::read(proofs.join(format!("{id}.json"))).unwrap(), bytes);
    }
    assert_eq!(
        directory_digest_map(&fixture.repo.root.path().join("dist/rust")),
        payload_before
    );
    assert_eq!(
        fs::read(
            fixture
                .repo
                .root
                .path()
                .join("dist/rust-evidence/1.0.0/ledger.json")
        )
        .unwrap(),
        ledger_before
    );
    assert!(!tripwire.exists());
}

#[test]
fn prove_resume_preserves_payload_ledger_and_first_proof_without_rebuild() {
    let fixture = retained_fixture();
    let proofs = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs");
    let templates = tempfile::tempdir().unwrap();
    for id in proof_ids() {
        fs::copy(
            proofs.join(format!("{id}.json")),
            templates.path().join(format!("{id}.json")),
        )
        .unwrap();
        fs::remove_file(proofs.join(format!("{id}.json"))).unwrap();
    }
    let payload_before = directory_digest_map(&fixture.repo.root.path().join("dist/rust"));
    let ledger_before = fixture.ledger_bytes.clone();
    let tripwire_dir = tempfile::tempdir().unwrap();
    let tripwire = tripwire_dir.path().join("build");
    let (_first_bin, first) =
        proof_output_processes(templates.path(), Some("rpm-x86_64"), &tripwire);
    assert!(prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &first).is_err());
    let first_proof = fs::read(proofs.join("debian-amd64.json")).unwrap();
    assert!(!proofs.join("rpm-x86_64.json").exists());
    let (_second_bin, second) = proof_output_processes(templates.path(), None, &tripwire);
    let status =
        prove_candidate(&fixture.repo.root, "1.0.0", &fixture.descriptor, &second).unwrap();
    assert_eq!(status.status, "candidate-proven");
    assert_eq!(
        fs::read(proofs.join("debian-amd64.json")).unwrap(),
        first_proof
    );
    assert_eq!(
        directory_digest_map(&fixture.repo.root.path().join("dist/rust")),
        payload_before
    );
    assert_eq!(
        fs::read(
            fixture
                .repo
                .root
                .path()
                .join("dist/rust-evidence/1.0.0/ledger.json")
        )
        .unwrap(),
        ledger_before
    );
    assert!(!tripwire.exists());
}

#[test]
fn candidate_status_and_recovery_reject_on_disk_ledger_replacement() {
    let fixture = retained_fixture();
    let path = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/ledger.json");
    let mut value: Value = serde_json::from_slice(&fixture.ledger_bytes).unwrap();
    value["advisory_cohort"]["source_id"] = Value::String("replacement cohort".into());
    fs::write(&path, canonical_json(&value).unwrap()).unwrap();
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err());
    assert!(recover_candidate(&fixture.repo.root, "1.0.0").is_err());
}

#[test]
fn create_readiness_ledger_replacement_rolls_back_promoted_candidate() {
    let fixture = retained_fixture();
    let path = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/ledger.json");
    let mut value: Value = serde_json::from_slice(&fixture.ledger_bytes).unwrap();
    value["advisory_cohort"]["source_id"] = Value::String("replacement cohort".into());
    fs::write(&path, canonical_json(&value).unwrap()).unwrap();
    let finalized = FinalizedCandidate::new(
        fixture.ledger.clone(),
        fixture.ledger_bytes.clone(),
        fixture.repo.root.path().join("dist/rust"),
        fixture.repo.root.path().join("dist/rust-evidence/1.0.0"),
    )
    .unwrap();
    let readiness = candidate_status(
        &fixture.repo.root,
        &finalized.ledger,
        &finalized.ledger_bytes,
    );
    let owned = proof_ids().map(|id| {
        finalized
            .evidence_root
            .join("proofs")
            .join(format!("{id}.json"))
    });
    assert!(finish_created_candidate(&finalized, &owned, readiness).is_err());
    assert!(!finalized.payload_root.exists());
    assert!(!finalized.evidence_root.exists());
}

#[test]
fn package_member_enumeration_propagates_entry_errors() {
    let error = package_members_from_paths(
        [Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "enumeration denied",
        ))],
        "1.0.0",
    )
    .unwrap_err();
    assert!(error.to_string().contains("enumeration denied"));
}

#[test]
fn candidate_staging_cleanup_is_owned_reported_and_sibling_safe() {
    let fixture = retained_fixture();
    let lock = CandidateLock::acquire(&fixture.repo.root).unwrap();
    let staging = StagingLayout::create(&fixture.repo.root, &lock).unwrap();
    let staging_parent = staging.root.parent().unwrap().to_owned();
    let sibling = staging_parent.join("foreign");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("canary"), b"foreign").unwrap();
    finish_candidate_staging_owned(&fixture.repo.root, &staging, Ok(())).unwrap();
    assert!(!staging.root.exists());
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");

    let staging = StagingLayout::create(&fixture.repo.root, &lock).unwrap();
    let finalized = FinalizedCandidate::new(
        fixture.ledger.clone(),
        fixture.ledger_bytes.clone(),
        fixture.repo.root.path().join("dist/rust"),
        fixture.repo.root.path().join("dist/rust-evidence/1.0.0"),
    )
    .unwrap();
    let primary =
        finish_created_candidate::<()>(&finalized, &[], Err(Error::new("primary failure")))
            .unwrap_err();
    let displaced = staging_parent.join("displaced");
    fs::rename(&staging.root, &displaced).unwrap();
    fs::write(&staging.root, b"foreign replacement").unwrap();
    let error = finish_candidate_staging_owned::<()>(&fixture.repo.root, &staging, Err(primary))
        .unwrap_err();
    assert!(error.to_string().contains("primary failure"));
    assert!(error.to_string().contains("repair: inspect only"));
    assert!(
        !error
            .to_string()
            .contains(fixture.repo.root.path().to_str().unwrap())
    );
    assert_eq!(fs::read(&staging.root).unwrap(), b"foreign replacement");
    assert!(displaced.is_dir());
    assert!(!fixture.repo.root.path().join("dist/rust").exists());
    assert!(
        !fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0")
            .exists()
    );
    assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
}

#[test]
fn docker_identity_is_normalized_and_retained_validation_is_provider_neutral() {
    let bin = tempfile::tempdir().unwrap();
    let docker = bin.path().join("docker");
    fs::write(
        &docker,
        "#!/bin/sh\necho 'Docker version 27.5.1, build fixture'\n",
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
    let processes = ProcessEnvironment::with_path(bin.path().as_os_str());
    assert_eq!(
        observe_container_engine(&processes, ContainerEngine::Docker).unwrap(),
        "docker 27.5.1"
    );

    let mut fixture = retained_fixture();
    fixture.ledger.images.engine = "docker".into();
    fixture.ledger.images.engine_version = "docker 27.5.1".into();
    fixture
        .ledger
        .tools
        .insert("container_engine".into(), "docker 27.5.1".into());
    fixture.ledger_bytes = ledger_bytes(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &fixture.ledger,
    )
    .unwrap();
    fs::write(
        fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0/ledger.json"),
        &fixture.ledger_bytes,
    )
    .unwrap();
    for id in proof_ids() {
        let path = fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0/proofs")
            .join(format!("{id}.json"));
        fs::remove_file(path).unwrap();
        write_valid_proof(
            &fixture.repo.root,
            &fixture.ledger,
            &fixture.ledger_bytes,
            id,
        );
    }
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_ok());
    assert_eq!(
        recover_candidate(&fixture.repo.root, "1.0.0").unwrap(),
        "retained-candidate-valid"
    );
    for invalid in ["Docker 27.5.1", "docker 27.5.2", "DOCKER 27.5.1"] {
        fixture.ledger.images.engine_version = invalid.into();
        fixture
            .ledger
            .tools
            .insert("container_engine".into(), invalid.into());
        assert!(
            candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err()
        );
    }
}

#[test]
fn git_commit_identity_accepts_sha1_and_sha256_only() {
    for valid in ["a".repeat(40), "b".repeat(64)] {
        require_commit(&valid, "commit").unwrap();
    }
    for invalid in [
        "a".repeat(39),
        "a".repeat(41),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(40),
        format!("{}g", "a".repeat(39)),
        format!("{}\n", "a".repeat(40)),
        format!(" {}", "a".repeat(40)),
    ] {
        assert!(
            require_commit(&invalid, "commit").is_err(),
            "accepted {invalid:?}"
        );
    }
    let fixture = retained_fixture();
    let mut ledger = fixture.ledger.clone();
    ledger.source.commit = "a".repeat(64);
    ledger.advisory_cohort.commit = "b".repeat(64);
    assert!(
        ledger_bytes(
            &fixture.repo.root,
            &fixture.repo.root.path().join("dist/rust"),
            &ledger
        )
        .is_ok()
    );
    let mut proof = valid_proof();
    proof["source_commit"] = Value::String("c".repeat(64));
    let mut expected = proof_bindings();
    expected.source_commit = "c".repeat(64);
    validate_candidate_proof(&proof, &expected).unwrap();
}

#[test]
fn sha256_git_fixture_flows_through_context_lane_status_and_recovery() {
    let repo = crate::candidate_tests::sha256_fixture();
    assert_eq!(repo.commit.len(), 64);
    require_expected_commit(&repo.root, &repo.commit).unwrap();
    let lock = CandidateLock::acquire(&repo.root).unwrap();
    let staging = StagingLayout::create(&repo.root, &lock).unwrap();
    let context = export_immutable_context(&repo.root, &staging.context).unwrap();
    assert_eq!(context.commit, repo.commit);
    let policy = ReleaseImages::from_context(&context).unwrap();
    let ubuntu = proof_image_identity(&policy.build_ubuntu);
    let fedora = proof_image_identity(&policy.build_fedora);
    let lane = crate::candidate_tests::lane_fixture(
        &staging.deb_lane,
        Lane::Deb,
        &context,
        "0123456789abcdef0123456789abcdef",
        &ubuntu.digest,
        b"same tar",
    );
    validate_lane_evidence(
        &lane,
        &LaneRequest {
            repo: &repo.root,
            context: &context,
            lane: Lane::Deb,
            engine: ContainerEngine::Podman,
            invocation_id: "0123456789abcdef0123456789abcdef",
            version: "1.0.0",
            ubuntu: &ubuntu,
            fedora: &fedora,
            output: &staging.deb_lane,
            processes: &ProcessEnvironment::default(),
        },
    )
    .unwrap();
    fs::remove_dir_all(&staging.root).unwrap();
    drop(lock);

    let retained = retained_fixture_from(repo);
    assert_eq!(retained.ledger.source.commit.len(), 64);
    assert!(
        candidate_status(
            &retained.repo.root,
            &retained.ledger,
            &retained.ledger_bytes
        )
        .is_ok()
    );
    assert_eq!(
        recover_candidate(&retained.repo.root, "1.0.0").unwrap(),
        "retained-candidate-valid"
    );
}

fn docker_create_templates(root: &RepoRoot, directory: &Path, executables: Option<[&[u8]; 3]>) {
    let products = executables.map_or_else(crate::tests::release_fixture, |values| {
        crate::tests::release_fixture_with(values)
    });
    for entry in fs::read_dir(products.path()).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), directory.join(entry.file_name())).unwrap();
    }
    let policy = ReleaseImages::from_root(root.path()).unwrap();
    let tar = directory.join("solstone-linux-1.0.0-linux-x86_64.tar.gz");
    let baseline = package_member_evidence(&tar, "1.0.0").unwrap();
    for (lane, image, name) in [
        (Lane::Deb, &policy.build_ubuntu, "deb-lane.json"),
        (Lane::Rpm, &policy.build_fedora, "rpm-lane.json"),
    ] {
        let native = match lane {
            Lane::Deb => "solstone-linux_1.0.0-1_amd64.deb",
            Lane::Rpm => "solstone-linux-1.0.0-1.x86_64.rpm",
        };
        let evidence = LaneEvidence {
            invocation_id: "@INVOCATION@".into(),
            lane,
            source_commit: "@SOURCE_COMMIT@".into(),
            source_archive_sha256: "@ARCHIVE@".into(),
            cargo_lock_sha256: "@LOCK@".into(),
            version: "1.0.0".into(),
            target: TARGET_TRIPLE.into(),
            profile: "release".into(),
            features: vec![],
            rustc_verbose: "rustc 1.97.1 (abcdef012 2026-06-30)\nbinary: rustc\ncommit-hash: abcdef0123456789abcdef0123456789abcdef01\ncommit-date: 2026-06-30\nhost: x86_64-unknown-linux-gnu\nrelease: 1.97.1\nLLVM version: 18.1.0".into(),
            cargo: "cargo 1.97.1 (abcdef012 2026-06-30)".into(),
            baseline_executable_sha256: baseline.sha256.clone(),
            image_digest: "@IMAGE@".into(),
            packaging_tool: match lane {
                Lane::Deb => "cargo-deb 3.7.0",
                Lane::Rpm => "cargo-generate-rpm 0.21.0",
            }
            .into(),
            native_tools: crate::candidate_tests::lane_tools(
                lane,
                &proof_image_identity(image).digest,
            ),
            artifacts: ["solstone-linux-1.0.0-linux-x86_64.tar.gz", native]
                .iter()
                .map(|path| artifact(&directory.join(path)).unwrap())
                .collect(),
        };
        fs::write(directory.join(name), serde_json::to_vec(&evidence).unwrap()).unwrap();
    }
    for id in proof_ids() {
        let platform = policy.proof_policy(id).unwrap();
        let artifact_name = match id {
            "debian-amd64" => "solstone-linux_1.0.0-1_amd64.deb",
            "rpm-x86_64" => "solstone-linux-1.0.0-1.x86_64.rpm",
            _ => "solstone-linux-1.0.0-linux-x86_64.tar.gz",
        };
        let artifact_record = artifact(&directory.join(artifact_name)).unwrap();
        let member = package_member_evidence(&directory.join(artifact_name), "1.0.0").unwrap();
        let proof = CandidateProof {
            schema_version: 1,
            platform: id.into(),
            candidate_digest: "@CANDIDATE@".into(),
            ledger_sha256: "@LEDGER@".into(),
            source_commit: "@SOURCE_COMMIT@".into(),
            cargo_lock_sha256: "@LOCK@".into(),
            artifact_basename: artifact_name.into(),
            artifact_bytes: artifact_record.bytes,
            artifact_sha256: artifact_record.sha256,
            proof_image_digest: "@IMAGE@".into(),
            os_release: platform.os_release.clone(),
            package_manager_version: platform.package_manager_version.clone(),
            install_command: platform.install_command.clone(),
            install_exit_status: 0,
            version_command: platform.version_command.clone(),
            version_exit_status: 0,
            executable_path: platform.executable_path.clone(),
            executable_mode: platform.executable_mode,
            executable_sha256: member.sha256,
            version_output: "solstone-linux 1.0.0".into(),
            result: "pass".into(),
            proof_time: "@PROOF_TIME@".into(),
            architecture: if id == "debian-amd64" {
                "amd64"
            } else {
                "x86_64"
            }
            .into(),
            network: "none".into(),
            isolation: "fresh-container".into(),
            dry_run_passed: (id == "tar-x86_64").then_some(true),
            isolated_prefix_passed: (id == "tar-x86_64").then_some(true),
        };
        fs::write(
            directory.join(format!("{id}.json")),
            canonical_json(&serde_json::to_value(proof).unwrap()).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn docker_create_candidate_is_offline_normalized_and_recoverable() {
    docker_create_candidate_harness(None);
}

fn docker_create_candidate_harness(divergent: Option<[&[u8]; 3]>) {
    let repo = crate::candidate_tests::fixture();
    let db = crate::candidate_tests::git_repo();
    let descriptor_dir = tempfile::tempdir().unwrap();
    let descriptor = crate::candidate_tests::descriptor(
        descriptor_dir.path(),
        db.path(),
        None,
        &crate::candidate_tests::current_time(),
    );
    let stubs = tempfile::tempdir().unwrap();
    let templates = tempfile::tempdir().unwrap();
    docker_create_templates(&repo.root, templates.path(), divergent);
    let fail_producer = stubs.path().join("fail-producer");
    let fail_validator = stubs.path().join("fail-validator");
    let mutate_source = stubs.path().join("mutate-source");
    let mutate_lock_digest = stubs.path().join("mutate-lock-digest");
    let mutate_cohort = stubs.path().join("mutate-cohort");
    fs::write(
        stubs.path().join("git"),
        "#!/bin/sh\nexec /usr/bin/git \"$@\"\n",
    )
    .unwrap();
    fs::write(stubs.path().join("cargo"), "#!/bin/sh\nexit 0\n").unwrap();
    let forbidden = stubs.path().join("forbidden");
    let docker = format!(
        r#"#!/bin/sh
case "$1" in
  --version) echo 'Docker version 27.5.1, build fixture'; exit 0;;
  login|logout|pull|tag|push) printf '%s' "$*" > '{forbidden}'; exit 90;;
esac
if [ "$1" = buildx ] && [ "$2" = version ]; then echo 'github.com/docker/buildx v0.20.0'; exit 0; fi
if [ "$1" = image ] && [ "$2" = inspect ]; then
  id=${{3##*sha256:}}
  printf '[{{"Id":"sha256:%s","Os":"linux","Architecture":"amd64"}}]' "$id"
  exit 0
fi
if [ "$1" = buildx ] && [ "$2" = build ]; then
  pull=0; network=0; output=; target=; invocation=; commit=; archive=; lock=; ubuntu=; fedora=
  previous=
  for argument in "$@"; do
    [ "$argument" = --pull=false ] && pull=1
    [ "$argument" = --network=none ] && network=1
    [ "$previous" = --output ] && output=${{argument#type=local,dest=}}
    [ "$previous" = --target ] && target=$argument
    case "$argument" in
      INVOCATION_ID=*) invocation=${{argument#*=}};; SOURCE_COMMIT=*) commit=${{argument#*=}};;
      SOURCE_ARCHIVE_SHA256=*) archive=${{argument#*=}};; CARGO_LOCK_SHA256=*) lock=${{argument#*=}};;
      UBUNTU_TOOL_BASE=*) ubuntu=${{argument#*=}};; FEDORA_TOOL_BASE=*) fedora=${{argument#*=}};;
    esac
    previous=$argument
  done
  [ "$pull" = 1 ] && [ "$network" = 1 ] || exit 91
  case "$target" in
    deb) native=solstone-linux_1.0.0-1_amd64.deb; template=deb-lane.json; image=$ubuntu;;
    rpm) native=solstone-linux-1.0.0-1.x86_64.rpm; template=rpm-lane.json; image=$fedora;;
    *) exit 92;;
  esac
  /bin/cp '{templates}/solstone-linux-1.0.0-linux-x86_64.tar.gz' "$output/"
  /bin/cp "{templates}/$native" "$output/"
  /bin/sed -e "s#@INVOCATION@#$invocation#g" -e "s#@SOURCE_COMMIT@#$commit#g" -e "s#@ARCHIVE@#$archive#g" -e "s#@LOCK@#$lock#g" -e "s#@IMAGE@#$image#g" "{templates}/$template" > "$output/.lane-evidence-handoff.json"
  exit 0
fi
if [ "$1" = run ]; then
  network=0; pull=0; output=; platform=; candidate=; ledger=; commit=; lock=; image=; previous=
  for argument in "$@"; do
    [ "$argument" = --network=none ] && network=1
    [ "$argument" = --pull=never ] && pull=1
    case "$argument" in type=bind,src=*,dst=/evidence) output=${{argument#type=bind,src=}}; output=${{output%,dst=/evidence}};; esac
    [ "$previous" = --platform ] && platform=$argument
    [ "$previous" = --candidate-digest ] && candidate=$argument
    [ "$previous" = --ledger-sha256 ] && ledger=$argument
    [ "$previous" = --source-commit ] && commit=$argument
    [ "$previous" = --cargo-lock-sha256 ] && lock=$argument
    [ "$previous" = --proof-image-digest ] && image=$argument
    previous=$argument
  done
  [ "$network" = 1 ] && [ "$pull" = 1 ] || exit 93
  [ -f '{fail_producer}' ] && [ "$(/bin/cat '{fail_producer}')" = "$platform" ] && exit 97
  proof_time=$(/bin/date -u +%Y-%m-%dT%H:%M:%SZ)
  /bin/sed -e "s#@CANDIDATE@#$candidate#g" -e "s#@LEDGER@#$ledger#g" -e "s#@SOURCE_COMMIT@#$commit#g" -e "s#@LOCK@#$lock#g" -e "s#@IMAGE@#$image#g" -e "s#@PROOF_TIME@#$proof_time#g" "{templates}/$platform.json" > "$output/proof.json"
  [ -f '{fail_validator}' ] && [ "$(/bin/cat '{fail_validator}')" = "$platform" ] && /usr/bin/printf ' ' >> "$output/proof.json"
  [ -f '{mutate_source}' ] && [ "$platform" = tar-x86_64 ] && /usr/bin/printf 'mid-create source drift\n' > Cargo.lock
  if [ -f '{mutate_lock_digest}' ] && [ "$platform" = tar-x86_64 ]; then
    /usr/bin/printf 'mid-create hidden lock drift\n' > Cargo.lock
    /usr/bin/git update-index --assume-unchanged Cargo.lock
  fi
  [ -f '{mutate_cohort}' ] && [ "$platform" = tar-x86_64 ] && /usr/bin/printf dirty > '{advisory_db}/DIRTY'
  exit 0
fi
printf '%s' "$*" > '{forbidden}'
exit 94
"#,
        forbidden = forbidden.display(),
        templates = templates.path().display(),
        fail_producer = fail_producer.display(),
        fail_validator = fail_validator.display(),
        mutate_source = mutate_source.display(),
        mutate_lock_digest = mutate_lock_digest.display(),
        mutate_cohort = mutate_cohort.display(),
        advisory_db = db.path().display(),
    );
    fs::write(stubs.path().join("docker"), docker).unwrap();
    for name in ["git", "cargo", "docker"] {
        fs::set_permissions(stubs.path().join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let processes = ProcessEnvironment::with_path(stubs.path().as_os_str());
    if divergent.is_some() {
        let error = create_candidate(&repo.root, &repo.commit, &descriptor, &processes)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("candidate executable identity mismatch"),
            "unexpected creation error: {error}"
        );
        assert!(!error.contains("candidate-proven"));
        return;
    }
    for (marker, class) in [(&fail_producer, "producer"), (&fail_validator, "validator")] {
        for id in proof_ids() {
            fs::write(marker, id).unwrap();
            let failed = crate::candidate_tests::fixture();
            let descriptor_dir = tempfile::tempdir().unwrap();
            let failed_descriptor = crate::candidate_tests::descriptor(
                descriptor_dir.path(),
                db.path(),
                None,
                &crate::candidate_tests::current_time(),
            );
            let sibling = failed
                .root
                .path()
                .join("dist/.rust-release-candidate-staging/foreign");
            fs::create_dir_all(&sibling).unwrap();
            fs::write(sibling.join("canary"), b"foreign").unwrap();
            let error =
                create_candidate(&failed.root, &failed.commit, &failed_descriptor, &processes)
                    .unwrap_err();
            assert!(!error.to_string().contains("candidate-proven"));
            assert!(
                !failed.root.path().join("dist/rust").exists(),
                "{class} {id}"
            );
            assert!(
                !failed.root.path().join("dist/rust-evidence/1.0.0").exists(),
                "{class} {id}"
            );
            assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
            let owned = fs::read_dir(sibling.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            assert_eq!(owned, vec![std::ffi::OsString::from("foreign")]);
        }
        fs::remove_file(marker).unwrap();
    }
    for (marker, class) in [
        (&mutate_source, "source"),
        (&mutate_lock_digest, "lock digest"),
        (&mutate_cohort, "cohort"),
    ] {
        fs::write(marker, b"enabled").unwrap();
        let failed = crate::candidate_tests::fixture();
        let descriptor_dir = tempfile::tempdir().unwrap();
        let failed_descriptor = crate::candidate_tests::descriptor(
            descriptor_dir.path(),
            db.path(),
            None,
            &crate::candidate_tests::current_time(),
        );
        let sibling = failed
            .root
            .path()
            .join("dist/.rust-release-candidate-staging/foreign");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("canary"), b"foreign").unwrap();
        let error = create_candidate(&failed.root, &failed.commit, &failed_descriptor, &processes)
            .unwrap_err();
        assert!(!error.to_string().contains("candidate-proven"));
        assert!(!failed.root.path().join("dist/rust").exists(), "{class}");
        assert!(
            !failed.root.path().join("dist/rust-evidence/1.0.0").exists(),
            "{class}"
        );
        assert_eq!(fs::read(sibling.join("canary")).unwrap(), b"foreign");
        let staging_inventory = fs::read_dir(sibling.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(staging_inventory, vec![std::ffi::OsString::from("foreign")]);
        fs::remove_file(marker).unwrap();
        if class == "cohort" {
            fs::remove_file(db.path().join("DIRTY")).unwrap();
        }
    }
    let status = create_candidate(&repo.root, &repo.commit, &descriptor, &processes).unwrap();
    assert_eq!(status.status, "candidate-proven");
    assert!(!forbidden.exists());
    let (ledger, bytes) = read_ledger(&repo.root, "1.0.0").unwrap();
    assert_eq!(ledger.images.engine, "docker");
    assert_eq!(ledger.images.engine_version, "docker 27.5.1");
    assert_eq!(ledger.tools["container_engine"], "docker 27.5.1");
    assert!(candidate_status(&repo.root, &ledger, &bytes).is_ok());
    assert_eq!(
        recover_candidate(&repo.root, "1.0.0").unwrap(),
        "retained-candidate-valid"
    );
}

#[test]
fn production_creation_rejects_divergent_tar_executable() {
    docker_create_candidate_harness(Some(divergent_executables(0)));
}

#[test]
fn production_creation_rejects_divergent_deb_executable() {
    docker_create_candidate_harness(Some(divergent_executables(1)));
}

#[test]
fn production_creation_rejects_divergent_rpm_executable() {
    docker_create_candidate_harness(Some(divergent_executables(2)));
}
