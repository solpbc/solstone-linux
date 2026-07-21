// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use std::cell::Cell;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;

fn payload_cleanup() -> (
    candidate_tests::TestRepo,
    CleanupPlan,
    PathBuf,
    FileIdentity,
) {
    let repo = candidate_tests::fixture();
    let payload = repo.root.path().join("dist/rust");
    fs::create_dir_all(&payload).unwrap();
    fs::write(payload.join("owned"), b"owned").unwrap();
    let identity = FileIdentity::from_metadata(&fs::symlink_metadata(&payload).unwrap());
    let plan = CleanupPlan::new(vec![CleanupEntry {
        path: ReservedPath::Payload,
        expected_type: ExpectedLeaf::Directory,
        expected_identity: identity,
        ownership: OwnershipEvidence::Created,
    }])
    .unwrap();
    (repo, plan, payload, identity)
}

#[test]
fn reserved_path_catalog_covers_every_variant_and_phase() {
    let version = VersionComponent::new("1.0.0").unwrap();
    let transaction = TransactionComponent::new("0123456789abcdef").unwrap();
    let cases = ReservedPath::test_cases(version, transaction);
    assert_eq!(cases.len(), 18);
    let names = cases
        .iter()
        .map(|case| case.path.relative())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), cases.len());
    assert!(cases.iter().any(|case| {
        case.path.relative() == Path::new("dist/rust") && case.expected == ExpectedLeaf::Directory
    }));
    assert!(cases.iter().any(|case| {
        case.path.relative() == Path::new("dist/.rust-release-candidate.lock")
            && case.expected == ExpectedLeaf::RegularFile
    }));
}

#[test]
fn cleanup_final_barrier_rejects_symlink_swap() {
    let (repo, plan, payload, _) = payload_cleanup();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"external").unwrap();
    let displaced = repo.root.path().join("dist/displaced");
    let error = plan
        .preflight(ReservedReleaseBoundary::new(&repo.root))
        .unwrap()
        .execute_with(
            |_, _| {
                fs::rename(&payload, &displaced).unwrap();
                symlink(external.path(), &payload).unwrap();
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("cleanup begun: false"));
    assert_eq!(fs::read(sentinel).unwrap(), b"external");
    assert!(displaced.join("owned").is_file());
}

#[test]
fn cleanup_final_barrier_rejects_directory_swap() {
    let (repo, plan, payload, _) = payload_cleanup();
    let displaced = repo.root.path().join("dist/displaced");
    let error = plan
        .preflight(ReservedReleaseBoundary::new(&repo.root))
        .unwrap()
        .execute_with(
            |_, _| {
                fs::rename(&payload, &displaced).unwrap();
                fs::create_dir(&payload).unwrap();
                fs::write(payload.join("foreign"), b"foreign").unwrap();
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("owned identity"));
    assert_eq!(fs::read(payload.join("foreign")).unwrap(), b"foreign");
    assert!(displaced.join("owned").is_file());
}

#[test]
fn cleanup_final_barrier_rejects_regular_file_swap() {
    let (repo, plan, payload, _) = payload_cleanup();
    let displaced = repo.root.path().join("dist/displaced");
    let error = plan
        .preflight(ReservedReleaseBoundary::new(&repo.root))
        .unwrap()
        .execute_with(
            |_, _| {
                fs::rename(&payload, &displaced).unwrap();
                fs::write(&payload, b"foreign").unwrap();
                Ok(())
            },
            |_, _| Ok(()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("wrong type"));
    assert_eq!(fs::read(payload).unwrap(), b"foreign");
}

#[test]
fn cleanup_final_barrier_rejects_foreign_same_name_swap() {
    cleanup_final_barrier_rejects_directory_swap();
}

#[test]
fn cleanup_quarantine_identity_mismatch_is_preserved() {
    let (repo, plan, payload, _) = payload_cleanup();
    let preserved = repo.root.path().join("dist/preserved-quarantine");
    let error = plan
        .preflight(ReservedReleaseBoundary::new(&repo.root))
        .unwrap()
        .execute_with(
            |_, _| Ok(()),
            |_, quarantine| {
                fs::rename(quarantine, &preserved).unwrap();
                fs::create_dir(quarantine).unwrap();
                fs::write(quarantine.join("foreign"), b"foreign").unwrap();
                Ok(())
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("owned identity"));
    assert_eq!(fs::read(preserved.join("owned")).unwrap(), b"owned");
    assert_eq!(
        fs::read(
            repo.root
                .path()
                .join("dist")
                .read_dir()
                .unwrap()
                .find_map(|entry| {
                    let path = entry.unwrap().path();
                    path.join("foreign").is_file().then(|| path.join("foreign"))
                })
                .unwrap()
        )
        .unwrap(),
        b"foreign"
    );
    assert!(!payload.exists());
}

#[test]
fn cleanup_rejects_fifo_leaf_without_blocking() {
    let repo = candidate_tests::fixture();
    fs::create_dir(repo.root.path().join("dist")).unwrap();
    let lock = repo.root.path().join("dist/.rust-release-candidate.lock");
    assert!(
        Command::new("mkfifo")
            .arg(&lock)
            .status()
            .unwrap()
            .success()
    );
    let error = ReservedReleaseBoundary::new(&repo.root)
        .resolve_for_read(ReservedPath::Lock, ExpectedLeaf::RegularFile)
        .unwrap_err();
    assert!(error.to_string().contains("wrong type"));
}

#[test]
fn cleanup_rejects_unix_socket_leaf_without_connecting() {
    let repo = candidate_tests::fixture();
    fs::create_dir(repo.root.path().join("dist")).unwrap();
    let lock = repo.root.path().join("dist/.rust-release-candidate.lock");
    let _listener = UnixListener::bind(&lock).unwrap();
    let error = ReservedReleaseBoundary::new(&repo.root)
        .resolve_for_read(ReservedPath::Lock, ExpectedLeaf::RegularFile)
        .unwrap_err();
    assert!(error.to_string().contains("wrong type"));
}

#[test]
fn cleanup_does_not_modify_external_hardlink_sentinel() {
    let repo = candidate_tests::fixture();
    fs::create_dir(repo.root.path().join("dist")).unwrap();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"external").unwrap();
    let lock = repo.root.path().join("dist/.rust-release-candidate.lock");
    fs::hard_link(&sentinel, &lock).unwrap();
    let identity = FileIdentity::from_metadata(&fs::symlink_metadata(&lock).unwrap());
    CleanupPlan::new(vec![CleanupEntry {
        path: ReservedPath::Lock,
        expected_type: ExpectedLeaf::RegularFile,
        expected_identity: identity,
        ownership: OwnershipEvidence::Created,
    }])
    .unwrap()
    .preflight(ReservedReleaseBoundary::new(&repo.root))
    .unwrap()
    .execute()
    .unwrap();
    assert_eq!(fs::read(sentinel).unwrap(), b"external");
}

#[test]
fn cleanup_mid_failure_reports_attempted_deleted_preserved_and_residual() {
    let (repo, _, payload, payload_identity) = payload_cleanup();
    let version = VersionComponent::new("1.0.0").unwrap();
    let evidence = repo.root.path().join("dist/rust-evidence/1.0.0");
    fs::create_dir_all(&evidence).unwrap();
    let evidence_identity = FileIdentity::from_metadata(&fs::symlink_metadata(&evidence).unwrap());
    let plan = CleanupPlan::new(vec![
        CleanupEntry {
            path: ReservedPath::Payload,
            expected_type: ExpectedLeaf::Directory,
            expected_identity: payload_identity,
            ownership: OwnershipEvidence::Created,
        },
        CleanupEntry {
            path: ReservedPath::EvidenceVersion(version),
            expected_type: ExpectedLeaf::Directory,
            expected_identity: evidence_identity,
            ownership: OwnershipEvidence::Created,
        },
    ])
    .unwrap();
    let barriers = Cell::new(0);
    let error = plan
        .preflight(ReservedReleaseBoundary::new(&repo.root))
        .unwrap()
        .execute_with(
            |_, _| {
                barriers.set(barriers.get() + 1);
                if barriers.get() == 2 {
                    Err(Error::new("controlled cleanup I/O failure"))
                } else {
                    Ok(())
                }
            },
            |_, _| Ok(()),
        )
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("controlled cleanup I/O failure\n"));
    assert!(error.contains("attempted=[dist/rust,dist/rust-evidence/1.0.0]"));
    assert!(error.contains("deleted=[dist/rust]"));
    assert!(error.contains("residual=[dist/rust-evidence/1.0.0]"));
    assert!(!payload.exists());
    assert!(evidence.is_dir());
}

#[test]
fn cleanup_rejects_plausible_invalid_payload_and_evidence() {
    let repo = candidate_tests::fixture();
    let payload = repo.root.path().join("dist/rust");
    let evidence = repo.root.path().join("dist/rust-evidence/1.0.0");
    fs::create_dir_all(&payload).unwrap();
    fs::create_dir_all(&evidence).unwrap();
    fs::write(payload.join("plausible"), b"foreign payload").unwrap();
    fs::write(evidence.join("ledger.json"), b"{}").unwrap();
    let processes = ProcessEnvironment::default();
    let error = create_candidate(
        &repo.root,
        &repo.commit,
        Path::new("unused-advisory-descriptor"),
        &processes,
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("existing release candidate ownership mismatch:"));
    assert_eq!(
        fs::read(payload.join("plausible")).unwrap(),
        b"foreign payload"
    );
    assert_eq!(fs::read(evidence.join("ledger.json")).unwrap(), b"{}");
}

#[test]
fn production_create_rejects_symlinked_cleanup_ancestor_without_external_mutation() {
    let repo = candidate_tests::fixture();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"external").unwrap();
    fs::create_dir(repo.root.path().join("dist")).unwrap();
    symlink(external.path(), repo.root.path().join("dist/rust")).unwrap();
    fs::create_dir_all(repo.root.path().join("dist/rust-evidence/1.0.0")).unwrap();
    let error = create_candidate(
        &repo.root,
        &repo.commit,
        Path::new("unused-advisory-descriptor"),
        &ProcessEnvironment::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("existing release candidate ownership mismatch:"));
    assert_eq!(fs::read(sentinel).unwrap(), b"external");
}

#[test]
fn production_recover_and_status_reject_symlinked_retained_roots_without_mutation() {
    let fixture = proof_tests::retained_fixture();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"external").unwrap();
    let proofs = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs");
    let retained = fixture.repo.root.path().join("dist/retained-proofs");
    fs::rename(&proofs, &retained).unwrap();
    symlink(external.path(), &proofs).unwrap();
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err());
    assert!(recover_candidate(&fixture.repo.root, "1.0.0").is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"external");
    assert!(retained.is_dir());
}

#[test]
fn production_status_rejects_symlinked_proof_leaf_without_external_mutation() {
    let fixture = proof_tests::retained_fixture();
    let external = tempfile::tempdir().unwrap();
    let sentinel = external.path().join("sentinel");
    fs::write(&sentinel, b"external").unwrap();
    let proof = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs/debian-amd64.json");
    fs::remove_file(&proof).unwrap();
    symlink(&sentinel, &proof).unwrap();
    assert!(candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"external");
}
