// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What the app holds for the journal from the browser, on this computer.
//!
//! - A batch is accepted only after its records and its receipt are on disk, so a
//!   replayed batch id is answered `duplicate` and nothing is taken in twice.
//! - Text is kept in five-minute periods, one `browser_pages.jsonl` each. A finished
//!   period becomes a capture segment under the `_browser` stream folder, and the sync
//!   service delivers it as the `browser` source.
//! - Everything is stamped with a destination generation, which belongs to exactly one
//!   journal: the paired instance plus its CA chain. When the app finds itself paired
//!   to a different journal, everything held for the old one is retired before
//!   anything else runs. Retired text is kept and shown, never delivered or counted,
//!   until the owner discards it.
//! - One bound covers everything held: past it, new batches are refused as
//!   `queue_full` and the extension is told custody is full.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use native_browser_frame::{
    ACCEPTED_RETENTION_MS_MIN, FILE_MAX, FUTURE_SKEW_MS_MAX, OUTBOX_AGE_MS_MAX, SPOOL_BYTES_MAX,
    canonical_stringify,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Sidecar naming the period and its generation. Dot files are never uploaded.
pub const PERIOD_FILE: &str = ".browser_period.json";
/// The receipts that cover the period's payload, for duplicate answers.
pub const RECEIPTS_FILE: &str = ".browser_receipts.jsonl";
const GENERATION_FILE: &str = "generation.json";
const PERIOD_MINUTES: u32 = 5;
/// The longest a finished period can be, and so the ceiling of its segment length.
const PERIOD_MS: u64 = PERIOD_MINUTES as u64 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub captures: PathBuf,
}

impl Layout {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            root: super::custody_root(base_dir),
            captures: base_dir.join("captures"),
        }
    }
    fn open(&self) -> PathBuf {
        self.root.join("open")
    }
    pub fn retired(&self) -> PathBuf {
        self.root.join("retired")
    }
    fn generation(&self) -> PathBuf {
        self.root.join(GENERATION_FILE)
    }
}

/// The strict identity of a journal: the exact instance and its CA chain, with PEM
/// formatting differences removed. Never instance alone.
pub fn journal_identity(instance_id: &str, ca_chain_pem: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"solstone-browser-journal-v1\0");
    digest.update(instance_id.as_bytes());
    // One separator per certificate, however the chain's PEM text is split.
    for line in ca_chain_pem
        .iter()
        .flat_map(|pem| pem.lines())
        .map(str::trim)
    {
        if line.starts_with("-----BEGIN") {
            digest.update(b"\0");
        } else if !line.is_empty() && !line.starts_with("-----") {
            digest.update(line.as_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

#[derive(Serialize, Deserialize)]
struct GenerationRecord {
    journal: String,
    generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PeriodRecord {
    period_id: String,
    generation: String,
    created_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct ReceiptLine {
    inst: String,
    batch_id: String,
    end: u64,
    at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bucket {
    date: NaiveDate,
    index: u32,
}

impl Bucket {
    fn of(now: &DateTime<Local>) -> Self {
        Self {
            date: now.date_naive(),
            index: (now.hour() * 60 + now.minute()) / PERIOD_MINUTES,
        }
    }
    fn start_ms(&self) -> Option<u64> {
        let minutes = self.index * PERIOD_MINUTES;
        let naive = self.date.and_hms_opt(minutes / 60, minutes % 60, 0)?;
        let local = Local.from_local_datetime(&naive).earliest()?;
        u64::try_from(local.timestamp_millis()).ok()
    }
}

#[derive(Debug)]
struct Period {
    id: String,
    bucket: Bucket,
    created_at_ms: u64,
    committed: u64,
    receipts_len: u64,
    contexts: HashSet<(String, String)>,
    has_dir: bool,
}

#[derive(Clone, Debug)]
struct Receipt {
    period_id: String,
    at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted { period_id: String },
    Duplicate { period_id: String },
    Rejected { reason: &'static str },
}

/// What the extension is told about capture and delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub capture: &'static str,
    pub delivery: &'static str,
    pub failure: Option<&'static str>,
    pub generation: Option<String>,
    pub period_id: Option<String>,
    pub full: bool,
    pub held_bytes: u64,
}

pub struct Custody {
    layout: Layout,
    generation: Option<String>,
    period: Option<Period>,
    receipts: HashMap<(String, String), Receipt>,
    /// Finished periods that could not yet be turned into segments; retried each tick.
    unfinished: Vec<(Period, DateTime<Local>)>,
    held_bytes: u64,
    bound: u64,
}

impl Custody {
    /// Open custody for the journal this app is paired with (`None` when unpaired).
    /// Retirement happens here, before the sync service can deliver anything.
    pub fn open(layout: Layout, journal: Option<&str>, now: DateTime<Local>) -> io::Result<Self> {
        create_private_dir(&layout.root)?;
        create_private_dir(&layout.open())?;
        create_private_dir(&layout.retired())?;
        let generation = match journal {
            None => None,
            Some(journal) => Some(generation_for(&layout, journal)?),
        };
        let mut custody = Self {
            layout,
            generation,
            period: None,
            receipts: HashMap::new(),
            unfinished: Vec::new(),
            held_bytes: 0,
            bound: SPOOL_BYTES_MAX as u64,
        };
        if custody.generation.is_some() {
            custody.retire_foreign(now);
            custody.recover_open(now);
            custody.load_finalized_receipts();
            if custody.period.is_none() {
                custody.period = Some(new_period(&now));
            }
        }
        custody.refresh_held();
        Ok(custody)
    }

    #[cfg(test)]
    fn with_bound(mut self, bound: u64) -> Self {
        self.bound = bound;
        self
    }

    pub fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }

    pub fn period_id(&self) -> Option<&str> {
        self.period.as_ref().map(|period| period.id.as_str())
    }

    pub fn facts(&self, paused: bool) -> Facts {
        let held = self.held_bytes > 0;
        let Some(generation) = &self.generation else {
            return Facts {
                capture: "not_paired",
                delivery: if held { "kept_locally" } else { "unknown" },
                failure: None,
                generation: None,
                period_id: None,
                full: false,
                held_bytes: self.held_bytes,
            };
        };
        let full = self.held_bytes >= self.bound;
        Facts {
            capture: if paused {
                "paused"
            } else if full {
                "intake_off"
            } else {
                "permitted"
            },
            delivery: if held { "kept_locally" } else { "idle" },
            failure: full.then_some("queue_full"),
            generation: Some(generation.clone()),
            period_id: self.period_id().map(str::to_owned),
            full,
            held_bytes: self.held_bytes,
        }
    }

    /// Start a new period when the five-minute local-clock bucket changes. Returns the
    /// new period id, which every connected browser must hear as a `boundary`.
    pub fn tick(&mut self, now: DateTime<Local>) -> Option<String> {
        self.generation.as_ref()?;
        if !self.unfinished.is_empty() {
            let pending = std::mem::take(&mut self.unfinished);
            for (period, end) in pending {
                if let Err(error) = self.finalize(&period, end) {
                    tracing::debug!(%error, "A browser period is still waiting to finish");
                    self.unfinished.push((period, end));
                }
            }
        }
        let bucket = Bucket::of(&now);
        if self
            .period
            .as_ref()
            .is_some_and(|period| period.bucket == bucket)
        {
            return None;
        }
        self.rotate(now)
    }

    fn rotate(&mut self, now: DateTime<Local>) -> Option<String> {
        if let Some(period) = self.period.take()
            && let Err(error) = self.finalize(&period, now)
        {
            tracing::warn!(%error, "Could not finish a browser period yet; it stays held");
            self.unfinished.push((period, now));
        }
        self.prune_receipts(now_ms(&now));
        let period = new_period(&now);
        let id = period.id.clone();
        self.period = Some(period);
        self.refresh_held();
        Some(id)
    }

    /// Accept one decoded batch. The second value is a new period id when accepting it
    /// had to start a new period.
    pub fn accept(&mut self, batch: &Value, now: DateTime<Local>) -> (Outcome, Option<String>) {
        let field = |name: &str| batch.get(name).and_then(Value::as_str).unwrap_or_default();
        let (generation, inst, batch_id) = (
            field("destination_generation"),
            field("inst"),
            field("batch_id"),
        );
        let key = (inst.to_owned(), batch_id.to_owned());
        if self.generation.as_deref() == Some(generation)
            && let Some(receipt) = self.receipts.get(&key)
        {
            return (
                Outcome::Duplicate {
                    period_id: receipt.period_id.clone(),
                },
                None,
            );
        }
        if self.generation.as_deref() != Some(generation) || self.period.is_none() {
            return (reject("stale_generation"), None);
        }
        let now_ms = now_ms(&now);
        let queued_at = batch
            .get("queued_at_ms")
            .and_then(native_browser_frame::codec::nonnegative_integer)
            .unwrap_or(0);
        if queued_at > now_ms.saturating_add(FUTURE_SKEW_MS_MAX) {
            return (reject("age_policy"), None);
        }
        if now_ms.saturating_sub(queued_at) >= OUTBOX_AGE_MS_MAX {
            return (reject("expired_unaccepted"), None);
        }
        let Some(records) = batch.get("records").and_then(Value::as_array) else {
            return (reject("malformed"), None);
        };
        let mut payload = String::new();
        for record in records {
            if canonical_stringify(record, &mut payload).is_err() {
                return (reject("malformed"), None);
            }
            payload.push('\n');
        }
        let bytes = payload.len() as u64;
        if bytes > FILE_MAX as u64 {
            return (reject("resource_exhausted"), None);
        }
        let mut rotated = None;
        if self.period.as_ref().is_some_and(|period| {
            period.committed > 0 && period.committed + bytes > FILE_MAX as u64
        }) {
            rotated = self.rotate(now);
        }
        let first = &records[0];
        let context = (
            inst.to_owned(),
            first
                .get("ctx")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        let snapshot = first.get("t").and_then(Value::as_str) == Some("segment_start");
        let period = self
            .period
            .as_ref()
            .expect("a generation always has a period");
        if !snapshot && !period.contexts.contains(&context) {
            return (reject("snapshot_required"), rotated);
        }
        if self.held_bytes + bytes > self.bound {
            return (reject("queue_full"), rotated);
        }
        match self.commit(&payload, inst, batch_id, now_ms) {
            Ok(period_id) => {
                let period = self.period.as_mut().expect("committed into a period");
                if snapshot {
                    period.contexts.insert(context);
                }
                self.receipts.insert(
                    key,
                    Receipt {
                        period_id: period_id.clone(),
                        at_ms: now_ms,
                    },
                );
                self.held_bytes += bytes;
                (Outcome::Accepted { period_id }, rotated)
            }
            Err(error) => {
                tracing::warn!(%error, "Could not keep a browser batch");
                (reject("resource_exhausted"), rotated)
            }
        }
    }

    /// Write the records, then the receipt that covers them, each at the length this
    /// process knows is committed. Bytes past that length are never covered by a
    /// receipt, so a failed or torn write is overwritten by the next one, and a crash
    /// leaves them to be cut back on reopen.
    fn commit(
        &mut self,
        payload: &str,
        inst: &str,
        batch_id: &str,
        at_ms: u64,
    ) -> io::Result<String> {
        let generation = self.generation.clone().expect("commit needs a generation");
        let open_root = self.layout.open();
        let period = self.period.as_mut().expect("commit needs a period");
        let directory = open_root.join(&period.id);
        let first = !period.has_dir;
        if first {
            create_private_dir(&directory)?;
            write_atomic(
                &directory.join(PERIOD_FILE),
                &serde_json::to_vec(&PeriodRecord {
                    period_id: period.id.clone(),
                    generation,
                    created_at_ms: period.created_at_ms,
                })
                .map_err(io::Error::other)?,
            )?;
            sync_dir(&open_root)?;
            period.has_dir = true;
        }
        let end = period.committed + payload.len() as u64;
        let mut line = serde_json::to_vec(&ReceiptLine {
            inst: inst.to_owned(),
            batch_id: batch_id.to_owned(),
            end,
            at_ms,
        })
        .map_err(io::Error::other)?;
        line.push(b'\n');
        write_at(
            &directory.join(super::PAGES_FILENAME),
            period.committed,
            payload.as_bytes(),
        )?;
        write_at(&directory.join(RECEIPTS_FILE), period.receipts_len, &line)?;
        if first {
            // The payload and receipt files are new: make their names durable too.
            sync_dir(&directory)?;
        }
        period.committed = end;
        period.receipts_len += line.len() as u64;
        Ok(period.id.clone())
    }

    /// Turn a finished period into a capture segment the sync service delivers.
    fn finalize(&self, period: &Period, end: DateTime<Local>) -> io::Result<()> {
        let directory = self.layout.open().join(&period.id);
        if !period.has_dir {
            return Ok(());
        }
        if truncate_to_receipts(&directory)? == 0 {
            return remove_dir_all_if_present(&directory);
        }
        let bucket_start = period.bucket.start_ms();
        let start_ms = bucket_start
            .unwrap_or(period.created_at_ms)
            .max(period.created_at_ms);
        // A period never runs past its own window, however late it is finished: the
        // first tick after a suspend can come hours after the window closed.
        let window_end = bucket_start
            .map(|bucket_start| bucket_start + PERIOD_MS)
            .filter(|window_end| *window_end > start_ms)
            .unwrap_or(start_ms + PERIOD_MS);
        let captures = &self.layout.captures;
        move_period(
            &directory,
            |day| captures.join(day).join(super::STREAM_DIR),
            start_ms,
            now_ms(&end).min(window_end),
        )
    }

    /// Move everything held for any other journal out of reach of delivery. Each item
    /// is handled on its own: one that cannot be moved now stays where it is, and the
    /// sync service still never delivers it, because it is not this generation's.
    fn retire_foreign(&mut self, now: DateTime<Local>) {
        let current = self.generation.clone().expect("retire needs a generation");
        for directory in sorted_dirs(&self.layout.open()) {
            let record = read_period(&directory);
            if record
                .as_ref()
                .is_some_and(|record| record.generation == current)
            {
                continue;
            }
            let label = record
                .as_ref()
                .map_or("unknown", |record| record.generation.as_str());
            let result = (|| -> io::Result<()> {
                if truncate_to_receipts(&directory)? == 0 {
                    return remove_dir_all_if_present(&directory);
                }
                let start_ms = record
                    .as_ref()
                    .map_or_else(|| now_ms(&now), |r| r.created_at_ms);
                let end_ms = last_accepted_ms(&directory)
                    .or_else(|| modified_ms(&directory.join(super::PAGES_FILENAME)))
                    .unwrap_or(start_ms);
                let retired = self.layout.retired().join(safe_label(label));
                move_period(
                    &directory,
                    |day| retired.join(day),
                    start_ms,
                    end_ms.max(start_ms),
                )
            })();
            if let Err(error) = result {
                tracing::warn!(%error, "Could not retire an open browser period");
            }
        }
        for segment in browser_segments(&self.layout.captures) {
            let generation = read_period(&segment).map(|record| record.generation);
            if generation.as_deref() == Some(current.as_str()) {
                continue;
            }
            let (Some(key), Some(day)) = (
                segment.file_name(),
                segment
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name),
            ) else {
                continue;
            };
            let parent = self
                .layout
                .retired()
                .join(safe_label(generation.as_deref().unwrap_or("unknown")))
                .join(day);
            let result = fs::create_dir_all(&parent)
                .and_then(|()| fs::rename(&segment, parent.join(key)))
                .and_then(|()| sync_dir(&parent));
            if let Err(error) = result {
                tracing::warn!(%error, "Could not retire a browser period");
            }
        }
    }

    fn recover_open(&mut self, now: DateTime<Local>) {
        let current = self.generation.clone().expect("recover needs a generation");
        let bucket = Bucket::of(&now);
        for directory in sorted_dirs(&self.layout.open()) {
            let Some(record) = read_period(&directory) else {
                continue;
            };
            if record.generation != current {
                continue;
            }
            let committed = match truncate_to_receipts(&directory) {
                Ok(committed) => committed,
                Err(error) => {
                    tracing::warn!(%error, "Could not reopen a browser period");
                    continue;
                }
            };
            let receipts = read_receipts(&directory);
            for line in &receipts {
                self.receipts.insert(
                    (line.inst.clone(), line.batch_id.clone()),
                    Receipt {
                        period_id: record.period_id.clone(),
                        at_ms: line.at_ms,
                    },
                );
            }
            let created = Local
                .timestamp_millis_opt(record.created_at_ms as i64)
                .single()
                .unwrap_or(now);
            let period = Period {
                id: record.period_id.clone(),
                bucket: Bucket::of(&created),
                created_at_ms: record.created_at_ms,
                committed,
                receipts_len: receipts_length(&directory),
                contexts: snapshot_contexts(&directory.join(super::PAGES_FILENAME)),
                has_dir: true,
            };
            if period.bucket == bucket && self.period.is_none() {
                self.period = Some(period);
            } else {
                // The period ended no later than its last accepted batch. Reopening
                // can rewrite the pages file, so its modified time is only a fallback,
                // and the finish clamps the end to the period's window either way.
                let end = last_accepted_ms(&directory)
                    .or_else(|| modified_ms(&directory.join(super::PAGES_FILENAME)))
                    .and_then(|ms| Local.timestamp_millis_opt(ms as i64).single())
                    .unwrap_or(now);
                if let Err(error) = self.finalize(&period, end) {
                    tracing::warn!(%error, "Could not finish a browser period yet");
                    self.unfinished.push((period, end));
                }
            }
        }
    }

    fn load_finalized_receipts(&mut self) {
        let current = self.generation.clone();
        for segment in browser_segments(&self.layout.captures) {
            let Some(record) = read_period(&segment) else {
                continue;
            };
            if Some(&record.generation) != current.as_ref() {
                continue;
            }
            for line in read_receipts(&segment) {
                self.receipts.insert(
                    (line.inst, line.batch_id),
                    Receipt {
                        period_id: record.period_id.clone(),
                        at_ms: line.at_ms,
                    },
                );
            }
        }
    }

    fn prune_receipts(&mut self, now_ms: u64) {
        // Keep receipts of the open period, and finished ones for at least the
        // contract's retention, so a replay after a lost reply is still a duplicate.
        let open = self.period_id().map(str::to_owned);
        self.receipts.retain(|_, receipt| {
            Some(&receipt.period_id) == open.as_ref()
                || now_ms.saturating_sub(receipt.at_ms) < 3 * ACCEPTED_RETENTION_MS_MIN
        });
    }

    /// Recount what is held for the journal: open periods plus finished browser
    /// segments not yet released by the sync service.
    pub fn refresh_held(&mut self) {
        self.held_bytes = held_bytes(&self.layout);
    }
}

pub fn held_bytes(layout: &Layout) -> u64 {
    let open: u64 = sorted_dirs(&layout.open())
        .iter()
        .map(|directory| file_len(&directory.join(super::PAGES_FILENAME)))
        .sum();
    let finished: u64 = browser_segments(&layout.captures)
        .iter()
        .map(|segment| file_len(&segment.join(super::PAGES_FILENAME)))
        .sum();
    open + finished
}

/// The generation the sync service may deliver browser periods for: the recorded
/// generation, but only while the paired journal is still the one it belongs to.
pub fn deliverable_generation(layout: &Layout, journal: Option<&str>) -> Option<String> {
    let record: GenerationRecord =
        serde_json::from_slice(&fs::read(layout.generation()).ok()?).ok()?;
    (Some(record.journal.as_str()) == journal).then_some(record.generation)
}

/// The generation a finished browser period was accepted under.
pub fn segment_generation(segment_dir: &Path) -> Option<String> {
    read_period(segment_dir).map(|record| record.generation)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetiredSummary {
    pub periods: usize,
    pub bytes: u64,
}

pub fn retired_summary(layout: &Layout) -> RetiredSummary {
    let mut summary = RetiredSummary::default();
    for generation in sorted_dirs(&layout.retired()) {
        for day in sorted_dirs(&generation) {
            for period in sorted_dirs(&day) {
                summary.periods += 1;
                summary.bytes += file_len(&period.join(super::PAGES_FILENAME));
            }
        }
    }
    summary
}

/// Discard browser text kept for a journal this computer is no longer paired with.
/// Nothing held for the current journal is touched.
pub fn discard_retired(layout: &Layout) -> io::Result<RetiredSummary> {
    let summary = retired_summary(layout);
    for generation in sorted_dirs(&layout.retired()) {
        fs::remove_dir_all(&generation)?;
    }
    if layout.retired().exists() {
        sync_dir(&layout.retired())?;
    }
    Ok(summary)
}

fn generation_for(layout: &Layout, journal: &str) -> io::Result<String> {
    if let Ok(bytes) = fs::read(layout.generation())
        && let Ok(record) = serde_json::from_slice::<GenerationRecord>(&bytes)
        && record.journal == journal
    {
        return Ok(record.generation);
    }
    let generation = random_id()?;
    write_atomic(
        &layout.generation(),
        &serde_json::to_vec(&GenerationRecord {
            journal: journal.to_owned(),
            generation: generation.clone(),
        })
        .map_err(io::Error::other)?,
    )?;
    Ok(generation)
}

fn new_period(now: &DateTime<Local>) -> Period {
    Period {
        id: random_id().unwrap_or_else(|_| format!("{:032x}", now_ms(now))),
        bucket: Bucket::of(now),
        created_at_ms: now_ms(now),
        committed: 0,
        receipts_len: 0,
        contexts: HashSet::new(),
        has_dir: false,
    }
}

/// Move an open period directory to `<stream_root>/<day>/<HHMMSS_LEN>`, named by its
/// local start time like every capture segment.
fn move_period(
    directory: &Path,
    parent_for_day: impl Fn(&str) -> PathBuf,
    start_ms: u64,
    end_ms: u64,
) -> io::Result<()> {
    let start = Local
        .timestamp_millis_opt(start_ms as i64)
        .single()
        .ok_or_else(|| io::Error::other("period start is not a local time"))?;
    let day = format!("{:04}{:02}{:02}", start.year(), start.month(), start.day());
    let stem = format!(
        "{:02}{:02}{:02}",
        start.hour(),
        start.minute(),
        start.second()
    );
    let ceiling = PERIOD_MS / 1000;
    let length = (end_ms.saturating_sub(start_ms) / 1000).clamp(1, ceiling);
    let parent = parent_for_day(&day);
    // A taken name is resolved by ending the period a second earlier, never later,
    // so the length stays within the period.
    for length in (1..=length).rev().take(60) {
        let target = parent.join(format!("{stem}_{length}"));
        if target.exists() {
            continue;
        }
        // The sync service removes empty stream folders, so the folder can vanish
        // between creating it and the rename; create it again and retry.
        let mut attempts = 0;
        loop {
            fs::create_dir_all(&parent)?;
            match fs::rename(directory, &target) {
                Err(error) if error.kind() == io::ErrorKind::NotFound && attempts < 3 => {
                    attempts += 1;
                }
                result => {
                    result?;
                    break;
                }
            }
        }
        sync_dir(&parent)?;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free segment name for a browser period",
    ))
}

/// Cut the payload back to what the last receipt covers: bytes past it were never
/// acknowledged, so the extension still has them.
fn truncate_to_receipts(directory: &Path) -> io::Result<u64> {
    let committed = read_receipts(directory).last().map_or(0, |line| line.end);
    let pages = directory.join(super::PAGES_FILENAME);
    match OpenOptions::new().write(true).open(&pages) {
        Ok(file) => {
            if file.metadata()?.len() > committed {
                file.set_len(committed)?;
                file.sync_data()?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    }
    Ok(committed)
}

/// The receipts that cover the payload: complete lines only, each ending past the one
/// before it and within the payload actually on disk. Anything after the first line
/// that fails those checks never committed.
fn read_receipts(directory: &Path) -> Vec<ReceiptLine> {
    let Ok(bytes) = fs::read(directory.join(RECEIPTS_FILE)) else {
        return Vec::new();
    };
    let payload = file_len(&directory.join(super::PAGES_FILENAME));
    let mut previous = 0;
    let mut out = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        let Ok(receipt) = serde_json::from_slice::<ReceiptLine>(line) else {
            break;
        };
        if receipt.end <= previous || receipt.end > payload {
            break;
        }
        previous = receipt.end;
        out.push(receipt);
    }
    out
}

/// The byte length of the receipts `read_receipts` accepts.
fn receipts_length(directory: &Path) -> u64 {
    read_receipts(directory)
        .iter()
        .map(|receipt| serde_json::to_vec(receipt).map_or(0, |line| line.len() as u64 + 1))
        .sum()
}

fn snapshot_contexts(pages: &Path) -> HashSet<(String, String)> {
    let Ok(file) = File::open(pages) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|record| record.get("t").and_then(Value::as_str) == Some("segment_start"))
        .filter_map(|record| {
            Some((
                record.get("inst")?.as_str()?.to_owned(),
                record.get("ctx")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn read_period(directory: &Path) -> Option<PeriodRecord> {
    serde_json::from_slice(&fs::read(directory.join(PERIOD_FILE)).ok()?).ok()
}

fn browser_segments(captures: &Path) -> Vec<PathBuf> {
    sorted_dirs(captures)
        .into_iter()
        .flat_map(|day| sorted_dirs(&day.join(super::STREAM_DIR)))
        .filter(|segment| {
            let name = segment.file_name().unwrap_or_default().to_string_lossy();
            !name.ends_with(".incomplete") && !name.ends_with(".failed")
        })
        .collect()
}

fn sorted_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .collect();
    paths.sort();
    paths
}

fn safe_label(label: &str) -> String {
    let label: String = label
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(64)
        .collect();
    if label.is_empty() {
        "unknown".to_owned()
    } else {
        label
    }
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |meta| meta.len())
}

fn last_accepted_ms(directory: &Path) -> Option<u64> {
    read_receipts(directory).last().map(|line| line.at_ms)
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

fn now_ms(now: &DateTime<Local>) -> u64 {
    u64::try_from(now.timestamp_millis()).unwrap_or(0)
}

fn reject(reason: &'static str) -> Outcome {
    Outcome::Rejected { reason }
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
    {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Write `bytes` at `offset`, drop anything after them, and make it durable.
fn write_at(path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all_at(bytes, offset)?;
    file.set_len(offset + bytes.len() as u64)?;
    file.sync_data()
}

fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    sync_dir(directory)
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn remove_dir_all_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 2, hour, minute, second)
            .earliest()
            .unwrap()
    }

    fn layout(temp: &tempfile::TempDir) -> Layout {
        Layout::new(temp.path())
    }

    fn batch(custody: &Custody, id: u8, records: Value, now: DateTime<Local>) -> Value {
        json!({
            "type": "batch",
            "destination_generation": custody.generation().unwrap(),
            "inst": "inst-a",
            "batch_id": format!("{id:032x}"),
            "queued_at_ms": now_ms(&now) - 1000,
            "records": records,
        })
    }

    fn snapshot(ctx: &str, text: &str) -> Value {
        json!([{"t": "segment_start", "ts": 1, "ctx": ctx, "inst": "inst-a", "site": "example.org",
                "blocks": [{"id": "b1", "text": text}]}])
    }

    fn delta(ctx: &str, text: &str) -> Value {
        json!([{"t": "delta", "ts": 2, "ctx": ctx, "inst": "inst-a", "op": "add",
                "block": {"id": "b2", "text": text}}])
    }

    fn accepted(outcome: (Outcome, Option<String>)) -> String {
        match outcome.0 {
            Outcome::Accepted { period_id } => period_id,
            other => panic!("expected accepted, got {other:?}"),
        }
    }

    fn segment_files(captures: &Path) -> Vec<PathBuf> {
        browser_segments(captures)
            .into_iter()
            .map(|segment| segment.join(crate::browser::PAGES_FILENAME))
            .collect()
    }

    #[test]
    fn journal_identity_is_the_instance_and_the_normalised_ca_chain() {
        let pem = "-----BEGIN CERTIFICATE-----\nAAAA\nBBBB\n-----END CERTIFICATE-----\n".to_owned();
        let reflowed =
            "-----BEGIN CERTIFICATE-----\r\nAAAABBBB\r\n-----END CERTIFICATE-----".to_owned();
        assert_eq!(
            journal_identity("i1", std::slice::from_ref(&pem)),
            journal_identity("i1", &[reflowed])
        );
        assert_ne!(
            journal_identity("i1", std::slice::from_ref(&pem)),
            journal_identity("i2", std::slice::from_ref(&pem))
        );
        assert_ne!(
            journal_identity("i1", std::slice::from_ref(&pem)),
            journal_identity(
                "i1",
                &["-----BEGIN CERTIFICATE-----\nCCCC\n-----END CERTIFICATE-----".to_owned()]
            )
        );
    }

    #[test]
    fn unpaired_custody_reports_not_paired_and_accepts_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let mut custody = Custody::open(layout(&temp), None, at(10, 1, 0)).unwrap();
        let facts = custody.facts(false);
        assert_eq!((facts.capture, facts.delivery), ("not_paired", "unknown"));
        assert_eq!(facts.generation, None);
        let batch =
            json!({"destination_generation": "g", "inst": "i", "batch_id": "0", "records": []});
        assert_eq!(
            custody.accept(&batch, at(10, 1, 0)).0,
            reject("stale_generation")
        );
        assert_eq!(custody.tick(at(10, 6, 0)), None);
    }

    #[test]
    fn a_batch_is_kept_once_and_a_replay_is_a_duplicate() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let first = batch(&custody, 1, snapshot("c1", "hello"), now);
        let period = accepted(custody.accept(&first, now));
        assert_eq!(
            custody.accept(&first, now).0,
            Outcome::Duplicate {
                period_id: period.clone()
            }
        );
        let facts = custody.facts(false);
        assert_eq!(
            (facts.capture, facts.delivery),
            ("permitted", "kept_locally")
        );
        // A reopen (an app restart) still knows the receipt.
        drop(custody);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        assert_eq!(custody.period_id(), Some(period.as_str()));
        assert_eq!(
            custody.accept(&first, now).0,
            Outcome::Duplicate { period_id: period }
        );
    }

    #[test]
    fn a_delta_needs_a_snapshot_of_its_context_in_the_same_period() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let orphan = batch(&custody, 1, delta("c1", "x"), now);
        assert_eq!(custody.accept(&orphan, now).0, reject("snapshot_required"));
        // Nothing was recorded, so the same batch id can come back as a snapshot.
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "x"), now), now));
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "y"), now), now));
        // A new period needs a new snapshot.
        let later = at(10, 6, 0);
        assert!(custody.tick(later).is_some());
        assert_eq!(
            custody
                .accept(&batch(&custody, 3, delta("c1", "z"), later), later)
                .0,
            reject("snapshot_required")
        );
    }

    #[test]
    fn a_finished_period_becomes_one_browser_segment_named_by_its_start() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 2, 30);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let generation = custody.generation().unwrap().to_owned();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "alpha"), now), now));
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "beta"), now), now));
        assert!(custody.tick(at(10, 4, 59)).is_none());
        let next = custody.tick(at(10, 5, 0)).unwrap();
        assert_eq!(custody.period_id(), Some(next.as_str()));
        let segment = temp.path().join("captures/20261002/_browser/100230_150");
        let pages = fs::read_to_string(segment.join(crate::browser::PAGES_FILENAME)).unwrap();
        assert_eq!(pages.lines().count(), 2);
        assert!(pages.contains("alpha") && pages.contains("beta"));
        assert_eq!(
            segment_generation(&segment).as_deref(),
            Some(generation.as_str())
        );
        // An empty period leaves nothing behind.
        assert!(custody.tick(at(10, 10, 0)).is_some());
        assert_eq!(segment_files(&temp.path().join("captures")).len(), 1);
        assert!(sorted_dirs(&temp.path().join("browser/open")).is_empty());
    }

    #[test]
    fn a_different_journal_retires_everything_held_for_the_old_one() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let old_generation = custody.generation().unwrap().to_owned();
        accepted(custody.accept(
            &batch(&custody, 1, snapshot("c1", "for-a-finished"), now),
            now,
        ));
        custody.tick(at(10, 5, 0));
        let later = at(10, 6, 0);
        accepted(custody.accept(
            &batch(&custody, 2, snapshot("c1", "for-a-open"), later),
            later,
        ));
        drop(custody);

        let custody = Custody::open(layout(&temp), Some("journal-b"), at(10, 7, 0)).unwrap();
        assert_ne!(custody.generation(), Some(old_generation.as_str()));
        assert!(segment_files(&temp.path().join("captures")).is_empty());
        assert_eq!(custody.facts(false).held_bytes, 0);
        let summary = retired_summary(&custody.layout);
        assert_eq!(summary.periods, 2);
        assert!(summary.bytes > 0);
        assert_eq!(
            deliverable_generation(&custody.layout, Some("journal-a")),
            None
        );
        assert_eq!(
            deliverable_generation(&custody.layout, Some("journal-b")).as_deref(),
            custody.generation()
        );
        assert_eq!(discard_retired(&custody.layout).unwrap(), summary);
        assert_eq!(retired_summary(&custody.layout), RetiredSummary::default());
    }

    #[test]
    fn the_same_journal_keeps_its_generation() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let generation = custody.generation().unwrap().to_owned();
        drop(custody);
        let custody = Custody::open(layout(&temp), Some("journal-a"), at(11, 0, 0)).unwrap();
        assert_eq!(custody.generation(), Some(generation.as_str()));
        // Unpairing does not retire anything: an unknown journal is not a different one.
        drop(custody);
        let custody = Custody::open(layout(&temp), None, at(11, 0, 0)).unwrap();
        assert_eq!(custody.generation(), None);
        let custody =
            Custody::open(custody.layout.clone(), Some("journal-a"), at(11, 0, 0)).unwrap();
        assert_eq!(custody.generation(), Some(generation.as_str()));
    }

    #[test]
    fn a_stale_generation_and_aged_batches_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let mut stale = batch(&custody, 1, snapshot("c1", "x"), now);
        stale["destination_generation"] = json!("someone-else");
        assert_eq!(custody.accept(&stale, now).0, reject("stale_generation"));
        let mut future = batch(&custody, 2, snapshot("c1", "x"), now);
        future["queued_at_ms"] = json!(now_ms(&now) + FUTURE_SKEW_MS_MAX + 1);
        assert_eq!(custody.accept(&future, now).0, reject("age_policy"));
        let mut old = batch(&custody, 3, snapshot("c1", "x"), now);
        old["queued_at_ms"] = json!(now_ms(&now) - OUTBOX_AGE_MS_MAX);
        assert_eq!(custody.accept(&old, now).0, reject("expired_unaccepted"));
    }

    #[test]
    fn a_full_queue_refuses_new_intake_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "first"), now), now));
        let held = custody.facts(false).held_bytes;
        custody = custody.with_bound(held + 50);
        assert_eq!(
            custody
                .accept(
                    &batch(&custody, 2, snapshot("c2", &"y".repeat(200)), now),
                    now
                )
                .0,
            reject("queue_full")
        );
        assert_eq!(custody.facts(false).capture, "permitted");
        custody = custody.with_bound(held);
        let facts = custody.facts(false);
        assert!(facts.full);
        assert_eq!(
            (facts.capture, facts.delivery, facts.failure),
            ("intake_off", "kept_locally", Some("queue_full"))
        );
        assert_eq!(custody.facts(true).capture, "paused");
    }

    #[test]
    fn bytes_no_receipt_covers_are_dropped_on_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let period =
            accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "kept"), now), now));
        drop(custody);
        let pages = temp
            .path()
            .join("browser/open")
            .join(&period)
            .join(crate::browser::PAGES_FILENAME);
        let committed = fs::read(&pages).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&pages)
            .unwrap()
            .write_all(b"{\"t\":\"segme")
            .unwrap();
        let custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        assert_eq!(fs::read(&pages).unwrap(), committed);
        assert_eq!(custody.facts(false).held_bytes, committed.len() as u64);
    }

    #[test]
    fn bytes_left_by_a_failed_write_are_overwritten_and_never_covered() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let period =
            accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "first"), now), now));
        let directory = temp.path().join("browser/open").join(&period);
        // What an interrupted write leaves: a partial record and a partial receipt.
        for (name, junk) in [
            (
                crate::browser::PAGES_FILENAME,
                &b"{\"t\":\"delta\",\"ts\""[..],
            ),
            (RECEIPTS_FILE, &b"{\"inst\":\"inst-a\",\"batch_"[..]),
        ] {
            OpenOptions::new()
                .append(true)
                .open(directory.join(name))
                .unwrap()
                .write_all(junk)
                .unwrap();
        }
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "second"), now), now));
        drop(custody);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let pages = fs::read_to_string(directory.join(crate::browser::PAGES_FILENAME)).unwrap();
        assert_eq!(pages.lines().count(), 2);
        assert!(
            pages
                .lines()
                .all(|line| serde_json::from_str::<Value>(line).is_ok())
        );
        for id in [1, 2] {
            assert!(matches!(
                custody
                    .accept(&batch(&custody, id, snapshot("c1", "replay"), now), now)
                    .0,
                Outcome::Duplicate { .. }
            ));
        }
    }

    #[test]
    fn a_receipt_for_bytes_that_are_not_there_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        let period =
            accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "first"), now), now));
        drop(custody);
        let directory = temp.path().join("browser/open").join(&period);
        let mut line = serde_json::to_vec(&ReceiptLine {
            inst: "inst-a".into(),
            batch_id: format!("{:032x}", 2),
            end: 1_000_000,
            at_ms: now_ms(&now),
        })
        .unwrap();
        line.push(b'\n');
        OpenOptions::new()
            .append(true)
            .open(directory.join(RECEIPTS_FILE))
            .unwrap()
            .write_all(&line)
            .unwrap();
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        // The text of batch 2 never reached disk, so it is not a duplicate.
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "second"), now), now));
    }

    fn segment_names(captures: &Path) -> Vec<String> {
        browser_segments(captures)
            .iter()
            .map(|segment| segment.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn assert_lengths_within_ceiling(names: &[String]) {
        for name in names {
            let (_, length) = name.split_once('_').unwrap();
            let length: u64 = length.parse().unwrap();
            assert!((1..=PERIOD_MS / 1000).contains(&length), "{name}");
        }
    }

    #[test]
    fn a_period_finished_late_after_a_suspend_stays_within_its_window() {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let opened = at(10, 2, 30);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), opened).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "alpha"), opened), opened));
        // The computer sleeps through the end of the window; the next tick is hours later.
        assert!(custody.tick(at(18, 0, 0)).is_some());
        assert_eq!(segment_names(&captures), ["100230_150"]);

        let opened = at(18, 0, 0);
        accepted(custody.accept(&batch(&custody, 2, snapshot("c2", "beta"), opened), opened));
        assert!(custody.tick(at(23, 59, 0)).is_some());
        let names = segment_names(&captures);
        assert_eq!(names, ["100230_150", "180000_300"]);
        assert_lengths_within_ceiling(&names);
    }

    #[test]
    fn a_taken_segment_name_never_lengthens_a_period() {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let opened = at(10, 0, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), opened).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "alpha"), opened), opened));
        fs::create_dir_all(captures.join("20261002/_browser/100000_300")).unwrap();
        assert!(custody.tick(at(18, 0, 0)).is_some());
        let names = segment_names(&captures);
        assert_eq!(names, ["100000_299", "100000_300"]);
        assert_lengths_within_ceiling(&names);
    }

    #[test]
    fn a_period_recovered_after_a_suspend_ends_at_its_last_batch() {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let opened = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), opened).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "alpha"), opened), opened));
        let last = at(10, 3, 20);
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "beta"), last), last));
        drop(custody);
        // The pages file's time says nothing about when the period ended: here it was
        // touched long after the window closed.
        let open = sorted_dirs(&temp.path().join("browser/open"));
        let late = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now_ms(&at(18, 0, 0)));
        File::options()
            .write(true)
            .open(open[0].join(crate::browser::PAGES_FILENAME))
            .unwrap()
            .set_modified(late)
            .unwrap();
        Custody::open(layout(&temp), Some("journal-a"), at(18, 0, 0)).unwrap();
        let names = segment_names(&captures);
        assert_eq!(names, ["100100_140"]);
        assert_lengths_within_ceiling(&names);
    }

    #[test]
    fn a_period_without_a_known_end_is_bounded_by_its_window() {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let opened = at(10, 1, 0);
        let custody = Custody::open(layout(&temp), Some("journal-a"), opened).unwrap();
        let mut period = new_period(&opened);
        period.has_dir = true;
        let directory = custody.layout.open().join(&period.id);
        create_private_dir(&directory).unwrap();
        let payload = "{}\n";
        fs::write(directory.join(crate::browser::PAGES_FILENAME), payload).unwrap();
        let receipt = serde_json::to_string(&ReceiptLine {
            inst: "inst-a".into(),
            batch_id: "b".into(),
            end: payload.len() as u64,
            at_ms: now_ms(&opened),
        })
        .unwrap();
        fs::write(directory.join(RECEIPTS_FILE), receipt + "\n").unwrap();
        // Recovery falls back to the time it runs when nothing else is known.
        custody.finalize(&period, at(18, 0, 0)).unwrap();
        let names = segment_names(&captures);
        assert_eq!(names, ["100100_240"]);
        assert_lengths_within_ceiling(&names);
    }

    #[test]
    fn an_old_open_period_is_finished_on_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), Some("journal-a"), now).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "kept"), now), now));
        drop(custody);
        let custody = Custody::open(layout(&temp), Some("journal-a"), at(12, 0, 0)).unwrap();
        assert_eq!(segment_files(&temp.path().join("captures")).len(), 1);
        assert!(custody.period_id().is_some());
    }
}
