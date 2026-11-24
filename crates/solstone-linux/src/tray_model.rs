// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    desktop_session::SessionPresence,
    observer::{Mode, ScreenStartFailureKind, StateSnapshot},
    sync_health::SyncHealth,
};
use solstone_i18n::{message, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenStartFault {
    MissingElements,
    PortalDeclined,
    StartFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayStatus {
    Recording,
    Idle,
    /// Live session, initial screen intake is starting (for example, waiting on portal consent).
    ScreenStarting,
    /// Live session, screen intake failed to start once and will retry automatically.
    ScreenRetrying,
    /// Live session, screen intake cannot start and needs owner attention.
    ScreenAttention(ScreenStartFault),
    Paused,
    /// Running, with no live desktop session of this owner to take in.
    Waiting(SessionPresence),
    // Not produced by `status()`: retained as a real rung in the reference icon/status ladder.
    Stopped,
}

impl TrayStatus {
    /// Something is wrong with desktop or screen intake, not because the owner stepped away.
    pub(crate) fn needs_attention(self) -> bool {
        matches!(
            self,
            Self::Waiting(SessionPresence::Unreachable | SessionPresence::LoginUnavailable)
                | Self::ScreenAttention(_)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayModel {
    pub import_held: bool,
    pub status: TrayStatus,
    pub about: crate::about::AboutBlock,
    pub header: String,
    pub sync: String,
    pub tooltip: String,
    pub icon: String,
    pub sni_status: String,
    pub cache: String,
    pub resume: String,
    pub pause_visible: bool,
    pub resume_visible: bool,
    pub open_journal_enabled: bool,
    pub health_state: crate::sync_health::HealthState,
    /// Whole seconds left on a timed pause. Zero is "until I resume" or none.
    pub pause_seconds: i32,
    /// Items waiting, when the count is known and above zero.
    pub waiting_count: Option<u64>,
}

pub fn status(snapshot: &StateSnapshot) -> TrayStatus {
    if snapshot.paused {
        TrayStatus::Paused
    } else if !snapshot.session.is_live() {
        TrayStatus::Waiting(snapshot.session)
    } else if snapshot.mode == Mode::Screencast {
        TrayStatus::Recording
    } else if snapshot.screen_starting {
        TrayStatus::ScreenStarting
    } else if let Some(failure) = &snapshot.screen_start_failure {
        if failure.needs_attention() {
            TrayStatus::ScreenAttention(match &failure.kind {
                ScreenStartFailureKind::MissingElements { .. } => ScreenStartFault::MissingElements,
                ScreenStartFailureKind::PortalDeclined => ScreenStartFault::PortalDeclined,
                ScreenStartFailureKind::StartFailed => ScreenStartFault::StartFailed,
            })
        } else if failure.retrying() {
            TrayStatus::ScreenRetrying
        } else {
            TrayStatus::Idle
        }
    } else {
        TrayStatus::Idle
    }
}

pub fn status_name(status: TrayStatus) -> &'static str {
    match status {
        TrayStatus::Recording => "recording",
        TrayStatus::Idle
        | TrayStatus::ScreenStarting
        | TrayStatus::ScreenRetrying
        | TrayStatus::ScreenAttention(_) => "idle",
        TrayStatus::Paused => "paused",
        TrayStatus::Waiting(_) => "waiting",
        TrayStatus::Stopped => "stopped",
    }
}

pub fn segment_remaining(snapshot: &StateSnapshot, interval: i64, now: f64) -> i32 {
    if snapshot.paused {
        return 0;
    }
    snapshot.segment_start_mono.map_or(0, |start| {
        ((interval as f64 - (now - start)).max(0.0)) as i32
    })
}

pub fn pause_remaining(snapshot: &StateSnapshot, now: f64) -> i32 {
    if !snapshot.paused {
        return 0;
    }
    snapshot
        .pause_until
        .map_or(0, |until| ((until - now).max(0.0)) as i32)
}

pub fn uptime(snapshot: &StateSnapshot, now: f64) -> i32 {
    ((now - snapshot.process_start_mono).max(0.0)) as i32
}

pub fn header_label(status: TrayStatus, health: &SyncHealth, pause: i32) -> String {
    match status {
        TrayStatus::Paused if pause > 0 => {
            solstone_i18n::cardinal("linux.tray.pausedxmremaining", i64::from(pause / 60), &[])
        }
        TrayStatus::Paused => text!("linux.copy.paused").into(),
        TrayStatus::Waiting(SessionPresence::LoginUnavailable) => {
            text!("linux.tray.cantcheckwhichsessionisinfront").into()
        }
        status @ TrayStatus::Waiting(_) if status.needs_attention() => {
            text!("linux.tray.cantreachyourdesktop").into()
        }
        TrayStatus::Waiting(_) => text!("linux.tray.waitingforyourdesktop").into(),
        TrayStatus::ScreenStarting | TrayStatus::ScreenRetrying => {
            text!("linux.tray.waitingforscreenintake").into()
        }
        TrayStatus::ScreenAttention(ScreenStartFault::MissingElements) => {
            text!("linux.tray.screenintakecantstart").into()
        }
        TrayStatus::ScreenAttention(ScreenStartFault::PortalDeclined) => {
            text!("linux.tray.screensharingnotallowed").into()
        }
        TrayStatus::ScreenAttention(ScreenStartFault::StartFailed) => {
            text!("linux.tray.screenintakecouldntstart").into()
        }
        TrayStatus::Stopped => text!("linux.tray.notrunning").into(),
        TrayStatus::Recording => health.header_recording.clone(),
        TrayStatus::Idle => health.header_idle.clone(),
    }
}

pub fn tooltip(status: TrayStatus, health: &SyncHealth) -> String {
    let status_text = match status {
        TrayStatus::Recording => text!("linux.copy.on"),
        TrayStatus::Paused => text!("linux.copy.paused"),
        TrayStatus::Idle => text!("linux.tray.idlescreeninactive"),
        TrayStatus::ScreenStarting | TrayStatus::ScreenRetrying => {
            text!("linux.tray.waitingforscreenintake")
        }
        TrayStatus::ScreenAttention(ScreenStartFault::MissingElements) => {
            text!("linux.tray.screenintakecantstart")
        }
        TrayStatus::ScreenAttention(ScreenStartFault::PortalDeclined) => {
            text!("linux.tray.screensharingnotallowed")
        }
        TrayStatus::ScreenAttention(ScreenStartFault::StartFailed) => {
            text!("linux.tray.screenintakecouldntstart")
        }
        TrayStatus::Waiting(SessionPresence::NotActive) => {
            text!("linux.tray.waitingyoursessionisntinfront")
        }
        TrayStatus::Waiting(SessionPresence::Unreachable) => {
            text!("linux.tray.cantreachyourdesktop")
        }
        TrayStatus::Waiting(SessionPresence::LoginUnavailable) => {
            text!("linux.tray.cantcheckwhichsessionisinfront")
        }
        TrayStatus::Waiting(_) => {
            text!("linux.tray.waitingnoneofyoursessionsisinfront")
        }
        TrayStatus::Stopped => text!("linux.tray.notrunning"),
    };
    message!("linux.tray.tooltip", "status" => status_text, "health" => health.tooltip)
}

pub fn sni_status(status: TrayStatus, health: &SyncHealth) -> String {
    if status == TrayStatus::Stopped || status.needs_attention() {
        "NeedsAttention".into()
    } else {
        health.sni_status.clone()
    }
}

pub fn icon_name(status: TrayStatus, health: &SyncHealth) -> String {
    if status.needs_attention() {
        return "attention".into();
    }
    match health.icon.as_str() {
        "healthy" => match status {
            TrayStatus::Paused | TrayStatus::Stopped | TrayStatus::Waiting(_) => "paused",
            TrayStatus::Recording
            | TrayStatus::Idle
            | TrayStatus::ScreenStarting
            | TrayStatus::ScreenRetrying
            | TrayStatus::ScreenAttention(_) => "healthy",
        },
        "connecting" => match status {
            TrayStatus::Paused | TrayStatus::Stopped | TrayStatus::Waiting(_) => "paused",
            TrayStatus::Recording
            | TrayStatus::Idle
            | TrayStatus::ScreenStarting
            | TrayStatus::ScreenRetrying
            | TrayStatus::ScreenAttention(_) => "connecting",
        },
        "attention" => "attention",
        "paused" => "paused",
        "offline" => "offline",
        "error" => "error",
        _ => "offline",
    }
    .to_owned()
}

pub fn build(snapshot: &StateSnapshot, now: f64, health: &SyncHealth) -> TrayModel {
    build_with_open_journal(snapshot, now, health, false)
}

pub fn build_with_open_journal(
    snapshot: &StateSnapshot,
    now: f64,
    health: &SyncHealth,
    open_journal_enabled: bool,
) -> TrayModel {
    let status = status(snapshot);
    let pause = pause_remaining(snapshot, now);
    TrayModel {
        import_held: health.import_held,
        status,
        about: crate::about::AboutBlock::unknown(crate::about::HostFacts::default()),
        header: header_label(status, health, pause),
        sync: health.sync_line.clone(),
        tooltip: tooltip(status, health),
        icon: icon_name(status, health),
        sni_status: sni_status(status, health),
        cache: message!("linux.tray.cachexmb", "value1" => snapshot.total_size_mb),
        resume: if pause > 0 {
            solstone_i18n::cardinal("linux.tray.resumexmremaining", i64::from(pause / 60), &[])
        } else {
            text!("linux.copy.resume").into()
        },
        pause_visible: status != TrayStatus::Paused,
        resume_visible: status == TrayStatus::Paused,
        open_journal_enabled,
        health_state: health.state,
        pause_seconds: pause,
        waiting_count: health.waiting_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        private_link::LinkFactState,
        sync_health::{ErrorType, HealthState, SURFACE_BY_STATE, SyncFacts, derive_health},
    };
    fn connected_facts() -> SyncFacts {
        SyncFacts {
            last_successful_contact: Some(0.0),
            pending_confirmed: Some(0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        }
    }
    #[test]
    fn no_live_session_waits_and_a_broken_one_asks_for_attention() {
        let health = derive_health(&connected_facts(), 0.0, 300.0);
        let mut value = snapshot();
        value.mode = Mode::Screencast;
        value.session = SessionPresence::NotActive;
        let away = build(&value, 0.0, &health);
        assert_eq!(away.status, TrayStatus::Waiting(SessionPresence::NotActive));
        assert_eq!(status_name(away.status), "waiting");
        assert_eq!(away.header, "waiting for your desktop");
        assert_eq!(away.icon, "paused");
        assert_ne!(away.sni_status, "NeedsAttention");
        value.session = SessionPresence::Unreachable;
        let broken = build(&value, 0.0, &health);
        assert_eq!(broken.header, "can't reach your desktop");
        assert_eq!(broken.icon, "attention");
        assert_eq!(broken.sni_status, "NeedsAttention");
        value.paused = true;
        assert_eq!(build(&value, 0.0, &health).status, TrayStatus::Paused);
    }

    fn snapshot() -> StateSnapshot {
        StateSnapshot {
            mode: Mode::Screencast,
            paused: false,
            segment_open: true,
            captures_today: 2,
            total_size_mb: 3,
            pause_until: None,
            segment_start_mono: Some(100.0),
            process_start_mono: 50.0,
            session: Default::default(),
            activity_through_pause: false,
            away: false,
            screen_starting: false,
            screen_start_failure: None,
            screen_retry_seq: 0,
        }
    }
    fn connected_health() -> SyncHealth {
        derive_health(&connected_facts(), 0.0, 600.0)
    }
    #[test]
    fn countdowns_are_computed_at_read_time() {
        let s = snapshot();
        assert!(segment_remaining(&s, 300, 102.0) > segment_remaining(&s, 300, 103.0));
    }
    #[test]
    fn header_matrix_all_ten_typed_rows_byte_exact() {
        let connected = connected_health();
        let syncing = derive_health(
            &SyncFacts {
                in_progress: true,
                last_successful_contact: Some(100.0),
                link: Some(LinkFactState {
                    carrier_proven: true,
                    observer_registered: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            100.0,
            600.0,
        );
        let offline = derive_health(
            &SyncFacts {
                last_error_class: Some(ErrorType::Transient),
                last_successful_contact: Some(0.0),
                ..Default::default()
            },
            100.0,
            600.0,
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &connected, 0),
            "on, connected"
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &syncing, 0),
            "on, syncing"
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &offline, 0),
            "on, offline (held on this device)"
        );
        assert_eq!(
            header_label(TrayStatus::Idle, &connected, 0),
            "idle, connected"
        );
        assert_eq!(header_label(TrayStatus::Idle, &syncing, 0), "idle, syncing");
        assert_eq!(
            header_label(TrayStatus::Idle, &offline, 0),
            "idle, offline (held on this device)"
        );
        assert_eq!(header_label(TrayStatus::Paused, &connected, 0), "paused");
        assert_eq!(
            header_label(TrayStatus::Paused, &connected, 900),
            "paused (15 minutes remaining)"
        );
        assert_eq!(
            header_label(TrayStatus::Paused, &offline, 59),
            "paused (0 minutes remaining)"
        );
        assert_eq!(
            header_label(TrayStatus::Stopped, &connected, 0),
            "not running"
        );
    }
    #[test]
    fn recording_and_idle_headers_cover_complete_health_axis_from_surfaces() {
        let cases = [
            (
                SyncFacts {
                    last_successful_contact: Some(1_000.0),
                    ..connected_facts()
                },
                HealthState::Connected,
            ),
            (
                SyncFacts {
                    in_progress: true,
                    last_successful_contact: Some(1_000.0),
                    link: Some(LinkFactState {
                        carrier_proven: true,
                        observer_registered: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                HealthState::Syncing,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Transient),
                    last_successful_contact: Some(0.0),
                    ..Default::default()
                },
                HealthState::Offline,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    ..Default::default()
                },
                HealthState::UpdateRequired,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Auth),
                    ..Default::default()
                },
                HealthState::RePairRequired,
            ),
            (
                SyncFacts {
                    last_successful_contact: Some(0.0),
                    ..Default::default()
                },
                HealthState::Offline,
            ),
            (SyncFacts::default(), HealthState::NotReported),
        ];
        for (facts, expected_state) in cases {
            let health = derive_health(&facts, 1_000.0, 600.0);
            assert_eq!(health.state, expected_state);
            let surface = SURFACE_BY_STATE
                .get(&expected_state)
                .unwrap_or_else(|| panic!("surface missing for {expected_state:?}"));
            assert_eq!(
                header_label(TrayStatus::Recording, &health, 0),
                surface.header_recording
            );
            assert_eq!(
                header_label(TrayStatus::Idle, &health, 0),
                surface.header_idle
            );
        }
    }
    #[test]
    fn paused_and_idle_snapshots_select_typed_status() {
        let mut value = snapshot();
        value.paused = true;
        assert_eq!(status(&value), TrayStatus::Paused);
        value.paused = false;
        value.mode = Mode::Idle;
        assert_eq!(status(&value), TrayStatus::Idle);
        value.paused = true;
        let model = build(&value, 100.0, &connected_health());
        assert!(!model.pause_visible);
        assert!(model.resume_visible);
    }
    #[test]
    fn live_stats_and_tooltip_are_rendered_from_snapshot_and_health() {
        let model = build(&snapshot(), 100.0, &connected_health());
        assert_eq!(model.cache, "cache: 3 MB");
        assert_eq!(model.tooltip, "on\nsync: up to date");
    }
    #[test]
    fn a_recording_tray_is_unchanged_from_one_second_to_the_next() {
        let health = connected_health();
        let at = |now: f64| build(&snapshot(), now, &health);
        for now in 101..160 {
            assert_eq!(at(now as f64), at(100.0), "the tray changed at {now}");
        }
    }
    #[test]
    fn stopped_and_syncing_tooltips_are_byte_exact() {
        let syncing = derive_health(
            &SyncFacts {
                in_progress: true,
                progress: "2/5".into(),
                last_successful_contact: Some(100.0),
                link: Some(LinkFactState {
                    carrier_proven: true,
                    observer_registered: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            100.0,
            600.0,
        );
        assert_eq!(
            tooltip(TrayStatus::Stopped, &connected_health()),
            "not running\nsync: up to date"
        );
        assert_eq!(tooltip(TrayStatus::Recording, &syncing), "on\nsync: 2/5");
    }
    #[test]
    fn stopped_status_requests_attention() {
        assert_eq!(
            sni_status(TrayStatus::Stopped, &connected_health()),
            "NeedsAttention"
        );
    }
    #[test]
    fn identical_inputs_produce_an_identical_model() {
        let health = connected_health();
        assert_eq!(
            build(&snapshot(), 100.0, &health),
            build(&snapshot(), 100.0, &health)
        );
    }
    #[test]
    fn pause_countdown_changes_resume_label() {
        let mut value = snapshot();
        value.paused = true;
        value.pause_until = Some(1_000.0);
        let health = connected_health();
        assert_ne!(
            build(&value, 100.0, &health).resume,
            build(&value, 160.0, &health).resume
        );
    }
    #[test]
    fn update_needed_uses_live_attention_icon_and_attention() {
        let h = derive_health(
            &SyncFacts {
                last_error_class: Some(crate::sync_health::ErrorType::Incompatible),
                ..Default::default()
            },
            100.0,
            600.0,
        );
        let m = build(&snapshot(), 100.0, &h);
        assert_eq!(m.icon, "attention");
        assert_eq!(m.sni_status, "NeedsAttention");
    }
    #[test]
    fn sync_labels_follow_resolved_health_surfaces() {
        let facts = [
            connected_facts(),
            SyncFacts {
                in_progress: true,
                progress: "3/10 segments".into(),
                ..Default::default()
            },
            SyncFacts {
                last_error_class: Some(ErrorType::Transient),
                last_successful_contact: Some(0.0),
                ..Default::default()
            },
        ];
        for facts in facts {
            let health = derive_health(&facts, 100.0, 600.0);
            assert_eq!(build(&snapshot(), 100.0, &health).sync, health.sync_line);
        }
    }
    #[test]
    fn screen_start_failures_keep_idle_status_name_and_gate_attention() {
        let health = connected_health();

        // Ordinary idle (no start failure) stays calm Idle.
        let mut idle = snapshot();
        idle.mode = Mode::Idle;
        assert_eq!(status(&idle), TrayStatus::Idle);
        assert_eq!(status_name(status(&idle)), "idle");
        let idle_model = build(&idle, 100.0, &health);
        assert_eq!(idle_model.icon, "healthy");
        assert_eq!(idle_model.sni_status, "Active");

        // Initial startup (while screen intake is starting) stays calm and keeps status_name "idle".
        let mut starting = idle.clone();
        starting.screen_starting = true;
        assert_eq!(status(&starting), TrayStatus::ScreenStarting);
        assert!(!status(&starting).needs_attention());
        assert_eq!(status_name(status(&starting)), "idle");
        let start_model = build(&starting, 100.0, &health);
        assert_eq!(start_model.icon, "healthy");
        assert_eq!(start_model.sni_status, "Active");
        let mut starting_recording = starting.clone();
        starting_recording.mode = Mode::Screencast;
        assert_eq!(status(&starting_recording), TrayStatus::Recording);
        let mut starting_paused = starting.clone();
        starting_paused.paused = true;
        assert_eq!(status(&starting_paused), TrayStatus::Paused);
        let mut starting_away = starting.clone();
        starting_away.session = SessionPresence::NotActive;
        assert_eq!(
            status(&starting_away),
            TrayStatus::Waiting(SessionPresence::NotActive)
        );

        // Attempt 1 (retrying) stays calm (no attention icon/SNI, status_name still "idle").
        let mut retrying = idle.clone();
        retrying.screen_start_failure = Some(crate::observer::ScreenStartFailure {
            kind: ScreenStartFailureKind::StartFailed,
            consecutive_failures: 1,
        });
        assert_eq!(status(&retrying), TrayStatus::ScreenRetrying);
        assert!(!status(&retrying).needs_attention());
        assert_eq!(status_name(status(&retrying)), "idle");
        let retry_model = build(&retrying, 100.0, &health);
        assert_eq!(retry_model.icon, "healthy");
        assert_eq!(retry_model.sni_status, "Active");

        // Missing GStreamer component at attempt 1, and portal/generic failure at attempt 2,
        // each flip icon/SNI to attention while keeping status_name "idle".
        for (failure, expected_fault) in [
            (
                crate::observer::ScreenStartFailure {
                    kind: ScreenStartFailureKind::MissingElements {
                        elements: vec!["pipewiresrc".into()],
                        pipewire: true,
                    },
                    consecutive_failures: 1,
                },
                ScreenStartFault::MissingElements,
            ),
            (
                crate::observer::ScreenStartFailure {
                    kind: ScreenStartFailureKind::PortalDeclined,
                    consecutive_failures: 2,
                },
                ScreenStartFault::PortalDeclined,
            ),
            (
                crate::observer::ScreenStartFailure {
                    kind: ScreenStartFailureKind::StartFailed,
                    consecutive_failures: 2,
                },
                ScreenStartFault::StartFailed,
            ),
        ] {
            let mut faulted = idle.clone();
            faulted.screen_start_failure = Some(failure);
            assert_eq!(
                status(&faulted),
                TrayStatus::ScreenAttention(expected_fault)
            );
            assert!(status(&faulted).needs_attention());
            assert_eq!(status_name(status(&faulted)), "idle");
            let model = build(&faulted, 100.0, &health);
            assert_eq!(model.icon, "attention");
            assert_eq!(model.sni_status, "NeedsAttention");
        }
    }
}
