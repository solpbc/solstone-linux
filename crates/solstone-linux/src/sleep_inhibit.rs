// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Holds off automatic idle sleep while an import is actively being sent to the journal,
//! while keeping screen blanking, screen locking, and owner-requested sleep working cleanly
//! so an active delivery pauses on sleep and resumes immediately on wake.

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
const LOGIN1_USER_SELF_PATH: &str = "/org/freedesktop/login1/user/self";
const LOGIN1_USER_INTERFACE: &str = "org.freedesktop.login1.User";
const LOGIN1_SESSION_INTERFACE: &str = "org.freedesktop.login1.Session";

const GNOME_SESSION_BUS: &str = "org.gnome.SessionManager";
const GNOME_SESSION_PATH: &str = "/org/gnome/SessionManager";
const GNOME_SESSION_INTERFACE: &str = "org.gnome.SessionManager";
/// `GSM_INHIBITOR_FLAG_SUSPEND`: holds off automatic idle suspend without setting
/// `GSM_INHIBITOR_FLAG_IDLE` (8), so screen blanking and screen lock still run on schedule.
const GSM_INHIBITOR_FLAG_SUSPEND: u32 = 4;

const POWER_MGMT_BUS: &str = "org.freedesktop.PowerManagement.Inhibit";
const POWER_MGMT_PATH: &str = "/org/freedesktop/PowerManagement/Inhibit";
const POWER_MGMT_INTERFACE: &str = "org.freedesktop.PowerManagement.Inhibit";

const INHIBIT_TIMEOUT: Duration = Duration::from_secs(2);
const RELEASE_TIMEOUT: Duration = Duration::from_millis(500);
/// Upper bound to wait for an in-flight delivery to unwind before releasing held locks
/// on `PrepareForSleep(true)`. Stays well inside logind's default 5 s `InhibitDelayMaxSec`.
const PAUSE_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockKind {
    /// `org.freedesktop.login1` `sleep` `delay` lock: postpones sleep briefly so an active
    /// delivery can pause cleanly and resume on wake without blocking owner-requested sleep.
    SleepDelay,
    /// `org.gnome.SessionManager` `SUSPEND` (flag 4) inhibitor: holds off GNOME automatic
    /// idle suspend while allowing screen blanking, screen lock, and owner-requested sleep.
    GnomeSessionSuspend,
    /// `org.freedesktop.PowerManagement.Inhibit` inhibitor: holds off KDE and freedesktop
    /// desktop automatic idle suspend while allowing screen lock and owner-requested sleep.
    PowerManagementInhibit,
    /// `org.freedesktop.login1` `idle` `block` lock: fallback for headless (non-graphical)
    /// sessions where no desktop session inhibitor exists and no desktop screen locker is
    /// suppressed by `idle` block.
    IdleBlock,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InhibitError {
    /// Polkit or the desktop service refused this lock kind; another kind may still succeed.
    Refused(String),
    /// The target bus or service is unavailable or timed out.
    Unavailable(String),
}

pub(crate) type ReleaseFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

pub(crate) trait LockGuard: Send + Sync + 'static {
    /// Optional async cleanup (for example calling D-Bus `Uninhibit`) before dropping.
    fn release(self: Box<Self>) -> ReleaseFuture {
        Box::pin(async move {
            drop(self);
        })
    }
}

pub(crate) struct LockToken {
    guard: Option<Box<dyn LockGuard>>,
}

impl LockToken {
    pub(crate) fn new(guard: impl LockGuard + 'static) -> Self {
        Self {
            guard: Some(Box::new(guard)),
        }
    }

    pub(crate) async fn release(mut self) {
        if let Some(guard) = self.guard.take() {
            guard.release().await;
        }
    }
}

pub(crate) type InhibitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LockToken, InhibitError>> + Send + 'a>>;
pub(crate) type GraphicalSessionFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

pub(crate) trait SleepBackend: Send + Sync {
    fn inhibit<'a>(&'a self, kind: LockKind, who: &'a str, why: &'a str) -> InhibitFuture<'a>;
    fn has_graphical_session<'a>(&'a self) -> GraphicalSessionFuture<'a>;
}

struct FdGuard {
    _fd: std::os::fd::OwnedFd,
}

impl LockGuard for FdGuard {}

struct SessionInhibitorState {
    connection: zbus::Connection,
    bus: &'static str,
    path: &'static str,
    interface: &'static str,
    uninhibit_method: &'static str,
    cookie: u32,
}

impl SessionInhibitorState {
    async fn uninhibit(self) {
        let call = async {
            let proxy =
                zbus::Proxy::new(&self.connection, self.bus, self.path, self.interface).await?;
            proxy
                .call::<_, _, ()>(self.uninhibit_method, &(self.cookie,))
                .await
        };
        let _ = tokio::time::timeout(RELEASE_TIMEOUT, call).await;
        // Dropping `self.connection` closes the dedicated socket cleanly.
        drop(self.connection);
    }
}

struct SessionInhibitorGuard {
    state: Option<SessionInhibitorState>,
}

impl SessionInhibitorGuard {
    fn new(
        connection: zbus::Connection,
        bus: &'static str,
        path: &'static str,
        interface: &'static str,
        uninhibit_method: &'static str,
        cookie: u32,
    ) -> Self {
        Self {
            state: Some(SessionInhibitorState {
                connection,
                bus,
                path,
                interface,
                uninhibit_method,
                cookie,
            }),
        }
    }
}

impl LockGuard for SessionInhibitorGuard {
    fn release(mut self: Box<Self>) -> ReleaseFuture {
        let state = self.state.take();
        Box::pin(async move {
            if let Some(state) = state {
                state.uninhibit().await;
            }
        })
    }
}

impl Drop for SessionInhibitorGuard {
    fn drop(&mut self) {
        if let Some(state) = self.state.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            handle.spawn(state.uninhibit());
        }
        // If no Tokio runtime handle is available (for example during shutdown), `state`
        // drops here and closes the dedicated session D-Bus socket immediately, which
        // clears the inhibitor via `NameOwnerChanged`.
    }
}

#[derive(Default)]
struct SystemSleepBackend {
    system_connection: tokio::sync::Mutex<Option<zbus::Connection>>,
}

impl SystemSleepBackend {
    async fn system_connection(&self) -> Result<zbus::Connection, InhibitError> {
        let mut slot = self.system_connection.lock().await;
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

    async fn clear_system_connection(&self) {
        *self.system_connection.lock().await = None;
    }

    async fn inhibit_logind(
        &self,
        what: &str,
        mode: &str,
        who: &str,
        why: &str,
    ) -> Result<LockToken, InhibitError> {
        let connection = self.system_connection().await?;
        let call = async {
            let proxy =
                zbus::Proxy::new(&connection, LOGIN1_BUS, LOGIN1_PATH, LOGIN1_MANAGER).await?;
            proxy
                .call::<_, _, zbus::zvariant::OwnedFd>("Inhibit", &(what, who, why, mode))
                .await
        };
        match tokio::time::timeout(INHIBIT_TIMEOUT, call).await {
            Ok(Ok(fd)) => {
                let fd: std::os::fd::OwnedFd = fd.into();
                let _ = rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC);
                Ok(LockToken::new(FdGuard { _fd: fd }))
            }
            Ok(Err(error)) => {
                let classified = classify_zbus_error(error);
                if matches!(classified, InhibitError::Unavailable(_)) {
                    self.clear_system_connection().await;
                }
                Err(classified)
            }
            Err(_) => {
                self.clear_system_connection().await;
                Err(InhibitError::Unavailable("logind Inhibit timed out".into()))
            }
        }
    }

    async fn inhibit_gnome_session(&self, who: &str, why: &str) -> Result<LockToken, InhibitError> {
        // Each session inhibitor uses a dedicated session-bus connection so any early
        // cancellation, crash, or exit drops the socket and clears the inhibitor via
        // `NameOwnerChanged`.
        let connection = tokio::time::timeout(INHIBIT_TIMEOUT, zbus::Connection::session())
            .await
            .map_err(|_| InhibitError::Unavailable("session bus connect timed out".into()))?
            .map_err(|error| InhibitError::Unavailable(error.to_string()))?;
        let call = async {
            let proxy = zbus::Proxy::new(
                &connection,
                GNOME_SESSION_BUS,
                GNOME_SESSION_PATH,
                GNOME_SESSION_INTERFACE,
            )
            .await?;
            proxy
                .call::<_, _, u32>("Inhibit", &(who, 0u32, why, GSM_INHIBITOR_FLAG_SUSPEND))
                .await
        };
        match tokio::time::timeout(INHIBIT_TIMEOUT, call).await {
            Ok(Ok(cookie)) => Ok(LockToken::new(SessionInhibitorGuard::new(
                connection,
                GNOME_SESSION_BUS,
                GNOME_SESSION_PATH,
                GNOME_SESSION_INTERFACE,
                "Uninhibit",
                cookie,
            ))),
            Ok(Err(error)) => Err(classify_zbus_error(error)),
            Err(_) => Err(InhibitError::Unavailable(
                "org.gnome.SessionManager.Inhibit timed out".into(),
            )),
        }
    }

    async fn inhibit_power_management(
        &self,
        who: &str,
        why: &str,
    ) -> Result<LockToken, InhibitError> {
        let connection = tokio::time::timeout(INHIBIT_TIMEOUT, zbus::Connection::session())
            .await
            .map_err(|_| InhibitError::Unavailable("session bus connect timed out".into()))?
            .map_err(|error| InhibitError::Unavailable(error.to_string()))?;
        let call = async {
            let proxy = zbus::Proxy::new(
                &connection,
                POWER_MGMT_BUS,
                POWER_MGMT_PATH,
                POWER_MGMT_INTERFACE,
            )
            .await?;
            proxy.call::<_, _, u32>("Inhibit", &(who, why)).await
        };
        match tokio::time::timeout(INHIBIT_TIMEOUT, call).await {
            Ok(Ok(cookie)) => Ok(LockToken::new(SessionInhibitorGuard::new(
                connection,
                POWER_MGMT_BUS,
                POWER_MGMT_PATH,
                POWER_MGMT_INTERFACE,
                "UnInhibit",
                cookie,
            ))),
            Ok(Err(error)) => Err(classify_zbus_error(error)),
            Err(_) => Err(InhibitError::Unavailable(
                "org.freedesktop.PowerManagement.Inhibit timed out".into(),
            )),
        }
    }

    async fn logind_has_graphical_session(&self) -> bool {
        let Ok(connection) = self.system_connection().await else {
            return false;
        };
        let query = async {
            let user_proxy = zbus::Proxy::new(
                &connection,
                LOGIN1_BUS,
                LOGIN1_USER_SELF_PATH,
                LOGIN1_USER_INTERFACE,
            )
            .await?;
            let (session_id, session_path): (String, zbus::zvariant::OwnedObjectPath) =
                user_proxy.get_property("Display").await?;
            if session_id.is_empty() || session_path.as_str() == "/" {
                return Ok::<bool, zbus::Error>(false);
            }
            let session_proxy = zbus::Proxy::new(
                &connection,
                LOGIN1_BUS,
                session_path,
                LOGIN1_SESSION_INTERFACE,
            )
            .await?;
            let session_type: String = session_proxy.get_property("Type").await?;
            Ok(matches!(session_type.as_str(), "wayland" | "x11"))
        };
        matches!(
            tokio::time::timeout(INHIBIT_TIMEOUT, query).await,
            Ok(Ok(true))
        )
    }
}

fn env_has_graphical_session() -> bool {
    fn non_empty(var: &str) -> bool {
        std::env::var_os(var).is_some_and(|value| !value.is_empty())
    }
    if non_empty("WAYLAND_DISPLAY") || non_empty("DISPLAY") || non_empty("XDG_CURRENT_DESKTOP") {
        return true;
    }
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| {
        value.eq_ignore_ascii_case("wayland") || value.eq_ignore_ascii_case("x11")
    })
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

impl SleepBackend for SystemSleepBackend {
    fn inhibit<'a>(&'a self, kind: LockKind, who: &'a str, why: &'a str) -> InhibitFuture<'a> {
        Box::pin(async move {
            match kind {
                LockKind::SleepDelay => self.inhibit_logind("sleep", "delay", who, why).await,
                LockKind::GnomeSessionSuspend => self.inhibit_gnome_session(who, why).await,
                LockKind::PowerManagementInhibit => self.inhibit_power_management(who, why).await,
                LockKind::IdleBlock => self.inhibit_logind("idle", "block", who, why).await,
            }
        })
    }

    fn has_graphical_session<'a>(&'a self) -> GraphicalSessionFuture<'a> {
        Box::pin(async move {
            if env_has_graphical_session() {
                return true;
            }
            self.logind_has_graphical_session().await
        })
    }
}

#[derive(Default)]
struct ActiveLocks {
    delay: Option<LockToken>,
    idle: Option<LockToken>,
}

impl ActiveLocks {
    async fn release(mut self) {
        // Drop the logind sleep-delay FD first so sleep can proceed immediately without
        // waiting on any session-bus Uninhibit round-trip.
        drop(self.delay.take());
        if let Some(idle) = self.idle.take() {
            idle.release().await;
        }
    }
}

async fn acquire_locks(backend: &dyn SleepBackend) -> ActiveLocks {
    let who = text!("linux.metadata.name");
    let why = text!("linux.import.sendingtojournal");
    let mut locks = ActiveLocks::default();

    let logind_available = match backend.inhibit(LockKind::SleepDelay, who, why).await {
        Ok(token) => {
            locks.delay = Some(token);
            true
        }
        Err(InhibitError::Refused(error)) => {
            tracing::debug!(%error, "Sleep delay inhibitor refused");
            true
        }
        Err(InhibitError::Unavailable(error)) => {
            tracing::debug!(%error, "Sleep delay inhibitor unavailable");
            false
        }
    };

    // Always try desktop session-bus inhibitors first regardless of env vars (systemd user
    // units on KDE may not inherit WAYLAND_DISPLAY or DISPLAY). These hold off automatic
    // idle suspend without suppressing screen lock or owner-requested sleep.
    match backend
        .inhibit(LockKind::GnomeSessionSuspend, who, why)
        .await
    {
        Ok(token) => {
            locks.idle = Some(token);
            return locks;
        }
        Err(InhibitError::Refused(error) | InhibitError::Unavailable(error)) => {
            tracing::debug!(%error, "GNOME session suspend inhibitor unavailable");
        }
    }

    match backend
        .inhibit(LockKind::PowerManagementInhibit, who, why)
        .await
    {
        Ok(token) => {
            locks.idle = Some(token);
            return locks;
        }
        Err(InhibitError::Refused(error) | InhibitError::Unavailable(error)) => {
            tracing::debug!(%error, "PowerManagement suspend inhibitor unavailable");
        }
    }

    // Fall back to logind `idle` block only on headless (non-graphical) sessions: on a
    // graphical desktop without a session inhibitor, logind `idle` block can stop the
    // screen from locking, and screen locking always takes priority over holding off idle
    // suspend.
    if logind_available && !backend.has_graphical_session().await {
        match backend.inhibit(LockKind::IdleBlock, who, why).await {
            Ok(token) => locks.idle = Some(token),
            Err(InhibitError::Refused(error) | InhibitError::Unavailable(error)) => {
                tracing::debug!(%error, "Idle block inhibitor unavailable");
            }
        }
    }

    locks
}

#[derive(Default)]
struct SharedState {
    sleeping: bool,
    sleep_epoch: u64,
    active_deliveries: usize,
    releasing: usize,
    locks: Option<ActiveLocks>,
}

/// Coordinates desktop idle-suspend inhibition and `org.freedesktop.login1` sleep-delay /
/// `PrepareForSleep` pause-and-resume for active import deliveries.
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
    /// Delivery may proceed while holding this guard; releasing or dropping it clears all locks.
    Held(DeliveryHold),
    /// System is preparing to sleep or asleep; caller should pause and wait for wake.
    Sleeping,
}

struct ReleaseCounterGuard {
    inhibit: SleepInhibit,
}

impl Drop for ReleaseCounterGuard {
    fn drop(&mut self) {
        let idle = {
            let mut state = self
                .inhibit
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.releasing = state.releasing.saturating_sub(1);
            state.active_deliveries == 0 && state.releasing == 0
        };
        if idle {
            self.inhibit.idle_notify.notify_waiters();
        }
    }
}

/// RAII guard for an active import delivery. Calling `release().await` cleanly uninhibits
/// desktop session locks and closes any logind FDs; dropping it also releases every held
/// lock once no deliveries remain active.
pub(crate) struct DeliveryHold {
    inhibit: SleepInhibit,
    released: bool,
}

impl DeliveryHold {
    fn begin_release(&mut self, track_async_release: bool) -> (Option<ActiveLocks>, bool) {
        if self.released {
            return (None, false);
        }
        self.released = true;
        let mut state = self
            .inhibit
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active_deliveries = state.active_deliveries.saturating_sub(1);
        if state.active_deliveries == 0 {
            let locks = state.locks.take();
            if track_async_release && locks.is_some() {
                state.releasing += 1;
                (locks, false)
            } else {
                (locks, state.releasing == 0)
            }
        } else {
            (None, false)
        }
    }

    pub(crate) async fn release(mut self) {
        let (locks, notify_now) = self.begin_release(true);
        if let Some(locks) = locks {
            let _counter = ReleaseCounterGuard {
                inhibit: self.inhibit.clone(),
            };
            locks.release().await;
        } else if notify_now {
            self.inhibit.idle_notify.notify_waiters();
        }
    }
}

impl Drop for DeliveryHold {
    fn drop(&mut self) {
        let (locks, notify_now) = self.begin_release(false);
        drop(locks);
        if notify_now {
            self.inhibit.idle_notify.notify_waiters();
        }
    }
}

impl SleepInhibit {
    pub(crate) fn disabled() -> Self {
        Self::build(None, PAUSE_WAIT)
    }

    pub(crate) fn system() -> Self {
        Self::build(Some(Arc::new(SystemSleepBackend::default())), PAUSE_WAIT)
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
    /// acquiring locks (in which case any acquired locks are released immediately).
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
        // `active_deliveries` and releases any locks.
        let hold = DeliveryHold {
            inhibit: self.clone(),
            released: false,
        };
        if need_locks && let Some(backend) = self.backend.as_ref() {
            let acquired = acquire_locks(backend.as_ref()).await;
            let (interrupted, surplus) = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.sleeping || state.sleep_epoch != start_epoch {
                    (true, Some(acquired))
                } else if state.locks.is_none() {
                    state.locks = Some(acquired);
                    (false, None)
                } else {
                    (false, Some(acquired))
                }
            };
            if let Some(surplus) = surplus {
                surplus.release().await;
            }
            if interrupted {
                hold.release().await;
                return AcquireOutcome::Sleeping;
            }
        }
        AcquireOutcome::Held(hold)
    }

    /// Handles a `PrepareForSleep(going_to_sleep)` transition.
    ///
    /// On `true`, signals active deliveries to pause, waits briefly for them to unwind and
    /// release their holds, then releases any remaining locks so sleep can proceed.
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
                state.active_deliveries == 0 && state.releasing == 0
            };
            self.sleep_tx.send_replace(true);
            if !already_idle {
                let _ = tokio::time::timeout(self.pause_wait, async {
                    loop {
                        let notified = self.idle_notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        {
                            let state = self
                                .state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if state.active_deliveries == 0 && state.releasing == 0 {
                                break;
                            }
                        }
                        notified.await;
                    }
                })
                .await;
            }
            self.release_all_async().await;
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

    /// Releases any currently held inhibitor locks asynchronously.
    pub(crate) async fn release_all_async(&self) {
        let released = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .locks
            .take();
        if let Some(locks) = released {
            locks.release().await;
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
        graphical_session: AtomicBool,
        calls: Mutex<Vec<(LockKind, String, String)>>,
        active: Arc<AtomicUsize>,
        async_releases: Arc<AtomicUsize>,
        active_kinds: Arc<Mutex<Vec<LockKind>>>,
        gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
        entered: Arc<tokio::sync::Notify>,
    }

    struct FakeToken {
        kind: LockKind,
        active: Arc<AtomicUsize>,
        async_releases: Arc<AtomicUsize>,
        active_kinds: Arc<Mutex<Vec<LockKind>>>,
    }

    impl LockGuard for FakeToken {
        fn release(self: Box<Self>) -> ReleaseFuture {
            Box::pin(async move {
                self.async_releases.fetch_add(1, Ordering::AcqRel);
                drop(self);
            })
        }
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
            Self::with_graphical(true, decide)
        }

        pub(crate) fn with_graphical(
            graphical: bool,
            decide: impl Fn(LockKind) -> FakeOutcome + Send + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                decisions: Mutex::new(Box::new(decide)),
                graphical_session: AtomicBool::new(graphical),
                calls: Mutex::new(Vec::new()),
                active: Arc::new(AtomicUsize::new(0)),
                async_releases: Arc::new(AtomicUsize::new(0)),
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

        pub(crate) fn async_release_count(&self) -> usize {
            self.async_releases.load(Ordering::Acquire)
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
                            async_releases: self.async_releases.clone(),
                            active_kinds: self.active_kinds.clone(),
                        }))
                    }
                    FakeOutcome::Refuse(msg) => Err(InhibitError::Refused(msg.into())),
                    FakeOutcome::Unavailable(msg) => Err(InhibitError::Unavailable(msg.into())),
                }
            })
        }

        fn has_graphical_session<'a>(&'a self) -> GraphicalSessionFuture<'a> {
            let graphical = self.graphical_session.load(Ordering::Acquire);
            Box::pin(async move { graphical })
        }
    }

    #[tokio::test]
    async fn acquires_delay_and_gnome_suspend_and_releases_on_async_release_and_drop() {
        let backend = FakeBackend::new(|_| FakeOutcome::Allow);
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));
        assert_eq!(backend.active_count(), 0);

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![LockKind::SleepDelay, LockKind::GnomeSessionSuspend]
        );
        assert_eq!(
            backend.active_kinds(),
            vec![LockKind::SleepDelay, LockKind::GnomeSessionSuspend]
        );
        assert_eq!(backend.active_count(), 2);

        hold.release().await;
        assert_eq!(backend.active_count(), 0);
        assert!(backend.active_kinds().is_empty());
        assert_eq!(backend.async_release_count(), 1);

        let AcquireOutcome::Held(hold2) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(backend.active_count(), 2);
        drop(hold2);
        assert_eq!(backend.active_count(), 0);
        assert!(backend.active_kinds().is_empty());
    }

    #[tokio::test]
    async fn falls_back_to_power_management_on_kde_without_taking_idle_block() {
        let backend = FakeBackend::new(|kind| match kind {
            LockKind::GnomeSessionSuspend => FakeOutcome::Unavailable("ServiceUnknown"),
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
                LockKind::GnomeSessionSuspend,
                LockKind::PowerManagementInhibit,
            ]
        );
        assert_eq!(
            backend.active_kinds(),
            vec![LockKind::SleepDelay, LockKind::PowerManagementInhibit]
        );
        hold.release().await;
        assert_eq!(backend.active_count(), 0);
        assert_eq!(backend.async_release_count(), 1);
    }

    #[tokio::test]
    async fn bare_graphical_session_skips_logind_idle_block_to_protect_screen_lock() {
        let backend = FakeBackend::with_graphical(true, |kind| match kind {
            LockKind::SleepDelay => FakeOutcome::Allow,
            _ => FakeOutcome::Unavailable("ServiceUnknown"),
        });
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![
                LockKind::SleepDelay,
                LockKind::GnomeSessionSuspend,
                LockKind::PowerManagementInhibit,
            ]
        );
        assert_eq!(backend.active_kinds(), vec![LockKind::SleepDelay]);
        hold.release().await;
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn headless_session_falls_back_to_logind_idle_block() {
        let backend = FakeBackend::with_graphical(false, |kind| match kind {
            LockKind::GnomeSessionSuspend | LockKind::PowerManagementInhibit => {
                FakeOutcome::Unavailable("NoSessionBus")
            }
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
                LockKind::GnomeSessionSuspend,
                LockKind::PowerManagementInhibit,
                LockKind::IdleBlock,
            ]
        );
        assert_eq!(
            backend.active_kinds(),
            vec![LockKind::SleepDelay, LockKind::IdleBlock]
        );
        hold.release().await;
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn polkit_refusal_of_sleep_delay_still_takes_desktop_or_headless_idle_hold() {
        let desktop_backend = FakeBackend::new(|kind| match kind {
            LockKind::SleepDelay => FakeOutcome::Refuse("AccessDenied"),
            _ => FakeOutcome::Allow,
        });
        let desktop_sleep =
            SleepInhibit::with_backend(desktop_backend.clone(), Duration::from_secs(1));
        let AcquireOutcome::Held(hold) = desktop_sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(
            desktop_backend.active_kinds(),
            vec![LockKind::GnomeSessionSuspend]
        );
        hold.release().await;
        assert_eq!(desktop_backend.active_count(), 0);

        let headless_backend = FakeBackend::with_graphical(false, |kind| match kind {
            LockKind::SleepDelay => FakeOutcome::Refuse("AccessDenied"),
            LockKind::GnomeSessionSuspend | LockKind::PowerManagementInhibit => {
                FakeOutcome::Unavailable("NoSessionBus")
            }
            LockKind::IdleBlock => FakeOutcome::Allow,
        });
        let headless_sleep =
            SleepInhibit::with_backend(headless_backend.clone(), Duration::from_secs(1));
        let AcquireOutcome::Held(hold) = headless_sleep.acquire().await else {
            panic!("expected Held");
        };
        assert_eq!(headless_backend.active_kinds(), vec![LockKind::IdleBlock]);
        hold.release().await;
        assert_eq!(headless_backend.active_count(), 0);
    }

    #[tokio::test]
    async fn polkit_refusal_of_all_locks_degrades_cleanly_without_blocking_delivery() {
        let backend = FakeBackend::with_graphical(false, |_| FakeOutcome::Refuse("AccessDenied"));
        let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));

        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("delivery must proceed even when every lock is refused");
        };
        assert_eq!(
            backend.called_kinds(),
            vec![
                LockKind::SleepDelay,
                LockKind::GnomeSessionSuspend,
                LockKind::PowerManagementInhibit,
                LockKind::IdleBlock,
            ]
        );
        assert_eq!(backend.active_count(), 0);
        hold.release().await;
    }

    #[tokio::test]
    async fn unavailable_logind_still_tries_desktop_inhibitors_and_skips_idle_block() {
        let desktop_backend = FakeBackend::new(|kind| match kind {
            LockKind::SleepDelay => FakeOutcome::Unavailable("ServiceUnknown"),
            _ => FakeOutcome::Allow,
        });
        let desktop_sleep =
            SleepInhibit::with_backend(desktop_backend.clone(), Duration::from_secs(1));
        let AcquireOutcome::Held(hold) = desktop_sleep.acquire().await else {
            panic!("delivery must proceed when logind is absent");
        };
        assert_eq!(
            desktop_backend.active_kinds(),
            vec![LockKind::GnomeSessionSuspend]
        );
        hold.release().await;

        let headless_backend =
            FakeBackend::with_graphical(false, |_| FakeOutcome::Unavailable("ServiceUnknown"));
        let headless_sleep =
            SleepInhibit::with_backend(headless_backend.clone(), Duration::from_secs(1));
        let AcquireOutcome::Held(hold) = headless_sleep.acquire().await else {
            panic!("delivery must proceed when all backends are absent");
        };
        assert_eq!(
            headless_backend.called_kinds(),
            vec![
                LockKind::SleepDelay,
                LockKind::GnomeSessionSuspend,
                LockKind::PowerManagementInhibit,
            ]
        );
        assert_eq!(headless_backend.active_count(), 0);
        hold.release().await;
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
            loop {
                rx.changed().await.unwrap();
                if *rx.borrow_and_update() {
                    break;
                }
            }
            hold.release().await;
        });

        sleep.on_prepare_for_sleep(true, || {}).await;
        assert!(sleep.is_sleeping());
        assert_eq!(backend.active_count(), 0);
        assert_eq!(backend.async_release_count(), 1);
        delivery.await.unwrap();

        // While sleeping, acquire returns Sleeping without calling any backend.
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
        hold2.release().await;
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

        // Do not release `hold` during PrepareForSleep(true); pause_wait must force-release.
        sleep.on_prepare_for_sleep(true, || {}).await;
        assert_eq!(backend.active_count(), 0);
        assert_eq!(backend.async_release_count(), 1);
        drop(hold);
        assert_eq!(backend.active_count(), 0);
    }

    #[tokio::test]
    async fn prepare_for_sleep_during_in_flight_inhibit_drops_late_locks_immediately() {
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
        assert_eq!(backend.async_release_count(), 1);
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
    pub(crate) async fn real_has_graphical_session() -> bool {
        SystemSleepBackend::default().has_graphical_session().await
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
                    "sleep",
                    text!("linux.metadata.name"),
                    text!("linux.import.sendingtojournal"),
                    "delay",
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

        let graphical = real_has_graphical_session().await;
        let sleep = SleepInhibit::system();
        let AcquireOutcome::Held(hold) = sleep.acquire().await else {
            panic!("expected Held");
        };
        let ours = list_logind_inhibitors_for_pid(pid).await;
        println!("logind inhibitors during hold (pid {pid}, graphical={graphical}): {ours:?}");
        assert!(
            ours.iter().any(|(what, who, _, mode, _, _)| what == "sleep"
                && who == text!("linux.metadata.name")
                && mode == "delay"),
            "expected sleep delay lock for PID {pid}, got {ours:?}"
        );
        assert!(
            !ours
                .iter()
                .any(|(what, _, _, mode, _, _)| mode == "block" && what.contains("sleep")),
            "must never take a logind sleep block lock, got {ours:?}"
        );
        if graphical {
            assert!(
                !ours
                    .iter()
                    .any(|(what, _, _, mode, _, _)| what == "idle" && mode == "block"),
                "must not take logind idle block on a graphical session, got {ours:?}"
            );
        } else {
            assert!(
                ours.iter().any(|(what, who, _, mode, _, _)| what == "idle"
                    && who == text!("linux.metadata.name")
                    && mode == "block"),
                "expected headless idle block lock for PID {pid}, got {ours:?}"
            );
        }

        hold.release().await;
        wait_logind_inhibitors_cleared(pid).await;

        // Also verify synchronous Drop releases every logind lock.
        let AcquireOutcome::Held(hold2) = sleep.acquire().await else {
            panic!("expected Held");
        };
        assert!(!list_logind_inhibitors_for_pid(pid).await.is_empty());
        drop(hold2);
        wait_logind_inhibitors_cleared(pid).await;
    }
}
