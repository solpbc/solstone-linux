// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The terminal state the daemon maintains for `terminal status`, `status` and
//! the window. Only the daemon writes it; a reader that finds no daemon running
//! says so instead of trusting what the last daemon left.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STATE_FILENAME: &str = "terminal.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    /// The owner has terminal off.
    Off,
    /// Capture is paused.
    Paused,
    Running,
    /// No usable tmux; checked again every 30 seconds.
    TmuxNotFound,
    /// A solstone-tmux install is on this computer, so the engine does not start.
    WaitingForTmuxApp,
    /// The engine failed to start or ended with an error; it restarts with backoff.
    Restarting,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalState {
    pub terminal: bool,
    pub state: EngineState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `terminal_settings` keys that were ignored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_settings: Vec<String>,
}

impl TerminalState {
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(STATE_FILENAME)
    }

    pub fn read(state_dir: &Path) -> Option<Self> {
        let bytes = fs::read(Self::path(state_dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn write(&self, state_dir: &Path) {
        let result = fs::create_dir_all(state_dir)
            .map_err(|error| error.to_string())
            .and_then(|()| serde_json::to_vec(self).map_err(|error| error.to_string()))
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                crate::private_file::atomic_write_bytes(&Self::path(state_dir), &bytes)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = result {
            tracing::warn!(%error, "Could not record the terminal state");
        }
    }

    /// One owner-readable line for the engine's state.
    pub fn engine_line(&self) -> String {
        match self.state {
            EngineState::Off => "terminal capture is off".to_owned(),
            EngineState::Paused => "paused".to_owned(),
            EngineState::Running => "running".to_owned(),
            EngineState::TmuxNotFound => "tmux not found".to_owned(),
            EngineState::WaitingForTmuxApp => {
                "waiting: the solstone tmux app is still installed and has to be moved over first"
                    .to_owned()
            }
            EngineState::Restarting => match &self.error {
                Some(error) => format!("stopped by an error, restarting: {error}"),
                None => "stopped by an error, restarting".to_owned(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_absence_and_garbage() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(TerminalState::read(temp.path()), None);
        let state = TerminalState {
            terminal: true,
            state: EngineState::Restarting,
            tmux_path: Some("/usr/bin/tmux".into()),
            error: Some("boom".into()),
            ignored_settings: vec!["capture_interval".into()],
        };
        state.write(temp.path());
        assert_eq!(TerminalState::read(temp.path()), Some(state));
        fs::write(TerminalState::path(temp.path()), b"{").unwrap();
        assert_eq!(TerminalState::read(temp.path()), None);
    }
}
