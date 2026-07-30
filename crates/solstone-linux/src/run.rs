// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    env, io,
    path::PathBuf,
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

#[cfg(test)]
use crate::private_link::{start_private_link_owner, start_private_link_session};
use crate::{
    activity::CompositeActivityProbe,
    audio::{backend::PulseAudioCapture, writer::FlacAudioWriter},
    config::Config,
    observer::{
        Backends, BackgroundCaptureStats, Clock, EventSink, Mode, Observer, ObserverError,
        SegmentCompletedEvent, StateSnapshot, StoppedStream, StreamSilentEvent, VideoCapture,
        VideoStream, WatchStateSink, lifecycle,
    },
    private_link::{
        PrivateLinkCapability, PrivateStateLock, load_credential,
        start_private_link_owner_with_lock,
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

async fn shutdown_in_order<O, DF, SF, EF, LF>(
    mut observer: O,
    observer_shutdown: impl FnOnce(&mut O) -> Result<(), ObserverError>,
    desktop_shutdown: DF,
    sync_shutdown: SF,
    sender_stop: impl FnOnce() -> EF,
    linked_shutdown: impl FnOnce() -> LF,
    trace: &mut dyn FnMut(&'static str),
) -> (
    Result<(), ObserverError>,
    Result<(), ObserverError>,
    Result<(), ObserverError>,
    Result<(), ObserverError>,
)
where
    DF: std::future::Future<Output = ()>,
    SF: std::future::Future<Output = Result<(), tokio::task::JoinError>>,
    EF: std::future::Future<Output = Result<(), ObserverError>>,
    LF: std::future::Future<Output = Result<(), ObserverError>>,
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
    trace("linked_owner_shutdown");
    let linked_result = linked_shutdown().await;
    trace("linked_owner_join_complete");
    (observer_result, sync_result, sender_result, linked_result)
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
// 4. the run closure constructs capture backends, sync, and the observer.
// 5. initialize publishes the first snapshot, then desktop surfaces start and commands wake the
//    absolute-deadline tick loop.
// 6. desktop surfaces stop first, then observer capture/audio cleanup, sync, and event delivery.
// 7. the linked owner closes streams and joins bridge/carrier tasks last, then releases its lock.
pub(crate) fn run_observer(
    config: Config,
    host: String,
    state_lock: PrivateStateLock,
    transport_enabled: bool,
) -> i32 {
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
            run_capture(
                &runtime,
                run_config,
                host,
                connection,
                state_lock,
                transport_enabled,
            )
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
    state_lock: PrivateStateLock,
    transport_enabled: bool,
) -> Result<(), ObserverError> {
    let notifier: Arc<dyn ServiceNotifier> = Arc::new(SdNotifier);
    let stopped = Arc::new(AtomicBool::new(false));
    spawn_signal_task(Arc::clone(&stopped)).map_err(ObserverError::Io)?;
    let video = construct_video(Arc::clone(&notifier), || VideoBackend::new(&config))
        .map_err(ObserverError::VideoStart)?;
    let (audio, mute) = PulseAudioCapture::spawn().map_err(ObserverError::Io)?;
    let clock = SystemClock::new();
    let upload = Arc::new(UploadClient::new(
        &config,
        None::<PrivateLinkCapability>,
        host.clone(),
        "linux",
        env!("CARGO_PKG_VERSION"),
        Arc::new(clock.clone()),
    ));
    let linked_upload = Arc::clone(&upload);
    let linked_root = config.config_dir.clone();
    let linked_stream = config.stream.clone();
    let linked_state_lock = state_lock
        .try_clone()
        .map_err(|error| ObserverError::Io(format!("linked state lock clone failed: {error}")))?;
    let linked_start = runtime.spawn(start_linked_owner(
        linked_upload,
        linked_root,
        linked_stream,
        linked_state_lock,
        transport_enabled,
    ));
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
            Ok(command) => {
                run_result = dispatch_wake(
                    &mut observer,
                    LoopWake::Command(command),
                    apply_command,
                    |observer| tick_once(notifier.as_ref(), || observer.tick()),
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                run_result =
                    dispatch_wake(&mut observer, LoopWake::Tick, apply_command, |observer| {
                        tick_once(notifier.as_ref(), || observer.tick())
                    });
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
    let (shutdown, sync_shutdown, sender_shutdown, linked_shutdown) =
        runtime.block_on(shutdown_in_order(
            observer,
            Observer::shutdown,
            desktop_shell.shutdown(SHUTDOWN_TIMEOUT),
            sync.shutdown(SHUTDOWN_TIMEOUT),
            || stop_upload_sender(upload, SHUTDOWN_TIMEOUT),
            || async move {
                match linked_start.await {
                    Ok(Ok(owner)) => owner.shutdown().await.map_err(|error| {
                        ObserverError::Io(format!("linked shutdown failed: {error}"))
                    }),
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "Linked transport remained unavailable");
                        Ok(())
                    }
                    Err(error) => Err(ObserverError::Io(format!(
                        "linked startup task failed: {error}"
                    ))),
                }
            },
            &mut |_| {},
        ));
    run_result
        .and(shutdown)
        .and(sync_shutdown)
        .and(sender_shutdown)
        .and(linked_shutdown)
}

async fn start_linked_owner(
    upload: Arc<UploadClient>,
    config_root: PathBuf,
    stream: String,
    state_lock: PrivateStateLock,
    transport_enabled: bool,
) -> Result<crate::private_link::PrivateLinkOwner, crate::private_link::PrivateStateError> {
    if !transport_enabled {
        upload.publish_link_fact(crate::private_link::LinkFact::ConfigSanitationFailed);
        return Err(crate::private_link::PrivateStateError::BridgeUnavailable);
    }
    let credential = match load_credential(&config_root) {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            upload.publish_link_fact(crate::private_link::LinkFact::PairingRequired);
            return Err(crate::private_link::PrivateStateError::MalformedCredential);
        }
        Err(error) => {
            upload.publish_link_fact(crate::private_link::LinkFact::PrivateStateInvalid);
            return Err(error);
        }
    };
    let owner =
        start_private_link_owner_with_lock(state_lock, credential, &stream, upload.link_facts())
            .await
            .inspect_err(|_| {
                upload.publish_link_fact(crate::private_link::LinkFact::TransportUnavailable);
            })?;
    upload.install_capability(owner.capability());
    Ok(owner)
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
    match route_command(command) {
        ObserverAction::Pause(seconds) => observer.pause(seconds),
        ObserverAction::Resume => observer.resume(),
        ObserverAction::Desktop(command) => {
            if let Err(error) =
                crate::desktop_component::DesktopComponent::new(observer.config.clone())
                    .perform_desktop_command(command)
            {
                tracing::warn!(%error, "Failed to perform desktop command");
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObserverAction {
    Pause(u64),
    Resume,
    Desktop(TrayCommand),
}

fn route_command(command: TrayCommand) -> ObserverAction {
    match command {
        TrayCommand::Pause(seconds) => ObserverAction::Pause(seconds),
        TrayCommand::PauseIndefinite => ObserverAction::Pause(0),
        TrayCommand::Resume => ObserverAction::Resume,
        command => ObserverAction::Desktop(command),
    }
}

fn advance_tick_deadline(previous: Instant, now: Instant) -> Instant {
    if now.saturating_duration_since(previous) > TICK_INTERVAL {
        now + TICK_INTERVAL
    } else {
        previous + TICK_INTERVAL
    }
}

enum LoopWake {
    Command(TrayCommand),
    Tick,
}

fn dispatch_wake<T, E>(
    owner: &mut T,
    wake: LoopWake,
    apply: impl FnOnce(&mut T, TrayCommand),
    tick: impl FnOnce(&mut T) -> Result<(), E>,
) -> Result<(), E> {
    match wake {
        LoopWake::Command(command) => {
            apply(owner, command);
            Ok(())
        }
        LoopWake::Tick => tick(owner),
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
    pub(crate) fn new() -> Self {
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
    use crate::{
        dbus_service::{ObserverCommands, clamp_pause},
        observer::StateSink,
        private_link::{
            CREDENTIALS_FILENAME, LinkFactState, OBSERVER_FILENAME, ObserverState,
            PrivateLinkOwner, PrivateStateError, PrivateStateLock, persist_credential,
            publish_observer_registration,
        },
        private_link_test_peer::PrivateLinkPeer,
    };
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

    struct CommandObserver {
        now: f64,
        snapshot: StateSnapshot,
        states: WatchStateSink,
    }

    impl CommandObserver {
        fn pause(&mut self, seconds: u64) {
            self.snapshot.paused = true;
            self.snapshot.pause_until = (seconds > 0).then_some(self.now + seconds as f64);
            self.states.publish(self.snapshot.clone());
        }
    }

    fn command_observer() -> (CommandObserver, tokio::sync::watch::Receiver<StateSnapshot>) {
        let snapshot = StateSnapshot {
            mode: Mode::Idle,
            paused: false,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        };
        let (states, receiver) = WatchStateSink::channel(snapshot.clone());
        (
            CommandObserver {
                now: 42.0,
                snapshot,
                states,
            },
            receiver,
        )
    }

    fn apply_test_action(observer: &mut CommandObserver, action: ObserverAction) {
        if let ObserverAction::Pause(seconds) = action {
            observer.pause(seconds);
        }
    }

    fn route_test_command(observer: &mut CommandObserver, command: TrayCommand) {
        apply_test_action(observer, route_command(command));
    }

    #[test]
    fn every_tray_command_routes_to_one_action() {
        assert_eq!(
            route_command(TrayCommand::Pause(900)),
            ObserverAction::Pause(900)
        );
        assert_eq!(
            route_command(TrayCommand::PauseIndefinite),
            ObserverAction::Pause(0)
        );
        assert_eq!(route_command(TrayCommand::Resume), ObserverAction::Resume);
        for command in [
            TrayCommand::OpenJournal,
            TrayCommand::OpenUrl("https://example.test"),
            TrayCommand::OpenConfig,
            TrayCommand::CopyInstructions,
        ] {
            assert_eq!(route_command(command), ObserverAction::Desktop(command));
        }
    }

    // AC: 4 — the production wake dispatcher applies a command before the next tick.
    #[test]
    fn command_between_ticks_has_bounded_latency() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            sender.send(TrayCommand::Pause(900)).unwrap();
        });
        let (mut observer, _) = command_observer();
        let ticks = Arc::new(AtomicUsize::new(0));
        let tick_count = Arc::clone(&ticks);
        let started = Instant::now();
        let command = receiver.recv_timeout(TICK_INTERVAL).unwrap();
        dispatch_wake(
            &mut observer,
            LoopWake::Command(command),
            route_test_command,
            move |_| {
                tick_count.fetch_add(1, Ordering::AcqRel);
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        worker.join().unwrap();
        assert!(observer.snapshot.paused);
        assert_eq!(ticks.load(Ordering::Acquire), 0);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // AC: 4 — real command routing publishes distinct timed anchors and all indefinite variants.
    #[test]
    fn pause_durations_produce_distinct_anchors() {
        let (mut observer, receiver) = command_observer();
        let mut anchors = Vec::new();
        for seconds in [900, 1800, 3600] {
            dispatch_wake(
                &mut observer,
                LoopWake::Command(TrayCommand::Pause(seconds)),
                route_test_command,
                |_| Ok::<(), ()>(()),
            )
            .unwrap();
            let published = receiver.borrow().clone();
            assert!(published.paused);
            anchors.push(published.pause_until.unwrap());
        }
        assert_eq!(anchors, [942.0, 1842.0, 3642.0]);

        let (sender, commands) = std::sync::mpsc::channel();
        let dbus = CommandSender::new(sender);
        dbus.pause(clamp_pause(0));
        dbus.pause(clamp_pause(-1));
        for command in [
            TrayCommand::PauseIndefinite,
            commands.recv().unwrap(),
            commands.recv().unwrap(),
        ] {
            dispatch_wake(
                &mut observer,
                LoopWake::Command(command),
                route_test_command,
                |_| Ok::<(), ()>(()),
            )
            .unwrap();
            let published = receiver.borrow().clone();
            assert!(published.paused);
            assert_eq!(published.pause_until, None);
        }
    }

    // AC: 4 — absolute deadlines advance without drift and skip catch-up storms.
    #[test]
    fn tick_deadline_advances_absolutely_and_skips_storms() {
        let start = Instant::now();
        assert_eq!(advance_tick_deadline(start, start), start + TICK_INTERVAL);
        let one_second_late = start + Duration::from_secs(1);
        assert_eq!(
            advance_tick_deadline(start, one_second_late),
            start + TICK_INTERVAL
        );
        let six_seconds_late = start + Duration::from_secs(6);
        let reset = advance_tick_deadline(start, six_seconds_late);
        assert_eq!(reset, six_seconds_late + TICK_INTERVAL);
        assert!(reset > six_seconds_late);
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
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            "host",
            "linux",
            "test",
            Arc::new(SystemClock::new()),
        ));
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
    async fn shutdown_order_includes_linked_owner_last() {
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
            || async { Ok(()) },
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
                "linked_owner_shutdown",
                "linked_owner_join_complete",
            ]
        );
    }

    #[tokio::test]
    async fn linked_shutdown_failure_preserves_prior_shutdown_results() {
        let results = shutdown_in_order(
            (),
            |_| Err(ObserverError::Io("observer failed".into())),
            async {},
            async {
                Err(tokio::task::spawn(async { panic!("sync failed") })
                    .await
                    .unwrap_err())
            },
            || async { Err(ObserverError::Io("sender failed".into())) },
            || async { Err(ObserverError::Io("linked failed".into())) },
            &mut |_| {},
        )
        .await;
        assert!(results.0.is_err());
        assert!(results.1.is_err());
        assert!(results.2.is_err());
        assert!(results.3.is_err());
    }

    #[tokio::test]
    async fn shutdown_waits_for_active_sync_and_event_work() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sync_events = events.clone();
        let sender_events = events.clone();
        let linked_events = events.clone();
        let results = shutdown_in_order(
            (),
            |_| Ok(()),
            async {},
            async move {
                tokio::task::yield_now().await;
                sync_events.lock().unwrap().push("sync_complete");
                Ok(())
            },
            move || async move {
                sender_events.lock().unwrap().push("sender_complete");
                Ok(())
            },
            move || async move {
                linked_events.lock().unwrap().push("linked_complete");
                Ok(())
            },
            &mut |_| {},
        )
        .await;
        assert!(results.0.is_ok());
        assert!(results.1.is_ok());
        assert!(results.2.is_ok());
        assert!(results.3.is_ok());
        assert_eq!(
            &*events.lock().unwrap(),
            &["sync_complete", "sender_complete", "linked_complete"]
        );
    }

    async fn drive_real_link_start(
        temp: &tempfile::TempDir,
        transport_enabled: bool,
    ) -> (Result<PrivateLinkOwner, PrivateStateError>, LinkFactState) {
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "stream".to_owned(),
            ..Config::default()
        };
        let upload = Arc::new(UploadClient::new(
            &config,
            None::<PrivateLinkCapability>,
            "host",
            "linux",
            "1",
            Arc::new(SystemClock::new()),
        ));
        let lock = PrivateStateLock::acquire(temp.path()).unwrap();
        let start = tokio::spawn(start_linked_owner(
            upload.clone(),
            temp.path().to_path_buf(),
            "stream".to_owned(),
            lock,
            transport_enabled,
        ));
        assert_capture_ticks_advance();
        let result = start.await.unwrap();
        (result, upload.link_fact_state().unwrap())
    }

    fn assert_capture_ticks_advance() {
        let notifier = RecordingNotifier::default();
        let mut ticks = 0_usize;
        for _ in 0..2 {
            tick_once(&notifier, || {
                ticks += 1;
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(ticks, 2);
        assert_eq!(notifier.watchdogs.load(Ordering::Acquire), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_credentials_capture_without_transport() {
        let temp = tempfile::tempdir().unwrap();
        let pending = temp.path().join("pending.segment");
        std::fs::write(&pending, b"pending").unwrap();
        let (result, facts) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.pairing_required);
        assert!(pending.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_credentials_capture_without_transport() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(CREDENTIALS_FILENAME), b"{").unwrap();
        let (result, facts) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.private_state_invalid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mismatched_or_corrupt_observer_capture_then_register() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        persist_credential(temp.path(), &peer.credential()).unwrap();
        std::fs::write(temp.path().join(OBSERVER_FILENAME), b"{").unwrap();
        peer.enqueue_response(
            200,
            serde_json::json!({
                "key":"K", "name":"stream", "prefix":"prefix",
                "ingest_url":"/app/observer/ingest", "protocol_version":2
            })
            .to_string(),
        );
        let (result, facts) = drive_real_link_start(&temp, true).await;
        let owner = result.unwrap();
        assert!(facts.observer_registered);
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unavailable_carrier_capture_without_transport_wait() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        persist_credential(temp.path(), &peer.credential()).unwrap();
        peer.shutdown().await;
        let (result, facts) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.transport_unavailable);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_bootstrap_capture_without_transport_wait() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        persist_credential(temp.path(), &peer.credential()).unwrap();
        peer.shutdown().await;
        let (result, facts) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.transport_unavailable);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_initial_registration_capture_without_transport_wait() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        persist_credential(temp.path(), &peer.credential()).unwrap();
        peer.enqueue_response(503, Vec::new());
        let (result, facts) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.transport_unavailable);
        assert_eq!(peer.requests().len(), 1);
        peer.shutdown().await;
    }

    async fn assert_real_initial_registration_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        enqueue_registration(&peer);
        let gate = Arc::new(AtomicBool::new(false));
        peer.gate_next_response_nonblocking(gate.clone());
        let owner = tokio::spawn({
            let credential = peer.credential();
            let root = temp.path().to_path_buf();
            async move { start_private_link_owner(&root, credential, "stream").await }
        });
        peer.wait_for_requests(1).await;
        assert!(!owner.is_finished());
        assert_capture_ticks_advance();
        gate.store(true, Ordering::Release);
        peer.notify_response_gates();
        let owner = owner.await.unwrap().unwrap();
        assert_eq!(peer.requests().len(), 1);
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    fn enqueue_registration(peer: &PrivateLinkPeer) {
        peer.enqueue_response(
            200,
            serde_json::json!({
                "key":"K",
                "name":"stream",
                "prefix":"prefix",
                "ingest_url":"/app/observer/ingest",
                "protocol_version":2
            })
            .to_string(),
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multithreaded_capture_advances_while_real_link_registers() {
        assert_real_initial_registration_succeeds().await;
    }

    #[tokio::test]
    async fn concurrent_initial_demand_performs_one_registration() {
        assert_concurrent_initial_registration(200, true).await;
    }

    #[tokio::test]
    async fn initial_registration_waiters_share_one_success() {
        assert_concurrent_initial_registration(200, true).await;
    }

    #[tokio::test]
    async fn initial_registration_waiters_share_one_unavailable_result() {
        assert_concurrent_initial_registration(503, false).await;
    }

    async fn assert_concurrent_initial_registration(status: u16, expected: bool) {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(
            status,
            serde_json::json!({
                "key":"K",
                "name":"stream",
                "prefix":"prefix",
                "ingest_url":"/app/observer/ingest",
                "protocol_version":2
            })
            .to_string(),
        );
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let capability = session.capability("/app/observer/ingest".to_owned());
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "stream".to_owned(),
            ..Config::default()
        };
        let client = Arc::new(UploadClient::new(
            &config,
            capability,
            "host",
            "linux",
            "1",
            Arc::new(SystemClock::new()),
        ));
        let mut demands = Vec::new();
        for _ in 0..3 {
            let client = client.clone();
            let mut config = config.clone();
            demands.push(tokio::spawn(async move {
                client.ensure_registered(&mut config).await
            }));
        }
        for demand in demands {
            assert_eq!(demand.await.unwrap(), expected);
        }
        assert_eq!(peer.requests().len(), 1);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn linked_owner_holds_lock_through_bridge_task_join() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        enqueue_registration(&peer);
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        owner.shutdown().await.unwrap();
        let lock = PrivateStateLock::acquire(temp.path()).unwrap();
        drop(lock);
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn private_state_lock_releases_only_after_join() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        enqueue_registration(&peer);
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        owner.shutdown().await.unwrap();
        assert!(PrivateStateLock::acquire(temp.path()).is_ok());
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_and_runtime_contend_on_same_canonical_lock() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        enqueue_registration(&peer);
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            crate::private_link::setup(temp.path(), "device", std::io::Cursor::new(b"pair")).await,
            Err(PrivateStateError::LockContended)
        ));
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_lock_failure_does_not_mutate_capture_config_or_private_state() {
        let temp = tempfile::tempdir().unwrap();
        let lock = PrivateStateLock::acquire(temp.path()).unwrap();
        let before = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(matches!(
            crate::cli::prepare_run_config(crate::config::ConfigPaths {
                base_dir: Some(temp.path().join("data")),
                config_dir: Some(temp.path().to_path_buf()),
            }),
            Err(PrivateStateError::LockContended)
        ));
        let after = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
        drop(lock);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sanitation_failure_keeps_capture_advancing_and_exposes_fact() {
        let temp = tempfile::tempdir().unwrap();
        let (result, facts) = drive_real_link_start(&temp, false).await;
        assert!(result.is_err());
        assert!(facts.config_sanitation_failed);
    }

    async fn linked_upload_fixture(
        temp: &tempfile::TempDir,
        peer: &PrivateLinkPeer,
    ) -> (crate::private_link::PrivateLinkSession, Arc<UploadClient>) {
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".to_owned(),
                name: "stream".to_owned(),
                ingest_url: "/app/observer/ingest".to_owned(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "stream".to_owned(),
            ..Config::default()
        };
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability("/app/observer/ingest".to_owned()),
            "host",
            "linux",
            "1",
            Arc::new(SystemClock::new()),
        ));
        (session, client)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_backpressured_upload_does_not_stop_capture_progress() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(200, br#"{"status":"ok","segment":"large"}"#.to_vec());
        let gate = Arc::new(AtomicBool::new(false));
        peer.gate_next_response_nonblocking(gate.clone());
        let (session, client) = linked_upload_fixture(&temp, &peer).await;
        let media = temp.path().join("large.webm");
        std::fs::write(&media, vec![b'x'; 17 * 1024 * 1024]).unwrap();
        let upload =
            tokio::spawn(async move { client.upload_segment("20260101", "large", &[media]).await });
        peer.wait_for_requests(1).await;
        assert_capture_ticks_advance();
        assert!(!upload.is_finished());
        gate.store(true, Ordering::Release);
        peer.notify_response_gates();
        assert!(upload.await.unwrap().success);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chunked_rejection_does_not_stop_capture_progress() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let (session, _client) = linked_upload_fixture(&temp, &peer).await;
        let stream =
            futures_util::stream::once(async { Ok::<_, std::io::Error>(vec![b'x'; 1024]) });
        let form = reqwest::multipart::Form::new().part(
            "files",
            reqwest::multipart::Part::stream(reqwest::Body::wrap_stream(stream)),
        );
        let request = tokio::spawn({
            let capability = session.capability("/app/observer/ingest".to_owned());
            async move { capability.ingest(form).await }
        });
        assert_capture_ticks_advance();
        match request.await.unwrap() {
            crate::private_link::LinkOutcome::LocalRejected { status } => {
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
            }
            crate::private_link::LinkOutcome::Success { status, .. } => {
                panic!("chunked request unexpectedly succeeded with {status}");
            }
            crate::private_link::LinkOutcome::Unauthorized { .. } => {
                panic!("chunked request unexpectedly reached authority");
            }
            crate::private_link::LinkOutcome::Forbidden => {
                panic!("chunked request unexpectedly reached guard");
            }
            crate::private_link::LinkOutcome::TransportUnavailable => {
                panic!("chunked rejection was reported as transport unavailable");
            }
        }
        assert!(peer.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
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
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            "host",
            "linux",
            "test",
            Arc::new(SystemClock::new()),
        ));
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
