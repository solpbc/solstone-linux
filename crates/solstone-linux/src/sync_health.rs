// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::copy::{UNKNOWN_JOURNAL_SEEN_THROUGH_RELAY, unknown_journal_seen_at};
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

/// How long the app goes without hearing from the journal before it asks.
pub const STATUS_CHECK_INTERVAL: f64 = 15.0;
/// How long one answer from the journal keeps "connected" showing: a check interval, that
/// check's own timeout, and a second of slack, so one slow or failed check never reads as a
/// drop.
pub const CONNECTED_LEASE: f64 =
    STATUS_CHECK_INTERVAL + crate::private_link::SYSTEM_STATUS_TIMEOUT.as_secs_f64() + 1.0;
/// How long the app tries to reach the journal before it says offline instead of connecting.
pub const OFFLINE_AFTER: f64 = 30.0;
/// A drop from connected shows for at least this long, so a quick reconnect is not a flash.
pub const DOWN_DWELL: f64 = 1.0;
/// After a wake or a returning network, how long the last shown state holds while a fresh
/// check answers.
pub const FRESH_CHECK_GRACE: f64 = 0.5;
/// Once syncing shows, it stays this long after the queue drains.
pub const SYNCING_LINGER: f64 = 1.5;
/// The wall clock running ahead of the monotonic clock by more than this means the
/// machine slept.
const WAKE_GAP: f64 = 5.0;

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
    JournalMarkHeld,
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
    pub imports: Option<crate::import::ImportSummary>,
    pub last_successful_sync: Option<f64>,
    pub last_successful_contact: Option<f64>,
    pub last_error_class: Option<ErrorType>,
    pub last_error_code: Option<i64>,
    pub pending_confirmed: Option<i64>,
    /// True only while something is being sent. A status check is not syncing.
    pub in_progress: bool,
    pub progress: String,
    /// When the current attempt to reach the journal began: launch, wake, or the network
    /// coming back. Offline counts from the later of this and the journal's last answer.
    pub episode_started: Option<f64>,
    /// The OS reports no network (NetworkManager). Only ever read together with a down link
    /// or an unanswered check.
    pub no_network: bool,
    /// The last status check went unanswered; cleared by any answer.
    pub last_check_failed: bool,
    #[doc(hidden)]
    pub(crate) link: Option<LinkFactState>,
    #[doc(hidden)]
    pub(crate) link_epoch: Option<ProcessEpoch>,
}

impl SyncFacts {
    pub(crate) fn with_import_counts(&self) -> Self {
        let mut combined = self.clone();
        if let Some(imports) = &self.imports {
            let count = i64::try_from(imports.waiting).unwrap_or(i64::MAX);
            combined.pending_confirmed = self
                .pending_confirmed
                .and_then(|n| (n >= 0).then(|| n.saturating_add(count)));
            combined.in_progress |= imports.sending;
        }
        combined
    }
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
    pub import_held: bool,
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
    /// Items waiting to go into the journal. Absent when the count is missing or zero.
    pub waiting_count: Option<u64>,
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
                cli: "sync: pairing unsafe; repair this device's pairing and restart the solstone app",
                doctor_severity: "fail",
                doctor_detail: "sync health: pairing unsafe; repair this device's pairing and restart the solstone app",
                dbus: "unsafe-link-state",
            },
        ),
        (
            HealthState::JournalMarkHeld,
            HealthSurface {
                header_recording: crate::journal_mark::HELD_FIRST_SENTENCE,
                header_idle: crate::journal_mark::HELD_FIRST_SENTENCE,
                sync_line: crate::journal_mark::HELD_FIRST_SENTENCE,
                tooltip: crate::journal_mark::HELD_FIRST_SENTENCE,
                accessible_recording: crate::journal_mark::HELD_FIRST_SENTENCE,
                accessible_idle: crate::journal_mark::HELD_FIRST_SENTENCE,
                icon: "attention",
                sni: "NeedsAttention",
                cli: "waiting for you to confirm your journal's mark. nothing waiting goes into your journal until you do.\nwhen you're ready, run: solstone-linux confirm",
                doctor_severity: "fail",
                doctor_detail: "waiting for you to confirm your journal's mark. nothing waiting goes into your journal until you do.\nwhen you're ready, run: solstone-linux confirm",
                dbus: "journal-mark-held",
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
                cli: "sync: pair again; pair this device with your journal again",
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
                cli: "sync: pairing not saved; fix this device's pairing permissions, then restart the solstone app",
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
                cli: "sync: pairing required; pair this device with your journal",
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
                cli: "sync: update required; update the solstone app",
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
                cli: "sync: connection unavailable; held on this device; restart the solstone app; if this continues, pair this device again",
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
                cli: "sync: offline; held on this device; will retry",
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
                cli: "sync: wait while this device confirms with your journal",
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
                cli: "sync: no status; the solstone app is not running, or has no status yet",
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
                cli: "sync: connecting; wait while this device connects to your journal",
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
                cli: "sync: syncing; not confirmed by your journal yet",
                doctor_severity: "ok",
                doctor_detail: "sync health: syncing; not confirmed by your journal yet",
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
                cli: "sync: connected; up to date",
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
    let combined = facts.with_import_counts();
    let mut health = health_for_state(
        derive_state(&combined, now, stale_threshold),
        &combined,
        now,
    );
    apply_import_health(&mut health, facts);
    health
}

pub(crate) fn apply_import_health(health: &mut SyncHealth, facts: &SyncFacts) {
    let Some(imports) = &facts.imports else {
        return;
    };
    health.import_held = imports.held;
    if imports.held
        && facts.pending_confirmed == Some(0)
        && !facts.in_progress
        && matches!(health.state, HealthState::Connected | HealthState::Syncing)
    {
        let text = imports.recent.clone();
        health.header_recording = text.clone();
        health.header_idle = text.clone();
        health.sync_line = text.clone();
        health.tooltip = text.clone();
        health.accessible_recording = text.clone();
        health.accessible_idle = text.clone();
        health.cli = text.clone();
        health.doctor_detail = text;
        if !imports.sending {
            health.state = HealthState::Connected;
            health.dbus = "connected".to_owned();
            health.icon = "healthy".to_owned();
        }
        if imports.waiting == 0 && !imports.sending {
            health.icon = "attention".to_owned();
            health.sni_status = "NeedsAttention".to_owned();
        }
        health.pending_display = "pending unconfirmed".to_owned();
    }
}

/// The one rule for what the connection status says, from facts alone. Connected needs the
/// link up and an answer from the journal within the lease. Inside the lease a failed
/// request changes nothing; a failed dial clears the carrier proof and shows at once.
pub fn derive_state(facts: &SyncFacts, now: f64, stale_threshold: f64) -> HealthState {
    let link_missing = facts.link.is_none();
    let link = facts.link.clone().unwrap_or_default();
    let contact = facts.last_successful_contact;
    // A contact time ahead of the clock (the clock was set back) proves nothing.
    let heard = contact.is_some_and(|contact| (0.0..=CONNECTED_LEASE).contains(&(now - contact)));
    let unreached_since = match (contact, facts.episode_started) {
        (Some(contact), Some(episode)) => Some(contact.max(episode)),
        (contact, episode) => contact.or(episode),
    };
    // A configured stale threshold can only bring offline sooner.
    let offline_after = OFFLINE_AFTER.min(stale_threshold);
    let offline = unreached_since.is_some_and(|since| now - since >= offline_after);
    let terminal_auth =
        facts.last_error_class == Some(ErrorType::Auth) && facts.last_error_code != Some(401);
    // A refused credential is an answer, but not one that connects.
    let link_up = link.carrier_proven
        && link.observer_registered
        && facts.last_error_class != Some(ErrorType::Auth);
    let waiting = facts.in_progress || facts.pending_confirmed != Some(0);
    if link.private_state_invalid || link.config_sanitation_failed {
        HealthState::UnsafeLinkState
    } else if link.journal_mark_held {
        HealthState::JournalMarkHeld
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
    } else if !link_missing && facts.no_network && (!link.carrier_proven || facts.last_check_failed)
    {
        // No network and the journal not answering: offline at once. No network alone,
        // while the journal is still heard, changes nothing.
        HealthState::Offline
    } else if link_up && heard {
        if waiting {
            HealthState::Syncing
        } else {
            HealthState::Connected
        }
    } else if offline {
        HealthState::Offline
    } else if link.listener_ready && !link.observer_registered {
        HealthState::ListenerReady
    } else if link_missing {
        HealthState::NotReported
    } else {
        HealthState::Connecting
    }
}

pub(crate) fn health_for_state(state: HealthState, facts: &SyncFacts, now: f64) -> SyncHealth {
    let link = facts.link.clone().unwrap_or_default();
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
    let mut health = SyncHealth {
        import_held: false,
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
        waiting_count: facts.pending_confirmed.and_then(|count| {
            if count > 0 {
                u64::try_from(count).ok()
            } else {
                None
            }
        }),
    };
    apply_unknown_journal_overlay(&mut health, &link);
    health
}

/// What the live surfaces show (tray, panel, window and the D-Bus signal), from the derived
/// state and how long things have been showing. A single reading can't know that.
#[derive(Debug, Default)]
pub(crate) struct Presenter {
    shown: Option<Shown>,
    syncing_seen: Option<f64>,
    hold_until: Option<f64>,
    last_clock: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug)]
struct Shown {
    state: HealthState,
    /// When the link last dropped from up, kept through connecting and offline.
    dropped_at: Option<f64>,
}

fn is_up(state: HealthState) -> bool {
    matches!(state, HealthState::Connected | HealthState::Syncing)
}

fn is_down(state: HealthState) -> bool {
    matches!(state, HealthState::Connecting | HealthState::Offline)
}

impl Presenter {
    /// Reports whether the machine slept since the last reading: the wall clock moved on
    /// while the monotonic clock, which stops during suspend, did not.
    pub(crate) fn woke(&mut self, wall: f64, mono: f64) -> bool {
        let woke = self.last_clock.is_some_and(|(last_wall, last_mono)| {
            (wall - last_wall) - (mono - last_mono) > WAKE_GAP
        });
        self.last_clock = Some((wall, mono));
        woke
    }

    /// A fresh check is on its way: keep what is showing for a moment while it answers.
    pub(crate) fn hold_for_fresh_check(&mut self, mono: f64) {
        self.hold_until = Some(mono + FRESH_CHECK_GRACE);
    }

    /// `mono` is the monotonic clock; every hold and dwell here is measured on it.
    pub(crate) fn present(&mut self, derived: HealthState, mono: f64) -> HealthState {
        let mut state = derived;
        if let Some(shown) = self.shown {
            // A fresh check is answering: keep showing up for the grace.
            let held = self.hold_until.is_some_and(|until| mono < until)
                && is_up(shown.state)
                && is_down(derived);
            // A drop shows for at least the dwell before up returns.
            let dwelling = is_up(derived)
                && shown
                    .dropped_at
                    .is_some_and(|dropped| mono - dropped < DOWN_DWELL);
            if held || dwelling {
                state = shown.state;
            }
            if state == HealthState::Connected
                && shown.state == HealthState::Syncing
                && self
                    .syncing_seen
                    .is_some_and(|seen| mono - seen < SYNCING_LINGER)
            {
                state = HealthState::Syncing;
            }
        }
        if derived == HealthState::Syncing {
            self.syncing_seen = Some(mono);
        }
        let dropped_at = match self.shown {
            _ if !is_down(state) => None,
            Some(shown) if is_up(shown.state) => Some(mono),
            Some(shown) => shown.dropped_at,
            None => None,
        };
        self.shown = Some(Shown { state, dropped_at });
        state
    }
}

const UNKNOWN_JOURNAL_RESET_LINE: &str =
    "if you recently reset your journal, pair this device again.";
const UNKNOWN_JOURNAL_RELAY_LINE: &str = "this is less certain than a direct connection.";

fn apply_unknown_journal_overlay(health: &mut SyncHealth, link: &LinkFactState) {
    if link.unknown_journals.is_empty() {
        return;
    }

    let mut tooltip_lines = Vec::new();
    let mut detail_blocks = Vec::new();

    let paired_spoken = link.paired_spoken_mark.as_deref();

    for (index, sighting) in link.unknown_journals.iter().enumerate() {
        let sighting_spoken = link
            .unknown_spoken_marks
            .get(index)
            .and_then(|m| m.as_deref());
        let what_answered = if let Some(spoken) = sighting_spoken {
            format!("{spoken}  ({})", crate::copy::UNKNOWN_JOURNAL_CLAIMED)
        } else {
            crate::copy::UNKNOWN_JOURNAL_NO_IDENTITY.to_owned()
        };

        let your_journal = if let Some(paired) = paired_spoken {
            format!("your journal:   {paired}")
        } else {
            crate::journal_mark::MARK_UNAVAILABLE_LINE.to_owned()
        };
        // Nothing to act on or weigh when nothing was claimed.
        let footer = |line: &str| {
            if sighting_spoken.is_some() {
                format!("\n  {line}")
            } else {
                String::new()
            }
        };

        let (header, footer) = if let Some(address) = &sighting.address {
            (
                unknown_journal_seen_at(address),
                footer(UNKNOWN_JOURNAL_RESET_LINE),
            )
        } else {
            (
                UNKNOWN_JOURNAL_SEEN_THROUGH_RELAY.to_owned(),
                footer(UNKNOWN_JOURNAL_RELAY_LINE),
            )
        };
        detail_blocks.push(format!(
            "{header}:\n  what answered:  {what_answered}\n  {your_journal}{footer}"
        ));
        tooltip_lines.push(header);
    }

    for line in tooltip_lines {
        health.tooltip.push('\n');
        health.tooltip.push_str(&line);
    }

    let detail_text = detail_blocks.join("\n\n");
    health.cli.push_str("\n\n");
    health.cli.push_str(&detail_text);
    health.doctor_detail.push_str("\n\n");
    health.doctor_detail.push_str(&detail_text);
    health.accessible_recording.push_str("\n\n");
    health.accessible_recording.push_str(&detail_text);
    health.accessible_idle.push_str("\n\n");
    health.accessible_idle.push_str(&detail_text);
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
    let unknown_journals = link
        .get("unknown_journals")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|val| spl_transport::UnknownJournal {
                    address: val
                        .get("address")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    jid: val
                        .get("jid")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let paired_jid = link
        .get("paired_jid")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let (unknown_spoken_marks, paired_spoken_mark) = if unknown_journals.is_empty() {
        (Vec::new(), None)
    } else {
        let loaded_unknown_spoken: Option<Vec<Option<String>>> = link
            .get("unknown_spoken_marks")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|val| val.as_str().map(ToOwned::to_owned))
                    .collect()
            });
        let loaded_paired_spoken: Option<String> = link
            .get("paired_spoken_mark")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let unknown_marks = if let Some(loaded) = loaded_unknown_spoken {
            if loaded.len() == unknown_journals.len() {
                loaded
            } else {
                unknown_journals
                    .iter()
                    .map(|item| {
                        item.jid
                            .as_deref()
                            .and_then(crate::private_link::format_spoken_mark)
                    })
                    .collect()
            }
        } else {
            unknown_journals
                .iter()
                .map(|item| {
                    item.jid
                        .as_deref()
                        .and_then(crate::private_link::format_spoken_mark)
                })
                .collect()
        };

        let paired_mark = if loaded_paired_spoken.is_some() {
            loaded_paired_spoken
        } else {
            paired_jid
                .as_deref()
                .and_then(crate::private_link::format_spoken_mark)
        };

        (unknown_marks, paired_mark)
    };
    Ok(Some(LinkFactState {
        pairing_required: boolean("pairing_required")?,
        journal_mark_held: link
            .get("journal_mark_held")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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
        optional_dial: false,
        unknown_journals,
        paired_jid,
        unknown_spoken_marks,
        paired_spoken_mark,
        unknown_marks: Vec::new(),
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
        imports: data.get("import").filter(|v| !v.is_null()).map(|v| {
            serde_json::from_value(v.clone()).unwrap_or_else(|_| crate::import::ImportSummary {
                waiting: 0,
                held: true,
                sending: false,
                recent: "needs attention".to_owned(),
            })
        }),
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
        episode_started: optional_float(&data, "episode_started"),
        no_network: data
            .get("no_network")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        last_check_failed: data
            .get("last_check_failed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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
            let unknown_journals_json: Vec<Value> = link
                .unknown_journals
                .iter()
                .map(|uj| {
                    json!({
                        "address": uj.address,
                        "jid": uj.jid,
                    })
                })
                .collect();
            let unknown_spoken_marks_json: Vec<Value> = link
                .unknown_spoken_marks
                .iter()
                .map(|mark| match mark {
                    Some(m) => Value::String(m.clone()),
                    None => Value::Null,
                })
                .collect();
            json!({
                "pairing_required": link.pairing_required,
                "journal_mark_held": link.journal_mark_held,
                "private_state_invalid": link.private_state_invalid,
                "config_sanitation_failed": link.config_sanitation_failed,
                "listener_ready": link.listener_ready,
                "carrier_proven": link.carrier_proven,
                "observer_registered": link.observer_registered,
                "transport_unavailable": link.transport_unavailable,
                "terminal_revocation": link.terminal_revocation,
                "token_persistence_failure": link.token_persistence_failure,
                "journal_version_observed": link.journal_version_observed,
                "unknown_journals": unknown_journals_json,
                "paired_jid": link.paired_jid,
                "unknown_spoken_marks": unknown_spoken_marks_json,
                "paired_spoken_mark": link.paired_spoken_mark,
            })
        });
    let mut text = serde_json::to_string(&json!({
        "schema_version": SCHEMA_VERSION,
        "import": facts.imports,
        "last_successful_sync": facts.last_successful_sync,
        "last_successful_contact": facts.last_successful_contact,
        "last_error_class": facts.last_error_class.map(ErrorType::as_str),
        "last_error_code": facts.last_error_code,
        "pending_confirmed": facts.pending_confirmed,
        "in_progress": facts.in_progress,
        "progress": facts.progress,
        "episode_started": facts.episode_started,
        "no_network": facts.no_network,
        "last_check_failed": facts.last_check_failed,
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

/// Drop the remembered journal version. Pairing again must not remove the rest
/// of the state directory: the intake counter lives there too.
pub(crate) fn forget_paired_journal(state_dir: &Path) {
    let _ = fs::remove_file(paired_journal_path(state_dir));
}

#[derive(Clone, Debug, PartialEq)]
pub struct PairedJournalVersion {
    pub identity_key: String,
    pub version: String,
    pub name: Option<String>,
    pub observed_at: f64,
    pub about: Option<crate::about::About>,
    pub about_observed_at: Option<f64>,
}

pub fn load_paired_journal_version(state_dir: &Path) -> Option<PairedJournalVersion> {
    let text = fs::read_to_string(paired_journal_path(state_dir)).ok()?;
    let Value::Object(data) = serde_json::from_str(&text).ok()? else {
        return None;
    };
    let identity_key = data.get("identity_key")?.as_str()?.to_owned();
    let version = data.get("version")?.as_str()?.to_owned();
    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    // Legacy version-only stores stay readable; an absent time never gains an age.
    let observed_at = data
        .get("observed_at")
        .and_then(Value::as_f64)
        .filter(|time| time.is_finite() && *time >= 0.0)
        .unwrap_or(-1.0);
    let about = data
        .get("about")
        .and_then(|value| crate::about::decode_about(&serde_json::to_vec(value).ok()?))
        .filter(|facts| facts.version.trim_start_matches('v') == version.trim_start_matches('v'));
    let about_observed_at = data
        .get("about_observed_at")
        .and_then(Value::as_f64)
        .filter(|time| time.is_finite() && *time >= 0.0);
    Some(PairedJournalVersion {
        identity_key,
        version,
        name,
        observed_at,
        about,
        about_observed_at,
    })
}

pub fn save_paired_journal_version(
    state_dir: &Path,
    identity_key: &str,
    version: &str,
    name: Option<&str>,
) -> io::Result<()> {
    save_paired_journal_version_guarded(
        state_dir,
        identity_key,
        version,
        name,
        &crate::private_file::NoWriteFault,
        &|| true,
    )
}

pub(crate) fn save_paired_journal_version_guarded(
    state_dir: &Path,
    identity_key: &str,
    version: &str,
    name: Option<&str>,
    fault: &dyn crate::private_file::DurableWriteFault,
    is_current: &dyn Fn() -> bool,
) -> io::Result<()> {
    let observed_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    fs::create_dir_all(state_dir)?;
    let path = paired_journal_path(state_dir);
    let mut payload = json!({
        "identity_key": identity_key,
        "version": version,
        "observed_at": observed_at,
    });
    // A version acceptance refreshes only the version observation. Keep host
    // facts only for the same identity and normalized version, at their old time.
    if let Some(old) = load_paired_journal_version(state_dir).filter(|old| {
        old.identity_key == identity_key
            && old.version.trim_start_matches('v') == version.trim_start_matches('v')
    }) {
        if let Some(about) = old.about {
            payload["about"] = serde_json::to_value(about).map_err(io::Error::other)?;
        }
        if let Some(time) = old.about_observed_at {
            payload["about_observed_at"] = json!(time);
        }
    }
    if let Some(n) = name {
        payload
            .as_object_mut()
            .expect("payload is object")
            .insert("name".to_owned(), json!(n));
    }
    let mut text = serde_json::to_string(&payload).map_err(io::Error::other)?;
    text.push('\n');
    crate::private_file::atomic_write_bytes_guarded(&path, text.as_bytes(), fault, is_current)
        .map_err(|e| io::Error::other(e.to_string()))
}

pub(crate) fn save_paired_journal_about_guarded(
    state_dir: &Path,
    identity_key: &str,
    about: &crate::about::About,
    fault: &dyn crate::private_file::DurableWriteFault,
    is_current: &dyn Fn() -> bool,
) -> io::Result<()> {
    if !about.valid() {
        return Err(io::Error::other("invalid About"));
    }
    let path = paired_journal_path(state_dir);
    let mut value: Value = serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
    let matches = value["identity_key"].as_str() == Some(identity_key)
        && value["version"].as_str().is_some_and(|version| {
            version.trim_start_matches('v') == about.version.trim_start_matches('v')
        });
    if !matches {
        return Err(io::Error::other(
            "About no longer matches the accepted version",
        ));
    }
    // This is a facts-only update. Neither version timestamp nor version currentness changes.
    value["about"] = serde_json::to_value(about).map_err(io::Error::other)?;
    value["about_observed_at"] = json!(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    );
    let mut text = serde_json::to_vec(&value).map_err(io::Error::other)?;
    text.push(b'\n');
    crate::private_file::atomic_write_bytes_guarded(&path, &text, fault, is_current)
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_counts_preserve_unknown_segments_and_add_known_counts_once() {
        let summary = crate::import::ImportSummary {
            waiting: 1,
            held: true,
            sending: false,
            recent: String::new(),
        };
        for pending in [None, Some(-1)] {
            let facts = SyncFacts {
                pending_confirmed: pending,
                imports: Some(summary.clone()),
                ..SyncFacts::default()
            };
            assert_eq!(facts.with_import_counts().pending_confirmed, None);
        }
        let facts = SyncFacts {
            pending_confirmed: Some(3),
            imports: Some(summary),
            ..SyncFacts::default()
        };
        assert_eq!(facts.with_import_counts().pending_confirmed, Some(4));
        assert_eq!(facts.pending_confirmed, Some(3));
    }
    use crate::config::DEFAULT_SYNC_STALE_THRESHOLD;
    use crate::private_link::LinkFactState;

    // a failed save leaves no pid-named scratch file behind. The temporary is never
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
            "sync: no status; the solstone app is not running, or has no status yet"
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
            last_successful_contact: Some(1000.0),
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

    // A 401 is its own state: not revoked and never connected, even on an empty queue with
    // the link up. A transient failure inside the lease changes nothing.
    #[test]
    fn auth_401_is_neither_revoked_nor_connected() {
        for pending_confirmed in [None, Some(0)] {
            let template = SyncFacts {
                last_successful_contact: Some(990.0),
                pending_confirmed,
                in_progress: false,
                link: Some(LinkFactState {
                    carrier_proven: true,
                    observer_registered: true,
                    ..LinkFactState::default()
                }),
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
            assert_ne!(unauthorized, HealthState::RePairRequired);
            assert!(!matches!(
                unauthorized,
                HealthState::Connected | HealthState::Syncing
            ));
            assert_eq!(
                state_for(ErrorType::Auth, Some(403)),
                HealthState::RePairRequired
            );
            let up = if pending_confirmed == Some(0) {
                HealthState::Connected
            } else {
                HealthState::Syncing
            };
            assert_eq!(state_for(ErrorType::Transient, Some(500)), up);
            assert_eq!(state_for(ErrorType::Transient, None), up);
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
            last_successful_contact: Some(1000.0),
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
                carrier_proven: true,
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
            imports: Some(crate::import::ImportSummary {
                waiting: 2,
                held: true,
                sending: false,
                recent: "waiting".to_owned(),
            }),
            last_successful_sync: Some(100.5),
            last_successful_contact: Some(200.5),
            last_error_class: Some(ErrorType::Incompatible),
            last_error_code: Some(404),
            pending_confirmed: None,
            in_progress: true,
            progress: "uploading 120000_300".to_owned(),
            episode_started: Some(150.0),
            no_network: true,
            last_check_failed: true,
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
        assert_eq!(SURFACE_BY_STATE.len(), 13);
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
                    journal_mark_held: true,
                    ..Default::default()
                }),
                HealthState::JournalMarkHeld,
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
                    last_successful_contact: Some(0.0),
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
                HealthState::Connecting,
            ),
            (
                SyncFacts {
                    last_successful_contact: Some(1_000.0),
                    in_progress: true,
                    ..link(LinkFactState {
                        carrier_proven: true,
                        observer_registered: true,
                        ..Default::default()
                    })
                },
                HealthState::Syncing,
            ),
        ];
        for (facts, expected) in single_cases {
            assert_eq!(state(facts), expected);
        }

        let all = LinkFactState {
            pairing_required: true,
            journal_mark_held: true,
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
            optional_dial: false,
            ..Default::default()
        };
        let conflicting_cases = [
            (all.clone(), HealthState::UnsafeLinkState),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    ..all.clone()
                },
                HealthState::JournalMarkHeld,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    journal_mark_held: false,
                    ..all.clone()
                },
                HealthState::RePairRequired,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    journal_mark_held: false,
                    terminal_revocation: false,
                    ..all.clone()
                },
                HealthState::TokenPersistenceFailed,
            ),
            (
                LinkFactState {
                    private_state_invalid: false,
                    config_sanitation_failed: false,
                    journal_mark_held: false,
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
                    journal_mark_held: false,
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

    // invalid fields fall back independently and signed pending values round-trip.
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

    // formatters retain Python truncation, units, and defaulted-vs-raw progress behavior.
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
            last_successful_contact: Some(1.0),
            link: Some(LinkFactState {
                carrier_proven: true,
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

        save_paired_journal_version(temp.path(), "inst-1:fp-1", "1.4.0", Some("My Journal"))
            .unwrap();
        let loaded = load_paired_journal_version(temp.path()).unwrap();
        assert_eq!(loaded.identity_key, "inst-1:fp-1");
        assert_eq!(loaded.version, "1.4.0");
        assert_eq!(loaded.name.as_deref(), Some("My Journal"));
        assert!(loaded.observed_at > 0.0);

        // Overwrite without name
        save_paired_journal_version(temp.path(), "inst-2:fp-2", "2.0.0", None).unwrap();
        let loaded2 = load_paired_journal_version(temp.path()).unwrap();
        assert_eq!(loaded2.identity_key, "inst-2:fp-2");
        assert_eq!(loaded2.version, "2.0.0");
        assert_eq!(loaded2.name, None);

        // Corrupt file falls back to None
        fs::write(paired_journal_path(temp.path()), "not-json").unwrap();
        assert_eq!(load_paired_journal_version(temp.path()), None);
    }

    fn decode_hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn unknown_journal_overlay_formats_address_sighting_with_mark() {
        let spki_hex = "3059301306072a8648ce3d020106082a8648ce3d03010703420004471c3e758c4904285bba7e53118ed0f524adeb0757d25bd2f8e7b0d76dfa714cdd520f7aca8a8b917acc37f51de8f0c9bbe3ad858382e702dc25a12d09f7a858";
        let jid = spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex)).unwrap();
        let spki_hex_paired = "3059301306072a8648ce3d020106082a8648ce3d030107034200047cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1";
        let paired_jid =
            spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex_paired)).unwrap();

        let facts = SyncFacts {
            link: Some(LinkFactState {
                transport_unavailable: true,
                unknown_journals: vec![spl_transport::UnknownJournal {
                    address: Some("192.168.1.50:5015".into()),
                    jid: Some(jid),
                }],
                paired_jid: Some(paired_jid),
                unknown_spoken_marks: vec![Some("blue, purple · liquefy·smock".into())],
                paired_spoken_mark: Some("pink, cyan · distrust·chokehold".into()),
                ..Default::default()
            }),
            ..SyncFacts::default()
        };

        let health = derive_health(&facts, 1000.0, 600.0);
        assert_eq!(health.state, HealthState::TransportUnavailable);
        assert_eq!(health.sni_status, "NeedsAttention");
        assert_eq!(health.dbus, "transport-unavailable");
        assert_eq!(health.icon, "offline");
        assert!(
            health
                .tooltip
                .contains("unknown journal seen at 192.168.1.50:5015")
        );

        let expected_block = format!(
            "{}:
  what answered:  blue, purple · liquefy·smock  (claimed, not verified)
  your journal:   pink, cyan · distrust·chokehold
  {UNKNOWN_JOURNAL_RESET_LINE}",
            unknown_journal_seen_at("192.168.1.50:5015")
        );

        assert!(health.cli.ends_with(&expected_block));
        assert!(health.doctor_detail.ends_with(&expected_block));
        assert!(health.accessible_recording.ends_with(&expected_block));
        assert!(health.accessible_idle.ends_with(&expected_block));
    }

    #[test]
    fn unknown_journal_overlay_formats_relay_sighting() {
        let spki_hex = "3059301306072a8648ce3d020106082a8648ce3d03010703420004471c3e758c4904285bba7e53118ed0f524adeb0757d25bd2f8e7b0d76dfa714cdd520f7aca8a8b917acc37f51de8f0c9bbe3ad858382e702dc25a12d09f7a858";
        let jid = spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex)).unwrap();
        let spki_hex_paired = "3059301306072a8648ce3d020106082a8648ce3d030107034200047cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1";
        let paired_jid =
            spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex_paired)).unwrap();

        let facts = SyncFacts {
            link: Some(LinkFactState {
                transport_unavailable: true,
                unknown_journals: vec![spl_transport::UnknownJournal {
                    address: None,
                    jid: Some(jid),
                }],
                paired_jid: Some(paired_jid),
                unknown_spoken_marks: vec![Some("blue, purple · liquefy·smock".into())],
                paired_spoken_mark: Some("pink, cyan · distrust·chokehold".into()),
                ..Default::default()
            }),
            ..SyncFacts::default()
        };

        let health = derive_health(&facts, 1000.0, 600.0);
        assert_eq!(health.state, HealthState::TransportUnavailable);
        assert!(
            health
                .tooltip
                .contains("unknown journal seen through the relay")
        );

        let expected_block = format!(
            "{UNKNOWN_JOURNAL_SEEN_THROUGH_RELAY}:
  what answered:  blue, purple · liquefy·smock  (claimed, not verified)
  your journal:   pink, cyan · distrust·chokehold
  {UNKNOWN_JOURNAL_RELAY_LINE}"
        );

        assert!(health.cli.ends_with(&expected_block));
        assert!(health.doctor_detail.ends_with(&expected_block));
    }

    #[test]
    fn unknown_journal_overlay_formats_no_identity_presented() {
        let spki_hex_paired = "3059301306072a8648ce3d020106082a8648ce3d030107034200047cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1";
        let paired_jid =
            spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex_paired)).unwrap();

        let facts = SyncFacts {
            link: Some(LinkFactState {
                transport_unavailable: true,
                unknown_journals: vec![spl_transport::UnknownJournal {
                    address: Some("127.0.0.1:5015".into()),
                    jid: None,
                }],
                paired_jid: Some(paired_jid),
                unknown_spoken_marks: vec![None],
                paired_spoken_mark: Some("pink, cyan · distrust·chokehold".into()),
                ..Default::default()
            }),
            ..SyncFacts::default()
        };

        let health = derive_health(&facts, 1000.0, 600.0);
        assert_eq!(health.state, HealthState::TransportUnavailable);
        assert!(
            health
                .tooltip
                .contains("unknown journal seen at 127.0.0.1:5015")
        );

        // Nothing was claimed, so there's no footer to act on.
        let expected_block = format!(
            "{}:
  what answered:  no identity presented
  your journal:   pink, cyan · distrust·chokehold",
            unknown_journal_seen_at("127.0.0.1:5015")
        );

        assert!(health.cli.ends_with(&expected_block));
        assert!(!health.cli.contains("claimed, not verified"));
    }

    #[test]
    fn save_and_load_facts_preserves_unknown_journals_and_paired_jid() {
        let temp = tempfile::tempdir().unwrap();
        let facts = SyncFacts {
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                unknown_journals: vec![spl_transport::UnknownJournal {
                    address: Some("127.0.0.1:5015".into()),
                    jid: Some("test-jid".into()),
                }],
                paired_jid: Some("paired-jid".into()),
                ..Default::default()
            }),
            link_epoch: Some(ProcessEpoch::for_test(1)),
            ..SyncFacts::default()
        };
        save_facts(temp.path(), &facts).unwrap();

        let liveness = PrivateStateLockLiveness::LiveOwner;
        let text = fs::read_to_string(sync_health_path(temp.path())).unwrap();
        let Value::Object(data) = serde_json::from_str(&text).unwrap() else {
            panic!("expected json object");
        };
        let loaded_link = load_link_facts(&data, liveness).unwrap().unwrap();
        assert_eq!(
            loaded_link.unknown_journals,
            facts.link.unwrap().unknown_journals
        );
        assert_eq!(loaded_link.paired_jid, Some("paired-jid".to_owned()));
    }

    #[test]
    fn load_link_facts_backward_compatible_with_missing_unknown_journals() {
        let epoch = ProcessEpoch::for_test(1);
        let data: Map<String, Value> = serde_json::from_str(&format!(
            r#"{{
            "schema_version": 2,
            "link_epoch": "{}",
            "link": {{
                "pairing_required": false,
                "private_state_invalid": false,
                "config_sanitation_failed": false,
                "listener_ready": true,
                "carrier_proven": false,
                "observer_registered": false,
                "transport_unavailable": false,
                "terminal_revocation": false,
                "token_persistence_failure": false,
                "journal_version_observed": false,
                "paired_jid": "some-paired-jid"
            }}
        }}"#,
            epoch.as_str()
        ))
        .unwrap();

        let loaded = load_link_facts(&data, PrivateStateLockLiveness::LiveOwner)
            .unwrap()
            .unwrap();
        assert!(loaded.listener_ready);
        assert!(loaded.unknown_journals.is_empty());
        assert_eq!(loaded.paired_jid, Some("some-paired-jid".to_owned()));
        assert!(loaded.unknown_spoken_marks.is_empty());
        assert_eq!(loaded.paired_spoken_mark, None);
    }

    #[test]
    fn held_health_outranks_transport_auth_and_stale() {
        let now = 1000.0;
        let stale_threshold = 60.0;

        let facts = SyncFacts {
            link: Some(LinkFactState {
                journal_mark_held: true,
                transport_unavailable: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let health = derive_health(&facts, now, stale_threshold);
        assert_eq!(health.state, HealthState::JournalMarkHeld);

        let facts_stale = SyncFacts {
            last_successful_contact: Some(now - 100.0),
            link: Some(LinkFactState {
                journal_mark_held: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let health_stale = derive_health(&facts_stale, now, stale_threshold);
        assert_eq!(health_stale.state, HealthState::JournalMarkHeld);

        let facts_auth = SyncFacts {
            last_error_class: Some(ErrorType::Auth),
            last_error_code: Some(403),
            link: Some(LinkFactState {
                journal_mark_held: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let health_auth = derive_health(&facts_auth, now, stale_threshold);
        assert_eq!(health_auth.state, HealthState::JournalMarkHeld);

        let facts_incompat = SyncFacts {
            last_error_class: Some(ErrorType::Incompatible),
            link: Some(LinkFactState {
                journal_mark_held: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let health_incompat = derive_health(&facts_incompat, now, stale_threshold);
        assert_eq!(health_incompat.state, HealthState::JournalMarkHeld);

        let facts_unsafe = SyncFacts {
            link: Some(LinkFactState {
                journal_mark_held: true,
                private_state_invalid: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let health_unsafe = derive_health(&facts_unsafe, now, stale_threshold);
        assert_eq!(health_unsafe.state, HealthState::UnsafeLinkState);

        assert_eq!(
            health.header_recording,
            "waiting for you to confirm your journal's mark"
        );
        assert_eq!(
            health.header_idle,
            "waiting for you to confirm your journal's mark"
        );
        assert_eq!(
            health.sync_line,
            "waiting for you to confirm your journal's mark"
        );
        assert_eq!(
            health.tooltip,
            "waiting for you to confirm your journal's mark"
        );
        assert_eq!(
            health.accessible_recording,
            "waiting for you to confirm your journal's mark"
        );
        assert_eq!(
            health.accessible_idle,
            "waiting for you to confirm your journal's mark"
        );

        let expected_detail = "waiting for you to confirm your journal's mark. nothing waiting goes into your journal until you do.\nwhen you're ready, run: solstone-linux confirm";
        assert_eq!(health.cli, expected_detail);
        assert_eq!(health.doctor_detail, expected_detail);
    }

    /// Scripted traces for the connection-status rules. The wall clock and the monotonic
    /// clock run together unless a trace sleeps the machine.
    mod connection_status {
        use super::*;

        fn up(contact: f64) -> SyncFacts {
            SyncFacts {
                last_successful_contact: Some(contact),
                pending_confirmed: Some(0),
                episode_started: Some(0.0),
                link: Some(LinkFactState {
                    carrier_proven: true,
                    observer_registered: true,
                    ..LinkFactState::default()
                }),
                ..SyncFacts::default()
            }
        }

        fn carrier(facts: &mut SyncFacts, proven: bool) {
            facts.link.as_mut().unwrap().carrier_proven = proven;
        }

        #[derive(Default)]
        struct Trace {
            presenter: Presenter,
        }

        impl Trace {
            fn at(&mut self, facts: &SyncFacts, t: f64) -> HealthState {
                self.presenter.present(
                    derive_state(facts, t, DEFAULT_SYNC_STALE_THRESHOLD as f64),
                    t,
                )
            }
        }

        // I1
        #[test]
        fn a_failed_dial_shows_at_once() {
            let mut trace = Trace::default();
            let mut facts = up(100.0);
            assert_eq!(trace.at(&facts, 100.5), HealthState::Connected);
            carrier(&mut facts, false);
            assert_eq!(trace.at(&facts, 101.0), HealthState::Connecting);
        }

        // I2
        #[test]
        fn connected_never_outlives_the_lease() {
            let mut trace = Trace::default();
            let facts = up(100.0);
            let mut t = 100.0;
            while t < 100.0 + 2.0 * CONNECTED_LEASE {
                let shown = trace.at(&facts, t);
                assert_eq!(
                    shown == HealthState::Connected,
                    t - 100.0 <= CONNECTED_LEASE,
                    "at {t}"
                );
                t += 0.25;
            }
        }

        // I3
        #[test]
        fn a_drop_shows_for_at_least_the_dwell() {
            let mut trace = Trace::default();
            let mut facts = up(200.0);
            assert_eq!(trace.at(&facts, 200.0), HealthState::Connected);
            carrier(&mut facts, false);
            assert_eq!(trace.at(&facts, 200.1), HealthState::Connecting);
            carrier(&mut facts, true);
            facts.last_successful_contact = Some(200.2);
            for t in [200.2, 200.5, 201.0] {
                assert_eq!(trace.at(&facts, t), HealthState::Connecting, "at {t}");
            }
            assert_eq!(trace.at(&facts, 201.1), HealthState::Connected);
        }

        // I3, over a run of flaps sampled every 100 ms.
        #[test]
        fn no_dropped_state_is_shown_for_under_a_second() {
            let mut trace = Trace::default();
            let mut facts = up(0.0);
            let mut shown = Vec::new();
            for step in 0..400 {
                let t = f64::from(step) * 0.1;
                let proven = (step / 3) % 2 == 0;
                carrier(&mut facts, proven);
                if proven {
                    facts.last_successful_contact = Some(t);
                }
                shown.push((t, trace.at(&facts, t)));
            }
            let mut down_since = None;
            for (t, state) in shown {
                match (state, down_since) {
                    (HealthState::Connecting, None) => down_since = Some(t),
                    (HealthState::Connected, Some(since)) => {
                        assert!(t - since >= DOWN_DWELL - 1e-9, "down {since}..{t}");
                        down_since = None;
                    }
                    _ => {}
                }
            }
        }

        // I3, when the drop passes from connecting to offline inside the dwell.
        #[test]
        fn the_dwell_holds_across_connecting_to_offline() {
            let mut trace = Trace::default();
            let mut facts = up(0.0);
            assert_eq!(trace.at(&facts, 29.5), HealthState::Connected);
            carrier(&mut facts, false);
            assert_eq!(trace.at(&facts, 29.6), HealthState::Connecting);
            assert_eq!(trace.at(&facts, 30.0), HealthState::Offline);
            carrier(&mut facts, true);
            facts.last_successful_contact = Some(30.1);
            assert_eq!(trace.at(&facts, 30.1), HealthState::Offline);
            assert_eq!(trace.at(&facts, 30.6), HealthState::Connected);
        }

        // I4
        #[test]
        fn one_failed_check_inside_the_lease_changes_nothing() {
            let mut trace = Trace::default();
            let mut facts = up(300.0);
            assert_eq!(trace.at(&facts, 300.0), HealthState::Connected);
            facts.last_error_class = Some(ErrorType::Transient);
            let mut t = 315.0;
            while t <= 300.0 + CONNECTED_LEASE {
                assert_eq!(trace.at(&facts, t), HealthState::Connected, "at {t}");
                t += 0.5;
            }
        }

        // I5
        #[test]
        fn syncing_lingers_after_the_queue_drains() {
            let mut trace = Trace::default();
            let mut facts = up(10.0);
            facts.in_progress = true;
            assert_eq!(trace.at(&facts, 10.0), HealthState::Syncing);
            facts.in_progress = false;
            for t in [10.1, 11.0, 11.4] {
                assert_eq!(trace.at(&facts, t), HealthState::Syncing, "at {t}");
            }
            assert_eq!(trace.at(&facts, 11.5), HealthState::Connected);
        }

        // I6
        #[test]
        fn a_fresh_launch_never_shows_connected_before_evidence() {
            let mut trace = Trace::default();
            // What the previous run left: a recent answer and an empty queue. This run has
            // proven nothing yet.
            let mut facts = SyncFacts {
                last_successful_contact: Some(999.0),
                pending_confirmed: Some(0),
                episode_started: Some(1000.0),
                link: Some(LinkFactState::default()),
                ..SyncFacts::default()
            };
            for t in [1000.0, 1010.0, 1029.0] {
                assert_eq!(trace.at(&facts, t), HealthState::Connecting, "at {t}");
            }
            assert_eq!(trace.at(&facts, 1030.0), HealthState::Offline);
            facts.link = Some(LinkFactState {
                observer_registered: true,
                ..LinkFactState::default()
            });
            assert_eq!(trace.at(&facts, 1031.0), HealthState::Offline);
            carrier(&mut facts, true);
            facts.last_successful_contact = Some(1032.0);
            assert_eq!(trace.at(&facts, 1032.0), HealthState::Connected);
        }

        #[test]
        fn offline_waits_for_thirty_seconds_unreached_and_a_new_attempt_restarts_it() {
            let mut trace = Trace::default();
            let mut facts = up(0.0);
            carrier(&mut facts, false);
            assert_eq!(trace.at(&facts, 29.0), HealthState::Connecting);
            assert_eq!(trace.at(&facts, OFFLINE_AFTER), HealthState::Offline);
            facts.episode_started = Some(100.0);
            assert_eq!(trace.at(&facts, 100.0), HealthState::Connecting);
            assert_eq!(trace.at(&facts, 130.0), HealthState::Offline);
        }

        // R6: the OS reporting no network makes a down link offline at once.
        #[test]
        fn no_network_with_the_link_down_is_offline_at_once() {
            let mut trace = Trace::default();
            let mut facts = up(100.0);
            assert_eq!(trace.at(&facts, 100.0), HealthState::Connected);
            facts.no_network = true;
            carrier(&mut facts, false);
            assert_eq!(trace.at(&facts, 101.0), HealthState::Offline);
        }

        #[test]
        fn no_network_and_an_unanswered_check_is_offline_at_once() {
            let mut trace = Trace::default();
            let mut facts = up(100.0);
            assert_eq!(trace.at(&facts, 100.0), HealthState::Connected);
            facts.no_network = true;
            facts.last_check_failed = true;
            assert_eq!(trace.at(&facts, 101.0), HealthState::Offline);
            // The answer that comes back clears it, after the drop's dwell.
            facts.no_network = false;
            facts.last_check_failed = false;
            facts.last_successful_contact = Some(101.5);
            assert_eq!(trace.at(&facts, 101.5), HealthState::Offline);
            assert_eq!(trace.at(&facts, 102.0), HealthState::Connected);
        }

        #[test]
        fn no_network_alone_never_withdraws_connected_while_the_journal_is_heard() {
            let mut trace = Trace::default();
            let mut facts = up(100.0);
            facts.no_network = true;
            let mut t = 100.0;
            while t <= 100.0 + CONNECTED_LEASE {
                assert_eq!(trace.at(&facts, t), HealthState::Connected, "at {t}");
                t += 0.5;
            }
        }

        // I4 holds with a network: an unanswered check inside the lease changes nothing.
        #[test]
        fn an_unanswered_check_with_a_network_changes_nothing() {
            let mut trace = Trace::default();
            let mut facts = up(100.0);
            facts.last_check_failed = true;
            assert_eq!(trace.at(&facts, 116.0), HealthState::Connected);
        }

        #[test]
        fn a_status_check_alone_is_not_syncing() {
            let mut trace = Trace::default();
            let mut facts = up(50.0);
            facts.progress = "checking journal...".to_owned();
            assert_eq!(trace.at(&facts, 50.0), HealthState::Connected);
            facts.pending_confirmed = Some(2);
            assert_eq!(trace.at(&facts, 51.0), HealthState::Syncing);
        }

        #[test]
        fn a_wake_holds_connected_briefly_while_a_fresh_check_answers() {
            let mut presenter = Presenter::default();
            let mut facts = up(500.0);
            let stale = DEFAULT_SYNC_STALE_THRESHOLD as f64;
            assert!(!presenter.woke(500.0, 500.0));
            assert_eq!(
                presenter.present(derive_state(&facts, 500.0, stale), 500.0),
                HealthState::Connected
            );
            // An hour asleep: the monotonic clock barely moved.
            assert!(presenter.woke(4100.0, 501.0));
            presenter.hold_for_fresh_check(501.0);
            facts.episode_started = Some(4100.0);
            assert_eq!(
                presenter.present(derive_state(&facts, 4100.0, stale), 501.0),
                HealthState::Connected
            );
            // The check answers inside the grace: no blink.
            facts.last_successful_contact = Some(4100.3);
            assert_eq!(
                presenter.present(derive_state(&facts, 4100.3, stale), 501.3),
                HealthState::Connected
            );

            // When it does not answer, the truth shows once the grace ends.
            assert!(presenter.woke(9000.0, 502.0));
            presenter.hold_for_fresh_check(502.0);
            facts.episode_started = Some(9000.0);
            assert_eq!(
                presenter.present(derive_state(&facts, 9000.0, stale), 502.0),
                HealthState::Connected
            );
            assert_eq!(
                presenter.present(derive_state(&facts, 9000.5, stale), 502.5),
                HealthState::Connecting
            );
        }
    }
}
