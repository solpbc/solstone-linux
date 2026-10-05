// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The owner's pause, held across restarts.
//!
//! A pause the owner chose lasts until the owner resumes or its own deadline
//! passes. Quitting, a crash, an update, logging out or a reboot does not end
//! it, so the pause is written to the state directory when it starts, removed
//! when it ends, and read back at start-up before anything can be captured.
//! A screen lock or suspend is not an owner pause and is never written here.

use std::{fs, io, path::Path};

use serde_json::{Value, json};

use crate::config::Config;

const PAUSE_FILENAME: &str = "pause.json";

/// A pause the owner chose and has not ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeldPause {
    /// "until I resume".
    UntilResumed,
    /// A timed pause, ending at this wall-clock time (Unix seconds).
    Until(f64),
}

/// How the observer starts: paused or not, and when a timed pause ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StartingPause {
    pub paused: bool,
    /// The deadline on the monotonic clock, for the countdown and the tick.
    pub until_mono: Option<f64>,
    /// The same deadline on the wall clock, which is the one the owner chose.
    pub until_wall: Option<f64>,
}

impl StartingPause {
    /// The pause in effect at start-up: a held owner pause, or `start paused`.
    /// A timed pause whose deadline has passed is removed, and the app starts
    /// observing unless `start paused` is on.
    pub fn read(config: &Config, wall: f64, mono: f64) -> Self {
        let state_dir = config.state_dir();
        let unpaused = Self {
            paused: config.start_paused,
            until_mono: None,
            until_wall: None,
        };
        match load(&state_dir) {
            None => unpaused,
            Some(HeldPause::UntilResumed) => Self {
                paused: true,
                until_mono: None,
                until_wall: None,
            },
            Some(HeldPause::Until(deadline)) if deadline > wall => Self {
                paused: true,
                until_mono: Some(mono + (deadline - wall)),
                until_wall: Some(deadline),
            },
            Some(HeldPause::Until(_)) => {
                if let Err(error) = clear(&state_dir) {
                    tracing::warn!(%error, "Could not remove an ended pause");
                }
                unpaused
            }
        }
    }
}

fn pause_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join(PAUSE_FILENAME)
}

/// The held pause, if any. A pause file that exists but cannot be read is
/// treated as "until I resume": staying paused shows the paused mark and the
/// owner can resume, while capturing against their choice cannot be undone.
pub fn load(state_dir: &Path) -> Option<HeldPause> {
    let text = match fs::read_to_string(pause_path(state_dir)) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, "Could not read the held pause; staying paused");
            return Some(HeldPause::UntilResumed);
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => match map.get("until") {
            Some(Value::Null) => Some(HeldPause::UntilResumed),
            Some(value) => match value.as_f64() {
                Some(deadline) if deadline.is_finite() => Some(HeldPause::Until(deadline)),
                _ => {
                    tracing::warn!("The held pause has no usable deadline; staying paused");
                    Some(HeldPause::UntilResumed)
                }
            },
            None => {
                tracing::warn!("The held pause has no deadline field; staying paused");
                Some(HeldPause::UntilResumed)
            }
        },
        _ => {
            tracing::warn!("The held pause could not be parsed; staying paused");
            Some(HeldPause::UntilResumed)
        }
    }
}

/// Record the owner's pause durably before returning.
pub fn save(state_dir: &Path, pause: HeldPause) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let until = match pause {
        HeldPause::UntilResumed => Value::Null,
        HeldPause::Until(deadline) => json!(deadline),
    };
    let bytes = serde_json::to_vec(&json!({ "until": until })).map_err(io::Error::other)?;
    crate::private_file::atomic_write_bytes(&pause_path(state_dir), &bytes)
        .map_err(|error| io::Error::other(error.to_string()))
}

/// Remove the held pause. Absent is already clear.
pub fn clear(state_dir: &Path) -> io::Result<()> {
    match fs::remove_file(pause_path(state_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(base: &Path, start_paused: bool) -> Config {
        Config {
            base_dir: base.into(),
            start_paused,
            ..Config::default()
        }
    }

    #[test]
    fn nothing_held_starts_from_start_paused() {
        let temp = tempfile::tempdir().unwrap();
        for start_paused in [false, true] {
            let starting = StartingPause::read(&config(temp.path(), start_paused), 1_000.0, 5.0);
            assert_eq!(starting.paused, start_paused);
            assert_eq!(starting.until_mono, None);
        }
    }

    #[test]
    fn until_resumed_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path(), false);
        save(&config.state_dir(), HeldPause::UntilResumed).unwrap();
        assert_eq!(load(&config.state_dir()), Some(HeldPause::UntilResumed));
        let starting = StartingPause::read(&config, 1_000.0, 5.0);
        assert!(starting.paused);
        assert_eq!((starting.until_mono, starting.until_wall), (None, None));
    }

    #[test]
    fn timed_pause_keeps_its_wall_clock_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(temp.path(), false);
        save(&config.state_dir(), HeldPause::Until(1_900.0)).unwrap();
        // Restarted 600 seconds before the deadline, on a fresh monotonic clock.
        let starting = StartingPause::read(&config, 1_300.0, 7.0);
        assert!(starting.paused);
        assert_eq!(starting.until_mono, Some(607.0));
        assert_eq!(starting.until_wall, Some(1_900.0));
        assert_eq!(load(&config.state_dir()), Some(HeldPause::Until(1_900.0)));
    }

    #[test]
    fn timed_pause_past_its_deadline_observes_and_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        for start_paused in [false, true] {
            let config = config(temp.path(), start_paused);
            save(&config.state_dir(), HeldPause::Until(1_900.0)).unwrap();
            let starting = StartingPause::read(&config, 1_900.0, 7.0);
            assert_eq!(starting.paused, start_paused);
            assert_eq!(starting.until_mono, None);
            assert_eq!(load(&config.state_dir()), None);
        }
    }

    #[test]
    fn clear_removes_and_tolerates_absence() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        clear(&state_dir).unwrap();
        save(&state_dir, HeldPause::UntilResumed).unwrap();
        clear(&state_dir).unwrap();
        assert_eq!(load(&state_dir), None);
    }

    #[test]
    fn an_unreadable_pause_file_stays_paused() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        fs::create_dir_all(&state_dir).unwrap();
        for body in ["", "not json", "[]", "{}", r#"{"until":"soon"}"#] {
            fs::write(pause_path(&state_dir), body).unwrap();
            assert_eq!(load(&state_dir), Some(HeldPause::UntilResumed), "{body:?}");
        }
    }
}
