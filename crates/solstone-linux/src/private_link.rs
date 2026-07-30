// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fmt,
    fs::{self, File},
    future::Future,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use spl_core::bridge::{BridgeNames, RequestHeaderPolicy};
use spl_transport::credential::Credential;
use spl_transport::{
    TransportError,
    client::{DialedCarrier, TokenPersistHook, TransportClient},
    journal_bridge::{
        BridgePolicy, CapabilityGate, CarrierOpener, JournalBridgeConfig, JournalBridgeHandle,
    },
};

use crate::private_file::{
    DurableWriteFault, PrivateFileError, atomic_write_bytes, atomic_write_bytes_with_fault,
    ensure_private_directory, open_regular_readonly,
};

pub(crate) const CREDENTIALS_FILENAME: &str = "credentials.json";
pub(crate) const OBSERVER_FILENAME: &str = "observer.json";
const PRIVATE_STATE_LOCK_FILENAME: &str = ".solstone-linux.private-state.lock";
const MAX_PAIR_LINK_BYTES: u64 = 4096;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LOOPBACK_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const OBSERVER_HEADER_NAME: &str = "x-solstone-observer";
pub(crate) const PROTOCOL_VERSION_HEADER_NAME: &str = "x-solstone-protocol-version";

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObserverState {
    pub(crate) credential_instance_id: String,
    pub(crate) key: String,
    pub(crate) prefix: String,
    pub(crate) name: String,
    pub(crate) ingest_url: String,
    pub(crate) protocol_version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivateTargetKind {
    ConfigDirectory,
    Credential,
    Observer,
    Lock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivateIoOperation {
    EnsureDirectory,
    Canonicalize,
    Open,
    Inspect,
    Chmod,
    Lock,
    Read,
    Serialize,
    Persist,
}

pub(crate) enum PrivateStateError {
    MalformedCredential,
    MalformedObserver,
    InvalidTarget {
        kind: PrivateTargetKind,
    },
    Io {
        operation: PrivateIoOperation,
        source: io::Error,
    },
    LockContended,
    PairInputInvalid,
    PairingFailed,
    BridgeUnavailable,
    BootstrapFailed,
    RegistrationInvalid,
    TokenPersistenceFailed,
    ShutdownFailed,
}

impl fmt::Display for PrivateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedCredential => formatter.write_str("MalformedCredential"),
            Self::MalformedObserver => formatter.write_str("MalformedObserver"),
            Self::InvalidTarget { kind } => write!(formatter, "InvalidTarget({kind:?})"),
            Self::Io { operation, source } => {
                write!(formatter, "Io({operation:?}, {:?})", source.kind())
            }
            Self::LockContended => formatter.write_str("LockContended"),
            Self::PairInputInvalid => formatter.write_str("PairInputInvalid"),
            Self::PairingFailed => formatter.write_str("PairingFailed"),
            Self::BridgeUnavailable => formatter.write_str("BridgeUnavailable"),
            Self::BootstrapFailed => formatter.write_str("BootstrapFailed"),
            Self::RegistrationInvalid => formatter.write_str("RegistrationInvalid"),
            Self::TokenPersistenceFailed => formatter.write_str("TokenPersistenceFailed"),
            Self::ShutdownFailed => formatter.write_str("ShutdownFailed"),
        }
    }
}

impl fmt::Debug for PrivateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for PrivateStateError {}

fn map_private_file(
    error: PrivateFileError,
    kind: PrivateTargetKind,
    operation: PrivateIoOperation,
) -> PrivateStateError {
    match error {
        PrivateFileError::InvalidTarget(_) => PrivateStateError::InvalidTarget { kind },
        PrivateFileError::Io { kind, .. } => PrivateStateError::Io {
            operation,
            source: io::Error::from(kind),
        },
    }
}

pub(crate) fn confine_path(origin: &Url, path: &str) -> Result<Url, PrivateStateError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path
            .bytes()
            .any(|byte| matches!(byte, b'?' | b'#' | b'\\' | b'\r' | b'\n' | 0))
    {
        return Err(PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Observer,
        });
    }
    for segment in path.split('/') {
        // Reject encoded percent so double encoding has one bounded failure rule.
        if segment.to_ascii_lowercase().contains("%25") {
            return Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Observer,
            });
        }
        let decoded = percent_decode(segment)?;
        if decoded == b"."
            || decoded == b".."
            || decoded
                .iter()
                .any(|byte| matches!(byte, b'/' | b'\\' | b'\r' | b'\n' | 0))
        {
            return Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Observer,
            });
        }
    }
    let url = origin
        .join(path)
        .map_err(|_| PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Observer,
        })?;
    if url.scheme() != origin.scheme()
        || url.host_str() != origin.host_str()
        || url.port_or_known_default() != origin.port_or_known_default()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Observer,
        });
    }
    Ok(url)
}

fn percent_decode(value: &str) -> Result<Vec<u8>, PrivateStateError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Observer,
            });
        }
        let high = hex(bytes[index + 1])?;
        let low = hex(bytes[index + 2])?;
        output.push(high << 4 | low);
        index += 3;
    }
    Ok(output)
}

fn hex(byte: u8) -> Result<u8, PrivateStateError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Observer,
        }),
    }
}

pub(crate) struct PrivateStateLock {
    _file: File,
    canonical_root: PathBuf,
}

impl Drop for PrivateStateLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock);
    }
}

impl PrivateStateLock {
    pub(crate) fn acquire(config_root: &Path) -> Result<Self, PrivateStateError> {
        ensure_private_directory(config_root).map_err(|error| {
            map_private_file(
                error,
                PrivateTargetKind::ConfigDirectory,
                PrivateIoOperation::EnsureDirectory,
            )
        })?;
        let canonical_root =
            fs::canonicalize(config_root).map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Canonicalize,
                source,
            })?;
        let descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            canonical_root.join(PRIVATE_STATE_LOCK_FILENAME),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CREATE,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Open,
            source: source.into(),
        })?;
        let file = File::from(descriptor);
        if !file
            .metadata()
            .map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Inspect,
                source,
            })?
            .is_file()
        {
            return Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Lock,
            });
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Chmod,
                source,
            })?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => return Err(PrivateStateError::LockContended),
            Err(source) => {
                return Err(PrivateStateError::Io {
                    operation: PrivateIoOperation::Lock,
                    source: source.into(),
                });
            }
        }
        Ok(Self {
            _file: file,
            canonical_root,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.canonical_root
    }
}

pub(crate) fn read_pair_link<R: Read>(input: R) -> Result<String, PrivateStateError> {
    let mut bytes = Vec::new();
    input
        .take(MAX_PAIR_LINK_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Read,
            source,
        })?;
    if bytes.len() as u64 > MAX_PAIR_LINK_BYTES {
        return Err(PrivateStateError::PairInputInvalid);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| PrivateStateError::PairInputInvalid)?;
    let link = text.trim_end_matches(char::is_whitespace);
    if link.is_empty() || link.chars().any(char::is_whitespace) {
        return Err(PrivateStateError::PairInputInvalid);
    }
    Ok(link.to_owned())
}

pub(crate) trait Pairer: Send + Sync {
    fn pair<'a>(
        &'a self,
        link: &'a str,
        device_label: &'a str,
        additional_fields: &'a serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Credential, PrivateStateError>> + Send + 'a>>;
}

pub(crate) struct SplPairer;

impl Pairer for SplPairer {
    fn pair<'a>(
        &'a self,
        link: &'a str,
        device_label: &'a str,
        additional_fields: &'a serde_json::Map<String, serde_json::Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Credential, PrivateStateError>> + Send + 'a>> {
        Box::pin(async move {
            spl_transport::pairing::pair_from_link(link, device_label, additional_fields)
                .await
                .map_err(|_| PrivateStateError::PairingFailed)
        })
    }
}

pub(crate) async fn setup<R: Read>(
    config_root: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer(&SplPairer, config_root, device_label, input).await
}

pub(crate) async fn setup_with_pairer<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    let state_lock = PrivateStateLock::acquire(config_root)?;
    let link = read_pair_link(input)?;
    let credential = pairer
        .pair(&link, device_label, &serde_json::Map::new())
        .await?;
    persist_credential(state_lock.root(), &credential)
}

fn read_private_file(
    path: &Path,
    kind: PrivateTargetKind,
) -> Result<Option<Vec<u8>>, PrivateStateError> {
    let mut file = match open_regular_readonly(path) {
        Ok(file) => file,
        Err(PrivateFileError::Io {
            kind: io::ErrorKind::NotFound,
            ..
        }) => return Ok(None),
        Err(error) => {
            return Err(map_private_file(error, kind, PrivateIoOperation::Open));
        }
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Read,
            source,
        })?;
    Ok(Some(bytes))
}

pub(crate) fn load_credential(config_root: &Path) -> Result<Option<Credential>, PrivateStateError> {
    let Some(bytes) = read_private_file(
        &config_root.join(CREDENTIALS_FILENAME),
        PrivateTargetKind::Credential,
    )?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| PrivateStateError::MalformedCredential)
}

pub(crate) fn persist_credential(
    config_root: &Path,
    credential: &Credential,
) -> Result<(), PrivateStateError> {
    let bytes =
        serde_json::to_vec(credential).map_err(|_| PrivateStateError::MalformedCredential)?;
    atomic_write_bytes(&config_root.join(CREDENTIALS_FILENAME), &bytes).map_err(|error| {
        map_private_file(
            error,
            PrivateTargetKind::Credential,
            PrivateIoOperation::Persist,
        )
    })
}

pub(crate) fn load_observer(
    config_root: &Path,
    credential_instance_id: &str,
    expected_name: &str,
    origin: &Url,
) -> Result<Option<ObserverState>, PrivateStateError> {
    let Some(bytes) = read_private_file(
        &config_root.join(OBSERVER_FILENAME),
        PrivateTargetKind::Observer,
    )?
    else {
        return Ok(None);
    };
    let observer = serde_json::from_slice::<ObserverState>(&bytes)
        .map_err(|_| PrivateStateError::MalformedObserver)?;
    if observer.credential_instance_id != credential_instance_id
        || observer.name != expected_name
        || observer.protocol_version != 2
        || observer.key.is_empty()
        || observer.prefix.is_empty()
        || observer.name.is_empty()
        || observer.ingest_url.is_empty()
        || contains_invalid_header_value(&observer.key)
        || confine_path(origin, &observer.ingest_url).is_err()
    {
        return Ok(None);
    }
    Ok(Some(observer))
}

pub(crate) fn persist_observer(
    config_root: &Path,
    observer: &ObserverState,
) -> Result<(), PrivateStateError> {
    let bytes = serde_json::to_vec(observer).map_err(|_| PrivateStateError::MalformedObserver)?;
    atomic_write_bytes(&config_root.join(OBSERVER_FILENAME), &bytes).map_err(|error| {
        map_private_file(
            error,
            PrivateTargetKind::Observer,
            PrivateIoOperation::Persist,
        )
    })
}

fn contains_invalid_header_value(value: &str) -> bool {
    value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
}

fn persist_and_publish_observer(
    config_root: &Path,
    observer: &ObserverState,
    opener: &PrivateLinkOpener,
    fault: &dyn DurableWriteFault,
) -> Result<(), PrivateStateError> {
    let bytes = serde_json::to_vec(observer).map_err(|_| PrivateStateError::MalformedObserver)?;
    atomic_write_bytes_with_fault(&config_root.join(OBSERVER_FILENAME), &bytes, fault).map_err(
        |error| {
            map_private_file(
                error,
                PrivateTargetKind::Observer,
                PrivateIoOperation::Persist,
            )
        },
    )?;
    opener.set_registered(observer)
}

#[derive(Clone)]
enum OpenerAuth {
    Unregistered,
    Registered { key: String },
}

struct PrivateLinkOpener {
    transport: Arc<TransportClient>,
    auth: RwLock<OpenerAuth>,
    expected_name: String,
}

impl PrivateLinkOpener {
    fn new(transport: TransportClient, expected_name: String) -> Self {
        Self {
            transport: Arc::new(transport),
            auth: RwLock::new(OpenerAuth::Unregistered),
            expected_name,
        }
    }

    fn set_registered(&self, observer: &ObserverState) -> Result<(), PrivateStateError> {
        if observer.key.is_empty()
            || contains_invalid_header_value(&observer.key)
            || observer.protocol_version != 2
            || observer.name != self.expected_name
        {
            return Err(PrivateStateError::RegistrationInvalid);
        }
        let mut auth = self
            .auth
            .write()
            .map_err(|_| PrivateStateError::RegistrationInvalid)?;
        *auth = OpenerAuth::Registered {
            key: observer.key.clone(),
        };
        Ok(())
    }
}

impl CarrierOpener for PrivateLinkOpener {
    fn proxy_headers(
        &self,
        upstream_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, TransportError> {
        let auth = self
            .auth
            .read()
            .map_err(|_| TransportError::Pairing("opener state unavailable".into()))?;
        Ok(proxy_headers_for_auth(upstream_headers, &auth))
    }

    fn dial_carrier(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<DialedCarrier, TransportError>> + Send + '_>> {
        Box::pin(self.transport.dial_carrier())
    }
}

fn proxy_headers_for_auth(
    upstream_headers: &[(String, String)],
    auth: &OpenerAuth,
) -> Vec<(String, String)> {
    let mut headers = upstream_headers.to_vec();
    match auth {
        OpenerAuth::Unregistered => {
            headers.push((PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned()));
        }
        OpenerAuth::Registered { key } => {
            headers.push((OBSERVER_HEADER_NAME.to_owned(), key.clone()));
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
            headers.push((PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned()));
        }
    }
    headers
}

pub(crate) struct PrivateLinkSession {
    client: reqwest::Client,
    origin: Url,
    opener: Arc<PrivateLinkOpener>,
    handle: JournalBridgeHandle,
    token_persistence: Option<Arc<TokenPersistence>>,
}

impl PrivateLinkSession {
    pub(crate) fn request(
        &self,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder, PrivateStateError> {
        let url = confine_path(&self.origin, relative_path)?;
        Ok(self
            .client
            .request(method, url)
            .timeout(LOOPBACK_REQUEST_TIMEOUT))
    }

    pub(crate) async fn shutdown(self) -> Result<(), PrivateStateError> {
        let status = self.handle.shutdown_and_wait().await;
        if self
            .token_persistence
            .as_ref()
            .is_some_and(|state| state.failed())
        {
            return Err(PrivateStateError::TokenPersistenceFailed);
        }
        if status.listener_active || status.active_requests != 0 {
            return Err(PrivateStateError::ShutdownFailed);
        }
        Ok(())
    }
}

struct TokenPersistence {
    config_root: PathBuf,
    credential: Mutex<Credential>,
    failed: Mutex<bool>,
    fault: Arc<dyn DurableWriteFault>,
}

impl TokenPersistence {
    fn new(
        config_root: PathBuf,
        credential: Credential,
        fault: Arc<dyn DurableWriteFault>,
    ) -> (Arc<Self>, TokenPersistHook) {
        let state = Arc::new(Self {
            config_root,
            credential: Mutex::new(credential),
            failed: Mutex::new(false),
            fault,
        });
        let hook_state = state.clone();
        let hook: TokenPersistHook = Arc::new(move |token, expires_at| {
            // SPL makes the token live before this synchronous hook; a concurrent
            // request can observe it before durability, an upstream race we cannot close here.
            hook_state.persist(token, expires_at);
        });
        (state, hook)
    }

    fn persist(&self, token: &str, expires_at: i64) {
        let mut current = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        let mut updated = current.clone();
        updated.device_token = Some(token.to_owned());
        updated.device_token_expires_at = Some(expires_at);
        let durable = serde_json::to_vec(&updated).ok().is_some_and(|bytes| {
            atomic_write_bytes_with_fault(
                &self.config_root.join(CREDENTIALS_FILENAME),
                &bytes,
                self.fault.as_ref(),
            )
            .is_ok()
        });
        if durable {
            *current = updated;
        } else {
            *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = true;
        }
    }

    fn failed(&self) -> bool {
        *self.failed.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub(crate) async fn start_private_link_session(
    credential: Credential,
    expected_name: &str,
) -> Result<PrivateLinkSession, PrivateStateError> {
    start_private_link_session_inner(credential, expected_name, None, None).await
}

async fn start_private_link_session_inner(
    credential: Credential,
    expected_name: &str,
    persistence: Option<(PathBuf, Arc<dyn DurableWriteFault>)>,
    capability_capture: Option<&Mutex<Option<String>>>,
) -> Result<PrivateLinkSession, PrivateStateError> {
    let endpoint_hosts = credential
        .endpoints
        .iter()
        .map(|endpoint| endpoint.host.clone())
        .collect();
    let (token_persistence, hook) = persistence.map_or((None, None), |(root, fault)| {
        let (state, hook) = TokenPersistence::new(root, credential.clone(), fault);
        (Some(state), Some(hook))
    });
    let transport =
        TransportClient::new(credential, hook).map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let opener = Arc::new(PrivateLinkOpener::new(transport, expected_name.to_owned()));
    let bridge_names = BridgeNames {
        capability_cookie_name: "solstone_linux_cap".to_owned(),
        upstream_cookie_prefix: "solstone_linux_".to_owned(),
        observer_header_name: OBSERVER_HEADER_NAME.to_owned(),
        protocol_version_header_name: PROTOCOL_VERSION_HEADER_NAME.to_owned(),
    };
    let policy = BridgePolicy {
        port: 0,
        capability_gate: CapabilityGate::Enabled,
        stream_response: BridgePolicy::default().stream_response,
        local_response: Arc::new(|_, _| None),
        attribution_headers: Arc::new(|_| Vec::new()),
        request_headers: RequestHeaderPolicy::Allow(
            [
                "accept",
                "accept-language",
                "content-type",
                "cache-control",
                "if-none-match",
                "if-modified-since",
                "range",
                "user-agent",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
        max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
    };
    let handle = spl_transport::journal_bridge::start(JournalBridgeConfig {
        opener: opener.clone(),
        bridge_names,
        endpoint_hosts,
        policy,
    })
    .await
    .map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let bootstrap_url = handle
        .bootstrap_url()
        .ok_or(PrivateStateError::BootstrapFailed)?;
    if let Some(capture) = capability_capture {
        let capability = Url::parse(&bootstrap_url)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(name, _)| name == "cap")
                    .map(|(_, value)| value.into_owned())
            })
            .ok_or(PrivateStateError::BootstrapFailed)?;
        *capture.lock().unwrap_or_else(|p| p.into_inner()) = Some(capability);
    }
    let origin = Url::parse(&format!("http://127.0.0.1:{}", handle.port()))
        .map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let client = reqwest::Client::builder()
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(LOOPBACK_CONNECT_TIMEOUT)
        .build()
        .map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let response = client
        .get(bootstrap_url)
        .timeout(LOOPBACK_REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| PrivateStateError::BootstrapFailed)?;
    if response.status() != StatusCode::FOUND {
        handle.begin_shutdown();
        return Err(PrivateStateError::BootstrapFailed);
    }
    Ok(PrivateLinkSession {
        client,
        origin,
        opener,
        handle,
        token_persistence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_file::DurableWriteStage;
    use crate::private_link_test_peer::PrivateLinkPeer;
    use spl_transport::credential::EndpointAddr;
    use std::{
        io::Cursor,
        net::TcpListener,
        os::unix::fs::{MetadataExt, symlink},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    fn credential() -> Credential {
        Credential {
            client_key_pem: "client-key".into(),
            client_cert_pem: "client-cert".into(),
            ca_chain_pem: vec!["ca".into()],
            ca_fp_prefix: vec![1, 2, 3],
            instance_id: "instance".into(),
            home_label: "home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port: 7657,
            }],
            home_attestation: Some("attestation".into()),
            local_endpoints: Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}])),
            relay_origin: Some("https://relay.invalid".into()),
            device_token: Some("device-token".into()),
            device_token_expires_at: Some(123),
        }
    }

    fn observer(path: &str) -> ObserverState {
        ObserverState {
            credential_instance_id: "instance".into(),
            key: "observer-key".into(),
            prefix: "prefix".into(),
            name: "stream".into(),
            ingest_url: path.into(),
            protocol_version: 2,
        }
    }

    fn assert_load_rejection_keeps_opener_unregistered(state: ObserverState) {
        let temp = tempfile::tempdir().unwrap();
        persist_observer(temp.path(), &state).unwrap();
        let loaded = load_observer(
            temp.path(),
            "instance",
            "stream",
            &Url::parse("http://127.0.0.1:1").unwrap(),
        )
        .unwrap();
        assert!(loaded.is_none());
        let headers = proxy_headers_for_auth(&[], &OpenerAuth::Unregistered);
        assert_eq!(
            headers,
            vec![(PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned())]
        );
    }

    macro_rules! opener_rejection_test {
        ($name:ident, $state:expr) => {
            #[test]
            fn $name() {
                assert_load_rejection_keeps_opener_unregistered($state);
            }
        };
    }

    opener_rejection_test!(
        opener_stays_unregistered_for_credential_mismatch,
        ObserverState {
            credential_instance_id: "other".into(),
            ..observer("/ingest")
        }
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_name_mismatch,
        ObserverState {
            name: "other".into(),
            ..observer("/ingest")
        }
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_protocol_mismatch,
        ObserverState {
            protocol_version: 3,
            ..observer("/ingest")
        }
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_unsafe_key,
        ObserverState {
            key: "bad\rkey".into(),
            ..observer("/ingest")
        }
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_relative_path,
        observer("relative")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_scheme_relative_path,
        observer("//host/x")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_raw_traversal,
        observer("/a/../b")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_encoded_traversal,
        observer("/a/%2e%2e/b")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_mixed_encoded_traversal,
        observer("/a/%2E./b")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_encoded_slash,
        observer("/a/%2f/b")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_encoded_backslash,
        observer("/a/%5c/b")
    );
    opener_rejection_test!(
        opener_stays_unregistered_for_double_encoding,
        observer("/a/%252e%252e/b")
    );
    opener_rejection_test!(opener_stays_unregistered_for_query, observer("/a?q"));
    opener_rejection_test!(opener_stays_unregistered_for_fragment, observer("/a#f"));
    opener_rejection_test!(opener_stays_unregistered_for_backslash, observer("/a\\b"));

    fn assert_redacted_rejection(bytes: &[u8]) {
        let temp = tempfile::tempdir().unwrap();
        let error = read_pair_link(Cursor::new(bytes)).unwrap_err();
        let material = String::from_utf8_lossy(bytes);
        if !material.is_empty() {
            assert!(!format!("{error}").contains(material.as_ref()));
            assert!(!format!("{error:?}").contains(material.as_ref()));
        }
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
        assert!(!temp.path().join(OBSERVER_FILENAME).exists());
    }

    #[test]
    fn pair_input_empty_is_rejected() {
        assert_redacted_rejection(b"");
    }
    #[test]
    fn pair_input_invalid_utf8_is_rejected() {
        assert_redacted_rejection(b"\xff");
    }
    #[test]
    fn pair_input_embedded_whitespace_is_rejected() {
        assert_redacted_rejection(b"pair link");
    }
    #[test]
    fn pair_input_leading_whitespace_is_rejected() {
        assert_redacted_rejection(b" pair");
    }
    #[test]
    fn pair_input_trailing_spaces_and_tabs_are_accepted_after_trim() {
        assert_eq!(read_pair_link(Cursor::new(b"pair \t")).unwrap(), "pair");
    }
    #[test]
    fn pair_input_multiple_line_endings_are_rejected() {
        assert_redacted_rejection(b"pair\nother\n");
    }
    #[test]
    fn pair_input_without_terminator_is_accepted() {
        assert_eq!(read_pair_link(Cursor::new(b"pair")).unwrap(), "pair");
    }
    #[test]
    fn pair_input_lf_termination_is_accepted() {
        assert_eq!(read_pair_link(Cursor::new(b"pair\n")).unwrap(), "pair");
    }
    #[test]
    fn pair_input_crlf_termination_is_accepted() {
        assert_eq!(read_pair_link(Cursor::new(b"pair\r\n")).unwrap(), "pair");
    }
    #[test]
    fn pair_input_exactly_4096_bytes_is_accepted() {
        assert_eq!(
            read_pair_link(Cursor::new(vec![b'a'; 4096])).unwrap().len(),
            4096
        );
    }
    #[test]
    fn pair_input_4097_bytes_is_rejected() {
        assert_redacted_rejection(&vec![b'a'; 4097]);
    }

    struct FakePairer {
        calls: Arc<AtomicUsize>,
        result: Option<Credential>,
    }

    struct FailStage(DurableWriteStage);
    impl DurableWriteFault for FailStage {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if stage == self.0 {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }

    struct RecordingFault {
        stages: Arc<Mutex<Vec<DurableWriteStage>>>,
        fail: Option<DurableWriteStage>,
    }
    impl DurableWriteFault for RecordingFault {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            self.stages.lock().unwrap().push(stage);
            if self.fail == Some(stage) {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }
    impl Pairer for FakePairer {
        fn pair<'a>(
            &'a self,
            _link: &'a str,
            _device_label: &'a str,
            _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Credential, PrivateStateError>> + Send + 'a>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { self.result.clone().ok_or(PrivateStateError::PairingFailed) })
        }
    }

    #[tokio::test]
    async fn injected_pairer_persists_only_success() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let pairer = FakePairer {
            calls: calls.clone(),
            result: None,
        };
        let failed = setup_with_pairer(&pairer, temp.path(), "device", Cursor::new(b"pair")).await;
        assert!(failed.is_err());
        drop(failed);
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
        let pairer = FakePairer {
            calls: calls.clone(),
            result: Some(credential()),
        };
        setup_with_pairer(&pairer, temp.path(), "device", Cursor::new(b"pair"))
            .await
            .unwrap();
        assert_eq!(load_credential(temp.path()).unwrap(), Some(credential()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn spl_pairer_surfaces_typed_failure() {
        let error = SplPairer
            .pair("not-a-pair-link", "device", &serde_json::Map::new())
            .await
            .unwrap_err();
        assert!(matches!(error, PrivateStateError::PairingFailed));
    }

    #[test]
    fn credential_and_observer_round_trip_all_fields() {
        let temp = tempfile::tempdir().unwrap();
        ensure_private_directory(temp.path()).unwrap();
        persist_credential(temp.path(), &credential()).unwrap();
        persist_observer(temp.path(), &observer("/app/observer/ingest")).unwrap();
        assert_eq!(load_credential(temp.path()).unwrap(), Some(credential()));
        let origin = Url::parse("http://127.0.0.1:1234").unwrap();
        assert!(
            load_observer(temp.path(), "instance", "stream", &origin)
                .unwrap()
                .is_some()
        );
        for name in [CREDENTIALS_FILENAME, OBSERVER_FILENAME] {
            let metadata = fs::symlink_metadata(temp.path().join(name)).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn malformed_state_errors_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(CREDENTIALS_FILENAME), b"{").unwrap();
        assert!(matches!(
            load_credential(temp.path()),
            Err(PrivateStateError::MalformedCredential)
        ));
        fs::write(temp.path().join(OBSERVER_FILENAME), b"{").unwrap();
        let origin = Url::parse("http://127.0.0.1:1").unwrap();
        assert!(matches!(
            load_observer(temp.path(), "instance", "stream", &origin),
            Err(PrivateStateError::MalformedObserver)
        ));
    }

    #[test]
    fn observer_semantic_rejections_are_read_only() {
        let origin = Url::parse("http://127.0.0.1:1234").unwrap();
        let cases = [
            ObserverState {
                credential_instance_id: "other".into(),
                ..observer("/ingest")
            },
            ObserverState {
                name: "other".into(),
                ..observer("/ingest")
            },
            ObserverState {
                protocol_version: 3,
                ..observer("/ingest")
            },
            ObserverState {
                key: "bad\rkey".into(),
                ..observer("/ingest")
            },
            ObserverState {
                key: String::new(),
                ..observer("/ingest")
            },
            ObserverState {
                prefix: String::new(),
                ..observer("/ingest")
            },
        ];
        for state in cases {
            let temp = tempfile::tempdir().unwrap();
            let bytes = serde_json::to_vec(&state).unwrap();
            fs::write(temp.path().join(OBSERVER_FILENAME), &bytes).unwrap();
            assert!(
                load_observer(temp.path(), "instance", "stream", &origin)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn observer_ingest_path_rejections_are_read_only() {
        let origin = Url::parse("http://127.0.0.1:1234").unwrap();
        for path in [
            "relative",
            "//other/path",
            "/a/../b",
            "/%2e%2e/b",
            "/%252e%252e/b",
            "/a?query",
            "/a#fragment",
            "/a\\b",
            "/a/%2f/b",
            "/a/%5c/b",
            "/a/%zz",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let bytes = serde_json::to_vec(&observer(path)).unwrap();
            fs::write(temp.path().join(OBSERVER_FILENAME), &bytes).unwrap();
            assert!(
                load_observer(temp.path(), "instance", "stream", &origin)
                    .unwrap()
                    .is_none(),
                "{path}"
            );
            assert_eq!(
                fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn private_state_files_reject_symlinks_without_touching_referent() {
        let temp = tempfile::tempdir().unwrap();
        let referent = temp.path().join("referent");
        fs::write(&referent, b"external").unwrap();
        fs::set_permissions(&referent, fs::Permissions::from_mode(0o644)).unwrap();
        for name in [CREDENTIALS_FILENAME, OBSERVER_FILENAME] {
            let link = temp.path().join(name);
            symlink(&referent, &link).unwrap();
            let result = if name == CREDENTIALS_FILENAME {
                load_credential(temp.path()).map(|_| ())
            } else {
                load_observer(
                    temp.path(),
                    "instance",
                    "stream",
                    &Url::parse("http://127.0.0.1:1").unwrap(),
                )
                .map(|_| ())
            };
            assert!(matches!(
                result,
                Err(PrivateStateError::InvalidTarget { .. })
            ));
            fs::remove_file(link).unwrap();
        }
        assert_eq!(fs::read(&referent).unwrap(), b"external");
        assert_eq!(
            fs::metadata(referent).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn lock_release_preserves_inode_and_nonsymlink_aliases_contend() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let first = PrivateStateLock::acquire(&root).unwrap();
        let lock_path = root.join(PRIVATE_STATE_LOCK_FILENAME);
        let before = fs::metadata(&lock_path).unwrap();
        let alias = root.join(".");
        let contender = PrivateStateLock::acquire(&alias);
        assert!(matches!(contender, Err(PrivateStateError::LockContended)));
        drop(contender);
        drop(first);
        assert!(lock_path.exists());
        let second = PrivateStateLock::acquire(&alias).unwrap();
        let after = fs::metadata(lock_path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        drop(second);
    }

    #[test]
    fn lock_rejects_symlinked_config_root_without_touching_referent() {
        let temp = tempfile::tempdir().unwrap();
        let referent = temp.path().join("referent");
        fs::create_dir(&referent).unwrap();
        fs::set_permissions(&referent, fs::Permissions::from_mode(0o755)).unwrap();
        let alias = temp.path().join("alias");
        symlink(&referent, &alias).unwrap();
        assert!(matches!(
            PrivateStateLock::acquire(&alias),
            Err(PrivateStateError::InvalidTarget { .. })
        ));
        assert_eq!(
            fs::metadata(&referent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!referent.join(PRIVATE_STATE_LOCK_FILENAME).exists());
    }

    struct CountingReader(Arc<AtomicUsize>);
    impl Read for CountingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
    }

    #[tokio::test]
    async fn lock_loser_fails_before_consuming_pair_input_or_state() {
        let temp = tempfile::tempdir().unwrap();
        let held = PrivateStateLock::acquire(temp.path()).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        assert!(matches!(
            setup_with_pairer(
                &FakePairer {
                    calls: Arc::new(AtomicUsize::new(0)),
                    result: Some(credential()),
                },
                temp.path(),
                "device",
                CountingReader(reads.clone()),
            )
            .await,
            Err(PrivateStateError::LockContended)
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
        assert!(!temp.path().join(OBSERVER_FILENAME).exists());
        drop(held);
    }

    #[tokio::test]
    async fn bridge_reuses_one_carrier_across_registration_transition() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        peer.enqueue_response(200, b"{}".to_vec());
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        assert_eq!(
            session
                .request(Method::GET, "/unregistered")
                .unwrap()
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        session
            .opener
            .set_registered(&observer("/app/observer/ingest"))
            .unwrap();
        assert_eq!(
            session
                .request(Method::GET, "/registered")
                .unwrap()
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let requests = peer.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER_NAME))
                .map(|(_, value)| value.as_str()),
            Some("2")
        );
        assert!(
            !requests[0]
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(OBSERVER_HEADER_NAME)
                    || name.eq_ignore_ascii_case("authorization"))
        );
        for name in [
            OBSERVER_HEADER_NAME,
            PROTOCOL_VERSION_HEADER_NAME,
            "authorization",
        ] {
            assert!(
                requests[1]
                    .headers
                    .iter()
                    .any(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            );
        }
        assert_eq!(peer.accepted_carriers(), 1);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[test]
    fn confined_requests_reject_every_unsafe_target_locally() {
        let origin = Url::parse("http://127.0.0.1:1234").unwrap();
        for target in [
            "http://other/x",
            "//other/x",
            "/a/../b",
            "/a/%2e%2e/b",
            "/a/%2E./b",
            "/a/%2f/b",
            "/a/%2F/b",
            "/a/%5c/b",
            "/a/%5C/b",
            "/a\\b",
            "/a/%252e%252e/b",
            "/a?q",
            "/a#f",
        ] {
            assert!(confine_path(&origin, target).is_err(), "{target}");
        }
    }

    #[tokio::test]
    async fn bridge_rejects_untrusted_local_authority_and_auth_without_upstream() {
        let peer = PrivateLinkPeer::start().await;
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        for (name, value) in [
            (OBSERVER_HEADER_NAME, "forged"),
            (PROTOCOL_VERSION_HEADER_NAME, "2"),
            ("authorization", "Bearer forged"),
        ] {
            let response = session
                .request(Method::GET, "/blocked")
                .unwrap()
                .header(name, value)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let bare = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for request in [
            bare.get(session.origin.join("/missing").unwrap()),
            bare.get(session.origin.join("/wrong-cookie").unwrap())
                .header("cookie", "solstone_linux_cap=wrong"),
            bare.get(session.origin.join("/wrong-host").unwrap())
                .header("host", "example.invalid"),
        ] {
            assert_eq!(
                request.send().await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        assert!(peer.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn loopback_client_does_not_follow_upstream_redirects() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(302, Vec::new());
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        let response = session
            .request(Method::GET, "/redirect")
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(peer.requests().len(), 1);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn paired_peer_resumes_large_response_after_window_credit() {
        let peer = PrivateLinkPeer::start().await;
        let body = vec![b'x'; spl_core::mux::INITIAL_WINDOW + 131_072];
        peer.enqueue_response(200, body.clone());
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        let received = session
            .request(Method::GET, "/large")
            .unwrap()
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(received.as_ref(), body);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[test]
    fn loopback_client_ignores_proxy_environment() {
        let trap = TcpListener::bind("127.0.0.1:0").unwrap();
        trap.set_nonblocking(true).unwrap();
        let proxy = format!("http://{}", trap.local_addr().unwrap());
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_link::tests::loopback_client_ignores_proxy_environment_child",
                "--ignored",
                "--nocapture",
            ])
            .env("SOLSTONE_PROXY_TEST_CHILD", "1")
            .env("HTTP_PROXY", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("ALL_PROXY", &proxy)
            .env("http_proxy", &proxy)
            .env("https_proxy", &proxy)
            .env("all_proxy", &proxy)
            .status()
            .unwrap();
        assert!(child.success());
        assert!(matches!(trap.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    }

    #[tokio::test]
    #[ignore = "executed in a child with isolated proxy environment"]
    async fn loopback_client_ignores_proxy_environment_child() {
        assert_eq!(
            std::env::var("SOLSTONE_PROXY_TEST_CHILD").as_deref(),
            Ok("1")
        );
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        assert_eq!(
            session
                .request(Method::GET, "/proxy-proof")
                .unwrap()
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn observer_publication_is_durable_before_registered_auth() {
        let temp = tempfile::tempdir().unwrap();
        let prior = observer("/prior");
        persist_observer(temp.path(), &prior).unwrap();
        let prior_bytes = fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap();
        let peer = PrivateLinkPeer::start().await;
        for _ in 0..6 {
            peer.enqueue_response(200, b"{}".to_vec());
        }
        let session = start_private_link_session(peer.credential(), "stream")
            .await
            .unwrap();
        let next = ObserverState {
            key: "new-observer-key".into(),
            ingest_url: "/new".into(),
            ..observer("/new")
        };
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
            DurableWriteStage::DirSync,
        ] {
            assert!(
                persist_and_publish_observer(
                    temp.path(),
                    &next,
                    &session.opener,
                    &FailStage(stage)
                )
                .is_err()
            );
            assert_eq!(
                fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap(),
                prior_bytes
            );
            let loaded = load_observer(
                temp.path(),
                "instance",
                "stream",
                &Url::parse("http://127.0.0.1:1").unwrap(),
            )
            .unwrap();
            assert!(loaded.as_ref() == Some(&prior));
            session
                .request(Method::GET, "/still-unregistered")
                .unwrap()
                .send()
                .await
                .unwrap();
        }
        persist_and_publish_observer(
            temp.path(),
            &next,
            &session.opener,
            &crate::private_file::NoWriteFault,
        )
        .unwrap();
        session
            .request(Method::GET, "/registered-after-durable")
            .unwrap()
            .send()
            .await
            .unwrap();
        let requests = peer.requests();
        assert_eq!(requests.len(), 6);
        for request in &requests[..5] {
            assert!(request.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER_NAME) && value == "2"
            }));
            assert!(!request.headers.iter().any(|(name, _)| {
                name.eq_ignore_ascii_case(OBSERVER_HEADER_NAME)
                    || name.eq_ignore_ascii_case("authorization")
            }));
        }
        for name in [
            OBSERVER_HEADER_NAME,
            PROTOCOL_VERSION_HEADER_NAME,
            "authorization",
        ] {
            assert!(
                requests[5]
                    .headers
                    .iter()
                    .any(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            );
        }
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[test]
    fn token_hook_returns_only_after_directory_sync_and_reload_sees_refresh() {
        let temp = tempfile::tempdir().unwrap();
        persist_credential(temp.path(), &credential()).unwrap();
        let stages = Arc::new(Mutex::new(Vec::new()));
        let (_state, hook) = TokenPersistence::new(
            temp.path().to_path_buf(),
            credential(),
            Arc::new(RecordingFault {
                stages: stages.clone(),
                fail: None,
            }),
        );
        hook("refreshed-token", 456);
        assert_eq!(
            *stages.lock().unwrap(),
            [
                DurableWriteStage::Create,
                DurableWriteStage::Write,
                DurableWriteStage::Fsync,
                DurableWriteStage::Rename,
                DurableWriteStage::DirSync,
            ]
        );
        let loaded = load_credential(temp.path()).unwrap().unwrap();
        assert_eq!(loaded.device_token.as_deref(), Some("refreshed-token"));
        assert_eq!(loaded.device_token_expires_at, Some(456));
    }

    async fn assert_token_failure(stage: DurableWriteStage) {
        let temp = tempfile::tempdir().unwrap();
        let prior = credential();
        persist_credential(temp.path(), &prior).unwrap();
        let prior_bytes = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        let peer = PrivateLinkPeer::start().await;
        let session = start_private_link_session_inner(
            peer.credential(),
            "stream",
            Some((
                temp.path().to_path_buf(),
                Arc::new(RecordingFault {
                    stages: Arc::new(Mutex::new(Vec::new())),
                    fail: Some(stage),
                }),
            )),
            None,
        )
        .await
        .unwrap();
        session
            .token_persistence
            .as_ref()
            .unwrap()
            .persist("failed-refresh", 999);
        assert_eq!(
            fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            prior_bytes
        );
        assert!(matches!(
            session.shutdown().await,
            Err(PrivateStateError::TokenPersistenceFailed)
        ));
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn token_write_failure_is_latched_and_preserves_prior_credential() {
        assert_token_failure(DurableWriteStage::Write).await;
    }

    #[tokio::test]
    async fn token_fsync_failure_is_latched_and_preserves_prior_credential() {
        assert_token_failure(DurableWriteStage::Fsync).await;
    }

    #[tokio::test]
    async fn executed_session_surfaces_do_not_disclose_secrets() {
        let peer = PrivateLinkPeer::start().await;
        let mut paired = peer.credential();
        let client_key = paired.client_key_pem.clone();
        let key_interior = client_key
            .lines()
            .find(|line| !line.starts_with('-'))
            .unwrap()
            .to_owned();
        let device_token = "device-token-sentinel";
        paired.device_token = Some(device_token.into());
        let capability = Mutex::new(None);
        let session = start_private_link_session_inner(paired, "stream", None, Some(&capability))
            .await
            .unwrap();
        let capability = capability.into_inner().unwrap().unwrap();
        let observer_key = "observer-key-sentinel";
        let registered = ObserverState {
            key: observer_key.into(),
            ..observer("/ingest")
        };
        session.opener.set_registered(&registered).unwrap();
        let request_debug = format!(
            "{:?}",
            session
                .request(Method::GET, "/safe")
                .unwrap()
                .build()
                .unwrap()
        );
        let error = session.request(Method::GET, "/%252e%252e").unwrap_err();
        let outputs = [
            request_debug,
            format!("{error}"),
            format!("{error:?}"),
            format!("{:?}", session.shutdown().await),
        ];
        for output in outputs {
            for secret in [
                capability.as_str(),
                device_token,
                client_key.as_str(),
                key_interior.as_str(),
                observer_key,
            ] {
                assert!(!output.contains(secret));
            }
        }
        peer.shutdown().await;
    }

    #[test]
    fn session_implements_no_debug_clone_or_serialize_traits() {
        use core::marker::PhantomData;

        struct DebugProbe<T>(PhantomData<T>);
        trait DebugFallback {
            fn probe(&self) -> bool {
                false
            }
        }
        impl<T> DebugFallback for DebugProbe<T> {}
        impl<T: fmt::Debug> DebugProbe<T> {
            fn probe(&self) -> bool {
                true
            }
        }

        struct CloneProbe<T>(PhantomData<T>);
        trait CloneFallback {
            fn probe(&self) -> bool {
                false
            }
        }
        impl<T> CloneFallback for CloneProbe<T> {}
        impl<T: Clone> CloneProbe<T> {
            fn probe(&self) -> bool {
                true
            }
        }

        struct SerializeProbe<T>(PhantomData<T>);
        trait SerializeFallback {
            fn probe(&self) -> bool {
                false
            }
        }
        impl<T> SerializeFallback for SerializeProbe<T> {}
        impl<T: Serialize> SerializeProbe<T> {
            fn probe(&self) -> bool {
                true
            }
        }

        assert!(DebugProbe::<String>(PhantomData).probe());
        assert!(CloneProbe::<String>(PhantomData).probe());
        assert!(SerializeProbe::<String>(PhantomData).probe());
        assert!(!DebugProbe::<PrivateLinkSession>(PhantomData).probe());
        assert!(!CloneProbe::<PrivateLinkSession>(PhantomData).probe());
        assert!(!SerializeProbe::<PrivateLinkSession>(PhantomData).probe());
    }
}
