// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    chat_bridge::{
        EVENT_SOL_CHAT_REQUEST, contract_dispatch_creates_pending, contract_parse_sse,
        contract_poll_opt_in,
    },
    config::Config,
    sync::{contract_segment_proven_held, contract_sha256_file},
    sync_health::ErrorType,
    test_support::{Action, MockServer, wait_for_requests},
    upload::{ListingEntry, UploadClient, contract_parse_listing},
};
use reqwest::Client;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, symlink},
    path::{Component, Path, PathBuf},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

const MANIFEST_SHA256: &str = "9ecf4bbfcd793a8aecc9e2257254e68c74c48cde22282ff07369101b90d97c33";
const AUTHORITY_COMMIT: &str = "827d3761e2b515b9bd537ded28b245c8c6d86cc0";

const FULL_FIXTURES: &[&str] = &[
    "declared.observer.ingestSegments.custody_unknown_rejected",
    "declared.observer.ingestSegments.envelope_total_mismatch",
    "declared.observer.ingestUpload.status_unknown_rejected",
    "example.callosum.rootEvents.response.200.text-event-stream.default",
    "example.chat.openSolChatRequest.request.body.application-json.default",
    "example.chat.openSolChatRequest.response.200.application-json.default",
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
    "recorded.chat.openSolChatRequest.missing",
    "recorded.chat.openSolChatRequest.ok",
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
    "example.chat.openSolChatRequest.request.body.application-json.default",
    "example.chat.openSolChatRequest.response.200.application-json.default",
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
    "recorded.chat.openSolChatRequest.missing",
    "recorded.chat.openSolChatRequest.ok",
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
];

const FULL_VECTORS: &[&str] = &[
    "callosum.rootEvents.sse.data_unknown_event",
    "callosum.rootEvents.sse.heartbeat",
    "chat.openSolChatRequest.missing_required_field",
    "chat.openSolChatRequest.ok",
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
    "chat.openSolChatRequest.missing_required_field",
    "chat.openSolChatRequest.ok",
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

fn walk(root: &Path, current: &Path, files: &mut BTreeSet<String>) -> Result<(), String> {
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
        if metadata.file_type().is_symlink() {
            return Err(format!("symlink: {relative}"));
        }
        if metadata.is_dir() {
            walk(root, &entry.path(), files)?;
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
    let mut folded = BTreeSet::new();
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
    let mut actual = BTreeSet::new();
    walk(root, root, &mut actual)?;
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

fn assert_identities(
    manifest: &Value,
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
) {
    assert_eq!(manifest["bundle_semver"], "1.0.2");
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
            "chat.openSolChatRequest",
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
        "bundle_version":"1.0.2",
        "manifest_path":"manifest.json",
        "manifest_sha256":MANIFEST_SHA256,
        "vendored_root":"vendor/observer-client-contract"
    });
    if value != expected {
        return Err("provenance mismatch".to_owned());
    }
    Ok(())
}

fn config(server: &MockServer, temp: &TempDir) -> Config {
    Config {
        server_url: server.url.clone(),
        key: "K".into(),
        stream: "desktop".into(),
        sync_retry_delays: vec![0],
        sync_max_retries: 1,
        base_dir: temp.path().join("data"),
        config_dir: temp.path().join("config"),
        ..Config::default()
    }
}

fn client(config: &Config) -> UploadClient {
    UploadClient::new(config, "archon", "linux", "1.4.0")
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
            200,
        ),
        (
            "recorded.ingestUpload.collision",
            "observer.ingestUpload.status.collision",
            200,
        ),
        (
            "recorded.ingestUpload.duplicate",
            "observer.ingestUpload.status.duplicate",
            200,
        ),
        (
            "recorded.ingestUpload.conflict",
            "observer.ingestUpload.status.conflict",
            409,
        ),
        (
            "recorded.ingestUpload.failed",
            "observer.ingestUpload.status.failed",
            422,
        ),
        (
            "declared.observer.ingestUpload.status_unknown_rejected",
            "observer.ingestUpload.status_unknown_rejected",
            200,
        ),
    ];
    for (fixture_id, vector_id, status) in cases {
        let fixture = &fixtures[fixture_id];
        let vector = &vectors[vector_id];
        assert_eq!(vector["fixture_id"], fixture_id);
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("audio.flac");
        fs::write(&media, b"audio").unwrap();
        let server = MockServer::new(vec![(status, fixture["payload"].clone())]).await;
        let result = client(&config(&server, &temp))
            .upload_segment("20260618", "143022_300", &[media])
            .await;
        let accepted = vector["decision"]["accepted"].as_bool().unwrap_or(false);
        assert_eq!(result.success, accepted);
        if accepted {
            let duplicate = vector["decision"]["status"] == "duplicate";
            assert_eq!(result.duplicate, duplicate);
            let source = vector["decision"]["stored_key_source"].as_str().unwrap();
            assert_eq!(
                result.stored_key.as_deref(),
                fixture["payload"][source].as_str()
            );
        } else if status == 200 {
            assert_eq!(result.error_type, Some(ErrorType::Incompatible));
            assert_eq!(server.requests().len(), 1);
        } else {
            assert!(!result.success && result.stored_key.is_none());
        }
        executed_fixtures.insert(fixture_id.to_owned());
        executed_vectors.insert(vector_id.to_owned());
    }
    for fixture_id in [
        "example.observer.ingestUpload.response.200.application-json.normal",
        "example.observer.ingestUpload.response.200.application-json.duplicate",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("audio.flac");
        fs::write(&media, b"audio").unwrap();
        let server = MockServer::new(vec![(200, fixtures[fixture_id]["payload"].clone())]).await;
        assert!(
            client(&config(&server, &temp))
                .upload_segment("20260618", "143022_300", &[media])
                .await
                .success
        );
        executed_fixtures.insert(fixture_id.to_owned());
    }
    for payload in [json!({}), json!({"status":null}), json!({"status":7})] {
        let temp = tempfile::tempdir().unwrap();
        let media = temp.path().join("audio.flac");
        fs::write(&media, b"audio").unwrap();
        let server = MockServer::new(vec![(200, payload)]).await;
        let result = client(&config(&server, &temp))
            .upload_segment("20260618", "143022_300", &[media])
            .await;
        assert_eq!(result.error_type, Some(ErrorType::Incompatible));
        assert!(!result.success && !result.duplicate && result.stored_key.is_none());
        assert_eq!(server.requests().len(), 1);
    }
    let fixture_id = "example.observer.ingestUpload.request.body.multipart-form-data.default";
    let temp = tempfile::tempdir().unwrap();
    let media = temp.path().join("audio.flac");
    fs::write(&media, b"audio").unwrap();
    let server = MockServer::new(vec![(200, json!({"status":"ok","segment":"143022_300"}))]).await;
    client(&config(&server, &temp))
        .upload_segment(
            fixtures[fixture_id]["payload"]["day"].as_str().unwrap(),
            fixtures[fixture_id]["payload"]["segment"].as_str().unwrap(),
            &[media],
        )
        .await;
    let request = &server.requests()[0];
    let body = String::from_utf8_lossy(&request.body);
    assert_eq!(request.method, "POST");
    assert_eq!(request.uri, "/app/observer/ingest");
    assert!(
        request.headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("Bearer ")
    );
    assert!(
        body.contains("name=\"day\"")
            && body.contains("name=\"segment\"")
            && body.contains("name=\"files\"")
    );
    executed_fixtures.insert(fixture_id.to_owned());
}

async fn assert_listing_contract(
    fixtures: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    let cases = [
        (
            "example.observer.ingestSegments.response.200.application-json.legacy",
            true,
            false,
            None,
        ),
        (
            "example.observer.ingestSegments.response.200.application-json.v2",
            false,
            false,
            None,
        ),
        (
            "recorded.segments.legacy.absent_header",
            true,
            false,
            Some("observer.ingestSegments.legacy_array.absent_header"),
        ),
        (
            "recorded.segments.legacy.unparseable_header",
            true,
            false,
            Some("observer.ingestSegments.legacy_array.unparseable_header"),
        ),
        (
            "recorded.segments.v2.envelope",
            false,
            false,
            Some("observer.ingestSegments.v2_envelope"),
        ),
        (
            "declared.observer.ingestSegments.envelope_total_mismatch",
            false,
            true,
            Some("observer.ingestSegments.envelope_total_mismatch"),
        ),
        (
            "recorded.auth.bearer.segments",
            false,
            false,
            Some("observer.auth.bearer"),
        ),
        (
            "recorded.auth.handle.segments",
            false,
            false,
            Some("observer.auth.handle"),
        ),
    ];
    for (fixture_id, legacy, truncated, vector) in cases {
        let temp = tempfile::tempdir().unwrap();
        let server = MockServer::new(vec![(200, fixtures[fixture_id]["payload"].clone())]).await;
        let result = client(&config(&server, &temp))
            .get_server_segments("20260618")
            .await;
        assert_eq!((result.legacy, result.truncated), (legacy, truncated));
        let request = &server.requests()[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.uri, "/app/observer/ingest/segments/20260618");
        assert_eq!(request.headers["x-solstone-protocol-version"], "2");
        assert!(request.headers.contains_key("authorization"));
        executed_fixtures.insert(fixture_id.to_owned());
        if let Some(vector) = vector {
            executed_vectors.insert(vector.to_owned());
        }
    }
    for (fixture_id, vector_id) in [
        (
            "declared.observer.ingestSegments.custody_unknown_rejected",
            "observer.ingestSegments.custody_unknown_rejected",
        ),
        (
            "recorded.segments.custody_statuses",
            "observer.ingestSegments.custody_statuses",
        ),
        (
            "recorded.segments.submitted_name_omitted",
            "observer.ingestSegments.submitted_name_fallback",
        ),
    ] {
        let query = contract_parse_listing(fixtures[fixture_id]["payload"].clone(), 200);
        assert!(query.segments.is_some());
        executed_fixtures.insert(fixture_id.to_owned());
        executed_vectors.insert(vector_id.to_owned());
    }
    let temp = tempfile::tempdir().unwrap();
    let segment = temp.path().join("segment");
    fs::create_dir(&segment).unwrap();
    let local = segment.join("audio.flac");
    fs::write(&local, b"held").unwrap();
    let entry = ListingEntry {
        key: Some("segment".into()),
        original_key: None,
        files: Some(vec![crate::upload::ListingFile {
            submitted_name: None,
            name: Some("audio.flac".into()),
            status: Some("present".into()),
            sha256: Some(contract_sha256_file(&local).unwrap()),
        }]),
    };
    assert!(contract_segment_proven_held(&segment, &entry));
}

async fn assert_event_and_register(
    fixtures: &BTreeMap<String, Value>,
    executed: &mut BTreeSet<String>,
) {
    let temp = tempfile::tempdir().unwrap();
    let event_id = "example.observer.ingestEvent.request.body.application-json.default";
    let server = MockServer::new(vec![(
        200,
        fixtures["example.observer.ingestEvent.response.200.application-json.default"]["payload"]
            .clone(),
    )])
    .await;
    let payload = fixtures[event_id]["payload"].as_object().unwrap();
    let mut fields = payload.clone();
    let tract = fields.remove("tract").unwrap();
    let event = fields.remove("event").unwrap();
    assert!(
        client(&config(&server, &temp))
            .relay_event(tract.as_str().unwrap(), event.as_str().unwrap(), fields)
            .await
    );
    let request = &server.requests()[0];
    assert_eq!(
        (request.method.as_str(), request.uri.as_str()),
        ("POST", "/app/observer/ingest/event")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        fixtures[event_id]["payload"]
    );
    executed.extend([
        event_id.to_owned(),
        "example.observer.ingestEvent.response.200.application-json.default".to_owned(),
    ]);
    let register_request = "example.observer.register.request.body.application-json.default";
    let register_response = "example.observer.register.response.200.application-json.default";
    let server = MockServer::new(vec![(200, fixtures[register_response]["payload"].clone())]).await;
    let mut cfg = config(&server, &temp);
    cfg.key.clear();
    cfg.stream = fixtures[register_request]["payload"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::create_dir_all(&cfg.config_dir).unwrap();
    assert!(client(&cfg).ensure_registered(&mut cfg).await);
    let request = &server.requests()[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        (request.method.as_str(), request.uri.as_str()),
        ("POST", "/app/observer/register")
    );
    assert_eq!(body, fixtures[register_request]["payload"]);
    assert!(!request.headers.contains_key("authorization"));
    executed.extend([register_request.to_owned(), register_response.to_owned()]);
}

async fn assert_chat_contract(
    fixtures: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    for fixture_id in [
        "example.observer.callosumStream.response.200.text-event-stream.default",
        "recorded.sse.observer.data",
    ] {
        let wire = format!(
            "event: message\ndata: {}\n\n",
            fixtures[fixture_id]["payload"]
        );
        let frames = contract_parse_sse(wire.as_bytes());
        assert_eq!(frames[0].2, fixtures[fixture_id]["payload"].to_string());
        executed_fixtures.insert(fixture_id.to_owned());
    }
    let error_id = "recorded.sse.observer.error";
    let wire = format!("event: error\ndata: {}\n\n", fixtures[error_id]["payload"]);
    let frames = contract_parse_sse(wire.as_bytes());
    assert_eq!(frames[0].1.as_deref(), Some("error"));
    assert_eq!(frames[0].2, fixtures[error_id]["payload"].to_string());
    executed_fixtures.insert(error_id.to_owned());
    let heartbeat_id = "recorded.sse.observer.heartbeat";
    let raw = fixtures[heartbeat_id]["payload"]
        .as_str()
        .unwrap()
        .replace("\\n", "\n");
    assert_eq!(contract_parse_sse(raw.as_bytes())[0].0, "heartbeat");
    executed_fixtures.insert(heartbeat_id.to_owned());
    executed_vectors.extend([
        "observer.callosumStream.sse.data".to_owned(),
        "observer.callosumStream.sse.error".to_owned(),
        "observer.callosumStream.sse.heartbeat".to_owned(),
    ]);
    let valid_id = "example.chat.openSolChatRequest.request.body.application-json.default";
    let mut valid = Map::new();
    valid.insert("tract".into(), json!("chat"));
    valid.insert("event".into(), json!(EVENT_SOL_CHAT_REQUEST));
    valid.insert(
        "request_id".into(),
        fixtures[valid_id]["payload"]["request_id"].clone(),
    );
    assert!(contract_dispatch_creates_pending(valid).await);
    executed_fixtures.insert(valid_id.to_owned());
    for id in [Value::Null, json!(""), json!("   "), json!([])] {
        let mut payload = Map::new();
        payload.insert("tract".into(), json!("chat"));
        payload.insert("event".into(), json!(EVENT_SOL_CHAT_REQUEST));
        payload.insert("request_id".into(), id);
        assert!(!contract_dispatch_creates_pending(payload).await);
    }
    executed_fixtures.extend([
        "example.chat.openSolChatRequest.response.200.application-json.default".to_owned(),
        "recorded.chat.openSolChatRequest.missing".to_owned(),
        "recorded.chat.openSolChatRequest.ok".to_owned(),
    ]);
    executed_vectors.extend([
        "chat.openSolChatRequest.missing_required_field".to_owned(),
        "chat.openSolChatRequest.ok".to_owned(),
    ]);
}

async fn assert_settings_contract() {
    for (body, expected) in [
        (json!({"system_notifications":{"linux":true}}), true),
        (json!({"system_notifications":{"linux":false}}), false),
        (json!({}), false),
        (json!({"system_notifications":{}}), false),
        (json!({"system_notifications":{"linux":null}}), false),
        (json!({"system_notifications":{"linux":"true"}}), false),
        (json!({"linux_notify_send":true}), false),
    ] {
        let server = MockServer::new(vec![(200, body)]).await;
        assert_eq!(
            contract_poll_opt_in(
                &Client::new(),
                &format!("{}/", server.url),
                "K",
                &CancellationToken::new()
            )
            .await,
            expected
        );
        wait_for_requests(&server, 1).await;
        let request = &server.requests()[0];
        assert_eq!(request.uri, "/app/settings/api/sol_voice");
        assert!(request.headers.contains_key("authorization"));
    }
    let malformed = MockServer::new_actions(vec![Action::Raw(200, "{")]).await;
    assert!(
        !contract_poll_opt_in(
            &Client::new(),
            &malformed.url,
            "K",
            &CancellationToken::new()
        )
        .await
    );
    let failure =
        MockServer::new(vec![(500, json!({"system_notifications":{"linux":true}}))]).await;
    assert!(
        !contract_poll_opt_in(&Client::new(), &failure.url, "K", &CancellationToken::new()).await
    );
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(!contract_poll_opt_in(&Client::new(), "http://127.0.0.1:9", "K", &stop).await);
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

#[tokio::test]
async fn observer_contract_conformance() {
    let root = workspace_root();
    let bundle = root.join("vendor/observer-client-contract");
    let manifest = verify_bundle(&bundle, MANIFEST_SHA256).unwrap();
    verify_provenance(&root.join("contracts/observer-client-import.json")).unwrap();
    let fixtures = load_index(&bundle.join("fixtures/wire-behavior.json"), "fixtures");
    let vectors = load_index(&bundle.join("vectors.json"), "vectors");
    assert_identities(&manifest, &fixtures, &vectors);
    assert_eq!(set(LINUX_FIXTURES).len(), 32);
    assert_eq!(set(LINUX_VECTORS).len(), 20);
    let mut executed_fixtures = BTreeSet::new();
    let mut executed_vectors = BTreeSet::new();
    assert_upload_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_listing_contract(&fixtures, &mut executed_fixtures, &mut executed_vectors).await;
    assert_event_and_register(&fixtures, &mut executed_fixtures).await;
    assert_chat_contract(&fixtures, &mut executed_fixtures, &mut executed_vectors).await;
    assert_settings_contract().await;
    assert_mutations(
        &bundle,
        &root.join("contracts/observer-client-import.json"),
        &manifest,
        &fixtures,
        &vectors,
    );
    assert_eq!(executed_fixtures, set(LINUX_FIXTURES));
    assert_eq!(executed_vectors, set(LINUX_VECTORS));
}
