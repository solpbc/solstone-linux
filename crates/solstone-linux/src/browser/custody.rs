// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What the app holds for the journal from the browser, on this computer.
//!
//! - A batch is accepted only after its records and its receipt are on disk, so a
//!   replayed batch id is answered `duplicate` and nothing is taken in twice.
//! - Text is kept in five-minute periods, one `browser_pages.jsonl` each. A finished
//!   period becomes a capture segment under the `_browser` stream folder, and the sync
//!   service delivers it as the `browser` source.
//! - Accepted pages stay here across pairing changes. The extension re-stamps pending
//!   batches when a confirmed destination changes; generation is not an admission key.
//! - One bound covers everything held: past it, new batches are refused as
//!   `queue_full` and the extension is told custody is full.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use native_browser_frame::{
    ACCEPTED_RETENTION_MS_MIN, FILE_MAX, FUTURE_SKEW_MS_MAX, SPOOL_BYTES_MAX, canonical_stringify,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Sidecar naming the period. Dot files are never uploaded.
pub const PERIOD_FILE: &str = ".browser_period.json";
/// The receipts that cover the period's payload, for duplicate answers.
pub const RECEIPTS_FILE: &str = ".browser_receipts.jsonl";
pub const EVIDENCE_FILE: &str = "batch-evidence.jsonl";
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
    pub fn evidence(&self) -> PathBuf {
        self.root.join(EVIDENCE_FILE)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PeriodRecord {
    period_id: String,
    created_at_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct ReceiptLine {
    inst: String,
    batch_id: String,
    end: u64,
    at_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceKind {
    Accepted,
    Removed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvidenceLine {
    inst: String,
    batch_id: String,
    kind: EvidenceKind,
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
    period: Option<Period>,
    receipts: HashMap<(String, String), Receipt>,
    evidence: HashMap<(String, String), EvidenceKind>,
    evidence_lines: Vec<EvidenceLine>,
    reserved_uploads: HashSet<PathBuf>,
    /// Finished periods that could not yet be turned into segments; retried each tick.
    unfinished: Vec<(Period, DateTime<Local>)>,
    held_bytes: u64,
    bound: u64,
    #[cfg(test)]
    pub(crate) evidence_fault: Option<crate::private_file::DurableWriteStage>,
    #[cfg(test)]
    discard_remove_fault: Option<PathBuf>,
}

impl Custody {
    /// Recover all pending browser pages without binding them to a journal.
    pub fn open(layout: Layout, now: DateTime<Local>) -> io::Result<Self> {
        create_private_dir(&layout.root)?;
        create_private_dir(&layout.open())?;
        // Legacy state is deliberately neither parsed nor imported. Failure to remove
        // it cannot prevent recovery of the current pending store.
        if let Err(error) = remove_dir_all_if_present(&layout.root.join("retired")) {
            tracing::warn!(%error, "Could not remove legacy retired browser pages");
        }
        if let Err(error) = fs::remove_file(layout.root.join("generation.json"))
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "Could not remove legacy browser generation file");
        }
        let mut custody = Self {
            layout,
            period: None,
            receipts: HashMap::new(),
            evidence: HashMap::new(),
            evidence_lines: Vec::new(),
            reserved_uploads: HashSet::new(),
            unfinished: Vec::new(),
            held_bytes: 0,
            bound: SPOOL_BYTES_MAX as u64,
            #[cfg(test)]
            evidence_fault: None,
            #[cfg(test)]
            discard_remove_fault: None,
        };
        custody.load_evidence()?;
        custody.finish_tombstoned_unlinks()?;
        custody.recover_open(now);
        custody.load_finalized_receipts();
        custody.reconcile_evidence()?;
        if custody.period.is_none() {
            custody.period = Some(new_period(&now));
        }
        custody.refresh_held();
        Ok(custody)
    }

    #[cfg(test)]
    fn with_bound(mut self, bound: u64) -> Self {
        self.bound = bound;
        self
    }

    pub fn period_id(&self) -> Option<&str> {
        self.period.as_ref().map(|period| period.id.as_str())
    }

    pub fn facts(&self, generation: Option<&str>, paused: bool) -> Facts {
        let held = self.held_bytes > 0;
        let full = self.held_bytes >= self.bound;
        let Some(generation) = generation else {
            return Facts {
                capture: "not_paired",
                delivery: if held { "kept_locally" } else { "unknown" },
                failure: full.then_some("queue_full"),
                generation: None,
                period_id: None,
                full,
                held_bytes: self.held_bytes,
            };
        };
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
            generation: Some(generation.to_owned()),
            period_id: self.period_id().map(str::to_owned),
            full,
            held_bytes: self.held_bytes,
        }
    }

    /// Start a new period when the five-minute local-clock bucket changes. Returns the
    /// new period id, which every connected browser must hear as a `boundary`.
    pub fn tick(&mut self, now: DateTime<Local>) -> Option<String> {
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
        let (inst, batch_id) = (field("inst"), field("batch_id"));
        let key = (inst.to_owned(), batch_id.to_owned());
        if self.evidence.contains_key(&key) || self.receipts.contains_key(&key) {
            return (
                Outcome::Duplicate {
                    period_id: self
                        .receipts
                        .get(&key)
                        .map(|receipt| receipt.period_id.clone())
                        .or_else(|| self.period_for_identity(&key))
                        .or_else(|| self.period_id().map(str::to_owned))
                        .unwrap_or_default(),
                },
                None,
            );
        }
        if self
            .period
            .as_ref()
            .is_some_and(|period| self.has_removed_identity(&self.layout.open().join(&period.id)))
        {
            return (reject("queue_full"), None);
        }
        let now_ms = now_ms(&now);
        let queued_at = batch
            .get("queued_at_ms")
            .and_then(native_browser_frame::codec::nonnegative_integer)
            .unwrap_or(0);
        if queued_at > now_ms.saturating_add(FUTURE_SKEW_MS_MAX) {
            return (reject("age_policy"), None);
        }
        let Some(records) = batch.get("records").and_then(Value::as_array) else {
            return (reject("malformed"), None);
        };
        if records.is_empty() {
            return (reject("malformed"), None);
        }
        let mut payload = String::new();
        for record in records {
            if canonical_stringify(record, &mut payload).is_err() {
                return (reject("malformed"), None);
            }
            payload.push('\n');
        }
        let bytes = payload.len() as u64;
        if bytes > FILE_MAX as u64 {
            return (reject("oversize"), None);
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
        if self.period.is_none() {
            self.period = Some(new_period(&now));
        }
        let period = self.period.as_ref().expect("period initialized");
        if !snapshot && !period.contexts.contains(&context) {
            return (reject("snapshot_required"), rotated);
        }
        let evidence_line = EvidenceLine {
            inst: inst.to_owned(),
            batch_id: batch_id.to_owned(),
            kind: EvidenceKind::Accepted,
        };
        let evidence_growth =
            serde_json::to_vec(&evidence_line).map_or(0, |line| line.len() as u64 + 1);
        let receipt_growth = self
            .period
            .as_ref()
            .and_then(|period| {
                serde_json::to_vec(&ReceiptLine {
                    inst: inst.to_owned(),
                    batch_id: batch_id.to_owned(),
                    end: period.committed + bytes,
                    at_ms: now_ms,
                })
                .ok()
            })
            .map_or(0, |line| line.len() as u64 + 1);
        let period_growth = self.period.as_ref().map_or(0, |period| {
            if period.has_dir {
                0
            } else {
                serde_json::to_vec(&PeriodRecord {
                    period_id: period.id.clone(),
                    created_at_ms: period.created_at_ms,
                })
                .map_or(0, |line| line.len() as u64)
            }
        });
        if self.held_bytes + bytes + evidence_growth + receipt_growth + period_growth > self.bound {
            return (reject("queue_full"), rotated);
        }
        match self.commit(&payload, inst, batch_id, now_ms) {
            Ok(period_id) => {
                if let Err(error) = self.write_evidence(std::slice::from_ref(&evidence_line), false)
                {
                    tracing::warn!(%error, "Could not record a browser batch identity");
                    self.evidence.insert(key.clone(), EvidenceKind::Accepted);
                    self.evidence_lines.push(evidence_line.clone());
                }
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
                self.refresh_held();
                (Outcome::Accepted { period_id }, rotated)
            }
            Err(error) => {
                tracing::warn!(%error, "Could not keep a browser batch");
                (reject("queue_full"), rotated)
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
        if self.has_removed_identity(&directory) {
            return Err(io::Error::other("browser removal is awaiting unlink"));
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

    fn recover_open(&mut self, now: DateTime<Local>) {
        let bucket = Bucket::of(&now);
        for directory in sorted_dirs(&self.layout.open()) {
            let Some(record) = read_period(&directory) else {
                continue;
            };
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
        for segment in browser_segments(&self.layout.captures) {
            let Some(record) = read_period(&segment) else {
                continue;
            };
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
        self.receipts.retain(|key, receipt| {
            self.evidence.get(key) == Some(&EvidenceKind::Removed)
                || Some(&receipt.period_id) == open.as_ref()
                || now_ms.saturating_sub(receipt.at_ms) < 3 * ACCEPTED_RETENTION_MS_MIN
        });
    }

    /// Recount what is held for the journal: open periods plus finished browser
    /// segments not yet released by the sync service.
    pub fn refresh_held(&mut self) {
        self.held_bytes = held_bytes(&self.layout);
    }

    fn period_for_identity(&self, key: &(String, String)) -> Option<String> {
        sorted_dirs(&self.layout.open())
            .into_iter()
            .chain(browser_segments(&self.layout.captures))
            .find_map(|directory| {
                read_receipts(&directory)
                    .iter()
                    .any(|receipt| (&receipt.inst, &receipt.batch_id) == (&key.0, &key.1))
                    .then(|| read_period(&directory).map(|record| record.period_id))
                    .flatten()
            })
    }

    fn load_evidence(&mut self) -> io::Result<()> {
        let path = self.layout.evidence();
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if line.last() != Some(&b'\n') {
                break;
            }
            let record: EvidenceLine = serde_json::from_slice(&line[..line.len() - 1])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let key = (record.inst.clone(), record.batch_id.clone());
            self.evidence.insert(key, record.kind);
            self.evidence_lines.push(record);
        }
        Ok(())
    }

    fn write_evidence(&mut self, lines: &[EvidenceLine], removal: bool) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut next = self.evidence_lines.clone();
        next.extend_from_slice(lines);
        let mut bytes = Vec::new();
        for line in &next {
            serde_json::to_writer(&mut bytes, line).map_err(io::Error::other)?;
            bytes.push(b'\n');
        }
        let result = if removal {
            #[cfg(test)]
            {
                let write = if let Some(stage) = self.evidence_fault {
                    crate::private_file::atomic_write_bytes_with_fault(
                        &self.layout.evidence(),
                        &bytes,
                        &EvidenceWriteFault(stage),
                    )
                } else {
                    crate::private_file::atomic_write_bytes_with_fault(
                        &self.layout.evidence(),
                        &bytes,
                        &crate::private_file::NoWriteFault,
                    )
                };
                write.map_err(io::Error::other)
            }
            #[cfg(not(test))]
            {
                crate::private_file::atomic_write_bytes_with_fault(
                    &self.layout.evidence(),
                    &bytes,
                    &crate::private_file::NoWriteFault,
                )
                .map_err(io::Error::other)
            }
        } else {
            crate::private_file::atomic_write_bytes(&self.layout.evidence(), &bytes)
                .map_err(io::Error::other)
        };
        if result.is_ok()
            || fs::read(self.layout.evidence()).is_ok_and(|observed| observed == bytes)
        {
            for line in lines {
                self.evidence
                    .insert((line.inst.clone(), line.batch_id.clone()), line.kind);
            }
            self.evidence_lines = next;
        }
        result
    }

    fn identities_in(&self, directory: &Path) -> Vec<EvidenceLine> {
        read_receipts(directory)
            .into_iter()
            .map(|receipt| EvidenceLine {
                inst: receipt.inst,
                batch_id: receipt.batch_id,
                kind: EvidenceKind::Removed,
            })
            .filter(|line| {
                self.evidence
                    .get(&(line.inst.clone(), line.batch_id.clone()))
                    != Some(&EvidenceKind::Removed)
            })
            .collect()
    }

    fn tombstone_directory(&mut self, directory: &Path) -> io::Result<()> {
        let identities = self.identities_in(directory);
        self.write_evidence(&identities, true)
    }

    fn remove_discard_directory(&self, directory: &Path) -> io::Result<()> {
        #[cfg(test)]
        if self.discard_remove_fault.as_deref() == Some(directory) {
            return Err(io::Error::other("injected browser directory removal fault"));
        }
        remove_tombstoned_directory(directory)
    }

    /// Reserve a finalized browser segment while sync reads and uploads it.
    pub fn reserve_upload(&mut self, path: &Path) -> bool {
        if self.has_removed_identity(path) {
            return false;
        }
        self.reserved_uploads.insert(path.to_path_buf())
    }

    fn has_removed_identity(&self, directory: &Path) -> bool {
        read_receipt_identities(directory).is_some_and(|identities| {
            identities
                .iter()
                .any(|identity| self.evidence.get(identity) == Some(&EvidenceKind::Removed))
        })
    }

    pub fn release_upload(&mut self, path: &Path) {
        self.reserved_uploads.remove(path);
    }

    pub fn remove_confirmed_segment(&mut self, path: &Path) -> io::Result<()> {
        self.tombstone_directory(path)
    }

    pub fn discard_pending(&mut self) -> io::Result<()> {
        for directory in sorted_dirs(&self.layout.open())
            .into_iter()
            .chain(browser_segments(&self.layout.captures))
        {
            if self.reserved_uploads.contains(&directory) {
                continue;
            }
            if let Err(error) = self.tombstone_directory(&directory) {
                self.refresh_held();
                return Err(error);
            }
            if let Err(error) = self.remove_discard_directory(&directory) {
                self.refresh_held();
                return Err(error);
            }
            if self
                .period
                .as_ref()
                .is_some_and(|period| self.layout.open().join(&period.id) == directory)
            {
                self.period = None;
            }
            self.unfinished
                .retain(|(period, _)| self.layout.open().join(&period.id) != directory);
        }
        self.period = Some(new_period(&Local::now()));
        self.refresh_held();
        Ok(())
    }

    fn finish_tombstoned_unlinks(&self) -> io::Result<()> {
        for directory in sorted_dirs(&self.layout.open())
            .into_iter()
            .chain(browser_segments(&self.layout.captures))
        {
            let identities = read_receipt_identities(&directory);
            let tombstoned = identities.as_ref().is_some_and(|identities| {
                !identities.is_empty()
                    && identities
                        .iter()
                        .all(|identity| self.evidence.get(identity) == Some(&EvidenceKind::Removed))
            });
            let empty = fs::read_dir(&directory).is_ok_and(|mut entries| entries.next().is_none());
            let sidecar_shell = !directory.join(super::PAGES_FILENAME).exists()
                && !directory.join(RECEIPTS_FILE).exists();
            if tombstoned || empty || sidecar_shell {
                remove_tombstoned_directory(&directory)?;
            }
        }
        Ok(())
    }

    fn reconcile_evidence(&mut self) -> io::Result<()> {
        let mut found = HashSet::new();
        for directory in sorted_dirs(&self.layout.open())
            .into_iter()
            .chain(browser_segments(&self.layout.captures))
        {
            for receipt in read_receipts(&directory) {
                let key = (receipt.inst.clone(), receipt.batch_id.clone());
                found.insert(key.clone());
                if !self.evidence.contains_key(&key) {
                    self.write_evidence(
                        &[EvidenceLine {
                            inst: receipt.inst,
                            batch_id: receipt.batch_id,
                            kind: EvidenceKind::Accepted,
                        }],
                        false,
                    )?;
                }
            }
        }
        if self
            .evidence
            .iter()
            .any(|(key, kind)| *kind == EvidenceKind::Accepted && !found.contains(key))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "accepted browser identity has no pending payload or removal tombstone",
            ));
        }
        Ok(())
    }
}

pub fn held_bytes(layout: &Layout) -> u64 {
    let dirs = sorted_dirs(&layout.open())
        .into_iter()
        .chain(browser_segments(&layout.captures));
    let segments = dirs.fold(0_u64, |sum, directory| {
        sum + file_len(&directory.join(super::PAGES_FILENAME))
            + file_len(&directory.join(PERIOD_FILE))
            + file_len(&directory.join(RECEIPTS_FILE))
    });
    segments + file_len(&layout.evidence())
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

/// The complete receipt identities, without checking their byte coverage. This is
/// used to guard and finish a removal whose payload may already have been unlinked.
fn read_receipt_identities(directory: &Path) -> Option<Vec<(String, String)>> {
    let bytes = fs::read(directory.join(RECEIPTS_FILE)).ok()?;
    let mut identities = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        let receipt = serde_json::from_slice::<ReceiptLine>(line).ok()?;
        identities.push((receipt.inst, receipt.batch_id));
    }
    Some(identities)
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

#[cfg(test)]
struct EvidenceWriteFault(crate::private_file::DurableWriteStage);

#[cfg(test)]
impl crate::private_file::DurableWriteFault for EvidenceWriteFault {
    fn before(&self, stage: crate::private_file::DurableWriteStage) -> io::Result<()> {
        if stage == self.0 {
            Err(io::Error::other("injected evidence write fault"))
        } else {
            Ok(())
        }
    }
}

fn remove_dir_all_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_tombstoned_directory(directory: &Path) -> io::Result<()> {
    let receipts = directory.join(RECEIPTS_FILE);
    match fs::remove_file(directory.join(super::PAGES_FILENAME)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path == receipts {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
    }
    match fs::remove_file(receipts) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    remove_dir_all_if_present(directory)
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

    fn batch(_custody: &Custody, id: u8, records: Value, now: DateTime<Local>) -> Value {
        json!({
            "type": "batch",
            "destination_generation": "g1",
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
    fn unpaired_custody_reports_not_paired_and_accepts_valid_batches() {
        let temp = tempfile::tempdir().unwrap();
        let mut custody = Custody::open(layout(&temp), at(10, 1, 0)).unwrap();
        let now = at(10, 1, 0);
        let facts = custody.facts(None, false);
        assert_eq!((facts.capture, facts.delivery), ("not_paired", "unknown"));
        assert_eq!(facts.generation, None);
        let batch = batch(&custody, 1, snapshot("c1", "kept"), now);
        assert_eq!(
            custody.accept(&batch, now).0,
            Outcome::Accepted {
                period_id: custody.period_id().unwrap().to_owned()
            }
        );
        assert!(custody.tick(at(10, 6, 0)).is_some());
    }

    #[test]
    fn a_batch_is_kept_once_and_a_replay_is_a_duplicate() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let first = batch(&custody, 1, snapshot("c1", "hello"), now);
        let period = accepted(custody.accept(&first, now));
        assert_eq!(
            custody.accept(&first, now).0,
            Outcome::Duplicate {
                period_id: period.clone()
            }
        );
        let facts = custody.facts(Some("g1"), false);
        assert_eq!(
            (facts.capture, facts.delivery),
            ("permitted", "kept_locally")
        );
        // A reopen (an app restart) still knows the receipt.
        drop(custody);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        assert_eq!(custody.period_id(), Some(period.as_str()));
        assert_eq!(
            custody.accept(&first, now).0,
            Outcome::Duplicate { period_id: period }
        );
    }

    #[test]
    fn old_1_1_shaped_batches_are_accepted_and_pending_identity_survives_receipt_pruning() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let mut old_shape = batch(&custody, 9, snapshot("old-context", "held"), now);
        old_shape["destination_generation"] = json!("generation-from-journal-a");
        old_shape["queued_at_ms"] = json!(now_ms(&now).saturating_sub(60 * 60 * 1000));
        let period = accepted(custody.accept(&old_shape, now));
        custody.tick(at(10, 5, 0));
        let later = at(12, 2, 0);
        custody.prune_receipts(now_ms(&later));
        let mut replay = old_shape;
        replay["destination_generation"] = json!("generation-from-journal-b");
        assert_eq!(
            custody.accept(&replay, later).0,
            Outcome::Duplicate { period_id: period }
        );
    }

    #[test]
    fn with_bound_keeps_known_ids_duplicate_before_refusing_new_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let known = batch(&custody, 1, snapshot("c1", "known"), now);
        let period = accepted(custody.accept(&known, now));
        let held = custody.facts(Some("live-generation"), false).held_bytes;
        custody = custody.with_bound(held);
        let mut replay = known;
        replay["destination_generation"] = json!("old-generation");
        assert_eq!(
            custody.accept(&replay, now).0,
            Outcome::Duplicate { period_id: period }
        );
        assert_eq!(
            custody
                .accept(&batch(&custody, 2, snapshot("c2", "new"), now), now)
                .0,
            reject("queue_full")
        );
    }

    #[test]
    fn discard_tombstone_survives_restart_and_late_replay() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let batch = batch(&custody, 5, snapshot("c1", "discarded"), now);
        let period = accepted(custody.accept(&batch, now));
        let open_dir = custody.layout.open().join(&period);
        custody.discard_pending().unwrap();
        assert!(!open_dir.exists());
        drop(custody);

        let late_ms =
            now.timestamp_millis() + i64::try_from(3 * ACCEPTED_RETENTION_MS_MIN + 1).unwrap();
        let late = Local.timestamp_millis_opt(late_ms).single().unwrap();
        let mut custody = Custody::open(layout(&temp), late).unwrap();
        let mut replay = batch;
        replay["destination_generation"] = json!("different-live-generation");
        replay["queued_at_ms"] = json!(now_ms(&now));
        assert!(matches!(
            custody.accept(&replay, late).0,
            Outcome::Duplicate { .. }
        ));
        assert!(
            custody
                .evidence
                .values()
                .any(|kind| *kind == EvidenceKind::Removed)
        );
    }

    #[test]
    fn recovery_finishes_tombstoned_unlink_without_dropping_other_periods() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let removed_batch = batch(&custody, 1, snapshot("c1", "removed"), now);
        accepted(custody.accept(&removed_batch, now));
        custody.tick(at(10, 5, 0));

        let pending_batch = batch(&custody, 2, snapshot("c2", "pending"), at(10, 6, 0));
        let pending_period = accepted(custody.accept(&pending_batch, at(10, 6, 0)));
        let pending_dir = custody.layout.open().join(&pending_period);
        let segment = custody.layout.captures.join("20261002/_browser/100100_240");
        custody.tombstone_directory(&segment).unwrap();
        fs::remove_file(segment.join(crate::browser::PAGES_FILENAME)).unwrap();
        drop(custody);

        let mut recovered = Custody::open(layout(&temp), at(10, 7, 0)).unwrap();
        assert!(!segment.exists());
        assert!(pending_dir.join(crate::browser::PAGES_FILENAME).is_file());

        let mut replay = removed_batch;
        replay["destination_generation"] = json!("different-generation");
        assert!(matches!(
            recovered.accept(&replay, at(10, 7, 0)).0,
            Outcome::Duplicate { .. }
        ));
    }

    #[test]
    fn discard_skips_reserved_browser_segments_and_clears_open_contexts() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let pending = batch(&custody, 1, snapshot("c1", "open"), now);
        let open_id = accepted(custody.accept(&pending, now));
        assert!(custody.tick(at(10, 5, 0)).is_some());
        let segment = temp.path().join("captures/20261002/_browser/100100_240");
        assert!(custody.reserve_upload(&segment));
        custody.discard_pending().unwrap();
        assert!(segment.join(crate::browser::PAGES_FILENAME).is_file());
        assert!(custody.period.as_ref().unwrap().contexts.is_empty());
        custody.release_upload(&segment);
        custody.discard_pending().unwrap();
        assert!(!segment.exists());
        let next = at(10, 6, 0);
        assert_eq!(
            custody
                .accept(
                    &batch(&custody, 2, delta("c1", "needs snapshot"), next),
                    next
                )
                .0,
            reject("snapshot_required")
        );
        assert!(!custody.layout.open().join(open_id).exists());
    }

    #[test]
    fn failed_open_discard_cannot_mix_removed_bytes_with_new_intake() {
        let temp = tempfile::tempdir().unwrap();
        let now = Local::now();
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let first = batch(&custody, 201, snapshot("c1", "removed-before-unlink"), now);
        let period = accepted(custody.accept(&first, now));
        custody.discard_remove_fault = Some(custody.layout.open().join(period));
        assert!(custody.discard_pending().is_err());
        let next = batch(&custody, 202, snapshot("c1", "fresh-after-discard"), now);
        let _ = custody.accept(&next, now);
        custody.tick(now + chrono::Duration::minutes(6));
        for pages in segment_files(&custody.layout.captures) {
            let text = fs::read_to_string(pages).unwrap();
            assert!(!text.contains("removed-before-unlink"));
        }
        custody.discard_remove_fault = None;
        custody.discard_pending().unwrap();
        let fresh = batch(&custody, 203, snapshot("c1", "fresh-after-retry"), now);
        accepted(custody.accept(&fresh, now));
        custody.tick(now + chrono::Duration::minutes(12));
        let text = segment_files(&custody.layout.captures)
            .into_iter()
            .map(|pages| fs::read_to_string(pages).unwrap())
            .collect::<String>();
        assert!(text.contains("fresh-after-retry"));
        assert!(!text.contains("removed-before-unlink"));
    }

    #[test]
    fn partial_unlink_cannot_reopen_a_removed_period_for_intake() {
        let temp = tempfile::tempdir().unwrap();
        let now = Local::now();
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let first = batch(
            &custody,
            207,
            snapshot("c1", "removed-before-partial-unlink"),
            now,
        );
        let period = accepted(custody.accept(&first, now));
        let directory = custody.layout.open().join(period);
        custody.discard_remove_fault = Some(directory.clone());
        assert!(custody.discard_pending().is_err());
        fs::remove_file(directory.join(super::super::PAGES_FILENAME)).unwrap();
        let fresh = batch(&custody, 208, snapshot("c1", "new-payload"), now);
        assert_eq!(custody.accept(&fresh, now).0, reject("queue_full"));
        custody.discard_remove_fault = None;
        custody.discard_pending().unwrap();
        accepted(custody.accept(&fresh, now));
    }

    #[test]
    fn failed_finalized_discard_prevents_upload_reservation() {
        let temp = tempfile::tempdir().unwrap();
        let now = Local::now();
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let first = batch(&custody, 204, snapshot("c1", "removed-finalized"), now);
        accepted(custody.accept(&first, now));
        custody.tick(now + chrono::Duration::minutes(6));
        let segment = segment_files(&custody.layout.captures)[0]
            .parent()
            .unwrap()
            .to_path_buf();
        custody.discard_remove_fault = Some(segment.clone());
        assert!(custody.discard_pending().is_err());
        assert!(segment.is_dir());
        assert!(!custody.reserve_upload(&segment));
    }

    #[test]
    fn failed_removal_directory_sync_prevents_old_bytes_from_rotation() {
        let temp = tempfile::tempdir().unwrap();
        let now = Local::now();
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let first = batch(
            &custody,
            205,
            snapshot("c1", "removed-before-dir-sync"),
            now,
        );
        accepted(custody.accept(&first, now));
        custody.evidence_fault = Some(crate::private_file::DurableWriteStage::DirSync);
        assert!(custody.discard_pending().is_err());
        let next = batch(&custody, 206, snapshot("c1", "fresh-after-dir-sync"), now);
        assert_eq!(custody.accept(&next, now).0, reject("queue_full"));
        custody.tick(now + chrono::Duration::minutes(6));
        assert!(segment_files(&custody.layout.captures).is_empty());
    }

    #[test]
    fn partial_discard_keeps_failed_segment_and_a_retry_finishes_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut custody = Custody::open(layout(&temp), at(10, 1, 0)).unwrap();
        let first = batch(&custody, 1, snapshot("c1", "first"), at(10, 1, 0));
        accepted(custody.accept(&first, at(10, 1, 0)));
        custody.tick(at(10, 5, 0));
        let second = batch(&custody, 2, snapshot("c2", "second"), at(10, 6, 0));
        accepted(custody.accept(&second, at(10, 6, 0)));
        custody.tick(at(10, 10, 0));

        let segments = segment_files(&temp.path().join("captures"));
        assert_eq!(segments.len(), 2);
        let failed = segments[1].parent().unwrap().to_path_buf();
        custody.discard_remove_fault = Some(failed.clone());
        assert!(custody.discard_pending().is_err());
        assert!(!segments[0].exists());
        assert!(failed.exists());

        custody.discard_remove_fault = None;
        custody.discard_pending().unwrap();
        assert!(!failed.exists());
        drop(custody);

        let mut recovered = Custody::open(layout(&temp), at(10, 11, 0)).unwrap();
        assert!(matches!(
            recovered.accept(&second, at(10, 11, 0)).0,
            Outcome::Duplicate { .. }
        ));
    }

    #[test]
    fn removal_commit_faults_never_leave_payload_gone_without_identity_evidence() {
        for stage in [
            crate::private_file::DurableWriteStage::Create,
            crate::private_file::DurableWriteStage::Write,
            crate::private_file::DurableWriteStage::Fsync,
            crate::private_file::DurableWriteStage::Rename,
            crate::private_file::DurableWriteStage::DirSync,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let now = at(10, 1, 0);
            let mut custody = Custody::open(layout(&temp), now).unwrap();
            let batch = batch(&custody, 7, snapshot("c1", "durable"), now);
            let period = accepted(custody.accept(&batch, now));
            let pages = custody
                .layout
                .open()
                .join(&period)
                .join(crate::browser::PAGES_FILENAME);
            let expected_payload = fs::read(&pages).unwrap();
            custody.evidence_fault = Some(stage);
            assert!(custody.discard_pending().is_err(), "{stage:?}");
            assert!(
                pages.exists(),
                "{stage:?}: payload removed before durable tombstone"
            );
            drop(custody);

            let mut recovered = Custody::open(layout(&temp), at(12, 2, 0)).unwrap();
            let evidence: Vec<EvidenceLine> = recovered.evidence_lines.clone();
            let still_held = sorted_dirs(&recovered.layout.open())
                .into_iter()
                .chain(browser_segments(&recovered.layout.captures))
                .any(|directory| {
                    fs::read(directory.join(crate::browser::PAGES_FILENAME))
                        .is_ok_and(|payload| payload == expected_payload)
                });
            if !still_held {
                assert!(
                    evidence.iter().any(|line| {
                        line.batch_id == format!("{:032x}", 7) && line.kind == EvidenceKind::Removed
                    }),
                    "{stage:?}: payload gone without tombstone"
                );
            }
            let mut replay = batch;
            replay["destination_generation"] = json!("replacement-generation");
            assert!(
                matches!(
                    recovered.accept(&replay, at(12, 2, 0)).0,
                    Outcome::Duplicate { .. }
                ),
                "{stage:?}"
            );
        }
    }

    #[test]
    fn a_delta_needs_a_snapshot_of_its_context_in_the_same_period() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "alpha"), now), now));
        accepted(custody.accept(&batch(&custody, 2, delta("c1", "beta"), now), now));
        assert!(custody.tick(at(10, 4, 59)).is_none());
        let next = custody.tick(at(10, 5, 0)).unwrap();
        assert_eq!(custody.period_id(), Some(next.as_str()));
        let segment = temp.path().join("captures/20261002/_browser/100230_150");
        let pages = fs::read_to_string(segment.join(crate::browser::PAGES_FILENAME)).unwrap();
        assert_eq!(pages.lines().count(), 2);
        assert!(pages.contains("alpha") && pages.contains("beta"));
        // An empty period leaves nothing behind.
        assert!(custody.tick(at(10, 10, 0)).is_some());
        assert_eq!(segment_files(&temp.path().join("captures")).len(), 1);
        assert!(sorted_dirs(&temp.path().join("browser/open")).is_empty());
    }

    #[test]
    fn pairing_independent_open_and_finalized_pages_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let finished = batch(&custody, 1, snapshot("c1", "for-finished"), now);
        let finished_period = accepted(custody.accept(&finished, now));
        custody.tick(at(10, 5, 0));
        let later = at(10, 6, 0);
        let open = batch(&custody, 2, snapshot("c2", "for-open"), later);
        let open_period = accepted(custody.accept(&open, later));
        drop(custody);

        let mut custody = Custody::open(layout(&temp), at(10, 7, 0)).unwrap();
        assert_eq!(segment_files(&temp.path().join("captures")).len(), 1);
        assert!(custody.facts(None, false).held_bytes > 0);
        let mut replay = finished.clone();
        replay["destination_generation"] = json!("generation-for-journal-b");
        assert_eq!(
            custody.accept(&replay, at(10, 7, 0)).0,
            Outcome::Duplicate {
                period_id: finished_period
            }
        );
        let mut replay = open.clone();
        replay["destination_generation"] = json!("generation-for-journal-b");
        assert_eq!(
            custody.accept(&replay, at(10, 7, 0)).0,
            Outcome::Duplicate {
                period_id: open_period
            }
        );
    }

    #[test]
    fn leftover_retired_pages_are_deleted_without_importing_them() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("browser/retired/old/20260101/100000_300");
        create_private_dir(&legacy).unwrap();
        fs::write(legacy.join(crate::browser::PAGES_FILENAME), "old pages").unwrap();
        let custody = Custody::open(layout(&temp), at(10, 1, 0)).unwrap();
        assert!(segment_files(&temp.path().join("captures")).is_empty());
        assert!(custody.evidence.is_empty());
        assert!(!temp.path().join("browser/retired").exists());
    }

    #[test]
    fn stale_generation_and_old_batches_are_accepted_but_future_skew_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let mut stale = batch(&custody, 1, snapshot("c1", "x"), now);
        stale["destination_generation"] = json!("someone-else");
        accepted(custody.accept(&stale, now));
        let mut old = batch(&custody, 2, snapshot("c2", "old"), now);
        old["queued_at_ms"] = json!(now_ms(&now).saturating_sub(60 * 60 * 1000));
        accepted(custody.accept(&old, now));
        let mut future = batch(&custody, 3, snapshot("c1", "x"), now);
        future["queued_at_ms"] = json!(now_ms(&now) + FUTURE_SKEW_MS_MAX + 1);
        assert_eq!(custody.accept(&future, now).0, reject("age_policy"));
    }

    #[test]
    fn malformed_and_file_oversize_batches_are_refused_with_current_reasons() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        let mut malformed = batch(&custody, 1, snapshot("c1", "ok"), now);
        malformed["records"] = json!([]);
        assert_eq!(custody.accept(&malformed, now).0, reject("malformed"));

        let large = "x".repeat(FILE_MAX + 1);
        let oversized = batch(
            &custody,
            2,
            json!([{"t":"segment_start", "ts":1, "ctx":"c1", "inst":"inst-a",
                    "blocks":[{"id":"b1", "text":large}]}]),
            now,
        );
        assert_eq!(custody.accept(&oversized, now).0, reject("oversize"));
    }

    #[test]
    fn a_full_queue_refuses_new_intake_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "first"), now), now));
        let held = custody.facts(Some("g1"), false).held_bytes;
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
        assert_eq!(custody.facts(Some("g1"), false).capture, "permitted");
        custody = custody.with_bound(held);
        let facts = custody.facts(Some("g1"), false);
        assert!(facts.full);
        assert_eq!(
            (facts.capture, facts.delivery, facts.failure),
            ("intake_off", "kept_locally", Some("queue_full"))
        );
        assert_eq!(custody.facts(Some("g1"), true).capture, "paused");
    }

    #[test]
    fn bytes_no_receipt_covers_are_dropped_on_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let custody = Custody::open(layout(&temp), now).unwrap();
        assert_eq!(fs::read(&pages).unwrap(), committed);
        assert_eq!(
            custody.facts(Some("g1"), false).held_bytes,
            held_bytes(&layout(&temp))
        );
        assert!(custody.facts(Some("g1"), false).held_bytes >= committed.len() as u64);
    }

    #[test]
    fn bytes_left_by_a_failed_write_are_overwritten_and_never_covered() {
        let temp = tempfile::tempdir().unwrap();
        let now = at(10, 1, 0);
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let mut custody = Custody::open(layout(&temp), now).unwrap();
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
        let mut custody = Custody::open(layout(&temp), opened).unwrap();
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
        let mut custody = Custody::open(layout(&temp), opened).unwrap();
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
        let mut custody = Custody::open(layout(&temp), opened).unwrap();
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
        Custody::open(layout(&temp), at(18, 0, 0)).unwrap();
        let names = segment_names(&captures);
        assert_eq!(names, ["100100_140"]);
        assert_lengths_within_ceiling(&names);
    }

    #[test]
    fn a_period_without_a_known_end_is_bounded_by_its_window() {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let opened = at(10, 1, 0);
        let custody = Custody::open(layout(&temp), opened).unwrap();
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
        let mut custody = Custody::open(layout(&temp), now).unwrap();
        accepted(custody.accept(&batch(&custody, 1, snapshot("c1", "kept"), now), now));
        drop(custody);
        let custody = Custody::open(layout(&temp), at(12, 0, 0)).unwrap();
        assert_eq!(segment_files(&temp.path().join("captures")).len(), 1);
        assert!(custody.period_id().is_some());
    }
}
