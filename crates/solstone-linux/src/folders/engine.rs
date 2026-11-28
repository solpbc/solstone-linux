// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Stateful multi-folder engine: recursive complete-listing walks, sorting,
//! five-minute windows, note net-edit tracking, one-hour document revision
//! cool-down, rename matching, delete-before-send, pause-without-skipping,
//! and pairing-safe durable state (`state/folders/ledger.json`).
//!
//! Zero journal identity is stored in `ledger.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use solstone_folder_sync::files::{
    ListingError, is_placeholder, settled, sha256_hex, visit_directory,
};
use solstone_vault::{Clock, Window};

use crate::private_file::atomic_write_bytes;
use crate::screenshots::Span;
use crate::screenshots::proof::{self, Recents};

use super::classify::{
    OverlapOutcome, classify_file, detect_overlap, folder_label, folder_name, is_junk_entry,
    is_partial_download, normalize_path,
};
use super::local;
use super::metadata::{collect_metadata, extension_lower, system_time_ms};
use super::note_diff::net_line_diff;
use super::types::{
    ActivityVerb, BackfillEstimate, DeliveryMode, EstimateItem, FacetOption, FileKind,
    FolderAttention, FolderKinds, FolderMode, FolderSummary, FoldersDocument, OverlapTarget,
    PathCheckResult, PlatformMetadata, PreviewSummary, SupportedList,
};

const LEDGER_FILE: &str = "ledger.json";
const LEDGER_VERSION: u32 = 1;
const SETTLE_MS: i64 = 2_000;
const DAY_MS: i64 = 86_400_000;
const RETRY_MS: [i64; 4] = [30_000, 120_000, 600_000, 1_800_000];
/// Maximum companion content files per protocol-v3 folder segment upload
/// (64 files total including the activity JSONL).
pub const MAX_UPLOAD_CONTENT_PARTS: usize = 63;
/// Maximum preview items sent to `POST /app/devices/ingest/folder/estimate`.
pub const MAX_ESTIMATE_ITEMS: usize = 50_000;

/// Callback notified with a unique item identity when a folder item is taken in.
pub type IntakeHook = Arc<dyn Fn(&str) + Send + Sync>;

/// One event inside a ready five-minute folder segment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PendingEvent {
    pub id: String,
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub verb: ActivityVerb,
    pub kind: FileKind,
    pub delivery: DeliveryMode,
    pub event_ms: i64,
    #[serde(default)]
    pub observed_ms: i64,
    pub offset_s: i32,
    pub window: Window,
    pub window_end_ms: i64,
    pub metadata: PlatformMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Note text or net diff payload when `kind == FileKind::Note` and `delivery == DeliveryMode::Content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_payload: Option<String>,
    #[serde(default)]
    pub note_is_diff: bool,
    #[serde(default)]
    pub is_revision: bool,
    #[serde(default)]
    pub backfill: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    #[serde(default)]
    pub tries: u32,
}

/// Durable per-file record held per configured folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrackedFile {
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inode: Option<(u64, u64)>,
    pub size_bytes: u64,
    pub mtime_ns: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub kind: FileKind,
    pub delivery: DeliveryMode,
    /// True once the journal acknowledged this file (so a later local deletion
    /// emits `removed` rather than silently dropping).
    #[serde(default)]
    pub delivered_to_journal: bool,
    /// True when the file existed before `span_from_ms` and was intentionally
    /// excluded by the owner's range choice.
    #[serde(default)]
    pub excluded_before_span: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sent_hash: Option<String>,
    /// Baseline note text last sent/acknowledged for computing net window diffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_note_text: Option<String>,
    /// When an already-delivered document or transcript was last saved with a
    /// new hash, waiting for the one-hour cool-down before emitting a revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision_save_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision_hash: Option<String>,
}

/// Durable record for one configured folder.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderRecord {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub label: String,
    pub mode: FolderMode,
    #[serde(default)]
    pub returning: bool,
    #[serde(default)]
    pub pause_during_setup: bool,
    #[serde(default)]
    pub kinds: FolderKinds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<FacetOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_from_ms: Option<i64>,
    #[serde(default)]
    pub folder_seen: bool,
    #[serde(default)]
    pub files: BTreeMap<String, TrackedFile>,
    #[serde(default)]
    pub pending: Vec<PendingEvent>,
    #[serde(default)]
    pub backfill_total: u64,
    #[serde(default)]
    pub backfill_done: u64,
    #[serde(default)]
    pub items_taken_in: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_taken_in_ms: Option<i64>,
    #[serde(default)]
    pub fallback_to_details: u64,
    #[serde(skip)]
    pub attention: Option<FolderAttention>,
    #[serde(skip)]
    pub last_checked_ms: Option<i64>,
    #[serde(skip)]
    pub cant_read: u64,
    #[serde(default)]
    pub limited_mount: bool,
    #[serde(skip)]
    pub preview: Option<PreviewSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Ledger {
    v: u32,
    #[serde(default)]
    folders: Vec<FolderRecord>,
}

/// One staged five-minute segment ready to send to the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentDelivery {
    pub folder_id: String,
    pub folder_name: String,
    pub folder_root: PathBuf,
    pub facet: Option<String>,
    pub kinds: FolderKinds,
    pub window: Window,
    pub events: Vec<PendingEvent>,
    pub content_files: Vec<StagedContentFile>,
}

/// One content file included in a `SegmentDelivery`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedContentFile {
    pub event_id: String,
    pub rel_path: String,
    pub kind: FileKind,
    pub content_hash: Option<String>,
    pub note_payload: Option<String>,
    pub note_is_diff: bool,
    pub is_revision: bool,
    pub needs_image_convert: bool,
}

/// Outcome of delivering one five-minute segment to the journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    /// Segment accepted (`ok`, `duplicate`, `collision`, or `segment_removed`).
    Delivered,
    /// Segment accepted (`200 ok`), but the journal refused content for specific
    /// relative paths. Re-queues those paths as details-only (`part: null`) and
    /// increments `fallback_to_details`.
    DeliveredWithRefusedContent(Vec<String>),
    /// Every item in the upload failed per-file validation (`400 folder_file_invalid`).
    /// Re-queues refused paths as details-only (`part: null`) and increments
    /// `fallback_to_details`.
    AllFilesRefused(Vec<String>),
    /// Permanent contract rejection (`400 folder_invalid`): drop malformed batch.
    InvalidPermanent,
    /// Transient failure, offline, `503 journal_read_failed`, or `400 folder_ahead`:
    /// retry with backoff.
    Hold,
}

/// Context of other configured sources used for overlap detection.
#[derive(Clone, Debug, Default)]
pub struct OverlapContext {
    pub obsidian: Option<(PathBuf, String)>,
    pub screenshots: Option<(PathBuf, String)>,
}

struct WalkedFile {
    resident: bool,
    rel_path: String,
    inode: Option<(u64, u64)>,
    size_bytes: u64,
    mtime_ns: i128,
    metadata: PlatformMetadata,
    is_screenshot: bool,
}

struct WalkOutcome {
    limited_mount: bool,
    files: Vec<WalkedFile>,
    unreadable: u64,
    deferred_partials: usize,
    complete: bool,
}

fn apply_kind_choices(folder: &mut FolderRecord) {
    let kinds = folder.kinds;
    folder.pending.retain_mut(|event| {
        if kinds.enabled_for(event.kind) {
            return true;
        }
        if !kinds.other {
            return false;
        }
        event.delivery = DeliveryMode::MetadataOnly;
        event.content_hash = None;
        event.note_payload = None;
        true
    });
    if !kinds.other {
        for file in folder
            .files
            .values_mut()
            .filter(|f| !kinds.enabled_for(f.kind))
        {
            file.pending_revision_hash = None;
            file.pending_revision_save_ms = None;
        }
    }
}

pub struct Engine {
    state_dir: PathBuf,
    home: PathBuf,
    recents_path: Option<PathBuf>,
    clock: Arc<dyn Clock>,
    ledger: Ledger,
    supported: SupportedList,
    intake_hook: Option<IntakeHook>,
}

impl Engine {
    pub fn open(state_dir: PathBuf, home: PathBuf, clock: Arc<dyn Clock>) -> Self {
        let ledger_path = state_dir.join(LEDGER_FILE);
        let mut ledger = fs::read(&ledger_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ledger>(&bytes).ok())
            .filter(|l| l.v == LEDGER_VERSION)
            .unwrap_or_else(|| Ledger {
                v: LEDGER_VERSION,
                folders: Vec::new(),
            });
        for folder in &mut ledger.folders {
            folder.name = super::classify::bounded_folder_name(&folder.name);
            apply_kind_choices(folder);
        }
        let recents_path = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"))
            .join("recently-used.xbel");
        Self {
            state_dir,
            home,
            recents_path: Some(recents_path),
            clock,
            ledger,
            supported: SupportedList::default(),
            intake_hook: None,
        }
    }

    pub fn set_intake_hook(&mut self, hook: IntakeHook) {
        self.intake_hook = Some(hook);
    }

    pub fn set_recents_path(&mut self, path: Option<PathBuf>) {
        self.recents_path = path;
    }

    pub fn set_supported_list(&mut self, supported: SupportedList) {
        if self.supported == supported {
            return;
        }
        self.supported = supported;
        let mut changed = false;
        for folder in &mut self.ledger.folders {
            for event in &mut folder.pending {
                if event.kind != FileKind::Other
                    || !matches!(event.verb, ActivityVerb::Added | ActivityVerb::Edited)
                {
                    continue;
                }
                let Some(ext) = event.metadata.extension.as_deref() else {
                    continue;
                };
                if !self.supported.document_extensions.contains(ext)
                    && !self.supported.transcript_extensions.contains(ext)
                {
                    continue;
                }
                let path = Path::new(&event.rel_path);
                let Ok(handle) = local::object(&folder.path, path) else {
                    continue;
                };
                if !local::content_available(&handle) {
                    continue;
                }
                let Ok(meta) = handle.metadata() else {
                    continue;
                };
                if !meta.is_file()
                    || meta.len() != event.metadata.size_bytes
                    || meta.modified().ok().map(system_time_ms) != Some(event.metadata.modified_ms)
                {
                    continue;
                }
                let bytes = if self.supported.transcript_extensions.contains(ext)
                    && meta.len() <= self.supported.max_note_bytes
                {
                    local::read_file(&folder.path, &event.rel_path, self.supported.max_note_bytes)
                        .ok()
                } else {
                    None
                };
                let (kind, delivery) = classify_file(
                    Some(ext),
                    meta.len(),
                    false,
                    bytes.as_deref(),
                    folder.kinds,
                    &self.supported,
                );
                if delivery != DeliveryMode::Content {
                    continue;
                }
                let hash = bytes.as_deref().map(sha256_hex).or_else(|| {
                    local::hash_file(
                        &folder.path,
                        &event.rel_path,
                        self.supported.max_content_bytes,
                    )
                    .ok()
                });
                let Some(hash) = hash else {
                    continue;
                };
                event.kind = kind;
                event.delivery = delivery;
                event.content_hash = Some(hash.clone());
                if let Some(file) = folder.files.get_mut(&event.rel_path) {
                    file.kind = kind;
                    file.delivery = delivery;
                    file.content_hash = Some(hash);
                }
                changed = true;
            }
        }
        if changed {
            let _ = self.save();
        }
    }

    #[must_use]
    pub fn supported_list(&self) -> &SupportedList {
        &self.supported
    }

    #[must_use]
    pub fn folders(&self) -> &[FolderRecord] {
        &self.ledger.folders
    }

    fn save(&self) -> io::Result<()> {
        fs::create_dir_all(&self.state_dir)?;
        let bytes = serde_json::to_vec_pretty(&self.ledger)
            .map_err(|err| io::Error::other(err.to_string()))?;
        atomic_write_bytes(&self.state_dir.join(LEDGER_FILE), &bytes)
            .map_err(|err| io::Error::other(err.to_string()))
    }

    /// Check whether a candidate folder can be opened, already exists, or
    /// overlaps an Obsidian vault, screenshots folder, or another folder source.
    #[must_use]
    pub fn check_path(&self, raw_path: &Path, overlap_ctx: &OverlapContext) -> PathCheckResult {
        if !raw_path.is_dir() || fs::read_dir(raw_path).is_err() {
            return PathCheckResult::Refused {
                path: raw_path.display().to_string(),
            };
        }
        let canonical = normalize_path(raw_path);
        let name = folder_name(&canonical);
        let label = folder_label(&canonical, &self.home);
        let existing: Vec<(&str, &Path, &str)> = self
            .ledger
            .folders
            .iter()
            .map(|f| (f.id.as_str(), f.path.as_path(), f.name.as_str()))
            .collect();
        let obs = overlap_ctx
            .obsidian
            .as_ref()
            .map(|(p, n)| (p.as_path(), n.as_str()));
        let shots = overlap_ctx
            .screenshots
            .as_ref()
            .map(|(p, n)| (p.as_path(), n.as_str()));
        match detect_overlap(&canonical, obs, shots, &existing) {
            OverlapOutcome::AlreadyAdded { id, name } => PathCheckResult::AlreadyAdded {
                folder_id: id,
                name,
            },
            OverlapOutcome::Overlap(target) => PathCheckResult::Overlaps {
                path: canonical.display().to_string(),
                name,
                label,
                target,
            },
            OverlapOutcome::None => PathCheckResult::Ready {
                path: canonical.display().to_string(),
                name,
                label,
            },
        }
    }

    /// Add a new folder in `SettingUp` mode (nothing leaves the device until
    /// `confirm_folder` is called). Returns the folder's id, or `Err` if the
    /// folder cannot be opened.
    pub fn begin_setup(
        &mut self,
        raw_path: &Path,
        overlap: Option<OverlapTarget>,
    ) -> Result<String, PathCheckResult> {
        if !raw_path.is_dir() || fs::read_dir(raw_path).is_err() {
            return Err(PathCheckResult::Refused {
                path: raw_path.display().to_string(),
            });
        }
        let canonical = normalize_path(raw_path);
        if let Some(existing) = self.ledger.folders.iter().find(|f| f.path == canonical) {
            return Ok(existing.id.clone());
        }
        let name = folder_name(&canonical);
        let id = make_folder_id(&name, &canonical);
        let label = folder_label(&canonical, &self.home);
        let record = FolderRecord {
            id: id.clone(),
            path: canonical,
            name,
            label,
            mode: FolderMode::SettingUp,
            returning: false,
            pause_during_setup: false,
            kinds: FolderKinds::default(),
            facet: None,
            overlap,
            span_from_ms: None,
            folder_seen: true,
            files: BTreeMap::new(),
            pending: Vec::new(),
            backfill_total: 0,
            backfill_done: 0,
            items_taken_in: 0,
            last_taken_in_ms: None,
            fallback_to_details: 0,
            attention: None,
            last_checked_ms: None,
            cant_read: 0,
            limited_mount: false,
            preview: None,
        };
        self.ledger.folders.push(record);
        let _ = self.save();
        Ok(id)
    }

    /// Repoint an existing folder whose directory moved or was replaced.
    pub fn repoint_folder(&mut self, folder_id: &str, new_path: &Path) -> Result<(), String> {
        if !new_path.is_dir() || fs::read_dir(new_path).is_err() {
            return Err("refused".into());
        }
        let canonical = normalize_path(new_path);
        let name = folder_name(&canonical);
        let label = folder_label(&canonical, &self.home);
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return Err("not_found".into());
        };
        folder.path = canonical;
        folder.name = name;
        folder.label = label;
        folder.folder_seen = true;
        folder.attention = None;
        folder.preview = None;
        let _ = self.save();
        Ok(())
    }

    /// Update enabled kinds on a folder (during setup or while active; changes
    /// apply going forward and never remove what is already in the journal).
    pub fn set_kinds(&mut self, folder_id: &str, kinds: FolderKinds) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.kinds = kinds;
        apply_kind_choices(folder);
        folder.preview = None;
        let _ = self.save();
        true
    }

    /// Set or clear the optional facet on a folder.
    pub fn set_facet(&mut self, folder_id: &str, facet: Option<FacetOption>) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.facet = facet;
        let _ = self.save();
        true
    }

    /// Scan a folder (recursively) and compute preview counts, total bytes,
    /// date span, and local fallback work estimate without sending anything.
    pub fn preview_folder(&mut self, folder_id: &str, span: Span) -> Option<PreviewSummary> {
        self.preview_folder_with_items(folder_id, span)
            .map(|(s, _)| s)
    }

    /// Like `preview_folder`, also returning the capped `EstimateItem` list for
    /// `POST /app/devices/ingest/folder/estimate`.
    pub fn preview_folder_with_items(
        &mut self,
        folder_id: &str,
        span: Span,
    ) -> Option<(PreviewSummary, Vec<EstimateItem>)> {
        let idx = self.ledger.folders.iter().position(|f| f.id == folder_id)?;
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let already_delivered: BTreeSet<String> = self.ledger.folders[idx]
            .files
            .iter()
            .filter(|(_, file)| file.delivered_to_journal)
            .map(|(path, _)| path.clone())
            .collect();

        let stamp = self.clock.now();
        let from_ms = span_start_ms(span, stamp.ms, self.clock.as_ref());
        let recents = self.load_recents();
        let walk = match walk_folder(&root, &recents) {
            Ok(w) => w,
            Err(_) => {
                let summary = PreviewSummary {
                    span: Some(span),
                    failed: true,
                    ..PreviewSummary::default()
                };
                self.ledger.folders[idx].preview = Some(summary.clone());
                return Some((summary, Vec::new()));
            }
        };

        self.ledger.folders[idx].limited_mount = walk.limited_mount;
        let mut summary = PreviewSummary {
            from_ms: (span != Span::All).then_some(from_ms),
            to_ms: (span != Span::All).then_some(stamp.ms),
            span: Some(span),
            unreadable: walk.unreadable,
            ..PreviewSummary::default()
        };
        let mut windows: BTreeSet<Window> = BTreeSet::new();
        let mut days: BTreeSet<String> = BTreeSet::new();
        let mut content_thoughts: u64 = 0;
        let mut estimate_items: Vec<EstimateItem> = Vec::new();

        for file in walk.files {
            if file.metadata.modified_ms < from_ms {
                continue;
            }
            if already_delivered.contains(&file.rel_path) {
                continue;
            }
            let note_bytes = if file.resident
                && needs_classification_bytes(file.metadata.extension.as_deref(), &self.supported)
                && file.size_bytes <= self.supported.max_note_bytes
            {
                local::read_file(&root, &file.rel_path, self.supported.max_note_bytes).ok()
            } else {
                None
            };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            let delivery = if !file.resident && delivery == DeliveryMode::Content {
                DeliveryMode::MetadataOnly
            } else {
                delivery
            };
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            summary.total_items += 1;
            match delivery {
                DeliveryMode::Content => {
                    summary.content_items += 1;
                    summary.content_bytes = summary.content_bytes.saturating_add(file.size_bytes);
                    content_thoughts += 1;
                    match kind {
                        FileKind::Note => summary.notes += 1,
                        FileKind::Document => summary.documents += 1,
                        FileKind::Transcript => summary.transcripts += 1,
                        FileKind::Screenshot => summary.screenshots += 1,
                        FileKind::Photo => summary.photos += 1,
                        FileKind::Other => summary.other += 1,
                    }
                }
                DeliveryMode::MetadataOnly => {
                    summary.metadata_only_items += 1;
                    summary.other += 1;
                }
                DeliveryMode::Ignore => {}
            }
            let clamped_ms = file.metadata.modified_ms.min(stamp.ms);
            summary.oldest_ms = Some(summary.oldest_ms.map_or(clamped_ms, |o| o.min(clamped_ms)));
            summary.newest_ms = Some(summary.newest_ms.map_or(clamped_ms, |n| n.max(clamped_ms)));
            let ev_stamp = self.clock.stamp_at(clamped_ms);
            let w = ev_stamp.window();
            days.insert(w.day.clone());
            windows.insert(w);

            if estimate_items.len() < MAX_ESTIMATE_ITEMS {
                let has_content = delivery == DeliveryMode::Content;
                let content_hash = if has_content {
                    note_bytes.as_deref().map(sha256_hex)
                } else {
                    None
                };
                estimate_items.push(EstimateItem {
                    path: file.rel_path,
                    at: ev_stamp.rfc3339(),
                    file_type: file.metadata.extension.filter(|ft| {
                        !ft.is_empty() && ft.len() <= 64 && !ft.chars().any(char::is_control)
                    }),
                    kind_hint: if has_content {
                        kind.kind_hint().map(str::to_owned)
                    } else {
                        None
                    },
                    content_hash,
                    has_content,
                });
            }
        }

        summary.estimate = Some(BackfillEstimate {
            segments: windows.len() as u64,
            days: days.len() as u64,
            thoughts: content_thoughts,
            already_in_journal: 0,
            from_journal: false,
        });
        self.ledger.folders[idx].preview = Some(summary.clone());
        Some((summary, estimate_items))
    }

    /// Replace the cached preview estimate with the journal's own estimate.
    pub fn set_journal_estimate(&mut self, folder_id: &str, estimate: BackfillEstimate) {
        if let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id)
            && let Some(preview) = folder.preview.as_mut()
        {
            preview.estimate = Some(estimate);
        }
    }

    /// Confirm setup (or "bring in older files") for one folder. Transitions
    /// the folder to `On` and enqueues eligible existing files within `span`
    /// without re-sending items already delivered to this journal.
    pub fn confirm_folder(&mut self, folder_id: &str, span: Span) -> Result<(), String> {
        let idx = self
            .ledger
            .folders
            .iter()
            .position(|f| f.id == folder_id)
            .ok_or_else(|| "not_found".to_owned())?;
        if !self.ledger.folders[idx].kinds.any_selected() {
            return Err("no_kinds_selected".into());
        }
        let stamp = self.clock.now();
        let from_ms = span_start_ms(span, stamp.ms, self.clock.as_ref());
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let recents = self.load_recents();
        let walk = walk_folder(&root, &recents).map_err(|_| "unreadable".to_owned())?;

        let folder = &mut self.ledger.folders[idx];
        folder.mode = if folder.pause_during_setup {
            FolderMode::Paused
        } else {
            FolderMode::On
        };
        folder.pause_during_setup = false;
        folder.returning = true;
        folder.folder_seen = true;
        folder.attention = None;
        folder.last_checked_ms = Some(stamp.ms);
        folder.cant_read = walk.unreadable;
        folder.limited_mount = walk.limited_mount;
        folder.span_from_ms = Some(match folder.span_from_ms {
            Some(prev) => prev.min(from_ms),
            None => from_ms,
        });

        let waiting_paths: BTreeSet<_> = folder
            .pending
            .iter()
            .map(|event| event.rel_path.clone())
            .collect();
        let mut queued_now = 0u64;
        for file in walk.files {
            if waiting_paths.contains(&file.rel_path) {
                continue;
            }
            if let Some(existing) = folder.files.get(&file.rel_path)
                && (existing.delivered_to_journal
                    || (existing.pending_revision_save_ms.is_some()
                        && existing.size_bytes == file.size_bytes
                        && existing.mtime_ns == file.mtime_ns))
            {
                continue;
            }
            if file.metadata.modified_ms < from_ms {
                folder.files.insert(
                    file.rel_path.clone(),
                    TrackedFile {
                        rel_path: file.rel_path,
                        inode: file.inode,
                        size_bytes: file.size_bytes,
                        mtime_ns: file.mtime_ns,
                        content_hash: None,
                        kind: FileKind::Other,
                        delivery: DeliveryMode::Ignore,
                        delivered_to_journal: false,
                        excluded_before_span: true,
                        last_sent_hash: None,
                        last_note_text: None,
                        pending_revision_save_ms: None,
                        pending_revision_hash: None,
                    },
                );
                continue;
            }

            let note_bytes = if file.resident
                && needs_classification_bytes(file.metadata.extension.as_deref(), &self.supported)
                && file.size_bytes <= self.supported.max_note_bytes
            {
                local::read_file(&root, &file.rel_path, self.supported.max_note_bytes).ok()
            } else {
                None
            };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            let delivery = if !file.resident && delivery == DeliveryMode::Content {
                DeliveryMode::MetadataOnly
            } else {
                delivery
            };
            if delivery == DeliveryMode::Ignore {
                continue;
            }

            let content_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    local::hash_file(&root, &file.rel_path, self.supported.max_content_bytes).ok()
                }
            } else {
                None
            };
            let note_text = if kind == FileKind::Note && delivery == DeliveryMode::Content {
                note_bytes
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(str::to_owned)
            } else {
                None
            };

            let ev_ms = file.metadata.modified_ms.min(stamp.ms);
            let ev_stamp = self.clock.stamp_at(ev_ms);
            let window = ev_stamp.window();
            let window_end_ms = ev_stamp.window_end_ms();
            let is_backfill = window_end_ms <= stamp.ms;
            let event_id = event_id_for(&folder.id, &file.rel_path, ActivityVerb::Added, ev_ms);

            folder.pending.retain(|p| p.rel_path != file.rel_path);
            folder.pending.push(PendingEvent {
                id: event_id,
                rel_path: file.rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Added,
                kind,
                delivery,
                event_ms: ev_ms,
                observed_ms: stamp.ms,
                offset_s: ev_stamp.offset_s,
                window,
                window_end_ms,
                metadata: file.metadata,
                content_hash: content_hash.clone(),
                note_payload: note_text.clone(),
                note_is_diff: false,
                is_revision: false,
                backfill: is_backfill,
                retry_at_ms: None,
                tries: 0,
            });
            folder.files.insert(
                file.rel_path.clone(),
                TrackedFile {
                    rel_path: file.rel_path,
                    inode: file.inode,
                    size_bytes: file.size_bytes,
                    mtime_ns: file.mtime_ns,
                    content_hash,
                    kind,
                    delivery,
                    delivered_to_journal: false,
                    excluded_before_span: false,
                    last_sent_hash: None,
                    last_note_text: None,
                    pending_revision_save_ms: None,
                    pending_revision_hash: None,
                },
            );
            queued_now += 1;
        }

        if queued_now > 0 {
            folder.backfill_total = folder.backfill_done.saturating_add(queued_now);
        }
        let _ = self.save();
        Ok(())
    }

    /// Pause syncing for one folder. Scanning continues so no changes are skipped;
    /// delivery waits until `resume_folder` is called.
    pub fn pause_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        if folder.mode == FolderMode::On {
            folder.mode = FolderMode::Paused;
            let _ = self.save();
            return true;
        }
        false
    }

    /// Resume syncing for one paused folder. Queued changes deliver immediately.
    pub fn resume_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        if folder.mode == FolderMode::Paused {
            folder.mode = FolderMode::On;
            let _ = self.save();
            return true;
        }
        false
    }

    /// Turn a folder off. Stops watching and drops unsent queue entries;
    /// already-delivered journal records stay intact so re-enabling won't duplicate.
    pub fn turn_off_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.mode = FolderMode::Off;
        folder.pause_during_setup = false;
        folder.pending.clear();
        for tracked in folder.files.values_mut() {
            tracked.pending_revision_save_ms = None;
            tracked.pending_revision_hash = None;
        }
        folder.backfill_total = 0;
        folder.backfill_done = 0;
        folder.attention = None;
        let _ = self.save();
        true
    }

    /// Start turning a previously off folder back on (enters `SettingUp` with
    /// `returning = true` so the owner picks how far back to catch up).
    pub fn turn_on_folder(&mut self, folder_id: &str) -> bool {
        let Some(folder) = self.ledger.folders.iter_mut().find(|f| f.id == folder_id) else {
            return false;
        };
        folder.mode = FolderMode::SettingUp;
        folder.returning = true;
        let _ = self.save();
        true
    }

    /// Remove a folder from the app completely (nothing changes on disk or in
    /// the journal).
    pub fn remove_folder(&mut self, folder_id: &str) -> bool {
        let before = self.ledger.folders.len();
        self.ledger.folders.retain(|f| f.id != folder_id);
        let removed = self.ledger.folders.len() != before;
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Pairing principle 9: a new journal pairing restarts first setup on all
    /// active/paused folders while keeping the owner's granted folders, kinds,
    /// and facet choice. Already confirmed changes follow the new pairing.
    pub fn restart_setup(&mut self) {
        for folder in &mut self.ledger.folders {
            if matches!(folder.mode, FolderMode::On | FolderMode::Paused) {
                folder.pause_during_setup = folder.mode == FolderMode::Paused;
                folder.mode = FolderMode::SettingUp;
                folder.returning = true;
            }
            folder.backfill_total = 0;
            folder.backfill_done = 0;
            folder.items_taken_in = 0;
            folder.last_taken_in_ms = None;
            folder.fallback_to_details = 0;
            folder.span_from_ms = None;
            folder.preview = None;
            for tracked in folder.files.values_mut() {
                tracked.delivered_to_journal = false;
                tracked.last_sent_hash = None;
                tracked.last_note_text = None;
            }
        }
        let _ = self.save();
    }

    /// Scan every `On` and `Paused` folder on disk, reconciling additions,
    /// edits, renames, one-hour document revisions, and delete-before-send.
    /// Notice `Paused` folders STILL scan so changes are never skipped — only
    /// delivery waits until resume.
    pub fn scan_all(&mut self) {
        let recents = self.load_recents();
        let now_stamp = self.clock.now();
        let mut dirty = false;

        for idx in 0..self.ledger.folders.len() {
            let mode = self.ledger.folders[idx].mode;
            if mode == FolderMode::SettingUp {
                dirty |= self.release_due_revisions(idx, now_stamp.ms);
                continue;
            }
            if !matches!(mode, FolderMode::On | FolderMode::Paused) {
                continue;
            }
            if self.scan_one(idx, now_stamp.ms, &recents) {
                dirty = true;
            }
        }
        if dirty {
            let _ = self.save();
        }
    }

    fn scan_one(&mut self, idx: usize, now_ms: i64, recents: &Recents) -> bool {
        let root = self.ledger.folders[idx].path.clone();
        let kinds = self.ledger.folders[idx].kinds;
        let walk = match walk_folder(&root, recents) {
            Ok(w) => {
                self.ledger.folders[idx].attention = None;
                self.ledger.folders[idx].folder_seen = true;
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                self.ledger.folders[idx].cant_read = w.unreadable;
                self.ledger.folders[idx].limited_mount = w.limited_mount;
                w
            }
            Err(ListingError::Missing) => {
                self.ledger.folders[idx].attention = Some(FolderAttention::Missing);
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                return false;
            }
            Err(ListingError::Incomplete(_)) => {
                self.ledger.folders[idx].attention = Some(FolderAttention::Unreadable);
                self.ledger.folders[idx].last_checked_ms = Some(now_ms);
                return false;
            }
        };

        let mut changed = false;
        let mut seen_paths: BTreeSet<String> = BTreeSet::new();
        let mut new_candidates: Vec<WalkedFile> = Vec::new();
        let mut modified_candidates: Vec<WalkedFile> = Vec::new();

        for file in walk.files {
            seen_paths.insert(file.rel_path.clone());
            // Let freshly written files settle before hashing/reading.
            if !settled(now_ms, file.metadata.modified_ms, SETTLE_MS) {
                continue;
            }
            match self.ledger.folders[idx].files.get(&file.rel_path) {
                None => new_candidates.push(file),
                Some(tracked) => {
                    if tracked.size_bytes != file.size_bytes || tracked.mtime_ns != file.mtime_ns {
                        modified_candidates.push(file);
                    }
                }
            }
        }

        // Only a complete listing (with no in-progress partial downloads that
        // could be a rename target mid-flight) may treat disappeared paths as
        // renames or deletions.
        let disappeared_paths: Vec<String> = if walk.complete {
            self.ledger.folders[idx]
                .files
                .keys()
                .filter(|rel| !seen_paths.contains(*rel))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // Match renames first: same (dev, ino) OR same (size, sha256).
        let mut matched_disappeared: BTreeSet<String> = BTreeSet::new();
        let mut remaining_new: Vec<WalkedFile> = Vec::new();

        // Precompute hash for any new candidate whose size matches a disappeared file.
        let disappeared_sizes: BTreeSet<u64> = disappeared_paths
            .iter()
            .filter_map(|p| self.ledger.folders[idx].files.get(p).map(|t| t.size_bytes))
            .collect();

        for cand in new_candidates {
            let mut matched_old: Option<String> = None;
            // 1. Inode match
            if let Some(cand_ino) = cand.inode {
                for old_rel in &disappeared_paths {
                    if matched_disappeared.contains(old_rel) {
                        continue;
                    }
                    if let Some(old) = self.ledger.folders[idx].files.get(old_rel)
                        && old.inode == Some(cand_ino)
                    {
                        matched_old = Some(old_rel.clone());
                        break;
                    }
                }
            }
            // 2. Content hash match (when size matches and old had a content hash)
            if matched_old.is_none()
                && disappeared_sizes.contains(&cand.size_bytes)
                && let Ok(cand_hash) =
                    local::hash_file(&root, &cand.rel_path, self.supported.max_content_bytes)
            {
                for old_rel in &disappeared_paths {
                    if matched_disappeared.contains(old_rel) {
                        continue;
                    }
                    if let Some(old) = self.ledger.folders[idx].files.get(old_rel)
                        && old.size_bytes == cand.size_bytes
                        && old.content_hash.as_deref() == Some(cand_hash.as_str())
                    {
                        matched_old = Some(old_rel.clone());
                        break;
                    }
                }
            }

            if let Some(old_rel) = matched_old {
                matched_disappeared.insert(old_rel.clone());
                if let Some(mut old_tracked) = self.ledger.folders[idx].files.remove(&old_rel) {
                    let was_delivered = old_tracked.delivered_to_journal;
                    let folder_id = self.ledger.folders[idx].id.clone();
                    old_tracked.rel_path = cand.rel_path.clone();
                    old_tracked.inode = cand.inode;
                    old_tracked.size_bytes = cand.size_bytes;
                    old_tracked.mtime_ns = cand.mtime_ns;

                    // Update any still-unsent pending event in place if never delivered;
                    // if already delivered, emit a crisp `renamed` activity line.
                    if !was_delivered {
                        for ev in &mut self.ledger.folders[idx].pending {
                            if ev.rel_path == old_rel {
                                ev.rel_path = cand.rel_path.clone();
                            }
                        }
                    } else {
                        let ev_stamp = self.clock.stamp_at(now_ms);
                        let event_id =
                            event_id_for(&folder_id, &cand.rel_path, ActivityVerb::Renamed, now_ms);
                        self.ledger.folders[idx].pending.push(PendingEvent {
                            id: event_id,
                            rel_path: cand.rel_path.clone(),
                            previous_path: Some(old_rel),
                            verb: ActivityVerb::Renamed,
                            kind: old_tracked.kind,
                            delivery: DeliveryMode::MetadataOnly,
                            event_ms: now_ms,
                            observed_ms: now_ms,
                            offset_s: ev_stamp.offset_s,
                            window: ev_stamp.window(),
                            window_end_ms: ev_stamp.window_end_ms(),
                            metadata: cand.metadata,
                            content_hash: old_tracked.content_hash.clone(),
                            note_payload: None,
                            note_is_diff: false,
                            is_revision: false,
                            backfill: false,
                            retry_at_ms: None,
                            tries: 0,
                        });
                    }
                    self.ledger.folders[idx]
                        .files
                        .insert(cand.rel_path, old_tracked);
                    changed = true;
                }
            } else {
                remaining_new.push(cand);
            }
        }

        // Delete-before-send & removed events on unmatched disappeared paths
        // (runs ONLY when `walk.complete == true`).
        if walk.complete {
            let folder_id = self.ledger.folders[idx].id.clone();
            for old_rel in disappeared_paths {
                if matched_disappeared.contains(&old_rel) {
                    continue;
                }
                let Some(old) = self.ledger.folders[idx].files.remove(&old_rel) else {
                    continue;
                };
                self.ledger.folders[idx]
                    .pending
                    .retain(|ev| ev.rel_path != old_rel);
                if !old.delivered_to_journal {
                    changed = true;
                } else if !old.excluded_before_span {
                    // Already received by the journal: emit one `removed` activity line.
                    let ev_stamp = self.clock.stamp_at(now_ms);
                    let event_id =
                        event_id_for(&folder_id, &old_rel, ActivityVerb::Removed, now_ms);
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id,
                        rel_path: old_rel.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Removed,
                        kind: old.kind,
                        delivery: DeliveryMode::MetadataOnly,
                        event_ms: now_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_stamp.window(),
                        window_end_ms: ev_stamp.window_end_ms(),
                        metadata: PlatformMetadata {
                            size_bytes: old.size_bytes,
                            modified_ms: now_ms,
                            created_ms: None,
                            extension: extension_lower(Path::new(&old_rel)),
                            mime: None,
                            origin_url: None,
                            referrer_url: None,
                            origin_domain: None,
                        },
                        content_hash: None,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        tries: 0,
                    });
                    changed = true;
                }
            }
        }

        // Newly appeared files.
        let folder_id = self.ledger.folders[idx].id.clone();
        for file in remaining_new {
            let note_bytes = if file.resident
                && needs_classification_bytes(file.metadata.extension.as_deref(), &self.supported)
                && file.size_bytes <= self.supported.max_note_bytes
            {
                local::read_file(&root, &file.rel_path, self.supported.max_note_bytes).ok()
            } else {
                None
            };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            let delivery = if !file.resident && delivery == DeliveryMode::Content {
                DeliveryMode::MetadataOnly
            } else {
                delivery
            };
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            let content_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    local::hash_file(&root, &file.rel_path, self.supported.max_content_bytes).ok()
                }
            } else {
                None
            };
            let note_text = if kind == FileKind::Note && delivery == DeliveryMode::Content {
                note_bytes
                    .as_deref()
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(str::to_owned)
            } else {
                None
            };
            let ev_ms = file.metadata.modified_ms.min(now_ms);
            let ev_stamp = self.clock.stamp_at(ev_ms);
            let event_id = event_id_for(&folder_id, &file.rel_path, ActivityVerb::Added, ev_ms);

            self.ledger.folders[idx].pending.push(PendingEvent {
                id: event_id,
                rel_path: file.rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Added,
                kind,
                delivery,
                event_ms: ev_ms,
                observed_ms: now_ms,
                offset_s: ev_stamp.offset_s,
                window: ev_stamp.window(),
                window_end_ms: ev_stamp.window_end_ms(),
                metadata: file.metadata,
                content_hash: content_hash.clone(),
                note_payload: note_text,
                note_is_diff: false,
                is_revision: false,
                backfill: false,
                retry_at_ms: None,
                tries: 0,
            });
            self.ledger.folders[idx].files.insert(
                file.rel_path.clone(),
                TrackedFile {
                    rel_path: file.rel_path,
                    inode: file.inode,
                    size_bytes: file.size_bytes,
                    mtime_ns: file.mtime_ns,
                    content_hash,
                    kind,
                    delivery,
                    delivered_to_journal: false,
                    excluded_before_span: false,
                    last_sent_hash: None,
                    last_note_text: None,
                    pending_revision_save_ms: None,
                    pending_revision_hash: None,
                },
            );
            changed = true;
        }

        // Modified files: note edits coalesce in the 5-minute window; already-delivered
        // documents/transcripts enter the one-hour revision cool-down!
        for file in modified_candidates {
            let note_bytes = if file.resident
                && needs_classification_bytes(file.metadata.extension.as_deref(), &self.supported)
                && file.size_bytes <= self.supported.max_note_bytes
            {
                local::read_file(&root, &file.rel_path, self.supported.max_note_bytes).ok()
            } else {
                None
            };
            let (kind, delivery) = classify_file(
                file.metadata.extension.as_deref(),
                file.size_bytes,
                file.is_screenshot,
                note_bytes.as_deref(),
                kinds,
                &self.supported,
            );
            let delivery = if !file.resident && delivery == DeliveryMode::Content {
                DeliveryMode::MetadataOnly
            } else {
                delivery
            };
            if delivery == DeliveryMode::Ignore {
                continue;
            }
            let new_hash = if delivery == DeliveryMode::Content {
                if let Some(ref nb) = note_bytes {
                    Some(sha256_hex(nb))
                } else {
                    local::hash_file(&root, &file.rel_path, self.supported.max_content_bytes).ok()
                }
            } else {
                None
            };

            let Some(tracked) = self.ledger.folders[idx].files.get_mut(&file.rel_path) else {
                continue;
            };
            tracked.inode = file.inode;
            tracked.size_bytes = file.size_bytes;
            tracked.mtime_ns = file.mtime_ns;
            tracked.kind = kind;
            tracked.delivery = delivery;
            tracked.excluded_before_span = false;

            if new_hash.is_some() && new_hash == tracked.content_hash {
                // Metadata-only touch with identical bytes: no duplicate send.
                continue;
            }
            tracked.content_hash = new_hash.clone();

            let ev_ms = file.metadata.modified_ms.min(now_ms);
            let ev_stamp = self.clock.stamp_at(ev_ms);
            let ev_window = ev_stamp.window();
            let ev_window_end = ev_stamp.window_end_ms();

            match (kind, delivery, tracked.delivered_to_journal) {
                (FileKind::Note, DeliveryMode::Content, _) => {
                    let current_text = note_bytes
                        .as_deref()
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .map(str::to_owned);
                    let (verb, note_payload, note_is_diff) =
                        match (tracked.last_note_text.as_deref(), current_text.as_deref()) {
                            (Some(baseline), Some(after)) => {
                                if net_line_diff(baseline, after).is_none() {
                                    // Reverted back to baseline before window closed!
                                    self.ledger.folders[idx].pending.retain(|ev| {
                                        !(ev.rel_path == file.rel_path && ev.window == ev_window)
                                    });
                                    changed = true;
                                    continue;
                                }
                                // Send full current note bytes so the journal's per-folder
                                // note diff engine computes net change cleanly against its baseline.
                                (ActivityVerb::Edited, Some(after.to_owned()), false)
                            }
                            (_, Some(whole)) => {
                                (ActivityVerb::Added, Some(whole.to_owned()), false)
                            }
                            _ => (ActivityVerb::Edited, None, false),
                        };
                    // Coalesce into this five-minute window for the same note.
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| !(ev.rel_path == file.rel_path && ev.window == ev_window));
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, verb, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb,
                        kind: FileKind::Note,
                        delivery: DeliveryMode::Content,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: new_hash,
                        note_payload,
                        note_is_diff,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        tries: 0,
                    });
                    changed = true;
                }
                (FileKind::Document | FileKind::Transcript, DeliveryMode::Content, true) => {
                    // Already sent once: start or reset the one-hour revision cool-down.
                    let revised = new_hash.is_some() && new_hash != tracked.last_sent_hash;
                    tracked.pending_revision_save_ms = revised.then_some(ev_ms);
                    tracked.pending_revision_hash = if revised { new_hash } else { None };
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| ev.rel_path != file.rel_path || !ev.is_revision);
                    changed = true;
                }
                (FileKind::Document | FileKind::Transcript, DeliveryMode::Content, false) => {
                    // Not yet delivered: refresh its waiting upload in place.
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| ev.rel_path != file.rel_path);
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, ActivityVerb::Added, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Added,
                        kind,
                        delivery: DeliveryMode::Content,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: new_hash,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        tries: 0,
                    });
                    changed = true;
                }
                (_, DeliveryMode::MetadataOnly, _) => {
                    self.ledger.folders[idx]
                        .pending
                        .retain(|ev| !(ev.rel_path == file.rel_path && ev.window == ev_window));
                    self.ledger.folders[idx].pending.push(PendingEvent {
                        id: event_id_for(&folder_id, &file.rel_path, ActivityVerb::Edited, ev_ms),
                        rel_path: file.rel_path.clone(),
                        previous_path: None,
                        verb: ActivityVerb::Edited,
                        kind,
                        delivery: DeliveryMode::MetadataOnly,
                        event_ms: ev_ms,
                        observed_ms: now_ms,
                        offset_s: ev_stamp.offset_s,
                        window: ev_window,
                        window_end_ms: ev_window_end,
                        metadata: file.metadata,
                        content_hash: None,
                        note_payload: None,
                        note_is_diff: false,
                        is_revision: false,
                        backfill: false,
                        retry_at_ms: None,
                        tries: 0,
                    });
                    changed = true;
                }
                _ => {}
            }
        }

        self.release_due_revisions(idx, now_ms) || changed
    }

    fn release_due_revisions(&mut self, idx: usize, now_ms: i64) -> bool {
        let root = self.ledger.folders[idx].path.clone();
        let folder_id = self.ledger.folders[idx].id.clone();
        let mut changed = false;
        let rev_wait_ms = self.supported.document_revision_cooldown_ms;
        let due_revisions: Vec<(String, i64, FileKind, Option<String>)> = self.ledger.folders[idx]
            .files
            .values()
            .filter_map(|t| {
                let save_ms = t.pending_revision_save_ms?;
                if now_ms.saturating_sub(save_ms) >= rev_wait_ms {
                    Some((
                        t.rel_path.clone(),
                        save_ms,
                        t.kind,
                        t.pending_revision_hash.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();

        for (rel_path, save_ms, kind, rev_hash) in due_revisions {
            let Ok(object) = local::object(&root, Path::new(&rel_path)) else {
                continue;
            };
            let Ok(fs_meta) = object.metadata() else {
                continue;
            };
            let metadata =
                collect_metadata(Path::new(&rel_path), &local::fd_path(&object), &fs_meta);
            let tracked = &self.ledger.folders[idx].files[&rel_path];
            if metadata.size_bytes != tracked.size_bytes
                || i128::from(metadata.modified_ms) * 1_000_000 != tracked.mtime_ns
            {
                continue;
            }
            let ev_stamp = self.clock.stamp_at(save_ms);
            let event_id = event_id_for(&folder_id, &rel_path, ActivityVerb::Revised, save_ms);
            self.ledger.folders[idx].pending.push(PendingEvent {
                id: event_id,
                rel_path: rel_path.clone(),
                previous_path: None,
                verb: ActivityVerb::Revised,
                kind,
                delivery: DeliveryMode::Content,
                event_ms: save_ms,
                observed_ms: now_ms,
                offset_s: ev_stamp.offset_s,
                window: ev_stamp.window(),
                window_end_ms: ev_stamp.window_end_ms(),
                metadata,
                content_hash: rev_hash,
                note_payload: None,
                note_is_diff: false,
                is_revision: true,
                backfill: false,
                retry_at_ms: None,
                tries: 0,
            });
            if let Some(tracked) = self.ledger.folders[idx].files.get_mut(&rel_path) {
                tracked.pending_revision_save_ms = None;
                tracked.pending_revision_hash = None;
            }
            changed = true;
        }

        if changed {
            apply_kind_choices(&mut self.ledger.folders[idx]);
        }
        changed
    }

    /// Pick the next ready five-minute segment across all `On` folders
    /// (newest window first, honoring Band 4 newest-first backfill ordering).
    /// Returns `None` while paused by the computer or when no window is closed/due.
    pub fn next_delivery(&mut self, computer_paused: bool) -> Option<SegmentDelivery> {
        if computer_paused {
            return None;
        }
        let now_ms = self.clock.now_ms();
        let mut best_key: Option<(usize, Window)> = None;

        for (idx, folder) in self.ledger.folders.iter().enumerate() {
            if !(folder.mode == FolderMode::On
                || (folder.mode == FolderMode::SettingUp && !folder.pause_during_setup))
                || folder.attention.is_some()
            {
                continue;
            }
            for ev in &folder.pending {
                if ev.window_end_ms > now_ms {
                    continue;
                }
                if ev.retry_at_ms.is_some_and(|at| at > now_ms) {
                    continue;
                }
                let replace = match &best_key {
                    None => true,
                    Some((_, best_win)) => ev.window > *best_win,
                };
                if replace {
                    best_key = Some((idx, ev.window.clone()));
                }
            }
        }

        let (idx, window) = best_key?;
        let folder = &mut self.ledger.folders[idx];
        let mut events: Vec<PendingEvent> = folder
            .pending
            .iter()
            .filter(|ev| {
                ev.window == window
                    && ev.window_end_ms <= now_ms
                    && ev.retry_at_ms.is_none_or(|at| at <= now_ms)
            })
            .cloned()
            .collect();
        events.sort_by(|a, b| {
            a.event_ms
                .cmp(&b.event_ms)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
        });

        // Re-check local existence right before send (delete-before-send guard),
        // and cap companion content parts at `MAX_UPLOAD_CONTENT_PARTS` (63 parts
        // + 1 activity JSONL = 64 files max per upload).
        let mut dropped_ids: BTreeSet<String> = BTreeSet::new();
        let mut selected_events: Vec<PendingEvent> = Vec::with_capacity(events.len());
        let mut staged_files = Vec::new();
        for mut ev in events {
            // Bound the JSONL part as well as the number of content parts.
            if selected_events.len() >= 500 {
                break;
            }
            if ev.verb != ActivityVerb::Removed {
                match local::object(&folder.path, Path::new(&ev.rel_path))
                    .and_then(|fd| Ok((fd.metadata()?, local::content_available(&fd))))
                {
                    Ok((meta, resident)) if meta.is_file() => {
                        if !resident {
                            ev.delivery = DeliveryMode::MetadataOnly;
                            ev.content_hash = None;
                            ev.note_payload = None;
                        }
                    }
                    Ok(_) => {
                        dropped_ids.insert(ev.id);
                        continue;
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        dropped_ids.insert(ev.id);
                        continue;
                    }
                    Err(_) => {
                        if let Some(held) = folder.pending.iter_mut().find(|held| held.id == ev.id)
                        {
                            held.retry_at_ms = Some(now_ms.saturating_add(RETRY_MS[0]));
                        }
                        continue;
                    }
                }
            }
            if ev.delivery != DeliveryMode::Content {
                selected_events.push(ev);
                continue;
            }
            if staged_files.len() >= MAX_UPLOAD_CONTENT_PARTS {
                continue;
            }
            let ext = ev.metadata.extension.as_deref().unwrap_or("");
            let needs_image_convert = matches!(ev.kind, FileKind::Screenshot | FileKind::Photo)
                && self.supported.convertible_image_extensions.contains(ext);
            staged_files.push(StagedContentFile {
                event_id: ev.id.clone(),
                rel_path: ev.rel_path.clone(),
                kind: ev.kind,
                content_hash: ev.content_hash.clone(),
                note_payload: ev.note_payload.clone(),
                note_is_diff: ev.note_is_diff,
                is_revision: ev.is_revision,
                needs_image_convert,
            });
            selected_events.push(ev);
        }

        let folder_id = folder.id.clone();
        let folder_name = folder.name.clone();
        let folder_root = folder.path.clone();
        let facet = folder.facet.as_ref().map(|f| f.name.clone());
        let kinds = folder.kinds;

        if !dropped_ids.is_empty() {
            folder.pending.retain(|ev| !dropped_ids.contains(&ev.id));
            let _ = self.save();
        }
        if selected_events.is_empty() {
            return None;
        }

        Some(SegmentDelivery {
            folder_id,
            folder_name,
            folder_root,
            facet,
            kinds,
            window,
            events: selected_events,
            content_files: staged_files,
        })
    }

    /// Record the journal's reply for a delivered segment.
    pub fn record_outcome(&mut self, delivery: &SegmentDelivery, outcome: DeliveryOutcome) {
        let now_ms = self.clock.now_ms();
        let Some(folder) = self
            .ledger
            .folders
            .iter_mut()
            .find(|f| f.id == delivery.folder_id)
        else {
            return;
        };
        let ids: BTreeSet<&str> = delivery.events.iter().map(|e| e.id.as_str()).collect();

        match outcome {
            DeliveryOutcome::Delivered | DeliveryOutcome::DeliveredWithRefusedContent(_) => {
                let refused_set: BTreeSet<&str> = match &outcome {
                    DeliveryOutcome::DeliveredWithRefusedContent(paths) => {
                        folder.fallback_to_details = folder
                            .fallback_to_details
                            .saturating_add(paths.len() as u64);
                        paths.iter().map(String::as_str).collect()
                    }
                    _ => BTreeSet::new(),
                };
                let mut completed_backfill = 0u64;
                let mut fallback_events: Vec<PendingEvent> = Vec::new();
                for ev in &delivery.events {
                    if refused_set.contains(ev.rel_path.as_str())
                        && ev.delivery == DeliveryMode::Content
                    {
                        // Do not resend refused bytes; re-queue one details-only line (part: null).
                        let mut meta_only = ev.clone();
                        meta_only.delivery = DeliveryMode::MetadataOnly;
                        meta_only.content_hash = None;
                        meta_only.note_payload = None;
                        meta_only.retry_at_ms = None;
                        fallback_events.push(meta_only);
                        continue;
                    }
                    if ev.backfill {
                        completed_backfill += 1;
                    }
                    folder.items_taken_in = folder.items_taken_in.saturating_add(1);
                    folder.last_taken_in_ms = Some(now_ms);
                    if let Some(ref hook) = self.intake_hook {
                        hook(&ev.id);
                    }
                    if let Some(tracked) = folder.files.get_mut(&ev.rel_path) {
                        tracked.delivered_to_journal = true;
                        if ev.content_hash.is_some() {
                            tracked.last_sent_hash = ev.content_hash.clone();
                        }
                        if ev.kind == FileKind::Note
                            && ev.delivery == DeliveryMode::Content
                            && let Some(ref text) = ev.note_payload
                        {
                            tracked.last_note_text = Some(text.clone());
                        }
                    }
                }
                folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
                folder.pending.extend(fallback_events);
                if completed_backfill > 0 {
                    folder.backfill_done = folder.backfill_done.saturating_add(completed_backfill);
                    let remaining_backfill = folder.pending.iter().any(|ev| ev.backfill);
                    if !remaining_backfill {
                        folder.backfill_total = 0;
                        folder.backfill_done = 0;
                    }
                }
            }
            DeliveryOutcome::AllFilesRefused(refused) => {
                let refused_set: BTreeSet<&str> = refused.iter().map(String::as_str).collect();
                let mut converted = 0u64;
                for ev in &mut folder.pending {
                    if !ids.contains(ev.id.as_str()) {
                        continue;
                    }
                    if ev.delivery == DeliveryMode::Content
                        && (refused_set.is_empty() || refused_set.contains(ev.rel_path.as_str()))
                    {
                        ev.delivery = DeliveryMode::MetadataOnly;
                        ev.content_hash = None;
                        ev.note_payload = None;
                        ev.retry_at_ms = None;
                        converted += 1;
                    }
                }
                if converted > 0 {
                    folder.fallback_to_details =
                        folder.fallback_to_details.saturating_add(converted);
                } else {
                    // Already metadata-only and still refused: drop to avoid looping.
                    folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
                }
            }
            DeliveryOutcome::InvalidPermanent => {
                folder.pending.retain(|ev| !ids.contains(ev.id.as_str()));
            }
            DeliveryOutcome::Hold => {
                for ev in &mut folder.pending {
                    if ids.contains(ev.id.as_str()) {
                        let step = (ev.tries as usize).min(RETRY_MS.len() - 1);
                        ev.tries = ev.tries.saturating_add(1);
                        ev.retry_at_ms = Some(now_ms.saturating_add(RETRY_MS[step]));
                    }
                }
            }
        }
        let _ = self.save();
    }

    /// Build the D-Bus / UI / CLI status snapshot.
    #[must_use]
    pub fn document(
        &self,
        facets: Vec<FacetOption>,
        facets_state: super::types::FacetsAvailability,
        computer_paused: bool,
        paired: bool,
        offline: bool,
        journal_lists: Option<bool>,
    ) -> FoldersDocument {
        let folders = self
            .ledger
            .folders
            .iter()
            .map(|f| {
                let progress = if f.backfill_total > 0 && f.backfill_done < f.backfill_total {
                    Some([f.backfill_done, f.backfill_total])
                } else {
                    None
                };
                FolderSummary {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    path: f.path.display().to_string(),
                    label: f.label.clone(),
                    mode: f.mode,
                    returning: f.returning,
                    kinds: f.kinds,
                    facet: f.facet.clone(),
                    overlap: f.overlap.clone(),
                    attention: f.attention,
                    preview: f.preview.clone(),
                    progress,
                    waiting: f.pending.len() as u64,
                    items_taken_in: f.items_taken_in,
                    last_taken_in_ms: f.last_taken_in_ms,
                    last_checked_ms: f.last_checked_ms,
                    cant_read: f.cant_read,
                    limited_mount: f.limited_mount,
                    fallback_to_details: f.fallback_to_details,
                    journal_lists,
                    paused_by_computer: computer_paused && f.mode == FolderMode::On,
                }
            })
            .collect();
        FoldersDocument {
            v: 1,
            folders,
            facets,
            facets_state,
            computer_paused,
            paired,
            offline,
            journal_lists,
        }
    }

    fn load_recents(&self) -> Recents {
        let Some(ref path) = self.recents_path else {
            return Recents::default();
        };
        fs::read_to_string(path)
            .map(|text| Recents::parse(&text))
            .unwrap_or_default()
    }
}

fn needs_classification_bytes(ext: Option<&str>, supported: &SupportedList) -> bool {
    ext.is_some_and(|e| {
        supported.note_extensions.contains(e)
            || (supported.transcript_extensions.contains(e) && !matches!(e, "vtt" | "srt"))
    })
}

fn span_start_ms(span: Span, now_ms: i64, clock: &dyn Clock) -> i64 {
    use jiff::{Span as Period, Timestamp, tz::Offset};
    if span == Span::All {
        return i64::MIN;
    }
    let Ok(instant) = Timestamp::from_millisecond(now_ms) else {
        return now_ms;
    };
    let offset = Offset::from_seconds(clock.offset_s_at(now_ms)).unwrap_or(Offset::UTC);
    let today = offset.to_datetime(instant).date();
    let date = match span {
        Span::Today => today,
        Span::Last30Days => today.saturating_sub(Period::new().days(30)),
        Span::Last6Months => today.saturating_sub(Period::new().months(6)),
        Span::LastYear => today.saturating_sub(Period::new().years(1)),
        Span::All => unreachable!(),
    };
    let Ok(midnight) = Offset::UTC.to_timestamp(date.at(0, 0, 0, 0)) else {
        return now_ms;
    };
    let wall = midnight.as_millisecond();
    let local_day = |ms| (ms + i64::from(clock.offset_s_at(ms)) * 1000).div_euclid(DAY_MS);
    let target = wall.div_euclid(DAY_MS);
    let candidate = wall - i64::from(clock.offset_s_at(wall)) * 1000;
    if local_day(candidate) == target && local_day(candidate - 1) < target {
        return candidate;
    }
    let (mut lo, mut hi) = (wall - DAY_MS, wall + DAY_MS);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if local_day(mid) >= target {
            hi = mid
        } else {
            lo = mid
        }
    }
    hi
}

/// Build a deterministic client-assigned folder ID matching
/// `^[a-z0-9][a-z0-9_-]{0,63}$` (protocol v3 contract).
fn make_folder_id(name: &str, canonical: &Path) -> String {
    let hash = &sha256_hex(canonical.to_string_lossy().as_bytes())[..8];
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !slug.is_empty() && !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("folder-{hash}")
    } else {
        format!("{slug}-{hash}")
    }
}

fn event_id_for(folder_id: &str, rel_path: &str, verb: ActivityVerb, event_ms: i64) -> String {
    let raw = format!("{folder_id}:{rel_path}:{}:{event_ms}", verb.as_str());
    sha256_hex(raw.as_bytes())
}

/// Recursively walk `root`, honoring placeholder skips, partial download skips,
/// junk/hidden skips, and symlink confinement. Only `complete == true` permits
/// deletion reconciliation.
fn walk_folder(root: &Path, recents: &Recents) -> Result<WalkOutcome, ListingError> {
    let root_object = local::object(root, Path::new(".")).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            ListingError::Missing
        } else {
            ListingError::Incomplete(err)
        }
    })?;
    let mut out = WalkOutcome {
        files: Vec::new(),
        limited_mount: !local::content_available(&root_object),
        unreadable: 0,
        deferred_partials: 0,
        complete: true,
    };
    let root_meta = root_object.metadata().map_err(ListingError::Incomplete)?;
    let mut stack = vec![String::new()];
    let mut visited_dirs = BTreeSet::from([(root_meta.dev(), root_meta.ino())]);

    while let Some(prefix) = stack.pop() {
        let relative = if prefix.is_empty() { "." } else { &prefix };
        let directory =
            local::object(root, Path::new(relative)).map_err(ListingError::Incomplete)?;
        let result = visit_directory(&local::fd_path(&directory), |entry| {
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else {
                return Ok(());
            };
            if is_junk_entry(name) {
                return Ok(());
            }
            if is_partial_download(name) {
                out.deferred_partials += 1;
                return Ok(());
            }
            let rel_path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            if rel_path.len() > 1024
                || rel_path.contains('\\')
                || rel_path.chars().any(char::is_control)
            {
                return Ok(());
            }
            let object = match local::object(root, Path::new(&rel_path)) {
                Ok(object) => object,
                Err(err) if err.raw_os_error() == Some(rustix::io::Errno::XDEV.raw_os_error()) => {
                    return Ok(());
                }
                Err(_) => {
                    out.unreadable += 1;
                    out.complete = false;
                    return Ok(());
                }
            };
            let meta = object.metadata()?;
            if is_placeholder(&meta) {
                return Ok(());
            }
            if meta.is_dir() {
                if visited_dirs.insert((meta.dev(), meta.ino())) {
                    stack.push(rel_path);
                }
                return Ok(());
            }
            if !meta.is_file() {
                return Ok(());
            }
            let resident = local::content_available(&object);
            out.limited_mount |= !resident;
            let physical = local::fd_path(&object);
            let logical = root.join(&rel_path);
            let platform_meta = collect_metadata(&logical, &physical, &meta);
            let is_screenshot = recents.saved_ms(&logical).is_some()
                || (resident
                    && platform_meta.extension.as_deref() == Some("png")
                    && proof::file_has_gnome_mark(&physical).unwrap_or(false));
            let mtime_ns = meta
                .modified()
                .ok()
                .map(|t| i128::from(system_time_ms(t)) * 1_000_000)
                .unwrap_or(0);
            out.files.push(WalkedFile {
                resident,
                rel_path,
                inode: Some((meta.dev(), meta.ino())),
                size_bytes: meta.len(),
                mtime_ns,
                metadata: platform_meta,
                is_screenshot,
            });
            Ok(())
        });

        match result {
            Ok(()) => {}
            Err(ListingError::Missing) if prefix.is_empty() => {
                return Err(ListingError::Missing);
            }
            Err(ListingError::Incomplete(err)) if prefix.is_empty() => {
                return Err(ListingError::Incomplete(err));
            }
            Err(_) => {
                out.unreadable += 1;
                out.complete = false;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn supported_refresh_rechecks_a_waiting_office_file() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("Docs");
        fs::create_dir(&dir).unwrap();
        let now = 1_795_689_600_000;
        fs::write(dir.join("presentation.pptx"), b"content").unwrap();
        set_mtime_ms(&dir.join("presentation.pptx"), now - 310_000);
        let mut engine = Engine::open(
            temp.path().join("state"),
            temp.path().to_owned(),
            TestClock::new(now, 0),
        );
        let id = engine.begin_setup(&dir, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();
        assert!(
            engine
                .next_delivery(false)
                .unwrap()
                .content_files
                .is_empty()
        );
        let mut rules = SupportedList::default();
        rules.document_extensions.insert("pptx".into());
        engine.set_supported_list(rules);
        assert_eq!(
            engine.next_delivery(false).unwrap().content_files[0].kind,
            FileKind::Document
        );
    }

    #[test]
    fn preview_cutoff_clamps_months_and_leap_years() {
        let ms = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let clock = TestClock::new(0, 0);
        for (span, now, start) in [
            (
                Span::Last6Months,
                "2024-08-31T18:00:00Z",
                "2024-02-29T00:00:00Z",
            ),
            (
                Span::LastYear,
                "2024-02-29T18:00:00Z",
                "2023-02-28T00:00:00Z",
            ),
            (
                Span::Last30Days,
                "2024-03-01T18:00:00Z",
                "2024-01-31T00:00:00Z",
            ),
        ] {
            assert_eq!(span_start_ms(span, ms(now), clock.as_ref()), ms(start));
        }
    }

    #[test]
    fn today_starts_at_local_midnight_on_both_clock_changes() {
        struct Denver;
        impl Clock for Denver {
            fn now_ms(&self) -> i64 {
                unreachable!()
            }
            fn offset_s_at(&self, ms: i64) -> i32 {
                let zone = jiff::tz::TimeZone::get("America/Denver").unwrap();
                zone.to_offset(jiff::Timestamp::from_millisecond(ms).unwrap())
                    .seconds()
            }
        }
        for (now, expected) in [
            ("2026-03-08T19:00:00Z", "2026-03-08T07:00:00Z"),
            ("2026-11-01T19:00:00Z", "2026-11-01T06:00:00Z"),
        ] {
            let ms = |s: &str| s.parse::<jiff::Timestamp>().unwrap().as_millisecond();
            assert_eq!(span_start_ms(Span::Today, ms(now), &Denver), ms(expected));
        }
    }

    struct TestClock {
        ms: AtomicI64,
        offset_s: i32,
    }

    impl TestClock {
        fn new(ms: i64, offset_s: i32) -> Arc<Self> {
            Arc::new(Self {
                ms: AtomicI64::new(ms),
                offset_s,
            })
        }

        fn advance_ms(&self, delta: i64) {
            self.ms.fetch_add(delta, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.ms.load(Ordering::SeqCst)
        }
        fn offset_s_at(&self, _ms: i64) -> i32 {
            self.offset_s
        }
    }

    fn set_mtime_ms(path: &Path, ms: i64) {
        let secs = ms.div_euclid(1000);
        let nanos = (ms.rem_euclid(1000) * 1_000_000) as u32;
        let ts = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: secs,
                tv_nsec: nanos as _,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: secs,
                tv_nsec: nanos as _,
            },
        };
        let file = fs::File::open(path).unwrap();
        rustix::fs::futimens(&file, &ts).unwrap();
    }

    #[test]
    fn five_minute_windows_group_multiple_files_and_preview_sends_nothing_before_confirm() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Downloads");
        fs::create_dir_all(&folder).unwrap();

        // 2026-11-27T10:42:00Z and 10:43:00Z sit in the same 10:40:00_300 window.
        let t_base: i64 = 1_795_689_600_000; // 2026-11-27T10:40:00Z
        let clock = TestClock::new(t_base + 360_000, 0); // 10:46:00Z (window closed)

        let todo = folder.join("todo.md");
        let pdf = folder.join("contract.pdf");
        let exe = folder.join("installer.exe");
        let part = folder.join("unfinished.iso.crdownload");
        let dot = folder.join(".hidden");
        fs::write(&todo, "# todo\n- check dates\n").unwrap();
        fs::write(&pdf, b"%PDF-1.4 sample").unwrap();
        fs::write(&exe, b"binary payload").unwrap();
        fs::write(&part, b"partial").unwrap();
        fs::write(&dot, b"hidden").unwrap();
        for (p, offset) in [(&todo, 120_000), (&pdf, 180_000), (&exe, 240_000)] {
            set_mtime_ms(p, t_base + offset);
        }

        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        let preview = engine.preview_folder(&id, Span::Last30Days).unwrap();
        assert_eq!(preview.total_items, 3);
        assert_eq!(preview.notes, 1);
        assert_eq!(preview.documents, 1);
        assert_eq!(preview.other, 1);
        assert_eq!(preview.content_items, 2);
        assert_eq!(preview.metadata_only_items, 1);
        // Nothing queued before confirm.
        assert!(engine.next_delivery(false).is_none());

        engine.confirm_folder(&id, Span::Last30Days).unwrap();
        let delivery = engine.next_delivery(false).unwrap();
        assert_eq!(delivery.events.len(), 3);
        assert_eq!(delivery.content_files.len(), 2);
        assert_eq!(delivery.window.segment, "104000_300");
    }

    #[test]
    fn note_edit_tracking_coalesces_net_diff_and_document_waits_one_hour_for_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Work");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000; // 10:40:00Z
        let clock = TestClock::new(t0 + 60_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        let note = folder.join("todo.md");
        let doc = folder.join("proposal.pdf");
        fs::write(&note, "line 1\nline 2\n").unwrap();
        fs::write(&doc, b"%PDF-1.4 v1").unwrap();
        set_mtime_ms(&note, t0 + 10_000);
        set_mtime_ms(&doc, t0 + 20_000);

        engine.scan_all();
        // Window still open at 10:41:00Z -> not delivered yet.
        assert!(engine.next_delivery(false).is_none());

        // Close window (10:45:05Z) and deliver first sight.
        clock.advance_ms(245_000);
        let first = engine.next_delivery(false).unwrap();
        assert_eq!(first.events.len(), 2);
        engine.record_outcome(&first, DeliveryOutcome::Delivered);

        // Edit note twice in 10:45-10:50 window, and save a new PDF revision.
        let t_edit1 = clock.now_ms();
        fs::write(&note, "line 1\nline 2\nline 3 temp\n").unwrap();
        set_mtime_ms(&note, t_edit1);
        fs::write(&doc, b"%PDF-1.4 v2").unwrap();
        set_mtime_ms(&doc, t_edit1);
        clock.advance_ms(10_000);
        engine.scan_all();

        let t_edit2 = clock.now_ms();
        fs::write(&note, "line 1\nline 2\nline 3 final\n").unwrap();
        set_mtime_ms(&note, t_edit2);
        clock.advance_ms(10_000);
        engine.scan_all();

        // Advance past 10:50:00Z (note window closes, document 1h wait has NOT elapsed).
        clock.advance_ms(300_000);
        let note_seg = engine.next_delivery(false).unwrap();
        assert_eq!(note_seg.events.len(), 1);
        assert_eq!(note_seg.events[0].rel_path, "todo.md");
        assert_eq!(note_seg.events[0].verb, ActivityVerb::Edited);
        let payload = note_seg.events[0].note_payload.as_deref().unwrap();
        assert!(payload.contains("line 3 final"));
        assert!(!payload.contains("line 3 temp"));
        engine.record_outcome(&note_seg, DeliveryOutcome::Delivered);

        // At +50 min after PDF save: still waiting on 1-hour cool-down.
        clock.advance_ms(45 * 60 * 1000);
        engine.scan_all();
        assert!(engine.next_delivery(false).is_none());

        // At +65 min after PDF save: 1-hour cool-down elapsed -> revision queued and delivered!
        clock.advance_ms(20 * 60 * 1000);
        engine.scan_all();
        let rev_seg = engine.next_delivery(false).unwrap();
        assert_eq!(rev_seg.events.len(), 1);
        assert_eq!(rev_seg.events[0].rel_path, "proposal.pdf");
        assert_eq!(rev_seg.events[0].verb, ActivityVerb::Revised);
        assert!(rev_seg.events[0].is_revision);
        fs::write(&doc, b"%PDF-1.4 v1").unwrap();
        set_mtime_ms(&doc, clock.now_ms());
        clock.advance_ms(5_000);
        engine.scan_all();
        clock.advance_ms(65 * 60 * 1000);
        engine.scan_all();
        assert!(engine.next_delivery(false).is_none());
        fs::write(&doc, b"%PDF-1.4 v3").unwrap();
        set_mtime_ms(&doc, clock.now_ms());
        clock.advance_ms(5_000);
        engine.scan_all();
        engine.restart_setup();
        assert!(engine.next_delivery(false).is_none());
        clock.advance_ms(60 * 60 * 1000);
        engine.scan_all();
        assert_eq!(
            engine.next_delivery(false).unwrap().events[0].verb,
            ActivityVerb::Revised
        );
    }

    #[test]
    fn delete_before_send_drops_unsent_file_and_emits_removed_for_delivered_file() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Drop");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        let kept = folder.join("kept.md");
        let ephemeral = folder.join("temp.md");
        fs::write(&kept, "stay").unwrap();
        fs::write(&ephemeral, "gone before send").unwrap();
        set_mtime_ms(&kept, t0 + 1_000);
        set_mtime_ms(&ephemeral, t0 + 2_000);
        engine.scan_all();

        // Delete `temp.md` before the 5-minute window sends!
        fs::remove_file(&ephemeral).unwrap();
        clock.advance_ms(300_000);
        engine.scan_all();

        let seg = engine.next_delivery(false).unwrap();
        assert_eq!(seg.events.len(), 1);
        assert_eq!(seg.events[0].rel_path, "kept.md");
        engine.record_outcome(&seg, DeliveryOutcome::Delivered);

        // Now delete `kept.md` AFTER delivery -> emits `removed` activity line.
        fs::remove_file(&kept).unwrap();
        clock.advance_ms(5_000);
        engine.scan_all();
        clock.advance_ms(310_000);
        let rem = engine.next_delivery(false).unwrap();
        assert_eq!(rem.events.len(), 1);
        assert_eq!(rem.events[0].rel_path, "kept.md");
        assert_eq!(rem.events[0].verb, ActivityVerb::Removed);
        assert_eq!(rem.events[0].delivery, DeliveryMode::MetadataOnly);
    }

    #[test]
    fn pause_holds_sync_without_skipping_and_restart_setup_preserves_waiting_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Notes");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        // Pause this folder: changes made during pause MUST still be tracked,
        // held until resume, and delivered once resumed.
        assert!(engine.pause_folder(&id));
        let added = folder.join("during-pause.md");
        fs::write(&added, "written while paused").unwrap();
        set_mtime_ms(&added, t0 + 15_000);
        clock.advance_ms(310_000);
        engine.scan_all();
        assert!(engine.next_delivery(false).is_none());

        assert!(engine.resume_folder(&id));
        let seg = engine.next_delivery(false).unwrap();
        assert_eq!(seg.events.len(), 1);
        assert_eq!(seg.events[0].rel_path, "during-pause.md");

        // A new pairing can drain held material while first setup is still open.
        engine.restart_setup();
        assert_eq!(engine.folders()[0].mode, FolderMode::SettingUp);
        assert!(engine.folders()[0].returning);
        clock.advance_ms(2 * DAY_MS);
        let fresh = folder.join("fresh.md");
        fs::write(&fresh, "after pairing").unwrap();
        set_mtime_ms(&fresh, clock.now_ms() - 310_000);
        engine.scan_all();
        let waiting = engine.next_delivery(false).unwrap();
        assert_eq!(waiting.events, seg.events);
        let mut kinds = FolderKinds {
            notes: false,
            ..Default::default()
        };
        assert!(engine.set_kinds(&id, kinds));
        let limited = engine.next_delivery(false).unwrap();
        assert!(limited.content_files.is_empty());
        assert_eq!(limited.events[0].delivery, DeliveryMode::MetadataOnly);
        kinds.other = false;
        assert!(engine.set_kinds(&id, kinds));
        assert!(engine.next_delivery(false).is_none());
        assert!(engine.set_kinds(&id, FolderKinds::default()));
        assert!(engine.next_delivery(false).is_none());
        engine.confirm_folder(&id, Span::Today).unwrap();
        assert_eq!(
            engine.next_delivery(false).unwrap().events[0].rel_path,
            "fresh.md"
        );
    }

    #[test]
    fn rename_emits_single_renamed_line_after_delivery() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Docs");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        let a = folder.join("draft.pdf");
        let b = folder.join("proposal-final.pdf");
        fs::write(&a, b"%PDF-1.4 content").unwrap();
        set_mtime_ms(&a, t0 + 1_000);
        engine.scan_all();
        clock.advance_ms(310_000);
        let seg = engine.next_delivery(false).unwrap();
        engine.record_outcome(&seg, DeliveryOutcome::Delivered);

        fs::rename(&a, &b).unwrap();
        clock.advance_ms(10_000);
        engine.scan_all();
        clock.advance_ms(310_000);

        let ren = engine.next_delivery(false).unwrap();
        assert_eq!(ren.events.len(), 1);
        assert_eq!(ren.events[0].verb, ActivityVerb::Renamed);
        assert_eq!(ren.events[0].previous_path.as_deref(), Some("draft.pdf"));
        assert_eq!(ren.events[0].rel_path, "proposal-final.pdf");
    }

    #[test]
    fn incomplete_listing_never_counts_missing_files_as_deleted() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Shared");
        let locked_sub = folder.join("locked");
        fs::create_dir_all(&locked_sub).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        let note = folder.join("note.md");
        let sub_note = locked_sub.join("inside.md");
        fs::write(&note, "top").unwrap();
        fs::write(&sub_note, "nested").unwrap();
        set_mtime_ms(&note, t0 + 1_000);
        set_mtime_ms(&sub_note, t0 + 2_000);
        engine.scan_all();
        clock.advance_ms(310_000);
        let first = engine.next_delivery(false).unwrap();
        assert_eq!(first.events.len(), 2);
        engine.record_outcome(&first, DeliveryOutcome::Delivered);

        // Make subdirectory unreadable so the walk is incomplete; also remove
        // `note.md` while unreadable. Because listing is incomplete, neither
        // file may emit a `removed` event until a complete listing succeeds.
        fs::set_permissions(&locked_sub, fs::Permissions::from_mode(0o000)).unwrap();
        // Skip assertion if running as root where mode 0o000 is still readable.
        if fs::read_dir(&locked_sub).is_err() {
            fs::remove_file(&note).unwrap();
            clock.advance_ms(10_000);
            engine.scan_all();
            clock.advance_ms(310_000);
            assert!(engine.next_delivery(false).is_none());
            assert!(engine.folders()[0].cant_read > 0);
        }
        // Restore permissions so tempdir cleanup succeeds.
        fs::set_permissions(&locked_sub, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn changes_made_while_app_was_off_are_picked_up_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Docs");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        {
            let mut engine = Engine::open(state.clone(), tmp.path().to_path_buf(), clock.clone());
            let id = engine.begin_setup(&folder, None).unwrap();
            engine.confirm_folder(&id, Span::Today).unwrap();

            let initial = folder.join("one.md");
            fs::write(&initial, "first").unwrap();
            set_mtime_ms(&initial, t0 + 1_000);
            engine.scan_all();
            clock.advance_ms(310_000);
            let seg = engine.next_delivery(false).unwrap();
            engine.record_outcome(&seg, DeliveryOutcome::Delivered);
        }

        // App is closed. Owner creates a second file while app is off.
        let offline_file = folder.join("two.md");
        fs::write(&offline_file, "created while off").unwrap();
        set_mtime_ms(&offline_file, clock.now_ms() + 5_000);

        // App starts again later, scans, and delivers the missed file.
        clock.advance_ms(320_000);
        let mut reopened = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        reopened.scan_all();
        let seg = reopened.next_delivery(false).unwrap();
        assert_eq!(seg.events.len(), 1);
        assert_eq!(seg.events[0].rel_path, "two.md");
        assert_eq!(
            solstone_vault::Stamp::new(seg.events[0].event_ms, seg.events[0].offset_s).window(),
            seg.window
        );
    }

    #[test]
    fn refused_content_falls_back_to_details_only_and_increments_counter() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let folder = tmp.path().join("Drop");
        fs::create_dir_all(&folder).unwrap();

        let t0: i64 = 1_795_689_600_000;
        let clock = TestClock::new(t0 + 10_000, 0);
        let mut engine = Engine::open(state, tmp.path().to_path_buf(), clock.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::Today).unwrap();

        let bad_pdf = folder.join("corrupt.pdf");
        fs::write(&bad_pdf, b"not a valid pdf").unwrap();
        set_mtime_ms(&bad_pdf, t0 + 2_000);
        engine.scan_all();
        clock.advance_ms(310_000);

        let seg = engine.next_delivery(false).unwrap();
        assert_eq!(seg.content_files.len(), 1);
        engine.record_outcome(
            &seg,
            DeliveryOutcome::AllFilesRefused(vec!["corrupt.pdf".into()]),
        );
        assert_eq!(engine.folders()[0].fallback_to_details, 1);

        // Next delivery sends metadata-only (no companion content part).
        let retry_seg = engine.next_delivery(false).unwrap();
        assert_eq!(retry_seg.events.len(), 1);
        assert_eq!(retry_seg.events[0].delivery, DeliveryMode::MetadataOnly);
        assert!(retry_seg.content_files.is_empty());
    }
}
