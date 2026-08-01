// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use chrono::Utc;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::Instant;

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
    cache_controls: Vec<RecordedCacheControl>,
}

#[derive(Debug, PartialEq, Eq)]
struct RecordedCacheControl {
    operation: &'static str,
    destination: Destination,
    value: String,
}

struct FaultTransport {
    inner: DirectoryTransport,
    corrupt_pointer_signature_get: bool,
    reject_uploaded_pointer_signature: bool,
    move_tip_on_latest_get: bool,
    fail_first_public_get: bool,
    archive_response: Option<ArchiveResponse>,
}

impl FaultTransport {
    fn new() -> Self {
        Self {
            inner: DirectoryTransport::new(),
            corrupt_pointer_signature_get: false,
            reject_uploaded_pointer_signature: false,
            move_tip_on_latest_get: false,
            fail_first_public_get: false,
            archive_response: None,
        }
    }

    fn before(&mut self, operation: &str) -> Result<()> {
        operation_seam(operation)
    }
}

impl TransparencyTransport for FaultTransport {
    fn get(&mut self, destination: &Destination, cache_bypass: bool) -> Result<TransportResponse> {
        self.before("transport GET")?;
        let key = match destination {
            Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
        };
        if self.fail_first_public_get && matches!(destination, Destination::Public { .. }) {
            self.fail_first_public_get = false;
            let mut response = self.inner.get(destination, cache_bypass)?;
            response.http_status = 500;
            return Ok(response);
        }
        if self.move_tip_on_latest_get
            && matches!(destination, Destination::S3 { .. })
            && key.ends_with("/latest.json")
        {
            return Ok(TransportResponse {
                http_status: 200,
                body: b"concurrent pointer".to_vec(),
                etag: Some("\"concurrent\"".into()),
                process_exit: 0,
            });
        }
        let mut response = self.inner.get(destination, cache_bypass)?;
        if self.corrupt_pointer_signature_get
            && matches!(destination, Destination::S3 { .. })
            && key.ends_with("/latest.json.minisig")
            && response.http_status == 200
        {
            response.body.push(b'!');
        }
        Ok(response)
    }

    fn put_create_only(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.before("transport create-only PUT")?;
        self.inner
            .put_create_only(destination, bytes, cache_control)
    }

    fn put_conditional(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        etag: &str,
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.before("transport conditional PUT")?;
        self.inner
            .put_conditional(destination, bytes, etag, cache_control)
    }

    fn put_mutable(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.before("transport mutable PUT")?;
        self.inner.put_mutable(destination, bytes, cache_control)
    }

    fn list(&mut self, destination: &Destination, prefix: &str) -> Result<TransportResponse> {
        self.before("transport LIST")?;
        self.inner.list(destination, prefix)
    }
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
            cache_controls: Vec::new(),
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
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.cache_controls.push(RecordedCacheControl {
            operation: "create-only",
            destination: destination.clone(),
            value: cache_control.into(),
        });
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
            http_status: 200,
            body: Vec::new(),
            etag: Some(Self::etag(bytes)),
            process_exit: 0,
        })
    }

    fn put_conditional(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        etag: &str,
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.cache_controls.push(RecordedCacheControl {
            operation: "conditional",
            destination: destination.clone(),
            value: cache_control.into(),
        });
        self.record("PUT-CONDITIONAL", destination);
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let path = self.object_path(destination);
        let existing = fs::read(&path).ok();
        let matches = existing
            .as_deref()
            .is_some_and(|body| etag == Self::etag(body));
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

    fn put_mutable(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.cache_controls.push(RecordedCacheControl {
            operation: "mutable",
            destination: destination.clone(),
            value: cache_control.into(),
        });
        self.record("PUT-MUTABLE", destination);
        if let Some(response) = self.scripted() {
            return Ok(response);
        }
        let path = self.object_path(destination);
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
    fn archive(&mut self, staging: &Path, receipt_digest: &str) -> Result<()> {
        self.log.borrow_mut().push("Archive".into());
        if let Some(response) = &self.response {
            let expected = format!("ARCHIVED {receipt_digest}");
            if response.exit_status != 0
                || String::from_utf8_lossy(&response.stdout).lines().last() != Some(&expected)
            {
                return Err(transparency_error(
                    "retryable",
                    "transparency archive receipt",
                    expected,
                    sanitize_process_stderr(&response.stderr),
                    "repair the archive channel and retry make publish-transparency",
                ));
            }
            return Ok(());
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
        Ok(())
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
        _: &str,
        _: &str,
    ) -> Result<TransportResponse> {
        Err(Error::new("unexpected fake PUT"))
    }

    fn put_mutable(&mut self, _: &Destination, _: &[u8], _: &str) -> Result<TransportResponse> {
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
        if self.reject_tip
            && matches!(
                label,
                "transparency tip signature" | "uploaded transparency pointer signature"
            )
        {
            return Err(Error::new(format!(
                "terminal: {label} mismatch: expected valid, actual invalid\nrepair: restore the signed transparency object"
            )));
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

#[test]
fn transparency_schemas_are_digest_and_identity_pinned() {
    for (bytes, expected_len, expected_digest, expected_id) in [
        (
            TRANSPARENCY_ENTRY_SCHEMA_BYTES,
            2805,
            TRANSPARENCY_ENTRY_SCHEMA_SHA256,
            TRANSPARENCY_ENTRY_SCHEMA,
        ),
        (
            TRANSPARENCY_LATEST_SCHEMA_BYTES,
            1140,
            TRANSPARENCY_LATEST_SCHEMA_SHA256,
            TRANSPARENCY_LATEST_SCHEMA,
        ),
    ] {
        assert_eq!(bytes.len(), expected_len);
        assert_eq!(digest(bytes), expected_digest);
        let schema: Value = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["$id"], expected_id);
    }
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

fn environment_values() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("TRANSPARENCY_ARCHIVE_CHANNEL".into(), "archive".into()),
        (
            "TRANSPARENCY_BASE_URL".into(),
            TRANSPARENCY_DEFAULT_BASE_URL.into(),
        ),
        (
            "TRANSPARENCY_S3_ENDPOINT".into(),
            "https://example.invalid".into(),
        ),
        ("TRANSPARENCY_BUCKET".into(), "fixture".into()),
        ("TRANSPARENCY_S3_ACCESS_KEY_ID".into(), "access".into()),
        ("TRANSPARENCY_S3_SECRET_ACCESS_KEY".into(), "secret".into()),
        ("TRANSPARENCY_MINISIGN_KEY".into(), "fixture.key".into()),
        ("TRANSPARENCY_MINISIGN_PUB".into(), "fixture.pub".into()),
        ("TRANSPARENCY_GENESIS".into(), "1".into()),
    ])
}

#[test]
fn transparency_tool_versions_are_pinned() {
    assert!(validate_minisign_version("minisign 0.11").is_ok());
    assert!(validate_minisign_version("minisign 0.12").is_ok());
    assert!(validate_minisign_version("minisign 0.10").is_err());
    assert!(validate_minisign_version("minisign 0.13").is_err());
    assert!(validate_curl_version("curl 7.75.0 fixture").is_ok());
    assert!(validate_curl_version("curl 7.74.0 fixture").is_err());
}

#[test]
fn transparency_environment_rejects_non_https_and_control_urls() {
    for (name, value) in [
        ("TRANSPARENCY_BASE_URL", "http://public.invalid"),
        ("TRANSPARENCY_BASE_URL", "https://public.invalid\nwrong"),
        ("TRANSPARENCY_S3_ENDPOINT", "http://s3.invalid"),
        ("TRANSPARENCY_S3_ENDPOINT", "https://s3.invalid\rwrong"),
    ] {
        let mut values = environment_values();
        values.insert(name.into(), value.into());
        let error = TransparencyConfig::from_lookup(true, &mut |key| values.get(key).cloned())
            .unwrap_err()
            .to_string();
        assert!(error.contains(name));
        assert!(error.contains("HTTPS URL"));
    }
}

#[test]
fn transparency_environment_requires_archive_channel_for_publish_only() {
    let mut values = environment_values();
    values.remove("TRANSPARENCY_ARCHIVE_CHANNEL");
    assert!(TransparencyConfig::from_lookup(true, &mut |key| values.get(key).cloned()).is_err());
    assert!(TransparencyConfig::from_lookup(false, &mut |key| values.get(key).cloned()).is_ok());
}

#[test]
fn transparency_environment_reads_exact_contract_names() {
    let expected = environment_values().into_keys().collect::<BTreeSet<_>>();
    let mut names = Vec::new();
    let values = environment_values();
    let config = TransparencyConfig::from_lookup(true, &mut |name| {
        names.push(name.to_owned());
        values.get(name).cloned()
    })
    .unwrap();
    assert!(config.genesis);
    assert_eq!(names.len(), 9);
    assert_eq!(names.iter().cloned().collect::<BTreeSet<_>>(), expected);
    assert!(
        names
            .iter()
            .all(|name| names.iter().filter(|item| *item == name).count() == 1)
    );

    for value in [Some("0"), Some("true"), Some("01"), Some(" 1"), None] {
        let mut values = environment_values();
        match value {
            Some(value) => {
                values.insert("TRANSPARENCY_GENESIS".into(), value.into());
            }
            None => {
                values.remove("TRANSPARENCY_GENESIS");
            }
        }
        let config =
            TransparencyConfig::from_lookup(true, &mut |name| values.get(name).cloned()).unwrap();
        assert!(!config.genesis, "unexpected genesis for {value:?}");
    }
}

fn s3_destination(key: &str) -> Destination {
    Destination::S3 {
        endpoint: "https://example.invalid".into(),
        bucket: "fixture".into(),
        key: key.into(),
    }
}

fn seed_old_chain(transport: &mut FaultTransport) -> (VerifiedChain, Vec<u8>, Vec<u8>) {
    let mut entry = genesis_entry();
    entry.version = "0.9.0".into();
    let entry_bytes = transparency_canonical_json(&serde_json::to_value(&entry).unwrap()).unwrap();
    let entry_signature = fake_signature(&entry_trusted_comment(&entry, &digest(&entry_bytes)));
    let pointer = pointer_for(&entry);
    let pointer_bytes =
        transparency_canonical_json(&serde_json::to_value(&pointer).unwrap()).unwrap();
    let pointer_signature = fake_signature(&pointer_trusted_comment(&pointer));
    for (key, bytes) in [
        (
            "releases/solstone-linux/v/0.9.0/ledger-entry.json",
            entry_bytes.as_slice(),
        ),
        (
            "releases/solstone-linux/v/0.9.0/ledger-entry.json.minisig",
            entry_signature.as_slice(),
        ),
    ] {
        transport
            .inner
            .put_create_only(&s3_destination(key), bytes, "immutable")
            .unwrap();
    }
    for (key, bytes) in [
        (
            "releases/solstone-linux/ledger.jsonl",
            entry_bytes.as_slice(),
        ),
        (
            "releases/solstone-linux/latest.json.minisig",
            pointer_signature.as_slice(),
        ),
    ] {
        transport
            .inner
            .put_mutable(&s3_destination(key), bytes, "no-cache")
            .unwrap();
    }
    transport
        .inner
        .put_create_only(
            &s3_destination("releases/solstone-linux/latest.json"),
            &pointer_bytes,
            "no-cache",
        )
        .unwrap();
    let etag = transport
        .inner
        .get(&s3_destination("releases/solstone-linux/latest.json"), true)
        .unwrap()
        .etag;
    transport.inner.log.borrow_mut().clear();
    transport.inner.destinations.clear();
    (
        VerifiedChain {
            pointer: Some(pointer),
            pointer_bytes: Some(pointer_bytes.clone()),
            pointer_etag: etag,
            tip: Some(entry),
            transparency_ledger: entry_bytes,
        },
        pointer_bytes,
        pointer_signature,
    )
}

fn chain_with_historical_candidate(
    snapshot: &CandidateSnapshot,
) -> (VerifiedChain, RemoteEntryPair, TransparencyEntry) {
    let empty = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    set_test_now(Some(
        DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    let (historical, historical_bytes) = build_entry(
        &snapshot.staging,
        &snapshot.manifest,
        &snapshot.proofs,
        &empty,
    )
    .unwrap();
    set_test_now(None);
    let mut tip = historical.clone();
    tip.seq = 2;
    tip.version = "1.1.0".into();
    tip.prev_sha256 = digest(&historical_bytes);
    tip.prev_version = historical.version.clone();
    tip.published_utc = "2026-07-23T00:00:00Z".into();
    validate_entry(&tip, Some(&historical)).unwrap();
    let tip_bytes = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    let pair = RemoteEntryPair {
        signature_bytes: fake_signature(&entry_trusted_comment(
            &historical,
            &digest(&historical_bytes),
        )),
        entry_bytes: historical_bytes.clone(),
    };
    (
        VerifiedChain {
            pointer: Some(pointer_for(&tip)),
            pointer_bytes: None,
            pointer_etag: None,
            tip: Some(tip),
            transparency_ledger: [historical_bytes, tip_bytes].concat(),
        },
        pair,
        historical,
    )
}

fn stage_entry_pair(
    snapshot: &CandidateSnapshot,
    entry: &TransparencyEntry,
    pair: &RemoteEntryPair,
) {
    fs::write(
        snapshot.staging.join("ledger-entry.json"),
        &pair.entry_bytes,
    )
    .unwrap();
    fs::write(
        snapshot.staging.join("ledger-entry.json.minisig"),
        &pair.signature_bytes,
    )
    .unwrap();
    let (pointer, pointer_bytes) = build_pointer(entry, &pair.entry_bytes).unwrap();
    fs::write(snapshot.staging.join("latest.json"), pointer_bytes).unwrap();
    fs::write(
        snapshot.staging.join("latest.json.minisig"),
        fake_signature(&pointer_trusted_comment(&pointer)),
    )
    .unwrap();
}

enum RemoteEntryMode {
    Adopt,
    Poison,
    EntryOnly,
    SignatureOnly,
    ForeignSchema,
    NonCanonical,
    InvalidTimestamp,
}

fn run_fixture_publication(
    transport: &mut FaultTransport,
) -> (Result<String>, Vec<u8>, std::path::PathBuf) {
    run_fixture_publication_with_remote(transport, None)
}

fn run_fixture_publication_with_remote(
    transport: &mut FaultTransport,
    remote_mode: Option<RemoteEntryMode>,
) -> (Result<String>, Vec<u8>, std::path::PathBuf) {
    run_fixture_publication_with_remote_and_chain(transport, remote_mode, None)
}

fn run_fixture_publication_with_remote_and_chain(
    transport: &mut FaultTransport,
    remote_mode: Option<RemoteEntryMode>,
    chain: Option<VerifiedChain>,
) -> (Result<String>, Vec<u8>, std::path::PathBuf) {
    let fixture = crate::proof_tests::retained_fixture();
    let chain = chain.unwrap_or(VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    });
    let mut verifier = FakeVerifier {
        reject_tip: transport.reject_uploaded_pointer_signature,
    };
    let mut signer = |_: &Path, _: &Path, signature: &Path, comment: &str| {
        fs::write(signature, fake_signature(comment)).map_err(display_error)
    };
    let mut probe = |_: &str| Ok(None);
    let prepared = match prepare_publication(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
        &chain,
        Path::new("fixture.pub"),
        &mut verifier,
        &mut signer,
        &mut probe,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return (Err(error), Vec::new(), transport.inner.objects.clone()),
    };
    if let Some(mode) = remote_mode {
        let mut remote_entry = prepared.entry.clone();
        if !matches!(
            mode,
            RemoteEntryMode::EntryOnly | RemoteEntryMode::SignatureOnly
        ) {
            remote_entry.published_utc = "2026-01-01T00:00:00Z".into();
        }
        if matches!(mode, RemoteEntryMode::Poison) {
            remote_entry.source_commit = "f".repeat(40);
        }
        if matches!(mode, RemoteEntryMode::ForeignSchema) {
            remote_entry.schema = "https://example.invalid/foreign-schema.json".into();
        }
        if matches!(mode, RemoteEntryMode::InvalidTimestamp) {
            remote_entry.published_utc = "not-a-timestamp".into();
        }
        let mut remote_bytes =
            transparency_canonical_json(&serde_json::to_value(&remote_entry).unwrap()).unwrap();
        if matches!(mode, RemoteEntryMode::NonCanonical) {
            remote_bytes.insert(0, b' ');
        }
        let remote_signature = fake_signature(&entry_trusted_comment(
            &remote_entry,
            &digest(&remote_bytes),
        ));
        if !matches!(mode, RemoteEntryMode::SignatureOnly) {
            transport
                .inner
                .put_create_only(
                    &s3_destination("releases/solstone-linux/v/1.0.0/ledger-entry.json"),
                    &remote_bytes,
                    "immutable",
                )
                .unwrap();
        }
        if !matches!(mode, RemoteEntryMode::EntryOnly) {
            transport
                .inner
                .put_create_only(
                    &s3_destination("releases/solstone-linux/v/1.0.0/ledger-entry.json.minisig"),
                    &remote_signature,
                    "immutable",
                )
                .unwrap();
        }
    }
    let mut archive = FakeArchive {
        log: transport.inner.log.clone(),
        response: transport.archive_response.clone(),
        retained: BTreeMap::new(),
    };
    let result = upload_publication(
        &test_config(),
        transport,
        &mut archive,
        &mut verifier,
        &prepared.snapshot.staging,
        &StagedPublication {
            staging: &prepared.snapshot.staging,
            chain: &chain,
            entry: &prepared.entry,
            entry_bytes: &prepared.entry_bytes,
            entry_signature: &prepared.entry_signature,
            pointer_bytes: &prepared.pointer_bytes,
            pointer_signature: &prepared.pointer_signature,
            manifest: &prepared.snapshot.manifest,
            proofs: &prepared.snapshot.proofs,
        },
    );
    let result = result.and_then(|archive_digest| {
        append_head_row(
            fixture.repo.root.path(),
            &TransparencyHeadRow {
                entry_sha256: digest(&prepared.entry_bytes),
                product: PRODUCT.into(),
                published_utc: prepared.entry.published_utc.clone(),
                seq: prepared.entry.seq,
                version: prepared.entry.version.clone(),
            },
        )?;
        Ok(archive_digest)
    });
    (
        result,
        prepared.pointer_bytes,
        transport.inner.objects.clone(),
    )
}

#[test]
fn transparency_directory_fake_create_only_and_exact_get() {
    let mut fake = DirectoryTransport::new();
    let destination = s3_destination("releases/solstone-linux/v/1/file");
    assert_eq!(
        fake.put_create_only(&destination, b"one", "immutable")
            .unwrap()
            .http_status,
        200
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
    fake.put_mutable(&destination, b"old", "no-cache").unwrap();
    assert_eq!(
        fake.put_conditional(&destination, b"new", "\"wrong\"", "no-cache")
            .unwrap()
            .http_status,
        412
    );
    let etag = fake.get(&destination, true).unwrap().etag.unwrap();
    assert_eq!(
        fake.put_conditional(&destination, b"new", &etag, "no-cache")
            .unwrap()
            .http_status,
        200
    );
}

#[test]
fn transparency_directory_fake_mutable_put_overwrites_existing_object() {
    let mut fake = DirectoryTransport::new();
    let destination = s3_destination("releases/solstone-linux/ledger.jsonl");
    fake.put_mutable(&destination, b"first", "no-cache")
        .unwrap();
    fake.put_mutable(&destination, b"second", "no-cache")
        .unwrap();
    assert_eq!(fake.get(&destination, true).unwrap().body, b"second");
    assert_eq!(
        fake.log
            .borrow()
            .iter()
            .filter(|call| call.starts_with("PUT-MUTABLE"))
            .count(),
        2
    );
}

#[test]
fn transparency_second_publication_and_resign_replace_mutable_objects_end_to_end() {
    let mut transport = FaultTransport::new();
    let (chain, old_pointer, old_signature) = seed_old_chain(&mut transport);
    let old_ledger = chain.transparency_ledger.clone();
    let (result, new_pointer, objects) =
        run_fixture_publication_with_remote_and_chain(&mut transport, None, Some(chain));
    result.unwrap();
    let prefix = objects.join("releases/solstone-linux");
    let ledger_path = prefix.join("ledger.jsonl");
    let signature_path = prefix.join("latest.json.minisig");
    let pointer_path = prefix.join("latest.json");
    let second_ledger = fs::read(&ledger_path).unwrap();
    let second_signature = fs::read(&signature_path).unwrap();
    assert!(second_ledger.starts_with(&old_ledger));
    assert_ne!(second_ledger, old_ledger);
    assert_ne!(second_signature, old_signature);
    assert_ne!(new_pointer, old_pointer);
    assert_eq!(fs::read(&pointer_path).unwrap(), new_pointer);

    let published: TransparencyPointer = serde_json::from_slice(&new_pointer).unwrap();
    let now = DateTime::parse_from_rfc3339("2026-07-30T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let (renewed, renewed_bytes) = renew_pointer(&published, now).unwrap();
    let renewed_signature = fake_signature(&pointer_trusted_comment(&renewed));
    let signature_destination = s3_destination("releases/solstone-linux/latest.json.minisig");
    let pointer_destination = s3_destination("releases/solstone-linux/latest.json");
    transport
        .put_mutable(&signature_destination, &renewed_signature, MUTABLE_CACHE)
        .unwrap();
    let etag = transport
        .get(&pointer_destination, true)
        .unwrap()
        .etag
        .unwrap();
    assert_eq!(
        transport
            .put_conditional(&pointer_destination, &renewed_bytes, &etag, MUTABLE_CACHE,)
            .unwrap()
            .http_status,
        200
    );
    assert_eq!(fs::read(&ledger_path).unwrap(), second_ledger);
    assert_eq!(fs::read(&signature_path).unwrap(), renewed_signature);
    assert_eq!(fs::read(&pointer_path).unwrap(), renewed_bytes);
    assert_eq!(renewed.chain_length, published.chain_length);
    assert_eq!(renewed.tip_sha256, published.tip_sha256);
    assert_eq!(renewed.version, published.version);
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

fn assert_adjacent_argument(args: &[std::ffi::OsString], flag: &str, value: &str) {
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    );
}

#[test]
fn transparency_curl_create_only_argv_has_if_none_match() {
    let destination = s3_destination("object");
    let method = curl_create_only_args(Path::new("upload"), IMMUTABLE_CACHE);
    let args = curl_args(
        &destination,
        &method,
        false,
        Path::new("body"),
        Path::new("headers"),
    );
    assert_adjacent_argument(&args, "--header", "If-None-Match: *");
    assert_adjacent_argument(
        &args,
        "--header",
        &format!("Cache-Control: {IMMUTABLE_CACHE}"),
    );
}

#[test]
fn transparency_curl_conditional_argv_has_if_match() {
    let destination = s3_destination("object");
    let method = curl_conditional_args(Path::new("upload"), "\"fixture-etag\"", MUTABLE_CACHE);
    let args = curl_args(
        &destination,
        &method,
        false,
        Path::new("body"),
        Path::new("headers"),
    );
    assert_adjacent_argument(&args, "--header", "If-Match: \"fixture-etag\"");
    assert_adjacent_argument(
        &args,
        "--header",
        &format!("Cache-Control: {MUTABLE_CACHE}"),
    );
}

#[test]
fn transparency_curl_http_status_not_exit_zero_error_body_controls_outcome() {
    let response = TransportResponse {
        http_status: parse_curl_status(b"403"),
        body: b"forbidden despite exit zero".to_vec(),
        etag: None,
        process_exit: 0,
    };
    let error = ensure_http(&response, &[200], "curl fixture").unwrap_err();
    assert!(error.to_string().contains("HTTP 403"));
    assert_eq!(response.process_exit, 0);
}

#[test]
fn transparency_curl_credentials_are_stdin_only() {
    let access = "ACCESS_SENTINEL_7f39";
    let secret = "SECRET_SENTINEL_91ac";
    let config = curl_stdin_config(access, secret);
    let config = String::from_utf8(config).unwrap();
    assert!(config.contains(access));
    assert!(config.contains(secret));

    let args = curl_args(
        &s3_destination("object"),
        &[],
        false,
        Path::new("body"),
        Path::new("headers"),
    );
    let rendered = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    assert!(!rendered.iter().any(|arg| arg.contains(access)));
    assert!(!rendered.iter().any(|arg| arg.contains(secret)));
    assert!(!rendered.iter().any(|arg| arg == "--user"));
    assert_adjacent_argument(&args, "-K", "-");
    assert_eq!(curl_command(&args).get_envs().count(), 0);
}

#[test]
fn transparency_curl_command_captures_status_stdout() {
    let mut child = curl_command(&[OsString::from("--version")])
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.starts_with(b"curl "));
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
    let mut transport = FaultTransport::new();
    transport.archive_response = Some(ArchiveResponse {
        exit_status: 9,
        stdout: b"wrong".to_vec(),
        stderr: b"failure".to_vec(),
    });
    let (result, _, objects) = run_fixture_publication(&mut transport);
    let error = result.unwrap_err();
    assert!(error.to_string().contains("archive receipt mismatch"));
    assert!(
        transport
            .inner
            .log
            .borrow()
            .iter()
            .any(|call| call == "Archive")
    );
    assert!(
        !objects
            .join("releases/solstone-linux/ledger.jsonl")
            .exists()
    );
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

#[cfg(unix)]
#[test]
fn transparency_obsolete_staging_manifest_symlink_is_rejected_not_removed() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("target"), b"bytes").unwrap();
    let link = temp.path().join("staging-manifest.json");
    std::os::unix::fs::symlink("target", &link).unwrap();
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert!(remove_obsolete_staging_manifest(temp.path()).is_err());
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
}

#[test]
fn transparency_obsolete_staging_manifest_regular_file_is_removed() {
    let temp = tempfile::tempdir().unwrap();
    let obsolete = temp.path().join("staging-manifest.json");
    fs::write(&obsolete, b"obsolete").unwrap();
    remove_obsolete_staging_manifest(temp.path()).unwrap();
    assert!(!obsolete.exists());
}

#[test]
fn transparency_staging_manifest_rejects_interrupted_adoption_temps() {
    for name in [
        ".adopted-ledger-entry.json.tmp",
        ".adopted-ledger-entry.json.minisig.tmp",
    ] {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(name), b"interrupted").unwrap();
        let error = staging_manifest_v1(temp.path()).unwrap_err().to_string();
        assert!(error.starts_with("terminal: staging-manifest v1 temporary file mismatch:"));
        assert!(error.contains(&format!("actual temporary file at {name}")));
        assert!(error.contains("repair: discard the staging directory"));
    }
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
            .map(|item| (item.name.clone(), item.sha256.clone()))
            .collect::<BTreeSet<_>>(),
        snapshot
            .proofs
            .iter()
            .map(|(name, bytes)| (name.clone(), digest(bytes)))
            .collect::<BTreeSet<_>>()
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
    let (pointer, pointer_bytes) = build_pointer(&entry, &entry_bytes).unwrap();
    let entry_signature = fake_signature(&entry_trusted_comment(&entry, &digest(&entry_bytes)));
    let pointer_signature = fake_signature(&pointer_trusted_comment(&pointer));
    let published_objects = immutable_objects(
        &snapshot.staging,
        &entry_bytes,
        &entry_signature,
        &snapshot.manifest,
        &snapshot.proofs,
    )
    .unwrap();
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
        &mut FakeVerifier { reject_tip: false },
        &snapshot.staging,
        &StagedPublication {
            staging: &snapshot.staging,
            chain: &chain,
            entry: &entry,
            entry_bytes: &entry_bytes,
            entry_signature: &entry_signature,
            pointer_bytes: &pointer_bytes,
            pointer_signature: &pointer_signature,
            manifest: &snapshot.manifest,
            proofs: &snapshot.proofs,
        },
    )
    .unwrap();
    let log = transport.log.borrow();
    let manifest_name = manifest_name("1.0.0");
    let expected = vec![
        "GET-S3 releases/solstone-linux/v/1.0.0/ledger-entry.json".into(),
        "GET-S3 releases/solstone-linux/v/1.0.0/ledger-entry.json.minisig".into(),
        "GET-S3 releases/solstone-linux/v/1.0.0/debian-amd64.json".into(),
        "GET-S3 releases/solstone-linux/v/1.0.0/rpm-x86_64.json".into(),
        format!("GET-S3 releases/solstone-linux/v/1.0.0/{manifest_name}"),
        "GET-S3 releases/solstone-linux/v/1.0.0/tar-x86_64.json".into(),
        "Archive".into(),
        "PUT-CREATE releases/solstone-linux/v/1.0.0/debian-amd64.json".into(),
        "PUT-CREATE releases/solstone-linux/v/1.0.0/ledger-entry.json".into(),
        "PUT-CREATE releases/solstone-linux/v/1.0.0/ledger-entry.json.minisig".into(),
        "PUT-CREATE releases/solstone-linux/v/1.0.0/rpm-x86_64.json".into(),
        format!("PUT-CREATE releases/solstone-linux/v/1.0.0/{manifest_name}"),
        "PUT-CREATE releases/solstone-linux/v/1.0.0/tar-x86_64.json".into(),
        "GET-PUBLIC releases/solstone-linux/v/1.0.0/debian-amd64.json".into(),
        "GET-PUBLIC releases/solstone-linux/v/1.0.0/ledger-entry.json".into(),
        "GET-PUBLIC releases/solstone-linux/v/1.0.0/ledger-entry.json.minisig".into(),
        "GET-PUBLIC releases/solstone-linux/v/1.0.0/rpm-x86_64.json".into(),
        format!("GET-PUBLIC releases/solstone-linux/v/1.0.0/{manifest_name}"),
        "GET-PUBLIC releases/solstone-linux/v/1.0.0/tar-x86_64.json".into(),
        "GET-S3 releases/solstone-linux/latest.json".into(),
        "PUT-MUTABLE releases/solstone-linux/ledger.jsonl".into(),
        "GET-S3 releases/solstone-linux/ledger.jsonl".into(),
        "PUT-MUTABLE releases/solstone-linux/latest.json.minisig".into(),
        "GET-S3 releases/solstone-linux/latest.json.minisig".into(),
        "PUT-CREATE releases/solstone-linux/latest.json".into(),
        "GET-S3 releases/solstone-linux/latest.json".into(),
        "GET-S3 releases/solstone-linux/latest.json.minisig".into(),
    ];
    assert_eq!(*log, expected);
    let pointer_signature = log
        .iter()
        .rposition(|item| item.contains("PUT-MUTABLE releases/solstone-linux/latest.json.minisig"))
        .unwrap();
    let pointer_body = log
        .iter()
        .rposition(|item| item.contains("PUT-CREATE releases/solstone-linux/latest.json"))
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
    let mut expected_destinations = BTreeSet::new();
    for name in published_objects.keys() {
        expected_destinations.insert(s3_destination(&format!(
            "releases/solstone-linux/v/1.0.0/{name}"
        )));
        expected_destinations.insert(Destination::Public {
            base_url: TRANSPARENCY_DEFAULT_BASE_URL.into(),
            key: format!("releases/solstone-linux/v/1.0.0/{name}"),
        });
    }
    for key in ["ledger.jsonl", "latest.json", "latest.json.minisig"] {
        expected_destinations.insert(s3_destination(&format!("releases/solstone-linux/{key}")));
    }
    assert_eq!(transport.destinations, expected_destinations);
}

#[test]
fn transparency_upload_applies_immutable_and_mutable_cache_controls() {
    let mut transport = FaultTransport::new();
    run_fixture_publication(&mut transport).0.unwrap();
    let records = &transport.inner.cache_controls;
    assert!(!records.is_empty());
    for record in records {
        let key = match &record.destination {
            Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
        };
        if key.contains("/v/1.0.0/") {
            assert_eq!(record.operation, "create-only");
            assert_eq!(record.value, IMMUTABLE_CACHE);
        } else if matches!(
            key.as_str(),
            "releases/solstone-linux/ledger.jsonl"
                | "releases/solstone-linux/latest.json.minisig"
                | "releases/solstone-linux/latest.json"
        ) {
            assert!(matches!(record.operation, "mutable" | "create-only"));
            assert_eq!(record.value, MUTABLE_CACHE);
        } else {
            panic!("unexpected cached destination {key}");
        }
    }
    for expected in ["ledger.jsonl", "latest.json.minisig", "latest.json"] {
        assert!(records.iter().any(|record| {
            let key = match &record.destination {
                Destination::S3 { key, .. } | Destination::Public { key, .. } => key,
            };
            key.ends_with(expected) && record.value == MUTABLE_CACHE
        }));
    }
}

#[test]
fn transparency_crash_injection_table_preserves_pointer_body_commit_boundary() {
    let mut baseline = FaultTransport::new();
    let (chain, _, _) = seed_old_chain(&mut baseline);
    configure_test_operation_seam(None);
    let (result, _, _) =
        run_fixture_publication_with_remote_and_chain(&mut baseline, None, Some(chain));
    result.unwrap();
    let (seam_count, seam_labels) = test_operation_seam_state();
    for required in [
        "snapshot candidate",
        "sign entry",
        "sign pointer",
        "verify local entry signature",
        "verify local pointer signature",
        "archive",
        "transport GET",
        "transport create-only PUT",
        "transport conditional PUT",
        "transport mutable PUT",
        "head-log append",
    ] {
        assert!(
            seam_labels.iter().any(|label| label == required),
            "missing {required}"
        );
    }
    for seam in 0..seam_count {
        let mut transport = FaultTransport::new();
        let (chain, old_pointer, old_signature) = seed_old_chain(&mut transport);
        configure_test_operation_seam(Some(seam));
        let (result, new_pointer, objects) =
            run_fixture_publication_with_remote_and_chain(&mut transport, None, Some(chain));
        assert!(result.is_err(), "seam {seam} unexpectedly succeeded");
        let pointer_path = objects.join("releases/solstone-linux/latest.json");
        let body = fs::read(&pointer_path).unwrap();
        assert!(
            body == old_pointer || body == new_pointer,
            "seam {seam} committed neither pointer body"
        );
        let signature =
            fs::read(objects.join("releases/solstone-linux/latest.json.minisig")).unwrap();
        if body == new_pointer {
            assert_ne!(
                signature, old_signature,
                "new body retained old signature at {seam}"
            );
        } else if signature != old_signature {
            assert_eq!(body, old_pointer, "signature-first window changed the body");
        }
    }
    configure_test_operation_seam(None);
}

#[test]
fn transparency_no_mutable_write_after_failed_immutable_verification() {
    let mut transport = FaultTransport::new();
    transport.fail_first_public_get = true;
    let (result, _, objects) = run_fixture_publication(&mut transport);
    assert!(result.is_err());
    assert!(
        !objects
            .join("releases/solstone-linux/ledger.jsonl")
            .exists()
    );
    assert!(!objects.join("releases/solstone-linux/latest.json").exists());
}

#[test]
fn transparency_concurrent_tip_change_stops_before_pointer_body() {
    let mut transport = FaultTransport::new();
    transport.move_tip_on_latest_get = true;
    let (result, _, objects) = run_fixture_publication(&mut transport);
    let error = result.unwrap_err().to_string();
    assert!(error.contains("pre-pointer transparency chain state mismatch"));
    assert!(
        !objects
            .join("releases/solstone-linux/ledger.jsonl")
            .exists()
    );
    assert!(!objects.join("releases/solstone-linux/latest.json").exists());
}

#[test]
fn transparency_corrupt_pointer_signature_download_blocks_body_commit() {
    let mut transport = FaultTransport::new();
    transport.corrupt_pointer_signature_get = true;
    let (result, _, objects) = run_fixture_publication(&mut transport);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("pointer signature bytes mismatch")
    );
    assert!(!objects.join("releases/solstone-linux/latest.json").exists());
}

#[test]
fn transparency_invalid_downloaded_pointer_signature_blocks_body_commit() {
    let mut transport = FaultTransport::new();
    transport.reject_uploaded_pointer_signature = true;
    let (result, _, objects) = run_fixture_publication(&mut transport);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("uploaded transparency pointer signature mismatch")
    );
    assert!(!objects.join("releases/solstone-linux/latest.json").exists());
}

#[test]
fn transparency_remote_poison_and_local_stage_have_distinct_repairs() {
    let mut transport = FaultTransport::new();
    let remote = run_fixture_publication_with_remote(&mut transport, Some(RemoteEntryMode::Poison))
        .0
        .unwrap_err()
        .to_string();
    let fixture = crate::proof_tests::retained_fixture();
    let snapshot = snapshot_candidate(
        &fixture.repo.root,
        &fixture.repo.root.path().join("dist/rust"),
    )
    .unwrap();
    let mut entry = genesis_entry();
    entry.version = "different".into();
    let local = validate_staged_entry_candidate(&entry, &snapshot.manifest, &snapshot.staging)
        .unwrap_err()
        .to_string();
    assert!(remote.contains("cut the next version"));
    assert!(remote.contains("recorded commit"));
    assert!(remote.contains("seq"));
    assert!(remote.contains("entry"));
    assert!(local.contains("discard only"));
    assert!(!local.contains("cut the next version"));
}

#[test]
fn transparency_preflight_adopts_valid_signed_remote_entry_before_archive() {
    let mut transport = FaultTransport::new();
    let error = run_fixture_publication_with_remote(&mut transport, Some(RemoteEntryMode::Adopt))
        .0
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("retryable: transparency immutable adoption mismatch:"));
    assert!(error.contains("adopted signed remote entry"));
    assert!(
        !transport
            .inner
            .log
            .borrow()
            .iter()
            .any(|call| call == "Archive")
    );
    assert!(
        !transport
            .inner
            .log
            .borrow()
            .iter()
            .any(|call| call.contains("ledger.jsonl"))
    );
}

fn assert_partial_remote_entry_pair_recovers(mode: RemoteEntryMode) {
    let mut transport = FaultTransport::new();
    run_fixture_publication_with_remote(&mut transport, Some(mode))
        .0
        .unwrap();
    for name in ["ledger-entry.json", "ledger-entry.json.minisig"] {
        assert!(
            transport
                .inner
                .objects
                .join(format!("releases/solstone-linux/v/1.0.0/{name}"))
                .is_file(),
            "missing recovered {name}"
        );
    }
}

#[test]
fn transparency_partial_remote_entry_recovers_missing_signature() {
    assert_partial_remote_entry_pair_recovers(RemoteEntryMode::EntryOnly);
}

#[test]
fn transparency_partial_remote_signature_recovers_missing_entry() {
    assert_partial_remote_entry_pair_recovers(RemoteEntryMode::SignatureOnly);
}

fn assert_adoption_rejected(mode: RemoteEntryMode) {
    let mut transport = FaultTransport::new();
    let error = run_fixture_publication_with_remote(&mut transport, Some(mode))
        .0
        .unwrap_err()
        .to_string();
    assert!(error.contains("remote poisoned version"));
    assert!(
        !transport
            .inner
            .log
            .borrow()
            .iter()
            .any(|call| call == "Archive")
    );
}

#[test]
fn transparency_adoption_rejects_foreign_schema() {
    assert_adoption_rejected(RemoteEntryMode::ForeignSchema);
}

#[test]
fn transparency_adoption_rejects_noncanonical_bytes() {
    assert_adoption_rejected(RemoteEntryMode::NonCanonical);
}

#[test]
fn transparency_adoption_rejects_invalid_timestamp() {
    assert_adoption_rejected(RemoteEntryMode::InvalidTimestamp);
}

#[test]
fn transparency_adopted_pair_persistence_never_leaves_new_body_with_old_signature() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("ledger-entry.json"), b"old body").unwrap();
    fs::write(
        temp.path().join("ledger-entry.json.minisig"),
        b"old signature",
    )
    .unwrap();
    assert!(
        persist_adopted_entry_with(temp.path(), b"new body", b"new signature", || {
            Err(Error::new("injected persistence interruption"))
        })
        .is_err()
    );
    assert_eq!(
        fs::read(temp.path().join("ledger-entry.json")).unwrap(),
        b"old body"
    );
    assert_eq!(
        fs::read(temp.path().join("ledger-entry.json.minisig")).unwrap(),
        b"old signature"
    );
    assert!(!temp.path().join(".adopted-ledger-entry.json.tmp").exists());
    assert!(
        !temp
            .path()
            .join(".adopted-ledger-entry.json.minisig.tmp")
            .exists()
    );
}

#[test]
fn transparency_already_published_entry_is_explicit_noop_state() {
    let tip = genesis_entry();
    let entry_bytes = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    let chain = VerifiedChain {
        pointer: Some(pointer_for(&tip)),
        pointer_bytes: None,
        pointer_etag: None,
        tip: Some(tip),
        transparency_ledger: entry_bytes.clone(),
    };
    assert!(chain_includes_entry(&chain, &entry_bytes).unwrap());
    let mut changed = entry_bytes;
    changed[0] ^= 1;
    assert!(!chain_includes_entry(&chain, &changed).unwrap());
}

#[test]
fn transparency_publish_already_published_is_exit_zero_noop() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let (chain, pair, _) = chain_with_historical_candidate(&snapshot);
    let mut probe = |_: &str| Ok(Some(pair.clone()));
    let mut signer = |_: &Path, _: &Path, _: &Path, _: &str| {
        panic!("already-published preparation must not sign")
    };
    let prepared = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap();
    assert!(prepared.already_published);
    let expected = format!(
        "already published, chain unchanged: product={PRODUCT} version={} seq={} entry_sha256={}\n",
        prepared.entry.version,
        prepared.entry.seq,
        digest(&prepared.entry_bytes)
    );
    let head_path = fixture.repo.root.path().join(TRANSPARENCY_HEAD_LOG);
    let head_before = fs::read(&head_path).unwrap();
    let mut transport = DirectoryTransport::new();
    let archive_log = Rc::new(RefCell::new(Vec::new()));
    let mut archive = FakeArchive {
        log: archive_log.clone(),
        response: None,
        retained: BTreeMap::new(),
    };
    let mut output = Vec::new();
    complete_publication(
        fixture.repo.root.path(),
        &test_config(),
        &mut transport,
        (&mut archive, &mut FakeVerifier { reject_tip: false }),
        (&chain, prepared),
        Instant::now(),
        &mut output,
    )
    .unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), expected);
    assert!(archive_log.borrow().is_empty());
    assert!(transport.log.borrow().is_empty());
    assert!(transport.destinations.is_empty());
    assert_eq!(fs::read(head_path).unwrap(), head_before);
}

#[test]
fn transparency_non_tip_staged_republish_is_chain_unchanged_noop() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let (chain, pair, historical) = chain_with_historical_candidate(&snapshot);
    stage_entry_pair(&snapshot, &historical, &pair);
    let mut signer_calls = 0;
    let mut signer = |_: &Path, _: &Path, _: &Path, _: &str| {
        signer_calls += 1;
        Ok(())
    };
    let mut probe = |version: &str| {
        assert_eq!(version, "1.0.0");
        Ok(Some(pair.clone()))
    };
    let prepared = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap();
    assert!(prepared.already_published);
    assert_eq!(prepared.entry_bytes, pair.entry_bytes);
    assert_eq!(signer_calls, 0);
}

#[test]
fn transparency_non_tip_fresh_republish_is_chain_unchanged_noop() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let (chain, pair, _) = chain_with_historical_candidate(&snapshot);
    let mut signer_calls = 0;
    let mut signer = |_: &Path, _: &Path, _: &Path, _: &str| {
        signer_calls += 1;
        Ok(())
    };
    let mut probe = |_: &str| Ok(Some(pair.clone()));
    let prepared = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap();
    assert!(prepared.already_published);
    assert_eq!(prepared.entry.seq, 1);
    assert_eq!(prepared.entry.version, "1.0.0");
    assert_eq!(signer_calls, 0);
}

#[test]
fn transparency_non_tip_republish_rejects_remote_candidate_mismatch() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let (chain, mut pair, _) = chain_with_historical_candidate(&snapshot);
    let mut entry: TransparencyEntry = serde_json::from_slice(&pair.entry_bytes).unwrap();
    entry.source_commit = "f".repeat(40);
    pair.entry_bytes = transparency_canonical_json(&serde_json::to_value(&entry).unwrap()).unwrap();
    pair.signature_bytes =
        fake_signature(&entry_trusted_comment(&entry, &digest(&pair.entry_bytes)));
    let mut signer_calls = 0;
    let mut signer = |_: &Path, _: &Path, _: &Path, _: &str| {
        signer_calls += 1;
        Ok(())
    };
    let mut probe = |_: &str| Ok(Some(pair.clone()));
    let error = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap_err()
    .to_string();
    assert!(error.starts_with("terminal: remote recorded transparency version 1.0.0 mismatch:"));
    assert!(error.contains("permanently recorded with different evidence"));
    assert_eq!(signer_calls, 0);
}

#[test]
fn transparency_first_and_tip_republish_suppress_version_probe() {
    let first_fixture = crate::proof_tests::retained_fixture();
    let first_release = first_fixture.repo.root.path().join("dist/rust");
    let mut first_probe_calls = 0;
    let mut first_probe = |_: &str| {
        first_probe_calls += 1;
        Ok(None)
    };
    let mut signer = |_: &Path, _: &Path, signature: &Path, comment: &str| {
        fs::write(signature, fake_signature(comment)).map_err(display_error)
    };
    prepare_publication(
        &first_fixture.repo.root,
        &first_release,
        &VerifiedChain {
            pointer: None,
            pointer_bytes: None,
            pointer_etag: None,
            tip: None,
            transparency_ledger: Vec::new(),
        },
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut first_probe,
    )
    .unwrap();
    assert_eq!(first_probe_calls, 0);

    let tip_fixture = crate::proof_tests::retained_fixture();
    let tip_release = tip_fixture.repo.root.path().join("dist/rust");
    let tip_snapshot = snapshot_candidate(&tip_fixture.repo.root, &tip_release).unwrap();
    let (_, pair, entry) = chain_with_historical_candidate(&tip_snapshot);
    stage_entry_pair(&tip_snapshot, &entry, &pair);
    let mut tip_probe_calls = 0;
    let mut tip_probe = |_: &str| {
        tip_probe_calls += 1;
        Ok(None)
    };
    let chain = VerifiedChain {
        pointer: Some(pointer_for(&entry)),
        pointer_bytes: None,
        pointer_etag: None,
        tip: Some(entry),
        transparency_ledger: pair.entry_bytes,
    };
    prepare_publication(
        &tip_fixture.repo.root,
        &tip_release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut tip_probe,
    )
    .unwrap();
    assert_eq!(tip_probe_calls, 0);
}

#[test]
fn transparency_publication_prompts_once_and_reuses_passphrase_for_both_signatures() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let mut probe = |_: &str| Ok(None);
    let mut reader_calls = 0;
    let mut reader = || {
        reader_calls += 1;
        Ok(b"single-passphrase".to_vec())
    };
    let mut observed = Vec::new();
    let mut signer = |_: &Path, _: &Path, signature: &Path, comment: &str, passphrase: &[u8]| {
        observed.push(passphrase.to_vec());
        fs::write(signature, fake_signature(comment)).map_err(display_error)
    };
    prepare_publication_with_signing(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut probe,
        (&mut reader, &mut signer),
    )
    .unwrap();
    assert_eq!(reader_calls, 1);
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0], b"single-passphrase");
    assert_eq!(observed[1], observed[0]);
}

#[test]
fn transparency_stale_staged_publication_preserves_bytes_and_emits_resign_directive() {
    struct ResetClock;
    impl Drop for ResetClock {
        fn drop(&mut self) {
            set_test_now(None);
        }
    }

    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let mut signer = |_: &Path, _: &Path, signature: &Path, comment: &str| {
        fs::write(signature, fake_signature(comment)).map_err(display_error)
    };
    let mut probe = |_: &str| Ok(None);
    set_test_now(Some(
        DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    let _reset = ResetClock;
    let first = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap();
    let before = [
        first.entry_bytes.clone(),
        first.entry_signature.clone(),
        first.pointer_bytes.clone(),
        first.pointer_signature.clone(),
    ];
    set_test_now(Some(
        DateTime::parse_from_rfc3339("2026-08-06T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    let second = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut FakeVerifier { reject_tip: false },
        &mut signer,
        &mut probe,
    )
    .unwrap();
    let after = [
        second.entry_bytes.clone(),
        second.entry_signature.clone(),
        second.pointer_bytes.clone(),
        second.pointer_signature.clone(),
    ];
    assert_eq!(after, before);
    let mut transport = DirectoryTransport::new();
    let mut archive = FakeArchive {
        log: transport.log.clone(),
        response: None,
        retained: BTreeMap::new(),
    };
    let mut output = Vec::new();
    complete_publication(
        fixture.repo.root.path(),
        &test_config(),
        &mut transport,
        (&mut archive, &mut FakeVerifier { reject_tip: false }),
        (&chain, second),
        Instant::now(),
        &mut output,
    )
    .unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("pointer renewal: make resign-transparency-pointer")
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
fn transparency_previous_uncommitted_head_error_names_row() {
    let fixture = crate::candidate_tests::fixture();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.into(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        seq: 7,
        version: "1.2.3".into(),
    };
    let mut bytes = transparency_canonical_json(&serde_json::to_value(row).unwrap()).unwrap();
    bytes.push(b'\n');
    fs::write(fixture.root.path().join(TRANSPARENCY_HEAD_LOG), bytes).unwrap();
    let error = validate_previous_head_committed(fixture.root.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("product=solstone-linux seq=7 version=1.2.3")
    );
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
    assert_eq!(
        append_head_row(temp.path(), &row).unwrap(),
        "witness unavailable; gap"
    );
}

#[test]
fn transparency_candidate_rejects_source_dirty_manifest() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let manifest_path = fs::read_dir(&release)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".rust-release-manifest.json"))
        })
        .unwrap();
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let commit = manifest["source_commit"].as_str().unwrap().to_owned();
    manifest["source_dirty"] = Value::Bool(true);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let error = snapshot_candidate(&fixture.repo.root, &release)
        .unwrap_err()
        .to_string();
    assert!(error.contains(&commit), "{error}");
    assert!(error.contains("source_dirty=true"), "{error}");
}

#[test]
fn transparency_candidate_rejects_rail_ledger_manifest_mismatch() {
    for (field, value) in [
        ("version", "9.9.9"),
        ("source_commit", "ffffffffffffffffffffffffffffffffffffffff"),
    ] {
        let fixture = crate::proof_tests::retained_fixture();
        let release = fixture.repo.root.path().join("dist/rust");
        let ledger_path = fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0/ledger.json");
        let mut ledger: Value = serde_json::from_slice(&fs::read(&ledger_path).unwrap()).unwrap();
        if field == "version" {
            ledger["version"] = Value::String(value.into());
        } else {
            ledger["source"]["commit"] = Value::String(value.into());
        }
        fs::write(&ledger_path, serde_json::to_vec(&ledger).unwrap()).unwrap();
        let error = snapshot_candidate(&fixture.repo.root, &release)
            .unwrap_err()
            .to_string();
        assert!(error.contains("rail ledger candidate binding"));
        assert!(error.contains(value));
    }
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

#[test]
fn transparency_retry_uses_validated_staged_bytes_not_changed_live_proofs() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let first = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let staged = first.proofs.clone();
    fs::write(
        fixture
            .repo
            .root
            .path()
            .join("dist/rust-evidence/1.0.0/proofs/tar-x86_64.json"),
        b"changed live proof",
    )
    .unwrap();
    let second = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    assert_eq!(second.proofs, staged);
}

#[test]
fn transparency_retry_revalidates_staged_candidate_before_derivation() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let snapshot = snapshot_candidate(&fixture.repo.root, &release).unwrap();
    let artifact = &snapshot.manifest.artifacts[0].path;
    fs::write(snapshot.staging.join(artifact), b"corrupt staged artifact").unwrap();
    let error = snapshot_candidate(&fixture.repo.root, &release)
        .unwrap_err()
        .to_string();
    assert!(error.contains("staged candidate artifact mismatch"));
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

fn reverse_entry_fields() -> Vec<(String, Value)> {
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
    [
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
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect()
}

fn reverse_pointer_fields() -> Vec<(String, Value)> {
    [
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
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect()
}

#[test]
fn transparency_entry_canonical_vector_matches_cross_repo_fixture() {
    let fields = reverse_entry_fields();
    assert_eq!(fields.first().unwrap().0, "version");
    assert_eq!(fields.last().unwrap().0, "artifacts");
    let bytes = transparency_canonical_object_fields(&fields).unwrap();
    assert_eq!(bytes, ENTRY_VECTOR);
    assert_eq!(bytes.len(), 611);
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "30fa37a5d4a1b254e695339b1b0dcaa7a481bb26cca92dfd888f8186f049599f"
    );
}

#[test]
fn transparency_pointer_canonical_vector_matches_cross_repo_fixture() {
    let fields = reverse_pointer_fields();
    assert_eq!(fields.first().unwrap().0, "version");
    assert_eq!(fields.last().unwrap().0, "chain_length");
    let bytes = transparency_canonical_object_fields(&fields).unwrap();
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
fn transparency_entry_rejects_rail_compatible_but_schema_invalid_64_hex_commit() {
    let mut entry = genesis_entry();
    entry.source_commit = "a".repeat(64);
    assert!(validate_entry(&entry, None).is_err());
}

#[test]
fn transparency_entry_rejects_unsorted_inventory_names() {
    let artifact = |name: &str| TransparencyArtifact {
        bytes: 1,
        name: name.into(),
        sha256: "a".repeat(64),
    };
    let named = |name: &str| TransparencyNamedDigest {
        name: name.into(),
        sha256: "b".repeat(64),
    };
    for inventory in ["artifacts", "manifests", "proofs"] {
        let mut entry = genesis_entry();
        match inventory {
            "artifacts" => entry.artifacts = vec![artifact("z"), artifact("a")],
            "manifests" => entry.manifests = vec![named("z"), named("a")],
            "proofs" => entry.proofs = vec![named("z"), named("a")],
            _ => unreachable!(),
        }
        let error = validate_entry(&entry, None).unwrap_err().to_string();
        assert!(error.contains("transparency inventory order"));
    }
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

#[test]
fn transparency_pointer_requires_exact_fourteen_day_validity() {
    let tip = genesis_entry();
    for (valid_until, accepted) in [
        ("2026-08-04T00:00:00Z", false),
        ("2026-08-05T00:00:00Z", true),
        ("2026-08-06T00:00:00Z", false),
    ] {
        let mut pointer = pointer_for(&tip);
        pointer.valid_until = valid_until.into();
        assert_eq!(validate_pointer(&pointer, &tip).is_ok(), accepted);
    }
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
    let pointer = pointer_for(&tip);
    let mut transport = QueueTransport {
        responses: verified_chain_responses(&pointer, &tip),
    };
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let chain = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap();
    assert_eq!(digest(&chain.transparency_ledger), pointer.tip_sha256);
    assert!(transport.responses.is_empty());
}

#[test]
fn transparency_ledger_contradicting_locked_entry_fails() {
    let tip = genesis_entry();
    let pointer = pointer_for(&tip);
    let mut contradictory = tip.clone();
    contradictory.version = "9.9.9".into();
    let bytes = transparency_canonical_json(&serde_json::to_value(contradictory).unwrap()).unwrap();
    let mut responses = verified_chain_responses(&pointer, &tip);
    *responses.back_mut().unwrap() = response(bytes);
    let mut transport = QueueTransport { responses };
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let error = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("locked entry mismatch"));
}

#[test]
fn transparency_ledger_contradiction_before_malformed_suffix_fails_closed() {
    let tip = genesis_entry();
    let locked = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    let mut contradictory = tip.clone();
    contradictory.version = "9.9.9".into();
    let mut fetched =
        transparency_canonical_json(&serde_json::to_value(contradictory).unwrap()).unwrap();
    fetched.extend_from_slice(b"{malformed");
    let error = reject_locked_ledger_contradiction(&fetched, &locked)
        .unwrap_err()
        .to_string();
    assert!(error.contains("transparency ledger locked entry mismatch"));
}

#[test]
fn transparency_missing_ledger_is_rederivable() {
    let tip = genesis_entry();
    let pointer = pointer_for(&tip);
    let mut responses = verified_chain_responses(&pointer, &tip);
    *responses.back_mut().unwrap() = TransportResponse {
        http_status: 404,
        body: b"missing".to_vec(),
        etag: None,
        process_exit: 0,
    };
    let mut transport = QueueTransport { responses };
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let chain = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap();
    assert_eq!(
        chain.transparency_ledger,
        transparency_canonical_json(&serde_json::to_value(tip).unwrap()).unwrap()
    );
}

#[test]
fn transparency_superset_ledger_that_chains_is_rederived_to_locked_tip() {
    let tip = genesis_entry();
    let pointer = pointer_for(&tip);
    let tip_bytes = transparency_canonical_json(&serde_json::to_value(&tip).unwrap()).unwrap();
    let mut extra = tip.clone();
    extra.seq = 2;
    extra.version = "1.0.1".into();
    extra.prev_version = tip.version.clone();
    extra.prev_sha256 = digest(&tip_bytes);
    extra.published_utc = "2026-07-22T00:00:01Z".into();
    let extra_bytes = transparency_canonical_json(&serde_json::to_value(extra).unwrap()).unwrap();
    let mut responses = verified_chain_responses(&pointer, &tip);
    *responses.back_mut().unwrap() = response([tip_bytes.as_slice(), &extra_bytes].concat());
    let mut transport = QueueTransport { responses };
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), b"").unwrap();
    let chain = fetch_verified_chain(
        temp.path(),
        &test_config(),
        &mut transport,
        &mut FakeVerifier { reject_tip: false },
        false,
    )
    .unwrap();
    assert_eq!(chain.transparency_ledger, tip_bytes);
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
fn transparency_head_log_rejects_foreign_product() {
    let temp = tempfile::tempdir().unwrap();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: "foreign-product".into(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        seq: 1,
        version: "1.0.0".into(),
    };
    let mut bytes = transparency_canonical_json(&serde_json::to_value(row).unwrap()).unwrap();
    bytes.push(b'\n');
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), bytes).unwrap();
    let mut config = test_config();
    config.genesis = true;
    let error = fetch_verified_chain(
        temp.path(),
        &config,
        &mut DirectoryTransport::new(),
        &mut FakeVerifier { reject_tip: false },
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("transparency head log product"));
    assert!(error.contains("foreign-product"));
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
fn transparency_genesis_rejects_nonzero_local_head() {
    let temp = tempfile::tempdir().unwrap();
    let row = TransparencyHeadRow {
        entry_sha256: "a".repeat(64),
        product: PRODUCT.into(),
        published_utc: "2026-07-22T00:00:00Z".into(),
        seq: 3,
        version: "1.0.2".into(),
    };
    let mut bytes = transparency_canonical_json(&serde_json::to_value(row).unwrap()).unwrap();
    bytes.push(b'\n');
    fs::write(temp.path().join(TRANSPARENCY_HEAD_LOG), bytes).unwrap();
    let mut config = test_config();
    config.genesis = true;
    let error = fetch_verified_chain(
        temp.path(),
        &config,
        &mut DirectoryTransport::new(),
        &mut FakeVerifier { reject_tip: false },
        true,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("transparency genesis head log"));
    assert!(error.contains("local highest seq 3"));
    assert!(error.contains("wiped or wrong bucket"));
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
    let now = DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let (renewed, bytes) = renew_pointer(&old, now).unwrap();
    assert_eq!(renewed.chain_length, old.chain_length);
    assert_eq!(renewed.tip_sha256, old.tip_sha256);
    assert_eq!(renewed.version, old.version);
    let decoded: TransparencyPointer = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, renewed);
}

#[test]
fn transparency_resign_uses_injected_clock_without_changing_tip() {
    struct ResetClock;
    impl Drop for ResetClock {
        fn drop(&mut self) {
            set_test_now(None);
        }
    }

    let old = pointer_for(&genesis_entry());
    let now = DateTime::parse_from_rfc3339("2026-07-30T12:34:56Z")
        .unwrap()
        .with_timezone(&Utc);
    set_test_now(Some(now));
    let _reset = ResetClock;
    let (renewed, _) = renew_pointer_now(&old).unwrap();
    assert_eq!(renewed.signed_at, "2026-07-30T12:34:56Z");
    assert_eq!(renewed.valid_until, "2026-08-13T12:34:56Z");
    assert_eq!(renewed.chain_length, old.chain_length);
    assert_eq!(renewed.version, old.version);
    assert_eq!(renewed.tip_sha256, old.tip_sha256);
}

#[test]
fn transparency_deterministic_entry_and_pointer_bytes_ignore_later_clock() {
    let fixture = crate::proof_tests::retained_fixture();
    let release = fixture.repo.root.path().join("dist/rust");
    let chain = VerifiedChain {
        pointer: None,
        pointer_bytes: None,
        pointer_etag: None,
        tip: None,
        transparency_ledger: Vec::new(),
    };
    let mut verifier = FakeVerifier { reject_tip: false };
    let mut signer = |_: &Path, _: &Path, signature: &Path, comment: &str| {
        fs::write(signature, fake_signature(comment)).map_err(display_error)
    };
    let mut probe = |_: &str| Ok(None);
    set_test_now(Some(
        DateTime::parse_from_rfc3339("2026-07-22T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    configure_test_operation_seam(None);
    let first = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut verifier,
        &mut signer,
        &mut probe,
    )
    .unwrap();
    let mut transport = DirectoryTransport::new();
    let mut archive = FakeArchive {
        log: transport.log.clone(),
        response: Some(ArchiveResponse {
            exit_status: 9,
            stdout: Vec::new(),
            stderr: b"injected interruption after staging".to_vec(),
        }),
        retained: BTreeMap::new(),
    };
    assert!(
        upload_publication(
            &test_config(),
            &mut transport,
            &mut archive,
            &mut verifier,
            &first.snapshot.staging,
            &StagedPublication {
                staging: &first.snapshot.staging,
                chain: &chain,
                entry: &first.entry,
                entry_bytes: &first.entry_bytes,
                entry_signature: &first.entry_signature,
                pointer_bytes: &first.pointer_bytes,
                pointer_signature: &first.pointer_signature,
                manifest: &first.snapshot.manifest,
                proofs: &first.snapshot.proofs,
            },
        )
        .is_err()
    );
    set_test_now(Some(
        DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    ));
    let second = prepare_publication(
        &fixture.repo.root,
        &release,
        &chain,
        Path::new("fixture.pub"),
        &mut verifier,
        &mut signer,
        &mut probe,
    )
    .unwrap();
    assert_eq!(second.entry_bytes, first.entry_bytes);
    assert_eq!(second.entry_signature, first.entry_signature);
    assert_eq!(second.pointer_signature, first.pointer_signature);
    archive.response = None;
    upload_publication(
        &test_config(),
        &mut transport,
        &mut archive,
        &mut verifier,
        &second.snapshot.staging,
        &StagedPublication {
            staging: &second.snapshot.staging,
            chain: &chain,
            entry: &second.entry,
            entry_bytes: &second.entry_bytes,
            entry_signature: &second.entry_signature,
            pointer_bytes: &second.pointer_bytes,
            pointer_signature: &second.pointer_signature,
            manifest: &second.snapshot.manifest,
            proofs: &second.snapshot.proofs,
        },
    )
    .unwrap();
    set_test_now(None);
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

#[test]
fn transparency_public_key_filename_matches_cross_repo_contract() {
    assert_eq!(
        TRANSPARENCY_PUBLIC_KEY_FILENAME,
        "solpbc-transparency-1.pub"
    );
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
