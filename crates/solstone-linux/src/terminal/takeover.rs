// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Moving an existing solstone-tmux install into this app.
//!
//! The steps here are the ones either runner may perform while it holds this
//! app's capture-root lock: record what the old install is, stop it for good, and
//! recover its spool in place. Each is safe to run again, and the caller saves the
//! record after each one.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};
use solstone_terminal::legacy::{self, LegacyError};

use super::legacy_find::{self, LegacyFindings, LegacyRootSources, UNIT_NAME};
use super::settings::TerminalSettings;
use super::takeover_state::{Phase, RootRecord, TakeoverState};
use crate::session_env::Runner;

const OWNERSHIP_MARKER: &str = "Description=Solstone Tmux Observer";
const LOCAL_OBSERVER_FILENAME: &str = "local-observer.json";
const LEGACY_CONFIG_FILENAME: &str = "config.json";
const WANTS_DIR: &str = "default.target.wants";
/// The old unit's stop can take systemd's whole stop timeout.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(120);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);
const CARRIED_KEYS: [&str; 7] = [
    "capture_interval",
    "segment_interval",
    "output_sessions",
    "status_indicator",
    "pane_title",
    "working_directory",
    "executable",
];

/// Why a step stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepError {
    /// Safe to retry: the old service would not stop, or its lock is held.
    Temporary(String),
    /// Nothing has changed, and retrying alone won't help.
    Refused(String),
}

/// What the steps need from the computer.
pub struct TakeoverHost<'a> {
    pub runner: &'a dyn Runner,
    pub sources: LegacyRootSources,
    pub read_environ: Box<dyn Fn(u32) -> Option<HashMap<String, String>> + 'a>,
    pub stop_timeout: Duration,
}

impl<'a> TakeoverHost<'a> {
    pub fn new(runner: &'a dyn Runner, sources: LegacyRootSources) -> Self {
        Self {
            runner,
            sources,
            read_environ: Box::new(read_process_environ),
            stop_timeout: STOP_TIMEOUT,
        }
    }

    fn systemctl(&self) -> Option<String> {
        self.runner.which("systemctl")
    }

    fn user_systemctl(&self, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
        let systemctl = self.systemctl()?;
        let mut full = vec!["--user"];
        full.extend_from_slice(args);
        let output = self
            .runner
            .run(&systemctl, &full, timeout, &HashMap::new())
            .ok()?;
        Some((output.success, output.stdout))
    }

    fn has_user_manager(&self) -> bool {
        self.user_systemctl(&["show-environment"], SYSTEMCTL_TIMEOUT)
            .is_some_and(|(success, _)| success)
    }

    /// The running unit's own environment comes first: it names the roots the
    /// service actually wrote to.
    fn sources_with_unit_environment(&self) -> LegacyRootSources {
        let mut sources = self.sources.clone();
        let pid = self
            .user_systemctl(
                &["show", "-p", "MainPID", "--value", UNIT_NAME],
                SYSTEMCTL_TIMEOUT,
            )
            .filter(|(success, _)| *success)
            .and_then(|(_, stdout)| stdout.trim().parse::<u32>().ok())
            .filter(|pid| *pid != 0);
        if let Some(environment) = pid.and_then(|pid| (self.read_environ)(pid)) {
            sources.environments.insert(0, environment);
        }
        sources
    }
}

pub(crate) fn read_process_environ(pid: u32) -> Option<HashMap<String, String>> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        bytes
            .split(|byte| *byte == 0)
            .filter_map(|entry| {
                let text = std::str::from_utf8(entry).ok()?;
                let (name, value) = text.split_once('=')?;
                Some((name.to_owned(), value.to_owned()))
            })
            .collect(),
    )
}

/// Canonical spellings, one per directory, in the order given.
fn canonical_distinct(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut kept = Vec::new();
    for path in paths {
        let Ok(canonical) = fs::canonicalize(path) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&canonical) else {
            continue;
        };
        if seen.insert((metadata.dev(), metadata.ino())) {
            kept.push(canonical);
        }
    }
    kept
}

fn unit_is_ours(unit: &Path) -> bool {
    fs::read_to_string(unit)
        .is_ok_and(|text| text.lines().any(|line| line.trim() == OWNERSHIP_MARKER))
}

fn refuse_foreign_units(findings: &LegacyFindings) -> Result<(), StepError> {
    match findings.units.iter().find(|unit| !unit_is_ours(unit)) {
        Some(unit) => Err(StepError::Refused(format!(
            "{} is not the solstone for tmux service, so nothing was changed.",
            unit.display()
        ))),
        None => Ok(()),
    }
}

fn read_tmux_path(config_roots: &[PathBuf]) -> Option<String> {
    config_roots.iter().find_map(|root| {
        let bytes = fs::read(root.join(LOCAL_OBSERVER_FILENAME)).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        value.get("tmux_path")?.as_str().map(str::to_owned)
    })
}

/// The old app's capture choices that this app understands, key by key: an
/// unknown key is ignored and a bad value leaves that key at its default.
fn read_carried_settings(config_roots: &[PathBuf]) -> Option<Value> {
    let bytes = config_roots
        .iter()
        .find_map(|root| fs::read(root.join(LEGACY_CONFIG_FILENAME)).ok())?;
    let saved: Map<String, Value> = serde_json::from_slice(&bytes).ok()?;
    let carried: Map<String, Value> = CARRIED_KEYS
        .iter()
        .filter_map(|key| {
            let value = saved.get(*key)?;
            let one = json!({ *key: value });
            TerminalSettings::from_saved(Some(&one))
                .ignored
                .is_empty()
                .then(|| ((*key).to_owned(), value.clone()))
        })
        .collect();
    Some(Value::Object(carried))
}

fn service_was_on(host: &TakeoverHost<'_>, findings: &LegacyFindings) -> bool {
    let answered = |verb: &str, expected: &str| {
        host.user_systemctl(&[verb, UNIT_NAME], SYSTEMCTL_TIMEOUT)
            .is_some_and(|(_, stdout)| stdout.trim() == expected)
    };
    if answered("is-enabled", "enabled") || answered("is-active", "active") {
        return true;
    }
    // With no manager to ask, the enablement link says the same thing.
    !host.has_user_manager()
        && findings.units.iter().any(|unit| {
            unit.parent()
                .is_some_and(|dir| dir.join(WANTS_DIR).join(UNIT_NAME).exists())
        })
}

/// Step 1: what the old install is, read before anything is changed.
pub fn prepare(host: &TakeoverHost<'_>) -> Result<TakeoverState, StepError> {
    let sources = host.sources_with_unit_environment();
    let findings = legacy_find::find(&sources);
    refuse_foreign_units(&findings)?;
    let config_roots = canonical_distinct(&findings.config_roots);
    Ok(TakeoverState {
        phase: Phase::Consented,
        data_roots: canonical_distinct(&findings.data_roots),
        tmux_path: read_tmux_path(&config_roots),
        service_was_on: service_was_on(host, &findings),
        carried_settings: read_carried_settings(&config_roots),
        config_roots,
        ..TakeoverState::default()
    })
}

fn remove_if_present(path: &Path) -> Result<(), StepError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StepError::Temporary(format!(
            "could not remove {}: {error}",
            path.display()
        ))),
    }
}

fn require_free_locks(data_roots: &[PathBuf]) -> Result<(), StepError> {
    for root in data_roots {
        if let Err(LegacyError::StillRunning(_)) = legacy::open(root).map(drop) {
            return Err(StepError::Temporary(
                "solstone for tmux is still running.".to_owned(),
            ));
        }
    }
    Ok(())
}

/// Step 2: the old service stops and never comes back.
pub fn stop_for_good(host: &TakeoverHost<'_>, state: &mut TakeoverState) -> Result<(), StepError> {
    let findings = legacy_find::find(&host.sources_with_unit_environment());
    refuse_foreign_units(&findings)?;
    let manager = host.has_user_manager();
    if manager {
        if !findings.units.is_empty() {
            let stopped = host
                .user_systemctl(&["disable", "--now", UNIT_NAME], host.stop_timeout)
                .is_some_and(|(success, _)| success);
            if !stopped {
                return Err(StepError::Temporary(
                    "solstone for tmux would not stop.".to_owned(),
                ));
            }
        }
        let still_up = host
            .user_systemctl(&["is-active", UNIT_NAME], SYSTEMCTL_TIMEOUT)
            .is_some_and(|(_, stdout)| {
                matches!(stdout.trim(), "active" | "activating" | "deactivating")
            });
        if still_up {
            return Err(StepError::Temporary(
                "solstone for tmux is still stopping.".to_owned(),
            ));
        }
    } else {
        require_free_locks(&state.data_roots)?;
    }
    for unit in &findings.units {
        if let Some(dir) = unit.parent() {
            remove_if_present(&dir.join(WANTS_DIR).join(UNIT_NAME))?;
        }
        remove_if_present(unit)?;
    }
    if manager {
        let _ = host.user_systemctl(&["daemon-reload"], SYSTEMCTL_TIMEOUT);
    }
    for root in &state.config_roots {
        remove_if_present(&root.join(LOCAL_OBSERVER_FILENAME))?;
    }
    state.phase = state.phase.max(Phase::Stopped);
    Ok(())
}

/// Step 3: finish what the old app left open, in place. A root that can't be used
/// is recorded and the others continue; a held lock stops the whole step.
pub fn recover_roots(state: &mut TakeoverState) -> Result<(), StepError> {
    let interval = recovery_interval(state);
    for root in state.data_roots.clone() {
        let record = state.roots.entry(root.clone()).or_default();
        if record.recovered {
            continue;
        }
        let outcome = legacy::open(&root).and_then(|guard| guard.recover(interval));
        match outcome {
            Ok(recovery) => {
                *record = RootRecord {
                    recovered: true,
                    problem: None,
                    held: recovery.held.len(),
                    problem_paths: recovery
                        .problems
                        .into_iter()
                        .map(|problem| problem.path)
                        .collect(),
                    waiting: 0,
                };
            }
            Err(LegacyError::StillRunning(_)) => {
                return Err(StepError::Temporary(
                    "solstone for tmux is still running.".to_owned(),
                ));
            }
            Err(error) => {
                record.problem = Some(error.to_string());
            }
        }
    }
    state.phase = state.phase.max(Phase::Recovered);
    Ok(())
}

fn recovery_interval(state: &TakeoverState) -> u64 {
    let settings = TerminalSettings::from_saved(state.carried_settings.as_ref());
    settings.capture.segment_interval
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_env::Output;
    use std::io;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSystemctl {
        calls: Mutex<Vec<String>>,
        no_manager: bool,
        stuck: bool,
        enabled: bool,
    }

    impl FakeSystemctl {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for FakeSystemctl {
        fn which(&self, _: &str) -> Option<String> {
            Some("systemctl".to_owned())
        }

        fn run(
            &self,
            _: &str,
            args: &[&str],
            _: Duration,
            _: &HashMap<String, String>,
        ) -> io::Result<Output> {
            let joined = args[1..].join(" ");
            self.calls.lock().unwrap().push(joined.clone());
            if self.no_manager {
                return Err(io::Error::other("no manager"));
            }
            let (success, stdout) = match args[1] {
                "disable" => (!self.stuck, ""),
                "is-active" => (false, if self.stuck { "active" } else { "inactive" }),
                "is-enabled" if self.enabled => (true, "enabled"),
                "is-enabled" => (false, "disabled"),
                "show" => (true, "0"),
                _ => (true, ""),
            };
            Ok(Output {
                success,
                stdout: format!("{stdout}\n"),
            })
        }
    }

    struct Install {
        _temp: tempfile::TempDir,
        home: PathBuf,
    }

    impl Install {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = fs::canonicalize(temp.path()).unwrap();
            let unit_dir = home.join(".config/systemd/user");
            fs::create_dir_all(unit_dir.join(WANTS_DIR)).unwrap();
            fs::write(
                unit_dir.join(UNIT_NAME),
                format!("[Unit]\n{OWNERSHIP_MARKER}\n"),
            )
            .unwrap();
            fs::write(unit_dir.join(WANTS_DIR).join(UNIT_NAME), "").unwrap();
            fs::create_dir_all(home.join(".config/solstone-tmux")).unwrap();
            fs::create_dir_all(home.join(".local/share/solstone-tmux/captures")).unwrap();
            Self { _temp: temp, home }
        }

        fn config_root(&self) -> PathBuf {
            self.home.join(".config/solstone-tmux")
        }

        fn unit(&self) -> PathBuf {
            self.home.join(".config/systemd/user").join(UNIT_NAME)
        }

        fn link(&self) -> PathBuf {
            self.home
                .join(".config/systemd/user")
                .join(WANTS_DIR)
                .join(UNIT_NAME)
        }

        fn host<'a>(&self, runner: &'a dyn Runner) -> TakeoverHost<'a> {
            TakeoverHost {
                runner,
                sources: LegacyRootSources {
                    environments: Vec::new(),
                    home: self.home.clone(),
                },
                read_environ: Box::new(|_| None),
                stop_timeout: Duration::from_secs(1),
            }
        }
    }

    #[test]
    fn the_details_are_read_before_anything_changes() {
        let install = Install::new();
        fs::write(
            install.config_root().join(LOCAL_OBSERVER_FILENAME),
            r#"{"tmux_path":"/opt/tmux/bin/tmux"}"#,
        )
        .unwrap();
        let runner = FakeSystemctl {
            enabled: true,
            ..FakeSystemctl::default()
        };
        let state = prepare(&install.host(&runner)).unwrap();
        assert_eq!(state.tmux_path.as_deref(), Some("/opt/tmux/bin/tmux"));
        assert!(state.service_was_on);
        assert_eq!(state.config_roots, [install.config_root()]);
        assert!(install.unit().exists());
        assert!(install.config_root().join(LOCAL_OBSERVER_FILENAME).exists());
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.starts_with("disable"))
        );
    }

    #[test]
    fn an_old_config_is_read_key_by_key() {
        let install = Install::new();
        fs::write(
            install.config_root().join(LEGACY_CONFIG_FILENAME),
            r#"{"stream":null,"cache_retention_days":7,"segment_interval":600,
                "capture_interval":0,"output_sessions":["work"],"pane_title":false,
                "working_directory":false,"executable":false,"future_key":1}"#,
        )
        .unwrap();
        let runner = FakeSystemctl::default();
        let state = prepare(&install.host(&runner)).unwrap();
        let carried = state.carried_settings.as_ref().unwrap();
        assert_eq!(carried["segment_interval"], 600);
        assert_eq!(carried["pane_title"], false);
        assert!(carried.get("capture_interval").is_none(), "a bad value");
        assert!(carried.get("stream").is_none());
        assert!(carried.get("future_key").is_none());
        assert_eq!(recovery_interval(&state), 600);
    }

    #[test]
    fn a_stopped_service_is_remembered_as_off() {
        let install = Install::new();
        fs::remove_file(install.link()).unwrap();
        let runner = FakeSystemctl::default();
        assert!(!prepare(&install.host(&runner)).unwrap().service_was_on);
    }

    #[test]
    fn a_unit_that_is_not_ours_changes_nothing() {
        let install = Install::new();
        fs::write(install.unit(), "[Unit]\nDescription=Someone else\n").unwrap();
        let runner = FakeSystemctl::default();
        assert!(matches!(
            prepare(&install.host(&runner)),
            Err(StepError::Refused(_))
        ));
        let mut state = TakeoverState::default();
        assert!(matches!(
            stop_for_good(&install.host(&runner), &mut state),
            Err(StepError::Refused(_))
        ));
        assert!(install.unit().exists());
        assert!(install.link().exists());
        assert!(
            !runner
                .calls()
                .iter()
                .any(|call| call.starts_with("disable"))
        );
    }

    #[test]
    fn stopping_disables_removes_and_reloads() {
        let install = Install::new();
        fs::write(install.config_root().join(LOCAL_OBSERVER_FILENAME), "{}").unwrap();
        let runner = FakeSystemctl::default();
        let host = install.host(&runner);
        let mut state = prepare(&host).unwrap();
        stop_for_good(&host, &mut state).unwrap();
        let calls = runner.calls();
        assert!(calls.contains(&format!("disable --now {UNIT_NAME}")));
        assert!(calls.contains(&"daemon-reload".to_owned()));
        assert!(!install.unit().exists());
        assert!(!install.link().exists());
        assert!(!install.config_root().join(LOCAL_OBSERVER_FILENAME).exists());
        assert_eq!(state.phase, Phase::Stopped);
    }

    #[test]
    fn with_no_manager_and_a_free_lock_the_unit_is_still_removed() {
        let install = Install::new();
        let runner = FakeSystemctl {
            no_manager: true,
            ..FakeSystemctl::default()
        };
        let host = install.host(&runner);
        let mut state = prepare(&host).unwrap();
        stop_for_good(&host, &mut state).unwrap();
        assert!(!install.unit().exists());
        assert!(!install.link().exists());
    }

    #[test]
    fn a_service_that_will_not_stop_is_temporary_and_keeps_its_unit() {
        let install = Install::new();
        let runner = FakeSystemctl {
            stuck: true,
            ..FakeSystemctl::default()
        };
        let host = install.host(&runner);
        let mut state = prepare(&host).unwrap();
        assert!(matches!(
            stop_for_good(&host, &mut state),
            Err(StepError::Temporary(_))
        ));
        assert!(install.unit().exists());
        assert_eq!(state.phase, Phase::Consented);
    }

    #[test]
    fn one_unusable_root_is_recorded_and_the_rest_continue() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good");
        fs::create_dir_all(good.join("captures")).unwrap();
        let bad = temp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("captures"), "not a directory").unwrap();
        let mut state = TakeoverState {
            data_roots: vec![bad.clone(), good.clone()],
            ..TakeoverState::default()
        };
        recover_roots(&mut state).unwrap();
        assert!(state.roots[&good].recovered);
        assert!(!state.roots[&bad].recovered);
        assert!(state.roots[&bad].problem.is_some());
        assert_eq!(state.phase, Phase::Recovered);
    }

    #[test]
    fn a_root_already_recovered_is_not_opened_again() {
        let temp = tempfile::tempdir().unwrap();
        let gone = temp.path().join("gone");
        let mut state = TakeoverState {
            data_roots: vec![gone.clone()],
            ..TakeoverState::default()
        };
        state.roots.insert(
            gone.clone(),
            RootRecord {
                recovered: true,
                ..RootRecord::default()
            },
        );
        recover_roots(&mut state).unwrap();
        assert!(state.roots[&gone].problem.is_none());
    }
}
