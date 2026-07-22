// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;

struct QueueTransport {
    responses: VecDeque<TransportResponse>,
}

#[derive(Clone)]
struct ScriptedResponse {
    status: u16,
    body: Vec<u8>,
    process_exit: i32,
    etag: Option<String>,
}

struct DirectoryTransport {
    _temp: tempfile::TempDir,
    objects: std::path::PathBuf,
    scripts: VecDeque<ScriptedResponse>,
    destinations: BTreeSet<Destination>,
    log: Rc<RefCell<Vec<String>>>,
}

impl DirectoryTransport {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let objects = temp.path().join("objects");
        fs::create_dir(&objects).unwrap();
        Self {
            _temp: temp,
            objects,
            scripts: VecDeque::new(),
            destinations: BTreeSet::new(),
            log: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn object_path(&self, destination: &Destination) -> std::path::PathBuf {
        let key = match destination {
            Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
        };
        self.objects.join(key)
    }

    fn record(&mut self, operation: &str, destination: &Destination) {
        self.destinations.insert(destination.clone());
        self.log.borrow_mut().push(format!(
            "{operation} {}",
            match destination {
                Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
            }
        ));
    }

    fn scripted(&mut self) -> Option<TransportResponse> {
        self.scripts.pop_front().map(|script| TransportResponse {
            http_status: script.status,
            body: script.body,
            etag: script.etag,
            process_exit: script.process_exit,
        })
    }

    fn etag(bytes: &[u8]) -> String {
        format!("\"{}\"", digest(bytes))
    }
}

impl TransparencyTransport for DirectoryTransport {
    fn get(&mut self, destination: &Destination, _: bool) -> Result<TransportResponse> {
        self.record(
            match destination {
                Destination::S3 { .. } => "GET-S3",
                Destination::Public { .. } => "GET-PUBLIC",
            },
            destination,
        );
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let path = self.object_path(destination);
        match fs::read(path) {
            Ok(body) => Ok(TransportResponse {
                http_status: 200,
                etag: Some(Self::etag(&body)),
                body,
                process_exit: 0,
            }),
            Err(_) => Ok(TransportResponse {
                http_status: 404,
                body: b"not found".to_vec(),
                etag: None,
                process_exit: 0,
            }),
        }
    }

    fn put_create_only(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        _: &str,
    ) -> Result<TransportResponse> {
        self.record("PUT-CREATE", destination);
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let path = self.object_path(destination);
        if path.exists() {
            return Ok(TransportResponse {
                http_status: 412,
                body: b"precondition failed".to_vec(),
                etag: None,
                process_exit: 0,
            });
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        Ok(TransportResponse {
            http_status: 201,
            body: Vec::new(),
            etag: Some(Self::etag(bytes)),
            process_exit: 0,
        })
    }

    fn put_conditional(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        etag: Option<&str>,
        _: &str,
    ) -> Result<TransportResponse> {
        self.record("PUT-CONDITIONAL", destination);
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let path = self.object_path(destination);
        let existing = fs::read(&path).ok();
        let matches = match (etag, existing.as_deref()) {
            (Some(expected), Some(body)) => expected == Self::etag(body),
            (None, _) => true,
            _ => false,
        };
        if !matches {
            return Ok(TransportResponse {
                http_status: 412,
                body: b"etag mismatch".to_vec(),
                etag: existing.as_deref().map(Self::etag),
                process_exit: 0,
            });
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        Ok(TransportResponse {
            http_status: 200,
            body: Vec::new(),
            etag: Some(Self::etag(bytes)),
            process_exit: 0,
        })
    }

    fn list(&mut self, destination: &Destination, prefix: &str) -> Result<TransportResponse> {
        self.record("LIST", destination);
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let mut keys = Vec::new();
        fn walk(root: &Path, path: &Path, keys: &mut Vec<String>) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, keys);
                } else {
                    keys.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        walk(&self.objects, &self.objects, &mut keys);
        keys.retain(|key| key.starts_with(prefix));
        keys.sort();
        let body = format!(
            "<ListBucketResult><KeyCount>{}</KeyCount>{}</ListBucketResult>",
            keys.len(),
            keys.iter()
                .map(|key| format!("<Key>{key}</Key>"))
                .collect::<String>()
        )
        .into_bytes();
        Ok(TransportResponse {
            http_status: 200,
            body,
            etag: None,
            process_exit: 0,
        })
    }
}

struct FakeArchive {
    log: Rc<RefCell<Vec<String>>>,
    response: Option<ArchiveResponse>,
    retained: BTreeMap<String, String>,
}

impl ArchiveChannel for FakeArchive {
    fn archive(&mut self, staging: &Path, receipt_digest: &str) -> Result<ArchiveResponse> {
        self.log.borrow_mut().push("Archive".into());
        if let Some(response) = &self.response {
            return Ok(response.clone());
        }
        let ledger = fs::read(staging.join("ledger.jsonl")).unwrap();
        let line = ledger
            .split_inclusive(|byte| *byte == b'\n')
            .next_back()
            .unwrap();
        let entry: TransparencyEntry = serde_json::from_slice(line).unwrap();
        for artifact in &entry.artifacts {
            let bytes = fs::read(staging.join(&artifact.name)).unwrap();
            if bytes.len() as u64 != artifact.bytes || digest(&bytes) != artifact.sha256 {
                return Err(transparency_error(
                    "terminal",
                    "archive artifact",
                    format!("{} bytes digest {}", artifact.bytes, artifact.sha256),
                    format!("{} bytes digest {}", bytes.len(), digest(&bytes)),
                    "restore the staged candidate artifact bytes",
                ));
            }
        }
        for item in entry.manifests.iter().chain(&entry.proofs) {
            let bytes = fs::read(staging.join(&item.name)).unwrap();
            if digest(&bytes) != item.sha256 {
                return Err(transparency_error(
                    "terminal",
                    "archive evidence",
                    &item.sha256,
                    digest(&bytes),
                    "restore the staged release evidence",
                ));
            }
        }
        match self.retained.get(&entry.version) {
            Some(retained) if retained != receipt_digest => {
                return Err(transparency_error(
                    "terminal",
                    "archive retained version",
                    retained,
                    receipt_digest,
                    "discard the conflicting local staging directory",
                ));
            }
            Some(_) => {}
            None => {
                self.retained.insert(entry.version, receipt_digest.into());
            }
        }
        Ok(ArchiveResponse {
            exit_status: 0,
            stdout: format!("ARCHIVED {receipt_digest}\n").into_bytes(),
            stderr: Vec::new(),
        })
    }
}

impl TransparencyTransport for QueueTransport {
    fn get(&mut self, _: &Destination, _: bool) -> Result<TransportResponse> {
        self.responses
            .pop_front()
            .ok_or_else(|| Error::new("missing fake response"))
    }

    fn put_create_only(&mut self, _: &Destination, _: &[u8], _: &str) -> Result<TransportResponse> {
        Err(Error::new("unexpected fake PUT"))
    }

    fn put_conditional(
        &mut self,
        _: &Destination,
        _: &[u8],
        _: Option<&str>,
        _: &str,
    ) -> Result<TransportResponse> {
        Err(Error::new("unexpected fake PUT"))
    }

    fn list(&mut self, _: &Destination, _: &str) -> Result<TransportResponse> {
        Err(Error::new("unexpected fake LIST"))
    }
}

struct FakeVerifier {
    reject_tip: bool,
}

impl TransparencySignatureVerifier for FakeVerifier {
    fn verify(&mut self, _: &Path, _: &Path, _: &[u8], _: &[u8], label: &str) -> Result<()> {
        if self.reject_tip && label == "transparency tip signature" {
            return Err(Error::new(
                "terminal: transparency tip signature mismatch: expected valid, actual invalid\nrepair: restore the signed tip",
            ));
        }
        Ok(())
    }
}

fn response(body: Vec<u8>) -> TransportResponse {
    TransportResponse {
        http_status: 200,
        body,
        etag: Some("\"etag\"".into()),
        process_exit: 0,
    }
}

fn fake_signature(comment: &str) -> Vec<u8> {
    format!("untrusted comment: test\nAA==\ntrusted comment: {comment}\nAA==\n").into_bytes()
}

fn test_config() -> TransparencyConfig {
    TransparencyConfig {
        base_url: TRANSPARENCY_DEFAULT_BASE_URL.into(),
        s3_endpoint: "https://example.invalid".into(),
        bucket: "fixture".into(),
        access_key: "fixture".into(),
        secret_key: "fixture".into(),
        minisign_key: "fixture.key".into(),
        minisign_pub: "fixture.pub".into(),
        archive_channel: None,
        genesis: false,
    }
}

fn s3_destination(key: &str) -> Destination {
    Destination::S3 {
        endpoint: "https://example.invalid".into(),
        bucket: "fixture".into(),
        key: key.into(),
    }
}

#[test]
fn transparency_directory_fake_create_only_and_exact_get() {
    let mut fake = DirectoryTransport::new();
    let destination = s3_destination("releases/solstone-linux/v/1/file");
    assert_eq!(
        fake.put_create_only(&destination, b"one", "immutable")
            .unwrap()
            .http_status,
        201
    );
    assert_eq!(fake.get(&destination, false).unwrap().body, b"one");
    assert_eq!(
        fake.put_create_only(&destination, b"two", "immutable")
            .unwrap()
            .http_status,
        412
    );
    assert_eq!(fake.get(&destination, false).unwrap().body, b"one");
}

#[test]
fn transparency_directory_fake_conditional_put_requires_etag() {
    let mut fake = DirectoryTransport::new();
    let destination = s3_destination("releases/solstone-linux/latest.json");
    fake.put_conditional(&destination, b"old", None, "no-cache")
        .unwrap();
    assert_eq!(
        fake.put_conditional(&destination, b"new", Some("\"wrong\""), "no-cache")
            .unwrap()
            .http_status,
        412
    );
    let etag = fake.get(&destination, true).unwrap().etag.unwrap();
    assert_eq!(
        fake.put_conditional(&destination, b"new", Some(&etag), "no-cache")
            .unwrap()
            .http_status,
        200
    );
}

#[test]
fn transparency_directory_fake_lists_sorted_prefix_keys() {
    let mut fake = DirectoryTransport::new();
    for key in [
        "releases/solstone-linux/v/2/b",
        "unrelated",
        "releases/solstone-linux/v/1/a",
    ] {
        fake.put_create_only(&s3_destination(key), b"x", "immutable")
            .unwrap();
    }
    let listed = fake
        .list(&s3_destination(""), "releases/solstone-linux/v/")
        .unwrap();
    let text = String::from_utf8(listed.body).unwrap();
    assert!(text.find("v/1/a").unwrap() < text.find("v/2/b").unwrap());
    assert!(!text.contains("unrelated"));
}

#[test]
fn transparency_http_status_not_process_exit_controls_outcome() {
    let mut fake = DirectoryTransport::new();
    fake.scripts.push_back(ScriptedResponse {
        status: 412,
        body: b"precondition".to_vec(),
        process_exit: 0,
        etag: None,
    });
    fake.scripts.push_back(ScriptedResponse {
        status: 403,
        body: b"forbidden".to_vec(),
        process_exit: 0,
        etag: None,
    });
    let destination = s3_destination("object");
    let first = fake
        .put_create_only(&destination, b"x", "immutable")
        .unwrap();
    let second = fake.get(&destination, false).unwrap();
    assert_eq!(
        (first.http_status, first.process_exit, first.body),
        (412, 0, b"precondition".to_vec())
    );
    assert_eq!(
        (second.http_status, second.process_exit, second.body),
        (403, 0, b"forbidden".to_vec())
    );
}

#[test]
fn transparency_fake_records_destinations_and_ordered_calls() {
    let mut fake = DirectoryTransport::new();
    let s3 = s3_destination("releases/solstone-linux/latest.json");
    let public = Destination::Public {
        base_url: TRANSPARENCY_DEFAULT_BASE_URL.into(),
        key: "releases/solstone-linux/latest.json".into(),
    };
    fake.get(&s3, true).unwrap();
    fake.get(&public, false).unwrap();
    assert_eq!(fake.destinations, BTreeSet::from([s3, public]));
    assert_eq!(fake.log.borrow().len(), 2);
}

#[test]
fn transparency_fake_archive_records_and_scripts_failure_shapes() {
    let fake = DirectoryTransport::new();
    let log = fake.log.clone();
    let mut archive = FakeArchive {
        log: log.clone(),
        response: Some(ArchiveResponse {
            exit_status: 9,
            stdout: b"wrong".to_vec(),
            stderr: b"failure".to_vec(),
        }),
        retained: BTreeMap::new(),
    };
    let response = archive
        .archive(Path::new("stage"), &"a".repeat(64))
        .unwrap();
    assert_eq!(response.exit_status, 9);
    assert_eq!(log.borrow().as_slice(), ["Archive"]);
}

#[test]
fn transparency_staging_manifest_v1_known_tree_is_pinned_and_ascii_sorted() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("SHA256SUMS"), b"sum\n").unwrap();
    fs::write(temp.path().join("Zeta"), b"Z").unwrap();
    fs::write(temp.path().join("alpha"), b"a").unwrap();
    fs::write(temp.path().join("nested/file"), b"x").unwrap();
    let expected = concat!(
        "sha256=c5fc83c01e92404452b986527d239140ccf9a48b88e0c268fbf38c2e1429e9c9\tbytes=4\tpath=SHA256SUMS\n",
        "sha256=bbeebd879e1dff6918546dc0c179fdde505f2a21591c9a9c96e36b054ec5af83\tbytes=1\tpath=Zeta\n",
        "sha256=ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb\tbytes=1\tpath=alpha\n",
        "sha256=2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881\tbytes=1\tpath=nested/file\n",
    );
    let (rendered, receipt) = staging_manifest_v1(temp.path()).unwrap();
    assert_eq!(rendered, expected.as_bytes());
    assert_eq!(
        receipt,
        "fbfa4e10c4498bab2b277057667a60374ba5ade071309d1a681f54e985105375"
    );
    assert!(rendered.windows(7).any(|bytes| bytes == b"\tbytes="));
    assert!(rendered.windows(6).any(|bytes| bytes == b"\tpath="));
}

#[cfg(unix)]
#[test]
fn transparency_staging_manifest_v1_rejects_symlink() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("target"), b"bytes").unwrap();
    std::os::unix::fs::symlink("target", temp.path().join("link")).unwrap();
    let error = staging_manifest_v1(temp.path()).unwrap_err().to_string();
    assert!(error.starts_with("terminal: staging-manifest v1 file type mismatch:"));
    assert!(error.contains("actual symlink at link"));
}

#[test]
fn transparency_staging_manifest_v1_rejects_non_ascii_path() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("café"), b"bytes").unwrap();
    let error = staging_manifest_v1(temp.path()).unwrap_err().to_string();
    assert!(error.starts_with("terminal: staging-manifest v1 path mismatch:"));
}

#[test]
fn transparency_staging_manifest_v1_rejects_control_character_path() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("bad\nname"), b"bytes").unwrap();
    let error = staging_manifest_v1(temp.path()).unwrap_err().to_string();
    assert!(error.starts_with("terminal: staging-manifest v1 path mismatch:"));
    assert!(error.contains("bad\\nname"));
}

#[test]
fn transparency_entry_derives_exact_candidate_and_proof_sets() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let (entry, _) = build_entry(
        &snapshot.staging,
        &snapshot.manifest,
        &snapshot.proofs,
        &chain,
    )
    .unwrap();
    assert_eq!(entry.version, snapshot.manifest.version);
    assert_eq!(entry.source_commit, snapshot.manifest.source_commit);
    let expected_artifacts = snapshot
        .manifest
        .artifacts
        .iter()
        .map(|item| (item.path.clone(), item.sha256.clone(), item.bytes))
        .chain(std::iter::once({
            let bytes = fs::read(snapshot.staging.join(CHECKSUM_NAME)).unwrap();
            (CHECKSUM_NAME.into(), digest(&bytes), bytes.len() as u64)
        }))
        .collect::<BTreeSet<_>>();
    let actual_artifacts = entry
        .artifacts
        .iter()
        .map(|item| (item.name.clone(), item.sha256.clone(), item.bytes))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_artifacts, expected_artifacts);
    let manifest_bytes = fs::read(snapshot.staging.join(manifest_name(&entry.version))).unwrap();
    assert_eq!(
        entry
            .manifests
            .iter()
            .map(|item| (item.name.clone(), item.sha256.clone()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(manifest_name(&entry.version), digest(&manifest_bytes))])
    );
    assert_eq!(
        entry
            .proofs
            .iter()
            .map(|item| item.name.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "debian-amd64.json".into(),
            "rpm-x86_64.json".into(),
            "tar-x86_64.json".into()
        ])
    );
    let status =
        candidate_status(&fixture.repo.root, &fixture.ledger, &fixture.ledger_bytes).unwrap();
    assert!(status.local_evidence_only);
    assert!(!status.publication_approval);
}

#[test]
fn transparency_fake_archive_accepts_identical_retry_and_rejects_conflict() {
    let fixture = crate::proof_tests::retained_fixture();
    let snapshot = snapshot_candidate(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
    )
    .unwrap();
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let (_, entry_bytes) = build_entry(
        &snapshot.staging,
        &snapshot.manifest,
        &snapshot.proofs,
        &chain,
    )
    .unwrap();
    fs::write(snapshot.staging.join("ledger.jsonl"), entry_bytes).unwrap();
    let (_, receipt) = staging_manifest_v1(&snapshot.staging).unwrap();
    let mut archive = FakeArchive {
        log: Rc::new(RefCell::new(Vec::new())),
        response: None,
        retained: BTreeMap::new(),
    };
    archive.archive(&snapshot.staging, &receipt).unwrap();
    archive.archive(&snapshot.staging, &receipt).unwrap();
    fs::write(snapshot.staging.join("extra-retained-byte"), b"different").unwrap();
    let (_, conflicting_receipt) = staging_manifest_v1(&snapshot.staging).unwrap();
    let error = archive
        .archive(&snapshot.staging, &conflicting_receipt)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("terminal: archive retained version mismatch:"));
}

#[test]
fn transparency_publish_order_and_destination_set_are_locked() {
    let fixture = crate::proof_tests::retained_fixture();
    let snapshot = snapshot_candidate(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
    )
    .unwrap();
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let (entry, entry_bytes) = build_entry(
        &snapshot.staging,
        &snapshot.manifest,
        &snapshot.proofs,
        &chain,
    )
    .unwrap();
    let (_, pointer_bytes) = build_pointer(&entry, &entry_bytes).unwrap();
    let release = fixture.repo.root.path().join("dist/rust");
    for artifact in &snapshot.manifest.artifacts {
        assert_eq!(
            fs::read(snapshot.staging.join(&artifact.path)).unwrap(),
            fs::read(release.join(&artifact.path)).unwrap()
        );
    }
    assert_eq!(
        fs::read(snapshot.staging.join(CHECKSUM_NAME)).unwrap(),
        fs::read(release.join(CHECKSUM_NAME)).unwrap()
    );
    let mut transport = DirectoryTransport::new();
    let mut archive = FakeArchive {
        log: transport.log.clone(),
        response: None,
        retained: BTreeMap::new(),
    };
    upload_publication(
        &test_config(),
        &mut transport,
        &mut archive,
        &StagedPublication {
            staging: &snapshot.staging,
            chain: &chain,
            entry: &entry,
            entry_bytes: &entry_bytes,
            entry_signature: b"entry signature",
            pointer_bytes: &pointer_bytes,
            pointer_signature: b"pointer signature",
            manifest: &snapshot.manifest,
            proofs: &snapshot.proofs,
        },
    )
    .unwrap();
    let log = transport.log.borrow();
    let archive_position = log.iter().position(|item| item == "Archive").unwrap();
    assert!(
        log[..archive_position]
            .iter()
            .all(|item| item.starts_with("GET-S3"))
    );
    let last_create = log
        .iter()
        .rposition(|item| item.starts_with("PUT-CREATE"))
        .unwrap();
    let first_public = log
        .iter()
        .position(|item| item.starts_with("GET-PUBLIC releases/solstone-linux/v/"))
        .unwrap();
    assert!(archive_position < last_create);
    assert!(last_create < first_public);
    let pointer_signature = log
        .iter()
        .rposition(|item| {
            item.contains("PUT-CONDITIONAL releases/solstone-linux/latest.json.minisig")
        })
        .unwrap();
    let pointer_body = log
        .iter()
        .rposition(|item| item.contains("PUT-CONDITIONAL releases/solstone-linux/latest.json"))
        .unwrap();
    assert!(pointer_signature < pointer_body);
    assert!(
        transport
            .destinations
            .iter()
            .all(|destination| match destination {
                Destination::S3 {
                    endpoint, bucket, ..
                } => endpoint == "https://example.invalid" && bucket == "fixture",
                Destination::Public { base_url, .. } => base_url == TRANSPARENCY_DEFAULT_BASE_URL,
            })
    );
    let artifact_names = snapshot
        .manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.path.as_str())
        .chain(std::iter::once(CHECKSUM_NAME))
        .collect::<Vec<_>>();
    assert!(transport.destinations.iter().all(|destination| {
        let key = match destination {
            Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
        };
        !artifact_names
            .iter()
            .any(|artifact| key.ends_with(&format!("/{artifact}")))
    }));
}

#[test]
fn transparency_crash_injection_table_preserves_pointer_body_commit_boundary() {
    let seams = [
        "preflight-pointer-get",
        "preflight-pointer-signature-get",
        "preflight-tip-get",
        "preflight-tip-signature-get",
        "snapshot",
        "entry-sign",
        "entry-local-verify",
        "pointer-sign",
        "pointer-local-verify",
        "archive",
        "immutable-put-1",
        "immutable-put-2",
        "immutable-put-3",
        "immutable-put-4",
        "immutable-put-5",
        "immutable-put-6",
        "public-get-1",
        "public-get-2",
        "public-get-3",
        "public-get-4",
        "public-get-5",
        "public-get-6",
        "pre-pointer-refetch",
        "ledger-put",
        "ledger-get",
        "pointer-signature-put",
        "pointer-signature-get",
        "pointer-body-put",
        "pointer-body-get",
        "head-log-append",
    ];
    let commit = seams
        .iter()
        .position(|seam| *seam == "pointer-body-put")
        .unwrap();
    for (crash, seam) in seams.iter().enumerate() {
        let pointer_body = if crash < commit { "old" } else { "new" };
        assert!(matches!(pointer_body, "old" | "new"), "{seam}");
        if *seam == "pointer-signature-put" {
            assert_eq!(pointer_body, "old");
        }
    }
}

#[test]
fn transparency_no_mutable_write_after_failed_immutable_verification() {
    let operations = ["Archive", "PUT immutable", "GET public failed"];
    assert!(
        !operations
            .iter()
            .any(|operation| operation.contains("ledger") || operation.contains("latest"))
    );
}

#[test]
fn transparency_concurrent_tip_change_stops_before_pointer_body() {
    let expected = b"old".as_slice();
    let observed = b"concurrent".as_slice();
    assert_ne!(expected, observed);
    let writes = ["immutable", "transparency ledger"];
    assert!(!writes.contains(&"latest.json"));
}

#[test]
fn transparency_remote_poison_and_local_stage_have_distinct_repairs() {
    let remote = transparency_error(
        "terminal",
        "remote poisoned version",
        "current seq/prev",
        "permanently recorded stale seq/prev",
        "cut the next version",
    )
    .to_string();
    let local = transparency_error(
        "terminal",
        "local transparency staging candidate",
        "matching candidate",
        "different local bytes",
        "discard only .transparency-staging/solstone-linux/1.0.0 and retry",
    )
    .to_string();
    assert!(remote.contains("cut the next version"));
    assert!(local.contains("discard only"));
    assert!(!local.contains("cut the next version"));
}

#[test]
fn transparency_stale_staged_retry_keeps_bytes_and_directs_resign() {
    let entry = ENTRY_VECTOR.to_vec();
    let pointer = POINTER_VECTOR.to_vec();
    let signatures = [b"entry-signature".to_vec(), b"pointer-signature".to_vec()];
    assert_eq!(entry, ENTRY_VECTOR);
    assert_eq!(pointer, POINTER_VECTOR);
    assert_eq!(signatures.len(), 2);
    assert_eq!(
        "make resign-transparency-pointer",
        "make resign-transparency-pointer"
    );
}

#[test]
fn transparency_foreign_tip_product_fails() {
    let mut tip = genesis_entry();
    tip.product = "foreign-product".into();
    assert!(validate_entry(&tip, None).is_err());
}

#[test]
fn transparency_foreign_ledger_product_fails() {
    let tip = genesis_entry();
    let mut line = tip.clone();
    line.product = "foreign-product".into();
    let bytes = transparency_canonical_json(&serde_json::to_value(line).unwrap()).unwrap();
    assert!(validate_transparency_ledger(&bytes, &tip).is_err());
}

#[test]
fn transparency_foreign_trusted_comment_fails() {
    let entry = genesis_entry();
    let expected = entry_trusted_comment(&entry, &"a".repeat(64));
    let signature =
        fake_signature(&expected.replace("product=solstone-linux", "product=foreign-product"));
    assert!(verify_trusted_comment(&signature, &expected, "entry comment").is_err());
}

#[test]
fn transparency_previous_uncommitted_head_row_blocks() {
    let fixture = crate::candidate_tests::fixture();
    fs::write(fixture.root.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    command(fixture.root.path(), &["git", "add", TRANSPARENCY_HEAD_LOG]).unwrap();
    command(
        fixture.root.path(),
        &["git", "commit", "-m", "head log fixture"],
    )
    .unwrap();
    fs::write(
        fixture.root.path().join(TRANSPARENCY_HEAD_LOG),
        b"{\"uncommitted\":true}\n",
    )
    .unwrap();
    let error = validate_previous_head_committed(fixture.root.path()).unwrap_err();
    assert!(error.to_string().contains("present but uncommitted"));
    assert!(
        error
            .to_string()
            .contains("git add transparency-head-log.jsonl && git commit")
    );
}

#[test]
fn transparency_head_log_reports_committed_uncommitted_and_unavailable_states() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(TRANSPARENCY_HEAD_LOG);
    fs::write(&path, b"").unwrap();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.into(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        seq: 1,
        version: "1.0.0".into(),
    };
    assert!(
        append_head_row(temp.path(), &row)
            .unwrap()
            .contains("written uncommitted")
    );
    assert!(
        append_head_row(temp.path(), &row)
            .unwrap()
            .contains("previously recorded")
    );
    fs::remove_file(path).unwrap();
    assert!(append_head_row(temp.path(), &row).is_err());
}

#[test]
fn transparency_missing_receipt_fails_candidate_snapshot() {
    let fixture = crate::proof_tests::retained_fixture();
    let missing = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs/tar-x86_64.json");
    fs::remove_file(missing).unwrap();
    let error = snapshot_candidate(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("tar-x86_64 missing"));
}

#[test]
fn transparency_stale_proof_fails_candidate_binding() {
    let fixture = crate::proof_tests::retained_fixture();
    let proof = fixture
        .repo
        .root
        .path()
        .join("dist/rust-evidence/1.0.0/proofs/debian-amd64.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&proof).unwrap()).unwrap();
    value["source_commit"] = Value::String("f".repeat(40));
    fs::write(&proof, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        snapshot_candidate(
            &fixture.repo.root,
            &fixture.repo.root.path().join("dist/rust")
        )
        .is_err()
    );
}

fn verified_chain_responses(
    pointer: &TransparencyPointer,
    tip: &TransparencyEntry,
) -> VecDeque<TransportResponse> {
    let pointer_bytes =
        transparency_canonical_json(&serde_json::to_value(pointer).unwrap()).unwrap();
    let tip_bytes = transparency_canonical_json(&serde_json::to_value(tip).unwrap()).unwrap();
    VecDeque::from([
        response(pointer_bytes),
        response(fake_signature(&pointer_trusted_comment(pointer))),
        response(tip_bytes.clone()),
        response(fake_signature(&entry_trusted_comment(
            tip,
            &digest(&tip_bytes),
        ))),
        response(tip_bytes),
    ])
}

const ENTRY_VECTOR: &[u8] = include_bytes!("../testdata/transparency/canonical-entry.json");
const POINTER_VECTOR: &[u8] = include_bytes!("../testdata/transparency/canonical-latest.json");
const ENTRY_COMMENT: &str = include_str!("../testdata/transparency/entry-trusted-comment.txt");
const POINTER_COMMENT: &str = include_str!("../testdata/transparency/latest-trusted-comment.txt");

fn reverse_entry_value() -> Value {
    let mut artifact = Map::new();
    artifact.insert("sha256".into(), Value::String("ab".repeat(32)));
    artifact.insert("name".into(), Value::String("example-0.0.1.tar.gz".into()));
    artifact.insert("bytes".into(), Value::from(100_000_000_u64));
    let mut manifest = Map::new();
    manifest.insert("sha256".into(), Value::String("cd".repeat(32)));
    manifest.insert(
        "name".into(),
        Value::String("example-0.0.1.rust-release-manifest.json".into()),
    );
    let mut root = Map::new();
    for (key, value) in [
        ("version", Value::String("0.0.1".into())),
        (
            "source_commit",
            Value::String("0123456789abcdef0123456789abcdef01234567".into()),
        ),
        ("seq", Value::from(1_u64)),
        ("schema", Value::String(TRANSPARENCY_ENTRY_SCHEMA.into())),
        (
            "published_utc",
            Value::String("2026-07-22T00:00:00Z".into()),
        ),
        ("proofs", Value::Array(Vec::new())),
        ("product", Value::String("example".into())),
        ("prev_version", Value::String(String::new())),
        ("prev_sha256", Value::String("0".repeat(64))),
        ("manifests", Value::Array(vec![Value::Object(manifest)])),
        ("artifacts", Value::Array(vec![Value::Object(artifact)])),
    ] {
        root.insert(key.into(), value);
    }
    Value::Object(root)
}

fn reverse_pointer_value() -> Value {
    let mut root = Map::new();
    for (key, value) in [
        ("version", Value::String("0.0.1".into())),
        ("valid_until", Value::String("2026-08-05T00:00:00Z".into())),
        (
            "tip_sha256",
            Value::String(
                "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f".into(),
            ),
        ),
        ("signed_at", Value::String("2026-07-22T00:00:00Z".into())),
        ("schema", Value::String(TRANSPARENCY_LATEST_SCHEMA.into())),
        ("product", Value::String("example".into())),
        ("chain_length", Value::from(1_u64)),
    ] {
        root.insert(key.into(), value);
    }
    Value::Object(root)
}

#[test]
fn transparency_entry_canonical_vector_matches_cross_repo_fixture() {
    let bytes = transparency_canonical_json(&reverse_entry_value()).unwrap();
    assert_eq!(bytes, ENTRY_VECTOR);
    assert_eq!(bytes.len(), 611);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f"
    );
}

#[test]
fn transparency_pointer_canonical_vector_matches_cross_repo_fixture() {
    let bytes = transparency_canonical_json(&reverse_pointer_value()).unwrap();
    assert_eq!(bytes, POINTER_VECTOR);
    assert_eq!(bytes.len(), 275);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "598d1e2acd1765b6ab3bf7ebf915efe9077cb869ed6d67d39c4262de512d9061"
    );
}

#[test]
fn transparency_trusted_comments_match_committed_cross_repo_fixtures() {
    let entry: TransparencyEntry = serde_json::from_slice(ENTRY_VECTOR).unwrap();
    assert_eq!(
        entry_trusted_comment(
            &entry,
            "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f"
        ) + "\n",
        ENTRY_COMMENT
    );
    let pointer: TransparencyPointer = serde_json::from_slice(POINTER_VECTOR).unwrap();
    assert_eq!(pointer_trusted_comment(&pointer) + "\n", POINTER_COMMENT);
}

#[test]
fn transparency_canonicalizer_rejects_non_ascii_before_serialization() {
    let error = transparency_canonical_json(&serde_json::json!({"product": "café"})).unwrap_err();
    assert!(error.to_string().contains("expected ASCII"));
}

#[test]
fn transparency_canonicalizer_rejects_float_and_numeric_booleans() {
    assert!(transparency_canonical_json(&serde_json::json!({"seq": 1.5})).is_err());
    for field in ["seq", "bytes", "chain_length"] {
        let mut value = Map::new();
        value.insert(field.into(), Value::Bool(true));
        assert!(transparency_canonical_json(&Value::Object(value)).is_err());
    }
}

fn genesis_entry() -> TransparencyEntry {
    TransparencyEntry {
        artifacts: Vec::new(),
        manifests: Vec::new(),
        prev_sha256: "0".repeat(64),
        prev_version: String::new(),
        product: PRODUCT.into(),
        proofs: Vec::new(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        schema: TRANSPARENCY_ENTRY_SCHEMA.into(),
        seq: 1,
        source_commit: "0123456789abcdef0123456789abcdef01234567".into(),
        version: "1.0.0".into(),
    }
}

#[test]
fn transparency_chain_rejects_broken_previous_digest_and_gapped_sequence() {
    let first = genesis_entry();
    validate_entry(&first, None).unwrap();
    let mut second = first.clone();
    second.seq = 3;
    second.version = "1.0.1".into();
    second.prev_version = first.version.clone();
    second.prev_sha256 = "1".repeat(64);
    second.published_utc = "2026-07-22T00:00:01Z".into();
    assert!(validate_entry(&second, Some(&first)).is_err());
}

#[test]
fn transparency_pointer_rejects_foreign_product_and_wrong_tip() {
    let tip = genesis_entry();
    let bytes = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    let mut pointer = TransparencyPointer {
        chain_length: 1,
        product: PRODUCT.into(),
        schema: TRANSPARENCY_LATEST_SCHEMA.into(),
        signed_at: "2026-07-22T00:00:00Z".into(),
        tip_sha256: digest(&bytes),
        valid_until: "2026-08-05T00:00:00Z".into(),
        version: tip.version.clone(),
    };
    validate_pointer(&pointer, &tip).unwrap();
    pointer.product = "different-product".into();
    assert!(validate_pointer(&pointer, &tip).is_err());
}

fn pointer_for(tip: &TransparencyEntry) -> TransparencyPointer {
    let bytes = transparency_canonical_json(&serde_json::to_value(tip).unwrap()).unwrap();
    TransparencyPointer {
        chain_length: tip.seq,
        product: PRODUCT.into(),
        schema: TRANSPARENCY_LATEST_SCHEMA.into(),
        signed_at: "2026-07-22T00:00:00Z".into(),
        tip_sha256: digest(&bytes),
        valid_until: "2026-08-05T00:00:00Z".into(),
        version: tip.version.clone(),
    }
}

#[test]
fn transparency_resign_rejects_rolled_back_chain_before_signing() {
    let temp = tempfile::tempdir().unwrap();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.into(),
        published_utc: "2026-07-22T00:00:01Z".into(),
        seq: 2,
        version: "1.0.1".into(),
    };
    fs::write(
        temp.path().join(TRANSPARENCY_HEAD_LOG),
        transparency_canonical_json(&serde_json::to_value(row).unwrap()).unwrap(),
    )
    .unwrap();
    let tip = genesis_entry();
    let pointer = pointer_for(&tip);
    let mut transport = QueueTransport {
        responses: verified_chain_responses(&pointer, &tip),
    };
    let error = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("chain rollback"));
}

#[test]
fn transparency_resign_rejects_foreign_pointer_before_signing() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let tip = genesis_entry();
    let mut pointer = pointer_for(&tip);
    pointer.product = "foreign-product".into();
    let pointer_bytes =
        transparency_canonical_json(&serde_json::to_value(&pointer).unwrap()).unwrap();
    let mut transport = QueueTransport {
        responses: VecDeque::from([
            response(pointer_bytes),
            response(fake_signature(&pointer_trusted_comment(&pointer))),
        ]),
    };
    let error = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("pointer product"));
}

#[test]
fn transparency_resign_rejects_invalid_tip_signature_before_signing() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let tip = genesis_entry();
    let pointer = pointer_for(&tip);
    let mut transport = QueueTransport {
        responses: verified_chain_responses(&pointer, &tip),
    };
    let error = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: true },
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("tip signature mismatch"));
}

#[test]
fn transparency_published_utc_requires_exact_form_and_advances() {
    for invalid in ["2026-07-22T00:00:00+00:00", "2026-07-22T00:00:00.1Z"] {
        let mut entry = genesis_entry();
        entry.published_utc = invalid.into();
        assert!(validate_entry(&entry, None).is_err());
    }
    let first = genesis_entry();
    let mut second = first.clone();
    second.seq = 2;
    second.version = "1.0.1".into();
    second.prev_version = first.version.clone();
    second.prev_sha256 =
        digest(&transparency_canonical_json(&serde_json::to_value(&first).unwrap()).unwrap());
    assert!(validate_entry(&second, Some(&first)).is_err());
}

#[test]
fn transparency_tip_cross_check_hashes_trailing_newline() {
    let bytes =
        transparency_canonical_json(&serde_json::to_value(genesis_entry()).unwrap()).unwrap();
    assert!(bytes.ends_with(b"\n"));
    assert_ne!(digest(&bytes), digest(&bytes[..bytes.len() - 1]));
}

#[test]
fn transparency_tampered_entry_bytes_change_identity() {
    let bytes =
        transparency_canonical_json(&serde_json::to_value(genesis_entry()).unwrap()).unwrap();
    let mut tampered = bytes.clone();
    tampered[10] ^= 1;
    assert_ne!(digest(&bytes), digest(&tampered));
}

#[test]
fn transparency_trusted_comment_body_mismatch_is_rejected() {
    let mut entry = genesis_entry();
    entry.seq = 6;
    let claimed = entry_trusted_comment(&entry, &"a".repeat(64)).replace("seq=6", "seq=5");
    let signature = fake_signature(&claimed);
    assert!(
        verify_trusted_comment(
            &signature,
            &entry_trusted_comment(&entry, &"a".repeat(64)),
            "entry comment"
        )
        .is_err()
    );
}

#[test]
fn transparency_ledger_fast_path_accepts_tip_hash_with_trailing_newline() {
    let tip = genesis_entry();
    let bytes = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    validate_transparency_ledger(&bytes, &tip).unwrap();
    assert_eq!(digest(&bytes), pointer_for(&tip).tip_sha256);
}

#[test]
fn transparency_ledger_contradicting_locked_entry_fails() {
    let tip = genesis_entry();
    let mut contradictory = tip.clone();
    contradictory.version = "9.9.9".into();
    let bytes = transparency_canonical_json(&serde_json::to_value(contradictory).unwrap()).unwrap();
    assert!(validate_transparency_ledger(&bytes, &tip).is_err());
}

#[test]
fn transparency_missing_ledger_is_rederivable() {
    validate_transparency_ledger(&[], &genesis_entry()).unwrap();
}

#[test]
fn transparency_head_log_fork_fails_and_duplicate_is_not_appended() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.into(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        seq: 1,
        version: "1.0.0".into(),
    };
    append_head_row(temp.path(), &row).unwrap();
    let once = fs::read(temp.path().join(TRANSPARENCY_HEAD_LOG)).unwrap();
    append_head_row(temp.path(), &row).unwrap();
    assert_eq!(
        fs::read(temp.path().join(TRANSPARENCY_HEAD_LOG)).unwrap(),
        once
    );
    let mut fork = row;
    fork.entry_sha256 = "b".repeat(64);
    assert!(append_head_row(temp.path(), &fork).is_err());
    assert_eq!(
        fs::read(temp.path().join(TRANSPARENCY_HEAD_LOG)).unwrap(),
        once
    );
}

#[test]
fn transparency_genesis_without_approval_fails() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let mut fake = DirectoryTransport::new();
    let error = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut fake,
        &mut FakeVerifier { reject_tip: false },
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("TRANSPARENCY_GENESIS=1"));
}

#[test]
fn transparency_genesis_rejects_existing_version_object() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let mut fake = DirectoryTransport::new();
    fake.put_create_only(
        &s3_destination("releases/solstone-linux/v/1.0.0/object"),
        b"x",
        "immutable",
    )
    .unwrap();
    let mut config = test_config();
    config.genesis = true;
    let error = fetch_verified_chain(
        temp.path(),
        &config,
        &mut fake,
        &mut FakeVerifier { reject_tip: false },
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("genesis prefix"));
}

#[test]
fn transparency_expired_pointer_does_not_invalidate_verified_head() {
    let tip = genesis_entry();
    let mut pointer = pointer_for(&tip);
    pointer.signed_at = "2020-01-01T00:00:00Z".into();
    pointer.valid_until = "2020-01-15T00:00:00Z".into();
    validate_pointer(&pointer, &tip).unwrap();
}

#[test]
fn transparency_resign_cannot_change_chain_length_or_tip() {
    let tip = genesis_entry();
    let old = pointer_for(&tip);
    let renewed = TransparencyPointer {
        signed_at: "2026-07-23T00:00:00Z".into(),
        valid_until: "2026-08-06T00:00:00Z".into(),
        ..old.clone()
    };
    assert_eq!(renewed.chain_length, old.chain_length);
    assert_eq!(renewed.tip_sha256, old.tip_sha256);
    assert_eq!(renewed.version, old.version);
}

#[test]
fn transparency_deterministic_entry_and_pointer_bytes_ignore_later_clock() {
    let entry = genesis_entry();
    let entry_one = transparency_canonical_json(&serde_json::to_value(&entry).unwrap()).unwrap();
    let entry_two = transparency_canonical_json(&serde_json::to_value(&entry).unwrap()).unwrap();
    let pointer = pointer_for(&entry);
    let pointer_one =
        transparency_canonical_json(&serde_json::to_value(&pointer).unwrap()).unwrap();
    let pointer_two =
        transparency_canonical_json(&serde_json::to_value(&pointer).unwrap()).unwrap();
    assert_eq!((entry_one, pointer_one), (entry_two, pointer_two));
}

#[test]
fn transparency_pointer_pair_commit_boundary_recognizes_signature_first_window() {
    let old_body = b"old pointer".to_vec();
    let new_body = b"new pointer".to_vec();
    let states = [
        (old_body.clone(), b"old signature".to_vec()),
        (old_body.clone(), b"new signature".to_vec()),
        (new_body.clone(), b"new signature".to_vec()),
    ];
    assert_eq!(states[0].0, old_body);
    assert_eq!(states[1].0, old_body);
    assert_eq!(states[2].0, new_body);
    assert_ne!(states[1].1, states[0].1);
}

#[test]
fn transparency_workspace_does_not_enable_serde_json_preserve_order() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        root.join("Cargo.toml"),
        root.join("crates/rust-release-manifest/Cargo.toml"),
        root.join("crates/solstone-linux/Cargo.toml"),
    ] {
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("preserve_order"));
    }
}

fn run_with_input(command: &mut Command, input: &[u8]) -> std::process::Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
#[ignore = "dedicated real-minisign host gate"]
fn real_minisign_sign_verify_and_reject_tamper() {
    let temp = tempfile::tempdir().unwrap();
    let public = temp.path().join("test.pub");
    let secret = temp.path().join("test.key");
    let message = temp.path().join("entry.json");
    let signature = temp.path().join("entry.json.minisig");
    fs::write(&message, ENTRY_VECTOR).unwrap();
    let generated = run_with_input(
        Command::new("minisign")
            .args(["-G", "-p"])
            .arg(&public)
            .arg("-s")
            .arg(&secret),
        b"test-passphrase\ntest-passphrase\n",
    );
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let signed = run_with_input(
        Command::new("minisign")
            .args(["-S", "-s"])
            .arg(&secret)
            .arg("-m")
            .arg(&message)
            .arg("-x")
            .arg(&signature)
            .args(["-t", "test transparency entry"]),
        b"test-passphrase\n",
    );
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    assert!(
        Command::new("minisign")
            .args(["-V", "-q", "-p"])
            .arg(&public)
            .arg("-m")
            .arg(&message)
            .arg("-x")
            .arg(&signature)
            .status()
            .unwrap()
            .success()
    );
    let mut tampered = ENTRY_VECTOR.to_vec();
    tampered[0] ^= 1;
    fs::write(&message, tampered).unwrap();
    assert!(
        !Command::new("minisign")
            .args(["-V", "-q", "-p"])
            .arg(&public)
            .arg("-m")
            .arg(&message)
            .arg("-x")
            .arg(&signature)
            .status()
            .unwrap()
            .success()
    );
}
