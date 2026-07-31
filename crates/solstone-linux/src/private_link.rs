// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fmt,
    fs::{self, File},
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
use reqwest::Method;
use reqwest::{RequestBuilder, StatusCode, Url, multipart};
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

use crate::config::{
    ConfigPaths, sanitize_link_authority, save_linked_stream, save_linked_stream_with_fault,
};
use crate::private_file::{
    DurableWriteFault, NoWriteFault, PrivateFileError, atomic_write_bytes,
    atomic_write_bytes_with_fault, ensure_private_directory, open_regular_readonly,
};

pub(crate) const CREDENTIALS_FILENAME: &str = "credentials.json";
pub(crate) const OBSERVER_FILENAME: &str = "observer.json";
const PRIVATE_STATE_LOCK_FILENAME: &str = ".solstone-linux.private-state.lock";
const MAX_PAIR_LINK_BYTES: u64 = 4096;
pub(crate) const MAX_REQUEST_BODY_BYTES: u64 = 64 * 1024 * 1024;
const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(30);
const INGEST_TIMEOUT: Duration = Duration::from_secs(300);
const LISTING_TIMEOUT: Duration = Duration::from_secs(60);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const OBSERVER_HEADER_NAME: &str = "x-solstone-observer";
pub(crate) const PROTOCOL_VERSION_HEADER_NAME: &str = "x-solstone-protocol-version";
const REGISTRATION_MARKER_HEADER_NAME: &str = "x-solstone-linux-registration-route";
const REGISTRATION_MARKER_HEADER_VALUE: &str = "1";
const EVENT_PATH: &str = "/app/observer/ingest/event";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateStateLockLiveness {
    LiveOwner,
    NoLiveOwner,
}

#[derive(Debug)]
pub(crate) enum PrivateStateProbeError {
    InvalidTarget,
    Inspect,
    LocksUnavailable,
    LocksMalformed,
}

impl Drop for PrivateStateLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock);
    }
}

impl PrivateStateLock {
    pub(crate) fn try_probe(
        config_root: &Path,
    ) -> Result<PrivateStateLockLiveness, PrivateStateProbeError> {
        let root = match rustix::fs::openat(
            rustix::fs::CWD,
            config_root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        ) {
            Ok(root) => root,
            Err(rustix::io::Errno::NOENT) => {
                return Ok(PrivateStateLockLiveness::NoLiveOwner);
            }
            Err(_) => return Err(PrivateStateProbeError::Inspect),
        };
        let root_stat = rustix::fs::fstat(&root).map_err(|_| PrivateStateProbeError::Inspect)?;
        let expected_root_mode =
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR;
        if rustix::fs::FileType::from_raw_mode(root_stat.st_mode) != rustix::fs::FileType::Directory
            || rustix::fs::Mode::from_raw_mode(root_stat.st_mode) != expected_root_mode
            || root_stat.st_uid != rustix::process::geteuid().as_raw()
        {
            return Err(PrivateStateProbeError::InvalidTarget);
        }
        let descriptor = match rustix::fs::openat(
            &root,
            PRIVATE_STATE_LOCK_FILENAME,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => {
                return Ok(PrivateStateLockLiveness::NoLiveOwner);
            }
            Err(_) => return Err(PrivateStateProbeError::Inspect),
        };
        let file = File::from(descriptor);
        let stat = rustix::fs::fstat(&file).map_err(|_| PrivateStateProbeError::Inspect)?;
        let expected_mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || rustix::fs::Mode::from_raw_mode(stat.st_mode) != expected_mode
            || stat.st_uid != rustix::process::geteuid().as_raw()
        {
            return Err(PrivateStateProbeError::InvalidTarget);
        }
        let locks = fs::read_to_string("/proc/locks")
            .map_err(|_| PrivateStateProbeError::LocksUnavailable)?;
        probe_lock_table(&locks, stat.st_dev, stat.st_ino)
    }

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
        let root_descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            &canonical_root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .map_err(|source| PrivateStateError::Io {
            operation: PrivateIoOperation::Open,
            source: source.into(),
        })?;
        let descriptor = rustix::fs::openat(
            &root_descriptor,
            PRIVATE_STATE_LOCK_FILENAME,
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
        rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR).map_err(
            |source| PrivateStateError::Io {
                operation: PrivateIoOperation::Chmod,
                source: source.into(),
            },
        )?;
        verify_private_lock(&file)?;
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

    pub(crate) fn try_clone(&self) -> Result<Self, PrivateStateError> {
        Ok(Self {
            _file: self
                ._file
                .try_clone()
                .map_err(|source| PrivateStateError::Io {
                    operation: PrivateIoOperation::Lock,
                    source,
                })?,
            canonical_root: self.canonical_root.clone(),
        })
    }
}

fn linux_device_major(device: u64) -> u64 {
    ((device >> 8) & 0xfff) | ((device >> 32) & 0xffff_f000)
}

fn linux_device_minor(device: u64) -> u64 {
    (device & 0xff) | ((device >> 12) & 0xffff_ff00)
}

fn probe_lock_table(
    locks: &str,
    device: u64,
    inode: u64,
) -> Result<PrivateStateLockLiveness, PrivateStateProbeError> {
    let expected_major = linux_device_major(device);
    let expected_minor = linux_device_minor(device);
    for line in locks.lines() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let offset = usize::from(fields.get(1) == Some(&"->"));
        if fields.len() < 8 + offset
            || !fields[0].ends_with(':')
            || !matches!(fields[1 + offset], "FLOCK" | "POSIX" | "OFDLCK")
        {
            return Err(PrivateStateProbeError::LocksMalformed);
        }
        let Some(identity) = fields.get(5 + offset) else {
            return Err(PrivateStateProbeError::LocksMalformed);
        };
        let mut parts = identity.split(':');
        let (Some(major), Some(minor), Some(candidate_inode), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(PrivateStateProbeError::LocksMalformed);
        };
        let major =
            u64::from_str_radix(major, 16).map_err(|_| PrivateStateProbeError::LocksMalformed)?;
        let minor =
            u64::from_str_radix(minor, 16).map_err(|_| PrivateStateProbeError::LocksMalformed)?;
        let candidate_inode = candidate_inode
            .parse::<u64>()
            .map_err(|_| PrivateStateProbeError::LocksMalformed)?;
        if fields[1 + offset] == "FLOCK"
            && fields[3 + offset] == "WRITE"
            && major == expected_major
            && minor == expected_minor
            && candidate_inode == inode
        {
            return Ok(PrivateStateLockLiveness::LiveOwner);
        }
    }
    Ok(PrivateStateLockLiveness::NoLiveOwner)
}

fn verify_private_lock(file: &File) -> Result<(), PrivateStateError> {
    let stat = rustix::fs::fstat(file).map_err(|source| PrivateStateError::Io {
        operation: PrivateIoOperation::Inspect,
        source: source.into(),
    })?;
    let expected_mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || rustix::fs::Mode::from_raw_mode(stat.st_mode) != expected_mode
    {
        return Err(PrivateStateError::InvalidTarget {
            kind: PrivateTargetKind::Lock,
        });
    }
    Ok(())
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
    let link = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
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

#[cfg(test)]
pub(crate) async fn setup<R: Read>(
    config_root: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_stream(config_root, device_label, None, input).await
}

pub(crate) async fn setup_with_stream<R: Read>(
    config_root: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(&SplPairer, config_root, device_label, stream, input).await
}

#[cfg(test)]
async fn setup_with_pairer<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(pairer, config_root, device_label, None, input).await
}

async fn setup_with_pairer_and_stream<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    let state_lock = PrivateStateLock::acquire(config_root)?;
    sanitize_link_authority(&private_config_paths(state_lock.root()))
        .map_err(config_persist_error)?;
    if let Some(stream) = stream {
        save_linked_stream(&private_config_paths(state_lock.root()), stream)
            .map_err(config_persist_error)?;
    }
    let link = read_pair_link(input)?;
    let credential = pairer
        .pair(&link, device_label, &serde_json::Map::new())
        .await?;
    persist_credential(state_lock.root(), &credential)
}

#[cfg(test)]
pub(crate) async fn setup_with_pairer_for_test<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(pairer, config_root, device_label, stream, input).await
}

fn private_config_paths(config_root: &Path) -> ConfigPaths {
    ConfigPaths {
        base_dir: None,
        config_dir: Some(config_root.to_path_buf()),
    }
}

fn config_persist_error(source: io::Error) -> PrivateStateError {
    PrivateStateError::Io {
        operation: PrivateIoOperation::Persist,
        source,
    }
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
    if !observer_is_valid(&observer, credential_instance_id, expected_name, origin) {
        return Ok(None);
    }
    Ok(Some(observer))
}

fn observer_is_valid(
    observer: &ObserverState,
    credential_instance_id: &str,
    expected_name: &str,
    origin: &Url,
) -> bool {
    observer.credential_instance_id == credential_instance_id
        && observer.name == expected_name
        && observer.protocol_version == 2
        && !observer.key.is_empty()
        && !observer.prefix.is_empty()
        && !observer.name.is_empty()
        && !observer.ingest_url.is_empty()
        && !contains_invalid_header_value(&observer.key)
        && confine_path(origin, &observer.ingest_url).is_ok()
}

fn write_observer_durably(
    config_root: &Path,
    observer: &ObserverState,
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
    )
}

#[cfg(test)]
pub(crate) fn persist_observer(
    config_root: &Path,
    observer: &ObserverState,
) -> Result<(), PrivateStateError> {
    write_observer_durably(config_root, observer, &NoWriteFault)
}

fn contains_invalid_header_value(value: &str) -> bool {
    reqwest::header::HeaderValue::from_bytes(value.as_bytes()).is_err()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkFact {
    PairingRequired,
    PrivateStateInvalid,
    ConfigSanitationFailed,
    ListenerReady,
    CarrierProven,
    ObserverRegistered,
    TransportUnavailable,
    TerminalRevocation,
    TokenPersistenceFailure,
}

#[derive(Clone, Default)]
pub(crate) struct LinkFacts {
    inner: Arc<Mutex<LinkFactState>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LinkFactState {
    pub(crate) pairing_required: bool,
    pub(crate) private_state_invalid: bool,
    pub(crate) config_sanitation_failed: bool,
    pub(crate) listener_ready: bool,
    pub(crate) carrier_proven: bool,
    pub(crate) observer_registered: bool,
    pub(crate) transport_unavailable: bool,
    pub(crate) terminal_revocation: bool,
    pub(crate) token_persistence_failure: bool,
}

impl LinkFacts {
    pub(crate) fn begin_owner_generation(&self) {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) = LinkFactState::default();
    }

    pub(crate) fn owner_lost(&self) {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        *state = LinkFactState {
            transport_unavailable: true,
            ..LinkFactState::default()
        };
    }

    pub(crate) fn publish(&self, fact: LinkFact) {
        let mut state = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match fact {
            LinkFact::PairingRequired => state.pairing_required = true,
            LinkFact::PrivateStateInvalid => state.private_state_invalid = true,
            LinkFact::ConfigSanitationFailed => state.config_sanitation_failed = true,
            LinkFact::ListenerReady => state.listener_ready = true,
            LinkFact::CarrierProven => state.carrier_proven = true,
            LinkFact::ObserverRegistered => {
                state.observer_registered = true;
                if !state.token_persistence_failure {
                    state.transport_unavailable = false;
                }
            }
            LinkFact::TransportUnavailable => state.transport_unavailable = true,
            LinkFact::TerminalRevocation => state.terminal_revocation = true,
            LinkFact::TokenPersistenceFailure => state.token_persistence_failure = true,
        }
    }

    pub(crate) fn snapshot(&self) -> LinkFactState {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

struct RegistrationCommitFaults<'a> {
    observer: &'a dyn DurableWriteFault,
    config: &'a dyn DurableWriteFault,
}

fn persist_and_publish_observer(
    config_root: &Path,
    credential_instance_id: &str,
    expected_name: &str,
    origin: &Url,
    observer: &ObserverState,
    opener: &PrivateLinkOpener,
    faults: RegistrationCommitFaults<'_>,
) -> Result<u64, PrivateStateError> {
    if !observer_is_valid(observer, credential_instance_id, expected_name, origin) {
        return Err(PrivateStateError::RegistrationInvalid);
    }
    write_observer_durably(config_root, observer, faults.observer)?;
    save_linked_stream_with_fault(
        &private_config_paths(config_root),
        &observer.name,
        faults.config,
    )
    .map_err(config_persist_error)?;
    opener.set_registered(observer)
}

#[derive(Clone)]
enum OpenerAuth {
    Unregistered,
    Registered { key: String },
}

struct AuthEpoch {
    generation: u64,
    state: OpenerAuth,
}

struct PrivateLinkOpener {
    transport: Arc<TransportClient>,
    auth: RwLock<Arc<AuthEpoch>>,
    admission: tokio::sync::Mutex<()>,
    transport_unavailable: Arc<AtomicBool>,
    facts: LinkFacts,
}

impl PrivateLinkOpener {
    fn new(
        transport: TransportClient,
        transport_unavailable: Arc<AtomicBool>,
        facts: LinkFacts,
    ) -> Self {
        Self {
            transport: Arc::new(transport),
            auth: RwLock::new(Arc::new(AuthEpoch {
                generation: 0,
                state: OpenerAuth::Unregistered,
            })),
            admission: tokio::sync::Mutex::new(()),
            transport_unavailable,
            facts,
        }
    }

    fn set_registered(&self, observer: &ObserverState) -> Result<u64, PrivateStateError> {
        if observer.key.is_empty()
            || contains_invalid_header_value(&observer.key)
            || observer.protocol_version != 2
        {
            return Err(PrivateStateError::RegistrationInvalid);
        }
        let mut auth = self
            .auth
            .write()
            .map_err(|_| PrivateStateError::RegistrationInvalid)?;
        let generation = auth.generation.saturating_add(1);
        *auth = Arc::new(AuthEpoch {
            generation,
            state: OpenerAuth::Registered {
                key: observer.key.clone(),
            },
        });
        self.facts.publish(LinkFact::ObserverRegistered);
        Ok(generation)
    }

    fn generation(&self) -> u64 {
        self.auth
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .generation
    }

    fn registered_key(&self) -> Option<String> {
        match &self
            .auth
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state
        {
            OpenerAuth::Unregistered => None,
            OpenerAuth::Registered { key } => Some(key.clone()),
        }
    }

    async fn admit_dial<T>(
        &self,
        dial: impl Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        if self.transport_unavailable.load(Ordering::Acquire) {
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        // Pinned client.rs:265 and :296 are the only refresh callers, both
        // inside dial_carrier_over_relay reached by TransportClient::dial_carrier.
        // There is no timer, background, or live-stream refresh path.
        let _admission = self.admission.lock().await;
        if self.transport_unavailable.load(Ordering::Acquire) {
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        let dialed = dial.await?;
        if self.transport_unavailable.load(Ordering::Acquire) {
            drop(dialed);
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        Ok(dialed)
    }
}

impl CarrierOpener for PrivateLinkOpener {
    fn proxy_headers(
        &self,
        upstream_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, TransportError> {
        let epoch = self
            .auth
            .read()
            .map_err(|_| TransportError::Pairing("opener state unavailable".into()))?;
        proxy_headers_for_epoch(upstream_headers, &epoch)
    }

    fn dial_carrier(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<DialedCarrier, TransportError>> + Send + '_>> {
        Box::pin(async move {
            let carrier = self.admit_dial(self.transport.dial_carrier()).await?;
            self.facts.publish(LinkFact::CarrierProven);
            Ok(carrier)
        })
    }
}

fn proxy_headers_for_epoch(
    upstream_headers: &[(String, String)],
    epoch: &AuthEpoch,
) -> Result<Vec<(String, String)>, TransportError> {
    let markers = upstream_headers
        .iter()
        .filter(|(name, _)| name == REGISTRATION_MARKER_HEADER_NAME)
        .collect::<Vec<_>>();
    let registration = match markers.as_slice() {
        [] => false,
        [(_, value)] if value == REGISTRATION_MARKER_HEADER_VALUE => true,
        _ => {
            return Err(TransportError::Pairing(
                "invalid registration route marker".into(),
            ));
        }
    };
    let mut headers = upstream_headers
        .iter()
        .filter(|(name, _)| name != REGISTRATION_MARKER_HEADER_NAME)
        .cloned()
        .collect::<Vec<_>>();
    if registration {
        headers.push((PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned()));
        return Ok(headers);
    }
    match &epoch.state {
        OpenerAuth::Unregistered => Err(TransportError::Pairing(
            "observer registration unavailable".into(),
        )),
        OpenerAuth::Registered { key } => {
            headers.push((OBSERVER_HEADER_NAME.to_owned(), key.clone()));
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
            headers.push((PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned()));
            Ok(headers)
        }
    }
}

pub(crate) struct PrivateLinkSession {
    client: reqwest::Client,
    origin: Url,
    opener: Arc<PrivateLinkOpener>,
    registration: Arc<RegistrationCoordinator>,
    handle: JournalBridgeHandle,
    token_persistence: Arc<TokenPersistence>,
    bootstrap_target: Option<String>,
    _state_lock: PrivateStateLock,
    #[cfg(test)]
    credential_instance_id: String,
    #[cfg(test)]
    expected_name: String,
    #[cfg(test)]
    facts: LinkFacts,
}

enum OpenJournalGate {
    Open(String),
    Closed,
}

struct OpenJournalTarget {
    gate: Mutex<OpenJournalGate>,
}

#[derive(Clone)]
pub(crate) struct OpenJournalCapability {
    target: std::sync::Weak<OpenJournalTarget>,
}

impl std::fmt::Debug for OpenJournalCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OpenJournalCapability(<redacted>)")
    }
}

impl OpenJournalCapability {
    fn available(&self) -> bool {
        self.target
            .upgrade()
            .and_then(|target| {
                target
                    .gate
                    .lock()
                    .ok()
                    .map(|gate| matches!(*gate, OpenJournalGate::Open(_)))
            })
            .unwrap_or(false)
    }

    pub(crate) fn open(&self) -> Result<(), ()> {
        // Opening necessarily hands the approved target to the desktop browser.
        // The target is accepted in child argv here; it remains excluded from the
        // secrecy contract's logs, errors, status, clipboard, D-Bus, state, and Debug surfaces.
        self.open_inner(|target| open::that_detached(target).map_err(|_| ()))
    }

    fn open_inner(&self, opener: impl FnOnce(&str) -> Result<(), ()>) -> Result<(), ()> {
        let target = self.target.upgrade().ok_or(())?;
        let gate = target.gate.lock().map_err(|_| ())?;
        match &*gate {
            OpenJournalGate::Open(target) => opener(target),
            OpenJournalGate::Closed => Err(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn open_with(&self, opener: impl FnOnce(&str) -> Result<(), ()>) -> Result<(), ()> {
        self.open_inner(opener)
    }

    fn close(&self) {
        let Some(target) = self.target.upgrade() else {
            return;
        };
        match target.gate.lock() {
            Ok(mut gate) => *gate = OpenJournalGate::Closed,
            Err(poisoned) => *poisoned.into_inner() = OpenJournalGate::Closed,
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct OpenJournalAccess {
    current: Arc<Mutex<Option<OpenJournalCapability>>>,
}

impl OpenJournalAccess {
    pub(crate) fn available(&self) -> bool {
        self.current
            .lock()
            .ok()
            .and_then(|current| current.clone())
            .is_some_and(|capability| capability.available())
    }

    pub(crate) fn open(&self) -> Result<(), ()> {
        let capability = self.current.lock().map_err(|_| ())?.clone().ok_or(())?;
        capability.open()
    }

    pub(crate) fn install(&self, capability: OpenJournalCapability) {
        if let Ok(mut current) = self.current.lock() {
            *current = Some(capability);
        }
    }

    pub(crate) fn close_current(&self) {
        let capability = self.current.lock().ok().and_then(|current| current.clone());
        if let Some(capability) = capability {
            capability.close();
            self.clear(&capability);
        }
    }

    fn clear(&self, capability: &OpenJournalCapability) {
        let Ok(mut current) = self.current.lock() else {
            return;
        };
        if current
            .as_ref()
            .is_some_and(|value| value.target.ptr_eq(&capability.target))
        {
            *current = None;
        }
    }
}

pub(crate) enum LinkOutcome {
    Success { status: StatusCode, body: Vec<u8> },
    Unauthorized { generation: u64 },
    Forbidden,
    TransportUnavailable,
    LocalRejected { status: StatusCode },
}

pub(crate) enum RepairOutcome {
    Repaired { generation: u64, name: String },
    AlreadySuperseded { generation: u64 },
    GuardRefused { reason_code: Option<String> },
    TransportUnavailable,
    PersistenceFailed,
    InvalidRegistration,
}

pub(crate) struct EventBody {
    pub(crate) tract: String,
    pub(crate) event: String,
    pub(crate) fields: serde_json::Map<String, serde_json::Value>,
}

struct PrivateLinkCapabilityInner {
    client: reqwest::Client,
    origin: Url,
    ingest_path: String,
    opener: Arc<PrivateLinkOpener>,
    registration: Arc<RegistrationCoordinator>,
}

#[derive(Clone)]
pub(crate) struct PrivateLinkCapability {
    inner: Arc<PrivateLinkCapabilityInner>,
}

impl PrivateLinkCapability {
    pub(crate) fn is_registered(&self) -> bool {
        self.inner.opener.generation() != 0
    }

    pub(crate) fn facts(&self) -> LinkFacts {
        self.inner.opener.facts.clone()
    }

    async fn send(&self, builder: RequestBuilder, timeout: Duration) -> LinkOutcome {
        let generation = self.inner.opener.generation();
        match builder.timeout(timeout).send().await {
            Ok(response) => {
                let status = response.status();
                if status == StatusCode::UNAUTHORIZED {
                    return LinkOutcome::Unauthorized { generation };
                }
                if status == StatusCode::FORBIDDEN {
                    return LinkOutcome::Forbidden;
                }
                if status.is_client_error() {
                    return LinkOutcome::LocalRejected { status };
                }
                match response.bytes().await {
                    Ok(body) => LinkOutcome::Success {
                        status,
                        body: body.to_vec(),
                    },
                    Err(_) => LinkOutcome::TransportUnavailable,
                }
            }
            Err(_) => LinkOutcome::TransportUnavailable,
        }
    }

    pub(crate) async fn ingest(&self, form: multipart::Form) -> LinkOutcome {
        let Ok(url) = confine_path(&self.inner.origin, &self.inner.ingest_path) else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(self.inner.client.post(url).multipart(form), INGEST_TIMEOUT)
            .await
    }

    pub(crate) async fn list_day(&self, day: &str) -> LinkOutcome {
        if day.len() != 8 || !day.bytes().all(|byte| byte.is_ascii_digit()) {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        }
        let path = format!(
            "{}/segments/{day}",
            self.inner.ingest_path.trim_end_matches('/')
        );
        let Ok(url) = confine_path(&self.inner.origin, &path) else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(self.inner.client.get(url), LISTING_TIMEOUT).await
    }

    pub(crate) async fn send_event(&self, body: EventBody) -> LinkOutcome {
        let mut fields = body.fields;
        fields.insert("tract".into(), serde_json::Value::String(body.tract));
        fields.insert("event".into(), serde_json::Value::String(body.event));
        let Ok(url) = confine_path(&self.inner.origin, EVENT_PATH) else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(self.inner.client.post(url).json(&fields), EVENT_TIMEOUT)
            .await
    }

    pub(crate) async fn report_unauthorized(&self, generation: u64) -> RepairOutcome {
        self.inner.registration.repair(generation).await
    }
}

struct RegistrationCoordinator {
    client: reqwest::Client,
    origin: Url,
    opener: Arc<PrivateLinkOpener>,
    config_root: PathBuf,
    credential_instance_id: String,
    name: Mutex<String>,
    hostname: String,
    platform: String,
    version: String,
    single_flight: tokio::sync::Mutex<()>,
}

impl RegistrationCoordinator {
    async fn repair(&self, generation: u64) -> RepairOutcome {
        let current = self.opener.generation();
        if current != generation {
            return RepairOutcome::AlreadySuperseded {
                generation: current,
            };
        }
        let _guard = self.single_flight.lock().await;
        let current = self.opener.generation();
        if current != generation {
            return RepairOutcome::AlreadySuperseded {
                generation: current,
            };
        }
        let label = self.name.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let prior_key_prefix = self
            .opener
            .registered_key()
            .map(|key| key.chars().take(8).collect::<String>())
            .unwrap_or_default();
        let body = serde_json::json!({
            "hostname": self.hostname,
            "label": label,
            "platform": self.platform,
            "stream_type": "desktop",
            "version": self.version,
        });
        let Ok(url) = confine_path(&self.origin, "/app/observer/register") else {
            return RepairOutcome::InvalidRegistration;
        };
        let response = match self
            .client
            .post(url)
            .header(
                REGISTRATION_MARKER_HEADER_NAME,
                REGISTRATION_MARKER_HEADER_VALUE,
            )
            .json(&body)
            .timeout(REGISTRATION_TIMEOUT)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return RepairOutcome::TransportUnavailable,
        };
        if response.status() == StatusCode::FORBIDDEN {
            let reason_code = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| {
                    body.get("reason_code")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            return RepairOutcome::GuardRefused { reason_code };
        }
        if response.status() != StatusCode::OK {
            return RepairOutcome::TransportUnavailable;
        }
        #[derive(Deserialize)]
        struct RegistrationResponse {
            key: String,
            prefix: String,
            name: String,
            ingest_url: String,
            protocol_version: u64,
        }
        let Ok(response) = response.json::<RegistrationResponse>().await else {
            return RepairOutcome::InvalidRegistration;
        };
        let observer = ObserverState {
            credential_instance_id: self.credential_instance_id.clone(),
            key: response.key,
            prefix: response.prefix,
            name: response.name,
            ingest_url: response.ingest_url,
            protocol_version: response.protocol_version,
        };
        if !observer_is_valid(
            &observer,
            &self.credential_instance_id,
            &observer.name,
            &self.origin,
        ) {
            return RepairOutcome::InvalidRegistration;
        }
        if self.opener.registered_key().as_deref() == Some(observer.key.as_str()) {
            return RepairOutcome::AlreadySuperseded {
                generation: self.opener.generation(),
            };
        }
        match persist_and_publish_observer(
            &self.config_root,
            &self.credential_instance_id,
            &observer.name,
            &self.origin,
            &observer,
            &self.opener,
            RegistrationCommitFaults {
                observer: &NoWriteFault,
                config: &NoWriteFault,
            },
        ) {
            Ok(generation) => {
                let new_key_prefix = observer.key.chars().take(8).collect::<String>();
                tracing::warn!(
                    outcome = "recovered",
                    name = observer.name,
                    old_key_prefix = prior_key_prefix,
                    new_key_prefix,
                    recovery_generation = generation,
                    "Journal identity repair completed"
                );
                *self.name.lock().unwrap_or_else(|p| p.into_inner()) = observer.name.clone();
                RepairOutcome::Repaired {
                    generation,
                    name: observer.name,
                }
            }
            Err(PrivateStateError::RegistrationInvalid) => RepairOutcome::InvalidRegistration,
            Err(_) => RepairOutcome::PersistenceFailed,
        }
    }
}

pub(crate) struct PrivateLinkOwner {
    capability: PrivateLinkCapability,
    open_journal_target: Arc<OpenJournalTarget>,
    open_journal_access: Option<OpenJournalAccess>,
    session: Option<PrivateLinkSession>,
    facts: LinkFacts,
}

impl PrivateLinkOwner {
    pub(crate) fn capability(&self) -> PrivateLinkCapability {
        self.capability.clone()
    }

    pub(crate) fn open_journal_capability(&self) -> OpenJournalCapability {
        OpenJournalCapability {
            target: Arc::downgrade(&self.open_journal_target),
        }
    }

    pub(crate) fn install_open_journal_access(&mut self, access: OpenJournalAccess) {
        let capability = self.open_journal_capability();
        access.install(capability);
        self.open_journal_access = Some(access);
    }

    fn close_open_journal(&self) {
        let capability = self.open_journal_capability();
        capability.close();
        if let Some(access) = &self.open_journal_access {
            access.clear(&capability);
        }
    }

    pub(crate) async fn shutdown(mut self) -> Result<(), PrivateStateError> {
        self.close_open_journal();
        self.facts.owner_lost();
        self.session.take().unwrap().shutdown().await
    }

    #[cfg(test)]
    pub(crate) async fn shutdown_with_join_probe(
        mut self,
        joined: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Result<(), PrivateStateError> {
        self.close_open_journal();
        self.facts.owner_lost();
        self.session
            .take()
            .unwrap()
            .shutdown_with_join_probe(joined, release)
            .await
    }

    #[cfg(test)]
    fn loopback_addr(&self) -> std::net::SocketAddr {
        format!(
            "{}:{}",
            self.capability.inner.origin.host_str().unwrap(),
            self.capability.inner.origin.port().unwrap()
        )
        .parse()
        .unwrap()
    }

    #[cfg(test)]
    async fn register(&self, body: &serde_json::Value) -> LinkOutcome {
        let Ok(url) = confine_path(&self.capability.inner.origin, "/app/observer/register") else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.capability
            .send(
                self.capability
                    .inner
                    .client
                    .post(url)
                    .header(
                        REGISTRATION_MARKER_HEADER_NAME,
                        REGISTRATION_MARKER_HEADER_VALUE,
                    )
                    .json(body),
                REGISTRATION_TIMEOUT,
            )
            .await
    }

    #[cfg(test)]
    pub(crate) async fn register_for_test(&self, body: &serde_json::Value) -> LinkOutcome {
        self.register(body).await
    }
}

impl Drop for PrivateLinkOwner {
    fn drop(&mut self) {
        self.close_open_journal();
        self.facts.owner_lost();
    }
}

#[cfg(test)]
pub(crate) async fn start_private_link_owner(
    config_root: &Path,
    credential: Credential,
    expected_name: &str,
) -> Result<PrivateLinkOwner, PrivateStateError> {
    let session = start_private_link_session(config_root, credential, expected_name).await?;
    finish_owner_start(session).await
}

async fn finish_owner_start(
    mut session: PrivateLinkSession,
) -> Result<PrivateLinkOwner, PrivateStateError> {
    let capability = session.capability("/app/observer/ingest".to_owned());
    if session.opener.generation() == 0 {
        match capability.report_unauthorized(0).await {
            RepairOutcome::Repaired { .. } | RepairOutcome::AlreadySuperseded { .. } => {}
            RepairOutcome::PersistenceFailed | RepairOutcome::TransportUnavailable => {
                capability.facts().publish(LinkFact::TransportUnavailable);
            }
            RepairOutcome::GuardRefused { .. } | RepairOutcome::InvalidRegistration => {
                return Err(PrivateStateError::RegistrationInvalid);
            }
        }
    }
    let facts = capability.facts();
    let bootstrap_target = session
        .bootstrap_target
        .take()
        .ok_or(PrivateStateError::BootstrapFailed)?;
    Ok(PrivateLinkOwner {
        capability,
        open_journal_target: Arc::new(OpenJournalTarget {
            gate: Mutex::new(OpenJournalGate::Open(bootstrap_target)),
        }),
        open_journal_access: None,
        session: Some(session),
        facts,
    })
}

pub(crate) async fn start_private_link_owner_with_lock(
    state_lock: PrivateStateLock,
    credential: Credential,
    expected_name: &str,
    hostname: String,
    platform: String,
    version: String,
    facts: LinkFacts,
) -> Result<PrivateLinkOwner, PrivateStateError> {
    let config_root = state_lock.root().to_path_buf();
    let session = start_private_link_session_inner(
        &config_root,
        credential,
        expected_name,
        SessionStartOptions {
            state_lock: Some(state_lock),
            persistence_fault: Arc::new(NoWriteFault),
            #[cfg(test)]
            test_capture: SessionTestCapture::default(),
            shared_facts: Some(facts),
            registration_metadata: Some(RegistrationMetadata {
                hostname,
                platform,
                version,
            }),
        },
    )
    .await?;
    finish_owner_start(session).await
}

#[cfg(test)]
pub(crate) async fn start_registered_private_link_for_test(
    credential: Credential,
    expected_name: &str,
    key: &str,
    ingest_path: &str,
) -> (tempfile::TempDir, PrivateLinkOwner) {
    let temp = tempfile::tempdir().unwrap();
    let session = start_private_link_session(temp.path(), credential.clone(), expected_name)
        .await
        .unwrap();
    publish_observer_registration(
        &session,
        &ObserverState {
            credential_instance_id: credential.instance_id,
            key: key.to_owned(),
            prefix: "contract".to_owned(),
            name: expected_name.to_owned(),
            ingest_url: ingest_path.to_owned(),
            protocol_version: 2,
        },
    )
    .unwrap();
    (temp, finish_owner_start(session).await.unwrap())
}

impl PrivateLinkSession {
    pub(crate) fn capability(&self, ingest_path: String) -> PrivateLinkCapability {
        PrivateLinkCapability {
            inner: Arc::new(PrivateLinkCapabilityInner {
                client: self.client.clone(),
                origin: self.origin.clone(),
                ingest_path,
                opener: self.opener.clone(),
                registration: self.registration.clone(),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) async fn register_for_test(&self, body: &serde_json::Value) -> LinkOutcome {
        let Ok(url) = confine_path(&self.origin, "/app/observer/register") else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        PrivateLinkCapability {
            inner: Arc::new(PrivateLinkCapabilityInner {
                client: self.client.clone(),
                origin: self.origin.clone(),
                ingest_path: String::new(),
                opener: self.opener.clone(),
                registration: self.registration.clone(),
            }),
        }
        .send(
            self.client
                .post(url)
                .header(
                    REGISTRATION_MARKER_HEADER_NAME,
                    REGISTRATION_MARKER_HEADER_VALUE,
                )
                .json(body),
            REGISTRATION_TIMEOUT,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn request(
        &self,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder, PrivateStateError> {
        let url = confine_path(&self.origin, relative_path)?;
        Ok(self.client.request(method, url).timeout(EVENT_TIMEOUT))
    }

    pub(crate) async fn shutdown(self) -> Result<(), PrivateStateError> {
        self.shutdown_inner(None).await
    }

    async fn shutdown_inner(
        self,
        #[cfg(test)] join_probe: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        #[cfg(not(test))] _join_probe: Option<()>,
    ) -> Result<(), PrivateStateError> {
        let status = self.handle.shutdown_and_wait().await;
        #[cfg(test)]
        if let Some((joined, release)) = join_probe {
            joined.notify_one();
            release.notified().await;
        }
        if status.listener_active || status.active_requests != 0 {
            return Err(PrivateStateError::ShutdownFailed);
        }
        if self.token_persistence.failed() {
            return Err(PrivateStateError::TokenPersistenceFailed);
        }
        Ok(())
    }

    #[cfg(test)]
    async fn shutdown_with_join_probe(
        self,
        joined: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Result<(), PrivateStateError> {
        self.shutdown_inner(Some((joined, release))).await
    }

    #[cfg(test)]
    fn publish_observer(&self, observer: &ObserverState) -> Result<u64, PrivateStateError> {
        persist_and_publish_observer(
            self._state_lock.root(),
            &self.credential_instance_id,
            &self.expected_name,
            &self.origin,
            observer,
            &self.opener,
            RegistrationCommitFaults {
                observer: &NoWriteFault,
                config: &NoWriteFault,
            },
        )
    }

    #[cfg(test)]
    fn publish_observer_with_fault(
        &self,
        observer: &ObserverState,
        fault: &dyn DurableWriteFault,
    ) -> Result<u64, PrivateStateError> {
        persist_and_publish_observer(
            self._state_lock.root(),
            &self.credential_instance_id,
            &self.expected_name,
            &self.origin,
            observer,
            &self.opener,
            RegistrationCommitFaults {
                observer: fault,
                config: &NoWriteFault,
            },
        )
    }

    #[cfg(test)]
    fn publish_observer_with_faults(
        &self,
        observer: &ObserverState,
        observer_fault: &dyn DurableWriteFault,
        config_fault: &dyn DurableWriteFault,
    ) -> Result<u64, PrivateStateError> {
        persist_and_publish_observer(
            self._state_lock.root(),
            &self.credential_instance_id,
            &self.expected_name,
            &self.origin,
            observer,
            &self.opener,
            RegistrationCommitFaults {
                observer: observer_fault,
                config: config_fault,
            },
        )
    }
}

#[cfg(test)]
pub(crate) fn publish_observer_registration(
    session: &PrivateLinkSession,
    observer: &ObserverState,
) -> Result<u64, PrivateStateError> {
    session.publish_observer(observer)
}

struct TokenPersistence {
    config_root: PathBuf,
    credential: Mutex<Credential>,
    failed: Mutex<bool>,
    fault: Arc<dyn DurableWriteFault>,
    transport_unavailable: Arc<AtomicBool>,
    facts: LinkFacts,
}

impl TokenPersistence {
    fn new(
        config_root: PathBuf,
        credential: Credential,
        fault: Arc<dyn DurableWriteFault>,
        transport_unavailable: Arc<AtomicBool>,
        facts: LinkFacts,
    ) -> (Arc<Self>, TokenPersistHook) {
        let state = Arc::new(Self {
            config_root,
            credential: Mutex::new(credential),
            failed: Mutex::new(false),
            fault,
            transport_unavailable,
            facts,
        });
        let hook_state = state.clone();
        let hook: TokenPersistHook = Arc::new(move |token, expires_at| {
            // The admission guard encloses the only pinned refresh path and this
            // synchronous hook, so no dialed carrier escapes before durability.
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
            self.transport_unavailable.store(true, Ordering::Release);
            self.facts.publish(LinkFact::TokenPersistenceFailure);
            self.facts.publish(LinkFact::TransportUnavailable);
        }
    }

    fn failed(&self) -> bool {
        *self.failed.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
pub(crate) async fn start_private_link_session(
    config_root: &Path,
    credential: Credential,
    expected_name: &str,
) -> Result<PrivateLinkSession, PrivateStateError> {
    start_private_link_session_inner(
        config_root,
        credential,
        expected_name,
        SessionStartOptions::default(),
    )
    .await
}

#[cfg(test)]
#[derive(Default)]
struct SessionTestCapture {
    capability: Option<Arc<Mutex<Option<String>>>>,
}

struct RegistrationMetadata {
    hostname: String,
    platform: String,
    version: String,
}

struct SessionStartOptions {
    state_lock: Option<PrivateStateLock>,
    persistence_fault: Arc<dyn DurableWriteFault>,
    #[cfg(test)]
    test_capture: SessionTestCapture,
    shared_facts: Option<LinkFacts>,
    registration_metadata: Option<RegistrationMetadata>,
}

impl Default for SessionStartOptions {
    fn default() -> Self {
        Self {
            state_lock: None,
            persistence_fault: Arc::new(NoWriteFault),
            #[cfg(test)]
            test_capture: SessionTestCapture::default(),
            shared_facts: None,
            registration_metadata: None,
        }
    }
}

async fn start_private_link_session_inner(
    config_root: &Path,
    credential: Credential,
    expected_name: &str,
    options: SessionStartOptions,
) -> Result<PrivateLinkSession, PrivateStateError> {
    let state_lock = match options.state_lock {
        Some(state_lock) => state_lock,
        None => PrivateStateLock::acquire(config_root)?,
    };
    let config_root = state_lock.root().to_path_buf();
    let facts = options.shared_facts.unwrap_or_default();
    let paths = private_config_paths(&config_root);
    let sanitized = match sanitize_link_authority(&paths) {
        Ok(config) => config,
        Err(error) => {
            facts.publish(LinkFact::ConfigSanitationFailed);
            facts.publish(LinkFact::TransportUnavailable);
            return Err(config_persist_error(error));
        }
    };
    let expected_name = if sanitized.stream.is_empty() {
        save_linked_stream(&paths, expected_name)
            .map_err(config_persist_error)?
            .stream
    } else {
        sanitized.stream
    };
    let credential_instance_id = credential.instance_id.clone();
    let transport_unavailable = Arc::new(AtomicBool::new(false));
    let endpoint_hosts = credential
        .endpoints
        .iter()
        .map(|endpoint| endpoint.host.clone())
        .collect();
    let (token_persistence, hook) = TokenPersistence::new(
        config_root.clone(),
        credential.clone(),
        options.persistence_fault,
        transport_unavailable.clone(),
        facts.clone(),
    );
    let transport = if credential.endpoints.is_empty() {
        TransportClient::new_relay_only(credential, Some(hook))
    } else {
        TransportClient::new(credential, Some(hook))
    }
    .map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let opener = Arc::new(PrivateLinkOpener::new(
        transport,
        transport_unavailable,
        facts.clone(),
    ));
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
                REGISTRATION_MARKER_HEADER_NAME,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
        max_request_body_bytes: MAX_REQUEST_BODY_BYTES as usize,
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
    #[cfg(test)]
    if let Some(capture) = options.test_capture.capability {
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
        .get(&bootstrap_url)
        .timeout(BOOTSTRAP_TIMEOUT)
        .send()
        .await
        .map_err(|_| PrivateStateError::BootstrapFailed)?;
    if response.status() != StatusCode::FOUND {
        handle.begin_shutdown();
        return Err(PrivateStateError::BootstrapFailed);
    }
    facts.publish(LinkFact::ListenerReady);
    match load_observer(
        &config_root,
        &credential_instance_id,
        &expected_name,
        &origin,
    ) {
        Ok(Some(observer)) => {
            opener.set_registered(&observer)?;
        }
        Ok(None) => {}
        Err(PrivateStateError::MalformedObserver) => {
            facts.publish(LinkFact::PrivateStateInvalid);
        }
        Err(error) => return Err(error),
    }
    let registration_metadata =
        options
            .registration_metadata
            .unwrap_or_else(|| RegistrationMetadata {
                hostname: expected_name.clone(),
                platform: "linux".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            });
    let registration = Arc::new(RegistrationCoordinator {
        client: client.clone(),
        origin: origin.clone(),
        opener: opener.clone(),
        config_root: config_root.clone(),
        credential_instance_id: credential_instance_id.clone(),
        name: Mutex::new(expected_name.clone()),
        hostname: registration_metadata.hostname,
        platform: registration_metadata.platform,
        version: registration_metadata.version,
        single_flight: tokio::sync::Mutex::new(()),
    });
    Ok(PrivateLinkSession {
        client,
        origin,
        opener,
        registration,
        handle,
        token_persistence,
        bootstrap_target: Some(bootstrap_url),
        _state_lock: state_lock,
        #[cfg(test)]
        credential_instance_id,
        #[cfg(test)]
        expected_name,
        #[cfg(test)]
        facts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_file::DurableWriteStage;
    use crate::private_link_test_peer::PrivateLinkPeer;
    use crate::sync_health::{ProcessEpoch, SyncFacts, load_facts_with_liveness, save_facts};
    use spl_transport::credential::EndpointAddr;
    use std::{
        io::Cursor,
        net::TcpListener,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        process::Command,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    async fn raw_local_request(port: u16, request: String) -> Vec<u8> {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        response
    }

    fn base64url_no_pad(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut output = String::new();
        let mut index = 0;
        while index + 3 <= input.len() {
            let chunk = ((input[index] as u32) << 16)
                | ((input[index + 1] as u32) << 8)
                | input[index + 2] as u32;
            output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
            output.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
            output.push(TABLE[(chunk & 0x3f) as usize] as char);
            index += 3;
        }
        match input.len() - index {
            1 => {
                let chunk = (input[index] as u32) << 16;
                output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
                output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
            }
            2 => {
                let chunk = ((input[index] as u32) << 16) | ((input[index + 1] as u32) << 8);
                output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
                output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
                output.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
            }
            _ => {}
        }
        output
    }

    fn test_jwt(exp: i64) -> String {
        let claims = format!(r#"{{"iat":{},"exp":{exp}}}"#, exp - 3600);
        format!(
            "{}.{}.sig",
            base64url_no_pad(b"{}"),
            base64url_no_pad(claims.as_bytes())
        )
    }

    async fn read_http_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        request
    }

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

    fn assert_registered_auth(request: &crate::private_link_test_peer::PeerRequest, key: &str) {
        for (name, expected) in [
            (OBSERVER_HEADER_NAME, key.to_owned()),
            ("authorization", format!("Bearer {key}")),
            (PROTOCOL_VERSION_HEADER_NAME, "2".to_owned()),
        ] {
            let values = request
                .headers
                .iter()
                .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .collect::<Vec<_>>();
            assert_eq!(values, vec![expected.as_str()], "{name}");
        }
    }

    async fn start_peer_session(peer: &PrivateLinkPeer) -> (tempfile::TempDir, PrivateLinkSession) {
        let temp = tempfile::tempdir().unwrap();
        let credential = peer.credential();
        let state = ObserverState {
            credential_instance_id: credential.instance_id.clone(),
            ..observer("/ingest")
        };
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        publish_observer_registration(&session, &state).unwrap();
        (temp, session)
    }

    async fn assert_load_rejection_keeps_opener_unregistered(state: ObserverState) {
        let temp = tempfile::tempdir().unwrap();
        persist_observer(temp.path(), &state).unwrap();
        let peer = PrivateLinkPeer::start().await;
        let opener = PrivateLinkOpener::new(
            TransportClient::new(peer.credential(), None).unwrap(),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        );
        let loaded = load_observer(
            temp.path(),
            "instance",
            "stream",
            &Url::parse("http://127.0.0.1:1").unwrap(),
        )
        .unwrap();
        if let Some(observer) = loaded {
            opener.set_registered(&observer).unwrap();
        }
        assert!(opener.proxy_headers(&[]).is_err());
        peer.shutdown().await;
    }

    macro_rules! opener_rejection_test {
        ($name:ident, $state:expr) => {
            #[tokio::test]
            async fn $name() {
                assert_load_rejection_keeps_opener_unregistered($state).await;
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
            key: "bad\u{1}key".into(),
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

    async fn assert_publish_rejection(mutate: impl FnOnce(&mut ObserverState)) {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let credential = peer.credential();
        let mut state = ObserverState {
            credential_instance_id: credential.instance_id.clone(),
            ..observer("/ingest")
        };
        mutate(&mut state);
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        assert!(matches!(
            publish_observer_registration(&session, &state),
            Err(PrivateStateError::RegistrationInvalid)
        ));
        assert!(!temp.path().join(OBSERVER_FILENAME).exists());
        assert_eq!(
            session
                .request(Method::GET, "/still-unregistered")
                .unwrap()
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_GATEWAY
        );
        assert!(peer.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    macro_rules! publish_rejection_test {
        ($name:ident, $field:ident, $value:expr) => {
            #[tokio::test]
            async fn $name() {
                assert_publish_rejection(|state| state.$field = $value.into()).await;
            }
        };
    }

    publish_rejection_test!(
        publish_rejects_credential_instance_mismatch,
        credential_instance_id,
        "other"
    );
    publish_rejection_test!(publish_rejects_name_mismatch, name, "other");
    #[tokio::test]
    async fn publish_rejects_unsupported_protocol() {
        assert_publish_rejection(|state| state.protocol_version = 3).await;
    }
    publish_rejection_test!(publish_rejects_empty_key, key, "");
    publish_rejection_test!(publish_rejects_unsafe_key, key, "bad\u{1}key");
    publish_rejection_test!(publish_rejects_empty_prefix, prefix, "");
    publish_rejection_test!(publish_rejects_empty_name, name, "");
    publish_rejection_test!(publish_rejects_empty_ingest_path, ingest_url, "");
    publish_rejection_test!(publish_rejects_relative_ingest_path, ingest_url, "relative");
    publish_rejection_test!(
        publish_rejects_scheme_relative_ingest_path,
        ingest_url,
        "//host/x"
    );
    publish_rejection_test!(publish_rejects_raw_ingest_traversal, ingest_url, "/a/../b");
    publish_rejection_test!(
        publish_rejects_encoded_ingest_traversal,
        ingest_url,
        "/a/%2e%2e/b"
    );
    publish_rejection_test!(
        publish_rejects_mixed_encoded_ingest_traversal,
        ingest_url,
        "/a/%2E./b"
    );
    publish_rejection_test!(publish_rejects_encoded_ingest_slash, ingest_url, "/a/%2f/b");
    publish_rejection_test!(
        publish_rejects_encoded_ingest_backslash,
        ingest_url,
        "/a/%5c/b"
    );
    publish_rejection_test!(
        publish_rejects_double_encoded_ingest_traversal,
        ingest_url,
        "/a/%252e%252e/b"
    );
    publish_rejection_test!(publish_rejects_query_ingest_path, ingest_url, "/a?q");
    publish_rejection_test!(publish_rejects_fragment_ingest_path, ingest_url, "/a#f");
    publish_rejection_test!(publish_rejects_backslash_ingest_path, ingest_url, "/a\\b");

    async fn assert_redacted_setup_rejection(bytes: &[u8]) {
        let temp = tempfile::tempdir().unwrap();
        let pairer = FakePairer {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Some(credential()),
        };
        let error = setup_with_pairer(&pairer, temp.path(), "device", Cursor::new(bytes.to_vec()))
            .await
            .unwrap_err();
        let material = String::from_utf8_lossy(bytes);
        if !material.is_empty() {
            assert!(!format!("{error}").contains(material.as_ref()));
            assert!(!format!("{error:?}").contains(material.as_ref()));
        }
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
        assert!(!temp.path().join(OBSERVER_FILENAME).exists());
    }

    #[tokio::test]
    async fn pair_input_empty_is_rejected() {
        assert_redacted_setup_rejection(b"").await;
    }
    #[tokio::test]
    async fn pair_input_invalid_utf8_is_rejected() {
        assert_redacted_setup_rejection(b"\xff").await;
    }
    #[tokio::test]
    async fn pair_input_embedded_whitespace_is_rejected() {
        assert_redacted_setup_rejection(b"pair link").await;
    }
    #[tokio::test]
    async fn pair_input_leading_whitespace_is_rejected() {
        assert_redacted_setup_rejection(b" pair").await;
    }
    #[test]
    fn pair_input_trailing_spaces_and_tabs_are_rejected() {
        assert!(matches!(
            read_pair_link(Cursor::new(b"pair \t")),
            Err(PrivateStateError::PairInputInvalid)
        ));
    }
    #[tokio::test]
    async fn pair_input_multiple_line_endings_are_rejected() {
        assert_redacted_setup_rejection(b"pair\nother\n").await;
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
    fn pair_input_exactly_4096_unterminated_bytes_is_accepted() {
        assert_eq!(
            read_pair_link(Cursor::new(vec![b'a'; 4096])).unwrap().len(),
            4096
        );
    }
    #[test]
    fn pair_input_4095_bytes_plus_lf_is_accepted() {
        let mut input = vec![b'a'; 4095];
        input.push(b'\n');
        assert_eq!(read_pair_link(Cursor::new(input)).unwrap().len(), 4095);
    }
    #[test]
    fn pair_input_4094_bytes_plus_crlf_is_accepted() {
        let mut input = vec![b'a'; 4094];
        input.extend_from_slice(b"\r\n");
        assert_eq!(read_pair_link(Cursor::new(input)).unwrap().len(), 4094);
    }
    #[test]
    fn pair_input_4096_bytes_plus_lf_is_rejected() {
        let mut input = vec![b'a'; 4096];
        input.push(b'\n');
        assert!(matches!(
            read_pair_link(Cursor::new(input)),
            Err(PrivateStateError::PairInputInvalid)
        ));
    }
    #[test]
    fn pair_input_4095_bytes_plus_crlf_is_rejected() {
        let mut input = vec![b'a'; 4095];
        input.extend_from_slice(b"\r\n");
        assert!(matches!(
            read_pair_link(Cursor::new(input)),
            Err(PrivateStateError::PairInputInvalid)
        ));
    }
    #[tokio::test]
    async fn pair_input_4097_bytes_is_rejected() {
        assert_redacted_setup_rejection(&vec![b'a'; 4097]).await;
    }

    struct FakePairer {
        calls: Arc<AtomicUsize>,
        result: Option<Credential>,
    }

    struct SanitizedConfigPairer {
        config_path: PathBuf,
        calls: Arc<AtomicUsize>,
        result: Credential,
    }

    impl Pairer for SanitizedConfigPairer {
        fn pair<'a>(
            &'a self,
            _link: &'a str,
            _device_label: &'a str,
            _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Credential, PrivateStateError>> + Send + 'a>>
        {
            Box::pin(async move {
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&self.config_path).unwrap()).unwrap();
                assert!(value.get("server_url").is_none());
                assert!(value.get("key").is_none());
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.result.clone())
            })
        }
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

    struct BlockingDirSyncFault {
        stages: Arc<Mutex<Vec<DurableWriteStage>>>,
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl DurableWriteFault for BlockingDirSyncFault {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            self.stages.lock().unwrap().push(stage);
            if stage == DurableWriteStage::DirSync {
                self.entered
                    .send(())
                    .map_err(|_| io::Error::other("test observer dropped"))?;
                self.release
                    .lock()
                    .unwrap()
                    .recv()
                    .map_err(|_| io::Error::other("test release dropped"))?;
            }
            Ok(())
        }
    }

    async fn blocking_admission_observation() -> (Vec<DurableWriteStage>, Vec<&'static str>) {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let transport_unavailable = Arc::new(AtomicBool::new(false));
        let facts = LinkFacts::default();
        let opener = Arc::new(PrivateLinkOpener::new(
            TransportClient::new(peer.credential(), None).unwrap(),
            transport_unavailable.clone(),
            facts.clone(),
        ));
        let stages = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (_persistence, hook) = TokenPersistence::new(
            temp.path().to_path_buf(),
            peer.credential(),
            Arc::new(BlockingDirSyncFault {
                stages: stages.clone(),
                entered: entered_tx,
                release: Mutex::new(release_rx),
            }),
            transport_unavailable,
            facts,
        );
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();

        let relay_opener = opener.clone();
        let relay_completed = completed_tx.clone();
        let relay = tokio::spawn(async move {
            let result = relay_opener
                .admit_dial(async move {
                    hook("refreshed-token", 456);
                    Ok("relay")
                })
                .await;
            relay_completed.send(result.unwrap()).unwrap();
        });
        tokio::task::spawn_blocking(move || entered_rx.recv().unwrap())
            .await
            .unwrap();

        let direct_opener = opener.clone();
        let direct_completed = completed_tx.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let direct = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            let result = direct_opener.admit_dial(async { Ok("direct") }).await;
            direct_completed.send(result.unwrap()).unwrap();
        });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        assert!(matches!(
            completed_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        release_tx.send(()).unwrap();
        relay.await.unwrap();
        direct.await.unwrap();
        let completion_order = vec![completed_rx.recv().unwrap(), completed_rx.recv().unwrap()];
        peer.shutdown().await;
        let recorded = stages.lock().unwrap().clone();
        (recorded, completion_order)
    }

    #[tokio::test]
    async fn real_transport_dial_waits_for_admission_guard() {
        let peer = PrivateLinkPeer::start().await;
        let opener = Arc::new(PrivateLinkOpener::new(
            TransportClient::new(peer.credential(), None).unwrap(),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let guard = opener.admission.lock().await;
        let dial = tokio::spawn({
            let opener = opener.clone();
            async move { opener.dial_carrier().await }
        });
        tokio::task::yield_now().await;
        assert!(!dial.is_finished());
        drop(guard);
        assert!(dial.await.unwrap().is_ok());
        peer.shutdown().await;
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
    fn read_only_probe_reports_live_and_unlocked_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let held = PrivateStateLock::acquire(temp.path()).unwrap();
        let lock_path = temp.path().join(PRIVATE_STATE_LOCK_FILENAME);
        let before = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            PrivateStateLock::try_probe(temp.path()).unwrap(),
            PrivateStateLockLiveness::LiveOwner
        );
        let after = fs::metadata(&lock_path).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.permissions().mode(), after.permissions().mode());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        drop(held);
        assert_eq!(
            PrivateStateLock::try_probe(temp.path()).unwrap(),
            PrivateStateLockLiveness::NoLiveOwner
        );
        let reacquired = PrivateStateLock::acquire(temp.path()).unwrap();
        drop(reacquired);
    }

    #[test]
    fn read_only_probe_is_conservative_for_missing_invalid_and_malformed_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert_eq!(
            PrivateStateLock::try_probe(&missing).unwrap(),
            PrivateStateLockLiveness::NoLiveOwner
        );

        fs::create_dir(&missing).unwrap();
        assert!(matches!(
            PrivateStateLock::try_probe(&missing),
            Err(PrivateStateProbeError::InvalidTarget)
        ));
        fs::set_permissions(&missing, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            PrivateStateLock::try_probe(&missing).unwrap(),
            PrivateStateLockLiveness::NoLiveOwner
        );
        let lock_path = missing.join(PRIVATE_STATE_LOCK_FILENAME);
        fs::write(&lock_path, b"unchanged").unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::read(&lock_path).unwrap();
        assert!(matches!(
            PrivateStateLock::try_probe(&missing),
            Err(PrivateStateProbeError::InvalidTarget)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), before);
        assert!(matches!(
            probe_lock_table("not a lock table", 0, 0),
            Err(PrivateStateProbeError::LocksMalformed)
        ));
        assert_eq!(
            probe_lock_table("1: POSIX ADVISORY READ 1 00:00:1 0 EOF\n", 0, 2).unwrap(),
            PrivateStateLockLiveness::NoLiveOwner
        );
    }

    #[test]
    fn liveness_sampling_races_correct_on_the_next_sample() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let connected = LinkFactState {
            carrier_proven: true,
            observer_registered: true,
            ..Default::default()
        };
        save_facts(
            &state,
            &SyncFacts {
                pending_confirmed: Some(0),
                link: Some(connected.clone()),
                link_epoch: Some(ProcessEpoch::for_test(3)),
                ..Default::default()
            },
        )
        .unwrap();

        let held = PrivateStateLock::acquire(temp.path()).unwrap();
        let sampled_live = PrivateStateLock::try_probe(temp.path()).unwrap();
        drop(held);
        assert_eq!(
            load_facts_with_liveness(&state, sampled_live).link,
            Some(connected.clone())
        );
        assert!(
            load_facts_with_liveness(&state, PrivateStateLock::try_probe(temp.path()).unwrap())
                .link
                .is_none()
        );

        let sampled_absent = PrivateStateLock::try_probe(temp.path()).unwrap();
        let held = PrivateStateLock::acquire(temp.path()).unwrap();
        assert!(
            load_facts_with_liveness(&state, sampled_absent)
                .link
                .is_none()
        );
        assert_eq!(
            load_facts_with_liveness(&state, PrivateStateLock::try_probe(temp.path()).unwrap())
                .link,
            Some(connected)
        );
        drop(held);
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

    #[test]
    fn lock_verification_rejects_unexpected_mode() {
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join(PRIVATE_STATE_LOCK_FILENAME);
        fs::write(&lock_path, b"").unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o640)).unwrap();
        let file = File::open(lock_path).unwrap();
        assert!(matches!(
            verify_private_lock(&file),
            Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Lock
            })
        ));
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
    async fn second_runtime_session_is_lock_contended() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let first = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            start_private_link_session(temp.path(), peer.credential(), "stream").await,
            Err(PrivateStateError::LockContended)
        ));
        first.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn session_restores_durable_observer_into_real_opener() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let durable = ObserverState {
            credential_instance_id: peer.credential().instance_id,
            ..observer("/ingest")
        };
        persist_observer(temp.path(), &durable).unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        session
            .request(Method::GET, "/restored")
            .unwrap()
            .send()
            .await
            .unwrap();
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_registered_auth(&requests[0], "observer-key");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn bridge_reuses_one_carrier_across_registration_transition() {
        let peer = PrivateLinkPeer::start().await;
        let credential_instance_id = peer.credential().instance_id;
        peer.enqueue_response(200, b"{}".to_vec());
        peer.enqueue_response(200, b"{}".to_vec());
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert_eq!(
            session
                .request(Method::POST, "/app/observer/register")
                .unwrap()
                .header(
                    REGISTRATION_MARKER_HEADER_NAME,
                    REGISTRATION_MARKER_HEADER_VALUE,
                )
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let first_generation = publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id,
                ..observer("/app/observer/ingest")
            },
        )
        .unwrap();
        assert_eq!(first_generation, 1);
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
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/app/observer/register");
        assert!(requests[0].body.is_empty());
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER_NAME))
                .map(|(_, value)| value.as_str()),
            Some("2")
        );
        assert!(!requests[0].headers.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case(REGISTRATION_MARKER_HEADER_NAME)
                || name.eq_ignore_ascii_case(OBSERVER_HEADER_NAME)
                || name.eq_ignore_ascii_case("authorization")
        }));
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, "/registered");
        assert!(requests[1].body.is_empty());
        assert_registered_auth(&requests[1], "observer-key");
        assert_eq!(peer.accepted_carriers(), 1);
        let capability = session.capability("/app/observer/ingest".to_owned());
        let second_generation = publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: session.credential_instance_id.clone(),
                key: "replacement-key".into(),
                ..observer("/app/observer/ingest")
            },
        )
        .unwrap();
        assert_eq!(second_generation, 2);
        assert!(matches!(
            capability.report_unauthorized(first_generation).await,
            RepairOutcome::AlreadySuperseded { generation: 2 }
        ));
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

    #[test]
    fn registration_marker_malformed_and_duplicate_forms_are_rejected_locally() {
        let epoch = AuthEpoch {
            generation: 0,
            state: OpenerAuth::Unregistered,
        };
        for headers in [
            vec![(REGISTRATION_MARKER_HEADER_NAME.to_owned(), String::new())],
            vec![(REGISTRATION_MARKER_HEADER_NAME.to_owned(), " 1".to_owned())],
            vec![(REGISTRATION_MARKER_HEADER_NAME.to_owned(), "1 ".to_owned())],
            vec![(REGISTRATION_MARKER_HEADER_NAME.to_owned(), "2".to_owned())],
            vec![
                (
                    REGISTRATION_MARKER_HEADER_NAME.to_owned(),
                    REGISTRATION_MARKER_HEADER_VALUE.to_owned(),
                ),
                (
                    REGISTRATION_MARKER_HEADER_NAME.to_owned(),
                    REGISTRATION_MARKER_HEADER_VALUE.to_owned(),
                ),
            ],
        ] {
            assert!(proxy_headers_for_epoch(&headers, &epoch).is_err());
        }
    }

    #[test]
    fn registration_marker_is_stripped_before_forwarding() {
        let epoch = AuthEpoch {
            generation: 0,
            state: OpenerAuth::Unregistered,
        };
        let headers = proxy_headers_for_epoch(
            &[(
                REGISTRATION_MARKER_HEADER_NAME.to_owned(),
                REGISTRATION_MARKER_HEADER_VALUE.to_owned(),
            )],
            &epoch,
        )
        .unwrap();
        assert_eq!(
            headers,
            vec![(PROTOCOL_VERSION_HEADER_NAME.to_owned(), "2".to_owned())]
        );
    }

    #[tokio::test]
    async fn unregistered_data_routes_never_dial() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let response = session
            .request(Method::GET, "/data")
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(peer.accepted_carriers(), 0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn capability_rejects_admin_path_query_and_route_substitution() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session) = start_peer_session(&peer).await;
        let capability = session.capability("/app/observer/ingest".to_owned());
        for day in ["", "2026010", "202601011", "202601?1", "../20260101"] {
            assert!(matches!(
                capability.list_day(day).await,
                LinkOutcome::LocalRejected {
                    status: StatusCode::BAD_REQUEST
                }
            ));
        }
        assert!(peer.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn typed_unauthorized_report_is_only_recovery_surface() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session) = start_peer_session(&peer).await;
        let capability = session.capability("/ingest".to_owned());
        assert!(matches!(
            capability.report_unauthorized(0).await,
            RepairOutcome::AlreadySuperseded { generation: 1 }
        ));
        assert!(matches!(
            capability.report_unauthorized(1).await,
            RepairOutcome::TransportUnavailable
        ));
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn caller_reserved_auth_headers_are_rejected_before_dial() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session) = start_peer_session(&peer).await;
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

    async fn session_with_capability(
        peer: &PrivateLinkPeer,
    ) -> (tempfile::TempDir, PrivateLinkSession, String) {
        let temp = tempfile::tempdir().unwrap();
        let captured = Arc::new(Mutex::new(None));
        let session = start_private_link_session_inner(
            temp.path(),
            peer.credential(),
            "stream",
            SessionStartOptions {
                test_capture: SessionTestCapture {
                    capability: Some(captured.clone()),
                },
                ..SessionStartOptions::default()
            },
        )
        .await
        .unwrap();
        let capability = captured.lock().unwrap().clone().unwrap();
        (temp, session, capability)
    }

    #[tokio::test]
    async fn chunked_unknown_length_is_local_400_before_carrier() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session, capability) = session_with_capability(&peer).await;
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability}\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ndata\r\n0\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 400"));
        assert_eq!(peer.accepted_carriers(), 0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[test]
    fn ingest_uses_300_second_policy() {
        assert_eq!(INGEST_TIMEOUT, Duration::from_secs(300));
        assert_eq!(MAX_REQUEST_BODY_BYTES, 64 * 1024 * 1024);
    }

    #[tokio::test]
    async fn large_upload_staging_is_credit_bounded() {
        let peer = PrivateLinkPeer::start().await;
        peer.hold_request_credit();
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session) = start_peer_session(&peer).await;
        let body = vec![b'x'; spl_core::mux::INITIAL_WINDOW * 2 + 1];
        let form = reqwest::multipart::Form::new()
            .part("files", reqwest::multipart::Part::bytes(body.clone()));
        let upload = tokio::spawn({
            let capability = session.capability("/ingest".to_owned());
            async move { capability.ingest(form).await }
        });
        peer.wait_for_request_staged_at_least(spl_core::mux::INITIAL_WINDOW)
            .await;
        assert_eq!(peer.max_request_staged(), spl_core::mux::INITIAL_WINDOW);
        assert!(!upload.is_finished());
        peer.release_request_credit();
        assert!(matches!(upload.await.unwrap(), LinkOutcome::Success { .. }));
        peer.wait_for_requests(1).await;
        let request = &peer.requests()[0];
        assert!(
            request
                .body
                .windows(body.len())
                .any(|window| window == body)
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn chunked_rejection_releases_staging_and_allows_small_request() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, br#"{"items":[],"total":0}"#.to_vec());
        let (_temp, session, capability_cookie) = session_with_capability(&peer).await;
        let state = ObserverState {
            credential_instance_id: peer.credential().instance_id,
            ..observer("/ingest")
        };
        publish_observer_registration(&session, &state).unwrap();
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability_cookie}\r\nTransfer-Encoding: chunked\r\n\r\n4\r\ndata\r\n0\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 400"));
        assert!(matches!(
            session
                .capability("/ingest".to_owned())
                .list_day("20260101")
                .await,
            LinkOutcome::Success { .. }
        ));
        assert_eq!(peer.requests().len(), 1);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn declared_over_limit_is_local_413_before_carrier() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session, capability) = session_with_capability(&peer).await;
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability}\r\nContent-Length: {}\r\n\r\n",
                MAX_REQUEST_BODY_BYTES + 1
            ),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 413"));
        assert_eq!(peer.accepted_carriers(), 0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn absent_content_length_with_trailing_bytes_dials_but_forwards_empty_body() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session, capability) = session_with_capability(&peer).await;
        let state = ObserverState {
            credential_instance_id: peer.credential().instance_id,
            ..observer("/ingest")
        };
        publish_observer_registration(&session, &state).unwrap();
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability}\r\n\r\ntrailing"
            ),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn loopback_client_does_not_follow_upstream_redirects() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(302, Vec::new());
        let (_temp, session) = start_peer_session(&peer).await;
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
        let (_temp, session) = start_peer_session(&peer).await;
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
        let (_temp, session) = start_peer_session(&peer).await;
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
        let peer = PrivateLinkPeer::start().await;
        let credential_instance_id = peer.credential().instance_id;
        let prior = ObserverState {
            credential_instance_id: credential_instance_id.clone(),
            ..observer("/prior")
        };
        persist_observer(temp.path(), &prior).unwrap();
        let prior_bytes = fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap();
        for _ in 0..6 {
            peer.enqueue_response(200, b"{}".to_vec());
        }
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let next = ObserverState {
            credential_instance_id,
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
                session
                    .publish_observer_with_fault(&next, &FailStage(stage))
                    .is_err()
            );
            let current_bytes = fs::read(temp.path().join(OBSERVER_FILENAME)).unwrap();
            if stage == DurableWriteStage::DirSync {
                assert!(
                    current_bytes == prior_bytes
                        || current_bytes == serde_json::to_vec(&next).unwrap()
                );
                assert!(
                    serde_json::from_slice::<ObserverState>(&current_bytes).is_ok(),
                    "directory-sync failure must leave one complete observer value"
                );
            } else {
                assert_eq!(current_bytes, prior_bytes);
            }
            let decoded: ObserverState = serde_json::from_slice(&current_bytes).unwrap();
            assert!(decoded == prior || decoded == next);
            session
                .request(Method::GET, "/still-unregistered")
                .unwrap()
                .send()
                .await
                .unwrap();
        }
        publish_observer_registration(&session, &next).unwrap();
        session
            .request(Method::GET, "/registered-after-durable")
            .unwrap()
            .send()
            .await
            .unwrap();
        let requests = peer.requests();
        assert_eq!(requests.len(), 6);
        for request in &requests[..5] {
            assert_registered_auth(request, "observer-key");
        }
        assert_registered_auth(&requests[5], "new-observer-key");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn sanitation_precedes_pairer_bridge_carrier_and_peer() {
        let temp = tempfile::tempdir().unwrap();
        let traps = LegacyNetworkTraps::bind();
        let peer = PrivateLinkPeer::start().await;
        fs::write(
            temp.path().join("config.json"),
            format!(
                r#"{{"server_url":"{}","key":"secret","stream":"stream"}}"#,
                traps.configured_origin()
            ),
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let pairer = SanitizedConfigPairer {
            config_path: temp.path().join("config.json"),
            calls: calls.clone(),
            result: peer.credential(),
        };
        setup_with_pairer(&pairer, temp.path(), "device", Cursor::new(b"pair"))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let config_path = temp.path().join("config.json");
        let mut reacquired: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        reacquired["server_url"] = serde_json::json!(traps.configured_origin());
        reacquired["key"] = serde_json::json!("reintroduced");
        fs::write(&config_path, serde_json::to_vec(&reacquired).unwrap()).unwrap();

        peer.enqueue_response(200, br#"{"items":[],"total":0}"#.to_vec());
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let sanitized: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        assert!(sanitized.get("server_url").is_none());
        assert!(sanitized.get("key").is_none());
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                ..observer("/ingest")
            },
        )
        .unwrap();
        assert!(matches!(
            session
                .capability("/ingest".to_owned())
                .list_day("20260101")
                .await,
            LinkOutcome::Success { .. }
        ));
        assert_eq!(peer.accepted_carriers(), 1);
        assert_eq!(peer.requests().len(), 1);
        traps.assert_zero_connections();
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    struct LegacyNetworkTraps {
        configured: TcpListener,
        default: Option<TcpListener>,
    }

    impl LegacyNetworkTraps {
        fn bind() -> Self {
            let configured = TcpListener::bind("127.0.0.1:0").unwrap();
            configured.set_nonblocking(true).unwrap();
            let default = TcpListener::bind("127.0.0.1:5015").ok();
            if let Some(default) = &default {
                default.set_nonblocking(true).unwrap();
            } else {
                eprintln!(
                    "criterion 12 note: opportunistic default-listener trap did not execute because the address is already in use"
                );
            }
            Self {
                configured,
                default,
            }
        }

        fn configured_origin(&self) -> String {
            format!("http://{}", self.configured.local_addr().unwrap())
        }

        fn assert_zero_connections(&self) {
            let listeners = std::iter::once(&self.configured).chain(self.default.iter());
            for listener in listeners {
                assert!(matches!(
                    listener.accept(),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock
                ));
            }
        }
    }

    #[tokio::test]
    async fn migration_never_contacts_legacy_origin_or_default_listener() {
        let traps = LegacyNetworkTraps::bind();
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("config.json"),
            format!(
                r#"{{"server_url":"{}","key":"secret","stream":"stream"}}"#,
                traps.configured_origin()
            ),
        )
        .unwrap();
        let peer = PrivateLinkPeer::start().await;
        let session = start_private_link_session(temp.path(), peer.credential(), "ignored")
            .await
            .unwrap();
        traps.assert_zero_connections();
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn already_sanitized_and_reacquired_authority_are_sanitized_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let first = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        first.shutdown().await.unwrap();
        let path = temp.path().join("config.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["server_url"] = serde_json::json!("http://127.0.0.1:9");
        value["key"] = serde_json::json!("reintroduced");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let second = start_private_link_session(temp.path(), peer.credential(), "ignored")
            .await
            .unwrap();
        let rewritten: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(rewritten.get("server_url").is_none());
        assert!(rewritten.get("key").is_none());
        assert_eq!(rewritten["stream"], "stream");
        second.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn observer_then_config_commit_restart_matrix() {
        for config_file in [false, true] {
            for stage in [
                DurableWriteStage::Create,
                DurableWriteStage::Write,
                DurableWriteStage::Fsync,
                DurableWriteStage::Rename,
                DurableWriteStage::DirSync,
            ] {
                let temp = tempfile::tempdir().unwrap();
                let peer = PrivateLinkPeer::start().await;
                let credential = peer.credential();
                let state = ObserverState {
                    credential_instance_id: credential.instance_id.clone(),
                    ..observer("/ingest")
                };
                let session = start_private_link_session(temp.path(), credential.clone(), "stream")
                    .await
                    .unwrap();
                let result = if config_file {
                    session.publish_observer_with_faults(&state, &NoWriteFault, &FailStage(stage))
                } else {
                    session.publish_observer_with_faults(&state, &FailStage(stage), &NoWriteFault)
                };
                assert!(result.is_err(), "{config_file} {stage:?}");
                assert_eq!(session.opener.generation(), 0);
                session.shutdown().await.unwrap();

                let restarted = start_private_link_session(temp.path(), credential, "stream")
                    .await
                    .unwrap();
                let persisted = load_observer(
                    temp.path(),
                    &restarted.credential_instance_id,
                    "stream",
                    &restarted.origin,
                )
                .unwrap();
                assert_eq!(
                    restarted.opener.generation(),
                    u64::from(persisted.is_some())
                );
                restarted.shutdown().await.unwrap();
                peer.shutdown().await;
            }
        }
    }

    #[tokio::test]
    async fn restart_mismatch_starts_unregistered_without_deleting_observer() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let credential = peer.credential();
        persist_observer(
            temp.path(),
            &ObserverState {
                credential_instance_id: credential.instance_id.clone(),
                ..observer("/ingest")
            },
        )
        .unwrap();
        fs::write(temp.path().join("config.json"), r#"{"stream":"other"}"#).unwrap();
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        assert_eq!(session.opener.generation(), 0);
        assert!(temp.path().join(OBSERVER_FILENAME).exists());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn registration_commit_never_overwrites_external_referent() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let credential = peer.credential();
        let state = ObserverState {
            credential_instance_id: credential.instance_id.clone(),
            ..observer("/ingest")
        };
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        let referent = temp.path().join("external.json");
        fs::write(&referent, "external").unwrap();
        symlink(&referent, temp.path().join(OBSERVER_FILENAME)).unwrap();
        assert!(matches!(
            publish_observer_registration(&session, &state),
            Err(PrivateStateError::InvalidTarget {
                kind: PrivateTargetKind::Observer
            })
        ));
        assert_eq!(fs::read_to_string(referent).unwrap(), "external");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn restart_never_mixes_old_and_new_authority() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let credential = peer.credential();
        let mut state = observer("/ingest");
        state.credential_instance_id = credential.instance_id.clone();
        state.name = "old".into();
        persist_observer(temp.path(), &state).unwrap();
        fs::write(temp.path().join("config.json"), r#"{"stream":"new"}"#).unwrap();
        let session = start_private_link_session(temp.path(), credential, "ignored")
            .await
            .unwrap();
        assert_eq!(session.expected_name, "new");
        assert_eq!(session.opener.generation(), 0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn failed_two_file_publish_preserves_usable_prior_credential() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session) = start_peer_session(&peer).await;
        let prior_generation = session.opener.generation();
        let next = ObserverState {
            credential_instance_id: session.credential_instance_id.clone(),
            key: "next-key".into(),
            ..observer("/next")
        };
        assert!(
            session
                .publish_observer_with_faults(
                    &next,
                    &NoWriteFault,
                    &FailStage(DurableWriteStage::Write),
                )
                .is_err()
        );
        assert_eq!(session.opener.generation(), prior_generation);
        session
            .request(Method::GET, "/uses-prior")
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_registered_auth(&peer.requests()[0], "observer-key");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn credential_instance_mismatch_rejects_prior_observer() {
        let temp = tempfile::tempdir().unwrap();
        persist_observer(temp.path(), &observer("/ingest")).unwrap();
        let peer = PrivateLinkPeer::start().await;
        let mut credential = peer.credential();
        credential.instance_id = "replacement-instance".into();
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        assert_eq!(session.opener.generation(), 0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn published_registration_authenticates_first_data_request() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let temp = tempfile::tempdir().unwrap();
        let credential = peer.credential();
        let state = ObserverState {
            credential_instance_id: credential.instance_id.clone(),
            ..observer("/ingest")
        };
        let session = start_private_link_session(temp.path(), credential, "stream")
            .await
            .unwrap();
        publish_observer_registration(&session, &state).unwrap();
        session
            .request(Method::GET, "/first-data")
            .unwrap()
            .send()
            .await
            .unwrap();
        assert_registered_auth(&peer.requests()[0], "observer-key");
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
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_direct_and_relay_dials_wait_for_blocking_token_hook() {
        let (_stages, completion_order) = blocking_admission_observation().await;
        assert_eq!(completion_order, ["relay", "direct"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_carrier_escapes_before_token_directory_sync() {
        let (stages, completion_order) = blocking_admission_observation().await;
        assert_eq!(
            stages,
            [
                DurableWriteStage::Create,
                DurableWriteStage::Write,
                DurableWriteStage::Fsync,
                DurableWriteStage::Rename,
                DurableWriteStage::DirSync,
            ]
        );
        assert_eq!(completion_order, ["relay", "direct"]);
    }

    async fn assert_token_failure(stage: DurableWriteStage) {
        let temp = tempfile::tempdir().unwrap();
        let prior = credential();
        persist_credential(temp.path(), &prior).unwrap();
        let prior_bytes = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        let peer = PrivateLinkPeer::start().await;
        let session = start_private_link_session_inner(
            temp.path(),
            peer.credential(),
            "stream",
            SessionStartOptions {
                persistence_fault: Arc::new(RecordingFault {
                    stages: Arc::new(Mutex::new(Vec::new())),
                    fail: Some(stage),
                }),
                ..SessionStartOptions::default()
            },
        )
        .await
        .unwrap();
        session.token_persistence.persist("failed-refresh", 999);
        let facts = session.facts.snapshot();
        assert!(facts.token_persistence_failure);
        assert!(facts.transport_unavailable);
        assert!(session.opener.dial_carrier().await.is_err());
        assert_eq!(peer.accepted_carriers(), 0);
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
    async fn token_write_failure_drops_carrier_and_latches() {
        assert_token_failure(DurableWriteStage::Write).await;
    }

    #[tokio::test]
    async fn token_fsync_failure_drops_carrier_and_latches() {
        assert_token_failure(DurableWriteStage::Fsync).await;
    }

    #[tokio::test]
    async fn owner_shutdown_joins_bridge_and_closes_listener() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(
            200,
            serde_json::json!({
                "key": "K",
                "name": "stream",
                "prefix": "prefix",
                "ingest_url": "/app/observer/ingest",
                "protocol_version": 2
            })
            .to_string(),
        );
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let address = owner.loopback_addr();
        assert!(tokio::net::TcpStream::connect(address).await.is_ok());
        owner.shutdown().await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn abrupt_owner_loss_clears_live_proofs_and_publishes_transport_loss() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(
            200,
            serde_json::json!({
                "key": "K",
                "name": "stream",
                "prefix": "prefix",
                "ingest_url": "/app/observer/ingest",
                "protocol_version": 2
            })
            .to_string(),
        );
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let facts = owner.capability().facts();
        assert!(facts.snapshot().listener_ready);
        drop(owner);
        assert_eq!(
            facts.snapshot(),
            LinkFactState {
                transport_unavailable: true,
                ..Default::default()
            }
        );
        peer.shutdown().await;
    }

    #[test]
    fn owner_generation_reset_clears_every_published_fact() {
        let facts = LinkFacts::default();
        for fact in [
            LinkFact::PairingRequired,
            LinkFact::PrivateStateInvalid,
            LinkFact::ConfigSanitationFailed,
            LinkFact::ListenerReady,
            LinkFact::CarrierProven,
            LinkFact::ObserverRegistered,
            LinkFact::TransportUnavailable,
            LinkFact::TerminalRevocation,
            LinkFact::TokenPersistenceFailure,
        ] {
            facts.publish(fact);
        }
        facts.begin_owner_generation();
        assert_eq!(facts.snapshot(), LinkFactState::default());
    }

    fn open_journal_fixture(target: &str) -> (Arc<OpenJournalTarget>, OpenJournalCapability) {
        let target = Arc::new(OpenJournalTarget {
            gate: Mutex::new(OpenJournalGate::Open(target.to_owned())),
        });
        let capability = OpenJournalCapability {
            target: Arc::downgrade(&target),
        };
        (target, capability)
    }

    #[test]
    fn open_journal_epoch_matrix_never_retargets_stale_capabilities() {
        let access = OpenJournalAccess::default();
        assert!(!access.available());
        assert!(access.open().is_err());

        let (first_target, first) = open_journal_fixture("first-private-target");
        access.install(first.clone());
        assert!(access.available());
        let calls = Arc::new(Mutex::new(Vec::new()));
        first
            .open_with({
                let calls = Arc::clone(&calls);
                move |target| {
                    calls.lock().unwrap().push(target.to_owned());
                    Ok(())
                }
            })
            .unwrap();

        first.close();
        assert!(!access.available());
        assert!(first.open_with(|_| panic!("closed target opened")).is_err());
        access.clear(&first);
        drop(first_target);
        assert!(
            first
                .open_with(|_| panic!("dropped target opened"))
                .is_err()
        );

        let (replacement_target, replacement) = open_journal_fixture("replacement-private-target");
        access.install(replacement.clone());
        access.clear(&first);
        assert!(access.available());
        assert!(
            first
                .open_with(|_| panic!("stale clone retargeted"))
                .is_err()
        );
        replacement
            .open_with({
                let calls = Arc::clone(&calls);
                move |target| {
                    calls.lock().unwrap().push(target.to_owned());
                    Ok(())
                }
            })
            .unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            ["first-private-target", "replacement-private-target"]
        );
        drop(replacement_target);
        assert!(!access.available());
    }

    #[test]
    fn open_journal_open_and_shutdown_linearize_on_one_gate() {
        let (_target, capability) = open_journal_fixture("private-target");
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let opener = {
            let capability = capability.clone();
            let calls = Arc::clone(&calls);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                capability.open_with(|_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.wait();
                    release.wait();
                    Ok(())
                })
            })
        };
        entered.wait();
        let closer = {
            let capability = capability.clone();
            std::thread::spawn(move || capability.close())
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.wait();
        assert!(opener.join().unwrap().is_ok());
        closer.join().unwrap();
        assert!(capability.open_with(|_| panic!("post-close open")).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn opener_panic_poison_closes_capability_fail_closed() {
        let (_target, capability) = open_journal_fixture("private-target");
        let panic = std::panic::catch_unwind({
            let capability = capability.clone();
            move || {
                let _ = capability.open_with(|_| -> Result<(), ()> {
                    panic!("injected opener panic");
                });
            }
        });
        assert!(panic.is_err());
        assert!(!capability.available());
        let calls = AtomicUsize::new(0);
        assert!(
            capability
                .open_with(|_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        capability.close();
        assert!(!capability.available());
    }

    #[test]
    fn open_journal_debug_is_always_redacted() {
        let (_target, capability) =
            open_journal_fixture("http://127.0.0.1:49152/?cap=capability-secret");
        assert_eq!(
            format!("{capability:?}"),
            "OpenJournalCapability(<redacted>)"
        );
    }

    #[test]
    fn open_journal_secrets_never_enter_owner_or_serialized_surfaces_at_any_epoch() {
        let secrets = [
            "http://127.0.0.1:49152/?cap=capability-cookie-sentinel",
            "49152",
            "capability-cookie-sentinel",
            "credential-sentinel",
            "observer-key-sentinel",
            "relay-token-sentinel",
        ];
        let access = OpenJournalAccess::default();
        let before = format!("available={}", access.available());
        let (_target, capability) = open_journal_fixture(secrets[0]);
        access.install(capability.clone());
        let live = format!("{capability:?};available={}", access.available());
        capability.close();
        let after = format!("{capability:?};available={}", access.available());

        let temp = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Default::default()
        };
        let config_json = serde_json::to_string(&config).unwrap();
        let facts = SyncFacts {
            link_epoch: Some(ProcessEpoch::for_test(4)),
            ..Default::default()
        };
        save_facts(temp.path(), &facts).unwrap();
        let state_json = fs::read_to_string(temp.path().join("sync_health.json")).unwrap();
        let health = crate::sync_health::derive_health(&facts, 0.0, 600.0);
        let clipboard = crate::clipboard::agent_instructions(
            &config.config_path().display().to_string(),
            &config.captures_dir().display().to_string(),
        );
        let introspection = include_str!("../testdata/introspection/observer1.xml").to_owned();
        let unavailable = crate::desktop_component::DesktopComponent::new(config)
            .perform_desktop_command(crate::tray::TrayCommand::OpenJournal)
            .unwrap_err();
        let outputs = [
            before,
            live,
            after,
            config_json,
            state_json,
            health.cli,
            health.doctor_detail,
            health.dbus,
            clipboard,
            introspection,
            unavailable,
        ];
        for output in outputs {
            for secret in secrets {
                assert!(
                    !output.contains(secret),
                    "owner or serialized surface disclosed a private Open Journal value"
                );
            }
        }
    }

    #[tokio::test]
    async fn owner_shutdown_closes_active_bridge_stream() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(
            200,
            serde_json::json!({
                "key": "K",
                "name": "stream",
                "prefix": "prefix",
                "ingest_url": "/app/observer/ingest",
                "protocol_version": 2
            })
            .to_string(),
        );
        peer.enqueue_response(200, br#"{"items":[],"total":0}"#.to_vec());
        let response_gate = Arc::new(AtomicBool::new(false));
        peer.gate_queued_response_nonblocking(1, response_gate);
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let request = tokio::spawn({
            let capability = owner.capability();
            async move { capability.list_day("20260101").await }
        });
        peer.wait_for_requests(2).await;
        assert!(!request.is_finished());
        tokio::time::timeout(Duration::from_secs(1), owner.shutdown())
            .await
            .expect("shutdown must close active bridge streams")
            .unwrap();
        assert!(matches!(
            request.await.unwrap(),
            LinkOutcome::TransportUnavailable
        ));
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn refreshed_relay_token_is_durable_before_owner_shutdown_returns() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let relay_origin = format!("http://{}", listener.local_addr().unwrap());
        let refreshed = test_jwt(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600,
        );
        let relay_refreshed = refreshed.clone();
        let relay = tokio::spawn(async move {
            let (mut refresh, _) = listener.accept().await.unwrap();
            let request = read_http_head(&mut refresh).await;
            assert!(
                request.starts_with(b"POST /token/refresh HTTP/1.1"),
                "{}",
                String::from_utf8_lossy(&request)
            );
            let body = serde_json::json!({"device_token": relay_refreshed}).to_string();
            refresh
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let (mut dial, _) = listener.accept().await.unwrap();
            let _ = read_http_head(&mut dial).await;
            dial.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        });
        let mut credential = peer.credential();
        credential.endpoints.clear();
        credential.relay_origin = Some(relay_origin);
        credential.device_token = Some(test_jwt(1));
        credential.device_token_expires_at = Some(1);
        let owner = start_private_link_owner(temp.path(), credential, "stream")
            .await
            .unwrap();
        relay.await.unwrap();
        let persisted = load_credential(temp.path()).unwrap().unwrap();
        assert_eq!(persisted.device_token.as_deref(), Some(refreshed.as_str()));
        owner.shutdown().await.unwrap();
        let persisted_after_shutdown = load_credential(temp.path()).unwrap().unwrap();
        assert_eq!(
            persisted_after_shutdown.device_token.as_deref(),
            Some(refreshed.as_str())
        );
        peer.shutdown().await;
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
        let temp = tempfile::tempdir().unwrap();
        let capability = Arc::new(Mutex::new(None));
        let session = start_private_link_session_inner(
            temp.path(),
            paired,
            "stream",
            SessionStartOptions {
                test_capture: SessionTestCapture {
                    capability: Some(capability.clone()),
                },
                ..SessionStartOptions::default()
            },
        )
        .await
        .unwrap();
        let capability = capability.lock().unwrap().clone().unwrap();
        let observer_key = "observer-key-sentinel";
        let registered = ObserverState {
            credential_instance_id: session.credential_instance_id.clone(),
            key: observer_key.into(),
            ..observer("/ingest")
        };
        publish_observer_registration(&session, &registered).unwrap();
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
    fn private_link_types_enforce_authority_and_ownership_boundaries() {
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
        assert!(!DebugProbe::<PrivateLinkCapability>(PhantomData).probe());
        assert!(CloneProbe::<PrivateLinkCapability>(PhantomData).probe());
        assert!(!SerializeProbe::<PrivateLinkCapability>(PhantomData).probe());
        assert!(!DebugProbe::<PrivateLinkOwner>(PhantomData).probe());
        assert!(!CloneProbe::<PrivateLinkOwner>(PhantomData).probe());
        assert!(!SerializeProbe::<PrivateLinkOwner>(PhantomData).probe());
    }
}
