// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Capture-loop policy, independent of desktop and network backends.

use crate::{
    capture_stats::{CaptureStats, compute_capture_stats},
    chunking::{DrainedChunk, HitGate},
    config::Config,
    encoding::{AudioOutputPlan, audio_output_plan},
    pause_hold::{self, HeldPause, StartingPause},
    recovery::{
        CAPTURE_ZONE_FILENAME, CaptureZone, SegmentProgress, is_segment_sidecar,
        scan_segment_progress, write_capture_zone, write_segment_metadata,
    },
    segment::{
        ZoneReading, ZoneSource, clamp_duration, finalize_segment_dir, segment_key, timestamp_parts,
    },
};
#[cfg(test)]
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const CAPTURE_STATS_REFRESH_INTERVAL: f64 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Idle,
    Screencast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStream {
    pub connector: String,
    pub position: String,
    pub file_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoppedStream {
    pub node_id: u32,
    pub connector: String,
    pub position: String,
    pub file_bytes: u64,
}

pub trait VideoCapture {
    fn start(
        &mut self,
        directory: &Path,
        framerate: i64,
        draw_cursor: bool,
    ) -> Result<Vec<VideoStream>, String>;
    fn stop(&mut self) -> Result<Vec<StoppedStream>, String>;
    fn stop_for_shutdown(&mut self) -> Result<Vec<StoppedStream>, String> {
        self.stop()
    }
    fn is_healthy(&self) -> bool;
}
pub trait AudioCapture {
    /// Must be a nonblocking drain; device probing belongs to the backend worker.
    fn drain(&mut self) -> DrainedChunk;
    fn audio_available(&self) -> bool;
    fn fatal_error(&self) -> Option<String>;
    fn stop(&mut self);
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityState {
    pub screen_locked: bool,
    pub power_save: bool,
    pub user_idle: bool,
    pub power_unreadable: bool,
}
pub trait ActivityProbe {
    fn probe(&mut self) -> Result<ActivityState, String>;
}
pub trait MuteProbe {
    fn probe_muted(&mut self) -> Result<bool, String>;
}
pub trait AudioWriter {
    fn write(
        &mut self,
        frames: &[f32],
        plan: &AudioOutputPlan,
        directory: &Path,
    ) -> Result<(), String>;
}
pub trait Clock {
    fn wall_seconds(&self) -> f64;
    fn monotonic_seconds(&self) -> f64;
}
pub trait CaptureStatsSource {
    /// Returns a cached value without performing filesystem work on the tick thread.
    fn snapshot(&mut self, root: &Path, today: &str) -> CaptureStats;
}
pub struct BackgroundCaptureStats {
    requests: std::sync::mpsc::SyncSender<(PathBuf, String)>,
    results: std::sync::mpsc::Receiver<CaptureStats>,
    latest: CaptureStats,
}
impl BackgroundCaptureStats {
    pub fn new() -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::sync_channel::<(PathBuf, String)>(1);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok((root, today)) = request_rx.recv() {
                let _ = result_tx.send(compute_capture_stats(&root, &today));
            }
        });
        Self {
            requests: request_tx,
            results: result_rx,
            latest: CaptureStats {
                captures_today: 0,
                total_size_mb: 0,
            },
        }
    }
}
impl Default for BackgroundCaptureStats {
    fn default() -> Self {
        Self::new()
    }
}
impl CaptureStatsSource for BackgroundCaptureStats {
    fn snapshot(&mut self, root: &Path, today: &str) -> CaptureStats {
        if let Ok(value) = self.results.try_recv() {
            self.latest = value;
        }
        let _ = self
            .requests
            .try_send((root.to_path_buf(), today.to_owned()));
        CaptureStats {
            captures_today: self.latest.captures_today,
            total_size_mb: self.latest.total_size_mb,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentCompletedEvent {
    pub key: String,
}
pub trait EventSink {
    fn segment_completed(&mut self, event: SegmentCompletedEvent);
}
#[derive(Clone, Debug, PartialEq)]
pub struct StateSnapshot {
    pub mode: Mode,
    pub paused: bool,
    // `segment_open` is the legacy boolean; desktop readers use `segment_start_mono` as the
    // authoritative active-segment fact. Publishers must keep the pair consistent.
    pub segment_open: bool,
    pub captures_today: u64,
    pub total_size_mb: u64,
    // Additive desktop-surface anchors. Countdowns are derived when read, never stored.
    pub pause_until: Option<f64>,
    pub segment_start_mono: Option<f64>,
    pub process_start_mono: f64,
}
pub trait StateSink {
    fn publish(&mut self, snapshot: StateSnapshot);
}
pub struct WatchStateSink {
    sender: tokio::sync::watch::Sender<StateSnapshot>,
}
impl WatchStateSink {
    pub fn channel(initial: StateSnapshot) -> (Self, tokio::sync::watch::Receiver<StateSnapshot>) {
        let (sender, receiver) = tokio::sync::watch::channel(initial);
        (Self { sender }, receiver)
    }
}
impl StateSink for WatchStateSink {
    fn publish(&mut self, snapshot: StateSnapshot) {
        self.sender.send_replace(snapshot);
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum ObserverError {
    VideoStart(String),
    VideoStop(String),
    AudioWrite(String),
    Io(String),
    AudioFatal(String),
}
impl From<io::Error> for ObserverError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

pub struct Backends<V, A, P, M, W, E, C, Q, N> {
    pub video: V,
    pub audio: A,
    pub activity: P,
    pub mute: M,
    pub writer: W,
    pub events: E,
    pub clock: C,
    pub stats: Q,
    pub states: N,
}

pub struct ObserverState {
    pub mode: Mode,
    pub paused: bool,
    pub pause_until: Option<f64>,
    // The owner's deadline on the wall clock. A suspend stops the monotonic clock,
    // so a timed pause also ends once wall time reaches the deadline.
    pub pause_until_wall: Option<f64>,
    pub segment_dir: Option<PathBuf>,
    pub segment_start_wall: f64,
    pub segment_start_mono: f64,
    // Process uptime for tray/GetStats. The observe/status uptime remains segment elapsed.
    pub process_start_mono: f64,
    pub segment_is_muted: bool,
    pub cached_is_muted: bool,
    pub cached_is_active: bool,
    pub cached_activity: ActivityState,
    pub current_streams: Vec<VideoStream>,
    // Survives stop_video: the watchdog must not look like "video never started".
    pub video_started: bool,
    pub hit_gate: HitGate,
    pub frames: Vec<f32>,
    pub capture_stats: CaptureStats,
    pub last_stats_refresh: f64,
}

pub struct Observer<V, A, P, M, W, E, C, Q, N> {
    pub config: Config,
    pub state: ObserverState,
    pub backends: Backends<V, A, P, M, W, E, C, Q, N>,
    zone: Box<dyn ZoneSource>,
}

impl<V, A, P, M, W, E, C, Q, N> Observer<V, A, P, M, W, E, C, Q, N>
where
    V: VideoCapture,
    A: AudioCapture,
    P: ActivityProbe,
    M: MuteProbe,
    W: AudioWriter,
    E: EventSink,
    C: Clock,
    Q: CaptureStatsSource,
    N: StateSink,
{
    /// Build reading the held pause itself. The app reads it once up front
    /// instead, so its first published state agrees (`with_starting_pause`).
    #[cfg(test)]
    pub(crate) fn new(
        config: Config,
        backends: Backends<V, A, P, M, W, E, C, Q, N>,
        zone: Box<dyn ZoneSource>,
    ) -> Self {
        let starting = StartingPause::read(
            &config,
            backends.clock.wall_seconds(),
            backends.clock.monotonic_seconds(),
        );
        Self::with_starting_pause(config, backends, zone, starting)
    }

    /// Build with a starting pause already read, so the first published state
    /// and the observer agree on it.
    pub(crate) fn with_starting_pause(
        config: Config,
        backends: Backends<V, A, P, M, W, E, C, Q, N>,
        zone: Box<dyn ZoneSource>,
        starting: StartingPause,
    ) -> Self {
        let wall = backends.clock.wall_seconds();
        let mono = backends.clock.monotonic_seconds();
        Self {
            config,
            backends,
            zone,
            state: ObserverState {
                mode: Mode::Idle,
                paused: starting.paused,
                pause_until: starting.until_mono,
                pause_until_wall: starting.until_wall,
                segment_dir: None,
                segment_start_wall: wall,
                segment_start_mono: mono,
                process_start_mono: mono,
                segment_is_muted: false,
                cached_is_muted: false,
                cached_is_active: false,
                cached_activity: ActivityState::default(),
                current_streams: vec![],
                video_started: false,
                hit_gate: HitGate::default(),
                frames: vec![],
                capture_stats: CaptureStats {
                    captures_today: 0,
                    total_size_mb: 0,
                },
                last_stats_refresh: -CAPTURE_STATS_REFRESH_INTERVAL,
            },
        }
    }

    pub fn initialize(&mut self) -> Result<(), ObserverError> {
        let target = self.probe_status().unwrap_or(Mode::Screencast);
        self.state.segment_is_muted = self.state.cached_is_muted;
        self.state.cached_is_active = target == Mode::Screencast;
        self.state.mode = target;
        if !self.state.paused {
            self.start_capture(target)?;
        }
        self.publish();
        Ok(())
    }

    /// The owner's pause. `seconds == 0` is "until I resume". It is recorded so a
    /// restart keeps it.
    pub fn pause(&mut self, seconds: u64) {
        self.state.paused = true;
        self.state.pause_until =
            (seconds > 0).then(|| self.backends.clock.monotonic_seconds() + seconds as f64);
        self.state.pause_until_wall =
            (seconds > 0).then(|| self.backends.clock.wall_seconds() + seconds as f64);
        let held = match self.state.pause_until_wall {
            Some(deadline) => HeldPause::Until(deadline),
            None => HeldPause::UntilResumed,
        };
        if let Err(error) = pause_hold::save(&self.config.state_dir(), held) {
            tracing::warn!(%error, "Could not record the pause; it holds until this app stops");
        }
        self.publish();
    }
    /// Ends the owner's pause, by the owner or at its deadline.
    pub fn resume(&mut self) {
        self.state.paused = false;
        self.state.pause_until = None;
        self.state.pause_until_wall = None;
        if let Err(error) = pause_hold::clear(&self.config.state_dir()) {
            tracing::warn!(%error, "Could not remove the recorded pause");
        }
        self.publish();
    }

    pub fn tick(&mut self) -> Result<(), ObserverError> {
        let now = self.backends.clock.monotonic_seconds();
        // Reference order: stats refresh precedes auto-resume and the paused branch.
        if now - self.state.last_stats_refresh >= CAPTURE_STATS_REFRESH_INTERVAL {
            self.state.last_stats_refresh = now;
            self.refresh_stats();
        }
        if self.state.paused
            && (self
                .state
                .pause_until
                .is_some_and(|deadline| now >= deadline)
                || self
                    .state
                    .pause_until_wall
                    .is_some_and(|deadline| self.backends.clock.wall_seconds() >= deadline))
        {
            self.resume();
        }
        if self.state.paused {
            self.finish_paused_segment()?;
            let _ = self.backends.audio.drain();
            self.publish();
            return Ok(());
        }
        if self.state.segment_dir.is_none() {
            // Deliberate reference parity: resume-reopen ticks do not drain audio.
            let target = self.probe_status().unwrap_or(self.state.mode);
            self.state.segment_is_muted = self.state.cached_is_muted;
            self.state.mode = target;
            match self.start_capture(target) {
                Err(ObserverError::VideoStart(_)) => {
                    // Deliberate reference parity: failed resume start leaves its first .incomplete potentially orphaned for recovery.
                    self.start_segment()?;
                }
                Err(error) => return Err(error),
                Ok(()) => {}
            }
            self.publish();
            return Ok(());
        }
        if let Some(dir) = self.state.segment_dir.as_ref() {
            let (has_durable_media, durable_byte_count) = scan_segment_progress(dir);
            let last_durable_write_at =
                has_durable_media.then(|| self.backends.clock.wall_seconds());
            write_segment_metadata(
                dir,
                self.state.segment_start_wall,
                SegmentProgress {
                    has_durable_media,
                    durable_byte_count,
                    last_durable_write_at,
                },
            );
        }
        let target = self.probe_status().unwrap_or(self.state.mode);
        if self.state.mode == Mode::Screencast && !self.backends.video.is_healthy() {
            self.stop_video()?;
            self.state.mode = Mode::Idle;
        }
        let mode_changed = target != self.state.mode;
        let screen_transition =
            mode_changed && (self.state.mode == Mode::Screencast || target == Mode::Screencast);
        let mute_transition = self.state.cached_is_muted != self.state.segment_is_muted;
        let chunk = self.backends.audio.drain();
        self.state.frames.extend_from_slice(chunk.interleaved());
        self.state.hit_gate.observe(&chunk);
        let elapsed = now - self.state.segment_start_mono;
        if elapsed >= self.config.segment_interval as f64 || screen_transition || mute_transition {
            self.handle_boundary(target)?;
        }
        self.publish();
        Ok(())
    }

    pub fn handle_boundary(&mut self, target: Mode) -> Result<(), ObserverError> {
        if self.state.mode == Mode::Screencast {
            self.stop_video()?;
        }
        self.write_gated_audio()?;
        self.state.frames.clear();
        self.state.hit_gate = HitGate::default();
        self.finalize_segment()?;
        self.state.segment_is_muted = self.state.cached_is_muted;
        self.state.mode = target;
        self.start_capture(target)
    }

    pub fn shutdown(&mut self) -> Result<(), ObserverError> {
        if self.state.mode == Mode::Screencast {
            let _ = self
                .backends
                .video
                .stop_for_shutdown()
                .map_err(ObserverError::VideoStop)?;
            self.state.current_streams.clear();
        }
        self.write_gated_audio()?;
        self.finalize_segment()?;
        self.backends.audio.stop();
        self.publish();
        if let Some(error) = self.backends.audio.fatal_error() {
            return Err(ObserverError::AudioFatal(error));
        }
        Ok(())
    }

    fn finish_paused_segment(&mut self) -> Result<(), ObserverError> {
        if self.state.segment_dir.is_none() {
            return Ok(());
        }
        if self.state.mode == Mode::Screencast {
            self.stop_video()?
        }
        self.write_gated_audio()?;
        self.state.frames.clear();
        self.state.hit_gate = HitGate::default();
        self.finalize_segment()
    }
    fn write_gated_audio(&mut self) -> Result<(), ObserverError> {
        if self.state.hit_gate.should_save()
            && let Some(directory) = self.state.segment_dir.as_deref()
        {
            self.backends
                .writer
                .write(
                    &self.state.frames,
                    &audio_output_plan(self.state.segment_is_muted),
                    directory,
                )
                .map_err(ObserverError::AudioWrite)?;
        }
        Ok(())
    }
    fn start_capture(&mut self, target: Mode) -> Result<(), ObserverError> {
        let dir = self.start_segment()?;
        if target == Mode::Screencast && !self.state.cached_activity.screen_locked {
            let streams = self
                .backends
                .video
                .start(&dir, self.config.capture_framerate, self.config.draw_cursor)
                .map_err(ObserverError::VideoStart)?;
            if streams.is_empty() {
                return Err(ObserverError::VideoStart("no streams available".into()));
            }
            self.state.current_streams = streams;
            self.state.video_started = true;
        }
        Ok(())
    }
    fn start_segment(&mut self) -> Result<PathBuf, ObserverError> {
        let wall = self.backends.clock.wall_seconds();
        if let Some(ref dir) = self.state.segment_dir
            && (wall.floor() as i64) == (self.state.segment_start_wall.floor() as i64)
        {
            return Ok(dir.clone());
        }
        let reading = self.zone.read_zone(wall);
        let (date, time) = self.choose_stem(&reading);
        let dir = self
            .config
            .captures_dir()
            .join(date)
            .join(&self.config.stream)
            .join(format!("{time}.incomplete"));
        // Resume may fall back after video startup fails. Only this observer's
        // still-open segment can be reused; never adopt a directory from disk.
        if self.state.segment_dir.as_ref() == Some(&dir) {
            return Ok(dir);
        }
        fs::create_dir_all(dir.parent().expect("segment directory has a parent"))?;
        fs::create_dir(&dir)?;
        write_segment_metadata(&dir, wall, SegmentProgress::default());
        write_capture_zone(
            &dir,
            &CaptureZone {
                tz: reading.tz,
                utc_offset_seconds: reading.utc_offset_seconds,
            },
        );
        self.state.segment_start_wall = wall;
        self.state.segment_start_mono = self.backends.clock.monotonic_seconds();
        self.state.segment_dir = Some(dir.clone());
        self.state.video_started = false;
        Ok(dir)
    }
    fn choose_stem(&self, reading: &ZoneReading) -> (String, String) {
        let mut curr_day = reading.day.clone();
        let mut curr_hms = reading.hms.clone();
        loop {
            let dir = self
                .config
                .captures_dir()
                .join(&curr_day)
                .join(&self.config.stream);
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    let mut taken = false;
                    for entry in entries {
                        let Ok(entry) = entry else {
                            tracing::warn!("could not list segment names; using the unbumped stem");
                            return (reading.day.clone(), reading.hms.clone());
                        };
                        let path = entry.path();
                        if self.state.segment_dir.as_ref() == Some(&path) {
                            continue;
                        }
                        if let Some(file_name) = path.file_name().and_then(|n| n.to_str())
                            && file_name.starts_with(&curr_hms)
                        {
                            taken = true;
                            break;
                        }
                    }
                    if !taken {
                        return (curr_day, curr_hms);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    return (curr_day, curr_hms);
                }
                Err(_) => {
                    tracing::warn!("could not list segment names; using the unbumped stem");
                    return (reading.day.clone(), reading.hms.clone());
                }
            }

            let (next_day, next_hms) = bump_stem(&curr_day, &curr_hms);
            if next_day == curr_day && next_hms == curr_hms {
                return (curr_day, curr_hms);
            }
            curr_day = next_day;
            curr_hms = next_hms;
        }
    }
    fn finalize_segment(&mut self) -> Result<(), ObserverError> {
        let Some(dir) = self.state.segment_dir.take() else {
            return Ok(());
        };
        let nonempty = fs::read_dir(&dir)?
            .any(|e| e.is_ok_and(|x| x.path().is_file() && !is_segment_sidecar(&x.file_name())));
        if !nonempty {
            let _ = fs::remove_file(dir.join(".metadata"));
            let _ = fs::remove_file(dir.join(CAPTURE_ZONE_FILENAME));
            let _ = fs::remove_dir(&dir);
            return Ok(());
        }
        let Some(time) = dir
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".incomplete"))
        else {
            return Err(ObserverError::Io(
                "invalid incomplete directory name".into(),
            ));
        };
        // Length is how long capture ran, never a wall-clock difference: wall time
        // keeps moving while the computer is suspended.
        let duration = clamp_duration(
            self.backends.clock.monotonic_seconds() - self.state.segment_start_mono,
            self.config.segment_interval.max(1) as u64,
        );
        let key = segment_key(time, duration);
        let final_dir = finalize_segment_dir(&dir, &key)?;
        let _ = fs::remove_file(final_dir.join(".metadata"));
        self.backends
            .events
            .segment_completed(SegmentCompletedEvent { key });
        Ok(())
    }
    fn stop_video(&mut self) -> Result<(), ObserverError> {
        let _ = self
            .backends
            .video
            .stop()
            .map_err(ObserverError::VideoStop)?;
        self.state.current_streams.clear();
        Ok(())
    }
    fn refresh_stats(&mut self) {
        let (today, _) = timestamp_parts(self.backends.clock.wall_seconds());
        self.state.capture_stats = self
            .backends
            .stats
            .snapshot(&self.config.captures_dir(), &today);
    }
    fn probe_status(&mut self) -> Result<Mode, String> {
        let activity = self.backends.activity.probe()?;
        let muted = self.backends.mute.probe_muted()?;
        self.state.cached_activity = activity;
        self.state.cached_is_muted = muted;
        let target = mode(activity);
        // Deliberate reference parity: activity.active observes hits from before this tick drains.
        self.state.cached_is_active =
            target == Mode::Screencast || self.state.hit_gate.should_save();
        Ok(target)
    }
    fn publish(&mut self) {
        self.backends.states.publish(StateSnapshot {
            mode: self.state.mode,
            paused: self.state.paused,
            segment_open: self.state.segment_dir.is_some(),
            captures_today: self.state.capture_stats.captures_today,
            total_size_mb: self.state.capture_stats.total_size_mb,
            pause_until: self.state.pause_until,
            segment_start_mono: self
                .state
                .segment_dir
                .as_ref()
                .map(|_| self.state.segment_start_mono),
            process_start_mono: self.state.process_start_mono,
        })
    }
}

fn bump_stem(day: &str, hms: &str) -> (String, String) {
    if hms.len() != 6 {
        return (day.to_string(), hms.to_string());
    }
    let Ok(hh) = hms[0..2].parse::<u32>() else {
        return (day.to_string(), hms.to_string());
    };
    let Ok(mm) = hms[2..4].parse::<u32>() else {
        return (day.to_string(), hms.to_string());
    };
    let Ok(ss) = hms[4..6].parse::<u32>() else {
        return (day.to_string(), hms.to_string());
    };

    let (new_hh, new_mm, new_ss, rollover_day) = if ss < 59 {
        (hh, mm, ss + 1, false)
    } else if mm < 59 {
        (hh, mm + 1, 0, false)
    } else if hh < 23 {
        (hh + 1, 0, 0, false)
    } else {
        (0, 0, 0, true)
    };

    let bumped_hms = format!("{new_hh:02}{new_mm:02}{new_ss:02}");
    if rollover_day {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(day, "%Y%m%d")
            && let Some(next_date) = date.succ_opt()
        {
            return (next_date.format("%Y%m%d").to_string(), bumped_hms);
        }
        (day.to_string(), hms.to_string())
    } else {
        (day.to_string(), bumped_hms)
    }
}

fn mode(a: ActivityState) -> Mode {
    if a.screen_locked || a.power_save || (a.power_unreadable && a.user_idle) {
        Mode::Idle
    } else {
        Mode::Screencast
    }
}

/// Reference lifecycle policy. Session readiness (exit 75) remains owned by `cli`.
pub fn lifecycle<Setup, Recover, Run, Clean, Fatal>(
    config: &Config,
    setup: Setup,
    mut recover: Recover,
    run: Run,
    cleanup: Clean,
    audio_fatal: Fatal,
) -> i32
where
    Setup: FnOnce() -> bool,
    Recover: FnMut(&Path, i64),
    Run: FnOnce() -> Result<(), ObserverError>,
    Clean: FnOnce() -> Result<(), ObserverError>,
    Fatal: FnOnce() -> bool,
{
    if !setup() {
        return 1;
    }
    recover(&config.captures_dir(), config.segment_interval);
    let run_result = run();
    let cleanup_result = cleanup();
    if let Err(error) = &run_result {
        tracing::error!(?error, "observer run failed");
    }
    if let Err(error) = &cleanup_result {
        tracing::error!(?error, "observer cleanup failed");
    }
    if run_result.is_err() || cleanup_result.is_err() || audio_fatal() {
        1
    } else {
        0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chunking::{AudioLeg, LegBlock, StereoAccumulator};
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        rc::Rc,
        time::Instant,
    };

    #[derive(Clone)]
    struct FakeClock {
        wall: Rc<Cell<f64>>,
        mono: Rc<Cell<f64>>,
        wall_step: Rc<Cell<f64>>,
    }
    impl Clock for FakeClock {
        fn wall_seconds(&self) -> f64 {
            let value = self.wall.get();
            self.wall.set(value + self.wall_step.get());
            value
        }
        fn monotonic_seconds(&self) -> f64 {
            self.mono.get()
        }
    }
    struct FakeVideo {
        healthy: bool,
        starts: Rc<Cell<usize>>,
        stops: Rc<Cell<usize>>,
        fail_start: bool,
        empty_start: bool,
        fail_stop: bool,
        stopped: Vec<StoppedStream>,
    }
    impl VideoCapture for FakeVideo {
        fn start(&mut self, d: &Path, _: i64, _: bool) -> Result<Vec<VideoStream>, String> {
            self.starts.set(self.starts.get() + 1);
            if self.fail_start {
                return Err("start".into());
            }
            if self.empty_start {
                return Ok(vec![]);
            }
            fs::write(d.join("screen.webm"), b"video").unwrap();
            Ok(vec![VideoStream {
                connector: "HDMI-1".into(),
                position: "left".into(),
                file_path: "screen.webm".into(),
            }])
        }
        fn stop(&mut self) -> Result<Vec<StoppedStream>, String> {
            if self.fail_stop {
                return Err("stop".into());
            }
            self.stops.set(self.stops.get() + 1);
            Ok(std::mem::take(&mut self.stopped))
        }
        fn is_healthy(&self) -> bool {
            self.healthy
        }
    }
    struct FakeAudio {
        chunks: VecDeque<DrainedChunk>,
        drains: Rc<Cell<usize>>,
        available: bool,
        fatal: Option<String>,
        stopped: Rc<Cell<bool>>,
        probe_release: std::sync::Arc<std::sync::atomic::AtomicBool>,
        probe_started: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl FakeAudio {
        fn start_hanging_probe(&self) -> std::thread::JoinHandle<()> {
            let release = self.probe_release.clone();
            let started = self.probe_started.clone();
            std::thread::spawn(move || {
                started.store(true, std::sync::atomic::Ordering::Release);
                while !release.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::yield_now();
                }
            })
        }
    }
    impl AudioCapture for FakeAudio {
        fn drain(&mut self) -> DrainedChunk {
            self.drains.set(self.drains.get() + 1);
            self.chunks.pop_front().unwrap_or_else(empty)
        }
        fn audio_available(&self) -> bool {
            self.available
        }
        fn fatal_error(&self) -> Option<String> {
            self.fatal.clone()
        }
        fn stop(&mut self) {
            self.stopped.set(true)
        }
    }
    struct FakeActivity(VecDeque<Result<ActivityState, String>>);
    impl ActivityProbe for FakeActivity {
        fn probe(&mut self) -> Result<ActivityState, String> {
            self.0.pop_front().unwrap_or(Ok(active()))
        }
    }
    struct FakeMute(VecDeque<Result<bool, String>>);
    impl MuteProbe for FakeMute {
        fn probe_muted(&mut self) -> Result<bool, String> {
            self.0.pop_front().unwrap_or(Ok(false))
        }
    }
    #[derive(Clone, Default)]
    struct Writes(Rc<RefCell<Vec<AudioOutputPlan>>>, Rc<Cell<bool>>);
    impl AudioWriter for Writes {
        fn write(&mut self, _: &[f32], p: &AudioOutputPlan, d: &Path) -> Result<(), String> {
            if self.1.get() {
                return Err("write".into());
            }
            self.0.borrow_mut().push(p.clone());
            for f in &p.files {
                let path = d.join(f.filename);
                fs::write(&path, b"flac").unwrap();
            }
            Ok(())
        }
    }
    #[derive(Clone, Default)]
    struct FakeStats(Rc<Cell<usize>>);
    impl CaptureStatsSource for FakeStats {
        fn snapshot(&mut self, _: &Path, _: &str) -> CaptureStats {
            self.0.set(self.0.get() + 1);
            CaptureStats {
                captures_today: self.0.get() as u64,
                total_size_mb: 7,
            }
        }
    }
    #[derive(Clone, Default)]
    struct Events {
        completed: Rc<RefCell<Vec<SegmentCompletedEvent>>>,
    }
    impl EventSink for Events {
        fn segment_completed(&mut self, v: SegmentCompletedEvent) {
            self.completed.borrow_mut().push(v)
        }
    }
    #[derive(Clone, Default)]
    struct States(Rc<RefCell<Vec<StateSnapshot>>>);
    impl StateSink for States {
        fn publish(&mut self, s: StateSnapshot) {
            self.0.borrow_mut().push(s)
        }
    }
    type TestObserver = Observer<
        FakeVideo,
        FakeAudio,
        FakeActivity,
        FakeMute,
        Writes,
        Events,
        FakeClock,
        FakeStats,
        States,
    >;
    #[derive(Clone)]
    struct CountingZone {
        reads: Rc<Cell<usize>>,
    }
    impl ZoneSource for CountingZone {
        fn read_zone(&mut self, wall_seconds: f64) -> ZoneReading {
            self.reads.set(self.reads.get() + 1);
            let dt = chrono::DateTime::from_timestamp(wall_seconds.floor() as i64, 0)
                .expect("valid unix seconds")
                .naive_utc();
            ZoneReading {
                day: dt.format("%Y%m%d").to_string(),
                hms: dt.format("%H%M%S").to_string(),
                utc_offset_seconds: 46800,
                tz: Some("Pacific/Auckland".into()),
            }
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        observer: TestObserver,
        wall: Rc<Cell<f64>>,
        mono: Rc<Cell<f64>>,
        wall_step: Rc<Cell<f64>>,
        starts: Rc<Cell<usize>>,
        stops: Rc<Cell<usize>>,
        drains: Rc<Cell<usize>>,
        audio_stopped: Rc<Cell<bool>>,
        zone_reads: Rc<Cell<usize>>,
        writes: Writes,
        events: Events,
        states: States,
        stats: FakeStats,
    }
    fn active() -> ActivityState {
        ActivityState {
            screen_locked: false,
            power_save: false,
            user_idle: false,
            power_unreadable: false,
        }
    }
    fn idle() -> ActivityState {
        ActivityState {
            screen_locked: true,
            power_save: false,
            user_idle: false,
            power_unreadable: false,
        }
    }

    #[test]
    fn user_idle_only_changes_mode_when_power_is_unreadable() {
        // No 1:1 Python ancestor: user idle is a screen-off proxy only without readable power.
        assert_eq!(mode(ActivityState::default()), Mode::Screencast);
        assert_eq!(
            mode(ActivityState {
                user_idle: true,
                ..ActivityState::default()
            }),
            Mode::Screencast
        );
        assert_eq!(
            mode(ActivityState {
                user_idle: true,
                power_unreadable: true,
                ..ActivityState::default()
            }),
            Mode::Idle
        );
    }
    fn chunk(level: f32) -> DrainedChunk {
        let mut a = StereoAccumulator::default();
        a.push(LegBlock::new(AudioLeg::Microphone, vec![level; 4]));
        a.push(LegBlock::new(AudioLeg::System, vec![0.0; 4]));
        a.drain()
    }
    fn empty() -> DrainedChunk {
        StereoAccumulator::default().drain()
    }
    fn fixture(start_paused: bool) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let zone_reads = Rc::new(Cell::new(0));
        let zone = Box::new(CountingZone {
            reads: zone_reads.clone(),
        });
        fixture_in(start_paused, temp, zone, zone_reads)
    }

    fn fixture_in(
        start_paused: bool,
        temp: tempfile::TempDir,
        zone: Box<dyn ZoneSource>,
        zone_reads: Rc<Cell<usize>>,
    ) -> Fixture {
        let wall = Rc::new(Cell::new(1_700_000_000.0));
        let mono = Rc::new(Cell::new(0.0));
        let wall_step = Rc::new(Cell::new(0.0));
        let starts = Rc::new(Cell::new(0));
        let stops = Rc::new(Cell::new(0));
        let drains = Rc::new(Cell::new(0));
        let audio_stopped = Rc::new(Cell::new(false));
        let writes = Writes::default();
        let events = Events::default();
        let states = States::default();
        let stats = FakeStats::default();
        let mut config = Config {
            base_dir: temp.path().into(),
            stream: "desk".into(),
            start_paused,
            ..Config::default()
        };
        config.segment_interval = 300;
        let backends = Backends {
            video: FakeVideo {
                healthy: true,
                starts: starts.clone(),
                stops: stops.clone(),
                fail_start: false,
                empty_start: false,
                fail_stop: false,
                stopped: vec![],
            },
            audio: FakeAudio {
                chunks: VecDeque::new(),
                drains: drains.clone(),
                available: true,
                fatal: None,
                stopped: audio_stopped.clone(),
                probe_release: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                probe_started: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            activity: FakeActivity(VecDeque::new()),
            mute: FakeMute(VecDeque::new()),
            writer: writes.clone(),
            events: events.clone(),
            clock: FakeClock {
                wall: wall.clone(),
                mono: mono.clone(),
                wall_step: wall_step.clone(),
            },
            stats: stats.clone(),
            states: states.clone(),
        };
        Fixture {
            _temp: temp,
            observer: Observer::new(config, backends, zone),
            wall,
            mono,
            wall_step,
            starts,
            stops,
            drains,
            audio_stopped,
            zone_reads,
            writes,
            events,
            states,
            stats,
        }
    }
    fn initialize(f: &mut Fixture) {
        f.observer.initialize().unwrap()
    }

    #[test]
    fn existing_incomplete_segment_is_not_adopted_or_overwritten() {
        let mut f = fixture(false);
        let reading = f.observer.zone.read_zone(f.wall.get());
        f.zone_reads.set(0);
        let (day, time) = (reading.day, reading.hms);
        let dir = f
            .observer
            .config
            .captures_dir()
            .join(day)
            .join("desk")
            .join(format!("{time}.incomplete"));
        fs::create_dir_all(&dir).unwrap();
        let files = [
            ("screen.webm", b"first hour".as_slice()),
            (".metadata", b"first progress".as_slice()),
            (CAPTURE_ZONE_FILENAME, b"first zone".as_slice()),
        ];
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).unwrap();
        }
        let bumped_dir = f.observer.start_segment().unwrap();
        assert_ne!(bumped_dir, dir);
        for (name, bytes) in files {
            assert_eq!(fs::read(dir.join(name)).unwrap(), bytes);
        }
        assert_eq!(f.observer.state.segment_dir, Some(bumped_dir));
        assert_eq!(f.starts.get(), 0);
        assert!(f.events.completed.borrow().is_empty());
    }

    #[test]
    fn final_name_collision_preserves_media_and_recovery_metadata() {
        let mut f = fixture(false);
        let dir = f.observer.start_segment().unwrap();
        fs::write(dir.join("screen.webm"), b"second hour").unwrap();
        let metadata = fs::read(dir.join(".metadata")).unwrap();
        let zone = fs::read(dir.join(CAPTURE_ZONE_FILENAME)).unwrap();
        f.wall.set(f.wall.get() + 300.0);
        f.mono.set(f.mono.get() + 300.0);
        let stem = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".incomplete"))
            .unwrap();
        let destination = dir.with_file_name(segment_key(stem, 300));
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("screen.webm"), b"first hour").unwrap();

        assert!(matches!(
            f.observer.finalize_segment(),
            Err(ObserverError::Io(_))
        ));
        assert_eq!(fs::read(dir.join("screen.webm")).unwrap(), b"second hour");
        assert_eq!(fs::read(dir.join(".metadata")).unwrap(), metadata);
        assert_eq!(fs::read(dir.join(CAPTURE_ZONE_FILENAME)).unwrap(), zone);
        assert_eq!(
            fs::read(destination.join("screen.webm")).unwrap(),
            b"first hour"
        );
        assert!(f.events.completed.borrow().is_empty());
    }

    pub(crate) fn drive_real_observer_ticks() {
        let mut fixture = fixture(true);
        initialize(&mut fixture);
        let published_after_initialize = fixture.states.0.borrow().len();
        fixture.observer.tick().unwrap();
        fixture.observer.tick().unwrap();
        assert_eq!(
            fixture.states.0.borrow().len(),
            published_after_initialize + 2
        );
    }

    // tests/test_observer.py::test_compute_rms_mic_left_sys_right
    // Already covered by chunking::tests::rms_and_hit_gate_match_chunk_contract; this pins consumption.
    #[test]
    fn observer_consumes_landed_hit_gate() {
        let mut g = HitGate::default();
        let mut a = crate::chunking::StereoAccumulator::default();
        a.push(crate::chunking::LegBlock::new(
            crate::chunking::AudioLeg::Microphone,
            vec![0.0],
        ));
        a.push(crate::chunking::LegBlock::new(
            crate::chunking::AudioLeg::System,
            vec![0.02],
        ));
        assert!(g.observe(&a.drain()));
    }

    // tests/test_observer.py::test_finalize_segment_clamps_duration_to_interval
    // tests/test_observer.py::test_finalize_segment_floor_is_one
    // No 1:1 Python ancestor: wall/monotonic separation.
    #[test]
    fn interval_uses_monotonic_and_wall_jump_does_not_rotate() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.wall.set(f.wall.get() + 10_000.0);
        f.observer.tick().unwrap();
        assert_eq!(f.events.completed.borrow().len(), 0);
        f.mono.set(300.0);
        f.observer.tick().unwrap();
        assert_eq!(f.events.completed.borrow().len(), 1)
    }
    // A suspend stops monotonic time while wall time keeps going. The segment open
    // across it keeps its own start and its length is the capture time; the next
    // segment is named and filed by the real wall time.
    #[test]
    fn segment_after_suspend_uses_real_wall_time_and_capture_length() {
        let mut f = fixture(false);
        initialize(&mut f);
        let opened = f.wall.get();
        let first = f.observer.state.segment_dir.clone().unwrap();
        f.mono.set(60.0);
        f.wall.set(opened + 60.0);
        f.observer.tick().unwrap();
        // Eight hours suspended: wall time moves, monotonic time does not.
        let suspend = 8.0 * 3600.0;
        f.wall.set(opened + 60.0 + suspend);
        f.observer.tick().unwrap();
        assert!(f.events.completed.borrow().is_empty());
        // Capture continues for another minute, then a mute change closes the segment.
        f.mono.set(120.0);
        let resumed_wall = opened + 120.0 + suspend;
        f.wall.set(resumed_wall);
        f.observer.backends.mute.0.push_back(Ok(true));
        f.observer.tick().unwrap();

        let completed = f.events.completed.borrow();
        assert_eq!(completed.len(), 1);
        let first_stem = first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_suffix(".incomplete")
            .unwrap()
            .to_owned();
        let (stem, len) = completed[0].key.split_once('_').unwrap();
        assert_eq!(stem, first_stem);
        // Two minutes of capture, not eight hours of wall time.
        assert_eq!(len, "120");

        let expected = f.observer.zone.read_zone(resumed_wall);
        assert_ne!(
            expected.day,
            f.observer.zone.read_zone(opened).day,
            "the suspend crosses midnight"
        );
        let next = f.observer.state.segment_dir.clone().unwrap();
        assert_eq!(
            next,
            f.observer
                .config
                .captures_dir()
                .join(&expected.day)
                .join("desk")
                .join(format!("{}.incomplete", expected.hms))
        );
        assert_eq!(f.observer.state.segment_start_wall, resumed_wall);
    }
    // No 1:1 Python ancestor: interval+mute witness.
    #[test]
    fn interval_and_mute_flip_rotate_once() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.mono.set(300.0);
        f.observer.backends.mute.0.push_back(Ok(true));
        f.observer.tick().unwrap();
        assert_eq!(f.events.completed.borrow().len(), 1);
    }
    // tests/test_observer.py::test_start_paused_false_starts_capture
    // No 1:1 Python ancestor: screencast entry and exit triggers.
    #[test]
    fn screencast_entry_and_exit_rotate() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        f.observer.backends.activity.0.push_back(Ok(active()));
        f.observer.tick().unwrap();
        assert_eq!(f.starts.get(), 1);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        assert_eq!(f.stops.get(), 1);
        assert_eq!(f.events.completed.borrow().len(), 1)
    }
    // tests/test_observer.py::test_save_audio_segment_unmuted_writes_combined
    #[test]
    fn two_hits_discard_three_hits_save_and_reset() {
        let mut f = fixture(false);
        initialize(&mut f);
        for _ in 0..2 {
            f.observer.backends.audio.chunks.push_back(chunk(0.02));
            f.observer.tick().unwrap()
        }
        f.observer.handle_boundary(Mode::Screencast).unwrap();
        assert!(f.writes.0.borrow().is_empty());
        assert_eq!(f.observer.state.hit_gate.hits(), 0);
        f.wall.set(f.wall.get() + 2.0);
        for _ in 0..3 {
            f.observer.backends.audio.chunks.push_back(chunk(0.02));
            f.observer.tick().unwrap()
        }
        assert_eq!(f.observer.state.hit_gate.hits(), 3);
        f.observer.handle_boundary(Mode::Screencast).unwrap();
        assert_eq!(f.writes.0.borrow().len(), 1);
        assert_eq!(f.observer.state.hit_gate.hits(), 0)
    }
    // tests/test_observer.py::test_save_audio_segment_muted_writes_split_files
    #[test]
    fn mute_flip_saves_pre_flip_plan_then_adopts_new_mute() {
        let mut f = fixture(false);
        initialize(&mut f);
        for _ in 0..3 {
            f.observer.backends.audio.chunks.push_back(chunk(0.02));
            f.observer.tick().unwrap()
        }
        f.observer.backends.mute.0.push_back(Ok(true));
        f.observer.tick().unwrap();
        let w = f.writes.0.borrow();
        assert_eq!(w[0], audio_output_plan(false));
        drop(w);
        assert!(f.observer.state.segment_is_muted)
    }
    // tests/test_observer.py::test_observer_init_not_paused
    // tests/test_observer.py::test_start_paused_true_skips_initial_capture
    #[test]
    fn start_paused_is_observable_and_has_no_segment() {
        let mut f = fixture(true);
        initialize(&mut f);
        assert!(f.observer.state.paused);
        assert!(f.observer.state.segment_dir.is_none());
        assert!(f.states.0.borrow().last().unwrap().paused)
    }
    /// A second observer on the same data directory: the app after a restart.
    fn restarted(previous: &Fixture) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let state = Config {
            base_dir: temp.path().into(),
            ..Config::default()
        }
        .state_dir();
        let held = previous.observer.config.state_dir().join("pause.json");
        if held.exists() {
            fs::create_dir_all(&state).unwrap();
            fs::copy(held, state.join("pause.json")).unwrap();
        }
        let zone_reads = Rc::new(Cell::new(0));
        let zone = Box::new(CountingZone {
            reads: zone_reads.clone(),
        });
        fixture_in(false, temp, zone, zone_reads)
    }
    #[test]
    fn until_resumed_survives_a_restart_and_captures_nothing() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.pause(0);
        let mut after = restarted(&f);
        assert!(after.observer.state.paused);
        initialize(&mut after);
        for _ in 0..3 {
            after.mono.set(after.mono.get() + 3600.0);
            after.wall.set(after.wall.get() + 3600.0);
            after.observer.tick().unwrap();
        }
        assert_eq!(
            after.starts.get(),
            0,
            "nothing starts while the pause is held"
        );
        assert!(after.observer.state.segment_dir.is_none());
        let published = after.states.0.borrow().last().unwrap().clone();
        assert!(published.paused);
        assert_eq!(published.pause_until, None);
        // Resume ends it, and the next restart observes.
        after.observer.resume();
        after.observer.tick().unwrap();
        assert_eq!(after.starts.get(), 1);
        let mut again = restarted(&after);
        assert!(!again.observer.state.paused);
        initialize(&mut again);
        assert_eq!(again.starts.get(), 1);
    }
    #[test]
    fn timed_pause_survives_a_restart_and_ends_at_its_original_deadline() {
        let mut f = fixture(false);
        initialize(&mut f);
        // Paused for 15 minutes, 600 seconds before the restart below.
        let restart_at = f.wall.get();
        f.wall.set(restart_at - 600.0);
        f.observer.pause(900);
        let mut after = restarted(&f);
        assert!(after.observer.state.paused);
        assert_eq!(after.observer.state.pause_until, Some(300.0));
        initialize(&mut after);
        assert_eq!(after.starts.get(), 0);
        after.wall.set(restart_at + 299.0);
        after.observer.tick().unwrap();
        assert!(after.observer.state.paused);
        // The monotonic clock has not moved (a suspend, say); the wall deadline still ends it.
        after.wall.set(restart_at + 300.0);
        after.observer.tick().unwrap();
        assert!(!after.observer.state.paused);
        assert_eq!(after.starts.get(), 1);
        assert!(
            !after
                .observer
                .config
                .state_dir()
                .join("pause.json")
                .exists()
        );
    }
    #[test]
    fn timed_pause_restarted_after_its_deadline_observes() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.wall.set(f.wall.get() - 1_000.0);
        f.observer.pause(900);
        let mut after = restarted(&f);
        assert!(!after.observer.state.paused);
        initialize(&mut after);
        assert_eq!(after.starts.get(), 1);
        assert!(
            !after
                .observer
                .config
                .state_dir()
                .join("pause.json")
                .exists()
        );
    }
    // No 1:1 test: observer.py:611-634 agrees pause finalizes on the next tick.
    #[test]
    fn pause_finalizes_next_tick_and_drains_without_growth() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.state.frames = vec![1.0, 0.0];
        f.observer.pause(0);
        assert!(f.observer.state.segment_dir.is_some());
        f.observer.backends.audio.chunks.push_back(chunk(0.5));
        f.observer.tick().unwrap();
        assert!(f.observer.state.segment_dir.is_none());
        assert!(f.observer.state.frames.is_empty());
        assert_eq!(f.drains.get(), 1)
    }
    // No 1:1 Python ancestor: gated pause save, capture length, and timed resume.
    #[test]
    fn paused_finalize_saves_three_hits_with_capture_length_and_timed_pause_resumes() {
        let mut f = fixture(false);
        initialize(&mut f);
        for _ in 0..3 {
            f.observer.backends.audio.chunks.push_back(chunk(0.02));
            f.observer.tick().unwrap()
        }
        f.wall.set(f.wall.get() + 999.0);
        f.observer.pause(5);
        f.mono.set(f.mono.get() + 4.0);
        f.observer.tick().unwrap();
        assert!(f.observer.state.paused);
        f.mono.set(f.mono.get() + 1.0);
        f.observer.tick().unwrap();
        assert!(!f.observer.state.paused);
        assert_eq!(f.writes.0.borrow().len(), 1);
        // The wall jump does not stretch the length: four seconds of capture ran.
        assert!(f.events.completed.borrow()[0].key.ends_with("_4"))
    }
    // No 1:1 test: observer.py:663-733 agrees watchdog restart is same-tick and fatal.
    #[test]
    fn watchdog_active_restarts_same_tick_and_failure_is_fatal() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.backends.video.healthy = false;
        f.observer.backends.video.fail_start = true;
        let e = f.observer.tick().unwrap_err();
        assert!(matches!(e, ObserverError::VideoStart(_)));
        assert_eq!(f.stops.get(), 1);
        assert_eq!(f.starts.get(), 2)
    }
    // No 1:1 Python ancestor: unhealthy idle continues audio-only.
    #[test]
    fn watchdog_idle_continues_audio_only() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.backends.video.healthy = false;
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.mode, Mode::Idle)
    }
    // real Observer state branches all flow through run::tick_once's single watchdog point.
    #[test]
    fn runtime_watchdog_covers_real_observer_state_matrix() {
        use crate::run::{ServiceNotifier, tick_once};
        struct Notifier(std::sync::atomic::AtomicUsize);
        impl ServiceNotifier for Notifier {
            fn ready(&self) -> io::Result<()> {
                Ok(())
            }
            fn watchdog(&self) -> io::Result<()> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                Ok(())
            }
            fn stopping(&self) -> io::Result<()> {
                Ok(())
            }
        }
        let notifier = Notifier(std::sync::atomic::AtomicUsize::new(0));

        let mut timed = fixture(false);
        initialize(&mut timed);
        timed.observer.pause(30);
        tick_once(&notifier, || timed.observer.tick()).unwrap();

        let mut indefinite = fixture(false);
        initialize(&mut indefinite);
        indefinite.observer.pause(0);
        tick_once(&notifier, || indefinite.observer.tick()).unwrap();

        let mut idle_state = fixture(false);
        initialize(&mut idle_state);
        idle_state
            .observer
            .backends
            .activity
            .0
            .push_back(Ok(idle()));
        tick_once(&notifier, || idle_state.observer.tick()).unwrap();

        let mut unhealthy = fixture(false);
        initialize(&mut unhealthy);
        unhealthy.observer.backends.video.healthy = false;
        tick_once(&notifier, || unhealthy.observer.tick()).unwrap();

        assert_eq!(notifier.0.load(std::sync::atomic::Ordering::Acquire), 4);
    }
    // No 1:1 Python ancestor: startup activity failure defaults to screencast.
    #[test]
    fn startup_activity_failure_defaults_screencast_and_start_failure_is_fatal() {
        let mut f = fixture(false);
        f.observer
            .backends
            .activity
            .0
            .push_back(Err("probe".into()));
        f.observer.backends.video.fail_start = true;
        assert!(matches!(
            f.observer.initialize(),
            Err(ObserverError::VideoStart(_))
        ))
    }
    // No 1:1 Python ancestor: empty stream lists follow initialize_screencast's RuntimeError policy.
    #[test]
    fn empty_streams_are_fatal_at_boundary_and_fall_back_on_resume() {
        let mut fatal = fixture(false);
        initialize(&mut fatal);
        fatal.observer.backends.video.empty_start = true;
        assert!(matches!(
            fatal.observer.handle_boundary(Mode::Screencast),
            Err(ObserverError::VideoStart(_))
        ));
        let mut resume = fixture(true);
        initialize(&mut resume);
        resume.observer.resume();
        resume.observer.backends.video.empty_start = true;
        resume.observer.tick().unwrap();
        assert!(resume.observer.state.segment_dir.is_some());
    }
    // No 1:1 Python ancestor: regular and resume activity failures keep mode.
    #[test]
    fn activity_failure_regular_and_resume_keep_mode() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.backends.activity.0.push_back(Err("x".into()));
        f.observer.backends.mute.0.push_back(Ok(true));
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.mode, Mode::Screencast);
        assert!(!f.observer.state.cached_is_muted);
        assert_eq!(f.observer.backends.mute.0.len(), 1);
        f.observer.finalize_segment().unwrap();
        f.observer.backends.activity.0.push_back(Err("x".into()));
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.mode, Mode::Screencast);
        assert!(!f.observer.state.cached_is_muted)
    }
    // No 1:1 Python ancestor: resume failure calls start_segment twice, potentially orphaning the first directory.
    #[test]
    fn resume_reopen_failure_falls_back_and_double_starts() {
        let mut f = fixture(true);
        initialize(&mut f);
        f.observer.resume();
        f.observer.backends.video.fail_start = true;
        f.wall_step.set(1.0);
        f.observer.tick().unwrap();
        assert_eq!(f.starts.get(), 1);
        assert!(f.observer.state.segment_dir.is_some());
        assert_eq!(
            fs::read_dir(
                f.observer
                    .state
                    .segment_dir
                    .as_ref()
                    .unwrap()
                    .parent()
                    .unwrap()
            )
            .unwrap()
            .count(),
            2
        );
        assert_eq!(f.zone_reads.get(), 2);
    }
    // tests/test_observer.py::test_async_run_returns_1_when_audio_recorder_has_fatal_error
    #[test]
    fn audio_fatal_surfaces_after_clean_shutdown() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.backends.audio.fatal = Some("fatal".into());
        assert!(matches!(
            f.observer.shutdown(),
            Err(ObserverError::AudioFatal(_))
        ));
        assert!(f.audio_stopped.get());
        assert!(f.observer.state.segment_dir.is_none())
    }
    // tests/test_observer.py::test_async_run_returns_1_when_main_loop_runtime_error
    // tests/test_observer.py::test_async_run_returns_0_on_normal_main_loop_return
    // tests/test_observer.py::test_async_run_returns_0_when_audio_degraded_no_fatal
    #[test]
    fn lifecycle_maps_outcomes_and_always_cleans_after_run() {
        let f = fixture(false);
        let cleaned = Rc::new(Cell::new(false));
        let clean = cleaned.clone();
        assert_eq!(
            lifecycle(
                &f.observer.config,
                || true,
                |_, _| {},
                || Err(ObserverError::VideoStart("x".into())),
                move || {
                    clean.set(true);
                    Ok(())
                },
                || false
            ),
            1
        );
        assert!(cleaned.get());
        assert_eq!(
            lifecycle(
                &f.observer.config,
                || true,
                |_, _| {},
                || Ok(()),
                || Ok(()),
                || false
            ),
            0
        );
        let order = Rc::new(RefCell::new(Vec::new()));
        let cleaned = order.clone();
        let checked = order.clone();
        assert_eq!(
            lifecycle(
                &f.observer.config,
                || true,
                |_, _| {},
                || Ok(()),
                move || {
                    cleaned.borrow_mut().push("cleanup");
                    Ok(())
                },
                move || {
                    checked.borrow_mut().push("fatal");
                    true
                }
            ),
            1
        );
        assert_eq!(&*order.borrow(), &["cleanup", "fatal"]);
    }
    // tests/test_observer.py::test_initial_screencast_failure_runs_shutdown_and_propagates
    #[test]
    fn lifecycle_initial_capture_failure_runs_cleanup() {
        let f = fixture(false);
        let cleaned = Rc::new(Cell::new(false));
        let seen = cleaned.clone();
        let code = lifecycle(
            &f.observer.config,
            || true,
            |_, _| {},
            || Err(ObserverError::VideoStart("portal".into())),
            move || {
                seen.set(true);
                Ok(())
            },
            || false,
        );
        assert_eq!(code, 1);
        assert!(cleaned.get());
    }
    // tests/test_observer.py::test_degraded_segment_finalizes_with_video_only.
    #[test]
    fn completed_once_for_nonempty_and_none_for_empty() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.handle_boundary(Mode::Idle).unwrap();
        assert_eq!(f.events.completed.borrow().len(), 1);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        f.observer.finalize_segment().unwrap();
        assert!(!dir.exists());
        assert_eq!(f.events.completed.borrow().len(), 1)
    }
    // No 1:1 Python ancestor: empty cleanup races are nonfatal like observer.py:322-330.
    #[test]
    fn empty_segment_rmdir_failure_is_nonfatal() {
        let mut f = fixture(true);
        initialize(&mut f);
        f.observer.resume();
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        fs::create_dir(
            f.observer
                .state
                .segment_dir
                .as_ref()
                .unwrap()
                .join("racing-child"),
        )
        .unwrap();
        f.observer.finalize_segment().unwrap();
        assert!(f.events.completed.borrow().is_empty())
    }
    // tests/test_observer.py::test_hanging_redetect_does_not_block_tick
    #[test]
    fn hung_audio_probe_does_not_block_tick() {
        let mut f = fixture(false);
        initialize(&mut f);
        let handle = f.observer.backends.audio.start_hanging_probe();
        while !f
            .observer
            .backends
            .audio
            .probe_started
            .load(std::sync::atomic::Ordering::Acquire)
        {
            std::thread::yield_now();
        }
        let began = Instant::now();
        f.observer.tick().unwrap();
        assert!(began.elapsed().as_millis() < 200);
        f.observer
            .backends
            .audio
            .probe_release
            .store(true, std::sync::atomic::Ordering::Release);
        handle.join().unwrap()
    }
    // No 1:1 Python ancestor: Observer::shutdown() is the deterministic seam a signal handler will call.
    #[test]
    fn shutdown_mid_segment_saves_and_finalizes() {
        let mut f = fixture(false);
        initialize(&mut f);
        for _ in 0..3 {
            f.observer.backends.audio.chunks.push_back(chunk(0.02));
            f.observer.tick().unwrap()
        }
        f.observer.shutdown().unwrap();
        assert_eq!(f.writes.0.borrow().len(), 1);
        assert_eq!(f.events.completed.borrow().len(), 1)
    }
    // No 1:1 Python ancestor: typed backend/filesystem failures remain distinguishable.
    #[test]
    fn typed_stop_write_and_io_errors_propagate() {
        let mut stop = fixture(false);
        initialize(&mut stop);
        stop.observer.backends.video.fail_stop = true;
        assert!(matches!(
            stop.observer.shutdown(),
            Err(ObserverError::VideoStop(_))
        ));
        let mut write = fixture(false);
        initialize(&mut write);
        for _ in 0..3 {
            write.observer.backends.audio.chunks.push_back(chunk(0.02));
            write.observer.tick().unwrap();
        }
        write.observer.backends.writer.1.set(true);
        assert!(matches!(
            write.observer.shutdown(),
            Err(ObserverError::AudioWrite(_))
        ));
        let mut io = fixture(true);
        io.observer.resume();
        fs::write(io.observer.config.captures_dir(), b"blocker").unwrap();
        let result = io.observer.tick();
        assert!(matches!(result, Err(ObserverError::Io(_))), "{result:?}");
    }
    // No 1:1 Python ancestor: first/60s/paused stats and snapshot.
    #[test]
    fn stats_first_tick_then_sixty_seconds_and_while_paused() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.pause(0);
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.last_stats_refresh, 0.0);
        assert_eq!(f.stats.0.get(), 1);
        f.mono.set(59.0);
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.last_stats_refresh, 0.0);
        assert_eq!(f.stats.0.get(), 1);
        f.mono.set(60.0);
        f.observer.tick().unwrap();
        assert_eq!(f.observer.state.last_stats_refresh, 60.0);
        assert_eq!(f.stats.0.get(), 2);
        assert_eq!(f.states.0.borrow().last().unwrap().captures_today, 2)
    }
    // tests/test_observer.py::test_pause_state_fields_exist; ordered subscription.
    #[test]
    fn subscriber_observes_pause_resume_in_order() {
        let mut f = fixture(false);
        initialize(&mut f);
        f.observer.pause(0);
        f.observer.resume();
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        let states = f.states.0.borrow();
        assert_eq!(
            states
                .iter()
                .map(|s| (s.mode, s.paused, s.segment_open))
                .collect::<Vec<_>>(),
            vec![
                (Mode::Screencast, false, true),
                (Mode::Screencast, true, true),
                (Mode::Screencast, false, true),
                (Mode::Idle, false, true)
            ]
        );
        assert_eq!(states.last().unwrap().total_size_mb, 7)
    }
    // tests/test_observer.py::test_async_run_returns_0_on_normal_main_loop_return
    // tests/test_observer.py::test_async_run_returns_1_and_skips_recovery_when_setup_fails
    #[test]
    fn recovery_runs_once_with_interval_and_skips_setup_failure() {
        let f = fixture(false);
        let calls = Rc::new(RefCell::new(Vec::new()));
        let recorded = calls.clone();
        assert_eq!(
            lifecycle(
                &f.observer.config,
                || true,
                move |path, ceiling| recorded.borrow_mut().push((path.to_path_buf(), ceiling)),
                || Ok(()),
                || Ok(()),
                || false
            ),
            0
        );
        assert_eq!(&*calls.borrow(), &[(f.observer.config.captures_dir(), 300)]);
        let skipped = Rc::new(Cell::new(0));
        let seen = skipped.clone();
        assert_eq!(
            lifecycle(
                &f.observer.config,
                || false,
                move |_, _| seen.set(seen.get() + 1),
                || Ok(()),
                || Ok(()),
                || false
            ),
            1
        );
        assert_eq!(skipped.get(), 0)
    }
    // audit imports rather than prose, so comments cannot false-positive.
    #[test]
    fn observer_dependency_surface_contains_no_network_backend() {
        let source = include_str!("observer.rs");
        let mut uses = Vec::new();
        let mut current = String::new();
        for line in source.lines().map(str::trim) {
            if !current.is_empty() || line.starts_with("use ") {
                current.push_str(line);
            }
            if !current.is_empty() && line.ends_with(';') {
                uses.push(std::mem::take(&mut current));
            }
        }
        for forbidden in ["reqwest", "hyper", "socket", "upload", "sync"] {
            assert!(
                !uses.iter().any(|statement| statement.contains(forbidden)),
                "{forbidden}"
            );
        }
    }

    fn sidecar(dir: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join(".metadata")).unwrap()).unwrap()
    }

    // open sidecar is default progress with last_durable_write_at omitted.
    #[test]
    fn open_metadata_is_default_progress() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let meta = sidecar(f.observer.state.segment_dir.as_ref().unwrap());
        assert_eq!(meta["start_timestamp"], 1_700_000_000.0);
        assert_eq!(meta["has_durable_media"], false);
        assert_eq!(meta["durable_byte_count"], 0);
        assert!(meta.get("last_durable_write_at").is_none());
    }

    // production tick refresh sees a test-planted file. Do not call the writer.
    #[test]
    fn tick_refresh_stamps_observer_wall() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        fs::write(dir.join("planted.bin"), b"abcd").unwrap();
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        let meta = sidecar(&dir);
        assert_eq!(meta["has_durable_media"], true);
        assert_eq!(meta["durable_byte_count"], 4);
        assert_eq!(meta["last_durable_write_at"], 1_700_000_000.0);
        assert_eq!(meta["start_timestamp"], 1_700_000_000.0);
    }

    // a tick with no media omits last_durable_write_at.
    #[test]
    fn tick_refresh_without_media_omits_write_at() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        let meta = sidecar(&dir);
        assert_eq!(meta["has_durable_media"], false);
        assert_eq!(meta["durable_byte_count"], 0);
        assert!(meta.get("last_durable_write_at").is_none());
    }

    // start_timestamp is write-once from state; last_durable_write_at is the refresh wall.
    #[test]
    fn tick_refresh_keeps_open_start_timestamp() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        fs::write(dir.join("planted.bin"), b"x").unwrap();
        f.wall.set(1_700_000_050.0);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();
        let meta = sidecar(&dir);
        assert_eq!(meta["start_timestamp"], 1_700_000_000.0);
        assert_eq!(meta["last_durable_write_at"], 1_700_000_050.0);
        assert_eq!(
            crate::recovery::read_segment_start(&dir),
            Some(1_700_000_000.0)
        );
    }

    // Idle→Idle boundary that keeps planted media completes.
    #[test]
    fn idle_boundary_with_media_completes() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        fs::write(
            f.observer
                .state
                .segment_dir
                .as_ref()
                .unwrap()
                .join("kept.bin"),
            b"keep",
        )
        .unwrap();
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.mono.set(300.0);
        f.observer.tick().unwrap();
        assert_eq!(f.events.completed.borrow().len(), 1);
    }

    #[test]
    fn capture_zone_written_on_init_and_unchanged_on_tick() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        let zone = crate::recovery::read_capture_zone(&dir).expect("capture zone exists");
        assert_eq!(zone.utc_offset_seconds, 46800);
        assert_eq!(zone.tz, Some("Pacific/Auckland".into()));
        let initial_bytes = fs::read(dir.join(CAPTURE_ZONE_FILENAME)).unwrap();

        // Advance wall clock and tick
        f.wall.set(1_700_000_050.0);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.observer.tick().unwrap();

        let meta = sidecar(&dir);
        assert_eq!(meta["start_timestamp"], 1_700_000_000.0);
        let updated_bytes = fs::read(dir.join(CAPTURE_ZONE_FILENAME)).unwrap();
        assert_eq!(initial_bytes, updated_bytes);
        assert_eq!(f.zone_reads.get(), 1);
    }

    #[test]
    fn idle_boundary_with_media_preserves_capture_zone() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        let initial_bytes = fs::read(dir.join(CAPTURE_ZONE_FILENAME)).unwrap();
        fs::write(dir.join("screen.webm"), b"video").unwrap();

        f.observer.backends.activity.0.push_back(Ok(idle()));
        f.mono.set(300.0);
        f.observer.tick().unwrap();
        assert_eq!(f.events.completed.borrow().len(), 1);

        let sealed_key = &f.events.completed.borrow()[0].key;
        let sealed_dir = dir.with_file_name(sealed_key);
        assert!(sealed_dir.exists());
        assert!(!sealed_dir.join(crate::recovery::METADATA_FILENAME).exists());
        let sealed_bytes = fs::read(sealed_dir.join(CAPTURE_ZONE_FILENAME)).unwrap();
        assert_eq!(initial_bytes, sealed_bytes);
    }

    #[test]
    fn pause_finalize_with_no_media_cleans_up_capture_zone() {
        let mut f = fixture(false);
        f.observer.backends.activity.0.push_back(Ok(idle()));
        initialize(&mut f);
        let dir = f.observer.state.segment_dir.clone().unwrap();
        assert!(dir.join(CAPTURE_ZONE_FILENAME).exists());

        f.observer.pause(0);
        f.observer.tick().unwrap();

        assert!(!dir.exists());
        assert!(!dir.join(CAPTURE_ZONE_FILENAME).exists());
        assert_eq!(f.events.completed.borrow().len(), 0);
    }

    #[test]
    fn repeated_hour_preserves_planted_and_finalizes_bumped() {
        use crate::segment::DeviceZoneReader;
        let temp = tempfile::tempdir().unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/zoneinfo");
        let link = temp.path().join("localtime");
        let reader = DeviceZoneReader::new(
            Some(std::ffi::OsString::from("Europe/Berlin")),
            link,
            vec![root],
        );
        let zone_reads = Rc::new(Cell::new(0));
        let mut f = fixture_in(false, temp, Box::new(reader), zone_reads);

        let planted_dir = f
            .observer
            .config
            .captures_dir()
            .join("20261025/desk/023000_300");
        fs::create_dir_all(&planted_dir).unwrap();
        fs::write(planted_dir.join("screen.webm"), b"planted media").unwrap();

        f.wall.set(1792891800.0);
        let dir = f.observer.start_segment().unwrap();
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "023001.incomplete"
        );
        fs::write(dir.join("screen.webm"), b"new video").unwrap();

        f.wall.set(1792891800.0 + 300.0);
        f.mono.set(f.mono.get() + 300.0);
        f.observer.finalize_segment().unwrap();

        assert_eq!(
            fs::read(planted_dir.join("screen.webm")).unwrap(),
            b"planted media"
        );

        let final_dir = f
            .observer
            .config
            .captures_dir()
            .join("20261025/desk/023001_300");
        assert!(final_dir.exists());
        let zone = crate::recovery::read_capture_zone(&final_dir).unwrap();
        assert_eq!(zone.tz, Some("Europe/Berlin".into()));
        assert_eq!(zone.utc_offset_seconds, 3600);

        assert!(f.observer.start_segment().is_ok());
    }

    #[test]
    fn travel_link_retargeting() {
        use crate::segment::DeviceZoneReader;
        let temp = tempfile::tempdir().unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/zoneinfo");
        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(root.join("Asia/Kolkata"), &link).unwrap();

        let reader = DeviceZoneReader::new(None, link.clone(), vec![root.clone()]);
        let zone_reads = Rc::new(Cell::new(0));
        let mut f = fixture_in(false, temp, Box::new(reader), zone_reads);

        f.wall.set(1790706000.0);
        let dir1 = f.observer.start_segment().unwrap();
        assert_eq!(
            dir1.file_name().unwrap().to_str().unwrap(),
            "235000.incomplete"
        );
        fs::write(dir1.join("screen.webm"), b"video1").unwrap();
        f.wall.set(1790706000.0 + 300.0);
        f.mono.set(f.mono.get() + 300.0);
        f.observer.finalize_segment().unwrap();

        let final_dir1 = f
            .observer
            .config
            .captures_dir()
            .join("20260929/desk/235000_300");
        assert!(final_dir1.exists());
        let zone1 = crate::recovery::read_capture_zone(&final_dir1).unwrap();
        assert_eq!(zone1.tz, Some("Asia/Kolkata".into()));
        assert_eq!(zone1.utc_offset_seconds, 19800);

        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(root.join("Pacific/Auckland"), &link).unwrap();

        f.wall.set(1790706300.0);
        let dir2 = f.observer.start_segment().unwrap();
        let expected_dir2 = f
            .observer
            .config
            .captures_dir()
            .join("20260930/desk/072500.incomplete");
        assert_eq!(dir2, expected_dir2);
        let zone2 = crate::recovery::read_capture_zone(&dir2).unwrap();
        assert_eq!(zone2.tz, Some("Pacific/Auckland".into()));
        assert_eq!(zone2.utc_offset_seconds, 46800);
    }

    #[test]
    fn one_read_per_new_segment() {
        let mut f = fixture(false);
        assert_eq!(f.zone_reads.get(), 0);

        let _dir1 = f.observer.start_segment().unwrap();
        assert_eq!(f.zone_reads.get(), 1);

        let _dir1_again = f.observer.start_segment().unwrap();
        assert_eq!(f.zone_reads.get(), 1);

        f.observer.finalize_segment().unwrap();
        f.wall.set(f.wall.get() + 10.0);
        let _dir2 = f.observer.start_segment().unwrap();
        assert_eq!(f.zone_reads.get(), 2);
    }

    #[test]
    fn chained_stem_bump() {
        let mut f = fixture(false);
        f.wall.set(1704076200.0);
        let base = f.observer.config.captures_dir().join("20240101/desk");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir(base.join("023000_300")).unwrap();
        fs::create_dir(base.join("023001_300")).unwrap();

        let dir = f.observer.start_segment().unwrap();
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "023002.incomplete"
        );
    }

    #[test]
    fn failed_dir_alone_bumps() {
        let mut f = fixture(false);
        f.wall.set(1704076200.0);
        let base = f.observer.config.captures_dir().join("20240101/desk");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir(base.join("023000.failed")).unwrap();

        let dir = f.observer.start_segment().unwrap();
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "023001.incomplete"
        );

        let mut f2 = fixture(false);
        f2.wall.set(1704076200.0);
        let base2 = f2.observer.config.captures_dir().join("20240101/desk");
        fs::create_dir_all(&base2).unwrap();
        fs::create_dir(base2.join("023000_300.failed")).unwrap();

        let dir2 = f2.observer.start_segment().unwrap();
        assert_eq!(
            dir2.file_name().unwrap().to_str().unwrap(),
            "023001.incomplete"
        );
    }

    #[test]
    fn midnight_rollover_bump() {
        let mut f = fixture(false);
        f.wall.set(1798761599.0);
        let base = f.observer.config.captures_dir().join("20261231/desk");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir(base.join("235959_300")).unwrap();

        let dir = f.observer.start_segment().unwrap();
        let expected_dir = f
            .observer
            .config
            .captures_dir()
            .join("20270101/desk/000000.incomplete");
        assert_eq!(dir, expected_dir);
        let meta = sidecar(&dir);
        assert_eq!(meta["start_timestamp"], 1798761599.0);
        let zone = crate::recovery::read_capture_zone(&dir).unwrap();
        assert_eq!(zone.tz, Some("Pacific/Auckland".into()));
        assert_eq!(zone.utc_offset_seconds, 46800);

        fs::write(dir.join("screen.webm"), b"video").unwrap();
        f.wall.set(1798761599.0 + 300.0);
        f.mono.set(f.mono.get() + 300.0);
        f.observer.finalize_segment().unwrap();

        let final_dir = f
            .observer
            .config
            .captures_dir()
            .join("20270101/desk/000000_300");
        assert!(final_dir.exists());
    }

    #[test]
    fn negative_twin_different_stream_or_day_does_not_bump() {
        let mut f = fixture(false);
        f.wall.set(1704076200.0);
        let other_stream = f.observer.config.captures_dir().join("20240101/room");
        fs::create_dir_all(&other_stream).unwrap();
        fs::create_dir(other_stream.join("023000_300")).unwrap();

        let other_day = f.observer.config.captures_dir().join("20240102/desk");
        fs::create_dir_all(&other_day).unwrap();
        fs::create_dir(other_day.join("023000_300")).unwrap();

        let dir = f.observer.start_segment().unwrap();
        assert_eq!(
            dir.file_name().unwrap().to_str().unwrap(),
            "023000.incomplete"
        );
    }

    #[test]
    fn resume_reopen_bumped_stem_same_second() {
        let mut f = fixture(true);
        initialize(&mut f);
        f.observer.resume();

        let (day, time) = ("20231114", "221320");
        let base = f.observer.config.captures_dir().join(day).join("desk");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir(base.join(format!("{time}_300"))).unwrap();

        f.observer.backends.video.fail_start = true;
        f.wall_step.set(0.0);
        f.observer.tick().unwrap();

        assert_eq!(f.starts.get(), 1);
        assert!(f.observer.state.segment_dir.is_some());
        let seg_dir = f.observer.state.segment_dir.clone().unwrap();
        assert_eq!(
            seg_dir.file_name().unwrap().to_str().unwrap(),
            "221321.incomplete"
        );
        assert_eq!(
            fs::read_dir(seg_dir.parent().unwrap())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .ends_with(".incomplete"))
                .count(),
            1
        );
        let zone_path = seg_dir.join(".capture_zone");
        let zone_bytes = fs::read(&zone_path).unwrap();
        let reopened = f.observer.start_segment().unwrap();
        assert_eq!(reopened, seg_dir);
        assert_eq!(fs::read(&zone_path).unwrap(), zone_bytes);
        assert_eq!(f.zone_reads.get(), 1);
    }
}
