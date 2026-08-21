// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config,
    event_sender::{EventSender, SILENT_QUEUE_MAX},
    observer::Clock,
    private_link::{
        EventBody, LinkOutcome, MAX_REQUEST_BODY_BYTES, PrivateLinkCapability, RepairOutcome,
    },
    sync_health::ErrorType,
};
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
#[cfg(test)]
use serde_json::json;
use serde_json::{Map, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
const OBSERVER_PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";
const DEFAULT_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const MAX_IMMEDIATE_ATTEMPTS: usize = 2;
const RECOVERY_COOLDOWN: Duration = Duration::from_secs(300);
const TELLING_WINDOW: Duration = Duration::from_secs(300);
const TELLING_BURST_LIMIT: usize = 12;
pub(crate) const KEY_PREFIX_CHARS: usize = 8;
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
    hostname: String,
    platform: String,
    version: String,
    retry_delays: Vec<i64>,
    immediate_attempts: usize,
    clock: Arc<dyn Clock + Send + Sync>,
    recovery_lock: tokio::sync::Mutex<()>,
    last_recovery_attempt: Mutex<Option<f64>>,
    recovery_generation: AtomicU64,
    telling: Mutex<TellingState>,
    dropped_events: AtomicU64,
}

#[derive(Default)]
struct TellingState {
    window_start: Option<f64>,
    records: usize,
    rejection_warned: bool,
}

pub struct UploadClient {
    inner: Arc<Inner>,
    event_sender: EventSender,
}

impl UploadClient {
    /// Create the linked upload and event client.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime because the event sender task is
    /// started during construction.
    pub(crate) fn new(
        config: &Config,
        capability: impl Into<Option<PrivateLinkCapability>>,
        hostname: impl Into<String>,
        platform: impl Into<String>,
        version: impl Into<String>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        Self::with_silent_capacity(
            config,
            capability.into(),
            hostname,
            platform,
            version,
            clock,
            SILENT_QUEUE_MAX,
        )
    }

    fn with_silent_capacity(
        config: &Config,
        capability: Option<PrivateLinkCapability>,
        hostname: impl Into<String>,
        platform: impl Into<String>,
        version: impl Into<String>,
        clock: Arc<dyn Clock + Send + Sync>,
        silent_capacity: usize,
    ) -> Self {
        let retry_delays = if config.sync_retry_delays.is_empty() {
            DEFAULT_RETRY_DELAYS.to_vec()
        } else {
            config.sync_retry_delays.clone()
        };
        let inner = Arc::new(Inner {
            capability: std::sync::RwLock::new(capability),
            fallback_link_facts: crate::private_link::LinkFacts::default(),
            #[cfg(test)]
            expose_link_facts: AtomicBool::new(true),
            revoked: AtomicBool::new(false),
            cancellation: CancellationToken::new(),
            hostname: hostname.into(),
            platform: platform.into(),
            version: version.into(),
            retry_delays,
            immediate_attempts: config
                .sync_max_retries
                .clamp(1, MAX_IMMEDIATE_ATTEMPTS as i64) as usize,
            clock,
            recovery_lock: tokio::sync::Mutex::new(()),
            last_recovery_attempt: Mutex::new(None),
            recovery_generation: AtomicU64::new(0),
            telling: Mutex::new(TellingState::default()),
            dropped_events: AtomicU64::new(0),
        });
        let event_sender = EventSender::with_capacity(Arc::clone(&inner), silent_capacity);
        Self {
            inner,
            event_sender,
        }
    }

    pub fn is_revoked(&self) -> bool {
        self.inner.revoked.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn revoke_for_test(&self) {
        self.inner.revoked.store(true, Ordering::Release);
    }

    pub fn is_registered(&self) -> bool {
        if let Some(capability) = self.inner.capability() {
            return capability.is_registered();
        }
        false
    }

    pub(crate) fn has_capability(&self) -> bool {
        self.inner.capability().is_some()
    }

    pub(crate) fn recovery_generation(&self) -> u64 {
        self.inner.recovery_generation.load(Ordering::Acquire)
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

    pub(crate) fn registration_metadata(&self) -> (String, String, String) {
        (
            self.inner.hostname.clone(),
            self.inner.platform.clone(),
            self.inner.version.clone(),
        )
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

    #[cfg(test)]
    pub(crate) fn stop_requested(&self) -> bool {
        self.inner.cancellation.is_cancelled()
    }

    #[cfg(test)]
    pub(crate) fn last_recovery_attempt_for_test(&self) -> Option<f64> {
        *self.inner.last_recovery_attempt.lock().unwrap()
    }

    pub async fn ensure_registered(&self, config: &mut Config) -> bool {
        if self.is_registered() {
            return true;
        }
        if let Some(capability) = self.inner.capability() {
            let _registration = self.inner.recovery_lock.lock().await;
            if self.is_registered() {
                return true;
            }
            let now = self.inner.clock.monotonic_seconds();
            if self.inner.recovery_on_cooldown(now) {
                return false;
            }
            let outcome = capability.report_unauthorized(0).await;
            if !matches!(
                outcome,
                RepairOutcome::Repaired { .. } | RepairOutcome::AlreadySuperseded { .. }
            ) {
                *self.inner.last_recovery_attempt.lock().unwrap() = Some(now);
            }
            return match outcome {
                RepairOutcome::Repaired { generation, name } => {
                    self.inner
                        .recovery_generation
                        .store(generation, Ordering::Release);
                    config.stream = name;
                    true
                }
                RepairOutcome::AlreadySuperseded { generation } => {
                    self.inner
                        .recovery_generation
                        .store(generation, Ordering::Release);
                    true
                }
                RepairOutcome::GuardRefused { reason_code } => {
                    self.inner.tell_guard_refusal(
                        reason_code.as_deref(),
                        "",
                        self.inner.clock.monotonic_seconds(),
                    );
                    false
                }
                RepairOutcome::TransportUnavailable
                | RepairOutcome::PersistenceFailed
                | RepairOutcome::InvalidRegistration => false,
            };
        }
        false
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
                    LinkOutcome::Unauthorized { generation } => {
                        self.inner
                            .recover_linked_after_401("upload", false, generation)
                            .await;
                        return UploadResult::failure(
                            Some(ErrorType::Auth),
                            Some(StatusCode::UNAUTHORIZED.as_u16()),
                        );
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

    pub async fn relay_event(&self, tract: &str, event: &str, fields: Map<String, Value>) -> bool {
        self.inner.relay_event(tract, event, fields).await
    }

    pub fn enqueue_status(&self, fields: Map<String, Value>) {
        self.event_sender.submit_status(fields);
    }

    pub fn enqueue_stream_silent(&self, fields: Map<String, Value>) -> bool {
        self.event_sender.submit_stream_silent(fields)
    }

    pub async fn stop(&mut self, timeout: Duration) -> usize {
        self.request_stop();
        self.event_sender.stop(timeout).await
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

    async fn read_failure(&self, outcome: LinkOutcome, route: &str) -> DayCustody {
        match outcome {
            LinkOutcome::Success { status, .. } | LinkOutcome::LocalRejected { status } => {
                custody_failure(
                    Self::classify_error(Some(status.as_u16()), false),
                    Some(status.as_u16()),
                )
            }
            LinkOutcome::Unauthorized { generation } => {
                self.inner
                    .recover_linked_after_401(route, false, generation)
                    .await;
                custody_failure(ErrorType::Auth, Some(StatusCode::UNAUTHORIZED.as_u16()))
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
    hostname: impl Into<String>,
    platform: impl Into<String>,
    version: impl Into<String>,
    clock: Arc<dyn Clock + Send + Sync>,
) -> UploadClient {
    let client = UploadClient::with_silent_capacity(
        config,
        None,
        hostname,
        platform,
        version,
        clock,
        SILENT_QUEUE_MAX,
    );
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
    hostname: impl Into<String>,
    platform: impl Into<String>,
    version: impl Into<String>,
    clock: Arc<dyn Clock + Send + Sync>,
) -> UploadClient {
    let capability = crate::test_support::linked_fixture_capability(origin)
        .expect("linked fixture registered for configured test origin");
    UploadClient::with_silent_capacity(
        config,
        Some(capability),
        hostname,
        platform,
        version,
        clock,
        SILENT_QUEUE_MAX,
    )
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

    async fn recover_linked_after_401(&self, route: &str, rejected_event: bool, generation: u64) {
        if self.revoked.load(Ordering::Acquire) {
            return;
        }
        if rejected_event {
            self.dropped_events.fetch_add(1, Ordering::AcqRel);
        }
        let now = self.clock.monotonic_seconds();
        let current_key = String::new();
        self.tell_first_rejection(route, &current_key, now);
        if self.recovery_on_cooldown(now) {
            return;
        }
        let _recovery = self.recovery_lock.lock().await;
        if self.revoked.load(Ordering::Acquire) {
            return;
        }
        let now = self.clock.monotonic_seconds();
        if self.recovery_on_cooldown(now) {
            return;
        }
        *self.last_recovery_attempt.lock().unwrap() = Some(now);
        let Some(capability) = self.capability() else {
            return;
        };
        match capability.report_unauthorized(generation).await {
            RepairOutcome::Repaired {
                generation: repaired,
                name,
            } => {
                self.recovery_generation.store(repaired, Ordering::Release);
                self.tell_outcome("recovered", &name, &current_key, "", now);
            }
            RepairOutcome::AlreadySuperseded {
                generation: repaired,
            } => {
                if repaired == generation {
                    self.tell_outcome("same_key_returned", "", &current_key, &current_key, now);
                } else {
                    self.recovery_generation.store(repaired, Ordering::Release);
                    self.tell_outcome("already_recovered", "", &current_key, "", now);
                }
            }
            RepairOutcome::GuardRefused { reason_code } => {
                self.tell_guard_refusal(reason_code.as_deref(), &current_key, now);
            }
            RepairOutcome::TransportUnavailable
            | RepairOutcome::PersistenceFailed
            | RepairOutcome::InvalidRegistration => {}
        }
    }

    fn recovery_on_cooldown(&self, now: f64) -> bool {
        self.last_recovery_attempt
            .lock()
            .unwrap()
            .is_some_and(|last| now - last < RECOVERY_COOLDOWN.as_secs_f64())
    }

    fn prepare_telling(&self, now: f64) -> std::sync::MutexGuard<'_, TellingState> {
        let mut telling = self.telling.lock().unwrap();
        if telling
            .window_start
            .is_none_or(|start| now - start >= TELLING_WINDOW.as_secs_f64())
        {
            *telling = TellingState {
                window_start: Some(now),
                ..TellingState::default()
            };
        }
        telling
    }

    fn tell_first_rejection(&self, route: &str, key: &str, now: f64) {
        let mut telling = self.prepare_telling(now);
        if telling.rejection_warned {
            return;
        }
        telling.rejection_warned = true;
        if telling.records >= TELLING_BURST_LIMIT {
            return;
        }
        telling.records += 1;
        drop(telling);
        tracing::warn!(
            reason = "unauthorized",
            route,
            status = 401,
            key_prefix = key_prefix(key),
            recovery_generation = self.recovery_generation.load(Ordering::Acquire),
            "Journal rejected the current key; attempting identity repair"
        );
    }

    fn tell_outcome(&self, outcome: &str, name: &str, old_key: &str, new_key: &str, now: f64) {
        let dropped_events_lower_bound = self.dropped_events.swap(0, Ordering::AcqRel);
        let mut telling = self.prepare_telling(now);
        if telling.records >= TELLING_BURST_LIMIT {
            return;
        }
        telling.records += 1;
        drop(telling);
        tracing::warn!(
            outcome,
            name,
            old_key_prefix = key_prefix(old_key),
            new_key_prefix = key_prefix(new_key),
            recovery_generation = self.recovery_generation.load(Ordering::Acquire),
            dropped_events_lower_bound,
            dropped_events_note =
                "lower bound; queued, superseded, or otherwise rejected events are not included",
            "Journal identity repair completed"
        );
    }

    fn tell_guard_refusal(&self, reason_code: Option<&str>, key: &str, now: f64) {
        let mut telling = self.prepare_telling(now);
        if telling.records >= TELLING_BURST_LIMIT {
            return;
        }
        telling.records += 1;
        drop(telling);
        tracing::error!(
            status = 403,
            reason_code,
            key_prefix = key_prefix(key),
            recovery_generation = self.recovery_generation.load(Ordering::Acquire),
            "Journal refused local identity repair"
        );
    }

    pub(crate) async fn relay_event(
        &self,
        tract: &str,
        event: &str,
        fields: Map<String, Value>,
    ) -> bool {
        if self.revoked.load(Ordering::Acquire) {
            return false;
        }
        let payload = fields;
        if let Some(capability) = self.capability() {
            return match capability
                .send_event(EventBody {
                    tract: tract.to_owned(),
                    event: event.to_owned(),
                    fields: payload,
                })
                .await
            {
                LinkOutcome::Success { status, .. } => status == StatusCode::OK,
                LinkOutcome::Unauthorized { generation } => {
                    self.recover_linked_after_401("event", true, generation)
                        .await;
                    false
                }
                LinkOutcome::Forbidden => {
                    self.revoked.store(true, Ordering::Release);
                    self.publish_link_fact(crate::private_link::LinkFact::TerminalRevocation);
                    false
                }
                LinkOutcome::TransportUnavailable | LinkOutcome::LocalRejected { .. } => false,
            };
        }
        false
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

pub(crate) fn key_prefix(key: &str) -> String {
    key.chars().take(KEY_PREFIX_CHARS).collect()
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
        private_link::{
            LinkOutcome, ObserverState, publish_observer_registration, start_private_link_session,
        },
        private_link_test_peer::PrivateLinkPeer,
        test_support::{
            Action, DayCustodyFixture, MockServer, MutableClock, OpportunisticDefaultListenerTrap,
            wait_for_requests,
        },
    };
    use tempfile::TempDir;
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

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
            "host-a",
            "linux",
            "0.1.0",
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
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".into(),
                name: "host-a".into(),
                ingest_url: "/app/devices/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            "host-a",
            "linux",
            "0.1.0",
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        (temp, legacy, peer, session, client)
    }

    async fn linked_recovery_client(
        clock: Arc<MutableClock>,
    ) -> (
        TempDir,
        PrivateLinkPeer,
        crate::private_link::PrivateLinkSession,
        UploadClient,
    ) {
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
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "STALE-KEY".into(),
                prefix: "prefix".into(),
                name: "desktop".into(),
                ingest_url: "/app/devices/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            "host",
            "linux",
            "test",
            clock,
        );
        (temp, peer, session, client)
    }

    fn registration(key: &str, name: &str) -> Value {
        json!({
            "key": key,
            "name": name,
            "prefix": "prefix",
            "ingest_url": "/app/observer/ingest",
            "protocol_version": 2
        })
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
    async fn registration_upload_listing_event_and_status_share_one_carrier() {
        let default_trap = OpportunisticDefaultListenerTrap::bind();
        let server = MockServer::new(vec![]).await;
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(
            200,
            serde_json::to_vec(&json!({"key":"K123456789", "name":"host-a"})).unwrap(),
        );
        peer.enqueue_response(
            200,
            serde_json::to_vec(&json!({"status":"ok", "key":"stored"})).unwrap(),
        );
        peer.enqueue_day_custody(DayCustodyFixture::new("20260101", Vec::new()));
        peer.enqueue_response(200, b"{}".to_vec());
        peer.enqueue_response(200, b"{}".to_vec());
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.stream = "host-a".into();
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host-a")
            .await
            .unwrap();
        let descriptor = json!({
            "hostname": "host-a",
            "label": "host-a",
            "platform": "linux",
            "stream_type": "desktop",
            "version": "0.1.0",
        });
        assert!(matches!(
            session.register_for_test(&descriptor).await,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K123456789".into(),
                prefix: "prefix".into(),
                name: "host-a".into(),
                ingest_url: "/app/devices/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            "host-a",
            "linux",
            "0.1.0",
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        let capture = write_file(&temp, "capture.jsonl", b"{\"event\":1}\n");
        assert!(
            client
                .upload_segment("20260101", "120000", &[capture])
                .await
                .success
        );
        assert!(
            client
                .fetch_day_custody("20260101")
                .await
                .error_type
                .is_none()
        );
        assert!(
            client
                .relay_event("observe", "stream_silent", Map::new())
                .await
        );
        client.enqueue_status(Map::from_iter([("mode".into(), json!("idle"))]));
        for _ in 0..100 {
            if peer.requests().len() == 7 {
                break;
            }
            tokio::task::yield_now().await;
        }

        let requests = peer.requests();
        assert_eq!(requests.len(), 7);
        assert_eq!(
            (requests[0].method.as_str(), requests[0].path.as_str()),
            ("POST", "/app/devices/register")
        );
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str()),
            Some("application/json")
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .all(|(name, _)| !name.eq_ignore_ascii_case("authorization"))
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
            json!({
                "hostname": "host-a",
                "label": "host-a",
                "platform": "linux",
                "stream_type": "desktop",
                "version": "0.1.0",
            })
        );
        assert_eq!(
            (requests[1].method.as_str(), requests[1].path.as_str()),
            ("POST", "/app/devices/ingest")
        );
        assert_eq!(peer_header(&requests[1], "authorization"), None);
        assert_eq!(peer_header(&requests[1], "x-solstone-observer"), None);
        assert_eq!(
            peer_header(&requests[1], OBSERVER_PROTOCOL_VERSION_HEADER),
            Some("3")
        );
        let content_type = requests[1]
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str())
            .unwrap();
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .filter(|value| {
                !value.is_empty()
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
            .unwrap();
        assert_eq!(
            content_type,
            format!("multipart/form-data; boundary={boundary}")
        );
        let parts = parse_multipart(&requests[1].body, boundary);
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0],
            (
                vec![("Content-Disposition", "form-data; name=\"envelope\"")],
                b"{\"day\":\"20260101\",\"segment\":\"120000\",\"files\":[{\"submitted\":\"capture.jsonl\"}]}".as_slice()
            )
        );
        assert_eq!(
            parts[1],
            (
                vec![
                    (
                        "Content-Disposition",
                        "form-data; name=\"files\"; filename=\"capture.jsonl\""
                    ),
                    ("Content-Type", "application/octet-stream"),
                ],
                b"{\"event\":1}\n".as_slice()
            )
        );
        for (request, path) in [
            (&requests[2], "/app/devices/ingest/manifest"),
            (&requests[3], "/app/devices/ingest/manifest/20260101"),
            (&requests[4], "/app/devices/ingest/segments/20260101"),
        ] {
            assert_eq!(
                (request.method.as_str(), request.path.as_str()),
                ("GET", path)
            );
            assert_eq!(
                peer_header(request, OBSERVER_PROTOCOL_VERSION_HEADER),
                Some("3")
            );
            assert_eq!(peer_header(request, "authorization"), None);
            assert_eq!(peer_header(request, "x-solstone-observer"), None);
        }
        assert!(server.requests().is_empty());
        default_trap.assert_zero_connections();
        assert_eq!(peer.accepted_carriers(), 1);
        for (request, expected) in [
            (
                &requests[5],
                json!({"tract":"observe","event":"stream_silent"}),
            ),
            (
                &requests[6],
                json!({"tract":"observe","event":"status","mode":"idle"}),
            ),
        ] {
            assert_eq!(
                (request.method.as_str(), request.path.as_str()),
                ("POST", "/app/devices/ingest/event")
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).unwrap(),
                expected
            );
        }
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // tests/test_upload.py::test_ensure_registered_skips_when_key_present
    // AC 18: constructor clock plumbing deliberately preserves the existing-key short circuit.
    #[tokio::test]
    async fn ensure_registered_skips_when_key_present() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        assert!(
            client(&config, &server.url)
                .ensure_registered(&mut config)
                .await
        );
        assert!(server.requests().is_empty());
    }

    // AC 1/2: a stale-key 401 registers, persists a distinct replacement, and the next relay uses it.
    #[tokio::test]
    async fn later_safe_retry_uses_new_generation() {
        const NEW: &str = "NEW-KEY-222";
        let (_temp, peer, session, client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration(NEW, "desktop-new").to_string());
        peer.enqueue_response(200, b"{}".to_vec());
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert!(client.relay_event("observe", "status", Map::new()).await);
        let requests = peer.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            peer_header(&requests[2], "authorization").unwrap(),
            format!("Bearer {NEW}")
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn rejected_request_is_never_blindly_replayed() {
        let (_temp, peer, session, client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("NEW-KEY", "desktop-new").to_string());
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        let requests = peer.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/app/devices/ingest/event");
        assert_eq!(requests[1].path, "/app/devices/register");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 3/7: an idempotent same-key response is not recovery and still arms cooldown.
    #[tokio::test]
    async fn same_key_repair_reports_truthfully_without_generation_publish() {
        let (_temp, peer, session, client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("STALE-KEY", "desktop").to_string());
        peer.enqueue_response(401, Vec::new());
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let generation = client.recovery_generation();
        async {
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert_eq!(client.recovery_generation(), generation);
            assert!(!client.relay_event("observe", "status", Map::new()).await);
        }
        .with_subscriber(subscriber)
        .await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            1
        );
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("outcome=\"same_key_returned\""),
            "{captured}"
        );
        assert!(!captured.contains("outcome=\"recovered\""), "{captured}");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 6: the real sync-query/event race shares one in-flight recovery attempt.
    #[tokio::test]
    async fn concurrent_401_burst_registers_once_per_generation() {
        let (_temp, peer, session, client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("NEW-KEY", "desktop-new").to_string());
        let gate = Arc::new(AtomicBool::new(false));
        peer.gate_queued_responses_nonblocking(2, gate.clone());
        let client = Arc::new(client);
        let listing = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.fetch_day_custody("20260101").await })
        };
        let event = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.relay_event("observe", "status", Map::new()).await })
        };
        peer.wait_for_requests(2).await;
        gate.store(true, Ordering::Release);
        peer.notify_response_gates();
        let (listing, event) = tokio::join!(listing, event);
        let listing = listing.unwrap();
        let event = event.unwrap();
        assert!(listing.error_type.is_some());
        assert!(!event);
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            1
        );
        drop(client);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 7: a burst is bounded by cooldown and the next 300-second window starts fresh.
    #[tokio::test]
    async fn cooldown_suppresses_registration_until_next_generation_window() {
        let clock = Arc::new(MutableClock::new(0.0, 0.0));
        let (_temp, peer, session, client) = linked_recovery_client(clock.clone()).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("STALE-KEY", "desktop").to_string());
        for _ in 0..20 {
            peer.enqueue_response(401, Vec::new());
        }
        peer.enqueue_response(200, registration("STALE-KEY", "desktop").to_string());
        for _ in 0..20 {
            assert!(!client.relay_event("observe", "status", Map::new()).await);
        }
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            1
        );
        clock.set_mono(300.0);
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            2
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 9: every failed register shape preserves disk, runtime, caller config, and stale bearer use.
    #[tokio::test]
    async fn failed_registration_preserves_all_stores_and_stale_bearer() {
        for failed in [(500, json!({})), (200, json!({"name":"desktop"}))] {
            let (temp, peer, session, client) =
                linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
            peer.enqueue_response(401, Vec::new());
            peer.enqueue_response(failed.0, failed.1.to_string());
            peer.enqueue_response(200, b"{}".to_vec());
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert!(client.relay_event("observe", "status", Map::new()).await);
            let requests = peer.requests();
            assert_eq!(
                peer_header(requests.last().unwrap(), "authorization"),
                Some("Bearer STALE-KEY")
            );
            let observer: ObserverState =
                serde_json::from_slice(&std::fs::read(temp.path().join("observer.json")).unwrap())
                    .unwrap();
            assert_eq!(observer.key, "STALE-KEY");
            session.shutdown().await.unwrap();
            peer.shutdown().await;
        }
    }

    // AC 8: persistence after a successful register is synchronous and await-free, so
    // cancellation cannot be scheduled between the response and persistence. Cancellation before
    // the response leaves the exact stale pair; uninterrupted completion writes the exact new
    // pair. Those are therefore the only two identities reachable on disk.
    #[tokio::test]
    async fn cancellation_boundary_exposes_only_stale_or_new_disk_identity() {
        const NEW: &str = "NEW-KEY";
        for cancel in [true, false] {
            let (temp, peer, session, client) =
                linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
            peer.enqueue_response(401, Vec::new());
            peer.enqueue_response(200, registration(NEW, "desktop-new").to_string());
            let gate = Arc::new(AtomicBool::new(false));
            peer.gate_queued_response_nonblocking(1, gate.clone());
            let client = Arc::new(client);
            let relay = {
                let client = Arc::clone(&client);
                tokio::spawn(
                    async move { client.relay_event("observe", "status", Map::new()).await },
                )
            };
            peer.wait_for_requests(2).await;
            if cancel {
                relay.abort();
            }
            gate.store(true, Ordering::Release);
            peer.notify_response_gates();
            let _ = relay.await;
            let observer: ObserverState =
                serde_json::from_slice(&std::fs::read(temp.path().join("observer.json")).unwrap())
                    .unwrap();
            assert_eq!(observer.key, if cancel { "STALE-KEY" } else { NEW });
            drop(client);
            session.shutdown().await.unwrap();
            peer.shutdown().await;
        }
    }

    // AC 15: twenty clean relays emit no rejection warning.
    #[tokio::test]
    async fn clean_relays_emit_no_rejection_warning() {
        let server = MockServer::new((0..20).map(|_| (200, json!({}))).collect()).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config, &server.url);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        async {
            for _ in 0..20 {
                assert!(client.relay_event("observe", "status", Map::new()).await);
            }
        }
        .with_subscriber(subscriber)
        .await;
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(
            !captured.contains("Journal rejected the current key"),
            "{captured}"
        );
    }

    // AC 15: a directly-awaited path with no relay and no rejection is silent.
    #[tokio::test]
    async fn zero_rejections_emit_no_rejection_warning() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let _client = client(&config, &server.url);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        async {}.with_subscriber(subscriber).await;
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(
            !captured.contains("Journal rejected the current key"),
            "{captured}"
        );
    }

    // Python ancestor: tests/test_upload.py::test_upload_segment_uses_bearer_and_keyless_route.
    // V3 uses certificate-only identity; this test asserts those identity headers are absent.
    // tests/test_upload.py::test_upload_segment_declares_content_types
    // AC: multipart received bytes preserve fields, filenames, content types, and file content
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
        let result = capability_less_client_for_test(
            &config,
            "host-a",
            "linux",
            "0.1.0",
            Arc::new(MutableClock::new(0.0, 0.0)),
        )
        .upload_segment("d", "s", &[media])
        .await;
        assert_eq!(result.error_type, Some(ErrorType::Transient));
    }

    // tests/test_upload.py::test_relay_event_uses_bearer_and_keyless_route
    #[tokio::test]
    async fn relay_event_uses_bearer_and_keyless_route() {
        let server = MockServer::new(vec![(200, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let mut fields = Map::new();
        fields.insert("mode".into(), json!("idle"));
        assert!(
            client(&config, &server.url)
                .relay_event("observe", "status", fields)
                .await
        );
        let request = &server.requests()[0];
        assert_eq!(request.uri, "/app/devices/ingest/event");
        assert_eq!(request.headers["authorization"], "Bearer K");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            body,
            json!({"tract":"observe", "event":"status", "mode":"idle"})
        );
    }

    // AC: relay 401 does not latch revoked
    #[tokio::test]
    async fn relay_401_does_not_latch_revoked() {
        let server = MockServer::new(vec![(401, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let client = client(&config(&server, &temp), &server.url);
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert!(!client.is_revoked());
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

    // AC: a paired but unregistered observer can use the v3 mTLS ingest route.
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
            "host",
            "linux",
            "test",
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
            "host-a",
            "linux",
            "0.1.0",
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
            "event" => {
                assert!(!client.relay_event("observe", "status", Map::new()).await);
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

    // AC 18: register-route guard refusal deliberately no longer latches revocation.
    #[tokio::test]
    async fn registration_route_guard_refusal_keeps_prior_authority_and_does_not_latch_or_pair() {
        let temp = TempDir::new().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(403, json!({"reason_code":"local_request_only"}).to_string());
        let session = start_private_link_session(temp.path(), peer.credential(), "desktop")
            .await
            .unwrap();
        let mut config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "desktop".to_owned(),
            ..Config::default()
        };
        let client = UploadClient::new(
            &config,
            session.capability(),
            "host",
            "linux",
            "1",
            Arc::new(MutableClock::new(0.0, 0.0)),
        );
        assert!(!client.ensure_registered(&mut config).await);
        assert!(!client.is_revoked());
        assert_eq!(peer.requests().len(), 1);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 5: register guard refusal preserves the stale identity; ingest 403 still revokes it.
    #[tokio::test]
    async fn register_guard_refusal_does_not_revoke_but_ingest_403_does() {
        let (_temp, peer, session, client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(403, json!({"reason_code":"local_request_only"}).to_string());
        peer.enqueue_response(403, Vec::new());
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        async {
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert!(!client.is_revoked());
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert!(client.is_revoked());
        }
        .with_subscriber(subscriber)
        .await;
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("Journal refused local identity repair"));
        assert!(captured.contains("local_request_only"));
        session.shutdown().await.unwrap();
        peer.shutdown().await;
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
    // AC: event relay 403 latches revoked
    #[tokio::test]
    async fn event_403_latches_revoked() {
        assert_403_latches("event").await;
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
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".into(),
                name: "host-a".into(),
                ingest_url: "/app/devices/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let client = UploadClient::new(
            &config,
            session.capability(),
            "host-a",
            "linux",
            "0.1.0",
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

    // AC: revoked upload, listing, and event relay preflight make zero requests
    #[tokio::test]
    async fn revoked_preflight_makes_zero_requests() {
        let server = MockServer::new(vec![(403, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config, &server.url);
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert!(client.is_revoked());
        let before = server.requests().len();
        let media = write_file(&temp, "a.flac", b"a");
        assert_eq!(
            client.upload_segment("d", "s", &[media]).await.error_type,
            Some(ErrorType::Auth)
        );
        assert_eq!(
            client.fetch_day_custody("d").await.error_type,
            Some(ErrorType::Auth)
        );
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert_eq!(server.requests().len(), before);
    }

    // tests/test_event_sender.py::test_submit_status_is_nonblocking_while_relay_is_blocked
    #[tokio::test]
    async fn submit_status_is_nonblocking_while_relay_is_blocked() {
        let (server, gate) = MockServer::gated().await;
        let temp = TempDir::new().unwrap();
        let capability = crate::test_support::linked_fixture_capability(&server.url).unwrap();
        let mut client = UploadClient::with_silent_capacity(
            &config(&server, &temp),
            Some(capability),
            "host",
            "linux",
            "v",
            Arc::new(MutableClock::new(0.0, 0.0)),
            1,
        );
        client.enqueue_status(Map::from_iter([("seq".into(), json!(1))]));
        wait_for_requests(&server, 1).await;
        let start = tokio::time::Instant::now();
        client.enqueue_status(Map::from_iter([("seq".into(), json!(2))]));
        assert!(start.elapsed() < Duration::from_millis(100));
        gate.notify_one();
        wait_for_requests(&server, 2).await;
        gate.notify_one();
        assert_eq!(client.stop(Duration::from_secs(1)).await, 0);
    }

    // tests/test_event_sender.py::test_status_supersession_delivers_newest_after_blocked_relay_recovers
    #[tokio::test]
    async fn status_supersession_delivers_newest() {
        let (server, gate) = MockServer::gated().await;
        let temp = TempDir::new().unwrap();
        let mut client = client(&config(&server, &temp), &server.url);
        client.enqueue_status(Map::from_iter([("seq".into(), json!(1))]));
        wait_for_requests(&server, 1).await;
        client.enqueue_status(Map::from_iter([("seq".into(), json!(2))]));
        client.enqueue_status(Map::from_iter([("seq".into(), json!(3))]));
        gate.notify_one();
        wait_for_requests(&server, 2).await;
        gate.notify_one();
        assert_eq!(client.stop(Duration::from_secs(1)).await, 0);
        let requests = server.requests();
        let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(body["seq"], 3);
    }

    // tests/test_event_sender.py::test_stream_silent_overflow_drop_and_bounded_stop
    #[tokio::test]
    async fn silent_overflow_rejects_incoming_and_stop_is_bounded() {
        let (server, gate) = MockServer::gated().await;
        let temp = TempDir::new().unwrap();
        let capability = crate::test_support::linked_fixture_capability(&server.url).unwrap();
        let mut client = UploadClient::with_silent_capacity(
            &config(&server, &temp),
            Some(capability),
            "host",
            "linux",
            "v",
            Arc::new(MutableClock::new(0.0, 0.0)),
            1,
        );
        assert!(client.enqueue_stream_silent(Map::from_iter([("node_id".into(), json!(1))])));
        assert!(!client.enqueue_stream_silent(Map::from_iter([("node_id".into(), json!(2))])));
        wait_for_requests(&server, 1).await;
        let start = tokio::time::Instant::now();
        assert_eq!(client.stop(Duration::from_millis(10)).await, 1);
        assert!(start.elapsed() < Duration::from_millis(500));
        gate.notify_one();
        assert_eq!(client.stop(Duration::from_secs(1)).await, 0);
        let body: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
        assert_eq!(body["node_id"], 1);
    }

    // AC 13: event recovery performs exactly one register request while the bounded queue remains
    // usable. There is no retry loop on this path, so a stalled worker is bounded by the single
    // register request's EVENT_TIMEOUT without making this a 30-second real-time test.
    #[tokio::test]
    async fn event_recovery_is_single_attempt_and_queue_stays_bounded() {
        let (_temp, peer, session, mut client) =
            linked_recovery_client(Arc::new(MutableClock::new(0.0, 0.0))).await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(500, Vec::new());
        let gate = Arc::new(AtomicBool::new(false));
        peer.gate_queued_response_nonblocking(1, gate.clone());
        client.enqueue_status(Map::new());
        peer.wait_for_requests(2).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            1
        );
        for sequence in 0..20 {
            assert!(
                client
                    .enqueue_stream_silent(Map::from_iter([("sequence".into(), json!(sequence),)]))
            );
        }
        let started = tokio::time::Instant::now();
        gate.store(true, Ordering::Release);
        peer.notify_response_gates();
        assert_eq!(client.stop(Duration::from_secs(1)).await, 0);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/devices/register")
                .count(),
            1
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
