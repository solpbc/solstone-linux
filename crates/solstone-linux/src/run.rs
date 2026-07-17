// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    env, io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ashpd::zbus::{
    Connection,
    fdo::{RequestNameFlags, RequestNameReply},
};
use sd_notify::NotifyState;
use serde_json::{Map, Value, json};

use crate::{
    activity::CompositeActivityProbe,
    audio::{backend::PulseAudioCapture, writer::FlacAudioWriter},
    config::Config,
    observer::{
        Backends, BackgroundCaptureStats, Clock, EventSink, Observer, ObserverError,
        SegmentCompletedEvent, StateSink, StateSnapshot, StoppedStream, StreamSilentEvent,
        VideoCapture, VideoStream, lifecycle,
    },
    recovery::{ClaxonMediaDurationProbe, recover_incomplete_segments},
    sync::{SyncService, SyncTrigger},
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

const BUS_NAME: &str = "org.solpbc.solstone.Observer1";

/// Side-by-side soak scaffolding (delete at cutover): the Python observer owns
/// the well-known name on a box where both run, so the soak instance overrides
/// it via SOLSTONE_LINUX_BUS_NAME. Production never sets this.
fn bus_name() -> String {
    std::env::var("SOLSTONE_LINUX_BUS_NAME").unwrap_or_else(|_| BUS_NAME.to_owned())
}
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

async fn shutdown_in_order<O, SF, EF>(
    mut observer: O,
    observer_shutdown: impl FnOnce(&mut O) -> Result<(), ObserverError>,
    sync_shutdown: SF,
    sender_stop: impl FnOnce() -> EF,
    trace: &mut dyn FnMut(&'static str),
) -> (
    Result<(), ObserverError>,
    Result<(), ObserverError>,
    Result<(), ObserverError>,
)
where
    SF: std::future::Future<Output = Result<(), tokio::task::JoinError>>,
    EF: std::future::Future<Output = Result<(), ObserverError>>,
{
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
// 4. the run closure constructs audio, video, upload, and observer backends.
// 5. initialize starts the first capture, then ticks run until SIGINT or SIGTERM.
// 6. observer shutdown completes capture/audio cleanup, then event delivery is flushed.
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
    let setup_singleton = Arc::clone(&singleton);
    let run_config = config.clone();

    lifecycle(
        &config,
        || match runtime.block_on(acquire_singleton()) {
            Ok(connection) => {
                *setup_singleton.lock().expect("singleton lock") = Some(connection);
                true
            }
            Err(error) => {
                tracing::error!(%error, "Another solstone-linux observer is already running");
                false
            }
        },
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
        || run_capture(&runtime, run_config, host),
        || Ok(()),
        || false,
    )
}

async fn acquire_singleton() -> Result<Connection, String> {
    let connection = Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    let reply = connection
        .request_name_with_flags(bus_name().as_str(), RequestNameFlags::DoNotQueue.into())
        .await
        .map_err(|error| error.to_string())?;
    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(connection),
        RequestNameReply::Exists | RequestNameReply::InQueue => {
            Err(format!("{} is already owned", bus_name()))
        }
    }
}

fn run_capture(
    runtime: &tokio::runtime::Runtime,
    config: Config,
    host: String,
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
    sync.trigger();
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
        states: NoopStateSink,
    };
    let mut observer = Observer::new(config, backends, host, "linux".into());
    let mut run_result = if stopped.load(Ordering::Acquire) {
        Ok(())
    } else {
        observer.initialize()
    };
    while run_result.is_ok() && !stopped.load(Ordering::Acquire) {
        thread::park_timeout(TICK_INTERVAL);
        run_result = tick_once(notifier.as_ref(), || observer.tick());
    }
    if let Err(error) = notifier.stopping() {
        tracing::warn!(%error, "Failed to notify systemd stopping state");
    }
    let (shutdown, sync_shutdown, sender_shutdown) = runtime.block_on(shutdown_in_order(
        observer,
        Observer::shutdown,
        sync.shutdown(SHUTDOWN_TIMEOUT),
        || stop_upload_sender(upload, SHUTDOWN_TIMEOUT),
        &mut |_| {},
    ));
    run_result
        .and(shutdown)
        .and(sync_shutdown)
        .and(sender_shutdown)
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
struct SystemClock {
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

struct NoopStateSink;
impl StateSink for NoopStateSink {
    fn publish(&mut self, _snapshot: StateSnapshot) {}
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

    // AC: shutdown composition orders final observer work, walker join, then sender stop.
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
