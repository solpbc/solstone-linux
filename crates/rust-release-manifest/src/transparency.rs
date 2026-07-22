// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Operator-driven publication of retained release evidence.

use super::*;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as ShaDigest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const TRANSPARENCY_ENTRY_SCHEMA: &str =
    "https://solpbc.org/schemas/transparency-ledger-entry/v1.json";
pub const TRANSPARENCY_LATEST_SCHEMA: &str =
    "https://solpbc.org/schemas/transparency-latest/v1.json";
pub const TRANSPARENCY_DEFAULT_BASE_URL: &str = "https://transparency.solstone.app";
pub const TRANSPARENCY_PUBLIC_KEY_FILENAME: &str = "solpbc-transparency-1.pub";
pub const TRANSPARENCY_HEAD_LOG: &str = "transparency-head-log.jsonl";
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const MUTABLE_CACHE: &str = "no-cache";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn create(parent: &Path, prefix: &str) -> Result<Self> {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".{prefix}-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).map_err(|error| {
            transparency_error(
                "retryable",
                format!("{prefix} temporary directory"),
                "new writable directory",
                error,
                "restore local staging permissions and retry the transparency command",
            )
        })?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn transparency_error(
    class: &str,
    thing: impl std::fmt::Display,
    expected: impl std::fmt::Display,
    actual: impl std::fmt::Display,
    repair: impl std::fmt::Display,
) -> Error {
    Error::new(format!(
        "{class}: {thing} mismatch: expected {expected}, actual {actual}\nrepair: {repair}"
    ))
}

fn display_error(error: impl std::fmt::Display) -> Error {
    transparency_error(
        "retryable",
        "transparency operation",
        "successful local or subprocess operation",
        error,
        "restore the configured tools and local staging permissions, then retry the transparency command",
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct TransparencyArtifact {
    pub bytes: u64,
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct TransparencyNamedDigest {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyEntry {
    pub artifacts: Vec<TransparencyArtifact>,
    pub manifests: Vec<TransparencyNamedDigest>,
    pub prev_sha256: String,
    pub prev_version: String,
    pub product: String,
    pub proofs: Vec<TransparencyNamedDigest>,
    pub published_utc: String,
    pub schema: String,
    pub seq: u64,
    pub source_commit: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyPointer {
    pub chain_length: u64,
    pub product: String,
    pub schema: String,
    pub signed_at: String,
    pub tip_sha256: String,
    pub valid_until: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransparencyHeadRow {
    pub entry_sha256: String,
    pub product: String,
    pub published_utc: String,
    pub seq: u64,
    pub version: String,
}

fn validate_ascii(value: &Value) -> Result<()> {
    match value {
        Value::String(text) => {
            if !text.is_ascii() {
                return Err(transparency_error(
                    "terminal",
                    "transparency canonical JSON string",
                    "ASCII",
                    "non-ASCII value",
                    "replace the non-ASCII value before retrying",
                ));
            }
        }
        Value::Array(items) => {
            for item in items {
                validate_ascii(item)?;
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if !key.is_ascii() {
                    return Err(transparency_error(
                        "terminal",
                        "transparency canonical JSON key",
                        "ASCII",
                        "non-ASCII value",
                        "replace the non-ASCII key before retrying",
                    ));
                }
                validate_ascii(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn write_transparency_value(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(number) => {
            if !number.is_i64() && !number.is_u64() {
                return Err(transparency_error(
                    "terminal",
                    "transparency canonical JSON number",
                    "integer",
                    "float",
                    "provide an integer-valued transparency document",
                ));
            }
            output.extend_from_slice(number.to_string().as_bytes());
        }
        Value::String(text) => output.extend_from_slice(
            serde_json::to_string(text)
                .map_err(display_error)?
                .as_bytes(),
        ),
        Value::Array(items) => {
            output.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_transparency_value(item, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            write_transparency_object_fields(object.iter().collect(), output)?;
        }
    }
    Ok(())
}

fn write_transparency_object_fields(
    mut fields: Vec<(&String, &Value)>,
    output: &mut Vec<u8>,
) -> Result<()> {
    fields.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    output.push(b'{');
    for (index, (key, item)) in fields.into_iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        output.extend_from_slice(
            serde_json::to_string(key)
                .map_err(display_error)?
                .as_bytes(),
        );
        output.push(b':');
        write_transparency_value(item, output)?;
    }
    output.push(b'}');
    Ok(())
}

pub(crate) fn transparency_canonical_object_fields(fields: &[(String, Value)]) -> Result<Vec<u8>> {
    for (key, value) in fields {
        if !key.is_ascii() {
            return Err(transparency_error(
                "terminal",
                "transparency canonical JSON key",
                "ASCII",
                "non-ASCII value",
                "replace the non-ASCII key before retrying",
            ));
        }
        validate_ascii(value)?;
        if matches!(key.as_str(), "seq" | "bytes" | "chain_length") && value.is_boolean() {
            return Err(transparency_error(
                "terminal",
                format!("transparency {key}"),
                "integer",
                "boolean",
                "provide an integer-valued transparency document",
            ));
        }
        for numeric in ["seq", "bytes", "chain_length"] {
            reject_boolean_numeric_field(value, numeric)?;
        }
    }
    let mut output = Vec::new();
    write_transparency_object_fields(
        fields.iter().map(|(key, value)| (key, value)).collect(),
        &mut output,
    )?;
    output.push(b'\n');
    Ok(output)
}

/// Canonicalizes transparency JSON. This is deliberately separate from rail canonical JSON.
pub fn transparency_canonical_json(value: &Value) -> Result<Vec<u8>> {
    if let Value::Object(object) = value {
        let fields = object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        return transparency_canonical_object_fields(&fields);
    }
    validate_ascii(value)?;
    for numeric in ["seq", "bytes", "chain_length"] {
        reject_boolean_numeric_field(value, numeric)?;
    }
    let mut output = Vec::new();
    write_transparency_value(value, &mut output)?;
    output.push(b'\n');
    Ok(output)
}

fn reject_boolean_numeric_field(value: &Value, field: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            if object.get(field).is_some_and(Value::is_boolean) {
                return Err(transparency_error(
                    "terminal",
                    format!("transparency {field}"),
                    "integer",
                    "boolean",
                    "provide an integer-valued transparency document",
                ));
            }
            for child in object.values() {
                reject_boolean_numeric_field(child, field)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                reject_boolean_numeric_field(child, field)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn entry_trusted_comment(entry: &TransparencyEntry, identity: &str) -> String {
    format!(
        "solpbc-transparency-v1 entry product={} seq={} version={} sha256={} prev={}",
        entry.product, entry.seq, entry.version, identity, entry.prev_sha256
    )
}

pub fn pointer_trusted_comment(pointer: &TransparencyPointer) -> String {
    format!(
        "solpbc-transparency-v1 latest product={} chain_length={} tip={} valid_until={}",
        pointer.product, pointer.chain_length, pointer.tip_sha256, pointer.valid_until
    )
}

fn exact_timestamp(label: &str, timestamp: &str) -> Result<DateTime<Utc>> {
    if timestamp.len() != 20
        || !timestamp.ends_with('Z')
        || timestamp.as_bytes().get(4) != Some(&b'-')
        || timestamp.as_bytes().get(7) != Some(&b'-')
        || timestamp.as_bytes().get(10) != Some(&b'T')
        || timestamp.as_bytes().get(13) != Some(&b':')
        || timestamp.as_bytes().get(16) != Some(&b':')
    {
        return Err(transparency_error(
            "terminal",
            label,
            "YYYY-MM-DDTHH:MM:SSZ",
            timestamp,
            "provide an exact UTC-seconds timestamp",
        ));
    }
    DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            transparency_error(
                "terminal",
                label,
                "valid UTC timestamp",
                timestamp,
                "provide an exact UTC-seconds timestamp",
            )
        })
}

pub fn validate_entry(
    entry: &TransparencyEntry,
    previous: Option<&TransparencyEntry>,
) -> Result<()> {
    if entry.schema != TRANSPARENCY_ENTRY_SCHEMA || entry.product != PRODUCT {
        return Err(transparency_error(
            "terminal",
            "transparency entry identity",
            format!("schema {TRANSPARENCY_ENTRY_SCHEMA} and product {PRODUCT}"),
            format!("schema {} and product {}", entry.schema, entry.product),
            "use the transparency chain for solstone-linux",
        ));
    }
    let exact_source_commit = entry.source_commit.len() == 40
        && entry
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !exact_source_commit || !is_sha256(&entry.prev_sha256) {
        return Err(transparency_error(
            "terminal",
            "transparency entry binding",
            "lowercase commit and SHA-256",
            "invalid binding",
            "rebuild the entry from validated candidate state",
        ));
    }
    exact_timestamp("transparency published_utc", &entry.published_utc)?;
    let mut artifacts = entry.artifacts.clone();
    artifacts.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let mut manifests = entry.manifests.clone();
    manifests.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let mut proofs = entry.proofs.clone();
    proofs.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    if artifacts != entry.artifacts || manifests != entry.manifests || proofs != entry.proofs {
        return Err(transparency_error(
            "terminal",
            "transparency inventory order",
            "name-sorted arrays",
            "unsorted array",
            "rebuild from the validated candidate",
        ));
    }
    match previous {
        None if entry.seq == 1
            && entry.prev_sha256 == ZERO_SHA256
            && entry.prev_version.is_empty() => {}
        Some(previous) => {
            let previous_bytes = transparency_canonical_json(
                &serde_json::to_value(previous).map_err(display_error)?,
            )?;
            let expected_digest = digest(&previous_bytes);
            let previous_time = exact_timestamp("previous published_utc", &previous.published_utc)?;
            let current_time = exact_timestamp("transparency published_utc", &entry.published_utc)?;
            if current_time <= previous_time {
                return Err(transparency_error(
                    "retryable",
                    "transparency published_utc progression",
                    format!("later than {}", previous.published_utc),
                    &entry.published_utc,
                    "wait one second, discard the incomplete staging directory, and retry make publish-transparency",
                ));
            }
            if entry.seq != previous.seq + 1
                || entry.prev_sha256 != expected_digest
                || entry.prev_version != previous.version
            {
                return Err(transparency_error(
                    "terminal",
                    "transparency chain linkage",
                    format!(
                        "seq {} prev {} version {} and later time",
                        previous.seq + 1,
                        expected_digest,
                        previous.version
                    ),
                    format!(
                        "seq {} prev {} version {}",
                        entry.seq, entry.prev_sha256, entry.prev_version
                    ),
                    "restore the verified transparency chain before retrying",
                ));
            }
        }
        None => {
            return Err(transparency_error(
                "terminal",
                "transparency genesis",
                "seq 1, zero previous digest, and empty previous version",
                "invalid genesis binding",
                "start genesis only with the fixed genesis values",
            ));
        }
    }
    Ok(())
}

pub fn validate_pointer(pointer: &TransparencyPointer, tip: &TransparencyEntry) -> Result<()> {
    let tip_bytes =
        transparency_canonical_json(&serde_json::to_value(tip).map_err(display_error)?)?;
    let tip_identity = digest(&tip_bytes);
    let signed = exact_timestamp("transparency pointer signed_at", &pointer.signed_at)?;
    let valid = exact_timestamp("transparency pointer valid_until", &pointer.valid_until)?;
    if pointer.schema != TRANSPARENCY_LATEST_SCHEMA
        || pointer.product != PRODUCT
        || pointer.chain_length != tip.seq
        || pointer.tip_sha256 != tip_identity
        || pointer.version != tip.version
        || valid != signed + Duration::days(14)
    {
        return Err(transparency_error(
            "terminal",
            "transparency pointer binding",
            "verified solstone-linux tip and fourteen-day validity",
            "different pointer semantics",
            "restore the signed pointer for the verified tip",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    S3 {
        endpoint: String,
        bucket: String,
        key: String,
    },
    Public {
        base_url: String,
        key: String,
    },
}

impl Destination {
    fn url(&self) -> String {
        match self {
            Self::S3 {
                endpoint,
                bucket,
                key,
            } => format!("{}/{}/{}", endpoint.trim_end_matches('/'), bucket, key),
            Self::Public { base_url, key } => {
                format!("{}/{}", base_url.trim_end_matches('/'), key)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportResponse {
    pub http_status: u16,
    pub body: Vec<u8>,
    pub etag: Option<String>,
    pub process_exit: i32,
}

pub trait TransparencyTransport {
    fn get(&mut self, destination: &Destination, cache_bypass: bool) -> Result<TransportResponse>;
    fn put_create_only(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse>;
    fn put_conditional(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        etag: &str,
        cache_control: &str,
    ) -> Result<TransportResponse>;
    fn put_mutable(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse>;
    fn list(&mut self, destination: &Destination, prefix: &str) -> Result<TransportResponse>;
}

pub trait ArchiveChannel {
    fn archive(&mut self, staging: &Path, manifest_sha256: &str) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveResponse {
    pub exit_status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct CurlTransport {
    endpoint: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    response_root: PathBuf,
    sequence: u64,
}

impl CurlTransport {
    fn new(config: &TransparencyConfig, root: &Path) -> Self {
        Self {
            endpoint: config.s3_endpoint.clone(),
            bucket: config.bucket.clone(),
            access_key: config.access_key.clone(),
            secret_key: config.secret_key.clone(),
            response_root: root.to_owned(),
            sequence: 0,
        }
    }

    fn execute(
        &mut self,
        destination: &Destination,
        method_args: &[&OsStr],
        cache_bypass: bool,
    ) -> Result<TransportResponse> {
        self.sequence += 1;
        let body_path = self
            .response_root
            .join(format!(".curl-body-{}", self.sequence));
        let header_path = self
            .response_root
            .join(format!(".curl-headers-{}", self.sequence));
        let mut command = Command::new("curl");
        command.args([
            "--silent",
            "--show-error",
            "--aws-sigv4",
            "aws:amz:auto:s3",
            "--user",
            &format!("{}:{}", self.access_key, self.secret_key),
            "--output",
        ]);
        command
            .arg(&body_path)
            .arg("--dump-header")
            .arg(&header_path);
        command.args(["--write-out", "%{http_code}"]);
        if cache_bypass {
            command.args(["--header", "Cache-Control: no-cache"]);
        }
        command.args(method_args).arg(destination.url());
        let output = command.output().map_err(display_error)?;
        let status_text = String::from_utf8_lossy(&output.stdout);
        let http_status = status_text.trim().parse::<u16>().unwrap_or(0);
        let body = fs::read(&body_path).unwrap_or_default();
        let headers = fs::read_to_string(&header_path).unwrap_or_default();
        let etag = headers.lines().rev().find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("etag")
                    .then(|| value.trim().to_owned())
            })
        });
        let _ = fs::remove_file(body_path);
        let _ = fs::remove_file(header_path);
        Ok(TransportResponse {
            http_status,
            body,
            etag,
            process_exit: output.status.code().unwrap_or(-1),
        })
    }

    fn assert_s3(&self, destination: &Destination) -> Result<()> {
        match destination {
            Destination::S3 {
                endpoint, bucket, ..
            } if endpoint == &self.endpoint && bucket == &self.bucket => Ok(()),
            _ => Err(transparency_error(
                "terminal",
                "transparency transport destination",
                "configured S3 endpoint and bucket",
                "different destination",
                "use only the configured transparency destinations",
            )),
        }
    }
}

impl TransparencyTransport for CurlTransport {
    fn get(&mut self, destination: &Destination, cache_bypass: bool) -> Result<TransportResponse> {
        self.execute(destination, &[], cache_bypass)
    }

    fn put_create_only(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.assert_s3(destination)?;
        let upload = self
            .response_root
            .join(format!(".curl-upload-{}", self.sequence + 1));
        fs::write(&upload, bytes).map_err(display_error)?;
        let header = format!("Cache-Control: {cache_control}");
        let args = [
            OsStr::new("--upload-file"),
            upload.as_os_str(),
            OsStr::new("--header"),
            OsStr::new("If-None-Match: *"),
            OsStr::new("--header"),
            OsStr::new(&header),
        ];
        let response = self.execute(destination, &args, false);
        let _ = fs::remove_file(upload);
        response
    }

    fn put_conditional(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        etag: &str,
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.assert_s3(destination)?;
        let upload = self
            .response_root
            .join(format!(".curl-upload-{}", self.sequence + 1));
        fs::write(&upload, bytes).map_err(display_error)?;
        let cache = format!("Cache-Control: {cache_control}");
        let condition = format!("If-Match: {etag}");
        let args = [
            OsStr::new("--upload-file"),
            upload.as_os_str(),
            OsStr::new("--header"),
            OsStr::new(&condition),
            OsStr::new("--header"),
            OsStr::new(&cache),
        ];
        let response = self.execute(destination, &args, false);
        let _ = fs::remove_file(upload);
        response
    }

    fn put_mutable(
        &mut self,
        destination: &Destination,
        bytes: &[u8],
        cache_control: &str,
    ) -> Result<TransportResponse> {
        self.assert_s3(destination)?;
        let upload = self
            .response_root
            .join(format!(".curl-upload-{}", self.sequence + 1));
        fs::write(&upload, bytes).map_err(|error| {
            transparency_error(
                "retryable",
                "transparency mutable upload staging",
                "writable temporary file",
                error,
                "restore local staging permissions and retry the transparency command",
            )
        })?;
        let cache = format!("Cache-Control: {cache_control}");
        let args = [
            OsStr::new("--upload-file"),
            upload.as_os_str(),
            OsStr::new("--header"),
            OsStr::new(&cache),
        ];
        let response = self.execute(destination, &args, false);
        let _ = fs::remove_file(upload);
        response
    }

    fn list(&mut self, destination: &Destination, prefix: &str) -> Result<TransportResponse> {
        self.assert_s3(destination)?;
        let separator = if destination.url().contains('?') {
            '&'
        } else {
            '?'
        };
        let listed = match destination {
            Destination::S3 {
                endpoint, bucket, ..
            } => Destination::S3 {
                endpoint: endpoint.clone(),
                bucket: bucket.clone(),
                key: format!(
                    "{separator}list-type=2&prefix={}",
                    prefix.replace('/', "%2F")
                ),
            },
            _ => unreachable!(),
        };
        self.execute(&listed, &[], true)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TransparencyConfig {
    pub(crate) base_url: String,
    pub(crate) s3_endpoint: String,
    pub(crate) bucket: String,
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) minisign_key: PathBuf,
    pub(crate) minisign_pub: PathBuf,
    pub(crate) archive_channel: Option<String>,
    pub(crate) genesis: bool,
}

impl TransparencyConfig {
    fn from_env(require_archive: bool) -> Result<Self> {
        let required = |name: &str| {
            env::var(name).map_err(|_| {
                transparency_error(
                    "retryable",
                    "transparency environment",
                    name,
                    "missing",
                    format!("set {name} and retry the transparency command"),
                )
            })
        };
        let archive_channel = env::var("TRANSPARENCY_ARCHIVE_CHANNEL").ok();
        if require_archive && archive_channel.is_none() {
            return Err(transparency_error(
                "retryable",
                "transparency archive channel",
                "TRANSPARENCY_ARCHIVE_CHANNEL",
                "missing",
                "set TRANSPARENCY_ARCHIVE_CHANNEL and retry make publish-transparency",
            ));
        }
        let config = Self {
            base_url: env::var("TRANSPARENCY_BASE_URL")
                .unwrap_or_else(|_| TRANSPARENCY_DEFAULT_BASE_URL.into()),
            s3_endpoint: required("TRANSPARENCY_S3_ENDPOINT")?,
            bucket: required("TRANSPARENCY_BUCKET")?,
            access_key: required("TRANSPARENCY_S3_ACCESS_KEY_ID")?,
            secret_key: required("TRANSPARENCY_S3_SECRET_ACCESS_KEY")?,
            minisign_key: required("TRANSPARENCY_MINISIGN_KEY")?.into(),
            minisign_pub: required("TRANSPARENCY_MINISIGN_PUB")?.into(),
            archive_channel,
            genesis: env::var("TRANSPARENCY_GENESIS").is_ok_and(|value| value == "1"),
        };
        for (label, value) in [
            ("TRANSPARENCY_BASE_URL", config.base_url.as_str()),
            ("TRANSPARENCY_S3_ENDPOINT", config.s3_endpoint.as_str()),
        ] {
            if !value.starts_with("https://") || value.chars().any(char::is_control) {
                return Err(transparency_error(
                    "terminal",
                    "transparency URL",
                    "HTTPS URL",
                    format!("invalid {label}"),
                    format!("set {label} to its approved HTTPS value"),
                ));
            }
        }
        Ok(config)
    }
}

struct CommandArchive {
    command: String,
}

impl ArchiveChannel for CommandArchive {
    fn archive(&mut self, staging: &Path, manifest_sha256: &str) -> Result<()> {
        let output = Command::new("sh")
            .args(["-c", "exec $1 $2", "transparency-archive", &self.command])
            .arg(staging)
            .output()
            .map_err(display_error)?;
        let response = ArchiveResponse {
            exit_status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        };
        let expected = format!("ARCHIVED {manifest_sha256}");
        if response.exit_status != 0
            || String::from_utf8_lossy(&response.stdout).lines().last() != Some(&expected)
        {
            return Err(transparency_error(
                "retryable",
                "transparency archive receipt",
                expected,
                format!(
                    "stdout {}; stderr {}",
                    sanitize_process_stderr(&response.stdout),
                    sanitize_process_stderr(&response.stderr)
                ),
                "repair the archive channel and retry make publish-transparency",
            ));
        }
        Ok(())
    }
}

fn s3(config: &TransparencyConfig, key: &str) -> Destination {
    Destination::S3 {
        endpoint: config.s3_endpoint.clone(),
        bucket: config.bucket.clone(),
        key: key.into(),
    }
}

fn public(config: &TransparencyConfig, key: &str) -> Destination {
    Destination::Public {
        base_url: config.base_url.clone(),
        key: key.into(),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| {
        transparency_error(
            "terminal",
            label,
            "strict JSON",
            "invalid bytes",
            "restore the signed transparency object and retry",
        )
    })
}

fn highest_head_seq(root: &Path) -> Result<u64> {
    let path = root.join(TRANSPARENCY_HEAD_LOG);
    let bytes = fs::read(&path).map_err(display_error)?;
    let mut highest = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line == b"\n" || line.is_empty() {
            continue;
        }
        if !line.ends_with(b"\n") {
            return Err(transparency_error(
                "terminal",
                "transparency head log",
                "newline-terminated JSONL",
                "partial row",
                "restore transparency-head-log.jsonl from version control",
            ));
        }
        let row: TransparencyHeadRow =
            parse_json(&line[..line.len() - 1], "transparency head log row")?;
        if row.product != PRODUCT {
            return Err(transparency_error(
                "terminal",
                "transparency head log product",
                PRODUCT,
                row.product,
                "restore transparency-head-log.jsonl from version control",
            ));
        }
        highest = highest.max(row.seq);
    }
    Ok(highest)
}

fn ensure_http(response: &TransportResponse, expected: &[u16], label: &str) -> Result<()> {
    if !expected.contains(&response.http_status) {
        return Err(transparency_error(
            "retryable",
            label,
            format!("HTTP {expected:?}"),
            format!(
                "HTTP {} with {} body bytes",
                response.http_status,
                response.body.len()
            ),
            "retry after restoring the transparency transport",
        ));
    }
    Ok(())
}

fn s3_list_is_empty(bytes: &[u8]) -> Result<bool> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        transparency_error(
            "retryable",
            "transparency genesis LIST XML",
            "UTF-8 ListBucketResult",
            "non-UTF-8 response",
            "retry after restoring the transparency S3 plane",
        )
    })?;
    let start = text.find("<KeyCount>").ok_or_else(|| {
        transparency_error(
            "retryable",
            "transparency genesis LIST XML",
            "one KeyCount element",
            "missing KeyCount",
            "retry after restoring the transparency S3 plane",
        )
    })? + "<KeyCount>".len();
    let remainder = &text[start..];
    let end = remainder.find("</KeyCount>").ok_or_else(|| {
        transparency_error(
            "retryable",
            "transparency genesis LIST XML",
            "closed KeyCount element",
            "malformed KeyCount",
            "retry after restoring the transparency S3 plane",
        )
    })?;
    let count = remainder[..end].parse::<u64>().map_err(|_| {
        transparency_error(
            "retryable",
            "transparency genesis LIST XML",
            "decimal KeyCount",
            &remainder[..end],
            "retry after restoring the transparency S3 plane",
        )
    })?;
    Ok(count == 0 && !text.contains("<Contents>"))
}

fn verify_minisign_bytes(
    root: &Path,
    public_key: &Path,
    message: &[u8],
    signature: &[u8],
    label: &str,
) -> Result<()> {
    let temporary_parent = root.parent().unwrap_or(root);
    let temporary = TemporaryDirectory::create(temporary_parent, "transparency-verify")?;
    let message_path = temporary.path().join("message.json");
    let signature_path = temporary.path().join("message.minisig");
    fs::write(&message_path, message).map_err(display_error)?;
    fs::write(&signature_path, signature).map_err(display_error)?;
    let status = Command::new("minisign")
        .args(["-V", "-q", "-p"])
        .arg(public_key)
        .arg("-m")
        .arg(&message_path)
        .arg("-x")
        .arg(&signature_path)
        .status()
        .map_err(display_error)?;
    if !status.success() {
        return Err(transparency_error(
            "terminal",
            label,
            "valid minisign signature",
            "verification failure",
            "restore the object signed by the configured transparency public key",
        ));
    }
    Ok(())
}

pub(crate) trait TransparencySignatureVerifier {
    fn verify(
        &mut self,
        root: &Path,
        public_key: &Path,
        message: &[u8],
        signature: &[u8],
        label: &str,
    ) -> Result<()>;
}

struct MinisignVerifier;

impl TransparencySignatureVerifier for MinisignVerifier {
    fn verify(
        &mut self,
        root: &Path,
        public_key: &Path,
        message: &[u8],
        signature: &[u8],
        label: &str,
    ) -> Result<()> {
        verify_minisign_bytes(root, public_key, message, signature, label)
    }
}

pub(crate) fn verify_trusted_comment(signature: &[u8], expected: &str, label: &str) -> Result<()> {
    let text = std::str::from_utf8(signature).map_err(|_| {
        transparency_error(
            "terminal",
            label,
            "UTF-8 minisign signature",
            "invalid bytes",
            "restore the signed transparency object",
        )
    })?;
    let actual = text
        .lines()
        .find_map(|line| line.strip_prefix("trusted comment: "));
    if actual != Some(expected) {
        return Err(transparency_error(
            "terminal",
            label,
            expected,
            actual.unwrap_or("missing"),
            "restore the signature with the exact transparency trusted comment",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedChain {
    pub(crate) pointer: Option<TransparencyPointer>,
    pub(crate) pointer_bytes: Option<Vec<u8>>,
    pub(crate) pointer_etag: Option<String>,
    pub(crate) tip: Option<TransparencyEntry>,
    pub(crate) transparency_ledger: Vec<u8>,
}

pub(crate) fn validate_transparency_ledger(bytes: &[u8], tip: &TransparencyEntry) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut previous = None;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            return Err(transparency_error(
                "terminal",
                "transparency ledger",
                "newline-terminated canonical entries",
                "partial final line",
                "restore the transparency ledger from locked entries",
            ));
        }
        let entry: TransparencyEntry = parse_json(line, "transparency ledger entry")?;
        let canonical =
            transparency_canonical_json(&serde_json::to_value(&entry).map_err(display_error)?)?;
        if canonical != line {
            return Err(transparency_error(
                "terminal",
                "transparency ledger canonical bytes",
                digest(&canonical),
                digest(line),
                "re-derive the transparency ledger from locked entries",
            ));
        }
        validate_entry(&entry, previous.as_ref())?;
        previous = Some(entry);
    }
    if let Some(last) = previous
        && last.seq >= tip.seq
        && (last.seq == tip.seq && last != *tip)
    {
        return Err(transparency_error(
            "terminal",
            "transparency ledger locked tip",
            format!("entry {}", tip.seq),
            format!("contradictory entry {}", last.seq),
            "restore the transparency ledger from locked entries",
        ));
    }
    Ok(())
}

fn parsed_transparency_ledger(bytes: &[u8]) -> Result<Vec<(TransparencyEntry, Vec<u8>)>> {
    let mut previous = None;
    let mut parsed = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            return Err(transparency_error(
                "retryable",
                "transparency ledger structure",
                "newline-terminated canonical entries",
                "partial or empty final line",
                "re-derive the transparency ledger from locked entries",
            ));
        }
        let entry: TransparencyEntry = parse_json(line, "transparency ledger entry")?;
        let canonical =
            transparency_canonical_json(&serde_json::to_value(&entry).map_err(display_error)?)?;
        if canonical != line {
            return Err(transparency_error(
                "retryable",
                "transparency ledger canonical bytes",
                digest(&canonical),
                digest(line),
                "re-derive the transparency ledger from locked entries",
            ));
        }
        validate_entry(&entry, previous.as_ref())?;
        previous = Some(entry.clone());
        parsed.push((entry, line.to_vec()));
    }
    Ok(parsed)
}

fn reject_locked_ledger_contradiction(fetched: &[u8], locked: &[u8]) -> Result<()> {
    let fetched = match parsed_transparency_ledger(fetched) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let locked = parsed_transparency_ledger(locked)?;
    let locked_by_seq = locked
        .into_iter()
        .map(|(entry, bytes)| (entry.seq, bytes))
        .collect::<BTreeMap<_, _>>();
    for (entry, bytes) in fetched {
        if let Some(expected) = locked_by_seq.get(&entry.seq)
            && expected != &bytes
        {
            return Err(transparency_error(
                "terminal",
                "transparency ledger locked entry",
                digest(expected),
                format!("seq {} digest {}", entry.seq, digest(&bytes)),
                "stop and restore the transparency ledger from the signed locked entries",
            ));
        }
    }
    Ok(())
}

fn transparency_ledger_has_tip(bytes: &[u8], tip_sha256: &str) -> bool {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .next_back()
        .is_some_and(|line| line.ends_with(b"\n") && digest(line) == tip_sha256)
}

fn rederive_transparency_ledger<T: TransparencyTransport, V: TransparencySignatureVerifier>(
    root: &Path,
    config: &TransparencyConfig,
    transport: &mut T,
    verifier: &mut V,
    tip: &TransparencyEntry,
    tip_bytes: &[u8],
) -> Result<Vec<u8>> {
    let prefix = format!("releases/{PRODUCT}/v");
    let mut entries = vec![(tip.clone(), tip_bytes.to_vec())];
    while let Some((current, _)) = entries.last() {
        if current.seq == 1 {
            break;
        }
        let key = format!("{prefix}/{}/ledger-entry.json", current.prev_version);
        let response = transport.get(&s3(config, &key), false)?;
        let signature = transport.get(&s3(config, &format!("{key}.minisig")), false)?;
        ensure_http(&response, &[200], "locked transparency entry GET")?;
        ensure_http(
            &signature,
            &[200],
            "locked transparency entry signature GET",
        )?;
        verifier.verify(
            root,
            &config.minisign_pub,
            &response.body,
            &signature.body,
            "locked transparency entry signature",
        )?;
        let previous: TransparencyEntry = parse_json(&response.body, "locked transparency entry")?;
        verify_trusted_comment(
            &signature.body,
            &entry_trusted_comment(&previous, &digest(&response.body)),
            "locked transparency entry trusted comment",
        )?;
        let canonical =
            transparency_canonical_json(&serde_json::to_value(&previous).map_err(display_error)?)?;
        if canonical != response.body {
            return Err(transparency_error(
                "terminal",
                "locked transparency entry canonical bytes",
                digest(&canonical),
                digest(&response.body),
                "restore the signed locked transparency entry",
            ));
        }
        validate_entry(current, Some(&previous))?;
        entries.push((previous, response.body));
    }
    entries.reverse();
    Ok(entries.into_iter().flat_map(|(_, bytes)| bytes).collect())
}

pub(crate) fn fetch_verified_chain<T: TransparencyTransport, V: TransparencySignatureVerifier>(
    root: &Path,
    config: &TransparencyConfig,
    transport: &mut T,
    verifier: &mut V,
    allow_genesis: bool,
) -> Result<VerifiedChain> {
    let prefix = format!("releases/{PRODUCT}");
    let pointer_response = transport.get(&s3(config, &format!("{prefix}/latest.json")), true)?;
    if pointer_response.http_status == 404 {
        if !allow_genesis || !config.genesis {
            return Err(transparency_error(
                "terminal",
                "transparency genesis approval",
                "TRANSPARENCY_GENESIS=1",
                "missing",
                "set TRANSPARENCY_GENESIS=1 only after confirming first publication",
            ));
        }
        let listed = transport.list(&s3(config, ""), &format!("{prefix}/v/"))?;
        ensure_http(&listed, &[200], "transparency genesis LIST")?;
        if !s3_list_is_empty(&listed.body)? {
            return Err(transparency_error(
                "terminal",
                "transparency genesis prefix",
                "no existing object",
                "existing object",
                "cut a new version after reconciling the existing transparency chain",
            ));
        }
        return Ok(VerifiedChain {
            pointer: None,
            pointer_bytes: None,
            pointer_etag: None,
            tip: None,
            transparency_ledger: Vec::new(),
        });
    }
    ensure_http(&pointer_response, &[200], "transparency pointer GET")?;
    let signature = transport.get(&s3(config, &format!("{prefix}/latest.json.minisig")), true)?;
    ensure_http(&signature, &[200], "transparency pointer signature GET")?;
    verifier.verify(
        root,
        &config.minisign_pub,
        &pointer_response.body,
        &signature.body,
        "transparency pointer signature",
    )?;
    let pointer: TransparencyPointer = parse_json(&pointer_response.body, "transparency pointer")?;
    verify_trusted_comment(
        &signature.body,
        &pointer_trusted_comment(&pointer),
        "transparency pointer trusted comment",
    )?;
    if pointer.product != PRODUCT {
        return Err(transparency_error(
            "terminal",
            "transparency pointer product",
            PRODUCT,
            pointer.product,
            "use the solstone-linux transparency chain",
        ));
    }
    let tip_key = format!("{prefix}/v/{}/ledger-entry.json", pointer.version);
    let tip_response = transport.get(&s3(config, &tip_key), false)?;
    let tip_signature = transport.get(&s3(config, &format!("{tip_key}.minisig")), false)?;
    ensure_http(&tip_response, &[200], "transparency tip GET")?;
    ensure_http(&tip_signature, &[200], "transparency tip signature GET")?;
    verifier.verify(
        root,
        &config.minisign_pub,
        &tip_response.body,
        &tip_signature.body,
        "transparency tip signature",
    )?;
    let tip: TransparencyEntry = parse_json(&tip_response.body, "transparency tip entry")?;
    verify_trusted_comment(
        &tip_signature.body,
        &entry_trusted_comment(&tip, &digest(&tip_response.body)),
        "transparency tip trusted comment",
    )?;
    validate_pointer(&pointer, &tip)?;
    let highest = highest_head_seq(root)?;
    if pointer.chain_length < highest {
        return Err(transparency_error(
            "terminal",
            "transparency chain rollback",
            format!("chain_length at least {highest}"),
            pointer.chain_length,
            "stop and reconcile the transparency S3 plane with the transparency head log",
        ));
    }
    let ledger_response = transport.get(&s3(config, &format!("{prefix}/ledger.jsonl")), true)?;
    let fetched_ledger = if ledger_response.http_status == 404 {
        None
    } else {
        ensure_http(&ledger_response, &[200], "transparency ledger GET")?;
        Some(ledger_response.body)
    };
    let transparency_ledger = match fetched_ledger {
        Some(bytes)
            if validate_transparency_ledger(&bytes, &tip).is_ok()
                && transparency_ledger_has_tip(&bytes, &pointer.tip_sha256) =>
        {
            bytes
        }
        fetched => {
            let locked = rederive_transparency_ledger(
                root,
                config,
                transport,
                verifier,
                &tip,
                &tip_response.body,
            )?;
            if let Some(bytes) = fetched {
                reject_locked_ledger_contradiction(&bytes, &locked)?;
            }
            locked
        }
    };
    validate_transparency_ledger(&transparency_ledger, &tip)?;
    Ok(VerifiedChain {
        pointer: Some(pointer),
        pointer_bytes: Some(pointer_response.body),
        pointer_etag: pointer_response.etag,
        tip: Some(tip),
        transparency_ledger,
    })
}

fn sign_file(
    secret_key: &Path,
    message: &Path,
    signature: &Path,
    comment: &str,
    passphrase: &[u8],
) -> Result<()> {
    let mut child = Command::new("minisign")
        .args(["-S", "-s"])
        .arg(secret_key)
        .arg("-m")
        .arg(message)
        .arg("-x")
        .arg(signature)
        .args(["-t", comment])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(display_error)?;
    let stdin = child.stdin.as_mut().ok_or_else(|| Error::new("terminal: minisign stdin mismatch: expected pipe, actual unavailable\nrepair: restore minisign and retry"))?;
    stdin.write_all(passphrase).map_err(display_error)?;
    stdin.write_all(b"\n").map_err(display_error)?;
    let status = child.wait().map_err(display_error)?;
    if !status.success() {
        return Err(transparency_error(
            "retryable",
            "minisign signing",
            "successful signature",
            "signing failure",
            "verify the encrypted transparency key and retry",
        ));
    }
    Ok(())
}

fn read_passphrase_once() -> Result<Vec<u8>> {
    let status = Command::new("stty")
        .arg("-echo")
        .status()
        .map_err(display_error)?;
    if !status.success() {
        return Err(transparency_error(
            "retryable",
            "passphrase terminal",
            "echo disabled",
            "stty failure",
            "run from an interactive terminal",
        ));
    }
    eprint!("Password: ");
    let mut value = String::new();
    let read = std::io::stdin().read_line(&mut value);
    let _ = Command::new("stty").arg("echo").status();
    eprintln!();
    read.map_err(display_error)?;
    while value.ends_with(['\n', '\r']) {
        value.pop();
    }
    Ok(value.into_bytes())
}

fn observed_version(program: &str, argument: &str) -> Result<String> {
    let output = Command::new(program).arg(argument).output().map_err(|_| {
        transparency_error(
            "retryable",
            "transparency tool",
            program,
            "missing",
            format!("install {program} and retry"),
        )
    })?;
    if !output.status.success() {
        return Err(transparency_error(
            "retryable",
            "transparency tool",
            program,
            "unavailable",
            format!("install {program} and retry"),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate_tools() -> Result<()> {
    let minisign = observed_version("minisign", "-v")?;
    if minisign != "minisign 0.11" && minisign != "minisign 0.12" {
        return Err(transparency_error(
            "terminal",
            "minisign version",
            "minisign 0.11 or 0.12",
            minisign,
            "install minisign 0.12 and retry",
        ));
    }
    let curl = observed_version("curl", "--version")?;
    let first = curl.lines().next().unwrap_or_default();
    let version = first.split_whitespace().nth(1).unwrap_or_default();
    let parts = version
        .split('.')
        .filter_map(|part| part.parse::<u64>().ok())
        .collect::<Vec<_>>();
    if parts.as_slice() < [7, 75].as_slice() {
        return Err(transparency_error(
            "terminal",
            "curl version",
            "curl 7.75 or newer",
            version,
            "install curl 7.75 or newer and retry",
        ));
    }
    Ok(())
}

pub(crate) fn append_head_row(root: &Path, row: &TransparencyHeadRow) -> Result<&'static str> {
    let path = root.join(TRANSPARENCY_HEAD_LOG);
    let existing = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Ok("witness unavailable; gap"),
    };
    for line in existing
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let old: TransparencyHeadRow = parse_json(line, "transparency head log row")?;
        if old.product == row.product && old.seq == row.seq {
            if old.entry_sha256 != row.entry_sha256 {
                return Err(transparency_error(
                    "terminal",
                    "transparency head log fork",
                    &row.entry_sha256,
                    old.entry_sha256,
                    "stop and reconcile the conflicting transparency heads",
                ));
            }
            return Ok("written and committed or previously recorded");
        }
    }
    let bytes = transparency_canonical_json(&serde_json::to_value(row).map_err(display_error)?)?;
    let mut file = match OpenOptions::new().append(true).open(&path) {
        Ok(file) => file,
        Err(_) => return Ok("witness unavailable; gap"),
    };
    if file.write_all(&bytes).is_err() {
        return Ok("witness unavailable; gap");
    }
    Ok("written uncommitted; gap: git add transparency-head-log.jsonl && git commit")
}

pub(crate) fn validate_previous_head_committed(root: &Path) -> Result<()> {
    let worktree = fs::read(root.join(TRANSPARENCY_HEAD_LOG)).map_err(display_error)?;
    let committed = Command::new("git")
        .args(["show", &format!("HEAD:{TRANSPARENCY_HEAD_LOG}")])
        .current_dir(root)
        .output()
        .map_err(display_error)?;
    if committed.status.success() && committed.stdout != worktree {
        return Err(transparency_error(
            "terminal",
            "previous transparency head row",
            "committed row",
            "present but uncommitted",
            "git add transparency-head-log.jsonl && git commit",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct CandidateSnapshot {
    pub(crate) staging: PathBuf,
    pub(crate) manifest: Manifest,
    pub(crate) proofs: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn snapshot_candidate(root: &RepoRoot, release_dir: &Path) -> Result<CandidateSnapshot> {
    classify_release_dir(root, release_dir)?;
    let manifest_path = fs::read_dir(release_dir).map_err(display_error)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.file_name().and_then(OsStr::to_str).is_some_and(|name| name.ends_with(".rust-release-manifest.json")))
        .ok_or_else(|| Error::new("terminal: transparency manifest mismatch: expected one companion manifest, actual missing\nrepair: restore the retained five-file candidate"))?;
    let manifest =
        validate_manifest_bytes(root, &fs::read(&manifest_path).map_err(display_error)?)?;
    if manifest.source_dirty {
        return Err(transparency_error(
            "terminal",
            "transparency candidate source",
            format!("commit {} with source_dirty=false", manifest.source_commit),
            format!("commit {} with source_dirty=true", manifest.source_commit),
            "cut a clean-source candidate before publishing transparency evidence",
        ));
    }
    let parent = release_dir.parent().ok_or_else(|| Error::new("terminal: transparency release parent mismatch: expected parent, actual missing\nrepair: provide the retained release directory"))?;
    let evidence = parent.join("rust-evidence").join(&manifest.version);
    let rail_ledger_bytes = fs::read(evidence.join("ledger.json")).map_err(|_| {
        transparency_error(
            "terminal",
            "transparency evidence",
            "rail ledger and three bound proofs",
            "rail ledger missing",
            format!(
                "retain complete evidence for version {} and retry",
                manifest.version
            ),
        )
    })?;
    let rail_ledger: CandidateLedger =
        serde_json::from_slice(&rail_ledger_bytes).map_err(display_error)?;
    if rail_ledger.version != manifest.version
        || rail_ledger.source.commit != manifest.source_commit
    {
        return Err(transparency_error(
            "terminal",
            "rail ledger candidate binding",
            format!(
                "version {} commit {}",
                manifest.version, manifest.source_commit
            ),
            format!(
                "version {} commit {}",
                rail_ledger.version, rail_ledger.source.commit
            ),
            "restore the rail ledger for this candidate",
        ));
    }
    let policies = ReleaseImages::from_root(root.path())?;
    let staging = root
        .path()
        .join(".transparency-staging")
        .join(PRODUCT)
        .join(&manifest.version);
    let existing_staging = staging.exists();
    let mut proofs = BTreeMap::new();
    for spec in PROOF_SPECS {
        let name = format!("{}.json", spec.id);
        let path = if existing_staging {
            staging.join(&name)
        } else {
            evidence.join("proofs").join(&name)
        };
        let bytes = fs::read(&path).map_err(|_| {
            transparency_error(
                "terminal",
                "transparency proof inventory",
                "debian-amd64, rpm-x86_64, and tar-x86_64",
                format!("{} missing", spec.id),
                format!(
                    "retain complete evidence for version {} and retry",
                    manifest.version
                ),
            )
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(display_error)?;
        let artifact = proof_artifact(&rail_ledger, spec.id)?;
        let member = proof_member(&rail_ledger, spec.id)?;
        let policy = policies.proof_policy(spec.id)?;
        let proof_time = value
            .get("proof_time")
            .and_then(Value::as_str)
            .unwrap_or(&rail_ledger.policy.checked_at);
        validate_candidate_proof(
            &value,
            &ProofBindings {
                platform: spec.id.into(),
                candidate_digest: rail_ledger.candidate_digest.clone(),
                ledger_sha256: digest(&rail_ledger_bytes),
                source_commit: rail_ledger.source.commit.clone(),
                cargo_lock_sha256: rail_ledger.source.cargo_lock_sha256.clone(),
                artifact_basename: artifact.path.clone(),
                artifact_bytes: artifact.bytes,
                artifact_sha256: artifact.sha256.clone(),
                proof_image_digest: policy.image_digest.clone(),
                os_release: policy.os_release.clone(),
                package_manager_version: policy.package_manager_version.clone(),
                install_command: policy.install_command.clone(),
                install_exit_status: 0,
                version_command: policy.version_command.clone(),
                version_exit_status: 0,
                executable_path: policy.executable_path.clone(),
                executable_mode: policy.executable_mode,
                executable_sha256: member.sha256.clone(),
                version_output: format!("solstone-linux {}", rail_ledger.version),
                result: "pass".into(),
                policy_checked_at: rail_ledger.policy.checked_at.clone(),
                validation_time: proof_time.into(),
            },
        )?;
        proofs.insert(name, bytes);
    }
    if existing_staging {
        validate_staged_candidate(root, &staging, &manifest)?;
        return Ok(CandidateSnapshot {
            staging,
            manifest,
            proofs,
        });
    }
    let temporary = staging.with_extension(format!("building-{}", std::process::id()));
    fs::create_dir_all(&temporary).map_err(display_error)?;
    for artifact in &manifest.artifacts {
        fs::copy(
            release_dir.join(&artifact.path),
            temporary.join(&artifact.path),
        )
        .map_err(display_error)?;
    }
    fs::copy(
        release_dir.join(CHECKSUM_NAME),
        temporary.join(CHECKSUM_NAME),
    )
    .map_err(display_error)?;
    fs::copy(
        &manifest_path,
        temporary.join(manifest_name(&manifest.version)),
    )
    .map_err(display_error)?;
    validate_staged_candidate(root, &temporary, &manifest)?;
    for (name, bytes) in &proofs {
        fs::write(temporary.join(name), bytes).map_err(display_error)?;
    }
    fs::create_dir_all(staging.parent().unwrap()).map_err(display_error)?;
    fs::rename(&temporary, &staging).map_err(display_error)?;
    Ok(CandidateSnapshot {
        staging,
        manifest,
        proofs,
    })
}

fn validate_staged_candidate(root: &RepoRoot, staging: &Path, manifest: &Manifest) -> Result<()> {
    let manifest_file = manifest_name(&manifest.version);
    let staged_manifest_bytes = fs::read(staging.join(&manifest_file)).map_err(|error| {
        transparency_error(
            "terminal",
            "staged companion manifest",
            &manifest_file,
            error,
            "discard the staging directory and retry make publish-transparency",
        )
    })?;
    let staged_manifest = validate_manifest_bytes(root, &staged_manifest_bytes)?;
    if staged_manifest.source_commit != manifest.source_commit
        || staged_manifest.version != manifest.version
        || staged_manifest.artifacts != manifest.artifacts
    {
        return Err(transparency_error(
            "terminal",
            "staged companion manifest binding",
            format!(
                "version {} commit {}",
                manifest.version, manifest.source_commit
            ),
            format!(
                "version {} commit {}",
                staged_manifest.version, staged_manifest.source_commit
            ),
            "discard the staging directory and retry make publish-transparency",
        ));
    }
    for artifact in &manifest.artifacts {
        let bytes = fs::read(staging.join(&artifact.path)).map_err(|error| {
            transparency_error(
                "terminal",
                "staged candidate file",
                &artifact.path,
                error,
                "discard the staging directory and retry make publish-transparency",
            )
        })?;
        if bytes.len() as u64 != artifact.bytes || digest(&bytes) != artifact.sha256 {
            return Err(transparency_error(
                "terminal",
                "staged candidate artifact",
                format!("{} bytes sha256 {}", artifact.bytes, artifact.sha256),
                format!("{} bytes sha256 {}", bytes.len(), digest(&bytes)),
                "discard the staging directory and retry make publish-transparency",
            ));
        }
    }
    let checksums = fs::read_to_string(staging.join(CHECKSUM_NAME)).map_err(display_error)?;
    let expected_checksums = render_sha256sums(&manifest.artifacts)?;
    if checksums != expected_checksums {
        return Err(transparency_error(
            "terminal",
            "staged SHA256SUMS bytes",
            digest(expected_checksums.as_bytes()),
            digest(checksums.as_bytes()),
            "discard the staging directory and retry make publish-transparency",
        ));
    }
    Ok(())
}

pub(crate) fn build_entry(
    staging: &Path,
    manifest: &Manifest,
    proofs: &BTreeMap<String, Vec<u8>>,
    chain: &VerifiedChain,
) -> Result<(TransparencyEntry, Vec<u8>)> {
    let mut artifacts = manifest
        .artifacts
        .iter()
        .map(|artifact| TransparencyArtifact {
            bytes: artifact.bytes,
            name: artifact.path.clone(),
            sha256: artifact.sha256.clone(),
        })
        .collect::<Vec<_>>();
    let checksum = fs::read(staging.join(CHECKSUM_NAME)).map_err(display_error)?;
    artifacts.push(TransparencyArtifact {
        bytes: checksum.len() as u64,
        name: CHECKSUM_NAME.into(),
        sha256: digest(&checksum),
    });
    artifacts.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let manifest_bytes =
        fs::read(staging.join(manifest_name(&manifest.version))).map_err(display_error)?;
    let mut proof_inventory = proofs
        .iter()
        .map(|(name, bytes)| TransparencyNamedDigest {
            name: name.clone(),
            sha256: digest(bytes),
        })
        .collect::<Vec<_>>();
    proof_inventory.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let entry = TransparencyEntry {
        artifacts,
        manifests: vec![TransparencyNamedDigest {
            name: manifest_name(&manifest.version),
            sha256: digest(&manifest_bytes),
        }],
        prev_sha256: chain.tip.as_ref().map_or_else(
            || ZERO_SHA256.into(),
            |tip| {
                digest(&transparency_canonical_json(&serde_json::to_value(tip).unwrap()).unwrap())
            },
        ),
        prev_version: chain
            .tip
            .as_ref()
            .map_or_else(String::new, |tip| tip.version.clone()),
        product: PRODUCT.into(),
        proofs: proof_inventory,
        published_utc: now,
        schema: TRANSPARENCY_ENTRY_SCHEMA.into(),
        seq: chain
            .pointer
            .as_ref()
            .map_or(1, |pointer| pointer.chain_length + 1),
        source_commit: manifest.source_commit.clone(),
        version: manifest.version.clone(),
    };
    validate_entry(&entry, chain.tip.as_ref())?;
    let bytes = transparency_canonical_json(&serde_json::to_value(&entry).map_err(display_error)?)?;
    Ok((entry, bytes))
}

pub(crate) fn build_pointer(
    entry: &TransparencyEntry,
    entry_bytes: &[u8],
) -> Result<(TransparencyPointer, Vec<u8>)> {
    let signed = exact_timestamp("transparency signed_at", &entry.published_utc)?;
    let pointer = TransparencyPointer {
        chain_length: entry.seq,
        product: PRODUCT.into(),
        schema: TRANSPARENCY_LATEST_SCHEMA.into(),
        signed_at: entry.published_utc.clone(),
        tip_sha256: digest(entry_bytes),
        valid_until: (signed + Duration::days(14)).to_rfc3339_opts(SecondsFormat::Secs, true),
        version: entry.version.clone(),
    };
    let bytes =
        transparency_canonical_json(&serde_json::to_value(&pointer).map_err(display_error)?)?;
    Ok((pointer, bytes))
}

pub(crate) fn renew_pointer(
    old: &TransparencyPointer,
    now: DateTime<Utc>,
) -> Result<(TransparencyPointer, Vec<u8>)> {
    let pointer = TransparencyPointer {
        chain_length: old.chain_length,
        product: PRODUCT.into(),
        schema: TRANSPARENCY_LATEST_SCHEMA.into(),
        signed_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        tip_sha256: old.tip_sha256.clone(),
        valid_until: (now + Duration::days(14)).to_rfc3339_opts(SecondsFormat::Secs, true),
        version: old.version.clone(),
    };
    let bytes =
        transparency_canonical_json(&serde_json::to_value(&pointer).map_err(display_error)?)?;
    Ok((pointer, bytes))
}

pub(crate) fn pointer_is_stale(pointer: &TransparencyPointer, now: DateTime<Utc>) -> Result<bool> {
    Ok(exact_timestamp(
        "staged transparency pointer valid_until",
        &pointer.valid_until,
    )? < now)
}

pub(crate) fn validate_staged_entry_candidate(
    entry: &TransparencyEntry,
    manifest: &Manifest,
    staging: &Path,
) -> Result<()> {
    if entry.version != manifest.version || entry.source_commit != manifest.source_commit {
        return Err(transparency_error(
            "terminal",
            "local transparency staging candidate",
            format!(
                "version {} commit {}",
                manifest.version, manifest.source_commit
            ),
            format!("version {} commit {}", entry.version, entry.source_commit),
            format!(
                "discard only {} and retry; a never-published local stage is not a poisoned remote version",
                staging.display()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn chain_includes_entry(chain: &VerifiedChain, entry_bytes: &[u8]) -> Result<bool> {
    match &chain.tip {
        Some(tip) => Ok(transparency_canonical_json(
            &serde_json::to_value(tip).map_err(display_error)?,
        )? == entry_bytes),
        None => Ok(false),
    }
}

pub(crate) fn immutable_objects(
    staging: &Path,
    entry: &[u8],
    signature: &[u8],
    manifest: &Manifest,
    proofs: &BTreeMap<String, Vec<u8>>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut objects = BTreeMap::new();
    objects.insert("ledger-entry.json".into(), entry.to_vec());
    objects.insert("ledger-entry.json.minisig".into(), signature.to_vec());
    let name = manifest_name(&manifest.version);
    objects.insert(
        name.clone(),
        fs::read(staging.join(name)).map_err(display_error)?,
    );
    objects.extend(proofs.clone());
    Ok(objects)
}

pub(crate) fn staging_manifest_v1(staging: &Path) -> Result<(Vec<u8>, String)> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
        for entry in fs::read_dir(directory).map_err(display_error)? {
            let entry = entry.map_err(display_error)?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(display_error)?;
            let relative = relative.to_str().ok_or_else(|| {
                transparency_error(
                    "terminal",
                    "staging-manifest v1 path",
                    "ASCII path without control characters",
                    "non-UTF-8 path",
                    "discard the staging directory and retry make publish-transparency",
                )
            })?;
            if !relative.is_ascii()
                || relative
                    .as_bytes()
                    .iter()
                    .any(|byte| byte.is_ascii_control())
            {
                return Err(transparency_error(
                    "terminal",
                    "staging-manifest v1 path",
                    "ASCII path without control characters",
                    relative.escape_default(),
                    "discard the staging directory and retry make publish-transparency",
                ));
            }
            let file_type = entry.file_type().map_err(display_error)?;
            if file_type.is_symlink() {
                return Err(transparency_error(
                    "terminal",
                    "staging-manifest v1 file type",
                    "regular file or directory",
                    format!("symlink at {relative}"),
                    "discard the staging directory and retry make publish-transparency",
                ));
            }
            if file_type.is_dir() {
                visit(root, &path, files)?;
            } else if file_type.is_file() {
                files.push((relative.replace(std::path::MAIN_SEPARATOR, "/"), path));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(staging, staging, &mut files)?;
    files.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut rendered = Vec::new();
    for (path, file_path) in files {
        let mut file = fs::File::open(&file_path).map_err(|error| {
            transparency_error(
                "terminal",
                "staging-manifest v1 file",
                "readable regular file",
                error,
                "discard the staging directory and retry make publish-transparency",
            )
        })?;
        let bytes = file.metadata().map_err(display_error)?.len();
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(display_error)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        writeln!(
            rendered,
            "sha256={}\tbytes={}\tpath={path}",
            format_args!("{:x}", hasher.finalize()),
            bytes
        )
        .map_err(display_error)?;
    }
    let receipt = digest(&rendered);
    Ok((rendered, receipt))
}

pub(crate) fn remove_obsolete_staging_manifest(staging: &Path) -> Result<()> {
    let obsolete_manifest = staging.join("staging-manifest.json");
    match fs::symlink_metadata(&obsolete_manifest) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(transparency_error(
            "terminal",
            "staging-manifest v1 file type",
            "regular file or directory",
            "symlink at staging-manifest.json",
            "discard the staging directory and retry make publish-transparency",
        )),
        Ok(metadata) if metadata.is_file() => {
            fs::remove_file(obsolete_manifest).map_err(display_error)
        }
        Ok(_) | Err(_) => Ok(()),
    }
}

pub(crate) struct StagedPublication<'a> {
    pub(crate) staging: &'a Path,
    pub(crate) chain: &'a VerifiedChain,
    pub(crate) entry: &'a TransparencyEntry,
    pub(crate) entry_bytes: &'a [u8],
    pub(crate) entry_signature: &'a [u8],
    pub(crate) pointer_bytes: &'a [u8],
    pub(crate) pointer_signature: &'a [u8],
    pub(crate) manifest: &'a Manifest,
    pub(crate) proofs: &'a BTreeMap<String, Vec<u8>>,
}

fn verify_adoptable_remote_entry<V: TransparencySignatureVerifier>(
    config: &TransparencyConfig,
    verifier: &mut V,
    verification_root: &Path,
    expected: &TransparencyEntry,
    expected_bytes: &[u8],
    remote_bytes: &[u8],
    remote_signature: &[u8],
) -> Result<String> {
    let recorded: TransparencyEntry =
        parse_json(remote_bytes, "remote recorded transparency entry")?;
    let recorded_identity = digest(remote_bytes);
    let semantic_match = recorded.product == PRODUCT
        && recorded.version == expected.version
        && recorded.seq == expected.seq
        && recorded.prev_sha256 == expected.prev_sha256
        && recorded.prev_version == expected.prev_version
        && recorded.source_commit == expected.source_commit
        && recorded.artifacts == expected.artifacts
        && recorded.manifests == expected.manifests
        && recorded.proofs == expected.proofs;
    let signature_valid = verifier
        .verify(
            verification_root,
            &config.minisign_pub,
            remote_bytes,
            remote_signature,
            "remote recorded transparency entry signature",
        )
        .is_ok()
        && verify_trusted_comment(
            remote_signature,
            &entry_trusted_comment(&recorded, &recorded_identity),
            "remote recorded transparency entry trusted comment",
        )
        .is_ok();
    if !semantic_match || !signature_valid {
        return Err(transparency_error(
            "terminal",
            "remote poisoned version",
            format!(
                "candidate commit {} seq {} entry {}",
                expected.source_commit,
                expected.seq,
                digest(expected_bytes)
            ),
            format!(
                "recorded commit {} seq {} entry {}",
                recorded.source_commit, recorded.seq, recorded_identity
            ),
            "cut the next version because this version key is permanently recorded",
        ));
    }
    Ok(recorded_identity)
}

fn persist_adopted_entry(staging: &Path, entry_bytes: &[u8], signature_bytes: &[u8]) -> Result<()> {
    fs::write(staging.join("ledger-entry.json"), entry_bytes).map_err(display_error)?;
    fs::write(staging.join("ledger-entry.json.minisig"), signature_bytes).map_err(display_error)?;
    for name in ["latest.json", "latest.json.minisig"] {
        match fs::remove_file(staging.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(display_error(error)),
        }
    }
    Ok(())
}

pub(crate) fn upload_publication<
    T: TransparencyTransport,
    A: ArchiveChannel,
    V: TransparencySignatureVerifier,
>(
    config: &TransparencyConfig,
    transport: &mut T,
    archive: &mut A,
    verifier: &mut V,
    verification_root: &Path,
    publication: &StagedPublication<'_>,
) -> Result<String> {
    let StagedPublication {
        staging,
        chain,
        entry,
        entry_bytes,
        entry_signature,
        pointer_bytes,
        pointer_signature,
        manifest,
        proofs,
    } = publication;
    let objects = immutable_objects(staging, entry_bytes, entry_signature, manifest, proofs)?;
    let prefix = format!("releases/{PRODUCT}/v/{}", entry.version);
    let mut adopted = BTreeSet::new();
    let remote_entry = transport.get(&s3(config, &format!("{prefix}/ledger-entry.json")), false)?;
    let remote_signature = transport.get(
        &s3(config, &format!("{prefix}/ledger-entry.json.minisig")),
        false,
    )?;
    match (remote_entry.http_status, remote_signature.http_status) {
        (404, 404) => {}
        (200, 200)
            if remote_entry.body == *entry_bytes && remote_signature.body == *entry_signature =>
        {
            adopted.insert("ledger-entry.json".into());
            adopted.insert("ledger-entry.json.minisig".into());
        }
        (200, 200) => {
            let recorded_identity = verify_adoptable_remote_entry(
                config,
                verifier,
                verification_root,
                entry,
                entry_bytes,
                &remote_entry.body,
                &remote_signature.body,
            )?;
            persist_adopted_entry(staging, &remote_entry.body, &remote_signature.body)?;
            return Err(transparency_error(
                "retryable",
                "transparency immutable adoption",
                "staged entry and signature",
                format!("adopted signed remote entry {recorded_identity}"),
                "retry make publish-transparency so the pointer pair is signed for the adopted entry before archive and mutable writes",
            ));
        }
        _ => {
            return Err(transparency_error(
                "terminal",
                "remote immutable entry pair",
                "both entry and signature absent or present",
                format!(
                    "entry HTTP {} signature HTTP {}",
                    remote_entry.http_status, remote_signature.http_status
                ),
                "cut the next version because the remote version key contains an incomplete entry pair",
            ));
        }
    }
    for (name, bytes) in &objects {
        if matches!(
            name.as_str(),
            "ledger-entry.json" | "ledger-entry.json.minisig"
        ) {
            continue;
        }
        let destination = s3(config, &format!("{prefix}/{name}"));
        let remote = transport.get(&destination, false)?;
        match remote.http_status {
            404 => {}
            200 if remote.body == *bytes => {
                adopted.insert(name.clone());
            }
            200 => {
                return Err(transparency_error(
                    "terminal",
                    "remote poisoned version object",
                    digest(bytes),
                    digest(&remote.body),
                    "cut the next version because the remote version key is permanently recorded with different bytes",
                ));
            }
            _ => ensure_http(&remote, &[200, 404], "transparency immutable preflight GET")?,
        }
    }
    let transparency_ledger = [chain.transparency_ledger.as_slice(), entry_bytes].concat();
    fs::write(staging.join("ledger.jsonl"), transparency_ledger).map_err(display_error)?;
    remove_obsolete_staging_manifest(staging)?;
    let (_, archive_digest) = staging_manifest_v1(staging)?;
    archive.archive(staging, &archive_digest)?;
    // Immutable mutation completes before any public-plane verification begins.
    // This phase boundary makes mutable writes unreachable after a verification failure.
    for (name, bytes) in &objects {
        if adopted.contains(name) {
            continue;
        }
        let destination = s3(config, &format!("{prefix}/{name}"));
        let response = transport.put_create_only(&destination, bytes, IMMUTABLE_CACHE)?;
        match response.http_status {
            200 | 201 | 204 => {}
            412 => {
                if matches!(
                    name.as_str(),
                    "ledger-entry.json" | "ledger-entry.json.minisig"
                ) {
                    let remote_entry = transport
                        .get(&s3(config, &format!("{prefix}/ledger-entry.json")), false)?;
                    let remote_signature = transport.get(
                        &s3(config, &format!("{prefix}/ledger-entry.json.minisig")),
                        false,
                    )?;
                    ensure_http(&remote_entry, &[200], "raced transparency entry GET")?;
                    ensure_http(
                        &remote_signature,
                        &[200],
                        "raced transparency entry signature GET",
                    )?;
                    let identity = verify_adoptable_remote_entry(
                        config,
                        verifier,
                        verification_root,
                        entry,
                        entry_bytes,
                        &remote_entry.body,
                        &remote_signature.body,
                    )?;
                    persist_adopted_entry(staging, &remote_entry.body, &remote_signature.body)?;
                    return Err(transparency_error(
                        "retryable",
                        "transparency immutable adoption",
                        "preflight adoption before archive",
                        format!("PUT-time race adopted signed remote entry {identity}"),
                        "retry make publish-transparency so preflight archives adopted bytes before mutable writes",
                    ));
                }
                let remote = transport.get(&destination, false)?;
                ensure_http(&remote, &[200], "transparency immutable adoption GET")?;
                if remote.body != *bytes {
                    return Err(transparency_error(
                        "terminal",
                        "transparency immutable conflict",
                        digest(bytes),
                        digest(&remote.body),
                        "cut the next version because immutable bytes differ",
                    ));
                }
                fs::write(staging.join(name), &remote.body).map_err(display_error)?;
                return Err(transparency_error(
                    "retryable",
                    "transparency immutable adoption",
                    "preflight adoption before archive",
                    "PUT-time race with byte-identical remote object",
                    "retry make publish-transparency so preflight can adopt before archive and mutable writes",
                ));
            }
            _ => ensure_http(&response, &[200, 201, 204], "transparency immutable PUT")?,
        }
    }
    for (name, bytes) in &objects {
        let remote = transport.get(&public(config, &format!("{prefix}/{name}")), false)?;
        ensure_http(
            &remote,
            &[200],
            "transparency immutable public verification",
        )?;
        if digest(&remote.body) != digest(bytes) {
            return Err(transparency_error(
                "retryable",
                "transparency immutable public digest",
                digest(bytes),
                digest(&remote.body),
                "retry after the public surface returns the uploaded bytes",
            ));
        }
    }
    let current = transport.get(
        &s3(config, &format!("releases/{PRODUCT}/latest.json")),
        true,
    )?;
    if !chain
        .pointer_bytes
        .as_deref()
        .map_or(current.http_status == 404, |old| {
            current.http_status == 200 && current.body == old
        })
    {
        return Err(transparency_error(
            "retryable",
            "pre-pointer transparency chain state",
            "unchanged pointer",
            format!("HTTP {} changed bytes", current.http_status),
            "restart publication against the new chain head",
        ));
    }
    let ledger = [chain.transparency_ledger.as_slice(), entry_bytes].concat();
    let ledger_destination = s3(config, &format!("releases/{PRODUCT}/ledger.jsonl"));
    ensure_http(
        &transport.put_mutable(&ledger_destination, &ledger, MUTABLE_CACHE)?,
        &[200, 201, 204],
        "transparency ledger PUT",
    )?;
    let ledger_remote = transport.get(&ledger_destination, true)?;
    ensure_http(
        &ledger_remote,
        &[200],
        "transparency ledger verification GET",
    )?;
    if ledger_remote.body != ledger {
        return Err(transparency_error(
            "retryable",
            "transparency ledger bytes",
            digest(&ledger),
            digest(&ledger_remote.body),
            "retry after restoring the transparency S3 plane",
        ));
    }
    let signature_destination = s3(config, &format!("releases/{PRODUCT}/latest.json.minisig"));
    ensure_http(
        &transport.put_mutable(&signature_destination, pointer_signature, MUTABLE_CACHE)?,
        &[200, 201, 204],
        "transparency pointer signature PUT",
    )?;
    let signature_remote = transport.get(&signature_destination, true)?;
    ensure_http(
        &signature_remote,
        &[200],
        "transparency pointer signature verification GET",
    )?;
    if signature_remote.body != *pointer_signature {
        return Err(transparency_error(
            "retryable",
            "transparency pointer signature bytes",
            digest(pointer_signature),
            digest(&signature_remote.body),
            "retry after restoring the transparency S3 plane",
        ));
    }
    verifier.verify(
        verification_root,
        &config.minisign_pub,
        pointer_bytes,
        &signature_remote.body,
        "uploaded transparency pointer signature",
    )?;
    verify_trusted_comment(
        &signature_remote.body,
        &pointer_trusted_comment(&parse_json(pointer_bytes, "staged transparency pointer")?),
        "uploaded transparency pointer trusted comment",
    )?;
    let pointer_destination = s3(config, &format!("releases/{PRODUCT}/latest.json"));
    let pointer_put = match chain.pointer_etag.as_deref() {
        Some(etag) => {
            transport.put_conditional(&pointer_destination, pointer_bytes, etag, MUTABLE_CACHE)?
        }
        None => transport.put_create_only(&pointer_destination, pointer_bytes, MUTABLE_CACHE)?,
    };
    ensure_http(&pointer_put, &[200, 201, 204], "transparency pointer PUT")?;
    let pointer_remote = transport.get(&pointer_destination, true)?;
    ensure_http(
        &pointer_remote,
        &[200],
        "transparency pointer verification GET",
    )?;
    if pointer_remote.body != *pointer_bytes {
        return Err(transparency_error(
            "retryable",
            "transparency pointer bytes",
            digest(pointer_bytes),
            digest(&pointer_remote.body),
            "retry after restoring the transparency S3 plane",
        ));
    }
    let final_signature = transport.get(&signature_destination, true)?;
    ensure_http(
        &final_signature,
        &[200],
        "published transparency pointer signature final GET",
    )?;
    if final_signature.body != *pointer_signature {
        return Err(transparency_error(
            "retryable",
            "published transparency pointer signature bytes",
            digest(pointer_signature),
            digest(&final_signature.body),
            "retry after restoring the transparency S3 plane",
        ));
    }
    verifier.verify(
        verification_root,
        &config.minisign_pub,
        &pointer_remote.body,
        &final_signature.body,
        "published transparency pointer signature",
    )?;
    verify_trusted_comment(
        &final_signature.body,
        &pointer_trusted_comment(&parse_json(
            &pointer_remote.body,
            "published transparency pointer",
        )?),
        "published transparency pointer trusted comment",
    )?;
    Ok(archive_digest)
}

pub fn publish_transparency(release_dir: &Path) -> Result<()> {
    let started = Instant::now();
    let root = RepoRoot::resolve()?;
    validate_previous_head_committed(root.path())?;
    validate_tools()?;
    let config = TransparencyConfig::from_env(true)?;
    let staging_root = root.path().join(".transparency-staging");
    fs::create_dir_all(&staging_root).map_err(display_error)?;
    let mut transport = CurlTransport::new(&config, &staging_root);
    let chain = fetch_verified_chain(
        root.path(),
        &config,
        &mut transport,
        &mut MinisignVerifier,
        true,
    )?;
    let snapshot = snapshot_candidate(&root, release_dir)?;
    let staging = &snapshot.staging;
    let manifest = &snapshot.manifest;
    let proofs = &snapshot.proofs;
    let entry_path = staging.join("ledger-entry.json");
    let entry_signature_path = staging.join("ledger-entry.json.minisig");
    let pointer_path = staging.join("latest.json");
    let pointer_signature_path = staging.join("latest.json.minisig");
    let entry_complete = entry_path.is_file() && entry_signature_path.is_file();
    let pointer_complete = pointer_path.is_file() && pointer_signature_path.is_file();
    let (entry, entry_bytes, already_published, sign_entry) = if entry_complete {
        let entry_bytes = fs::read(&entry_path).map_err(display_error)?;
        let entry: TransparencyEntry = parse_json(&entry_bytes, "staged transparency entry")?;
        let already_published = chain_includes_entry(&chain, &entry_bytes)?;
        if !already_published {
            validate_entry(&entry, chain.tip.as_ref())?;
        }
        validate_staged_entry_candidate(&entry, manifest, staging)?;
        (entry, entry_bytes, already_published, false)
    } else {
        let (entry, entry_bytes) = build_entry(staging, manifest, proofs, &chain)?;
        fs::write(&entry_path, &entry_bytes).map_err(display_error)?;
        (entry, entry_bytes, false, true)
    };
    let (pointer, pointer_bytes, sign_pointer) = if pointer_complete {
        let pointer_bytes = fs::read(&pointer_path).map_err(display_error)?;
        let pointer: TransparencyPointer =
            parse_json(&pointer_bytes, "staged transparency pointer")?;
        validate_pointer(&pointer, &entry)?;
        (pointer, pointer_bytes, false)
    } else {
        let (pointer, pointer_bytes) = build_pointer(&entry, &entry_bytes)?;
        fs::write(&pointer_path, &pointer_bytes).map_err(display_error)?;
        (pointer, pointer_bytes, true)
    };
    if sign_entry || sign_pointer {
        let mut passphrase = read_passphrase_once()?;
        let signing = (|| {
            if sign_entry {
                sign_file(
                    &config.minisign_key,
                    &entry_path,
                    &entry_signature_path,
                    &entry_trusted_comment(&entry, &digest(&entry_bytes)),
                    &passphrase,
                )?;
            }
            if sign_pointer {
                sign_file(
                    &config.minisign_key,
                    &pointer_path,
                    &pointer_signature_path,
                    &pointer_trusted_comment(&pointer),
                    &passphrase,
                )?;
            }
            Ok(())
        })();
        passphrase.fill(0);
        signing?;
    }
    let entry_signature = fs::read(&entry_signature_path).map_err(display_error)?;
    let pointer_signature = fs::read(&pointer_signature_path).map_err(display_error)?;
    verify_trusted_comment(
        &entry_signature,
        &entry_trusted_comment(&entry, &digest(&entry_bytes)),
        "new transparency entry trusted comment",
    )?;
    verify_trusted_comment(
        &pointer_signature,
        &pointer_trusted_comment(&pointer),
        "new transparency pointer trusted comment",
    )?;
    verify_minisign_bytes(
        staging,
        &config.minisign_pub,
        &entry_bytes,
        &entry_signature,
        "new transparency entry signature",
    )?;
    if already_published {
        println!(
            "already published, chain unchanged: product={PRODUCT} version={} seq={} entry_sha256={}",
            entry.version,
            entry.seq,
            digest(&entry_bytes)
        );
        return Ok(());
    }
    verify_minisign_bytes(
        staging,
        &config.minisign_pub,
        &pointer_bytes,
        &pointer_signature,
        "new transparency pointer signature",
    )?;
    let mut archive = CommandArchive {
        command: config.archive_channel.clone().unwrap(),
    };
    let archive_digest = upload_publication(
        &config,
        &mut transport,
        &mut archive,
        &mut MinisignVerifier,
        staging,
        &StagedPublication {
            staging,
            chain: &chain,
            entry: &entry,
            entry_bytes: &entry_bytes,
            entry_signature: &entry_signature,
            pointer_bytes: &pointer_bytes,
            pointer_signature: &pointer_signature,
            manifest,
            proofs,
        },
    )?;
    let witness = append_head_row(
        root.path(),
        &TransparencyHeadRow {
            entry_sha256: digest(&entry_bytes),
            product: PRODUCT.into(),
            published_utc: entry.published_utc.clone(),
            seq: entry.seq,
            version: entry.version.clone(),
        },
    )?;
    let stale_pointer = pointer_is_stale(&pointer, Utc::now())?;
    println!(
        "product: {PRODUCT}\nversion: {}\nseq: {}\nentry_sha256: {}\npublic_entry: {}/releases/{PRODUCT}/v/{}/ledger-entry.json\npublic_entry_signature: {}/releases/{PRODUCT}/v/{}/ledger-entry.json.minisig\npublic_transparency_ledger: {}/releases/{PRODUCT}/ledger.jsonl\npublic_pointer: {}/releases/{PRODUCT}/latest.json\npublic_pointer_signature: {}/releases/{PRODUCT}/latest.json.minisig\narchive: {}\nwitness: {}\nwall_clock_ms: {}",
        entry.version,
        entry.seq,
        digest(&entry_bytes),
        config.base_url,
        entry.version,
        config.base_url,
        entry.version,
        config.base_url,
        config.base_url,
        config.base_url,
        archive_digest,
        witness,
        started.elapsed().as_millis()
    );
    if stale_pointer {
        println!("pointer renewal: make resign-transparency-pointer");
    }
    Ok(())
}

pub fn resign_transparency_pointer() -> Result<()> {
    let root = RepoRoot::resolve()?;
    validate_tools()?;
    let config = TransparencyConfig::from_env(false)?;
    let temporary = root.path().join(".transparency-staging").join("resign");
    fs::create_dir_all(&temporary).map_err(display_error)?;
    let mut transport = CurlTransport::new(&config, &temporary);
    // Freeze defense must never re-attest a rolled-back, foreign, or invalid chain.
    let chain = fetch_verified_chain(
        root.path(),
        &config,
        &mut transport,
        &mut MinisignVerifier,
        false,
    )?;
    let old = chain.pointer.ok_or_else(|| {
        transparency_error(
            "terminal",
            "transparency pointer",
            "verified existing pointer",
            "missing",
            "publish genesis before resigning its pointer",
        )
    })?;
    let (pointer, bytes) = renew_pointer(&old, Utc::now())?;
    let message = temporary.join("latest.json");
    let signature = temporary.join("latest.json.minisig");
    fs::write(&message, &bytes).map_err(display_error)?;
    let mut passphrase = read_passphrase_once()?;
    let signing = sign_file(
        &config.minisign_key,
        &message,
        &signature,
        &pointer_trusted_comment(&pointer),
        &passphrase,
    );
    passphrase.fill(0);
    signing?;
    let signature_bytes = fs::read(&signature).map_err(display_error)?;
    verify_minisign_bytes(
        &temporary,
        &config.minisign_pub,
        &bytes,
        &signature_bytes,
        "resigned transparency pointer signature",
    )?;
    let prefix = format!("releases/{PRODUCT}");
    let current = transport.get(&s3(&config, &format!("{prefix}/latest.json")), true)?;
    if current.body != chain.pointer_bytes.unwrap_or_default() {
        return Err(transparency_error(
            "retryable",
            "pre-pointer transparency chain state",
            "unchanged verified pointer",
            "changed pointer",
            "restart resign-transparency-pointer",
        ));
    }
    let signature_destination = s3(&config, &format!("{prefix}/latest.json.minisig"));
    ensure_http(
        &transport.put_mutable(&signature_destination, &signature_bytes, MUTABLE_CACHE)?,
        &[200, 201, 204],
        "resigned transparency signature PUT",
    )?;
    let uploaded_signature = transport.get(&signature_destination, true)?;
    ensure_http(
        &uploaded_signature,
        &[200],
        "resigned transparency signature verification GET",
    )?;
    if uploaded_signature.body != signature_bytes {
        return Err(transparency_error(
            "retryable",
            "resigned transparency signature bytes",
            digest(&signature_bytes),
            digest(&uploaded_signature.body),
            "retry make resign-transparency-pointer after restoring the transparency S3 plane",
        ));
    }
    verify_minisign_bytes(
        &temporary,
        &config.minisign_pub,
        &bytes,
        &uploaded_signature.body,
        "uploaded resigned transparency pointer signature",
    )?;
    let pointer_destination = s3(&config, &format!("{prefix}/latest.json"));
    let pointer_put = match chain.pointer_etag.as_deref() {
        Some(etag) => {
            transport.put_conditional(&pointer_destination, &bytes, etag, MUTABLE_CACHE)?
        }
        None => transport.put_create_only(&pointer_destination, &bytes, MUTABLE_CACHE)?,
    };
    ensure_http(
        &pointer_put,
        &[200, 201, 204],
        "resigned transparency pointer PUT",
    )?;
    let remote_signature = transport.get(&signature_destination, true)?;
    ensure_http(
        &remote_signature,
        &[200],
        "resigned transparency signature verification GET",
    )?;
    if remote_signature.body != signature_bytes {
        return Err(transparency_error(
            "retryable",
            "resigned transparency signature bytes",
            digest(&signature_bytes),
            digest(&remote_signature.body),
            "retry make resign-transparency-pointer after restoring the transparency S3 plane",
        ));
    }
    let remote_pointer = transport.get(&pointer_destination, true)?;
    ensure_http(
        &remote_pointer,
        &[200],
        "resigned transparency pointer verification GET",
    )?;
    if remote_pointer.body != bytes {
        return Err(transparency_error(
            "retryable",
            "resigned transparency pointer bytes",
            digest(&bytes),
            digest(&remote_pointer.body),
            "retry make resign-transparency-pointer after restoring the transparency S3 plane",
        ));
    }
    verify_minisign_bytes(
        &temporary,
        &config.minisign_pub,
        &remote_pointer.body,
        &remote_signature.body,
        "remote resigned transparency pointer signature",
    )?;
    println!(
        "product: {PRODUCT}\nchain_length: {}\ntip_sha256: {}\nvalid_until: {}",
        pointer.chain_length, pointer.tip_sha256, pointer.valid_until
    );
    Ok(())
}
