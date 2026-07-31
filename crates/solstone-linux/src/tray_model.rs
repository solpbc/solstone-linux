// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    observer::{Mode, StateSnapshot},
    sync_health::SyncHealth,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayStatus {
    Recording,
    Idle,
    Paused,
    // Not produced by `status()`: retained as a real rung in the reference icon/status ladder.
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayModel {
    pub status: TrayStatus,
    pub header: String,
    pub sync: String,
    pub tooltip: String,
    pub icon: String,
    pub sni_status: String,
    pub segment: String,
    pub cache: String,
    pub captures: String,
    pub uptime: String,
    pub resume: String,
    pub pause_visible: bool,
    pub resume_visible: bool,
    pub open_journal_enabled: bool,
}

pub fn status(snapshot: &StateSnapshot) -> TrayStatus {
    if snapshot.paused {
        TrayStatus::Paused
    } else if snapshot.mode == Mode::Screencast {
        TrayStatus::Recording
    } else {
        TrayStatus::Idle
    }
}

pub fn status_name(status: TrayStatus) -> &'static str {
    match status {
        TrayStatus::Recording => "recording",
        TrayStatus::Idle => "idle",
        TrayStatus::Paused => "paused",
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
        TrayStatus::Paused if pause > 0 => format!("paused ({}m remaining)", pause / 60),
        TrayStatus::Paused => "paused".into(),
        TrayStatus::Stopped => "not running".into(),
        TrayStatus::Recording => health.header_recording.clone(),
        TrayStatus::Idle => health.header_idle.clone(),
    }
}

pub fn tooltip(status: TrayStatus, health: &SyncHealth) -> String {
    let status_text = match status {
        TrayStatus::Recording => "on",
        TrayStatus::Paused => "paused",
        TrayStatus::Idle => "idle (screen inactive)",
        TrayStatus::Stopped => "not running",
    };
    format!("{status_text}\n{}", health.tooltip)
}

pub fn sni_status(status: TrayStatus, health: &SyncHealth) -> String {
    if status == TrayStatus::Stopped {
        "NeedsAttention".into()
    } else {
        health.sni_status.clone()
    }
}

pub fn icon_name(status: TrayStatus, health: &SyncHealth) -> String {
    if health.icon == "error" {
        "error"
    } else if status == TrayStatus::Stopped {
        "stopped"
    } else if status == TrayStatus::Paused {
        "paused"
    } else if health.icon == "syncing" {
        "syncing"
    } else if status == TrayStatus::Idle
        && health.state == crate::sync_health::HealthState::Connected
    {
        "idle"
    } else {
        match health.icon.as_str() {
            "recording" | "paused" | "idle" | "stopped" | "syncing" | "error" => &health.icon,
            _ => "recording",
        }
    }
    .to_owned()
}

pub fn build(snapshot: &StateSnapshot, interval: i64, now: f64, health: &SyncHealth) -> TrayModel {
    build_with_open_journal(snapshot, interval, now, health, false)
}

pub fn build_with_open_journal(
    snapshot: &StateSnapshot,
    interval: i64,
    now: f64,
    health: &SyncHealth,
    open_journal_enabled: bool,
) -> TrayModel {
    let status = status(snapshot);
    let segment = segment_remaining(snapshot, interval, now);
    let pause = pause_remaining(snapshot, now);
    let up = uptime(snapshot, now);
    TrayModel {
        status,
        header: header_label(status, health, pause),
        sync: health.sync_line.clone(),
        tooltip: tooltip(status, health),
        icon: icon_name(status, health),
        sni_status: sni_status(status, health),
        segment: format!("segment: {}:{:02} remaining", segment / 60, segment % 60),
        cache: format!("cache: {} MB", snapshot.total_size_mb),
        captures: format!("today: {} segments", snapshot.captures_today),
        uptime: format!("uptime: {}h {}m", up / 3600, (up % 3600) / 60),
        resume: if pause > 0 {
            format!("resume ({}m remaining)", pause / 60)
        } else {
            "resume".into()
        },
        pause_visible: status != TrayStatus::Paused,
        resume_visible: status == TrayStatus::Paused,
        open_journal_enabled,
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
            pending_confirmed: Some(0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        }
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
        }
    }
    fn connected_health() -> SyncHealth {
        derive_health(&connected_facts(), 100.0, 600.0)
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
                link: Some(LinkFactState {
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
                ..Default::default()
            },
            100.0,
            600.0,
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &connected, 0),
            "on — connected"
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &syncing, 0),
            "on — syncing"
        );
        assert_eq!(
            header_label(TrayStatus::Recording, &offline, 0),
            "on — offline (saving locally)"
        );
        assert_eq!(
            header_label(TrayStatus::Idle, &connected, 0),
            "idle — connected"
        );
        assert_eq!(
            header_label(TrayStatus::Idle, &syncing, 0),
            "idle — syncing"
        );
        assert_eq!(
            header_label(TrayStatus::Idle, &offline, 0),
            "idle — offline (saving locally)"
        );
        assert_eq!(header_label(TrayStatus::Paused, &connected, 0), "paused");
        assert_eq!(
            header_label(TrayStatus::Paused, &connected, 900),
            "paused (15m remaining)"
        );
        assert_eq!(
            header_label(TrayStatus::Paused, &offline, 59),
            "paused (0m remaining)"
        );
        assert_eq!(
            header_label(TrayStatus::Stopped, &connected, 0),
            "not running"
        );
    }
    #[test]
    fn recording_and_idle_headers_cover_complete_health_axis_from_surfaces() {
        let cases = [
            (connected_facts(), HealthState::Connected),
            (
                SyncFacts {
                    in_progress: true,
                    link: Some(LinkFactState {
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
        let model = build(&value, 300, 100.0, &connected_health());
        assert!(!model.pause_visible);
        assert!(model.resume_visible);
    }
    #[test]
    fn live_stats_and_tooltip_are_rendered_from_snapshot_and_health() {
        let model = build(&snapshot(), 300, 100.0, &connected_health());
        assert_eq!(model.cache, "cache: 3 MB");
        assert_eq!(model.captures, "today: 2 segments");
        assert_eq!(model.uptime, "uptime: 0h 0m");
        assert_eq!(model.segment, "segment: 5:00 remaining");
        assert_eq!(model.tooltip, "on\nsync: up to date");
    }
    #[test]
    fn stopped_and_syncing_tooltips_are_byte_exact() {
        let syncing = derive_health(
            &SyncFacts {
                in_progress: true,
                progress: "2/5".into(),
                link: Some(LinkFactState {
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
            build(&snapshot(), 300, 100.0, &health),
            build(&snapshot(), 300, 100.0, &health)
        );
    }
    #[test]
    fn pause_countdown_changes_resume_label() {
        let mut value = snapshot();
        value.paused = true;
        value.pause_until = Some(1_000.0);
        let health = connected_health();
        assert_ne!(
            build(&value, 300, 100.0, &health).resume,
            build(&value, 300, 160.0, &health).resume
        );
    }
    #[test]
    fn update_needed_uses_live_error_icon_and_attention() {
        let h = derive_health(
            &SyncFacts {
                last_error_class: Some(crate::sync_health::ErrorType::Incompatible),
                ..Default::default()
            },
            100.0,
            600.0,
        );
        let m = build(&snapshot(), 300, 100.0, &h);
        assert_eq!(m.icon, "error");
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
                ..Default::default()
            },
        ];
        for facts in facts {
            let health = derive_health(&facts, 100.0, 600.0);
            assert_eq!(
                build(&snapshot(), 300, 100.0, &health).sync,
                health.sync_line
            );
        }
    }
    #[test]
    fn icon_ladder_covers_four_statuses_by_seven_health_states() {
        let cases = [
            (
                connected_facts(),
                ["recording", "idle", "paused", "stopped"],
            ),
            (
                SyncFacts {
                    in_progress: true,
                    link: Some(LinkFactState {
                        observer_registered: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ["syncing", "syncing", "paused", "stopped"],
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Transient),
                    ..Default::default()
                },
                ["syncing", "syncing", "paused", "stopped"],
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    ..Default::default()
                },
                ["error", "error", "error", "error"],
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Auth),
                    ..Default::default()
                },
                ["error", "error", "error", "error"],
            ),
            (
                SyncFacts {
                    last_successful_contact: Some(0.0),
                    ..Default::default()
                },
                ["syncing", "syncing", "paused", "stopped"],
            ),
            (
                SyncFacts::default(),
                ["syncing", "syncing", "paused", "stopped"],
            ),
        ];
        let statuses = [
            TrayStatus::Recording,
            TrayStatus::Idle,
            TrayStatus::Paused,
            TrayStatus::Stopped,
        ];
        for (facts, expected) in cases {
            let health = derive_health(&facts, 1_000.0, 600.0);
            for (status, expected_icon) in statuses.into_iter().zip(expected) {
                assert_eq!(
                    icon_name(status, &health),
                    expected_icon,
                    "status={status:?}, health={:?}",
                    health.state
                );
            }
        }
    }
}
