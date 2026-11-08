// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! # solstone-terminal
//!
//! The terminal capture engine extracted from solstone-tmux 2.0.28, a reader for a
//! stopped 2.0.28 install's spool, and the `solstone-terminal` helper binary.
//!
//! Facts that define this crate's contract:
//!
//! 1. **The layout is the source of truth.** After start-up recovery the
//!    closed-segment sink is called once for every finished segment in the layout,
//!    then on every finalize (rotation and stop). A consumer that misses a call loses
//!    nothing: it scans the layout at start and on each wake. A segment directory
//!    without a zone file is still a segment, delivered without capture-time
//!    metadata; a zone file whose segment directory is absent is not a segment.
//! 2. **The tmux path is used as given.** It must be absolute and is never
//!    canonicalized or cached, and it is checked before each poll. While it is absent
//!    or not executable the engine reports `missing`, keeps its lock and captures
//!    nothing; when it is executable again the engine reports `found` and capture
//!    and the ☼ resume without a restart.
//! 3. **Every way out restores and unlocks.** [`EngineHandle::stop`] and every error
//!    end finalize the open segment, restore `status-left`, `status-left-length` and
//!    `@solstone` as far as tmux allows, then release the lock, which is immediately
//!    re-acquirable. [`EngineHandle::wait`] reports an engine that ended on its own.
//! 4. **The library writes nothing to stderr.** Warnings go to the caller's
//!    [`DiagnosticSink`].

#![forbid(unsafe_code)]

pub mod clock;
pub mod command;
pub mod config;
pub mod foreground;
pub mod indicator;
pub mod instance_lock;
pub mod legacy;
pub mod model;
pub mod name;
pub mod observer;
pub mod paths;
pub mod recovery;
pub mod segment;
pub mod serialize;
pub mod storage;
pub mod tmux;

use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use crate::clock::{Clock, SystemClock, SystemZoneSource, Zone, ZoneSource};
pub use crate::command::{CommandRunner, TokioCommandRunner};
pub use crate::config::{CaptureSettings, SettingsError, read_lenient_legacy_segment_interval};
pub use crate::foreground::{ForegroundLookup, SystemLookup};
pub use crate::legacy::{
    Guard, HeldEntry, HeldReason, LegacyError, LegacyListing, LegacyRecovery, LegacySegment,
    Problem, open as open_legacy,
};
pub use crate::observer::{
    ActivityLinger, FailureWarning, LayoutFn, SYNCING_LINGER, SegmentLifecycle, SegmentManager,
    SyncActivity,
};

use crate::indicator::{CommandIndicatorIo, ServerIndicator};
use crate::model::PollOutcome;
use crate::recovery::RecoveryAction;
use crate::segment::SegmentClose;
use crate::tmux::{TmuxAdapter, WarningSink};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedSegment {
    pub day: String,
    pub segment: String,
    pub dir: PathBuf,
}

pub type ClosedSegmentSink = Arc<dyn Fn(ClosedSegment) + Send + Sync>;

/// A warning the engine reports and keeps running after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    /// A ☼ write failed. One per run of failures.
    IndicatorFailure(String),
    /// A capture poll failed and was skipped. One per run of failures.
    PollSkip(String),
    /// tmux reported something capture could not use, such as a session skipped or
    /// pane metadata that was not recorded.
    TmuxWarning(String),
    /// A durable append rolled back and will be retried.
    AppendRollback(String),
    /// The local time zone could not be loaded.
    ZoneLoadFailure(String),
    /// Start-up recovery retained, quarantined or could not finalize a segment.
    Recovery(String),
}

impl Diagnostic {
    pub fn message(&self) -> &str {
        match self {
            Self::IndicatorFailure(message)
            | Self::PollSkip(message)
            | Self::TmuxWarning(message)
            | Self::AppendRollback(message)
            | Self::ZoneLoadFailure(message)
            | Self::Recovery(message) => message,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "warning: {}", self.message())
    }
}

pub type DiagnosticSink = Arc<dyn Fn(Diagnostic) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TmuxHealth {
    Found,
    Missing,
}

pub type HealthSink = Arc<dyn Fn(TmuxHealth) + Send + Sync>;

pub struct EngineConfig {
    pub root: PathBuf,
    pub layout: LayoutFn,
    pub lock_path: PathBuf,
    pub tmux_path: PathBuf,
    pub settings: CaptureSettings,
    pub closed_segments: ClosedSegmentSink,
    pub diagnostics: DiagnosticSink,
    pub health: HealthSink,
}

pub struct EngineDependencies {
    pub runner: Arc<dyn CommandRunner>,
    pub clock: Arc<dyn Clock>,
    pub zone_source: Box<dyn ZoneSource>,
    pub lookup: Option<Arc<dyn ForegroundLookup>>,
}

enum EngineControlMsg {
    SetSyncActivity(SyncActivity),
    Stop,
}

type EngineResult = Result<(), EngineError>;

/// A running engine. Dropping the handle stops the engine as [`EngineHandle::stop`]
/// does, without waiting for it: a closed control channel is a stop.
pub struct EngineHandle {
    control_tx: tokio::sync::mpsc::Sender<EngineControlMsg>,
    join_handle: Option<tokio::task::JoinHandle<EngineResult>>,
    result: Option<EngineResult>,
}

impl EngineHandle {
    pub async fn set_sync_activity(&self, activity: SyncActivity) -> Result<(), EngineError> {
        self.control_tx
            .send(EngineControlMsg::SetSyncActivity(activity))
            .await
            .map_err(|_| EngineError::EngineStopped)
    }

    /// Waits until the engine has ended and returns how it ended: `Ok` after a
    /// stop, the error after an error end. By then the open segment is finalized,
    /// the owner's tmux options are restored and the lock is released. Safe to use
    /// in `select!`; once the engine has ended, every call returns the same result.
    pub async fn wait(&mut self) -> Result<(), EngineError> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let Some(join_handle) = self.join_handle.as_mut() else {
            return Err(EngineError::EngineStopped);
        };
        let result = match join_handle.await {
            Ok(result) => result,
            Err(error) => Err(EngineError::TaskFailed(error.to_string())),
        };
        self.join_handle = None;
        self.result = Some(result.clone());
        result
    }

    /// Finalizes and reports the open segment, restores the owner's tmux options,
    /// releases the lock and returns how the engine ended.
    pub async fn stop(mut self) -> Result<(), EngineError> {
        // A closed channel means the engine has already ended; `wait` reports how.
        let _ = self.control_tx.send(EngineControlMsg::Stop).await;
        self.wait().await
    }
}

pub fn check_tmux_health(path: &Path) -> TmuxHealth {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0) => {
            TmuxHealth::Found
        }
        _ => TmuxHealth::Missing,
    }
}

pub async fn start(config: EngineConfig) -> Result<EngineHandle, EngineError> {
    let runner = Arc::new(TokioCommandRunner);
    let clock = Arc::new(SystemClock::utc());
    let zone_source = Box::new(SystemZoneSource);
    start_with_dependencies(
        config,
        EngineDependencies {
            runner,
            clock,
            zone_source,
            lookup: None,
        },
    )
    .await
}

type Indicator = ServerIndicator<CommandIndicatorIo<Arc<dyn CommandRunner>>>;
type Adapter = TmuxAdapter<Arc<dyn CommandRunner>, Arc<dyn WarningSink>>;

/// Starts the engine: takes the lock, recovers the caller's day directories,
/// reports the finished segments in the layout, opens a segment and installs the
/// ☼, in 2.0.28's order. An error here leaves nothing running and the lock free.
pub async fn start_with_dependencies(
    config: EngineConfig,
    deps: EngineDependencies,
) -> Result<EngineHandle, EngineError> {
    if !config.tmux_path.is_absolute() {
        return Err(EngineError::ExecutableMustBeAbsolute(config.tmux_path));
    }
    config.settings.validate().map_err(EngineError::Settings)?;

    let lock =
        instance_lock::InstanceLock::acquire_path(&config.lock_path).map_err(|err| match err {
            instance_lock::InstanceLockError::AlreadyLocked(path) => {
                EngineError::AlreadyRunning(path)
            }
            other => EngineError::Io {
                operation: "acquire engine lock",
                path: config.lock_path.clone(),
                source: Arc::new(std::io::Error::other(other.to_string())),
            },
        })?;

    let finished = recover_layout(&config)?;

    let health = check_tmux_health(&config.tmux_path);
    (config.health)(health);
    for segment in finished {
        (config.closed_segments)(segment);
    }

    let capture_interval = config.settings.capture_interval_duration();
    let segment_interval = config.settings.segment_interval_duration();

    let diagnostics = config.diagnostics.clone();
    let segment_warnings: Arc<dyn WarningSink> = Arc::new(move |message: &str| {
        let message = message.to_owned();
        diagnostics(if message.starts_with(observer::ZONE_LOAD_WARNING) {
            Diagnostic::ZoneLoadFailure(message)
        } else {
            Diagnostic::AppendRollback(message)
        });
    });
    let closed_sink = config.closed_segments.clone();
    let wake_clock = deps.clock.clone();
    let sync_wake = observer::SyncWake::new(move |close| {
        if let SegmentClose::Finalized(path) = close {
            closed_sink(closed_segment_from_path(path, wake_clock.as_ref()));
        }
    });
    let mut segments = SegmentManager::start_with_layout(
        config.layout.clone(),
        deps.clock.as_ref(),
        segment_interval,
        sync_wake,
        deps.zone_source,
        segment_warnings,
    )
    .map_err(|error| EngineError::FatalSegmentError(error.to_string()))?;

    let diagnostics = config.diagnostics.clone();
    let tmux_warnings: Arc<dyn WarningSink> = Arc::new(move |message: &str| {
        diagnostics(Diagnostic::TmuxWarning(message.to_owned()));
    });
    let lookup = deps
        .lookup
        .unwrap_or_else(|| Arc::new(foreground::SystemLookup));
    let adapter = TmuxAdapter::with_context(
        config.tmux_path.clone(),
        deps.runner.clone(),
        tmux_warnings,
        config.settings.pane_title,
        config.settings.working_directory,
        config.settings.executable,
        lookup,
    )
    .map_err(|error| EngineError::Io {
        operation: "create tmux adapter",
        path: config.tmux_path.clone(),
        source: Arc::new(std::io::Error::other(error.to_string())),
    })?
    .with_output_sessions(config.settings.output_sessions.clone());
    let indicator_io = CommandIndicatorIo::new(deps.runner.clone(), config.tmux_path.clone())
        .map_err(|error| EngineError::Io {
            operation: "create indicator io",
            path: config.tmux_path.clone(),
            source: Arc::new(std::io::Error::other(error.to_string())),
        })?;

    // As 2.0.28: a failed ☼ install at start removes the empty start-up segment and
    // the engine does not start. A missing tmux defers the install to when it is found.
    let indicator = if config.settings.status_indicator && health == TmuxHealth::Found {
        match ServerIndicator::start(indicator_io.clone(), capture_interval).await {
            Ok(indicator) => Some(indicator),
            Err(error) => {
                let detail = match segments.segment_mut().remove_confirmed_empty() {
                    Ok(_) => error.to_string(),
                    Err(cleanup) => format!(
                        "{error}; removing the empty start-up segment also failed: {cleanup}"
                    ),
                };
                return Err(EngineError::Indicator(detail));
            }
        }
    } else {
        None
    };

    let diagnostics = config.diagnostics.clone();
    let indicator_warnings: Arc<dyn WarningSink> = Arc::new(move |message: &str| {
        diagnostics(Diagnostic::IndicatorFailure(message.to_owned()));
    });
    let engine = Engine {
        config,
        clock: deps.clock,
        adapter,
        segments,
        indicator_io,
        indicator,
        indicator_warnings,
        health,
        lock,
        capture_interval,
        segment_interval,
        shown_working: false,
        activity_pending: false,
        indicator_warning: FailureWarning::default(),
    };
    let (control_tx, control_rx) = tokio::sync::mpsc::channel(32);
    let join_handle = tokio::spawn(engine.run(control_rx));

    Ok(EngineHandle {
        control_tx,
        join_handle: Some(join_handle),
        result: None,
    })
}

/// Runs 2.0.28's recovery on the caller's day directory of every day in the root,
/// and returns the finished segments then in the layout. A day without that
/// directory has nothing to recover; no other directory is touched.
fn recover_layout(config: &EngineConfig) -> Result<Vec<ClosedSegment>, EngineError> {
    let entries = match fs::read_dir(&config.root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(EngineError::Io {
                operation: "scan capture days",
                path: config.root.clone(),
                source: Arc::new(source),
            });
        }
    };
    let mut days = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| EngineError::Io {
            operation: "read capture day entry",
            path: config.root.clone(),
            source: Arc::new(source),
        })?;
        let Some(day) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !recovery::valid_date_name(&day) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        days.push(day);
    }
    days.sort();

    let segment_interval = config.settings.segment_interval_duration();
    let mut finished = Vec::new();
    for day in days {
        let directory = (config.layout)(&day);
        match fs::symlink_metadata(&directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(EngineError::RecoveryFailed(format!(
                    "inspect day directory failed for {}: {source}",
                    directory.display()
                )));
            }
        }
        let records = recovery::recover_stream_inner(
            &config.root,
            &directory,
            segment_interval,
            recovery::RecoveryOptions::default(),
        )
        .map_err(|error| EngineError::RecoveryFailed(error.to_string()))?;
        for record in records {
            if matches!(
                record.action,
                RecoveryAction::Retain | RecoveryAction::Quarantine | RecoveryAction::Failed
            ) {
                (config.diagnostics)(Diagnostic::Recovery(format!(
                    "recovery {:?} for {}: {}",
                    record.action,
                    record.candidate.display(),
                    record.detail
                )));
            }
        }

        let entries = fs::read_dir(&directory).map_err(|source| {
            EngineError::RecoveryFailed(format!(
                "scan day directory failed for {}: {source}",
                directory.display()
            ))
        })?;
        let mut names = Vec::new();
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !legacy::valid_segment_name(&name) {
                continue;
            }
            let plain_directory = fs::symlink_metadata(entry.path())
                .is_ok_and(|metadata| metadata.file_type().is_dir());
            if plain_directory {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            finished.push(ClosedSegment {
                day: day.clone(),
                dir: directory.join(&name),
                segment: name,
            });
        }
    }
    Ok(finished)
}

struct Engine {
    config: EngineConfig,
    clock: Arc<dyn Clock>,
    adapter: Adapter,
    segments: SegmentManager,
    indicator_io: CommandIndicatorIo<Arc<dyn CommandRunner>>,
    indicator: Option<Indicator>,
    indicator_warnings: Arc<dyn WarningSink>,
    health: TmuxHealth,
    lock: instance_lock::InstanceLock,
    capture_interval: Duration,
    segment_interval: Duration,
    /// What the ☼ shows, as the linger last decided.
    shown_working: bool,
    /// The ☼ owes a write that was skipped while tmux was missing.
    activity_pending: bool,
    indicator_warning: FailureWarning,
}

impl Engine {
    async fn run(
        mut self,
        mut control_rx: tokio::sync::mpsc::Receiver<EngineControlMsg>,
    ) -> EngineResult {
        let mut failure = self.capture(&mut control_rx).await.err();

        // Capture ends here. As 2.0.28's shutdown: finalize, restore, unlock.
        let monotonic_now = self.clock.monotonic_now();
        match self.segments.shutdown(monotonic_now) {
            Ok(SegmentClose::Finalized(path)) => {
                (self.config.closed_segments)(closed_segment_from_path(path, self.clock.as_ref()));
            }
            Ok(_) => {}
            Err(error) => {
                failure = Some(match failure {
                    Some(EngineError::FatalSegmentError(detail)) => EngineError::FatalSegmentError(
                        format!("{detail}; segment shutdown failed: {error}"),
                    ),
                    Some(other) => other,
                    None => {
                        EngineError::FatalSegmentError(format!("segment shutdown failed: {error}"))
                    }
                });
            }
        }
        if let Some(indicator) = self.indicator.as_mut()
            && let Err(error) = indicator.restore().await
        {
            (self.config.diagnostics)(Diagnostic::IndicatorFailure(format!(
                "indicator shutdown failed: {error}"
            )));
        }
        drop(self.indicator.take());
        drop(self.lock);
        failure.map_or(Ok(()), Err)
    }

    /// Captures until stopped or until a segment error, which ends the engine.
    async fn capture(
        &mut self,
        control_rx: &mut tokio::sync::mpsc::Receiver<EngineControlMsg>,
    ) -> EngineResult {
        let mut ticker = tokio::time::interval(self.capture_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut refresh = self.config.settings.status_indicator.then(|| {
            let mut refresh = tokio::time::interval_at(
                tokio::time::Instant::now() + self.capture_interval,
                self.capture_interval,
            );
            refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            refresh
        });
        let mut linger = ActivityLinger::default();
        let mut poll_warning = FailureWarning::default();

        loop {
            tokio::select! {
                biased;

                message = control_rx.recv() => match message {
                    Some(EngineControlMsg::SetSyncActivity(activity)) => {
                        let now = tokio::time::Instant::now().into_std();
                        if let Some(shown) = linger.observe(activity, now) {
                            self.show_activity(shown).await;
                        }
                    }
                    Some(EngineControlMsg::Stop) | None => return Ok(()),
                },

                _ = ticker.tick() => {
                    let health = check_tmux_health(&self.config.tmux_path);
                    if health != self.health {
                        self.health = health;
                        (self.config.health)(health);
                        if health == TmuxHealth::Found {
                            self.refresh_indicator().await;
                        }
                    }

                    let wall_now = self.clock.wall_now();
                    let monotonic_now = self.clock.monotonic_now();
                    // While tmux is missing, or when a poll fails, the segment sees an
                    // empty poll, so it still rotates on schedule.
                    let outcome = if self.health == TmuxHealth::Missing {
                        empty_poll()
                    } else {
                        match self.adapter.poll_context(wall_now, self.capture_interval).await {
                            Ok(outcome) => {
                                poll_warning.record_success();
                                outcome
                            }
                            Err(error) => {
                                let diagnostics = &self.config.diagnostics;
                                poll_warning.record_failure(|| {
                                    diagnostics(Diagnostic::PollSkip(format!(
                                        "capture poll skipped: {error}"
                                    )));
                                });
                                empty_poll()
                            }
                        }
                    };
                    self.segments
                        .process_poll(
                            &outcome.captures,
                            &outcome.activity_jsonl,
                            wall_now,
                            monotonic_now,
                            self.segment_interval,
                        )
                        .map_err(|error| {
                            EngineError::FatalSegmentError(format!("segment poll failed: {error}"))
                        })?;
                }

                () = sleep_until_linger(linger.ends_at()) => {
                    let now = tokio::time::Instant::now().into_std();
                    if let Some(shown) = linger.expire(now) {
                        self.show_activity(shown).await;
                    }
                }

                _ = next_indicator_tick(&mut refresh) => {
                    if self.health == TmuxHealth::Found {
                        self.refresh_indicator().await;
                    }
                }
            }
        }
    }

    async fn show_activity(&mut self, shown: SyncActivity) {
        self.shown_working = shown == SyncActivity::Working;
        if self.health == TmuxHealth::Missing {
            self.activity_pending = self.indicator.is_some();
            return;
        }
        if let Some(indicator) = self.indicator.as_mut() {
            let result = indicator.set_working(self.shown_working).await;
            self.indicator_warning.record(
                result.map_err(indicator_update_error),
                &*self.indicator_warnings,
            );
        }
    }

    /// As 2.0.28's refresh tick: installs the ☼ on a server that does not have it
    /// yet and forgets a server that has exited. The first refresh after tmux is
    /// found creates the indicator when tmux was missing at start.
    async fn refresh_indicator(&mut self) {
        if !self.config.settings.status_indicator {
            return;
        }
        let result = match self.indicator.as_mut() {
            Some(indicator) => {
                let mut result = indicator.refresh().await;
                if result.is_ok() && self.activity_pending {
                    self.activity_pending = false;
                    result = indicator.set_working(self.shown_working).await;
                }
                result
            }
            None => {
                match ServerIndicator::start(self.indicator_io.clone(), self.capture_interval).await
                {
                    Ok(mut indicator) => {
                        let result = if self.shown_working {
                            indicator.set_working(true).await
                        } else {
                            Ok(())
                        };
                        self.activity_pending = false;
                        self.indicator = Some(indicator);
                        result
                    }
                    Err(error) => Err(error),
                }
            }
        };
        self.indicator_warning.record(
            result.map_err(indicator_update_error),
            &*self.indicator_warnings,
        );
    }
}

fn indicator_update_error(error: indicator::IndicatorError) -> String {
    format!("indicator update failed: {error}")
}

fn empty_poll() -> PollOutcome {
    PollOutcome {
        captures: Vec::new(),
        activity_jsonl: Vec::new(),
    }
}

async fn sleep_until_linger(at: Option<std::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}

async fn next_indicator_tick(refresh: &mut Option<tokio::time::Interval>) {
    match refresh {
        Some(refresh) => {
            refresh.tick().await;
        }
        None => std::future::pending().await,
    }
}

#[derive(Clone, Debug)]
pub enum EngineError {
    AlreadyRunning(PathBuf),
    ExecutableMustBeAbsolute(PathBuf),
    RecoveryFailed(String),
    FatalSegmentError(String),
    Indicator(String),
    Settings(SettingsError),
    EngineStopped,
    TaskFailed(String),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: Arc<std::io::Error>,
    },
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning(path) => {
                write!(
                    formatter,
                    "another instance is already running (lock: {})",
                    path.display()
                )
            }
            Self::ExecutableMustBeAbsolute(path) => {
                write!(
                    formatter,
                    "tmux executable path must be absolute: {}",
                    path.display()
                )
            }
            Self::RecoveryFailed(detail) => {
                write!(formatter, "startup recovery failed: {detail}")
            }
            Self::FatalSegmentError(detail) => {
                write!(formatter, "segment fatal error: {detail}")
            }
            Self::Indicator(detail) => write!(formatter, "indicator start failed: {detail}"),
            Self::Settings(error) => error.fmt(formatter),
            Self::EngineStopped => write!(formatter, "engine is stopped"),
            Self::TaskFailed(detail) => write!(formatter, "engine task failed: {detail}"),
            Self::Io {
                operation,
                path,
                source,
            } => {
                write!(
                    formatter,
                    "{operation} failed for {}: {source}",
                    path.display()
                )
            }
        }
    }
}

fn closed_segment_from_path(path: PathBuf, clock: &dyn Clock) -> ClosedSegment {
    let segment = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let mut day = String::new();
    for ancestor in path.ancestors().skip(1) {
        if let Some(name) = ancestor.file_name().and_then(|n| n.to_str())
            && recovery::valid_date_name(name)
        {
            day = name.to_string();
            break;
        }
    }
    if day.is_empty() {
        let wall = clock.wall_now();
        let offset = clock.offset_at(wall);
        let (d, _) = clock::local_date_and_time(wall, offset);
        day = d;
    }
    ClosedSegment {
        day,
        segment,
        dir: path,
    }
}

impl std::error::Error for EngineError {}
