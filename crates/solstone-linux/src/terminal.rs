// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Terminal capture as one of this app's sources.
//!
//! The `solstone-terminal` engine runs in-process on its own thread and runtime
//! ([`supervisor`]) while the owner has `terminal` on and capture is not paused.
//! Its segments live under `captures/<day>/_tmux/`, each finished one beside its
//! `<name>.zone.json`, and sync delivers them as the `tmux` source ([`layout`]).
//! The desktop session never gates it: only pause and the owner's switch do.

pub mod control;
#[cfg(all(test, feature = "full-tests"))]
pub(crate) mod fake_tmux;
pub mod layout;
pub mod legacy_find;
pub mod settings;
pub mod state;
pub mod supervisor;
pub mod takeover;
pub mod takeover_continuity;
pub mod takeover_runner;
pub mod takeover_state;
pub mod tmux_path;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub use solstone_terminal::SyncActivity;

/// The local stream folder that holds terminal segments. A stream name must start
/// with a lowercase letter or digit, so this can never collide with one.
pub const STREAM_DIR: &str = "_tmux";
/// The journal-side source name for terminal segments.
pub const SOURCE: &str = "tmux";
/// The engine's instance lock, in the app's state directory.
pub const ENGINE_LOCK_FILENAME: &str = "terminal-engine.lock";

/// Whether a capture segment directory is a terminal segment.
pub fn is_terminal_segment(segment_dir: &Path) -> bool {
    segment_dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == STREAM_DIR)
}

/// `captures/<day>/_tmux`, the engine's day directory.
pub fn day_directory(captures_dir: &Path, day: &str) -> PathBuf {
    captures_dir.join(day).join(STREAM_DIR)
}

/// The engine's layout over this app's captures root.
pub fn engine_layout(captures_dir: PathBuf) -> solstone_terminal::LayoutFn {
    Arc::new(move |day: &str| day_directory(&captures_dir, day))
}

/// The engine's lock path in the app's state directory.
pub fn engine_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(ENGINE_LOCK_FILENAME)
}

/// What sync shares with the terminal source: whether terminal capture is on
/// (today's `_tmux` folder is the engine's while it is), and the latest sync
/// activity, which drives the engine's ☼.
#[derive(Clone)]
pub struct TerminalSync {
    enabled: Arc<AtomicBool>,
    activity: Arc<tokio::sync::watch::Sender<SyncActivity>>,
}

impl Default for TerminalSync {
    fn default() -> Self {
        Self::new(false).0
    }
}

impl TerminalSync {
    pub fn new(enabled: bool) -> (Self, tokio::sync::watch::Receiver<SyncActivity>) {
        let (sender, receiver) = tokio::sync::watch::channel(SyncActivity::Idle);
        (
            Self {
                enabled: Arc::new(AtomicBool::new(enabled)),
                activity: Arc::new(sender),
            },
            receiver,
        )
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub fn set_activity(&self, activity: SyncActivity) {
        self.activity.send_replace(activity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_segments_are_the_tmux_folder_only() {
        assert!(is_terminal_segment(Path::new(
            "/x/captures/20261108/_tmux/101500_300"
        )));
        assert!(!is_terminal_segment(Path::new(
            "/x/captures/20261108/_browser/101500_300"
        )));
        assert!(!is_terminal_segment(Path::new(
            "/x/captures/20261108/archon/101500_300"
        )));
        let layout = engine_layout(PathBuf::from("/x/captures"));
        assert_eq!(
            layout("20261108"),
            PathBuf::from("/x/captures/20261108/_tmux")
        );
    }
}
