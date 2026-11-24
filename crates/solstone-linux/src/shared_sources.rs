// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{config::Config, private_link::LinkOutcome, upload::UploadClient};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};

const FILE: &str = "known-sources.json";
#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub(crate) enum Advertisement {
    #[default]
    Unknown,
    Unsupported,
    Available(Option<String>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Entry {
    source: &'static str,
    state: &'static str,
    detail: Option<String>,
}

pub(crate) struct LocalDesktop {
    pub session: crate::desktop_session::SessionPresence,
    pub paused: bool,
    pub screen_needs_attention: bool,
}

pub(crate) fn local(
    config: &Config,
    desktop: LocalDesktop,
    vault: &crate::obsidian::ObsidianHandle,
    shots: &crate::screenshots::ScreenshotsHandle,
    terminal: bool,
    browser: bool,
) -> Vec<Entry> {
    use crate::obsidian::State;
    let mut rows = Vec::new();
    let mut add = |source, state, detail| {
        rows.push(Entry {
            source,
            state,
            detail,
        })
    };
    if let Some(v) = crate::obsidian::decode_document(&vault.state_json()) {
        let state = match v.status.state {
            State::On => Some("on"),
            State::Paused => Some("paused"),
            State::NeedsAttention => Some("needs_attention"),
            _ => None,
        };
        if let Some(state) = state {
            add("obsidian", state, v.status.vault);
        }
    }
    if let Some(s) = crate::screenshots::decode_document(&shots.state_json())
        && s.mode == crate::screenshots::Mode::On
        && s.journal_lists != Some(false)
    {
        add(
            "screenshots",
            if s.folder_gone {
                "needs_attention"
            } else if s.paused {
                "paused"
            } else {
                "on"
            },
            None,
        );
    }
    let active = if desktop.paused { "paused" } else { "on" };
    if terminal {
        use crate::terminal::state::{EngineState, TerminalState};
        if let Some(t) = TerminalState::read(&config.state_dir()) {
            let state = match t.state {
                EngineState::Off => None,
                EngineState::Paused => Some("paused"),
                EngineState::Running => Some("on"),
                _ => Some("needs_attention"),
            };
            if let Some(state) = state {
                add("terminal", state, None);
            }
        }
    }
    if config.desktop_capture
        && !config.activity_only
        && desktop.session == crate::desktop_session::SessionPresence::Live
    {
        add(
            "screen",
            if !desktop.paused && desktop.screen_needs_attention {
                "needs_attention"
            } else {
                active
            },
            None,
        );
        add("audio", active, None);
    }
    if browser {
        add("browsers", active, None);
    }
    rows
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Known {
    source: String,
    detail: Option<String>,
    state: Option<String>,
    origin: String,
    declared_at: Option<String>,
    last_synced_at: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Device {
    cid: String,
    display_label: String,
    device_type: Option<String>,
    platform: Option<String>,
    device_last_seen_at: Option<String>,
    sources: Vec<Known>,
}
#[derive(Deserialize)]
struct Snapshot {
    self_cid: String,
    sources_rev: String,
    devices: Vec<Device>,
}
#[derive(Serialize, Deserialize)]
struct Cache {
    pairing_id: String,
    sources_rev: String,
    fetched_at: i64,
    devices: Vec<Device>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OtherVault {
    pub cid: String,
    pub device: String,
    pub device_type: Option<String>,
    pub platform: Option<String>,
    pub vault: Option<String>,
    pub state: Option<String>,
    pub observed: bool,
    pub at: Option<String>,
}

fn ordered(devices: &[Device]) -> Vec<OtherVault> {
    let mut rows = Vec::new();
    for d in devices {
        for s in d.sources.iter().filter(|s| s.source == "obsidian") {
            let observed = s.origin == "observed";
            let stamp = if observed {
                s.last_synced_at.clone()
            } else {
                [&d.device_last_seen_at, &s.declared_at, &s.last_synced_at]
                    .into_iter()
                    .filter_map(|s| s.as_ref())
                    .filter_map(|s| Some((s.parse::<jiff::Timestamp>().ok()?, s)))
                    .max()
                    .map(|(_, s)| s.clone())
            };
            let timestamp = stamp
                .as_ref()
                .and_then(|s| s.parse::<jiff::Timestamp>().ok());
            rows.push((
                timestamp,
                OtherVault {
                    cid: d.cid.clone(),
                    device: d.display_label.clone(),
                    device_type: d.device_type.clone(),
                    platform: d.platform.clone(),
                    vault: s.detail.clone(),
                    state: s.state.clone(),
                    observed,
                    at: stamp,
                },
            ));
        }
    }
    rows.sort_by(|(ta, a), (tb, b)| {
        tb.cmp(ta)
            .then_with(|| (&a.device, &a.cid, &a.vault).cmp(&(&b.device, &b.cid, &b.vault)))
    });
    rows.into_iter().map(|(_, v)| v).collect()
}

pub fn by_device(rows: &[OtherVault]) -> Vec<Vec<&OtherVault>> {
    let mut groups: Vec<Vec<&OtherVault>> = Vec::new();
    let mut indices = std::collections::HashMap::new();
    for row in rows {
        let index = *indices.entry(row.cid.as_str()).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[index].push(row);
    }
    groups
}

pub fn distinct(rows: &[OtherVault]) -> usize {
    rows.iter().map(|r| &r.cid).collect::<HashSet<_>>().len()
}

pub fn heading(rows: &[OtherVault]) -> String {
    let n = distinct(rows);
    if n == 1 {
        solstone_i18n::text!("linux.sharedsources.one").into()
    } else {
        solstone_i18n::message!("linux.sharedsources.many","count"=>n)
    }
}

pub fn pair(first: &str, second: &str) -> String {
    if first.is_empty() {
        return second.to_owned();
    }
    if second.is_empty() {
        return first.to_owned();
    }
    solstone_i18n::message!("linux.sharedsources.pair","first"=>first,"second"=>second)
}

impl OtherVault {
    pub fn line(&self, include_detail: bool) -> String {
        pair(&self.device, &self.detail_line(include_detail))
    }

    pub fn detail_line(&self, include_detail: bool) -> String {
        self.detail_parts(include_detail)
            .iter()
            .fold(String::new(), |text, part| pair(&text, part))
    }

    /// The vault (when asked for), the device's state when it is paused or needs
    /// attention, and the relative time, in that order. Absent parts are left out.
    pub fn detail_parts(&self, include_detail: bool) -> Vec<String> {
        let mut parts = Vec::new();
        if include_detail && let Some(vault) = self.vault.as_ref().filter(|v| !v.is_empty()) {
            parts.push(vault.clone());
        }
        let state = match self.state.as_deref() {
            Some("paused") => Some(crate::shell_model::StateWord::Paused),
            Some("needs_attention") => Some(crate::shell_model::StateWord::NeedsAttention),
            _ => None,
        };
        if let Some(state) = state {
            parts.push(crate::shell_model::state_label(state).to_owned());
        }
        if let Some(stamp) = &self.at {
            parts.push(crate::shell_model::format_relative(
                stamp,
                &crate::local_time::now(),
            ));
        }
        parts
    }
}

pub(crate) fn forget(root: &Path) {
    if let Err(e) = fs::remove_file(root.join(FILE))
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%e,"could not clear known sources");
    }
}

fn cached(root: &Path, id: &str) -> Option<Cache> {
    let data = crate::private_file::open_regular_readonly(&root.join(FILE)).ok()?;
    let cache: Cache = serde_json::from_reader(data).ok()?;
    (cache.pairing_id == id).then_some(cache)
}

#[derive(Default, Deserialize, Serialize)]
pub struct VaultView {
    pub pairing_id: Option<String>,
    pub rows: Vec<OtherVault>,
}

pub(crate) fn pairing_id(config: &Config) -> Option<String> {
    if crate::journal_mark::journal_mark_held_on_disk(&config.config_dir) {
        return None;
    }
    let cred = crate::private_link::load_credential(&config.config_dir).ok()??;
    Some(crate::private_link::compute_pairing_id(
        &cred.client_cert_pem,
    ))
}

pub(crate) fn visible(config: &Config, refused: bool) -> VaultView {
    if refused {
        return VaultView::default();
    }
    let Some(id) = pairing_id(config) else {
        return VaultView::default();
    };
    let rows = cached(&config.state_dir(), &id).map_or_else(Vec::new, |c| ordered(&c.devices));
    VaultView {
        pairing_id: Some(id),
        rows,
    }
}

#[derive(Clone, Default)]
pub struct Refresh {
    signal: Arc<Notify>,
    reason: Arc<AtomicU8>,
}
impl Refresh {
    pub(crate) fn request(&self, detail: bool) {
        self.reason
            .fetch_max(if detail { 2 } else { 1 }, Ordering::AcqRel);
        self.signal.notify_one();
    }
}

pub(crate) struct Worker {
    local: watch::Sender<Option<Vec<Entry>>>,
    pub refresh: Refresh,
    task: tokio::task::JoinHandle<()>,
}
impl Worker {
    pub(crate) fn start(
        runtime: &tokio::runtime::Runtime,
        config: Config,
        client: Arc<UploadClient>,
    ) -> Self {
        let (local, rx) = watch::channel(None);
        let refresh = Refresh::default();
        let task = runtime.spawn(run(config, client, rx, refresh.clone()));
        Self {
            local,
            refresh,
            task,
        }
    }
    // Wake on the daemon's existing observer pass. The worker compares the
    // status advertisement and local configuration before making any request.
    pub(crate) fn update(&self, entries: Vec<Entry>) {
        self.local.send_replace(Some(entries));
    }
    pub(crate) async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

#[derive(Default)]
struct Progress {
    pairing: String,
    sent: Option<Vec<Entry>>,
    rejected: Option<Vec<Entry>>,
    rev: Option<String>,
    handled_advert: Option<Option<String>>,
    retry: Option<Instant>,
    was_connected: bool,
    unsupported_until: Option<u64>,
    loaded: bool,
    fetched_at: i64,
}

fn pending_declaration<'a>(
    local: &'a Option<Vec<Entry>>,
    progress: &Progress,
) -> Option<&'a [Entry]> {
    let entries = local.as_deref()?;
    if progress.sent.as_deref() == Some(entries) || progress.rejected.as_deref() == Some(entries) {
        return None;
    }
    Some(entries)
}

async fn run(
    config: Config,
    client: Arc<UploadClient>,
    mut input: watch::Receiver<Option<Vec<Entry>>>,
    refresh: Refresh,
) {
    let mut progress = Progress::default();
    loop {
        tokio::select! {
            changed=input.changed()=>if changed.is_err(){break},
            ()=refresh.signal.notified()=>{}
        }
        let reason = refresh.reason.swap(0, Ordering::AcqRel);
        let assoc = client.capture_association();
        let Some(cap) = assoc.capability.clone() else {
            progress = Progress::default();
            continue;
        };
        let facts = cap.facts().snapshot();
        if facts.terminal_revocation
            || client.is_revoked()
            || facts.journal_mark_held
            || facts.pairing_required
        {
            if facts.terminal_revocation || client.is_revoked() {
                forget(&config.state_dir());
            }
            progress = Progress::default();
            continue;
        }
        let id = cap.writer().pairing_id().to_owned();
        if progress.pairing != id {
            progress = Progress {
                pairing: id.clone(),
                ..Default::default()
            };
        }
        let (serial, advert) = cap.sources_advertisement();
        if advert == Advertisement::Unsupported {
            if !progress.loaded || progress.rev.is_some() {
                forget(&config.state_dir());
            }
            progress = Progress {
                pairing: id,
                loaded: true,
                ..Default::default()
            };
            continue;
        }
        let connected = facts.carrier_proven && !facts.transport_unavailable;
        if !connected {
            progress.was_connected = false;
            continue;
        }
        if !progress.was_connected {
            progress.sent = None;
            progress.retry = None;
            progress.was_connected = true;
        }
        let Advertisement::Available(rev) = advert else {
            continue;
        };
        if progress.unsupported_until == Some(serial)
            || progress.retry.is_some_and(|t| Instant::now() < t)
        {
            continue;
        }
        if !progress.loaded {
            if let Some(cache) = cached(&config.state_dir(), &id) {
                progress.rev = Some(cache.sources_rev);
                progress.fetched_at = cache.fetched_at;
            }
            progress.loaded = true;
        }
        let local = input.borrow_and_update().clone();
        if let Some(entries) = pending_declaration(&local, &progress) {
            let answer = cap.shared_source_request(Some(entries)).await;
            if !client.association_is_current(&assoc) {
                continue;
            }
            match answer.status_code() {
                Some(204) => {
                    progress.sent = Some(entries.to_vec());
                    progress.rejected = None;
                }
                Some(400) => {
                    tracing::warn!(
                        "journal rejected source declaration; waiting for a configuration change"
                    );
                    progress.rejected = Some(entries.to_vec());
                }
                _ => {
                    failure(&client, &assoc, &answer, &config, &mut progress, serial);
                    continue;
                }
            }
        }
        if !client.association_is_current(&assoc) || cap.facts().snapshot().terminal_revocation {
            continue;
        }
        let age = jiff::Timestamp::now()
            .as_second()
            .saturating_sub(progress.fetched_at);
        if rev.is_some() && rev == progress.rev {
            progress.handled_advert = Some(rev.clone());
        }
        if progress.handled_advert.as_ref() == Some(&rev)
            && reason != 2
            && !(reason == 1 && !(0..300).contains(&age))
        {
            continue;
        }
        let answer = cap.shared_source_request(None).await;
        if !client.association_is_current(&assoc) {
            continue;
        }
        if let LinkOutcome::Success { status, body } = &answer
            && status.as_u16() == 200
            && let Ok(snapshot) = serde_json::from_slice::<Snapshot>(body)
            && !snapshot.self_cid.is_empty()
        {
            let now = jiff::Timestamp::now().as_second();
            let cache = Cache {
                pairing_id: id,
                sources_rev: snapshot.sources_rev.clone(),
                fetched_at: now,
                devices: snapshot
                    .devices
                    .into_iter()
                    .filter(|d| d.cid != snapshot.self_cid)
                    .collect(),
            };
            let bytes = serde_json::to_vec(&cache).expect("source cache is serializable");
            let saved = client.with_sources_current(&assoc, || {
                crate::private_file::atomic_write_bytes(&config.state_dir().join(FILE), &bytes)
            });
            if matches!(saved, Some(Ok(()))) {
                progress.handled_advert = Some(rev);
                progress.rev = Some(snapshot.sources_rev);
                progress.fetched_at = now;
            } else {
                progress.retry = Some(Instant::now() + Duration::from_secs(30));
            }
        } else {
            failure(&client, &assoc, &answer, &config, &mut progress, serial);
        }
    }
}

fn failure(
    client: &UploadClient,
    assoc: &crate::upload::CapturedAssociation,
    answer: &LinkOutcome,
    config: &Config,
    progress: &mut Progress,
    serial: u64,
) {
    match answer.status_code() {
        Some(403) => {
            client.import_auth_failure(assoc, answer);
        }
        Some(404) => {
            forget(&config.state_dir());
            progress.handled_advert = None;
            progress.unsupported_until = Some(serial);
            progress.rev = None;
            progress.sent = None;
        }
        _ => {
            progress.retry = Some(Instant::now() + Duration::from_secs(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_rows_use_receipt_time_and_two_vaults_count_as_one_device() {
        let devices: Vec<Device> = serde_json::from_value(serde_json::json!([
            {"cid":"c","display_label":"Desk","device_last_seen_at":"2026-11-20T12:00:00Z","sources":[
                {"source":"obsidian","detail":"Older","state":null,"origin":"observed","last_synced_at":"2026-11-19T03:00:00Z"}
            ]},
            {"cid":"b","display_label":"Desk","device_last_seen_at":"2026-11-20T09:00:00Z","sources":[
                {"source":"obsidian","detail":"Z","state":"paused","origin":"declared","declared_at":"2026-11-20T07:00:00Z"},
                {"source":"obsidian","detail":"A","state":"on","origin":"declared","declared_at":"2026-11-20T08:00:00Z"}
            ]}
        ])).unwrap();
        let rows = ordered(&devices);
        assert_eq!(distinct(&rows), 2);
        assert_eq!(
            rows.iter().map(|r| r.vault.as_deref()).collect::<Vec<_>>(),
            [Some("A"), Some("Z"), Some("Older")]
        );
        assert_eq!(rows[0].at.as_deref(), Some("2026-11-20T09:00:00Z"));
        assert_eq!(rows[2].at.as_deref(), Some("2026-11-19T03:00:00Z"));
    }

    #[test]
    fn skips_declaration_until_first_local_update_arrives() {
        let mut progress = Progress::default();
        assert!(pending_declaration(&None, &progress).is_none());

        let empty = Some(Vec::new());
        assert_eq!(pending_declaration(&empty, &progress), Some(&[][..]));

        progress.sent = Some(Vec::new());
        assert!(pending_declaration(&empty, &progress).is_none());

        let populated = Some(vec![Entry {
            source: "screen",
            detail: None,
            state: "on",
        }]);
        assert_eq!(
            pending_declaration(&populated, &progress),
            populated.as_deref()
        );
        progress.rejected = populated.clone();
        assert!(pending_declaration(&populated, &progress).is_none());
    }
}
