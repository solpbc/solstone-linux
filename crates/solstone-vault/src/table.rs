// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The table: what the last scans established about each note, persisted privately.

use crate::{
    clock::{Stamp, Window},
    store,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::Path};

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

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Table {
    pub rows: BTreeMap<String, Row>,
    /// Missing paths held one scan, with when each was first seen missing.
    pub held: BTreeMap<String, Stamp>,
    pub first_send_done: bool,
    pub first_sync: Option<FirstSync>,
    pub first_sync_started: bool,
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
}
