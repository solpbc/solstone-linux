// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The folder source on Linux: watches one or more owner-chosen folders, sorts
//! files against the journal's supported list, tracks note edits and one-hour
//! document revisions, and delivers five-minute activity segments over the
//! paired upload path.

pub mod classify;
pub mod engine;
pub(crate) mod local;
pub mod metadata;
pub mod note_diff;
mod structured_exports;
pub mod types;
pub mod wire;

pub use engine::{DeliveryOutcome, Engine, OverlapContext, SegmentDelivery};
pub use types::{
    ActivityVerb, BackfillEstimate, DeliveryMode, EstimateItem, FacetOption, FacetsAvailability,
    FileKind, FolderAttention, FolderKinds, FolderMode, FolderSummary, FoldersDocument,
    OverlapTarget, PathCheckResult, PlatformMetadata, PreviewSummary, SupportedList,
    decode_document,
};
pub use wire::{CAPABILITY, FolderTransport, SOURCE};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use solstone_folder_sync::worker::{
    self as folder_worker, CapabilityCache, CheckSchedule, Event, Wake,
};
use solstone_vault::SystemClock;
use tokio::sync::{mpsc, oneshot, watch};

use crate::screenshots::Span;
use crate::shell_model::StateWord;
use crate::upload::UploadClient;

const LOOK_EVERY: Duration = Duration::from_secs(5);
const CAPABILITY_RETRY: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Canonical state word for one configured folder.
#[must_use]
pub fn folder_word(folder: &FolderSummary) -> StateWord {
    if folder.attention.is_some() {
        return StateWord::NeedsAttention;
    }
    match folder.mode {
        FolderMode::SettingUp => StateWord::SettingUp,
        FolderMode::Off => StateWord::Off,
        FolderMode::Paused => StateWord::Paused,
        FolderMode::On if folder.paused_by_computer => StateWord::Paused,
        FolderMode::On => StateWord::On,
    }
}

/// Worst-signal-wins tile rollup across all configured folders.
#[must_use]
pub fn word(doc: &FoldersDocument) -> StateWord {
    if doc.folders.is_empty() {
        return StateWord::ReadyToSetUp;
    }
    if doc.folders.iter().any(|f| f.attention.is_some()) {
        return StateWord::NeedsAttention;
    }
    if doc.folders.iter().any(|f| f.mode == FolderMode::SettingUp) {
        return StateWord::SettingUp;
    }
    if doc
        .folders
        .iter()
        .any(|f| f.mode == FolderMode::On && !f.paused_by_computer)
    {
        return StateWord::On;
    }
    if doc
        .folders
        .iter()
        .any(|f| f.mode == FolderMode::Paused || (f.mode == FolderMode::On && f.paused_by_computer))
    {
        return StateWord::Paused;
    }
    StateWord::Off
}

/// Sub-line for the single `folders` tile on the home deck.
#[must_use]
pub fn tile_line(doc: &FoldersDocument) -> Option<String> {
    use crate::copy::folders as copy;
    use crate::screenshots::group;

    if doc.folders.is_empty() {
        return None;
    }
    let attn: Vec<&FolderSummary> = doc
        .folders
        .iter()
        .filter(|f| f.attention.is_some())
        .collect();
    if attn.len() >= 2 {
        return Some(copy::f20(&group(attn.len() as u64)));
    }
    if let Some(one) = attn.first() {
        return Some(match one.attention {
            Some(FolderAttention::Missing) => copy::e1(&one.name),
            Some(FolderAttention::Unreadable) | None => copy::e2(&one.name),
        });
    }
    if let Some(setup) = doc.folders.iter().find(|f| f.mode == FolderMode::SettingUp) {
        return Some(copy::f14(&setup.name));
    }
    if doc.computer_paused
        && doc
            .folders
            .iter()
            .any(|f| matches!(f.mode, FolderMode::On | FolderMode::Paused))
    {
        return Some(copy::f15b().to_owned());
    }
    let active: Vec<&FolderSummary> = doc
        .folders
        .iter()
        .filter(|f| f.mode == FolderMode::On)
        .collect();
    let paused: Vec<&FolderSummary> = doc
        .folders
        .iter()
        .filter(|f| f.mode == FolderMode::Paused)
        .collect();

    if let Some(bf) = active.iter().find(|f| f.progress.is_some())
        && let Some([done, total]) = bf.progress
    {
        return Some(copy::f9(&bf.name, &group(done), &group(total)));
    }
    if !active.is_empty() && !doc.paired {
        return Some(copy::f12().to_owned());
    }
    let total_waiting: u64 = active.iter().map(|f| f.waiting).sum();
    if total_waiting > 0 && (doc.offline || doc.journal_lists == Some(false)) {
        return Some(copy::f11(&group(total_waiting)));
    }
    if !active.is_empty() && !paused.is_empty() {
        return Some(copy::f8(
            &group(active.len() as u64),
            &group(paused.len() as u64),
        ));
    }
    match active.len() {
        1 => Some(active[0].name.clone()),
        2 => Some(copy::f7_two(&active[0].name, &active[1].name)),
        n if n >= 3 => Some(copy::f7_many(&group(n as u64))),
        _ => {
            if !paused.is_empty() {
                Some(copy::f15b().to_owned())
            } else {
                doc.folders.first().map(|f| f.name.clone())
            }
        }
    }
}

/// JSON request payload sent over D-Bus `FoldersAction`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FolderActionRequest {
    BeginSetup {
        path: String,
        #[serde(default)]
        overlap: Option<OverlapTarget>,
    },
    Repoint {
        id: String,
        path: String,
    },
    SetKinds {
        id: String,
        kinds: FolderKinds,
    },
    SetFacet {
        id: String,
        facet: Option<FacetOption>,
    },
    Preview {
        id: String,
        span: Span,
    },
    Confirm {
        id: String,
        span: Span,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    TurnOff {
        id: String,
    },
    TurnOn {
        id: String,
    },
    Remove {
        id: String,
    },
    Retry {
        id: String,
    },
    RefreshFacets,
}

enum Command {
    CheckPath {
        path: String,
        reply: oneshot::Sender<PathCheckResult>,
    },
    Action {
        req: FolderActionRequest,
        reply: oneshot::Sender<FoldersDocument>,
    },
}

/// Handle to the running folder source worker.
#[derive(Clone, Default)]
pub struct FoldersHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<Command>,
    doc: Arc<Mutex<FoldersDocument>>,
    wake: Arc<Wake>,
    paused_tx: watch::Sender<bool>,
    shutdown_tx: watch::Sender<bool>,
}

impl FoldersHandle {
    #[must_use]
    pub fn state_json(&self) -> String {
        let Some(ref inner) = self.inner else {
            return FoldersDocument {
                v: 1,
                ..FoldersDocument::default()
            }
            .encoded();
        };
        inner
            .doc
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .encoded()
    }

    pub async fn check_path(&self, path: String) -> String {
        let Some(ref inner) = self.inner else {
            return serde_json::to_string(&PathCheckResult::Refused { path })
                .unwrap_or_else(|_| "{}".into());
        };
        let (tx, rx) = oneshot::channel();
        if inner
            .commands
            .send(Command::CheckPath {
                path: path.clone(),
                reply: tx,
            })
            .await
            .is_err()
        {
            return serde_json::to_string(&PathCheckResult::Refused { path })
                .unwrap_or_else(|_| "{}".into());
        }
        let result = rx.await.unwrap_or(PathCheckResult::Refused { path });
        serde_json::to_string(&result).unwrap_or_else(|_| "{}".into())
    }

    pub async fn action(&self, payload_json: &str) -> String {
        let Ok(req) = serde_json::from_str::<FolderActionRequest>(payload_json) else {
            return self.state_json();
        };
        let Some(ref inner) = self.inner else {
            return self.state_json();
        };
        let (tx, rx) = oneshot::channel();
        if inner
            .commands
            .send(Command::Action { req, reply: tx })
            .await
            .is_err()
        {
            return self.state_json();
        }
        rx.await
            .map(|doc| doc.encoded())
            .unwrap_or_else(|_| self.state_json())
    }

    pub fn set_computer_paused(&self, paused: bool) {
        if let Some(ref inner) = self.inner {
            let _ = inner.paused_tx.send(paused);
            inner.wake.nudge();
        }
    }

    pub fn restart_setup(&self) {
        if let Some(ref inner) = self.inner {
            inner.wake.restart();
        }
    }

    pub async fn shutdown(&self, task: tokio::task::JoinHandle<()>) {
        if let Some(ref inner) = self.inner {
            let _ = inner.shutdown_tx.send(true);
            inner.wake.nudge();
        }
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, task).await;
    }
}

/// Callback returning the current Obsidian vault & screenshots paths for
/// overlap detection.
pub type OverlapProvider = Arc<dyn Fn() -> OverlapContext + Send + Sync>;

/// Start the live folder source background worker.
pub fn start(
    upload: Arc<UploadClient>,
    state_dir: PathBuf,
    home: PathBuf,
    intake_state_dir: PathBuf,
    overlap_provider: OverlapProvider,
) -> (FoldersHandle, Option<tokio::task::JoinHandle<()>>) {
    let transport: Arc<dyn FolderTransport> = Arc::new(wire::LinkTransport::new(upload));
    let clock = Arc::new(SystemClock);
    start_with(
        transport,
        state_dir,
        home,
        intake_state_dir,
        clock,
        overlap_provider,
    )
}

pub fn start_with(
    transport: Arc<dyn FolderTransport>,
    state_dir: PathBuf,
    home: PathBuf,
    intake_state_dir: PathBuf,
    clock: Arc<dyn solstone_vault::Clock>,
    overlap_provider: OverlapProvider,
) -> (FoldersHandle, Option<tokio::task::JoinHandle<()>>) {
    let mut engine = Engine::open(state_dir, home, clock);
    let intake_dir = intake_state_dir;
    engine.set_intake_hook(Arc::new(move |id| {
        crate::source_intake::add_folder_item(&intake_dir, id);
    }));

    let initial_doc = engine.document(
        Vec::new(),
        FacetsAvailability::Unpaired,
        false,
        transport.is_paired(),
        transport.is_offline(),
        None,
    );
    let doc = Arc::new(Mutex::new(initial_doc));
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let wake = Arc::new(Wake::default());
    let (paused_tx, paused_rx) = watch::channel(false);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let handle = FoldersHandle {
        inner: Some(Arc::new(HandleInner {
            commands: cmd_tx,
            doc: Arc::clone(&doc),
            wake: Arc::clone(&wake),
            paused_tx,
            shutdown_tx,
        })),
    };

    let task = tokio::spawn(run_worker(
        engine,
        transport,
        doc,
        cmd_rx,
        wake,
        paused_rx,
        shutdown_rx,
        overlap_provider,
    ));
    (handle, Some(task))
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    engine: Engine,
    transport: Arc<dyn FolderTransport>,
    doc_slot: Arc<Mutex<FoldersDocument>>,
    mut commands: mpsc::Receiver<Command>,
    wake: Arc<Wake>,
    paused_rx: watch::Receiver<bool>,
    shutdown_rx: watch::Receiver<bool>,
    overlap_provider: OverlapProvider,
) {
    let mut state = Some(engine);
    let mut cap_cache = CapabilityCache::default();
    let mut schedule = CheckSchedule::default();
    let mut facets: Vec<FacetOption> = Vec::new();
    let mut facets_state = FacetsAvailability::Unpaired;
    let mut supported_until = None;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }
        let computer_paused = *paused_rx.borrow();

        if wake.take_restart() {
            cap_cache.clear();
            supported_until = None;
            facets.clear();
            let _ = folder_worker::with_state(&mut state, |eng| eng.restart_setup()).await;
        }

        if schedule.due() {
            let paired = transport.is_paired();
            let listed = if paired {
                let listed = cap_cache.check(&*transport, CAPABILITY_RETRY).await;
                if listed
                    && supported_until.is_none_or(|until| std::time::Instant::now() >= until)
                    && let Some(sup) = transport.fetch_supported().await
                {
                    let _ = folder_worker::with_state(&mut state, move |eng| {
                        eng.set_supported_list(sup);
                    })
                    .await;
                    supported_until = Some(std::time::Instant::now() + Duration::from_secs(300));
                }
                listed
            } else {
                false
            };
            let _ = folder_worker::with_state(&mut state, |eng| eng.scan_all()).await;
            if paired && listed && supported_until.is_some() {
                while !*shutdown_rx.borrow() && !wake.restart_pending() {
                    let paused_now = *paused_rx.borrow();
                    let next = folder_worker::with_state(&mut state, move |eng| {
                        eng.next_delivery(paused_now)
                    })
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                    let Some(delivery) = next else {
                        break;
                    };
                    let reply = folder_worker::deliver(&*transport, &delivery, None)
                        .await
                        .flatten();
                    let outcome = reply.unwrap_or(DeliveryOutcome::Hold);
                    let held = matches!(outcome, DeliveryOutcome::Hold);
                    let _ = folder_worker::with_state(&mut state, move |eng| {
                        eng.record_outcome(&delivery, outcome);
                    })
                    .await;
                    if held {
                        break;
                    }
                }
            }
            schedule.defer(LOOK_EVERY);
        }

        publish_snapshot(
            &mut state,
            &doc_slot,
            &facets,
            facets_state,
            computer_paused,
            &*transport,
            cap_cache.listed(),
        )
        .await;

        let delay = schedule.remaining().min(LOOK_EVERY);
        match folder_worker::wait(&mut commands, &wake, delay).await {
            Event::Closed => break,
            Event::Wake => continue,
            Event::Command(Command::CheckPath { path, reply }) => {
                let ctx = overlap_provider();
                let p = PathBuf::from(path);
                let result =
                    folder_worker::with_state(&mut state, move |eng| eng.check_path(&p, &ctx))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(PathCheckResult::Refused {
                            path: String::new(),
                        });
                let _ = reply.send(result);
            }
            Event::Command(Command::Action { req, reply }) => {
                match req {
                    FolderActionRequest::RefreshFacets => {
                        let (st, list) = transport.fetch_facets().await;
                        facets_state = st;
                        facets = list;
                    }
                    FolderActionRequest::Preview { id, span } => {
                        let id_clone = id.clone();
                        let preview = folder_worker::with_state(&mut state, move |eng| {
                            eng.preview_folder_with_items(&id_clone, span)
                        })
                        .await
                        .ok()
                        .flatten()
                        .flatten();
                        if let Some((_summary, items)) = preview
                            && transport.is_paired()
                            && !items.is_empty()
                            && let Some(journal_est) =
                                transport.fetch_estimate(id.clone(), items).await
                        {
                            let id_set = id.clone();
                            let _ = folder_worker::with_state(&mut state, move |eng| {
                                eng.set_journal_estimate(&id_set, journal_est);
                            })
                            .await;
                        }
                        if matches!(
                            facets_state,
                            FacetsAvailability::Unpaired | FacetsAvailability::Offline
                        ) && transport.is_paired()
                        {
                            let (st, list) = transport.fetch_facets().await;
                            facets_state = st;
                            facets = list;
                        }
                    }
                    other => {
                        let _ = folder_worker::with_state(&mut state, move |eng| {
                            apply_action(eng, other);
                        })
                        .await;
                        schedule.defer(Duration::ZERO);
                    }
                }
                let paused_now = *paused_rx.borrow();
                let snap = publish_snapshot(
                    &mut state,
                    &doc_slot,
                    &facets,
                    facets_state,
                    paused_now,
                    &*transport,
                    cap_cache.listed(),
                )
                .await;
                let _ = reply.send(snap);
            }
        }
    }
}

fn apply_action(engine: &mut Engine, req: FolderActionRequest) {
    match req {
        FolderActionRequest::BeginSetup { path, overlap } => {
            let _ = engine.begin_setup(PathBuf::from(path).as_path(), overlap);
        }
        FolderActionRequest::Repoint { id, path } => {
            let _ = engine.repoint_folder(&id, PathBuf::from(path).as_path());
        }
        FolderActionRequest::SetKinds { id, kinds } => {
            engine.set_kinds(&id, kinds);
        }
        FolderActionRequest::SetFacet { id, facet } => {
            engine.set_facet(&id, facet);
        }
        FolderActionRequest::Confirm { id, span } => {
            let _ = engine.confirm_folder(&id, span);
        }
        FolderActionRequest::Pause { id } => {
            engine.pause_folder(&id);
        }
        FolderActionRequest::Resume { id } => {
            engine.resume_folder(&id);
        }
        FolderActionRequest::TurnOff { id } => {
            engine.turn_off_folder(&id);
        }
        FolderActionRequest::TurnOn { id } => {
            engine.turn_on_folder(&id);
        }
        FolderActionRequest::Remove { id } => {
            engine.remove_folder(&id);
        }
        FolderActionRequest::Retry { .. } => {
            engine.scan_all();
        }
        FolderActionRequest::Preview { .. } | FolderActionRequest::RefreshFacets => {}
    }
}

async fn publish_snapshot(
    state: &mut Option<Engine>,
    doc_slot: &Arc<Mutex<FoldersDocument>>,
    facets: &[FacetOption],
    facets_state: FacetsAvailability,
    computer_paused: bool,
    transport: &dyn FolderTransport,
    journal_lists: Option<bool>,
) -> FoldersDocument {
    let facets_owned = facets.to_vec();
    let paired = transport.is_paired();
    let offline = transport.is_offline();
    let snap = folder_worker::with_state(state, move |eng| {
        eng.document(
            facets_owned,
            facets_state,
            computer_paused,
            paired,
            offline,
            journal_lists,
        )
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| FoldersDocument {
        v: 1,
        ..FoldersDocument::default()
    });
    *doc_slot.lock().unwrap_or_else(|p| p.into_inner()) = snap.clone();
    snap
}

/// Read configured folders directly from disk without starting the worker
/// (used by `solstone-linux status` when the daemon is not running).
#[must_use]
pub fn read_disk_document(state_dir: &Path, home: PathBuf, paired: bool) -> FoldersDocument {
    let engine = Engine::open(state_dir.to_path_buf(), home, Arc::new(SystemClock));
    engine.document(
        Vec::new(),
        if paired {
            FacetsAvailability::Offline
        } else {
            FacetsAvailability::Unpaired
        },
        false,
        paired,
        false,
        None,
    )
}
