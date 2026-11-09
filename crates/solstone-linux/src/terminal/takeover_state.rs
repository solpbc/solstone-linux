// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The durable record of moving an existing solstone-tmux install into this app.
//!
//! Only the process holding this app's capture-root lock writes it. Writes are
//! atomic and durable, so a reader sees the old record or the new one. A record
//! that cannot be read is therefore not a torn write: it is a hard stop, shown to
//! the owner, and nothing is re-run or re-minted behind it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::private_file::{DurableWriteFault, NoWriteFault, atomic_write_bytes_with_fault};

pub const STATE_FILENAME: &str = "terminal-takeover.json";

/// How far the takeover has come. Every phase is safe to run again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The owner agreed and the old install's details are recorded.
    #[default]
    Consented,
    /// The old service is stopped for good.
    Stopped,
    /// Every root's recovery has been attempted.
    Recovered,
    /// Finished: an install is never found again.
    Done,
}

/// Which journal a credential belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalIdentity {
    /// The id of the CA the credential pins, when readable.
    pub pinned_jid: Option<String>,
    pub instance_id: String,
    /// Lowercase hex of the pinned CA fingerprint prefix.
    pub ca_fingerprint: String,
}

impl JournalIdentity {
    /// Compared on the pinned CA where both sides can read one, because a stored
    /// instance id may have been rewritten to it.
    pub fn same_journal(&self, other: &Self) -> bool {
        match (&self.pinned_jid, &other.pinned_jid) {
            (Some(ours), Some(theirs)) => ours == theirs,
            _ => {
                self.instance_id == other.instance_id && self.ca_fingerprint == other.ca_fingerprint
            }
        }
    }
}

/// A migration request as first sent. Every retry sends exactly these bytes from
/// exactly this caller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub operation_id: String,
    pub body: String,
    pub caller_id: String,
}

/// How the terminal history's continuity settled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
pub enum Continuity {
    /// This app took over the old pairing, so the device and stream are unchanged.
    Adopted,
    /// The journal handed the old device's stream to this app's device.
    Continued,
    /// Later captures start a new stream.
    NewStream {
        reason: String,
        /// The journal may still hold a prepared decision for this device.
        #[serde(default)]
        needs_attention: bool,
    },
}

/// What one legacy data root's recovery left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootRecord {
    pub recovered: bool,
    /// Why this root could not be used; it is retried at each daemon start.
    pub problem: Option<String>,
    pub held: usize,
    pub problem_paths: Vec<PathBuf>,
    /// Finished segments still waiting to be delivered, as of the last drain.
    #[serde(default)]
    pub waiting: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverState {
    pub phase: Phase,
    /// Canonical legacy config roots, in candidate order.
    pub config_roots: Vec<PathBuf>,
    /// Canonical legacy data roots, in candidate order.
    pub data_roots: Vec<PathBuf>,
    /// `tmux_path` from the old `local-observer.json`, read before it is deleted.
    pub tmux_path: Option<String>,
    /// Whether the old service was enabled or active when the owner agreed.
    pub service_was_on: bool,
    /// The owner asked for this move by turning terminal on.
    #[serde(default)]
    pub owner_turned_on: bool,
    /// The owner's capture choices, as this app's `terminal_settings`.
    pub carried_settings: Option<Value>,
    /// Those choices are in this app's config.
    pub settings_applied: bool,
    /// Terminal has been turned on (or deliberately left off) for this takeover.
    pub terminal_settled: bool,
    pub roots: BTreeMap<PathBuf, RootRecord>,
    /// Device id of the old pairing (`sha256:<hex>`).
    pub legacy_device_id: Option<String>,
    pub legacy_journal: Option<JournalIdentity>,
    pub migration: Option<MigrationRecord>,
    pub continuity: Option<Continuity>,
    /// Two different old pairings exist; neither is touched.
    pub conflicting_credentials: Vec<PathBuf>,
    /// The latest journal answer still being retried, for status.
    pub last_answer: Option<String>,
}

impl TakeoverState {
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Records "done" once nothing is left to decide. The drain carries on after it.
    pub fn finish_if_complete(&mut self) {
        if self.phase == Phase::Recovered
            && self.settings_applied
            && self.terminal_settled
            && self.continuity.is_some()
        {
            self.phase = Phase::Done;
        }
    }

    /// `source = "tmux"` uploads wait until continuity is settled.
    pub fn uploads_gated(&self) -> bool {
        self.continuity.is_none()
    }
}

/// The record on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    Absent,
    Present(Box<TakeoverState>),
    /// Unreadable or corrupt: a hard stop naming this path.
    Unusable(PathBuf),
}

impl Stored {
    pub fn is_unusable(&self) -> bool {
        matches!(self, Self::Unusable(_))
    }

    pub fn present(&self) -> Option<&TakeoverState> {
        match self {
            Self::Present(state) => Some(state),
            _ => None,
        }
    }

    /// Whether `source = "tmux"` uploads must wait.
    pub fn uploads_gated(&self) -> bool {
        match self {
            Self::Absent => false,
            Self::Present(state) => state.uploads_gated(),
            Self::Unusable(_) => true,
        }
    }
}

pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILENAME)
}

pub fn load(state_dir: &Path) -> Stored {
    let path = state_path(state_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Stored::Absent,
        Err(_) => return Stored::Unusable(path),
    };
    match serde_json::from_slice(&bytes) {
        Ok(state) => Stored::Present(Box::new(state)),
        Err(_) => Stored::Unusable(path),
    }
}

/// One reader-modifier at a time within the daemon: the takeover's own thread and
/// sync both move the record forward.
static WRITE: Mutex<()> = Mutex::new(());

/// Applies `change` to the stored record and saves it. `None` when there is no
/// usable record.
pub fn update<R>(
    state_dir: &Path,
    change: impl FnOnce(&mut TakeoverState) -> R,
) -> io::Result<Option<R>> {
    let _guard = WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Stored::Present(mut state) = load(state_dir) else {
        return Ok(None);
    };
    let result = change(&mut state);
    save(state_dir, &state)?;
    Ok(Some(result))
}

/// Records `state` unless a record exists. `false` when one already did.
pub fn save_if_absent(state_dir: &Path, state: &TakeoverState) -> io::Result<bool> {
    let _guard = WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if load(state_dir) != Stored::Absent {
        return Ok(false);
    }
    save(state_dir, state)?;
    Ok(true)
}

pub fn save(state_dir: &Path, state: &TakeoverState) -> io::Result<()> {
    save_with_fault(state_dir, state, &NoWriteFault)
}

pub(crate) fn save_with_fault(
    state_dir: &Path,
    state: &TakeoverState,
    fault: &dyn DurableWriteFault,
) -> io::Result<()> {
    // A computer the move reaches first may never have started this app.
    fs::create_dir_all(state_dir)?;
    let mut bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    bytes.push(b'\n');
    atomic_write_bytes_with_fault(&state_path(state_dir), &bytes, fault)
        .map_err(|_| io::Error::other("could not save the move's record"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_file::DurableWriteStage;

    fn identity(pinned: Option<&str>, instance: &str, ca: &str) -> JournalIdentity {
        JournalIdentity {
            pinned_jid: pinned.map(str::to_owned),
            instance_id: instance.to_owned(),
            ca_fingerprint: ca.to_owned(),
        }
    }

    #[test]
    fn nothing_on_disk_is_absent_and_gates_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let stored = load(temp.path());
        assert_eq!(stored, Stored::Absent);
        assert!(!stored.uploads_gated());
    }

    #[test]
    fn a_saved_record_reads_back_whole() {
        let temp = tempfile::tempdir().unwrap();
        let state = TakeoverState {
            phase: Phase::Stopped,
            tmux_path: Some("/opt/tmux".into()),
            service_was_on: true,
            migration: Some(MigrationRecord {
                operation_id: "id".into(),
                body: "{\"a\":1}".into(),
                caller_id: "sha256:ab".into(),
            }),
            ..TakeoverState::default()
        };
        save(temp.path(), &state).unwrap();
        assert_eq!(load(temp.path()).present(), Some(&state));
    }

    #[test]
    fn a_corrupt_record_is_a_hard_stop_that_names_its_path() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(state_path(temp.path()), b"{\"phase\":").unwrap();
        let stored = load(temp.path());
        assert_eq!(stored, Stored::Unusable(state_path(temp.path())));
        assert!(stored.uploads_gated());
    }

    #[test]
    fn uploads_wait_only_until_continuity_settles() {
        let mut state = TakeoverState::default();
        assert!(state.uploads_gated());
        state.continuity = Some(Continuity::Adopted);
        assert!(!state.uploads_gated());
    }

    struct FailAt(DurableWriteStage);

    impl DurableWriteFault for FailAt {
        fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
            if stage == self.0 {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn a_write_faulted_at_any_stage_leaves_a_readable_record() {
        for stage in [
            DurableWriteStage::Create,
            DurableWriteStage::Write,
            DurableWriteStage::Fsync,
            DurableWriteStage::Rename,
            DurableWriteStage::DirSync,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let old = TakeoverState::default();
            save(temp.path(), &old).unwrap();
            let new = TakeoverState {
                phase: Phase::Recovered,
                ..TakeoverState::default()
            };
            let _ = save_with_fault(temp.path(), &new, &FailAt(stage));
            let stored = load(temp.path());
            let read = stored.present().expect("still readable");
            assert!(read == &old || read == &new, "{stage:?}");
        }
    }

    #[test]
    fn journals_compare_on_the_pinned_ca_when_both_are_readable() {
        assert!(
            identity(Some("j1"), "rewritten", "aa").same_journal(&identity(
                Some("j1"),
                "original",
                "bb"
            ))
        );
        assert!(!identity(Some("j1"), "x", "aa").same_journal(&identity(Some("j2"), "x", "aa")));
        assert!(identity(None, "x", "aa").same_journal(&identity(Some("j2"), "x", "aa")));
        assert!(!identity(None, "x", "aa").same_journal(&identity(None, "y", "aa")));
    }
}
