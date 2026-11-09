// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The source: turning it on and off, scanning, closing windows, offering files to
//! deliver and taking the journal's answers, and the status it all adds up to.

use crate::{
    clock::{Clock, Stamp, Window},
    discover::{self, Refusal},
    outbox::{Delivery, Outbox},
    pending::{Key, Meta, Pending},
    platform::{self, DirFileIds},
    scan::{self, Note, VaultUnreadable, Walked},
    store,
    table::{FirstSync, Row, Table},
    wire::{self, Answer, Change, Line, MAX_FILE_BYTES, MAX_NOTE_BYTES},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// A window closes this long after it ends.
const WINDOW_GRACE_MS: i64 = 90_000;
/// The usual pause between scans.
const SCAN_INTERVAL: Duration = Duration::from_secs(60);
/// Scans slower than this get proportionally rarer.
const SLOW_SCAN: Duration = Duration::from_millis(600);
const MAX_SCAN_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// A held `obsidian_note_ahead` window is tried again after this long.
const AHEAD_RETRY_MS: i64 = 5 * 60_000;
const BACKOFF_INITIAL_MS: i64 = 30_000;
const BACKOFF_MAX_MS: i64 = 15 * 60_000;
/// A long scan checkpoints durably this often, so a crash or a turn-off loses
/// at most this much work.
const CHECKPOINT_NOTES: usize = 2_000;
const CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
/// How often a first sync's sent count is saved mid-delivery.
const PROGRESS_SAVE_INTERVAL_MS: i64 = 30_000;

/// How long until the next scan, given how long the last one took: scanning stays
/// near 1% of one core.
pub fn next_scan_delay(last_scan: Duration) -> Duration {
    if last_scan > SLOW_SCAN {
        (last_scan * 100).min(MAX_SCAN_INTERVAL)
    } else {
        SCAN_INTERVAL
    }
}

/// How the turned-on source is going.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Phase {
    /// Waiting to ask the journal whether it already has this vault.
    #[default]
    Checking,
    /// The journal has this vault from another device; the owner decides.
    Confirm {
        device: Option<String>,
        last_synced_at: Option<String>,
    },
    Active,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct SourceState {
    enabled: bool,
    vault_root: Option<String>,
    vault_name: Option<String>,
    phase: Phase,
}

/// A vault this journal has from another device.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ListedVault {
    pub vault: String,
    pub device: Option<String>,
    #[serde(default)]
    pub last_synced_at: Option<String>,
}

/// What a finished scan means for the caller's schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanOutcome {
    NotActive,
    Unreadable,
    Cancelled,
    Done,
}

/// What needs the owner's attention.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    VaultUnreadable,
    PartUnreadable,
    FileRefused,
    Storage,
}

/// The one thing the source is doing, for the owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Off,
    Checking,
    Confirm,
    On,
    Paused,
    WaitingForJournalUpdate,
    NeedsAttention,
}

/// What the owner sees.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Status {
    pub state: State,
    pub vault: Option<String>,
    pub warning: Option<ListedVault>,
    pub last_checked_ms: Option<i64>,
    pub waiting: u64,
    pub first_sync: Option<FirstSync>,
    pub not_on_device: u64,
    pub too_large: u64,
    pub attention: Option<Attention>,
}

#[derive(Default)]
struct Runtime {
    last_checked_ms: Option<i64>,
    not_on_device: u64,
    too_large: u64,
    vault_unreadable: bool,
    part_unreadable: bool,
    storage_fault: bool,
    file_lost: bool,
    last_scan: Duration,
}

enum ReadNote {
    Text {
        text: String,
        hash: String,
    },
    TooLarge,
    /// Gone since the walk.
    Gone,
    /// There, but it couldn't be read.
    Unreadable,
}

#[derive(Eq, PartialEq)]
enum Emitted {
    /// Placed in pending at this time.
    Placed(Stamp),
    /// Its window has already gone for this note and the clock has not moved past it.
    Deferred,
    /// The line would not fit in a file.
    TooLarge,
    /// A note first seen and removed before going anywhere: nothing to say.
    Dropped,
    /// Removed, but what the journal knows nets to nothing further to say.
    Netted,
}

struct Emission<'a> {
    change: Change,
    vault: &'a str,
    path: &'a str,
    previous_path: Option<&'a str>,
    changed_at: Stamp,
    observed_at: Stamp,
    text: Option<&'a str>,
    content_hash: Option<&'a str>,
    first_sight: bool,
    sent_window: Option<&'a Window>,
    rows: &'a BTreeMap<String, Row>,
}

/// A note that moved, ready to place in pending.
struct Rename<'a> {
    old: &'a str,
    path: &'a str,
    note: &'a Note,
    text: &'a str,
    hash: String,
    changed_at: Stamp,
    observed: Stamp,
}

/// A rename whose content did not change. It is stamped from the notes changed in
/// the same scan, so it waits until they have all been read.
struct PureRename<'a> {
    old: String,
    path: &'a str,
    note: &'a Note,
}

/// The earliest mtime among notes changed in this scan and newer than the previous
/// scan: when a rename's own link edits began.
struct EarliestChange {
    previous_scan_ms: Option<i64>,
    earliest_ms: Option<i64>,
}

impl EarliestChange {
    fn note(&mut self, mtime_ms: i64, now_ms: i64) {
        if mtime_ms <= now_ms
            && self
                .previous_scan_ms
                .is_some_and(|previous| mtime_ms > previous)
        {
            self.earliest_ms = Some(self.earliest_ms.map_or(mtime_ms, |ms| ms.min(mtime_ms)));
        }
    }
}

/// The source's whole held state, under one private directory.
pub struct Engine {
    root: PathBuf,
    clock: Arc<dyn Clock>,
    source: SourceState,
    table: Option<Table>,
    pending: Pending,
    outbox: Outbox,
    runtime: Runtime,
    retry_after_ms: Option<i64>,
    retry_step: u32,
    table_saved_ms: i64,
    table_dirty: bool,
    dir_file_ids: Option<Arc<DirFileIds>>,
}

impl Engine {
    pub fn open(root: PathBuf, clock: Arc<dyn Clock>) -> io::Result<Self> {
        store::ensure_dir(&root)?;
        store::ensure_dir(&root.join("tables"))?;
        let source = load_source(&root.join("source.json"))?;
        let pending = Pending::open(&root.join("pending.log"))?;
        let outbox = Outbox::open(&root.join("outbox"), &root.join("aside"))?;
        let mut engine = Self {
            root,
            clock,
            source,
            table: None,
            pending,
            outbox,
            runtime: Runtime::default(),
            retry_after_ms: None,
            retry_step: 0,
            table_saved_ms: 0,
            table_dirty: false,
            dir_file_ids: None,
        };
        if engine.source.enabled {
            engine.table = Some(Table::load(&engine.table_path())?);
        }
        engine.outbox.restore_aside()?;
        Ok(engine)
    }

    /// Read each directory's file ids this way where the platform's file metadata
    /// carries none, so a rename pairs by file id before content.
    pub fn with_dir_file_ids(mut self, read: Arc<DirFileIds>) -> Self {
        self.dir_file_ids = Some(read);
        self
    }

    fn vault_root(&self) -> Option<PathBuf> {
        self.source.vault_root.as_ref().map(PathBuf::from)
    }

    fn table_path(&self) -> PathBuf {
        let root = self.source.vault_root.as_deref().unwrap_or_default();
        self.root
            .join("tables")
            .join(format!("{}.json", &wire::sha256_hex(root.as_bytes())[..16]))
    }

    /// Beside the table rather than in it: this changes every scan, and the table
    /// is only rewritten when a note did.
    fn last_scan_path(&self) -> PathBuf {
        self.table_path().with_extension("scan")
    }

    fn previous_scan_ms(&self) -> Option<i64> {
        std::fs::read_to_string(self.last_scan_path())
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn save_source(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec(&self.source).map_err(io::Error::other)?;
        store::atomic_write(&self.root.join("source.json"), &bytes)
    }

    fn save_table(&mut self) -> io::Result<()> {
        if let Some(table) = &self.table {
            table.save(&self.table_path())?;
            self.table_saved_ms = self.clock.now_ms();
            self.table_dirty = false;
        }
        Ok(())
    }

    // Turning on and off

    /// Turn the source on for a chosen folder. Choosing another means turning this
    /// one off first, so a source already on stays as it is.
    pub fn turn_on(&mut self, folder: &Path) -> Result<(), TurnOnError> {
        if self.source.enabled {
            return Ok(());
        }
        let name = discover::check_vault_folder(folder).map_err(TurnOnError::Refused)?;
        let root = folder
            .to_str()
            .ok_or(TurnOnError::Refused(Refusal::UnusableName))?
            .to_owned();
        self.source = SourceState {
            enabled: true,
            vault_root: Some(root),
            vault_name: Some(name),
            phase: Phase::Checking,
        };
        let table = Table::load(&self.table_path()).map_err(TurnOnError::Io)?;
        if table.first_send_done {
            self.source.phase = Phase::Active;
        }
        self.table = Some(table);
        self.save_source().map_err(TurnOnError::Io)?;
        self.outbox.restore_aside().map_err(TurnOnError::Io)?;
        self.runtime = Runtime::default();
        Ok(())
    }

    /// Stops everything. What is waiting stays and goes when the source is on again.
    pub fn turn_off(&mut self) -> io::Result<()> {
        self.save_table()?;
        self.source.enabled = false;
        self.table = None;
        self.save_source()?;
        self.runtime = Runtime::default();
        Ok(())
    }

    pub fn is_on(&self) -> bool {
        self.source.enabled
    }

    pub fn phase(&self) -> Option<&Phase> {
        self.source.enabled.then_some(&self.source.phase)
    }

    /// The journal's list of vaults it holds from other devices, before the first send.
    /// The caller passes an empty list for a journal without the route.
    pub fn apply_vault_list(&mut self, listed: &[ListedVault]) -> io::Result<()> {
        if self.source.phase != Phase::Checking || !self.source.enabled {
            return Ok(());
        }
        let name = self.source.vault_name.clone().unwrap_or_default();
        match listed.iter().find(|vault| vault.vault == name) {
            Some(vault) => {
                self.source.phase = Phase::Confirm {
                    device: vault.device.clone(),
                    last_synced_at: vault.last_synced_at.clone(),
                };
                self.save_source()
            }
            None => self.mark_active(),
        }
    }

    pub fn continue_warning(&mut self) -> io::Result<()> {
        if matches!(self.source.phase, Phase::Confirm { .. }) && self.source.enabled {
            self.mark_active()?;
        }
        Ok(())
    }

    /// The owner cancelled: the source goes off and nothing was sent.
    pub fn cancel_warning(&mut self) -> io::Result<()> {
        if !self.source.enabled || !matches!(self.source.phase, Phase::Confirm { .. }) {
            return Ok(());
        }
        self.table = None;
        self.source = SourceState::default();
        self.save_source()
    }

    fn mark_active(&mut self) -> io::Result<()> {
        self.source.phase = Phase::Active;
        if let Some(table) = &mut self.table {
            table.first_send_done = true;
        }
        self.save_table()?;
        self.save_source()
    }

    // Scanning

    pub fn should_scan(&self) -> bool {
        self.source.enabled && self.source.phase == Phase::Active
    }

    pub fn last_scan_duration(&self) -> Duration {
        self.runtime.last_scan
    }

    /// One scan. Nothing it concludes depends on a vault it couldn't read.
    pub fn scan(&mut self, cancel: &AtomicBool) -> io::Result<ScanOutcome> {
        if !self.should_scan() || self.table.is_none() {
            return Ok(ScanOutcome::NotActive);
        }
        let (Some(root), Some(vault)) = (self.vault_root(), self.source.vault_name.clone()) else {
            return Ok(ScanOutcome::NotActive);
        };
        let scan_began_ms = self.clock.now_ms();
        // Only the walk sets the pace: saving what it found waits on the disk,
        // and a stalled write says nothing about the size of the vault.
        let started = Instant::now();
        let walk_result = scan::walk_with_ids(&root, cancel, self.dir_file_ids.as_deref());
        let walk_time = started.elapsed();
        let walked = match walk_result {
            Err(VaultUnreadable) => {
                self.runtime.vault_unreadable = true;
                return Ok(ScanOutcome::Unreadable);
            }
            Ok(walked) => walked,
        };
        self.runtime.vault_unreadable = false;
        if walked.cancelled {
            return Ok(ScanOutcome::Cancelled);
        }
        self.runtime.part_unreadable = !walked.unreadable_dirs.is_empty();
        let mut table = self.table.take().expect("checked above");
        let outcome = self.apply_walk(&root, &vault, &walked, &mut table, cancel);
        self.table = Some(table);
        match outcome {
            Err(error) => {
                self.runtime.storage_fault = true;
                return Err(error);
            }
            Ok(cancelled) if cancelled => {
                self.runtime.last_scan = walk_time;
                return Ok(ScanOutcome::Cancelled);
            }
            Ok(_) => {}
        }
        if let Err(error) =
            store::atomic_write(&self.last_scan_path(), scan_began_ms.to_string().as_bytes())
        {
            self.runtime.storage_fault = true;
            return Err(error);
        }
        self.runtime.storage_fault = false;
        self.runtime.not_on_device = walked.placeholders.len() as u64;
        self.runtime.last_checked_ms = Some(self.clock.now_ms());
        self.runtime.last_scan = walk_time;
        self.finish_first_sync_if_complete();
        Ok(ScanOutcome::Done)
    }

    fn apply_walk(
        &mut self,
        root: &Path,
        vault: &str,
        walked: &Walked,
        table: &mut Table,
        cancel: &AtomicBool,
    ) -> io::Result<bool> {
        let mut changed = false;
        if !table.first_sync_started {
            table.first_sync_started = true;
            table.first_sync = Some(FirstSync { found: 0, sent: 0 });
            changed = true;
        }
        let mut too_large = walked.too_large.len() as u64;
        let mut new_paths = Vec::new();
        let mut modified = Vec::new();
        for (path, note) in &walked.notes {
            match table.rows.get_mut(path) {
                None => new_paths.push(path.as_str()),
                Some(row) if row.mtime_ms == note.mtime_ms && row.size == note.size => {
                    if row.file_id != note.file_id {
                        row.file_id.clone_from(&note.file_id);
                        changed = true;
                    }
                }
                Some(_) => modified.push(path.as_str()),
            }
        }
        let missing: BTreeSet<String> = table
            .rows
            .iter()
            .filter(|(path, row)| {
                !walked.saw(path)
                    && !walked.under_unreadable_dir(path)
                    && row
                        .file_id
                        .as_ref()
                        .is_none_or(|id| !walked.too_large_ids.contains(id))
            })
            .map(|(path, _)| path.clone())
            .collect();
        let held_before = table.held.len();
        table
            .held
            .retain(|path, _| missing.contains(path) || walked.under_unreadable_dir(path));
        changed |= table.held.len() != held_before;

        // Rename pairing: by file id first, across all new paths, before any reading.
        let by_id: BTreeMap<String, String> = missing
            .iter()
            .filter_map(|path| {
                table.rows[path]
                    .file_id
                    .clone()
                    .map(|id| (id, path.clone()))
            })
            .collect();
        let mut free = missing.clone();
        let mut renamed_by_id: BTreeMap<String, String> = BTreeMap::new();
        for path in &new_paths {
            if let Some(id) = &walked.notes[*path].file_id
                && let Some(old) = by_id.get(id)
                && free.remove(old)
            {
                renamed_by_id.insert((*path).to_owned(), old.clone());
            }
        }
        let mut by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in &free {
            by_hash
                .entry(table.rows[path].content_hash.clone())
                .or_default()
                .push(path.clone());
        }
        let mut since_checkpoint = (0_usize, 0_usize);
        let mut earliest = EarliestChange {
            previous_scan_ms: self.previous_scan_ms(),
            earliest_ms: None,
        };
        let mut pure_renames: Vec<PureRename<'_>> = Vec::new();
        // A new path this scan couldn't read may be where a missing note of its size went.
        let mut unreadable_sizes: BTreeSet<u64> = BTreeSet::new();

        for path in &modified {
            if self.checkpoint_due(&since_checkpoint, cancel) {
                self.checkpoint(table)?;
                since_checkpoint = (0, 0);
                if cancel.load(Ordering::Relaxed) {
                    return Ok(true);
                }
            }
            let note = &walked.notes[*path];
            match read_note(&root.join(path), note.size) {
                ReadNote::Gone => {}
                ReadNote::Unreadable => self.runtime.part_unreadable = true,
                ReadNote::TooLarge => too_large += 1,
                ReadNote::Text { text, hash } => {
                    let row = table.rows[*path].clone();
                    since_checkpoint.1 += text.len();
                    if hash == row.content_hash {
                        let entry = table.rows.get_mut(*path).expect("row exists");
                        entry.mtime_ms = note.mtime_ms;
                        entry.size = note.size;
                        entry.file_id.clone_from(&note.file_id);
                        changed = true;
                        continue;
                    }
                    let observed = self.clock.now();
                    let changed_at =
                        self.changed_rule(note.mtime_ms, observed, row.last_reported_ms);
                    let emitted = self.emit(Emission {
                        change: Change::Changed,
                        vault,
                        path,
                        previous_path: None,
                        changed_at,
                        observed_at: observed,
                        text: Some(&text),
                        content_hash: Some(&hash),
                        first_sight: false,
                        sent_window: row.sent_window.as_ref(),
                        rows: &table.rows,
                    })?;
                    match emitted {
                        Emitted::Placed(at) => {
                            changed = true;
                            earliest.note(note.mtime_ms, observed.ms);
                            let entry = table.rows.get_mut(*path).expect("row exists");
                            entry.mtime_ms = note.mtime_ms;
                            entry.size = note.size;
                            entry.file_id.clone_from(&note.file_id);
                            entry.content_hash = hash;
                            entry.last_reported_ms = Some(at.ms);
                        }
                        Emitted::TooLarge => too_large += 1,
                        Emitted::Deferred | Emitted::Dropped | Emitted::Netted => {}
                    }
                }
            }
            since_checkpoint.0 += 1;
        }

        for path in &new_paths {
            if self.checkpoint_due(&since_checkpoint, cancel) {
                self.checkpoint(table)?;
                since_checkpoint = (0, 0);
                if cancel.load(Ordering::Relaxed) {
                    return Ok(true);
                }
            }
            let note = &walked.notes[*path];
            let (text, hash) = match read_note(&root.join(path), note.size) {
                ReadNote::Gone => continue,
                ReadNote::Unreadable => {
                    self.runtime.part_unreadable = true;
                    unreadable_sizes.insert(note.size);
                    continue;
                }
                ReadNote::TooLarge => {
                    too_large += 1;
                    continue;
                }
                ReadNote::Text { text, hash } => (text, hash),
            };
            since_checkpoint.0 += 1;
            since_checkpoint.1 += text.len();
            let observed = self.clock.now();
            let old = renamed_by_id.remove(*path).or_else(|| {
                let olds = by_hash.get_mut(&hash)?;
                let position = olds.iter().position(|old| free.contains(old))?;
                let old = olds.remove(position);
                free.remove(&old);
                Some(old)
            });
            if let Some(old) = old {
                let row = &table.rows[&old];
                if hash == row.content_hash {
                    pure_renames.push(PureRename { old, path, note });
                    continue;
                }
                let changed_at = self.changed_rule(note.mtime_ms, observed, row.last_reported_ms);
                let emitted = self.place_rename(
                    vault,
                    table,
                    Rename {
                        old: &old,
                        path,
                        note,
                        text: &text,
                        hash,
                        changed_at,
                        observed,
                    },
                )?;
                match emitted {
                    Emitted::Placed(_) => {
                        changed = true;
                        earliest.note(note.mtime_ms, observed.ms);
                    }
                    Emitted::TooLarge => too_large += 1,
                    // Left as it is: the next scan pairs it again.
                    Emitted::Deferred | Emitted::Dropped | Emitted::Netted => {}
                }
                continue;
            }
            let changed_at = self.changed_rule(note.mtime_ms, observed, None);
            let emitted = self.emit(Emission {
                change: Change::Changed,
                vault,
                path,
                previous_path: None,
                changed_at,
                observed_at: observed,
                text: Some(&text),
                content_hash: Some(&hash),
                first_sight: true,
                sent_window: None,
                rows: &table.rows,
            })?;
            match emitted {
                Emitted::Placed(at) => {
                    changed = true;
                    earliest.note(note.mtime_ms, observed.ms);
                    if let Some(first_sync) = &mut table.first_sync {
                        first_sync.found += 1;
                    }
                    table.rows.insert(
                        (*path).to_owned(),
                        Row {
                            mtime_ms: note.mtime_ms,
                            size: note.size,
                            file_id: note.file_id.clone(),
                            content_hash: hash,
                            last_reported_ms: Some(at.ms),
                            sent_window: None,
                        },
                    );
                }
                Emitted::TooLarge => too_large += 1,
                Emitted::Deferred | Emitted::Dropped | Emitted::Netted => {}
            }
        }

        // Renames with unchanged content, now that every changed note has been seen.
        // Their text is read again here so a large move never sits in memory.
        for rename in &pure_renames {
            if self.checkpoint_due(&since_checkpoint, cancel) {
                self.checkpoint(table)?;
                since_checkpoint = (0, 0);
                if cancel.load(Ordering::Relaxed) {
                    return Ok(true);
                }
            }
            let (text, hash) = match read_note(&root.join(rename.path), rename.note.size) {
                ReadNote::Gone => continue,
                ReadNote::Unreadable => {
                    self.runtime.part_unreadable = true;
                    continue;
                }
                ReadNote::TooLarge => {
                    too_large += 1;
                    continue;
                }
                ReadNote::Text { text, hash } => (text, hash),
            };
            since_checkpoint.0 += 1;
            since_checkpoint.1 += text.len();
            let observed = self.clock.now();
            let row = &table.rows[&rename.old];
            let changed_at = if hash == row.content_hash {
                let candidate = earliest
                    .earliest_ms
                    .map_or(observed, |ms| self.clock.stamp_at(ms));
                // The journal drops anything earlier than the note's latest as stale.
                match row.last_reported_ms {
                    Some(last) if last > candidate.ms => self.clock.stamp_at(last),
                    _ => candidate,
                }
            } else {
                // Edited since it was first read this scan.
                self.changed_rule(rename.note.mtime_ms, observed, row.last_reported_ms)
            };
            let emitted = self.place_rename(
                vault,
                table,
                Rename {
                    old: &rename.old,
                    path: rename.path,
                    note: rename.note,
                    text: &text,
                    hash,
                    changed_at,
                    observed,
                },
            )?;
            match emitted {
                Emitted::Placed(_) => changed = true,
                Emitted::TooLarge => too_large += 1,
                Emitted::Deferred | Emitted::Dropped | Emitted::Netted => {}
            }
        }

        // Whatever is still unpaired: a held delete, or a new hold. A note the scan
        // couldn't look at may be where a missing note of the same size went.
        let unknown_folder = unreachable_folder_is_unknown(walked, &table.rows);
        let placeholder_sizes: BTreeSet<u64> = walked
            .placeholders
            .iter()
            .filter(|(path, _)| !table.rows.contains_key(*path))
            .map(|(_, size)| *size)
            .collect();
        let scan_now = self.clock.now();
        for path in free {
            let size = table.rows[&path].size;
            if unreadable_sizes.contains(&size) {
                continue;
            }
            let Some(first_missing) = table.held.get(&path).copied() else {
                table.held.insert(path, scan_now);
                changed = true;
                continue;
            };
            if unknown_folder || placeholder_sizes.contains(&size) {
                continue;
            }
            let row = table.rows[&path].clone();
            let emitted = self.emit(Emission {
                change: Change::Removed,
                vault,
                path: &path,
                previous_path: None,
                changed_at: first_missing,
                observed_at: first_missing,
                text: None,
                content_hash: None,
                first_sight: false,
                sent_window: row.sent_window.as_ref(),
                rows: &table.rows,
            })?;
            match emitted {
                Emitted::Deferred => {}
                Emitted::Dropped => {
                    changed = true;
                    if let Some(first_sync) = &mut table.first_sync {
                        first_sync.found = first_sync.found.saturating_sub(1);
                    }
                    table.rows.remove(&path);
                    table.held.remove(&path);
                }
                Emitted::Placed(_) | Emitted::TooLarge | Emitted::Netted => {
                    changed = true;
                    table.rows.remove(&path);
                    table.held.remove(&path);
                }
            }
        }

        self.runtime.too_large = too_large;
        if changed || self.table_dirty {
            self.checkpoint(table)?;
        }
        Ok(false)
    }

    /// Put a rename in pending and move the note's row to its new path.
    fn place_rename(
        &mut self,
        vault: &str,
        table: &mut Table,
        rename: Rename<'_>,
    ) -> io::Result<Emitted> {
        let row = table.rows[rename.old].clone();
        let emitted = self.emit(Emission {
            change: Change::Renamed,
            vault,
            path: rename.path,
            previous_path: Some(rename.old),
            changed_at: rename.changed_at,
            observed_at: rename.observed,
            text: Some(rename.text),
            content_hash: Some(&rename.hash),
            first_sight: false,
            sent_window: row.sent_window.as_ref(),
            rows: &table.rows,
        })?;
        if let Emitted::Placed(at) = emitted {
            table.rows.remove(rename.old);
            table.held.remove(rename.old);
            table.rows.insert(
                rename.path.to_owned(),
                Row {
                    mtime_ms: rename.note.mtime_ms,
                    size: rename.note.size,
                    file_id: rename.note.file_id.clone(),
                    content_hash: rename.hash,
                    last_reported_ms: Some(at.ms),
                    sent_window: row.sent_window,
                },
            );
        }
        Ok(emitted)
    }

    fn checkpoint_due(&self, since: &(usize, usize), cancel: &AtomicBool) -> bool {
        since.0 >= CHECKPOINT_NOTES
            || since.1 >= CHECKPOINT_BYTES
            || (since.0 > 0 && since.0.is_multiple_of(256) && cancel.load(Ordering::Relaxed))
    }

    /// The pending log first, then the table: a change is never recorded as known
    /// before the change itself is durable.
    fn checkpoint(&mut self, table: &mut Table) -> io::Result<()> {
        self.pending.sync()?;
        table.save(&self.table_path())?;
        self.table_saved_ms = self.clock.now_ms();
        self.table_dirty = false;
        Ok(())
    }

    /// The contract's rule: the file's mtime, unless it is later than now or isn't
    /// later than the note's last reported change while its content changed.
    fn changed_rule(&self, mtime_ms: i64, observed: Stamp, last_reported_ms: Option<i64>) -> Stamp {
        if mtime_ms > observed.ms || last_reported_ms.is_some_and(|last| mtime_ms <= last) {
            observed
        } else {
            self.clock.stamp_at(mtime_ms)
        }
    }

    /// Put one change into pending, netting it against what the same window holds
    /// for the same note, and honouring the note's already-sent window.
    fn emit(&mut self, change: Emission<'_>) -> io::Result<Emitted> {
        let mut changed_at = change.changed_at;
        let mut observed_at = change.observed_at;
        if let Some(sent) = change.sent_window
            && changed_at.window() <= *sent
        {
            // One line per note per window: a window already gone moves to the observed
            // time, and if even the clock hasn't moved past it, a later scan picks it up.
            changed_at = observed_at;
            if changed_at.window() <= *sent {
                changed_at = self.clock.now();
                observed_at = changed_at;
            }
            // A sent window later than now means the clock went back: waiting for it
            // could take any length of time, and this window is a different one.
            if changed_at.window() == *sent {
                return Ok(Emitted::Deferred);
            }
        }
        let line = Line {
            change: change.change,
            changed_at: changed_at.rfc3339(),
            content_hash: change.content_hash.map(str::to_owned),
            observed_at: observed_at.rfc3339(),
            path: change.path.to_owned(),
            previous_path: change.previous_path.map(str::to_owned),
            text: change.text.map(str::to_owned),
            v: 1,
            vault: change.vault.to_owned(),
        };
        if line.to_json().len() >= MAX_FILE_BYTES {
            return Ok(Emitted::TooLarge);
        }
        let window = changed_at.window();
        let mut meta = Meta {
            change: change.change,
            vault: change.vault.to_owned(),
            path: change.path.to_owned(),
            previous_path: change.previous_path.map(str::to_owned),
            changed_at,
            observed_at,
            content_hash: change.content_hash.map(str::to_owned),
            first_sight: change.first_sight,
        };
        let here: Key = (
            window.clone(),
            change.vault.to_owned(),
            change.path.to_owned(),
        );
        let mut replace = Vec::new();
        match change.change {
            Change::Changed => {
                if let Some(existing) = self.pending.get(&here).cloned() {
                    replace.push(here);
                    meta.first_sight = existing.first_sight;
                    if existing.change == Change::Renamed {
                        meta.change = Change::Renamed;
                        meta.previous_path = existing.previous_path;
                    }
                }
            }
            Change::Renamed => {
                if self.pending.get(&here).is_some() {
                    replace.push(here);
                }
                let from: Key = (
                    window.clone(),
                    change.vault.to_owned(),
                    change.previous_path.unwrap_or("").to_owned(),
                );
                if let Some(existing) = self.pending.get(&from).cloned() {
                    replace.push(from);
                    match existing.change {
                        Change::Changed if existing.first_sight => {
                            // The journal has never heard of this note under its old
                            // name either: to it, this is first sight at the new one.
                            meta.change = Change::Changed;
                            meta.previous_path = None;
                            meta.first_sight = true;
                        }
                        Change::Renamed
                            if existing.previous_path.as_deref() == Some(change.path) =>
                        {
                            // Renamed away and back within the window: only the text matters.
                            meta.change = Change::Changed;
                            meta.previous_path = None;
                        }
                        Change::Renamed => meta.previous_path = existing.previous_path,
                        Change::Changed | Change::Removed => {}
                    }
                }
            }
            Change::Removed => {
                if let Some(existing) = self.pending.get(&here).cloned() {
                    replace.push(here);
                    match existing.change {
                        Change::Changed if existing.first_sight => {
                            self.pending.put(&replace, None)?;
                            return Ok(Emitted::Dropped);
                        }
                        Change::Renamed => {
                            // It never reached the journal under this name: delete the old
                            // one, unless a live note has taken that name since.
                            let origin = existing.previous_path.clone().unwrap_or_default();
                            if change.rows.contains_key(&origin) {
                                self.pending.put(&replace, None)?;
                                return Ok(Emitted::Netted);
                            }
                            replace.push((window.clone(), change.vault.to_owned(), origin.clone()));
                            meta.path = origin;
                        }
                        Change::Changed | Change::Removed => {}
                    }
                }
            }
        }
        self.pending
            .put(&replace, Some((&window, &meta, change.text)))?;
        Ok(Emitted::Placed(changed_at))
    }

    fn finish_first_sync_if_complete(&mut self) {
        let done = self
            .table
            .as_ref()
            .and_then(|table| table.first_sync)
            .is_some_and(|sync| {
                sync.sent >= sync.found
                    && self.pending.first_sight_count() == 0
                    && self.outbox_first_sight() == 0
            });
        if done {
            if let Some(table) = &mut self.table {
                table.first_sync = None;
            }
            self.table_dirty = true;
        }
    }

    fn outbox_first_sight(&self) -> u64 {
        self.outbox_counts().1
    }

    fn outbox_counts(&self) -> (u64, u64) {
        let mut lines = 0;
        let mut first_sight = 0;
        for delivery in self.outbox.iter() {
            lines += u64::from(delivery.lines);
            first_sight += u64::from(delivery.first_sight);
        }
        (lines, first_sight)
    }

    // Windows and delivery

    /// Close windows that ended at least 90 s ago: their lines become files in the
    /// outbox, and the pending entries go. Bounded so a very long history doesn't
    /// hold up everything else.
    pub fn close_due_windows(&mut self, max_windows: usize) -> io::Result<usize> {
        let due = self
            .pending
            .due_windows(self.clock.now_ms(), WINDOW_GRACE_MS);
        let mut closed_keys: Vec<Key> = Vec::new();
        let mut sent: Vec<(String, Window)> = Vec::new();
        let mut damaged: Vec<Meta> = Vec::new();
        let mut closed = 0;
        for window in due.iter().take(max_windows) {
            let keys = self.pending.window_keys_sorted(window);
            let mut builder = wire::FileBuilder::new(window.clone());
            for key in &keys {
                // A record that can't be decoded can't be sent, so the next scan
                // reports that note afresh. Any other failure leaves the whole window
                // pending for the next try.
                let (line, first_sight) = match self.pending.read_line(key) {
                    Ok(read) => read,
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        damaged.extend(self.pending.get(key).cloned());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if line.change != Change::Removed
                    && self.source.vault_name.as_deref() == Some(line.vault.as_str())
                {
                    sent.push((line.path.clone(), window.clone()));
                }
                if let Some(file) = builder.push(&line.to_json(), first_sight) {
                    self.outbox.write(&file)?;
                }
            }
            if let Some(file) = builder.finish() {
                self.outbox.write(&file)?;
            }
            closed_keys.extend(keys);
            closed += 1;
        }
        if closed == 0 {
            return Ok(0);
        }
        if let Some(table) = &mut self.table {
            for (path, window) in sent {
                if let Some(row) = table.rows.get_mut(&path)
                    && row.sent_window.as_ref().is_none_or(|sent| window > *sent)
                {
                    row.sent_window = Some(window);
                }
            }
            for meta in &damaged {
                if self.source.vault_name.as_deref() != Some(meta.vault.as_str()) {
                    continue;
                }
                if let Some(row) = table.rows.get_mut(&meta.path) {
                    row.mtime_ms = i64::MIN;
                    row.content_hash.clear();
                }
                if meta.first_sight
                    && let Some(sync) = &mut table.first_sync
                {
                    sync.found = sync.found.saturating_sub(1);
                }
            }
        }
        // The files are durable before the windows are forgotten, and the table is
        // saved before the pending entries go: a crash in between re-closes the
        // same windows into the same bytes under the same names.
        self.save_table()?;
        self.pending.remove_keys(&closed_keys)?;
        self.pending.sync()?;
        Ok(closed)
    }

    /// The next file to deliver, oldest window first, if backoff allows.
    pub fn next_delivery(&self) -> Option<Delivery> {
        if self
            .retry_after_ms
            .is_some_and(|after| self.clock.now_ms() < after)
        {
            return None;
        }
        self.outbox.oldest().cloned()
    }

    /// Take the journal's answer for a file.
    pub fn record_answer(&mut self, delivery: &Delivery, answer: Answer) -> io::Result<()> {
        match answer {
            Answer::Delivered => {
                self.outbox.remove(delivery)?;
                self.retry_after_ms = None;
                self.retry_step = 0;
                self.runtime.file_lost = false;
                if delivery.first_sight > 0 {
                    if let Some(sync) = self
                        .table
                        .as_mut()
                        .and_then(|table| table.first_sync.as_mut())
                    {
                        sync.sent += u64::from(delivery.first_sight);
                    }
                    self.table_dirty = true;
                }
                self.finish_first_sync_if_complete();
                self.flush_progress()?;
            }
            Answer::Ahead => {
                self.retry_after_ms = Some(self.clock.now_ms() + AHEAD_RETRY_MS);
            }
            Answer::Invalid => {
                self.outbox.set_aside(delivery)?;
                self.retry_after_ms = None;
            }
            Answer::Retry => {
                self.retry_step = self.retry_step.saturating_add(1);
                let backoff = BACKOFF_INITIAL_MS
                    .saturating_mul(1 << self.retry_step.min(16).saturating_sub(1))
                    .min(BACKOFF_MAX_MS);
                self.retry_after_ms = Some(self.clock.now_ms() + backoff);
            }
        }
        Ok(())
    }

    /// A file waiting in the outbox is gone: something outside the app removed it,
    /// antivirus for one. Its notes go back to unreported so the next scan sends
    /// them again, and the source says some couldn't go.
    pub fn record_file_missing(&mut self, delivery: &Delivery) -> io::Result<()> {
        self.outbox.remove(delivery)?;
        self.retry_after_ms = None;
        self.retry_step = 0;
        self.runtime.file_lost = true;
        if let Some(table) = &mut self.table {
            for row in table.rows.values_mut() {
                if row.sent_window.as_ref() == Some(&delivery.window) {
                    row.sent_window = None;
                    row.mtime_ms = i64::MIN;
                    row.content_hash.clear();
                    row.last_reported_ms = None;
                }
            }
            if let Some(sync) = &mut table.first_sync {
                sync.found = sync.found.saturating_sub(u64::from(delivery.first_sight));
            }
        }
        self.finish_first_sync_if_complete();
        self.save_table()
    }

    /// Save first-sync progress, at most every so often.
    pub fn flush_progress(&mut self) -> io::Result<()> {
        if self.table_dirty
            && self.clock.now_ms() - self.table_saved_ms >= PROGRESS_SAVE_INTERVAL_MS
        {
            self.save_table()?;
        }
        Ok(())
    }

    /// Save anything left unsaved. For shutdown.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.table_dirty {
            self.save_table()?;
        }
        self.pending.sync()
    }

    /// The status the owner sees. `paused` and `capability` are the caller's facts.
    pub fn status(&self, paused: bool, capability: Option<bool>) -> Status {
        let (outbox_lines, _) = self.outbox_counts();
        let waiting = self.pending.len() as u64 + outbox_lines;
        let attention = if self.runtime.vault_unreadable {
            Some(Attention::VaultUnreadable)
        } else if self.runtime.storage_fault {
            Some(Attention::Storage)
        } else if self.runtime.part_unreadable {
            Some(Attention::PartUnreadable)
        } else if self.outbox.aside_count() > 0 || self.runtime.file_lost {
            Some(Attention::FileRefused)
        } else {
            None
        };
        let state = if !self.source.enabled {
            State::Off
        } else if matches!(self.source.phase, Phase::Confirm { .. }) {
            State::Confirm
        } else if self.source.phase == Phase::Checking {
            State::Checking
        } else if attention.is_some() {
            State::NeedsAttention
        } else if paused {
            State::Paused
        } else if capability == Some(false) {
            State::WaitingForJournalUpdate
        } else {
            State::On
        };
        let warning = match &self.source.phase {
            Phase::Confirm {
                device,
                last_synced_at,
            } if self.source.enabled => Some(ListedVault {
                vault: self.source.vault_name.clone().unwrap_or_default(),
                device: device.clone(),
                last_synced_at: last_synced_at.clone(),
            }),
            _ => None,
        };
        Status {
            state,
            vault: self
                .source
                .enabled
                .then(|| self.source.vault_name.clone())
                .flatten(),
            warning,
            last_checked_ms: self.runtime.last_checked_ms,
            waiting,
            first_sync: self.table.as_ref().and_then(|table| table.first_sync),
            not_on_device: self.runtime.not_on_device,
            too_large: self.runtime.too_large,
            attention,
        }
    }
}

/// Whether a directory this scan couldn't reach is one the table knows nothing
/// about, so a missing note may have moved into it.
fn unreachable_folder_is_unknown(walked: &Walked, rows: &BTreeMap<String, Row>) -> bool {
    walked.unreadable_dirs.iter().any(|prefix| {
        rows.range(prefix.clone()..)
            .next()
            .is_none_or(|(path, _)| !path.starts_with(prefix.as_str()))
    })
}

fn load_source(path: &Path) -> io::Result<SourceState> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SourceState::default()),
        Err(error) => Err(error),
    }
}

fn read_note(path: &Path, size: u64) -> ReadNote {
    if size > MAX_NOTE_BYTES as u64 {
        return ReadNote::TooLarge;
    }
    let file = match platform::open_for_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return ReadNote::Gone,
        Err(_) => return ReadNote::Unreadable,
    };
    let mut bytes = Vec::with_capacity(size as usize);
    if file
        .take((MAX_NOTE_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return ReadNote::Unreadable;
    }
    if bytes.len() > MAX_NOTE_BYTES {
        return ReadNote::TooLarge;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.len() > MAX_NOTE_BYTES {
        return ReadNote::TooLarge;
    }
    let hash = wire::sha256_hex(text.as_bytes());
    ReadNote::Text { text, hash }
}

/// A folder couldn't become the source, and why.
#[derive(Debug)]
pub enum TurnOnError {
    Refused(Refusal),
    Io(io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            mtime_ms: 0,
            size: 0,
            file_id: None,
            content_hash: String::new(),
            last_reported_ms: None,
            sent_window: None,
        }
    }

    #[test]
    fn only_an_unreachable_folder_with_no_rows_is_unknown() {
        let mut rows = BTreeMap::new();
        rows.insert("known/a.md".to_owned(), row());

        let mut walked = Walked::default();
        assert!(!unreachable_folder_is_unknown(&walked, &rows));
        walked.unreadable_dirs.push("known/".to_owned());
        assert!(!unreachable_folder_is_unknown(&walked, &rows), "has a row");
        walked.unreadable_dirs.push("new/".to_owned());
        assert!(unreachable_folder_is_unknown(&walked, &rows));
    }
}
