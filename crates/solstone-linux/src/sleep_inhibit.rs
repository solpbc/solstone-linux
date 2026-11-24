// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Holds off idle sleep while an import is actively being sent to the journal, and pauses
//! active import delivery cleanly when the system prepares to sleep so it resumes on wake.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use solstone_i18n::text;

use crate::sync::NewEpisode;

const LOGIN1_BUS: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const INHIBIT_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound to wait for an in-flight delivery to unwind before releasing the delay lock
/// on `PrepareForSleep(true)`. Stays well inside logind's default 5 s `InhibitDelayMaxSec`.
const PAUSE_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockKind {
    /// Postpones sleep for up to `InhibitDelayMaxSec` so an active delivery can pause cleanly.
    SleepDelay,
    /// Prevents automatic idle sleep on active desktop seats.
    SleepIdleBlock,
    /// Fallback when polkit refuses `sleep` block (for example over SSH or headless):
    /// prevents `logind` idle suspend (`allow_any=yes`).
    IdleBlock,
}

impl LockKind {
    pub(crate) fn what(self) -> &'static str {
        match self {
            Self::SleepDelay => "sleep",
            Self::SleepIdleBlock => "sleep:idle",
            Self::IdleBlock => "idle",
        }
    }

    pub(crate) fn mode(self) -> &'static str {
        match self {
            Self::SleepDelay => "delay",
            Self::SleepIdleBlock | Self::IdleBlock => "block",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InhibitError {
    /// Polkit or logind refused this lock kind; a weaker lock kind may still succeed.
    Refused(String),
    /// System D-Bus or `org.freedesktop.login1` is unavailable or timed out.
    Unavailable(String),
}

pub(crate) struct LockToken {
    _guard: Box<dyn Send + Sync>,
}

impl LockToken {
    pub(crate) fn new(guard: impl Send + Sync + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

pub(crate) type InhibitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LockToken, InhibitError>> + Send + 'a>>;

pub(crate) trait SleepBackend: Send + Sync {
    fn inhibit<'a>(&'a self, kind: LockKind, who: &'a str, why: &'a str) -> InhibitFuture<'a>;
}

#[derive(Default)]
struct LogindBackend {
    connection: tokio::sync::Mutex<Option<zbus::Connection>>,
}

impl LogindBackend {
    async fn connection(&self) -> Result<zbus::Connection, InhibitError> {
        let mut slot = self.connection.lock().await;
        if let Some(existing) = slot.as_ref() {
            return Ok(existing.clone());
        }
        let connected = tokio::time::timeout(INHIBIT_TIMEOUT, zbus::Connection::system())
            .await
            .map_err(|_| InhibitError::Unavailable("system bus connect timed out".into()))?
            .map_err(|error| InhibitError::Unavailable(error.to_string()))?;
        *slot = Some(connected.clone());
        Ok(connected)
    }

    async fn clear_connection(&self) {
        *self.connection.lock().await = None;
    }
}

fn classify_zbus_error(error: zbus::Error) -> InhibitError {
    match &error {
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.NoReply"
            | "org.freedesktop.DBus.Error.Timeout"
            | "org.freedesktop.DBus.Error.TimedOut" => InhibitError::Unavailable(error.to_string()),
            _ => InhibitError::Refused(error.to_string()),
        },
        _ => InhibitError::Unavailable(error.to_string()),
    }
}

impl SleepBackend for LogindBackend {
    fn inhibit<'a>(&'a self, kind: LockKind, who: &'a str, why: &'a str) -> InhibitFuture<'a> {
        Box::pin(async move {
            let connection = self.connection().await?;
            let call = async {
                let proxy =
                    zbus::Proxy::new(&connection, LOGIN1_BUS, LOGIN1_PATH, LOGIN1_MANAGER).await?;
                proxy
                    .call::<_, _, zbus::zvariant::OwnedFd>(
                        "Inhibit",
                        &(kind.what(), who, why, kind.mode()),
                    )
                    .await
            };
            match tokio::time::timeout(INHIBIT_TIMEOUT, call).await {
                Ok(Ok(fd)) => {
                    let fd: std::os::fd::OwnedFd = fd.into();
                    let _ = rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC);
                    Ok(LockToken::new(fd))
                }
                Ok(Err(error)) => {
                    let classified = classify_zbus_error(error);
                    if matches!(classified, InhibitError::Unavailable(_)) {
                        self.clear_connection().await;
                    }
                    Err(classified)
                }
                Err(_) => {
                    self.clear_connection().await;
                    Err(InhibitError::Unavailable("logind Inhibit timed out".into()))
                }
            }
        })
    }
}

#[derive(Default)]
struct ActiveLocks {
    delay: Option<LockToken>,
    block: Option<LockToken>,
}

async fn acquire_locks(backend: &dyn SleepBackend) -> ActiveLocks {
    let who = text!("linux.metadata.name");
    let why = text!("linux.import.sendingtojournal");
    let mut locks = ActiveLocks::default();
    match backend.inhibit(LockKind::SleepDelay, who, why).await {
        Ok(token) => locks.delay = Some(token),
        Err(InhibitError::Unavailable(error)) => {
            tracing::debug!(%error, "Sleep inhibitor unavailable");
            return locks;
        }
        Err(InhibitError::Refused(error)) => {
            tracing::debug!(%error, "Sleep delay inhibitor refused");
        }
    }
    match backend.inhibit(LockKind::SleepIdleBlock, who, why).await {
        Ok(token) => locks.block = Some(token),
        Err(InhibitError::Refused(error)) => {
            tracing::debug!(%error, "Sleep block inhibitor refused; falling back to idle block");
            match backend.inhibit(LockKind::IdleBlock, who, why).await {
                Ok(token) => locks.block = Some(token),
                Err(InhibitError::Refused(error) | InhibitError::Unavailable(error)) => {
                    tracing::debug!(%error, "Idle block inhibitor unavailable");
                }
            }
        }
        Err(InhibitError::Unavailable(error)) => {
            tracing::debug!(%error, "Sleep block inhibitor unavailable");
        }
    }
    locks
}

#[derive(Default)]
struct SharedState {
    sleeping: bool,
    sleep_epoch: u64,
    active_deliveries: usize,
    locks: Option<ActiveLocks>,
}

/// Coordinates `org.freedesktop.login1` sleep inhibition and `PrepareForSleep` pause/resume
/// for active import deliveries.
#[derive(Clone)]
pub(crate) struct SleepInhibit {
    backend: Option<Arc<dyn SleepBackend>>,
    state: Arc<Mutex<SharedState>>,
    sleep_tx: Arc<tokio::sync::watch::Sender<bool>>,
    idle_notify: Arc<tokio::sync::Notify>,
    pause_wait: Duration,
}

impl Default for SleepInhibit {
    fn default() -> Self {
        Self::disabled()
    }
}

pub(crate) enum AcquireOutcome {
    /// Delivery may proceed while holding this guard; dropping it releases all locks.
    Held(DeliveryHold),
    /// System is preparing to sleep or asleep; caller should pause and wait for wake.
    Sleeping,
}

/// RAII guard for an active import delivery. Dropping it releases every held sleep lock
/// once no deliveries remain active.
pub(crate) struct DeliveryHold {
    inhibit: SleepInhibit,
}

impl Drop for DeliveryHold {
    fn drop(&mut self) {
        let (released, became_idle) = {
            let mut state = self
                .inhibit
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.active_deliveries = state.active_deliveries.saturating_sub(1);
            if state.active_deliveries == 0 {
                (state.locks.take(), true)
            } else {
                (None, false)
            }
        };
        drop(released);
        if became_idle {
            self.inhibit.idle_notify.notify_waiters();
        }
    }
}

impl SleepInhibit {
    pub(crate) fn disabled() -> Self {
        Self::build(None, PAUSE_WAIT)
    }

    pub(crate) fn logind() -> Self {
        Self::build(Some(Arc::new(LogindBackend::default())), PAUSE_WAIT)
    }

    #[cfg(test)]
    pub(crate) fn with_backend(backend: Arc<dyn SleepBackend>, pause_wait: Duration) -> Self {
        Self::build(Some(backend), pause_wait)
    }

    fn build(backend: Option<Arc<dyn SleepBackend>>, pause_wait: Duration) -> Self {
        let (sleep_tx, _sleep_rx) = tokio::sync::watch::channel(false);
        Self {
            backend,
            state: Arc::new(Mutex::new(SharedState::default())),
            sleep_tx: Arc::new(sleep_tx),
            idle_notify: Arc::new(tokio::sync::Notify::new()),
            pause_wait,
        }
    }

    pub(crate) fn is_sleeping(&self) -> bool {
        *self.sleep_tx.borrow()
    }

    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<bool> {
        self.sleep_tx.subscribe()
    }

    /// Registers an active import delivery and acquires inhibitor locks if the system is not
    /// sleeping. Returns `Sleeping` if `PrepareForSleep(true)` is active or arrives while
    /// acquiring locks (in which case any acquired lock FDs are closed immediately).
    pub(crate) async fn acquire(&self) -> AcquireOutcome {
        let (need_locks, start_epoch) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.sleeping {
                return AcquireOutcome::Sleeping;
            }
            state.active_deliveries += 1;
            (
                state.locks.is_none() && self.backend.is_some(),
                state.sleep_epoch,
            )
        };
        // Construct the guard before awaiting so cancellation or panic always unwinds
        // `active_deliveries` and drops any locks.
        let hold = DeliveryHold {
            inhibit: self.clone(),
        };
        if need_locks && let Some(backend) = self.backend.as_ref() {
            let acquired = acquire_locks(backend.as_ref()).await;
            let interrupted = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.sleeping || state.sleep_epoch != start_epoch {
                    true
                } else {
                    if state.locks.is_none() {
                        state.locks = Some(acquired);
                    }
                    false
                }
            };
            if interrupted {
                drop(hold);
                return AcquireOutcome::Sleeping;
            }
        }
        AcquireOutcome::Held(hold)
    }

    /// Handles a `PrepareForSleep(going_to_sleep)` transition.
    ///
    /// On `true`, signals active deliveries to pause, waits briefly for them to unwind and
    /// drop their holds, then releases any remaining lock FDs so sleep can proceed.
    /// On `false`, clears the sleep flag and calls `on_wake` so paused deliveries resume.
    pub(crate) async fn on_prepare_for_sleep(&self, going_to_sleep: bool, on_wake: impl FnOnce()) {
        if going_to_sleep {
            let already_idle = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.sleeping = true;
                state.sleep_epoch = state.sleep_epoch.wrapping_add(1);
                state.active_deliveries == 0
            };
            self.sleep_tx.send_replace(true);
            if !already_idle {
                let _ = tokio::time::timeout(self.pause_wait, async {
                    loop {
                        let notified = self.idle_notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        if self
                            .state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .active_deliveries
                            == 0
                        {
                            break;
                        }
                        notified.await;
                    }
                })
                .await;
            }
            self.release_all();
        } else {
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.sleeping = false;
            }
            self.sleep_tx.send_replace(false);
            on_wake();
        }
    }

    /// Drops any currently held inhibitor locks immediately.
    pub(crate) fn release_all(&self) {
        let released = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .locks
            .take();
        drop(released);
    }

    /// Clears a held sleep state if the `PrepareForSleep` stream stops, so deliveries are
    /// never stranded paused after a system-bus or logind restart.
    pub(crate) fn reset_sleeping(&self) {
        let was_sleeping = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let prev = state.sleeping;
            state.sleeping = false;
            prev
        };
        if was_sleeping {
            self.sleep_tx.send_replace(false);
        }
    }
}

/// Runs for the life of the app. Without `systemd-logind` there is nothing to watch, and
/// import deliveries still run normally.
pub(crate) async fn watch(sleep: SleepInhibit, episode: NewEpisode) {
    if let Err(error) = watch_changes(&sleep, &episode).await {
        tracing::debug!(%error, "Sleep events are not watched");
    }
    sleep.reset_sleeping();
}

async fn watch_changes(sleep: &SleepInhibit, episode: &NewEpisode) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(&connection, LOGIN1_BUS, LOGIN1_PATH, LOGIN1_MANAGER).await?;
    let mut changes = proxy.receive_signal("PrepareForSleep").await?;
    while let Some(message) = changes.next().await {
        let Ok(going_to_sleep) = message.body().deserialize::<bool>() else {
            continue;
        };
        sleep
            .on_prepare_for_sleep(going_to_sleep, || episode.begin())
            .await;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum FakeOutcome {
        Allow,
        Refuse(&'static str),
        Unavailable(&'static str),
    }

    pub(crate) struct FakeBackend {
        decisions: Mutex<Box<dyn Fn(LockKind) -> FakeOutcome + Send>>,
        calls: Mutex<Vec<(LockKind, String, String)>>,
        active: Arc<AtomicUsize>,
        active_kinds: Arc<Mutex<Vec<LockKind>>>,
        gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
        entered: Arc<tokio::sync::Notify>,
    }

    struct FakeToken {
        kind: LockKind,
        active: Arc<AtomicUsize>,
        active_kinds: Arc<Mutex<Vec<LockKind>>>,
    }

    impl Drop for FakeToken {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::AcqRel);
            let mut kinds = self.active_kinds.lock().unwrap();
            if let Some(pos) = kinds.iter().position(|k| *k == self.kind) {
                kinds.remove(pos);
            }
        }
    }

    impl FakeBackend {
        pub(crate) fn new(decide: impl Fn(LockKind) -> FakeOutcome + Send + 'static) -> Arc<Self> {
            Arc::new(Self {
                decisions: Mutex::new(Box::new(decide)),
                calls: Mutex::new(Vec::new()),
                active: Arc::new(AtomicUsize::new(0)),
                active_kinds: Arc::new(Mutex::new(Vec::new())),
                gate: Mutex::new(None),
                entered: Arc::new(tokio::sync::Notify::new()),
            })
        }

        pub(crate) fn block_calls(&self) -> Arc<tokio::sync::Notify> {
            let notify = Arc::new(tokio::sync::Notify::new());
            *self.gate.lock().unwrap() = Some(notify.clone());
            notify
        }

        pub(crate) fn active_count(&self) -> usize {
            self.active.load(Ordering::Acquire)
        }

        pub(crate) fn active_kinds(&self) -> Vec<LockKind> {
            self.active_kinds.lock().unwrap().clone()
        }

        pub(crate) fn called_kinds(&self) -> Vec<LockKind> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|(kind, _, _)| *kind)
                .collect()
        }
    }

    impl SleepBackend for FakeBackend {
        fn inhibit<'a>(&'a self, kind: LockKind, who: &'a str, why: &'a str) -> InhibitFuture<'a> {
            let who = who.to_owned();
            let why = why.to_owned();
            Box::pin(async move {
                self.calls.lock().unwrap().push((kind, who, why));
                let gate = self.gate.lock().unwrap().clone();
                self.entered.notify_waiters();
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                let decision = (self.decisions.lock().unwrap())(kind);
                match decision {
                    FakeOutcome::Allow => {
                        self.active.fetch_add(1, Ordering::AcqRel);
                        self.active_kinds.lock().unwrap().push(kind);
                        Ok(LockToken::new(FakeToken {
                            kind,
                            active: self.active.clone(),
                            active_kinds: self.active_kinds.clone(),
                        }))
                    }
                    FakeOutcome::Refuse(msg) => Err(InhibitError::Refused(msg.into())),
                    FakeOutcome::Unavailable(msg) => Err(InhibitError::Unavailable(msg.into())),
                }
            })
        }
    }

    #[tokio::test]
    async fn acquires_delay_and_sleep_idle_block_and_releases_on_drop() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));
        assert_eq!(backend.active_count(), 0);

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![LockKind::SleepDelay, LockKind::SleepIdleBlock]
        );
        assert_eq!(
            backend.active_kinds(),
            vec![LockKind::SleepDelay, LockKind::SleepIdleBlock]
        );
        assert_eq!(backend.active_count(), 2);

        drop(hold);
        assert_eq!(backend.active_count(), 0);
        assert!(backend.active_kinds().is_empty());
    }

    #[tokio::test]
    async fn polkit_refusal_of_sleep_block_falls_back_to_idle_block() {
        let backend = FakeBackend::new(|kind| match kind {
            LockKind::SleepIdleBlock => FakeOutcome::Refuse("AccessDenied"),
            _ => FakeOutcome::Allow,
        });
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![
                LockKind::SleepDelay,
                LockKind::SleepIdleBlock,
                LockKind::IdleBlock,
            ]
        );
        assert_eq!(
            backend.active_kinds(),
            vec![LockKind::SleepDelay, LockKind::IdleBlock]
        );
        drop(hold);
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn polkit_refusal_of_all_locks_degrades_cleanly_without_blocking_delivery() {
        let backend = FakeBackend::new(|_| FakeOutcome::Refuse("AccessDenied"));
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("delivery must proceed even when polkit refuses every lock");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![
                LockKind::SleepDelay,
                LockKind::SleepIdleBlock,
                LockKind::IdleBlock,
            ]
        );
        assert_eq!(backend.active_count(), 0);
        drop(hold);
    }

    #[tokio::test]
    async fn unavailable_logind_stops_after_first_probe_and_degrades_cleanly() {
        let backend = FakeBackend::new(|_| FakeOutcome::Unavailable("ServiceUnknown"));
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("delivery must proceed when logind is absent");
        };
        assert_eq!(backend.called_kinds(), vec![LockKind::SleepDelay]);
        assert_eq!(backend.active_count(), 0);
        drop(hold);
    }

    #[tokio::test]
    async fn prepare_for_sleep_pauses_active_delivery_releases_holds_and_wakes_on_resume() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(backend.active_count(), 2);

        let mut rx = sleep.subscribe();
        let delivery = tokio::spawn(async move {
            let _hold = hold;
            loop {
                rx.changed().await.unwrap();
                if *rx.borrow_and_update() {
                    break;
                }
            }
        });

        sleep.on_prepare_for_sleep(true, || {}).await;
        assert!(sleep.is_sleeping());
        assert_eq!(backend.active_count(), 0);
        delivery.await.unwrap();

        // While sleeping, acquire returns Sleeping without calling logind.
        let calls_before = backend.called_kinds().len();
        assert!(matches!(sleep.acquire().await, AcquireOutcome::Sleeping));
        assert_eq!(backend.called_kinds().len(), calls_before);

        // Wake clears sleeping and runs the wake callback.
        let woke = Arc::new(AtomicBool::new(false));
        let woke_flag = woke.clone();
        sleep
            .on_prepare_for_sleep(false, move || woke_flag.store(true, Ordering::Release))
            .await;
        assert!(!sleep.is_sleeping());
        assert!(woke.load(Ordering::Acquire));

        // Delivery can re-acquire locks after wake.
        let AcquireOutcome::Held(hold2) = sleep.acquire().await else {
            panic!("expected Held after wake");
        };
        assert_eq!(backend.active_count(), 2);
        drop(hold2);
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn prepare_for_sleep_releases_locks_even_when_delivery_stalls_past_pause_wait() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_millis(50));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(backend.active_count(), 2);

        // Do not drop `hold` during PrepareForSleep(true); pause_wait must force-release.
        sleep.on_prepare_for_sleep(true, || {}).await;
        assert_eq!(backend.active_count(), 0);
        drop(hold);
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn prepare_for_sleep_during_in_flight_inhibit_drops_late_fds_immediately() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let gate = backend.block_calls();
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_millis(20));

        let sleep_clone = sleep.clone();
        let acquirer = tokio::spawn(async move { sleep_clone.acquire().await });
        backend.entered.notified().await;

        // Sleep arrives while the D-Bus Inhibit call is still pending.
        let sleep_clone2 = sleep.clone();
        let sleeper = tokio::spawn(async move {
            sleep_clone2.on_prepare_for_sleep(true, || {}).await;
        });
        // Yield until sleeping flips true, then release the D-Bus gate for both calls.
        let mut rx = sleep.subscribe();
        while !*rx.borrow_and_update() {
            rx.changed().await.unwrap();
        }
        *backend.gate.lock().unwrap() = None;
        gate.notify_waiters();

        assert!(matches!(acquirer.await.unwrap(), AcquireOutcome::Sleeping));
        sleeper.await.unwrap();
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn task_abort_and_reset_sleeping_leave_no_leaks_or_stuck_pause() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(backend.active_count(), 2);
        let task = tokio::spawn(async move {
            let _hold = hold;
            std::future::pending::<()>().await;
        });
        task.abort();
        let _ = task.await;
        assert_eq!(backend.active_count(), 0);

        // Simulate PrepareForSleep(true) followed by signal stream loss.
        sleep.on_prepare_for_sleep(true, || {}).await;
        assert!(sleep.is_sleeping());
        sleep.reset_sleeping();
        assert!(!sleep.is_sleeping());
    }

    #[cfg(feature = "full-tests")]
    static REAL_LOGIND_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[cfg(feature = "full-tests")]
    pub(crate) type LogindInhibitorRow = (String, String, String, String, u32, u32);

    #[cfg(feature = "full-tests")]
    pub(crate) async fn lock_real_logind_test() -> tokio::sync::MutexGuard<'static, ()> {
        REAL_LOGIND_TEST_MUTEX.lock().await
    }

    #[cfg(feature = "full-tests")]
    pub(crate) async fn list_logind_inhibitors_for_pid(pid: u32) -> Vec<LogindInhibitorRow> {
        let connection = zbus::Connection::system()
            .await
            .expect("system bus must be reachable in full-tests");
        let proxy = zbus::Proxy::new(&connection, LOGIN1_BUS, LOGIN1_PATH, LOGIN1_MANAGER)
            .await
            .expect("logind manager proxy");
        let all: Vec<LogindInhibitorRow> = proxy
            .call("ListInhibitors", &())
            .await
            .expect("ListInhibitors");
        all.into_iter()
            .filter(|(_, _, _, _, _, p)| *p == pid)
            .collect()
    }

    #[cfg(feature = "full-tests")]
    pub(crate) async fn wait_logind_inhibitors_cleared(pid: u32) {
        // logind removes inhibitor entries asynchronously on FIFO EPOLLHUP after close(fd).
        let cleared = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if list_logind_inhibitors_for_pid(pid).await.is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if cleared.is_err() {
            let remaining = list_logind_inhibitors_for_pid(pid).await;
            panic!("inhibitor lock leaked for PID {pid}: {remaining:?}");
        }
    }

    #[cfg(feature = "full-tests")]
    #[tokio::test]
    async fn real_logind_inhibit_registers_and_releases_on_drop() {
        let _guard = lock_real_logind_test().await;
        let pid = std::process::id();
        wait_logind_inhibitors_cleared(pid).await;

        let connection = zbus::Connection::system()
            .await
            .expect("system bus must be reachable in full-tests");
        let proxy = zbus::Proxy::new(&connection, LOGIN1_BUS, LOGIN1_PATH, LOGIN1_MANAGER)
            .await
            .expect("logind manager proxy");

        // Verify the low-level FD returned by logind carries FD_CLOEXEC so child processes
        // cannot inherit an active inhibitor lock.
        let raw_fd: zbus::zvariant::OwnedFd = proxy
            .call(
                "Inhibit",
                &(
                    LockKind::SleepDelay.what(),
                    text!("linux.metadata.name"),
                    text!("linux.import.sendingtojournal"),
                    LockKind::SleepDelay.mode(),
                ),
            )
            .await
            .expect("logind sleep delay lock");
        let raw_fd: std::os::fd::OwnedFd = raw_fd.into();
        rustix::io::fcntl_setfd(&raw_fd, rustix::io::FdFlags::CLOEXEC).expect("set CLOEXEC");
        assert!(
            rustix::io::fcntl_getfd(&raw_fd)
                .expect("get FD flags")
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        drop(raw_fd);
        wait_logind_inhibitors_cleared(pid).await;

        let sleep = SleepInhibit::logind();
        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        let ours = list_logind_inhibitors_for_pid(pid).await;
        println!("logind inhibitors during hold (pid {pid}): {ours:?}");
        assert!(
            ours.iter().any(|(what, who, _, mode, _, _)| what == "sleep"
                && who == text!("linux.metadata.name")
                && mode == "delay"),
            "expected sleep delay lock for PID {pid}, got {ours:?}"
        );
        assert!(
            ours.iter().any(|(what, who, _, mode, _, _)| {
                (what == "sleep:idle" || what == "idle")
                    && who == text!("linux.metadata.name")
                    && mode == "block"
            }),
            "expected sleep:idle or idle block lock for PID {pid}, got {ours:?}"
        );

        drop(hold);
        wait_logind_inhibitors_cleared(pid).await;
    }
}
