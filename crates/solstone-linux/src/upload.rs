// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::{Config, ConfigPaths, save_identity},
    event_sender::{EventSender, SILENT_QUEUE_MAX},
    observer::Clock,
    sync_health::ErrorType,
};
use reqwest::{Client, StatusCode, multipart};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_TYPE: &str = "desktop";
const OBSERVER_PROTOCOL_VERSION_HEADER: &str = "X-Solstone-Protocol-Version";
const DEFAULT_RETRY_DELAYS: [i64; 4] = [5, 30, 120, 300];
const MAX_IMMEDIATE_ATTEMPTS: usize = 2;
const RECOVERY_COOLDOWN: Duration = Duration::from_secs(300);
const TELLING_WINDOW: Duration = Duration::from_secs(300);
const TELLING_BURST_LIMIT: usize = 12;
pub(crate) const KEY_PREFIX_CHARS: usize = 8;

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
pub struct QueryResult {
    pub segments: Option<Vec<ListingEntry>>,
    pub error_type: Option<ErrorType>,
    pub status_code: Option<u16>,
    pub legacy: bool,
    pub truncated: bool,
}

pub(crate) struct Inner {
    url: String,
    key: Mutex<String>,
    stream: Mutex<String>,
    revoked: AtomicBool,
    client: Client,
    cancellation: CancellationToken,
    hostname: String,
    platform: String,
    version: String,
    retry_delays: Vec<i64>,
    immediate_attempts: usize,
    paths: ConfigPaths,
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

enum RegisterAttempt {
    Registered { key: String, name: String },
    GuardRefused { reason_code: Option<String> },
    Failed,
}

pub struct UploadClient {
    inner: Arc<Inner>,
    event_sender: EventSender,
}

impl UploadClient {
    /// Create an observer HTTP client.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime because the event sender task is
    /// started during construction.
    pub fn new(
        config: &Config,
        hostname: impl Into<String>,
        platform: impl Into<String>,
        version: impl Into<String>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        Self::with_silent_capacity(config, hostname, platform, version, clock, SILENT_QUEUE_MAX)
    }

    fn with_silent_capacity(
        config: &Config,
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
            url: config.server_url.trim_end_matches('/').to_owned(),
            key: Mutex::new(config.key.clone()),
            stream: Mutex::new(config.stream.clone()),
            revoked: AtomicBool::new(false),
            client: Client::new(),
            cancellation: CancellationToken::new(),
            hostname: hostname.into(),
            platform: platform.into(),
            version: version.into(),
            retry_delays,
            immediate_attempts: config
                .sync_max_retries
                .clamp(1, MAX_IMMEDIATE_ATTEMPTS as i64) as usize,
            paths: ConfigPaths {
                base_dir: Some(config.base_dir.clone()),
                config_dir: Some(config.config_dir.clone()),
            },
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

    pub fn is_registered(&self) -> bool {
        !self.inner.key.lock().unwrap().is_empty()
    }

    pub(crate) fn recovery_generation(&self) -> u64 {
        self.inner.recovery_generation.load(Ordering::Acquire)
    }

    pub fn request_stop(&self) {
        self.inner.cancellation.cancel();
    }

    #[cfg(test)]
    pub(crate) fn stop_requested(&self) -> bool {
        self.inner.cancellation.is_cancelled()
    }

    pub async fn ensure_registered(&self, config: &mut Config) -> bool {
        if self.is_registered() {
            return true;
        }
        if self.inner.url.is_empty() {
            return false;
        }
        let _registration = self.inner.recovery_lock.lock().await;
        if self.is_registered() {
            return true;
        }
        let attempts = 3.min(self.inner.retry_delays.len());
        for attempt in 0..attempts {
            match self.inner.register_once().await {
                RegisterAttempt::Registered { key, name } => {
                    if let Err(error) = save_identity(&self.inner.paths, &key, &name) {
                        // Named deviation: Python propagates the persistence error; Rust keeps all
                        // three identity stores unchanged so a later call can retry.
                        tracing::error!(%error, "Failed to persist registration");
                        return false;
                    }
                    *self.inner.key.lock().unwrap() = key.clone();
                    *self.inner.stream.lock().unwrap() = name.clone();
                    config.key = key;
                    config.stream = name.clone();
                    tracing::info!(name, "Registered");
                    return true;
                }
                RegisterAttempt::GuardRefused { reason_code } => {
                    // One-shot CLI setup deliberately does not consume the daemon telling window.
                    tracing::error!(
                        status = 403,
                        reason_code = reason_code.as_deref(),
                        key_prefix = key_prefix(&self.inner.key.lock().unwrap()),
                        recovery_generation =
                            self.inner.recovery_generation.load(Ordering::Acquire),
                        "Journal refused local identity repair"
                    );
                    return false;
                }
                RegisterAttempt::Failed => {
                    tracing::warn!(attempt = attempt + 1, "Registration attempt failed")
                }
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(retry_delay(&self.inner.retry_delays, attempt)).await;
            }
        }
        tracing::error!(attempts, "Registration failed after all attempts");
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
        let key = self.inner.key.lock().unwrap().clone();
        if key.is_empty() || self.inner.url.is_empty() {
            return UploadResult::failure(Some(ErrorType::Client), None);
        }
        if !files.iter().any(|path| path.exists()) {
            return UploadResult::failure(None, None);
        }

        let url = format!("{}/app/observer/ingest", self.inner.url);
        let mut last_error = None;
        let mut last_status = None;
        for attempt in 0..self.inner.immediate_attempts {
            let mut form = multipart::Form::new()
                .text("day", day.to_owned())
                .text("segment", segment.to_owned());
            let mut file_count = 0;
            for path in files.iter().filter(|path| path.exists()) {
                // Named deviation: Python propagates file-open errors; Rust skips
                // unreadable files so one bad capture does not crash the observer.
                match multipart_part(path).await {
                    Ok(part) => {
                        form = form.part("files", part);
                        file_count += 1;
                    }
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "File not found, skipping")
                    }
                }
            }
            if file_count == 0 {
                return UploadResult::failure(None, None);
            }

            let response = self
                .inner
                .client
                .post(&url)
                .bearer_auth(&key)
                .multipart(form)
                .timeout(UPLOAD_TIMEOUT)
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::OK => {
                    match response.json::<Value>().await {
                        Ok(body) => {
                            return match body.get("status").and_then(Value::as_str) {
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
                                _ => UploadResult::failure(
                                    Some(ErrorType::Incompatible),
                                    Some(StatusCode::OK.as_u16()),
                                ),
                            };
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
                Ok(response) => {
                    let status = response.status();
                    let error_type = Self::classify_error(Some(status.as_u16()), false);
                    last_error = Some(error_type);
                    last_status = Some(status.as_u16());
                    if status == StatusCode::FORBIDDEN {
                        self.inner.revoked.store(true, Ordering::Release);
                    } else if status == StatusCode::UNAUTHORIZED {
                        self.inner.recover_after_401("upload", false).await;
                    }
                    if error_type != ErrorType::Transient {
                        tracing::error!(%status, ?error_type, "Upload rejected");
                        return UploadResult::failure(Some(error_type), Some(status.as_u16()));
                    }
                    tracing::warn!(attempt = attempt + 1, %status, "Upload attempt failed");
                }
                Err(error) => {
                    tracing::warn!(attempt = attempt + 1, %error, "Upload attempt failed");
                    last_error = Some(ErrorType::Transient);
                    last_status = None;
                }
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

    pub async fn get_server_segments(&self, day: &str) -> QueryResult {
        if self.is_revoked() {
            return query_failure(ErrorType::Auth, None);
        }
        let key = self.inner.key.lock().unwrap().clone();
        if key.is_empty() || self.inner.url.is_empty() {
            return query_failure(ErrorType::Client, None);
        }
        let url = format!("{}/app/observer/ingest/segments/{day}", self.inner.url);
        let response = self
            .inner
            .client
            .get(url)
            .bearer_auth(key)
            .header(OBSERVER_PROTOCOL_VERSION_HEADER, "2")
            .timeout(EVENT_TIMEOUT)
            .send()
            .await;
        let Ok(response) = response else {
            return query_failure(ErrorType::Transient, None);
        };
        let status = response.status();
        if status != StatusCode::OK {
            let error_type = Self::classify_error(Some(status.as_u16()), false);
            if status == StatusCode::FORBIDDEN {
                self.inner.revoked.store(true, Ordering::Release);
            } else if status == StatusCode::UNAUTHORIZED {
                self.inner.recover_after_401("listing", false).await;
            }
            tracing::warn!(%status, ?error_type, "Segments query failed");
            return query_failure(error_type, Some(status.as_u16()));
        }
        let body = match response.json::<Value>().await {
            Ok(body) => body,
            Err(error) => {
                tracing::debug!(%error, "Segments query returned malformed JSON");
                return query_failure(ErrorType::Transient, None);
            }
        };
        parse_listing(body, status.as_u16())
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
            Some(400) => ErrorType::Client,
            Some(404) => ErrorType::Incompatible,
            _ => ErrorType::Transient,
        }
    }
}

impl Inner {
    async fn register_once(&self) -> RegisterAttempt {
        let stream = self.stream.lock().unwrap().clone();
        let mut descriptor = json!({
            "platform": self.platform,
            "hostname": self.hostname,
            "stream_type": STREAM_TYPE,
            "version": self.version,
        });
        if !stream.is_empty() {
            descriptor["label"] = Value::String(stream);
        }
        let request = self
            .client
            .post(format!("{}/app/observer/register", self.url))
            .json(&descriptor)
            .timeout(EVENT_TIMEOUT)
            .send();
        let response = tokio::select! {
            response = request => response,
            () = self.cancellation.cancelled() => return RegisterAttempt::Failed,
        };
        let Ok(response) = response else {
            return RegisterAttempt::Failed;
        };
        if response.status() == StatusCode::FORBIDDEN {
            let reason_code = response.json::<Value>().await.ok().and_then(|body| {
                body.get("reason_code")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            return RegisterAttempt::GuardRefused { reason_code };
        }
        if response.status() != StatusCode::OK {
            return RegisterAttempt::Failed;
        }
        let Ok(body) = response.json::<Value>().await else {
            return RegisterAttempt::Failed;
        };
        let (Some(key), Some(name)) = (
            body.get("key").and_then(Value::as_str),
            body.get("name").and_then(Value::as_str),
        ) else {
            return RegisterAttempt::Failed;
        };
        RegisterAttempt::Registered {
            key: key.to_owned(),
            name: name.to_owned(),
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

    async fn recover_after_401(&self, route: &str, rejected_event: bool) {
        // A 403 latch is an authorization boundary: no trigger may attempt recovery once set.
        if self.revoked.load(Ordering::Acquire) {
            return;
        }
        if rejected_event {
            self.dropped_events.fetch_add(1, Ordering::AcqRel);
        }
        let now = self.clock.monotonic_seconds();
        let current_key = self.key.lock().unwrap().clone();
        self.tell_first_rejection(route, &current_key, now);
        if self.recovery_on_cooldown(now) {
            return;
        }

        let _recovery = self.recovery_lock.lock().await;
        // Recheck both authorization and cooldown after waiting for a concurrent trigger.
        if self.revoked.load(Ordering::Acquire) {
            return;
        }
        let now = self.clock.monotonic_seconds();
        if self.recovery_on_cooldown(now) {
            return;
        }
        *self.last_recovery_attempt.lock().unwrap() = Some(now);
        // Mitigation 1 compares against the key rejected by this attempt, never a prior response.
        let rejected_key = self.key.lock().unwrap().clone();
        let attempt = self.register_once().await;
        if self.revoked.load(Ordering::Acquire) {
            return;
        }
        match attempt {
            RegisterAttempt::Registered { key, name } if key == rejected_key => {
                self.tell_outcome("same_key_returned", &name, &rejected_key, &key, now);
            }
            RegisterAttempt::Registered { key, name } => {
                if let Err(error) = save_identity(&self.paths, &key, &name) {
                    tracing::error!(%error, "Failed to persist repaired journal identity");
                    return;
                }
                *self.key.lock().unwrap() = key.clone();
                *self.stream.lock().unwrap() = name.clone();
                self.recovery_generation.fetch_add(1, Ordering::AcqRel);
                self.tell_outcome("recovered", &name, &rejected_key, &key, now);
            }
            RegisterAttempt::GuardRefused { reason_code } => {
                self.tell_guard_refusal(reason_code.as_deref(), &rejected_key, now);
            }
            RegisterAttempt::Failed => {}
        }
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
        let key = self.key.lock().unwrap().clone();
        if key.is_empty() || self.url.is_empty() {
            return false;
        }
        let mut payload = fields;
        payload.insert("tract".into(), Value::String(tract.into()));
        payload.insert("event".into(), Value::String(event.into()));
        let response = self
            .client
            .post(format!("{}/app/observer/ingest/event", self.url))
            .bearer_auth(key)
            .json(&payload)
            .timeout(EVENT_TIMEOUT)
            .send()
            .await;
        match response {
            Ok(response) if response.status() == StatusCode::OK => true,
            Ok(response) => {
                if response.status() == StatusCode::FORBIDDEN {
                    self.revoked.store(true, Ordering::Release);
                } else if response.status() == StatusCode::UNAUTHORIZED {
                    self.recover_after_401("event", true).await;
                }
                false
            }
            Err(_) => false,
        }
    }
}

pub(crate) fn key_prefix(key: &str) -> String {
    key.chars().take(KEY_PREFIX_CHARS).collect()
}

fn retry_delay(delays: &[i64], attempt: usize) -> Duration {
    Duration::from_secs(delays[attempt.min(delays.len() - 1)].max(0) as u64)
}

async fn multipart_part(path: &Path) -> Result<multipart::Part, std::io::Error> {
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("flac") => "audio/flac",
        Some(extension) if extension.eq_ignore_ascii_case("webm") => "video/webm",
        _ => "application/octet-stream",
    };
    multipart::Part::file(path)
        .await?
        .mime_str(content_type)
        .map_err(std::io::Error::other)
}

fn query_failure(error_type: ErrorType, status_code: Option<u16>) -> QueryResult {
    QueryResult {
        segments: None,
        error_type: Some(error_type),
        status_code,
        legacy: false,
        truncated: false,
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

fn parse_listing(body: Value, status_code: u16) -> QueryResult {
    let (items, legacy, truncated) = match body {
        Value::Array(items) => (items, true, false),
        Value::Object(mut body) => {
            // Named deviation: Python can fail while taking len() of malformed
            // `items`; Rust treats non-array items as an empty listing.
            let items = body
                .remove("items")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            let total = body.remove("total").and_then(|value| value.as_i64());
            let truncated = total.is_some_and(|total| total != items.len() as i64);
            (items, false, truncated)
        }
        _ => (Vec::new(), false, false),
    };
    let segments = items
        .into_iter()
        .map(|item| {
            serde_json::from_value(item).unwrap_or(ListingEntry {
                key: None,
                original_key: None,
                files: None,
            })
        })
        .collect();
    QueryResult {
        segments: Some(segments),
        error_type: None,
        status_code: Some(status_code),
        legacy,
        truncated,
    }
}

#[cfg(test)]
pub(crate) fn contract_parse_listing(body: Value, status_code: u16) -> QueryResult {
    parse_listing(body, status_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{ConfigPaths, load_config},
        private_link_test_peer::PrivateLinkPeer,
        test_support::{Action, MockServer, MutableClock, wait_for_requests},
    };
    use tempfile::TempDir;
    use tokio::net::TcpListener;
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

    fn config(server: &MockServer, temp: &TempDir) -> Config {
        Config {
            server_url: server.url.clone(),
            key: "K".into(),
            base_dir: temp.path().join("data"),
            config_dir: temp.path().join("config"),
            ..Config::default()
        }
    }

    fn client(config: &Config) -> UploadClient {
        UploadClient::new(
            config,
            "host-a",
            "linux",
            "0.1.0",
            Arc::new(MutableClock::new(0.0, 0.0)),
        )
    }

    fn write_file(temp: &TempDir, name: &str, body: &[u8]) -> PathBuf {
        let path = temp.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
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

    // tests/test_upload.py::test_ensure_registered_posts_descriptor_and_persists
    #[tokio::test]
    async fn ensure_registered_posts_descriptor_and_persists() {
        let server =
            MockServer::new(vec![(200, json!({"key":"K123456789", "name":"fedora"}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.stream = "host-a".into();
        let client = client(&config);
        assert!(client.ensure_registered(&mut config).await);
        let request = &server.requests()[0];
        assert_eq!(request.uri, "/app/observer/register");
        assert!(request.headers.get("authorization").is_none());
        let descriptor: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(descriptor["label"], "host-a");
        assert_eq!(descriptor["hostname"], "host-a");
        assert_eq!(descriptor["platform"], "linux");
        assert_eq!(descriptor["version"], "0.1.0");
        assert_eq!(descriptor["stream_type"], "desktop");
        let loaded = load_config(ConfigPaths {
            base_dir: Some(config.base_dir.clone()),
            config_dir: Some(config.config_dir.clone()),
        });
        assert_eq!(loaded.config.key, "K123456789");
        assert_eq!(loaded.config.stream, "fedora");
    }

    #[tokio::test]
    async fn daemon_requests_remain_on_production_http_not_private_link() {
        let server = MockServer::new(vec![
            (200, json!({"key":"K123456789", "name":"host-a"})),
            (200, json!({"status":"ok", "key":"stored"})),
            (200, json!({"segments":[]})),
        ])
        .await;
        let peer = PrivateLinkPeer::start().await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.stream = "host-a".into();
        let client = client(&config);
        assert!(client.ensure_registered(&mut config).await);
        let capture = write_file(&temp, "capture.jsonl", b"{\"event\":1}\n");
        assert!(
            client
                .upload_segment("20260101", "120000", &[capture])
                .await
                .success
        );
        assert!(
            client
                .get_server_segments("20260101")
                .await
                .segments
                .is_some()
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            (requests[0].method.as_str(), requests[0].uri.as_str()),
            ("POST", "/app/observer/register")
        );
        assert_eq!(
            requests[0].headers.get("content-type").unwrap(),
            "application/json"
        );
        assert!(requests[0].headers.get("authorization").is_none());
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
            (requests[1].method.as_str(), requests[1].uri.as_str()),
            ("POST", "/app/observer/ingest")
        );
        assert_eq!(
            requests[1]
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer K123456789"
        );
        let content_type = requests[1]
            .headers
            .get("content-type")
            .unwrap()
            .to_str()
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
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts[0],
            (
                vec![("Content-Disposition", "form-data; name=\"day\"")],
                b"20260101".as_slice()
            )
        );
        assert_eq!(
            parts[1],
            (
                vec![("Content-Disposition", "form-data; name=\"segment\"")],
                b"120000".as_slice()
            )
        );
        assert_eq!(
            parts[2],
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
        assert_eq!(
            (requests[2].method.as_str(), requests[2].uri.as_str()),
            ("GET", "/app/observer/ingest/segments/20260101")
        );
        assert_eq!(
            requests[2]
                .headers
                .get(OBSERVER_PROTOCOL_VERSION_HEADER)
                .unwrap(),
            "2"
        );
        assert_eq!(
            requests[2]
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer K123456789"
        );
        assert!(peer.requests().is_empty());
        peer.shutdown().await;
    }

    // tests/test_upload.py::test_ensure_registered_skips_when_key_present
    // AC 18: constructor clock plumbing deliberately preserves the existing-key short circuit.
    #[tokio::test]
    async fn ensure_registered_skips_when_key_present() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        assert!(client(&config).ensure_registered(&mut config).await);
        assert!(server.requests().is_empty());
    }

    // AC 1/2: a stale-key 401 registers, persists a distinct replacement, and the next relay uses it.
    #[tokio::test]
    async fn stale_key_401_issues_register_request() {
        const STALE: &str = "STALE-KEY-111";
        const NEW: &str = "NEW-KEY-222";
        let server = MockServer::new(vec![
            (401, json!({})),
            (200, json!({"key":NEW,"name":"desktop-new"})),
            (200, json!({})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: STALE.into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        save_identity(
            &ConfigPaths {
                base_dir: Some(config.base_dir.clone()),
                config_dir: Some(config.config_dir.clone()),
            },
            STALE,
            "desktop",
        )
        .unwrap();
        let client = client(&config);

        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert_eq!(server.request_count("/app/observer/register"), 1);
        assert_eq!(client.inner.key.lock().unwrap().as_str(), NEW);
        assert_eq!(load_config(client.inner.paths.clone()).config.key, NEW);
        assert!(client.relay_event("observe", "status", Map::new()).await);
        let requests = server.requests();
        assert_eq!(
            requests[2]
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {NEW}")
        );
    }

    // AC 3/7: an idempotent same-key response is not recovery and still arms cooldown.
    #[tokio::test]
    async fn same_key_registration_is_not_recovery_and_arms_cooldown() {
        let server = MockServer::new(vec![
            (401, json!({})),
            (200, json!({"key":"STALE-KEY","name":"desktop"})),
            (401, json!({})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: "STALE-KEY".into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        let client = client(&config);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        async {
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert_eq!(client.recovery_generation(), 0);
            assert!(!client.relay_event("observe", "status", Map::new()).await);
        }
        .with_subscriber(subscriber)
        .await;
        assert_eq!(server.request_count("/app/observer/register"), 1);
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("outcome=\"same_key_returned\""),
            "{captured}"
        );
        assert!(!captured.contains("outcome=\"recovered\""), "{captured}");
    }

    // AC 6: the real sync-query/event race shares one in-flight recovery attempt.
    #[tokio::test]
    async fn sync_and_event_401_share_one_inflight_recovery() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let server = MockServer::new_actions(vec![
            Action::GatedResponse(401, json!({}), Arc::clone(&gate)),
            Action::GatedResponse(401, json!({}), Arc::clone(&gate)),
            Action::Response(200, json!({"key":"NEW-KEY","name":"desktop-new"})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: "STALE-KEY".into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        let client = Arc::new(client(&config));
        let listing = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.get_server_segments("20260101").await })
        };
        let event = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { client.relay_event("observe", "status", Map::new()).await })
        };
        wait_for_requests(&server, 2).await;
        tokio::task::yield_now().await;
        gate.notify_waiters();
        let (listing, event) = tokio::join!(listing, event);
        let listing = listing.unwrap();
        let event = event.unwrap();
        assert!(listing.segments.is_none());
        assert!(!event);
        assert_eq!(server.request_count("/app/observer/register"), 1);
    }

    // AC 7: a burst is bounded by cooldown and the next 300-second window starts fresh.
    #[tokio::test]
    async fn recovery_cooldown_bounds_register_requests_and_resets() {
        let mut responses = vec![
            (401, json!({})),
            (200, json!({"key":"STALE-KEY","name":"desktop"})),
        ];
        responses.extend((0..19).map(|_| (401, json!({}))));
        responses.extend([
            (401, json!({})),
            (200, json!({"key":"STALE-KEY","name":"desktop"})),
        ]);
        let server = MockServer::new(responses).await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: "STALE-KEY".into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        let clock = Arc::new(MutableClock::new(0.0, 0.0));
        let client = UploadClient::new(&config, "host", "linux", "test", clock.clone());
        for _ in 0..20 {
            assert!(!client.relay_event("observe", "status", Map::new()).await);
        }
        assert_eq!(server.request_count("/app/observer/register"), 1);
        clock.set_mono(300.0);
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert_eq!(server.request_count("/app/observer/register"), 2);
    }

    // AC 9: every failed register shape preserves disk, runtime, caller config, and stale bearer use.
    #[tokio::test]
    async fn failed_registration_preserves_all_stores_and_stale_bearer() {
        for failed in [
            Action::Disconnect,
            Action::Response(500, json!({})),
            Action::Response(200, json!({"name":"desktop"})),
        ] {
            let server = MockServer::new_actions(vec![
                Action::Response(401, json!({})),
                failed,
                Action::Response(200, json!({})),
            ])
            .await;
            let temp = TempDir::new().unwrap();
            let config = Config {
                key: "STALE-KEY".into(),
                stream: "desktop".into(),
                ..config(&server, &temp)
            };
            save_identity(
                &ConfigPaths {
                    base_dir: Some(config.base_dir.clone()),
                    config_dir: Some(config.config_dir.clone()),
                },
                &config.key,
                &config.stream,
            )
            .unwrap();
            let before = config.clone();
            let client = client(&config);

            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert_eq!(config, before);
            assert_eq!(client.inner.key.lock().unwrap().as_str(), "STALE-KEY");
            assert_eq!(
                load_config(client.inner.paths.clone()).config.key,
                "STALE-KEY"
            );
            assert!(client.relay_event("observe", "status", Map::new()).await);
            let requests = server.requests();
            assert_eq!(
                requests.last().unwrap().headers["authorization"],
                "Bearer STALE-KEY"
            );
        }
    }

    // AC 8: persistence after a successful register is synchronous and await-free, so
    // cancellation cannot be scheduled between the response and persistence. Cancellation before
    // the response leaves the exact stale pair; uninterrupted completion writes the exact new
    // pair. Those are therefore the only two identities reachable on disk.
    #[tokio::test]
    async fn cancellation_boundary_exposes_only_stale_or_new_disk_identity() {
        const STALE: &str = "STALE-KEY";
        const NEW: &str = "NEW-KEY";
        for cancel in [true, false] {
            let gate = Arc::new(tokio::sync::Notify::new());
            let server = MockServer::new_actions(vec![
                Action::Response(401, json!({})),
                Action::GatedResponse(
                    200,
                    json!({"key":NEW,"name":"desktop-new"}),
                    Arc::clone(&gate),
                ),
            ])
            .await;
            let temp = TempDir::new().unwrap();
            let config = Config {
                key: STALE.into(),
                stream: "desktop-old".into(),
                ..config(&server, &temp)
            };
            save_identity(
                &ConfigPaths {
                    base_dir: Some(config.base_dir.clone()),
                    config_dir: Some(config.config_dir.clone()),
                },
                STALE,
                "desktop-old",
            )
            .unwrap();
            let client = Arc::new(client(&config));
            let relay = {
                let client = Arc::clone(&client);
                tokio::spawn(
                    async move { client.relay_event("observe", "status", Map::new()).await },
                )
            };
            wait_for_requests(&server, 2).await;
            if cancel {
                client.request_stop();
            }
            gate.notify_one();
            assert!(!relay.await.unwrap());
            let saved = load_config(client.inner.paths.clone()).config;
            let pair = (saved.key.as_str(), saved.stream.as_str());
            assert!(
                pair == (STALE, "desktop-old") || pair == (NEW, "desktop-new"),
                "unexpected reachable identity {pair:?}"
            );
            assert_eq!(
                pair,
                if cancel {
                    (STALE, "desktop-old")
                } else {
                    (NEW, "desktop-new")
                }
            );
        }
    }

    // AC 10: already-green regression pin; failed registration keeps keyless relay offline.
    #[tokio::test]
    async fn failed_registration_keeps_empty_key_and_relay_offline() {
        let server = MockServer::new(vec![(500, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.sync_retry_delays = vec![0];
        let client = client(&config);
        assert!(!client.ensure_registered(&mut config).await);
        let before = server.requests().len();
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert_eq!(server.requests().len(), before);
        assert!(!client.is_registered());
    }

    // AC 15: twenty clean relays emit no rejection warning.
    #[tokio::test]
    async fn clean_relays_emit_no_rejection_warning() {
        let server = MockServer::new((0..20).map(|_| (200, json!({}))).collect()).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config);
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
        let _client = client(&config);
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

    // tests/test_upload.py::test_upload_segment_uses_bearer_and_keyless_route
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
            client(&config)
                .upload_segment("20260101", "120000_005", &files)
                .await
                .success
        );
        let request = &server.requests()[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri, "/app/observer/ingest");
        assert_eq!(request.headers["authorization"], "Bearer K");
        assert!(
            request
                .headers
                .get(OBSERVER_PROTOCOL_VERSION_HEADER)
                .is_none()
        );
        let body = String::from_utf8_lossy(&request.body);
        for expected in [
            "name=\"day\"",
            "20260101",
            "name=\"segment\"",
            "120000_005",
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
            let result = client(&config)
                .upload_segment("day", "segment", &[media])
                .await;
            assert!(result.success);
            assert_eq!(result.duplicate, duplicate);
            assert_eq!(result.stored_key.as_deref(), Some(key));
        }
    }

    async fn attempts_for(max_retries: i64) -> (usize, UploadResult) {
        let server = MockServer::new(vec![(500, json!({})), (500, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_max_retries = max_retries;
        config.sync_retry_delays = vec![0];
        let media = write_file(&temp, "audio.flac", b"audio");
        let result = client(&config)
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

    // AC: Client and Incompatible upload responses are not retried
    #[tokio::test]
    async fn upload_400_and_404_make_one_request() {
        for (status, expected) in [(400, ErrorType::Client), (404, ErrorType::Incompatible)] {
            let server = MockServer::new(vec![(status, json!({})), (200, json!({}))]).await;
            let temp = TempDir::new().unwrap();
            let mut config = config(&server, &temp);
            config.sync_max_retries = 10;
            config.sync_retry_delays = vec![0];
            let media = write_file(&temp, "audio.flac", b"audio");
            let result = client(&config).upload_segment("d", "s", &[media]).await;
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
        let client = client(&config);
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
        let client = client(&config);
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
            client(&config)
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
        let result = client(&config).upload_segment("d", "s", &[media]).await;
        assert!(result.success);
        assert_eq!(server.requests().len(), 2);

        let terminal_server = MockServer::new_actions(vec![Action::Raw(200, "not-json")]).await;
        let terminal_temp = TempDir::new().unwrap();
        let mut terminal_config = self::config(&terminal_server, &terminal_temp);
        terminal_config.sync_max_retries = 1;
        let terminal_media = write_file(&terminal_temp, "audio.flac", b"audio");
        let terminal_result = client(&terminal_config)
            .upload_segment("d", "s", &[terminal_media])
            .await;
        assert_eq!(terminal_result.error_type, Some(ErrorType::Transient));
    }

    // AC: connection-refused upload is classified Transient end to end
    #[tokio::test]
    async fn connection_refused_upload_is_transient() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = TempDir::new().unwrap();
        let config = Config {
            server_url: format!("http://{address}"),
            key: "K".into(),
            sync_max_retries: 1,
            ..Config::default()
        };
        let media = write_file(&temp, "audio.flac", b"audio");
        let result = client(&config).upload_segment("d", "s", &[media]).await;
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
            client(&config)
                .relay_event("observe", "status", fields)
                .await
        );
        let request = &server.requests()[0];
        assert_eq!(request.uri, "/app/observer/ingest/event");
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
        let client = client(&config(&server, &temp));
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert!(!client.is_revoked());
    }

    // tests/test_upload.py::test_get_server_segments_uses_bearer_and_keyless_route
    #[tokio::test]
    async fn listing_uses_bearer_protocol_and_keyless_route() {
        let server = MockServer::new(vec![(200, json!([]))]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .get_server_segments("20260101")
            .await;
        assert_eq!(result.segments, Some(vec![]));
        assert!(result.legacy);
        let request = &server.requests()[0];
        assert_eq!(request.uri, "/app/observer/ingest/segments/20260101");
        assert_eq!(request.headers["authorization"], "Bearer K");
        assert_eq!(request.headers[OBSERVER_PROTOCOL_VERSION_HEADER], "2");
    }

    // tests/test_upload.py::test_get_server_segments_parses_envelope
    // AC: absent total defaults to item count and absent file strings stay None
    #[tokio::test]
    async fn listing_parses_envelope_verbatim_with_absent_total() {
        let body =
            json!({"items":[{"key":"new", "original_key":"old", "files":[{"name":"a.flac"}]}]});
        let server = MockServer::new(vec![(200, body)]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .get_server_segments("day")
            .await;
        assert!(!result.legacy && !result.truncated);
        let entry = &result.segments.unwrap()[0];
        assert_eq!(entry.key.as_deref(), Some("new"));
        assert_eq!(entry.original_key.as_deref(), Some("old"));
        let file = &entry.files.as_ref().unwrap()[0];
        assert_eq!(file.name.as_deref(), Some("a.flac"));
        assert_eq!(file.submitted_name, None);
        assert_eq!(file.status, None);
        assert_eq!(file.sha256, None);
    }

    // AC: malformed listing fields become None without dropping entries
    #[tokio::test]
    async fn listing_lenient_fields_preserve_every_entry() {
        let server = MockServer::new(vec![(
            200,
            json!({"items":[
                {"key":"kept", "original_key":7, "files":[{"name":false}]},
                "unexpected-entry"
            ], "total":-1}),
        )])
        .await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .get_server_segments("d")
            .await;
        let entries = result.segments.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key.as_deref(), Some("kept"));
        assert_eq!(entries[0].original_key, None);
        assert_eq!(entries[0].files.as_ref().unwrap()[0].name, None);
        assert_eq!(entries[1].key, None);
        assert!(result.truncated);
    }

    // AC: malformed listing JSON is Transient rather than empty success
    #[tokio::test]
    async fn malformed_listing_json_is_transient() {
        let server = MockServer::new_actions(vec![Action::Raw(200, "not-json")]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .get_server_segments("d")
            .await;
        assert_eq!(result.segments, None);
        assert_eq!(result.error_type, Some(ErrorType::Transient));
        assert_eq!(result.status_code, None);
    }

    // tests/test_upload.py::test_get_server_segments_marks_truncated_envelope
    #[tokio::test]
    async fn listing_marks_truncated_envelope() {
        let server = MockServer::new(vec![(200, json!({"items":[{"key":"a"}], "total":2}))]).await;
        let temp = TempDir::new().unwrap();
        assert!(
            client(&config(&server, &temp))
                .get_server_segments("day")
                .await
                .truncated
        );
    }

    // tests/test_upload.py::test_get_server_segments_classifies_404_as_incompatible
    #[tokio::test]
    async fn listing_classifies_404_as_incompatible() {
        let server = MockServer::new(vec![(404, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .get_server_segments("day")
            .await;
        assert_eq!(result.error_type, Some(ErrorType::Incompatible));
        assert_eq!(result.status_code, Some(404));
    }

    // AC: all missing files fail without an error class or request
    #[tokio::test]
    async fn all_missing_files_make_no_request() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let result = client(&config(&server, &temp))
            .upload_segment("d", "s", &[temp.path().join("missing")])
            .await;
        assert_eq!(result.error_type, None);
        assert!(!result.success);
        assert!(server.requests().is_empty());
    }

    // AC: unregistered upload is Client, not Auth, and makes no request
    #[tokio::test]
    async fn unregistered_upload_is_client_without_request() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        let media = write_file(&temp, "a.flac", b"a");
        let result = client(&config).upload_segment("d", "s", &[media]).await;
        assert_eq!(result.error_type, Some(ErrorType::Client));
        assert!(server.requests().is_empty());
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
            let result = client(&config(&server, &temp))
                .upload_segment("d", "s", &[media])
                .await;
            assert_eq!(result.error_type, Some(ErrorType::Auth));
            assert_eq!(result.status_code, Some(status));
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let temp = TempDir::new().unwrap();
        let transport_config = Config {
            server_url: format!("http://{address}"),
            key: "K".into(),
            sync_max_retries: 1,
            ..Config::default()
        };
        let media = write_file(&temp, "a.flac", b"a");
        let result = client(&transport_config)
            .upload_segment("d", "s", &[media])
            .await;
        assert_eq!(result.error_type, Some(ErrorType::Transient));
        assert_eq!(result.status_code, None);
    }

    // AC: empty retry delays fall back and registration survives 500, network error, then 200
    #[tokio::test]
    async fn registration_fallback_retries_and_persists() {
        let server = MockServer::new_actions(vec![
            Action::Response(500, json!({})),
            Action::Disconnect,
            Action::Response(200, json!({"key":"new-key", "name":"fedora"})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.stream = "host-a".into();
        config.sync_retry_delays = vec![0, 0, 0];
        let client = client(&config);
        assert!(client.ensure_registered(&mut config).await);
        assert_eq!(server.requests().len(), 3);
        assert_eq!(config.key, "new-key");
        assert_eq!(config.stream, "fedora");

        let empty_server = MockServer::new(vec![(200, json!({"key":"k", "name":"n"}))]).await;
        let mut empty = self::config(&empty_server, &temp);
        empty.key.clear();
        empty.sync_retry_delays.clear();
        let empty_client = self::client(&empty);
        assert!(empty_client.ensure_registered(&mut empty).await);
        assert_eq!(empty_server.requests().len(), 1);
    }

    // AC: empty retry delays use the complete local fallback
    #[tokio::test]
    async fn empty_retry_delays_use_full_fallback() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays.clear();
        let client = client(&config);
        assert_eq!(client.inner.retry_delays, vec![5, 30, 120, 300]);
    }

    // AC: malformed registration JSON continues to the next attempt
    #[tokio::test]
    async fn malformed_registration_json_retries() {
        let server = MockServer::new_actions(vec![
            Action::Raw(200, "not-json"),
            Action::Response(200, json!({"key":"key", "name":"fedora"})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.sync_retry_delays = vec![0, 0];
        let client = client(&config);
        assert!(client.ensure_registered(&mut config).await);
        assert_eq!(server.requests().len(), 2);
    }

    // AC: failed registration persistence does not latch in-memory registration
    #[tokio::test]
    async fn registration_persistence_failure_can_retry() {
        let server = MockServer::new(vec![
            (200, json!({"key":"key", "name":"fedora"})),
            (200, json!({"key":"key", "name":"fedora"})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        config.config_dir = blocked.clone();
        let client = client(&config);
        assert!(!client.ensure_registered(&mut config).await);
        assert!(!client.is_registered());
        std::fs::remove_file(&blocked).unwrap();
        assert!(client.ensure_registered(&mut config).await);
        assert!(client.is_registered());
        assert_eq!(server.requests().len(), 2);
    }

    // AC: cancellation raised during backoff interrupts the active wait
    #[tokio::test]
    async fn cancellation_during_backoff_interrupts_wait() {
        let server = MockServer::new(vec![(500, json!({})), (200, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.sync_retry_delays = vec![30];
        let media = write_file(&temp, "a.flac", b"a");
        let client = Arc::new(client(&config));
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
        let server = MockServer::new(vec![(403, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config);
        match path {
            "upload" => {
                let media = write_file(&temp, "a.flac", b"a");
                assert!(!client.upload_segment("d", "s", &[media]).await.success);
                assert!(client.is_revoked());
            }
            "listing" => {
                assert!(client.get_server_segments("d").await.segments.is_none());
                assert!(client.is_revoked());
            }
            "event" => {
                assert!(!client.relay_event("observe", "status", Map::new()).await);
                assert!(client.is_revoked());
            }
            _ => unreachable!(),
        }
    }

    // AC 18: register-route guard refusal deliberately no longer latches revocation.
    #[tokio::test]
    async fn registration_403_does_not_latch_revoked() {
        let server =
            MockServer::new(vec![(403, json!({"reason_code":"local_request_only"}))]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        config.key.clear();
        let client = client(&config);
        assert!(!client.ensure_registered(&mut config).await);
        assert!(!client.is_revoked());
        assert!(config.key.is_empty());
    }

    // AC 5: register guard refusal preserves the stale identity; ingest 403 still revokes it.
    #[tokio::test]
    async fn register_guard_refusal_does_not_revoke_but_ingest_403_does() {
        let server = MockServer::new(vec![
            (401, json!({})),
            (403, json!({"reason_code":"local_request_only"})),
            (403, json!({})),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: "STALE-KEY".into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        save_identity(
            &ConfigPaths {
                base_dir: Some(config.base_dir.clone()),
                config_dir: Some(config.config_dir.clone()),
            },
            &config.key,
            &config.stream,
        )
        .unwrap();
        let client = client(&config);
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
            assert_eq!(client.inner.key.lock().unwrap().as_str(), "STALE-KEY");
            let saved = load_config(client.inner.paths.clone()).config;
            assert_eq!(saved.key, "STALE-KEY");
            assert_eq!(saved.stream, "desktop");
            assert!(!client.relay_event("observe", "status", Map::new()).await);
            assert!(client.is_revoked());
        }
        .with_subscriber(subscriber)
        .await;
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(captured.contains("Journal refused local identity repair"));
        assert!(captured.contains("local_request_only"));
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

    // AC: revoked upload, listing, and event relay preflight make zero requests
    #[tokio::test]
    async fn revoked_preflight_makes_zero_requests() {
        let server = MockServer::new(vec![(403, json!({}))]).await;
        let temp = TempDir::new().unwrap();
        let config = config(&server, &temp);
        let client = client(&config);
        assert!(!client.relay_event("observe", "status", Map::new()).await);
        assert!(client.is_revoked());
        let before = server.requests().len();
        let media = write_file(&temp, "a.flac", b"a");
        assert_eq!(
            client.upload_segment("d", "s", &[media]).await.error_type,
            Some(ErrorType::Auth)
        );
        assert_eq!(
            client.get_server_segments("d").await.error_type,
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
        let mut client = UploadClient::with_silent_capacity(
            &config(&server, &temp),
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
        let mut client = client(&config(&server, &temp));
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
        let mut client = UploadClient::with_silent_capacity(
            &config(&server, &temp),
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
        let gate = Arc::new(tokio::sync::Notify::new());
        let server = MockServer::new_actions(vec![
            Action::Response(401, json!({})),
            Action::GatedResponse(500, json!({}), Arc::clone(&gate)),
        ])
        .await;
        let temp = TempDir::new().unwrap();
        let config = Config {
            key: "STALE-KEY".into(),
            stream: "desktop".into(),
            ..config(&server, &temp)
        };
        let mut client = client(&config);
        client.enqueue_status(Map::new());
        wait_for_requests(&server, 2).await;
        assert_eq!(server.request_count("/app/observer/register"), 1);
        for sequence in 0..20 {
            assert!(
                client
                    .enqueue_stream_silent(Map::from_iter([("sequence".into(), json!(sequence),)]))
            );
        }
        let started = tokio::time::Instant::now();
        gate.notify_one();
        assert_eq!(client.stop(Duration::from_secs(1)).await, 0);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.request_count("/app/observer/register"), 1);
    }
}
