// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! OAuth browser-callback brokerage for the Linux journal viewer.
//!
//! Runs inside the unconfined desktop daemon over the session D-Bus
//! (`org.solpbc.solstone.Observer1`) so the AppArmor-confined WebKitGTK journal
//! viewer never binds a socket or launches an unconfined browser directly for
//! OAuth. The daemon holds zero OAuth tokens or PKCE verifiers and never logs
//! `callback_url`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, Semaphore, oneshot};

pub const MAX_ID_BYTES: usize = 128;
pub const MAX_PREFERENCES: usize = 16;
pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_AUTHORIZE_URL_BYTES: usize = 8_192;
pub const MAX_STATE_BYTES: usize = 1_024;
pub const MAX_PAYLOAD_BYTES: usize = 65_536;
pub const MAX_REQUEST_HEADER_BYTES: usize = 8_192;
pub const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(5);
pub const MIN_TIMEOUT_SECONDS: f64 = 1.0;
pub const MAX_TIMEOUT_SECONDS: f64 = 600.0;
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 600.0;
pub const SUCCESS_BODY: &str = "signed in. you can close this tab and go back to your journal";
const MAX_CONCURRENT_CALLBACK_HANDLERS: usize = 16;

/// Frozen v2 OAuth brokerage contract shipped beside the journal viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContract {
    pub version: u64,
    pub capability: String,
    pub prepare: String,
    pub prepared: String,
    pub authorize: String,
    pub cancel: String,
    pub result: String,
    pub unsupported: String,
}

pub fn bundled_auth_contract_bytes() -> &'static [u8] {
    include_bytes!("../../../journal-viewer/auth-host-contract.json")
}

pub fn parse_auth_contract(bytes: &[u8]) -> Result<AuthContract, &'static str> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "malformed")?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or("malformed")
    };
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or("malformed")?;
    if version != 2 {
        return Err("unsupported");
    }
    Ok(AuthContract {
        version,
        capability: text("capability")?,
        prepare: text("prepare")?,
        prepared: text("prepared")?,
        authorize: text("authorize")?,
        cancel: text("cancel")?,
        result: text("result")?,
        unsupported: text("unsupported")?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthPreference {
    Loopback {
        host: String,
        path: String,
        port: u16,
    },
    CustomScheme {
        uri: String,
    },
    Other {
        mode: String,
    },
}

pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

pub fn is_valid_loopback_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.contains('?')
        && !path.contains('#')
        && !path.contains("..")
        && path.bytes().all(|b| (0x21..=0x7E).contains(&b))
}

pub fn parse_preferences(raw: Option<&Value>) -> Result<Vec<AuthPreference>, &'static str> {
    let Some(list) = raw.and_then(Value::as_array) else {
        return Err("invalid_preferences");
    };
    if list.is_empty() || list.len() > MAX_PREFERENCES {
        return Err("invalid_preferences");
    }
    let mut parsed = Vec::with_capacity(list.len());
    for item in list {
        let Some(object) = item.as_object() else {
            return Err("invalid_preferences");
        };
        let Some(mode) = object
            .get("mode")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Err("invalid_preferences");
        };
        match mode {
            "loopback" => {
                let Some(host) = object.get("host").and_then(Value::as_str) else {
                    return Err("invalid_loopback_host");
                };
                if host != "127.0.0.1" && host != "localhost" {
                    return Err("invalid_loopback_host");
                }
                let Some(path) = object.get("path").and_then(Value::as_str) else {
                    return Err("invalid_loopback_path");
                };
                if !is_valid_loopback_path(path) {
                    return Err("invalid_loopback_path");
                }
                let port = match object.get("port") {
                    None | Some(Value::Null) => 0_u16,
                    Some(Value::Number(n)) => {
                        let Some(val) = n.as_u64() else {
                            return Err("invalid_loopback_port");
                        };
                        if val == 0 {
                            0
                        } else if (1024..=65535).contains(&val) {
                            val as u16
                        } else {
                            return Err("invalid_loopback_port");
                        }
                    }
                    _ => return Err("invalid_loopback_port"),
                };
                parsed.push(AuthPreference::Loopback {
                    host: host.to_owned(),
                    path: path.to_owned(),
                    port,
                });
            }
            "custom_scheme" => {
                let Some(uri) = object
                    .get("uri")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    return Err("invalid_custom_scheme");
                };
                parsed.push(AuthPreference::CustomScheme {
                    uri: uri.to_owned(),
                });
            }
            other => parsed.push(AuthPreference::Other {
                mode: other.to_owned(),
            }),
        }
    }
    Ok(parsed)
}

/// Strict validation of `authorize_url`: `https://` only, <= 8192 bytes, no
/// userinfo (`@`), no whitespace or ASCII control characters, valid non-empty
/// host and optional port.
pub fn validate_authorize_url(raw: Option<&Value>) -> Option<String> {
    let text = raw.and_then(Value::as_str)?;
    if text.is_empty() || text.len() > MAX_AUTHORIZE_URL_BYTES {
        return None;
    }
    if text
        .chars()
        .any(|c| c.is_whitespace() || (c as u32) <= 0x20 || (c as u32) == 0x7F)
    {
        return None;
    }
    if text.len() < 8 || !text[..8].eq_ignore_ascii_case("https://") {
        return None;
    }
    let after_scheme = &text[8..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return None;
    }
    let (host, port_opt) = if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let ipv6 = &rest[..close];
        if ipv6.is_empty()
            || !ipv6
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        {
            return None;
        }
        let after_bracket = &rest[close + 1..];
        if after_bracket.is_empty() {
            (ipv6, None)
        } else {
            let port_str = after_bracket.strip_prefix(':')?;
            (ipv6, Some(port_str))
        }
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        if h.contains(':') {
            return None;
        }
        (h, Some(p))
    } else {
        (authority, None)
    };
    if host.is_empty() {
        return None;
    }
    if !authority.starts_with('[')
        && !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    {
        return None;
    }
    if let Some(port_str) = port_opt {
        if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let port: u16 = port_str.parse().ok()?;
        if port == 0 {
            return None;
        }
    }
    Some(text.to_owned())
}

pub fn validate_expected_state(raw: Option<&Value>) -> Option<String> {
    let state = raw.and_then(Value::as_str)?;
    if state.is_empty()
        || state.len() > MAX_STATE_BYTES
        || !state.bytes().all(|b| (0x21..=0x7E).contains(&b))
    {
        return None;
    }
    Some(state.to_owned())
}

pub fn parse_timeout_seconds(raw: Option<&Value>) -> Result<Duration, &'static str> {
    match raw {
        None | Some(Value::Null) => Ok(Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS)),
        Some(Value::Number(n)) => {
            let Some(value) = n.as_f64() else {
                return Err("invalid_timeout");
            };
            if !value.is_finite() {
                return Err("invalid_timeout");
            }
            let clamped = value.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);
            Ok(Duration::from_secs_f64(clamped))
        }
        _ => Err("invalid_timeout"),
    }
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = hex_nibble(bytes[i + 1])?;
                let lo = hex_nibble(bytes[i + 2])?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

pub fn parse_query_refusing_duplicates(query: &str) -> Option<HashMap<String, String>> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = percent_decode(raw_key)?;
        let value = percent_decode(raw_value)?;
        if params.insert(key, value).is_some() {
            return None;
        }
    }
    Some(params)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallbackValidation {
    Accepted { callback_url: String },
    Rejected { status: u16 },
}

pub fn validate_http_callback(
    raw_headers: &[u8],
    expected_host: &str,
    bound_port: u16,
    expected_path: &str,
    expected_state: Option<&str>,
) -> CallbackValidation {
    if raw_headers.is_empty() || raw_headers.len() > MAX_REQUEST_HEADER_BYTES {
        return CallbackValidation::Rejected { status: 400 };
    }
    let Some(header_end) = find_header_end(raw_headers) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Ok(header_text) = std::str::from_utf8(&raw_headers[..header_end]) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let parts: Vec<&str> = request_line.split(' ').collect();
    if parts.len() != 3 {
        return CallbackValidation::Rejected { status: 400 };
    }
    let (method, target, version) = (parts[0], parts[1], parts[2]);
    if method != "GET" || version != "HTTP/1.1" {
        return CallbackValidation::Rejected { status: 400 };
    }

    let expected_host_header = format!("{expected_host}:{bound_port}");
    let mut saw_host: Option<&str> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return CallbackValidation::Rejected { status: 400 };
        }
        let Some((name, raw_val)) = line.split_once(':') else {
            return CallbackValidation::Rejected { status: 400 };
        };
        if name.is_empty() || name.bytes().any(|b| b <= 0x20 || b >= 0x7F) {
            return CallbackValidation::Rejected { status: 400 };
        }
        if name.eq_ignore_ascii_case("host") {
            if saw_host.is_some() {
                return CallbackValidation::Rejected { status: 400 };
            }
            saw_host = Some(raw_val.trim_matches(|c| c == ' ' || c == '\t'));
        }
    }
    let Some(host_val) = saw_host else {
        return CallbackValidation::Rejected { status: 400 };
    };
    if !host_val.eq_ignore_ascii_case(&expected_host_header) {
        return CallbackValidation::Rejected { status: 400 };
    }

    if !target.starts_with('/')
        || target.contains('#')
        || target.bytes().any(|b| b <= 0x20 || b == 0x7F)
    {
        return CallbackValidation::Rejected { status: 400 };
    }
    let (req_path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    if req_path != expected_path {
        return CallbackValidation::Rejected { status: 404 };
    }
    let Some(expected_state) = expected_state else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(raw_query) = raw_query.filter(|q| !q.is_empty()) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(params) = parse_query_refusing_duplicates(raw_query) else {
        return CallbackValidation::Rejected { status: 400 };
    };
    let Some(state) = params.get("state") else {
        return CallbackValidation::Rejected { status: 400 };
    };
    if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
        return CallbackValidation::Rejected { status: 400 };
    }
    if !params.contains_key("code") && !params.contains_key("error") {
        return CallbackValidation::Rejected { status: 400 };
    }

    CallbackValidation::Accepted {
        callback_url: format!("http://{expected_host}:{bound_port}{req_path}?{raw_query}"),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionOutcome {
    Callback(String),
    Cancelled,
    Timeout,
    Error(&'static str),
}

struct SessionState {
    authorizing: bool,
    expected_state: Option<String>,
    outcome: Option<SessionOutcome>,
}

struct Session {
    session_id: String,
    owner: String,
    host: String,
    path: String,
    bound_port: u16,
    state: Mutex<SessionState>,
    outcome_notify: Notify,
    stop_notify: Notify,
    listener_dropped: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Session {
    async fn settle(&self, outcome: SessionOutcome) -> bool {
        let mut guard = self.state.lock().await;
        if guard.outcome.is_some() {
            return false;
        }
        guard.outcome = Some(outcome);
        drop(guard);
        self.outcome_notify.notify_waiters();
        true
    }

    async fn stop_listener(&self) {
        self.stop_notify.notify_waiters();
        let rx = self.listener_dropped.lock().await.take();
        if let Some(rx) = rx {
            let _ = tokio::time::timeout(Duration::from_secs(2), rx).await;
        }
    }
}

type OpenUrlFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

struct Inner {
    contract: AuthContract,
    open_url: OpenUrlFn,
    idle_timeout: Duration,
    active: Mutex<Option<Arc<Session>>>,
}

/// Shareable handle to the daemon's single-session OAuth loopback broker.
#[derive(Clone)]
pub struct AuthHandle {
    inner: Arc<Inner>,
}

impl Default for AuthHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthHandle {
    pub fn new() -> Self {
        Self::with_opener(
            |url| crate::desktop_component::open_detached(url),
            Duration::from_secs_f64(DEFAULT_TIMEOUT_SECONDS),
        )
    }

    pub fn with_opener(
        open_url: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static,
        idle_timeout: Duration,
    ) -> Self {
        let contract = parse_auth_contract(bundled_auth_contract_bytes())
            .expect("bundled auth-host-contract.json must be valid v2");
        Self {
            inner: Arc::new(Inner {
                contract,
                open_url: Arc::new(open_url),
                idle_timeout,
                active: Mutex::new(None),
            }),
        }
    }

    fn parse_message(&self, payload: &str) -> Result<(String, Map<String, Value>), String> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(json!({"type": self.inner.contract.unsupported, "id": ""}).to_string());
        }
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(payload) else {
            return Err(json!({"type": self.inner.contract.unsupported, "id": ""}).to_string());
        };
        let Some(id) = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_valid_id(id))
            .map(str::to_owned)
        else {
            return Err(json!({"type": self.inner.contract.unsupported, "id": ""}).to_string());
        };
        Ok((id, obj))
    }

    async fn clear_if_same(&self, session: &Arc<Session>) {
        let mut active = self.inner.active.lock().await;
        if active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            *active = None;
        }
    }

    /// Tears down any active session owned by `owner` (used when the viewer's
    /// D-Bus unique name disappears).
    pub async fn cancel_owner(&self, owner: &str) {
        let taken = {
            let mut active = self.inner.active.lock().await;
            if active.as_ref().is_some_and(|s| s.owner == owner) {
                active.take()
            } else {
                None
            }
        };
        if let Some(session) = taken {
            session.settle(SessionOutcome::Cancelled).await;
            session.stop_listener().await;
        }
    }

    /// Tears down any active session unconditionally.
    pub async fn tear_down(&self) {
        let taken = self.inner.active.lock().await.take();
        if let Some(session) = taken {
            session.settle(SessionOutcome::Cancelled).await;
            session.stop_listener().await;
        }
    }

    pub async fn prepare(&self, owner: &str, payload: &str) -> (String, Option<String>) {
        let (id, raw) = match self.parse_message(payload) {
            Ok(parsed) => parsed,
            Err(reply) => return (reply, None),
        };
        let preferences = match parse_preferences(raw.get("preferences")) {
            Ok(prefs) => prefs,
            Err(reason) => {
                return (
                    json!({
                        "type": self.inner.contract.prepared,
                        "id": id,
                        "outcome": "refused",
                        "reason": reason,
                    })
                    .to_string(),
                    None,
                );
            }
        };

        // Single active session: tear down any prior session before binding so
        // an earlier `auth.authorize` settles as `cancelled` and any fixed port
        // is freed before re-binding.
        self.tear_down().await;

        let mut saw_unavailable = false;
        for pref in preferences {
            let AuthPreference::Loopback { host, path, port } = pref else {
                continue;
            };
            let listener = match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => listener,
                Err(_) => {
                    saw_unavailable = true;
                    continue;
                }
            };
            let Ok(bound_addr) = listener.local_addr() else {
                saw_unavailable = true;
                continue;
            };
            let bound_port = bound_addr.port();
            let session_id = random_session_id();
            let redirect_uri = format!("http://{host}:{bound_port}{path}");
            let (drop_tx, drop_rx) = oneshot::channel();
            let session = Arc::new(Session {
                session_id: session_id.clone(),
                owner: owner.to_owned(),
                host,
                path,
                bound_port,
                state: Mutex::new(SessionState {
                    authorizing: false,
                    expected_state: None,
                    outcome: None,
                }),
                outcome_notify: Notify::new(),
                stop_notify: Notify::new(),
                listener_dropped: Mutex::new(Some(drop_rx)),
            });

            let accept_session = Arc::clone(&session);
            tokio::spawn(async move {
                run_accept_loop(listener, accept_session, drop_tx).await;
            });

            *self.inner.active.lock().await = Some(Arc::clone(&session));

            // Idle watchdog: closes the prepared listener if `auth.authorize`
            // never arrives within `idle_timeout`.
            let handle = self.clone();
            let idle_session = Arc::clone(&session);
            let idle_timeout = self.inner.idle_timeout;
            tokio::spawn(async move {
                let notified = idle_session.outcome_notify.notified();
                tokio::pin!(notified);
                let sleep = tokio::time::sleep(idle_timeout);
                tokio::pin!(sleep);
                loop {
                    {
                        let guard = idle_session.state.lock().await;
                        if guard.authorizing || guard.outcome.is_some() {
                            return;
                        }
                    }
                    tokio::select! {
                        () = &mut notified => {
                            // Re-check loop condition; exit if authorizing or settled.
                            notified.set(idle_session.outcome_notify.notified());
                        }
                        () = &mut sleep => {
                            let mut guard = idle_session.state.lock().await;
                            if !guard.authorizing && guard.outcome.is_none() {
                                guard.outcome = Some(SessionOutcome::Timeout);
                                drop(guard);
                                idle_session.outcome_notify.notify_waiters();
                                handle.clear_if_same(&idle_session).await;
                                idle_session.stop_listener().await;
                            }
                            return;
                        }
                    }
                }
            });

            return (
                json!({
                    "type": self.inner.contract.prepared,
                    "id": id,
                    "outcome": "ready",
                    "session_id": session_id,
                    "mode": "loopback",
                    "redirect_uri": redirect_uri,
                })
                .to_string(),
                Some(session_id),
            );
        }

        if saw_unavailable {
            (
                json!({
                    "type": self.inner.contract.prepared,
                    "id": id,
                    "outcome": "unavailable",
                    "reason": "port_unavailable",
                })
                .to_string(),
                None,
            )
        } else {
            (
                json!({
                    "type": self.inner.contract.prepared,
                    "id": id,
                    "outcome": "no_compatible_mode",
                })
                .to_string(),
                None,
            )
        }
    }

    pub async fn authorize(&self, owner: &str, payload: &str) -> String {
        let (id, raw) = match self.parse_message(payload) {
            Ok(parsed) => parsed,
            Err(reply) => return reply,
        };
        let refused = |reason: &str| {
            json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "refused",
                "reason": reason,
            })
            .to_string()
        };
        let Some(session_id) = raw
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return refused("invalid_session");
        };
        let Some(authorize_url) = validate_authorize_url(raw.get("authorize_url")) else {
            return refused("invalid_authorize_url");
        };
        let Some(expected_state) = validate_expected_state(raw.get("expected_state")) else {
            return refused("invalid_state");
        };
        let timeout = match parse_timeout_seconds(raw.get("timeout_seconds")) {
            Ok(d) => d,
            Err(reason) => return refused(reason),
        };

        let session = {
            let active = self.inner.active.lock().await;
            let Some(current) = active.as_ref() else {
                return refused("invalid_session");
            };
            if current.session_id != session_id || current.owner != owner {
                return refused("invalid_session");
            }
            let mut state = current.state.lock().await;
            if state.authorizing || state.outcome.is_some() {
                return refused("invalid_session");
            }
            state.authorizing = true;
            state.expected_state = Some(expected_state);
            drop(state);
            current.outcome_notify.notify_waiters();
            Arc::clone(current)
        };

        if (self.inner.open_url)(&authorize_url).is_err() {
            session
                .settle(SessionOutcome::Error("browser_open_failed"))
                .await;
            self.clear_if_same(&session).await;
            session.stop_listener().await;
            return json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "error",
                "reason": "browser_open_failed",
            })
            .to_string();
        }

        let outcome = {
            let sleep = tokio::time::sleep(timeout);
            tokio::pin!(sleep);
            loop {
                let notified = session.outcome_notify.notified();
                {
                    let guard = session.state.lock().await;
                    if let Some(outcome) = guard.outcome.clone() {
                        break outcome;
                    }
                }
                tokio::select! {
                    () = notified => {}
                    () = &mut sleep => {
                        let mut guard = session.state.lock().await;
                        if let Some(outcome) = guard.outcome.clone() {
                            break outcome;
                        }
                        guard.outcome = Some(SessionOutcome::Timeout);
                        drop(guard);
                        session.outcome_notify.notify_waiters();
                        break SessionOutcome::Timeout;
                    }
                }
            }
        };

        self.clear_if_same(&session).await;
        session.stop_listener().await;

        match outcome {
            SessionOutcome::Callback(callback_url) => json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "callback",
                "callback_url": callback_url,
            })
            .to_string(),
            SessionOutcome::Cancelled => json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "cancelled",
            })
            .to_string(),
            SessionOutcome::Timeout => json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "timeout",
            })
            .to_string(),
            SessionOutcome::Error(reason) => json!({
                "type": self.inner.contract.result,
                "id": id,
                "outcome": "error",
                "reason": reason,
            })
            .to_string(),
        }
    }

    pub async fn cancel(&self, owner: &str, payload: &str) -> String {
        let (id, raw) = match self.parse_message(payload) {
            Ok(parsed) => parsed,
            Err(reply) => return reply,
        };
        let target_session = match raw.get("session_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => {
                return json!({
                    "type": self.inner.contract.result,
                    "id": id,
                    "outcome": "cancelled",
                })
                .to_string();
            }
        };

        let cancelled = {
            let mut active = self.inner.active.lock().await;
            let should_cancel = match (active.as_ref(), target_session) {
                (Some(current), None) => current.owner == owner,
                (Some(current), Some(target)) => {
                    current.owner == owner && current.session_id == target
                }
                (None, _) => false,
            };
            if should_cancel { active.take() } else { None }
        };

        if let Some(session) = cancelled {
            session.settle(SessionOutcome::Cancelled).await;
            session.stop_listener().await;
        }

        json!({
            "type": self.inner.contract.result,
            "id": id,
            "outcome": "cancelled",
        })
        .to_string()
    }
}

fn random_session_id() -> String {
    let mut bytes = [0_u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !filled {
        static FALLBACK: AtomicU64 = AtomicU64::new(1);
        let seq = FALLBACK.fetch_add(1, Ordering::Relaxed).to_be_bytes();
        let pid = std::process::id().to_be_bytes();
        bytes[..8].copy_from_slice(&seq);
        bytes[8..12].copy_from_slice(&pid);
    }
    let mut hex = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{b:02x}");
    }
    hex
}

async fn run_accept_loop(
    listener: TcpListener,
    session: Arc<Session>,
    drop_tx: oneshot::Sender<()>,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CALLBACK_HANDLERS));
    loop {
        tokio::select! {
            biased;
            () = session.stop_notify.notified() => break,
            accepted = listener.accept() => {
                let Ok((mut stream, _peer)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    let _ = tokio::time::timeout(
                        SOCKET_IO_TIMEOUT,
                        write_http_response(&mut stream, 400, "Bad Request", "invalid request"),
                    ).await;
                    continue;
                };
                let handler_session = Arc::clone(&session);
                tokio::spawn(async move {
                    let _permit = permit;
                    handle_connection(stream, &handler_session).await;
                });
            }
        }
    }
    drop(listener);
    let _ = drop_tx.send(());
}

async fn handle_connection(mut stream: TcpStream, session: &Session) {
    let raw_headers = match tokio::time::timeout(
        SOCKET_IO_TIMEOUT,
        read_request_headers(&mut stream, MAX_REQUEST_HEADER_BYTES),
    )
    .await
    {
        Ok(Some(headers)) => headers,
        _ => {
            let _ = tokio::time::timeout(
                SOCKET_IO_TIMEOUT,
                write_http_response(&mut stream, 400, "Bad Request", "invalid request"),
            )
            .await;
            return;
        }
    };

    let expected_state = {
        let guard = session.state.lock().await;
        if guard.outcome.is_some() {
            None
        } else {
            guard.expected_state.clone()
        }
    };

    match validate_http_callback(
        &raw_headers,
        &session.host,
        session.bound_port,
        &session.path,
        expected_state.as_deref(),
    ) {
        CallbackValidation::Accepted { callback_url } => {
            if session.settle(SessionOutcome::Callback(callback_url)).await {
                let _ = tokio::time::timeout(
                    SOCKET_IO_TIMEOUT,
                    write_http_response(&mut stream, 200, "OK", SUCCESS_BODY),
                )
                .await;
                session.stop_notify.notify_waiters();
            } else {
                let _ = tokio::time::timeout(
                    SOCKET_IO_TIMEOUT,
                    write_http_response(&mut stream, 400, "Bad Request", "session closed"),
                )
                .await;
            }
        }
        CallbackValidation::Rejected { status: 404 } => {
            let _ = tokio::time::timeout(
                SOCKET_IO_TIMEOUT,
                write_http_response(&mut stream, 404, "Not Found", "not found"),
            )
            .await;
        }
        CallbackValidation::Rejected { .. } => {
            let _ = tokio::time::timeout(
                SOCKET_IO_TIMEOUT,
                write_http_response(&mut stream, 400, "Bad Request", "invalid request"),
            )
            .await;
        }
    }
}

async fn read_request_headers(stream: &mut TcpStream, limit: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while buf.len() < limit {
        let to_read = chunk.len().min(limit - buf.len());
        match stream.read(&mut chunk[..to_read]).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if find_header_end(&buf).is_some() {
                    return Some(buf);
                }
            }
            Err(_) => return None,
        }
    }
    find_header_end(&buf).map(|_| buf)
}

async fn write_http_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_auth_contract_matches_frozen_v2_schema() {
        let c = parse_auth_contract(bundled_auth_contract_bytes()).expect("valid v2 contract");
        assert_eq!(c.version, 2);
        assert_eq!(c.capability, "solstoneJournalHost");
        assert_eq!(c.prepare, "auth.prepare");
        assert_eq!(c.prepared, "auth.prepared");
        assert_eq!(c.authorize, "auth.authorize");
        assert_eq!(c.cancel, "auth.cancel");
        assert_eq!(c.result, "auth.result");
        assert_eq!(c.unsupported, "unsupported");
        assert_eq!(parse_auth_contract(br#"{"version":3}"#), Err("unsupported"));
        assert_eq!(parse_auth_contract(b"{"), Err("malformed"));
    }

    #[test]
    fn authorize_url_validation_allows_only_strict_https() {
        assert!(
            validate_authorize_url(Some(&json!("https://provider.example/oauth?a=1&b=2")))
                .is_some()
        );
        for bad in [
            json!("http://provider.example/oauth"),
            json!("javascript:alert(1)"),
            json!("file:///etc/passwd"),
            json!("data:text/html,hi"),
            json!("https://user:pass@provider.example/oauth"),
            json!("https://user@provider.example/oauth"),
            json!("https://provider.example/oauth with space"),
            json!("https://provider.example/oauth\r\nX: 1"),
            json!("https://"),
            json!(""),
            json!(format!("https://provider.example/{}", "a".repeat(8200))),
        ] {
            assert!(
                validate_authorize_url(Some(&bad)).is_none(),
                "must reject {bad}"
            );
        }
    }

    #[test]
    fn http_callback_validation_enforces_host_path_method_unique_keys_and_state() {
        let ok = b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n";
        assert_eq!(
            validate_http_callback(ok, "127.0.0.1", 41234, "/cb", Some("st-1")),
            CallbackValidation::Accepted {
                callback_url: "http://127.0.0.1:41234/cb?code=abc&state=st-1".to_owned(),
            }
        );

        let cases: &[(&[u8], u16)] = &[
            (
                b"POST /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: evil.example:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /wrong?code=abc&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                404,
            ),
            (
                b"GET /cb?code=abc&state=wrong HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=abc&code=def&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
            (
                b"GET /cb?code=%ZZ&state=st-1 HTTP/1.1\r\nHost: 127.0.0.1:41234\r\n\r\n",
                400,
            ),
        ];
        for (req, expected) in cases {
            assert_eq!(
                validate_http_callback(req, "127.0.0.1", 41234, "/cb", Some("st-1")),
                CallbackValidation::Rejected { status: *expected }
            );
        }
    }

    #[cfg(feature = "full-tests")]
    mod socket_tests {
        use super::*;
        use url::Url;

        async fn send_raw_http(port: u16, request: &str) -> String {
            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("connect to loopback listener");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut out = Vec::new();
            let _ = stream.read_to_end(&mut out).await;
            String::from_utf8_lossy(&out).into_owned()
        }

        #[tokio::test]
        async fn happy_path_non_consuming_refusals_and_single_use_close() {
            let opened = Arc::new(Notify::new());
            let opened_url = Arc::new(Mutex::new(None::<String>));
            let opened_cb = Arc::clone(&opened);
            let url_cb = Arc::clone(&opened_url);
            let handle = AuthHandle::with_opener(
                move |url| {
                    if let Ok(mut guard) = url_cb.try_lock() {
                        *guard = Some(url.to_owned());
                    }
                    opened_cb.notify_one();
                    Ok(())
                },
                Duration::from_secs(60),
            );

            let (prep_str, sid) = handle
                .prepare(
                    ":1.42",
                    &json!({
                        "type": "auth.prepare",
                        "id": "p1",
                        "preferences": [
                            {"mode": "custom_scheme", "uri": "solstone://oauth/callback"},
                            {"mode": "loopback", "host": "localhost", "path": "/oauth/cb", "port": 0}
                        ]
                    })
                    .to_string(),
                )
                .await;
            let prep: Value = serde_json::from_str(&prep_str).unwrap();
            assert_eq!(prep["type"], "auth.prepared");
            assert_eq!(prep["outcome"], "ready");
            assert_eq!(prep["mode"], "loopback");
            let sid = sid.unwrap();
            let redirect_uri = prep["redirect_uri"].as_str().unwrap().to_owned();
            let parsed = Url::parse(&redirect_uri).unwrap();
            let port = parsed.port().unwrap();

            let auth_handle = handle.clone();
            let sid_auth = sid.clone();
            let auth_task = tokio::spawn(async move {
                auth_handle
                    .authorize(
                        ":1.42",
                        &json!({
                            "type": "auth.authorize",
                            "id": "a1",
                            "session_id": sid_auth,
                            "authorize_url": "https://provider.example/auth?x=1",
                            "expected_state": "secret_state_1",
                            "timeout_seconds": 30
                        })
                        .to_string(),
                    )
                    .await
            });

            opened.notified().await;
            assert_eq!(
                opened_url.lock().await.as_deref(),
                Some("https://provider.example/auth?x=1")
            );

            // Different D-Bus owner cannot authorize or cancel this session.
            let wrong_owner_cancel: Value = serde_json::from_str(
                &handle
                    .cancel(
                        ":1.99",
                        &json!({"type": "auth.cancel", "id": "cx", "session_id": sid}).to_string(),
                    )
                    .await,
            )
            .unwrap();
            assert_eq!(wrong_owner_cancel["outcome"], "cancelled");

            // Wrong state, wrong host, wrong path, duplicate keys must return 400/404 without closing listener.
            let r1 = send_raw_http(
                port,
                &format!(
                    "GET /oauth/cb?code=c1&state=wrong HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
                ),
            )
            .await;
            assert!(r1.starts_with("HTTP/1.1 400 "));

            let r2 = send_raw_http(
                port,
                "GET /oauth/cb?code=c1&state=secret_state_1 HTTP/1.1\r\nHost: evil.example:80\r\n\r\n",
            )
            .await;
            assert!(r2.starts_with("HTTP/1.1 400 "));

            let r3 = send_raw_http(
                port,
                &format!(
                    "GET /nope?code=c1&state=secret_state_1 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
                ),
            )
            .await;
            assert!(r3.starts_with("HTTP/1.1 404 "));

            let r4 = send_raw_http(
                port,
                &format!(
                    "GET /oauth/cb?code=c1&code=c2&state=secret_state_1 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
                ),
            )
            .await;
            assert!(r4.starts_with("HTTP/1.1 400 "));

            // Valid callback resolves and closes the listener immediately.
            let ok = send_raw_http(
                port,
                &format!(
                    "GET /oauth/cb?code=ok_code&state=secret_state_1 HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
                ),
            )
            .await;
            assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
            assert!(ok.contains("Cache-Control: no-store\r\n"), "{ok}");
            assert!(ok.ends_with(SUCCESS_BODY), "{ok}");

            let result: Value = serde_json::from_str(&auth_task.await.unwrap()).unwrap();
            assert_eq!(result["type"], "auth.result");
            assert_eq!(result["id"], "a1");
            assert_eq!(result["outcome"], "callback");
            assert_eq!(
                result["callback_url"],
                format!("http://localhost:{port}/oauth/cb?code=ok_code&state=secret_state_1")
            );
            assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
        }

        #[tokio::test]
        async fn cancel_supersede_owner_exit_timeout_and_invalid_urls() {
            // 1. Explicit auth.cancel
            {
                let opened = Arc::new(Notify::new());
                let opened_cb = Arc::clone(&opened);
                let handle = AuthHandle::with_opener(
                    move |_| {
                        opened_cb.notify_one();
                        Ok(())
                    },
                    Duration::from_secs(60),
                );
                let (_, sid) = handle
                    .prepare(
                        ":1.10",
                        &json!({
                            "type": "auth.prepare", "id": "p1",
                            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                        })
                        .to_string(),
                    )
                    .await;
                let sid = sid.unwrap();
                let h = handle.clone();
                let sid_c = sid.clone();
                let task = tokio::spawn(async move {
                    h.authorize(
                        ":1.10",
                        &json!({
                            "type": "auth.authorize", "id": "a1", "session_id": sid_c,
                            "authorize_url": "https://provider.example/auth", "expected_state": "st1"
                        })
                        .to_string(),
                    )
                    .await
                });
                opened.notified().await;
                let cancel_reply: Value = serde_json::from_str(
                    &handle
                        .cancel(
                            ":1.10",
                            &json!({"type": "auth.cancel", "id": "c1", "session_id": sid})
                                .to_string(),
                        )
                        .await,
                )
                .unwrap();
                assert_eq!(
                    cancel_reply,
                    json!({"type": "auth.result", "id": "c1", "outcome": "cancelled"})
                );
                let auth_reply: Value = serde_json::from_str(&task.await.unwrap()).unwrap();
                assert_eq!(
                    auth_reply,
                    json!({"type": "auth.result", "id": "a1", "outcome": "cancelled"})
                );
            }

            // 2. Superseding prepare settles prior in-flight authorize and frees fixed port
            {
                let opened = Arc::new(Notify::new());
                let opened_cb = Arc::clone(&opened);
                let handle = AuthHandle::with_opener(
                    move |_| {
                        opened_cb.notify_one();
                        Ok(())
                    },
                    Duration::from_secs(60),
                );
                let (p1_str, sid1) = handle
                    .prepare(
                        ":1.11",
                        &json!({
                            "type": "auth.prepare", "id": "p2",
                            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                        })
                        .to_string(),
                    )
                    .await;
                let p1: Value = serde_json::from_str(&p1_str).unwrap();
                let port1 = Url::parse(p1["redirect_uri"].as_str().unwrap())
                    .unwrap()
                    .port()
                    .unwrap();
                let sid1 = sid1.unwrap();
                let h = handle.clone();
                let task = tokio::spawn(async move {
                    h.authorize(
                        ":1.11",
                        &json!({
                            "type": "auth.authorize", "id": "a2", "session_id": sid1,
                            "authorize_url": "https://provider.example/auth", "expected_state": "st2"
                        })
                        .to_string(),
                    )
                    .await
                });
                opened.notified().await;
                let (p2_str, _) = handle
                    .prepare(
                        ":1.11",
                        &json!({
                            "type": "auth.prepare", "id": "p3",
                            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb", "port": port1}]
                        })
                        .to_string(),
                    )
                    .await;
                let p2: Value = serde_json::from_str(&p2_str).unwrap();
                assert_eq!(p2["outcome"], "ready");
                assert_eq!(p2["redirect_uri"], format!("http://127.0.0.1:{port1}/cb"));
                let prior: Value = serde_json::from_str(&task.await.unwrap()).unwrap();
                assert_eq!(
                    prior,
                    json!({"type": "auth.result", "id": "a2", "outcome": "cancelled"})
                );
                handle.tear_down().await;
            }

            // 3. Viewer D-Bus disappearance tears down session and settles authorize as cancelled
            {
                let opened = Arc::new(Notify::new());
                let opened_cb = Arc::clone(&opened);
                let handle = AuthHandle::with_opener(
                    move |_| {
                        opened_cb.notify_one();
                        Ok(())
                    },
                    Duration::from_secs(60),
                );
                let (_, sid) = handle
                    .prepare(
                        ":1.12",
                        &json!({
                            "type": "auth.prepare", "id": "p4",
                            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                        })
                        .to_string(),
                    )
                    .await;
                let sid = sid.unwrap();
                let h = handle.clone();
                let task = tokio::spawn(async move {
                    h.authorize(
                        ":1.12",
                        &json!({
                            "type": "auth.authorize", "id": "a4", "session_id": sid,
                            "authorize_url": "https://provider.example/auth", "expected_state": "st4"
                        })
                        .to_string(),
                    )
                    .await
                });
                opened.notified().await;
                handle.cancel_owner(":1.12").await;
                let reply: Value = serde_json::from_str(&task.await.unwrap()).unwrap();
                assert_eq!(
                    reply,
                    json!({"type": "auth.result", "id": "a4", "outcome": "cancelled"})
                );
            }

            // 4. Timeout and invalid authorize_url refusal
            {
                let handle = AuthHandle::with_opener(|_| Ok(()), Duration::from_secs(60));
                let (cs_str, _) = handle
                    .prepare(
                        ":1.13",
                        &json!({
                            "type": "auth.prepare", "id": "p-cs",
                            "preferences": [{"mode": "custom_scheme", "uri": "solstone://oauth/callback"}]
                        })
                        .to_string(),
                    )
                    .await;
                let cs: Value = serde_json::from_str(&cs_str).unwrap();
                assert_eq!(cs["outcome"], "no_compatible_mode");

                let (_, sid) = handle
                    .prepare(
                        ":1.13",
                        &json!({
                            "type": "auth.prepare", "id": "p5",
                            "preferences": [{"mode": "loopback", "host": "127.0.0.1", "path": "/cb"}]
                        })
                        .to_string(),
                    )
                    .await;
                let sid = sid.unwrap();
                for bad_url in [
                    "http://provider.example/auth",
                    "javascript:alert(1)",
                    "file:///etc/passwd",
                    "data:text/html,hi",
                ] {
                    let bad_reply: Value = serde_json::from_str(
                        &handle
                            .authorize(
                                ":1.13",
                                &json!({
                                    "type": "auth.authorize", "id": "a-bad", "session_id": sid,
                                    "authorize_url": bad_url, "expected_state": "st5"
                                })
                                .to_string(),
                            )
                            .await,
                    )
                    .unwrap();
                    assert_eq!(bad_reply["outcome"], "refused");
                    assert_eq!(bad_reply["reason"], "invalid_authorize_url");
                }

                let timed_out: Value = serde_json::from_str(
                    &handle
                        .authorize(
                            ":1.13",
                            &json!({
                                "type": "auth.authorize", "id": "a5", "session_id": sid,
                                "authorize_url": "https://provider.example/auth",
                                "expected_state": "st5", "timeout_seconds": 1
                            })
                            .to_string(),
                        )
                        .await,
                )
                .unwrap();
                assert_eq!(
                    timed_out,
                    json!({"type": "auth.result", "id": "a5", "outcome": "timeout"})
                );
            }
        }
    }
}
