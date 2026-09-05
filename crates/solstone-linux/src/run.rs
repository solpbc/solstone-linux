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

#[cfg(test)]
use crate::private_link::{start_private_link_owner, start_private_link_session};
use crate::{
    activity::CompositeActivityProbe,
    audio::{backend::PulseAudioCapture, writer::FlacAudioWriter},
    config::Config,
    observer::{
        Backends, BackgroundCaptureStats, Clock, EventSink, Mode, Observer, ObserverError,
        SegmentCompletedEvent, StateSnapshot, StoppedStream, VideoCapture, VideoStream,
        WatchStateSink, lifecycle,
    },
    private_link::{
        PrivateLinkCapability, PrivateStateLock, load_credential,
        start_private_link_owner_with_lock,
    },
    recovery::{ClaxonMediaDurationProbe, recover_incomplete_segments},
    shell::{CommandSender, ConnectionRequester, ShellInputs, stashed},
    sync::{SyncService, SyncTrigger},
    sync_health::ProcessEpoch,
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
use ashpd::zbus::Connection;
use sd_notify::NotifyState;
use tokio_util::sync::CancellationToken;

const TICK_INTERVAL: Duration = Duration::from_secs(5);
const STARTUP_WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);
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

fn begin_startup(notifier: Arc<dyn ServiceNotifier>) -> WatchdogHeartbeat {
    begin_startup_with_interval(notifier, STARTUP_WATCHDOG_INTERVAL)
}

fn begin_startup_with_interval(
    notifier: Arc<dyn ServiceNotifier>,
    interval: Duration,
) -> WatchdogHeartbeat {
    if let Err(error) = notifier.ready() {
        tracing::warn!(%error, "Failed to notify systemd readiness");
    }
    // Portal consent and the subsequent observer initialization may each block. Keep the
    // startup heartbeat alive through both; the normal tick loop takes over after startup.
    WatchdogHeartbeat::spawn(notifier, interval)
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

async fn shutdown_in_order<O, DF, SF, JF, LS, LF>(
    mut observer: O,
    shutdown_callbacks: (
        impl FnOnce(&mut O) -> Result<(), ObserverError>,
        impl FnOnce(),
    ),
    desktop_shutdown: DF,
    sync_shutdown: SF,
    linked_lifecycle: (JF, impl FnOnce(LS) -> LF),
    trace: &mut dyn FnMut(&'static str),
) -> (
    Result<(), ObserverError>,
    Result<(), ObserverError>,
    Result<(), ObserverError>,
)
where
    DF: std::future::Future<Output = ()>,
    SF: std::future::Future<Output = Result<(), tokio::task::JoinError>>,
    JF: std::future::Future<Output = LS>,
    LF: std::future::Future<Output = Result<(), ObserverError>>,
{
    let (observer_shutdown, disable_open_journal) = shutdown_callbacks;
    let (linked_start_join, linked_shutdown) = linked_lifecycle;
    trace("open_journal_disabled");
    disable_open_journal();
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
    trace("linked_start_join");
    let linked_start = linked_start_join.await;
    trace("linked_start_join_complete");
    trace("linked_owner_shutdown");
    let linked_result = linked_shutdown(linked_start).await;
    trace("linked_owner_join_complete");
    (observer_result, sync_result, linked_result)
}

// Runtime order is a safety contract:
// 1. CLI session readiness gate completes before this module is entered.
// 2. lifecycle setup acquires the singleton bus name and performs no capture work.
// 3. recovery finalizes old incomplete segments while singleton ownership is held.
// 4. the run closure constructs capture backends, sync, and the observer.
// 5. initialize publishes the first snapshot, then desktop surfaces start and commands wake the
//    absolute-deadline tick loop.
// 6. desktop surfaces stop first, then observer capture/audio cleanup and sync.
// 7. the linked owner closes streams and joins bridge/carrier tasks last, then releases its lock.
pub(crate) fn run_observer(
    config: Config,
    state_lock: PrivateStateLock,
    transport_enabled: bool,
    process_epoch: Option<ProcessEpoch>,
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
                connection,
                state_lock,
                transport_enabled,
                process_epoch,
            )
        },
        || Ok(()),
        || false,
    )
}

fn run_capture(
    runtime: &tokio::runtime::Runtime,
    config: Config,
    connection: Option<Connection>,
    state_lock: PrivateStateLock,
    transport_enabled: bool,
    process_epoch: Option<ProcessEpoch>,
) -> Result<(), ObserverError> {
    let notifier: Arc<dyn ServiceNotifier> = Arc::new(SdNotifier);
    let stopped = Arc::new(AtomicBool::new(false));
    let portal_startup_cancellation = CancellationToken::new();
    spawn_signal_task(Arc::clone(&stopped), portal_startup_cancellation.clone())
        .map_err(ObserverError::Io)?;
    let startup_heartbeat = begin_startup(Arc::clone(&notifier));
    let video = VideoBackend::new(&config, portal_startup_cancellation)
        .map_err(ObserverError::VideoStart)?;
    let (audio, mute) = PulseAudioCapture::spawn().map_err(ObserverError::Io)?;
    let clock = SystemClock::new();
    let upload = Arc::new(UploadClient::new(
        &config,
        None::<PrivateLinkCapability>,
        Arc::new(clock.clone()),
    ));
    let open_journal = crate::private_link::OpenJournalAccess::default();
    if process_epoch.is_none() {
        upload.publish_link_fact(crate::private_link::LinkFact::PrivateStateInvalid);
    }
    let linked_upload = Arc::clone(&upload);
    let linked_root = config.config_dir.clone();
    let linked_stream = config.stream.clone();
    let linked_state_lock = state_lock
        .try_clone()
        .map_err(|error| ObserverError::Io(format!("linked state lock clone failed: {error}")))?;
    let linked_start = if process_epoch.is_some() {
        runtime.spawn(start_linked_owner(
            linked_upload,
            linked_root,
            linked_stream,
            linked_state_lock,
            transport_enabled,
            open_journal.clone(),
        ))
    } else {
        runtime.spawn(async {
            Err::<crate::private_link::PrivateLinkOwner, _>(
                crate::private_link::PrivateStateError::BridgeUnavailable,
            )
        })
    };
    let sync = SyncService::start_with_epoch(
        config.clone(),
        Arc::clone(&upload),
        Arc::new(clock.clone()),
        process_epoch,
    );
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
        events: UploadEventSink { sync: sync_trigger },
        clock,
        stats: BackgroundCaptureStats::new(),
        states,
    };
    let mut observer = Observer::new(config, backends);
    let initialized = !stopped.load(Ordering::Acquire);
    let mut run_result = if initialized {
        observer.initialize()
    } else {
        Ok(())
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
            open_journal: open_journal.clone(),
        },
    );
    if initialized && run_result.is_ok() {
        // systemd readiness precedes portal and observer initialization so its watchdog can
        // cover that work. This log marks the later point after observer initialization and
        // desktop-surface startup have both been attempted.
        tracing::info!("observer initialization complete");
    }
    let mut next_tick = Instant::now() + TICK_INTERVAL;
    // Startup can include portal consent and observer initialization. Once the tick loop is
    // ready, its successful five-second ticks own the watchdog heartbeat.
    drop(startup_heartbeat);
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
                    |observer, command| apply_command(observer, command, &open_journal),
                    |observer| tick_once(notifier.as_ref(), || observer.tick()),
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                run_result = dispatch_wake(
                    &mut observer,
                    LoopWake::Tick,
                    |observer, command| apply_command(observer, command, &open_journal),
                    |observer| tick_once(notifier.as_ref(), || observer.tick()),
                );
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
    let (shutdown, sync_shutdown, linked_shutdown) = runtime.block_on(shutdown_in_order(
        observer,
        (Observer::shutdown, || open_journal.close_current()),
        desktop_shell.shutdown(SHUTDOWN_TIMEOUT),
        sync.shutdown(SHUTDOWN_TIMEOUT),
        (linked_start, |linked_start| async move {
            match linked_start {
                Ok(Ok(owner)) => owner
                    .shutdown()
                    .await
                    .map_err(|error| ObserverError::Io(format!("linked shutdown failed: {error}"))),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Linked transport remained unavailable");
                    Ok(())
                }
                Err(error) => Err(ObserverError::Io(format!(
                    "linked startup task failed: {error}"
                ))),
            }
        }),
        &mut |stage| tracing::info!(stage, "shutdown progress"),
    ));
    run_result
        .and(shutdown)
        .and(sync_shutdown)
        .and(linked_shutdown)
}

async fn start_linked_owner(
    upload: Arc<UploadClient>,
    config_root: PathBuf,
    stream: String,
    state_lock: PrivateStateLock,
    transport_enabled: bool,
    open_journal: crate::private_link::OpenJournalAccess,
) -> Result<crate::private_link::PrivateLinkOwner, crate::private_link::PrivateStateError> {
    upload.begin_owner_generation();
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
    let mut owner =
        start_private_link_owner_with_lock(state_lock, credential, &stream, upload.link_facts())
            .await
            .inspect_err(|_| {
                upload.publish_link_fact(crate::private_link::LinkFact::TransportUnavailable);
            })?;
    owner.install_open_journal_access(open_journal);
    upload.install_capability(owner.capability());
    Ok(owner)
}

fn apply_command<V, A, P, M, W, E, C, Q, N>(
    observer: &mut Observer<V, A, P, M, W, E, C, Q, N>,
    command: TrayCommand,
    open_journal: &crate::private_link::OpenJournalAccess,
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
            if let Err(error) = crate::desktop_component::DesktopComponent::with_open_journal(
                observer.config.clone(),
                open_journal.clone(),
            )
            .perform_desktop_command(command)
            {
                tracing::warn!(%error, "Failed to perform desktop command");
                if matches!(command, TrayCommand::OpenJournal) {
                    let message = crate::desktop_component::OPEN_JOURNAL_REMEDIATION;
                    if let Err(notification_error) = notify_rust::Notification::new()
                        .summary("solstone app")
                        .body(message)
                        .show()
                    {
                        tracing::warn!(
                            %notification_error,
                            "Failed to show Open Journal notification"
                        );
                    }
                }
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

fn spawn_signal_task(
    stopped: Arc<AtomicBool>,
    portal_startup_cancellation: CancellationToken,
) -> Result<(), String> {
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
        portal_startup_cancellation.cancel();
    });
    Ok(())
}

enum VideoBackend {
    Portal(PortalVideoCapture),
    X11(X11VideoCapture),
}

impl VideoBackend {
    fn new(
        config: &Config,
        portal_startup_cancellation: CancellationToken,
    ) -> Result<Self, String> {
        match select_backend(
            env::var("XDG_SESSION_TYPE").ok().as_deref(),
            env::var("WAYLAND_DISPLAY").ok().as_deref(),
            env::var("DISPLAY").ok().as_deref(),
        ) {
            BackendKind::X11 => X11VideoCapture::new().map(Self::X11),
            BackendKind::Portal => PortalVideoCapture::spawn_with_cancellation(
                AshpdPortalOps::new(),
                FileTokenStore::new(config.restore_token_path()),
                NativeWaylandGeometry,
                GstreamerPipelineFactory::new()?,
                portal_startup_cancellation,
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
    sync: W,
}

impl<W: SyncWake> EventSink for UploadEventSink<W> {
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
            CREDENTIALS_FILENAME, LinkFactState, PRIVATE_STATE_READY_LOCK_FILENAME,
            PrivateLinkOwner, PrivateStateError, PrivateStateLock, persist_credential,
        },
        private_link_test_peer::PrivateLinkPeer,
        sync_health::{
            ProcessEpoch, SyncFacts, derive_health, load_facts_with_liveness, save_facts,
        },
        test_support::{DayCustodyFixture, MockServer, OpportunisticDefaultListenerTrap},
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

    // AC: READY precedes the startup watchdog, which covers every blocking startup phase.
    #[test]
    fn ready_precedes_startup_watchdog() {
        let notifier = Arc::new(RecordingNotifier::default());
        let dynamic: Arc<dyn ServiceNotifier> = notifier.clone();
        let heartbeat = begin_startup_with_interval(dynamic, Duration::from_millis(1));
        assert_eq!(&*notifier.events.lock().unwrap(), &["ready"]);
        let deadline = Instant::now() + Duration::from_secs(1);
        while notifier.watchdogs.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(notifier.watchdogs.load(Ordering::Acquire) > 0);
        drop(heartbeat);
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

    // Segment completion wakes sync.
    #[tokio::test]
    async fn segment_completion_is_the_only_sync_trigger() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut sink = UploadEventSink {
            sync: CountingWake(Arc::clone(&count)),
        };
        sink.segment_completed(SegmentCompletedEvent {
            key: "120000_300".into(),
        });
        assert_eq!(count.load(Ordering::Acquire), 1);
    }

    // Desktop tasks stop before final observer work, sync, and linked-owner shutdown.
    #[tokio::test]
    async fn shutdown_order_includes_linked_owner_last() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = Arc::clone(&events);
        let disable_events = Arc::clone(&events);
        let trace_events = Arc::clone(&events);
        let mut trace = move |event| trace_events.lock().unwrap().push(event);
        let results = shutdown_in_order(
            (),
            (
                move |_| {
                    observer_events
                        .lock()
                        .unwrap()
                        .push("final_segment_trigger");
                    Ok(())
                },
                move || {
                    disable_events.lock().unwrap().push("capability_closed");
                },
            ),
            async {
                events.lock().unwrap().push("desktop_stopped");
            },
            async { Ok(()) },
            (async {}, |()| async { Ok(()) }),
            &mut trace,
        )
        .await;
        assert!(results.0.is_ok() && results.1.is_ok() && results.2.is_ok());
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "open_journal_disabled",
                "capability_closed",
                "desktop_shutdown",
                "desktop_stopped",
                "observer_shutdown",
                "final_segment_trigger",
                "sync_shutdown",
                "sync_join_complete",
                "linked_start_join",
                "linked_start_join_complete",
                "linked_owner_shutdown",
                "linked_owner_join_complete",
            ]
        );
    }

    #[tokio::test]
    async fn linked_shutdown_failure_preserves_prior_shutdown_results() {
        let results = shutdown_in_order(
            (),
            (|_| Err(ObserverError::Io("observer failed".into())), || {}),
            async {},
            async {
                Err(tokio::task::spawn(async { panic!("sync failed") })
                    .await
                    .unwrap_err())
            },
            (async {}, |()| async {
                Err(ObserverError::Io("linked failed".into()))
            }),
            &mut |_| {},
        )
        .await;
        assert!(results.0.is_err());
        assert!(results.1.is_err());
        assert!(results.2.is_err());
    }

    #[tokio::test]
    async fn shutdown_waits_for_active_sync_and_linked_work() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sync_events = events.clone();
        let linked_events = events.clone();
        let results = shutdown_in_order(
            (),
            (|_| Ok(()), || {}),
            async {},
            async move {
                tokio::task::yield_now().await;
                sync_events.lock().unwrap().push("sync_complete");
                Ok(())
            },
            (async {}, move |()| async move {
                linked_events.lock().unwrap().push("linked_complete");
                Ok(())
            }),
            &mut |_| {},
        )
        .await;
        assert!(results.0.is_ok());
        assert!(results.1.is_ok());
        assert!(results.2.is_ok());
        assert_eq!(
            &*events.lock().unwrap(),
            &["sync_complete", "linked_complete"]
        );
    }

    async fn drive_real_link_start(
        temp: &tempfile::TempDir,
        transport_enabled: bool,
    ) -> (
        Result<PrivateLinkOwner, PrivateStateError>,
        LinkFactState,
        Arc<UploadClient>,
    ) {
        let legacy_origin = MockServer::new(Vec::new()).await;
        let default_listener = OpportunisticDefaultListenerTrap::bind();
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "stream".to_owned(),
            ..Config::default()
        };
        let upload = Arc::new(UploadClient::new(
            &config,
            None::<PrivateLinkCapability>,
            Arc::new(SystemClock::new()),
        ));
        let lock = PrivateStateLock::acquire(temp.path()).unwrap();
        let start = tokio::spawn(start_linked_owner(
            upload.clone(),
            temp.path().to_path_buf(),
            "stream".to_owned(),
            lock,
            transport_enabled,
            crate::private_link::OpenJournalAccess::default(),
        ));
        assert_real_observer_ticks_advance();
        let result = start.await.unwrap();
        assert!(legacy_origin.requests().is_empty());
        default_listener.assert_zero_connections();
        (result, upload.link_fact_state().unwrap(), upload)
    }

    fn assert_real_observer_ticks_advance() {
        let notifier = RecordingNotifier::default();
        tick_once(&notifier, || {
            crate::observer::tests::drive_real_observer_ticks();
            Ok(())
        })
        .unwrap();
        assert_eq!(notifier.watchdogs.load(Ordering::Acquire), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_credentials_observer_ticks_without_transport() {
        let temp = tempfile::tempdir().unwrap();
        let (result, facts, _upload) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.pairing_required);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_credentials_observer_ticks_without_transport() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(CREDENTIALS_FILENAME), b"{").unwrap();
        let (result, facts, _upload) = drive_real_link_start(&temp, true).await;
        assert!(result.is_err());
        assert!(facts.private_state_invalid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unavailable_peer_owner_starts_without_a_carrier_dial() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        persist_credential(temp.path(), &peer.credential()).unwrap();
        peer.shutdown().await;
        let (result, facts, _upload) = drive_real_link_start(&temp, true).await;
        let owner = result.expect("owner startup does not require a carrier dial");
        assert!(facts.listener_ready);
        assert!(!facts.transport_unavailable);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn linked_owner_holds_lock_through_bridge_task_join() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let mut owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let open_journal = crate::private_link::OpenJournalAccess::default();
        owner.install_open_journal_access(open_journal.clone());
        assert!(open_journal.available());
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        let joined = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let shutdown =
            tokio::spawn(owner.shutdown_with_join_probe(joined.clone(), release.clone()));
        joined.notified().await;
        assert!(!shutdown.is_finished());
        assert!(!open_journal.available());
        assert!(open_journal.open().is_err());
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        release.notify_one();
        shutdown.await.unwrap().unwrap();
        let lock = PrivateStateLock::acquire(temp.path()).unwrap();
        drop(lock);
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn private_state_lock_releases_only_after_join() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        let joined = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let shutdown =
            tokio::spawn(owner.shutdown_with_join_probe(joined.clone(), release.clone()));
        joined.notified().await;
        assert!(matches!(
            PrivateStateLock::acquire(temp.path()),
            Err(PrivateStateError::LockContended)
        ));
        release.notify_one();
        shutdown.await.unwrap().unwrap();
        assert!(PrivateStateLock::acquire(temp.path()).is_ok());
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn setup_and_runtime_contend_on_same_canonical_lock() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let owner = start_private_link_owner(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        assert!(matches!(
            crate::private_link::setup(
                temp.path(),
                &temp.path().join("state"),
                "device",
                std::io::Cursor::new(b"pair")
            )
            .await,
            Err(PrivateStateError::LockContended)
        ));
        owner.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn prepare_run_config_lock_failure_does_not_mutate_config_or_private_state() {
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

    #[tokio::test]
    async fn prepare_run_config_reset_failure_releases_lock_and_starts_no_private_link_transport() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let base_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(&base_dir).unwrap();
        let state_path = base_dir.join("state");
        let prior_bytes = br#"{"schema_version":2,"link_epoch":"0808080808080808080808080808080808080808080808080808080808080808","link":{"listener_ready":true,"carrier_proven":true,"observer_registered":true}}"#;
        std::fs::write(&state_path, prior_bytes).unwrap();

        assert!(matches!(
            crate::cli::prepare_run_config(crate::config::ConfigPaths {
                base_dir: Some(base_dir.clone()),
                config_dir: Some(config_dir.clone()),
            }),
            Err(PrivateStateError::HealthInitializationFailed)
        ));
        assert_eq!(std::fs::read(&state_path).unwrap(), prior_bytes);
        assert!(!config_dir.join(PRIVATE_STATE_READY_LOCK_FILENAME).exists());
        let reacquired = PrivateStateLock::acquire(&config_dir).unwrap();
        drop(reacquired);
        assert!(peer.requests().is_empty());
        assert_eq!(peer.accepted_carriers(), 0);

        std::fs::remove_file(&state_path).unwrap();
        save_facts(
            &state_path,
            &SyncFacts {
                pending_confirmed: Some(0),
                link: Some(LinkFactState {
                    listener_ready: true,
                    carrier_proven: true,
                    observer_registered: true,
                    ..Default::default()
                }),
                link_epoch: Some(ProcessEpoch::for_test(8)),
                ..Default::default()
            },
        )
        .unwrap();
        let unready_owner = PrivateStateLock::acquire(&config_dir).unwrap();
        let liveness = PrivateStateLock::try_probe(&config_dir).unwrap();
        assert_eq!(
            liveness,
            crate::private_link::PrivateStateLockLiveness::LiveOwnerNotReady
        );
        let facts = load_facts_with_liveness(&state_path, liveness);
        assert!(facts.link.is_none());
        assert!(!matches!(
            derive_health(&facts, 1_000.0, 600.0).state,
            crate::sync_health::HealthState::ListenerReady
                | crate::sync_health::HealthState::Syncing
                | crate::sync_health::HealthState::Connected
        ));
        drop(unready_owner);
        let mut ready_owner = PrivateStateLock::acquire(&config_dir).unwrap();
        ready_owner.mark_ready().unwrap();
        let ready_liveness = PrivateStateLock::try_probe(&config_dir).unwrap();
        assert_eq!(
            ready_liveness,
            crate::private_link::PrivateStateLockLiveness::LiveOwner
        );
        assert!(
            load_facts_with_liveness(&state_path, ready_liveness)
                .link
                .is_some()
        );
        peer.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_transport_keeps_observer_ticks_advancing_and_exposes_sanitation_fact() {
        let temp = tempfile::tempdir().unwrap();
        let (result, facts, _upload) = drive_real_link_start(&temp, false).await;
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
        let config = Config {
            config_dir: temp.path().to_path_buf(),
            stream: "stream".to_owned(),
            ..Config::default()
        };
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability(),
            Arc::new(SystemClock::new()),
        ));
        (session, client)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_large_upload_response_does_not_stop_observer_ticks() {
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
        assert_real_observer_ticks_advance();
        assert!(!upload.is_finished());
        gate.store(true, Ordering::Release);
        peer.notify_response_gates();
        assert!(upload.await.unwrap().success);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn chunked_rejection_does_not_stop_observer_ticks() {
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
            let capability = session.capability();
            async move { capability.ingest(form).await }
        });
        assert_real_observer_ticks_advance();
        match request.await.unwrap() {
            crate::private_link::LinkOutcome::LocalRejected { status } => {
                assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
            }
            crate::private_link::LinkOutcome::Success { status, .. } => {
                panic!("chunked request unexpectedly succeeded with {status}");
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

    mod upgrade_composition {
        use super::*;
        use crate::{
            cli::dispatch_setup_with_pairer_for_test,
            config::{ConfigPaths, sanitize_link_authority_with_fault},
            private_file::{DurableWriteFault, DurableWriteStage},
            private_link::{Pairer, PrivateStateLock, load_credential},
            sync::cleanup_synced_day_for_composition,
        };
        use sha2::{Digest, Sha256};
        use std::{
            fs,
            future::Future,
            io::Cursor,
            os::unix::fs::PermissionsExt,
            path::{Path, PathBuf},
            pin::Pin,
            sync::atomic::{AtomicUsize, Ordering},
        };

        struct CompositionPairer {
            credential: spl_transport::credential::Credential,
            calls: Arc<AtomicUsize>,
            fail: bool,
        }

        impl Pairer for CompositionPairer {
            fn pair<'a>(
                &'a self,
                _link: &'a str,
                _device_label: &'a str,
                _additional_fields: &'a serde_json::Map<String, serde_json::Value>,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                spl_transport::credential::Credential,
                                PrivateStateError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    if self.fail {
                        Err(PrivateStateError::PairingFailed)
                    } else {
                        Ok(self.credential.clone())
                    }
                })
            }
        }

        struct FailStage(DurableWriteStage);

        impl DurableWriteFault for FailStage {
            fn before(&self, stage: DurableWriteStage) -> io::Result<()> {
                if stage == self.0 {
                    Err(io::Error::other("injected upgrade sanitation failure"))
                } else {
                    Ok(())
                }
            }
        }

        fn old_config() -> serde_json::Value {
            serde_json::json!({
                "server_url": "https://legacy.invalid/private",
                "key": "legacy-key-sentinel",
                "chat_bridge_enabled": true,
                "stream": "desktop",
                "segment_interval": 173,
                "sync_max_retries": 0,
                "cache_retention_days": 0,
                "capture_framerate": 7,
                "draw_cursor": false,
                "start_paused": false
            })
        }

        fn write_old_config(paths: &ConfigPaths) {
            let root = paths.config_dir.as_ref().unwrap();
            fs::create_dir_all(root).unwrap();
            fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(
                root.join("config.json"),
                serde_json::to_vec_pretty(&old_config()).unwrap(),
            )
            .unwrap();
        }

        fn create_pending(config: &Config, day: &str) -> Vec<(PathBuf, Vec<u8>)> {
            ["120000_173", "120173_173", "120346_173"]
                .into_iter()
                .enumerate()
                .map(|(index, name)| {
                    let path = config.captures_dir().join(day).join("archon").join(name);
                    let body = format!("byte-distinct-upgrade-segment-{index}").into_bytes();
                    fs::create_dir_all(&path).unwrap();
                    fs::write(path.join("screen.webm"), &body).unwrap();
                    (path, body)
                })
                .collect()
        }

        fn assert_pending_unchanged(pending: &[(PathBuf, Vec<u8>)], present: &[bool]) {
            for ((path, bytes), expected) in pending.iter().zip(present) {
                assert_eq!(path.exists(), *expected, "{}", path.display());
                assert!(!path.with_extension("failed").exists());
                if *expected {
                    assert_eq!(fs::read(path.join("screen.webm")).unwrap(), *bytes);
                }
            }
        }

        fn custody_listing(day: &str, path: &Path) -> DayCustodyFixture {
            let bytes = fs::read(path.join("screen.webm")).unwrap();
            let sha = format!("{:x}", Sha256::digest(&bytes));
            let key = path.file_name().unwrap().to_string_lossy();
            DayCustodyFixture::new(
                day,
                vec![serde_json::json!({
                    "key": key,
                    "files": [{
                        "name": "screen.webm",
                        "status": "present",
                        "sha256": sha,
                        "size": bytes.len(),
                    }]
                })],
            )
        }

        async fn start_owner(
            config: &Config,
            lock: PrivateStateLock,
        ) -> (PrivateLinkOwner, Arc<UploadClient>) {
            let upload = Arc::new(UploadClient::new(
                config,
                None::<PrivateLinkCapability>,
                Arc::new(SystemClock::new()),
            ));
            let owner = start_linked_owner(
                Arc::clone(&upload),
                config.config_dir.clone(),
                config.stream.clone(),
                lock,
                true,
                crate::private_link::OpenJournalAccess::default(),
            )
            .await
            .unwrap();
            assert_real_observer_ticks_advance();
            (owner, upload)
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn old_config_unpaired_capture_pair_once_custody_and_restart() {
            for stage in [
                DurableWriteStage::Create,
                DurableWriteStage::Write,
                DurableWriteStage::Fsync,
                DurableWriteStage::Rename,
                DurableWriteStage::DirSync,
            ] {
                let temp = tempfile::tempdir().unwrap();
                let paths = ConfigPaths {
                    base_dir: Some(temp.path().join("data")),
                    config_dir: Some(temp.path().join("config")),
                };
                write_old_config(&paths);
                let before =
                    fs::read(paths.config_dir.as_ref().unwrap().join("config.json")).unwrap();
                assert!(
                    sanitize_link_authority_with_fault(&paths, &FailStage(stage)).is_err(),
                    "{stage:?}"
                );
                if stage != DurableWriteStage::DirSync {
                    assert_eq!(
                        fs::read(paths.config_dir.as_ref().unwrap().join("config.json")).unwrap(),
                        before
                    );
                }
            }

            let temp = tempfile::tempdir().unwrap();
            let paths = ConfigPaths {
                base_dir: Some(temp.path().join("data")),
                config_dir: Some(temp.path().join("config")),
            };
            write_old_config(&paths);
            let peer = PrivateLinkPeer::start().await;
            let (lock, config, transport_enabled, _process_epoch) =
                crate::cli::prepare_run_config(paths.clone()).unwrap();
            assert!(transport_enabled);
            let persisted: serde_json::Value =
                serde_json::from_slice(&fs::read(config.config_path()).unwrap()).unwrap();
            for legacy in ["server_url", "key", "chat_bridge_enabled"] {
                assert!(persisted.get(legacy).is_none());
            }
            assert_eq!(config.segment_interval, 173);
            assert_eq!(config.capture_framerate, 7);
            assert!(!config.draw_cursor);
            let day = "20260101";
            let pending = create_pending(&config, day);
            assert_pending_unchanged(&pending, &[true, true, true]);

            let upload = Arc::new(UploadClient::new(
                &config,
                None::<PrivateLinkCapability>,
                Arc::new(SystemClock::new()),
            ));
            let first_start = start_linked_owner(
                Arc::clone(&upload),
                config.config_dir.clone(),
                config.stream.clone(),
                lock,
                true,
                crate::private_link::OpenJournalAccess::default(),
            )
            .await;
            assert!(first_start.is_err());
            assert!(upload.link_fact_state().unwrap().pairing_required);
            assert_real_observer_ticks_advance();
            assert!(peer.requests().is_empty());
            assert_eq!(peer.accepted_carriers(), 0);
            assert_pending_unchanged(&pending, &[true, true, true]);
            drop(upload);
            let released = PrivateStateLock::acquire(&config.config_dir).unwrap();
            drop(released);

            let failed_calls = Arc::new(AtomicUsize::new(0));
            let failed_pairer = CompositionPairer {
                credential: peer.credential(),
                calls: Arc::clone(&failed_calls),
                fail: true,
            };
            let failure_root = temp.path().join("failed-pairing");
            let mut failure_output = Vec::new();
            let mut failure_errors = Vec::new();
            assert_eq!(
                dispatch_setup_with_pairer_for_test(
                    &failed_pairer,
                    &failure_root,
                    &failure_root.join("state"),
                    "desktop",
                    Cursor::new(b"pair link with whitespace\n"),
                    &mut failure_output,
                    &mut failure_errors,
                )
                .await,
                1
            );
            assert_eq!(failed_calls.load(Ordering::SeqCst), 0);
            failure_output.clear();
            failure_errors.clear();
            assert_eq!(
                dispatch_setup_with_pairer_for_test(
                    &failed_pairer,
                    &failure_root,
                    &failure_root.join("state"),
                    "desktop",
                    Cursor::new(format!(
                        "{}\n",
                        crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
                    )),
                    &mut failure_output,
                    &mut failure_errors,
                )
                .await,
                1
            );
            assert_eq!(failed_calls.load(Ordering::SeqCst), 1);
            assert!(load_credential(&failure_root).unwrap().is_none());

            let pair_calls = Arc::new(AtomicUsize::new(0));
            let pairer = CompositionPairer {
                credential: peer.credential(),
                calls: Arc::clone(&pair_calls),
                fail: false,
            };
            let mut output = Vec::new();
            let mut errors = Vec::new();
            assert_eq!(
                dispatch_setup_with_pairer_for_test(
                    &pairer,
                    &config.config_dir,
                    &config.state_dir(),
                    "desktop",
                    Cursor::new(format!(
                        "{}\n",
                        crate::private_link::DIRECT_PAIR_LINK_FOR_TEST
                    )),
                    &mut output,
                    &mut errors,
                )
                .await,
                0
            );
            assert_eq!(pair_calls.load(Ordering::SeqCst), 1);
            assert!(errors.is_empty());
            assert!(load_credential(&config.config_dir).unwrap().is_some());

            let contended_lock = PrivateStateLock::acquire(&config.config_dir).unwrap();
            let failed_upload = Arc::new(UploadClient::new(
                &config,
                None::<PrivateLinkCapability>,
                Arc::new(SystemClock::new()),
            ));
            let failed_owner = start_linked_owner(
                Arc::clone(&failed_upload),
                config.config_dir.clone(),
                config.stream.clone(),
                contended_lock,
                true,
                crate::private_link::OpenJournalAccess::default(),
            )
            .await
            .unwrap();
            assert_pending_unchanged(&pending, &[true, true, true]);
            failed_owner.shutdown().await.unwrap();

            let restart_lock = PrivateStateLock::acquire(&config.config_dir).unwrap();
            let (owner, upload) = start_owner(&config, restart_lock).await;
            assert_eq!(pair_calls.load(Ordering::SeqCst), 1);

            peer.enqueue_response(503, Vec::new());
            let transport_failure = upload
                .upload_segment(
                    day,
                    pending[0].0.file_name().unwrap().to_str().unwrap(),
                    &[pending[0].0.join("screen.webm")],
                )
                .await;
            assert_eq!(
                transport_failure.error_type,
                Some(crate::sync_health::ErrorType::Transient)
            );
            assert_pending_unchanged(&pending, &[true, true, true]);

            peer.enqueue_response(503, Vec::new());
            cleanup_synced_day_for_composition(
                config.clone(),
                Arc::clone(&upload),
                Arc::new(SystemClock::new()),
                day,
            )
            .await;
            assert_pending_unchanged(&pending, &[true, true, true]);
            assert_real_observer_ticks_advance();

            for fixture in [
                DayCustodyFixture::new(day, Vec::new()),
                DayCustodyFixture::new(day, Vec::new()).with_segments_total(3),
            ] {
                peer.enqueue_day_custody(fixture);
                cleanup_synced_day_for_composition(
                    config.clone(),
                    Arc::clone(&upload),
                    Arc::new(SystemClock::new()),
                    day,
                )
                .await;
                assert_pending_unchanged(&pending, &[true, true, true]);
                assert_real_observer_ticks_advance();
            }

            for index in 0..pending.len() {
                peer.enqueue_day_custody(custody_listing(day, &pending[index].0));
                cleanup_synced_day_for_composition(
                    config.clone(),
                    Arc::clone(&upload),
                    Arc::new(SystemClock::new()),
                    day,
                )
                .await;
                let present = [false, index == 0, index <= 1];
                assert_pending_unchanged(&pending, &present);
            }
            owner.shutdown().await.unwrap();
            drop(upload);
            let released = PrivateStateLock::acquire(&config.config_dir).unwrap();
            drop(released);

            let requests_before_final_restart = peer.requests().len();
            let (final_lock, final_config, final_transport, _process_epoch) =
                crate::cli::prepare_run_config(paths).unwrap();
            assert!(final_transport);
            let (final_owner, final_upload) = start_owner(&final_config, final_lock).await;
            assert_eq!(pair_calls.load(Ordering::SeqCst), 1);
            assert_eq!(peer.requests().len(), requests_before_final_restart);
            assert_real_observer_ticks_advance();
            final_owner.shutdown().await.unwrap();
            drop(final_upload);
            assert!(PrivateStateLock::acquire(&final_config.config_dir).is_ok());
            peer.shutdown().await;
        }
    }
}
