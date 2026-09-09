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
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(test)]
use reqwest::Method;
use reqwest::{RequestBuilder, StatusCode, Url, multipart};
use spl_core::bridge::{BridgeNames, RequestHead, RequestHeaderPolicy};
use spl_core::pairlink::{self, ParsedPairLink};
use spl_transport::credential::Credential;
use spl_transport::{
    TransportError,
    client::{DialedCarrier, TokenPersistHook, TransportClient},
    journal_bridge::{
        BridgePolicy, CapabilityGate, CarrierOpener, JournalBridgeConfig, JournalBridgeHandle,
    },
};

use crate::config::{ConfigPaths, sanitize_link_authority, save_linked_stream};
use crate::private_file::{
    DurableWriteFault, NoWriteFault, PrivateFileError, atomic_write_bytes,
    atomic_write_bytes_guarded, atomic_write_bytes_with_fault, ensure_private_directory,
    open_regular_readonly,
};

pub(crate) const CREDENTIALS_FILENAME: &str = "credentials.json";
const PRIVATE_STATE_LOCK_FILENAME: &str = ".solstone-linux.private-state.lock";
pub(crate) const PRIVATE_STATE_READY_LOCK_FILENAME: &str =
    ".solstone-linux.private-state.ready.lock";
const MAX_PAIR_LINK_BYTES: u64 = 4096;
#[cfg(test)]
pub(crate) const DIRECT_PAIR_LINK_FOR_TEST: &str =
    "0G0QY00004EYJ001081G81860W40J2GB1G6GW3X0M6HA7955MTKTHADANEPAVBNF";
pub(crate) const MAX_REQUEST_BODY_BYTES: u64 = 128 * 1024 * 1024;
const LOOPBACK_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LAN_CARRIER_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const INGEST_TIMEOUT: Duration = Duration::from_secs(300);
const LISTING_TIMEOUT: Duration = Duration::from_secs(60);
#[allow(dead_code)]
const SYSTEM_STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const SYSTEM_STATUS_PATH: &str = "/api/system/status";
pub(crate) const OBSERVER_HEADER_NAME: &str = "x-solstone-observer";
pub(crate) const PROTOCOL_VERSION_HEADER_NAME: &str = "x-solstone-protocol-version";
const ROUTE_CLASS_MARKER_HEADER_NAME: &str = "x-solstone-linux-route-class";
const INGEST_V3_ROUTE_CLASS: &str = "ingest-v3";
const INGEST_PATH: &str = "/app/devices/ingest";
const JOURNAL_MEDIA_PATH_PREFIX: &str = "/app/transcripts/api/serve_file/";

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
    TokenPersistenceFailed,
    ShutdownFailed,
    HealthInitializationFailed,
}

impl fmt::Display for PrivateStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedCredential => formatter.write_str("MalformedCredential"),
            Self::InvalidTarget { kind } => write!(formatter, "InvalidTarget({kind:?})"),
            Self::Io { operation, source } => {
                write!(formatter, "Io({operation:?}, {:?})", source.kind())
            }
            Self::LockContended => formatter.write_str("LockContended"),
            Self::PairInputInvalid => formatter.write_str("PairInputInvalid"),
            Self::PairingFailed => formatter.write_str("PairingFailed"),
            Self::BridgeUnavailable => formatter.write_str("BridgeUnavailable"),
            Self::BootstrapFailed => formatter.write_str("BootstrapFailed"),
            Self::TokenPersistenceFailed => formatter.write_str("TokenPersistenceFailed"),
            Self::ShutdownFailed => formatter.write_str("ShutdownFailed"),
            Self::HealthInitializationFailed => formatter.write_str("HealthInitializationFailed"),
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
    readiness_file: Option<File>,
    canonical_root: PathBuf,
    handle_count: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateStateLockLiveness {
    LiveOwner,
    LiveOwnerNotReady,
    NoLiveOwner,
}

#[derive(Debug)]
pub(crate) enum PrivateStateProbeError {
    InvalidTarget,
    Inspect,
    LocksUnavailable,
    LocksMalformed,
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
        let readiness_stat = rustix::fs::openat(
            &root,
            PRIVATE_STATE_READY_LOCK_FILENAME,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .ok()
        .and_then(|descriptor| rustix::fs::fstat(File::from(descriptor)).ok())
        .filter(|readiness_stat| {
            rustix::fs::FileType::from_raw_mode(readiness_stat.st_mode)
                == rustix::fs::FileType::RegularFile
                && rustix::fs::Mode::from_raw_mode(readiness_stat.st_mode) == expected_mode
                && readiness_stat.st_uid == rustix::process::geteuid().as_raw()
        });
        let locks = fs::read_to_string("/proc/locks")
            .map_err(|_| PrivateStateProbeError::LocksUnavailable)?;
        if !probe_lock_table(&locks, stat.st_dev, stat.st_ino)? {
            return Ok(PrivateStateLockLiveness::NoLiveOwner);
        }
        let readiness_live = match readiness_stat {
            Some(readiness_stat) => {
                probe_lock_table(&locks, readiness_stat.st_dev, readiness_stat.st_ino)?
            }
            None => false,
        };
        if readiness_live {
            Ok(PrivateStateLockLiveness::LiveOwner)
        } else {
            Ok(PrivateStateLockLiveness::LiveOwnerNotReady)
        }
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
            readiness_file: None,
            canonical_root,
            handle_count: Arc::new(AtomicUsize::new(1)),
        })
    }

    pub(crate) fn mark_ready(&mut self) -> Result<(), PrivateStateError> {
        let root_descriptor = rustix::fs::openat(
            rustix::fs::CWD,
            &self.canonical_root,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| PrivateStateError::HealthInitializationFailed)?;
        let descriptor = rustix::fs::openat(
            &root_descriptor,
            PRIVATE_STATE_READY_LOCK_FILENAME,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CREATE,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|_| PrivateStateError::HealthInitializationFailed)?;
        let file = File::from(descriptor);
        rustix::fs::fchmod(&file, rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR)
            .map_err(|_| PrivateStateError::HealthInitializationFailed)?;
        verify_private_lock(&file).map_err(|_| PrivateStateError::HealthInitializationFailed)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| PrivateStateError::HealthInitializationFailed)?;
        self.readiness_file = Some(file);
        Ok(())
    }

    pub(crate) fn root(&self) -> &Path {
        &self.canonical_root
    }

    pub(crate) fn try_clone(&self) -> Result<Self, PrivateStateError> {
        let file = self
            ._file
            .try_clone()
            .map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Lock,
                source,
            })?;
        let readiness_file = self
            .readiness_file
            .as_ref()
            .map(File::try_clone)
            .transpose()
            .map_err(|source| PrivateStateError::Io {
                operation: PrivateIoOperation::Lock,
                source,
            })?;
        self.handle_count.fetch_add(1, Ordering::AcqRel);
        Ok(Self {
            _file: file,
            readiness_file,
            canonical_root: self.canonical_root.clone(),
            handle_count: Arc::clone(&self.handle_count),
        })
    }
}

impl Drop for PrivateStateLock {
    fn drop(&mut self) {
        if self.handle_count.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        if let Some(readiness_file) = &self.readiness_file
            && let Err(error) =
                rustix::fs::flock(readiness_file, rustix::fs::FlockOperation::Unlock)
        {
            tracing::error!(%error, "Failed to release private state readiness lock");
        }
        if let Err(error) = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock) {
            tracing::error!(%error, "Failed to release private state lock");
        }
    }
}

fn linux_device_major(device: u64) -> u64 {
    ((device >> 8) & 0xfff) | ((device >> 32) & 0xffff_f000)
}

fn linux_device_minor(device: u64) -> u64 {
    (device & 0xff) | ((device >> 12) & 0xffff_ff00)
}

fn probe_lock_table(locks: &str, device: u64, inode: u64) -> Result<bool, PrivateStateProbeError> {
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
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_private_lock(file: &File) -> Result<(), PrivateStateError> {
    let stat = rustix::fs::fstat(file).map_err(|source| PrivateStateError::Io {
        operation: PrivateIoOperation::Inspect,
        source: source.into(),
    })?;
    let expected_mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
        || rustix::fs::Mode::from_raw_mode(stat.st_mode) != expected_mode
        || stat.st_uid != rustix::process::geteuid().as_raw()
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

const MAX_PAIRING_CLIENT_LABEL_BYTES: usize = 253;

fn pairing_ceremony_identity(device_label: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut fields = serde_json::Map::new();
    if (1..=MAX_PAIRING_CLIENT_LABEL_BYTES).contains(&device_label.len()) {
        fields.insert(
            "client_label".to_owned(),
            serde_json::Value::String(device_label.to_owned()),
        );
    }
    fields.insert(
        "platform".to_owned(),
        serde_json::Value::String("linux".to_owned()),
    );
    fields
}

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
    state_dir: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_stream(config_root, state_dir, device_label, None, input).await
}

pub(crate) async fn setup_with_stream<R: Read>(
    config_root: &Path,
    state_dir: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(
        &SplPairer,
        config_root,
        state_dir,
        device_label,
        stream,
        input,
    )
    .await
}

#[cfg(test)]
async fn setup_with_pairer<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    state_dir: &Path,
    device_label: &str,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(pairer, config_root, state_dir, device_label, None, input).await
}

async fn setup_with_pairer_and_stream<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    state_dir: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream_with_fault(
        pairer,
        config_root,
        state_dir,
        device_label,
        stream,
        input,
        None,
    )
    .await
}

async fn setup_with_pairer_and_stream_with_fault<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    state_dir: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
    credential_fault: Option<&dyn DurableWriteFault>,
) -> Result<(), PrivateStateError> {
    let state_lock = PrivateStateLock::acquire(config_root)?;
    sanitize_link_authority(&private_config_paths(state_lock.root()))
        .map_err(config_persist_error)?;
    if let Some(stream) = stream {
        save_linked_stream(&private_config_paths(state_lock.root()), stream)
            .map_err(config_persist_error)?;
    }
    let link = read_pair_link(input)?;
    // Carrier selection is governed by the shared pair-link parser, before
    // pairing returns any credential fields. A pairer performs the ceremony;
    // it cannot reclassify the link or infer carrier selection from its result.
    let relay_pair_link =
        match pairlink::parse(&link).map_err(|_| PrivateStateError::PairInputInvalid)? {
            ParsedPairLink::Relay(relay_link) => Some(relay_link),
            ParsedPairLink::Direct(_) => None,
        };
    let pairing_identity = pairing_ceremony_identity(device_label);
    let mut credential = pairer.pair(&link, device_label, &pairing_identity).await?;
    if let Some(relay_link) = relay_pair_link {
        if credential.relay_origin.as_deref() != Some(relay_link.relay_origin.as_str()) {
            return Err(PrivateStateError::PairingFailed);
        }
        credential.endpoints.clear();
        credential.local_endpoints = None;
        TransportClient::new_relay_only(credential.clone(), None)
            .map_err(|_| PrivateStateError::PairingFailed)?;
    }
    let result = match credential_fault {
        Some(fault) => persist_credential_with_fault(state_lock.root(), &credential, fault),
        None => persist_credential(state_lock.root(), &credential),
    };
    if result.is_ok() {
        let _ = std::fs::remove_file(crate::sync_health::paired_journal_path(state_dir));
    }
    result
}

#[cfg(test)]
pub(crate) async fn setup_with_pairer_for_test<R: Read>(
    pairer: &dyn Pairer,
    config_root: &Path,
    state_dir: &Path,
    device_label: &str,
    stream: Option<&str>,
    input: R,
) -> Result<(), PrivateStateError> {
    setup_with_pairer_and_stream(pairer, config_root, state_dir, device_label, stream, input).await
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

/// Whether pairing material exists, without reading or parsing it.
///
/// Status rendering needs to know that a link exists; it has no business holding the
/// credential to find that out. A present-but-unreadable file still counts as present —
/// the health surfaces already distinguish broken private state from absent state, and
/// reporting "not paired" for a file we merely failed to read would be its own lie.
pub(crate) fn credential_present(config_root: &Path) -> bool {
    config_root.join(CREDENTIALS_FILENAME).exists()
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

pub(crate) fn persist_credential_with_fault(
    config_root: &Path,
    credential: &Credential,
    fault: &dyn DurableWriteFault,
) -> Result<(), PrivateStateError> {
    let bytes =
        serde_json::to_vec(credential).map_err(|_| PrivateStateError::MalformedCredential)?;
    atomic_write_bytes_with_fault(&config_root.join(CREDENTIALS_FILENAME), &bytes, fault).map_err(
        |error| {
            map_private_file(
                error,
                PrivateTargetKind::Credential,
                PrivateIoOperation::Persist,
            )
        },
    )
}

pub(crate) fn journal_identity_key(credential: &Credential) -> String {
    let mut key = credential.instance_id.clone();
    key.push(':');
    for byte in &credential.ca_fp_prefix {
        use std::fmt::Write;
        let _ = write!(&mut key, "{byte:02x}");
    }
    key
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

pub(crate) type LinkFactSink = Arc<dyn Fn(&LinkFacts) + Send + Sync>;

#[derive(Default)]
struct LinkFactsInner {
    state: Mutex<LinkFactState>,
    sink: Mutex<Option<LinkFactSink>>,
    // Never reset to 0, unlike dial_generation: it identifies the owner
    // association itself, so a stale in-flight fetch from a prior owner can't
    // alias a same-numbered dial_generation from a later one (ABA).
    owner_epoch: AtomicU64,
    association_epoch: AtomicU64,
}

#[derive(Clone, Default)]
pub(crate) struct LinkFacts {
    inner: Arc<LinkFactsInner>,
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
    pub(crate) journal_version_observed: bool,
    pub(crate) dial_generation: u64,
    pub(crate) optional_dial: bool,
}

impl LinkFacts {
    pub(crate) fn begin_owner_generation(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            self.inner.owner_epoch.fetch_add(1, Ordering::AcqRel);
            self.inner.association_epoch.fetch_add(1, Ordering::AcqRel);
            *state = LinkFactState::default();
        }
        self.persist();
    }

    pub(crate) fn owner_lost(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            self.inner.owner_epoch.fetch_add(1, Ordering::AcqRel);
            self.inner.association_epoch.fetch_add(1, Ordering::AcqRel);
            *state = LinkFactState {
                transport_unavailable: true,
                ..LinkFactState::default()
            };
        }
        self.persist();
    }

    fn begin_journal_dial(&self) {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            self.inner.owner_epoch.fetch_add(1, Ordering::AcqRel);
            state.journal_version_observed = false;
            state.carrier_proven = false;
        }
        self.persist();
    }

    /// Identifies the current owner association, independent of dial_generation
    /// (which resets to 0 at the start of every owner epoch and so can repeat
    /// across them). Bumped on owner changes and dial attempts so a
    /// completion captured under a prior owner can never be mistaken for one
    /// from the current owner, even when their dial_generation numbers match.
    #[cfg(test)]
    pub(crate) fn owner_epoch(&self) -> u64 {
        self.inner.owner_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn publish(&self, fact: LinkFact) {
        self.publish_with_generation(fact, self.snapshot().dial_generation);
    }

    pub(crate) fn publish_with_generation(&self, fact: LinkFact, generation: u64) {
        self.publish_with_source(fact, generation, false, None);
    }

    fn publish_with_source(
        &self,
        fact: LinkFact,
        generation: u64,
        optional: bool,
        owner: Option<(u64, &AtomicBool)>,
    ) -> bool {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if owner.is_some_and(|(epoch, closed)| {
                self.association_epoch() != epoch || closed.load(Ordering::Acquire)
            }) {
                return false;
            }
            // A freshly proven dial hasn't had its own journal version confirmed
            // yet: an observed flag left over from an earlier generation within
            // the same owner epoch (e.g. a transport-level reconnect) must not
            // be reported as current until metadata publication confirms it
            // again for this generation.
            if fact == LinkFact::CarrierProven && generation != state.dial_generation {
                state.journal_version_observed = false;
            }
            state.dial_generation = generation;
            if matches!(fact, LinkFact::CarrierProven | LinkFact::ObserverRegistered) {
                state.optional_dial = optional;
            }
            match fact {
                LinkFact::PairingRequired => state.pairing_required = true,
                LinkFact::PrivateStateInvalid => state.private_state_invalid = true,
                LinkFact::ConfigSanitationFailed => state.config_sanitation_failed = true,
                LinkFact::ListenerReady => state.listener_ready = true,
                LinkFact::CarrierProven => state.carrier_proven = true,
                LinkFact::ObserverRegistered => {
                    state.observer_registered = true;
                    state.transport_unavailable = false;
                }
                LinkFact::TransportUnavailable => {
                    state.transport_unavailable = true;
                    state.journal_version_observed = false;
                }
                LinkFact::TerminalRevocation => {
                    state.terminal_revocation = true;
                    state.journal_version_observed = false;
                }
                LinkFact::TokenPersistenceFailure => state.token_persistence_failure = true,
            }
        }
        self.persist();
        true
    }

    pub(crate) fn association_epoch(&self) -> u64 {
        self.inner.association_epoch.load(Ordering::Acquire)
    }

    pub(crate) fn metadata_owner_is_current(&self, epoch: u64) -> bool {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        self.association_epoch() == epoch
            && state.carrier_proven
            && !state.transport_unavailable
            && !state.terminal_revocation
    }

    pub(crate) fn note_metadata_saved(&self, epoch: u64, dial_generation: u64) {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        if self.association_epoch() != epoch
            || state.dial_generation != dial_generation
            || !state.carrier_proven
            || state.transport_unavailable
            || state.terminal_revocation
        {
            return;
        }
        state.journal_version_observed = true;
        drop(state);
        self.persist();
    }

    pub(crate) fn snapshot_with_epoch(&self) -> (LinkFactState, u64) {
        let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        (
            state.clone(),
            self.inner.owner_epoch.load(Ordering::Acquire),
        )
    }

    pub(crate) fn snapshot(&self) -> LinkFactState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn install_sink(&self, sink: LinkFactSink) {
        *self.inner.sink.lock().unwrap_or_else(|p| p.into_inner()) = Some(sink);
        self.persist();
    }

    pub(crate) fn republish_current(&self) {
        self.persist();
    }

    fn persist(&self) {
        let sink = self
            .inner
            .sink
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(sink) = sink {
            sink(self);
        }
    }
}

pub(crate) struct PrivateLinkOpener {
    lan_transport: Option<Arc<TransportClient>>,
    relay_transport: Mutex<Option<Arc<TransportClient>>>,
    admission: tokio::sync::Mutex<()>,
    transport_unavailable: Arc<AtomicBool>,
    facts: LinkFacts,
    generation: AtomicU64,
    relay_incarnation: AtomicU64,
    shutdown_fenced: AtomicBool,
}

impl PrivateLinkOpener {
    fn new(
        lan_transport: Option<TransportClient>,
        relay_transport: Option<TransportClient>,
        transport_unavailable: Arc<AtomicBool>,
        facts: LinkFacts,
    ) -> Self {
        Self {
            lan_transport: lan_transport.map(Arc::new),
            relay_transport: Mutex::new(relay_transport.map(Arc::new)),
            admission: tokio::sync::Mutex::new(()),
            transport_unavailable,
            facts,
            generation: AtomicU64::new(0),
            relay_incarnation: AtomicU64::new(1),
            shutdown_fenced: AtomicBool::new(false),
        }
    }

    pub(crate) fn relay_incarnation(&self) -> u64 {
        self.relay_incarnation.load(Ordering::Acquire)
    }

    pub(crate) fn replace_relay_transport(&self, client: TransportClient) {
        let mut guard = self
            .relay_transport
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *guard = Some(Arc::new(client));
        self.relay_incarnation.fetch_add(1, Ordering::AcqRel);
        self.transport_unavailable.store(false, Ordering::Release);
    }

    pub(crate) fn clear_relay_transport(&self) {
        let mut guard = self
            .relay_transport
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *guard = None;
        self.relay_incarnation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn fence_shutdown(&self) {
        self.shutdown_fenced.store(true, Ordering::Release);
        self.relay_incarnation.fetch_add(1, Ordering::AcqRel);
    }

    async fn admit_dial<T>(
        &self,
        dial: impl Future<Output = Result<T, TransportError>>,
    ) -> Result<T, TransportError> {
        if self.shutdown_fenced.load(Ordering::Acquire)
            || (self.lan_transport.is_none() && self.transport_unavailable.load(Ordering::Acquire))
        {
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        // The relay transport is the only client that holds a device token. Pinned
        // client.rs:265 and :296 refresh only inside its dial_carrier relay path, so
        // this guard keeps a refreshed carrier behind synchronous token persistence.
        // There is no timer, background, or live-stream refresh path.
        let _admission = self.admission.lock().await;
        if self.shutdown_fenced.load(Ordering::Acquire)
            || (self.lan_transport.is_none() && self.transport_unavailable.load(Ordering::Acquire))
        {
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        let result = dial.await?;
        if self.shutdown_fenced.load(Ordering::Acquire)
            || (self.lan_transport.is_none() && self.transport_unavailable.load(Ordering::Acquire))
        {
            drop(result);
            return Err(TransportError::Pairing(
                "linked transport unavailable".into(),
            ));
        }
        Ok(result)
    }
}

impl CarrierOpener for PrivateLinkOpener {
    fn proxy_headers(
        &self,
        upstream_headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, TransportError> {
        proxy_headers_for_v3(upstream_headers)
    }

    fn dial_carrier(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<DialedCarrier, TransportError>> + Send + '_>> {
        self.dial_carrier_with_source(false)
    }
}

struct OptionalPrivateLinkOpener(Arc<PrivateLinkOpener>);
impl CarrierOpener for OptionalPrivateLinkOpener {
    fn proxy_headers(
        &self,
        headers: &[(String, String)],
    ) -> Result<Vec<(String, String)>, TransportError> {
        self.0.proxy_headers(headers)
    }
    fn dial_carrier(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<DialedCarrier, TransportError>> + Send + '_>> {
        self.0.dial_carrier_with_source(true)
    }
}

impl PrivateLinkOpener {
    fn dial_carrier_with_source(
        &self,
        optional: bool,
    ) -> Pin<Box<dyn Future<Output = Result<DialedCarrier, TransportError>> + Send + '_>> {
        Box::pin(async move {
            let association = self.facts.association_epoch();
            self.facts.begin_journal_dial();
            let (carrier, relay_dial_incarnation) = self
                .admit_dial(async {
                    let mut lan_error = None;
                    let has_relay = self
                        .relay_transport
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_some();
                    if let Some(lan) = &self.lan_transport {
                        // Reserve time for the alternate relay leg only when it exists. A
                        // LAN-only credential keeps the pinned client's behavior unchanged.
                        let result = if has_relay {
                            match tokio::time::timeout(LAN_CARRIER_TIMEOUT, lan.dial_carrier())
                                .await
                            {
                                Ok(result) => result,
                                Err(_) => Err(TransportError::Io(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "LAN carrier dial timed out",
                                ))),
                            }
                        } else {
                            lan.dial_carrier().await
                        };
                        match result {
                            Ok(carrier) => return Ok((carrier, None)),
                            Err(error) => lan_error = Some(error),
                        }
                    }
                    let (relay_client, dial_incarnation) = {
                        let guard = self
                            .relay_transport
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        (
                            guard.clone(),
                            self.relay_incarnation.load(Ordering::Acquire),
                        )
                    };
                    if let Some(relay) = relay_client {
                        if self.transport_unavailable.load(Ordering::Acquire) {
                            return Err(TransportError::Pairing(
                                "linked transport unavailable".into(),
                            ));
                        }
                        match relay.dial_carrier().await {
                            Ok(carrier) => {
                                if self.transport_unavailable.load(Ordering::Acquire)
                                    || self.relay_incarnation.load(Ordering::Acquire)
                                        != dial_incarnation
                                {
                                    return Err(TransportError::Pairing(
                                        "relay transport incarnation changed".into(),
                                    ));
                                }
                                return Ok((carrier, Some(dial_incarnation)));
                            }
                            Err(error) if lan_error.is_none() => return Err(error),
                            Err(_) => {}
                        }
                    }
                    Err(lan_error.unwrap_or(TransportError::NoEndpoint))
                })
                .await?;

            if let Some(dial_incarnation) = relay_dial_incarnation
                && (self.transport_unavailable.load(Ordering::Acquire)
                    || self.relay_incarnation.load(Ordering::Acquire) != dial_incarnation)
            {
                return Err(TransportError::Pairing(
                    "relay transport incarnation changed".into(),
                ));
            }

            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            // Owner retirement and proof publication share the facts lock.
            for fact in [LinkFact::CarrierProven, LinkFact::ObserverRegistered] {
                if !self.facts.publish_with_source(
                    fact,
                    generation,
                    optional,
                    Some((association, &self.shutdown_fenced)),
                ) {
                    return Err(TransportError::Pairing(
                        "linked transport unavailable".into(),
                    ));
                }
            }
            Ok(carrier)
        })
    }
}

fn proxy_headers_for_v3(
    upstream_headers: &[(String, String)],
) -> Result<Vec<(String, String)>, TransportError> {
    let markers = upstream_headers
        .iter()
        .filter(|(name, _)| name == ROUTE_CLASS_MARKER_HEADER_NAME)
        .collect::<Vec<_>>();
    // The internal marker identifies app-originated ingest calls.  A browser
    // request that reached the loopback bridge through Open Journal has no
    // such marker, and is authorized instead by the bridge capability cookie.
    // Preserve the explicit-marker check: a malformed or duplicate marker must
    // still fail closed rather than being mistaken for browser traffic.
    let marker_valid = markers.is_empty()
        || matches!(markers.as_slice(), [(_, value)] if value == INGEST_V3_ROUTE_CLASS);
    if !marker_valid {
        return Err(TransportError::Pairing("invalid route class marker".into()));
    }
    let mut headers = upstream_headers
        .iter()
        .filter(|(name, _)| name != ROUTE_CLASS_MARKER_HEADER_NAME)
        .cloned()
        .collect::<Vec<_>>();
    headers.push((PROTOCOL_VERSION_HEADER_NAME.to_owned(), "3".to_owned()));
    Ok(headers)
}

fn streams_journal_response(request: &RequestHead) -> bool {
    request.method == "GET"
        && (request.path() == "/sse/events"
            || request.path().starts_with(JOURNAL_MEDIA_PATH_PREFIX))
}

pub(crate) struct PrivateLinkSession {
    client: reqwest::Client,
    origin: Url,
    opener: Arc<PrivateLinkOpener>,
    handle: JournalBridgeHandle,
    optional_handle: JournalBridgeHandle,
    optional_client: reqwest::Client,
    optional_origin: Url,
    token_persistence: Arc<TokenPersistence>,
    bootstrap_target: Option<String>,
    _state_lock: Arc<PrivateStateLock>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkOutcome {
    Success { status: StatusCode, body: Vec<u8> },
    Forbidden,
    TransportUnavailable,
    LocalRejected { status: StatusCode },
}

struct PrivateLinkCapabilityInner {
    client: reqwest::Client,
    origin: Url,
    opener: Arc<PrivateLinkOpener>,
    writer: Arc<OrderedCredentialWriter>,
    optional_client: reqwest::Client,
    optional_origin: Url,
    state_lock: std::sync::Weak<PrivateStateLock>,
}

#[derive(Clone)]
pub(crate) struct PrivateLinkCapability {
    inner: Arc<PrivateLinkCapabilityInner>,
}

impl PrivateLinkCapability {
    pub(crate) fn facts(&self) -> LinkFacts {
        self.inner.opener.facts.clone()
    }

    pub(crate) async fn send_optional(
        &self,
        builder: RequestBuilder,
        timeout: Duration,
    ) -> LinkOutcome {
        match builder.timeout(timeout).send().await {
            Ok(response) => {
                let status = response.status();
                if status == StatusCode::FORBIDDEN {
                    return LinkOutcome::Forbidden;
                }
                if status.is_client_error() {
                    return LinkOutcome::LocalRejected { status };
                }
                if let Some(len) = response.content_length()
                    && len > 65536
                {
                    return LinkOutcome::LocalRejected {
                        status: StatusCode::PAYLOAD_TOO_LARGE,
                    };
                }
                use futures_util::StreamExt;
                let mut stream = response.bytes_stream();
                let mut body = Vec::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(bytes) => {
                            if body.len() + bytes.len() > 65536 {
                                return LinkOutcome::LocalRejected {
                                    status: StatusCode::PAYLOAD_TOO_LARGE,
                                };
                            }
                            body.extend_from_slice(&bytes);
                        }
                        Err(_) => return LinkOutcome::TransportUnavailable,
                    }
                }
                LinkOutcome::Success { status, body }
            }
            Err(_) => LinkOutcome::TransportUnavailable,
        }
    }

    pub(crate) async fn clients_self_get(&self, timeout: Duration) -> LinkOutcome {
        let url = match confine_path(&self.inner.optional_origin, "/app/network/api/clients/self") {
            Ok(u) => u,
            Err(_) => {
                return LinkOutcome::LocalRejected {
                    status: StatusCode::BAD_REQUEST,
                };
            }
        };
        self.send_optional(self.inner.optional_client.get(url), timeout)
            .await
    }

    // Authenticated current-device PUT is forwarded; optional failure if the journal/bridge rejects it.
    pub(crate) async fn clients_self_put(&self, body: Vec<u8>, timeout: Duration) -> LinkOutcome {
        let url = match confine_path(&self.inner.optional_origin, "/app/network/api/clients/self") {
            Ok(u) => u,
            Err(_) => {
                return LinkOutcome::LocalRejected {
                    status: StatusCode::BAD_REQUEST,
                };
            }
        };
        self.send_optional(
            self.inner
                .optional_client
                .put(url)
                .header("content-type", "application/json")
                .body(body),
            timeout,
        )
        .await
    }

    pub(crate) async fn relay_access_get(&self, timeout: Duration) -> LinkOutcome {
        let url = match confine_path(&self.inner.optional_origin, "/app/network/api/relay/access") {
            Ok(u) => u,
            Err(_) => {
                return LinkOutcome::LocalRejected {
                    status: StatusCode::BAD_REQUEST,
                };
            }
        };
        self.send_optional(self.inner.optional_client.get(url), timeout)
            .await
    }

    pub(crate) async fn blocking_optional<T: Send + 'static>(
        &self,
        operation: impl FnOnce(Arc<OrderedCredentialWriter>, Arc<PrivateLinkOpener>) -> Result<T, ()>
        + Send
        + 'static,
    ) -> Result<T, ()> {
        let state_lock = self.inner.state_lock.upgrade().ok_or(())?;
        let writer = Arc::clone(&self.inner.writer);
        let opener = Arc::clone(&self.inner.opener);
        tokio::task::spawn_blocking(move || {
            let _state_lock = state_lock;
            operation(writer, opener)
        })
        .await
        .map_err(|_| ())?
    }

    pub(crate) async fn retry_optional_reconciliation(
        &self,
        attempt: Arc<OptionalAttemptLease>,
    ) -> Result<bool, ()> {
        self.blocking_optional(move |writer, opener| {
            writer.retry_durable_reconciliation_with_attempt(&opener, Some(&attempt))
        })
        .await
    }

    pub(crate) fn writer(&self) -> Arc<OrderedCredentialWriter> {
        self.inner.writer.clone()
    }

    pub(crate) fn opener_relay_incarnation(&self) -> u64 {
        self.inner.opener.relay_incarnation()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn replace_relay_transport(&self, client: TransportClient) {
        self.inner.opener.replace_relay_transport(client);
    }

    #[cfg(test)]
    pub(crate) fn opener(&self) -> Arc<PrivateLinkOpener> {
        self.inner.opener.clone()
    }

    async fn send(&self, builder: RequestBuilder, timeout: Duration) -> LinkOutcome {
        match builder.timeout(timeout).send().await {
            Ok(response) => {
                let status = response.status();
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

    fn ingest_v3_url(&self, suffix: &str) -> Result<Url, LinkOutcome> {
        confine_path(&self.inner.origin, &format!("{INGEST_PATH}{suffix}")).map_err(|_| {
            LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            }
        })
    }

    fn validate_day(day: &str) -> bool {
        day.len() == 8 && day.bytes().all(|byte| byte.is_ascii_digit())
    }

    pub(crate) async fn ingest(&self, form: multipart::Form) -> LinkOutcome {
        let Ok(url) = self.ingest_v3_url("") else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(
            self.inner
                .client
                .post(url)
                .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS)
                .multipart(form),
            INGEST_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn probe_manifest(&self) -> LinkOutcome {
        let Ok(url) = self.ingest_v3_url("/manifest") else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(
            self.inner
                .client
                .get(url)
                .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS),
            LISTING_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn manifest_day(&self, day: &str) -> LinkOutcome {
        if !Self::validate_day(day) {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        }
        let Ok(url) = self.ingest_v3_url(&format!("/manifest/{day}")) else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(
            self.inner
                .client
                .get(url)
                .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS),
            LISTING_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn segments_day(&self, day: &str) -> LinkOutcome {
        if !Self::validate_day(day) {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        }
        let Ok(url) = self.ingest_v3_url(&format!("/segments/{day}")) else {
            return LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            };
        };
        self.send(
            self.inner
                .client
                .get(url)
                .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS),
            LISTING_TIMEOUT,
        )
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn system_status(&self) -> Result<Option<String>, LinkOutcome> {
        let Ok(url) = confine_path(&self.inner.origin, SYSTEM_STATUS_PATH) else {
            return Err(LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            });
        };
        let outcome = self
            .send(
                self.inner
                    .client
                    .get(url)
                    .header(reqwest::header::CACHE_CONTROL, "no-cache"),
                SYSTEM_STATUS_TIMEOUT,
            )
            .await;
        match outcome {
            LinkOutcome::Success { status, body } if status == StatusCode::OK => {
                let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) else {
                    return Ok(None);
                };
                let Some(raw_version) = json
                    .get("version")
                    .and_then(|v| v.get("current"))
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(None);
                };
                if let Some(sanitized) = sanitize_journal_version(raw_version) {
                    Ok(Some(sanitized))
                } else {
                    Ok(None)
                }
            }
            LinkOutcome::Success { status, body } => Err(LinkOutcome::Success { status, body }),
            other => Err(other),
        }
    }

    pub(crate) async fn system_status_optional(
        &self,
        timeout: Duration,
    ) -> Result<Option<String>, LinkOutcome> {
        let Ok(url) = confine_path(&self.inner.optional_origin, SYSTEM_STATUS_PATH) else {
            return Err(LinkOutcome::LocalRejected {
                status: StatusCode::BAD_REQUEST,
            });
        };
        let outcome = self
            .send_optional(
                self.inner
                    .optional_client
                    .get(url)
                    .header(reqwest::header::CACHE_CONTROL, "no-cache"),
                timeout,
            )
            .await;
        match outcome {
            LinkOutcome::Success { status, body } if status == StatusCode::OK => {
                let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) else {
                    return Ok(None);
                };
                let Some(raw_version) = json
                    .get("version")
                    .and_then(|v| v.get("current"))
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(None);
                };
                if let Some(sanitized) = sanitize_journal_version(raw_version) {
                    Ok(Some(sanitized))
                } else {
                    Ok(None)
                }
            }
            LinkOutcome::Success { status, body } => Err(LinkOutcome::Success { status, body }),
            other => Err(other),
        }
    }
}

pub(crate) fn sanitize_journal_version(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > 64 {
        return None;
    }
    let is_valid_char = |b: u8| {
        b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+' || b == b'_' || b == b'~'
    };
    if raw.bytes().all(is_valid_char) {
        Some(raw.to_owned())
    } else {
        None
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
}

impl Drop for PrivateLinkOwner {
    fn drop(&mut self) {
        self.capability.inner.writer.fence_shutdown();
        self.capability.inner.opener.fence_shutdown();
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
    let capability = session.capability();
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
        },
    )
    .await?;
    finish_owner_start(session).await
}

#[cfg(test)]
pub(crate) async fn start_private_link_for_test(
    credential: Credential,
    expected_name: &str,
) -> (tempfile::TempDir, PrivateLinkOwner) {
    let temp = tempfile::tempdir().unwrap();
    let session = start_private_link_session(temp.path(), credential, expected_name)
        .await
        .unwrap();
    (temp, finish_owner_start(session).await.unwrap())
}

pub(crate) fn compute_pairing_id(client_cert_pem: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(client_cert_pem.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableClearPending {
    pub(crate) pairing_id: String,
    pub(crate) access_mutation_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DurableReadyPending {
    pub(crate) pairing_id: String,
    pub(crate) access_mutation_generation: u64,
    pub(crate) credential: Credential,
}

pub(crate) struct OptionalAttempt {
    pub(crate) lease: Arc<OptionalAttemptLease>,
}

pub(crate) struct OptionalAttemptLease {
    revision: Arc<AtomicU64>,
    expected: u64,
    deadline: tokio::time::Instant,
}

impl OptionalAttempt {
    fn new(revision: Arc<AtomicU64>, deadline: tokio::time::Instant) -> Self {
        let expected = revision.fetch_add(1, Ordering::AcqRel) + 1;
        Self {
            lease: Arc::new(OptionalAttemptLease {
                revision,
                expected,
                deadline,
            }),
        }
    }
}
impl Drop for OptionalAttempt {
    fn drop(&mut self) {
        let _ = self.lease.revision.compare_exchange(
            self.lease.expected,
            self.lease.expected.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}
impl OptionalAttemptLease {
    pub(crate) fn is_current(&self) -> bool {
        tokio::time::Instant::now() < self.deadline
            && self.revision.load(Ordering::Acquire) == self.expected
    }
}

pub(crate) struct OrderedCredentialWriter {
    config_root: PathBuf,
    pairing_id: String,
    identity_key: String,
    instance_id: String,
    metadata_attempt: Arc<AtomicU64>,
    access_attempt: Arc<AtomicU64>,
    live_hook_epoch: AtomicU64,
    access_mutation_generation: AtomicU64,
    credential: Mutex<Credential>,
    failed: Mutex<bool>,
    fault: Arc<dyn DurableWriteFault>,
    transport_unavailable: Arc<AtomicBool>,
    facts: LinkFacts,
    durable_clear_pending: Mutex<Option<DurableClearPending>>,
    durable_ready_pending: Mutex<Option<DurableReadyPending>>,
    shutdown_fenced: AtomicBool,
}

impl OrderedCredentialWriter {
    pub(crate) fn new(
        config_root: PathBuf,
        credential: Credential,
        fault: Arc<dyn DurableWriteFault>,
        transport_unavailable: Arc<AtomicBool>,
        facts: LinkFacts,
    ) -> Self {
        let pairing_id = compute_pairing_id(&credential.client_cert_pem);
        Self {
            config_root,
            pairing_id,
            identity_key: journal_identity_key(&credential),
            instance_id: credential.instance_id.clone(),
            metadata_attempt: Arc::new(AtomicU64::new(0)),
            access_attempt: Arc::new(AtomicU64::new(0)),
            live_hook_epoch: AtomicU64::new(1),
            access_mutation_generation: AtomicU64::new(1),
            credential: Mutex::new(credential),
            failed: Mutex::new(false),
            fault,
            transport_unavailable,
            facts,
            durable_clear_pending: Mutex::new(None),
            durable_ready_pending: Mutex::new(None),
            shutdown_fenced: AtomicBool::new(false),
        }
    }

    pub(crate) fn pairing_id(&self) -> &str {
        &self.pairing_id
    }

    pub(crate) fn identity_key(&self) -> &str {
        &self.identity_key
    }
    pub(crate) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub(crate) fn metadata_attempt(&self, deadline: tokio::time::Instant) -> OptionalAttempt {
        OptionalAttempt::new(Arc::clone(&self.metadata_attempt), deadline)
    }
    pub(crate) fn access_attempt(&self, deadline: tokio::time::Instant) -> OptionalAttempt {
        OptionalAttempt::new(Arc::clone(&self.access_attempt), deadline)
    }
    pub(crate) fn invalidate_optional_attempts(&self) {
        self.metadata_attempt.fetch_add(1, Ordering::AcqRel);
        self.access_attempt.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn access_mutation_generation(&self) -> u64 {
        self.access_mutation_generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn current_credential(&self) -> Credential {
        self.credential
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn failed(&self) -> bool {
        *self.failed.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn fence_shutdown(&self) {
        self.shutdown_fenced.store(true, Ordering::Release);
        self.invalidate_optional_attempts();
        self.live_hook_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn create_token_hook(self: &Arc<Self>) -> TokenPersistHook {
        let epoch = self.live_hook_epoch.load(Ordering::Acquire);
        self.create_token_hook_with_epoch(epoch)
    }

    pub(crate) fn create_token_hook_with_epoch(self: &Arc<Self>, epoch: u64) -> TokenPersistHook {
        let writer = Arc::clone(self);
        let captured_pairing_id = self.pairing_id.clone();
        Arc::new(move |token, expires_at| {
            writer.commit_mandatory_refresh(&captured_pairing_id, epoch, token, expires_at);
        })
    }

    pub(crate) fn commit_mandatory_refresh(
        &self,
        hook_pairing_id: &str,
        hook_epoch: u64,
        token: &str,
        expires_at: i64,
    ) {
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return;
        }
        let mut current = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return;
        }
        if self.pairing_id != hook_pairing_id {
            // Stale hook from previous pairing -> no-op
            return;
        }
        if self.live_hook_epoch.load(Ordering::Acquire) != hook_epoch {
            // Stale hook after newer access/disable mutation -> no-op
            return;
        }
        let mut updated = current.clone();
        updated.device_token = Some(token.to_owned());
        updated.device_token_expires_at = Some(expires_at);
        let cred_path = self.config_root.join(CREDENTIALS_FILENAME);
        let durable = serde_json::to_vec(&updated).ok().is_some_and(|bytes| {
            atomic_write_bytes_with_fault(&cred_path, &bytes, self.fault.as_ref()).is_ok()
        });
        if durable {
            *current = updated;
            self.access_mutation_generation
                .fetch_add(1, Ordering::AcqRel);
            let mut clear_pending = self
                .durable_clear_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *clear_pending = None;
            let mut ready_pending = self
                .durable_ready_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *ready_pending = None;
        } else {
            *self.failed.lock().unwrap_or_else(|p| p.into_inner()) = true;
            self.transport_unavailable.store(true, Ordering::Release);
            self.facts.publish(LinkFact::TokenPersistenceFailure);
            if current.endpoints.is_empty() {
                self.facts.publish(LinkFact::TransportUnavailable);
            }
        }
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_optional_relay(
        self: &Arc<Self>,
        opener: &PrivateLinkOpener,
        caller_pairing_id: &str,
        caller_access_gen: u64,
        caller_opener_incarnation: u64,
        relay_origin: &str,
        device_token: &str,
        expires_at: i64,
    ) -> Result<(), ()> {
        self.commit_optional_relay_with_attempt(
            opener,
            caller_pairing_id,
            caller_access_gen,
            caller_opener_incarnation,
            relay_origin,
            device_token,
            expires_at,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn commit_optional_relay_with_attempt(
        self: &Arc<Self>,
        opener: &PrivateLinkOpener,
        caller_pairing_id: &str,
        caller_access_gen: u64,
        caller_opener_incarnation: u64,
        relay_origin: &str,
        device_token: &str,
        expires_at: i64,
        attempt: Option<&OptionalAttemptLease>,
    ) -> Result<(), ()> {
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return Err(());
        }
        let mut current = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return Err(());
        }
        if attempt.is_some_and(|attempt| {
            !attempt.is_current() || expires_at <= chrono::Utc::now().timestamp()
        }) {
            return Err(());
        }
        if self.pairing_id != caller_pairing_id {
            return Err(());
        }
        if self.access_mutation_generation.load(Ordering::Acquire) != caller_access_gen {
            return Err(());
        }
        if opener.relay_incarnation.load(Ordering::Acquire) != caller_opener_incarnation {
            return Err(());
        }
        let mut updated = current.clone();
        updated.relay_origin = Some(relay_origin.to_owned());
        updated.device_token = Some(device_token.to_owned());
        updated.device_token_expires_at = Some(expires_at);

        let mut test_cred = updated.clone();
        test_cred.endpoints.clear();
        test_cred.local_endpoints = None;
        let new_hook =
            self.create_token_hook_with_epoch(self.live_hook_epoch.load(Ordering::Acquire) + 1);
        let new_client = match TransportClient::new_relay_only(test_cred, Some(new_hook)) {
            Ok(c) => c,
            Err(_) => return Err(()),
        };

        let bytes = serde_json::to_vec(&updated).map_err(|_| ())?;
        let cred_path = self.config_root.join(CREDENTIALS_FILENAME);
        let durable = atomic_write_bytes_guarded(&cred_path, &bytes, self.fault.as_ref(), &|| {
            !self.shutdown_fenced.load(Ordering::Acquire)
                && attempt
                    .is_none_or(|a| a.is_current() && expires_at > chrono::Utc::now().timestamp())
        })
        .is_ok();
        if durable
            && (self.shutdown_fenced.load(Ordering::Acquire)
                || attempt.is_some_and(|a| {
                    !a.is_current() || expires_at <= chrono::Utc::now().timestamp()
                }))
        {
            *self
                .durable_ready_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(DurableReadyPending {
                pairing_id: caller_pairing_id.to_owned(),
                access_mutation_generation: caller_access_gen,
                credential: updated,
            });
            return Err(());
        }
        if durable {
            *current = updated;
            self.access_mutation_generation
                .fetch_add(1, Ordering::AcqRel);
            self.live_hook_epoch.fetch_add(1, Ordering::AcqRel);
            opener.replace_relay_transport(new_client);
            *self
                .durable_clear_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
            *self
                .durable_ready_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
            Ok(())
        } else {
            // Check if disk has parseable bytes (DirSync-uncertain)
            let read_parseable = std::fs::read(&cred_path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<Credential>(&raw).ok())
                .is_some_and(|disk| disk == updated);
            tracing::warn!(error = %PrivateStateError::TokenPersistenceFailed, optional = true);
            if read_parseable {
                // Post-rename failure: retain prior live tuple + LAN, new tuple inactive,
                // store durable_ready_pending for this exact revision.
                *self
                    .durable_ready_pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(DurableReadyPending {
                    pairing_id: caller_pairing_id.to_owned(),
                    access_mutation_generation: caller_access_gen,
                    credential: updated,
                });
            }
            Err(())
        }
    }

    #[cfg(test)]
    pub(crate) fn commit_optional_clear(
        &self,
        opener: &PrivateLinkOpener,
        caller_pairing_id: &str,
        caller_access_gen: u64,
        caller_opener_incarnation: u64,
    ) -> Result<(), ()> {
        self.commit_optional_clear_with_attempt(
            opener,
            caller_pairing_id,
            caller_access_gen,
            caller_opener_incarnation,
            None,
        )
    }

    pub(crate) fn commit_optional_clear_with_attempt(
        &self,
        opener: &PrivateLinkOpener,
        caller_pairing_id: &str,
        caller_access_gen: u64,
        caller_opener_incarnation: u64,
        attempt: Option<&OptionalAttemptLease>,
    ) -> Result<(), ()> {
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return Err(());
        }
        let mut current = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        if self.shutdown_fenced.load(Ordering::Acquire) {
            return Err(());
        }
        if attempt.is_some_and(|attempt| !attempt.is_current()) {
            return Err(());
        }
        if self.pairing_id != caller_pairing_id {
            return Err(());
        }
        if self.access_mutation_generation.load(Ordering::Acquire) != caller_access_gen {
            return Err(());
        }
        if opener.relay_incarnation.load(Ordering::Acquire) != caller_opener_incarnation {
            return Err(());
        }
        // Asymmetric: clear live relay immediately, bump epochs
        opener.clear_relay_transport();
        self.live_hook_epoch.fetch_add(1, Ordering::AcqRel);
        let new_gen = self
            .access_mutation_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;

        current.relay_origin = None;
        current.device_token = None;
        current.device_token_expires_at = None;
        *self
            .durable_ready_pending
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;

        let bytes = serde_json::to_vec(&*current).map_err(|_| ())?;
        let cred_path = self.config_root.join(CREDENTIALS_FILENAME);
        let durable = atomic_write_bytes_guarded(&cred_path, &bytes, self.fault.as_ref(), &|| {
            !self.shutdown_fenced.load(Ordering::Acquire) && attempt.is_none_or(|a| a.is_current())
        })
        .is_ok();
        if durable {
            *self
                .durable_clear_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
            Ok(())
        } else {
            *self
                .durable_clear_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(DurableClearPending {
                pairing_id: caller_pairing_id.to_owned(),
                access_mutation_generation: new_gen,
            });
            tracing::warn!(error = %PrivateStateError::TokenPersistenceFailed, optional = true);
            Err(())
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn publish_metadata(
        &self,
        attempt: &OptionalAttemptLease,
        association_epoch: u64,
        state_dir: &Path,
        version: &str,
        name: Option<&str>,
        preserve_name: bool,
    ) -> Result<(), ()> {
        let _owner = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        if self.shutdown_fenced.load(Ordering::Acquire)
            || !attempt.is_current()
            || !self.facts.metadata_owner_is_current(association_epoch)
        {
            return Err(());
        }
        let saved_name = if preserve_name {
            crate::sync_health::load_paired_journal_version(state_dir)
                .filter(|saved| saved.identity_key == self.identity_key)
                .and_then(|saved| saved.name)
        } else {
            name.map(str::to_owned)
        };
        let dial_generation = self.facts.snapshot().dial_generation;
        crate::sync_health::save_paired_journal_version_guarded(
            state_dir,
            &self.identity_key,
            version,
            saved_name.as_deref(),
            self.fault.as_ref(),
            &|| {
                !self.shutdown_fenced.load(Ordering::Acquire)
                    && attempt.is_current()
                    && self.facts.metadata_owner_is_current(association_epoch)
            },
        )
        .map_err(|_| ())?;
        drop(_owner);
        self.facts
            .note_metadata_saved(association_epoch, dial_generation);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retry_durable_reconciliation_if_pending(
        self: &Arc<Self>,
        opener: &PrivateLinkOpener,
    ) -> Result<bool, ()> {
        self.retry_durable_reconciliation_with_attempt(opener, None)
    }

    fn retry_durable_reconciliation_with_attempt(
        self: &Arc<Self>,
        opener: &PrivateLinkOpener,
        attempt: Option<&OptionalAttemptLease>,
    ) -> Result<bool, ()> {
        let mut current = self.credential.lock().unwrap_or_else(|p| p.into_inner());
        if self.shutdown_fenced.load(Ordering::Acquire) || attempt.is_some_and(|a| !a.is_current())
        {
            return Ok(false);
        }
        let revision = self.access_mutation_generation();
        let clear = self
            .durable_clear_pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(pending) = clear
            && pending.pairing_id == self.pairing_id
            && pending.access_mutation_generation == revision
        {
            let bytes = serde_json::to_vec(&*current).map_err(|_| ())?;
            atomic_write_bytes_guarded(
                &self.config_root.join(CREDENTIALS_FILENAME),
                &bytes,
                self.fault.as_ref(),
                &|| {
                    !self.shutdown_fenced.load(Ordering::Acquire)
                        && attempt.is_none_or(|a| a.is_current())
                },
            )
            .map_err(|_| {
                tracing::warn!(error = %PrivateStateError::TokenPersistenceFailed, optional = true);
            })?;
            *self
                .durable_clear_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
            return Ok(true);
        }
        let ready = self
            .durable_ready_pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(pending) = ready
            && pending.pairing_id == self.pairing_id
            && pending.access_mutation_generation == revision
        {
            if attempt.is_some()
                && pending
                    .credential
                    .device_token_expires_at
                    .is_none_or(|exp| exp <= chrono::Utc::now().timestamp())
            {
                return Err(());
            }
            let mut relay_credential = pending.credential.clone();
            relay_credential.endpoints.clear();
            relay_credential.local_endpoints = None;
            let hook =
                self.create_token_hook_with_epoch(self.live_hook_epoch.load(Ordering::Acquire) + 1);
            let client =
                TransportClient::new_relay_only(relay_credential, Some(hook)).map_err(|_| ())?;
            let bytes = serde_json::to_vec(&pending.credential).map_err(|_| ())?;
            atomic_write_bytes_guarded(
                &self.config_root.join(CREDENTIALS_FILENAME),
                &bytes,
                self.fault.as_ref(),
                &|| {
                    !self.shutdown_fenced.load(Ordering::Acquire)
                        && attempt.is_none_or(|a| {
                            a.is_current()
                                && pending
                                    .credential
                                    .device_token_expires_at
                                    .is_some_and(|exp| exp > chrono::Utc::now().timestamp())
                        })
                },
            )
            .map_err(|_| {
                tracing::warn!(error = %PrivateStateError::TokenPersistenceFailed, optional = true);
            })?;
            if self.shutdown_fenced.load(Ordering::Acquire)
                || attempt.is_some_and(|a| {
                    !a.is_current()
                        || pending
                            .credential
                            .device_token_expires_at
                            .is_none_or(|exp| exp <= chrono::Utc::now().timestamp())
                })
            {
                return Err(());
            }
            *current = pending.credential;
            self.access_mutation_generation
                .fetch_add(1, Ordering::AcqRel);
            self.live_hook_epoch.fetch_add(1, Ordering::AcqRel);
            opener.replace_relay_transport(client);
            *self
                .durable_ready_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = None;
            return Ok(true);
        }
        Ok(false)
    }
}

pub(crate) struct TokenPersistence {
    pub(crate) writer: Arc<OrderedCredentialWriter>,
}

impl TokenPersistence {
    fn new(
        config_root: PathBuf,
        credential: Credential,
        fault: Arc<dyn DurableWriteFault>,
        transport_unavailable: Arc<AtomicBool>,
        facts: LinkFacts,
    ) -> (Arc<Self>, TokenPersistHook) {
        let writer = Arc::new(OrderedCredentialWriter::new(
            config_root,
            credential,
            fault,
            transport_unavailable,
            facts,
        ));
        let hook = writer.create_token_hook();
        let state = Arc::new(Self { writer });
        (state, hook)
    }

    fn failed(&self) -> bool {
        self.writer.failed()
    }

    #[cfg(test)]
    fn persist(&self, token: &str, expires_at: i64) {
        let pairing_id = self.writer.pairing_id().to_string();
        let access_gen = self.writer.access_mutation_generation();
        self.writer
            .commit_mandatory_refresh(&pairing_id, access_gen, token, expires_at);
    }
}

impl PrivateLinkSession {
    pub(crate) fn capability(&self) -> PrivateLinkCapability {
        PrivateLinkCapability {
            inner: Arc::new(PrivateLinkCapabilityInner {
                client: self.client.clone(),
                origin: self.origin.clone(),
                opener: self.opener.clone(),
                writer: self.token_persistence.writer.clone(),
                optional_client: self.optional_client.clone(),
                optional_origin: self.optional_origin.clone(),
                state_lock: Arc::downgrade(&self._state_lock),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn request(
        &self,
        method: Method,
        relative_path: &str,
    ) -> Result<RequestBuilder, PrivateStateError> {
        let url = confine_path(&self.origin, relative_path)?;
        Ok(self.client.request(method, url).timeout(INGEST_TIMEOUT))
    }

    pub(crate) async fn shutdown(self) -> Result<(), PrivateStateError> {
        self.shutdown_inner(None).await
    }

    async fn shutdown_inner(
        self,
        #[cfg(test)] join_probe: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
        #[cfg(not(test))] _join_probe: Option<()>,
    ) -> Result<(), PrivateStateError> {
        self.token_persistence.writer.fence_shutdown();
        self.opener.fence_shutdown();
        let (status, optional_status) = tokio::join!(
            self.handle.shutdown_and_wait(),
            self.optional_handle.shutdown_and_wait()
        );
        #[cfg(test)]
        if let Some((joined, release)) = join_probe {
            joined.notify_one();
            release.notified().await;
        }
        if status.listener_active
            || status.active_requests != 0
            || optional_status.listener_active
            || optional_status.active_requests != 0
        {
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
pub(crate) async fn start_private_link_session_with_facts(
    config_root: &Path,
    credential: Credential,
    expected_name: &str,
    facts: LinkFacts,
) -> Result<PrivateLinkSession, PrivateStateError> {
    start_private_link_session_inner(
        config_root,
        credential,
        expected_name,
        SessionStartOptions {
            shared_facts: Some(facts),
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
#[derive(Default)]
struct SessionTestCapture {
    capability: Option<Arc<Mutex<Option<String>>>>,
}

struct SessionStartOptions {
    state_lock: Option<PrivateStateLock>,
    persistence_fault: Arc<dyn DurableWriteFault>,
    #[cfg(test)]
    test_capture: SessionTestCapture,
    shared_facts: Option<LinkFacts>,
}

impl Default for SessionStartOptions {
    fn default() -> Self {
        Self {
            state_lock: None,
            persistence_fault: Arc::new(NoWriteFault),
            #[cfg(test)]
            test_capture: SessionTestCapture::default(),
            shared_facts: None,
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
    if sanitized.stream.is_empty() {
        save_linked_stream(&paths, expected_name).map_err(config_persist_error)?;
    }
    let transport_unavailable = Arc::new(AtomicBool::new(false));
    let endpoint_hosts: Vec<String> = credential
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
    let lan_transport = if credential.endpoints.is_empty() {
        None
    } else {
        let mut lan_credential = credential.clone();
        lan_credential.relay_origin = None;
        lan_credential.device_token = None;
        lan_credential.device_token_expires_at = None;
        Some(
            TransportClient::new(lan_credential, None)
                .map_err(|_| PrivateStateError::BridgeUnavailable)?,
        )
    };
    let relay_transport = if lan_transport.is_none() {
        Some(
            TransportClient::new_relay_only(credential.clone(), Some(hook))
                .map_err(|_| PrivateStateError::BridgeUnavailable)?,
        )
    } else if credential
        .relay_origin
        .as_deref()
        .is_some_and(|origin| !origin.is_empty())
        && credential
            .device_token
            .as_deref()
            .is_some_and(|token| !token.is_empty())
    {
        let mut relay_credential = credential.clone();
        relay_credential.endpoints.clear();
        relay_credential.local_endpoints = None;
        TransportClient::new_relay_only(relay_credential, Some(hook)).ok()
    } else {
        None
    };
    let opener = Arc::new(PrivateLinkOpener::new(
        lan_transport,
        relay_transport,
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
        stream_response: Arc::new(streams_journal_response),
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
                ROUTE_CLASS_MARKER_HEADER_NAME,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ),
        max_request_body_bytes: MAX_REQUEST_BODY_BYTES as usize,
    };
    // This private bridge gives optional requests an actual dial source. The pinned
    // bridge opener has no per-request context; sharing one listener would require
    // guessing which caller caused its dial. Both bridges share the same authority,
    // ordered writer and admission gate, and their listeners shut down together.
    let optional_handle = spl_transport::journal_bridge::start(JournalBridgeConfig {
        opener: Arc::new(OptionalPrivateLinkOpener(Arc::clone(&opener))),
        bridge_names: bridge_names.clone(),
        endpoint_hosts: endpoint_hosts.clone(),
        policy: policy.clone(),
    })
    .await
    .map_err(|_| PrivateStateError::BridgeUnavailable)?;
    let (optional_client, optional_origin) = {
        let handle = &optional_handle;
        let bootstrap = handle
            .bootstrap_url()
            .ok_or(PrivateStateError::BootstrapFailed)?;
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
            .get(bootstrap)
            .timeout(BOOTSTRAP_TIMEOUT)
            .send()
            .await
            .map_err(|_| PrivateStateError::BootstrapFailed)?;
        if response.status() != StatusCode::FOUND {
            return Err(PrivateStateError::BootstrapFailed);
        }
        (client, origin)
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
    Ok(PrivateLinkSession {
        client,
        origin,
        opener,
        handle,
        optional_handle,
        optional_client,
        optional_origin,
        token_persistence,
        bootstrap_target: Some(bootstrap_url),
        _state_lock: Arc::new(state_lock),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::private_file::DurableWriteStage;
    use crate::private_link_test_peer::PrivateLinkPeer;
    use crate::sync_health::{ProcessEpoch, SyncFacts, load_facts_with_liveness, save_facts};
    use serde::Serialize;
    use spl_transport::credential::EndpointAddr;
    use std::{
        io::Cursor,
        net::TcpListener,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
        process::Command,
        sync::{
            Arc, Mutex,
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

    pub(crate) fn base64url_no_pad(input: &[u8]) -> String {
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

    pub(crate) fn test_jwt_for_instance(instance_id: &str, exp: i64) -> String {
        let iat = exp - 3600;
        let claims = format!(
            r#"{{"iss":"https://relay.example.com","sub":"instance:{instance_id}","aud":"spl-relay","scope":"session.dial","ver":2,"instance_id":"{instance_id}","iat":{iat},"exp":{exp},"jti":"test-jti"}}"#
        );
        format!(
            "{}.{}.sig",
            base64url_no_pad(b"{\"alg\":\"none\"}"),
            base64url_no_pad(claims.as_bytes())
        )
    }

    pub(crate) fn test_jwt(exp: i64) -> String {
        test_jwt_for_instance("inst-42", exp)
    }

    const RELAY_PAIR_LINK: &str = "0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXW00";

    fn relay_pair_link_for(origin: &str) -> String {
        assert!(u8::try_from(origin.len()).is_ok());
        let mut blob = vec![0x06];
        blob.extend_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        blob.push(0x01);
        blob.extend_from_slice(&[0xde; 16]);
        blob.push(origin.len() as u8);
        blob.extend_from_slice(origin.as_bytes());
        spl_core::crockford::encode(&blob)
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
        static CACHED: std::sync::OnceLock<Credential> = std::sync::OnceLock::new();
        CACHED
            .get_or_init(|| {
                use rcgen::{
                    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
                    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
                };
                let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
                let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
                ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                ca_params.key_usages = vec![
                    KeyUsagePurpose::DigitalSignature,
                    KeyUsagePurpose::KeyCertSign,
                ];
                let ca = ca_params.self_signed(&ca_key).unwrap();
                let client_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
                let mut client_params =
                    CertificateParams::new(vec!["observer.test".into()]).unwrap();
                client_params
                    .extended_key_usages
                    .push(ExtendedKeyUsagePurpose::ClientAuth);
                let client = client_params.signed_by(&client_key, &ca, &ca_key).unwrap();
                let ca_der = ca.der();
                Credential {
                    client_key_pem: client_key.serialize_pem(),
                    client_cert_pem: client.pem(),
                    ca_chain_pem: vec![ca.pem()],
                    ca_fp_prefix: spl_core::ca::sha256(ca_der.as_ref())[..16].to_vec(),
                    instance_id: "instance".into(),
                    home_label: "home".into(),
                    endpoints: vec![EndpointAddr {
                        host: "127.0.0.1".into(),
                        port: 7657,
                    }],
                    home_attestation: Some("attestation".into()),
                    local_endpoints: Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}])),
                    relay_origin: Some("https://relay.invalid".into()),
                    device_token: Some(test_jwt_for_instance("instance", 123)),
                    device_token_expires_at: Some(123),
                }
            })
            .clone()
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

    async fn start_keyless_peer_session(
        peer: &PrivateLinkPeer,
    ) -> (tempfile::TempDir, PrivateLinkSession) {
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        (temp, session)
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
        stage: DurableWriteStage,
        stages: Arc<Mutex<Vec<DurableWriteStage>>>,
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl DurableWriteFault for BlockingDirSyncFault {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            self.stages.lock().unwrap().push(stage);
            if stage == self.stage {
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
            Some(TransportClient::new(peer.credential(), None).unwrap()),
            None,
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
                stage: DurableWriteStage::DirSync,
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
            Some(TransportClient::new(peer.credential(), None).unwrap()),
            None,
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

    #[tokio::test]
    async fn no_relay_origin_credential_keeps_lan_path() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            owner.capability().probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        assert_eq!(peer.accepted_carriers(), 1);
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn relay_origin_without_device_token_uses_lan_only() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let relay = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let mut credential = peer.credential();
        credential.relay_origin = Some(format!("http://{}", relay.local_addr().unwrap()));
        credential.device_token = None;
        let owner = start_private_link_owner(temp.path(), credential, "stream")
            .await
            .unwrap();
        assert!(matches!(
            owner.capability().probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        assert_eq!(peer.accepted_carriers(), 1);
        assert!(
            tokio::time::timeout(Duration::ZERO, relay.accept())
                .await
                .is_err()
        );
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_lan_tls_attempts_relay_before_preserving_lan_timeout() {
        let peer = PrivateLinkPeer::start().await;
        let lan = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let lan_port = lan.local_addr().unwrap().port();
        let lan_accepted = Arc::new(AtomicUsize::new(0));
        let lan_count = lan_accepted.clone();
        let stalled = tokio::spawn(async move {
            let (_stream, _) = lan.accept().await.unwrap();
            lan_count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        });
        let relay = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let relay_origin = format!("http://{}", relay.local_addr().unwrap());
        let relay_accepted = Arc::new(AtomicUsize::new(0));
        let relay_count = relay_accepted.clone();
        let relay_task = tokio::spawn(async move {
            let (mut stream, _) = relay.accept().await.unwrap();
            relay_count.fetch_add(1, Ordering::SeqCst);
            let _ = read_http_head(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let mut lan_credential = peer.credential();
        lan_credential.endpoints = vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: lan_port,
        }];
        lan_credential.relay_origin = None;
        lan_credential.device_token = None;
        let mut relay_credential = peer.credential();
        relay_credential.endpoints.clear();
        relay_credential.relay_origin = Some(relay_origin);
        relay_credential.device_token = Some(test_jwt(i64::MAX / 2));
        let opener = Arc::new(PrivateLinkOpener::new(
            Some(TransportClient::new(lan_credential, None).unwrap()),
            Some(TransportClient::new_relay_only(relay_credential, None).unwrap()),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let dial = tokio::spawn({
            let opener = opener.clone();
            async move { opener.dial_carrier().await }
        });
        while lan_accepted.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(LAN_CARRIER_TIMEOUT).await;
        relay_task.await.unwrap();
        let error = match dial.await.unwrap() {
            Ok(_) => panic!("both carrier legs unexpectedly succeeded"),
            Err(error) => error,
        };
        match error {
            TransportError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                assert_eq!(error.to_string(), "LAN carrier dial timed out");
            }
            other => panic!("expected LAN timeout, got {other}"),
        }
        assert_eq!(relay_accepted.load(Ordering::SeqCst), 1);
        stalled.abort();
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

    struct IdentityAssertingPairer;

    impl Pairer for IdentityAssertingPairer {
        fn pair<'a>(
            &'a self,
            _link: &'a str,
            device_label: &'a str,
            additional_fields: &'a serde_json::Map<String, serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Credential, PrivateStateError>> + Send + 'a>>
        {
            assert_eq!(device_label, "suze");
            assert_eq!(
                additional_fields.get("client_label"),
                Some(&serde_json::Value::String("suze".to_owned()))
            );
            assert_eq!(
                additional_fields.get("platform"),
                Some(&serde_json::Value::String("linux".to_owned()))
            );
            Box::pin(async move { Ok(credential()) })
        }
    }

    #[test]
    fn pairing_identity_omits_only_an_invalid_client_label() {
        let empty = pairing_ceremony_identity("");
        assert!(!empty.contains_key("client_label"));
        assert_eq!(empty["platform"], "linux");

        let oversize = pairing_ceremony_identity(&"a".repeat(254));
        assert!(!oversize.contains_key("client_label"));
        assert_eq!(oversize["platform"], "linux");
    }

    #[tokio::test]
    async fn setup_passes_hostname_and_linux_platform_as_pairing_identity() {
        let temporary = tempfile::tempdir().unwrap();
        setup_with_pairer(
            &IdentityAssertingPairer,
            temporary.path(),
            &temporary.path().join("state"),
            "suze",
            std::io::Cursor::new(DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
        )
        .await
        .unwrap();
    }

    #[test]
    fn shared_pairlink_parser_classifies_direct_and_relay_setup_forms() {
        assert!(matches!(
            pairlink::parse(DIRECT_PAIR_LINK_FOR_TEST),
            Ok(ParsedPairLink::Direct(_))
        ));
        assert!(matches!(
            pairlink::parse(RELAY_PAIR_LINK),
            Ok(ParsedPairLink::Relay(_))
        ));
    }

    #[tokio::test]
    async fn pair_link_form_controls_persisted_carrier_candidates() {
        let direct_temp = tempfile::tempdir().unwrap();
        let relay_temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let mut source = peer.credential();
        source.local_endpoints = Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}]));
        source.relay_origin = Some("https://link.solstone.app".to_owned());
        source.device_token = Some(test_jwt(i64::MAX / 2));
        assert!(!source.endpoints.is_empty());
        assert!(source.local_endpoints.is_some());

        setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Some(source.clone()),
            },
            direct_temp.path(),
            &direct_temp.path().join("state"),
            "device",
            Cursor::new(DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
        )
        .await
        .unwrap();
        let direct = load_credential(direct_temp.path()).unwrap().unwrap();
        assert_eq!(direct, source);
        peer.enqueue_response(200, b"{}".to_vec());
        let direct_owner = start_private_link_owner(direct_temp.path(), direct, "stream")
            .await
            .unwrap();
        assert!(matches!(
            direct_owner.capability().probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        assert_eq!(
            peer.accepted_carriers(),
            1,
            "a direct-form pairing reloads with its LAN transport intact"
        );
        direct_owner.shutdown().await.unwrap();

        setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Some(source.clone()),
            },
            relay_temp.path(),
            &relay_temp.path().join("state"),
            "device",
            Cursor::new(RELAY_PAIR_LINK.as_bytes()),
        )
        .await
        .unwrap();
        let relay = load_credential(relay_temp.path()).unwrap().unwrap();
        let mut expected_relay = source;
        expected_relay.endpoints.clear();
        expected_relay.local_endpoints = None;
        assert_eq!(relay, expected_relay);
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn relay_pairing_refuses_to_persist_when_relay_transport_cannot_be_constructed() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let direct = peer.credential();
        persist_credential(temp.path(), &direct).unwrap();
        let credential_path = temp.path().join(CREDENTIALS_FILENAME);
        let before = fs::read(&credential_path).unwrap();
        let before_mode = fs::metadata(&credential_path).unwrap().permissions().mode() & 0o777;
        let mut valid_relay = peer.credential();
        valid_relay.local_endpoints = Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}]));
        valid_relay.relay_origin = Some("https://link.solstone.app".to_owned());
        valid_relay.device_token = Some(test_jwt(i64::MAX / 2));

        for invalid in [
            {
                let mut value = valid_relay.clone();
                value.relay_origin = None;
                value
            },
            {
                let mut value = valid_relay.clone();
                value.relay_origin = Some(String::new());
                value
            },
            {
                let mut value = valid_relay.clone();
                value.relay_origin = Some("https://other-relay.invalid".to_owned());
                value
            },
            {
                let mut value = valid_relay.clone();
                value.device_token = None;
                value
            },
            {
                let mut value = valid_relay.clone();
                value.device_token = Some(String::new());
                value
            },
            {
                let mut value = valid_relay.clone();
                value.client_key_pem = "not a private key".to_owned();
                value
            },
        ] {
            let error = setup_with_pairer(
                &FakePairer {
                    calls: Arc::new(AtomicUsize::new(0)),
                    result: Some(invalid),
                },
                temp.path(),
                &temp.path().join("state"),
                "device",
                Cursor::new(RELAY_PAIR_LINK.as_bytes()),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, PrivateStateError::PairingFailed));
            assert_eq!(fs::read(&credential_path).unwrap(), before);
            assert_eq!(
                fs::metadata(&credential_path).unwrap().permissions().mode() & 0o777,
                before_mode
            );
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }));
        }
        assert_eq!(load_credential(temp.path()).unwrap(), Some(direct));
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn relay_pairing_rejects_a_ceremony_result_bound_to_another_relay() {
        let temp = tempfile::tempdir().unwrap();
        let direct = credential();
        persist_credential(temp.path(), &direct).unwrap();
        let before = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        let mut mismatched = credential();
        mismatched.relay_origin = Some("https://other-relay.invalid".to_owned());
        mismatched.device_token = Some(test_jwt(i64::MAX / 2));
        let error = setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Some(mismatched),
            },
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(RELAY_PAIR_LINK.as_bytes()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, PrivateStateError::PairingFailed));
        assert_eq!(
            fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            before
        );
        assert_eq!(load_credential(temp.path()).unwrap(), Some(direct));
    }

    #[tokio::test]
    async fn failed_relay_pairing_preserves_an_existing_direct_credential() {
        let temp = tempfile::tempdir().unwrap();
        let direct = credential();
        persist_credential(temp.path(), &direct).unwrap();
        let before = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        let error = setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: None,
            },
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(RELAY_PAIR_LINK.as_bytes()),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, PrivateStateError::PairingFailed));
        assert_eq!(
            fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            before
        );
        assert_eq!(load_credential(temp.path()).unwrap(), Some(direct));
    }

    #[tokio::test]
    async fn relay_pairing_reloads_without_dialing_its_retired_lan_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let lan_decoy = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let mut paired = peer.credential();
        paired.endpoints = vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: lan_decoy.local_addr().unwrap().port(),
        }];
        paired.local_endpoints = Some(serde_json::json!([{
            "ip": "127.0.0.1",
            "port": lan_decoy.local_addr().unwrap().port(),
        }]));
        let relay = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let relay_origin = format!("http://{}", relay.local_addr().unwrap());
        let relay_pair_link = relay_pair_link_for(&relay_origin);
        paired.relay_origin = Some(relay_origin);
        paired.device_token = Some(test_jwt(i64::MAX / 2));
        paired.device_token_expires_at = Some(i64::MAX / 2);
        setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Some(paired),
            },
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(relay_pair_link.as_bytes()),
        )
        .await
        .unwrap();
        let reloaded = load_credential(temp.path()).unwrap().unwrap();
        assert!(reloaded.endpoints.is_empty());
        assert!(reloaded.local_endpoints.is_none());

        let relay_reply = tokio::spawn(async move {
            let (mut stream, _) = relay.accept().await.unwrap();
            let request = read_http_head(&mut stream).await;
            assert!(request.starts_with(b"GET /session/dial?instance="));
            stream
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let owner = start_private_link_owner(temp.path(), reloaded, "stream")
            .await
            .unwrap();
        assert!(matches!(
            owner.capability().probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        relay_reply.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), lan_decoy.accept())
                .await
                .is_err(),
            "a reloaded relay pairing must not dial retained LAN candidates"
        );
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_rejects_an_unparseable_pair_link_before_the_pairing_ceremony() {
        let temp = tempfile::tempdir().unwrap();
        let direct = credential();
        persist_credential(temp.path(), &direct).unwrap();
        let before = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let error = setup_with_pairer(
            &FakePairer {
                calls: calls.clone(),
                result: Some(credential()),
            },
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(b"not-a-pair-link"),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, PrivateStateError::PairInputInvalid));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let credential_path = temp.path().join(CREDENTIALS_FILENAME);
        assert_eq!(fs::read(&credential_path).unwrap(), before);
        assert_eq!(load_credential(temp.path()).unwrap(), Some(direct));
        assert_eq!(
            fs::metadata(credential_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn relay_credential_persistence_failures_never_leave_torn_state() {
        let peer = PrivateLinkPeer::start().await;
        let prior = peer.credential();
        let mut hybrid = peer.credential();
        hybrid.local_endpoints = Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}]));
        hybrid.relay_origin = Some("https://link.solstone.app".to_owned());
        hybrid.device_token = Some(test_jwt(i64::MAX / 2));
        let mut projected = hybrid.clone();
        projected.endpoints.clear();
        projected.local_endpoints = None;
        let projected_bytes = serde_json::to_vec(&projected).unwrap();

        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
            DurableWriteStage::DirSync,
        ] {
            let temp = tempfile::tempdir().unwrap();
            persist_credential(temp.path(), &prior).unwrap();
            let credential_path = temp.path().join(CREDENTIALS_FILENAME);
            let prior_bytes = fs::read(&credential_path).unwrap();
            let error = setup_with_pairer_and_stream_with_fault(
                &FakePairer {
                    calls: Arc::new(AtomicUsize::new(0)),
                    result: Some(hybrid.clone()),
                },
                temp.path(),
                &temp.path().join("state"),
                "device",
                None,
                Cursor::new(RELAY_PAIR_LINK.as_bytes()),
                Some(&FailStage(stage)),
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                PrivateStateError::Io {
                    operation: PrivateIoOperation::Persist,
                    ..
                }
            ));

            let current = fs::read(&credential_path).unwrap();
            if stage == DurableWriteStage::DirSync {
                assert!(current == prior_bytes || current == projected_bytes);
            } else {
                assert_eq!(current, prior_bytes);
            }
            let loaded: Credential = serde_json::from_slice(&current).unwrap();
            assert!(loaded == prior || loaded == projected);
            assert_eq!(
                fs::metadata(&credential_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            }));
        }
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn injected_pairer_persists_only_success() {
        let temp = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let pairer = FakePairer {
            calls: calls.clone(),
            result: None,
        };
        let failed = setup_with_pairer(
            &pairer,
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
        )
        .await;
        assert!(failed.is_err());
        drop(failed);
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
        let pairer = FakePairer {
            calls: calls.clone(),
            result: Some(credential()),
        };
        setup_with_pairer(
            &pairer,
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
        )
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
    fn credential_round_trip_preserves_private_permissions() {
        let temp = tempfile::tempdir().unwrap();
        ensure_private_directory(temp.path()).unwrap();
        persist_credential(temp.path(), &credential()).unwrap();
        assert_eq!(load_credential(temp.path()).unwrap(), Some(credential()));
        let metadata = fs::symlink_metadata(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn malformed_credential_errors_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(CREDENTIALS_FILENAME), b"{").unwrap();
        assert!(matches!(
            load_credential(temp.path()),
            Err(PrivateStateError::MalformedCredential)
        ));
    }

    #[test]
    fn private_state_files_reject_symlinks_without_touching_referent() {
        let temp = tempfile::tempdir().unwrap();
        let referent = temp.path().join("referent");
        fs::write(&referent, b"external").unwrap();
        fs::set_permissions(&referent, fs::Permissions::from_mode(0o644)).unwrap();
        let link = temp.path().join(CREDENTIALS_FILENAME);
        symlink(&referent, &link).unwrap();
        assert!(matches!(
            load_credential(temp.path()),
            Err(PrivateStateError::InvalidTarget { .. })
        ));
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
        let mut held = PrivateStateLock::acquire(temp.path()).unwrap();
        let lock_path = temp.path().join(PRIVATE_STATE_LOCK_FILENAME);
        let before = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            PrivateStateLock::try_probe(temp.path()).unwrap(),
            PrivateStateLockLiveness::LiveOwnerNotReady
        );
        held.mark_ready().unwrap();
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
    fn dropping_clone_keeps_ready_owner_locked_until_original_drops() {
        let temp = tempfile::tempdir().unwrap();
        let mut original = PrivateStateLock::acquire(temp.path()).unwrap();
        original.mark_ready().unwrap();
        let cloned = original.try_clone().unwrap();

        drop(cloned);
        assert_eq!(
            PrivateStateLock::try_probe(temp.path()).unwrap(),
            PrivateStateLockLiveness::LiveOwner
        );
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));

        drop(original);
        assert_eq!(
            PrivateStateLock::try_probe(temp.path()).unwrap(),
            PrivateStateLockLiveness::NoLiveOwner
        );
        assert!(PrivateStateLock::acquire(temp.path()).is_ok());
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
        assert!(!probe_lock_table("1: POSIX ADVISORY READ 1 00:00:1 0 EOF\n", 0, 2).unwrap());
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

        let mut held = PrivateStateLock::acquire(temp.path()).unwrap();
        held.mark_ready().unwrap();
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
        let mut held = PrivateStateLock::acquire(temp.path()).unwrap();
        held.mark_ready().unwrap();
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
                &temp.path().join("state"),
                "device",
                CountingReader(reads.clone()),
            )
            .await,
            Err(PrivateStateError::LockContended)
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(!temp.path().join(CREDENTIALS_FILENAME).exists());
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
    fn explicit_route_class_marker_malformed_and_duplicate_forms_are_rejected_locally() {
        for headers in [
            vec![(ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), String::new())],
            vec![(ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), " 1".to_owned())],
            vec![(ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), "1 ".to_owned())],
            vec![(ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), "2".to_owned())],
            vec![
                (ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), "2".to_owned()),
                (ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(), "2".to_owned()),
            ],
        ] {
            assert!(proxy_headers_for_v3(&headers).is_err());
        }
    }

    #[test]
    fn browser_headers_without_internal_route_marker_are_forwarded() {
        let headers =
            proxy_headers_for_v3(&[("accept".to_owned(), "text/html".to_owned())]).unwrap();
        assert_eq!(
            headers,
            vec![
                ("accept".to_owned(), "text/html".to_owned()),
                (PROTOCOL_VERSION_HEADER_NAME.to_owned(), "3".to_owned()),
            ]
        );
    }

    #[test]
    fn journal_media_downloads_stream_without_widening_other_bridge_routes() {
        let request = |method: &str, target: &str| RequestHead {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: Vec::new(),
        };

        assert!(streams_journal_response(&request("GET", "/sse/events")));
        assert!(streams_journal_response(&request(
            "GET",
            "/app/transcripts/api/serve_file/20260829/run/screen.mp4?download=1",
        )));
        assert!(!streams_journal_response(&request(
            "HEAD",
            "/app/transcripts/api/serve_file/20260829/run/screen.mp4",
        )));
        assert!(!streams_journal_response(&request("GET", "/app/timeline")));
        assert!(!streams_journal_response(&request(
            "GET",
            "/app/devices/ingest"
        )));
    }

    #[test]
    fn route_class_marker_is_stripped_before_forwarding() {
        let headers = proxy_headers_for_v3(&[(
            ROUTE_CLASS_MARKER_HEADER_NAME.to_owned(),
            INGEST_V3_ROUTE_CLASS.to_owned(),
        )])
        .unwrap();
        assert_eq!(
            headers,
            vec![(PROTOCOL_VERSION_HEADER_NAME.to_owned(), "3".to_owned())]
        );
    }

    #[tokio::test]
    async fn capability_authorized_browser_route_without_marker_reaches_the_peer() {
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session, capability) = session_with_capability(&peer).await;
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "GET /app/timeline HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability}\r\nAccept: text/html\r\n\r\n"
            ),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/app/timeline");
        assert_eq!(
            requests[0]
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER_NAME))
                .map(|(_, value)| value.as_str()),
            Some("3")
        );
        assert!(
            !requests[0]
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(ROUTE_CLASS_MARKER_HEADER_NAME))
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn v3_capability_operations_are_mtls_only() {
        let peer = PrivateLinkPeer::start().await;
        for body in [
            br#"{"status":"ok","segment":"120000_1"}"#.as_slice(),
            br#"{"days":{"20260101":{"segments":1}}}"#.as_slice(),
            br#"{"version":1,"day":"20260101","segments":{}}"#.as_slice(),
            br#"{"protocol_version":3,"total":0,"items":[]}"#.as_slice(),
        ] {
            peer.enqueue_response(200, body);
        }
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability();
        assert!(matches!(
            capability
                .ingest(multipart::Form::new().text("envelope", "{}"))
                .await,
            LinkOutcome::Success { .. }
        ));
        assert!(matches!(
            capability.probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        assert!(matches!(
            capability.manifest_day("20260101").await,
            LinkOutcome::Success { .. }
        ));
        assert!(matches!(
            capability.segments_day("20260101").await,
            LinkOutcome::Success { .. }
        ));
        peer.wait_for_requests(4).await;
        for (request, (method, path)) in peer.requests().into_iter().zip([
            ("POST", "/app/devices/ingest"),
            ("GET", "/app/devices/ingest/manifest"),
            ("GET", "/app/devices/ingest/manifest/20260101"),
            ("GET", "/app/devices/ingest/segments/20260101"),
        ]) {
            assert_eq!(
                (request.method.as_str(), request.path.as_str()),
                (method, path)
            );
            assert_eq!(
                request
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(PROTOCOL_VERSION_HEADER_NAME))
                    .map(|(_, value)| value.as_str()),
                Some("3")
            );
            assert!(!request.headers.iter().any(|(name, _)| {
                name.eq_ignore_ascii_case("authorization")
                    || name.eq_ignore_ascii_case(OBSERVER_HEADER_NAME)
            }));
        }
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn capability_rejects_admin_path_query_and_route_substitution() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability();
        for day in ["", "2026010", "202601011", "202601?1", "../20260101"] {
            assert!(matches!(
                capability.segments_day(day).await,
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
    async fn caller_reserved_auth_headers_are_rejected_before_dial() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session) = start_keyless_peer_session(&peer).await;
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
        assert_eq!(MAX_REQUEST_BODY_BYTES, 128 * 1024 * 1024);
    }

    #[tokio::test]
    async fn large_upload_staging_is_credit_bounded() {
        let peer = PrivateLinkPeer::start().await;
        peer.hold_request_credit();
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session) = start_keyless_peer_session(&peer).await;
        let body = vec![b'x'; spl_core::mux::INITIAL_WINDOW * 2 + 1];
        let form = reqwest::multipart::Form::new()
            .part("files", reqwest::multipart::Part::bytes(body.clone()));
        let upload = tokio::spawn({
            let capability = session.capability();
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
            session.capability().segments_day("20260101").await,
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
        let port = session.handle.port();
        let response = raw_local_request(
            port,
            format!(
                "POST /ingest HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: solstone_linux_cap={capability}\r\n{ROUTE_CLASS_MARKER_HEADER_NAME}: {INGEST_V3_ROUTE_CLASS}\r\n\r\ntrailing"
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
        peer.enqueue_response(302, Vec::new());
        peer.enqueue_response(302, Vec::new());
        let (_temp, session) = start_keyless_peer_session(&peer).await;
        let capability = session.capability();

        let response = session
            .request(Method::GET, "/redirect")
            .unwrap()
            .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);

        let put_res = capability
            .clients_self_put(b"{}".to_vec(), Duration::from_secs(5))
            .await;
        assert!(matches!(
            put_res,
            LinkOutcome::Success {
                status: StatusCode::FOUND,
                ..
            }
        ));

        let get_res = capability.relay_access_get(Duration::from_secs(5)).await;
        assert!(matches!(
            get_res,
            LinkOutcome::Success {
                status: StatusCode::FOUND,
                ..
            }
        ));

        assert_eq!(peer.requests().len(), 3);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn paired_peer_resumes_large_response_after_window_credit() {
        let peer = PrivateLinkPeer::start().await;
        let body = vec![b'x'; spl_core::mux::INITIAL_WINDOW + 131_072];
        peer.enqueue_response(200, body.clone());
        let (_temp, session) = start_keyless_peer_session(&peer).await;
        let received = session
            .request(Method::GET, "/large")
            .unwrap()
            .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS)
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
        peer.enqueue_response(200, b"{}".to_vec());
        peer.enqueue_response(200, b"{}".to_vec());
        let (_temp, session) = start_keyless_peer_session(&peer).await;
        let capability = session.capability();

        assert_eq!(
            session
                .request(Method::GET, "/proxy-proof")
                .unwrap()
                .header(ROUTE_CLASS_MARKER_HEADER_NAME, INGEST_V3_ROUTE_CLASS)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        let put_res = capability
            .clients_self_put(b"{}".to_vec(), Duration::from_secs(5))
            .await;
        assert!(matches!(
            put_res,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

        let get_res = capability.relay_access_get(Duration::from_secs(5)).await;
        assert!(matches!(
            get_res,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

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
        setup_with_pairer(
            &pairer,
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(DIRECT_PAIR_LINK_FOR_TEST.as_bytes()),
        )
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
        assert!(matches!(
            session.capability().segments_day("20260101").await,
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
        let facts = session.opener.facts.snapshot();
        assert!(facts.token_persistence_failure);
        assert!(!facts.transport_unavailable);
        session.opener.facts.publish(LinkFact::TransportUnavailable);
        assert!(session.opener.dial_carrier().await.is_ok());
        assert!(!session.opener.facts.snapshot().transport_unavailable);
        assert_eq!(peer.accepted_carriers(), 1);
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

    #[test]
    fn owner_epoch_advances_on_generation_reset_and_owner_loss_only() {
        let facts = LinkFacts::default();
        let initial = facts.owner_epoch();

        facts.publish(LinkFact::CarrierProven);
        assert_eq!(
            facts.owner_epoch(),
            initial,
            "publishing a fact must not itself advance the owner epoch"
        );

        facts.begin_owner_generation();
        let after_begin = facts.owner_epoch();
        assert_ne!(after_begin, initial);

        facts.owner_lost();
        let after_loss = facts.owner_epoch();
        assert_ne!(after_loss, after_begin);
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
        peer.enqueue_response(200, br#"{"items":[],"total":0}"#.to_vec());
        let response_gate = Arc::new(AtomicBool::new(false));
        peer.gate_queued_response_nonblocking(0, response_gate);
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let request = tokio::spawn({
            let capability = owner.capability();
            async move { capability.segments_day("20260101").await }
        });
        peer.wait_for_requests(1).await;
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
        let paired_inst = peer.credential().instance_id;
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let expires_at_str = chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let refreshed = test_jwt_for_instance(&paired_inst, exp);
        let relay_refreshed = refreshed.clone();
        let relay_expires_at = expires_at_str.clone();
        let relay = tokio::spawn(async move {
            let (mut refresh, _) = listener.accept().await.unwrap();
            let request = read_http_head(&mut refresh).await;
            assert!(
                request.starts_with(b"POST /token/refresh HTTP/1.1"),
                "{}",
                String::from_utf8_lossy(&request)
            );
            let body = serde_json::json!({
                "protocol_version": 2,
                "device_token": relay_refreshed,
                "expires_at": relay_expires_at,
            })
            .to_string();
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
        let relay_pair_link = relay_pair_link_for(&relay_origin);
        let mut paired = peer.credential();
        paired.local_endpoints = Some(serde_json::json!([{"ip":"127.0.0.1","port":7657}]));
        paired.relay_origin = Some(relay_origin);
        paired.device_token = Some(test_jwt_for_instance(&paired_inst, 1));
        paired.device_token_expires_at = Some(1);
        setup_with_pairer(
            &FakePairer {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Some(paired),
            },
            temp.path(),
            &temp.path().join("state"),
            "device",
            Cursor::new(relay_pair_link.as_bytes()),
        )
        .await
        .unwrap();
        let credential = load_credential(temp.path()).unwrap().unwrap();
        assert!(credential.endpoints.is_empty());
        assert!(credential.local_endpoints.is_none());
        let owner = start_private_link_owner(temp.path(), credential, "stream")
            .await
            .unwrap();
        assert!(matches!(
            owner.capability().probe_manifest().await,
            LinkOutcome::Success { .. }
        ));
        relay.await.unwrap();
        let persisted = load_credential(temp.path()).unwrap().unwrap();
        assert_eq!(persisted.device_token.as_deref(), Some(refreshed.as_str()));
        assert!(persisted.endpoints.is_empty());
        assert!(persisted.local_endpoints.is_none());
        owner.shutdown().await.unwrap();
        let persisted_after_shutdown = load_credential(temp.path()).unwrap().unwrap();
        assert_eq!(
            persisted_after_shutdown.device_token.as_deref(),
            Some(refreshed.as_str())
        );
        assert!(persisted_after_shutdown.endpoints.is_empty());
        assert!(persisted_after_shutdown.local_endpoints.is_none());
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

    #[tokio::test]
    async fn system_status_success_and_failures() {
        use serde_json::json;

        let server = crate::test_support::LinkedMockServer::new(vec![
            (200, json!({"version": {"current": "1.4.0"}})),
            (200, json!({"version": {"current": "2.0.0-beta+build.3"}})),
            (200, json!({"version": {"current": ""}})),
            (200, json!({"version": {}})),
            (200, json!({"invalid": "json-shape"})),
            (500, json!({"error": "internal"})),
        ])
        .await;
        let cap = server.capability();

        assert_eq!(cap.system_status().await, Ok(Some("1.4.0".to_string())));
        assert_eq!(
            cap.system_status().await,
            Ok(Some("2.0.0-beta+build.3".to_string()))
        );
        assert_eq!(cap.system_status().await, Ok(None));
        assert_eq!(cap.system_status().await, Ok(None));
        assert_eq!(cap.system_status().await, Ok(None));
        assert!(matches!(
            cap.system_status().await,
            Err(LinkOutcome::Success { status, .. }) if status == StatusCode::INTERNAL_SERVER_ERROR
        ));
    }

    #[test]
    fn sanitize_journal_version_rules() {
        assert_eq!(sanitize_journal_version("1.4.0"), Some("1.4.0".to_string()));
        assert_eq!(
            sanitize_journal_version("2.0.0-beta+build.3"),
            Some("2.0.0-beta+build.3".to_string())
        );
        assert_eq!(
            sanitize_journal_version("v1.2.3_rc1~patch"),
            Some("v1.2.3_rc1~patch".to_string())
        );
        assert_eq!(sanitize_journal_version(""), None);
        assert_eq!(sanitize_journal_version(&"a".repeat(65)), None);
        assert_eq!(sanitize_journal_version("1.4.0\n"), None);
        assert_eq!(sanitize_journal_version("1.4.0\r"), None);
        assert_eq!(sanitize_journal_version("1.4.0\x1b[31m"), None);
        assert_eq!(sanitize_journal_version("1.4.0\0"), None);
        assert_eq!(sanitize_journal_version("1.4.0 2.0"), None);
    }

    #[test]
    fn postrename_clear_keeps_retry_until_directory_sync_succeeds() {
        struct FailOnce(std::sync::atomic::AtomicBool);
        impl DurableWriteFault for FailOnce {
            fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
                if stage == DurableWriteStage::DirSync && self.0.swap(false, Ordering::SeqCst) {
                    return Err(io::Error::other("injected directory sync failure"));
                }
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let credential = Credential {
            relay_origin: Some("https://relay.example.com".to_owned()),
            device_token: Some("old-token".to_owned()),
            ..credential()
        };
        persist_credential(temp.path(), &credential).unwrap();
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_owned(),
            credential,
            Arc::new(FailOnce(AtomicBool::new(true))),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        );
        assert!(
            writer
                .commit_optional_clear(
                    &opener,
                    writer.pairing_id(),
                    writer.access_mutation_generation(),
                    opener.relay_incarnation()
                )
                .is_err()
        );
        assert!(
            load_credential(temp.path())
                .unwrap()
                .unwrap()
                .device_token
                .is_none()
        );
        assert!(
            writer.durable_clear_pending.lock().unwrap().is_some(),
            "visible cleared bytes do not prove directory durability"
        );
        assert_eq!(
            writer.retry_durable_reconciliation_if_pending(&opener),
            Ok(true)
        );
        assert!(writer.durable_clear_pending.lock().unwrap().is_none());
        assert_eq!(
            writer.retry_durable_reconciliation_if_pending(&opener),
            Ok(false)
        );
    }

    #[test]
    fn durable_clear_retry_then_ready() {
        let temp = tempfile::tempdir().unwrap();
        let cred = Credential {
            relay_origin: Some("https://relay.example.com".to_string()),
            device_token: Some("token1".to_string()),
            device_token_expires_at: Some(1000),
            ..credential()
        };
        serde_json::to_writer(
            std::fs::File::create(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            &cred,
        )
        .unwrap();

        let fault = Arc::new(RecordingFault {
            stages: Arc::new(Mutex::new(Vec::new())),
            fail: Some(DurableWriteStage::Rename),
        });
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred,
            fault,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();

        assert!(
            writer
                .commit_optional_clear(&opener, &pairing_id, access_gen, opener_inc)
                .is_err()
        );
        assert!(writer.durable_clear_pending.lock().unwrap().is_some());
        assert!(opener.relay_transport.lock().unwrap().is_none());

        let writer_no_fault = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            writer.current_credential(),
            Arc::new(NoWriteFault),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        *writer_no_fault.durable_clear_pending.lock().unwrap() =
            writer.durable_clear_pending.lock().unwrap().clone();
        writer_no_fault.access_mutation_generation.store(
            writer.access_mutation_generation.load(Ordering::SeqCst),
            Ordering::SeqCst,
        );

        assert_eq!(
            writer_no_fault.retry_durable_reconciliation_if_pending(&opener),
            Ok(true)
        );
        assert!(
            writer_no_fault
                .durable_clear_pending
                .lock()
                .unwrap()
                .is_none()
        );
        assert_eq!(writer_no_fault.current_credential().relay_origin, None);
        assert!(opener.relay_transport.lock().unwrap().is_none());

        let next_gen = writer_no_fault.access_mutation_generation();
        let next_inc = opener.relay_incarnation();
        let tok2 = test_jwt_for_instance(&writer_no_fault.current_credential().instance_id, 2000);
        assert!(
            writer_no_fault
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    next_gen,
                    next_inc,
                    "https://relay2.example.com",
                    &tok2,
                    2000,
                )
                .is_ok()
        );
        assert_eq!(
            writer_no_fault.current_credential().relay_origin.as_deref(),
            Some("https://relay2.example.com")
        );
        assert!(opener.relay_transport.lock().unwrap().is_some());
    }

    #[test]
    fn ready_then_stale_durable_clear_retry_noops() {
        let temp = tempfile::tempdir().unwrap();
        let cred = Credential {
            relay_origin: Some("https://relay.example.com".to_string()),
            device_token: Some("token1".to_string()),
            device_token_expires_at: Some(1000),
            ..credential()
        };
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred,
            Arc::new(NoWriteFault),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let pairing_id = writer.pairing_id().to_string();

        *writer.durable_clear_pending.lock().unwrap() = Some(DurableClearPending {
            pairing_id: pairing_id.clone(),
            access_mutation_generation: 0,
        });

        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();
        let tok_ready = test_jwt_for_instance(&writer.current_credential().instance_id, 3000);
        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    access_gen,
                    opener_inc,
                    "https://relay-ready.example.com",
                    &tok_ready,
                    3000,
                )
                .is_ok()
        );
        assert!(opener.relay_transport.lock().unwrap().is_some());

        assert_eq!(
            writer.retry_durable_reconciliation_if_pending(&opener),
            Ok(false)
        );
        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://relay-ready.example.com")
        );
        assert!(opener.relay_transport.lock().unwrap().is_some());
    }

    #[test]
    fn late_io_after_timeout_cannot_apply_after_newer_commit() {
        let temp = tempfile::tempdir().unwrap();
        let cred = credential();
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred,
            Arc::new(NoWriteFault),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let pairing_id = writer.pairing_id().to_string();
        let gen1 = writer.access_mutation_generation();
        let inc1 = opener.relay_incarnation();
        let tok_b = test_jwt_for_instance(&writer.current_credential().instance_id, 2000);
        let tok_a = test_jwt_for_instance(&writer.current_credential().instance_id, 1000);

        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    gen1,
                    inc1,
                    "https://b.example.com",
                    &tok_b,
                    2000
                )
                .is_ok()
        );

        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    gen1,
                    inc1,
                    "https://a.example.com",
                    &tok_a,
                    1000
                )
                .is_err()
        );
        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://b.example.com")
        );

        assert!(
            writer
                .commit_optional_clear(&opener, &pairing_id, gen1, inc1)
                .is_err()
        );
        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://b.example.com")
        );
    }

    #[test]
    fn optional_rename_failure_preserves_previous() {
        let temp = tempfile::tempdir().unwrap();
        let cred = Credential {
            relay_origin: Some("https://orig.example.com".to_string()),
            device_token: Some("orig_token".to_string()),
            ..credential()
        };
        serde_json::to_writer(
            std::fs::File::create(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            &cred,
        )
        .unwrap();

        let fault = Arc::new(RecordingFault {
            stages: Arc::new(Mutex::new(Vec::new())),
            fail: Some(DurableWriteStage::Rename),
        });
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred.clone(),
            fault,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();
        let new_tok = test_jwt_for_instance(&writer.current_credential().instance_id, 5000);

        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    access_gen,
                    opener_inc,
                    "https://new.example.com",
                    &new_tok,
                    5000
                )
                .is_err()
        );
        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://orig.example.com")
        );
        assert!(opener.relay_transport.lock().unwrap().is_none());
        assert!(!writer.failed());
    }

    #[test]
    fn optional_dirsync_failure_old_or_new_parseable_no_live_replace() {
        let temp = tempfile::tempdir().unwrap();
        let cred = Credential {
            relay_origin: Some("https://orig.example.com".to_string()),
            device_token: Some("orig_token".to_string()),
            ..credential()
        };
        serde_json::to_writer(
            std::fs::File::create(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            &cred,
        )
        .unwrap();

        let fault = Arc::new(RecordingFault {
            stages: Arc::new(Mutex::new(Vec::new())),
            fail: Some(DurableWriteStage::DirSync),
        });
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred.clone(),
            fault,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();
        let new_tok = test_jwt_for_instance(&writer.current_credential().instance_id, 5000);

        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    &pairing_id,
                    access_gen,
                    opener_inc,
                    "https://new.example.com",
                    &new_tok,
                    5000
                )
                .is_err()
        );
        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://orig.example.com")
        );
        assert!(opener.relay_transport.lock().unwrap().is_none());
        assert!(writer.durable_ready_pending.lock().unwrap().is_some());
        assert!(!writer.failed());
    }

    #[test]
    fn stale_token_hook_after_disable_does_not_resurrect_or_latch_token_persistence_failure() {
        let temp = tempfile::tempdir().unwrap();
        let cred = Credential {
            relay_origin: Some("https://orig.example.com".to_string()),
            device_token: Some("orig_token".to_string()),
            ..credential()
        };
        serde_json::to_writer(
            std::fs::File::create(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
            &cred,
        )
        .unwrap();

        let facts = LinkFacts::default();
        let transport_unavail = Arc::new(AtomicBool::new(false));
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred,
            Arc::new(NoWriteFault),
            transport_unavail,
            facts.clone(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));

        let stale_hook = writer.create_token_hook();

        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();
        assert!(
            writer
                .commit_optional_clear(&opener, &pairing_id, access_gen, opener_inc)
                .is_ok()
        );
        assert_eq!(writer.current_credential().relay_origin, None);
        assert!(opener.relay_transport.lock().unwrap().is_none());

        stale_hook("resurrect_token", 99999);

        assert_eq!(writer.current_credential().relay_origin, None);
        assert_eq!(writer.current_credential().device_token, None);
        assert!(!writer.failed());
        let (fact_state, _) = facts.snapshot_with_epoch();
        assert!(!fact_state.token_persistence_failure);
    }

    #[test]
    fn same_home_repair_pairing_id_rejects_stale_access_commit() {
        let temp = tempfile::tempdir().unwrap();
        let cred = credential();
        let writer = Arc::new(OrderedCredentialWriter::new(
            temp.path().to_path_buf(),
            cred,
            Arc::new(NoWriteFault),
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let opener = Arc::new(PrivateLinkOpener::new(
            None,
            None,
            Arc::new(AtomicBool::new(false)),
            LinkFacts::default(),
        ));
        let access_gen = writer.access_mutation_generation();
        let opener_inc = opener.relay_incarnation();

        assert!(
            writer
                .commit_optional_relay(
                    &opener,
                    "different_pairing_id",
                    access_gen,
                    opener_inc,
                    "https://relay.example.com",
                    "tok",
                    1000
                )
                .is_err()
        );
        assert!(
            writer
                .commit_optional_clear(&opener, "different_pairing_id", access_gen, opener_inc)
                .is_err()
        );
    }

    #[tokio::test]
    async fn relay_ready_next_dial_uses_new_origin_without_repair() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();

        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability();
        let initial_inc = capability.opener_relay_incarnation();

        let instance_id = capability.writer().current_credential().instance_id;
        let peer_ep = &peer.credential().endpoints[0];
        let ready_exp = chrono::Utc::now().timestamp() + 3600;
        let ready_expires_at = chrono::DateTime::from_timestamp(ready_exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let relay_origin = format!("https://{}:{}", peer_ep.host, peer_ep.port);
        let ready_resp = serde_json::json!({
            "protocol_version": 2,
            "status": "ready",
            "relay_origin": relay_origin,
            "instance_id": instance_id,
            "device_token": test_jwt_for_instance(&instance_id, ready_exp),
            "expires_at": ready_expires_at,
        });
        peer.enqueue_response(200, serde_json::to_vec(&ready_resp).unwrap());

        assert!(
            crate::private_link_access::execute_relay_access_sync(
                &capability,
                Duration::from_secs(5)
            )
            .await
            .is_ok()
        );

        assert_eq!(
            capability
                .writer()
                .current_credential()
                .relay_origin
                .as_deref(),
            Some(format!("https://{}:{}", peer_ep.host, peer_ep.port).as_str())
        );
        assert!(
            capability
                .opener()
                .relay_transport
                .lock()
                .unwrap()
                .is_some()
        );
        assert!(capability.opener_relay_incarnation() > initial_inc);

        peer.enqueue_response(200, b"{}".to_vec());
        let outcome = capability.clients_self_get(Duration::from_secs(5)).await;
        assert!(matches!(
            outcome,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn relay_lan_only_to_ready_to_not_configured_without_restart() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();

        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability();
        let instance_id = capability.writer().current_credential().instance_id;
        let peer_ep = &peer.credential().endpoints[0];

        let ready_exp = chrono::Utc::now().timestamp() + 3600;
        let ready_expires_at = chrono::DateTime::from_timestamp(ready_exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let ready_resp = serde_json::json!({
            "protocol_version": 2,
            "status": "ready",
            "relay_origin": format!("https://{}:{}", peer_ep.host, peer_ep.port),
            "instance_id": instance_id,
            "device_token": test_jwt_for_instance(&instance_id, ready_exp),
            "expires_at": ready_expires_at,
        });
        peer.enqueue_response(200, serde_json::to_vec(&ready_resp).unwrap());
        assert!(
            crate::private_link_access::execute_relay_access_sync(
                &capability,
                Duration::from_secs(5)
            )
            .await
            .is_ok()
        );
        assert!(
            capability
                .opener()
                .relay_transport
                .lock()
                .unwrap()
                .is_some()
        );

        let not_configured = serde_json::json!({
            "protocol_version": 2,
            "status": "not_configured"
        });
        peer.enqueue_response(200, serde_json::to_vec(&not_configured).unwrap());
        assert!(
            crate::private_link_access::execute_relay_access_sync(
                &capability,
                Duration::from_secs(5)
            )
            .await
            .is_ok()
        );

        assert!(
            capability
                .opener()
                .relay_transport
                .lock()
                .unwrap()
                .is_none()
        );
        peer.enqueue_response(200, b"{}".to_vec());
        let outcome = capability.clients_self_get(Duration::from_secs(5)).await;
        assert!(matches!(
            outcome,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn stale_gated_relay_access_response_cannot_mutate_opener_after_newer_commit() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability();
        let instance_id = capability.writer().current_credential().instance_id;

        let ready_old_exp = chrono::Utc::now().timestamp() + 3600;
        let ready_old_expires_at = chrono::DateTime::from_timestamp(ready_old_exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let ready_old = serde_json::json!({
            "protocol_version": 2,
            "status": "ready",
            "relay_origin": "https://old-relay.example.com",
            "instance_id": instance_id,
            "device_token": test_jwt_for_instance(&instance_id, ready_old_exp),
            "expires_at": ready_old_expires_at,
        });
        let gate = Arc::new(tokio::sync::Notify::new());
        peer.enqueue_gated_response(200, serde_json::to_vec(&ready_old).unwrap(), gate.clone());

        let cap_clone = capability.clone();
        let sync_task = tokio::spawn(async move {
            crate::private_link_access::execute_relay_access_sync(
                &cap_clone,
                Duration::from_secs(5),
            )
            .await
        });

        peer.wait_for_requests(1).await;

        let writer = capability.writer();
        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_inc = capability.opener_relay_incarnation();
        let newer_exp = chrono::Utc::now().timestamp() + 7200;
        let new_tok = test_jwt_for_instance(&instance_id, newer_exp);
        assert!(
            writer
                .commit_optional_relay(
                    &capability.opener(),
                    &pairing_id,
                    access_gen,
                    opener_inc,
                    "https://newer-relay.example.com",
                    &new_tok,
                    newer_exp,
                )
                .is_ok()
        );

        gate.notify_waiters();

        let _ = sync_task.await;

        assert_eq!(
            writer.current_credential().relay_origin.as_deref(),
            Some("https://newer-relay.example.com")
        );
        assert!(
            capability
                .opener()
                .relay_transport
                .lock()
                .unwrap()
                .is_some()
        );

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn request_opens_carrier_does_not_loop_after_settle() {
        let peer = PrivateLinkPeer::start().await;
        let (_temp, session) = start_keyless_peer_session(&peer).await;
        let capability = session.capability();

        peer.enqueue_response(200, b"{}".to_vec());
        let outcome = capability.clients_self_get(Duration::from_secs(5)).await;
        assert!(matches!(
            outcome,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

        let (facts, _) = capability.facts().snapshot_with_epoch();
        assert!(facts.carrier_proven);
        let dial_gen_initial = facts.dial_generation;

        peer.enqueue_response(200, b"{}".to_vec());
        let outcome2 = capability.clients_self_get(Duration::from_secs(5)).await;
        assert!(matches!(
            outcome2,
            LinkOutcome::Success {
                status: StatusCode::OK,
                ..
            }
        ));

        let (facts2, _) = capability.facts().snapshot_with_epoch();
        assert_eq!(facts2.dial_generation, dial_gen_initial);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn optional_fsync_timeout_keeps_runtime_and_root_ownership_alive() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let session = start_private_link_session_inner(
            temp.path(),
            peer.credential(),
            "stream",
            SessionStartOptions {
                persistence_fault: Arc::new(BlockingDirSyncFault {
                    stage: DurableWriteStage::DirSync,
                    stages: Arc::new(Mutex::new(Vec::new())),
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
                ..SessionStartOptions::default()
            },
        )
        .await
        .unwrap();
        let cap = session.capability();
        peer.enqueue_response(
            200,
            br#"{"protocol_version":2,"status":"not_configured"}"#.to_vec(),
        );
        let job_cap = cap.clone();
        let job = tokio::spawn(async move {
            crate::private_link_access::execute_relay_access_sync(&job_cap, Duration::from_secs(2))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while entered_rx.try_recv().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // This timer and the HTTP job deadline must progress on the single Tokio thread.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(job.await.unwrap().is_err());
        session.shutdown().await.unwrap();
        assert!(PrivateStateLock::acquire(temp.path()).is_err());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(lock) = PrivateStateLock::acquire(temp.path()) {
                    drop(lock);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            load_credential(temp.path())
                .unwrap()
                .unwrap()
                .relay_origin
                .is_none()
        );
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn expired_optional_staging_never_renames_credentials_or_metadata() {
        for (metadata, abrupt) in [(false, false), (true, false), (false, true), (true, true)] {
            let peer = PrivateLinkPeer::start().await;
            let temp = tempfile::tempdir().unwrap();
            persist_credential(temp.path(), &peer.credential()).unwrap();
            let prior = fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let session = start_private_link_session_inner(
                temp.path(),
                peer.credential(),
                "stream",
                SessionStartOptions {
                    persistence_fault: Arc::new(BlockingDirSyncFault {
                        stage: DurableWriteStage::Rename,
                        stages: Arc::new(Mutex::new(Vec::new())),
                        entered: entered_tx,
                        release: Mutex::new(release_rx),
                    }),
                    ..SessionStartOptions::default()
                },
            )
            .await
            .unwrap();
            let cap = session.capability();
            let body = if metadata {
                br#"{"protocol_version":1,"revision":1,"reported":null,"owner_label":null,"updated_at":null,"display_label":"test","journal":{"name":"late","version":"1.0"}}"#.to_vec()
            } else {
                br#"{"protocol_version":2,"status":"not_configured"}"#.to_vec()
            };
            peer.enqueue_response(200, body);
            let path = temp.path().to_path_buf();
            let job = tokio::spawn(async move {
                if metadata {
                    crate::private_link_metadata::execute_metadata_sync(
                        &cap,
                        &path,
                        "ignored",
                        crate::private_link_metadata::DeviceSnapshot::default,
                        Duration::from_secs(2),
                    )
                    .await
                } else {
                    crate::private_link_access::execute_relay_access_sync(
                        &cap,
                        Duration::from_secs(2),
                    )
                    .await
                }
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while entered_rx.try_recv().is_err() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(job.await.unwrap().is_err());
            if abrupt {
                drop(finish_owner_start(session).await.unwrap());
            } else {
                session.shutdown().await.unwrap();
            }
            assert!(PrivateStateLock::acquire(temp.path()).is_err());
            release_tx.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(lock) = PrivateStateLock::acquire(temp.path()) {
                        drop(lock);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                fs::read(temp.path().join(CREDENTIALS_FILENAME)).unwrap(),
                prior
            );
            assert!(crate::sync_health::load_paired_journal_version(temp.path()).is_none());
            assert_eq!(peer.requests().len(), 1);
            peer.shutdown().await;
        }
    }
    #[tokio::test]
    async fn owner_retired_during_actual_carrier_publication_cannot_restore_proof() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let retired = Arc::new(AtomicBool::new(false));
        let retire = Arc::clone(&retired);
        session.opener.facts.install_sink(Arc::new(move |facts| {
            if facts.snapshot().carrier_proven && !retire.swap(true, Ordering::AcqRel) {
                facts.owner_lost();
            }
        }));
        assert!(session.opener.dial_carrier().await.is_err());
        assert!(retired.load(Ordering::Acquire));
        assert_eq!(peer.accepted_carriers(), 1);
        assert!(!session.opener.facts.snapshot().carrier_proven);
        assert!(!session.opener.facts.snapshot().observer_registered);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
