// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Runs a consented move inside the daemon, on its own thread: stopping the old
//! service can take its whole stop timeout, and nothing here may stall the thread
//! that feeds the systemd watchdog.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use super::legacy_find::LegacyRootSources;
use super::settings::TMUX_PATH_KEY;
use super::takeover::{self, StepError, TakeoverHost};
use super::takeover_continuity;
use super::takeover_state::{self, Phase, Stored, TakeoverState};
use crate::config::{ConfigPaths, set_guided_field};
use crate::session_env::Runner;
use crate::window_control::LiveSettings;

pub const RETRY_INTERVAL: Duration = Duration::from_secs(60);

pub type SourceFinder = Arc<dyn Fn(&dyn Runner) -> LegacyRootSources + Send + Sync>;

pub struct RunnerParts {
    pub state_dir: PathBuf,
    pub paths: ConfigPaths,
    pub live: Arc<dyn LiveSettings>,
    pub runner: Arc<dyn Runner + Send + Sync>,
    pub sources: SourceFinder,
    pub wake_sync: Arc<dyn Fn() + Send + Sync>,
    pub stop_timeout: Duration,
    pub retry_interval: Duration,
}

enum Signal {
    Wake,
    Stop,
}

pub struct TakeoverRunner {
    signals: Sender<Signal>,
}

impl TakeoverRunner {
    pub fn spawn(parts: RunnerParts) -> std::io::Result<Self> {
        let (signals, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("solstone-takeover".into())
            .spawn(move || {
                loop {
                    advance(&parts);
                    match receiver.recv_timeout(parts.retry_interval) {
                        Ok(Signal::Wake) | Err(RecvTimeoutError::Timeout) => {}
                        Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            })?;
        Ok(Self { signals })
    }

    pub fn wake(&self) -> impl Fn() + Send + Sync + use<> {
        let signals = std::sync::Mutex::new(self.signals.clone());
        move || {
            if let Ok(signals) = signals.lock() {
                let _ = signals.send(Signal::Wake);
            }
        }
    }

    /// Asks it to end, without waiting: a stop of the old service in progress can
    /// outlast this app's own stop timeout, and every step resumes at the next start.
    pub fn stop(self) {
        let _ = self.signals.send(Signal::Stop);
    }
}

/// Moves the takeover as far as it can go now. Safe to call at any time.
pub fn advance(parts: &RunnerParts) {
    let Stored::Present(state) = takeover_state::load(&parts.state_dir) else {
        return;
    };
    let mut working = *state;
    let runner: &dyn Runner = &*parts.runner;
    let host = TakeoverHost {
        runner,
        sources: (parts.sources)(runner),
        read_environ: Box::new(takeover::read_process_environ),
        stop_timeout: parts.stop_timeout,
    };
    if working.phase == Phase::Consented && !stop(parts, &host, &mut working) {
        return;
    }
    if !recover(parts, &mut working) {
        return;
    }
    if working.phase < Phase::Recovered {
        return;
    }
    if !working.settings_applied && !apply_settings(parts, &working) {
        return;
    }
    if !working.terminal_settled && !settle_terminal(parts, &working) {
        return;
    }
    settle_locally(parts);
    let _ = takeover_state::update(&parts.state_dir, TakeoverState::finish_if_complete);
    parts.live.recheck_terminal();
    (parts.wake_sync)();
}

/// Sync has nothing to run on until a pairing exists, so settling that needs no
/// journal happens here.
fn settle_locally(parts: &RunnerParts) {
    let Stored::Present(state) = takeover_state::load(&parts.state_dir) else {
        return;
    };
    let config_root = crate::config::resolve_config_paths(&parts.paths).config_dir;
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(takeover_continuity::settle_locally(
        &parts.state_dir,
        &config_root,
        &state,
    ));
    if let Stored::Present(state) = takeover_state::load(&parts.state_dir) {
        takeover_continuity::tidy_settled(&state);
    }
}

fn note_failure(parts: &RunnerParts, text: String) {
    tracing::warn!(%text, "the move from solstone for tmux is waiting");
    let _ = takeover_state::update(&parts.state_dir, |state| state.last_answer = Some(text));
}

fn stop(parts: &RunnerParts, host: &TakeoverHost<'_>, working: &mut TakeoverState) -> bool {
    match takeover::stop_for_good(host, working) {
        Ok(()) => {
            let phase = working.phase;
            takeover_state::update(&parts.state_dir, |state| {
                state.phase = phase;
                state.last_answer = None;
            })
            .is_ok()
        }
        Err(StepError::Temporary(text) | StepError::Refused(text)) => {
            note_failure(parts, text);
            false
        }
    }
}

fn recover(parts: &RunnerParts, working: &mut TakeoverState) -> bool {
    crate::source_intake::carry_tmux_roots(&parts.state_dir, &working.data_roots);
    let unfinished = working.data_roots.iter().any(|root| {
        !working
            .roots
            .get(root)
            .is_some_and(|record| record.recovered)
    });
    if !unfinished && working.phase >= Phase::Recovered {
        return true;
    }
    match takeover::recover_roots(working) {
        Ok(()) => {
            let (phase, roots) = (working.phase, working.roots.clone());
            takeover_state::update(&parts.state_dir, |state| {
                state.phase = state.phase.max(phase);
                state.last_answer = None;
                for (root, record) in roots {
                    let stored = state.roots.entry(root).or_default();
                    let waiting = stored.waiting;
                    *stored = record;
                    stored.waiting = waiting;
                }
            })
            .is_ok()
        }
        Err(StepError::Temporary(text) | StepError::Refused(text)) => {
            note_failure(parts, text);
            false
        }
    }
}

/// The owner's old capture choices, with the tmux they used if it is still there.
fn carried_settings(state: &TakeoverState) -> Option<Value> {
    let mut settings = state
        .carried_settings
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(path) = state
        .tmux_path
        .as_deref()
        .filter(|path| super::tmux_path::is_executable_file(std::path::Path::new(path)))
    {
        settings.insert(TMUX_PATH_KEY.to_owned(), Value::String(path.to_owned()));
    }
    (!settings.is_empty()).then_some(Value::Object(settings))
}

fn apply_settings(parts: &RunnerParts, working: &TakeoverState) -> bool {
    if let Some(settings) = carried_settings(working) {
        if let Err(error) = set_guided_field(&parts.paths, "terminal_settings", settings.clone()) {
            note_failure(
                parts,
                format!("could not save the terminal settings: {error}"),
            );
            return false;
        }
        parts.live.terminal_settings(settings);
    }
    takeover_state::update(&parts.state_dir, |state| state.settings_applied = true).is_ok()
}

fn settle_terminal(parts: &RunnerParts, working: &TakeoverState) -> bool {
    if working.service_was_on || working.owner_turned_on {
        if let Err(error) = set_guided_field(&parts.paths, "terminal", Value::Bool(true)) {
            note_failure(parts, format!("could not turn terminal on: {error}"));
            return false;
        }
        parts.live.terminal(true);
    }
    takeover_state::update(&parts.state_dir, |state| state.terminal_settled = true).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_env::Output;
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;

    struct NoManager;

    impl Runner for NoManager {
        fn which(&self, _: &str) -> Option<String> {
            None
        }
        fn run(
            &self,
            _: &str,
            _: &[&str],
            _: Duration,
            _: &HashMap<String, String>,
        ) -> io::Result<Output> {
            Err(io::Error::other("no systemctl"))
        }
    }

    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl LiveSettings for Recording {
        fn terminal(&self, enabled: bool) {
            self.0.lock().unwrap().push(format!("terminal {enabled}"));
        }
        fn terminal_settings(&self, settings: Value) {
            self.0.lock().unwrap().push(format!("settings {settings}"));
        }
        fn recheck_terminal(&self) {
            self.0.lock().unwrap().push("recheck".to_owned());
        }
        fn desktop_capture(&self, _: bool) {}
        fn pause(&self, _: Option<u64>) {}
        fn resume(&self) {}
    }

    fn parts(root: &Path, live: Arc<Recording>) -> RunnerParts {
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        RunnerParts {
            state_dir,
            paths: ConfigPaths {
                base_dir: Some(root.join("data")),
                config_dir: Some(root.join("config")),
            },
            live,
            runner: Arc::new(NoManager),
            sources: Arc::new(|_| LegacyRootSources::default()),
            wake_sync: Arc::new(|| {}),
            stop_timeout: Duration::from_secs(1),
            retry_interval: Duration::from_secs(60),
        }
    }

    fn saved(parts: &RunnerParts) -> crate::config::Config {
        crate::config::load_config(parts.paths.clone()).config
    }

    fn stored(parts: &RunnerParts) -> TakeoverState {
        *match takeover_state::load(&parts.state_dir) {
            Stored::Present(state) => state,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn with_nothing_recorded_it_does_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live.clone());
        advance(&parts);
        assert!(live.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_choices_land_before_terminal_turns_on() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live.clone());
        let state = TakeoverState {
            service_was_on: true,
            carried_settings: Some(serde_json::json!({"segment_interval": 600})),
            ..TakeoverState::default()
        };
        takeover_state::save(&parts.state_dir, &state).unwrap();
        advance(&parts);
        let config = saved(&parts);
        assert!(config.terminal);
        assert_eq!(
            config.terminal_settings.as_ref().unwrap()["segment_interval"],
            600
        );
        let calls = live.0.lock().unwrap().clone();
        let settings = calls
            .iter()
            .position(|c| c.starts_with("settings"))
            .unwrap();
        let on = calls.iter().position(|c| c == "terminal true").unwrap();
        assert!(settings < on, "{calls:?}");
        assert!(calls.iter().any(|call| call == "recheck"));
        let after = stored(&parts);
        assert!(after.settings_applied && after.terminal_settled);
        assert_eq!(
            after.phase,
            Phase::Done,
            "no old pairing leaves nothing to wait for"
        );
    }

    #[test]
    fn an_owner_who_had_stopped_the_old_service_stays_off() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live.clone());
        takeover_state::save(&parts.state_dir, &TakeoverState::default()).unwrap();
        advance(&parts);
        assert!(!saved(&parts).terminal);
        assert!(stored(&parts).terminal_settled);
        assert!(!live.0.lock().unwrap().iter().any(|c| c == "terminal true"));
    }

    #[test]
    fn an_owner_who_turned_terminal_on_gets_it_on_even_if_the_old_service_was_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live.clone());
        let state = TakeoverState {
            owner_turned_on: true,
            ..TakeoverState::default()
        };
        takeover_state::save(&parts.state_dir, &state).unwrap();
        advance(&parts);
        assert!(saved(&parts).terminal);
        assert!(live.0.lock().unwrap().iter().any(|c| c == "terminal true"));
    }

    #[test]
    fn it_finishes_once_continuity_has_settled_and_does_not_run_steps_twice() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live.clone());
        let state = TakeoverState {
            service_was_on: true,
            continuity: Some(super::super::takeover_state::Continuity::Adopted),
            ..TakeoverState::default()
        };
        takeover_state::save(&parts.state_dir, &state).unwrap();
        advance(&parts);
        assert_eq!(stored(&parts).phase, Phase::Done);
        let first = live
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "terminal true")
            .count();
        advance(&parts);
        let second = live
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "terminal true")
            .count();
        assert_eq!((first, second), (1, 1));
    }

    #[test]
    fn an_unpaired_app_takes_on_the_old_pairing_with_no_link_to_the_journal() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live);
        let legacy = temp.path().join("old-config");
        fs::create_dir_all(&legacy).unwrap();
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::new(Vec::<String>::new())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let credential = spl_transport::credential::Credential {
            client_cert_pem: cert.pem(),
            client_key_pem: key.serialize_pem(),
            ..crate::journal_mark::sample_credential("old", "unused")
        };
        fs::write(
            legacy.join(takeover_continuity::LEGACY_CREDENTIALS_FILENAME),
            serde_json::to_vec(&credential).unwrap(),
        )
        .unwrap();
        let state = TakeoverState {
            phase: Phase::Recovered,
            config_roots: vec![legacy.clone()],
            ..TakeoverState::default()
        };
        takeover_state::save(&parts.state_dir, &state).unwrap();
        advance(&parts);
        let config_root = crate::config::resolve_config_paths(&parts.paths).config_dir;
        assert_eq!(
            crate::private_link::load_credential(&config_root).unwrap(),
            Some(credential)
        );
        assert_eq!(
            stored(&parts).continuity,
            Some(takeover_state::Continuity::Adopted)
        );
        assert_eq!(stored(&parts).phase, Phase::Done);
        assert!(
            !legacy
                .join(takeover_continuity::LEGACY_CREDENTIALS_FILENAME)
                .exists()
        );
    }

    #[test]
    fn a_problem_root_is_retried_after_done_and_clears() {
        let temp = tempfile::tempdir().unwrap();
        let live = Arc::new(Recording::default());
        let parts = parts(temp.path(), live);
        let root = temp.path().join("old");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("captures"), "not a directory").unwrap();
        let state = TakeoverState {
            phase: Phase::Done,
            data_roots: vec![root.clone()],
            settings_applied: true,
            terminal_settled: true,
            ..TakeoverState::default()
        };
        takeover_state::save(&parts.state_dir, &state).unwrap();
        advance(&parts);
        assert!(stored(&parts).roots[&root].problem.is_some());
        fs::remove_file(root.join("captures")).unwrap();
        fs::create_dir_all(root.join("captures")).unwrap();
        advance(&parts);
        let record = stored(&parts).roots[&root].clone();
        assert!(record.recovered && record.problem.is_none());
        assert_eq!(stored(&parts).phase, Phase::Done);
    }
}
