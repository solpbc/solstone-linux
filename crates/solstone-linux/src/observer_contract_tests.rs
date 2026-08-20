// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config,
    private_link::{PrivateLinkOwner, start_registered_private_link_for_test},
    private_link_test_peer::{PeerRequest, PrivateLinkPeer},
    sync::{contract_segment_proven_held, contract_sha256_file},
    sync_health::ErrorType,
    upload::{ListingEntry, UploadClient, contract_parse_listing},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, symlink},
    path::{Component, Path, PathBuf},
};
use tempfile::TempDir;

const MANIFEST_SHA256: &str = "9b3bcd6b7f8a83adb9007e32501af44403cb93cfb8d80f256b6a7b5b9f93057e";
const AUTHORITY_COMMIT: &str = "766021cd44d4a0a7ce471d2affb461bf3ce0fc39";

const FULL_FIXTURES: &[&str] = &[
    "declared.observer.ingestSegments.custody_unknown_rejected",
    "declared.observer.ingestSegments.envelope_total_mismatch",
    "declared.observer.ingestUpload.status_unknown_rejected",
    "example.callosum.rootEvents.response.200.text-event-stream.default",
    "example.link.pair.request.body.application-json.default",
    "example.link.pair.response.200.application-json.default",
    "example.observer.callosumStream.response.200.text-event-stream.default",
    "example.observer.ingestEvent.request.body.application-json.default",
    "example.observer.ingestEvent.response.200.application-json.default",
    "example.observer.ingestSegments.response.200.application-json.legacy",
    "example.observer.ingestSegments.response.200.application-json.v2",
    "example.observer.ingestUpload.request.body.multipart-form-data.default",
    "example.observer.ingestUpload.response.200.application-json.duplicate",
    "example.observer.ingestUpload.response.200.application-json.normal",
    "example.observer.register.request.body.application-json.default",
    "example.observer.register.response.200.application-json.default",
    "recorded.auth.bearer.segments",
    "recorded.auth.handle.segments",
    "recorded.ingestUpload.collision",
    "recorded.ingestUpload.conflict",
    "recorded.ingestUpload.duplicate",
    "recorded.ingestUpload.failed",
    "recorded.ingestUpload.ok",
    "recorded.segments.custody_statuses",
    "recorded.segments.legacy.absent_header",
    "recorded.segments.legacy.unparseable_header",
    "recorded.segments.submitted_name_omitted",
    "recorded.segments.v2.envelope",
    "recorded.sse.observer.data",
    "recorded.sse.observer.error",
    "recorded.sse.observer.heartbeat",
    "recorded.sse.root.data_unknown_event",
    "recorded.sse.root.heartbeat",
];

const LINUX_FIXTURES: &[&str] = &[
    "declared.observer.ingestSegments.custody_unknown_rejected",
    "declared.observer.ingestSegments.envelope_total_mismatch",
    "declared.observer.ingestUpload.status_unknown_rejected",
    "example.observer.ingestEvent.request.body.application-json.default",
    "example.observer.ingestEvent.response.200.application-json.default",
    "example.observer.ingestSegments.response.200.application-json.legacy",
    "example.observer.ingestSegments.response.200.application-json.v2",
    "example.observer.ingestUpload.request.body.multipart-form-data.default",
    "example.observer.ingestUpload.response.200.application-json.duplicate",
    "example.observer.ingestUpload.response.200.application-json.normal",
    "example.observer.register.request.body.application-json.default",
    "example.observer.register.response.200.application-json.default",
    "recorded.auth.bearer.segments",
    "recorded.auth.handle.segments",
    "recorded.ingestUpload.collision",
    "recorded.ingestUpload.conflict",
    "recorded.ingestUpload.duplicate",
    "recorded.ingestUpload.failed",
    "recorded.ingestUpload.ok",
    "recorded.segments.custody_statuses",
    "recorded.segments.legacy.absent_header",
    "recorded.segments.legacy.unparseable_header",
    "recorded.segments.submitted_name_omitted",
    "recorded.segments.v2.envelope",
];

const FULL_VECTORS: &[&str] = &[
    "callosum.rootEvents.sse.data_unknown_event",
    "callosum.rootEvents.sse.heartbeat",
    "observer.auth.bearer",
    "observer.auth.handle",
    "observer.callosumStream.sse.data",
    "observer.callosumStream.sse.error",
    "observer.callosumStream.sse.heartbeat",
    "observer.ingestSegments.custody_statuses",
    "observer.ingestSegments.custody_unknown_rejected",
    "observer.ingestSegments.envelope_total_mismatch",
    "observer.ingestSegments.legacy_array.absent_header",
    "observer.ingestSegments.legacy_array.unparseable_header",
    "observer.ingestSegments.submitted_name_fallback",
    "observer.ingestSegments.v2_envelope",
    "observer.ingestUpload.status.collision",
    "observer.ingestUpload.status.conflict",
    "observer.ingestUpload.status.duplicate",
    "observer.ingestUpload.status.failed",
    "observer.ingestUpload.status.ok",
    "observer.ingestUpload.status_unknown_rejected",
];

const LINUX_VECTORS: &[&str] = &[
    "observer.auth.bearer",
    "observer.auth.handle",
    "observer.ingestSegments.custody_statuses",
    "observer.ingestSegments.custody_unknown_rejected",
    "observer.ingestSegments.envelope_total_mismatch",
    "observer.ingestSegments.legacy_array.absent_header",
    "observer.ingestSegments.legacy_array.unparseable_header",
    "observer.ingestSegments.submitted_name_fallback",
    "observer.ingestSegments.v2_envelope",
    "observer.ingestUpload.status.collision",
    "observer.ingestUpload.status.conflict",
    "observer.ingestUpload.status.duplicate",
    "observer.ingestUpload.status.failed",
    "observer.ingestUpload.status.ok",
    "observer.ingestUpload.status_unknown_rejected",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn portable_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with(['/', '\\'])
        || path.contains('\\')
        || path.chars().any(|value| value.is_control())
    {
        return Err(format!("unsafe path: {path:?}"));
    }
    let candidate = Path::new(path);
    if candidate
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("non-relative path: {path:?}"));
    }
    for part in path.split('/') {
        if part.is_empty()
            || part.ends_with(['.', ' '])
            || part.contains(['<', '>', ':', '"', '|', '?', '*'])
        {
            return Err(format!("non-portable path: {path:?}"));
        }
        let stem = part
            .split('.')
            .next()
            .unwrap()
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
                });
        if reserved {
            return Err(format!("reserved path: {path:?}"));
        }
    }
    Ok(())
}

fn walk(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<String>,
    directories: &mut BTreeSet<String>,
    folded: &mut BTreeSet<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(current).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .ok_or("non-UTF-8 path")?
            .to_owned();
        portable_path(&relative)?;
        if !folded.insert(relative.to_ascii_lowercase()) {
            return Err(format!("case-colliding tree path: {relative}"));
        }
        if metadata.file_type().is_symlink() {
            return Err(format!("symlink: {relative}"));
        }
        if metadata.is_dir() {
            if !directories.insert(relative.clone()) {
                return Err(format!("duplicate directory: {relative}"));
            }
            walk(root, &entry.path(), files, directories, folded)?;
        } else if metadata.is_file() {
            if metadata.mode() & 0o7111 != 0 {
                return Err(format!("executable or special mode: {relative}"));
            }
            if !files.insert(relative.clone()) {
                return Err(format!("duplicate file: {relative}"));
            }
        } else if metadata.file_type().is_socket()
            || metadata.file_type().is_fifo()
            || metadata.file_type().is_block_device()
            || metadata.file_type().is_char_device()
        {
            return Err(format!("special file: {relative}"));
        } else {
            return Err(format!("unsupported file: {relative}"));
        }
    }
    Ok(())
}

fn verify_bundle(root: &Path, expected_manifest_digest: &str) -> Result<Value, String> {
    let root_metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("bundle root is not a no-follow directory".to_owned());
    }
    let manifest_metadata =
        fs::symlink_metadata(root.join("manifest.json")).map_err(|error| error.to_string())?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err("manifest is not a no-follow regular file".to_owned());
    }
    let mut actual = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut actual_folded = BTreeSet::new();
    walk(
        root,
        root,
        &mut actual,
        &mut directories,
        &mut actual_folded,
    )?;
    if directories != BTreeSet::from(["fixtures".to_owned()]) {
        return Err(format!("directory inventory mismatch: {directories:?}"));
    }
    let manifest_bytes = fs::read(root.join("manifest.json")).map_err(|error| error.to_string())?;
    if digest(&manifest_bytes) != expected_manifest_digest {
        return Err("manifest digest mismatch".to_owned());
    }
    let manifest: Value =
        serde_json::from_slice(&manifest_bytes).map_err(|error| error.to_string())?;
    let entries = manifest["files"]
        .as_array()
        .ok_or("manifest files missing")?;
    let mut expected = BTreeSet::from(["manifest.json".to_owned()]);
    let mut folded = BTreeSet::from(["manifest.json".to_owned()]);
    for entry in entries {
        let path = entry["path"].as_str().ok_or("manifest path missing")?;
        portable_path(path)?;
        if !expected.insert(path.to_owned()) || !folded.insert(path.to_ascii_lowercase()) {
            return Err(format!("duplicate or case-colliding path: {path}"));
        }
        let bytes = fs::read(root.join(path)).map_err(|error| error.to_string())?;
        if digest(&bytes) != entry["sha256"].as_str().ok_or("file digest missing")? {
            return Err(format!("file digest mismatch: {path}"));
        }
    }
    if actual != expected {
        return Err(format!("inventory mismatch: {actual:?} != {expected:?}"));
    }
    Ok(manifest)
}

fn load_index(path: &Path, key: &str) -> BTreeMap<String, Value> {
    let body: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut indexed = BTreeMap::new();
    for item in body[key].as_array().unwrap() {
        let id = item["id"].as_str().unwrap().to_owned();
        assert!(
            indexed.insert(id.clone(), item.clone()).is_none(),
            "duplicate {id}"
        );
    }
    indexed
}

fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn record(executed: &mut BTreeSet<String>, id: &str, passed: bool) {
    assert!(passed, "production-path assertion failed for {id}");
    assert!(
        executed.insert(id.to_owned()),
        "duplicate coverage for {id}"
    );
}

fn assert_identities(
    manifest: &Value,
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
) {
    assert_eq!(manifest["bundle_semver"], "8.0.0");
    assert_eq!(manifest["openapi_document_version"], "1.0.0");
    assert_eq!(
        manifest["generator_identity"],
        "solstone.convey.contract.observer_bundle.v1"
    );
    assert_eq!(
        manifest["schema_dialect_uri"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        manifest["bundle_schema_identity"],
        "solstone.observer-client-contract-bundle.schema.v1"
    );
    assert_eq!(manifest["observer_protocol_version"], 2);
    assert_eq!(manifest["supported_response_variants"], json!([1, 2]));
    assert_eq!(
        manifest["consumer_identifiers"],
        json!([
            "solstone-android",
            "solstone-browser",
            "solstone-linux",
            "solstone-macos",
            "solstone-swift",
            "solstone-tmux",
            "solstone-windows"
        ])
    );
    let targets: BTreeSet<_> = manifest["windows_linux_rollout_targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["consumer_identifier"].as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from(["solstone-linux", "solstone-windows"])
    );
    assert_eq!(
        manifest["operation_ids"],
        json!([
            "callosum.rootEvents",
            "link.pair",
            "observer.callosumStream",
            "observer.ingestEvent",
            "observer.ingestSegments",
            "observer.ingestUpload",
            "observer.register"
        ])
    );
    assert_eq!(
        fixtures.keys().cloned().collect::<BTreeSet<_>>(),
        set(FULL_FIXTURES)
    );
    assert_eq!(
        vectors.keys().cloned().collect::<BTreeSet<_>>(),
        set(FULL_VECTORS)
    );
    for vector in vectors.values() {
        assert!(fixtures.contains_key(vector["fixture_id"].as_str().unwrap()));
    }
}

fn verify_provenance(root: &Path) -> Result<(), String> {
    let value: Value = serde_json::from_slice(&fs::read(root).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let expected = json!({
        "authority_repository":"https://github.com/solpbc/solstone-journal",
        "authority_commit":AUTHORITY_COMMIT,
        "bundle_version":"8.0.0",
        "manifest_path":"manifest.json",
        "manifest_sha256":MANIFEST_SHA256,
        "vendored_root":"vendor/observer-client-contract"
    });
    if value != expected {
        return Err("provenance mismatch".to_owned());
    }
    Ok(())
}

fn config(temp: &TempDir) -> Config {
    Config {
        stream: "desktop".into(),
        sync_retry_delays: vec![0],
        sync_max_retries: 1,
        base_dir: temp.path().join("data"),
        config_dir: temp.path().join("config"),
        ..Config::default()
    }
}

fn client(config: &Config, capability: crate::private_link::PrivateLinkCapability) -> UploadClient {
    UploadClient::new(
        config,
        capability,
        "archon",
        "linux",
        "1.4.0",
        std::sync::Arc::new(crate::test_support::MutableClock::new(0.0, 0.0)),
    )
}

struct LinkedHarness {
    peer: PrivateLinkPeer,
    owner: PrivateLinkOwner,
    client: UploadClient,
}

impl LinkedHarness {
    async fn start(temp: &TempDir) -> Self {
        let peer = PrivateLinkPeer::start().await;
        let (_state, owner) = start_registered_private_link_for_test(
            peer.credential(),
            "desktop",
            "K",
            "/app/devices/ingest",
        )
        .await;
        let client = client(&config(temp), owner.capability());
        Self {
            peer,
            owner,
            client,
        }
    }

    async fn finish(self) {
        self.owner.shutdown().await.unwrap();
        self.peer.shutdown().await;
    }
}

fn header<'a>(request: &'a PeerRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn assert_upload_contract(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    let cases = [
        (
            "recorded.ingestUpload.ok",
            "observer.ingestUpload.status.ok",
        ),
        (
            "recorded.ingestUpload.collision",
            "observer.ingestUpload.status.collision",
        ),
        (
            "recorded.ingestUpload.duplicate",
            "observer.ingestUpload.status.duplicate",
        ),
        (
            "recorded.ingestUpload.conflict",
            "observer.ingestUpload.status.conflict",
        ),
        (
            "recorded.ingestUpload.failed",
            "observer.ingestUpload.status.failed",
        ),
        (
            "declared.observer.ingestUpload.status_unknown_rejected",
            "observer.ingestUpload.status_unknown_rejected",
        ),
    ];
    for (fixture_id, vector_id) in cases {
        let fixture = &fixtures[fixture_id];
        let vector = &vectors[vector_id];
        assert_eq!(vector["fixture_id"], fixture_id);
        let status = vector["observed_status"]
            .as_u64()
            .or_else(|| fixture["provenance"]["status"].as_u64())
            .expect("authority observed HTTP status") as u16;
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("audio.flac");
        fs::write(&media, b"audio").unwrap();
        let harness = LinkedHarness::start(&temp).await;
        harness
            .peer
            .enqueue_response(status, fixture["payload"].to_string());
        let result = harness
            .client
            .upload_segment("20260618", "143022_300", &[media])
            .await;
        let accepted = vector["decision"]["accepted"].as_bool().unwrap_or_else(|| {
            assert_eq!(vector["decision"]["unknown_value_behavior"], "reject");
            false
        });
        assert_eq!(result.success, accepted);
        if accepted {
            let duplicate = vector["decision"]["stored_key_source"] == "existing_segment";
            assert_eq!(result.duplicate, duplicate);
            let source = vector["decision"]["stored_key_source"].as_str().unwrap();
            assert_eq!(
                result.stored_key.as_deref(),
                fixture["payload"][source].as_str()
            );
        } else if status == 200 {
            assert_eq!(result.error_type, Some(ErrorType::Incompatible));
            assert_eq!(harness.peer.requests().len(), 1);
        } else {
            assert!(!result.success && result.stored_key.is_none());
        }
        record(executed_fixtures, fixture_id, true);
        record(executed_vectors, vector_id, true);
        harness.finish().await;
    }
    for fixture_id in [
        "example.observer.ingestUpload.response.200.application-json.normal",
        "example.observer.ingestUpload.response.200.application-json.duplicate",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("audio.flac");
        fs::write(&media, b"audio").unwrap();
        let harness = LinkedHarness::start(&temp).await;
        harness
            .peer
            .enqueue_response(200, fixtures[fixture_id]["payload"].to_string());
        assert!(
            harness
                .client
                .upload_segment("20260618", "143022_300", &[media])
                .await
                .success
        );
        record(executed_fixtures, fixture_id, true);
        harness.finish().await;
    }
    let fixture_id = "example.observer.ingestUpload.request.body.multipart-form-data.default";
    let temp = tempfile::tempdir().unwrap();
    let fixture_files = fixtures[fixture_id]["payload"]["files"]
        .as_array()
        .expect("multipart fixture files");
    let media: Vec<_> = fixture_files
        .iter()
        .map(|name| {
            let path = temp.path().join(name.as_str().expect("fixture filename"));
            fs::write(&path, format!("fixture bytes for {}", path.display())).unwrap();
            path
        })
        .collect();
    let harness = LinkedHarness::start(&temp).await;
    harness.peer.enqueue_response(
        200,
        json!({"status":"ok","segment":"143022_300"}).to_string(),
    );
    harness
        .client
        .upload_segment(
            fixtures[fixture_id]["payload"]["day"].as_str().unwrap(),
            fixtures[fixture_id]["payload"]["segment"].as_str().unwrap(),
            &media,
        )
        .await;
    let requests = harness.peer.requests();
    let request = &requests[0];
    let body = String::from_utf8_lossy(&request.body);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/app/devices/ingest");
    assert!(header(request, "authorization").is_some_and(|value| value.starts_with("Bearer ")));
    assert_eq!(body.matches("name=\"day\"").count(), 1);
    assert_eq!(body.matches("name=\"segment\"").count(), 1);
    assert_eq!(body.matches("name=\"files\"").count(), fixture_files.len());
    assert!(body.contains("\r\n\r\n20260618\r\n"));
    assert!(body.contains("\r\n\r\n143022_300\r\n"));
    for name in fixture_files {
        let name = name.as_str().unwrap();
        assert!(body.contains(&format!("name=\"files\"; filename=\"{name}\"")));
        let content_type = if name.ends_with(".flac") {
            "audio/flac"
        } else {
            "application/octet-stream"
        };
        assert!(body.contains(&format!(
            "filename=\"{name}\"\r\nContent-Type: {content_type}"
        )));
    }
    for forbidden in ["name=\"host\"", "name=\"meta\"", "name=\"platform\""] {
        assert!(!body.contains(forbidden));
    }
    record(executed_fixtures, fixture_id, true);
    harness.finish().await;
}

async fn assert_listing_contract(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    let cases = [
        (
            "example.observer.ingestSegments.response.200.application-json.legacy",
            None,
        ),
        (
            "example.observer.ingestSegments.response.200.application-json.v2",
            None,
        ),
        (
            "recorded.segments.legacy.absent_header",
            Some("observer.ingestSegments.legacy_array.absent_header"),
        ),
        (
            "recorded.segments.legacy.unparseable_header",
            Some("observer.ingestSegments.legacy_array.unparseable_header"),
        ),
        (
            "recorded.segments.v2.envelope",
            Some("observer.ingestSegments.v2_envelope"),
        ),
        (
            "declared.observer.ingestSegments.envelope_total_mismatch",
            Some("observer.ingestSegments.envelope_total_mismatch"),
        ),
        (
            "recorded.auth.bearer.segments",
            Some("observer.auth.bearer"),
        ),
        (
            "recorded.auth.handle.segments",
            Some("observer.auth.handle"),
        ),
    ];
    for (fixture_id, vector_id) in cases {
        let payload = &fixtures[fixture_id]["payload"];
        let (legacy, truncated) = if let Some(vector_id) = vector_id {
            let vector = &vectors[vector_id];
            assert_eq!(vector["fixture_id"], fixture_id);
            let decision = &vector["decision"];
            let legacy = decision["response_variant"]
                .as_str()
                .map_or_else(|| payload.is_array(), |variant| variant == "legacy_array");
            let truncated = decision["valid"].as_bool().map_or_else(
                || {
                    payload["total"].as_u64().is_some_and(|total| {
                        total != payload["items"].as_array().expect("listing items").len() as u64
                    })
                },
                |valid| !valid,
            );
            (legacy, truncated)
        } else {
            (
                payload.is_array(),
                payload["total"].as_u64().is_some_and(|total| {
                    total != payload["items"].as_array().expect("listing items").len() as u64
                }),
            )
        };
        let temp = tempfile::tempdir().unwrap();
        let harness = LinkedHarness::start(&temp).await;
        let response_headers = vector_id
            .and_then(|id| vectors[id]["decision"]["header"].as_str())
            .map_or_else(
                || vec![("x-solstone-protocol-version".to_owned(), "2".to_owned())],
                |header| match header {
                    "absent" => Vec::new(),
                    "unparseable" => vec![(
                        "x-solstone-protocol-version".to_owned(),
                        "unparseable".to_owned(),
                    )],
                    value => vec![("x-solstone-protocol-version".to_owned(), value.to_owned())],
                },
            );
        harness
            .peer
            .enqueue_response_with_headers(200, response_headers, payload.to_string());
        let result = harness.client.get_server_segments("20260618").await;
        assert_eq!(
            (result.legacy, result.truncated),
            (legacy, truncated),
            "{fixture_id}"
        );
        let requests = harness.peer.requests();
        let request = &requests[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/app/devices/ingest/segments/20260618");
        assert_eq!(header(request, "x-solstone-protocol-version"), Some("2"));
        let authorization = header(request, "authorization");
        assert!(authorization.is_some_and(|value| value.starts_with("Bearer ")));
        record(executed_fixtures, fixture_id, true);
        if let Some(vector_id) = vector_id {
            let decision = &vectors[vector_id]["decision"];
            if vector_id.starts_with("observer.auth.") {
                assert!(
                    decision["accepted"]
                        .as_bool()
                        .expect("authority auth accepted")
                );
                let form = decision["auth_form"].as_str().expect("authority auth form");
                assert!(matches!(
                    form,
                    "authorization_bearer" | "x_solstone_observer"
                ));
                assert!(
                    vectors["observer.auth.bearer"]["decision"]["accepted"]
                        .as_bool()
                        .expect("authority bearer accepted")
                );
                assert_eq!(
                    vectors["observer.auth.bearer"]["decision"]["auth_form"],
                    "authorization_bearer"
                );
            }
            record(executed_vectors, vector_id, true);
        }
        harness.finish().await;
    }

    let custody_cases = [
        (
            "recorded.segments.custody_statuses",
            "observer.ingestSegments.custody_statuses",
        ),
        (
            "declared.observer.ingestSegments.custody_unknown_rejected",
            "observer.ingestSegments.custody_unknown_rejected",
        ),
        (
            "recorded.segments.submitted_name_omitted",
            "observer.ingestSegments.submitted_name_fallback",
        ),
    ];
    for (fixture_id, vector_id) in custody_cases {
        let query = contract_parse_listing(fixtures[fixture_id]["payload"].clone(), 200);
        let entries = query.segments.expect("fixture listing entries");
        let vector = &vectors[vector_id];
        assert_eq!(vector["fixture_id"], fixture_id);
        let decision = &vector["decision"];
        let temp = tempfile::tempdir().unwrap();
        let mut asserted = false;
        for entry in entries {
            for (index, remote) in entry
                .files
                .expect("fixture listing files")
                .into_iter()
                .enumerate()
            {
                let segment = temp.path().join(format!("segment-{index}"));
                fs::create_dir(&segment).unwrap();
                let filename = remote.name.as_deref().expect("fixture remote name");
                let local = segment.join(filename);
                fs::write(&local, format!("fixture custody bytes {filename}")).unwrap();
                let mut matching = remote.clone();
                matching.sha256 = Some(contract_sha256_file(&local).unwrap());
                let candidate = ListingEntry {
                    key: entry.key.clone(),
                    original_key: entry.original_key.clone(),
                    files: Some(vec![matching.clone()]),
                };
                let status = matching.status.as_deref().expect("fixture custody status");
                let expected = if decision["fallback"] == "name" {
                    true
                } else if let Some(holding) = decision["holding_by_status"][status].as_str() {
                    holding == "held"
                } else {
                    assert_eq!(decision["unknown_status"], "reject");
                    false
                };
                assert_eq!(contract_segment_proven_held(&segment, &candidate), expected);
                asserted = true;

                if decision["fallback"] == "name" {
                    assert!(
                        !decision["submitted_name_present"]
                            .as_bool()
                            .expect("authority submitted_name_present")
                    );
                    assert!(contract_segment_proven_held(&segment, &candidate));
                    matching.submitted_name = Some("different.flac".to_owned());
                    let precedence = ListingEntry {
                        files: Some(vec![matching.clone()]),
                        ..candidate.clone()
                    };
                    assert!(!contract_segment_proven_held(&segment, &precedence));
                    matching.submitted_name = matching.name.clone();
                    let precedence = ListingEntry {
                        files: Some(vec![matching]),
                        ..candidate
                    };
                    assert!(contract_segment_proven_held(&segment, &precedence));
                }
            }
        }
        record(executed_fixtures, fixture_id, asserted);
        record(executed_vectors, vector_id, asserted);
    }
}

async fn assert_event_and_register(
    fixtures: &BTreeMap<String, Value>,
    executed: &mut BTreeSet<String>,
) {
    let temp = tempfile::tempdir().unwrap();
    let event_id = "example.observer.ingestEvent.request.body.application-json.default";
    let harness = LinkedHarness::start(&temp).await;
    harness.peer.enqueue_response(
        200,
        fixtures["example.observer.ingestEvent.response.200.application-json.default"]["payload"]
            .to_string(),
    );
    let payload = fixtures[event_id]["payload"].as_object().unwrap();
    let mut fields = payload.clone();
    let tract = fields.remove("tract").unwrap();
    let event = fields.remove("event").unwrap();
    assert!(
        harness
            .client
            .relay_event(tract.as_str().unwrap(), event.as_str().unwrap(), fields)
            .await
    );
    let requests = harness.peer.requests();
    let request = &requests[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/app/devices/ingest/event")
    );
    assert_eq!(header(request, "content-type"), Some("application/json"));
    assert_eq!(
        request.body,
        br#"{"event":"status","state":"recording","tract":"observe"}"#
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        fixtures[event_id]["payload"]
    );
    record(executed, event_id, true);
    record(
        executed,
        "example.observer.ingestEvent.response.200.application-json.default",
        true,
    );
    harness.finish().await;
    let register_request = "example.observer.register.request.body.application-json.default";
    let register_response = "example.observer.register.response.200.application-json.default";
    let peer = PrivateLinkPeer::start().await;
    peer.enqueue_response(200, fixtures[register_response]["payload"].to_string());
    let label = fixtures[register_request]["payload"]["label"]
        .as_str()
        .unwrap();
    let (_state, owner) = start_registered_private_link_for_test(
        peer.credential(),
        label,
        "K",
        "/app/devices/ingest",
    )
    .await;
    assert!(matches!(
        owner
            .register_for_test(&fixtures[register_request]["payload"])
            .await,
        crate::private_link::LinkOutcome::Success { .. }
    ));
    let requests = peer.requests();
    let request = &requests[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", "/app/devices/register")
    );
    assert_eq!(body, fixtures[register_request]["payload"]);
    assert!(header(request, "authorization").is_none());
    record(executed, register_request, true);
    record(executed, register_response, true);
    owner.shutdown().await.unwrap();
    peer.shutdown().await;
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target)
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn assert_mutations(
    bundle: &Path,
    provenance: &Path,
    manifest: &Value,
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
) {
    let temp = tempfile::tempdir().unwrap();
    let copy = temp.path().join("bundle");
    copy_tree(bundle, &copy);
    fs::write(copy.join("extra.json"), b"{}").unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    let copy = temp.path().join("extra-empty-directory");
    copy_tree(bundle, &copy);
    fs::create_dir(copy.join("empty")).unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    let copy = temp.path().join("case-collision");
    copy_tree(bundle, &copy);
    fs::write(copy.join("Vectors.json"), b"{}").unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    let copy = temp.path().join("missing");
    copy_tree(bundle, &copy);
    fs::remove_file(copy.join("vectors.json")).unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    let copy = temp.path().join("link");
    copy_tree(bundle, &copy);
    symlink(copy.join("vectors.json"), copy.join("linked.json")).unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    assert!(portable_path("../escape").is_err());
    assert!(portable_path("bad\\name").is_err());
    assert!(portable_path("CON.json").is_err());
    assert!(portable_path("name. ").is_err());
    for unsafe_name in ["bad\\name", "CON.json"] {
        let copy = temp
            .path()
            .join(format!("unsafe-{}", unsafe_name.replace(['\\', '.'], "-")));
        copy_tree(bundle, &copy);
        fs::write(copy.join(unsafe_name), b"unsafe").unwrap();
        assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    }
    let copy = temp.path().join("special-file");
    copy_tree(bundle, &copy);
    let fifo = copy.join("fixture.pipe");
    match std::process::Command::new("mkfifo").arg(&fifo).status() {
        Ok(status) if status.success() => assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err()),
        result => eprintln!("skipping special-file mutation: mkfifo unavailable: {result:?}"),
    }
    let copy = temp.path().join("duplicate-manifest-path");
    copy_tree(bundle, &copy);
    let mut duplicate_manifest: Value =
        serde_json::from_slice(&fs::read(copy.join("manifest.json")).unwrap()).unwrap();
    let duplicate_entry = duplicate_manifest["files"][0].clone();
    duplicate_manifest["files"]
        .as_array_mut()
        .unwrap()
        .push(duplicate_entry);
    let duplicate_bytes = serde_json::to_vec(&duplicate_manifest).unwrap();
    fs::write(copy.join("manifest.json"), &duplicate_bytes).unwrap();
    assert!(verify_bundle(&copy, &digest(&duplicate_bytes)).is_err());
    let copy = temp.path().join("fixture-bytes");
    copy_tree(bundle, &copy);
    fs::write(copy.join("fixtures/wire-behavior.json"), b"{}\n").unwrap();
    assert!(verify_bundle(&copy, MANIFEST_SHA256).is_err());
    for key in ["authority_commit", "bundle_version", "manifest_sha256"] {
        let mutated = temp.path().join(format!("provenance-{key}.json"));
        let mut value: Value = serde_json::from_slice(&fs::read(provenance).unwrap()).unwrap();
        value[key] = json!("wrong");
        fs::write(&mutated, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(verify_provenance(&mutated).is_err());
    }
    let mut missing_operation = manifest.clone();
    missing_operation["operation_ids"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(
        std::panic::catch_unwind(|| assert_identities(&missing_operation, fixtures, vectors))
            .is_err()
    );
    for (field, value) in [
        ("bundle_semver", json!("9.9.9")),
        ("observer_protocol_version", json!(1)),
    ] {
        let mut mutated = manifest.clone();
        mutated[field] = value;
        assert!(
            std::panic::catch_unwind(|| assert_identities(&mutated, fixtures, vectors)).is_err()
        );
    }
    let mut missing_fixture = fixtures.clone();
    missing_fixture.pop_first();
    assert!(
        std::panic::catch_unwind(|| assert_identities(manifest, &missing_fixture, vectors))
            .is_err()
    );
    let mut missing_vector = vectors.clone();
    missing_vector.pop_first();
    assert!(
        std::panic::catch_unwind(|| assert_identities(manifest, fixtures, &missing_vector))
            .is_err()
    );
}

async fn assert_production_contradiction_mutation(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
) {
    let fixture_id = "recorded.ingestUpload.ok";
    let vector_id = "observer.ingestUpload.status.ok";
    let mut payload = fixtures[fixture_id]["payload"].clone();
    payload["status"] = json!("duplicate");
    payload["existing_segment"] = json!("mutated-existing-segment");
    let status = vectors[vector_id]["observed_status"]
        .as_u64()
        .expect("authority observed status") as u16;
    let temp = tempfile::tempdir().unwrap();
    let media = temp.path().join("audio.flac");
    fs::write(&media, b"audio").unwrap();
    let harness = LinkedHarness::start(&temp).await;
    harness.peer.enqueue_response(status, payload.to_string());
    let result = harness
        .client
        .upload_segment("20260618", "143022_300", &[media])
        .await;
    let decision = &vectors[vector_id]["decision"];
    let expected_duplicate = decision["stored_key_source"] == "existing_segment";
    let expected_key = fixtures[fixture_id]["payload"][decision["stored_key_source"]
        .as_str()
        .expect("stored key source")]
    .as_str();
    assert!(result.duplicate != expected_duplicate || result.stored_key.as_deref() != expected_key);
    harness.finish().await;
}

#[tokio::test]
async fn observer_contract_conformance() {
    let root = workspace_root();
    let bundle = root.join("vendor/observer-client-contract");
    let manifest = verify_bundle(&bundle, MANIFEST_SHA256).unwrap();
    verify_provenance(&root.join("contracts/observer-client-import.json")).unwrap();
    let fixtures = load_index(&bundle.join("fixtures/wire-behavior.json"), "fixtures");
    let vectors = load_index(&bundle.join("vectors.json"), "vectors");
    assert_identities(&manifest, &fixtures, &vectors);
    assert_eq!(set(LINUX_FIXTURES).len(), 24);
    assert_eq!(set(LINUX_VECTORS).len(), 15);
    let mut executed_fixtures = BTreeSet::new();
    let mut executed_vectors = BTreeSet::new();
    assert_upload_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_listing_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_event_and_register(&fixtures, &mut executed_fixtures).await;
    assert_mutations(
        &bundle,
        &root.join("contracts/observer-client-import.json"),
        &manifest,
        &fixtures,
        &vectors,
    );
    assert_production_contradiction_mutation(&fixtures, &vectors).await;
    assert_eq!(executed_fixtures, set(LINUX_FIXTURES));
    assert_eq!(executed_vectors, set(LINUX_VECTORS));
}
