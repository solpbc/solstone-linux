// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    chat_bridge::{
        EVENT_SOL_CHAT_REQUEST, ack_contract_request, consume_contract_body, contract_poll_opt_in,
        dispatch_contract_payload, parse_contract_sse,
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
    UploadClient::new(
        config,
        "archon",
        "linux",
        "1.4.0",
        std::sync::Arc::new(crate::test_support::MutableClock::new(0.0, 0.0)),
    )
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
        let server = MockServer::new(vec![(status, fixture["payload"].clone())]).await;
        let result = client(&config(&server, &temp))
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
            assert_eq!(server.requests().len(), 1);
        } else {
            assert!(!result.success && result.stored_key.is_none());
        }
        record(executed_fixtures, fixture_id, true);
        record(executed_vectors, vector_id, true);
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
        record(executed_fixtures, fixture_id, true);
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
    let server = MockServer::new(vec![(200, json!({"status":"ok","segment":"143022_300"}))]).await;
    client(&config(&server, &temp))
        .upload_segment(
            fixtures[fixture_id]["payload"]["day"].as_str().unwrap(),
            fixtures[fixture_id]["payload"]["segment"].as_str().unwrap(),
            &media,
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
        let server = MockServer::new(vec![(200, payload.clone())]).await;
        let result = client(&config(&server, &temp))
            .get_server_segments("20260618")
            .await;
        assert_eq!((result.legacy, result.truncated), (legacy, truncated));
        let request = &server.requests()[0];
        assert_eq!(request.method, "GET");
        assert_eq!(request.uri, "/app/observer/ingest/segments/20260618");
        assert_eq!(request.headers["x-solstone-protocol-version"], "2");
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
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
    record(executed, event_id, true);
    record(
        executed,
        "example.observer.ingestEvent.response.200.application-json.default",
        true,
    );
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
    record(executed, register_request, true);
    record(executed, register_response, true);
}

async fn assert_chat_contract(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    let example_data = "example.observer.callosumStream.response.200.text-event-stream.default";
    let wire = format!("data: {}\n\n", fixtures[example_data]["payload"]);
    let frames = parse_contract_sse(wire.as_bytes());
    assert_eq!(
        frames.as_slice(),
        &[(
            "frame".to_owned(),
            None,
            fixtures[example_data]["payload"].to_string()
        )]
    );
    record(executed_fixtures, example_data, true);

    let data_id = "recorded.sse.observer.data";
    let data_vector = "observer.callosumStream.sse.data";
    let wire = format!("data: {}\n\n", fixtures[data_id]["payload"]);
    let frames = parse_contract_sse(wire.as_bytes());
    assert_eq!(frames[0].0, "frame");
    assert_eq!(frames[0].2, fixtures[data_id]["payload"].to_string());
    assert_eq!(vectors[data_vector]["fixture_id"], data_id);
    assert_eq!(vectors[data_vector]["decision"]["frame_kind"], "data");
    assert_eq!(
        vectors[data_vector]["decision"]["action"],
        "dispatch_callosum_event"
    );
    record(executed_fixtures, data_id, true);
    record(executed_vectors, data_vector, true);

    let error_id = "recorded.sse.observer.error";
    let error_vector = "observer.callosumStream.sse.error";
    let wire = format!("event: error\ndata: {}\n\n", fixtures[error_id]["payload"]);
    let (terminal, pending, side_effects) = consume_contract_body(wire).await;
    assert!(terminal);
    assert_eq!(pending, 0);
    assert_eq!(side_effects, 0);
    let decision = &vectors[error_vector]["decision"];
    assert_eq!(vectors[error_vector]["fixture_id"], error_id);
    assert_eq!(decision["frame_kind"], "error");
    assert_eq!(decision["action"], "surface_error_and_close");
    assert_eq!(
        decision["reason_code"],
        fixtures[error_id]["payload"]["reason_code"]
    );
    record(executed_fixtures, error_id, true);
    record(executed_vectors, error_vector, true);

    let heartbeat_id = "recorded.sse.observer.heartbeat";
    let heartbeat_vector = "observer.callosumStream.sse.heartbeat";
    let raw = fixtures[heartbeat_id]["payload"]
        .as_str()
        .expect("heartbeat payload");
    assert_eq!(parse_contract_sse(raw.as_bytes())[0].0, "heartbeat");
    assert_eq!(vectors[heartbeat_vector]["fixture_id"], heartbeat_id);
    assert_eq!(
        vectors[heartbeat_vector]["decision"]["frame_kind"],
        "heartbeat"
    );
    assert_eq!(
        vectors[heartbeat_vector]["decision"]["action"],
        "ignore_keepalive"
    );
    record(executed_fixtures, heartbeat_id, true);
    record(executed_vectors, heartbeat_vector, true);

    let valid_id = "example.chat.openSolChatRequest.request.body.application-json.default";
    let ok_vector = "chat.openSolChatRequest.ok";
    let original_request_id = fixtures[valid_id]["payload"]["request_id"]
        .as_str()
        .expect("request fixture id");
    let mut valid = Map::new();
    valid.insert("tract".into(), json!("chat"));
    valid.insert("event".into(), json!(EVENT_SOL_CHAT_REQUEST));
    valid.insert(
        "request_id".into(),
        fixtures[valid_id]["payload"]["request_id"].clone(),
    );
    let (created, preserved_id) = dispatch_contract_payload(valid).await;
    assert!(created);
    assert_eq!(preserved_id.as_deref(), Some(original_request_id));
    assert!(
        vectors[ok_vector]["decision"]["accepted"]
            .as_bool()
            .expect("authority chat accepted")
    );
    assert_eq!(
        vectors[ok_vector]["decision"]["missing_field_behavior"],
        "non_empty_trimmed_request_id_required"
    );
    record(executed_fixtures, valid_id, true);
    record(executed_vectors, ok_vector, true);

    let missing_vector = "chat.openSolChatRequest.missing_required_field";
    let mut rejected = Vec::new();
    for id in [
        None,
        Some(Value::Null),
        Some(json!("")),
        Some(json!("   ")),
        Some(json!([])),
    ] {
        let mut payload = Map::new();
        payload.insert("tract".into(), json!("chat"));
        payload.insert("event".into(), json!(EVENT_SOL_CHAT_REQUEST));
        if let Some(id) = id {
            payload.insert("request_id".into(), id);
        }
        rejected.push(!dispatch_contract_payload(payload).await.0);
    }
    assert!(rejected.into_iter().all(|value| value));
    assert!(
        !vectors[missing_vector]["decision"]["accepted"]
            .as_bool()
            .expect("authority chat rejected")
    );
    assert_eq!(
        vectors[missing_vector]["decision"]["missing_field_behavior"],
        "absent_malformed_empty_or_blank_rejected"
    );

    for (response_id, vector_id) in [
        (
            "example.chat.openSolChatRequest.response.200.application-json.default",
            None,
        ),
        ("recorded.chat.openSolChatRequest.ok", Some(ok_vector)),
        (
            "recorded.chat.openSolChatRequest.missing",
            Some(missing_vector),
        ),
    ] {
        let fixture = &fixtures[response_id];
        let status = fixture["provenance"]["status"]
            .as_u64()
            .expect("chat response status") as u16;
        let server = MockServer::new(vec![(status, fixture["payload"].clone())]).await;
        ack_contract_request(Client::new(), &server.url, "K", original_request_id).await;
        wait_for_requests(&server, 1).await;
        let request = &server.requests()[0];
        assert_eq!(
            (request.method.as_str(), request.uri.as_str()),
            ("POST", "/api/chat/sol_chat_request/open")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"request_id": original_request_id})
        );
        assert!(
            request.headers["authorization"]
                .to_str()
                .unwrap()
                .starts_with("Bearer ")
        );
        if let Some(vector_id) = vector_id {
            assert_eq!(vectors[vector_id]["fixture_id"], response_id);
            assert_eq!(
                vectors[vector_id]["observed_status"].as_u64(),
                Some(status as u64)
            );
            if vector_id == missing_vector {
                assert_eq!(
                    fixtures[response_id]["payload"]["reason_code"],
                    vectors[vector_id]["decision"]["reason_code"]
                );
            }
        }
        record(executed_fixtures, response_id, true);
    }
    record(executed_vectors, missing_vector, true);
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
    let server = MockServer::new(vec![(status, payload)]).await;
    let result = client(&config(&server, &temp))
        .upload_segment("20260618", "143022_300", &[media])
        .await;
    let decision = &vectors[vector_id]["decision"];
    let expected_duplicate = decision["stored_key_source"] == "existing_segment";
    let expected_key = fixtures[fixture_id]["payload"][decision["stored_key_source"]
        .as_str()
        .expect("stored key source")]
    .as_str();
    assert!(result.duplicate != expected_duplicate || result.stored_key.as_deref() != expected_key);
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
    assert_listing_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_event_and_register(&fixtures, &mut executed_fixtures).await;
    assert_chat_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_settings_contract().await;
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
