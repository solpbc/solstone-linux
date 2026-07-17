// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::{Config, save_config},
    event_sender::{EventSender, SILENT_QUEUE_MAX},
    sync_health::ErrorType,
};
use reqwest::{Client, StatusCode, multipart};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadResult {
    pub success: bool,
    pub duplicate: bool,
    pub error_type: Option<ErrorType>,
    pub stored_key: Option<String>,
}

impl UploadResult {
    fn failure(error_type: Option<ErrorType>) -> Self {
        Self {
            success: false,
            duplicate: false,
            error_type,
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
    ) -> Self {
        Self::with_silent_capacity(config, hostname, platform, version, SILENT_QUEUE_MAX)
    }

    fn with_silent_capacity(
        config: &Config,
        hostname: impl Into<String>,
        platform: impl Into<String>,
        version: impl Into<String>,
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
        let stream = self.inner.stream.lock().unwrap().clone();
        let mut descriptor = json!({
            "platform": self.inner.platform,
            "hostname": self.inner.hostname,
            "stream_type": STREAM_TYPE,
            "version": self.inner.version,
        });
        if !stream.is_empty() {
            descriptor["label"] = Value::String(stream);
        }
        let attempts = 3.min(self.inner.retry_delays.len());
        let url = format!("{}/app/observer/register", self.inner.url);
        for attempt in 0..attempts {
            let response = self
                .inner
                .client
                .post(&url)
                .json(&descriptor)
                .timeout(EVENT_TIMEOUT)
                .send()
                .await;
            match response {
                Ok(response) if response.status() == StatusCode::OK => {
                    let body = match response.json::<Value>().await {
                        Ok(body) => body,
                        Err(error) => {
                            tracing::warn!(
                                attempt = attempt + 1,
                                %error,
                                "Registration attempt returned malformed JSON"
                            );
                            if attempt + 1 < attempts {
                                tokio::time::sleep(retry_delay(&self.inner.retry_delays, attempt))
                                    .await;
                            }
                            continue;
                        }
                    };
                    let (Some(key), Some(name)) = (
                        body.get("key").and_then(Value::as_str),
                        body.get("name").and_then(Value::as_str),
                    ) else {
                        // Named deviation: Python raises KeyError for a malformed
                        // success body; Rust reports registration failure without panicking.
                        return false;
                    };
                    config.key = key.to_owned();
                    config.stream = name.to_owned();
                    if let Err(error) = save_config(config) {
                        // Named deviation: Python propagates the persistence error;
                        // Rust leaves the client unregistered so a later call can retry.
                        tracing::error!(%error, "Failed to persist observer registration");
                        return false;
                    }
                    *self.inner.key.lock().unwrap() = key.to_owned();
                    *self.inner.stream.lock().unwrap() = name.to_owned();
                    tracing::info!(name, "Registered observer");
                    return true;
                }
                Ok(response) if response.status() == StatusCode::FORBIDDEN => {
                    self.inner.revoked.store(true, Ordering::Release);
                    tracing::error!("Registration rejected (403)");
                    return false;
                }
                Ok(response) => tracing::warn!(
                    attempt = attempt + 1,
                    status = %response.status(),
                    "Registration attempt failed"
                ),
                Err(error) => {
                    tracing::warn!(attempt = attempt + 1, %error, "Registration attempt failed")
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
            return UploadResult::failure(Some(ErrorType::Auth));
        }
        let key = self.inner.key.lock().unwrap().clone();
        if key.is_empty() || self.inner.url.is_empty() {
            return UploadResult::failure(Some(ErrorType::Client));
        }
        if !files.iter().any(|path| path.exists()) {
            return UploadResult::failure(None);
        }

        let url = format!("{}/app/observer/ingest", self.inner.url);
        let mut last_error = None;
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
                return UploadResult::failure(None);
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
                            let duplicate =
                                body.get("status").and_then(Value::as_str) == Some("duplicate");
                            let stored_key = body
                                .get(if duplicate {
                                    "existing_segment"
                                } else {
                                    "segment"
                                })
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            return UploadResult {
                                success: true,
                                duplicate,
                                error_type: None,
                                stored_key,
                            };
                        }
                        Err(error) => {
                            tracing::warn!(
                                attempt = attempt + 1,
                                %error,
                                "Upload attempt returned malformed JSON"
                            );
                            last_error = Some(ErrorType::Transient);
                        }
                    }
                }
                Ok(response) => {
                    let status = response.status();
                    let error_type = Self::classify_error(Some(status.as_u16()), false);
                    last_error = Some(error_type);
                    if status == StatusCode::FORBIDDEN {
                        self.inner.revoked.store(true, Ordering::Release);
                    }
                    if error_type != ErrorType::Transient {
                        tracing::error!(%status, ?error_type, "Upload rejected");
                        return UploadResult::failure(Some(error_type));
                    }
                    tracing::warn!(attempt = attempt + 1, %status, "Upload attempt failed");
                }
                Err(error) => {
                    tracing::warn!(attempt = attempt + 1, %error, "Upload attempt failed");
                    last_error = Some(ErrorType::Transient);
                }
            }
            if attempt + 1 < self.inner.immediate_attempts {
                tokio::select! {
                    () = tokio::time::sleep(retry_delay(&self.inner.retry_delays, attempt)) => {}
                    () = self.inner.cancellation.cancelled() => {
                        return UploadResult::failure(Some(ErrorType::Transient));
                    }
                }
            }
        }
        UploadResult::failure(last_error)
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
                }
                false
            }
            Err(_) => false,
        }
    }
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
mod tests {
    use super::*;
    use crate::{
        config::{ConfigPaths, load_config},
        test_support::{Action, MockServer, wait_for_requests},
    };
    use tempfile::TempDir;
    use tokio::net::TcpListener;

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
        UploadClient::new(config, "host-a", "linux", "0.1.0")
    }

    fn write_file(temp: &TempDir, name: &str, body: &[u8]) -> PathBuf {
        let path = temp.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
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

    // tests/test_upload.py::test_ensure_registered_skips_when_key_present
    #[tokio::test]
    async fn ensure_registered_skips_when_key_present() {
        let server = MockServer::new(vec![]).await;
        let temp = TempDir::new().unwrap();
        let mut config = config(&server, &temp);
        assert!(client(&config).ensure_registered(&mut config).await);
        assert!(server.requests().is_empty());
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
        let mut config = config(&server, &temp);
        let client = client(&config);
        match path {
            "registration" => {
                config.key.clear();
                let client = self::client(&config);
                assert!(!client.ensure_registered(&mut config).await);
                assert!(client.is_revoked());
            }
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

    // AC: registration 403 latches revoked
    #[tokio::test]
    async fn registration_403_latches_revoked() {
        assert_403_latches("registration").await;
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
        let mut client =
            UploadClient::with_silent_capacity(&config(&server, &temp), "host", "linux", "v", 1);
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
        let mut client =
            UploadClient::with_silent_capacity(&config(&server, &temp), "host", "linux", "v", 1);
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
}
