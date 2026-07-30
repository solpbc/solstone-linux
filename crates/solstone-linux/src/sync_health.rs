// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::private_link::LinkFactState;
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

// Decision 4: no new ErrorType means no new persisted string, version bump, or downgrade loss.
pub const SCHEMA_VERSION: u64 = 1;

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
    Connected,
    Syncing,
    Offline,
    UpdateNeeded,
    Revoked,
    Stale,
    Unknown,
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
            HealthState::Connected,
            HealthSurface {
                header_recording: "on — connected",
                header_idle: "idle — connected",
                sync_line: "sync: up to date",
                tooltip: "sync: up to date",
                accessible_recording: "sol — on, sync up to date",
                accessible_idle: "sol — idle, sync up to date",
                icon: "recording",
                sni: "Active",
                cli: "Sync: connected — up to date (0 pending)",
                doctor_severity: "ok",
                doctor_detail: "sync health: up to date; 0 pending confirmed at {sync_ts}",
                dbus: "connected",
            },
        ),
        (
            HealthState::Syncing,
            HealthSurface {
                header_recording: "on — syncing",
                header_idle: "idle — syncing",
                sync_line: "sync: {progress}",
                tooltip: "sync: {progress}",
                accessible_recording: "sol — on, syncing",
                accessible_idle: "sol — idle, syncing",
                icon: "syncing",
                sni: "Active",
                cli: "Sync: syncing — pending unconfirmed until this pass finishes",
                doctor_severity: "ok",
                doctor_detail: "sync health: sync pass active; pending unconfirmed until check completes",
                dbus: "syncing",
            },
        ),
        (
            HealthState::Offline,
            HealthSurface {
                header_recording: "on — offline (saving locally)",
                header_idle: "idle — offline (saving locally)",
                sync_line: "sync: offline; will retry",
                tooltip: "sync: offline; saving locally",
                accessible_recording: "sol — on, offline, saving locally",
                accessible_idle: "sol — idle, offline, saving locally",
                icon: "syncing",
                sni: "Active",
                cli: "Sync: offline — saving locally; pending unconfirmed (will retry)",
                doctor_severity: "warn",
                doctor_detail: "sync health: offline; pending unconfirmed; will retry",
                dbus: "offline",
            },
        ),
        (
            HealthState::UpdateNeeded,
            HealthSurface {
                header_recording: "on — update needed",
                header_idle: "idle — update needed",
                sync_line: "sync: update solstone-linux",
                tooltip: "sync: update needed; update solstone-linux",
                accessible_recording: "sol — on, update needed",
                accessible_idle: "sol — idle, update needed",
                icon: "error",
                sni: "NeedsAttention",
                cli: "Sync: update needed — update solstone-linux; pending unconfirmed",
                doctor_severity: "fail",
                doctor_detail: "sync health: update needed; the journal returned 404",
                dbus: "update-needed",
            },
        ),
        (
            HealthState::Revoked,
            HealthSurface {
                header_recording: "on — re-auth needed",
                header_idle: "idle — re-auth needed",
                sync_line: "sync: re-auth required",
                tooltip: "sync: access revoked; re-auth required",
                accessible_recording: "sol — on, re-auth required",
                accessible_idle: "sol — idle, re-auth required",
                icon: "error",
                sni: "NeedsAttention",
                cli: "Sync: revoked — re-auth required; pending unconfirmed",
                doctor_severity: "fail",
                doctor_detail: "sync health: access revoked; re-auth required",
                dbus: "revoked",
            },
        ),
        (
            HealthState::Stale,
            HealthSurface {
                header_recording: "on — sync stale",
                header_idle: "idle — sync stale",
                sync_line: "sync: stale; no journal response in {contact_age}",
                tooltip: "sync: stale; last contact {contact_ts}",
                accessible_recording: "sol — on, sync stale",
                accessible_idle: "sol — idle, sync stale",
                icon: "error",
                sni: "NeedsAttention",
                cli: "Sync: stale — no journal response in {contact_age}; check service and journal",
                doctor_severity: "fail",
                doctor_detail: "sync health: stale; last contact {contact_ts}, threshold {threshold}",
                dbus: "stale",
            },
        ),
        (
            HealthState::Unknown,
            HealthSurface {
                header_recording: "on — sync unconfirmed",
                header_idle: "idle — sync unconfirmed",
                sync_line: "sync: checking...",
                tooltip: "sync: not confirmed yet",
                accessible_recording: "sol — on, sync unconfirmed",
                accessible_idle: "sol — idle, sync unconfirmed",
                icon: "syncing",
                sni: "Active",
                cli: "Sync: unconfirmed — waiting for first successful journal check; pending unconfirmed",
                doctor_severity: "warn",
                doctor_detail: "sync health: unconfirmed; no successful journal check yet",
                dbus: "unknown",
            },
        ),
    ])
});

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
    let state =
        if facts.last_error_class == Some(ErrorType::Auth) && facts.last_error_code != Some(401) {
            HealthState::Revoked
        } else if facts.last_error_class == Some(ErrorType::Incompatible) {
            HealthState::UpdateNeeded
        } else if facts
            .last_successful_contact
            .is_some_and(|contact| now - contact > stale_threshold)
        {
            HealthState::Stale
        } else if facts.in_progress {
            HealthState::Syncing
        } else if facts.last_error_class == Some(ErrorType::Auth) {
            // Decision 3: a 401 is explicit Unknown above Connected so persisted empty-queue
            // facts cannot repaint a refused identity green. The live worker also clears
            // pending_confirmed in record_failure. This depends on POST propagation retaining
            // Some(401); losing that code would conservatively repaint the failure Revoked.
            HealthState::Unknown
        } else if facts.pending_confirmed == Some(0)
            && facts.link.as_ref().is_none_or(|link| {
                link.carrier_proven
                    && link.observer_registered
                    && !link.transport_unavailable
                    && !link.terminal_revocation
                    && !link.token_persistence_failure
            })
        {
            HealthState::Connected
        } else if facts.last_error_class == Some(ErrorType::Transient) {
            HealthState::Offline
        } else {
            HealthState::Unknown
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
        ("contact_ts", format_ts(facts.last_successful_contact)),
        (
            "contact_age",
            format_age(facts.last_successful_contact.map(|value| now - value)),
        ),
        ("threshold", format_age(Some(stale_threshold))),
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

pub fn load_facts(state_dir: &Path) -> SyncFacts {
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
        link: None,
    }
}

pub fn save_facts(state_dir: &Path, facts: &SyncFacts) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = sync_health_path(state_dir);
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut text = serde_json::to_string(&json!({
        "schema_version": SCHEMA_VERSION,
        "last_successful_sync": facts.last_successful_sync,
        "last_successful_contact": facts.last_successful_contact,
        "last_error_class": facts.last_error_class.map(ErrorType::as_str),
        "last_error_code": facts.last_error_code,
        "pending_confirmed": facts.pending_confirmed,
        "in_progress": facts.in_progress,
        "progress": facts.progress,
    }))
    .map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&temporary, text)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SYNC_STALE_THRESHOLD;
    use crate::private_link::LinkFactState;

    // tests/test_sync_health.py::test_empty_facts_derive_unknown
    #[test]
    fn empty_facts_derive_unknown() {
        let health = derive_health(
            &SyncFacts::default(),
            1000.0,
            DEFAULT_SYNC_STALE_THRESHOLD as f64,
        );
        assert_eq!(health.state, HealthState::Unknown);
        assert_eq!(health.sni_status, "Active");
        assert_eq!(health.pending_display, "pending unconfirmed");
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
            HealthState::Unknown
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
            (ErrorType::Auth, HealthState::Revoked),
            (ErrorType::Incompatible, HealthState::UpdateNeeded),
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

            assert_ne!(unauthorized, HealthState::Revoked);
            assert_ne!(unauthorized, HealthState::Connected);
            assert_ne!(unauthorized, server_error);
            assert_ne!(unauthorized, timeout);
            assert_eq!(forbidden, HealthState::Revoked);
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
        assert_eq!(health.state, HealthState::Stale);
        assert_eq!(health.sni_status, "NeedsAttention");
        assert!(health.tooltip.contains("last contact"));
    }

    // tests/test_sync_health.py::test_pending_confirmed_zero_is_only_connected_gate
    #[test]
    fn pending_confirmed_zero_is_only_connected_gate() {
        let connected = SyncFacts {
            pending_confirmed: Some(0),
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
            HealthState::Unknown
        );
    }

    // tests/test_sync_health.py::test_fresh_contact_prevents_stale_when_sync_timestamp_is_old
    #[test]
    fn fresh_contact_prevents_stale_when_sync_timestamp_is_old() {
        let facts = SyncFacts {
            last_successful_sync: Some(100.0),
            last_successful_contact: Some(990.0),
            in_progress: true,
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

    // tests/test_sync_health.py::test_every_health_state_has_complete_surface
    #[test]
    fn every_health_state_has_complete_surface() {
        assert_eq!(SURFACE_BY_STATE.len(), 7);
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
}
