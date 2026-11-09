// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Runs the terminal engine on its own thread and runtime.
//!
//! The engine runs while the owner has terminal on, capture is not paused, no
//! solstone-tmux install is found and a usable tmux resolves. A failed start, or
//! an engine that ends with an error, is retried with capped backoff, and every
//! restart stops and awaits the old engine first. While tmux is not found, or the
//! engine reports it missing, tmux is resolved again on the re-check interval; a
//! different usable path starts or restarts the engine, the same path changes
//! nothing. The legacy find runs before every start and on every re-check.
//!
//! The engine's sinks only wake: a finished segment wakes sync, which scans the
//! layout; health and diagnostics are recorded and logged. Sync activity reaches
//! the engine through a latest-value relay, re-sent after every start because a
//! new engine starts idle.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use solstone_terminal::{
    Diagnostic, EngineConfig, EngineDependencies, EngineHandle, SyncActivity, TmuxHealth,
};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;

use super::legacy_find::LegacyFindings;
use super::settings::TerminalSettings;
use super::state::{EngineState, TerminalState};

/// How often the legacy find and a missing tmux are checked again.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(30);
pub const BACKOFF_INITIAL: Duration = Duration::from_secs(2);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// An engine that ran this long before ending starts its retries afresh.
const STABLE_RUN: Duration = Duration::from_secs(60);

pub type LegacyProbe = Arc<dyn Fn() -> LegacyFindings + Send + Sync>;
pub type DependencyFactory = Arc<dyn Fn() -> EngineDependencies + Send + Sync>;
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct SupervisorConfig {
    pub captures_dir: PathBuf,
    pub state_dir: PathBuf,
    pub enabled: bool,
    pub paused: bool,
    pub settings: TerminalSettings,
    /// The service's `PATH`, searched for tmux.
    pub path_env: Option<OsString>,
    pub legacy: LegacyProbe,
    /// Called for every finished segment the engine reports. Must not block.
    pub on_closed_segment: Wake,
    pub sync_activity: watch::Receiver<SyncActivity>,
    pub recheck_interval: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// Engine dependencies for tests; production uses the engine's own.
    pub dependencies: Option<DependencyFactory>,
}

impl SupervisorConfig {
    pub fn new(
        captures_dir: PathBuf,
        state_dir: PathBuf,
        legacy: LegacyProbe,
        on_closed_segment: Wake,
        sync_activity: watch::Receiver<SyncActivity>,
    ) -> Self {
        Self {
            captures_dir,
            state_dir,
            enabled: false,
            paused: false,
            settings: TerminalSettings::default(),
            path_env: std::env::var_os("PATH"),
            legacy,
            on_closed_segment,
            sync_activity,
            recheck_interval: RECHECK_INTERVAL,
            backoff_initial: BACKOFF_INITIAL,
            backoff_max: BACKOFF_MAX,
            dependencies: None,
        }
    }
}

enum Control {
    Enabled(bool),
    Paused(bool),
    Settings(TerminalSettings),
    Recheck,
    Shutdown,
}

/// The daemon's handle on the terminal source.
pub struct TerminalSupervisor {
    control: mpsc::UnboundedSender<Control>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TerminalSupervisor {
    pub fn spawn(config: SupervisorConfig) -> std::io::Result<Self> {
        let (control, receiver) = mpsc::unbounded_channel();
        let thread = thread::Builder::new()
            .name("solstone-terminal".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "Terminal could not start its runtime");
                        return;
                    }
                };
                runtime.block_on(Supervisor::new(config).run(receiver));
            })?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.control.send(Control::Enabled(enabled));
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.control.send(Control::Paused(paused));
    }

    /// Applies at the next engine start.
    pub fn set_settings(&self, settings: TerminalSettings) {
        let _ = self.control.send(Control::Settings(settings));
    }

    /// Runs the re-check now rather than at the next interval.
    pub fn recheck(&self) {
        let _ = self.control.send(Control::Recheck);
    }

    /// A handle that can switch terminal on and off from other threads.
    pub fn switches(&self) -> TerminalSwitches {
        TerminalSwitches {
            control: self.control.clone(),
        }
    }

    /// Stops the engine, which finalizes and reports its open segment and restores
    /// the owner's tmux options, and waits for that to finish.
    pub fn shutdown(mut self) {
        let _ = self.control.send(Control::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TerminalSupervisor {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Control::Shutdown);
            let _ = thread.join();
        }
    }
}

/// The live switches of the terminal source, shareable across threads.
#[derive(Clone)]
pub struct TerminalSwitches {
    control: mpsc::UnboundedSender<Control>,
}

impl TerminalSwitches {
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.control.send(Control::Enabled(enabled));
    }

    pub fn set_paused(&self, paused: bool) {
        let _ = self.control.send(Control::Paused(paused));
    }

    pub fn set_settings(&self, settings: TerminalSettings) {
        let _ = self.control.send(Control::Settings(settings));
    }

    pub fn recheck(&self) {
        let _ = self.control.send(Control::Recheck);
    }
}

struct RunningEngine {
    handle: EngineHandle,
    tmux_path: PathBuf,
    started_at: Instant,
}

struct HealthFlag {
    missing: AtomicBool,
    changed: Notify,
}

struct Supervisor {
    config: SupervisorConfig,
    health: Arc<HealthFlag>,
    legacy_found: bool,
    retry_at: Option<Instant>,
    failures: u32,
    last_error: Option<String>,
    state: EngineState,
    published: Option<TerminalState>,
}

impl Supervisor {
    fn new(config: SupervisorConfig) -> Self {
        Self {
            config,
            health: Arc::new(HealthFlag {
                missing: AtomicBool::new(false),
                changed: Notify::new(),
            }),
            legacy_found: false,
            retry_at: None,
            failures: 0,
            last_error: None,
            state: EngineState::Off,
            published: None,
        }
    }

    async fn run(mut self, mut control: mpsc::UnboundedReceiver<Control>) {
        self.config.settings.log_ignored();
        let mut relay = self.config.sync_activity.clone();
        let mut engine: Option<RunningEngine> = None;
        let mut recheck = tokio::time::interval_at(
            Instant::now() + self.config.recheck_interval,
            self.config.recheck_interval,
        );
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let health = Arc::clone(&self.health);
        self.legacy_found = self.find_legacy().await;
        loop {
            self.reconcile(&mut engine, &relay).await;
            self.publish(&engine);
            let retry_at = self.retry_at;
            tokio::select! {
                message = control.recv() => match message {
                    Some(Control::Enabled(enabled)) => self.config.enabled = enabled,
                    Some(Control::Paused(paused)) => self.config.paused = paused,
                    Some(Control::Settings(settings)) => {
                        settings.log_ignored();
                        self.config.settings = settings;
                    }
                    Some(Control::Recheck) => self.recheck(&mut engine, &relay).await,
                    Some(Control::Shutdown) | None => break,
                },
                ended = wait_for_end(&mut engine) => {
                    let finished = engine.take();
                    if finished
                        .as_ref()
                        .is_some_and(|running| running.started_at.elapsed() >= STABLE_RUN)
                    {
                        self.failures = 0;
                    }
                    let tmux_path = finished.map(|running| running.tmux_path);
                    let detail = match ended {
                        Ok(()) => "the engine stopped by itself".to_owned(),
                        Err(error) => error.to_string(),
                    };
                    tracing::warn!(error = %detail, tmux = ?tmux_path, "Terminal stopped; restarting");
                    self.fail(detail);
                }
                _ = recheck.tick() => self.recheck(&mut engine, &relay).await,
                () = sleep_until(retry_at) => self.retry_at = None,
                () = health.changed.notified() => {}
                changed = relay.changed() => {
                    if changed.is_ok() && let Some(running) = engine.as_ref() {
                        let activity = *relay.borrow_and_update();
                        let _ = running.handle.set_sync_activity(activity).await;
                    }
                }
            }
        }
        stop(&mut engine).await;
        self.publish(&engine);
    }

    fn wanted(&self) -> bool {
        self.config.enabled && !self.config.paused
    }

    async fn reconcile(
        &mut self,
        engine: &mut Option<RunningEngine>,
        relay: &watch::Receiver<SyncActivity>,
    ) {
        if !self.wanted() {
            stop(engine).await;
            self.retry_at = None;
            self.failures = 0;
            self.last_error = None;
            self.state = if self.config.enabled {
                EngineState::Paused
            } else {
                EngineState::Off
            };
            return;
        }
        if self.legacy_found {
            stop(engine).await;
            self.state = EngineState::WaitingForTmuxApp;
            return;
        }
        if engine.is_some() {
            self.state = if self.health.missing.load(Ordering::Acquire) {
                EngineState::TmuxNotFound
            } else {
                EngineState::Running
            };
            return;
        }
        if self.retry_at.is_some() {
            self.state = EngineState::Restarting;
            return;
        }
        let Some(tmux_path) = self.resolve_tmux() else {
            self.state = EngineState::TmuxNotFound;
            return;
        };
        self.start(engine, relay, tmux_path).await;
    }

    async fn start(
        &mut self,
        engine: &mut Option<RunningEngine>,
        relay: &watch::Receiver<SyncActivity>,
        tmux_path: PathBuf,
    ) {
        self.legacy_found = self.find_legacy().await;
        if self.legacy_found {
            self.state = EngineState::WaitingForTmuxApp;
            return;
        }
        self.health.missing.store(false, Ordering::Release);
        let config = self.engine_config(tmux_path.clone());
        let started = match &self.config.dependencies {
            Some(factory) => solstone_terminal::start_with_dependencies(config, factory()).await,
            None => solstone_terminal::start(config).await,
        };
        match started {
            Ok(handle) => {
                let activity = *relay.borrow();
                if activity != SyncActivity::Idle {
                    let _ = handle.set_sync_activity(activity).await;
                }
                tracing::info!(tmux = %tmux_path.display(), "Terminal started");
                *engine = Some(RunningEngine {
                    handle,
                    tmux_path,
                    started_at: Instant::now(),
                });
                self.last_error = None;
                self.state = if self.health.missing.load(Ordering::Acquire) {
                    EngineState::TmuxNotFound
                } else {
                    EngineState::Running
                };
            }
            Err(error) => {
                tracing::warn!(%error, "Terminal did not start");
                self.fail(error.to_string());
            }
        }
    }

    fn fail(&mut self, detail: String) {
        self.failures = self.failures.saturating_add(1);
        let factor = 1u32
            .checked_shl(self.failures.saturating_sub(1).min(16))
            .unwrap_or(u32::MAX);
        let delay = self
            .config
            .backoff_initial
            .saturating_mul(factor)
            .min(self.config.backoff_max);
        self.retry_at = Some(Instant::now() + delay);
        self.last_error = Some(detail);
        self.state = EngineState::Restarting;
    }

    /// The periodic check: the legacy find, then tmux again while it is missing.
    async fn recheck(
        &mut self,
        engine: &mut Option<RunningEngine>,
        relay: &watch::Receiver<SyncActivity>,
    ) {
        self.legacy_found = self.find_legacy().await;
        if self.legacy_found || !self.wanted() {
            return;
        }
        let Some(running) = engine.as_ref() else {
            return;
        };
        if !self.health.missing.load(Ordering::Acquire) {
            return;
        }
        let Some(found) = self.resolve_tmux() else {
            return;
        };
        if found == running.tmux_path {
            return;
        }
        tracing::info!(tmux = %found.display(), "Terminal moves to another tmux");
        stop(engine).await;
        self.start(engine, relay, found).await;
    }

    fn resolve_tmux(&self) -> Option<PathBuf> {
        super::tmux_path::resolve(
            self.config.settings.tmux_path.as_deref(),
            self.config.path_env.as_deref(),
        )
    }

    async fn find_legacy(&self) -> bool {
        let probe = Arc::clone(&self.config.legacy);
        match tokio::task::spawn_blocking(move || probe()).await {
            Ok(findings) => findings.found(),
            // A find that could not run holds, as finding something does.
            Err(_) => true,
        }
    }

    fn engine_config(&self, tmux_path: PathBuf) -> EngineConfig {
        let wake = Arc::clone(&self.config.on_closed_segment);
        let health = Arc::clone(&self.health);
        EngineConfig {
            root: self.config.captures_dir.clone(),
            layout: super::engine_layout(self.config.captures_dir.clone()),
            lock_path: super::engine_lock_path(&self.config.state_dir),
            tmux_path,
            settings: self.config.settings.capture.clone(),
            closed_segments: Arc::new(move |_| wake()),
            diagnostics: Arc::new(|diagnostic: Diagnostic| {
                tracing::warn!(diagnostic = %diagnostic.message(), "terminal");
            }),
            health: Arc::new(move |state: TmuxHealth| {
                health
                    .missing
                    .store(state == TmuxHealth::Missing, Ordering::Release);
                health.changed.notify_one();
            }),
        }
    }

    fn publish(&mut self, engine: &Option<RunningEngine>) {
        let state = TerminalState {
            terminal: self.config.enabled,
            state: self.state,
            tmux_path: engine.as_ref().map(|running| running.tmux_path.clone()),
            error: (self.state == EngineState::Restarting)
                .then(|| self.last_error.clone())
                .flatten(),
            ignored_settings: self.config.settings.ignored.clone(),
        };
        if self.published.as_ref() != Some(&state) {
            state.write(&self.config.state_dir);
            self.published = Some(state);
        }
    }
}

async fn stop(engine: &mut Option<RunningEngine>) {
    if let Some(running) = engine.take()
        && let Err(error) = running.handle.stop().await
    {
        tracing::warn!(%error, "Terminal ended with an error");
    }
}

async fn wait_for_end(
    engine: &mut Option<RunningEngine>,
) -> Result<(), solstone_terminal::EngineError> {
    match engine.as_mut() {
        Some(running) => running.handle.wait().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(all(test, feature = "full-tests"))]
mod tests {
    use super::super::fake_tmux::FakeTmux;
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Instant as StdInstant;

    struct Harness {
        _temp: tempfile::TempDir,
        captures: PathBuf,
        state: PathBuf,
        bin: PathBuf,
        fake: FakeTmux,
        found: Arc<AtomicBool>,
    }

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn harness() -> Harness {
        let temp = tempfile::tempdir().unwrap();
        let captures = temp.path().join("captures");
        let state = temp.path().join("state");
        let bin = temp.path().join("bin");
        fs::create_dir_all(&captures).unwrap();
        fs::create_dir_all(&bin).unwrap();
        Harness {
            _temp: temp,
            captures,
            state,
            bin,
            fake: FakeTmux::with_server(&[("status-left", "OWNER"), ("status-left-length", "10")]),
            found: Arc::new(AtomicBool::new(false)),
        }
    }

    fn config(harness: &Harness, enabled: bool) -> SupervisorConfig {
        let found = Arc::clone(&harness.found);
        let (_sender, activity) = watch::channel(SyncActivity::Idle);
        let mut config = SupervisorConfig::new(
            harness.captures.clone(),
            harness.state.clone(),
            Arc::new(move || {
                let mut findings = LegacyFindings::default();
                if found.load(Ordering::Acquire) {
                    findings.credentials.push("/x".into());
                }
                findings
            }),
            Arc::new(|| {}),
            activity,
        );
        config.enabled = enabled;
        config.path_env = Some(harness.bin.clone().into_os_string());
        config.recheck_interval = Duration::from_millis(100);
        config.backoff_initial = Duration::from_millis(50);
        config.backoff_max = Duration::from_millis(200);
        config.settings.capture.capture_interval = 1;
        let fake = harness.fake.clone();
        config.dependencies = Some(Arc::new(move || EngineDependencies {
            runner: Arc::new(fake.clone()),
            clock: Arc::new(solstone_terminal::clock::SystemClock::utc()),
            zone_source: Box::new(super::super::fake_tmux::UtcZoneSource),
            lookup: None,
        }));
        config
    }

    fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = StdInstant::now() + Duration::from_secs(15);
        while !ready() {
            assert!(StdInstant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn state_is(harness: &Harness, state: EngineState) -> bool {
        TerminalState::read(&harness.state).is_some_and(|read| read.state == state)
    }

    #[test]
    fn captures_while_on_and_a_stop_finalizes_and_restores_the_owner_options() {
        let harness = harness();
        executable(&harness.bin.join("tmux"));
        let supervisor = TerminalSupervisor::spawn(config(&harness, true)).unwrap();
        wait_for("the engine", || state_is(&harness, EngineState::Running));
        wait_for("polls", || harness.fake.polls() >= 2);
        supervisor.shutdown();
        assert_eq!(harness.fake.option("status-left").as_deref(), Some("OWNER"));
        let days: Vec<_> = fs::read_dir(&harness.captures).unwrap().flatten().collect();
        assert_eq!(days.len(), 1);
        assert!(days[0].path().join("_tmux").is_dir());
    }

    #[test]
    fn off_runs_nothing_and_pause_stops_capture_and_clears_the_cell() {
        let harness = harness();
        executable(&harness.bin.join("tmux"));
        let supervisor = TerminalSupervisor::spawn(config(&harness, false)).unwrap();
        wait_for("the off state", || state_is(&harness, EngineState::Off));
        assert!(harness.fake.calls().is_empty(), "off spawns no tmux");
        supervisor.set_enabled(true);
        wait_for("the engine", || state_is(&harness, EngineState::Running));
        supervisor.set_paused(true);
        wait_for("the paused state", || {
            state_is(&harness, EngineState::Paused)
        });
        assert_eq!(harness.fake.option("status-left").as_deref(), Some("OWNER"));
        let polls = harness.fake.polls();
        thread::sleep(Duration::from_millis(1500));
        assert_eq!(harness.fake.polls(), polls, "paused does not poll");
        supervisor.set_paused(false);
        wait_for("resumed capture", || harness.fake.polls() > polls);
        supervisor.shutdown();
    }

    #[test]
    fn an_old_install_holds_the_engine_until_it_is_gone() {
        let harness = harness();
        executable(&harness.bin.join("tmux"));
        harness.found.store(true, Ordering::Release);
        let supervisor = TerminalSupervisor::spawn(config(&harness, true)).unwrap();
        wait_for("the hold", || {
            state_is(&harness, EngineState::WaitingForTmuxApp)
        });
        assert!(harness.fake.calls().is_empty());
        harness.found.store(false, Ordering::Release);
        wait_for("the engine", || state_is(&harness, EngineState::Running));
        supervisor.shutdown();
    }

    #[test]
    fn tmux_installed_later_starts_capture_with_no_restart() {
        let harness = harness();
        let supervisor = TerminalSupervisor::spawn(config(&harness, true)).unwrap();
        wait_for("tmux not found", || {
            state_is(&harness, EngineState::TmuxNotFound)
        });
        executable(&harness.bin.join("tmux"));
        supervisor.recheck();
        wait_for("the engine", || state_is(&harness, EngineState::Running));
        supervisor.shutdown();
    }

    #[test]
    fn a_failed_start_shows_the_condition_and_retries() {
        let harness = harness();
        executable(&harness.bin.join("tmux"));
        fs::create_dir_all(&harness.state).unwrap();
        let lock_path = crate::terminal::engine_lock_path(&harness.state);
        let held = fs::File::create(&lock_path).unwrap();
        rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let supervisor = TerminalSupervisor::spawn(config(&harness, true)).unwrap();
        wait_for("the error state", || {
            state_is(&harness, EngineState::Restarting)
        });
        drop(held);
        wait_for("the engine", || state_is(&harness, EngineState::Running));
        assert_eq!(TerminalState::read(&harness.state).unwrap().error, None);
        supervisor.shutdown();
    }
}
