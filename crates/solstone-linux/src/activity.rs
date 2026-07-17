// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod dbus;
pub mod wayland_idle;
pub mod x11;

use std::{
    env,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crate::observer::{ActivityProbe, ActivityState};

pub const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendOutcome<T> {
    Available(T),
    Absent,
    Broken(String),
}

impl<T> BackendOutcome<T> {
    pub fn is_bound(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PowerObservation {
    pub power_save: bool,
    pub readable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSaverSpec {
    pub key: &'static str,
    pub bus: &'static str,
    pub path: &'static str,
}

pub const FDO: ScreenSaverSpec = ScreenSaverSpec {
    key: "fdo",
    bus: "org.freedesktop.ScreenSaver",
    path: "/ScreenSaver",
};
pub const GNOME: ScreenSaverSpec = ScreenSaverSpec {
    key: "gnome",
    bus: "org.gnome.ScreenSaver",
    path: "/org/gnome/ScreenSaver",
};
pub const CINNAMON: ScreenSaverSpec = ScreenSaverSpec {
    key: "cinnamon",
    bus: "org.cinnamon.ScreenSaver",
    path: "/org/cinnamon/ScreenSaver",
};
pub const XFCE: ScreenSaverSpec = ScreenSaverSpec {
    key: "xfce",
    bus: "org.xfce.ScreenSaver",
    path: "/org/xfce/ScreenSaver",
};
pub const MATE: ScreenSaverSpec = ScreenSaverSpec {
    key: "mate",
    bus: "org.mate.ScreenSaver",
    path: "/org/mate/ScreenSaver",
};
const GNOME_LOCK_CHAIN: &[ScreenSaverSpec] = &[GNOME, CINNAMON, XFCE, MATE];
const DEFAULT_LOCK_CHAIN: &[ScreenSaverSpec] = &[FDO, GNOME, CINNAMON, XFCE, MATE];

pub fn lock_chain(desktop: &str) -> &'static [ScreenSaverSpec] {
    if desktop_has_token(desktop, "gnome") {
        GNOME_LOCK_CHAIN
    } else {
        DEFAULT_LOCK_CHAIN
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockSignal {
    Lock,
    Unlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleEdge {
    Idled,
    Resumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpmsPower {
    On,
    Standby,
    Suspend,
    Off,
}

pub trait SessionBusOps {
    fn get_active(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<bool>;
    fn mutter_power_mode(&mut self) -> BackendOutcome<i32>;
    fn mutter_idletime_ms(&mut self) -> BackendOutcome<u64>;
}

pub trait SystemBusOps {
    fn subscribe(&mut self) -> Result<(), String>;
    fn locked_hint(&mut self) -> BackendOutcome<bool>;
    fn drain_lock_signals(&mut self) -> Vec<LockSignal>;
}

pub trait WaylandIdleOps {
    fn bind(&mut self) -> BackendOutcome<()>;
    fn drain_edges(&mut self) -> Vec<IdleEdge>;
}

pub trait XActivityOps {
    fn dpms_state(&mut self) -> BackendOutcome<DpmsPower>;
    fn screensaver_idle_ms(&mut self) -> BackendOutcome<u64>;
}

#[derive(Default)]
pub struct CacheState {
    pub lock_signal: SignalCache<bool>,
    pub subscription_attempted: bool,
    pub subscription_warning_logged: bool,
    pub wayland_idle: bool,
    pub warnings: Vec<String>,
    mutter_warned: bool,
    dpms_warned: bool,
}

pub fn resolve(
    desktop: &str,
    session_type: &str,
    session: &mut impl SessionBusOps,
    system: &mut impl SystemBusOps,
    wayland: &mut impl WaylandIdleOps,
    x: &mut impl XActivityOps,
    cache: &mut CacheState,
) -> (ActivityState, BoundBackends) {
    if !cache.subscription_attempted {
        cache.subscription_attempted = true;
        if let Err(error) = system.subscribe() {
            cache.subscription_warning_logged = true;
            cache
                .warnings
                .push(format!("logind lock subscription failed: {error}"));
        }
    }
    for signal in system.drain_lock_signals() {
        cache.lock_signal.warm(matches!(signal, LockSignal::Lock));
    }

    let mut bound = BoundBackends::default();
    let mut chain_answer = None;
    for spec in lock_chain(desktop) {
        match session.get_active(spec) {
            BackendOutcome::Available(value) => {
                set_lock_bound(&mut bound, spec.key);
                chain_answer = Some(value);
                break;
            }
            BackendOutcome::Absent => {}
            BackendOutcome::Broken(error) => cache
                .warnings
                .push(format!("{} lock backend failed: {error}", spec.bus)),
        }
    }
    let logind_allowed = desktop_has_token(desktop, "gnome") || desktop_has_token(desktop, "kde");
    let logind = if logind_allowed {
        if let Some(value) = cache.lock_signal.take_for_poll() {
            BackendOutcome::Available(value)
        } else {
            system.locked_hint()
        }
    } else {
        BackendOutcome::Absent
    };
    bound.logind_lock = logind.is_bound();
    let locked = chain_answer.unwrap_or(matches!(logind, BackendOutcome::Available(true)));

    let power = match session.mutter_power_mode() {
        BackendOutcome::Available(mode) => {
            bound.mutter_power = true;
            mutter_power(mode)
        }
        BackendOutcome::Absent => resolve_x_power(session_type, x, cache, &mut bound),
        BackendOutcome::Broken(error) => {
            warn_once(
                &mut cache.warnings,
                &mut cache.mutter_warned,
                "Mutter",
                error,
            );
            resolve_x_power(session_type, x, cache, &mut bound)
        }
    };

    let user_idle = match wayland.bind() {
        BackendOutcome::Available(()) => {
            bound.wayland_idle = true;
            for edge in wayland.drain_edges() {
                cache.wayland_idle = matches!(edge, IdleEdge::Idled);
            }
            cache.wayland_idle
        }
        BackendOutcome::Absent | BackendOutcome::Broken(_) => match session.mutter_idletime_ms() {
            BackendOutcome::Available(ms) => {
                bound.mutter_idle = true;
                ms >= IDLE_THRESHOLD.as_millis() as u64
            }
            BackendOutcome::Absent | BackendOutcome::Broken(_) => match x.screensaver_idle_ms() {
                BackendOutcome::Available(ms) => {
                    bound.x11_idle = true;
                    ms >= IDLE_THRESHOLD.as_millis() as u64
                }
                BackendOutcome::Absent | BackendOutcome::Broken(_) => false,
            },
        },
    };
    (state(locked, power, user_idle), bound)
}

fn resolve_x_power(
    session_type: &str,
    x: &mut impl XActivityOps,
    cache: &mut CacheState,
    bound: &mut BoundBackends,
) -> PowerObservation {
    if !session_type.eq_ignore_ascii_case("x11") {
        return PowerObservation::default();
    }
    match x.dpms_state() {
        BackendOutcome::Available(value) => {
            bound.dpms_power = true;
            PowerObservation {
                power_save: !matches!(value, DpmsPower::On),
                readable: true,
            }
        }
        BackendOutcome::Absent => PowerObservation::default(),
        BackendOutcome::Broken(error) => {
            warn_once(&mut cache.warnings, &mut cache.dpms_warned, "DPMS", error);
            PowerObservation::default()
        }
    }
}

fn warn_once(warnings: &mut Vec<String>, warned: &mut bool, backend: &str, error: String) {
    warnings.push(format!(
        "{}: {backend} backend failed: {error}",
        if *warned { "DEBUG" } else { "WARNING" }
    ));
    *warned = true;
}

fn set_lock_bound(bound: &mut BoundBackends, key: &str) {
    match key {
        "fdo" => bound.fdo_lock = true,
        "gnome" => bound.gnome_lock = true,
        "cinnamon" => bound.cinnamon_lock = true,
        "xfce" => bound.xfce_lock = true,
        "mate" => bound.mate_lock = true,
        _ => {}
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalCache<T> {
    warmed: Option<T>,
}

impl<T> SignalCache<T> {
    pub fn warm(&mut self, value: T) {
        self.warmed = Some(value);
    }

    pub fn take_for_poll(&mut self) -> Option<T> {
        self.warmed.take()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoundBackends {
    pub fdo_lock: bool,
    pub gnome_lock: bool,
    pub cinnamon_lock: bool,
    pub xfce_lock: bool,
    pub mate_lock: bool,
    pub logind_lock: bool,
    pub mutter_power: bool,
    pub dpms_power: bool,
    pub wayland_idle: bool,
    pub mutter_idle: bool,
    pub x11_idle: bool,
}

impl BoundBackends {
    fn any_lock(self) -> bool {
        self.fdo_lock
            || self.gnome_lock
            || self.cinnamon_lock
            || self.xfce_lock
            || self.mate_lock
            || self.logind_lock
    }

    fn any_power(self) -> bool {
        self.mutter_power || self.dpms_power
    }

    fn any_idle(self) -> bool {
        self.wayland_idle || self.mutter_idle || self.x11_idle
    }
}

pub fn startup_report(bound: &BoundBackends) -> (Vec<String>, bool) {
    let mut lock = Vec::new();
    let mut power = Vec::new();
    let mut idle = Vec::new();
    for (yes, name) in [
        (bound.fdo_lock, "org.freedesktop.ScreenSaver"),
        (bound.gnome_lock, "org.gnome.ScreenSaver"),
        (bound.cinnamon_lock, "org.cinnamon.ScreenSaver"),
        (bound.xfce_lock, "org.xfce.ScreenSaver"),
        (bound.mate_lock, "org.mate.ScreenSaver"),
        (bound.logind_lock, "logind LockedHint"),
    ] {
        if yes {
            lock.push(name);
        }
    }
    for (yes, name) in [
        (bound.mutter_power, "Mutter PowerSaveMode"),
        (bound.dpms_power, "X11 DPMS"),
    ] {
        if yes {
            power.push(name);
        }
    }
    for (yes, name) in [
        (bound.wayland_idle, "ext-idle-notify-v1"),
        (bound.mutter_idle, "Mutter IdleMonitor"),
        (bound.x11_idle, "XScreenSaver"),
    ] {
        if yes {
            idle.push(name);
        }
    }
    let lines = vec![
        format!("Screen lock backends: {}", display_names(&lock)),
        format!("Power save backends: {}", display_names(&power)),
        format!("User idle backends: {}", display_names(&idle)),
    ];
    // Deliberate adjustment from activity.py:249-255: an idle source prevents
    // always-capture mode even when no lock or power source is bound.
    let always_capture = !bound.any_lock() && !bound.any_power() && !bound.any_idle();
    (lines, always_capture)
}

fn display_names(names: &[&str]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

pub fn desktop_has_token(desktop: &str, wanted: &str) -> bool {
    desktop
        .split(':')
        .any(|token| token.trim().eq_ignore_ascii_case(wanted))
}

pub fn choose_lock(
    desktop: &str,
    screensavers: &[BackendOutcome<bool>],
    logind: BackendOutcome<bool>,
) -> bool {
    if let Some(value) = screensavers.iter().find_map(|outcome| match outcome {
        BackendOutcome::Available(value) => Some(*value),
        BackendOutcome::Absent | BackendOutcome::Broken(_) => None,
    }) {
        return value;
    }
    if desktop_has_token(desktop, "gnome") || desktop_has_token(desktop, "kde") {
        matches!(logind, BackendOutcome::Available(true))
    } else {
        false
    }
}

pub fn mutter_power(mode: i32) -> PowerObservation {
    PowerObservation {
        power_save: mode != 0,
        readable: true,
    }
}

pub fn state(lock: bool, power: PowerObservation, user_idle: bool) -> ActivityState {
    ActivityState {
        screen_locked: lock,
        power_save: power.power_save,
        user_idle,
        power_unreadable: !power.readable,
    }
}

pub trait ActivityOps: Send + 'static {
    fn probe_once(&mut self) -> (ActivityState, BoundBackends);
}

pub struct CompositeActivityProbe {
    latest: Arc<Mutex<ActivityState>>,
}

impl CompositeActivityProbe {
    pub fn spawn() -> Self {
        let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
        let latest = Arc::new(Mutex::new(ActivityState::default()));
        let worker_latest = Arc::clone(&latest);
        let _ = thread::Builder::new()
            .name("solstone-activity".into())
            .spawn(move || {
                run_worker(
                    dbus::NativeActivityOps::new(desktop, session_type),
                    worker_latest,
                )
            });
        Self { latest }
    }

    pub fn spawn_with_ops<O: ActivityOps>(ops: O) -> Self {
        let latest = Arc::new(Mutex::new(ActivityState::default()));
        let worker_latest = Arc::clone(&latest);
        let _ = thread::Builder::new()
            .name("solstone-activity".into())
            .spawn(move || run_worker(ops, worker_latest));
        Self { latest }
    }
}

fn run_worker(mut ops: impl ActivityOps, latest: Arc<Mutex<ActivityState>>) {
    let mut reported = false;
    loop {
        let (next, bound) = ops.probe_once();
        *latest.lock().expect("activity state lock") = next;
        if !reported {
            let (lines, always_capture) = startup_report(&bound);
            for line in lines {
                tracing::info!("{line}");
            }
            if always_capture {
                tracing::warn!("No activity backends available — running in always-capture mode");
            }
            reported = true;
        }
        thread::park_timeout(POLL_INTERVAL);
    }
}

impl ActivityProbe for CompositeActivityProbe {
    /// Transport absence and exhaustion are normal results. The trait error arm
    /// is reserved for a future probe-wide failure and is practically unused.
    fn probe(&mut self) -> Result<ActivityState, String> {
        Ok(*self.latest.lock().expect("activity state lock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    struct FakeSession {
        lock: HashMap<&'static str, VecDeque<BackendOutcome<bool>>>,
        calls: Vec<&'static str>,
        constructions: HashMap<&'static str, usize>,
        proxy_valid: HashMap<&'static str, bool>,
        power: BackendOutcome<i32>,
        idle: BackendOutcome<u64>,
    }

    impl Default for FakeSession {
        fn default() -> Self {
            Self {
                lock: HashMap::new(),
                calls: vec![],
                constructions: HashMap::new(),
                proxy_valid: HashMap::new(),
                power: BackendOutcome::Absent,
                idle: BackendOutcome::Absent,
            }
        }
    }

    impl SessionBusOps for FakeSession {
        fn get_active(&mut self, spec: &ScreenSaverSpec) -> BackendOutcome<bool> {
            self.calls.push(spec.key);
            if !self.proxy_valid.get(spec.key).copied().unwrap_or(false) {
                *self.constructions.entry(spec.key).or_default() += 1;
                self.proxy_valid.insert(spec.key, true);
            }
            let result = self
                .lock
                .get_mut(spec.key)
                .and_then(VecDeque::pop_front)
                .unwrap_or(BackendOutcome::Absent);
            if matches!(result, BackendOutcome::Absent | BackendOutcome::Broken(_)) {
                self.proxy_valid.insert(spec.key, false);
            }
            result
        }
        fn mutter_power_mode(&mut self) -> BackendOutcome<i32> {
            self.power.clone()
        }
        fn mutter_idletime_ms(&mut self) -> BackendOutcome<u64> {
            self.idle.clone()
        }
    }

    struct FakeSystem {
        subscribe: Result<(), String>,
        subscribe_calls: usize,
        hint: BackendOutcome<bool>,
        hint_calls: usize,
        signals: Vec<LockSignal>,
    }
    impl Default for FakeSystem {
        fn default() -> Self {
            Self {
                subscribe: Ok(()),
                subscribe_calls: 0,
                hint: BackendOutcome::Absent,
                hint_calls: 0,
                signals: vec![],
            }
        }
    }
    impl SystemBusOps for FakeSystem {
        fn subscribe(&mut self) -> Result<(), String> {
            self.subscribe_calls += 1;
            self.subscribe.clone()
        }
        fn locked_hint(&mut self) -> BackendOutcome<bool> {
            self.hint_calls += 1;
            self.hint.clone()
        }
        fn drain_lock_signals(&mut self) -> Vec<LockSignal> {
            std::mem::take(&mut self.signals)
        }
    }

    struct FakeWayland {
        bound: BackendOutcome<()>,
        edges: Vec<IdleEdge>,
        bind_calls: usize,
    }
    impl Default for FakeWayland {
        fn default() -> Self {
            Self {
                bound: BackendOutcome::Absent,
                edges: vec![],
                bind_calls: 0,
            }
        }
    }
    impl WaylandIdleOps for FakeWayland {
        fn bind(&mut self) -> BackendOutcome<()> {
            self.bind_calls += 1;
            self.bound.clone()
        }
        fn drain_edges(&mut self) -> Vec<IdleEdge> {
            std::mem::take(&mut self.edges)
        }
    }

    struct FakeX {
        dpms: BackendOutcome<DpmsPower>,
        idle: BackendOutcome<u64>,
        dpms_calls: usize,
        idle_calls: usize,
    }
    impl Default for FakeX {
        fn default() -> Self {
            Self {
                dpms: BackendOutcome::Absent,
                idle: BackendOutcome::Absent,
                dpms_calls: 0,
                idle_calls: 0,
            }
        }
    }
    impl XActivityOps for FakeX {
        fn dpms_state(&mut self) -> BackendOutcome<DpmsPower> {
            self.dpms_calls += 1;
            self.dpms.clone()
        }
        fn screensaver_idle_ms(&mut self) -> BackendOutcome<u64> {
            self.idle_calls += 1;
            self.idle.clone()
        }
    }

    fn queue(
        session: &mut FakeSession,
        key: &'static str,
        values: impl IntoIterator<Item = BackendOutcome<bool>>,
    ) {
        session.lock.insert(key, values.into_iter().collect());
    }

    fn run(
        desktop: &str,
        session_type: &str,
        session: &mut FakeSession,
        system: &mut FakeSystem,
        wayland: &mut FakeWayland,
        x: &mut FakeX,
        cache: &mut CacheState,
    ) -> (ActivityState, BoundBackends) {
        resolve(desktop, session_type, session, system, wayland, x, cache)
    }

    #[test]
    fn exact_desktop_tokens_only() {
        // tests/test_activity.py::TestIsScreenLocked::test_xdg_current_desktop_ubuntu_gnome_skips_fdo_and_returns_gnome_state
        assert!(desktop_has_token("ubuntu:GNOME", "gnome"));
        // tests/test_activity.py::TestIsScreenLocked::test_xdg_current_desktop_not_gnome_does_not_match_substring
        assert!(!desktop_has_token("NOT-GNOME", "gnome"));
        assert_eq!(lock_chain("NOT-GNOME")[0], FDO);
    }

    #[test]
    fn lock_chain_calls_are_ordered_and_first_bound_wins() {
        // tests/test_activity.py::TestIsScreenLocked::test_fdo_backend_returns_false_without_gnome_fallback
        let mut session = FakeSession::default();
        queue(&mut session, "fdo", [BackendOutcome::Available(false)]);
        queue(&mut session, "gnome", [BackendOutcome::Available(true)]);
        let state = run(
            "KDE",
            "wayland",
            &mut session,
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut CacheState::default(),
        )
        .0;
        assert!(!state.screen_locked);
        assert_eq!(session.calls, ["fdo"]);
        // tests/test_activity.py::TestIsScreenLocked::test_fdo_failure_gnome_returns_true
        let mut session = FakeSession::default();
        queue(&mut session, "fdo", [BackendOutcome::Absent]);
        queue(&mut session, "gnome", [BackendOutcome::Available(true)]);
        let state = run(
            "KDE",
            "wayland",
            &mut session,
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut CacheState::default(),
        )
        .0;
        assert!(state.screen_locked);
        assert_eq!(session.calls, ["fdo", "gnome"]);

        for (index, spec) in DEFAULT_LOCK_CHAIN.iter().enumerate() {
            let mut session = FakeSession::default();
            for prior in &DEFAULT_LOCK_CHAIN[..index] {
                queue(&mut session, prior.key, [BackendOutcome::Absent]);
            }
            queue(&mut session, spec.key, [BackendOutcome::Available(true)]);
            let state = run(
                "KDE",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut CacheState::default(),
            )
            .0;
            assert!(state.screen_locked);
            assert_eq!(
                session.calls,
                DEFAULT_LOCK_CHAIN[..=index]
                    .iter()
                    .map(|value| value.key)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn every_lock_backend_obeys_the_outcome_taxonomy() {
        // tests/test_activity.py::TestIsScreenLocked::test_both_backends_fail_returns_false
        for (index, spec) in DEFAULT_LOCK_CHAIN.iter().enumerate() {
            for (outcome, expected, warns) in [
                (BackendOutcome::Available(true), true, 0),
                (BackendOutcome::Available(false), false, 0),
                (BackendOutcome::Absent, false, 0),
                (BackendOutcome::Broken("NoReply".into()), false, 1),
            ] {
                let mut session = FakeSession::default();
                for prior in &DEFAULT_LOCK_CHAIN[..index] {
                    queue(&mut session, prior.key, [BackendOutcome::Absent]);
                }
                queue(&mut session, spec.key, [outcome]);
                let mut cache = CacheState::default();
                let state = run(
                    "XFCE",
                    "wayland",
                    &mut session,
                    &mut FakeSystem::default(),
                    &mut FakeWayland::default(),
                    &mut FakeX::default(),
                    &mut cache,
                )
                .0;
                assert_eq!(state.screen_locked, expected);
                assert_eq!(cache.warnings.len(), warns);
                if warns == 0
                    && session.lock.get(spec.key).is_some_and(VecDeque::is_empty)
                    && expected
                {
                    assert_eq!(session.calls.last(), Some(&spec.key));
                }
            }
        }
    }

    #[test]
    fn gnome_skip_and_logind_policy_are_observable_at_seams() {
        // tests/test_activity.py::TestIsScreenLocked::test_xdg_current_desktop_ubuntu_gnome_skips_fdo_and_returns_gnome_state
        let mut session = FakeSession::default();
        queue(&mut session, "gnome", [BackendOutcome::Available(false)]);
        let mut system = FakeSystem {
            hint: BackendOutcome::Available(true),
            ..FakeSystem::default()
        };
        let state = run(
            "ubuntu:GNOME",
            "wayland",
            &mut session,
            &mut system,
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut CacheState::default(),
        )
        .0;
        assert!(!state.screen_locked);
        assert_eq!(session.calls, ["gnome"]);
        assert_eq!(system.hint_calls, 1); // corroboration only while the chain is bound
        // No 1:1 Python ancestor: logind LockedHint is answer-bearing only after chain exhaustion.
        let mut session = FakeSession::default();
        let mut system = FakeSystem {
            hint: BackendOutcome::Available(true),
            ..FakeSystem::default()
        };
        assert!(
            run(
                "KDE",
                "wayland",
                &mut session,
                &mut system,
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut CacheState::default()
            )
            .0
            .screen_locked
        );
        let mut session = FakeSession::default();
        let mut system = FakeSystem {
            hint: BackendOutcome::Available(true),
            ..FakeSystem::default()
        };
        assert!(
            !run(
                "XFCE",
                "wayland",
                &mut session,
                &mut system,
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut CacheState::default()
            )
            .0
            .screen_locked
        );
        assert_eq!(system.hint_calls, 0);
    }

    #[test]
    fn signal_cache_suppresses_exactly_one_property_roundtrip() {
        // No 1:1 Python ancestor: a signal warms exactly one poll.
        let mut session = FakeSession::default();
        let mut system = FakeSystem {
            hint: BackendOutcome::Available(false),
            signals: vec![LockSignal::Lock],
            ..FakeSystem::default()
        };
        let mut cache = CacheState::default();
        assert!(
            run(
                "KDE",
                "wayland",
                &mut session,
                &mut system,
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache
            )
            .0
            .screen_locked
        );
        assert_eq!(system.hint_calls, 0);
        assert!(
            !run(
                "KDE",
                "wayland",
                &mut session,
                &mut system,
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache
            )
            .0
            .screen_locked
        );
        assert_eq!(system.hint_calls, 1);
    }

    #[test]
    fn broken_subscription_warns_once_and_polling_continues() {
        // No 1:1 Python ancestor: logind subscription failure degrades to polling.
        let mut system = FakeSystem {
            subscribe: Err("no system bus".into()),
            hint: BackendOutcome::Available(true),
            ..FakeSystem::default()
        };
        let mut cache = CacheState::default();
        for _ in 0..2 {
            let _ = run(
                "KDE",
                "wayland",
                &mut FakeSession::default(),
                &mut system,
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache,
            );
        }
        assert_eq!(system.subscribe_calls, 1);
        assert_eq!(system.hint_calls, 2);
        assert_eq!(
            cache
                .warnings
                .iter()
                .filter(|value| value.contains("subscription failed"))
                .count(),
            1
        );
    }

    #[test]
    fn absent_is_quiet_broken_warns_and_both_fall_through() {
        // tests/test_activity.py::TestIsScreenLocked::test_is_screen_locked_service_missing_does_not_log
        let mut absent = FakeSession::default();
        queue(&mut absent, "fdo", [BackendOutcome::Absent]);
        queue(&mut absent, "gnome", [BackendOutcome::Available(true)]);
        let mut cache = CacheState::default();
        assert!(
            run(
                "KDE",
                "wayland",
                &mut absent,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache
            )
            .0
            .screen_locked
        );
        assert!(cache.warnings.is_empty());
        // tests/test_activity.py::TestIsScreenLocked::test_is_screen_locked_fdo_parser_error_falls_through_to_gnome
        let mut broken = FakeSession::default();
        queue(
            &mut broken,
            "fdo",
            [BackendOutcome::Broken("NoReply".into())],
        );
        queue(&mut broken, "gnome", [BackendOutcome::Available(true)]);
        let mut cache = CacheState::default();
        assert!(
            run(
                "KDE",
                "wayland",
                &mut broken,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache
            )
            .0
            .screen_locked
        );
        assert_eq!(cache.warnings.len(), 1);
    }

    #[test]
    fn real_chain_drives_proxy_reuse_and_invalidation_counter() {
        // tests/test_activity.py::TestIsScreenLocked::test_is_screen_locked_caches_and_invalidates_same_bus
        let mut session = FakeSession::default();
        queue(
            &mut session,
            "fdo",
            [
                BackendOutcome::Available(false),
                BackendOutcome::Available(false),
                BackendOutcome::Broken("NoReply".into()),
                BackendOutcome::Available(false),
            ],
        );
        let mut cache = CacheState::default();
        for _ in 0..2 {
            let _ = run(
                "KDE",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache,
            );
        }
        assert_eq!(session.constructions["fdo"], 1);
        let _ = run(
            "KDE",
            "wayland",
            &mut session,
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut cache,
        );
        let _ = run(
            "KDE",
            "wayland",
            &mut session,
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut cache,
        );
        assert_eq!(session.constructions["fdo"], 2);
    }

    #[test]
    fn idle_capability_falls_through_and_agrees_on_threshold() {
        // No 1:1 Python ancestor: all idle transports produce the same raw answer.
        let mut wayland = FakeWayland {
            bound: BackendOutcome::Available(()),
            edges: vec![IdleEdge::Idled],
            bind_calls: 0,
        };
        assert!(
            run(
                "GNOME",
                "wayland",
                &mut FakeSession::default(),
                &mut FakeSystem::default(),
                &mut wayland,
                &mut FakeX::default(),
                &mut CacheState::default()
            )
            .0
            .user_idle
        );
        let mut session = FakeSession {
            idle: BackendOutcome::Available(600_000),
            ..FakeSession::default()
        };
        let mut x = FakeX {
            idle: BackendOutcome::Available(600_000),
            ..FakeX::default()
        };
        assert!(
            run(
                "GNOME",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut x,
                &mut CacheState::default()
            )
            .0
            .user_idle
        );
        assert_eq!(x.idle_calls, 0);
        let mut x = FakeX {
            idle: BackendOutcome::Available(600_000),
            ..FakeX::default()
        };
        assert!(
            run(
                "COSMIC",
                "wayland",
                &mut FakeSession::default(),
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut x,
                &mut CacheState::default()
            )
            .0
            .user_idle
        );
        assert_eq!(x.idle_calls, 1);
    }

    #[test]
    fn power_resolution_preserves_raw_truth_and_readability() {
        // tests/test_activity.py::TestIsPowerSaveActive::test_gnome_backend_nonzero_mode_returns_true
        for mode in [1, 2, 3, -1] {
            let mut session = FakeSession {
                power: BackendOutcome::Available(mode),
                ..FakeSession::default()
            };
            let state = run(
                "GNOME",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut CacheState::default(),
            )
            .0;
            assert!(state.power_save);
            assert!(!state.power_unreadable);
        }
        // tests/test_activity.py::TestIsPowerSaveActive::test_gnome_backend_zero_mode_returns_false
        let mut session = FakeSession {
            power: BackendOutcome::Available(0),
            ..FakeSession::default()
        };
        assert!(
            !run(
                "GNOME",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut CacheState::default()
            )
            .0
            .power_save
        );
        for (dpms, expected) in [
            (DpmsPower::On, false),
            (DpmsPower::Standby, true),
            (DpmsPower::Suspend, true),
            (DpmsPower::Off, true),
        ] {
            let mut x = FakeX {
                dpms: BackendOutcome::Available(dpms),
                ..FakeX::default()
            };
            let state = run(
                "KDE",
                "x11",
                &mut FakeSession::default(),
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut x,
                &mut CacheState::default(),
            )
            .0;
            assert_eq!(state.power_save, expected);
            assert!(!state.power_unreadable);
        }
        let state = run(
            "COSMIC",
            "wayland",
            &mut FakeSession::default(),
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut CacheState::default(),
        )
        .0;
        assert!(!state.power_save);
        assert!(state.power_unreadable);

        // tests/test_activity.py::TestIsPowerSaveActive::test_is_power_save_active_repeated_mutter_failures_log_debug_after_first
        let mut session = FakeSession {
            power: BackendOutcome::Broken("NoReply".into()),
            ..FakeSession::default()
        };
        let mut cache = CacheState::default();
        for _ in 0..2 {
            let _ = run(
                "GNOME",
                "wayland",
                &mut session,
                &mut FakeSystem::default(),
                &mut FakeWayland::default(),
                &mut FakeX::default(),
                &mut cache,
            );
        }
        assert!(cache.warnings[0].starts_with("WARNING:"));
        assert!(cache.warnings[1].starts_with("DEBUG:"));
    }

    #[test]
    fn no_idle_source_is_quiet_and_unbound() {
        // No 1:1 Python ancestor: missing idle capabilities degrade without error spam.
        let mut cache = CacheState::default();
        let (_, bound) = run(
            "COSMIC",
            "wayland",
            &mut FakeSession::default(),
            &mut FakeSystem::default(),
            &mut FakeWayland::default(),
            &mut FakeX::default(),
            &mut cache,
        );
        assert!(!bound.wayland_idle && !bound.mutter_idle && !bound.x11_idle);
        assert!(cache.warnings.is_empty());
    }

    #[test]
    fn composite_returns_default_before_worker_then_publishes() {
        // No 1:1 Python ancestor: synchronous probe reads the latest completed worker state.
        struct BlockingOps(std::sync::mpsc::Receiver<()>);
        impl ActivityOps for BlockingOps {
            fn probe_once(&mut self) -> (ActivityState, BoundBackends) {
                let _ = self.0.recv();
                (
                    ActivityState {
                        screen_locked: true,
                        ..ActivityState::default()
                    },
                    BoundBackends {
                        fdo_lock: true,
                        ..BoundBackends::default()
                    },
                )
            }
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut probe = CompositeActivityProbe::spawn_with_ops(BlockingOps(receiver));
        assert_eq!(probe.probe().unwrap(), ActivityState::default());
        sender.send(()).unwrap();
        for _ in 0..100 {
            if probe.probe().unwrap().screen_locked {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("worker did not publish its completed poll");
    }

    #[test]
    fn mutter_nonzero_including_unknown_is_power_save() {
        // tests/test_activity.py::TestIsPowerSaveActive::test_gnome_backend_nonzero_mode_returns_true
        assert!(mutter_power(2).power_save);
        // Deliberate reference parity: Python's PowerSaveMode predicate is mode != 0.
        assert!(mutter_power(-1).power_save);
        // tests/test_activity.py::TestIsPowerSaveActive::test_gnome_backend_zero_mode_returns_false
        assert!(!mutter_power(0).power_save);
    }

    #[test]
    fn startup_warning_includes_idle_availability() {
        // tests/test_activity.py::TestProbeActivityServices::test_no_services_available_logs_warning
        assert!(startup_report(&BoundBackends::default()).1);
        // No 1:1 Python ancestor: idle availability prevents always-capture mode.
        let bound = BoundBackends {
            wayland_idle: true,
            ..BoundBackends::default()
        };
        let (lines, warning) = startup_report(&bound);
        assert!(!warning);
        assert!(lines[2].contains("ext-idle-notify-v1"));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("kscreen") || line.contains("gtk4"))
        );
        let all = BoundBackends {
            fdo_lock: true,
            gnome_lock: true,
            cinnamon_lock: true,
            xfce_lock: true,
            mate_lock: true,
            logind_lock: true,
            mutter_power: true,
            dpms_power: true,
            wayland_idle: true,
            mutter_idle: true,
            x11_idle: true,
        };
        let (lines, warning) = startup_report(&all);
        assert!(!warning);
        for name in [
            "org.freedesktop.ScreenSaver",
            "org.gnome.ScreenSaver",
            "org.cinnamon.ScreenSaver",
            "org.xfce.ScreenSaver",
            "org.mate.ScreenSaver",
            "logind LockedHint",
            "Mutter PowerSaveMode",
            "X11 DPMS",
            "ext-idle-notify-v1",
            "Mutter IdleMonitor",
            "XScreenSaver",
        ] {
            assert!(
                lines.iter().any(|line| line.contains(name)),
                "missing {name}"
            );
        }
    }

    // tests/test_activity.py::TestIsScreenLocked::test_is_screen_locked_caches_weakref_less_bus
    // is intentionally dropped: it is a dbus-fast Cython weak-reference artifact with no Rust analogue.
}
