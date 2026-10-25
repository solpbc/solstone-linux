// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop activity rows. Idle and lock for media stay in `activity` and `observer::mode`.

use std::sync::Mutex;

use crate::activity::{ActivityFacts, BoundBackends};
use crate::activity_prefs::ActivityPrefsSnapshot;
use crate::activity_writer::DesktopActivityRow;
use crate::desktop_session::DesktopKind;
use crate::observer::ActivityState;

pub const ACTIVITY_FAULT: &str = "activity-write-failed";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityStatus {
    pub published: bool,
    pub route: Option<String>,
    pub foreground: String,
    pub title: String,
    pub url: String,
    pub idle: String,
}

impl Default for CapabilityStatus {
    fn default() -> Self {
        Self {
            published: false,
            route: None,
            foreground: "unavailable".into(),
            title: "unavailable".into(),
            url: "unavailable".into(),
            idle: "unavailable".into(),
        }
    }
}

#[derive(Debug)]
pub struct ActivityFault {
    token: Mutex<Option<String>>,
}

impl ActivityFault {
    pub fn new() -> Self {
        Self {
            token: Mutex::new(None),
        }
    }

    pub fn record_write_error(&self) {
        *self.token.lock().unwrap_or_else(|p| p.into_inner()) = Some(ACTIVITY_FAULT.into());
    }

    pub fn token(&self) -> Option<String> {
        self.token.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Default for ActivityFault {
    fn default() -> Self {
        Self::new()
    }
}

/// App identity, and a title only when the caller asked for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundWindow {
    pub app: String,
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    /// X11 window id or GNOME window id. Zero means the window cannot be told apart.
    pub window_id: u64,
    pub window_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForegroundRead {
    Window(ForegroundWindow),
    Unavailable,
}

pub trait ForegroundProvider {
    /// `want_title` false must not read a title. A second call with `want_title`
    /// true is the title read, and it must describe the same window.
    fn read_foreground(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead;

    /// Committed Firefox document address, or nothing. Must not read an editable value.
    fn read_committed_url(&mut self, _pid: u32) -> Option<String> {
        None
    }

    fn set_reading(&mut self, _enabled: bool) {}

    fn confirm_window(&mut self, _facts: &ActivityFacts) -> ForegroundRead {
        ForegroundRead::Unavailable
    }

    fn bind_prefs(&mut self, _prefs: std::sync::Arc<crate::activity_prefs::ActivityPrefs>) {}

    fn set_paused(&mut self, _paused: bool) {}

    fn note_session(&mut self, _route: &str, _live: bool, _epoch: u64) {}
}

pub struct NoForeground;

impl ForegroundProvider for NoForeground {
    fn read_foreground(&mut self, _want_title: bool, _facts: &ActivityFacts) -> ForegroundRead {
        ForegroundRead::Unavailable
    }
}

pub struct PollInput {
    pub now_ms: u64,
    pub prefs: ActivityPrefsSnapshot,
    pub activity: ActivityState,
    pub bound: BoundBackends,
    pub paused: bool,
    pub route: &'static str,
}

#[derive(Clone, Debug)]
enum Span {
    Foreground {
        app: String,
        app_id: Option<String>,
        title: Option<String>,
        pid: Option<u32>,
        url: Option<String>,
        method: String,
        ts: u64,
    },
    Activity {
        state: String,
        method: String,
        ts: u64,
    },
    Availability {
        capability: String,
        status: String,
        method: String,
        ts: u64,
    },
}

#[derive(Clone, Copy)]
enum SlotKind {
    Foreground,
    Presence,
    Url,
}

enum UrlCheck {
    Ready(Option<String>),
    Moved,
}

/// Open activity carried across a segment boundary. The next segment continues
/// these intervals at the boundary instant.
#[derive(Clone, Default)]
pub struct ActivityContinuation {
    foreground: Option<Span>,
    presence: Option<Span>,
    url: Option<Span>,
}

pub struct ActivityRuntime {
    pub prefs: std::sync::Arc<crate::activity_prefs::ActivityPrefs>,
    pub sampler: ActivitySampler,
    pub writer: Option<(std::path::PathBuf, crate::activity_writer::ActivityWriter)>,
    pub fault: std::sync::Arc<ActivityFault>,
    pub capability: std::sync::Arc<Mutex<CapabilityStatus>>,
}

pub struct ActivitySampler {
    generation: u64,
    epoch: u64,
    foreground: Option<Span>,
    presence: Option<Span>,
    url: Option<Span>,
    pending: Vec<DesktopActivityRow>,
    status: CapabilityStatus,
}

impl ActivitySampler {
    pub fn new() -> Self {
        Self {
            generation: 0,
            epoch: 0,
            foreground: None,
            presence: None,
            url: None,
            pending: Vec::new(),
            status: CapabilityStatus::default(),
        }
    }

    pub fn has_open(&self) -> bool {
        self.foreground.is_some() || self.presence.is_some() || self.url.is_some()
    }

    pub fn status(&self) -> CapabilityStatus {
        self.status.clone()
    }

    pub fn take_pending(&mut self) -> Vec<DesktopActivityRow> {
        std::mem::take(&mut self.pending)
    }

    pub fn restore_pending(&mut self, rows: Vec<DesktopActivityRow>) {
        let mut restored = rows;
        restored.append(&mut self.pending);
        self.pending = restored;
    }

    pub fn poll(
        &mut self,
        input: &PollInput,
        provider: &mut dyn ForegroundProvider,
        facts: &ActivityFacts,
        still_ok: &mut dyn FnMut() -> bool,
    ) {
        provider.note_session(
            input.route,
            input.activity.session.is_live(),
            input.activity.session_epoch,
        );
        if !still_ok()
            || input.prefs.generation != self.generation
            || input.activity.session_epoch != self.epoch
        {
            self.seal(
                input
                    .prefs
                    .policy_boundary_ms
                    .unwrap_or(input.now_ms)
                    .min(input.now_ms),
            );
            self.generation = input.prefs.generation;
            self.epoch = input.activity.session_epoch;
        }
        if !still_ok() || facts.session_epoch != input.activity.session_epoch {
            provider.set_reading(false);
            self.close_slot(SlotKind::Foreground, input.now_ms);
            self.close_slot(SlotKind::Url, input.now_ms);
            self.status.published = true;
            self.status.route = None;
            self.status.foreground = "unavailable".into();
            self.status.title = "unavailable".into();
            self.status.url = "unavailable".into();
            self.observe_presence(input);
            return;
        }
        self.status.published = true;
        self.status.url = "unavailable".into();
        self.observe_presence(input);
        self.observe_foreground(input, provider, facts, still_ok);
        self.observe_url(input);
    }

    pub fn seal(&mut self, now_ms: u64) {
        if let Some(span) = self.foreground.take() {
            self.push_span(span, now_ms);
        }
        if let Some(span) = self.presence.take() {
            self.push_span(span, now_ms);
        }
        if let Some(span) = self.url.take() {
            self.push_span(span, now_ms);
        }
    }

    pub fn continuation_for_generation(&self, generation: u64) -> ActivityContinuation {
        if self.generation == generation {
            self.continuation()
        } else {
            ActivityContinuation::default()
        }
    }

    /// Copy of the open spans so a segment boundary can close them and continue
    /// the same intervals in the next segment with no gap.
    pub fn continuation(&self) -> ActivityContinuation {
        ActivityContinuation {
            foreground: self.foreground.clone(),
            presence: self.presence.clone(),
            url: self.url.clone(),
        }
    }

    pub fn resume_continuation(&mut self, continued: ActivityContinuation, at_ms: u64) {
        self.foreground = continued.foreground.map(|span| stamp_span(span, at_ms));
        self.presence = continued.presence.map(|span| stamp_span(span, at_ms));
        self.url = continued.url.map(|span| stamp_span(span, at_ms));
    }

    fn observe_presence(&mut self, input: &PollInput) {
        if !input.prefs.source || input.paused {
            self.close_slot(SlotKind::Presence, input.now_ms);
            if !input.prefs.source {
                self.status.idle = "unavailable".into();
            }
            return;
        }
        let next = presence_span(&input.activity, &input.bound, input.now_ms);
        self.status.idle = match &next {
            Some(Span::Activity { state, .. }) if state == "active" || state == "idle" => {
                "available".into()
            }
            Some(Span::Activity { state, .. }) if state == "locked" => "available".into(),
            _ => "unavailable".into(),
        };
        self.continue_or_replace(SlotKind::Presence, next, input.now_ms);
    }

    fn observe_url(&mut self, input: &PollInput) {
        if !input.prefs.source || !input.prefs.urls || input.paused {
            self.close_slot(SlotKind::Url, input.now_ms);
            return;
        }
        if matches!(self.foreground, Some(Span::Foreground { url: Some(_), .. })) {
            self.close_slot(SlotKind::Url, input.now_ms);
            return;
        }
        let next = Some(Span::Availability {
            capability: "url".into(),
            status: "unavailable".into(),
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Url, next, input.now_ms);
    }

    fn observe_foreground(
        &mut self,
        input: &PollInput,
        provider: &mut dyn ForegroundProvider,
        facts: &ActivityFacts,
        still_ok: &mut dyn FnMut() -> bool,
    ) {
        let suppressed = !input.prefs.source
            || input.paused
            || !input.activity.session.is_live()
            || input.activity.screen_locked;
        if suppressed || !still_ok() {
            provider.set_reading(false);
            self.close_slot(SlotKind::Foreground, input.now_ms);
            if !input.prefs.source {
                self.status.route = None;
                self.status.foreground = "unavailable".into();
                self.status.title = "unavailable".into();
            }
            return;
        }
        provider.set_reading(true);

        let identity = provider.read_foreground(false, facts);
        if !still_ok() {
            provider.set_reading(false);
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }
        let ForegroundRead::Window(window) = identity else {
            self.status.route = None;
            self.status.foreground = "unavailable".into();
            self.status.title = "unavailable".into();
            self.open_availability_foreground(input);
            return;
        };
        if window.app.is_empty() {
            self.status.route = None;
            self.status.foreground = "unavailable".into();
            self.status.title = "unavailable".into();
            self.open_availability_foreground(input);
            return;
        }
        if app_excluded(&input.prefs, &window.app, window.app_id.as_deref()) {
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }

        let mut title = None;
        if input.prefs.titles || !input.prefs.exclude_titles.is_empty() {
            if !still_ok() {
                provider.set_reading(false);
                self.close_slot(SlotKind::Foreground, input.now_ms);
                return;
            }
            match provider.read_foreground(true, facts) {
                ForegroundRead::Window(second)
                    if still_ok()
                        && second.app == window.app
                        && second.app_id == window.app_id
                        && second.pid == window.pid
                        && second.window_id == window.window_id
                        && second.window_token == window.window_token =>
                {
                    title = second.title;
                }
                _ => {
                    self.close_slot(SlotKind::Foreground, input.now_ms);
                    return;
                }
            }
        }
        if title.as_ref().is_some_and(|value| value.len() > 4096) {
            title = None;
        }
        if !input.prefs.exclude_titles.is_empty()
            && (title.is_none() || title_excluded(&input.prefs, title.as_deref().unwrap_or("")))
        {
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }
        let mut stored_title = if input.prefs.titles { title } else { None };
        let stored_url = match self.committed_url(input, provider, facts, &window, still_ok) {
            UrlCheck::Moved => {
                self.close_slot(SlotKind::Foreground, input.now_ms);
                return;
            }
            UrlCheck::Ready(url) => url,
        };

        if !input.prefs.exclude_titles.is_empty() {
            if !still_ok() {
                provider.set_reading(false);
                self.close_slot(SlotKind::Foreground, input.now_ms);
                return;
            }
            match provider.read_foreground(true, facts) {
                ForegroundRead::Window(fresh)
                    if still_ok()
                        && fresh.app == window.app
                        && fresh.app_id == window.app_id
                        && fresh.pid == window.pid
                        && fresh.window_id == window.window_id
                        && fresh.window_token == window.window_token =>
                {
                    let fresh_title = fresh.title.filter(|t| t.len() <= 4096);
                    let Some(ref t) = fresh_title else {
                        self.close_slot(SlotKind::Foreground, input.now_ms);
                        return;
                    };
                    if title_excluded(&input.prefs, t) {
                        self.close_slot(SlotKind::Foreground, input.now_ms);
                        return;
                    }
                    if input.prefs.titles {
                        stored_title = fresh_title;
                    }
                }
                _ => {
                    self.close_slot(SlotKind::Foreground, input.now_ms);
                    return;
                }
            }
        }
        if !still_ok() {
            provider.set_reading(false);
            self.close_slot(SlotKind::Foreground, input.now_ms);
            return;
        }
        if stored_url.is_none() && input.prefs.urls {
            self.status.url = "unavailable".into();
        } else if stored_url.is_some() {
            self.status.url = "available".into();
        }
        let route = if input.route.is_empty() {
            None
        } else {
            Some(input.route.to_string())
        };
        self.status.route = route;
        self.status.foreground = "available".into();
        self.status.title = if stored_title.is_some() {
            "available".into()
        } else {
            "unavailable".into()
        };
        let next = Some(Span::Foreground {
            app: window.app,
            app_id: window.app_id,
            title: stored_title,
            pid: window.pid,
            url: stored_url,
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Foreground, next, input.now_ms);
    }

    fn committed_url(
        &mut self,
        input: &PollInput,
        provider: &mut dyn ForegroundProvider,
        facts: &ActivityFacts,
        window: &ForegroundWindow,
        still_ok: &mut dyn FnMut() -> bool,
    ) -> UrlCheck {
        if !input.prefs.urls || !still_ok() {
            return UrlCheck::Ready(None);
        }
        if !is_firefox(&window.app, window.app_id.as_deref()) {
            return UrlCheck::Ready(None);
        }
        // Two Firefox windows share a pid. Without a window id the address
        // cannot be tied to the window that was already allowed.
        let Some(pid) = window.pid else {
            return UrlCheck::Ready(None);
        };
        if window.window_id == 0 && window.window_token.is_empty() {
            return UrlCheck::Ready(None);
        }
        let Some(raw) = provider.read_committed_url(pid) else {
            return UrlCheck::Ready(None);
        };
        if !still_ok() {
            return UrlCheck::Ready(None);
        }
        let ForegroundRead::Window(again) = provider.confirm_window(facts) else {
            return UrlCheck::Ready(None);
        };
        if again.window_id != window.window_id
            || again.window_token != window.window_token
            || again.pid != Some(pid)
        {
            return UrlCheck::Moved;
        }
        UrlCheck::Ready(crate::activity_writer::sanitize_url(&raw))
    }

    fn open_availability_foreground(&mut self, input: &PollInput) {
        let next = Some(Span::Availability {
            capability: "foreground".into(),
            status: "unavailable".into(),
            method: method_or_unavailable(input.route),
            ts: input.now_ms,
        });
        self.continue_or_replace(SlotKind::Foreground, next, input.now_ms);
    }

    fn slot(&mut self, kind: SlotKind) -> &mut Option<Span> {
        match kind {
            SlotKind::Foreground => &mut self.foreground,
            SlotKind::Presence => &mut self.presence,
            SlotKind::Url => &mut self.url,
        }
    }

    fn continue_or_replace(&mut self, kind: SlotKind, next: Option<Span>, now_ms: u64) {
        let unchanged = matches!(
            (self.slot(kind).as_ref(), next.as_ref()),
            (Some(open), Some(incoming)) if same_span(open, incoming)
        );
        if unchanged {
            return;
        }
        self.close_slot(kind, now_ms);
        *self.slot(kind) = next.map(|span| stamp_span(span, now_ms));
    }

    fn close_slot(&mut self, kind: SlotKind, now_ms: u64) {
        let span = self.slot(kind).take();
        if let Some(span) = span {
            self.push_span(span, now_ms);
        }
    }

    fn push_span(&mut self, span: Span, now_ms: u64) {
        let row = match span {
            Span::Foreground {
                app,
                app_id,
                title,
                url,
                method,
                ts,
                ..
            } => {
                if app.is_empty() || now_ms < ts {
                    return;
                }
                DesktopActivityRow::foreground(ts, now_ms, method, app, app_id, title, url)
            }
            Span::Activity { state, method, ts } => {
                if now_ms < ts {
                    return;
                }
                DesktopActivityRow::activity(ts, now_ms, method, state)
            }
            Span::Availability {
                capability,
                status,
                method,
                ts,
            } => {
                if now_ms < ts {
                    return;
                }
                DesktopActivityRow::availability(ts, now_ms, method, capability, status)
            }
        };
        self.pending.push(row);
    }
}

impl Default for ActivitySampler {
    fn default() -> Self {
        Self::new()
    }
}

fn method_or_unavailable(route: &str) -> String {
    if route.is_empty() {
        "unavailable".into()
    } else {
        route.to_string()
    }
}

fn app_excluded(prefs: &ActivityPrefsSnapshot, app: &str, app_id: Option<&str>) -> bool {
    prefs
        .exclude_apps
        .iter()
        .any(|excluded| excluded == app || app_id == Some(excluded.as_str()))
}

fn is_firefox(app: &str, app_id: Option<&str>) -> bool {
    fn token(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        matches!(
            lower.strip_suffix(".desktop").unwrap_or(&lower),
            "firefox" | "firefox_firefox"
        )
    }
    token(app) || app_id.is_some_and(token)
}

fn title_excluded(prefs: &ActivityPrefsSnapshot, title: &str) -> bool {
    prefs
        .exclude_titles
        .iter()
        .any(|excluded| excluded == title)
}

fn stamp_span(span: Span, now_ms: u64) -> Span {
    match span {
        Span::Foreground {
            app,
            app_id,
            title,
            pid,
            url,
            method,
            ..
        } => Span::Foreground {
            app,
            app_id,
            title,
            pid,
            url,
            method,
            ts: now_ms,
        },
        Span::Activity { state, method, .. } => Span::Activity {
            state,
            method,
            ts: now_ms,
        },
        Span::Availability {
            capability,
            status,
            method,
            ..
        } => Span::Availability {
            capability,
            status,
            method,
            ts: now_ms,
        },
    }
}

fn same_span(open: &Span, next: &Span) -> bool {
    match (open, next) {
        (
            Span::Foreground {
                app,
                app_id,
                title,
                pid,
                url,
                method,
                ..
            },
            Span::Foreground {
                app: app2,
                app_id: id2,
                title: title2,
                pid: pid2,
                url: url2,
                method: method2,
                ..
            },
        ) => {
            app == app2
                && app_id == id2
                && title == title2
                && pid == pid2
                && url == url2
                && method == method2
        }
        (
            Span::Activity { state, method, .. },
            Span::Activity {
                state: state2,
                method: method2,
                ..
            },
        ) => state == state2 && method == method2,
        (
            Span::Availability {
                capability,
                status,
                method,
                ..
            },
            Span::Availability {
                capability: capability2,
                status: status2,
                method: method2,
                ..
            },
        ) => capability == capability2 && status == status2 && method == method2,
        _ => false,
    }
}

fn presence_span(activity: &ActivityState, bound: &BoundBackends, now_ms: u64) -> Option<Span> {
    if activity.screen_locked
        && let Some(method) = bound.lock_signal()
    {
        return Some(Span::Activity {
            state: "locked".into(),
            method: method.to_string(),
            ts: now_ms,
        });
    }
    if let Some(method) = bound.idle_signal() {
        if !activity.idle_observed {
            return Some(Span::Availability {
                capability: "idle".into(),
                status: "unavailable".into(),
                method: method.to_string(),
                ts: now_ms,
            });
        }
        let state = if activity.user_idle { "idle" } else { "active" };
        return Some(Span::Activity {
            state: state.into(),
            method: method.to_string(),
            ts: now_ms,
        });
    }
    Some(Span::Availability {
        capability: "idle".into(),
        status: "unavailable".into(),
        method: "unavailable".into(),
        ts: now_ms,
    })
}

pub fn route_name(kind: Option<DesktopKind>, desktop: &str) -> &'static str {
    match kind {
        Some(DesktopKind::X11) => "x11-ewmh",
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "GNOME") => {
            "gnome-shell"
        }
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "KDE") => "kwin",
        Some(DesktopKind::Wayland) if crate::activity::desktop_has_token(desktop, "sway") => {
            "sway-ipc"
        }
        _ => "",
    }
}

pub fn ewmh_display(kind: Option<DesktopKind>, display: Option<&str>) -> Option<&str> {
    match kind {
        Some(DesktopKind::X11) => display.filter(|value| !value.is_empty()),
        _ => None,
    }
}

/// A title read is refused unless the previous read stored this window.
fn stored_title_token<T: Clone>(
    want_title: bool,
    stored: &Option<T>,
) -> Result<Option<T>, &'static str> {
    if want_title {
        stored.clone().map(Some).ok_or("focus-changed")
    } else {
        Ok(None)
    }
}

fn window_from_snapshot(
    snapshot: crate::activity_helpers::WindowSnapshot,
    want_title: bool,
) -> ForegroundRead {
    ForegroundRead::Window(ForegroundWindow {
        app: snapshot.app,
        app_id: snapshot.app_id,
        title: if want_title { snapshot.title } else { None },
        pid: snapshot.pid,
        window_id: u64::from(snapshot.window_id),
        window_token: snapshot.window_token,
    })
}

/// Compositor-selected foreground and committed document addresses.
pub struct ProductionForeground {
    state_dir: Option<std::path::PathBuf>,
    prefs: Option<std::sync::Arc<crate::activity_prefs::ActivityPrefs>>,
    addresses: Box<dyn crate::activity_address::AddressReader>,
    sway: Box<dyn crate::activity_live::SwayBus>,
    sway_expected: Option<(String, String, Option<u32>)>,
    live: crate::activity_live::LiveDesktop,
    x11_expected: Option<(u32, String)>,
    gnome_expected: Option<(u64, String, u32)>,
    /// KWin `internalId`, class, and pid. The id is what a title read must match.
    kwin_expected: Option<(String, String, u32)>,
}

impl ProductionForeground {
    /// Opens the helper socket and KWin bus name. Tests do not call this.
    pub fn start(state_dir: std::path::PathBuf) -> Self {
        Self {
            state_dir: Some(state_dir),
            prefs: None,
            addresses: Box::new(crate::activity_address::NativeAddressReader::start()),
            sway: Box::new(crate::activity_live::NativeSwayBus),
            sway_expected: None,
            live: crate::activity_live::LiveDesktop::start(),
            x11_expected: None,
            gnome_expected: None,
            kwin_expected: None,
        }
    }

    fn read_x11(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead {
        let Some(display) = ewmh_display(facts.kind, facts.display.as_deref()) else {
            return ForegroundRead::Unavailable;
        };
        let expected = match stored_title_token(want_title, &self.x11_expected) {
            Ok(token) => token,
            Err(_) => return ForegroundRead::Unavailable,
        };
        match crate::activity_helpers::read_live_ewmh(
            display,
            facts.session_epoch,
            want_title,
            0,
            expected,
        ) {
            Ok(snapshot) => {
                self.x11_expected = Some((snapshot.window_id, snapshot.wm_class.clone()));
                window_from_snapshot(snapshot, want_title)
            }
            Err(_) => {
                if want_title {
                    self.x11_expected = None;
                }
                ForegroundRead::Unavailable
            }
        }
    }

    fn read_gnome(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead {
        let expected = match stored_title_token(want_title, &self.gnome_expected) {
            Ok(token) => token,
            Err(_) => return ForegroundRead::Unavailable,
        };
        match self
            .live
            .gnome_snapshot(facts.session_epoch, want_title, expected)
        {
            Ok(snapshot) => {
                let pid = snapshot.pid.unwrap_or(0);
                if snapshot.window_id != 0 || !snapshot.wm_class.is_empty() {
                    self.gnome_expected = Some((
                        u64::from(snapshot.window_id),
                        snapshot.wm_class.clone(),
                        pid,
                    ));
                }
                window_from_snapshot(snapshot, want_title)
            }
            Err(_) => {
                if want_title {
                    self.gnome_expected = None;
                }
                ForegroundRead::Unavailable
            }
        }
    }

    fn read_kwin(&mut self, want_title: bool, _facts: &ActivityFacts) -> ForegroundRead {
        let expected = match stored_title_token(want_title, &self.kwin_expected) {
            Ok(expected) => expected,
            Err(_) => return ForegroundRead::Unavailable,
        };
        match self.live.kwin_snapshot(want_title, expected) {
            Ok(snapshot) if !snapshot.window_token.is_empty() => {
                self.kwin_expected = Some((
                    snapshot.window_token.clone(),
                    snapshot.wm_class.clone(),
                    snapshot.pid.unwrap_or(0),
                ));
                window_from_snapshot(snapshot, want_title)
            }
            _ => {
                self.kwin_expected = None;
                ForegroundRead::Unavailable
            }
        }
    }

    fn read_sway(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead {
        let Some(path) = facts.sway_socket.as_deref() else {
            return ForegroundRead::Unavailable;
        };
        let read = self.sway.tree(path).and_then(|json| {
            crate::activity_helpers::parse_sway_tree_response(
                &json,
                facts.session_epoch,
                crate::activity_helpers::current_unix_ms(),
            )
        });
        match read {
            Ok(snapshot) => {
                let token = (
                    snapshot.window_token.clone(),
                    snapshot.app.clone(),
                    snapshot.pid,
                );
                if want_title && self.sway_expected.as_ref() != Some(&token) {
                    self.sway_expected = None;
                    return ForegroundRead::Unavailable;
                }
                self.sway_expected = Some(token);
                window_from_snapshot(snapshot, want_title)
            }
            Err(_) => {
                self.sway_expected = None;
                ForegroundRead::Unavailable
            }
        }
    }
}

impl ForegroundProvider for ProductionForeground {
    fn read_foreground(&mut self, want_title: bool, facts: &ActivityFacts) -> ForegroundRead {
        match route_name(facts.kind, &facts.desktop_token) {
            "x11-ewmh" => self.read_x11(want_title, facts),
            "gnome-shell" => self.read_gnome(want_title, facts),
            "kwin" => self.read_kwin(want_title, facts),
            "sway-ipc" => self.read_sway(want_title, facts),
            _ => ForegroundRead::Unavailable,
        }
    }

    fn read_committed_url(&mut self, pid: u32) -> Option<String> {
        let prefs = self.prefs.as_ref()?.clone();
        let policy = prefs.snapshot();
        if !policy.source || !policy.urls {
            return None;
        }
        self.addresses.read(pid, prefs)
    }

    fn confirm_window(&mut self, facts: &ActivityFacts) -> ForegroundRead {
        let x11 = self.x11_expected.clone();
        let gnome = self.gnome_expected.clone();
        let kwin = self.kwin_expected.clone();
        let sway = self.sway_expected.clone();
        let read = self.read_foreground(false, facts);
        self.x11_expected = x11;
        self.gnome_expected = gnome;
        self.kwin_expected = kwin;
        self.sway_expected = sway;
        read
    }

    fn set_reading(&mut self, enabled: bool) {
        self.live.set_enabled(enabled);
        if !enabled {
            self.x11_expected = None;
            self.gnome_expected = None;
            self.kwin_expected = None;
            self.sway_expected = None;
        }
    }

    fn bind_prefs(&mut self, prefs: std::sync::Arc<crate::activity_prefs::ActivityPrefs>) {
        if let Some(state_dir) = self.state_dir.take() {
            crate::activity_address::follow_permission(prefs.clone(), state_dir);
        }
        self.prefs = Some(prefs.clone());
        self.live.bind_prefs(prefs);
    }

    fn set_paused(&mut self, paused: bool) {
        self.live.set_paused(paused);
        if paused {
            self.x11_expected = None;
            self.gnome_expected = None;
            self.kwin_expected = None;
            self.sway_expected = None;
        }
    }

    fn note_session(&mut self, route: &str, live: bool, epoch: u64) {
        self.live.note_route(route, live, epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;

    use crate::desktop_session::SessionPresence;

    #[test]
    fn firefox_identity_includes_the_native_snap_class() {
        assert!(is_firefox("firefox_firefox", None));
        assert!(is_firefox("firefox_firefox.desktop", None));
        assert!(is_firefox("Firefox", None));
        assert!(!is_firefox("not-firefox", None));
        assert!(!is_firefox("firefox-helper", None));
    }

    #[test]
    fn settings_to_production_adapter_and_sanitized_rows() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        struct Helper(Arc<AtomicBool>);
        impl crate::activity_live::HelperBus for Helper {
            fn exchange(&mut self, request: &str) -> Result<String, &'static str> {
                let request: crate::activity_helpers::GnomeSnapshotRequest =
                    serde_json::from_str(request).unwrap();
                Ok(
                    serde_json::to_string(&crate::activity_helpers::GnomeSnapshotReply {
                        v: 1,
                        generation: request.generation,
                        captured_unix_ms: crate::activity_helpers::current_unix_ms(),
                        app: "firefox".into(),
                        app_id: Some("firefox".into()),
                        title: request.want_title.then(|| {
                            if self.0.load(Ordering::SeqCst) {
                                "blocked".into()
                            } else {
                                "page".into()
                            }
                        }),
                        wm_class: Some("firefox".into()),
                        pid: Some(9),
                        window_id: Some(7),
                    })
                    .unwrap(),
                )
            }
        }
        struct Addresses {
            reads: Arc<AtomicUsize>,
            changed: Arc<AtomicBool>,
            mutate: bool,
        }
        impl crate::activity_address::AddressReader for Addresses {
            fn read(
                &mut self,
                pid: u32,
                prefs: Arc<crate::activity_prefs::ActivityPrefs>,
            ) -> Option<String> {
                assert_eq!(pid, 9);
                assert!(prefs.snapshot().source && prefs.snapshot().urls);
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.changed.store(self.mutate, Ordering::SeqCst);
                Some("https://user:secret@example.com/page?unsubmitted=never#fragment".into())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::config::ConfigPaths {
            base_dir: Some(temp.path().join("data")),
            config_dir: Some(temp.path().join("config")),
        };
        std::fs::create_dir_all(paths.config_dir.as_ref().unwrap()).unwrap();
        std::fs::write(paths.config_dir.as_ref().unwrap().join("config.json"),
            r#"{"activity_source":true,"activity_titles":true,"activity_urls":true,"activity_exclude_apps":["other"],"activity_exclude_titles":["other"]}"#).unwrap();
        let legacy = crate::config::load_config(paths.clone()).config;
        assert!(!legacy.activity_urls);
        assert!(legacy.activity_source && legacy.activity_titles);
        assert_eq!(legacy.activity_exclude_apps, ["other"]);
        for (source, urls, app_excluded, title_excluded, mutate, expected_reads, expected_url) in [
            (true, true, false, false, false, 1, true),
            (true, false, false, false, false, 0, false),
            (false, true, false, false, false, 0, false),
            (true, true, true, false, false, 0, false),
            (true, true, false, true, false, 0, false),
            (true, true, false, true, true, 1, false),
        ] {
            let policy = serde_json::json!({"activity_source":source,"activity_titles":true,
                "activity_urls":urls,"activity_exclude_apps":if app_excluded {vec!["firefox"]} else {vec![]},
                "activity_exclude_titles":if title_excluded {vec!["blocked"]} else {vec![]}});
            let config =
                crate::config::set_guided_field(&paths, "activity_policy", policy).unwrap();
            assert_eq!(
                crate::config::load_config(paths.clone())
                    .config
                    .activity_urls,
                urls
            );
            let prefs = Arc::new(crate::activity_prefs::ActivityPrefs::new(&config));
            let reads = Arc::new(AtomicUsize::new(0));
            let changed = Arc::new(AtomicBool::new(title_excluded && !mutate));
            let mut provider = ProductionForeground {
                state_dir: None,
                prefs: None,
                addresses: Box::new(Addresses {
                    reads: reads.clone(),
                    changed: changed.clone(),
                    mutate,
                }),
                sway: Box::new(crate::activity_live::NativeSwayBus),
                sway_expected: None,
                live: crate::activity_live::LiveDesktop::with_helper(Box::new(Helper(changed))),
                x11_expected: None,
                gnome_expected: None,
                kwin_expected: None,
            };
            provider.bind_prefs(prefs.clone());
            let mut sampler = ActivitySampler::new();
            let facts = ActivityFacts {
                kind: Some(DesktopKind::Wayland),
                desktop_token: "GNOME".into(),
                ..ActivityFacts::default()
            };
            sampler.poll(
                &PollInput {
                    now_ms: 1000,
                    prefs: prefs.snapshot(),
                    activity: live_state(),
                    bound: BoundBackends::default(),
                    paused: false,
                    route: "gnome-shell",
                },
                &mut provider,
                &facts,
                &mut || true,
            );
            sampler.seal(2000);
            let rows = sampler.take_pending();
            assert_eq!(reads.load(Ordering::SeqCst), expected_reads);
            let blob = serde_json::to_string(&rows).unwrap();
            assert_eq!(blob.contains("https://example.com/page"), expected_url);
            assert!(
                !blob.contains("secret")
                    && !blob.contains("unsubmitted")
                    && !blob.contains("fragment")
            );
            if app_excluded || title_excluded || !source {
                assert!(foreground_apps(&rows).is_empty());
            }
        }
    }

    fn prefs(
        source: bool,
        titles: bool,
        urls: bool,
        apps: &[&str],
        title_exclusions: &[&str],
    ) -> ActivityPrefsSnapshot {
        ActivityPrefsSnapshot {
            source,
            titles,
            urls,
            exclude_apps: apps.iter().map(|s| (*s).to_string()).collect(),
            exclude_titles: title_exclusions.iter().map(|s| (*s).to_string()).collect(),
            through_pause: false,
            generation: 1,
            policy_boundary_ms: None,
        }
    }

    fn live_state() -> ActivityState {
        ActivityState {
            session: SessionPresence::Live,
            ..ActivityState::default()
        }
    }

    #[test]
    fn revocation_during_url_or_confirmation_discards_the_whole_sample() {
        use std::rc::Rc;
        struct Revoking {
            allowed: Rc<Cell<bool>>,
            confirm: bool,
            value: Option<String>,
        }
        impl ForegroundProvider for Revoking {
            fn read_foreground(&mut self, want_title: bool, _: &ActivityFacts) -> ForegroundRead {
                ForegroundRead::Window(ForegroundWindow {
                    app: "firefox".into(),
                    app_id: Some("firefox".into()),
                    title: want_title.then(|| "PRIVATE_TITLE".into()),
                    pid: Some(9),
                    window_id: 7,
                    window_token: String::new(),
                })
            }
            fn read_committed_url(&mut self, _: u32) -> Option<String> {
                if !self.confirm {
                    self.allowed.set(false);
                }
                self.value.clone()
            }
            fn confirm_window(&mut self, facts: &ActivityFacts) -> ForegroundRead {
                self.allowed.set(false);
                self.read_foreground(false, facts)
            }
        }
        for (confirm, value) in [
            (false, None),
            (false, Some("https://private.example/".into())),
            (true, Some("https://private.example/".into())),
        ] {
            let allowed = Rc::new(Cell::new(true));
            let mut provider = Revoking {
                allowed: allowed.clone(),
                confirm,
                value,
            };
            let mut sampler = ActivitySampler::new();
            sampler.poll(
                &PollInput {
                    now_ms: 1000,
                    prefs: prefs(true, true, true, &[], &[]),
                    activity: live_state(),
                    bound: BoundBackends::default(),
                    paused: false,
                    route: "x11-ewmh",
                },
                &mut provider,
                &ActivityFacts::default(),
                &mut || allowed.get(),
            );
            sampler.seal(2000);
            let rows = sampler.take_pending();
            assert!(foreground_apps(&rows).is_empty());
            assert!(
                !serde_json::to_string(&rows)
                    .unwrap()
                    .contains("PRIVATE_TITLE")
            );
        }
    }

    struct Scripted {
        identity: VecDeque<ForegroundRead>,
        titled: VecDeque<ForegroundRead>,
        title_reads: Cell<usize>,
        confirm: VecDeque<ForegroundRead>,
        url: Option<String>,
        url_reads: Cell<usize>,
    }

    impl Scripted {
        fn window(app: &str, title: Option<&str>) -> ForegroundRead {
            ForegroundRead::Window(ForegroundWindow {
                app: app.into(),
                app_id: None,
                title: title.map(str::to_string),
                pid: None,
                window_id: 0,
                window_token: String::new(),
            })
        }
    }

    impl ForegroundProvider for Scripted {
        fn read_foreground(&mut self, want_title: bool, _facts: &ActivityFacts) -> ForegroundRead {
            if want_title {
                self.title_reads.set(self.title_reads.get() + 1);
                self.titled
                    .pop_front()
                    .unwrap_or(ForegroundRead::Unavailable)
            } else {
                self.identity
                    .pop_front()
                    .unwrap_or(ForegroundRead::Unavailable)
            }
        }

        fn read_committed_url(&mut self, _pid: u32) -> Option<String> {
            self.url_reads.set(self.url_reads.get() + 1);
            self.url.clone()
        }

        /// Pops the confirm queue only. A mistaken identity re-read would consume `identity`.
        fn confirm_window(&mut self, _facts: &ActivityFacts) -> ForegroundRead {
            self.confirm
                .pop_front()
                .unwrap_or(ForegroundRead::Unavailable)
        }
    }

    fn poll_at(
        sampler: &mut ActivitySampler,
        now_ms: u64,
        prefs: ActivityPrefsSnapshot,
        activity: ActivityState,
        bound: BoundBackends,
        provider: &mut Scripted,
    ) {
        sampler.poll(
            &PollInput {
                now_ms,
                prefs,
                activity,
                bound,
                paused: false,
                route: "x11-ewmh",
            },
            provider,
            &ActivityFacts::default(),
            &mut || true,
        );
    }

    fn foreground_apps(rows: &[DesktopActivityRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match &row.payload {
                crate::activity_writer::DesktopActivityPayload::Foreground { app, .. } => {
                    Some(app.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn source_off_does_not_read_or_write() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("A", None)]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(false, true, true, &[], &[]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        assert!(sampler.take_pending().is_empty());
        assert_eq!(provider.title_reads.get(), 0);
        assert_eq!(provider.identity.len(), 1);
    }

    #[test]
    fn excluded_app_skips_title_getter_and_is_not_stored() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("SENTINEL_APP", None)]),
            titled: VecDeque::from([Scripted::window("SENTINEL_APP", Some("SENTINEL_TITLE"))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &["SENTINEL_APP"], &[]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        assert_eq!(provider.title_reads.get(), 0);
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("SENTINEL_APP"));
        assert!(!blob.contains("SENTINEL_TITLE"));
    }

    #[test]
    fn excluded_title_is_not_stored() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("Notes", None)]),
            titled: VecDeque::from([Scripted::window("Notes", Some("SENTINEL_TITLE"))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &[], &["SENTINEL_TITLE"]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert_eq!(provider.title_reads.get(), 1);
        assert_eq!(provider.url_reads.get(), 0);
        assert!(!blob.contains("SENTINEL_TITLE"));
        assert!(!foreground_apps(&rows).iter().any(|app| app == "Notes"));
        assert!(sampler.status().route.is_none());
    }

    #[test]
    fn overlong_title_with_exclusions_is_not_stored() {
        let long = "T".repeat(4097);
        let window = |title: Option<&str>| {
            ForegroundRead::Window(ForegroundWindow {
                app: "Notes".into(),
                app_id: None,
                title: title.map(str::to_string),
                pid: Some(4),
                window_id: 9,
                window_token: String::new(),
            })
        };
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([window(None)]),
            titled: VecDeque::from([window(Some(&long))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: Some("https://user:secret@sentinel.example/path?q=1#frag".into()),
            url_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, true, &[], &["Other"]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let blob = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert_eq!(provider.title_reads.get(), 1);
        assert_eq!(provider.url_reads.get(), 0);
        assert!(!blob.contains("Notes"));
        assert!(!blob.contains(&long));
        assert!(!blob.contains("sentinel.example"));
        assert!(!blob.contains("secret"));
    }

    #[test]
    fn foreground_gap_is_two_intervals() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([
                Scripted::window("A", None),
                Scripted::window("SENTINEL_APP", None),
                Scripted::window("A", None),
            ]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let included = prefs(true, false, false, &["SENTINEL_APP"], &[]);
        poll_at(
            &mut sampler,
            1_000,
            included.clone(),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        poll_at(
            &mut sampler,
            2_000,
            included.clone(),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        poll_at(
            &mut sampler,
            3_000,
            included,
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(4_000);
        let rows = sampler.take_pending();
        let apps = foreground_apps(&rows);
        assert_eq!(apps, vec!["A".to_string(), "A".to_string()]);
        let foregrounds: Vec<_> = rows
            .iter()
            .filter(|row| {
                matches!(
                    row.payload,
                    crate::activity_writer::DesktopActivityPayload::Foreground { .. }
                )
            })
            .collect();
        assert_eq!(foregrounds[0].ts, 1_000);
        assert_eq!(foregrounds[0].end_ts, 2_000);
        assert_eq!(foregrounds[1].ts, 3_000);
        assert_eq!(foregrounds[1].end_ts, 4_000);
        assert!(foregrounds[0].end_ts <= foregrounds[1].ts);
    }

    #[test]
    fn absent_idle_is_availability_not_active() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([ForegroundRead::Unavailable]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let mut activity = live_state();
        activity.user_idle = false;
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, false, &[], &[]),
            activity,
            BoundBackends::default(),
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(blob.contains("\"capability\":\"idle\""));
        assert!(blob.contains("\"status\":\"unavailable\""));
        assert!(!blob.contains("\"state\":\"active\""));
    }

    #[test]
    fn measured_not_idle_is_active() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([ForegroundRead::Unavailable]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let bound = BoundBackends {
            x11_idle: true,
            ..BoundBackends::default()
        };
        let mut activity = live_state();
        activity.idle_observed = true;
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, false, &[], &[]),
            activity,
            bound,
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(blob.contains("\"state\":\"active\""));
        assert!(blob.contains("XScreenSaver"));
    }

    #[test]
    fn lock_suppresses_foreground() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("Secret", Some("hidden"))]),
            titled: VecDeque::from([Scripted::window("Secret", Some("hidden"))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let mut activity = live_state();
        activity.screen_locked = true;
        let bound = BoundBackends {
            gnome_lock: true,
            ..BoundBackends::default()
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, true, false, &[], &[]),
            activity,
            bound,
            &mut provider,
        );
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        assert_eq!(provider.title_reads.get(), 0);
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("Secret"));
        assert!(blob.contains("\"state\":\"locked\""));
    }

    #[test]
    fn wayland_display_is_not_an_ewmh_source() {
        assert_eq!(ewmh_display(Some(DesktopKind::Wayland), Some(":0")), None);
        assert_eq!(ewmh_display(Some(DesktopKind::X11), Some(":0")), Some(":0"));
        assert_eq!(ewmh_display(None, Some(":0")), None);
    }

    #[test]
    fn title_token_is_required_before_a_title_read() {
        let stored = Some((1u32, "Allowed".to_string()));
        assert!(stored_title_token(true, &None::<(u32, String)>).is_err());
        assert_eq!(stored_title_token(false, &stored).unwrap(), None);
        assert_eq!(
            stored_title_token(true, &stored).unwrap(),
            Some((1, "Allowed".into()))
        );
    }

    #[test]
    fn mismatched_epochs_do_not_read_foreground() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("A", Some("SECRET"))]),
            titled: VecDeque::from([Scripted::window("A", Some("SECRET"))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let mut activity = live_state();
        activity.session_epoch = 2;
        let facts = ActivityFacts {
            session_epoch: 1,
            kind: Some(DesktopKind::X11),
            display: Some(":0".into()),
            ..ActivityFacts::default()
        };
        sampler.poll(
            &PollInput {
                now_ms: 1_000,
                prefs: prefs(true, true, false, &[], &[]),
                activity,
                bound: BoundBackends::default(),
                paused: false,
                route: "x11-ewmh",
            },
            &mut provider,
            &facts,
            &mut || true,
        );
        assert_eq!(provider.identity.len(), 1);
        assert_eq!(provider.title_reads.get(), 0);
        sampler.seal(2_000);
        let blob = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert!(!blob.contains("SECRET"));
        assert!(!blob.contains("\"app\""));
    }

    #[test]
    fn still_ok_false_after_identity_skips_the_title_getter() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([Scripted::window("Notes", None)]),
            titled: VecDeque::from([Scripted::window("Notes", Some("SECRET"))]),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let checks = Cell::new(0u32);
        sampler.poll(
            &PollInput {
                now_ms: 1_000,
                prefs: prefs(true, true, false, &[], &[]),
                activity: live_state(),
                bound: BoundBackends::default(),
                paused: false,
                route: "x11-ewmh",
            },
            &mut provider,
            &ActivityFacts::default(),
            &mut || {
                let seen = checks.get();
                checks.set(seen + 1);
                seen < 3
            },
        );
        assert_eq!(provider.title_reads.get(), 0);
        assert!(provider.identity.is_empty());
        sampler.seal(2_000);
        let blob = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert!(!blob.contains("SECRET"));
        assert!(!blob.contains("Notes"));
    }

    #[test]
    fn cold_wayland_idle_is_availability_not_active() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([ForegroundRead::Unavailable]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let bound = BoundBackends {
            wayland_idle: true,
            ..BoundBackends::default()
        };
        let mut activity = live_state();
        activity.user_idle = false;
        activity.idle_observed = false;
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, false, &[], &[]),
            activity,
            bound,
            &mut provider,
        );
        sampler.seal(2_000);
        let blob = serde_json::to_string(&sampler.take_pending()).unwrap();
        assert!(blob.contains("\"capability\":\"idle\""));
        assert!(blob.contains("\"status\":\"unavailable\""));
        assert!(!blob.contains("\"state\":\"active\""));
    }

    #[test]
    fn lost_idle_backend_is_availability_until_a_new_measurement() {
        let mut sampler = ActivitySampler::new();
        let mut provider = Scripted {
            identity: VecDeque::from([
                ForegroundRead::Unavailable,
                ForegroundRead::Unavailable,
                ForegroundRead::Unavailable,
            ]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::new(),
            url: None,
            url_reads: Cell::new(0),
        };
        let policy = prefs(true, false, false, &[], &[]);
        let mut activity = live_state();
        activity.idle_observed = true;
        activity.user_idle = false;
        poll_at(
            &mut sampler,
            1_000,
            policy.clone(),
            activity,
            BoundBackends {
                wayland_idle: true,
                ..BoundBackends::default()
            },
            &mut provider,
        );
        activity.idle_observed = false;
        poll_at(
            &mut sampler,
            2_000,
            policy.clone(),
            activity,
            BoundBackends::default(),
            &mut provider,
        );
        activity.idle_observed = true;
        poll_at(
            &mut sampler,
            3_000,
            policy,
            activity,
            BoundBackends {
                mutter_idle: true,
                ..BoundBackends::default()
            },
            &mut provider,
        );
        sampler.seal(4_000);
        let rows = sampler.take_pending();
        let presence: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.payload {
                crate::activity_writer::DesktopActivityPayload::Activity { state } => {
                    Some((state.clone(), row.ts, row.end_ts, row.method.clone()))
                }
                crate::activity_writer::DesktopActivityPayload::Availability {
                    capability,
                    status,
                } if capability == "idle" => {
                    Some((status.clone(), row.ts, row.end_ts, row.method.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            presence,
            vec![
                ("active".into(), 1_000, 2_000, "ext-idle-notify-v1".into()),
                ("unavailable".into(), 2_000, 3_000, "unavailable".into()),
                ("active".into(), 3_000, 4_000, "Mutter IdleMonitor".into()),
            ]
        );
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains("\"state\":\"active\",\"method\":\"unavailable\""));
    }

    #[test]
    fn url_from_a_different_window_is_dropped_without_rereading_identity() {
        let mut sampler = ActivitySampler::new();
        let secret = "https://user:secret@hidden.example/path?q=1#frag";
        let mut provider = Scripted {
            identity: VecDeque::from([
                ForegroundRead::Window(ForegroundWindow {
                    app: "Firefox".into(),
                    app_id: Some("firefox".into()),
                    title: None,
                    pid: Some(42),
                    window_id: 7,
                    window_token: String::new(),
                }),
                ForegroundRead::Window(ForegroundWindow {
                    app: "POISON_APP".into(),
                    app_id: None,
                    title: Some("POISON_TITLE".into()),
                    pid: Some(42),
                    window_id: 99,
                    window_token: String::new(),
                }),
            ]),
            titled: VecDeque::new(),
            title_reads: Cell::new(0),
            confirm: VecDeque::from([ForegroundRead::Window(ForegroundWindow {
                app: "Firefox".into(),
                app_id: Some("firefox".into()),
                title: Some("EXCLUDED_TITLE".into()),
                pid: Some(42),
                window_id: 8,
                window_token: String::new(),
            })]),
            url: Some(secret.into()),
            url_reads: Cell::new(0),
        };
        poll_at(
            &mut sampler,
            1_000,
            prefs(true, false, true, &[], &[]),
            live_state(),
            BoundBackends::default(),
            &mut provider,
        );
        assert_eq!(provider.url_reads.get(), 1);
        assert_eq!(provider.identity.len(), 1);
        assert!(provider.confirm.is_empty());
        sampler.seal(2_000);
        let rows = sampler.take_pending();
        let blob = serde_json::to_string(&rows).unwrap();
        assert!(!blob.contains(secret));
        assert!(!blob.contains("hidden.example"));
        assert!(!blob.contains("POISON_APP"));
        assert!(!blob.contains("POISON_TITLE"));
        assert!(!blob.contains("EXCLUDED_TITLE"));
        assert!(!foreground_apps(&rows).iter().any(|app| app == "Firefox"));
    }

    #[test]
    fn write_error_token_has_no_payload() {
        let fault = ActivityFault::new();
        fault.record_write_error();
        let token = fault.token().unwrap();
        assert_eq!(token, ACTIVITY_FAULT);
        assert!(!token.contains("SENTINEL"));
        assert!(!token.contains('/'));
    }
}
