// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    future::Future,
    sync::{Arc, Mutex, mpsc::Sender},
    time::Duration,
};

use ksni::TrayMethods;
use tokio::{sync::watch, task::JoinHandle};
use zbus::{Connection, fdo};

use crate::{
    config::Config,
    dbus_service::{Observer1, ObserverCommands},
    desktop_component::{BusNameRequester, ComponentSignal, DesktopComponent, SignalState},
    observer::{Clock, StateSnapshot},
    run::SystemClock,
    sync::SyncSampler,
    sync_health::SyncHealth,
    tray::{KsniTray, TrayCommand},
    tray_model::{self, TrayModel},
};

pub(crate) const OBSERVER_PATH: &str = "/org/solpbc/solstone/Observer1";

pub(crate) struct ConnectionRequester<'a> {
    pub runtime: &'a tokio::runtime::Runtime,
    pub connection: Arc<Mutex<Option<Connection>>>,
}

impl BusNameRequester for ConnectionRequester<'_> {
    fn request_name(
        &self,
        name: &str,
        flag: fdo::RequestNameFlags,
    ) -> Result<fdo::RequestNameReply, String> {
        let connection = self
            .runtime
            .block_on(Connection::session())
            .map_err(|e| e.to_string())?;
        let reply = self
            .runtime
            .block_on(connection.request_name_with_flags(name, flag.into()))
            .map_err(|e| e.to_string())?;
        stash_owned(&self.connection, connection, &reply)?;
        Ok(reply)
    }
}

fn stash_owned<T>(
    cell: &Arc<Mutex<Option<T>>>,
    value: T,
    reply: &fdo::RequestNameReply,
) -> Result<(), String> {
    if matches!(
        reply,
        fdo::RequestNameReply::PrimaryOwner | fdo::RequestNameReply::AlreadyOwner
    ) {
        match cell.lock() {
            Ok(mut slot) => *slot = Some(value),
            Err(_) => return Err("singleton connection lock poisoned".into()),
        }
    }
    Ok(())
}

pub(crate) fn stashed<T: Clone>(cell: &Arc<Mutex<Option<T>>>) -> Option<T> {
    cell.lock()
        .map(|slot| slot.clone())
        .unwrap_or_else(|error| error.into_inner().clone())
}

fn with_connection<T, R>(connection: Option<&T>, serve: impl FnOnce(&T) -> R) -> Option<R> {
    connection.map(serve)
}

#[derive(Clone)]
pub(crate) struct CommandSender {
    sender: Sender<TrayCommand>,
}

impl CommandSender {
    pub fn new(sender: Sender<TrayCommand>) -> Self {
        Self { sender }
    }

    pub fn tray_sender(&self) -> Sender<TrayCommand> {
        self.sender.clone()
    }
}

impl ObserverCommands for CommandSender {
    fn pause(&self, duration: Option<u64>) {
        let command = duration.map_or(TrayCommand::PauseIndefinite, TrayCommand::Pause);
        let _ = self.sender.send(command);
    }

    fn resume(&self) {
        let _ = self.sender.send(TrayCommand::Resume);
    }
}

trait ComponentSignalSink: Send + Sync + 'static {
    fn emit(&self, signal: ComponentSignal) -> impl Future<Output = zbus::Result<()>> + Send;
}

struct BusSignalSink {
    emitter: zbus::object_server::SignalEmitter<'static>,
}

impl ComponentSignalSink for BusSignalSink {
    async fn emit(&self, signal: ComponentSignal) -> zbus::Result<()> {
        match signal {
            ComponentSignal::StatusChanged(value) => {
                Observer1::<SystemClock, CommandSender>::status_changed(&self.emitter, &value).await
            }
            ComponentSignal::SyncProgressChanged(value) => {
                Observer1::<SystemClock, CommandSender>::sync_progress_changed(
                    &self.emitter,
                    &value,
                )
                .await
            }
        }
    }
}

pub(crate) struct ShellInputs {
    pub config: Config,
    pub clock: SystemClock,
    pub connection: Option<Connection>,
    pub snapshot: Arc<Mutex<StateSnapshot>>,
    pub health: Arc<Mutex<SyncHealth>>,
    pub progress: Arc<Mutex<String>>,
    pub tray_receiver: watch::Receiver<StateSnapshot>,
    pub signal_receiver: watch::Receiver<StateSnapshot>,
    pub sampler: SyncSampler,
    pub commands: CommandSender,
}

pub(crate) struct DesktopShell {
    render_task: Option<JoinHandle<()>>,
    apply_task: Option<JoinHandle<()>>,
    signal_task: JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    connection: Option<Connection>,
    interface_served: bool,
}

#[derive(Clone)]
struct ShellCells {
    snapshot: Arc<Mutex<StateSnapshot>>,
    health: Arc<Mutex<SyncHealth>>,
    progress: Arc<Mutex<String>>,
}

#[derive(Clone)]
struct TrayHealth {
    cell: Arc<Mutex<SyncHealth>>,
}

fn bind_consumers<C: Clock, O: ObserverCommands>(
    snapshot: Arc<Mutex<StateSnapshot>>,
    health: Arc<Mutex<SyncHealth>>,
    progress: Arc<Mutex<String>>,
    config: Config,
    clock: C,
    commands: O,
) -> (Observer1<C, O>, TrayHealth) {
    let tray_health = TrayHealth {
        cell: Arc::clone(&health),
    };
    (
        Observer1 {
            snapshot,
            health,
            progress,
            config,
            clock,
            commands,
        },
        tray_health,
    )
}

pub(crate) fn start(runtime: &tokio::runtime::Runtime, inputs: ShellInputs) -> DesktopShell {
    let component = DesktopComponent::new(inputs.config.clone());
    let initial_snapshot = inputs
        .snapshot
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|error| error.into_inner().clone());
    let initial_health = inputs
        .health
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|error| error.into_inner().clone());
    let initial_progress = inputs
        .progress
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|error| error.into_inner().clone());

    let mut interface_served = false;
    let (interface, tray_health) = bind_consumers(
        Arc::clone(&inputs.snapshot),
        Arc::clone(&inputs.health),
        Arc::clone(&inputs.progress),
        inputs.config.clone(),
        inputs.clock.clone(),
        inputs.commands.clone(),
    );
    let signal_sink = with_connection(inputs.connection.as_ref(), |connection| {
        // Setup owns the name before recovery. Observer1 can only be served after recovery and
        // video construction (including a portal consent wait that can block on a human), so
        // `busctl` may hang transiently in that startup window. That is expected and is distinct
        // from the previous permanent hang where no interface was ever served.
        match runtime.block_on(connection.object_server().at(OBSERVER_PATH, interface)) {
            Ok(_) => {
                interface_served = true;
                match runtime.block_on(
                    connection
                        .object_server()
                        .interface::<_, Observer1<SystemClock, CommandSender>>(OBSERVER_PATH),
                ) {
                    Ok(reference) => Some(BusSignalSink {
                        emitter: reference.signal_emitter().clone(),
                    }),
                    Err(error) => {
                        tracing::warn!(%error, "Failed to obtain Observer1 signal emitter");
                        None
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Failed to serve Observer1 interface");
                None
            }
        }
    })
    .flatten();
    let signal_sink = if inputs.connection.is_some() {
        signal_sink
    } else {
        tracing::error!("Singleton connection absent; desktop D-Bus interface disabled");
        None
    };

    let (shutdown, shutdown_rx) = watch::channel(false);
    let signal_task = tokio::spawn(run_shell_state(
        inputs.signal_receiver,
        ShellCells {
            snapshot: Arc::clone(&inputs.snapshot),
            health: Arc::clone(&inputs.health),
            progress: Arc::clone(&inputs.progress),
        },
        inputs.sampler,
        SignalState::new(&initial_snapshot, &initial_health, &initial_progress),
        signal_sink,
        shutdown_rx.clone(),
    ));

    let initial_model = tray_model::build(
        &initial_snapshot,
        inputs.config.segment_interval,
        inputs.clock.monotonic_seconds(),
        &initial_health,
    );
    let mut tray_handle = None;
    let registered = component.setup(
        || {
            let tray = KsniTray {
                model: initial_model.clone(),
                commands: inputs.commands.tray_sender(),
            };
            match runtime.block_on(tray.spawn()) {
                Ok(handle) => {
                    tray_handle = Some(handle);
                    true
                }
                Err(error) => {
                    tracing::debug!(%error, "Failed to register status notifier item");
                    false
                }
            }
        },
        std::thread::sleep,
    );

    let (render_task, apply_task) = if registered {
        let handle = tray_handle.expect("successful tray registration stores a handle");
        let (models, model_receiver) = watch::channel(initial_model);
        let render_task = tokio::spawn(run_tray_renderer(
            component,
            inputs.tray_receiver,
            models,
            inputs.config.segment_interval,
            inputs.clock,
            tray_health,
        ));
        let apply_task = tokio::spawn(run_tray_applier(handle, model_receiver, shutdown_rx));
        (Some(render_task), Some(apply_task))
    } else {
        (None, None)
    };

    DesktopShell {
        render_task,
        apply_task,
        signal_task,
        shutdown,
        connection: inputs.connection,
        interface_served,
    }
}

async fn run_tray_renderer(
    component: DesktopComponent,
    receiver: watch::Receiver<StateSnapshot>,
    models: watch::Sender<TrayModel>,
    segment_interval: i64,
    clock: SystemClock,
    health: TrayHealth,
) {
    component
        .watch_until_lost(receiver, move |snapshot| {
            let health = health
                .cell
                .lock()
                .map(|value| value.clone())
                .unwrap_or_else(|error| error.into_inner().clone());
            models.send_replace(tray_model::build(
                snapshot,
                segment_interval,
                clock.monotonic_seconds(),
                &health,
            ));
            Ok(())
        })
        .await;
}

async fn run_tray_applier(
    handle: ksni::Handle<KsniTray>,
    mut models: watch::Receiver<TrayModel>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            changed = models.changed() => {
                if changed.is_err() {
                    break;
                }
                let model = models.borrow_and_update().clone();
                if handle.update(move |tray| tray.model = model).await.is_none() {
                    tracing::warn!("Status notifier item closed while applying a tray model");
                    return;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    handle.shutdown().await;
}

async fn run_shell_state<S: ComponentSignalSink>(
    mut receiver: watch::Receiver<StateSnapshot>,
    cells: ShellCells,
    sampler: SyncSampler,
    mut signals: SignalState,
    sink: Option<S>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut cadence = tokio::time::interval(Duration::from_secs(1));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut snapshots_open = true;
    loop {
        tokio::select! {
            changed = receiver.changed(), if snapshots_open => match changed {
                Ok(()) => {
                    let value = receiver.borrow_and_update().clone();
                    match cells.snapshot.lock() {
                        Ok(mut current) => *current = value.clone(),
                        Err(error) => *error.into_inner() = value.clone(),
                    }
                    if let Some(signal) = signals.snapshot_changed(&value) {
                        emit(&sink, signal).await;
                    }
                }
                Err(_) => {
                    tracing::warn!("observer snapshot subscription lost; retaining last D-Bus snapshot");
                    snapshots_open = false;
                }
            },
            _ = cadence.tick() => {
                let (next_health, next_progress) = sampler.sample();
                match cells.health.lock() {
                    Ok(mut current) => *current = next_health.clone(),
                    Err(error) => *error.into_inner() = next_health.clone(),
                }
                match cells.progress.lock() {
                    Ok(mut current) => *current = next_progress.clone(),
                    Err(error) => *error.into_inner() = next_progress.clone(),
                }
                if let Some(signal) = signals.sync_changed(&next_health, &next_progress) {
                    emit(&sink, signal).await;
                }
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

async fn emit<S: ComponentSignalSink>(sink: &Option<S>, signal: ComponentSignal) {
    if let Some(sink) = sink
        && let Err(error) = sink.emit(signal).await
    {
        tracing::warn!(%error, "Failed to emit Observer1 signal");
    }
}

impl DesktopShell {
    pub(crate) async fn shutdown(mut self, timeout: Duration) {
        let _ = self.shutdown.send(true);
        if let Some(task) = &self.render_task {
            task.abort();
        }
        let removal = async {
            if self.interface_served
                && let Some(connection) = &self.connection
            {
                connection
                    .object_server()
                    .remove::<Observer1<SystemClock, CommandSender>, _>(OBSERVER_PATH)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        };
        // Three tasks can each consume two bounds (join, then post-abort join), and interface
        // removal consumes one: total worst case is seven times `timeout`.
        finish_shutdown(
            self.render_task.take(),
            self.apply_task.take(),
            Some(self.signal_task),
            removal,
            timeout,
        )
        .await;
    }
}

async fn finish_shutdown<F>(
    render_task: Option<JoinHandle<()>>,
    apply_task: Option<JoinHandle<()>>,
    signal_task: Option<JoinHandle<()>>,
    removal: F,
    timeout: Duration,
) where
    F: Future<Output = Result<(), String>>,
{
    stop_task("tray renderer", render_task, timeout).await;
    stop_task("tray applier", apply_task, timeout).await;
    stop_task("desktop signal", signal_task, timeout).await;
    match tokio::time::timeout(timeout, removal).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "Failed to remove Observer1 interface"),
        Err(_) => tracing::warn!("Observer1 interface removal timed out"),
    }
}

async fn stop_task(name: &str, task: Option<JoinHandle<()>>, timeout: Duration) {
    let Some(mut task) = task else { return };
    if tokio::time::timeout(timeout, &mut task).await.is_err() {
        tracing::warn!(task = name, "Desktop task shutdown timed out; aborting");
        task.abort();
        if tokio::time::timeout(timeout, task).await.is_err() {
            tracing::warn!(
                task = name,
                "Aborted desktop task did not join before timeout"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dbus_service::{ObserverCommands, clamp_pause},
        observer::{Mode, StateSink, WatchStateSink},
        sync_health::{SyncFacts, derive_health},
    };
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[derive(Clone)]
    struct TestClock(Arc<AtomicU64>);
    impl Clock for TestClock {
        fn wall_seconds(&self) -> f64 {
            self.0.load(Ordering::Acquire) as f64
        }
        fn monotonic_seconds(&self) -> f64 {
            self.wall_seconds()
        }
    }

    fn snapshot() -> StateSnapshot {
        StateSnapshot {
            mode: Mode::Idle,
            paused: false,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        }
    }

    fn sampler(facts: Arc<Mutex<SyncFacts>>) -> SyncSampler {
        SyncSampler {
            facts,
            clock: Arc::new(TestClock(Arc::new(AtomicU64::new(0)))),
            stale_threshold: 600.0,
            poison_reports: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            link_facts: crate::private_link::LinkFacts::default(),
        }
    }

    #[derive(Clone)]
    struct FakeSink(Arc<Mutex<Vec<ComponentSignal>>>);
    impl ComponentSignalSink for FakeSink {
        async fn emit(&self, signal: ComponentSignal) -> zbus::Result<()> {
            self.0.lock().unwrap().push(signal);
            Ok(())
        }
    }

    // AC: 1, 3 — the shell receiver mirrors published observer state before emitting its payload.
    #[tokio::test]
    async fn published_snapshot_is_mirrored_and_emitted() {
        let initial = snapshot();
        let (mut states, receiver) = WatchStateSink::channel(initial.clone());
        let mirror = Arc::new(Mutex::new(initial.clone()));
        let health_value = derive_health(&SyncFacts::default(), 0.0, 600.0);
        let health = Arc::new(Mutex::new(health_value.clone()));
        let progress = Arc::new(Mutex::new(String::new()));
        let facts = Arc::new(Mutex::new(SyncFacts::default()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_shell_state(
            receiver,
            ShellCells {
                snapshot: Arc::clone(&mirror),
                health,
                progress,
            },
            sampler(facts),
            SignalState::new(&initial, &health_value, ""),
            Some(FakeSink(Arc::clone(&seen))),
            shutdown_rx,
        ));
        let mut next = initial;
        next.mode = Mode::Screencast;
        StateSink::publish(&mut states, next.clone());
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(*mirror.lock().unwrap(), next);
        assert_eq!(
            *seen.lock().unwrap(),
            [ComponentSignal::StatusChanged("recording".into())]
        );
        shutdown.send(true).unwrap();
        task.await.unwrap();
    }

    // AC: 2, 3 — one sampler cycle fans the same syncing state into both shared consumers and emits it.
    #[tokio::test(start_paused = true)]
    async fn sync_sample_fans_out_and_emits() {
        let initial = snapshot();
        let (_states, receiver) = WatchStateSink::channel(initial.clone());
        let mirror = Arc::new(Mutex::new(initial.clone()));
        let unknown = derive_health(&SyncFacts::default(), 0.0, 600.0);
        let health = Arc::new(Mutex::new(unknown.clone()));
        let progress = Arc::new(Mutex::new(String::new()));
        let (command_sender, _) = std::sync::mpsc::channel();
        let (interface, tray_health) = bind_consumers(
            Arc::new(Mutex::new(initial.clone())),
            Arc::clone(&health),
            Arc::clone(&progress),
            Config::default(),
            TestClock(Arc::new(AtomicU64::new(0))),
            CommandSender::new(command_sender),
        );
        assert!(Arc::ptr_eq(&interface.health, &tray_health.cell));
        assert!(Arc::ptr_eq(&interface.progress, &progress));
        let facts = Arc::new(Mutex::new(SyncFacts::default()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        facts.lock().unwrap().in_progress = true;
        facts.lock().unwrap().progress = "2/4".into();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(run_shell_state(
            receiver,
            ShellCells {
                snapshot: mirror,
                health: Arc::clone(&health),
                progress: Arc::clone(&progress),
            },
            sampler(facts),
            SignalState::new(&initial, &unknown, ""),
            Some(FakeSink(Arc::clone(&seen))),
            shutdown_rx,
        ));
        tokio::task::yield_now().await;
        assert_eq!(health.lock().unwrap().dbus, "syncing");
        assert_eq!(&*progress.lock().unwrap(), "2/4");
        assert_eq!(
            *seen.lock().unwrap(),
            [ComponentSignal::SyncProgressChanged("syncing:2/4".into())]
        );
        shutdown.send(true).unwrap();
        task.await.unwrap();
    }

    // AC: 4 — tray and D-Bus pause commands share one wake channel and preserve clamp semantics.
    #[test]
    fn command_adapter_preserves_pause_variants() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let commands = CommandSender::new(sender);
        for seconds in [900, 1800, 3600] {
            commands.pause(Some(seconds));
            assert_eq!(
                receiver.recv_timeout(Duration::from_millis(10)).unwrap(),
                TrayCommand::Pause(seconds)
            );
        }
        commands.pause(clamp_pause(0));
        commands.pause(clamp_pause(-1));
        assert_eq!(receiver.recv().unwrap(), TrayCommand::PauseIndefinite);
        assert_eq!(receiver.recv().unwrap(), TrayCommand::PauseIndefinite);
    }

    // AC: 5 — the required registration policy exhausts three attempts and returns tray-less.
    #[test]
    fn trayless_setup_retries_three_times() {
        let component = DesktopComponent::new(Config::default());
        let mut attempts = 0;
        let mut waits = 0;
        assert!(!component.setup(
            || {
                attempts += 1;
                false
            },
            |_| waits += 1,
        ));
        assert_eq!((attempts, waits), (3, 2));
    }

    // AC: 6 — only the exact name-owning value is stashed and handed to the object-server path.
    #[test]
    fn singleton_stash_preserves_owner_identity_for_both_success_replies() {
        for reply in [
            fdo::RequestNameReply::PrimaryOwner,
            fdo::RequestNameReply::AlreadyOwner,
        ] {
            let cell = Arc::new(Mutex::new(None));
            let requested_on = Arc::new(());
            stash_owned(&cell, Arc::clone(&requested_on), &reply).unwrap();
            let handed_to_shell = stashed(&cell).expect("owner connection is handed to shell");
            let served_on = with_connection(Some(&handed_to_shell), Arc::clone).unwrap();
            assert!(Arc::ptr_eq(&requested_on, &served_on));
        }
    }

    // AC: 6 — non-owner replies never make a connection available to the object server.
    #[test]
    fn singleton_stash_rejects_both_non_owner_replies() {
        for reply in [
            fdo::RequestNameReply::Exists,
            fdo::RequestNameReply::InQueue,
        ] {
            let cell = Arc::new(Mutex::new(None));
            stash_owned(&cell, Arc::new(()), &reply).unwrap();
            assert!(stashed(&cell).is_none());
        }
    }

    // AC: 8 — hung tasks and interface removal are both bounded without a live bus or tray.
    #[tokio::test]
    async fn shutdown_bounds_hung_task_and_removal() {
        let task = tokio::spawn(std::future::pending::<()>());
        let removal_started = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&removal_started);
        let removal = async move {
            seen.store(true, Ordering::Release);
            std::future::pending::<Result<(), String>>().await
        };
        let started = tokio::time::Instant::now();
        finish_shutdown(Some(task), None, None, removal, Duration::from_millis(5)).await;
        assert!(removal_started.load(Ordering::Acquire));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
