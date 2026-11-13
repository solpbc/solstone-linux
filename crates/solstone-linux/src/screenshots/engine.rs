// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What the screenshots source knows and decides, with no journal in it: the
//! folder, which of its images are screenshots, what the owner chose to bring in,
//! the pauses that skip, and where each screenshot stands.
//!
//! The record is `state/screenshots/ledger.json`. It names no journal: a new
//! pairing asks how far back again, and the journal answers what it already holds.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solstone_vault::Clock;

use super::folder::{self, Places};
use super::proof::{self, Proof, Recents};

/// What the journal keeps as it is.
pub(crate) const ACCEPTED: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// What the desktop's screenshot tools can also save, converted to PNG here.
pub(crate) const CONVERTED: [&str; 9] = [
    "avif", "heif", "heic", "jxl", "bmp", "tif", "tiff", "gif", "jp2",
];
/// A file written this recently may still be being written.
const SETTLE_MS: i64 = 2_000;
const DAY_MS: i64 = 86_400_000;
/// A screenshot ahead of the journal's day, or one the journal couldn't take right
/// now, is offered again after this long, then longer.
const RETRY_MS: [i64; 4] = [30_000, 120_000, 600_000, 1_800_000];
const LEDGER: &str = "ledger.json";

/// How far back the owner chose to start.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Span {
    #[serde(rename = "last_30_days")]
    Last30Days,
    #[serde(rename = "last_6_months")]
    Last6Months,
    #[serde(rename = "last_year")]
    LastYear,
    #[serde(rename = "all")]
    All,
    #[serde(rename = "today")]
    Today,
}

impl Span {
    pub const ALL: [Self; 5] = [
        Self::Last30Days,
        Self::Last6Months,
        Self::LastYear,
        Self::All,
        Self::Today,
    ];

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(text.to_owned())).ok()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last30Days => "last_30_days",
            Self::Last6Months => "last_6_months",
            Self::LastYear => "last_year",
            Self::All => "all",
            Self::Today => "today",
        }
    }

    /// Where this span begins, for a choice made at `now_ms`.
    fn start_ms(self, now_ms: i64, offset_s: i32) -> i64 {
        match self {
            Self::Last30Days => now_ms - 30 * DAY_MS,
            Self::Last6Months => now_ms - 183 * DAY_MS,
            Self::LastYear => now_ms - 365 * DAY_MS,
            Self::All => i64::MIN,
            Self::Today => {
                let local = now_ms + i64::from(offset_s) * 1000;
                local - local.rem_euclid(DAY_MS) - i64::from(offset_s) * 1000
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Never turned on.
    #[default]
    NotSetUp,
    /// Choosing how far back. Nothing is sent until the confirm.
    Choosing,
    On,
    /// Turned off by the owner.
    Off,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TakenFrom {
    Platform,
    File,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ItemState {
    /// Waiting to go to the journal.
    Waiting,
    /// In the journal: kept, or already there.
    Delivered,
    /// Deleted or changed in the folder before it was sent. It never goes.
    Gone,
    /// Final: a file that couldn't be converted, too big, or refused by the journal.
    CantComeIn,
    /// Held from an earlier choice, and outside the span the owner chose since.
    Left,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Item {
    pub path: PathBuf,
    pub captured_ms: i64,
    pub offset_s: i32,
    pub taken_from: TakenFrom,
    pub state: ItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    #[serde(default)]
    pub tries: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Ledger {
    pub v: u32,
    pub mode: Mode,
    /// The source has been on before, so turning it on says S17.
    #[serde(default)]
    pub returning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_folder: Option<PathBuf>,
    /// The folder was there while the source was set up, so its going is a fault.
    #[serde(default)]
    pub folder_seen: bool,
    /// Where the chosen span begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_from_ms: Option<i64>,
    /// Pauses in this on period, as `[start, end)`.
    #[serde(default)]
    pub pauses: Vec<[i64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_since_ms: Option<i64>,
    /// Keyed by the SHA-256 of the file in the folder.
    #[serde(default)]
    pub items: BTreeMap<String, Item>,
    /// What the last confirm brought in, so its count completes on its own.
    #[serde(default)]
    pub backfill: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_ms: Option<i64>,
}

/// One proven screenshot in the folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Found {
    pub id: String,
    pub path: PathBuf,
    pub captured_ms: i64,
    pub offset_s: i32,
    pub taken_from: TakenFrom,
}

/// What a look at the folder found.
#[derive(Clone, Debug, Default)]
pub(crate) struct Look {
    pub found: Vec<Found>,
    /// Proven screenshots whose bytes couldn't be read, with when they were taken.
    pub unreadable: Vec<(PathBuf, i64)>,
    /// The folder is there.
    pub present: bool,
}

/// A screenshot ready to send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Delivery {
    pub id: String,
    pub path: PathBuf,
    pub captured_ms: i64,
    pub offset_s: i32,
    pub taken_from: TakenFrom,
}

/// How one send ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    Delivered,
    CantComeIn,
    /// Hold it and offer it again later.
    Hold,
    /// The file is no longer the one that was found.
    Gone,
}

/// The chooser's count, frozen for the confirm.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Counted {
    span: Span,
    from_ms: i64,
    ids: BTreeSet<String>,
    unreadable: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Seen {
    size: u64,
    mtime_ns: i128,
}

/// A file's last reading. Only a proven one has an id.
#[derive(Clone, Debug)]
struct Reading {
    seen: Seen,
    found: Option<Found>,
}

/// Told each screenshot's identity the first time it is taken in.
pub(crate) type IntakeHook = Arc<dyn Fn(&str) + Send + Sync>;

pub(crate) struct Engine {
    dir: PathBuf,
    places: Places,
    clock: Arc<dyn Clock>,
    ledger: Ledger,
    readings: HashMap<PathBuf, Reading>,
    recents_seen: Option<Seen>,
    recents: Recents,
    counted: Option<Counted>,
    /// The last look's unreadable screenshots.
    unreadable: Vec<(PathBuf, i64)>,
    on_intake: Option<IntakeHook>,
}

impl Engine {
    /// Open the record. One that can't be read starts over, set up again from the
    /// chooser: nothing in it names a journal, and the journal answers a resend.
    pub(crate) fn open(dir: PathBuf, places: Places, clock: Arc<dyn Clock>) -> Self {
        let ledger = std::fs::read(dir.join(LEDGER))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ledger>(&bytes).ok())
            .filter(|ledger| ledger.v == 1)
            .unwrap_or_else(|| Ledger {
                v: 1,
                ..Ledger::default()
            });
        Self {
            dir,
            places,
            clock,
            ledger,
            readings: HashMap::new(),
            recents_seen: None,
            recents: Recents::default(),
            counted: None,
            unreadable: Vec::new(),
            on_intake: None,
        }
    }

    /// Called once for each screenshot the first time it is taken in.
    pub(crate) fn set_intake_hook(&mut self, hook: IntakeHook) {
        self.on_intake = Some(hook);
    }

    pub(crate) fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub(crate) fn unreadable(&self) -> usize {
        self.unreadable.len()
    }

    pub(crate) fn counted(&self) -> Option<(Span, usize)> {
        self.counted
            .as_ref()
            .map(|counted| (counted.span, counted.ids.len() + counted.unreadable))
    }

    fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let bytes = serde_json::to_vec(&self.ledger).map_err(std::io::Error::other)?;
        crate::private_file::atomic_write_bytes(&self.dir.join(LEDGER), &bytes)
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// The folder screenshots come from: the owner's choice, or the one resolved.
    pub(crate) fn folder(&self) -> Option<PathBuf> {
        self.ledger
            .chosen_folder
            .clone()
            .or_else(|| folder::resolve(&self.places))
    }

    pub(crate) fn home(&self) -> &Path {
        &self.places.home
    }

    /// The folder was there and isn't now.
    pub(crate) fn folder_gone(&self) -> bool {
        self.ledger.folder_seen && self.folder().is_none_or(|folder| !folder.is_dir())
    }

    pub(crate) fn is_on(&self) -> bool {
        self.ledger.mode == Mode::On
    }

    /// Turn on from off or never set up: the chooser comes next.
    pub(crate) fn turn_on(&mut self) -> std::io::Result<()> {
        if matches!(self.ledger.mode, Mode::NotSetUp | Mode::Off) {
            self.ledger.mode = Mode::Choosing;
            self.counted = None;
            self.note_folder();
            self.save()?;
        }
        Ok(())
    }

    pub(crate) fn turn_off(&mut self) -> std::io::Result<()> {
        if self.ledger.mode == Mode::NotSetUp {
            return Ok(());
        }
        if self.ledger.mode == Mode::On {
            self.ledger.returning = true;
        }
        self.ledger.mode = Mode::Off;
        self.ledger.backfill.clear();
        self.ledger.pauses.clear();
        self.ledger.paused_since_ms = None;
        self.counted = None;
        self.save()
    }

    /// The owner chose a folder. How far back is asked again for it.
    pub(crate) fn choose_folder(&mut self, folder: PathBuf) -> std::io::Result<()> {
        if self.ledger.mode == Mode::On {
            self.ledger.returning = true;
        }
        self.ledger.chosen_folder = Some(folder);
        self.ledger.mode = Mode::Choosing;
        self.ledger.backfill.clear();
        self.ledger.folder_seen = false;
        self.counted = None;
        self.readings.clear();
        self.note_folder();
        self.save()
    }

    /// The owner paired a journal: the source asks how far back again, and sends
    /// nothing until the confirm. What it holds stays.
    pub(crate) fn restart_setup(&mut self) -> std::io::Result<()> {
        if !matches!(self.ledger.mode, Mode::On | Mode::Choosing) {
            return Ok(());
        }
        if self.ledger.mode == Mode::On {
            self.ledger.returning = true;
        }
        self.ledger.mode = Mode::Choosing;
        self.ledger.backfill.clear();
        self.counted = None;
        self.save()
    }

    fn note_folder(&mut self) {
        if self.folder().is_some_and(|folder| folder.is_dir()) {
            self.ledger.folder_seen = true;
        }
    }

    /// The computer paused or resumed. A screenshot taken while paused never comes in.
    pub(crate) fn set_paused(&mut self, paused: bool) -> std::io::Result<()> {
        let now = self.clock.now_ms();
        match (paused, self.ledger.paused_since_ms) {
            (true, None) => self.ledger.paused_since_ms = Some(now),
            (false, Some(since)) => {
                self.ledger.paused_since_ms = None;
                if self.ledger.mode == Mode::On {
                    self.ledger.pauses.push([since, now]);
                }
            }
            _ => return Ok(()),
        }
        self.save()
    }

    fn taken_while_paused(&self, at_ms: i64) -> bool {
        self.ledger
            .paused_since_ms
            .is_some_and(|since| at_ms >= since)
            || self
                .ledger
                .pauses
                .iter()
                .any(|[start, end]| (*start..*end).contains(&at_ms))
    }

    fn reload_recents(&mut self) {
        let path = self.places.data.join("recently-used.xbel");
        let seen = std::fs::metadata(&path).ok().map(|meta| seen_of(&meta));
        if seen == self.recents_seen {
            return;
        }
        self.recents_seen = seen;
        self.recents = std::fs::read_to_string(&path)
            .map(|text| Recents::parse(&text))
            .unwrap_or_default();
        // What wasn't proven before may be now.
        self.readings.retain(|_, reading| reading.found.is_some());
    }

    /// Look at the folder: every image in it the app can tell is a screenshot.
    pub(crate) fn look(&mut self) -> Look {
        let now = self.clock.now_ms();
        let Some(folder) = self.folder() else {
            return Look::default();
        };
        let Ok(entries) = std::fs::read_dir(&folder) else {
            return Look::default();
        };
        self.reload_recents();
        let mut look = Look {
            present: true,
            ..Look::default()
        };
        let mut present = BTreeSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Some(extension) = extension_of(&path) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let seen = seen_of(&meta);
            if now - ((seen.mtime_ns / 1_000_000) as i64) < SETTLE_MS {
                continue;
            }
            present.insert(path.clone());
            if let Some(reading) = self.readings.get(&path)
                && reading.seen == seen
            {
                if let Some(found) = &reading.found {
                    look.found.push(found.clone());
                }
                continue;
            }
            let proof = match proof::prove(&path, &extension, &self.recents) {
                Ok(proof) => proof,
                Err(_) => continue,
            };
            let Some(proof) = proof else {
                self.readings.insert(path, Reading { seen, found: None });
                continue;
            };
            let (captured_ms, taken_from) = match proof {
                Proof::Recent { saved_ms } => (saved_ms, TakenFrom::Platform),
                Proof::Gnome => ((seen.mtime_ns / 1_000_000) as i64, TakenFrom::File),
            };
            match hash_file(&path) {
                Ok(id) => {
                    let found = Found {
                        id,
                        path: path.clone(),
                        captured_ms,
                        offset_s: self.clock.offset_s_at(captured_ms),
                        taken_from,
                    };
                    look.found.push(found.clone());
                    self.readings.insert(
                        path,
                        Reading {
                            seen,
                            found: Some(found),
                        },
                    );
                }
                Err(_) => look.unreadable.push((path, captured_ms)),
            }
        }
        self.readings.retain(|path, _| present.contains(path));
        look
    }

    /// Count what the owner would bring in from `span`, and freeze it for the confirm.
    pub(crate) fn count(&mut self, span: Span) -> Option<usize> {
        let now = self.clock.now_ms();
        let look = self.look();
        if !look.present && self.folder().is_some_and(|folder| folder.exists()) {
            return None;
        }
        let from_ms = span.start_ms(now, self.clock.offset_s_at(now));
        let in_span = |at: i64| at >= from_ms && at <= now;
        let ids = look
            .found
            .iter()
            .filter(|found| in_span(found.captured_ms))
            .map(|found| found.id.clone())
            .collect::<BTreeSet<_>>();
        let unreadable = look
            .unreadable
            .iter()
            .filter(|(_, at)| in_span(*at))
            .count();
        self.unreadable = look.unreadable;
        self.counted = Some(Counted {
            span,
            from_ms,
            ids,
            unreadable,
        });
        self.ledger.last_checked_ms = Some(now);
        self.counted().map(|(_, count)| count)
    }

    /// The owner pressed the confirm for `span`: exactly the counted screenshots come
    /// in, then each new one.
    pub(crate) fn confirm(&mut self, span: Span) -> std::io::Result<bool> {
        if !matches!(self.ledger.mode, Mode::NotSetUp | Mode::Choosing) {
            return Ok(false);
        }
        if self
            .counted
            .as_ref()
            .is_none_or(|counted| counted.span != span)
            && self.count(span).is_none()
        {
            return Ok(false);
        }
        let Some(counted) = self.counted.take() else {
            return Ok(false);
        };
        let found = self
            .readings
            .values()
            .filter_map(|reading| reading.found.clone())
            .filter(|found| counted.ids.contains(&found.id))
            .collect::<Vec<_>>();
        self.ledger.mode = Mode::On;
        self.ledger.span_from_ms = Some(counted.from_ms);
        self.ledger.pauses.clear();
        if self.ledger.paused_since_ms.is_some() {
            self.ledger.paused_since_ms = Some(self.clock.now_ms());
        }
        self.ledger.backfill = counted.ids.clone();
        for item in self.ledger.items.values_mut() {
            if item.state == ItemState::Waiting && item.captured_ms < counted.from_ms {
                item.state = ItemState::Left;
            }
        }
        for found in found {
            // A screenshot the first journal has goes to this one too; the journal
            // answers what it already holds.
            self.admit(found, true);
        }
        self.note_folder();
        self.save()?;
        Ok(true)
    }

    fn admit(&mut self, found: Found, reset: bool) {
        let first = !self.ledger.items.contains_key(&found.id);
        let item = self
            .ledger
            .items
            .entry(found.id.clone())
            .or_insert_with(|| Item {
                path: found.path.clone(),
                captured_ms: found.captured_ms,
                offset_s: found.offset_s,
                taken_from: found.taken_from,
                state: ItemState::Waiting,
                retry_at_ms: None,
                tries: 0,
            });
        item.path = found.path.clone();
        if reset {
            item.state = ItemState::Waiting;
            item.retry_at_ms = None;
            item.tries = 0;
        }
        if first && let Some(hook) = &self.on_intake {
            hook(&found.id);
        }
    }

    /// One look while on: take in each new screenshot, and settle what left the folder.
    pub(crate) fn tick(&mut self) -> std::io::Result<()> {
        if self.ledger.mode != Mode::On {
            return Ok(());
        }
        let look = self.look();
        self.ledger.last_checked_ms = Some(self.clock.now_ms());
        self.unreadable = look.unreadable;
        if !look.present {
            return self.save();
        }
        self.ledger.folder_seen = true;
        let from = self.ledger.span_from_ms.unwrap_or(i64::MAX);
        let unreadable = self
            .unreadable
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();
        let mut here = BTreeSet::new();
        for found in look.found {
            here.insert(found.id.clone());
            if self.ledger.items.contains_key(&found.id) {
                if let Some(item) = self.ledger.items.get_mut(&found.id) {
                    item.path = found.path;
                }
                continue;
            }
            if found.captured_ms < from || self.taken_while_paused(found.captured_ms) {
                continue;
            }
            self.admit(found, false);
        }
        for (id, item) in &mut self.ledger.items {
            if item.state == ItemState::Waiting
                && !here.contains(id)
                && !unreadable.contains(&item.path)
            {
                item.state = ItemState::Gone;
            }
        }
        self.settle_backfill();
        self.save()
    }

    fn settle_backfill(&mut self) {
        let (done, total) = self.progress_counts();
        if total > 0 && done >= total {
            self.ledger.backfill.clear();
        }
    }

    fn progress_counts(&self) -> (usize, usize) {
        let done = self
            .ledger
            .backfill
            .iter()
            .filter(|id| {
                self.ledger
                    .items
                    .get(*id)
                    .is_none_or(|item| item.state != ItemState::Waiting)
            })
            .count();
        (done, self.ledger.backfill.len())
    }

    /// `{d} of {N}` while what the owner chose is still going in.
    pub(crate) fn progress(&self) -> Option<(usize, usize)> {
        let (done, total) = self.progress_counts();
        (self.ledger.mode == Mode::On && total > 0 && done < total).then_some((done, total))
    }

    pub(crate) fn waiting(&self) -> usize {
        self.count_state(ItemState::Waiting)
    }

    pub(crate) fn cant_come_in(&self) -> usize {
        self.count_state(ItemState::CantComeIn)
    }

    fn count_state(&self, state: ItemState) -> usize {
        self.ledger
            .items
            .values()
            .filter(|item| item.state == state)
            .count()
    }

    /// The newest screenshot waiting whose turn has come.
    pub(crate) fn next_delivery(&self) -> Option<Delivery> {
        if self.ledger.mode != Mode::On {
            return None;
        }
        let now = self.clock.now_ms();
        self.ledger
            .items
            .iter()
            .filter(|(_, item)| {
                item.state == ItemState::Waiting && item.retry_at_ms.is_none_or(|at| at <= now)
            })
            .max_by_key(|(_, item)| item.captured_ms)
            .map(|(id, item)| Delivery {
                id: id.clone(),
                path: item.path.clone(),
                captured_ms: item.captured_ms,
                offset_s: item.offset_s,
                taken_from: item.taken_from,
            })
    }

    pub(crate) fn record(&mut self, id: &str, outcome: Outcome) -> std::io::Result<()> {
        let now = self.clock.now_ms();
        let Some(item) = self.ledger.items.get_mut(id) else {
            return Ok(());
        };
        match outcome {
            Outcome::Delivered => item.state = ItemState::Delivered,
            Outcome::CantComeIn => item.state = ItemState::CantComeIn,
            Outcome::Gone => item.state = ItemState::Gone,
            Outcome::Hold => {
                let wait = RETRY_MS[(item.tries as usize).min(RETRY_MS.len() - 1)];
                item.tries = item.tries.saturating_add(1);
                item.retry_at_ms = Some(now + wait);
            }
        }
        if outcome != Outcome::Hold {
            item.retry_at_ms = None;
        }
        self.settle_backfill();
        self.save()
    }

    /// `try again` on a screenshot that couldn't be read: look at once, and offer
    /// whatever is held now.
    pub(crate) fn retry(&mut self) -> std::io::Result<()> {
        for item in self.ledger.items.values_mut() {
            if item.state == ItemState::Waiting {
                item.retry_at_ms = None;
            }
        }
        self.readings.clear();
        self.tick()
    }
}

pub(crate) fn extension_of(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    (ACCEPTED.contains(&extension.as_str()) || CONVERTED.contains(&extension.as_str()))
        .then_some(extension)
}

fn seen_of(meta: &std::fs::Metadata) -> Seen {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos() as i128);
    Seen {
        size: meta.len(),
        mtime_ns,
    }
}

pub(crate) fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests;
