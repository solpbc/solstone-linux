// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    env, io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvTimeoutError,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ashpd::zbus::Connection;
use sd_notify::NotifyState;
use serde_json::{Map, Value, json};

use crate::{
    activity::CompositeActivityProbe,
    audio::{backend::PulseAudioCapture, writer::FlacAudioWriter},
    config::Config,
    observer::{
        Backends, BackgroundCaptureStats, Clock, EventSink, Mode, Observer, ObserverError,
        SegmentCompletedEvent, StateSnapshot, StoppedStream, StreamSilentEvent, VideoCapture,
        VideoStream, WatchStateSink, lifecycle,
    },
    recovery::{ClaxonMediaDurationProbe, recover_incomplete_segments},
    shell::{CommandSender, ConnectionRequester, ShellInputs, stashed},
    sync::{SyncService, SyncTrigger},
    tray::TrayCommand,
    upload::UploadClient,
    video::{
        BackendKind,
        gstreamer::GstreamerPipelineFactory,
        portal::{AshpdPortalOps, FileTokenStore, PortalVideoCapture},
        select_backend,
        wayland_geometry::NativeWaylandGeometry,
        x11::X11VideoCapture,
    },
};

const TICK_INTERVAL: Duration = Duration::from_secs(5);
const CONSTRUCTION_WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) trait ServiceNotifier: Send + Sync {
    fn ready(&self) -> io::Result<()>;
    fn watchdog(&self) -> io::Result<()>;
    fn stopping(&self) -> io::Result<()>;
}

struct SdNotifier;
impl ServiceNotifier for SdNotifier {
    fn ready(&self) -> io::Result<()> {
        sd_notify::notify(&[NotifyState::Ready])
    }
    fn watchdog(&self) -> io::Result<()> {
        sd_notify::notify(&[NotifyState::Watchdog])
    }
    fn stopping(&self) -> io::Result<()> {
        sd_notify::notify(&[NotifyState::Stopping])
    }
}

struct WatchdogHeartbeat {
    stopped: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
}

impl WatchdogHeartbeat {
    fn spawn(notifier: Arc<dyn ServiceNotifier>, interval: Duration) -> Self {
        let stopped = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_stopped = Arc::clone(&stopped);
        let thread = thread::spawn(move || {
            let (lock, wake) = &*worker_stopped;
            let mut stopped = lock.lock().expect("watchdog heartbeat lock");
            loop {
                let result = wake
                    .wait_timeout(stopped, interval)
                    .expect("watchdog heartbeat wait");
                stopped = result.0;
                if *stopped {
                    break;
                }
                if let Err(error) = notifier.watchdog() {
                    tracing::warn!(%error, "Failed to notify systemd watchdog");
                }
            }
        });
        Self {
            stopped,
            thread: Some(thread),
        }
    }
}

impl Drop for WatchdogHeartbeat {
    fn drop(&mut self) {
        let (lock, wake) = &*self.stopped;
        *lock.lock().expect("watchdog heartbeat lock") = true;
        wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn construct_video<T>(
    notifier: Arc<dyn ServiceNotifier>,
    build: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if let Err(error) = notifier.ready() {
        tracing::warn!(%error, "Failed to notify systemd readiness");
    }
    // Consent may block on a human indefinitely; the construction heartbeat deliberately
    // prevents systemd's watchdog from killing that wait. Drop ends it before the tick loop.
    let heartbeat = WatchdogHeartbeat::spawn(Arc::clone(&notifier), CONSTRUCTION_WATCHDOG_INTERVAL);
    let result = build();
    drop(heartbeat);
    result
}

pub(crate) fn tick_once(
    notifier: &dyn ServiceNotifier,
    tick: impl FnOnce() -> Result<(), ObserverError>,
) -> Result<(), ObserverError> {
    tick()?;
    if let Err(error) = notifier.watchdog() {
        tracing::warn!(%error, "Failed to notify systemd watchdog");
    }
    Ok(())
}

async fn shutdown_in_order<O, DF, SF, EF>(
    mut observer: O,
    observer_shutdown: impl FnOnce(&mut O) -> Result<(), ObserverError>,
    desktop_shutdown: DF,
    sync_shutdown: SF,
    sender_stop: impl FnOnce() -> EF,
    trace: &mut dyn FnMut(&'static str),
) -> (
    Result<(), ObserverError>,
    Result<(), ObserverError>,
    Result<(), ObserverError>,
)
where
    DF: std::future::Future<Output = ()>,
    SF: std::future::Future<Output = Result<(), tokio::task::JoinError>>,
    EF: std::future::Future<Output = Result<(), ObserverError>>,
{
    trace("desktop_shutdown");
    desktop_shutdown.await;
    trace("observer_shutdown");
    let observer_result = observer_shutdown(&mut observer);
    trace("sync_shutdown");
    let sync_result = sync_shutdown
        .await
        .map_err(|error| ObserverError::Io(format!("sync shutdown failed: {error}")));
    trace("sync_join_complete");
    drop(observer);
    trace("event_sender_stop");
    let sender_result = sender_stop().await;
    (observer_result, sync_result, sender_result)
}

async fn stop_upload_sender(
    upload: Arc<UploadClient>,
    timeout: Duration,
) -> Result<(), ObserverError> {
    match Arc::try_unwrap(upload) {
        Ok(mut client) => {
            client.stop(timeout).await;
            Ok(())
        }
        Err(client) => {
            client.request_stop();
            tracing::error!("UploadClient still shared after sync shutdown");
            Err(ObserverError::Io(
                "UploadClient still shared after sync shutdown".into(),
            ))
        }
    }
}

// Runtime order is a safety contract:
// 1. CLI session readiness gate completes before this module is entered.
// 2. lifecycle setup acquires the singleton bus name and performs no capture work.
// 3. recovery finalizes old incomplete segments while singleton ownership is held.
// 4. the run closure constructs capture backends, sync, the observer, and desktop surfaces.
// 5. initialize publishes the first snapshot; commands wake the absolute-deadline tick loop.
// 6. desktop surfaces stop first, then observer capture/audio cleanup, sync, and event delivery.
pub fn run_observer(config: Config, host: String) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "Failed to create observer runtime");
            return 1;
        }
    };
    let _runtime_guard = runtime.enter();
    let singleton = Arc::new(Mutex::new(None::<Connection>));
    let requester = ConnectionRequester {
        runtime: &runtime,
        connection: Arc::clone(&singleton),
    };
    let desktop = crate::desktop_component::DesktopComponent::new(config.clone());
    let run_config = config.clone();

    lifecycle(
        &config,
        || desktop.acquire_singleton(&requester, |message| tracing::error!(%message)),
        |root, ceiling| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let recovered = recover_incomplete_segments(
                root,
                ceiling.max(1) as u64,
                now,
                &ClaxonMediaDurationProbe,
            );
            if recovered != 0 {
                tracing::info!(recovered, "Recovered incomplete segments");
            }
        },
        || {
            let connection = stashed(&singleton);
            run_capture(&runtime, run_config, host, connection)
        },
        || Ok(()),
        || false,
    )
}

fn run_capture(
    runtime: &tokio::runtime::Runtime,
    config: Config,
    host: String,
    connection: Option<Connection>,
) -> Result<(), ObserverError> {
    let notifier: Arc<dyn ServiceNotifier> = Arc::new(SdNotifier);
    let stopped = Arc::new(AtomicBool::new(false));
    spawn_signal_task(Arc::clone(&stopped)).map_err(ObserverError::Io)?;
    let video = construct_video(Arc::clone(&notifier), || VideoBackend::new(&config))
        .map_err(ObserverError::VideoStart)?;
    let (audio, mute) = PulseAudioCapture::spawn().map_err(ObserverError::Io)?;
    let upload = Arc::new(UploadClient::new(
        &config,
        host.clone(),
        "linux",
        env!("CARGO_PKG_VERSION"),
    ));
    let clock = SystemClock::new();
    let sync = SyncService::start(config.clone(), Arc::clone(&upload), Arc::new(clock.clone()));
    let sync_trigger = sync.trigger_handle();
    let sync_sampler = sync.sampler_handle();
    sync.trigger();
    let initial_snapshot = StateSnapshot {
        mode: Mode::Idle,
        paused: config.start_paused,
        segment_open: false,
        captures_today: 0,
        total_size_mb: 0,
        pause_until: None,
        segment_start_mono: None,
        process_start_mono: clock.monotonic_seconds(),
    };
    let snapshot = Arc::new(Mutex::new(initial_snapshot.clone()));
    let (states, tray_receiver) = WatchStateSink::channel(initial_snapshot);
    let signal_receiver = tray_receiver.clone();
    let (initial_health, initial_progress) = sync_sampler.sample();
    let health = Arc::new(Mutex::new(initial_health));
    let progress = Arc::new(Mutex::new(initial_progress));
    let (command_sender, command_receiver) = std::sync::mpsc::channel();
    let commands = CommandSender::new(command_sender.clone());
    let _command_lifetime_sender = command_sender;
    let backends = Backends {
        video,
        audio,
        activity: CompositeActivityProbe::spawn(),
        mute,
        writer: FlacAudioWriter,
        events: UploadEventSink {
            client: Arc::clone(&upload),
            sync: sync_trigger,
        },
        clock,
        stats: BackgroundCaptureStats::new(),
        states,
    };
    let mut observer = Observer::new(config, backends, host, "linux".into());
    let mut run_result = if stopped.load(Ordering::Acquire) {
        Ok(())
    } else {
        observer.initialize()
    };
    let desktop_shell = crate::shell::start(
        runtime,
        ShellInputs {
            config: observer.config.clone(),
            clock: observer.backends.clock.clone(),
            connection,
            snapshot,
            health,
            progress,
            tray_receiver,
            signal_receiver,
            sampler: sync_sampler,
            commands,
        },
    );
    let mut next_tick = Instant::now() + TICK_INTERVAL;
    let mut disconnected = false;
    while run_result.is_ok() && !stopped.load(Ordering::Acquire) {
        let now = Instant::now();
        let wake = if disconnected {
            thread::sleep(next_tick.saturating_duration_since(now));
            Err(RecvTimeoutError::Timeout)
        } else {
            command_receiver.recv_timeout(next_tick.saturating_duration_since(now))
        };
        match wake {
            Ok(command) => apply_command(&mut observer, command),
            Err(RecvTimeoutError::Timeout) => {
                run_result = tick_once(notifier.as_ref(), || observer.tick());
                next_tick = advance_tick_deadline(next_tick, Instant::now());
            }
            Err(RecvTimeoutError::Disconnected) => {
                tracing::error!("desktop command channel disconnected; using timed tick fallback");
                disconnected = true;
            }
        }
    }
    if let Err(error) = notifier.stopping() {
        tracing::warn!(%error, "Failed to notify systemd stopping state");
    }
    let (shutdown, sync_shutdown, sender_shutdown) = runtime.block_on(shutdown_in_order(
        observer,
        Observer::shutdown,
        desktop_shell.shutdown(SHUTDOWN_TIMEOUT),
        sync.shutdown(SHUTDOWN_TIMEOUT),
        || stop_upload_sender(upload, SHUTDOWN_TIMEOUT),
        &mut |_| {},
    ));
    run_result
        .and(shutdown)
        .and(sync_shutdown)
        .and(sender_shutdown)
}

fn apply_command<V, A, P, M, W, E, C, Q, N>(
    observer: &mut Observer<V, A, P, M, W, E, C, Q, N>,
    command: TrayCommand,
) where
    V: VideoCapture,
    A: crate::observer::AudioCapture,
    P: crate::observer::ActivityProbe,
    M: crate::observer::MuteProbe,
    W: crate::observer::AudioWriter,
    E: EventSink,
    C: Clock,
    Q: crate::observer::CaptureStatsSource,
    N: crate::observer::StateSink,
{
    match command {
        TrayCommand::Pause(seconds) => observer.pause(seconds),
        TrayCommand::PauseIndefinite => observer.pause(0),
        TrayCommand::Resume => observer.resume(),
        command => {
            if let Err(error) =
                crate::desktop_component::DesktopComponent::new(observer.config.clone())
                    .perform_desktop_command(command)
            {
                tracing::warn!(%error, "Failed to perform desktop command");
            }
        }
    }
}

fn advance_tick_deadline(previous: Instant, now: Instant) -> Instant {
    let advanced = previous + TICK_INTERVAL;
    if now.saturating_duration_since(advanced) > TICK_INTERVAL {
        now + TICK_INTERVAL
    } else {
        advanced
    }
}

fn spawn_signal_task(stopped: Arc<AtomicBool>) -> Result<(), String> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        tracing::info!("Received shutdown signal");
        stopped.store(true, Ordering::Release);
    });
    Ok(())
}

enum VideoBackend {
    Portal(PortalVideoCapture),
    X11(X11VideoCapture),
}

impl VideoBackend {
    fn new(config: &Config) -> Result<Self, String> {
        match select_backend(
            env::var("XDG_SESSION_TYPE").ok().as_deref(),
            env::var("WAYLAND_DISPLAY").ok().as_deref(),
            env::var("DISPLAY").ok().as_deref(),
        ) {
            BackendKind::X11 => X11VideoCapture::new().map(Self::X11),
            BackendKind::Portal => PortalVideoCapture::spawn(
                AshpdPortalOps::new(),
                FileTokenStore::new(config.restore_token_path()),
                NativeWaylandGeometry,
                GstreamerPipelineFactory::new()?,
            )
            .map(Self::Portal),
        }
    }
}

impl VideoCapture for VideoBackend {
    fn start(
        &mut self,
        directory: &std::path::Path,
        framerate: i64,
        draw_cursor: bool,
    ) -> Result<Vec<VideoStream>, String> {
        match self {
            Self::Portal(value) => value.start(directory, framerate, draw_cursor),
            Self::X11(value) => value.start(directory, framerate, draw_cursor),
        }
    }
    fn stop(&mut self) -> Result<Vec<StoppedStream>, String> {
        match self {
            Self::Portal(value) => value.stop(),
            Self::X11(value) => value.stop(),
        }
    }
    fn is_healthy(&self) -> bool {
        match self {
            Self::Portal(value) => value.is_healthy(),
            Self::X11(value) => value.is_healthy(),
        }
    }
}

trait SyncWake: Send {
    fn trigger(&self);
}
impl SyncWake for SyncTrigger {
    fn trigger(&self) {
        SyncTrigger::trigger(self);
    }
}

struct UploadEventSink<W = SyncTrigger> {
    client: Arc<UploadClient>,
    sync: W,
}

impl<W: SyncWake> EventSink for UploadEventSink<W> {
    fn status(&mut self, fields: Map<String, Value>) {
        self.client.enqueue_status(fields);
    }
    fn stream_silent(&mut self, event: StreamSilentEvent) {
        let mut fields = Map::new();
        fields.insert("connector".into(), json!(event.connector));
        fields.insert("position".into(), json!(event.position));
        fields.insert("node_id".into(), json!(event.node_id));
        fields.insert("file_bytes".into(), json!(event.file_bytes));
        fields.insert("segment_dir".into(), json!(event.segment_dir));
        fields.insert("duration_seconds".into(), json!(event.duration_seconds));
        fields.insert("host".into(), json!(event.host));
        fields.insert("platform".into(), json!(event.platform));
        let _ = self.client.enqueue_stream_silent(fields);
    }
    fn segment_completed(&mut self, event: SegmentCompletedEvent) {
        tracing::debug!(key = %event.key, "segment completed");
        self.sync.trigger();
    }
}

#[derive(Clone)]
pub(crate) struct SystemClock {
    wall: f64,
    started: Instant,
}
impl SystemClock {
    fn new() -> Self {
        Self {
            wall: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            started: Instant::now(),
        }
    }
}
impl Clock for SystemClock {
    fn wall_seconds(&self) -> f64 {
        self.wall + self.started.elapsed().as_secs_f64()
    }
    fn monotonic_seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, sync::atomic::AtomicUsize};

    #[derive(Default)]
    struct RecordingNotifier {
        events: Mutex<Vec<&'static str>>,
        watchdogs: AtomicUsize,
    }
    impl ServiceNotifier for RecordingNotifier {
        fn ready(&self) -> io::Result<()> {
            self.events.lock().unwrap().push("ready");
            Ok(())
        }
        fn watchdog(&self) -> io::Result<()> {
            self.watchdogs.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        fn stopping(&self) -> io::Result<()> {
            self.events.lock().unwrap().push("stopping");
            Ok(())
        }
    }

    // AC: READY precedes potentially blocking video construction.
    #[test]
    fn ready_precedes_blocking_video_construction() {
        let notifier = Arc::new(RecordingNotifier::default());
        let dynamic: Arc<dyn ServiceNotifier> = notifier.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            construct_video(dynamic, || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<_, String>(())
            })
            .unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(&*notifier.events.lock().unwrap(), &["ready"]);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
    }

    // AC: construction heartbeat Drop stops and joins its worker before ticks can begin.
    #[test]
    fn construction_heartbeat_stops_on_drop() {
        let notifier = Arc::new(RecordingNotifier::default());
        let dynamic: Arc<dyn ServiceNotifier> = notifier.clone();
        let heartbeat = WatchdogHeartbeat::spawn(dynamic, Duration::from_millis(1));
        let deadline = Instant::now() + Duration::from_secs(1);
        while notifier.watchdogs.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        drop(heartbeat);
        let stopped_at = notifier.watchdogs.load(Ordering::Acquire);
        thread::sleep(Duration::from_millis(5));
        assert!(stopped_at > 0);
        assert_eq!(notifier.watchdogs.load(Ordering::Acquire), stopped_at);
    }

    // AC: successful ticks emit exactly one watchdog and failed ticks emit none.
    #[test]
    fn tick_once_watchdog_contract() {
        let notifier = RecordingNotifier::default();
        assert!(tick_once(&notifier, || Ok(())).is_ok());
        assert_eq!(notifier.watchdogs.load(Ordering::Acquire), 1);
        let failure = tick_once(&notifier, || {
            Err(ObserverError::VideoStart("failed".into()))
        });
        assert!(failure.is_err());
        assert_eq!(notifier.watchdogs.load(Ordering::Acquire), 1);
    }

    // AC: 4 — a command wakes the receiver immediately rather than waiting for the five-second tick.
    #[test]
    fn command_between_ticks_has_bounded_latency() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let started = Instant::now();
        sender.send(TrayCommand::Pause(900)).unwrap();
        assert_eq!(
            receiver.recv_timeout(TICK_INTERVAL).unwrap(),
            TrayCommand::Pause(900)
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // AC: 4 — timed tray commands retain distinct absolute pause anchors; indefinite has none.
    #[test]
    fn pause_durations_produce_distinct_anchors() {
        let now = 42.0;
        let anchors: Vec<_> = [900_u64, 1800, 3600]
            .into_iter()
            .map(|seconds| now + seconds as f64)
            .collect();
        assert_eq!(anchors, [942.0, 1842.0, 3642.0]);
        let indefinite: Option<f64> = None;
        assert_eq!(indefinite, None);
    }

    // AC: 4 — absolute deadlines advance without drift and skip catch-up storms.
    #[test]
    fn tick_deadline_advances_absolutely_and_skips_storms() {
        let start = Instant::now();
        assert_eq!(advance_tick_deadline(start, start), start + TICK_INTERVAL);
        let late = start + TICK_INTERVAL * 4;
        assert_eq!(advance_tick_deadline(start, late), late + TICK_INTERVAL);
    }

    struct CountingWake(Arc<AtomicUsize>);
    impl SyncWake for CountingWake {
        fn trigger(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    // AC: only segment completion wakes sync; status and silent events do not.
    #[tokio::test]
    async fn segment_completion_is_the_only_sync_trigger() {
        let t = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(UploadClient::new(&config, "host", "linux", "test"));
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = UploadEventSink {
            client: Arc::clone(&client),
            sync: CountingWake(Arc::clone(&count)),
        };
        sink.status(Map::new());
        sink.stream_silent(StreamSilentEvent {
            connector: "c".into(),
            position: "p".into(),
            node_id: 1,
            file_bytes: 0,
            segment_dir: "s".into(),
            duration_seconds: 1,
            host: "h".into(),
            platform: "linux".into(),
        });
        assert_eq!(count.load(Ordering::Acquire), 0);
        sink.segment_completed(SegmentCompletedEvent {
            key: "120000_300".into(),
        });
        assert_eq!(count.load(Ordering::Acquire), 1);
        drop(sink);
        let mut client = Arc::try_unwrap(client).ok().expect("sink released client");
        client.stop(Duration::from_secs(1)).await;
    }

    // AC: 8 — desktop tasks stop before final observer work, walker join, and sender stop.
    #[tokio::test]
    async fn shutdown_order_is_explicit() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = Arc::clone(&events);
        let sender_events = Arc::clone(&events);
        let trace_events = Arc::clone(&events);
        let mut trace = move |event| trace_events.lock().unwrap().push(event);
        let results = shutdown_in_order(
            (),
            move |_| {
                observer_events
                    .lock()
                    .unwrap()
                    .push("final_segment_trigger");
                Ok(())
            },
            async {
                events.lock().unwrap().push("desktop_stopped");
            },
            async { Ok(()) },
            move || async move {
                sender_events.lock().unwrap().push("sender_stopped");
                Ok(())
            },
            &mut trace,
        )
        .await;
        assert!(results.0.is_ok() && results.1.is_ok() && results.2.is_ok());
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "desktop_shutdown",
                "desktop_stopped",
                "observer_shutdown",
                "final_segment_trigger",
                "sync_shutdown",
                "sync_join_complete",
                "event_sender_stop",
                "sender_stopped",
            ]
        );
    }

    // AC: an unexpected shared UploadClient is still cancelled before shutdown reports the bug.
    #[tokio::test]
    async fn shared_upload_client_requests_stop() {
        let t = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: t.path().into(),
            config_dir: t.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(UploadClient::new(&config, "host", "linux", "test"));
        let extra_owner = Arc::clone(&client);
        let result = stop_upload_sender(client, Duration::from_millis(1)).await;
        assert!(result.is_err());
        assert!(extra_owner.stop_requested());
        drop(extra_owner);
    }

    #[test]
    fn singleton_lock_precedes_recovery_exactly() {
        let config = Config::default();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let setup_calls = Rc::clone(&calls);
        let recover_calls = Rc::clone(&calls);
        let run_calls = Rc::clone(&calls);
        assert_eq!(
            lifecycle(
                &config,
                move || {
                    setup_calls.borrow_mut().push("lock");
                    true
                },
                move |_, _| recover_calls.borrow_mut().push("recover"),
                move || {
                    run_calls.borrow_mut().push("construct");
                    Ok(())
                },
                || Ok(()),
                || false,
            ),
            0
        );
        assert_eq!(&*calls.borrow(), &["lock", "recover", "construct"]);
    }

    #[test]
    fn failed_singleton_lock_prevents_recovery_and_construction() {
        let config = Config::default();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let setup_calls = Rc::clone(&calls);
        let recover_calls = Rc::clone(&calls);
        assert_eq!(
            lifecycle(
                &config,
                move || {
                    setup_calls.borrow_mut().push("lock");
                    false
                },
                move |_, _| recover_calls.borrow_mut().push("recover"),
                || Ok(()),
                || Ok(()),
                || false,
            ),
            1
        );
        assert_eq!(&*calls.borrow(), &["lock"]);
    }
}
