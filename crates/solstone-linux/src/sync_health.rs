// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::private_link::{LinkFactState, PrivateStateLockLiveness};
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub const SCHEMA_VERSION: u64 = 2;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ErrorType {
    Auth,
    Client,
    Transient,
    Incompatible,
}

impl ErrorType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Client => "client",
            Self::Transient => "transient",
            Self::Incompatible => "incompatible",
        }
    }

    fn parse(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "auth" => Some(Self::Auth),
            "client" => Some(Self::Client),
            "transient" => Some(Self::Transient),
            "incompatible" => Some(Self::Incompatible),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum HealthState {
    UnsafeLinkState,
    RePairRequired,
    TokenPersistenceFailed,
    PairingRequired,
    UpdateRequired,
    TransportUnavailable,
    Offline,
    ListenerReady,
    NotReported,
    Connecting,
    Syncing,
    Connected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEpoch(String);

impl ProcessEpoch {
    pub(crate) fn generate() -> io::Result<Self> {
        use std::io::Read;
        let mut bytes = [0_u8; 32];
        fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn for_test(value: u8) -> Self {
        Self(format!("{value:02x}").repeat(32))
    }

    fn parse(value: &Value) -> Option<Self> {
        let value = value.as_str()?;
        (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Self(value.to_ascii_lowercase()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncFacts {
    pub last_successful_sync: Option<f64>,
    pub last_successful_contact: Option<f64>,
    pub last_error_class: Option<ErrorType>,
    pub last_error_code: Option<i64>,
    pub pending_confirmed: Option<i64>,
    pub in_progress: bool,
    pub progress: String,
    #[doc(hidden)]
    pub(crate) link: Option<LinkFactState>,
    #[doc(hidden)]
    pub(crate) link_epoch: Option<ProcessEpoch>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthSurface {
    pub header_recording: &'static str,
    pub header_idle: &'static str,
    pub sync_line: &'static str,
    pub tooltip: &'static str,
    pub accessible_recording: &'static str,
    pub accessible_idle: &'static str,
    pub icon: &'static str,
    pub sni: &'static str,
    pub cli: &'static str,
    pub doctor_severity: &'static str,
    pub doctor_detail: &'static str,
    pub dbus: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyncHealth {
    pub state: HealthState,
    pub header_recording: String,
    pub header_idle: String,
    pub sync_line: String,
    pub tooltip: String,
    pub accessible_recording: String,
    pub accessible_idle: String,
    pub icon: String,
    pub sni_status: String,
    pub cli: String,
    pub doctor_severity: String,
    pub doctor_detail: String,
    pub dbus: String,
    pub pending_display: String,
    pub last_success_age: Option<f64>,
    pub progress: String,
}

pub static SURFACE_BY_STATE: LazyLock<HashMap<HealthState, HealthSurface>> = LazyLock::new(|| {
    HashMap::from([
        (
            HealthState::UnsafeLinkState,
            HealthSurface {
                header_recording: "on, pairing unsafe",
                header_idle: "idle, pairing unsafe",
                sync_line: "sync: pairing unsafe",
                tooltip: "sync: pairing unsafe; repair this device's pairing and restart the solstone app",
                accessible_recording: "on, pairing unsafe",
                accessible_idle: "idle, pairing unsafe",
                icon: "attention",
                sni: "NeedsAttention",
                cli: "Sync: pairing unsafe; repair this device's pairing and restart the solstone app",
                doctor_severity: "fail",
                doctor_detail: "sync health: pairing unsafe; repair this device's pairing and restart the solstone app",
                dbus: "unsafe-link-state",
            },
        ),
        (
            HealthState::RePairRequired,
            HealthSurface {
                header_recording: "on, pair again",
                header_idle: "idle, pair again",
                sync_line: "sync: pair again",
                tooltip: "sync: pair this device with your journal again",
                accessible_recording: "on, pair again",
                accessible_idle: "idle, pair again",
                icon: "attention",
                sni: "NeedsAttention",
                cli: "Sync: pair again; pair this device with your journal again",
                doctor_severity: "fail",
                doctor_detail: "sync health: pair this device with your journal again",
                dbus: "re-pair-required",
            },
        ),
        (
            HealthState::TokenPersistenceFailed,
            HealthSurface {
                header_recording: "on, pairing not saved",
                header_idle: "idle, pairing not saved",
                sync_line: "sync: pairing not saved",
                tooltip: "sync: pairing not saved; fix this device's pairing permissions, then restart the solstone app",
                accessible_recording: "on, pairing not saved",
                accessible_idle: "idle, pairing not saved",
                icon: "error",
                sni: "NeedsAttention",
                cli: "Sync: pairing not saved; fix this device's pairing permissions, then restart the solstone app",
                doctor_severity: "fail",
                doctor_detail: "sync health: pairing not saved; fix this device's pairing permissions, then restart the solstone app",
                dbus: "token-persistence-failed",
            },
        ),
        (
            HealthState::PairingRequired,
            HealthSurface {
                header_recording: "on, pairing required",
                header_idle: "idle, pairing required",
                sync_line: "sync: pairing required",
                tooltip: "sync: pair this device with your journal",
                accessible_recording: "on, pairing required",
                accessible_idle: "idle, pairing required",
                icon: "attention",
                sni: "NeedsAttention",
                cli: "Sync: pairing required; pair this device with your journal",
                doctor_severity: "fail",
                doctor_detail: "sync health: pair this device with your journal",
                dbus: "pairing-required",
            },
        ),
        (
            HealthState::UpdateRequired,
            HealthSurface {
                header_recording: "on, update required",
                header_idle: "idle, update required",
                sync_line: "sync: update the solstone app",
                tooltip: "sync: update required; update the solstone app",
                accessible_recording: "on, update required",
                accessible_idle: "idle, update required",
                icon: "attention",
                sni: "NeedsAttention",
                cli: "Sync: update required; update the solstone app",
                doctor_severity: "fail",
                doctor_detail: "sync health: update required; update the solstone app",
                dbus: "update-required",
            },
        ),
        (
            HealthState::TransportUnavailable,
            HealthSurface {
                header_recording: "on, connection unavailable (held on this device)",
                header_idle: "idle, connection unavailable (held on this device)",
                sync_line: "sync: connection unavailable; held on this device",
                tooltip: "sync: connection unavailable; restart the solstone app; if this continues, pair this device again",
                accessible_recording: "on, connection unavailable, held on this device",
                accessible_idle: "idle, connection unavailable, held on this device",
                icon: "offline",
                sni: "NeedsAttention",
                cli: "Sync: connection unavailable; held on this device; restart the solstone app; if this continues, pair this device again",
                doctor_severity: "fail",
                doctor_detail: "sync health: connection unavailable; restart the solstone app; if this continues, pair this device again",
                dbus: "transport-unavailable",
            },
        ),
        (
            HealthState::Offline,
            HealthSurface {
                header_recording: "on, offline (held on this device)",
                header_idle: "idle, offline (held on this device)",
                sync_line: "sync: offline; will retry",
                tooltip: "sync: offline; held on this device; will retry",
                accessible_recording: "on, offline, held on this device",
                accessible_idle: "idle, offline, held on this device",
                icon: "offline",
                sni: "Active",
                cli: "Sync: offline; held on this device; will retry",
                doctor_severity: "warn",
                doctor_detail: "sync health: offline; held on this device; will retry",
                dbus: "offline",
            },
        ),
        (
            HealthState::ListenerReady,
            HealthSurface {
                header_recording: "on, confirming with your journal",
                header_idle: "idle, confirming with your journal",
                sync_line: "sync: confirming with your journal",
                tooltip: "sync: wait while this device confirms with your journal",
                accessible_recording: "on, confirming with your journal",
                accessible_idle: "idle, confirming with your journal",
                icon: "connecting",
                sni: "Active",
                cli: "Sync: wait while this device confirms with your journal",
                doctor_severity: "warn",
                doctor_detail: "sync health: wait while this device confirms with your journal",
                dbus: "listener-ready",
            },
        ),
        (
            HealthState::NotReported,
            HealthSurface {
                header_recording: "on, no status",
                header_idle: "idle, no status",
                sync_line: "sync: no status",
                tooltip: "sync: no status from the solstone app right now",
                accessible_recording: "on, no status",
                accessible_idle: "idle, no status",
                icon: "offline",
                sni: "Active",
                cli: "Sync: no status; the solstone app is not running, or has no status yet",
                doctor_severity: "warn",
                doctor_detail: "the solstone app is not running, or has no status yet",
                dbus: "not-reported",
            },
        ),
        (
            HealthState::Connecting,
            HealthSurface {
                header_recording: "on, connecting",
                header_idle: "idle, connecting",
                sync_line: "sync: connecting",
                tooltip: "sync: wait while this device connects to your journal",
                accessible_recording: "on, connecting",
                accessible_idle: "idle, connecting",
                icon: "connecting",
                sni: "Active",
                cli: "Sync: connecting; wait while this device connects to your journal",
                doctor_severity: "warn",
                doctor_detail: "sync health: connecting; wait while this device connects to your journal",
                dbus: "connecting",
            },
        ),
        (
            HealthState::Syncing,
            HealthSurface {
                header_recording: "on, syncing",
                header_idle: "idle, syncing",
                sync_line: "sync: {progress}",
                tooltip: "sync: {progress}",
                accessible_recording: "on, syncing",
                accessible_idle: "idle, syncing",
                icon: "healthy",
                sni: "Active",
                cli: "Sync: syncing; your journal is receiving; not confirmed yet",
                doctor_severity: "ok",
                doctor_detail: "sync health: your journal is receiving; not confirmed yet",
                dbus: "syncing",
            },
        ),
        (
            HealthState::Connected,
            HealthSurface {
                header_recording: "on, connected",
                header_idle: "idle, connected",
                sync_line: "sync: up to date",
                tooltip: "sync: up to date",
                accessible_recording: "on, sync up to date",
                accessible_idle: "idle, sync up to date",
                icon: "healthy",
                sni: "Active",
                cli: "Sync: connected; up to date",
                doctor_severity: "ok",
                doctor_detail: "sync health: up to date at {sync_ts}",
                dbus: "connected",
            },
        ),
    ])
});

#[cfg(test)]
fn format_age(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "unknown".to_owned();
    };
    let seconds = (seconds as i64).max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

fn format_ts(timestamp: Option<f64>) -> String {
    let Some(timestamp) = timestamp else {
        return "unknown".to_owned();
    };
    let seconds = timestamp.floor() as i64;
    let nanos = ((timestamp - timestamp.floor()) * 1e9) as u32;
    DateTime::from_timestamp(seconds, nanos)
        .map(|datetime| {
            datetime
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn fill(template: &str, values: &HashMap<&str, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find('}') else {
            result.push_str(&rest[open..]);
            return result;
        };
        let key = &after_open[..close];
        if let Some(value) = values.get(key) {
            result.push_str(value);
        } else {
            result.push_str(&rest[open..open + close + 2]);
        }
        rest = &after_open[close + 1..];
    }
    result.push_str(rest);
    result
}

pub fn derive_health(facts: &SyncFacts, now: f64, stale_threshold: f64) -> SyncHealth {
    let link_missing = facts.link.is_none();
    let link = facts.link.clone().unwrap_or_default();
    let stale = facts
        .last_successful_contact
        .is_some_and(|contact| now - contact > stale_threshold);
    let terminal_auth =
        facts.last_error_class == Some(ErrorType::Auth) && facts.last_error_code != Some(401);
    let connected = link.carrier_proven
        && link.observer_registered
        && facts.pending_confirmed == Some(0)
        && !facts.in_progress;
    let state = if link.private_state_invalid || link.config_sanitation_failed {
        HealthState::UnsafeLinkState
    } else if link.terminal_revocation || terminal_auth {
        HealthState::RePairRequired
    } else if link.token_persistence_failure {
        HealthState::TokenPersistenceFailed
    } else if link.pairing_required {
        HealthState::PairingRequired
    } else if facts.last_error_class == Some(ErrorType::Incompatible) {
        HealthState::UpdateRequired
    } else if link.transport_unavailable {
        HealthState::TransportUnavailable
    } else if facts.last_error_class == Some(ErrorType::Transient) || stale {
        HealthState::Offline
    } else if link.listener_ready && !link.observer_registered {
        HealthState::ListenerReady
    } else if link_missing {
        HealthState::NotReported
    } else if !link.observer_registered {
        HealthState::Connecting
    } else if connected {
        HealthState::Connected
    } else {
        HealthState::Syncing
    };
    let surface = SURFACE_BY_STATE
        .get(&state)
        .expect("every health state must have a surface");
    let progress = if facts.progress.trim().is_empty() {
        "syncing..."
    } else {
        facts.progress.trim()
    };
    let values = HashMap::from([
        ("progress", progress.to_owned()),
        ("sync_ts", format_ts(facts.last_successful_sync)),
    ]);
    SyncHealth {
        state,
        header_recording: fill(surface.header_recording, &values),
        header_idle: fill(surface.header_idle, &values),
        sync_line: fill(surface.sync_line, &values),
        tooltip: fill(surface.tooltip, &values),
        accessible_recording: fill(surface.accessible_recording, &values),
        accessible_idle: fill(surface.accessible_idle, &values),
        icon: surface.icon.to_owned(),
        sni_status: surface.sni.to_owned(),
        cli: fill(surface.cli, &values),
        doctor_severity: surface.doctor_severity.to_owned(),
        doctor_detail: fill(surface.doctor_detail, &values),
        dbus: surface.dbus.to_owned(),
        pending_display: if state == HealthState::Connected {
            "0 pending"
        } else {
            "pending unconfirmed"
        }
        .to_owned(),
        last_success_age: facts.last_successful_sync.map(|value| now - value),
        progress: facts.progress.clone(),
    }
}

pub fn sync_health_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sync_health.json")
}

fn optional_float(data: &Map<String, Value>, key: &str) -> Option<f64> {
    data.get(key).and_then(Value::as_f64)
}

fn optional_int(data: &Map<String, Value>, key: &str) -> Option<i64> {
    data.get(key).and_then(Value::as_i64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HealthLoadError {
    UnsupportedSchema,
    MalformedLinkEpoch,
    MalformedLink,
}

pub(crate) fn load_link_facts(
    data: &Map<String, Value>,
    liveness: PrivateStateLockLiveness,
) -> Result<Option<LinkFactState>, HealthLoadError> {
    if liveness != PrivateStateLockLiveness::LiveOwner {
        return Ok(None);
    }
    if data.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return Err(HealthLoadError::UnsupportedSchema);
    }
    ProcessEpoch::parse(
        data.get("link_epoch")
            .ok_or(HealthLoadError::MalformedLinkEpoch)?,
    )
    .ok_or(HealthLoadError::MalformedLinkEpoch)?;
    let link = data
        .get("link")
        .and_then(Value::as_object)
        .ok_or(HealthLoadError::MalformedLink)?;
    let boolean = |key| {
        link.get(key)
            .and_then(Value::as_bool)
            .ok_or(HealthLoadError::MalformedLink)
    };
    Ok(Some(LinkFactState {
        pairing_required: boolean("pairing_required")?,
        private_state_invalid: boolean("private_state_invalid")?,
        config_sanitation_failed: boolean("config_sanitation_failed")?,
        listener_ready: boolean("listener_ready")?,
        carrier_proven: boolean("carrier_proven")?,
        observer_registered: boolean("observer_registered")?,
        transport_unavailable: boolean("transport_unavailable")?,
        terminal_revocation: boolean("terminal_revocation")?,
        token_persistence_failure: boolean("token_persistence_failure")?,
        journal_version_observed: boolean("journal_version_observed")?,
        dial_generation: 0,
    }))
}

pub fn load_facts(state_dir: &Path) -> SyncFacts {
    load_facts_with_liveness(state_dir, PrivateStateLockLiveness::NoLiveOwner)
}

pub(crate) fn load_facts_with_liveness(
    state_dir: &Path,
    liveness: PrivateStateLockLiveness,
) -> SyncFacts {
    let Ok(text) = fs::read_to_string(sync_health_path(state_dir)) else {
        return SyncFacts::default();
    };
    let Ok(Value::Object(data)) = serde_json::from_str(&text) else {
        return SyncFacts::default();
    };
    SyncFacts {
        last_successful_sync: optional_float(&data, "last_successful_sync"),
        last_successful_contact: optional_float(&data, "last_successful_contact"),
        last_error_class: data.get("last_error_class").and_then(ErrorType::parse),
        last_error_code: optional_int(&data, "last_error_code"),
        pending_confirmed: optional_int(&data, "pending_confirmed"),
        in_progress: data
            .get("in_progress")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        progress: data
            .get("progress")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        link: load_link_facts(&data, liveness).unwrap_or(None),
        link_epoch: ProcessEpoch::parse(data.get("link_epoch").unwrap_or(&Value::Null)),
    }
}

pub fn save_facts(state_dir: &Path, facts: &SyncFacts) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = sync_health_path(state_dir);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let link = facts
        .link
        .as_ref()
        .zip(facts.link_epoch.as_ref())
        .map(|(link, _)| {
            json!({
                "pairing_required": link.pairing_required,
                "private_state_invalid": link.private_state_invalid,
                "config_sanitation_failed": link.config_sanitation_failed,
                "listener_ready": link.listener_ready,
                "carrier_proven": link.carrier_proven,
                "observer_registered": link.observer_registered,
                "transport_unavailable": link.transport_unavailable,
                "terminal_revocation": link.terminal_revocation,
                "token_persistence_failure": link.token_persistence_failure,
                "journal_version_observed": link.journal_version_observed,
            })
        });
    let mut text = serde_json::to_string(&json!({
        "schema_version": SCHEMA_VERSION,
        "last_successful_sync": facts.last_successful_sync,
        "last_successful_contact": facts.last_successful_contact,
        "last_error_class": facts.last_error_class.map(ErrorType::as_str),
        "last_error_code": facts.last_error_code,
        "pending_confirmed": facts.pending_confirmed,
        "in_progress": facts.in_progress,
        "progress": facts.progress,
        "link_epoch": facts.link_epoch.as_ref().map(ProcessEpoch::as_str),
        "link": link,
    }))
    .map_err(io::Error::other)?;
    text.push('\n');
    // Match `private_file::atomic_write_bytes`: a failed write never leaves its scratch
    // file behind. The temporary is pid-named and nothing ever collects it, so an early
    // return here litters the owner's state directory permanently.
    fs::write(&temporary, text)?;
    let write = || -> io::Result<()> {
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, &path)
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

pub const PAIRED_JOURNAL_FILENAME: &str = "paired_journal.json";

pub fn paired_journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(PAIRED_JOURNAL_FILENAME)
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairedJournalVersion {
    pub identity_key: String,
    pub version: String,
    pub observed_at: f64,
}

pub fn load_paired_journal_version(state_dir: &Path) -> Option<PairedJournalVersion> {
    let text = fs::read_to_string(paired_journal_path(state_dir)).ok()?;
    let Value::Object(data) = serde_json::from_str(&text).ok()? else {
        return None;
    };
    let identity_key = data.get("identity_key")?.as_str()?.to_owned();
    let version = data.get("version")?.as_str()?.to_owned();
    let observed_at = data.get("observed_at")?.as_f64()?;
    Some(PairedJournalVersion {
        identity_key,
        version,
        observed_at,
    })
}

pub fn save_paired_journal_version(
    state_dir: &Path,
    identity_key: &str,
    version: &str,
) -> io::Result<()> {
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    fs::create_dir_all(state_dir)?;
    let path = paired_journal_path(state_dir);
    let mut text = serde_json::to_string(&json!({
        "identity_key": identity_key,
        "version": version,
        "observed_at": observed_at,
    }))
    .map_err(io::Error::other)?;
    text.push('\n');
    crate::private_file::atomic_write_bytes(&path, text.as_bytes())
        .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SYNC_STALE_THRESHOLD;
    use crate::private_link::LinkFactState;

    // AC: a failed save leaves no pid-named scratch file behind. The temporary is never
    // collected by anything, so an early return would litter the state directory forever.
    #[test]
    fn failed_save_leaves_no_temporary() {
        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("state");
        // A directory at the destination makes the rename fail after the temporary exists.
        fs::create_dir_all(sync_health_path(&state)).unwrap();
        assert!(save_facts(&state, &SyncFacts::default()).is_err());
        let leftovers: Vec<_> = fs::read_dir(&state)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    // tests/test_sync_health.py::test_empty_facts_derive_unknown
    #[test]
    fn empty_facts_derive_not_reported() {
        let health = derive_health(
            &SyncFacts::default(),
            1000.0,
            DEFAULT_SYNC_STALE_THRESHOLD as f64,
        );
        assert_eq!(health.state, HealthState::NotReported);
        assert_eq!(health.sni_status, "Active");
        assert_eq!(health.pending_display, "pending unconfirmed");
    }

    #[test]
    fn missing_link_facts_use_exact_not_reported_surface() {
        let health = derive_health(
            &SyncFacts::default(),
            1000.0,
            DEFAULT_SYNC_STALE_THRESHOLD as f64,
        );
        assert_eq!(health.dbus, "not-reported");
        assert_eq!(health.header_recording, "on, no status");
        assert_eq!(health.header_idle, "idle, no status");
        assert_eq!(health.sync_line, "sync: no status");
        assert_eq!(
            health.tooltip,
            "sync: no status from the solstone app right now"
        );
        assert_eq!(health.accessible_recording, "on, no status");
        assert_eq!(health.accessible_idle, "idle, no status");
        assert_eq!(health.icon, "offline");
        assert_eq!(health.sni_status, "Active");
        assert_eq!(
            health.cli,
            "Sync: no status; the solstone app is not running, or has no status yet"
        );
        assert_eq!(health.doctor_severity, "warn");
        assert_eq!(
            health.doctor_detail,
            "the solstone app is not running, or has no status yet"
        );
    }

    #[test]
    fn listener_ready_alone_never_reads_connected() {
        let facts = SyncFacts {
            pending_confirmed: Some(0),
            link: Some(LinkFactState {
                listener_ready: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        };
        assert_eq!(
            derive_health(&facts, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64).state,
            HealthState::ListenerReady
        );
    }

    #[test]
    fn connected_requires_carrier_and_registered_observer_facts() {
        let facts = SyncFacts {
            pending_confirmed: Some(0),
            link: Some(LinkFactState {
                listener_ready: true,
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        };
        assert_eq!(
            derive_health(&facts, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64).state,
            HealthState::Connected
        );
    }

    // tests/test_sync_health.py::test_error_precedence_states
    #[test]
    fn error_precedence_states() {
        for (error, expected) in [
            (ErrorType::Auth, HealthState::RePairRequired),
            (ErrorType::Incompatible, HealthState::UpdateRequired),
            (ErrorType::Transient, HealthState::Offline),
        ] {
            let facts = SyncFacts {
                last_error_class: Some(error),
                // Auth without a response code is conservatively treated as revoked.
                last_error_code: None,
                ..SyncFacts::default()
            };
            assert_eq!(
                derive_health(&facts, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64).state,
                expected
            );
        }
    }

    // AC 7: auth_401_is_neither_revoked_nor_connected_nor_offline pins Decision 3.
    // The explicit 401 arm sits above Connected so persisted empty-queue facts cannot turn green.
    #[test]
    fn auth_401_is_neither_revoked_nor_connected_nor_offline() {
        for pending_confirmed in [None, Some(0)] {
            let template = SyncFacts {
                last_successful_contact: Some(990.0),
                pending_confirmed,
                in_progress: false,
                ..SyncFacts::default()
            };
            let state_for = |error, code| {
                derive_health(
                    &SyncFacts {
                        last_error_class: Some(error),
                        last_error_code: code,
                        ..template.clone()
                    },
                    1000.0,
                    DEFAULT_SYNC_STALE_THRESHOLD as f64,
                )
                .state
            };
            let unauthorized = state_for(ErrorType::Auth, Some(401));
            let server_error = state_for(ErrorType::Transient, Some(500));
            let timeout = state_for(ErrorType::Transient, None);
            let forbidden = state_for(ErrorType::Auth, Some(403));

            assert_ne!(unauthorized, HealthState::RePairRequired);
            assert_ne!(unauthorized, HealthState::Connected);
            assert_ne!(unauthorized, server_error);
            assert_ne!(unauthorized, timeout);
            assert_eq!(forbidden, HealthState::RePairRequired);
        }
    }

    // tests/test_sync_health.py::test_stale_uses_last_successful_contact
    #[test]
    fn stale_uses_last_successful_contact() {
        let facts = SyncFacts {
            last_successful_sync: Some(900.0),
            last_successful_contact: Some(100.0),
            in_progress: true,
            ..SyncFacts::default()
        };
        let health = derive_health(&facts, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64);
        assert_eq!(health.state, HealthState::Offline);
        assert_eq!(health.sni_status, "Active");
        assert!(health.tooltip.contains("held on this device"));
    }

    // Connected requires both zero pending custody and positive linked-transport evidence.
    #[test]
    fn pending_confirmed_zero_and_link_evidence_gate_connected() {
        let connected = SyncFacts {
            pending_confirmed: Some(0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        };
        assert_eq!(
            derive_health(&connected, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64).state,
            HealthState::Connected
        );
        assert_eq!(
            derive_health(
                &SyncFacts::default(),
                1000.0,
                DEFAULT_SYNC_STALE_THRESHOLD as f64,
            )
            .state,
            HealthState::NotReported
        );
    }

    // tests/test_sync_health.py::test_fresh_contact_prevents_stale_when_sync_timestamp_is_old
    #[test]
    fn fresh_contact_prevents_stale_when_sync_timestamp_is_old() {
        let facts = SyncFacts {
            last_successful_sync: Some(100.0),
            last_successful_contact: Some(990.0),
            in_progress: true,
            link: Some(LinkFactState {
                observer_registered: true,
                ..Default::default()
            }),
            ..SyncFacts::default()
        };
        assert_eq!(
            derive_health(&facts, 1000.0, DEFAULT_SYNC_STALE_THRESHOLD as f64).state,
            HealthState::Syncing
        );
    }

    // tests/test_sync_health.py::test_save_and_load_facts_round_trip
    #[test]
    fn save_and_load_facts_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let facts = SyncFacts {
            last_successful_sync: Some(100.5),
            last_successful_contact: Some(200.5),
            last_error_class: Some(ErrorType::Incompatible),
            last_error_code: Some(404),
            pending_confirmed: None,
            in_progress: true,
            progress: "uploading 120000_300".to_owned(),
            link: None,
            link_epoch: None,
        };
        save_facts(temp.path(), &facts).unwrap();
        assert_eq!(load_facts(temp.path()), facts);
    }

    // tests/test_sync_health.py::test_load_facts_missing_or_invalid_returns_empty
    #[test]
    fn load_facts_missing_or_invalid_returns_empty() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load_facts(temp.path()), SyncFacts::default());
        fs::write(sync_health_path(temp.path()), "{not-json").unwrap();
        assert_eq!(load_facts(temp.path()), SyncFacts::default());
    }

    #[test]
    fn live_link_facts_require_current_schema() {
        let mut data = serde_json::Map::new();
        data.insert("schema_version".to_owned(), json!(SCHEMA_VERSION - 1));
        data.insert(
            "link_epoch".to_owned(),
            json!(ProcessEpoch::for_test(1).as_str()),
        );
        data.insert(
            "link".to_owned(),
            json!({
                "pairing_required": false,
                "private_state_invalid": false,
                "config_sanitation_failed": false,
                "listener_ready": false,
                "carrier_proven": false,
                "observer_registered": false,
                "transport_unavailable": false,
                "terminal_revocation": false,
                "token_persistence_failure": false,
            }),
        );
        assert_eq!(
            load_link_facts(&data, PrivateStateLockLiveness::LiveOwner),
            Err(HealthLoadError::UnsupportedSchema)
        );
    }

    // tests/test_sync_health.py::test_every_health_state_has_complete_surface
    #[test]
    fn every_health_state_has_complete_surface() {
        assert_eq!(SURFACE_BY_STATE.len(), 12);
        for surface in SURFACE_BY_STATE.values() {
            assert!(!surface.header_recording.is_empty());
            assert!(!surface.header_idle.is_empty());
            assert!(!surface.sync_line.is_empty());
            assert!(!surface.tooltip.is_empty());
            assert!(!surface.accessible_recording.is_empty());
            assert!(!surface.accessible_idle.is_empty());
            assert!(!surface.icon.is_empty());
            assert!(!surface.sni.is_empty());
            assert!(!surface.cli.is_empty());
            assert!(!surface.doctor_severity.is_empty());
            assert!(!surface.doctor_detail.is_empty());
            assert!(!surface.dbus.is_empty());
        }
    }

    #[test]
    fn precedence_table_covers_single_and_conflicting_conditions() {
        let state = |facts: SyncFacts| derive_health(&facts, 1_000.0, 600.0).state;
        let link = |link: LinkFactState| SyncFacts {
            link: Some(link),
            ..Default::default()
        };
        let single_cases = [
            (
                link(LinkFactState {
                    private_state_invalid: true,
                    ..Default::default()
                }),
                HealthState::UnsafeLinkState,
            ),
            (
                link(LinkFactState {
                    config_sanitation_failed: true,
                    ..Default::default()
                }),
                HealthState::UnsafeLinkState,
            ),
            (
                link(LinkFactState {
                    terminal_revocation: true,
                    ..Default::default()
                }),
                HealthState::RePairRequired,
            ),
            (
                link(LinkFactState {
                    token_persistence_failure: true,
                    ..Default::default()
                }),
                HealthState::TokenPersistenceFailed,
            ),
            (
                link(LinkFactState {
                    pairing_required: true,
                    ..Default::default()
                }),
                HealthState::PairingRequired,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    ..Default::default()
                },
                HealthState::UpdateRequired,
            ),
            (
                link(LinkFactState {
                    transport_unavailable: true,
                    ..Default::default()
                }),
                HealthState::TransportUnavailable,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Transient),
                    ..Default::default()
                },
                HealthState::Offline,
            ),
            (
                link(LinkFactState {
                    listener_ready: true,
                    ..Default::default()
                }),
                HealthState::ListenerReady,
            ),
            (
                link(LinkFactState {
                    carrier_proven: true,
                    ..Default::default()
                }),
                HealthState::Connecting,
            ),
            (
                link(LinkFactState {
                    observer_registered: true,
                    ..Default::default()
                }),
                HealthState::Syncing,
            ),
        ];
        for (facts, expected) in single_cases {
            assert_eq!(state(facts), expected);
        }

        let all = LinkFactState {
            pairing_required: true,
            private_state_invalid: true,
            config_sanitation_failed: true,
            listener_ready: true,
            carrier_proven: true,
            observer_registered: true,
            transport_unavailable: true,
            terminal_revocation: true,
            token_persistence_failure: true,
            journal_version_observed: true,
            dial_generation: 0,
        };
        let conflicting_cases = [
            (all.clone(), HealthState::UnsafeLinkState),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    ..all.clone()
                },
                HealthState::RePairRequired,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    terminal_revocation: false,
                    ..all.clone()
                },
                HealthState::TokenPersistenceFailed,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    terminal_revocation: false,
                    token_persistence_failure: false,
                    ..all.clone()
                },
                HealthState::PairingRequired,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    terminal_revocation: false,
                    token_persistence_failure: false,
                    pairing_required: false,
                    ..all
                },
                HealthState::UpdateRequired,
            ),
        ];
        for (link, expected) in conflicting_cases {
            let facts = SyncFacts {
                last_error_class: Some(ErrorType::Incompatible),
                pending_confirmed: Some(0),
                link: Some(link),
                ..Default::default()
            };
            assert_eq!(state(facts), expected);
        }
    }

    #[test]
    fn replayed_connected_facts_never_override_six_current_failures() {
        let temp = tempfile::tempdir().unwrap();
        save_facts(
            temp.path(),
            &SyncFacts {
                pending_confirmed: Some(0),
                link: Some(LinkFactState {
                    carrier_proven: true,
                    observer_registered: true,
                    ..Default::default()
                }),
                link_epoch: Some(ProcessEpoch::for_test(7)),
                ..Default::default()
            },
        )
        .unwrap();
        let replayed = load_facts_with_liveness(temp.path(), PrivateStateLockLiveness::NoLiveOwner);
        assert!(replayed.link.is_none());
        let cases = [
            (
                LinkFactState {
                    pairing_required: true,
                    ..Default::default()
                },
                HealthState::PairingRequired,
            ),
            (
                LinkFactState {
                    private_state_invalid: true,
                    ..Default::default()
                },
                HealthState::UnsafeLinkState,
            ),
            (
                LinkFactState {
                    config_sanitation_failed: true,
                    ..Default::default()
                },
                HealthState::UnsafeLinkState,
            ),
            (
                LinkFactState {
                    terminal_revocation: true,
                    ..Default::default()
                },
                HealthState::RePairRequired,
            ),
            (
                LinkFactState {
                    token_persistence_failure: true,
                    ..Default::default()
                },
                HealthState::TokenPersistenceFailed,
            ),
            (
                LinkFactState {
                    transport_unavailable: true,
                    ..Default::default()
                },
                HealthState::TransportUnavailable,
            ),
        ];
        for (current, expected) in cases {
            let health = derive_health(
                &SyncFacts {
                    link: Some(current),
                    ..replayed.clone()
                },
                1_000.0,
                600.0,
            );
            assert_eq!(health.state, expected);
            assert_ne!(health.state, HealthState::Connected);
            assert!(!health.sync_line.contains("checking"));
        }
    }

    // AC: invalid fields fall back independently and signed pending values round-trip.
    #[test]
    fn field_parse_failures_are_independent() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            sync_health_path(temp.path()),
            r#"{"last_successful_sync":"bad","last_successful_contact":2.5,"last_error_code":3.7,"pending_confirmed":-5,"in_progress":"yes","progress":7}"#,
        )
        .unwrap();
        let facts = load_facts(temp.path());
        assert_eq!(facts.last_successful_sync, None);
        assert_eq!(facts.last_successful_contact, Some(2.5));
        assert_eq!(facts.last_error_code, None);
        assert_eq!(facts.pending_confirmed, Some(-5));
        assert!(!facts.in_progress);
        assert!(facts.progress.is_empty());
    }

    // AC: formatters retain Python truncation, units, and defaulted-vs-raw progress behavior.
    #[test]
    fn formatter_parity() {
        assert_eq!(format_age(None), "unknown");
        assert_eq!(format_age(Some(-1.2)), "0s");
        assert_eq!(format_age(Some(59.9)), "59s");
        assert_eq!(format_age(Some(60.0)), "1m");
        assert_eq!(format_age(Some(3600.0)), "1h");
        assert_eq!(format_age(Some(86400.0)), "1d");
        assert_eq!(format_ts(None), "unknown");
        let facts = SyncFacts {
            in_progress: true,
            progress: "  ".to_owned(),
            link: Some(LinkFactState {
                observer_registered: true,
                ..Default::default()
            }),
            ..SyncFacts::default()
        };
        let health = derive_health(&facts, 1.0, DEFAULT_SYNC_STALE_THRESHOLD as f64);
        assert_eq!(health.sync_line, "sync: syncing...");
        assert_eq!(health.progress, "  ");
        let values = HashMap::from([
            ("progress", "{sync_ts}".to_owned()),
            ("sync_ts", "changed".to_owned()),
        ]);
        assert_eq!(fill("sync: {progress}", &values), "sync: {sync_ts}");
    }

    #[test]
    fn paired_journal_version_persistence() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load_paired_journal_version(temp.path()), None);

        save_paired_journal_version(temp.path(), "inst-1:fp-1", "1.4.0").unwrap();
        let loaded = load_paired_journal_version(temp.path()).unwrap();
        assert_eq!(loaded.identity_key, "inst-1:fp-1");
        assert_eq!(loaded.version, "1.4.0");
        assert!(loaded.observed_at > 0.0);

        // Overwrite
        save_paired_journal_version(temp.path(), "inst-2:fp-2", "2.0.0").unwrap();
        let loaded2 = load_paired_journal_version(temp.path()).unwrap();
        assert_eq!(loaded2.identity_key, "inst-2:fp-2");
        assert_eq!(loaded2.version, "2.0.0");

        // Corrupt file falls back to None
        fs::write(paired_journal_path(temp.path()), "not-json").unwrap();
        assert_eq!(load_paired_journal_version(temp.path()), None);
    }
}
