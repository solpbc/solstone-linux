// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spl_core::pairlink;
use spl_transport::credential::Credential;
use tokio::sync::{mpsc, oneshot};

use crate::activity_prefs::{ActivityPolicy, ActivityPrefs};
use crate::config::{ConfigPaths, set_guided_field};
use crate::journal_mark::{
    JOURNAL_MARK_RECHECK_INTERVAL, PAIRING_ANSWER_FILENAME, begin_pairing_commit,
    finish_pairing_commit_with_fault, is_pairing_confirmed, pairing_commit_pending,
    read_pairing_answer, retire_client_registration, write_pairing_answer_with_fault,
};
use crate::observer::{Mode, StateSnapshot};
use crate::private_file::DurableWriteFault;
use crate::private_link::{
    CREDENTIALS_FILENAME, LinkFact, LinkFacts, OpenJournalAccess, Pairer, PrivateLinkOwner,
    PrivateStateError, PrivateStateLock, PrivateTargetKind, format_spoken_mark, load_credential,
    pair_and_admit, persist_credential_with_fault, read_private_file,
};
use crate::sync::{SyncResetHandle, SyncSampler, SyncTrigger};
use crate::sync_health::SyncHealth;
use crate::tray_model;
use crate::upload::UploadClient;

pub const CEREMONY_TTL: Duration = Duration::from_secs(600);
pub const PAIR_BOUND: Duration = Duration::from_secs(30);

pub const DEFAULT_CAPTURE_FRAMERATE: i64 = 1;
pub const DEFAULT_DRAW_CURSOR: bool = true;
pub const DEFAULT_START_PAUSED: bool = false;
pub const DEFAULT_SEGMENT_INTERVAL: i64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicalLinkError;

impl std::fmt::Display for GraphicalLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed pairing link")
    }
}

impl std::error::Error for GraphicalLinkError {}

pub fn adapt_graphical_link(input: &str) -> Result<String, GraphicalLinkError> {
    let s = input.trim();
    let (base, fragment) = s.split_once('#').ok_or(GraphicalLinkError)?;
    if fragment.is_empty() {
        return Err(GraphicalLinkError);
    }
    let (scheme, rest) = base.split_once("://").ok_or(GraphicalLinkError)?;
    if rest.contains('@') || rest.contains('?') || rest.contains(':') {
        return Err(GraphicalLinkError);
    }
    if scheme.eq_ignore_ascii_case("https") {
        if rest == "go.solstone.app/p" {
            return Ok(s.to_string());
        }
    } else if scheme.eq_ignore_ascii_case("solstone") && (rest == "pair" || rest == "pair/") {
        return Ok(s.to_string());
    }
    Err(GraphicalLinkError)
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MarkColorDocument {
    pub name: String,
    pub hex: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MarkIconDocument {
    pub name: String,
    pub svg: String,
    pub color: MarkColorDocument,
    pub rot: u16,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MarkRenderDocument {
    pub icon1: MarkIconDocument,
    pub icon2: MarkIconDocument,
    pub words: [String; 2],
}

impl From<spl_core::mark::MarkRenderSpec> for MarkRenderDocument {
    fn from(spec: spl_core::mark::MarkRenderSpec) -> Self {
        Self {
            icon1: MarkIconDocument {
                name: spec.icon1.name,
                svg: spec.icon1.svg,
                color: MarkColorDocument {
                    name: spec.icon1.color.name,
                    hex: spec.icon1.color.hex,
                },
                rot: spec.icon1.rot,
            },
            icon2: MarkIconDocument {
                name: spec.icon2.name,
                svg: spec.icon2.svg,
                color: MarkColorDocument {
                    name: spec.icon2.color.name,
                    hex: spec.icon2.color.hex,
                },
                rot: spec.icon2.rot,
            },
            words: spec.words,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SampledInstantDocument {
    pub at: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ControlDocument {
    pub v: u32,
    pub durable: String,
    pub owner: String,
    pub attempt: Option<AttemptDocument>,
    pub health: String,
    pub capture: String,
    pub settings: SettingsDocument,
    pub restart_required: Option<bool>,
    pub open_journal: Option<String>,
    pub fault: Option<String>,
    #[serde(default)]
    pub saved_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<SettingsDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_status: Option<RouteStatusDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_mark: Option<MarkRenderDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_mark: Option<MarkRenderDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_bounded: Option<bool>,
    #[serde(default)]
    pub capture_attention: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<SampledInstantDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AttemptDocument {
    pub id: String,
    pub pairing_id: String,
    pub state: String,
    pub mark: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RouteStatusDocument {
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default = "default_capability_status")]
    pub foreground: String,
    #[serde(default = "default_capability_status")]
    pub title: String,
    #[serde(default = "default_capability_status")]
    pub url: String,
    #[serde(default = "default_capability_status")]
    pub idle: String,
}

fn default_capability_status() -> String {
    "unavailable".to_string()
}

impl Default for RouteStatusDocument {
    fn default() -> Self {
        Self {
            route: None,
            foreground: "unavailable".into(),
            title: "unavailable".into(),
            url: "unavailable".into(),
            idle: "unavailable".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteStatusModel {
    pub route: Option<String>,
    pub foreground: String,
    pub title: String,
    pub url: String,
    pub idle: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SettingsDocument {
    pub capture_framerate: i64,
    pub draw_cursor: bool,
    pub start_paused: bool,
    pub segment_interval: i64,
    #[serde(default)]
    pub activity_source: bool,
    #[serde(default)]
    pub activity_titles: bool,
    #[serde(default)]
    pub activity_urls: bool,
    #[serde(default)]
    pub activity_exclude_apps: Vec<String>,
    #[serde(default)]
    pub activity_exclude_titles: Vec<String>,
    #[serde(default)]
    pub activity_through_pause: bool,
    #[serde(default)]
    pub activity_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionView {
    Confirmed,
    Disconnected,
    Unavailable,
    Pending,
    ConfirmedUnavailable,
    Failed,
    Held,
    Unpaired,
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlModel {
    pub view: SessionView,
    pub durable: String,
    pub owner: String,
    pub attempt: Option<AttemptModel>,
    pub health: String,
    pub capture: String,
    pub settings: SettingsModel,
    pub restart_required: Option<bool>,
    pub open_journal: Option<String>,
    pub fault: Option<String>,
    pub saved_available: bool,
    pub effective: Option<SettingsModel>,
    pub route_status: Option<RouteStatusModel>,
    pub pending_mark: Option<MarkRenderDocument>,
    pub confirmed_mark: Option<MarkRenderDocument>,
    pub pause_bounded: Option<bool>,
    pub capture_attention: bool,
    pub queue_count: Option<i64>,
    pub quarantine_count: Option<u64>,
    pub last_delivery: Option<SampledInstantDocument>,
    pub daemon_version: Option<String>,
    pub config_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptModel {
    pub id: String,
    pub pairing_id: String,
    pub state: String,
    pub mark: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsModel {
    pub capture_framerate: i64,
    pub draw_cursor: bool,
    pub start_paused: bool,
    pub segment_interval: i64,
    pub activity_source: bool,
    pub activity_titles: bool,
    pub activity_urls: bool,
    pub activity_exclude_apps: Vec<String>,
    pub activity_exclude_titles: Vec<String>,
    pub activity_through_pause: bool,
    pub activity_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    #[allow(dead_code)]
    InvalidJson(String),
    UnsupportedVersion(u32),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidJson(msg) => write!(f, "Invalid JSON: {msg}"),
            DecodeError::UnsupportedVersion(v) => write!(f, "Unsupported version: {v}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl ControlModel {
    pub fn unknown() -> Self {
        Self {
            view: SessionView::Unknown,
            durable: "unavailable".into(),
            owner: "absent".into(),
            attempt: None,
            health: "not-reported".into(),
            capture: String::new(),
            settings: SettingsModel {
                capture_framerate: DEFAULT_CAPTURE_FRAMERATE,
                draw_cursor: DEFAULT_DRAW_CURSOR,
                start_paused: DEFAULT_START_PAUSED,
                segment_interval: DEFAULT_SEGMENT_INTERVAL,
                activity_source: crate::config::DEFAULT_ACTIVITY_SOURCE,
                activity_titles: crate::config::DEFAULT_ACTIVITY_TITLES,
                activity_urls: false,
                activity_exclude_apps: Vec::new(),
                activity_exclude_titles: Vec::new(),
                activity_through_pause: crate::config::DEFAULT_ACTIVITY_THROUGH_PAUSE,
                activity_only: false,
            },
            restart_required: None,
            open_journal: None,
            fault: None,
            saved_available: false,
            effective: None,
            route_status: None,
            pending_mark: None,
            confirmed_mark: None,
            pause_bounded: None,
            capture_attention: false,
            queue_count: None,
            quarantine_count: None,
            last_delivery: None,
            daemon_version: None,
            config_path: None,
        }
    }
}

pub fn decode_control<E: std::fmt::Display>(
    raw: Result<String, E>,
) -> Result<ControlModel, DecodeError> {
    let s = match raw {
        Ok(s) => s,
        Err(_) => return Ok(ControlModel::unknown()),
    };
    let val: Value = match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(_) => return Ok(ControlModel::unknown()),
    };
    let version = val.get("v").and_then(Value::as_u64).unwrap_or(0) as u32;
    if version != 1 {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let doc: ControlDocument = match serde_json::from_value(val) {
        Ok(doc) => doc,
        Err(_) => return Ok(ControlModel::unknown()),
    };
    let view = derive_session_view(&doc);
    Ok(ControlModel {
        view,
        durable: doc.durable,
        owner: doc.owner,
        attempt: doc.attempt.map(|a| AttemptModel {
            id: a.id,
            pairing_id: a.pairing_id,
            state: a.state,
            mark: a.mark,
        }),
        health: doc.health,
        capture: doc.capture,
        settings: SettingsModel {
            capture_framerate: doc.settings.capture_framerate,
            draw_cursor: doc.settings.draw_cursor,
            start_paused: doc.settings.start_paused,
            segment_interval: doc.settings.segment_interval,
            activity_source: doc.settings.activity_source,
            activity_titles: doc.settings.activity_titles,
            activity_urls: doc.settings.activity_urls,
            activity_exclude_apps: doc.settings.activity_exclude_apps,
            activity_exclude_titles: doc.settings.activity_exclude_titles,
            activity_through_pause: doc.settings.activity_through_pause,
            activity_only: doc.settings.activity_only,
        },
        restart_required: doc.restart_required,
        open_journal: doc.open_journal,
        fault: doc.fault,
        saved_available: doc.saved_available,
        effective: doc.effective.map(|s| SettingsModel {
            capture_framerate: s.capture_framerate,
            draw_cursor: s.draw_cursor,
            start_paused: s.start_paused,
            segment_interval: s.segment_interval,
            activity_source: s.activity_source,
            activity_titles: s.activity_titles,
            activity_urls: s.activity_urls,
            activity_exclude_apps: s.activity_exclude_apps,
            activity_exclude_titles: s.activity_exclude_titles,
            activity_through_pause: s.activity_through_pause,
            activity_only: s.activity_only,
        }),
        route_status: doc.route_status.map(|r| RouteStatusModel {
            route: r.route,
            foreground: r.foreground,
            title: r.title,
            url: r.url,
            idle: r.idle,
        }),
        pending_mark: doc.pending_mark,
        confirmed_mark: doc.confirmed_mark,
        pause_bounded: doc.pause_bounded,
        capture_attention: doc.capture_attention,
        queue_count: doc.queue_count,
        quarantine_count: doc.quarantine_count,
        last_delivery: doc.last_delivery,
        daemon_version: doc.daemon_version,
        config_path: doc.config_path,
    })
}

pub fn decode_control_str(s: &str) -> Result<ControlModel, DecodeError> {
    decode_control(Ok::<_, &'static str>(s.to_string()))
}

fn derive_session_view(doc: &ControlDocument) -> SessionView {
    if let Some(ref attempt) = doc.attempt
        && attempt.state == "pending"
    {
        return SessionView::Pending;
    }

    if doc.durable == "confirmed" {
        if doc.owner == "running" {
            match doc.health.as_str() {
                "connected" | "syncing" | "connecting" | "listener-ready" => SessionView::Confirmed,
                "offline"
                | "transport-unavailable"
                | "unsafe-link-state"
                | "token-persistence-failed"
                | "re-pair-required"
                | "update-required"
                | "journal-mark-held"
                | "pairing-required" => SessionView::Disconnected,
                "not-reported" => SessionView::Unavailable,
                _ => SessionView::Unavailable,
            }
        } else {
            SessionView::ConfirmedUnavailable
        }
    } else {
        let is_failed_attempt = doc
            .attempt
            .as_ref()
            .is_some_and(|a| matches!(a.state.as_str(), "failed" | "unverifiable" | "malformed"));
        let is_failed_fault = doc
            .fault
            .as_deref()
            .is_some_and(|f| matches!(f, "pair-failed" | "unverifiable" | "malformed-link"));
        if is_failed_attempt || is_failed_fault {
            SessionView::Failed
        } else {
            match doc.durable.as_str() {
                "held" => SessionView::Held,
                "unpaired" => SessionView::Unpaired,
                "unavailable" => SessionView::Unavailable,
                _ => SessionView::Unknown,
            }
        }
    }
}

pub(crate) trait ManagedOwner: Send {
    fn fence_writer(&self);
    fn install_journal(&self, access: &OpenJournalAccess);
    fn arm(&self, upload: &UploadClient);
    fn shutdown(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PrivateStateError>> + Send>>;
}

pub(crate) trait OwnerStarter: Send + Sync {
    #[allow(clippy::type_complexity)]
    fn start_owner(
        &self,
        lock: PrivateStateLock,
        credential: &Credential,
        stream: &str,
        link_facts: LinkFacts,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Box<dyn ManagedOwner>, PrivateStateError>>
                + Send,
        >,
    >;
}

pub(crate) trait CeremonyClock: Send + Sync {
    fn monotonic_seconds(&self) -> f64;
    fn sleep(
        &self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    fn sleep_until(
        &self,
        deadline_seconds: f64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
}

pub(crate) struct ProductionOwner {
    pub(crate) owner: PrivateLinkOwner,
}

impl ManagedOwner for ProductionOwner {
    fn fence_writer(&self) {
        self.owner.fence_writer();
    }
    fn install_journal(&self, access: &OpenJournalAccess) {
        access.install(self.owner.open_journal_capability());
    }
    fn arm(&self, upload: &UploadClient) {
        upload.install_capability(self.owner.capability());
    }
    fn shutdown(
        self: Box<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), PrivateStateError>> + Send>>
    {
        Box::pin(async move {
            let writer = self.owner.capability().writer();
            let stopped = self.owner.shutdown().await;
            let drain = tokio::task::spawn_blocking(move || writer.wait_for_writes());
            match tokio::time::timeout(Duration::from_secs(2), drain).await {
                Ok(Ok(())) => stopped,
                _ => Err(PrivateStateError::ShutdownFailed),
            }
        })
    }
}

pub(crate) struct ProductionOwnerStarter;

impl OwnerStarter for ProductionOwnerStarter {
    fn start_owner(
        &self,
        lock: PrivateStateLock,
        credential: &Credential,
        stream: &str,
        link_facts: LinkFacts,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Box<dyn ManagedOwner>, PrivateStateError>>
                + Send,
        >,
    > {
        let cred = credential.clone();
        let stream = stream.to_string();
        Box::pin(async move {
            let owner = crate::private_link::start_private_link_owner_with_lock(
                lock, cred, &stream, link_facts,
            )
            .await?;
            Ok(Box::new(ProductionOwner { owner }) as Box<dyn ManagedOwner>)
        })
    }
}

pub(crate) struct SystemCeremonyClock;

impl CeremonyClock for SystemCeremonyClock {
    fn monotonic_seconds(&self) -> f64 {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        ORIGIN
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
    }
    fn sleep(
        &self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
    fn sleep_until(
        &self,
        deadline_seconds: f64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let now = self.monotonic_seconds();
        if deadline_seconds <= now {
            return Box::pin(std::future::ready(()));
        }
        let remaining = Duration::from_secs_f64(deadline_seconds - now);
        Box::pin(tokio::time::sleep(remaining))
    }
}

#[derive(Clone)]
struct PendingCandidate {
    attempt_id: String,
    pairing_id: String,
    credential: Credential,
    spoken_mark: String,
    expires_at: f64,
}

pub(crate) struct Phase {
    pub(crate) durable: String,
    pub(crate) owner_status: String,
    pub(crate) attempt: Option<AttemptDocument>,
    pub(crate) fault: Option<String>,
    pub(crate) restart_required: Option<bool>,
    pub(crate) open_journal: Option<String>,
    pub(crate) failed_owner_pairing_id: Option<String>,
    pub(crate) current_owner_pairing_id: Option<String>,
    pub(crate) pending_instance_id: Option<String>,
}

pub(crate) enum SupervisorCommand {
    BeginPairing {
        link: String,
        responder: oneshot::Sender<String>,
    },
    AnswerPairing {
        attempt_id: String,
        pairing_id: String,
        accept: bool,
        responder: oneshot::Sender<String>,
    },
    CancelPairing {
        attempt_id: String,
        responder: oneshot::Sender<String>,
    },
    AbandonPairing {
        attempt_id: String,
        responder: oneshot::Sender<String>,
    },
    WriteSetting {
        field: String,
        value: String,
        responder: oneshot::Sender<String>,
    },
    OpenJournal {
        opener: Option<CustomOpener>,
        responder: oneshot::Sender<String>,
    },
}

pub(crate) fn resolve_config_dir(paths: &ConfigPaths) -> PathBuf {
    crate::config::resolve_config_paths(paths).config_dir
}

pub(crate) fn resolve_config_file(paths: &ConfigPaths) -> PathBuf {
    crate::config::resolve_config_paths(paths).config_path()
}

pub fn default_settings_doc() -> SettingsDocument {
    SettingsDocument {
        capture_framerate: DEFAULT_CAPTURE_FRAMERATE,
        draw_cursor: DEFAULT_DRAW_CURSOR,
        start_paused: DEFAULT_START_PAUSED,
        segment_interval: DEFAULT_SEGMENT_INTERVAL,
        activity_source: crate::config::DEFAULT_ACTIVITY_SOURCE,
        activity_titles: crate::config::DEFAULT_ACTIVITY_TITLES,
        // Addresses wait for the owner's own address choice.
        activity_urls: false,
        activity_exclude_apps: Vec::new(),
        activity_exclude_titles: Vec::new(),
        activity_through_pause: crate::config::DEFAULT_ACTIVITY_THROUGH_PAUSE,
        activity_only: false,
    }
}

pub fn read_saved_settings(config_paths: &ConfigPaths) -> (SettingsDocument, bool) {
    let config_path = resolve_config_file(config_paths);
    let bytes = match fs::read(&config_path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return (default_settings_doc(), true);
        }
        Err(_) => {
            return (default_settings_doc(), false);
        }
    };

    let Ok(val) = serde_json::from_slice::<Value>(&bytes) else {
        return (default_settings_doc(), false);
    };

    let Some(obj) = val.as_object() else {
        return (default_settings_doc(), false);
    };

    let framerate = match obj.get("capture_framerate") {
        None => Ok(DEFAULT_CAPTURE_FRAMERATE),
        Some(Value::Number(n)) => n.as_i64().ok_or(()),
        Some(_) => Err(()),
    };
    let draw_cursor = match obj.get("draw_cursor") {
        None => Ok(DEFAULT_DRAW_CURSOR),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(()),
    };
    let start_paused = match obj.get("start_paused") {
        None => Ok(DEFAULT_START_PAUSED),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(()),
    };
    let segment_interval = match obj.get("segment_interval") {
        None => Ok(DEFAULT_SEGMENT_INTERVAL),
        Some(Value::Number(n)) => n.as_i64().ok_or(()),
        Some(_) => Err(()),
    };
    let activity_only = match obj.get("activity_only") {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(()),
    };
    // The same reading the app itself uses: defaults from code for missing keys,
    // and addresses only with the owner's address consent.
    let activity = crate::config::read_saved_activity(obj);

    match (
        framerate,
        draw_cursor,
        start_paused,
        segment_interval,
        activity,
        activity_only,
    ) {
        (Ok(f), Ok(d), Ok(p), Ok(s), Ok(activity), Ok(aonly)) => (
            SettingsDocument {
                capture_framerate: f,
                draw_cursor: d,
                start_paused: p,
                segment_interval: s,
                activity_source: activity.source,
                activity_titles: activity.titles,
                activity_urls: activity.urls,
                activity_exclude_apps: activity.exclude_apps,
                activity_exclude_titles: activity.exclude_titles,
                activity_through_pause: activity.through_pause,
                activity_only: aonly,
            },
            true,
        ),
        _ => (default_settings_doc(), false),
    }
}

fn apply_activity_status(
    phase: &mut Phase,
    fault: &Mutex<Option<Arc<crate::activity_source::ActivityFault>>>,
    capability: &Mutex<Option<Arc<Mutex<crate::activity_source::CapabilityStatus>>>>,
    route: &mut Option<RouteStatusDocument>,
) {
    if phase.fault.is_none()
        && let Some(token) = fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(|item| item.token())
    {
        phase.fault = Some(token);
    }
    if let Some(slot) = capability
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        let status = slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if status.published {
            *route = Some(RouteStatusDocument {
                route: status.route,
                foreground: status.foreground,
                title: status.title,
                url: status.url,
                idle: status.idle,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn assemble_document(
    phase: &Phase,
    health: &SyncHealth,
    snapshot: &StateSnapshot,
    config_paths: &ConfigPaths,
    restart_override: Option<bool>,
    sampler: Option<&SyncSampler>,
    effective: Option<&SettingsDocument>,
    route_status: Option<&RouteStatusDocument>,
    captures_dir: Option<&std::path::Path>,
) -> String {
    let (settings, saved_available) = read_saved_settings(config_paths);
    let capture_status = tray_model::status_name(tray_model::status(snapshot)).to_string();

    let confirmed_mark = if phase.durable == "confirmed" {
        let config_root = resolve_config_dir(config_paths);
        match load_credential(&config_root) {
            Ok(Some(cred)) => spl_core::mark::mark_from_jid(&cred.instance_id)
                .ok()
                .map(|m| m.to_render_spec().into()),
            _ => None,
        }
    } else {
        None
    };

    let pending_mark = phase.pending_instance_id.as_deref().and_then(|jid| {
        spl_core::mark::mark_from_jid(jid)
            .ok()
            .map(|m| m.to_render_spec().into())
    });

    let pause_bounded = if snapshot.paused {
        Some(snapshot.pause_until.is_some())
    } else {
        None
    };

    let capture_attention = tray_model::status(snapshot).needs_attention();

    let (queue_count, last_delivery) = match sampler {
        Some(s) => {
            let facts = s.facts();
            let q = facts.pending_confirmed;
            let l = Some(SampledInstantDocument {
                at: facts.last_successful_sync,
            });
            (q, l)
        }
        None => (None, None),
    };

    let quarantine_count = captures_dir.and_then(crate::capture_stats::strict_quarantine_count);

    let doc = ControlDocument {
        v: 1,
        durable: phase.durable.clone(),
        owner: phase.owner_status.clone(),
        attempt: phase.attempt.clone(),
        health: health.dbus.clone(),
        capture: capture_status,
        settings,
        restart_required: restart_override.or(phase.restart_required),
        open_journal: phase.open_journal.clone(),
        fault: phase.fault.clone(),
        saved_available,
        effective: effective.cloned(),
        route_status: route_status.cloned(),
        pending_mark,
        confirmed_mark,
        pause_bounded,
        capture_attention,
        queue_count,
        quarantine_count,
        last_delivery,
        daemon_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        config_path: Some(resolve_config_file(config_paths).display().to_string()),
    };
    serde_json::to_string(&doc).unwrap_or_else(|_| {
        r#"{"v":1,"durable":"unavailable","owner":"absent","health":"not-reported","capture":"","settings":{"capture_framerate":1,"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":false}"#.into()
    })
}

fn validate_exclusion_list(val: &Value) -> Result<(), &'static str> {
    let Some(arr) = val.as_array() else {
        return Err("invalid-setting");
    };
    if arr.len() > 64 {
        return Err("invalid-setting");
    }
    let mut seen = std::collections::HashSet::new();
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err("invalid-setting");
        };
        if s.is_empty() || s.len() > 256 {
            return Err("invalid-setting");
        }
        if !seen.insert(s) {
            return Err("invalid-setting");
        }
    }
    Ok(())
}

fn is_lone_activity_field(field: &str) -> bool {
    matches!(
        field,
        "activity_source"
            | "activity_titles"
            | "activity_urls"
            | "activity_exclude_apps"
            | "activity_exclude_titles"
    )
}

fn exclusion_superset(value: &Value, current: &[String]) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    let next: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
    current.iter().all(|item| next.contains(&item.as_str()))
}

/// A lone activity key may only keep the running policy or make it stricter.
/// Enabling the source, titles, or addresses, or dropping an exclusion, has to
/// arrive in one `activity_policy` so the running source never sees a gap.
fn lone_activity_write_keeps_or_tightens(
    field: &str,
    value: &Value,
    prefs: Option<&ActivityPrefs>,
) -> bool {
    let Some(prefs) = prefs else {
        return match field {
            "activity_source" | "activity_titles" | "activity_urls" => {
                value.as_bool() == Some(false)
            }
            "activity_exclude_apps" | "activity_exclude_titles" => exclusion_superset(value, &[]),
            _ => false,
        };
    };
    let snap = prefs.snapshot();
    match field {
        "activity_source" => value.as_bool().is_some_and(|next| !next || snap.source),
        "activity_titles" => value.as_bool().is_some_and(|next| !next || snap.titles),
        "activity_urls" => value.as_bool().is_some_and(|next| !next || snap.urls),
        "activity_exclude_apps" => exclusion_superset(value, &snap.exclude_apps),
        "activity_exclude_titles" => exclusion_superset(value, &snap.exclude_titles),
        _ => false,
    }
}

fn apply_live_activity_policy(supervisor: &WindowControlSupervisor, policy: ActivityPolicy) {
    if let Some(ref prefs) = supervisor.activity_prefs {
        let current = prefs.snapshot();
        let unchanged = current.source == policy.activity_source
            && current.titles == policy.activity_titles
            && current.urls == policy.activity_urls
            && current.exclude_apps == policy.activity_exclude_apps
            && current.exclude_titles == policy.activity_exclude_titles
            && current.through_pause == policy.activity_through_pause;
        if !unchanged {
            prefs.apply_policy(policy.clone());
        }
    }
    if let Ok(mut eff_lock) = supervisor.effective_settings.lock()
        && let Some(ref mut eff) = *eff_lock
    {
        eff.activity_source = policy.activity_source;
        eff.activity_titles = policy.activity_titles;
        eff.activity_urls = policy.activity_urls;
        eff.activity_exclude_apps = policy.activity_exclude_apps.clone();
        eff.activity_exclude_titles = policy.activity_exclude_titles;
        eff.activity_through_pause = policy.activity_through_pause;
    }
}

fn validate_setting(field: &str, value: &str) -> Result<Value, &'static str> {
    if value.is_empty() {
        return Err("invalid-setting");
    }
    match field {
        "capture_framerate" => {
            let n = value.trim().parse::<i64>().map_err(|_| "invalid-setting")?;
            Ok(Value::Number(n.clamp(1, 10).into()))
        }
        "draw_cursor" | "start_paused" | "activity_source" | "activity_titles"
        | "activity_urls" | "activity_only" => {
            let normalized = value.trim().to_lowercase();
            match normalized.as_str() {
                "y" | "yes" | "true" => Ok(Value::Bool(true)),
                "n" | "no" | "false" => Ok(Value::Bool(false)),
                _ => Err("invalid-setting"),
            }
        }
        "segment_interval" => {
            let n = value.trim().parse::<i64>().map_err(|_| "invalid-setting")?;
            if n <= 0 {
                return Err("invalid-setting");
            }
            Ok(Value::Number(n.into()))
        }
        "activity_exclude_apps" | "activity_exclude_titles" => {
            let val: Value = serde_json::from_str(value.trim()).map_err(|_| "invalid-setting")?;
            validate_exclusion_list(&val)?;
            Ok(val)
        }
        "activity_policy" => {
            let val: Value = serde_json::from_str(value.trim()).map_err(|_| "invalid-setting")?;
            let Some(obj) = val.as_object() else {
                return Err("invalid-setting");
            };
            // Only the choices the owner changed. Each one present must be valid.
            if obj.is_empty() {
                return Err("invalid-setting");
            }
            for (key, item) in obj {
                match key.as_str() {
                    "activity_source"
                    | "activity_titles"
                    | "activity_urls"
                    | "activity_through_pause" => {
                        if !item.is_boolean() {
                            return Err("invalid-setting");
                        }
                    }
                    "activity_exclude_apps" | "activity_exclude_titles" => {
                        validate_exclusion_list(item)?;
                    }
                    _ => return Err("invalid-setting"),
                }
            }
            Ok(val)
        }
        _ => Err("invalid-setting"),
    }
}

pub struct WindowControlSupervisor {
    cloned_lock: PrivateStateLock,
    upload: Arc<UploadClient>,
    config_paths: ConfigPaths,
    stream: String,
    transport_enabled: bool,
    open_journal: OpenJournalAccess,
    shutdown_notify: Arc<tokio::sync::Notify>,
    sync_trigger: Option<SyncTrigger>,
    sync_reset: SyncResetHandle,
    clock: Arc<dyn CeremonyClock>,
    device_label: String,
    starter: Arc<dyn OwnerStarter>,
    pairer: Arc<dyn Pairer>,
    durable_fault: Arc<dyn DurableWriteFault>,
    phase: Arc<Mutex<Phase>>,
    health: Arc<Mutex<SyncHealth>>,
    sampler: Option<SyncSampler>,
    snapshot: Arc<Mutex<StateSnapshot>>,
    effective_settings: Arc<Mutex<Option<SettingsDocument>>>,
    activity_prefs: Option<Arc<ActivityPrefs>>,
    route_status: Arc<Mutex<Option<RouteStatusDocument>>>,
    activity_fault: Arc<Mutex<Option<Arc<crate::activity_source::ActivityFault>>>>,
    activity_capability: Arc<Mutex<Option<Arc<Mutex<crate::activity_source::CapabilityStatus>>>>>,
    captures_dir: Option<PathBuf>,
    owner: Option<Box<dyn ManagedOwner>>,
    activation_blocked: bool,
    pending_candidate: Option<PendingCandidate>,
    attempt_counter: u64,
    cmd_rx: mpsc::Receiver<SupervisorCommand>,
}

impl WindowControlSupervisor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        cloned_lock: PrivateStateLock,
        upload: Arc<UploadClient>,
        config_paths: ConfigPaths,
        stream: String,
        transport_enabled: bool,
        open_journal: OpenJournalAccess,
        shutdown_notify: Arc<tokio::sync::Notify>,
        sync_trigger: Option<SyncTrigger>,
        sync_reset: SyncResetHandle,
        clock: Arc<dyn CeremonyClock>,
        device_label: String,
        starter: Arc<dyn OwnerStarter>,
        pairer: Arc<dyn Pairer>,
        durable_fault: Arc<dyn DurableWriteFault>,
        health: Arc<Mutex<SyncHealth>>,
        snapshot: Arc<Mutex<StateSnapshot>>,
        effective_settings: Arc<Mutex<Option<SettingsDocument>>>,
        captures_dir: Option<PathBuf>,
        activity_prefs: Option<Arc<ActivityPrefs>>,
        route_status: Arc<Mutex<Option<RouteStatusDocument>>>,
    ) -> (Self, WindowControlHandler) {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let sampler = Some(sync_reset.sampler_handle());
        let phase = Arc::new(Mutex::new(Phase {
            durable: "unavailable".into(),
            owner_status: "absent".into(),
            attempt: None,
            fault: None,
            restart_required: None,
            open_journal: None,
            failed_owner_pairing_id: None,
            current_owner_pairing_id: None,
            pending_instance_id: None,
        }));
        let activity_fault = Arc::new(Mutex::new(None));
        let activity_capability = Arc::new(Mutex::new(None));
        let handler = WindowControlHandler {
            inner: Arc::new(WindowControlHandlerInner {
                phase: Arc::clone(&phase),
                tx: Some(cmd_tx),
                config_paths: config_paths.clone(),
                health: Arc::clone(&health),
                sampler: sampler.clone(),
                snapshot: Arc::clone(&snapshot),
                effective_settings: Arc::clone(&effective_settings),
                route_status: Arc::clone(&route_status),
                activity_fault: Arc::clone(&activity_fault),
                activity_capability: Arc::clone(&activity_capability),
                captures_dir: captures_dir.clone(),
                custom_opener: None,
            }),
        };
        let supervisor = Self {
            cloned_lock,
            upload,
            config_paths,
            stream,
            transport_enabled,
            open_journal,
            shutdown_notify,
            sync_trigger,
            sync_reset,
            clock,
            device_label,
            starter,
            pairer,
            durable_fault,
            phase,
            health,
            sampler,
            snapshot,
            effective_settings,
            activity_prefs,
            route_status,
            activity_fault,
            activity_capability,
            captures_dir,
            owner: None,
            activation_blocked: false,
            pending_candidate: None,
            attempt_counter: 0,
            cmd_rx,
        };
        (supervisor, handler)
    }

    pub(crate) fn attach_activity_runtime(
        &self,
        fault: Arc<crate::activity_source::ActivityFault>,
        capability: Arc<Mutex<crate::activity_source::CapabilityStatus>>,
    ) {
        *self
            .activity_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fault);
        *self
            .activity_capability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(capability);
    }

    fn assemble_current(&self, restart_override: Option<bool>) -> String {
        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        let mut route = self
            .route_status
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        apply_activity_status(
            &mut phase,
            &self.activity_fault,
            &self.activity_capability,
            &mut route,
        );
        let health = self.sampler.as_ref().map_or_else(
            || {
                self.health
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            },
            SyncSampler::health,
        );
        let snapshot = self.snapshot.lock().unwrap_or_else(|p| p.into_inner());
        let eff = self.effective_settings.lock().ok().and_then(|g| g.clone());
        assemble_document(
            &phase,
            &health,
            &snapshot,
            &self.config_paths,
            restart_override,
            self.sampler.as_ref(),
            eff.as_ref(),
            route.as_ref(),
            self.captures_dir.as_deref(),
        )
    }

    fn check_durable_state(&self, candidate_pairing_id: Option<&str>) -> String {
        let config_root = resolve_config_dir(&self.config_paths);
        if pairing_commit_pending(&config_root) {
            return "held".into();
        }
        let cred_opt = crate::private_link::load_credential(&config_root)
            .ok()
            .flatten();
        if let Some(ref cred) = cred_opt {
            let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);
            if is_pairing_confirmed(&config_root, &pairing_id) {
                return "confirmed".to_string();
            }
        }
        let answer = read_pairing_answer(&config_root).ok().flatten();
        if let Some(ref ans) = answer {
            if !ans.confirmed.is_empty() && candidate_pairing_id != Some(ans.confirmed.as_str()) {
                "held".to_string()
            } else if ans.confirmed.is_empty() {
                "unpaired".to_string()
            } else {
                "held".to_string()
            }
        } else if cred_opt.is_some() || config_root.join(CREDENTIALS_FILENAME).exists() {
            "held".to_string()
        } else {
            "unpaired".to_string()
        }
    }

    pub(crate) async fn run(mut self) -> Result<(), PrivateStateError> {
        if !self.transport_enabled {
            self.upload
                .publish_link_fact(LinkFact::ConfigSanitationFailed);
            return Err(PrivateStateError::BridgeUnavailable);
        }

        self.upload.prepare_new_owner();

        let config_root = resolve_config_dir(&self.config_paths);
        {
            let _answer_lock = crate::journal_mark::AnswerLock::acquire(&config_root).await?;
            let _ = crate::journal_mark::grandfather_answer_file_with_fault(
                &config_root,
                None,
                self.durable_fault.as_ref(),
            );
        }

        let cred_opt = crate::private_link::load_credential(&config_root)
            .ok()
            .flatten();
        let is_confirmed = if let Some(ref cred) = cred_opt {
            let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);
            is_pairing_confirmed(&config_root, &pairing_id)
        } else {
            false
        };
        if is_confirmed {
            if let Some(cred) = cred_opt {
                let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);
                let lock_clone = self
                    .cloned_lock
                    .try_clone()
                    .map_err(|_| PrivateStateError::LockContended)?;
                match self
                    .starter
                    .start_owner(lock_clone, &cred, &self.stream, self.upload.link_facts())
                    .await
                {
                    Ok(owner) => {
                        owner.install_journal(&self.open_journal);
                        owner.arm(&self.upload);
                        if let Some(ref trigger) = self.sync_trigger {
                            trigger.trigger();
                        }
                        self.owner = Some(owner);
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.durable = "confirmed".into();
                        phase.owner_status = "running".into();
                        phase.current_owner_pairing_id = Some(pairing_id);
                        phase.failed_owner_pairing_id = None;
                    }
                    Err(_) => {
                        self.upload
                            .publish_link_fact(LinkFact::TransportUnavailable);
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.durable = "confirmed".into();
                        phase.owner_status = "failed".into();
                        phase.fault = Some("owner-start-failed".into());
                        phase.failed_owner_pairing_id = Some(pairing_id);
                    }
                }
            } else {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.durable = "confirmed".into();
                phase.owner_status = "failed".into();
            }
        } else {
            let durable_state = self.check_durable_state(None);
            if durable_state == "held" {
                self.upload.link_facts().set_journal_mark_held(true);
            } else {
                self.upload.link_facts().set_journal_mark_held(false);
                self.upload.publish_link_fact(LinkFact::PairingRequired);
            }
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.durable = durable_state;
            phase.owner_status = "absent".into();
        }

        loop {
            self.check_expiry().await;
            let sleep_future = self.clock.sleep(JOURNAL_MARK_RECHECK_INTERVAL);
            tokio::select! {
                _ = self.shutdown_notify.notified() => {
                    if let Some(cand) = self.pending_candidate.take() {
                        retire_client_registration(&cand.credential).await;
                    }
                    if let Some(owner) = self.owner.take() {
                        owner.fence_writer();
                        owner.shutdown().await?;
                    }
                    return Ok(());
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        if let Some(cand) = self.pending_candidate.take() {
                            retire_client_registration(&cand.credential).await;
                        }
                        if let Some(owner) = self.owner.take() {
                            owner.fence_writer();
                            owner.shutdown().await?;
                        }
                        return Ok(());
                    };
                    self.handle_command(cmd).await;
                }
                _ = sleep_future => {
                    self.check_expiry().await;
                    self.recheck_cli_confirm().await;
                }
            }
        }
    }

    async fn check_expiry(&mut self) {
        if let Some(ref cand) = self.pending_candidate
            && self.clock.monotonic_seconds() >= cand.expires_at
        {
            let cand = self.pending_candidate.take().unwrap();
            retire_client_registration(&cand.credential).await;
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.pending_instance_id = None;
            phase.attempt = Some(AttemptDocument {
                id: cand.attempt_id,
                pairing_id: cand.pairing_id,
                state: "expired".into(),
                mark: None,
            });
        }
    }

    async fn recheck_cli_confirm(&mut self) {
        if self.owner.is_some() || self.activation_blocked {
            return;
        }
        let config_root = resolve_config_dir(&self.config_paths);
        let cred_opt = crate::private_link::load_credential(&config_root)
            .ok()
            .flatten();
        if let Some(cred) = cred_opt {
            let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);
            if is_pairing_confirmed(&config_root, &pairing_id) {
                let already_failed = {
                    let phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.failed_owner_pairing_id.as_deref() == Some(&pairing_id)
                };
                if already_failed {
                    return;
                }
                self.upload.prepare_new_owner();
                self.sync_reset.reset_association_blockers();
                let lock_res = self.cloned_lock.try_clone();
                let Ok(lock_clone) = lock_res else { return };
                match self
                    .starter
                    .start_owner(lock_clone, &cred, &self.stream, self.upload.link_facts())
                    .await
                {
                    Ok(owner) => {
                        owner.install_journal(&self.open_journal);
                        owner.arm(&self.upload);
                        if let Some(ref trigger) = self.sync_trigger {
                            trigger.trigger();
                        }
                        self.owner = Some(owner);
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.durable = "confirmed".into();
                        phase.owner_status = "running".into();
                        phase.current_owner_pairing_id = Some(pairing_id);
                        phase.failed_owner_pairing_id = None;
                        phase.fault = None;
                    }
                    Err(_) => {
                        self.upload
                            .publish_link_fact(LinkFact::TransportUnavailable);
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.durable = "confirmed".into();
                        phase.owner_status = "failed".into();
                        phase.fault = Some("owner-start-failed".into());
                        phase.failed_owner_pairing_id = Some(pairing_id);
                    }
                }
            }
        }
    }

    async fn handle_command(&mut self, cmd: SupervisorCommand) {
        match cmd {
            SupervisorCommand::BeginPairing { link, responder } => {
                let doc = self.handle_begin_pairing(link).await;
                let _ = responder.send(doc);
            }
            SupervisorCommand::AnswerPairing {
                attempt_id,
                pairing_id,
                accept,
                responder,
            } => {
                let doc = self
                    .handle_answer_pairing(attempt_id, pairing_id, accept)
                    .await;
                let _ = responder.send(doc);
            }
            SupervisorCommand::CancelPairing {
                attempt_id,
                responder,
            } => {
                let doc = self.handle_cancel_pairing(attempt_id).await;
                let _ = responder.send(doc);
            }
            SupervisorCommand::AbandonPairing {
                attempt_id,
                responder,
            } => {
                let doc = self.handle_abandon_pairing(attempt_id).await;
                let _ = responder.send(doc);
            }
            SupervisorCommand::WriteSetting {
                field,
                value,
                responder,
            } => {
                let doc = self.handle_write_setting(field, value).await;
                let _ = responder.send(doc);
            }
            SupervisorCommand::OpenJournal { opener, responder } => {
                let doc = self.handle_open_journal(opener).await;
                let _ = responder.send(doc);
            }
        }
    }

    async fn handle_begin_pairing(&mut self, link: String) -> String {
        if let Some(cand) = self.pending_candidate.take() {
            retire_client_registration(&cand.credential).await;
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.pending_instance_id = None;
        }

        self.attempt_counter += 1;
        let attempt_id = self.attempt_counter.to_string();

        let link = match adapt_graphical_link(&link) {
            Ok(adapted) => adapted,
            Err(_) => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.attempt = Some(AttemptDocument {
                        id: attempt_id,
                        pairing_id: String::new(),
                        state: "malformed".into(),
                        mark: None,
                    });
                    phase.fault = Some("malformed-link".into());
                }
                return self.assemble_current(None);
            }
        };

        let _parsed_link = match pairlink::parse(&link) {
            Ok(p) => p,
            Err(_) => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.attempt = Some(AttemptDocument {
                        id: attempt_id,
                        pairing_id: String::new(),
                        state: "malformed".into(),
                        mark: None,
                    });
                    phase.fault = Some("malformed-link".into());
                }
                return self.assemble_current(None);
            }
        };

        let deadline = self.clock.monotonic_seconds() + PAIR_BOUND.as_secs_f64();
        let pair_future = pair_and_admit(self.pairer.as_ref(), &link, &self.device_label);
        let sleep_future = self.clock.sleep_until(deadline);

        tokio::pin!(pair_future);
        tokio::pin!(sleep_future);

        let pair_result = tokio::select! {
            res = &mut pair_future => Some(res),
            _ = &mut sleep_future => None,
        };

        let credential = match pair_result {
            Some(Ok(cred)) => cred,
            Some(Err(_)) => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.attempt = Some(AttemptDocument {
                        id: attempt_id,
                        pairing_id: String::new(),
                        state: "failed".into(),
                        mark: None,
                    });
                    phase.fault = Some("pair-failed".into());
                }
                return self.assemble_current(None);
            }
            None => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.attempt = Some(AttemptDocument {
                        id: attempt_id,
                        pairing_id: String::new(),
                        state: "failed".into(),
                        mark: None,
                    });
                    phase.fault = Some("pair-failed".into());
                }
                return self.assemble_current(None);
            }
        };

        let pairing_id = crate::private_link::compute_pairing_id(&credential.client_cert_pem);

        let spoken_mark_str = match format_spoken_mark(&credential.instance_id) {
            Some(mark) => mark,
            None => {
                retire_client_registration(&credential).await;
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.attempt = Some(AttemptDocument {
                        id: attempt_id,
                        pairing_id,
                        state: "unverifiable".into(),
                        mark: None,
                    });
                    phase.fault = Some("unverifiable".into());
                }
                return self.assemble_current(None);
            }
        };

        let expires_at = self.clock.monotonic_seconds() + CEREMONY_TTL.as_secs_f64();

        self.pending_candidate = Some(PendingCandidate {
            attempt_id: attempt_id.clone(),
            pairing_id: pairing_id.clone(),
            credential: credential.clone(),
            spoken_mark: spoken_mark_str.clone(),
            expires_at,
        });

        {
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.pending_instance_id = Some(credential.instance_id.clone());
            phase.attempt = Some(AttemptDocument {
                id: attempt_id,
                pairing_id,
                state: "pending".into(),
                mark: Some(spoken_mark_str),
            });
            phase.fault = None;
        }

        self.assemble_current(None)
    }

    async fn handle_answer_pairing(
        &mut self,
        attempt_id: String,
        pairing_id: String,
        accept: bool,
    ) -> String {
        self.check_expiry().await;
        let Some(candidate) = self.pending_candidate.clone() else {
            return self.assemble_current(None);
        };

        if candidate.attempt_id != attempt_id || candidate.pairing_id != pairing_id {
            // Refuse the old answer without destroying the newer comparison.
            let mut doc: ControlDocument = serde_json::from_str(&self.assemble_current(None))
                .expect("serialized control document");
            doc.fault = Some("stale-answer".into());
            return serde_json::to_string(&doc).expect("control document serialization");
        }

        if !accept {
            self.pending_candidate = None;
            retire_client_registration(&candidate.credential).await;
            {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.pending_instance_id = None;
                phase.attempt = Some(AttemptDocument {
                    id: candidate.attempt_id,
                    pairing_id: candidate.pairing_id,
                    state: "rejected".into(),
                    mark: None,
                });
                phase.fault = None;
            }
            return self.assemble_current(None);
        }

        let verified = match format_spoken_mark(&candidate.credential.instance_id) {
            Some(ref mark) => mark == &candidate.spoken_mark,
            None => false,
        };

        if !verified {
            self.pending_candidate = None;
            retire_client_registration(&candidate.credential).await;
            {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.pending_instance_id = None;
                phase.attempt = Some(AttemptDocument {
                    id: candidate.attempt_id,
                    pairing_id: candidate.pairing_id,
                    state: "unverifiable".into(),
                    mark: None,
                });
                phase.fault = Some("unverifiable".into());
            }
            return self.assemble_current(None);
        }

        if let Some(old) = self.owner.take() {
            old.fence_writer();
            if old.shutdown().await.is_err() {
                self.activation_blocked = true;
            }
        }
        if self.activation_blocked {
            self.pending_candidate = None;
            retire_client_registration(&candidate.credential).await;
            {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.pending_instance_id = None;
                phase.owner_status = "failed".into();
                phase.fault = Some("owner-stop-failed".into());
                phase.attempt = None;
            }
            return self.assemble_current(None);
        }

        self.upload.prepare_new_owner();
        let config_root = resolve_config_dir(&self.config_paths);
        let fault = self.durable_fault.clone();
        let cand = candidate.clone();

        let commit_result = async {
            let _answer_lock = crate::journal_mark::AnswerLock::acquire(&config_root).await?;
            // Refuse unreadable/unsafe previous state rather than treating it as absent.
            read_private_file(
                &config_root.join(CREDENTIALS_FILENAME),
                PrivateTargetKind::Credential,
            )?;
            read_private_file(
                &config_root.join(PAIRING_ANSWER_FILENAME),
                PrivateTargetKind::PairingAnswer,
            )?;
            // No rollback can be guaranteed after an I/O error. Leave this durable fence
            // through every partial write; a fresh explicit confirmation can finish it.
            begin_pairing_commit(&config_root, &cand.pairing_id, fault.as_ref())?;
            persist_credential_with_fault(&config_root, &cand.credential, fault.as_ref())?;
            write_pairing_answer_with_fault(&config_root, &cand.pairing_id, fault.as_ref())?;
            finish_pairing_commit_with_fault(&config_root, &cand.pairing_id, fault.as_ref())
        }
        .await;

        if let Err(error) = commit_result {
            let uncertain = matches!(error, PrivateStateError::PairingCommitUncertain);
            let durable_state = if uncertain {
                "unavailable".into()
            } else {
                self.check_durable_state(Some(&cand.pairing_id))
            };
            self.upload
                .link_facts()
                .set_journal_mark_held(durable_state == "held");
            self.pending_candidate = None;
            {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.pending_instance_id = None;
                phase.durable = durable_state;
                phase.owner_status = "absent".into();
                phase.fault = Some(
                    if uncertain {
                        "commit-outcome-unknown"
                    } else {
                        "commit-failed"
                    }
                    .into(),
                );
                if uncertain {
                    phase.failed_owner_pairing_id = Some(cand.pairing_id.clone());
                }
                phase.attempt = Some(AttemptDocument {
                    id: cand.attempt_id,
                    pairing_id: cand.pairing_id,
                    state: if uncertain {
                        "outcome-unknown"
                    } else {
                        "failed"
                    }
                    .into(),
                    mark: None,
                });
            }
            return self.assemble_current(None);
        }

        self.sync_reset.reset_association_blockers();

        let lock_clone = match self.cloned_lock.try_clone() {
            Ok(l) => l,
            Err(_) => {
                self.pending_candidate = None;
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.durable = "confirmed".into();
                    phase.owner_status = "failed".into();
                    phase.failed_owner_pairing_id = Some(cand.pairing_id.clone());
                    phase.fault = Some("owner-start-failed".into());
                    phase.attempt = None;
                }
                return self.assemble_current(None);
            }
        };

        match self
            .starter
            .start_owner(
                lock_clone,
                &cand.credential,
                &self.stream,
                self.upload.link_facts(),
            )
            .await
        {
            Ok(owner) => {
                owner.install_journal(&self.open_journal);
                owner.arm(&self.upload);
                if let Some(ref trigger) = self.sync_trigger {
                    trigger.trigger();
                }
                self.owner = Some(owner);
                self.pending_candidate = None;
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.durable = "confirmed".into();
                    phase.owner_status = "running".into();
                    phase.current_owner_pairing_id = Some(cand.pairing_id.clone());
                    phase.failed_owner_pairing_id = None;
                    phase.fault = None;
                    phase.attempt = None;
                }
            }
            Err(_) => {
                self.upload
                    .publish_link_fact(LinkFact::TransportUnavailable);
                self.pending_candidate = None;
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.pending_instance_id = None;
                    phase.durable = "confirmed".into();
                    phase.owner_status = "failed".into();
                    phase.failed_owner_pairing_id = Some(cand.pairing_id.clone());
                    phase.fault = Some("owner-start-failed".into());
                    phase.attempt = None;
                }
            }
        }

        self.assemble_current(None)
    }

    async fn handle_cancel_pairing(&mut self, attempt_id: String) -> String {
        if let Some(candidate) = self.pending_candidate.clone()
            && candidate.attempt_id == attempt_id
        {
            self.pending_candidate = None;
            retire_client_registration(&candidate.credential).await;
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.pending_instance_id = None;
            phase.attempt = Some(AttemptDocument {
                id: candidate.attempt_id,
                pairing_id: candidate.pairing_id,
                state: "cancelled".into(),
                mark: None,
            });
            phase.fault = None;
        }
        self.assemble_current(None)
    }

    async fn handle_abandon_pairing(&mut self, attempt_id: String) -> String {
        if let Some(candidate) = self.pending_candidate.clone()
            && candidate.attempt_id == attempt_id
        {
            self.pending_candidate = None;
            retire_client_registration(&candidate.credential).await;
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.pending_instance_id = None;
            phase.attempt = Some(AttemptDocument {
                id: candidate.attempt_id,
                pairing_id: candidate.pairing_id,
                state: "abandoned".into(),
                mark: None,
            });
            phase.fault = None;
        }
        self.assemble_current(None)
    }

    async fn handle_write_setting(&mut self, field: String, value: String) -> String {
        let normalized = match validate_setting(&field, &value) {
            Ok(v) => v,
            Err(fault) => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.fault = Some(fault.into());
                }
                return self.assemble_current(None);
            }
        };

        if is_lone_activity_field(&field)
            && !lone_activity_write_keeps_or_tightens(
                &field,
                &normalized,
                self.activity_prefs.as_deref(),
            )
        {
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.fault = Some("invalid-setting".into());
            drop(phase);
            return self.assemble_current(None);
        }

        let result = set_guided_field(&self.config_paths, &field, normalized.clone());
        match result {
            Ok(saved) => {
                if field == "activity_policy" || is_lone_activity_field(&field) {
                    // The saved file, read back with its defaults, is the policy.
                    apply_live_activity_policy(self, ActivityPolicy::from_config(&saved));
                    {
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.fault = None;
                    }
                    self.assemble_current(None)
                } else {
                    {
                        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                        phase.restart_required = Some(true);
                        phase.fault = None;
                    }
                    self.assemble_current(Some(true))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.fault = Some("invalid-config".into());
                }
                self.assemble_current(None)
            }
            Err(_) => {
                {
                    let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                    phase.fault = Some("invalid-setting".into());
                }
                self.assemble_current(None)
            }
        }
    }

    async fn handle_open_journal(&mut self, opener: Option<CustomOpener>) -> String {
        let is_confirmed = {
            let phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.durable == "confirmed"
        };
        if !is_confirmed {
            {
                let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
                phase.open_journal = Some("not-confirmed".into());
            }
            return self.assemble_current(None);
        }
        let open_result = match opener {
            Some(opener) => self.open_journal.open_with(|url| opener(url)),
            None => self.open_journal.open(),
        };
        if open_result.is_err() {
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.open_journal = Some("unavailable".into());
        } else {
            let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
            phase.open_journal = Some("opened".into());
        }
        self.assemble_current(None)
    }
}

#[allow(dead_code)]
pub type CustomOpener = Arc<dyn Fn(&str) -> Result<(), ()> + Send + Sync>;

struct WindowControlHandlerInner {
    phase: Arc<Mutex<Phase>>,
    tx: Option<mpsc::Sender<SupervisorCommand>>,
    config_paths: ConfigPaths,
    health: Arc<Mutex<SyncHealth>>,
    sampler: Option<SyncSampler>,
    snapshot: Arc<Mutex<StateSnapshot>>,
    effective_settings: Arc<Mutex<Option<SettingsDocument>>>,
    route_status: Arc<Mutex<Option<RouteStatusDocument>>>,
    activity_fault: Arc<Mutex<Option<Arc<crate::activity_source::ActivityFault>>>>,
    activity_capability: Arc<Mutex<Option<Arc<Mutex<crate::activity_source::CapabilityStatus>>>>>,
    captures_dir: Option<PathBuf>,
    custom_opener: Option<CustomOpener>,
}

#[derive(Clone)]
pub struct WindowControlHandler {
    inner: Arc<WindowControlHandlerInner>,
}

impl Default for WindowControlHandler {
    fn default() -> Self {
        let paths = ConfigPaths::default();
        let phase = Arc::new(Mutex::new(Phase {
            durable: "unavailable".into(),
            owner_status: "absent".into(),
            attempt: None,
            fault: None,
            restart_required: None,
            open_journal: None,
            failed_owner_pairing_id: None,
            current_owner_pairing_id: None,
            pending_instance_id: None,
        }));
        Self {
            inner: Arc::new(WindowControlHandlerInner {
                phase,
                tx: None,
                config_paths: paths,
                health: Arc::new(Mutex::new(crate::sync_health::derive_health(
                    &Default::default(),
                    0.0,
                    600.0,
                ))),
                sampler: None,
                snapshot: Arc::new(Mutex::new(StateSnapshot {
                    mode: Mode::Idle,
                    paused: false,
                    segment_open: false,
                    captures_today: 0,
                    total_size_mb: 0,
                    pause_until: None,
                    segment_start_mono: None,
                    process_start_mono: 0.0,
                    session: Default::default(),
                })),
                effective_settings: Arc::new(Mutex::new(None)),
                route_status: Arc::new(Mutex::new(None)),
                activity_fault: Arc::new(Mutex::new(None)),
                activity_capability: Arc::new(Mutex::new(None)),
                captures_dir: None,
                custom_opener: None,
            }),
        }
    }
}

impl WindowControlHandler {
    pub fn with_custom_opener(&self, opener: CustomOpener) -> Self {
        Self {
            inner: Arc::new(WindowControlHandlerInner {
                phase: Arc::clone(&self.inner.phase),
                tx: self.inner.tx.clone(),
                config_paths: self.inner.config_paths.clone(),
                health: Arc::clone(&self.inner.health),
                sampler: self.inner.sampler.clone(),
                snapshot: Arc::clone(&self.inner.snapshot),
                effective_settings: Arc::clone(&self.inner.effective_settings),
                route_status: Arc::clone(&self.inner.route_status),
                activity_fault: Arc::clone(&self.inner.activity_fault),
                activity_capability: Arc::clone(&self.inner.activity_capability),
                captures_dir: self.inner.captures_dir.clone(),
                custom_opener: Some(opener),
            }),
        }
    }

    pub fn attach_activity_runtime(
        &self,
        fault: Arc<crate::activity_source::ActivityFault>,
        capability: Arc<Mutex<crate::activity_source::CapabilityStatus>>,
    ) {
        *self
            .inner
            .activity_fault
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fault);
        *self
            .inner
            .activity_capability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(capability);
    }

    pub fn get_control_state(&self) -> String {
        let mut phase = self.inner.phase.lock().unwrap_or_else(|p| p.into_inner());
        let mut route = self
            .inner
            .route_status
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        apply_activity_status(
            &mut phase,
            &self.inner.activity_fault,
            &self.inner.activity_capability,
            &mut route,
        );
        let health = self.inner.sampler.as_ref().map_or_else(
            || {
                self.inner
                    .health
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            },
            SyncSampler::health,
        );
        let snapshot = self
            .inner
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let eff = self
            .inner
            .effective_settings
            .lock()
            .ok()
            .and_then(|g| g.clone());
        assemble_document(
            &phase,
            &health,
            &snapshot,
            &self.inner.config_paths,
            None,
            self.inner.sampler.as_ref(),
            eff.as_ref(),
            route.as_ref(),
            self.inner.captures_dir.as_deref(),
        )
    }

    pub async fn begin_pairing(&self, link: String) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::BeginPairing {
                link,
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    pub async fn answer_pairing(
        &self,
        attempt_id: String,
        pairing_id: String,
        accept: bool,
    ) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::AnswerPairing {
                attempt_id,
                pairing_id,
                accept,
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    pub async fn cancel_pairing(&self, attempt_id: String) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::CancelPairing {
                attempt_id,
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    pub async fn abandon_pairing(&self, attempt_id: String) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::AbandonPairing {
                attempt_id,
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    pub async fn write_setting(&self, field: String, value: String) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::WriteSetting {
                field,
                value,
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    pub async fn open_journal(&self) -> String {
        let Some(ref tx) = self.inner.tx else {
            return self.get_control_state();
        };
        let (resp_tx, resp_rx) = oneshot::channel();
        if tx
            .send(SupervisorCommand::OpenJournal {
                opener: self.inner.custom_opener.clone(),
                responder: resp_tx,
            })
            .await
            .is_err()
        {
            return self.get_control_state();
        }
        resp_rx
            .await
            .unwrap_or_else(|_| self.fallback_unavailable())
    }

    fn fallback_unavailable(&self) -> String {
        "{\"v\":1,\"durable\":\"unavailable\",\"owner\":\"absent\",\"attempt\":null,\"health\":\"not-reported\",\"capture\":\"\",\"settings\":{\"capture_framerate\":1,\"draw_cursor\":true,\"start_paused\":false,\"segment_interval\":300},\"restart_required\":null,\"open_journal\":null,\"fault\":null}".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::tempdir;

    use crate::config::ConfigPaths;
    use crate::journal_mark::{
        PAIRING_ANSWER_FILENAME, is_pairing_confirmed, read_pairing_answer, sample_credential,
    };
    use crate::observer::{Mode, StateSnapshot};
    use crate::private_file::{DurableWriteFault, DurableWriteStage, NoWriteFault};
    use crate::private_link::{
        CREDENTIALS_FILENAME, LinkFact, LinkFacts, OpenJournalAccess, OpenJournalCapability,
        Pairer, PrivateLinkCapability, PrivateStateError, PrivateStateLock, compute_pairing_id,
        open_journal_fixture,
    };
    use crate::run::SystemClock;
    use crate::sync::{SyncResetHandle, SyncSampler};
    use crate::sync_health::{SyncFacts, SyncHealth};
    use crate::upload::UploadClient;
    use spl_transport::credential::EndpointAddr;

    struct ManualClockInner {
        monotonic_seconds: f64,
        sleepers: Vec<(f64, oneshot::Sender<()>)>,
    }

    #[derive(Clone)]
    pub struct ManualClock {
        inner: Arc<Mutex<ManualClockInner>>,
    }

    impl ManualClock {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(ManualClockInner {
                    monotonic_seconds: 1000.0,
                    sleepers: Vec::new(),
                })),
            }
        }

        pub fn pending_sleeps(&self) -> usize {
            let mut inner = self.inner.lock().unwrap();
            inner.sleepers.retain(|(_, tx)| !tx.is_closed());
            inner
                .sleepers
                .iter()
                .filter(|(deadline, _)| {
                    *deadline <= inner.monotonic_seconds + PAIR_BOUND.as_secs_f64() + 0.1
                })
                .count()
        }

        pub fn advance(&self, duration: Duration) {
            let mut inner = self.inner.lock().unwrap();
            inner.monotonic_seconds += duration.as_secs_f64();
            let current = inner.monotonic_seconds;
            let mut remaining = Vec::new();
            for (target, tx) in inner.sleepers.drain(..) {
                if target <= current {
                    let _ = tx.send(());
                } else if !tx.is_closed() {
                    remaining.push((target, tx));
                }
            }
            inner.sleepers = remaining;
        }

        pub async fn advance_and_yield(&self, duration: Duration) {
            self.advance(duration);
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
        }
    }

    impl CeremonyClock for ManualClock {
        fn monotonic_seconds(&self) -> f64 {
            self.inner.lock().unwrap().monotonic_seconds
        }

        fn sleep(
            &self,
            duration: Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            let target = self.monotonic_seconds() + duration.as_secs_f64();
            self.sleep_until(target)
        }

        fn sleep_until(
            &self,
            deadline_seconds: f64,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            let mut inner = self.inner.lock().unwrap();
            let current = inner.monotonic_seconds;
            if deadline_seconds <= current {
                return Box::pin(std::future::ready(()));
            }
            let (tx, rx) = oneshot::channel();
            inner.sleepers.push((deadline_seconds, tx));
            Box::pin(async move {
                let _ = rx.await;
            })
        }
    }

    pub struct FakePairer {
        pub result: Arc<Mutex<Option<Credential>>>,
        pub error: Arc<Mutex<Option<PrivateStateError>>>,
        pub calls: Arc<AtomicUsize>,
        pub hang: Arc<AtomicBool>,
    }

    impl FakePairer {
        pub fn new(cred: Credential) -> Self {
            Self {
                result: Arc::new(Mutex::new(Some(cred))),
                error: Arc::new(Mutex::new(None)),
                calls: Arc::new(AtomicUsize::new(0)),
                hang: Arc::new(AtomicBool::new(false)),
            }
        }

        pub fn hanging() -> Self {
            Self {
                result: Arc::new(Mutex::new(None)),
                error: Arc::new(Mutex::new(None)),
                calls: Arc::new(AtomicUsize::new(0)),
                hang: Arc::new(AtomicBool::new(true)),
            }
        }
    }

    impl Pairer for FakePairer {
        fn pair<'a>(
            &'a self,
            _link: &'a str,
            _device_label: &'a str,
            _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Credential, PrivateStateError>> + Send + 'a,
            >,
        > {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let hang = self.hang.load(Ordering::SeqCst);
            let cred = self.result.lock().unwrap().clone();
            let is_err = self.error.lock().unwrap().is_some();
            Box::pin(async move {
                if hang {
                    std::future::pending::<()>().await;
                }
                if is_err {
                    return Err(PrivateStateError::PairingFailed);
                }
                cred.ok_or(PrivateStateError::PairingFailed)
            })
        }
    }

    #[allow(dead_code)]
    pub struct FakeOwnerState {
        pub fenced: Arc<AtomicBool>,
        pub shutdown: Arc<AtomicBool>,
        pub shutdown_fails: Arc<AtomicBool>,
        pub credential: Credential,
        pub link_facts: LinkFacts,
    }

    pub struct FakeOwner {
        pub state: Arc<FakeOwnerState>,
        pub open_journal_cap: Option<OpenJournalCapability>,
    }

    impl ManagedOwner for FakeOwner {
        fn fence_writer(&self) {
            self.state.fenced.store(true, Ordering::SeqCst);
        }

        fn install_journal(&self, access: &OpenJournalAccess) {
            if let Some(ref cap) = self.open_journal_cap {
                access.install(cap.clone());
            }
        }

        fn arm(&self, _upload: &UploadClient) {
            self.state.link_facts.publish(LinkFact::ListenerReady);
        }

        fn shutdown(
            self: Box<Self>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), PrivateStateError>> + Send>,
        > {
            self.state.shutdown.store(true, Ordering::SeqCst);
            Box::pin(async move {
                if self.state.shutdown_fails.load(Ordering::SeqCst) {
                    Err(PrivateStateError::ShutdownFailed)
                } else {
                    Ok(())
                }
            })
        }
    }

    #[derive(Clone, Default)]
    pub struct FakeOwnerStarter {
        pub should_fail: Arc<AtomicBool>,
        pub starts: Arc<AtomicUsize>,
        pub recorded_link_facts: Arc<Mutex<Vec<LinkFacts>>>,
        pub states: Arc<Mutex<Vec<Arc<FakeOwnerState>>>>,
        pub open_journal_cap: Arc<Mutex<Option<OpenJournalCapability>>>,
    }

    impl OwnerStarter for FakeOwnerStarter {
        fn start_owner(
            &self,
            _lock: PrivateStateLock,
            credential: &Credential,
            _stream: &str,
            link_facts: LinkFacts,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Box<dyn ManagedOwner>, PrivateStateError>>
                    + Send,
            >,
        > {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.should_fail.load(Ordering::SeqCst) {
                return Box::pin(async move { Err(PrivateStateError::BridgeUnavailable) });
            }
            let state = Arc::new(FakeOwnerState {
                fenced: Arc::new(AtomicBool::new(false)),
                shutdown: Arc::new(AtomicBool::new(false)),
                shutdown_fails: Arc::new(AtomicBool::new(false)),
                credential: credential.clone(),
                link_facts: link_facts.clone(),
            });
            self.states.lock().unwrap().push(Arc::clone(&state));
            self.recorded_link_facts.lock().unwrap().push(link_facts);
            let cap = self.open_journal_cap.lock().unwrap().clone();
            Box::pin(async move {
                Ok(Box::new(FakeOwner {
                    state,
                    open_journal_cap: cap,
                }) as Box<dyn ManagedOwner>)
            })
        }
    }

    pub struct FailWriteFault;
    impl DurableWriteFault for FailWriteFault {
        fn before(&self, _stage: DurableWriteStage) -> io::Result<()> {
            Err(io::Error::other("injected durable write fault"))
        }
    }

    fn test_snapshot(mode: Mode, paused: bool) -> StateSnapshot {
        StateSnapshot {
            mode,
            paused,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
            session: Default::default(),
        }
    }

    fn sample_valid_credential(cert_pem: &str) -> Credential {
        sample_credential("01234567-89ab-cdef-0123-456789abcdef", cert_pem)
    }

    fn sample_tls_credential() -> Credential {
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
                    instance_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
                    home_label: "home".into(),
                    endpoints: vec![EndpointAddr {
                        host: "127.0.0.1".into(),
                        port: 7657,
                    }],
                    home_attestation: Some("attestation".into()),
                    local_endpoints: Some(serde_json::json!([{"host":"127.0.0.1","port":7657}])),
                    relay_origin: None,
                    device_token: None,
                    device_token_expires_at: None,
                }
            })
            .clone()
    }

    const TEST_PAIR_LINK: &str = "https://go.solstone.app/p#0G0QY00004EYJ001081G81860W40J2GB1G6GW3X0M6HA7955MTKTHADANEPAVBNF";

    fn relay_pair_link_for(origin: &str) -> String {
        assert!(u8::try_from(origin.len()).is_ok());
        let mut blob = vec![0x06];
        blob.extend_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        blob.push(0x01);
        blob.extend_from_slice(&[0xde; 16]);
        blob.push(origin.len() as u8);
        blob.extend_from_slice(origin.as_bytes());
        format!(
            "https://go.solstone.app/p#{}",
            spl_core::crockford::encode(&blob)
        )
    }

    #[allow(dead_code)]
    struct TestHarness {
        _temp_dir: tempfile::TempDir,
        config_paths: ConfigPaths,
        config_root: PathBuf,
        state_dir: PathBuf,
        _daemon_lock: PrivateStateLock,
        clock: ManualClock,
        pairer: Arc<FakePairer>,
        starter: Arc<FakeOwnerStarter>,
        health: Arc<Mutex<SyncHealth>>,
        snapshot: Arc<Mutex<StateSnapshot>>,
        upload: Arc<UploadClient>,
        sync_reset: SyncResetHandle,
        sync_facts: Arc<Mutex<SyncFacts>>,
        shutdown_notify: Arc<tokio::sync::Notify>,
        open_journal: OpenJournalAccess,
        activity_prefs: Arc<ActivityPrefs>,
        effective_settings: Arc<Mutex<Option<SettingsDocument>>>,
        handler: WindowControlHandler,
        supervisor_task: tokio::task::JoinHandle<Result<(), PrivateStateError>>,
    }

    impl TestHarness {
        async fn new(
            initial_cred: Option<Credential>,
            initial_confirmed: bool,
            pairer: Arc<FakePairer>,
            starter: Arc<FakeOwnerStarter>,
            fault: Arc<dyn DurableWriteFault>,
        ) -> Self {
            let temp_dir = tempfile::tempdir().unwrap();
            let config_root = temp_dir.path().join("config");
            let state_dir = temp_dir.path().join("state");
            fs::create_dir_all(&config_root).unwrap();
            fs::create_dir_all(&state_dir).unwrap();

            if let Some(ref cred) = initial_cred {
                let _ = crate::private_link::persist_credential_with_fault(
                    &config_root,
                    cred,
                    &NoWriteFault,
                );
                let pairing_id = compute_pairing_id(&cred.client_cert_pem);
                let confirmed_id = if initial_confirmed {
                    pairing_id.as_str()
                } else {
                    ""
                };
                let _ = crate::journal_mark::write_pairing_answer_with_fault(
                    &config_root,
                    confirmed_id,
                    &NoWriteFault,
                );
            }

            let config_paths = ConfigPaths {
                config_dir: Some(config_root.clone()),
                base_dir: Some(temp_dir.path().to_path_buf()),
            };

            let daemon_lock = PrivateStateLock::acquire(&config_root).unwrap();
            let supervisor_lock = daemon_lock.try_clone().unwrap();

            let clock = ManualClock::new();
            let sync_health = if initial_confirmed {
                SyncHealth {
                    dbus: "connected".into(),
                    ..crate::sync_health::derive_health(&Default::default(), 0.0, 600.0)
                }
            } else {
                crate::sync_health::derive_health(&Default::default(), 0.0, 600.0)
            };
            let health = Arc::new(Mutex::new(sync_health));
            let snapshot = Arc::new(Mutex::new(test_snapshot(Mode::Idle, false)));

            let mut initial_settings = default_settings_doc();
            initial_settings.capture_framerate = 4;
            let effective_settings = Arc::new(Mutex::new(Some(initial_settings)));
            let activity_prefs = Arc::new(ActivityPrefs::new(&crate::config::Config::default()));

            let config = crate::config::resolve_config_paths(&config_paths);
            let upload = Arc::new(UploadClient::new(
                &config,
                None::<PrivateLinkCapability>,
                Arc::new(SystemClock::new()),
            ));
            let sync_facts = Arc::new(Mutex::new(SyncFacts::default()));
            let sync_reset = SyncResetHandle::new_for_test(
                Arc::clone(&sync_facts),
                upload.link_facts(),
                state_dir.clone(),
            );
            let shutdown_notify = Arc::new(tokio::sync::Notify::new());
            let open_journal = OpenJournalAccess::default();

            let (supervisor, handler) = WindowControlSupervisor::new(
                supervisor_lock,
                Arc::clone(&upload),
                config_paths.clone(),
                "test-stream".into(),
                true,
                open_journal.clone(),
                Arc::clone(&shutdown_notify),
                None,
                sync_reset.clone(),
                Arc::new(clock.clone()),
                "test-device".into(),
                Arc::clone(&starter) as Arc<dyn OwnerStarter>,
                Arc::clone(&pairer) as Arc<dyn Pairer>,
                fault,
                Arc::clone(&health),
                Arc::clone(&snapshot),
                Arc::clone(&effective_settings),
                None,
                Some(Arc::clone(&activity_prefs)),
                Arc::new(Mutex::new(None)),
            );

            let supervisor_task = tokio::spawn(supervisor.run());
            tokio::task::yield_now().await;

            Self {
                _temp_dir: temp_dir,
                config_paths,
                config_root,
                state_dir,
                _daemon_lock: daemon_lock,
                clock,
                pairer,
                starter,
                health,
                snapshot,
                upload,
                sync_reset,
                sync_facts,
                shutdown_notify,
                open_journal,
                activity_prefs,
                effective_settings,
                handler,
                supervisor_task,
            }
        }

        fn check_durable_state_on_root(&self, candidate_pairing_id: Option<&str>) -> String {
            let cred_opt = crate::private_link::load_credential(&self.config_root)
                .ok()
                .flatten();
            if let Some(ref cred) = cred_opt {
                let pairing_id = crate::private_link::compute_pairing_id(&cred.client_cert_pem);
                if is_pairing_confirmed(&self.config_root, &pairing_id) {
                    return "confirmed".to_string();
                }
            }
            let answer = read_pairing_answer(&self.config_root).ok().flatten();
            if let Some(ref ans) = answer {
                if !ans.confirmed.is_empty() && candidate_pairing_id != Some(ans.confirmed.as_str())
                {
                    "held".to_string()
                } else if ans.confirmed.is_empty() {
                    "unpaired".to_string()
                } else {
                    "held".to_string()
                }
            } else if cred_opt.is_some() || self.config_root.join(CREDENTIALS_FILENAME).exists() {
                "held".to_string()
            } else {
                "unpaired".to_string()
            }
        }

        async fn restart(&mut self) {
            self.shutdown_notify.notify_one();
            (&mut self.supervisor_task).await.unwrap().unwrap();
            let config = crate::config::resolve_config_paths(&self.config_paths);
            self.upload = Arc::new(UploadClient::new(
                &config,
                None::<PrivateLinkCapability>,
                Arc::new(SystemClock::new()),
            ));
            self.sync_facts = Arc::new(Mutex::new(SyncFacts::default()));
            self.sync_reset = SyncResetHandle::new_for_test(
                self.sync_facts.clone(),
                self.upload.link_facts(),
                self.state_dir.clone(),
            );
            self.open_journal = OpenJournalAccess::default();
            self.shutdown_notify = Arc::new(tokio::sync::Notify::new());
            let (supervisor, handler) = WindowControlSupervisor::new(
                self._daemon_lock.try_clone().unwrap(),
                self.upload.clone(),
                self.config_paths.clone(),
                "test-stream".into(),
                true,
                self.open_journal.clone(),
                self.shutdown_notify.clone(),
                None,
                self.sync_reset.clone(),
                Arc::new(self.clock.clone()),
                "test-device".into(),
                self.starter.clone(),
                self.pairer.clone(),
                Arc::new(NoWriteFault),
                self.health.clone(),
                self.snapshot.clone(),
                Arc::clone(&self.effective_settings),
                None,
                Some(Arc::clone(&self.activity_prefs)),
                Arc::new(Mutex::new(None)),
            );
            self.handler = handler;
            self.supervisor_task = tokio::spawn(supervisor.run());
            // Go through the real command channel after startup has reconstructed disk state.
            self.handler.open_journal().await;
        }

        async fn shutdown(self) {
            self.shutdown_notify.notify_one();
            let _ = self.supervisor_task.await;
        }
    }

    #[tokio::test]
    async fn unpaired_begin_shows_mark_and_yes_confirms() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness =
            TestHarness::new(None, false, pairer, starter.clone(), Arc::new(NoWriteFault)).await;

        let initial_doc = harness.handler.get_control_state();
        let initial_model = decode_control_str(&initial_doc).unwrap();
        assert_eq!(initial_model.view, SessionView::Unpaired);
        assert_eq!(initial_model.durable, "unpaired");
        assert_eq!(initial_model.owner, "absent");
        assert!(initial_model.attempt.is_none());

        let begin_doc = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let begin_model = decode_control_str(&begin_doc).unwrap();
        assert_eq!(begin_model.view, SessionView::Pending);
        let attempt = begin_model.attempt.expect("attempt should be present");
        assert_eq!(attempt.state, "pending");
        assert!(attempt.mark.is_some());
        assert_eq!(
            attempt.pairing_id,
            compute_pairing_id(&cred.client_cert_pem)
        );

        assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
        assert!(!is_pairing_confirmed(
            &harness.config_root,
            &attempt.pairing_id
        ));
        assert_eq!(
            read_pairing_answer(&harness.config_root)
                .unwrap()
                .unwrap()
                .confirmed,
            ""
        );
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);

        harness.health.lock().unwrap().dbus = "connected".into();

        let answer_doc = harness
            .handler
            .answer_pairing(attempt.id, attempt.pairing_id, true)
            .await;
        let answer_model = decode_control_str(&answer_doc).unwrap();
        assert_eq!(answer_model.view, SessionView::Confirmed);
        assert_eq!(answer_model.durable, "confirmed");
        assert_eq!(answer_model.owner, "running");
        assert_eq!(answer_model.health, "listener-ready");
        assert!(answer_model.attempt.is_none());

        assert!(harness.config_root.join(CREDENTIALS_FILENAME).exists());
        assert!(harness.config_root.join(PAIRING_ANSWER_FILENAME).exists());
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);
        assert_eq!(starter.recorded_link_facts.lock().unwrap().len(), 1);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn direct_and_relay_admission_behavior() {
        let mut direct_cred = sample_valid_credential("direct-cert");
        direct_cred.endpoints = vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 7657,
        }];
        direct_cred.local_endpoints =
            Some(serde_json::json!([{"host": "127.0.0.1", "port": 7657}]));

        let pairer = Arc::new(FakePairer::new(direct_cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
        let begin_doc = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let begin_model = decode_control_str(&begin_doc).unwrap();
        let att = begin_model.attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;

        let saved = crate::private_link::load_credential(&harness.config_root)
            .unwrap()
            .unwrap();
        assert!(!saved.endpoints.is_empty());
        assert!(saved.local_endpoints.is_some());
        harness.shutdown().await;

        let relay_link = relay_pair_link_for("https://relay.example.com");
        let mut relay_cred = sample_tls_credential();
        relay_cred.relay_origin = Some("https://relay.example.com".into());
        relay_cred.device_token = Some(crate::private_link::tests::test_jwt_for_instance(
            &relay_cred.instance_id,
            i64::MAX / 2,
        ));
        relay_cred.device_token_expires_at = Some(i64::MAX / 2);
        relay_cred.endpoints = vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 7657,
        }];
        relay_cred.local_endpoints = Some(serde_json::json!([{"host": "127.0.0.1", "port": 7657}]));

        let pairer = Arc::new(FakePairer::new(relay_cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
        let begin_doc = harness.handler.begin_pairing(relay_link.clone()).await;
        let begin_model = decode_control_str(&begin_doc).unwrap();
        let att = begin_model.attempt.clone().unwrap();
        let _ = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;

        let saved = crate::private_link::load_credential(&harness.config_root)
            .unwrap()
            .unwrap();
        assert!(saved.endpoints.is_empty());
        assert!(saved.local_endpoints.is_none());
        harness.shutdown().await;

        let mut mismatch_cred = sample_tls_credential();
        mismatch_cred.relay_origin = Some("https://other.relay".into());
        let pairer = Arc::new(FakePairer::new(mismatch_cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
        let mismatch_doc = harness.handler.begin_pairing(relay_link).await;
        let mismatch_model = decode_control_str(&mismatch_doc).unwrap();
        assert_eq!(mismatch_model.view, SessionView::Failed);
        assert_eq!(mismatch_model.fault, Some("pair-failed".into()));
        harness.shutdown().await;

        let pairer = Arc::new(FakePairer::new(direct_cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness =
            TestHarness::new(None, false, pairer.clone(), starter, Arc::new(NoWriteFault)).await;
        let malformed_doc = harness
            .handler
            .begin_pairing("not-a-pair-link".into())
            .await;
        let malformed_model = decode_control_str(&malformed_doc).unwrap();
        assert_eq!(malformed_model.view, SessionView::Failed);
        assert_eq!(malformed_model.fault, Some("malformed-link".into()));
        assert_eq!(pairer.calls.load(Ordering::SeqCst), 0);
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn stale_and_duplicate_answers_do_not_confirm() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness =
            TestHarness::new(None, false, pairer, starter.clone(), Arc::new(NoWriteFault)).await;

        let begin_doc = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let begin_model = decode_control_str(&begin_doc).unwrap();
        let att = begin_model.attempt.unwrap();

        let stale_doc = harness
            .handler
            .answer_pairing("wrong_attempt".into(), att.pairing_id.clone(), true)
            .await;
        let stale_model = decode_control_str(&stale_doc).unwrap();
        assert_eq!(stale_model.fault.as_deref(), Some("stale-answer"));
        assert_eq!(stale_model.attempt.as_ref().unwrap().id, att.id);
        assert_eq!(stale_model.attempt.as_ref().unwrap().state, "pending");
        assert_eq!(stale_model.attempt.as_ref().unwrap().mark, att.mark);
        let fresh = decode_control_str(&harness.handler.get_control_state()).unwrap();
        assert!(fresh.fault.is_none());
        assert_eq!(fresh.attempt.unwrap().mark, att.mark);
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);

        let stale_doc2 = harness
            .handler
            .answer_pairing(att.id.clone(), "wrong_pairing".into(), true)
            .await;
        let stale_model2 = decode_control_str(&stale_doc2).unwrap();
        assert_eq!(stale_model2.fault.as_deref(), Some("stale-answer"));
        assert_eq!(stale_model2.attempt.as_ref().unwrap().id, att.id);
        assert_eq!(stale_model2.attempt.as_ref().unwrap().mark, att.mark);
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);

        let ok_doc = harness
            .handler
            .answer_pairing(att.id.clone(), att.pairing_id.clone(), true)
            .await;
        let ok_model = decode_control_str(&ok_doc).unwrap();
        assert_eq!(ok_model.durable, "confirmed");
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);

        let dup_doc = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;
        let dup_model = decode_control_str(&dup_doc).unwrap();
        assert!(dup_model.attempt.is_none());
        assert_eq!(dup_model.durable, "confirmed");
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn reject_cancel_abandon_expiry_leave_files_unchanged() {
        let cred = sample_valid_credential("cert-pem-bytes");

        {
            let pairer = Arc::new(FakePairer::new(cred.clone()));
            let starter = Arc::new(FakeOwnerStarter::default());
            let harness =
                TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
            let b = harness
                .handler
                .begin_pairing(TEST_PAIR_LINK.to_string())
                .await;
            let att = decode_control_str(&b).unwrap().attempt.unwrap();
            let rej = harness
                .handler
                .answer_pairing(att.id, att.pairing_id.clone(), false)
                .await;
            let m = decode_control_str(&rej).unwrap();
            assert_eq!(m.attempt.unwrap().state, "rejected");
            assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
            assert!(!is_pairing_confirmed(&harness.config_root, &att.pairing_id));
            assert_eq!(
                read_pairing_answer(&harness.config_root)
                    .unwrap()
                    .unwrap()
                    .confirmed,
                ""
            );
            harness.shutdown().await;
        }

        {
            let pairer = Arc::new(FakePairer::new(cred.clone()));
            let starter = Arc::new(FakeOwnerStarter::default());
            let harness =
                TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
            let b = harness
                .handler
                .begin_pairing(TEST_PAIR_LINK.to_string())
                .await;
            let att = decode_control_str(&b).unwrap().attempt.unwrap();
            let can = harness.handler.cancel_pairing(att.id).await;
            let m = decode_control_str(&can).unwrap();
            assert_eq!(m.attempt.unwrap().state, "cancelled");
            assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
            assert!(!is_pairing_confirmed(&harness.config_root, &att.pairing_id));
            assert_eq!(
                read_pairing_answer(&harness.config_root)
                    .unwrap()
                    .unwrap()
                    .confirmed,
                ""
            );
            harness.shutdown().await;
        }

        {
            let pairer = Arc::new(FakePairer::new(cred.clone()));
            let starter = Arc::new(FakeOwnerStarter::default());
            let harness =
                TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
            let b = harness
                .handler
                .begin_pairing(TEST_PAIR_LINK.to_string())
                .await;
            let att = decode_control_str(&b).unwrap().attempt.unwrap();
            let ab = harness.handler.abandon_pairing(att.id).await;
            let m = decode_control_str(&ab).unwrap();
            assert_eq!(m.attempt.unwrap().state, "abandoned");
            assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
            assert!(!is_pairing_confirmed(&harness.config_root, &att.pairing_id));
            assert_eq!(
                read_pairing_answer(&harness.config_root)
                    .unwrap()
                    .unwrap()
                    .confirmed,
                ""
            );
            harness.shutdown().await;
        }

        {
            let pairer = Arc::new(FakePairer::new(cred.clone()));
            let starter = Arc::new(FakeOwnerStarter::default());
            let harness =
                TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;
            let b = harness
                .handler
                .begin_pairing(TEST_PAIR_LINK.to_string())
                .await;
            let m = decode_control_str(&b).unwrap();
            assert_eq!(m.attempt.as_ref().unwrap().state, "pending");
            let att = m.attempt.unwrap();

            harness.clock.advance_and_yield(CEREMONY_TTL).await;
            let mut expired = false;
            let mut exp_model = None;
            for _ in 0..64 {
                let exp_doc = harness.handler.get_control_state();
                let m = decode_control_str(&exp_doc).unwrap();
                if m.attempt.as_ref().map(|a| a.state.as_str()) == Some("expired") {
                    expired = true;
                    exp_model = Some(m);
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(expired, "attempt did not expire in time");
            let exp_model = exp_model.unwrap();
            assert_eq!(exp_model.attempt.unwrap().state, "expired");
            assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
            assert!(!is_pairing_confirmed(&harness.config_root, &att.pairing_id));
            assert_eq!(
                read_pairing_answer(&harness.config_root)
                    .unwrap()
                    .unwrap()
                    .confirmed,
                ""
            );
            harness.shutdown().await;
        }
    }

    #[tokio::test]
    async fn unverifiable_mark_retires_and_does_not_persist() {
        let unparseable_cred = sample_credential("not-a-valid-uuid", "cert-pem");
        let pairer = Arc::new(FakePairer::new(unparseable_cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let begin_doc = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let begin_model = decode_control_str(&begin_doc).unwrap();
        assert_eq!(begin_model.view, SessionView::Failed);
        assert_eq!(begin_model.attempt.unwrap().state, "unverifiable");
        assert_eq!(begin_model.fault, Some("unverifiable".into()));
        assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
        assert_eq!(
            read_pairing_answer(&harness.config_root)
                .unwrap()
                .unwrap()
                .confirmed,
            ""
        );

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn grandfather_behavior_with_pending_candidate_and_legacy() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairing_id = compute_pairing_id(&cred.client_cert_pem);

        {
            let temp = tempdir().unwrap();
            let _ = crate::private_link::persist_credential_with_fault(
                temp.path(),
                &cred,
                &NoWriteFault,
            );
            let _ = crate::journal_mark::grandfather_answer_file_with_fault(
                temp.path(),
                Some(&pairing_id),
                &NoWriteFault,
            );
            let ans = crate::journal_mark::read_pairing_answer(temp.path())
                .unwrap()
                .unwrap();
            assert_eq!(ans.confirmed, "");
        }

        {
            let temp = tempdir().unwrap();
            let _ = crate::private_link::persist_credential_with_fault(
                temp.path(),
                &cred,
                &NoWriteFault,
            );
            let _ = crate::journal_mark::grandfather_answer_file_with_fault(
                temp.path(),
                None,
                &NoWriteFault,
            );
            let ans = crate::journal_mark::read_pairing_answer(temp.path())
                .unwrap()
                .unwrap();
            assert_eq!(ans.confirmed, pairing_id);
        }
    }

    #[tokio::test]
    async fn unpaired_commit_fault_and_persistence() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness =
            TestHarness::new(None, false, pairer, starter, Arc::new(FailWriteFault)).await;

        let b = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att = decode_control_str(&b).unwrap().attempt.unwrap();
        let ans = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;
        let m = decode_control_str(&ans).unwrap();

        assert_eq!(m.fault, Some("commit-failed".into()));
        assert_ne!(m.view, SessionView::Confirmed);
        assert_ne!(m.health, "connected");
        assert_eq!(m.durable, "unpaired");

        let check_durable = harness.check_durable_state_on_root(None);
        assert_eq!(check_durable, "unpaired");

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn cli_confirmation_recovers_interrupted_commit_without_held_health() {
        let cred = sample_valid_credential("held-cert");
        let pairing_id = compute_pairing_id(&cred.client_cert_pem);
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(
            Some(cred),
            false,
            Arc::new(FakePairer::new(sample_valid_credential("next-cert"))),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;
        harness.handler.open_journal().await;
        begin_pairing_commit(&harness.config_root, &pairing_id, &NoWriteFault).unwrap();
        {
            let _lock = crate::journal_mark::AnswerLock::acquire(&harness.config_root)
                .await
                .unwrap();
            crate::journal_mark::write_pairing_answer(&harness.config_root, &pairing_id).unwrap();
        }
        harness
            .clock
            .advance_and_yield(JOURNAL_MARK_RECHECK_INTERVAL)
            .await;
        harness.handler.open_journal().await;
        let model = decode_control_str(&harness.handler.get_control_state()).unwrap();
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);
        assert_eq!(model.durable, "confirmed");
        assert_eq!(model.owner, "running");
        assert_eq!(model.health, "listener-ready");
        assert!(!pairing_commit_pending(&harness.config_root));
        harness.shutdown().await;
    }

    struct FinalCommitSyncFault {
        armed: AtomicBool,
        syncs: AtomicUsize,
    }

    impl DurableWriteFault for FinalCommitSyncFault {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if self.armed.load(Ordering::SeqCst)
                && stage == DurableWriteStage::DirSync
                && self.syncs.fetch_add(1, Ordering::SeqCst) == 3
            {
                return Err(io::Error::other("controlled final commit sync failure"));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn uncertain_final_commit_cannot_activate_until_restart_settles_it() {
        let starter = Arc::new(FakeOwnerStarter::default());
        let fault = Arc::new(FinalCommitSyncFault {
            armed: AtomicBool::new(false),
            syncs: AtomicUsize::new(0),
        });
        let mut harness = TestHarness::new(
            None,
            false,
            Arc::new(FakePairer::new(sample_valid_credential("new-cert"))),
            starter.clone(),
            fault.clone(),
        )
        .await;
        let begin = harness.handler.begin_pairing(TEST_PAIR_LINK.into()).await;
        let att = decode_control_str(&begin).unwrap().attempt.unwrap();
        fault.armed.store(true, Ordering::SeqCst);
        let answer = harness
            .handler
            .answer_pairing(att.id, att.pairing_id.clone(), true)
            .await;
        let model = decode_control_str(&answer).unwrap();
        assert_eq!(model.fault.as_deref(), Some("commit-outcome-unknown"));
        assert_eq!(model.durable, "unavailable");
        assert_eq!(model.attempt.unwrap().state, "outcome-unknown");
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);
        harness
            .clock
            .advance_and_yield(JOURNAL_MARK_RECHECK_INTERVAL)
            .await;
        harness.handler.open_journal().await;
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);
        // Both files had persisted after explicit yes; the real surviving filesystem
        // settles the removal uncertainty on a fresh supervisor reconstruction.
        harness.restart().await;
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            decode_control_str(&harness.handler.get_control_state())
                .unwrap()
                .durable,
            "confirmed"
        );
        assert!(is_pairing_confirmed(&harness.config_root, &att.pairing_id));
        harness.shutdown().await;
    }

    struct CommitWriteFault {
        armed: AtomicBool,
        writes: AtomicUsize,
        target: usize,
        stage: DurableWriteStage,
    }

    impl DurableWriteFault for CommitWriteFault {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if self.armed.load(Ordering::SeqCst) {
                if stage == DurableWriteStage::Create {
                    self.writes.fetch_add(1, Ordering::SeqCst);
                }
                if self.writes.load(Ordering::SeqCst) == self.target && stage == self.stage {
                    return Err(io::Error::other("controlled commit write failure"));
                }
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn interrupted_credential_and_answer_writes_stay_held_after_restart() {
        for replacement in [false, true] {
            for target in [2, 3] {
                for stage in [
                    DurableWriteStage::Create,
                    DurableWriteStage::Write,
                    DurableWriteStage::Fsync,
                    DurableWriteStage::Rename,
                    DurableWriteStage::DirSync,
                ] {
                    let cred = sample_valid_credential("new-cert");
                    let pairer = Arc::new(FakePairer::new(cred.clone()));
                    let starter = Arc::new(FakeOwnerStarter::default());
                    let fault = Arc::new(CommitWriteFault {
                        armed: AtomicBool::new(false),
                        writes: AtomicUsize::new(0),
                        target,
                        stage,
                    });
                    let mut harness = TestHarness::new(
                        replacement.then(|| sample_valid_credential("old-cert")),
                        replacement,
                        pairer,
                        starter.clone(),
                        fault.clone(),
                    )
                    .await;
                    let begin = harness.handler.begin_pairing(TEST_PAIR_LINK.into()).await;
                    let att = decode_control_str(&begin).unwrap().attempt.unwrap();
                    fault.armed.store(true, Ordering::SeqCst);
                    let answer = harness
                        .handler
                        .answer_pairing(att.id, att.pairing_id.clone(), true)
                        .await;
                    let model = decode_control_str(&answer).unwrap();
                    assert_eq!(model.fault.as_deref(), Some("commit-failed"));
                    assert_eq!(model.durable, "held");
                    assert_ne!(model.view, SessionView::Confirmed);
                    assert!(pairing_commit_pending(&harness.config_root));
                    assert!(!is_pairing_confirmed(&harness.config_root, &att.pairing_id));
                    assert_eq!(
                        starter.starts.load(Ordering::SeqCst),
                        usize::from(replacement)
                    );
                    harness.restart().await;
                    let restarted =
                        decode_control_str(&harness.handler.get_control_state()).unwrap();
                    assert_eq!(restarted.durable, "held");
                    assert_eq!(restarted.owner, "absent");
                    assert_eq!(
                        starter.starts.load(Ordering::SeqCst),
                        usize::from(replacement)
                    );
                    // A later deliberate ceremony recovers, without rollback or migration.
                    let begin = harness.handler.begin_pairing(TEST_PAIR_LINK.into()).await;
                    let att = decode_control_str(&begin).unwrap().attempt.unwrap();
                    let answer = harness
                        .handler
                        .answer_pairing(att.id, att.pairing_id.clone(), true)
                        .await;
                    assert_eq!(decode_control_str(&answer).unwrap().durable, "confirmed");
                    assert!(!pairing_commit_pending(&harness.config_root));
                    assert!(is_pairing_confirmed(&harness.config_root, &att.pairing_id));
                    harness.shutdown().await;
                }
            }
        }
    }

    #[tokio::test]
    async fn answer_rechecks_deadline_when_timer_delivery_is_delayed() {
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(
            None,
            false,
            Arc::new(FakePairer::new(sample_valid_credential("new-cert"))),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;
        let begin = harness.handler.begin_pairing(TEST_PAIR_LINK.into()).await;
        let att = decode_control_str(&begin).unwrap().attempt.unwrap();
        {
            let mut clock = harness.clock.inner.lock().unwrap();
            assert!(clock.sleepers.iter().any(|(deadline, tx)| !tx.is_closed()
                && *deadline
                    == clock.monotonic_seconds + JOURNAL_MARK_RECHECK_INTERVAL.as_secs_f64()));
            // Advance the clock, intentionally withholding timer delivery.
            clock.monotonic_seconds += CEREMONY_TTL.as_secs_f64() + 1.0;
        }
        let answer = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;
        let model = decode_control_str(&answer).unwrap();
        assert_eq!(model.attempt.unwrap().state, "expired");
        assert_eq!(starter.starts.load(Ordering::SeqCst), 0);
        assert!(!harness.config_root.join(CREDENTIALS_FILENAME).exists());
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn failed_quiescence_blocks_replacement_and_later_activation() {
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(
            Some(sample_valid_credential("old-cert")),
            true,
            Arc::new(FakePairer::new(sample_valid_credential("new-cert"))),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;
        let before = fs::read(harness.config_root.join(CREDENTIALS_FILENAME)).unwrap();
        let before_answer = fs::read(harness.config_root.join(PAIRING_ANSWER_FILENAME)).unwrap();
        for _ in 0..2 {
            let begin = harness.handler.begin_pairing(TEST_PAIR_LINK.into()).await;
            let att = decode_control_str(&begin).unwrap().attempt.unwrap();
            starter.states.lock().unwrap()[0]
                .shutdown_fails
                .store(true, Ordering::SeqCst);
            let answer = harness
                .handler
                .answer_pairing(att.id, att.pairing_id, true)
                .await;
            let model = decode_control_str(&answer).unwrap();
            assert_eq!(model.fault.as_deref(), Some("owner-stop-failed"));
            assert_eq!(model.owner, "failed");
            assert_eq!(starter.starts.load(Ordering::SeqCst), 1);
            assert_eq!(
                fs::read(harness.config_root.join(CREDENTIALS_FILENAME)).unwrap(),
                before
            );
            assert_eq!(
                fs::read(harness.config_root.join(PAIRING_ANSWER_FILENAME)).unwrap(),
                before_answer
            );
        }
        harness
            .clock
            .advance_and_yield(JOURNAL_MARK_RECHECK_INTERVAL)
            .await;
        harness.handler.open_journal().await;
        assert_eq!(starter.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            decode_control_str(&harness.handler.get_control_state())
                .unwrap()
                .owner,
            "failed"
        );
        harness.shutdown().await;
    }

    #[tokio::test]
    async fn owner_start_failure_recovers_on_subsequent_pairing() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());
        starter.should_fail.store(true, Ordering::SeqCst);

        let harness = TestHarness::new(
            None,
            false,
            pairer.clone(),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;

        let b1 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att1 = decode_control_str(&b1).unwrap().attempt.unwrap();
        let ans1 = harness
            .handler
            .answer_pairing(att1.id, att1.pairing_id, true)
            .await;
        let m1 = decode_control_str(&ans1).unwrap();
        assert_eq!(m1.view, SessionView::ConfirmedUnavailable);
        assert_eq!(m1.fault, Some("owner-start-failed".into()));
        assert_eq!(m1.owner, "failed");

        starter.should_fail.store(false, Ordering::SeqCst);
        harness.health.lock().unwrap().dbus = "connected".into();

        let b2 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att2 = decode_control_str(&b2).unwrap().attempt.unwrap();
        let ans2 = harness
            .handler
            .answer_pairing(att2.id, att2.pairing_id, true)
            .await;
        let m2 = decode_control_str(&ans2).unwrap();
        assert_eq!(m2.view, SessionView::Confirmed);
        assert_eq!(m2.owner, "running");
        assert!(m2.fault.is_none());

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn revoked_owner_replacement_clears_circuit_and_auth() {
        let initial_cred = sample_valid_credential("initial-cert");
        let pairer = Arc::new(FakePairer::new(sample_valid_credential("replacement-cert")));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(
            Some(initial_cred),
            true,
            pairer,
            starter,
            Arc::new(NoWriteFault),
        )
        .await;

        harness.upload.revoke_for_test();
        harness
            .sync_reset
            .shared_circuit_open_permanent()
            .store(true, Ordering::Release);
        harness
            .sync_reset
            .circuit_unblock_requested()
            .store(false, Ordering::Release);
        {
            let mut facts = harness.sync_facts.lock().unwrap();
            facts.last_error_class = Some(crate::sync_health::ErrorType::Auth);
            let _ = crate::sync_health::save_facts(&harness.state_dir, &facts);
        }
        assert!(harness.upload.is_revoked());

        let b = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att = decode_control_str(&b).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;

        assert!(!harness.upload.is_revoked());
        assert!(
            harness
                .sync_reset
                .circuit_unblock_requested()
                .load(Ordering::Acquire)
        );
        assert!(
            !harness
                .sync_reset
                .shared_circuit_open_permanent()
                .load(Ordering::Acquire)
        );
        let saved_facts = crate::sync_health::load_facts(&harness.state_dir);
        assert!(saved_facts.last_error_class.is_none());

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn two_successive_replacements_fences_and_shuts_down_first_owner() {
        let cred1 = sample_valid_credential("cert-1");
        let cred2 = sample_valid_credential("cert-2");
        let pairer = Arc::new(FakePairer::new(cred1.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());

        let harness = TestHarness::new(
            None,
            false,
            pairer.clone(),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;

        let b1 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att1 = decode_control_str(&b1).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att1.id, att1.pairing_id, true)
            .await;

        assert_eq!(starter.states.lock().unwrap().len(), 1);
        let owner1_state = Arc::clone(&starter.states.lock().unwrap()[0]);
        assert!(!owner1_state.fenced.load(Ordering::Acquire));
        assert!(!owner1_state.shutdown.load(Ordering::Acquire));

        *pairer.result.lock().unwrap() = Some(cred2.clone());

        let b2 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att2 = decode_control_str(&b2).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att2.id, att2.pairing_id, true)
            .await;

        assert_eq!(starter.states.lock().unwrap().len(), 2);
        assert!(owner1_state.fenced.load(Ordering::Acquire));
        assert!(owner1_state.shutdown.load(Ordering::Acquire));

        let saved_cred = crate::private_link::load_credential(&harness.config_root)
            .unwrap()
            .unwrap();
        assert_eq!(saved_cred.client_cert_pem, cred2.client_cert_pem);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn late_stale_upload_and_fenced_refresh_rejection() {
        let cred1 = sample_valid_credential("cert-1");
        let cred2 = sample_valid_credential("cert-2");
        let pairer = Arc::new(FakePairer::new(cred1.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());

        let harness = TestHarness::new(
            None,
            false,
            pairer.clone(),
            starter.clone(),
            Arc::new(NoWriteFault),
        )
        .await;

        let b1 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att1 = decode_control_str(&b1).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att1.id, att1.pairing_id, true)
            .await;

        let writer1 = Arc::new(crate::private_link::OrderedCredentialWriter::new(
            harness.config_root.clone(),
            cred1.clone(),
            Arc::new(NoWriteFault),
            Arc::new(AtomicBool::new(false)),
            harness.upload.link_facts(),
        ));

        *pairer.result.lock().unwrap() = Some(cred2.clone());
        let b2 = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att2 = decode_control_str(&b2).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att2.id, att2.pairing_id, true)
            .await;

        let cred2_bytes = fs::read(harness.config_root.join(CREDENTIALS_FILENAME)).unwrap();

        writer1.fence_shutdown();
        let old_pairing_id = compute_pairing_id(&cred1.client_cert_pem);
        writer1.commit_mandatory_refresh(&old_pairing_id, 1, "stale-token", 99999999);

        let bytes_after = fs::read(harness.config_root.join(CREDENTIALS_FILENAME)).unwrap();
        assert_eq!(bytes_after, cred2_bytes);

        harness.health.lock().unwrap().dbus = "connected".into();
        let _attempt =
            writer1.metadata_attempt(tokio::time::Instant::now() + Duration::from_secs(1));
        assert_eq!(harness.health.lock().unwrap().dbus, "connected");

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn fake_owner_publishes_on_provided_link_facts() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred.clone()));
        let starter = Arc::new(FakeOwnerStarter::default());

        let harness =
            TestHarness::new(None, false, pairer, starter.clone(), Arc::new(NoWriteFault)).await;

        let b = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att = decode_control_str(&b).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;

        let link_facts = starter.recorded_link_facts.lock().unwrap()[0].clone();
        link_facts.publish(LinkFact::ListenerReady);

        let sampler = SyncSampler {
            facts: harness.sync_facts.clone(),
            clock: Arc::new(SystemClock::new()),
            stale_threshold: 600.0,
            poison_reports: Arc::new(AtomicUsize::new(0)),
            link_facts: link_facts.clone(),
        };

        assert_eq!(sampler.health().dbus, "listener-ready");

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn write_setting_validation_and_preservation() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let config_file = harness.config_root.join("config.json");
        let initial_json = serde_json::json!({
            "unknown_key": "some_value",
            "panel_icon_offer": false,
            "capture_framerate": 2,
            "draw_cursor": false,
            "start_paused": true,
            "segment_interval": 120,
        });
        fs::write(&config_file, serde_json::to_vec(&initial_json).unwrap()).unwrap();

        let doc = harness.handler.get_control_state();
        let m = decode_control_str(&doc).unwrap();
        assert_eq!(m.settings.capture_framerate, 2);
        assert!(!m.settings.draw_cursor);
        assert!(m.settings.start_paused);
        assert_eq!(m.settings.segment_interval, 120);

        let mut current_map: serde_json::Map<String, Value> =
            serde_json::from_slice(&fs::read(&config_file).unwrap()).unwrap();
        current_map.insert("hand_edit".into(), Value::String("after_read".into()));
        fs::write(&config_file, serde_json::to_vec(&current_map).unwrap()).unwrap();

        let inv1 = harness
            .handler
            .write_setting("capture_framerate".into(), "invalid".into())
            .await;
        let m_inv1 = decode_control_str(&inv1).unwrap();
        assert_eq!(m_inv1.fault, Some("invalid-setting".into()));

        let inv2 = harness
            .handler
            .write_setting("draw_cursor".into(), "not_bool".into())
            .await;
        let m_inv2 = decode_control_str(&inv2).unwrap();
        assert_eq!(m_inv2.fault, Some("invalid-setting".into()));

        let inv3 = harness
            .handler
            .write_setting("segment_interval".into(), "0".into())
            .await;
        let m_inv3 = decode_control_str(&inv3).unwrap();
        assert_eq!(m_inv3.fault, Some("invalid-setting".into()));

        let ok = harness
            .handler
            .write_setting("capture_framerate".into(), "5".into())
            .await;
        let m_ok = decode_control_str(&ok).unwrap();
        assert_eq!(m_ok.settings.capture_framerate, 5);
        assert_eq!(m_ok.restart_required, Some(true));
        assert!(m_ok.fault.is_none());

        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_file).unwrap()).unwrap();
        assert_eq!(
            saved.get("unknown_key").unwrap().as_str().unwrap(),
            "some_value"
        );
        assert_eq!(
            saved.get("hand_edit").unwrap().as_str().unwrap(),
            "after_read"
        );
        assert!(!saved.get("panel_icon_offer").unwrap().as_bool().unwrap());
        assert!(!saved.get("draw_cursor").unwrap().as_bool().unwrap());
        assert!(saved.get("start_paused").unwrap().as_bool().unwrap());
        assert_eq!(
            saved.get("segment_interval").unwrap().as_i64().unwrap(),
            120
        );
        assert_eq!(saved.get("capture_framerate").unwrap().as_i64().unwrap(), 5);

        fs::write(&config_file, b"corrupted json").unwrap();
        let inv_json = harness
            .handler
            .write_setting("capture_framerate".into(), "3".into())
            .await;
        let m_inv_json = decode_control_str(&inv_json).unwrap();
        assert_eq!(m_inv_json.fault, Some("invalid-config".into()));

        fs::write(&config_file, b"{}").unwrap();
        let def_doc = harness.handler.get_control_state();
        let m_def = decode_control_str(&def_doc).unwrap();
        assert_eq!(m_def.settings.capture_framerate, DEFAULT_CAPTURE_FRAMERATE);
        assert_eq!(m_def.settings.draw_cursor, DEFAULT_DRAW_CURSOR);
        assert_eq!(m_def.settings.start_paused, DEFAULT_START_PAUSED);
        assert_eq!(m_def.settings.segment_interval, DEFAULT_SEGMENT_INTERVAL);

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn segment_and_browser_files_unmodified_across_transition() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let captures_dir = harness._temp_dir.path().join("captures");
        let segment_dir = captures_dir.join("20261017").join("140000");
        fs::create_dir_all(&segment_dir).unwrap();
        let frame_file = segment_dir.join("00001.raw");
        let frame_bytes = b"pixel_data_12345";
        fs::write(&frame_file, frame_bytes).unwrap();

        let browser_dir = harness._temp_dir.path().join("browser");
        fs::create_dir_all(&browser_dir).unwrap();
        let custody_file = browser_dir.join("custody.wal");
        let custody_bytes = b"browser_history_wal_bytes";
        fs::write(&custody_file, custody_bytes).unwrap();

        let b = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let att = decode_control_str(&b).unwrap().attempt.unwrap();
        let _ = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;

        assert_eq!(fs::read(&frame_file).unwrap(), frame_bytes);
        assert_eq!(fs::read(&custody_file).unwrap(), custody_bytes);

        let entries = fs::read_dir(&segment_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name(), "00001.raw");

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn planted_secrets_absent_from_encoded_document_and_debug() {
        let planted_pem =
            "-----BEGIN CERTIFICATE-----\nSECRET_PLANTED_PEM_VALUE\n-----END CERTIFICATE-----";
        let planted_token = "SECRET_PLANTED_TOKEN_VALUE";
        let planted_link = "0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXW00_SECRET_LINK";
        let planted_url = "https://secret.solstone.app/journal/planted-target-url";

        let mut cred = sample_valid_credential(planted_pem);
        cred.device_token = Some(planted_token.into());

        let pairer = Arc::new(FakePairer::new(cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let b = harness
            .handler
            .begin_pairing(TEST_PAIR_LINK.to_string())
            .await;
        let m_pending = decode_control_str(&b).unwrap();
        let spoken_mark = m_pending.attempt.as_ref().unwrap().mark.clone().unwrap();

        assert!(!spoken_mark.is_empty());

        let att = m_pending.attempt.unwrap();
        let ans = harness
            .handler
            .answer_pairing(att.id, att.pairing_id, true)
            .await;
        let m_confirmed = decode_control_str(&ans).unwrap();

        assert!(m_confirmed.attempt.is_none());

        let doc_str = harness.handler.get_control_state();
        let debug_doc = format!(
            "{:?}",
            serde_json::from_str::<ControlDocument>(&doc_str).unwrap()
        );
        let debug_model = format!("{:?}", m_confirmed);

        for secret in [planted_pem, planted_token, planted_link, planted_url] {
            assert!(
                !doc_str.contains(secret),
                "doc_str should not contain {secret}"
            );
            assert!(
                !debug_doc.contains(secret),
                "debug_doc should not contain {secret}"
            );
            assert!(
                !debug_model.contains(secret),
                "debug_model should not contain {secret}"
            );
        }

        assert!(!doc_str.contains(&spoken_mark));
        assert!(!debug_doc.contains(&spoken_mark));
        assert!(!debug_model.contains(&spoken_mark));

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn open_journal_behavior() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred));
        let starter = Arc::new(FakeOwnerStarter::default());

        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let opened_target = Arc::new(Mutex::new(None));
        let opener_target = Arc::clone(&opened_target);
        let handler = harness.handler.with_custom_opener(Arc::new(move |target| {
            *opener_target.lock().unwrap() = Some(target.to_string());
            Ok(())
        }));

        let doc1 = handler.open_journal().await;
        let m1 = decode_control_str(&doc1).unwrap();
        assert_eq!(m1.open_journal, Some("not-confirmed".into()));
        assert!(opened_target.lock().unwrap().is_none());

        let b = handler.begin_pairing(TEST_PAIR_LINK.to_string()).await;
        let att = decode_control_str(&b).unwrap().attempt.unwrap();
        let _ = handler.answer_pairing(att.id, att.pairing_id, true).await;

        let doc2 = handler.open_journal().await;
        let m2 = decode_control_str(&doc2).unwrap();
        assert_eq!(m2.open_journal, Some("unavailable".into()));
        assert!(opened_target.lock().unwrap().is_none());

        let (target, cap) = open_journal_fixture("https://journal.solstone.app/owner");
        harness.open_journal.install(cap);

        let doc3 = handler.open_journal().await;
        let m3 = decode_control_str(&doc3).unwrap();
        assert_eq!(m3.open_journal, Some("opened".into()));
        assert_eq!(
            opened_target.lock().unwrap().as_deref(),
            Some("https://journal.solstone.app/owner")
        );

        target.close();
        let doc4 = handler.open_journal().await;
        let m4 = decode_control_str(&doc4).unwrap();
        assert_eq!(m4.open_journal, Some("unavailable".into()));

        harness.shutdown().await;
    }

    #[tokio::test]
    async fn pair_bound_timeout_fails_attempt() {
        let pairer = Arc::new(FakePairer::hanging());
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        let handler = harness.handler.clone();
        let begin_task =
            tokio::spawn(async move { handler.begin_pairing(TEST_PAIR_LINK.to_string()).await });

        let mut sleeper_ready = false;
        for _ in 0..64 {
            if harness.clock.pending_sleeps() > 0 {
                sleeper_ready = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(sleeper_ready, "timeout sleeper was not registered in time");
        harness.clock.advance(PAIR_BOUND);

        let doc = begin_task.await.unwrap();
        let model = decode_control_str(&doc).unwrap();
        assert_eq!(model.view, SessionView::Failed);
        assert_eq!(model.fault, Some("pair-failed".into()));
        assert_eq!(model.attempt.unwrap().state, "failed");

        harness.shutdown().await;
    }

    #[test]
    fn decoder_parses_all_control_document_fields_and_rejects_unsupported_versions() {
        let valid_json = serde_json::json!({
            "v": 1,
            "durable": "confirmed",
            "owner": "running",
            "attempt": {
                "id": "attempt-1",
                "pairing_id": "pair-1",
                "state": "pending",
                "mark": "red, blue · word·word"
            },
            "health": "connected",
            "capture": "idle",
            "settings": {
                "capture_framerate": 5,
                "draw_cursor": false,
                "start_paused": true,
                "segment_interval": 600
            },
            "restart_required": true,
            "open_journal": "opened",
            "fault": "none"
        });

        let doc_str = serde_json::to_string(&valid_json).unwrap();
        let model = decode_control_str(&doc_str).unwrap();

        assert_eq!(model.view, SessionView::Pending);
        assert_eq!(model.durable, "confirmed");
        assert_eq!(model.owner, "running");
        let att = model.attempt.unwrap();
        assert_eq!(att.id, "attempt-1");
        assert_eq!(att.pairing_id, "pair-1");
        assert_eq!(att.state, "pending");
        assert_eq!(att.mark, Some("red, blue · word·word".into()));
        assert_eq!(model.health, "connected");
        assert_eq!(model.capture, "idle");
        assert_eq!(model.settings.capture_framerate, 5);
        assert!(!model.settings.draw_cursor);
        assert!(model.settings.start_paused);
        assert_eq!(model.settings.segment_interval, 600);
        assert_eq!(model.restart_required, Some(true));
        assert_eq!(model.open_journal, Some("opened".into()));
        assert_eq!(model.fault, Some("none".into()));

        let mut v2_json = valid_json.clone();
        v2_json["v"] = serde_json::json!(2);
        let v2_str = serde_json::to_string(&v2_json).unwrap();
        let err = decode_control_str(&v2_str).unwrap_err();
        assert_eq!(err, DecodeError::UnsupportedVersion(2));

        let mut v0_json = valid_json.clone();
        v0_json["v"] = serde_json::json!(0);
        let v0_str = serde_json::to_string(&v0_json).unwrap();
        let err0 = decode_control_str(&v0_str).unwrap_err();
        assert_eq!(err0, DecodeError::UnsupportedVersion(0));

        let err_result = decode_control::<&str>(Err("bus-error")).unwrap();
        assert_eq!(err_result.view, SessionView::Unknown);
        assert_ne!(err_result.view, SessionView::Confirmed);
        assert_ne!(err_result.health, "connected");

        let unparseable = decode_control_str("not-a-valid-json").unwrap();
        assert_eq!(unparseable.view, SessionView::Unknown);
        assert_ne!(unparseable.view, SessionView::Confirmed);
        assert_ne!(unparseable.health, "connected");
    }

    #[test]
    fn adapt_graphical_link_validates_and_normalizes() {
        assert_eq!(
            adapt_graphical_link("https://go.solstone.app/p#FRAG"),
            Ok("https://go.solstone.app/p#FRAG".to_string())
        );
        assert_eq!(
            adapt_graphical_link("https://go.solstone.app/p/#FRAG"),
            Err(GraphicalLinkError)
        );
        assert_eq!(
            adapt_graphical_link("solstone://pair#FRAG"),
            Ok("solstone://pair#FRAG".to_string())
        );
        assert_eq!(
            adapt_graphical_link("solstone://pair/#FRAG"),
            Ok("solstone://pair/#FRAG".to_string())
        );

        assert_eq!(
            adapt_graphical_link("HTTPS://go.solstone.app/p#FRAG"),
            Ok("HTTPS://go.solstone.app/p#FRAG".to_string())
        );
        assert_eq!(
            adapt_graphical_link("SoLsToNe://pair/#FRAG"),
            Ok("SoLsToNe://pair/#FRAG".to_string())
        );

        assert_eq!(
            adapt_graphical_link("   https://go.solstone.app/p#FRAG  \n"),
            Ok("https://go.solstone.app/p#FRAG".to_string())
        );

        assert_eq!(adapt_graphical_link("FRAG"), Err(GraphicalLinkError));
        assert_eq!(adapt_graphical_link("#FRAG"), Err(GraphicalLinkError));

        assert_eq!(
            adapt_graphical_link("https://go.solstone.app/p?query=1#FRAG"),
            Err(GraphicalLinkError)
        );

        assert_eq!(
            adapt_graphical_link("https://user@go.solstone.app/p#FRAG"),
            Err(GraphicalLinkError)
        );
        assert_eq!(
            adapt_graphical_link("https://go.solstone.app:443/p#FRAG"),
            Err(GraphicalLinkError)
        );

        assert_eq!(
            adapt_graphical_link("https://go.solstone.app/p#"),
            Err(GraphicalLinkError)
        );

        assert_eq!(
            adapt_graphical_link("https://evil.solstone.app/p#FRAG"),
            Err(GraphicalLinkError)
        );
        assert_eq!(
            adapt_graphical_link("http://go.solstone.app/p#FRAG"),
            Err(GraphicalLinkError)
        );
        assert_eq!(adapt_graphical_link(""), Err(GraphicalLinkError));
        assert_eq!(adapt_graphical_link("not-a-link"), Err(GraphicalLinkError));

        let secret_link = "https://evil.com#super_secret_fragment_12345";
        let err = adapt_graphical_link(secret_link).unwrap_err();
        assert!(!format!("{err}").contains("super_secret_fragment"));
        assert!(!format!("{err}").contains(secret_link));
    }

    #[test]
    fn read_saved_settings_availability_and_fallback() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let config_file = config_dir.join("config.json");
        let paths = ConfigPaths {
            config_dir: Some(config_dir.clone()),
            base_dir: Some(temp_dir.path().to_path_buf()),
        };

        let (settings, available) = read_saved_settings(&paths);
        assert!(available);
        assert_eq!(settings.capture_framerate, DEFAULT_CAPTURE_FRAMERATE);
        assert_eq!(settings.draw_cursor, DEFAULT_DRAW_CURSOR);
        assert_eq!(settings.start_paused, DEFAULT_START_PAUSED);
        assert_eq!(settings.segment_interval, DEFAULT_SEGMENT_INTERVAL);

        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(&config_file, b"{}").unwrap();
        let (settings, available) = read_saved_settings(&paths);
        assert!(available);
        assert_eq!(settings.capture_framerate, DEFAULT_CAPTURE_FRAMERATE);
        assert_eq!(settings.draw_cursor, DEFAULT_DRAW_CURSOR);
        assert_eq!(settings.start_paused, DEFAULT_START_PAUSED);
        assert_eq!(settings.segment_interval, DEFAULT_SEGMENT_INTERVAL);

        std::fs::write(&config_file, br#"{"capture_framerate": 5}"#).unwrap();
        let (settings, available) = read_saved_settings(&paths);
        assert!(available);
        assert_eq!(settings.capture_framerate, 5);
        assert_eq!(settings.draw_cursor, DEFAULT_DRAW_CURSOR);
        assert_eq!(settings.start_paused, DEFAULT_START_PAUSED);
        assert_eq!(settings.segment_interval, DEFAULT_SEGMENT_INTERVAL);

        std::fs::write(
            &config_file,
            br#"{"capture_framerate": 99, "segment_interval": -50}"#,
        )
        .unwrap();
        let (settings, available) = read_saved_settings(&paths);
        assert!(available);
        assert_eq!(settings.capture_framerate, 99);
        assert_eq!(settings.segment_interval, -50);

        std::fs::write(&config_file, b"not valid json").unwrap();
        let (settings, available) = read_saved_settings(&paths);
        assert!(!available);
        assert_eq!(settings.capture_framerate, DEFAULT_CAPTURE_FRAMERATE);

        std::fs::write(&config_file, b"[1, 2, 3]").unwrap();
        let (_settings, available) = read_saved_settings(&paths);
        assert!(!available);

        std::fs::write(&config_file, br#"{"capture_framerate": "five"}"#).unwrap();
        let (_settings, available) = read_saved_settings(&paths);
        assert!(!available);

        std::fs::write(&config_file, br#"{"draw_cursor": "true"}"#).unwrap();
        let (_settings, available) = read_saved_settings(&paths);
        assert!(!available);

        std::fs::remove_file(&config_file).unwrap();
        std::fs::create_dir(&config_file).unwrap();
        let (_settings, available) = read_saved_settings(&paths);
        assert!(!available);
    }

    struct PolicySentinel {
        title_reads: std::cell::Cell<usize>,
        url_reads: std::cell::Cell<usize>,
    }

    impl crate::activity_source::ForegroundProvider for PolicySentinel {
        fn read_foreground(
            &mut self,
            want_title: bool,
            _facts: &crate::activity::ActivityFacts,
        ) -> crate::activity_source::ForegroundRead {
            if want_title {
                self.title_reads.set(self.title_reads.get() + 1);
                return crate::activity_source::ForegroundRead::Window(
                    crate::activity_source::ForegroundWindow {
                        app: "SENTINEL_APP".into(),
                        app_id: None,
                        title: Some("SENTINEL_TITLE".into()),
                        pid: Some(9),
                        window_id: 7,
                        window_token: String::new(),
                    },
                );
            }
            crate::activity_source::ForegroundRead::Window(
                crate::activity_source::ForegroundWindow {
                    app: "SENTINEL_APP".into(),
                    app_id: None,
                    title: None,
                    pid: Some(9),
                    window_id: 7,
                    window_token: String::new(),
                },
            )
        }

        fn read_committed_url(&mut self, _pid: u32) -> Option<String> {
            self.url_reads.set(self.url_reads.get() + 1);
            Some("https://user:secret@sentinel.example/path?q=1#frag".into())
        }
    }

    #[tokio::test]
    async fn write_setting_policy_apply() {
        let cred = sample_valid_credential("cert-pem-bytes");
        let pairer = Arc::new(FakePairer::new(cred));
        let starter = Arc::new(FakeOwnerStarter::default());
        let harness = TestHarness::new(None, false, pairer, starter, Arc::new(NoWriteFault)).await;

        // Desktop activity starts on. A lone write may turn it off.
        assert!(harness.activity_prefs.snapshot().source);
        for field in ["activity_source", "activity_titles"] {
            let _ = harness
                .handler
                .write_setting(field.into(), "false".into())
                .await;
        }
        let initial_gen = harness.activity_prefs.generation();
        assert!(!harness.activity_prefs.snapshot().source);

        // Lone enables stay off. A later exclusion must not arrive after the source is already on.
        for (field, value) in [
            ("activity_source", "true"),
            ("activity_titles", "true"),
            ("activity_urls", "true"),
        ] {
            let _ = harness
                .handler
                .write_setting(field.into(), value.into())
                .await;
        }
        let rejected = harness.activity_prefs.snapshot();
        assert!(!rejected.source);
        assert!(!rejected.titles);
        assert!(!rejected.urls);
        assert_eq!(harness.activity_prefs.generation(), initial_gen);

        let mut sampler = crate::activity_source::ActivitySampler::new();
        let mut provider = PolicySentinel {
            title_reads: std::cell::Cell::new(0),
            url_reads: std::cell::Cell::new(0),
        };
        let activity = crate::observer::ActivityState {
            session: crate::desktop_session::SessionPresence::Live,
            ..crate::observer::ActivityState::default()
        };
        sampler.poll(
            &crate::activity_source::PollInput {
                now_ms: 1_000,
                prefs: rejected,
                activity,
                bound: crate::activity::BoundBackends::default(),
                paused: false,
                route: "gnome-shell",
            },
            &mut provider,
            &crate::activity::ActivityFacts::default(),
            &mut || true,
        );
        sampler.seal(2_000);
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.url_reads.get(), 0);
        let early = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert!(!early.contains("SENTINEL_APP"));
        assert!(!early.contains("SENTINEL_TITLE"));
        assert!(!early.contains("sentinel.example"));

        // One policy enables the source and both exclusions together.
        let policy_json = serde_json::json!({
            "activity_source": true,
            "activity_titles": true,
            "activity_urls": true,
            "activity_exclude_apps": ["SENTINEL_APP"],
            "activity_exclude_titles": ["SENTINEL_TITLE"]
        })
        .to_string();

        let _doc1 = harness
            .handler
            .write_setting("activity_policy".into(), policy_json.clone())
            .await;
        let snap1 = harness.activity_prefs.snapshot();
        assert!(snap1.source);
        assert!(snap1.titles);
        assert!(snap1.urls);
        assert_eq!(snap1.exclude_apps, vec!["SENTINEL_APP".to_string()]);
        assert_eq!(snap1.exclude_titles, vec!["SENTINEL_TITLE".to_string()]);
        assert_eq!(harness.activity_prefs.generation(), initial_gen + 1);

        let mut provider = PolicySentinel {
            title_reads: std::cell::Cell::new(0),
            url_reads: std::cell::Cell::new(0),
        };
        let activity = crate::observer::ActivityState {
            session: crate::desktop_session::SessionPresence::Live,
            ..crate::observer::ActivityState::default()
        };
        sampler.poll(
            &crate::activity_source::PollInput {
                now_ms: 1_000,
                prefs: snap1.clone(),
                activity,
                bound: crate::activity::BoundBackends::default(),
                paused: false,
                route: "gnome-shell",
            },
            &mut provider,
            &crate::activity::ActivityFacts::default(),
            &mut || true,
        );
        sampler.seal(2_000);
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.url_reads.get(), 0);
        let blob = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert!(!blob.contains("SENTINEL_APP"));
        assert!(!blob.contains("SENTINEL_TITLE"));
        assert!(!blob.contains("sentinel.example"));
        assert!(!blob.contains("secret"));

        let _ = harness
            .handler
            .write_setting("activity_exclude_apps".into(), "[]".into())
            .await;
        let kept = harness.activity_prefs.snapshot();
        assert_eq!(kept.exclude_apps, vec!["SENTINEL_APP".to_string()]);
        assert_eq!(kept.exclude_titles, vec!["SENTINEL_TITLE".to_string()]);
        assert_eq!(harness.activity_prefs.generation(), initial_gen + 1);

        let _ = harness
            .handler
            .write_setting("activity_source".into(), "false".into())
            .await;
        let stopped = harness.activity_prefs.snapshot();
        assert!(!stopped.source);
        assert_eq!(stopped.exclude_apps, vec!["SENTINEL_APP".to_string()]);
        assert_eq!(stopped.exclude_titles, vec!["SENTINEL_TITLE".to_string()]);
        assert_eq!(harness.activity_prefs.generation(), initial_gen + 2);

        // write_setting("activity_only", "true") then a second policy write: decoded document restart_required is still true, and effective framerate is still 4.
        let _ = harness
            .handler
            .write_setting("activity_only".into(), "true".into())
            .await;

        let doc2 = harness
            .handler
            .write_setting("activity_policy".into(), policy_json)
            .await;
        let model2 = decode_control_str(&doc2).unwrap();
        assert_eq!(model2.restart_required, Some(true));
        assert_eq!(model2.effective.as_ref().unwrap().capture_framerate, 4);

        // Keeping activity on through the pause is one more choice in the policy,
        // and it takes effect without a restart.
        assert!(!harness.activity_prefs.runs_through_pause());
        let doc3 = harness
            .handler
            .write_setting(
                "activity_policy".into(),
                serde_json::json!({"activity_through_pause": true}).to_string(),
            )
            .await;
        assert!(harness.activity_prefs.runs_through_pause());
        let model3 = decode_control_str(&doc3).unwrap();
        assert!(model3.settings.activity_through_pause);
        assert!(model3.effective.as_ref().unwrap().activity_through_pause);

        harness.shutdown().await;
    }
}
