// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config,
    observer::Clock,
    private_link::{LinkOutcome, MAX_REQUEST_BODY_BYTES, PrivateLinkCapability},
    sync_health::ErrorType,
};
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
const OBSERVER_PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";
const DEFAULT_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const MAX_IMMEDIATE_ATTEMPTS: usize = 2;
pub(crate) const MAX_MULTIPART_PART_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadResult {
    pub success: bool,
    pub duplicate: bool,
    pub error_type: Option<ErrorType>,
    /// The HTTP status of the response that produced this result; `None` when there was no response.
    pub status_code: Option<u16>,
    pub stored_key: Option<String>,
}

impl UploadResult {
    fn failure(error_type: Option<ErrorType>, status_code: Option<u16>) -> Self {
        Self {
            success: false,
            duplicate: false,
            error_type,
            status_code,
            stored_key: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ListingFile {
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub submitted_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub sha256: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ListingEntry {
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub key: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub original_key: Option<String>,
    #[serde(default, deserialize_with = "deserialize_lenient_option")]
    pub files: Option<Vec<ListingFile>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayCustody {
    pub day_present: bool,
    pub items: Vec<ListingEntry>,
    pub proof_available: bool,
    pub error_type: Option<ErrorType>,
    pub status_code: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestProbe {
    pub error_type: Option<ErrorType>,
    pub status_code: Option<u16>,
}

pub(crate) struct Inner {
    capability: std::sync::RwLock<Option<PrivateLinkCapability>>,
    fallback_link_facts: crate::private_link::LinkFacts,
    #[cfg(test)]
    expose_link_facts: AtomicBool,
    revoked: AtomicBool,
    cancellation: CancellationToken,
    retry_delays: Vec<i64>,
    immediate_attempts: usize,
}

pub struct UploadClient {
    inner: Arc<Inner>,
}

impl UploadClient {
    /// Create the linked upload client.
    pub(crate) fn new(
        config: &Config,
        capability: impl Into<Option<PrivateLinkCapability>>,
        _clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        let retry_delays = if config.sync_retry_delays.is_empty() {
            DEFAULT_RETRY_DELAYS.to_vec()
        } else {
            config.sync_retry_delays.clone()
        };
        let inner = Arc::new(Inner {
            capability: std::sync::RwLock::new(capability.into()),
            fallback_link_facts: crate::private_link::LinkFacts::default(),
            #[cfg(test)]
            expose_link_facts: AtomicBool::new(true),
            revoked: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
            retry_delays,
            immediate_attempts: config
                .sync_max_retries
                .clamp(1, MAX_IMMEDIATE_ATTEMPTS as i64) as usize,
        });
        Self { inner }
    }

    pub fn is_revoked(&self) -> bool {
        self.inner.revoked.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn revoke_for_test(&self) {
        self.inner.revoked.store(true, Ordering::Release);
    }

    pub(crate) fn has_capability(&self) -> bool {
        self.inner.capability().is_some()
    }

    pub(crate) fn link_fact_state(&self) -> Option<crate::private_link::LinkFactState> {
        #[cfg(test)]
        if !self.inner.expose_link_facts.load(Ordering::Acquire) {
            return None;
        }
        Some(
            self.inner
                .capability()
                .map_or_else(
                    || self.inner.fallback_link_facts.clone(),
                    |capability| capability.facts(),
                )
                .snapshot(),
        )
    }

    pub(crate) fn link_facts(&self) -> crate::private_link::LinkFacts {
        self.inner.capability().map_or_else(
            || self.inner.fallback_link_facts.clone(),
            |capability| capability.facts(),
        )
    }

    pub(crate) fn publish_link_fact(&self, fact: crate::private_link::LinkFact) {
        self.inner.publish_link_fact(fact);
    }

    pub(crate) fn begin_owner_generation(&self) {
        self.link_facts().begin_owner_generation();
    }

    pub fn request_stop(&self) {
        self.inner.cancellation.cancel();
    }

    pub(crate) fn install_capability(&self, capability: PrivateLinkCapability) {
        *self
            .inner
            .capability
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(capability);
    }

    pub async fn upload_segment(
        &self,
        day: &str,
        segment: &str,
        files: &[PathBuf],
    ) -> UploadResult {
        if self.is_revoked() {
            return UploadResult::failure(Some(ErrorType::Auth), None);
        }
        let mut last_error = None;
        let mut last_status = None;
        for attempt in 0..self.inner.immediate_attempts {
            let (form, framed_length) = match build_multipart_form(day, segment, files).await {
                Ok(form) => form,
                Err(MultipartBuildError::NoFiles) => {
                    return UploadResult::failure(Some(ErrorType::Client), None);
                }
                Err(MultipartBuildError::File { path, error }) => {
                    tracing::warn!(path = %path.display(), %error, "Unable to prepare upload file");
                    return UploadResult::failure(Some(ErrorType::Client), None);
                }
                Err(MultipartBuildError::PartTooLarge | MultipartBuildError::RequestTooLarge) => {
                    return UploadResult::failure(Some(ErrorType::Client), Some(413));
                }
            };
            debug_assert!(framed_length <= MAX_REQUEST_BODY_BYTES);

            if let Some(capability) = self.inner.capability() {
                match capability.ingest(form).await {
                    LinkOutcome::Success { status, body, .. } if status == StatusCode::OK => {
                        match serde_json::from_slice::<Value>(&body) {
                            Ok(body) => {
                                return parse_upload_body(body);
                            }
                            Err(error) => {
                                tracing::warn!(
                                    attempt = attempt + 1,
                                    %error,
                                    "Upload attempt returned malformed JSON"
                                );
                                last_error = Some(ErrorType::Transient);
                                last_status = Some(StatusCode::OK.as_u16());
                            }
                        }
                    }
                    LinkOutcome::Success { status, .. } | LinkOutcome::LocalRejected { status } => {
                        let error_type = Self::classify_error(Some(status.as_u16()), false);
                        last_error = Some(error_type);
                        last_status = Some(status.as_u16());
                        if error_type != ErrorType::Transient {
                            return UploadResult::failure(Some(error_type), Some(status.as_u16()));
                        }
                    }
                    LinkOutcome::Forbidden => {
                        self.inner.revoked.store(true, Ordering::Release);
                        self.inner
                            .publish_link_fact(crate::private_link::LinkFact::TerminalRevocation);
                        return UploadResult::failure(
                            Some(ErrorType::Auth),
                            Some(StatusCode::FORBIDDEN.as_u16()),
                        );
                    }
                    LinkOutcome::TransportUnavailable => {
                        last_error = Some(ErrorType::Transient);
                        last_status = None;
                    }
                }
            } else {
                return UploadResult::failure(Some(ErrorType::Transient), None);
            }
            if attempt + 1 < self.inner.immediate_attempts {
                tokio::select! {
                    () = tokio::time::sleep(retry_delay(&self.inner.retry_delays, attempt)) => {}
                    () = self.inner.cancellation.cancelled() => {
                        return UploadResult::failure(Some(ErrorType::Transient), None);
                    }
                }
            }
        }
        UploadResult::failure(last_error, last_status)
    }

    pub async fn probe_manifest(&self) -> ManifestProbe {
        if self.is_revoked() {
            return probe_failure(ErrorType::Auth, None);
        }
        let Some(capability) = self.inner.capability() else {
            return probe_failure(ErrorType::Transient, None);
        };
        match capability.probe_manifest().await {
            LinkOutcome::Success { status, .. } if status == StatusCode::OK => ManifestProbe {
                error_type: None,
                status_code: Some(status.as_u16()),
            },
            outcome => {
                let failure = self.read_failure(outcome, "manifest probe").await;
                probe_failure(
                    failure.error_type.expect("failure has an error type"),
                    failure.status_code,
                )
            }
        }
    }

    pub async fn fetch_day_custody(&self, day: &str) -> DayCustody {
        if self.is_revoked() {
            return custody_failure(ErrorType::Auth, None);
        }
        let Some(capability) = self.inner.capability() else {
            return custody_failure(ErrorType::Transient, None);
        };

        let (manifest, _) = match self
            .read_json(capability.probe_manifest().await, "manifest")
            .await
        {
            Ok(value) => value,
            Err(failure) => return failure,
        };
        let Some(days) = manifest.get("days").and_then(Value::as_object) else {
            return custody_failure(ErrorType::Incompatible, Some(StatusCode::OK.as_u16()));
        };
        if !days.contains_key(day) {
            // The manifest is authoritative for day existence. Absence is a reachable,
            // unproven state rather than a failed read, so the segment remains upload-eligible.
            return DayCustody {
                day_present: false,
                items: Vec::new(),
                proof_available: false,
                error_type: None,
                status_code: Some(StatusCode::OK.as_u16()),
            };
        }

        let (day_manifest, _) = match self
            .read_json(capability.manifest_day(day).await, "day manifest")
            .await
        {
            Ok(value) => value,
            Err(failure) => return failure,
        };
        let manifest_day = day_manifest.get("day").and_then(Value::as_str);
        let manifest_version = day_manifest.get("version").and_then(Value::as_u64);
        if manifest_day.is_none()
            || manifest_version.is_none()
            || day_manifest
                .get("segments")
                .and_then(Value::as_object)
                .is_none()
        {
            return custody_failure(ErrorType::Incompatible, Some(StatusCode::OK.as_u16()));
        }
        if manifest_day != Some(day) || manifest_version != Some(1) {
            // Repo-pinned policy: the authority only exemplifies version 1, so any other
            // version is deliberately unproven instead of being silently accepted.
            return DayCustody {
                day_present: true,
                items: Vec::new(),
                proof_available: false,
                error_type: None,
                status_code: Some(StatusCode::OK.as_u16()),
            };
        }

        let (segments, status_code) = match self
            .read_json(capability.segments_day(day).await, "segments")
            .await
        {
            Ok(value) => value,
            Err(failure) => return failure,
        };
        parse_segments_envelope(segments, status_code)
    }

    pub fn classify_error(status_code: Option<u16>, is_network_error: bool) -> ErrorType {
        if is_network_error {
            return ErrorType::Transient;
        }
        match status_code {
            Some(401 | 403) => ErrorType::Auth,
            // An unavailable ingest route is a client/server contract mismatch, not a
            // request the v3 client can make succeed by retrying.
            Some(404 | 426) => ErrorType::Incompatible,
            Some(400..=425) => ErrorType::Client,
            _ => ErrorType::Transient,
        }
    }

    async fn read_json(
        &self,
        outcome: LinkOutcome,
        route: &str,
    ) -> Result<(Value, u16), DayCustody> {
        match outcome {
            LinkOutcome::Success { status, body } if status == StatusCode::OK => {
                serde_json::from_slice(&body)
                    .map(|body| (body, status.as_u16()))
                    .map_err(|error| {
                        tracing::debug!(%error, %route, "V3 custody read returned malformed JSON");
                        custody_failure(ErrorType::Incompatible, Some(status.as_u16()))
                    })
            }
            outcome => Err(self.read_failure(outcome, route).await),
        }
    }

    async fn read_failure(&self, outcome: LinkOutcome, _route: &str) -> DayCustody {
        match outcome {
            LinkOutcome::Success { status, .. } | LinkOutcome::LocalRejected { status } => {
                custody_failure(
                    Self::classify_error(Some(status.as_u16()), false),
                    Some(status.as_u16()),
                )
            }
            LinkOutcome::Forbidden => {
                // The v3 authority's linked_device_required refusal is an auth latch, not a
                // retryable transport failure.
                self.inner.revoked.store(true, Ordering::Release);
                self.inner
                    .publish_link_fact(crate::private_link::LinkFact::TerminalRevocation);
                custody_failure(ErrorType::Auth, Some(StatusCode::FORBIDDEN.as_u16()))
            }
            LinkOutcome::TransportUnavailable => custody_failure(ErrorType::Transient, None),
        }
    }
}

#[cfg(test)]
/// Installs no capability and therefore makes no requests; used by scheduler/lifecycle and zero-request preflight tests, not as a transport path.
pub(crate) fn capability_less_client_for_test(
    config: &Config,
    clock: Arc<dyn Clock + Send + Sync>,
) -> UploadClient {
    let client = UploadClient::new(config, None, clock);
    client
        .inner
        .expose_link_facts
        .store(false, Ordering::Release);
    client
}

#[cfg(test)]
pub(crate) fn linked_fixture_client_for_test(
    config: &Config,
    origin: &str,
    clock: Arc<dyn Clock + Send + Sync>,
) -> UploadClient {
    let capability = crate::test_support::linked_fixture_capability(origin)
        .expect("linked fixture registered for configured test origin");
    UploadClient::new(config, Some(capability), clock)
}

impl Inner {
    fn publish_link_fact(&self, fact: crate::private_link::LinkFact) {
        self.capability()
            .map_or_else(
                || self.fallback_link_facts.clone(),
                |capability| capability.facts(),
            )
            .publish(fact);
    }

    fn capability(&self) -> Option<PrivateLinkCapability> {
        self.capability
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

fn parse_upload_body(body: Value) -> UploadResult {
    match body.get("status").and_then(Value::as_str) {
        Some("ok" | "collision") => UploadResult {
            success: true,
            duplicate: false,
            error_type: None,
            status_code: Some(StatusCode::OK.as_u16()),
            stored_key: body
                .get("segment")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("duplicate") => UploadResult {
            success: true,
            duplicate: true,
            error_type: None,
            status_code: Some(StatusCode::OK.as_u16()),
            stored_key: body
                .get("existing_segment")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        Some("failed") => {
            UploadResult::failure(Some(ErrorType::Client), Some(StatusCode::OK.as_u16()))
        }
        _ => UploadResult::failure(Some(ErrorType::Incompatible), Some(StatusCode::OK.as_u16())),
    }
}

fn retry_delay(delays: &[i64], attempt: usize) -> Duration {
    Duration::from_secs(delays[attempt.min(delays.len() - 1)].max(0) as u64)
}

async fn multipart_part(
    path: &Path,
) -> Result<(multipart::Part, u64, String, &'static str), std::io::Error> {
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(extension) if extension.eq_ignore_ascii_case("webm") => "video/webm",
        _ => "application/octet-stream",
    };
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = tokio::fs::File::open(path).await?;
    let length = file.metadata().await?.len();
    let part = multipart::Part::stream_with_length(file, length)
        .file_name(file_name.clone())
        .mime_str(content_type)
        .map_err(std::io::Error::other)?;
    Ok((part, length, file_name, content_type))
}

#[derive(Debug)]
enum MultipartBuildError {
    NoFiles,
    File {
        path: PathBuf,
        error: std::io::Error,
    },
    PartTooLarge,
    RequestTooLarge,
}

#[derive(Serialize)]
struct UploadEnvelope<'a> {
    day: &'a str,
    segment: &'a str,
    files: Vec<SubmittedFile>,
}

#[derive(Serialize)]
struct SubmittedFile {
    submitted: String,
}

fn multipart_field_length(
    boundary: &str,
    name: &str,
    value_length: u64,
    filename: Option<&str>,
    content_type: Option<&str>,
) -> Option<u64> {
    let mut header = format!("Content-Disposition: form-data; name=\"{name}\"");
    if let Some(filename) = filename {
        let filename = filename
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\\r")
            .replace('\n', "\\\n");
        header.push_str(&format!("; filename=\"{filename}\""));
    }
    if let Some(content_type) = content_type {
        header.push_str("\r\nContent-Type: ");
        header.push_str(content_type);
    }
    2_u64
        .checked_add(boundary.len() as u64)?
        .checked_add(2)?
        .checked_add(header.len() as u64)?
        .checked_add(4)?
        .checked_add(value_length)?
        .checked_add(2)
}

async fn build_multipart_form(
    day: &str,
    segment: &str,
    files: &[PathBuf],
) -> Result<(multipart::Form, u64), MultipartBuildError> {
    let form = multipart::Form::new();
    let boundary = form.boundary().to_owned();
    let mut prepared = Vec::with_capacity(files.len());
    let mut submitted = Vec::with_capacity(files.len());
    for path in files {
        let (part, file_length, filename, content_type) =
            multipart_part(path)
                .await
                .map_err(|error| MultipartBuildError::File {
                    path: path.clone(),
                    error,
                })?;
        if file_length > MAX_MULTIPART_PART_BYTES {
            return Err(MultipartBuildError::PartTooLarge);
        }
        prepared.push((part, file_length, filename.clone(), content_type));
        submitted.push(SubmittedFile {
            submitted: filename,
        });
    }
    if prepared.is_empty() {
        return Err(MultipartBuildError::NoFiles);
    }
    let envelope = serde_json::to_string(&UploadEnvelope {
        day,
        segment,
        files: submitted,
    })
    .expect("upload envelope is serializable");
    if envelope.len() as u64 > MAX_MULTIPART_PART_BYTES {
        return Err(MultipartBuildError::PartTooLarge);
    }
    let mut length =
        multipart_field_length(&boundary, "envelope", envelope.len() as u64, None, None)
            .ok_or(MultipartBuildError::RequestTooLarge)?;
    let mut form = form.text("envelope", envelope);
    for (part, file_length, filename, content_type) in prepared {
        let field_length = multipart_field_length(
            &boundary,
            "files",
            file_length,
            Some(&filename),
            Some(content_type),
        )
        .ok_or(MultipartBuildError::RequestTooLarge)?;
        length = length
            .checked_add(field_length)
            .ok_or(MultipartBuildError::RequestTooLarge)?;
        form = form.part("files", part);
    }
    length = length
        .checked_add(2)
        .and_then(|length| length.checked_add(boundary.len() as u64))
        .and_then(|length| length.checked_add(4))
        .ok_or(MultipartBuildError::RequestTooLarge)?;
    if length > MAX_REQUEST_BODY_BYTES {
        return Err(MultipartBuildError::RequestTooLarge);
    }
    Ok((form, length))
}

fn custody_failure(error_type: ErrorType, status_code: Option<u16>) -> DayCustody {
    DayCustody {
        day_present: false,
        items: Vec::new(),
        proof_available: false,
        error_type: Some(error_type),
        status_code,
    }
}

fn probe_failure(error_type: ErrorType, status_code: Option<u16>) -> ManifestProbe {
    ManifestProbe {
        error_type: Some(error_type),
        status_code,
    }
}

fn deserialize_lenient_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

fn parse_segments_envelope(body: Value, status_code: u16) -> DayCustody {
    let Some(object) = body.as_object() else {
        return custody_failure(ErrorType::Incompatible, Some(status_code));
    };
    let Some(protocol_version) = object.get("protocol_version").and_then(Value::as_u64) else {
        return custody_failure(ErrorType::Incompatible, Some(status_code));
    };
    let Some(total) = object.get("total").and_then(Value::as_u64) else {
        return custody_failure(ErrorType::Incompatible, Some(status_code));
    };
    let Some(items) = object.get("items").and_then(Value::as_array) else {
        return custody_failure(ErrorType::Incompatible, Some(status_code));
    };
    if protocol_version != 3 {
        return custody_failure(ErrorType::Incompatible, Some(status_code));
    }
    if total != items.len() as u64 {
        return DayCustody {
            day_present: true,
            items: Vec::new(),
            proof_available: false,
            error_type: None,
            status_code: Some(status_code),
        };
    }
    let items = items
        .iter()
        .cloned()
        .map(|item| {
            serde_json::from_value(item).unwrap_or(ListingEntry {
                key: None,
                original_key: None,
                files: None,
            })
        })
        .collect();
    DayCustody {
        day_present: true,
        items,
        proof_available: true,
        error_type: None,
        status_code: Some(status_code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        private_link::start_private_link_session,
        private_link_test_peer::PrivateLinkPeer,
        test_support::{
            Action, DayCustodyFixture, MockServer, MutableClock, OpportunisticDefaultListenerTrap,
            wait_for_requests,
        },
    };
    use tempfile::TempDir;

    fn config(_server: &MockServer, temp: &TempDir) -> Config {
        Config {
            base_dir: temp.path().join("data"),
            config_dir: temp.path().join("config"),
            ..Config::default()
        }
    }

    fn client(config: &Config, origin: &str) -> UploadClient {
        crate::upload::linked_fixture_client_for_test(
            config,
            origin,
            Arc::new(MutableClock::new(0.0, 0.0)),
        )
    }

    async fn linked_client(
        status: u16,
        body: Value,
    ) -> (
        TempDir,
        MockServer,
        PrivateLinkPeer,
        crate::private_link::PrivateLinkSession,
        UploadClient,
    ) {
        linked_client_with_prefixed_custody(status, body, None).await
    }

    async fn linked_client_with_prefixed_custody(
        status: u16,
        body: Value,
        custody: Option<DayCustodyFixture>,
    ) -> (
        TempDir,
        MockServer,
        PrivateLinkPeer,
        crate::private_link::PrivateLinkSession,
        UploadClient,
    ) {
        let legacy = MockServer::new(vec![]).await;
        let peer = PrivateLinkPeer::start().await;
        if let Some(custody) = custody {
            peer.enqueue_day_custody(custody);
        }
        peer.enqueue_response(status, serde_json::to_vec(&body).unwrap());
        let temp = TempDir::new().unwrap();
        let config = Config {
            stream: "host-a".into(),
            ..config(&legacy, &temp)
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host-a")
            .await
            .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        (temp, legacy, peer, session, client)
    }

    fn peer_header<'a>(
        request: &'a crate::private_link_test_peer::PeerRequest,
        name: &str,
    ) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn write_file(temp: &TempDir, name: &str, body: &[u8]) -> PathBuf {
        let path = temp.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    async fn fetch_custody_fixture(fixture: DayCustodyFixture) -> DayCustody {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        server.enqueue_day_custody(fixture);
        client(&config(&server, &temp), &server.url)
            .fetch_day_custody("20260101")
            .await
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn localhost_5015_opportunistic_clause_reports_or_asserts() {
        let trap = OpportunisticDefaultListenerTrap::bind();
        trap.assert_zero_connections();
    }

    type ParsedPart<'a> = (Vec<(&'a str, &'a str)>, &'a [u8]);

    fn parse_multipart<'a>(body: &'a [u8], boundary: &str) -> Vec<ParsedPart<'a>> {
        let delimiter = format!("--{boundary}").into_bytes();
        let next_delimiter = [b"\r\n".as_slice(), delimiter.as_slice()].concat();
        let mut cursor = body;
        let mut parts = Vec::new();
        loop {
            assert!(cursor.starts_with(&delimiter));
            cursor = &cursor[delimiter.len()..];
            if cursor.starts_with(b"--\r\n") {
                assert_eq!(cursor, b"--\r\n");
                return parts;
            }
            assert!(cursor.starts_with(b"\r\n"));
            cursor = &cursor[2..];
            let header_end = find_bytes(cursor, b"\r\n\r\n").unwrap();
            let header_text = std::str::from_utf8(&cursor[..header_end]).unwrap();
            let headers = header_text
                .split("\r\n")
                .map(|line| {
                    let (name, value) = line.split_once(": ").unwrap();
                    (name, value)
                })
                .collect();
            cursor = &cursor[header_end + 4..];
            let body_end = find_bytes(cursor, &next_delimiter).unwrap();
            parts.push((headers, &cursor[..body_end]));
            cursor = &cursor[body_end + 2..];
        }
    }

    #[tokio::test]
    async fn linked_upload_over_16mib_is_byte_exact_under_64mib() {
        let (temp, legacy, peer, session, client) =
            linked_client(200, json!({"status":"ok","segment":"large"})).await;
        let flac = vec![0x46; 9 * 1024 * 1024];
        let webm = vec![0x57; 8 * 1024 * 1024];
        let flac_path = write_file(&temp, "audio.flac", &flac);
        let webm_path = write_file(&temp, "screen.webm", &webm);

        assert!(
            client
                .upload_segment("20260101", "large", &[flac_path, webm_path])
                .await
                .success
        );
        peer.wait_for_requests(1).await;
        let requests = peer.requests();
        let request = &requests[0];
        let content_type = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str())
            .unwrap();
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let parts = parse_multipart(&request.body, boundary);
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts
                .iter()
                .filter(|(headers, _)| {
                    headers
                        .iter()
                        .any(|(_, value)| *value == "form-data; name=\"envelope\"")
                })
                .count(),
            1
        );
        assert_eq!(
            parts
                .iter()
                .filter(|(headers, _)| {
                    headers
                        .iter()
                        .any(|(_, value)| value.starts_with("form-data; name=\"files\""))
                })
                .count(),
            2
        );
        let envelope = parts
            .iter()
            .find(|(headers, _)| {
                headers
                    .iter()
                    .any(|(_, value)| *value == "form-data; name=\"envelope\"")
            })
            .expect("one envelope part");
        assert!(
            envelope
                .0
                .iter()
                .all(|(_, value)| !value.contains("filename="))
        );
        let envelope: Value = serde_json::from_slice(envelope.1).unwrap();
        assert_eq!(envelope["day"], "20260101");
        assert_eq!(envelope["segment"], "large");
        assert_eq!(
            envelope["files"],
            json!([{"submitted":"audio.flac"}, {"submitted":"screen.webm"}])
        );
        assert!(envelope.get("stream").is_none());
        assert!(envelope.get("observer").is_none());
        assert!(envelope.get("host").is_none());
        assert!(envelope.get("platform").is_none());
        assert!(envelope.get("meta").is_none());
        for (name, mime, expected) in [
            ("audio.flac", "audio/flac", flac.as_slice()),
            ("screen.webm", "video/webm", webm.as_slice()),
        ] {
            let (headers, body) = parts
                .iter()
                .find(|(headers, _)| {
                    headers
                        .iter()
                        .any(|(_, value)| value.contains(&format!("filename=\"{name}\"")))
                })
                .unwrap();
            assert!(headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("content-type") && *value == mime
            }));
            assert_eq!(*body, expected);
        }
        assert!(legacy.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_backpressured_upload_allows_small_concurrent_request() {
        let (temp, legacy, peer, session, client) = linked_client_with_prefixed_custody(
            200,
            json!({"status":"ok","segment":"large"}),
            Some(DayCustodyFixture::new("20260101", Vec::new())),
        )
        .await;
        peer.hold_request_credit();
        let media = write_file(&temp, "screen.webm", &vec![0x57; 17 * 1024 * 1024]);
        let client = Arc::new(client);
        let upload_client = Arc::clone(&client);
        let upload = tokio::spawn(async move {
            upload_client
                .upload_segment("20260101", "large", &[media])
                .await
        });
        peer.wait_for_request_staged_at_least(spl_core::mux::INITIAL_WINDOW)
            .await;
        assert_eq!(peer.max_request_staged(), spl_core::mux::INITIAL_WINDOW);
        assert!(!upload.is_finished());
        let listing_client = Arc::clone(&client);
        let listing =
            tokio::spawn(async move { listing_client.fetch_day_custody("20260101").await });
        peer.wait_for_requests(1).await;
        assert!(listing.await.unwrap().error_type.is_none());
        assert!(!upload.is_finished());
        peer.release_request_credit();
        assert!(upload.await.unwrap().success);
        peer.wait_for_requests(4).await;
        assert_eq!(peer.requests().len(), 4);
        assert!(legacy.requests().is_empty());
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn declared_over_64mib_is_local_413_and_preserves_custody() {
        let (temp, legacy, peer, session, client) =
            linked_client(200, json!({"status":"ok"})).await;
        let media = temp.path().join("oversize.webm");
        let file = std::fs::File::create(&media).unwrap();
        file.set_len((64 * 1024 * 1024 + 1) as u64).unwrap();
        drop(file);
        let result = client
            .upload_segment("20260101", "oversize", std::slice::from_ref(&media))
            .await;
        assert_eq!(result.status_code, Some(413));
        assert_eq!(result.error_type, Some(ErrorType::Client));
        assert!(media.exists());
        assert!(peer.requests().is_empty());
        assert!(legacy.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn multipart_part_at_64mib_is_admitted_pre_request() {
        let temp = TempDir::new().unwrap();
        let media = temp.path().join("limit.webm");
        let file = std::fs::File::create(&media).unwrap();
        file.set_len(MAX_MULTIPART_PART_BYTES).unwrap();
        drop(file);
        assert!(
            build_multipart_form("20260101", "boundary", &[media])
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn encoded_body_within_128mib_is_admitted_pre_request() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first.webm");
        let second = temp.path().join("second.webm");
        for path in [&first, &second] {
            let file = std::fs::File::create(path).unwrap();
            file.set_len(MAX_MULTIPART_PART_BYTES - 4096).unwrap();
        }
        let (_, encoded_length) = build_multipart_form("20260101", "boundary", &[first, second])
            .await
            .unwrap();
        assert!(encoded_length <= MAX_REQUEST_BODY_BYTES);
    }

    #[tokio::test]
    async fn multipart_overhead_crossing_128mib_is_local_413() {
        let (temp, legacy, peer, session, client) =
            linked_client(200, json!({"status":"ok"})).await;
        let media = temp.path().join("boundary.webm");
        let file = std::fs::File::create(&media).unwrap();
        file.set_len(MAX_MULTIPART_PART_BYTES).unwrap();
        drop(file);
        let second = temp.path().join("boundary-two.webm");
        let second_file = std::fs::File::create(&second).unwrap();
        second_file.set_len(MAX_MULTIPART_PART_BYTES).unwrap();
        drop(second_file);
        assert!(matches!(
            build_multipart_form("20260101", "boundary", &[media.clone(), second.clone()]).await,
            Err(MultipartBuildError::RequestTooLarge)
        ));
        let result = client
            .upload_segment("20260101", "boundary", &[media.clone(), second.clone()])
            .await;
        assert_eq!(result.status_code, Some(413));
        assert!(media.exists() && second.exists());
        assert!(peer.requests().is_empty());
        assert!(legacy.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_401_requests_record_auth_without_registration() {
        let peer = PrivateLinkPeer::start().await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            stream: "desktop".into(),
            config_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let session = start_private_link_session(temp.path(), peer.credential(), "desktop")
            .await
            .unwrap();
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability(),
            Arc::new(MutableClock::new(0.0, 0.0)),
        ));
        let media = write_file(&temp, "capture.flac", b"audio");
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(401, Vec::new());
        let listing = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.fetch_day_custody("20260101").await })
        };
        let upload = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.upload_segment("20260101", "120000", &[media]).await })
        };
        let (listing, upload) = tokio::join!(listing, upload);
        let listing = listing.unwrap();
        let upload = upload.unwrap();
        assert_eq!(listing.error_type, Some(ErrorType::Auth));
        assert_eq!(upload.error_type, Some(ErrorType::Auth));
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == format!("{}/{}", "/app/devices", "register"))
                .count(),
            0
        );
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 7: a burst is bounded by cooldown and the next 300-second window starts fresh.
    #[tokio::test]
    async fn upload_multipart_shape_headers_and_content_types() {
        let server = MockServer::new(vec![(200, json!({"status":"ok"}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let files = vec![
            write_file(&temp, "audio.flac", b"audio-content"),
            write_file(&temp, "screen.webm", b"video-content"),
            write_file(&temp, "notes.bin", b"binary-content"),
        ];
        assert!(
            client(&config, &server.url)
                .upload_segment("20260101", "120000_005", &files)
                .await
                .success
        );
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri, "/app/devices/ingest");
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("x-solstone-observer"));
        assert_eq!(request.headers[OBSERVER_PROTOCOL_VERSION_HEADER], "3");
        let body = String::from_utf8_lossy(&request.body);
        for expected in [
            "name=\"envelope\"",
            "{\"day\":\"20260101\",\"segment\":\"120000_005\",\"files\":[{\"submitted\":\"audio.flac\"},{\"submitted\":\"screen.webm\"},{\"submitted\":\"notes.bin\"}]}",
            "name=\"files\"",
            "filename=\"audio.flac\"",
            "audio/flac",
            "audio-content",
            "filename=\"screen.webm\"",
            "video/webm",
            "video-content",
            "filename=\"notes.bin\"",
            "application/octet-stream",
            "binary-content",
        ] {
            assert!(body.contains(expected), "missing {expected:?} in {body}");
        }
        for forbidden in ["stream", "observer", "host", "platform", "meta"] {
            assert!(!body.contains(&format!("\"{forbidden}\":")));
        }
        assert_eq!(body.matches("name=\"files\"").count(), 3);
    }

    // tests/test_upload.py::test_upload_segment_returns_stored_key
    #[tokio::test]
    async fn upload_segment_returns_stored_key() {
        for (body, duplicate, key) in [
            (
                json!({"status":"ok", "segment":"120000_005"}),
                false,
                "120000_005",
            ),
            (
                json!({"status":"collision", "segment":"120000_006"}),
                false,
                "120000_006",
            ),
            (
                json!({"status":"duplicate", "existing_segment":"115959_300"}),
                true,
                "115959_300",
            ),
        ] {
            let server = MockServer::new(vec![(200, body)]).await;
            let temp = TempDir::new().unwrap();
            let config = config(&server, &temp);
            let media = write_file(&temp, "audio.flac", b"audio");
            let result = client(&config, &server.url)
                .upload_segment("day", "segment", &[media])
                .await;
            assert!(result.success);
            assert_eq!(result.duplicate, duplicate);
            assert_eq!(result.stored_key.as_deref(), Some(key));
        }
    }

    #[test]
    fn upload_statuses_keep_authoritative_and_informational_keys_distinct() {
        let collision = parse_upload_body(json!({
            "status": "collision",
            "segment": "server-key",
            "segment_original": "client-key",
        }));
        assert!(collision.success);
        assert_eq!(collision.stored_key.as_deref(), Some("server-key"));

        let failed = parse_upload_body(json!({"status": "failed"}));
        assert!(!failed.success);
        assert_eq!(failed.error_type, Some(ErrorType::Client));
        assert!(failed.stored_key.is_none());

        let unknown = parse_upload_body(json!({"status": "future"}));
        assert!(!unknown.success);
        assert_eq!(unknown.error_type, Some(ErrorType::Incompatible));
        assert!(unknown.stored_key.is_none());
    }

    #[test]
    fn listing_file_size_is_leniently_parsed_but_never_defaulted() {
        for value in [json!({}), json!({"size": -1}), json!({"size": 1.5})] {
            assert!(
                serde_json::from_value::<ListingFile>(value)
                    .unwrap()
                    .size
                    .is_none()
            );
        }
        assert_eq!(
            serde_json::from_value::<ListingFile>(json!({"size": 6}))
                .unwrap()
                .size,
            Some(6)
        );
    }

    async fn attempts_for(max_retries: i64) -> (usize, UploadResult) {
        let server = MockServer::new(vec![(500, json!({})), (500, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_max_retries = max_retries;
        config.sync_retry_delays = vec![0];
        let media = write_file(&temp, "audio.flac", b"audio");
        let result = client(&config, &server.url)
            .upload_segment("day", "segment", &[media])
            .await;
        (server.requests().len(), result)
    }

    // tests/test_upload.py::test_upload_bounds_immediate_attempts
    #[tokio::test]
    async fn upload_bounds_immediate_attempts() {
        let (attempts, result) = attempts_for(10).await;
        assert_eq!(attempts, 2);
        assert_eq!(result.error_type, Some(ErrorType::Transient));
    }

    // tests/test_upload.py::test_upload_low_cap_makes_single_attempt
    // tests/test_upload.py::test_upload_zero_retries_makes_single_attempt
    // tests/test_upload.py::test_upload_negative_retries_makes_single_attempt
    #[tokio::test]
    async fn upload_low_zero_and_negative_caps_make_one_attempt() {
        for cap in [1, 0, -1] {
            assert_eq!(attempts_for(cap).await.0, 1);
        }
    }

    // AC: terminal client/protocol upload responses are not retried.
    #[tokio::test]
    async fn upload_terminal_statuses_make_one_request() {
        for (status, expected) in [
            (400, ErrorType::Client),
            (409, ErrorType::Client),
            (413, ErrorType::Client),
            (426, ErrorType::Incompatible),
        ] {
            let server = MockServer::new(vec![(status, json!({})), (200, json!({}))]).await;
            let temp = TempDir::new().unwrap();
            let mut config = config(&server, &temp);
            config.sync_max_retries = 10;
            config.sync_retry_delays = vec![0];
            let media = write_file(&temp, "audio.flac", b"audio");
            let result = client(&config, &server.url)
                .upload_segment("d", "s", &[media])
                .await;
            assert_eq!(result.error_type, Some(expected));
            assert_eq!(server.requests().len(), 1);
        }
    }

    // tests/test_upload.py::test_upload_interrupt_during_wait_returns_transient
    #[tokio::test]
    async fn upload_interrupt_before_wait_returns_transient() {
        let server = MockServer::new(vec![(500, json!({})), (200, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_max_retries = 10;
        let media = write_file(&temp, "audio.flac", b"audio");
        let client = client(&config, &server.url);
        client.request_stop();
        let result = tokio::time::timeout(
            Duration::from_millis(500),
            client.upload_segment("d", "s", &[media]),
        )
        .await
        .unwrap();
        assert_eq!(server.requests().len(), 1);
        assert_eq!(result.error_type, Some(ErrorType::Transient));
    }

    // AC: pre-cancellation does not override the only attempt's own result
    #[tokio::test]
    async fn pre_cancelled_single_attempt_returns_attempt_error() {
        let server = MockServer::new(vec![(400, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_max_retries = 1;
        let media = write_file(&temp, "audio.flac", b"audio");
        let client = client(&config, &server.url);
        client.request_stop();
        let result = client.upload_segment("d", "s", &[media]).await;
        assert_eq!(result.error_type, Some(ErrorType::Client));
        assert_eq!(server.requests().len(), 1);
    }

    // AC: retry rebuilds multipart streams and attempt two receives complete file bytes
    #[tokio::test]
    async fn retry_rebuilds_complete_multipart_body() {
        let server = MockServer::new(vec![(500, json!({})), (200, json!({"status":"ok"}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays = vec![0];
        let media = write_file(&temp, "audio.flac", b"complete-on-retry");
        assert!(
            client(&config, &server.url)
                .upload_segment("d", "s", &[media])
                .await
                .success
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .body
                .windows(17)
                .any(|bytes| bytes == b"complete-on-retry")
        );
    }

    // AC: malformed upload JSON is transient and retries
    #[tokio::test]
    async fn malformed_upload_json_retries_as_transient() {
        let server = MockServer::new_actions(vec![
            Action::Raw(200, "not-json"),
            Action::Response(200, json!({"status":"ok", "segment":"s"})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays = vec![0];
        let media = write_file(&temp, "audio.flac", b"audio");
        let result = client(&config, &server.url)
            .upload_segment("d", "s", &[media])
            .await;
        assert!(result.success);
        assert_eq!(server.requests().len(), 2);

        let terminal_server = MockServer::new_actions(vec![Action::Raw(200, "not-json")]).await;
        let terminal_temp = TempDir::new().unwrap();
        let mut terminal_config = self::config(&terminal_server, &terminal_temp);
        terminal_config.sync_max_retries = 1;
        let terminal_media = write_file(&terminal_temp, "audio.flac", b"audio");
        let terminal_result = client(&terminal_config, &terminal_server.url)
            .upload_segment("d", "s", &[terminal_media])
            .await;
        assert_eq!(terminal_result.error_type, Some(ErrorType::Transient));
    }

    // AC: a client without a linked capability is classified Transient.
    #[tokio::test]
    async fn capability_less_upload_is_transient() {
        let temp = TempDir::new().unwrap();
        let config = Config {
            sync_max_retries: 1,
            ..Config::default()
        };
        let media = write_file(&temp, "audio.flac", b"audio");
        let result =
            capability_less_client_for_test(&config, Arc::new(MutableClock::new(0.0, 0.0)))
                .upload_segment("d", "s", &[media])
                .await;
        assert_eq!(result.error_type, Some(ErrorType::Transient));
    }

    #[tokio::test]
    async fn v3_custody_requires_the_complete_triad() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        server.enqueue_day_custody(DayCustodyFixture::new(
            "20260101",
            vec![json!({"key":"new", "observed":true, "files":[{
                "name":"a.flac", "size":1, "status":"present", "sha256":"a"
            }]})],
        ));
        let result = client(&config(&server, &temp), &server.url)
            .fetch_day_custody("20260101")
            .await;
        assert!(result.day_present && result.proof_available);
        assert_eq!(result.items[0].key.as_deref(), Some("new"));
        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        for request in requests {
            assert_eq!(request.headers[OBSERVER_PROTOCOL_VERSION_HEADER], "3");
            assert!(!request.headers.contains_key("authorization"));
            assert!(!request.headers.contains_key("x-solstone-observer"));
        }
    }

    #[tokio::test]
    async fn v3_total_mismatch_is_reachable_but_unproven() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        server.enqueue_day_custody(
            DayCustodyFixture::new("20260101", vec![json!({"key":"a"})]).with_segments_total(2),
        );
        let result = client(&config(&server, &temp), &server.url)
            .fetch_day_custody("20260101")
            .await;
        assert!(result.day_present);
        assert!(!result.proof_available);
        assert!(result.error_type.is_none());
    }

    #[tokio::test]
    async fn v3_day_manifest_mismatch_and_version_are_unproven() {
        for fixture in [
            DayCustodyFixture::new("20260101", Vec::new()).with_day_manifest_day("20260102"),
            DayCustodyFixture::new("20260101", Vec::new()).with_version(2),
        ] {
            let result = fetch_custody_fixture(fixture).await;
            assert!(result.day_present);
            assert!(!result.proof_available);
            assert!(result.error_type.is_none());
        }
    }

    #[tokio::test]
    async fn v3_absent_manifest_day_is_reachable_without_proof() {
        let result = fetch_custody_fixture(DayCustodyFixture::absent("20260101")).await;
        assert!(!result.day_present);
        assert!(!result.proof_available);
        assert!(result.error_type.is_none());
        assert_eq!(result.status_code, Some(200));
    }

    #[tokio::test]
    async fn manifest_probe_is_one_reachability_request() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        server.enqueue_manifest_probe(200, b"not-a-manifest");
        let result = client(&config(&server, &temp), &server.url)
            .probe_manifest()
            .await;
        assert!(result.error_type.is_none());
        assert_eq!(server.requests().len(), 1);
        assert_eq!(server.requests()[0].uri, "/app/devices/ingest/manifest");
    }

    #[tokio::test]
    async fn v3_wrong_segments_protocol_and_malformed_legs_are_incompatible() {
        let protocol = fetch_custody_fixture(
            DayCustodyFixture::new("20260101", Vec::new()).with_segments_protocol_version(2),
        )
        .await;
        assert_eq!(protocol.error_type, Some(ErrorType::Incompatible));

        let malformed = fetch_custody_fixture(
            DayCustodyFixture::new("20260101", Vec::new())
                .with_malformed_leg(crate::test_support::DayCustodyLeg::Segments, b"not-json"),
        )
        .await;
        assert_eq!(malformed.error_type, Some(ErrorType::Incompatible));

        let failed = fetch_custody_fixture(
            DayCustodyFixture::new("20260101", Vec::new()).with_http_failure(
                crate::test_support::DayCustodyLeg::DayManifest,
                500,
                b"{}",
            ),
        )
        .await;
        assert_eq!(failed.error_type, Some(ErrorType::Transient));
    }

    #[tokio::test]
    async fn v2_shaped_segments_body_is_incompatible_without_a_legacy_request() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        server.enqueue_day_custody(
            DayCustodyFixture::new("20260101", Vec::new())
                .with_malformed_leg(crate::test_support::DayCustodyLeg::Segments, br#"[]"#),
        );
        let result = client(&config(&server, &temp), &server.url)
            .fetch_day_custody("20260101")
            .await;
        assert_eq!(result.error_type, Some(ErrorType::Incompatible));
        assert_eq!(
            server
                .requests()
                .iter()
                .map(|request| request.uri.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/app/devices/ingest/manifest",
                "/app/devices/ingest/manifest/20260101",
                "/app/devices/ingest/segments/20260101",
            ]
        );
    }

    // AC: all missing files are a local client failure and issue no request.
    #[tokio::test]
    async fn all_missing_files_make_no_request() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp), &server.url)
            .upload_segment("d", "s", &[temp.path().join("missing")])
            .await;
        assert_eq!(result.error_type, Some(ErrorType::Client));
        assert!(!result.success);
        assert!(server.requests().is_empty());
    }

    // A paired keyless capability can use the v3 mTLS ingest route.
    #[tokio::test]
    async fn unregistered_upload_uses_v3_route() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, json!({"status":"ok", "segment":"s"}).to_string());
        let temp = TempDir::new().unwrap();
        let config = Config {
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "desktop")
            .await
            .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        let media = write_file(&temp, "a.flac", b"a");
        let result = client.upload_segment("d", "s", &[media]).await;
        assert!(result.success);
        peer.wait_for_requests(1).await;
        let request = &peer.requests()[0];
        assert_eq!(request.path, "/app/devices/ingest");
        assert_eq!(
            peer_header(request, OBSERVER_PROTOCOL_VERSION_HEADER),
            Some("3")
        );
        assert_eq!(peer_header(request, "authorization"), None);
        assert_eq!(peer_header(request, "x-solstone-observer"), None);
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC: pure classification covers protocol statuses and timeout-shaped missing statuses
    #[test]
    fn error_classification() {
        assert_eq!(
            UploadClient::classify_error(Some(401), false),
            ErrorType::Auth
        );
        assert_eq!(
            UploadClient::classify_error(Some(404), false),
            ErrorType::Incompatible
        );
        assert_eq!(
            UploadClient::classify_error(Some(400), false),
            ErrorType::Client
        );
        assert_eq!(
            UploadClient::classify_error(Some(500), false),
            ErrorType::Transient
        );
        assert_eq!(
            UploadClient::classify_error(None, true),
            ErrorType::Transient
        );
        assert_eq!(
            UploadClient::classify_error(None, false),
            ErrorType::Transient
        );
    }

    // Supports AC 3/8: UploadResult preserves an HTTP status and uses None without a response.
    #[tokio::test]
    async fn upload_result_status_matches_terminal_attempt() {
        for status in [401, 403] {
            let server = MockServer::new(vec![(status, json!({}))]).await;
            let temp = TempDir::new().unwrap();
            let media = write_file(&temp, "a.flac", b"a");
            let result = client(&config(&server, &temp), &server.url)
                .upload_segment("d", "s", &[media])
                .await;
            assert_eq!(result.error_type, Some(ErrorType::Auth));
            assert_eq!(result.status_code, Some(status));
        }

        let temp = TempDir::new().unwrap();
        let transport_config = Config {
            sync_max_retries: 1,
            ..Config::default()
        };
        let media = write_file(&temp, "a.flac", b"a");
        let result = capability_less_client_for_test(
            &transport_config,
            Arc::new(MutableClock::new(0.0, 0.0)),
        )
        .upload_segment("d", "s", &[media])
        .await;
        assert_eq!(result.error_type, Some(ErrorType::Transient));
        assert_eq!(result.status_code, None);
    }

    // AC: empty retry delays use the complete local fallback
    #[tokio::test]
    async fn empty_retry_delays_use_full_fallback() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays.clear();
        let client = client(&config, &server.url);
        assert_eq!(client.inner.retry_delays, vec![5, 30, 120, 300]);
    }

    // AC: cancellation raised during backoff interrupts the active wait
    #[tokio::test]
    async fn cancellation_during_backoff_interrupts_wait() {
        let server = MockServer::new(vec![(500, json!({})), (200, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays = vec![30];
        let media = write_file(&temp, "a.flac", b"a");
        let client = Arc::new(client(&config, &server.url));
        let worker = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.upload_segment("d", "s", &[media]).await })
        };
        wait_for_requests(&server, 1).await;
        client.request_stop();
        let result = tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.error_type, Some(ErrorType::Transient));
        assert_eq!(server.requests().len(), 1);
    }

    async fn assert_403_latches(path: &str) {
        let default_trap = OpportunisticDefaultListenerTrap::bind();
        let (temp, legacy, peer, session, client) = linked_client(403, json!({})).await;
        match path {
            "upload" => {
                let media = write_file(&temp, "a.flac", b"a");
                assert!(!client.upload_segment("d", "s", &[media]).await.success);
                assert!(client.is_revoked());
            }
            "listing" => {
                assert!(
                    client
                        .fetch_day_custody("20260101")
                        .await
                        .error_type
                        .is_some()
                );
                assert!(client.is_revoked());
            }
            _ => unreachable!(),
        }
        assert!(legacy.requests().is_empty());
        default_trap.assert_zero_connections();
        assert_eq!(peer.requests().len(), 1);
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn ingest_403_latches_revocation() {
        assert_403_latches("upload").await;
    }
    // AC: upload 403 latches revoked
    #[tokio::test]
    async fn upload_403_latches_revoked() {
        assert_403_latches("upload").await;
    }
    // AC: listing 403 latches revoked
    #[tokio::test]
    async fn listing_403_latches_revoked() {
        assert_403_latches("listing").await;
    }
    #[tokio::test]
    async fn carrier_failure_does_not_latch_revocation() {
        let default_trap = OpportunisticDefaultListenerTrap::bind();
        let legacy = MockServer::new(vec![]).await;
        let peer = PrivateLinkPeer::start().await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            stream: "host-a".into(),
            ..config(&legacy, &temp)
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host-a")
            .await
            .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        peer.shutdown().await;
        assert!(
            client
                .fetch_day_custody("20260101")
                .await
                .error_type
                .is_some()
        );
        assert!(!client.is_revoked());
        assert!(legacy.requests().is_empty());
        default_trap.assert_zero_connections();
        drop(client);
        session.shutdown().await.unwrap();
    }

    // Revoked upload and listing preflight make zero requests.
    #[tokio::test]
    async fn revoked_preflight_makes_zero_requests() {
        let server = MockServer::new(vec![(403, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config, &server.url);
        let media = write_file(&temp, "a.flac", b"a");
        assert_eq!(
            client.upload_segment("d", "s", &[media]).await.error_type,
            Some(ErrorType::Auth)
        );
        assert!(client.is_revoked());
        let before = server.requests().len();
        let media = write_file(&temp, "b.flac", b"b");
        assert_eq!(
            client.upload_segment("d", "s", &[media]).await.error_type,
            Some(ErrorType::Auth)
        );
        assert_eq!(
            client.fetch_day_custody("d").await.error_type,
            Some(ErrorType::Auth)
        );
        assert_eq!(server.requests().len(), before);
    }
}
