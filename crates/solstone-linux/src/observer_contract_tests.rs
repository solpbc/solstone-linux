// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config,
    private_link::{PrivateLinkOwner, start_private_link_for_test},
    private_link_test_peer::PrivateLinkPeer,
    upload::UploadClient,
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

const MANIFEST_SHA256: &str = "86d4358916a0303c29a8e61c6d1e48ef1939d0b5a2f042ae617958b9d0c1a5a8";
const AUTHORITY_COMMIT: &str = "1e432dba3ecdfa43789c25f97077fdc3e71fab59";

const LINUX_FIXTURES: &[&str] = &[
    "declared.client.ingestUpload.status.collision",
    "declared.client.ingestUpload.status.conflict",
    "declared.client.ingestUpload.status.duplicate",
    "declared.client.ingestUpload.status.failed",
    "declared.client.ingestUpload.status.ok",
];

const LINUX_VECTORS: &[&str] = &[
    "client.ingestUpload.status.collision",
    "client.ingestUpload.status.conflict",
    "client.ingestUpload.status.duplicate",
    "client.ingestUpload.status.failed",
    "client.ingestUpload.status.ok",
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

fn load_document(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn load_index(body: &Value, key: &str) -> BTreeMap<String, Value> {
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

fn record(executed: &mut BTreeSet<String>, id: &str) {
    assert!(
        executed.insert(id.to_owned()),
        "duplicate coverage for {id}"
    );
}

fn vocabulary<'a>(manifest: &'a Value, id: &str) -> &'a Value {
    manifest["vocabularies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|vocabulary| vocabulary["id"] == id)
        .unwrap_or_else(|| panic!("missing vocabulary {id}"))
}

fn assert_identities(
    manifest: &Value,
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    fixture_document: &Value,
    vector_document: &Value,
    consumer_audit: &Value,
) {
    assert_eq!(manifest["bundle_semver"], "14.0.0");
    assert_eq!(manifest["openapi_document_version"], "1.0.0");
    assert_eq!(manifest["client_protocol_version"], 3);
    assert_eq!(manifest["supported_response_variants"], json!([3]));
    assert_eq!(
        manifest["generator_identity"],
        "solstone.repository_contracts.client_ingest_contract_bundle.v1"
    );
    assert_eq!(
        manifest["schema_dialect_uri"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(
        manifest["bundle_schema_identity"],
        "solstone.client-ingest-contract-bundle.schema.v1"
    );
    assert_eq!(
        manifest["operation_ids"],
        json!([
            "client.ingestUpload",
            "client.ingestManifest",
            "client.ingestManifestDay",
            "client.ingestSegments"
        ])
    );
    assert_eq!(
        manifest["consumer_identifiers"],
        json!([
            "solstone-linux",
            "solstone-macos",
            "solstone-tmux",
            "solstone-windows"
        ])
    );
    assert_eq!(
        manifest["component_closure"],
        json!([
            "Error",
            "FileDescriptor",
            "SegmentFile",
            "SegmentItem",
            "SegmentsEnvelope"
        ])
    );
    assert_eq!(
        fixture_document["schema"],
        "solstone.client-ingest-contract-fixtures.v2"
    );
    assert_eq!(
        vector_document["schema"],
        "solstone.client-ingest-contract-vectors.v2"
    );
    assert_eq!(
        consumer_audit["schema"],
        "solstone.client-ingest-contract-consumer-audit.v2"
    );

    assert_eq!(
        vocabulary(manifest, "SegmentFile.status"),
        &json!({
            "classification":"closed",
            "id":"SegmentFile.status",
            "source_pointer":"/components/schemas/SegmentFile/properties/status",
            "unknown_value_behavior":"reject",
            "values":["present","missing","processed"]
        })
    );
    assert_eq!(
        vocabulary(manifest, "client.ingestUpload.status"),
        &json!({
            "classification":"closed",
            "id":"client.ingestUpload.status",
            "source_pointers":[
                "/paths/~1app~1devices~1ingest/post/responses/200/content/application~1json/schema/properties/status",
                "/paths/~1app~1devices~1ingest/post/responses/409"
            ],
            "unknown_value_behavior":"reject",
            "values":["ok","duplicate","collision","conflict","failed"]
        })
    );

    assert!(
        manifest["windows_linux_rollout_targets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let linux_revision = manifest["audited_consumer_revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|revision| revision["consumer_identifier"] == "solstone-linux")
        .unwrap();
    assert_eq!(
        linux_revision["revision"],
        "f33878fb6c608bf43654777c4a3b7772d7375e7c"
    );
    let linux_audit = consumer_audit["audited_commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|commit| commit["consumer"] == "solstone-linux")
        .unwrap();
    assert_eq!(
        linux_audit["commit"],
        "f33878fb6c608bf43654777c4a3b7772d7375e7c"
    );

    assert!(set(LINUX_FIXTURES).is_subset(&fixtures.keys().cloned().collect()));
    assert!(set(LINUX_VECTORS).is_subset(&vectors.keys().cloned().collect()));
    for vector in vectors.values() {
        assert!(fixtures.contains_key(vector["fixture_id"].as_str().unwrap()));
    }
    assert_eq!(
        fixtures["declared.client.ingestUpload.status.failed"]["schema_validation"],
        json!({
            "note":"vocabulary-only status value; the full Error payload requires error, reason_code, and detail, and the authority enumerates no HTTP 500 reason code",
            "valid":false
        })
    );
}

fn verify_provenance(root: &Path) -> Result<(), String> {
    let value: Value = serde_json::from_slice(&fs::read(root).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let expected = json!({
        "authority_repository":"https://github.com/solpbc/solstone-journal",
        "authority_commit":AUTHORITY_COMMIT,
        "bundle_version":"14.0.0",
        "manifest_path":"manifest.json",
        "manifest_sha256":MANIFEST_SHA256,
        "vendored_root":"vendor/observer-client-contract"
    });
    if value != expected {
        return Err("provenance mismatch".to_owned());
    }
    Ok(())
}

fn verify_authority_adoption(root: &Path) -> Result<(), String> {
    let adoption_path = root.join("contracts/authority-adoption.json");
    let value: Value =
        serde_json::from_slice(&fs::read(&adoption_path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let expected = json!({
        "authority_repository":"https://github.com/solpbc/solstone-journal",
        "authority_commit":AUTHORITY_COMMIT,
        "device_migration":{
            "artifacts":[
                {"authority_path":"contracts/device-migration/v1.schema.json","local_path":"contracts/device-migration/v1.schema.json","sha256":"5ea0ce5bf0bc5f07233f05dda3334ccea3bf173363fcd4fd4cdcd9479130a373"},
                {"authority_path":"contracts/device-migration/v1.vectors.json","local_path":"contracts/device-migration/v1.vectors.json","sha256":"3ba1bb508a0cd5756626c6246bfbff77573c54538db60c8e7f938e38b306ef9c"}
            ]
        },
        "client_ingest_bundle":{
            "bundle_version":"14.0.0",
            "manifest":{"authority_path":"docs/openapi/client-ingest-contract/manifest.json","local_path":"vendor/observer-client-contract/manifest.json","sha256":MANIFEST_SHA256},
            "generator_input":{"id":"openapi.client_ingest_authority","authority_path":"core/crates/solstone-core-repository-contracts/src/contracts/client_ingest_authority.json","sha256":"afe354e26827d431d4766289a91c9d421edd99e67006937f7ad8d955a23a8f53"},
            "artifacts":[
                {"authority_path":"docs/openapi/client-ingest-contract/consumer-audit.json","local_path":"vendor/observer-client-contract/consumer-audit.json","sha256":"31589211391a7feaafda6fa8514ffcf900f3acf3931a285e8712c646c6e9ab07"},
                {"authority_path":"docs/openapi/client-ingest-contract/fixtures/wire-behavior.json","local_path":"vendor/observer-client-contract/fixtures/wire-behavior.json","sha256":"035e59297af21da998984910b4aa6a850d7e2e1225c79019045381bf3e17e708"},
                {"authority_path":"docs/openapi/client-ingest-contract/projection.openapi.json","local_path":"vendor/observer-client-contract/projection.openapi.json","sha256":"db75a94ab97e83c56603e44d9313db86a94a2d9c4a920deaf090fe0f3358b8b0"},
                {"authority_path":"docs/openapi/client-ingest-contract/vectors.json","local_path":"vendor/observer-client-contract/vectors.json","sha256":"7c61c1238184e1440110801478daf714aba05b2472338bd98c12cd508b303d0f"}
            ]
        }
    });
    if value != expected {
        return Err("authority adoption provenance mismatch".to_owned());
    }
    for section in ["device_migration", "client_ingest_bundle"] {
        for artifact in value[section]["artifacts"].as_array().unwrap() {
            let local = root.join(artifact["local_path"].as_str().unwrap());
            let actual = digest(&fs::read(&local).map_err(|error| error.to_string())?);
            if actual != artifact["sha256"].as_str().unwrap() {
                return Err(format!("authority artifact digest mismatch: {local:?}"));
            }
        }
    }
    let manifest_path = root.join("vendor/observer-client-contract/manifest.json");
    if digest(&fs::read(&manifest_path).map_err(|error| error.to_string())?) != MANIFEST_SHA256 {
        return Err("client-ingest manifest digest mismatch".to_owned());
    }
    Ok(())
}

fn assert_protocol_three_parameter(operation: &Value) {
    let parameter = operation["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "X-Solstone-Protocol-Version")
        .expect("required protocol parameter");
    assert_eq!(parameter["in"], "header");
    assert_eq!(parameter["required"], true);
    assert_eq!(parameter["schema"], json!({"const":3,"type":"integer"}));
    assert!(
        parameter["description"]
            .as_str()
            .is_some_and(|description| {
                description.contains(
                    "Identity is the linked-device mTLS certificate, not a request header.",
                )
            })
    );
}

fn assert_read_projection(projection: &Value) {
    let paths = &projection["paths"];
    let manifest = &paths["/app/devices/ingest/manifest"]["get"];
    let day_manifest = &paths["/app/devices/ingest/manifest/{day}"]["get"];
    let segments = &paths["/app/devices/ingest/segments/{day}"]["get"];
    for (route, operation, id) in [
        (
            "/app/devices/ingest/manifest",
            manifest,
            "client.ingestManifest",
        ),
        (
            "/app/devices/ingest/manifest/{day}",
            day_manifest,
            "client.ingestManifestDay",
        ),
        (
            "/app/devices/ingest/segments/{day}",
            segments,
            "client.ingestSegments",
        ),
    ] {
        let methods: Vec<_> = paths[route]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(methods, ["get"]);
        assert_eq!(operation["operationId"], id);
        assert_protocol_three_parameter(operation);
    }
    assert_eq!(
        manifest["responses"]["200"]["content"]["application/json"]["schema"]["required"],
        json!(["days"])
    );
    let day_schema = &day_manifest["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(
        day_schema["required"],
        json!(["version", "day", "segments"])
    );
    assert_eq!(day_schema["properties"]["version"]["type"], "integer");
    assert!(day_schema["properties"]["version"].get("const").is_none());
    assert!(day_schema["properties"]["version"].get("enum").is_none());
    assert_eq!(
        segments["responses"]["200"]["content"]["application/json"]["schema"],
        json!({"$ref":"#/components/schemas/SegmentsEnvelope"})
    );

    let schemas = &projection["components"]["schemas"];
    assert_eq!(
        schemas["SegmentsEnvelope"]["required"],
        json!(["items", "total", "protocol_version"])
    );
    assert_eq!(
        schemas["SegmentsEnvelope"]["properties"]["items"]["items"],
        json!({"$ref":"#/components/schemas/SegmentItem"})
    );
    assert_eq!(
        schemas["SegmentItem"]["required"],
        json!(["key", "observed", "files"])
    );
    assert_eq!(
        schemas["SegmentItem"]["properties"]["files"]["items"],
        json!({"$ref":"#/components/schemas/SegmentFile"})
    );
    assert_eq!(
        schemas["SegmentFile"]["required"],
        json!(["name", "size", "sha256", "status"])
    );
    assert_eq!(
        schemas["SegmentFile"]["properties"]["size"]["type"],
        "integer"
    );
    assert_eq!(
        schemas["SegmentFile"]["properties"]["status"]["enum"],
        json!(["present", "missing", "processed"])
    );
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
        let (_state, owner) = start_private_link_for_test(peer.credential(), "desktop").await;
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

async fn upload_fixture_result(fixture: &Value) -> crate::upload::UploadResult {
    let status = fixture["provenance"]["http_status"]
        .as_u64()
        .expect("fixture HTTP status") as u16;
    let payload = fixture["payload"].to_string();
    let temp = tempfile::tempdir().unwrap();
    let media = temp.path().join("audio.flac");
    fs::write(&media, b"audio").unwrap();
    let harness = LinkedHarness::start(&temp).await;
    harness.peer.enqueue_response(status, payload);
    let result = harness
        .client
        .upload_segment("20260618", "143022_300", &[media])
        .await;
    harness.finish().await;
    result
}

fn assert_upload_success_matches_decision(success: bool, decision: &Value) {
    assert_eq!(success, decision["accepted"].as_bool().unwrap());
}

async fn assert_upload_contract(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    executed_fixtures: &mut BTreeSet<String>,
    executed_vectors: &mut BTreeSet<String>,
) {
    for fixture_id in LINUX_FIXTURES {
        let fixture = &fixtures[*fixture_id];
        let status = fixture["payload"]["status"].as_str().unwrap();
        let vector_id = format!("client.ingestUpload.status.{status}");
        let vector = &vectors[&vector_id];
        let decision = &vector["decision"];
        assert_eq!(vector["fixture_id"], *fixture_id);
        assert_eq!(fixture["kind"], "declared");
        assert_eq!(
            fixture["provenance"]["vocabulary"],
            "client.ingestUpload.status"
        );
        assert_eq!(
            fixture["provenance"]["http_status"],
            decision["http_status"]
        );
        assert_eq!(decision["kind"], "ingest_status");
        assert_eq!(decision["status"], status);
        assert_eq!(vector["pointers"], json!(["/status"]));
        let result = upload_fixture_result(fixture).await;
        assert_upload_success_matches_decision(result.success, decision);
        if decision["accepted"] != true {
            assert!(!result.success);
        }
        record(executed_fixtures, fixture_id);
        record(executed_vectors, &vector_id);
    }
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

#[allow(clippy::too_many_arguments)]
fn assert_mutations(
    bundle: &Path,
    provenance: &Path,
    manifest: &Value,
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
    fixture_document: &Value,
    vector_document: &Value,
    consumer_audit: &Value,
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
        std::panic::catch_unwind(|| {
            assert_identities(
                &missing_operation,
                fixtures,
                vectors,
                fixture_document,
                vector_document,
                consumer_audit,
            )
        })
        .is_err()
    );
    for (field, value) in [
        ("bundle_semver", json!("9.9.9")),
        ("client_protocol_version", json!(2)),
    ] {
        let mut mutated = manifest.clone();
        mutated[field] = value;
        assert!(
            std::panic::catch_unwind(|| {
                assert_identities(
                    &mutated,
                    fixtures,
                    vectors,
                    fixture_document,
                    vector_document,
                    consumer_audit,
                )
            })
            .is_err()
        );
    }
    let mut missing_fixture = fixtures.clone();
    missing_fixture.remove(LINUX_FIXTURES[0]);
    assert!(
        std::panic::catch_unwind(|| {
            assert_identities(
                manifest,
                &missing_fixture,
                vectors,
                fixture_document,
                vector_document,
                consumer_audit,
            )
        })
        .is_err()
    );
    let mut missing_vector = vectors.clone();
    missing_vector.remove(LINUX_VECTORS[0]);
    assert!(
        std::panic::catch_unwind(|| {
            assert_identities(
                manifest,
                fixtures,
                &missing_vector,
                fixture_document,
                vector_document,
                consumer_audit,
            )
        })
        .is_err()
    );
}

async fn assert_production_contradiction_mutation(
    fixtures: &BTreeMap<String, Value>,
    vectors: &BTreeMap<String, Value>,
) {
    let fixture = &fixtures["declared.client.ingestUpload.status.ok"];
    let mut mutated_vector = vectors["client.ingestUpload.status.ok"].clone();
    mutated_vector["decision"]["accepted"] = json!(false);
    let result = upload_fixture_result(fixture).await;
    assert!(
        std::panic::catch_unwind(|| {
            assert_upload_success_matches_decision(result.success, &mutated_vector["decision"])
        })
        .is_err()
    );
}

#[tokio::test]
async fn observer_contract_conformance() {
    let root = workspace_root();
    let bundle = root.join("vendor/observer-client-contract");
    let manifest = verify_bundle(&bundle, MANIFEST_SHA256).unwrap();
    verify_provenance(&root.join("contracts/observer-client-import.json")).unwrap();
    verify_authority_adoption(&root).unwrap();
    let projection = load_document(&bundle.join("projection.openapi.json"));
    let fixture_document = load_document(&bundle.join("fixtures/wire-behavior.json"));
    let vector_document = load_document(&bundle.join("vectors.json"));
    let consumer_audit = load_document(&bundle.join("consumer-audit.json"));
    let fixtures = load_index(&fixture_document, "fixtures");
    let vectors = load_index(&vector_document, "vectors");
    assert_identities(
        &manifest,
        &fixtures,
        &vectors,
        &fixture_document,
        &vector_document,
        &consumer_audit,
    );
    assert_read_projection(&projection);
    assert_eq!(set(LINUX_FIXTURES).len(), 5);
    assert_eq!(set(LINUX_VECTORS).len(), 5);
    let mut executed_fixtures = BTreeSet::new();
    let mut executed_vectors = BTreeSet::new();
    assert_upload_contract(
        &fixtures,
        &vectors,
        &mut executed_fixtures,
        &mut executed_vectors,
    )
    .await;
    assert_mutations(
        &bundle,
        &root.join("contracts/observer-client-import.json"),
        &manifest,
        &fixtures,
        &vectors,
        &fixture_document,
        &vector_document,
        &consumer_audit,
    );
    assert_production_contradiction_mutation(&fixtures, &vectors).await;
    assert_eq!(executed_fixtures, set(LINUX_FIXTURES));
    assert_eq!(executed_vectors, set(LINUX_VECTORS));
}
