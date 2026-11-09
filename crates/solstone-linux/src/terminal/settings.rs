// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! `terminal_settings`, read key by key.
//!
//! The engine's `CaptureSettings` parser is strict: one unknown key or one bad
//! value fails the whole object. Here each key is checked on its own, with the
//! engine's own parser and validation, so a bad key costs only that key: it is
//! ignored with a warning naming it and takes the engine's default. The settings
//! handed to the engine therefore always validate, and a bad saved value can
//! never put the engine into a start-and-fail loop.

use std::path::PathBuf;

use serde_json::{Map, Value};
use solstone_terminal::CaptureSettings;

/// The engine's seven capture settings.
pub const CAPTURE_KEYS: [&str; 7] = [
    "capture_interval",
    "segment_interval",
    "output_sessions",
    "status_indicator",
    "pane_title",
    "working_directory",
    "executable",
];
/// The tmux the owner chose; not a capture setting.
pub const TMUX_PATH_KEY: &str = "tmux_path";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSettings {
    pub capture: CaptureSettings,
    /// An absolute path the owner chose, used while it is executable.
    pub tmux_path: Option<PathBuf>,
    /// Saved keys that were ignored, unknown or invalid, in the order saved.
    pub ignored: Vec<String>,
}

impl TerminalSettings {
    /// Reads the saved `terminal_settings` value. Absent means every default.
    pub fn from_saved(value: Option<&Value>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let Some(map) = value.as_object() else {
            return Self {
                ignored: vec!["terminal_settings".to_owned()],
                ..Self::default()
            };
        };
        let mut settings = Self::default();
        for (key, item) in map {
            if key == TMUX_PATH_KEY {
                match item.as_str().map(PathBuf::from) {
                    Some(path) if path.is_absolute() => settings.tmux_path = Some(path),
                    _ => settings.ignored.push(key.clone()),
                }
            } else if CAPTURE_KEYS.contains(&key.as_str()) {
                if !apply_capture_key(&mut settings.capture, key, item) {
                    settings.ignored.push(key.clone());
                }
            } else {
                settings.ignored.push(key.clone());
            }
        }
        debug_assert!(settings.capture.validate().is_ok());
        settings
    }

    /// One warning per ignored key, for the daemon's log.
    pub fn log_ignored(&self) {
        for key in &self.ignored {
            tracing::warn!(key = %key, "terminal_settings key ignored; its default applies");
        }
    }
}

/// Parses `{key: item}` alone with the engine's parser, which applies its own
/// validation, and copies the one field it set.
fn apply_capture_key(capture: &mut CaptureSettings, key: &str, item: &Value) -> bool {
    let mut single = Map::new();
    single.insert(key.to_owned(), item.clone());
    let Ok(parsed) = CaptureSettings::parse_json(&Value::Object(single).to_string()) else {
        return false;
    };
    match key {
        "capture_interval" => capture.capture_interval = parsed.capture_interval,
        "segment_interval" => capture.segment_interval = parsed.segment_interval,
        "output_sessions" => capture.output_sessions = parsed.output_sessions,
        "status_indicator" => capture.status_indicator = parsed.status_indicator,
        "pane_title" => capture.pane_title = parsed.pane_title,
        "working_directory" => capture.working_directory = parsed.working_directory,
        "executable" => capture.executable = parsed.executable,
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_is_every_engine_default() {
        assert_eq!(
            TerminalSettings::from_saved(None),
            TerminalSettings::default()
        );
        assert_eq!(
            TerminalSettings::from_saved(Some(&json!({}))).capture,
            CaptureSettings::default()
        );
    }

    #[test]
    fn the_terminal_interval_does_not_move_the_screen_interval() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.json"),
            json!({
                "segment_interval": 300,
                "terminal_settings": {"segment_interval": 60},
            })
            .to_string(),
        )
        .unwrap();
        let config = crate::config::load_config(crate::config::ConfigPaths {
            base_dir: Some(temp.path().join("data")),
            config_dir: Some(config_dir),
        })
        .config;
        assert_eq!(config.segment_interval, 300);
        let settings = TerminalSettings::from_saved(config.terminal_settings.as_ref());
        assert_eq!(settings.capture.segment_interval, 60);
    }

    #[test]
    fn each_bad_key_costs_only_itself() {
        let saved = json!({
            "segment_interval": 60,
            "capture_interval": 0,
            "output_sessions": [""],
            "tmux_path": "/opt/tmux/bin/tmux",
            "pane_title": false,
            "colour": "blue",
        });
        let settings = TerminalSettings::from_saved(Some(&saved));
        assert_eq!(settings.capture.segment_interval, 60);
        assert_eq!(
            settings.capture.capture_interval,
            CaptureSettings::default().capture_interval
        );
        assert!(settings.capture.output_sessions.is_empty());
        assert!(!settings.capture.pane_title);
        assert_eq!(
            settings.tmux_path,
            Some(PathBuf::from("/opt/tmux/bin/tmux"))
        );
        let mut ignored = settings.ignored.clone();
        ignored.sort();
        assert_eq!(ignored, ["capture_interval", "colour", "output_sessions"]);
        assert!(settings.capture.validate().is_ok());
    }

    #[test]
    fn relative_or_mistyped_tmux_path_is_ignored() {
        for bad in [json!("bin/tmux"), json!(""), json!(5), json!(null)] {
            let settings = TerminalSettings::from_saved(Some(&json!({ "tmux_path": bad })));
            assert_eq!(settings.tmux_path, None);
            assert_eq!(settings.ignored, ["tmux_path"]);
        }
    }

    #[test]
    fn a_wrong_type_takes_the_default_for_that_key() {
        let settings = TerminalSettings::from_saved(Some(&json!({
            "segment_interval": "sixty",
            "status_indicator": "yes",
            "output_sessions": ["main", "work*"],
        })));
        assert_eq!(
            settings.capture.segment_interval,
            CaptureSettings::default().segment_interval
        );
        assert!(settings.capture.status_indicator);
        assert_eq!(settings.capture.output_sessions, ["main", "work*"]);
        let mut ignored = settings.ignored.clone();
        ignored.sort();
        assert_eq!(ignored, ["segment_interval", "status_indicator"]);
    }

    #[test]
    fn a_non_object_is_one_ignored_name() {
        let settings = TerminalSettings::from_saved(Some(&json!([1, 2])));
        assert_eq!(settings.capture, CaptureSettings::default());
        assert_eq!(settings.ignored, ["terminal_settings"]);
    }
}
