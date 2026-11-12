// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The table: what the last scans established about each note, persisted privately.

use crate::{
    clock::{Stamp, Window},
    store,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

/// What the journal has been told about one note.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Row {
    pub mtime_ms: i64,
    pub size: u64,
    pub file_id: Option<String>,
    pub content_hash: String,
    pub last_reported_ms: Option<i64>,
    pub sent_window: Option<Window>,
}

/// The first bringing-in of a vault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirstSync {
    pub found: u64,
    pub sent: u64,
}

/// A note the engine has placed, recorded in the same save as the table row.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Intake {
    pub id: u64,
    pub path: String,
    pub previous_path: Option<String>,
    pub replaced: Vec<String>,
    pub first_sight: bool,
    pub removed: bool,
    /// The walk that first listed a vault already on this computer.
    pub inventory: bool,
    pub at_ms: i64,
}

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Table {
    pub rows: BTreeMap<String, Row>,
    /// Missing paths held one scan, with when each was first seen missing.
    pub held: BTreeMap<String, Stamp>,
    pub first_send_done: bool,
    pub first_sync: Option<FirstSync>,
    pub first_sync_started: bool,
    #[serde(default)]
    pub intakes: Vec<Intake>,
    #[serde(default)]
    pub next_intake_id: u64,
    /// The first listing of this vault has not finished. A checkpoint in the
    /// middle of that walk leaves this set.
    #[serde(default)]
    pub first_list_open: bool,
    /// Paths the first complete directory walk saw and has not yet recorded as
    /// rows. While the first listing is open, missing means that walk has not
    /// happened, which is not an empty set: an empty set is a vault the walk
    /// saw whole and found nothing in. After that listing closes, the set keeps
    /// only paths that still have no row, and missing means those are done. A
    /// path leaves the set when the walk could have seen it and did not, on the
    /// same wait as a row: a path with no row is remembered for one miss, and
    /// forgotten on the second, with no hash written. A new note there can then
    /// count.
    #[serde(default)]
    pub listing_paths: Option<BTreeSet<String>>,
    /// Listing paths with no row that the previous walk did not see.
    #[serde(default)]
    pub listing_absent: BTreeSet<String>,
    /// File id for a listing path, when the walk had one, so a rename of a note
    /// that has no row yet stays recorded.
    #[serde(default)]
    pub listing_ids: BTreeMap<String, String>,
}

impl Table {
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        store::atomic_write(path, &bytes)
    }

    /// Forget what was sent, so the vault's next scan is its first send again.
    /// The intakes and their ids stay: the counters ignore an id they have
    /// already passed.
    pub fn restart_send(&mut self) {
        self.rows.clear();
        self.held.clear();
        self.first_send_done = false;
        self.first_sync = None;
        self.first_sync_started = false;
        self.first_list_open = false;
        self.listing_paths = None;
        self.listing_absent.clear();
        self.listing_ids.clear();
    }
}
