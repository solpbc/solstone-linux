// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::shell_model::{self, PauseLeft};
use crate::sync_health::HealthState;
use crate::tray_model::TrayModel;
use ksni::menu::{MenuItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip, Tray};
use std::sync::mpsc::Sender;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray_icons.rs"));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Pause(u64),
    PauseIndefinite,
    Resume,
    OpenJournal,
    OpenUrl(&'static str),
    ReportProblem(crate::tray_model::TrayStatus, crate::about::AboutBlock),
    CopyAbout(crate::about::AboutBlock),
    OpenConfig,
    CopyInstructions,
    /// The owner's desktop-capture switch, already saved.
    SetDesktopCapture(bool),
    Quit,
}

pub struct KsniTray {
    pub model: TrayModel,
    pub commands: Sender<TrayCommand>,
}
fn item<T: Tray>(label: impl Into<String>, enabled: bool) -> MenuItem<T> {
    StandardItem {
        label: label.into(),
        enabled,
        ..Default::default()
    }
    .into()
}
fn journal_paired(state: HealthState) -> bool {
    !matches!(
        state,
        HealthState::PairingRequired | HealthState::NotReported
    )
}

/// The disabled status row. A timed pause uses the time-left line. Offline is never
/// blank: the word alone, or the count when one is known.
fn menu_status_line(model: &TrayModel) -> String {
    if model.resume_visible {
        return if model.pause_seconds > 0 {
            shell_model::pause_line(PauseLeft::Seconds(model.pause_seconds as u32))
        } else {
            shell_model::pause_line(PauseLeft::UntilResume)
        };
    }
    match model.health_state {
        HealthState::PairingRequired | HealthState::NotReported => "not paired".to_string(),
        HealthState::Connected => "connected · all caught up".to_string(),
        HealthState::Offline => match model.waiting_count {
            Some(count) if count > 0 => format!("offline · {count} waiting"),
            _ => "offline".to_string(),
        },
        _ => String::new(),
    }
}

fn action(label: &str, command: TrayCommand) -> StandardItem<KsniTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |tray| {
            let _ = tray.commands.send(command.clone());
        }),
        ..Default::default()
    }
}

impl Tray for KsniTray {
    /// A click opens this menu. The item does not handle activation itself.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "solstone-observer".into()
    }
    fn category(&self) -> Category {
        Category::SystemServices
    }
    fn title(&self) -> String {
        "solstone".into()
    }
    fn status(&self) -> Status {
        if self.model.sni_status == "NeedsAttention" {
            Status::NeedsAttention
        } else {
            Status::Active
        }
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        let data = match self.model.icon.as_str() {
            "healthy" => generated::HEALTHY,
            "attention" => generated::ATTENTION,
            "paused" => generated::PAUSED,
            "offline" => generated::OFFLINE,
            "error" => generated::ERROR,
            "connecting" => generated::CONNECTING,
            _ => generated::OFFLINE,
        };
        vec![Icon {
            width: 64,
            height: 64,
            data: data.to_vec(),
        }]
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "solstone".into(),
            description: self.model.tooltip.clone(),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut rows: Vec<MenuItem<Self>> = Vec::new();
        if self.model.status.needs_attention() {
            rows.push(
                StandardItem {
                    label: "screen needs attention".into(),
                    activate: Box::new(|_: &mut KsniTray| {
                        crate::cli::spawn_window_from_tray();
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        rows.push(item(menu_status_line(&self.model), false));
        if self.model.pause_visible {
            rows.push(
                SubMenu {
                    label: "pause".into(),
                    submenu: vec![
                        action("15 minutes", TrayCommand::Pause(900)).into(),
                        action("30 minutes", TrayCommand::Pause(1800)).into(),
                        action("1 hour", TrayCommand::Pause(3600)).into(),
                        action("until I resume", TrayCommand::PauseIndefinite).into(),
                    ],
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.model.resume_visible {
            rows.push(
                StandardItem {
                    label: "resume".into(),
                    activate: Box::new(|tray: &mut KsniTray| {
                        let _ = tray.commands.send(TrayCommand::Resume);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        rows.push(
            StandardItem {
                label: "open settings".into(),
                activate: Box::new(|_: &mut KsniTray| {
                    crate::cli::spawn_settings_window_from_tray();
                }),
                ..Default::default()
            }
            .into(),
        );
        if journal_paired(self.model.health_state) {
            rows.push(
                StandardItem {
                    label: "open journal".into(),
                    enabled: self.model.open_journal_enabled,
                    activate: Box::new(|tray: &mut KsniTray| {
                        let _ = tray.commands.send(TrayCommand::OpenJournal);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        rows.push(
            StandardItem {
                label: "about solstone".into(),
                activate: Box::new(|_: &mut KsniTray| {
                    crate::cli::spawn_window_from_tray();
                }),
                ..Default::default()
            }
            .into(),
        );
        rows.push(
            StandardItem {
                label: "quit solstone".into(),
                activate: Box::new(|tray: &mut KsniTray| {
                    let _ = tray.commands.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        rows
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        tracing::warn!(
            ?reason,
            "StatusNotifierWatcher offline; waiting for automatic re-registration"
        );
        true
    }
    fn watcher_online(&self) {
        tracing::info!("StatusNotifierWatcher online");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        desktop_session::SessionPresence,
        observer::{Mode, StateSnapshot},
        private_link::LinkFactState,
        sync_health::{ErrorType, SyncFacts, SyncHealth, derive_health},
        tray_model::{self, TrayStatus},
    };
    fn tray() -> KsniTray {
        let snapshot = StateSnapshot {
            mode: Mode::Screencast,
            paused: false,
            segment_open: true,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: Some(0.0),
            process_start_mono: 0.0,
            session: Default::default(),
            activity_through_pause: false,
            away: false,
        };
        let health = derive_health(
            &SyncFacts {
                pending_confirmed: Some(0),
                ..Default::default()
            },
            0.0,
            600.0,
        );
        let (commands, _) = std::sync::mpsc::channel();
        KsniTray {
            model: tray_model::build(&snapshot, 300, 0.0, &health),
            commands,
        }
    }
    #[test]
    fn title_and_tooltip_use_solstone_not_sol() {
        let tray = tray();
        assert_eq!(tray.title(), "solstone");
        assert_eq!(tray.tool_tip().title, "solstone");
        assert_ne!(tray.title(), "sol");
        assert_ne!(tray.tool_tip().title, "sol");
    }

    fn labels(menu: &[MenuItem<KsniTray>]) -> Vec<&str> {
        menu.iter()
            .map(|item| match item {
                MenuItem::Standard(value) => value.label.as_str(),
                MenuItem::SubMenu(value) => value.label.as_str(),
                MenuItem::Separator => "<separator>",
                _ => "<other>",
            })
            .collect()
    }

    #[test]
    fn menu_contains_reference_top_level_structure() {
        let mut tray = tray();
        tray.model.health_state = crate::sync_health::HealthState::Connected;
        tray.model.open_journal_enabled = true;
        let menu = tray.menu();
        assert_eq!(
            labels(&menu),
            [
                "connected · all caught up",
                "pause",
                "open settings",
                "open journal",
                "about solstone",
                "quit solstone",
            ]
        );
        let MenuItem::Standard(status) = &menu[0] else {
            panic!("status row missing");
        };
        assert!(!status.enabled);
        let MenuItem::SubMenu(pause) = &menu[1] else {
            panic!("pause submenu missing");
        };
        assert_eq!(pause.submenu.len(), 4);
        let (sender, receiver) = std::sync::mpsc::channel();
        tray.commands = sender;
        let mut actions = Vec::new();
        for item in &pause.submenu {
            if let MenuItem::Standard(value) = item {
                (value.activate)(&mut tray);
                if let Ok(command) = receiver.try_recv() {
                    actions.push(command);
                }
            }
        }
        assert_eq!(
            actions,
            [
                TrayCommand::Pause(900),
                TrayCommand::Pause(1800),
                TrayCommand::Pause(3600),
                TrayCommand::PauseIndefinite
            ]
        );
        let MenuItem::Standard(open_journal) = &menu[3] else {
            panic!("open journal row missing");
        };
        assert!(open_journal.enabled);
        (open_journal.activate)(&mut tray);
        assert_eq!(receiver.try_recv(), Ok(TrayCommand::OpenJournal));
        let MenuItem::Standard(quit) = &menu[5] else {
            panic!("quit row missing");
        };
        (quit.activate)(&mut tray);
        assert_eq!(receiver.try_recv(), Ok(TrayCommand::Quit));
    }

    #[test]
    fn offline_status_line_is_never_blank() {
        let mut tray = tray();
        tray.model.health_state = crate::sync_health::HealthState::Offline;
        tray.model.waiting_count = None;
        assert_eq!(labels(&tray.menu())[0], "offline");
        tray.model.waiting_count = Some(12);
        assert_eq!(labels(&tray.menu())[0], "offline · 12 waiting");
        tray.model.waiting_count = Some(0);
        assert_eq!(labels(&tray.menu())[0], "offline");
    }

    #[test]
    fn about_row_does_not_carry_the_journal_snapshot() {
        let mut tray = tray();
        let saved = crate::sync_health::PairedJournalVersion {
            identity_key: "PRIVATE_identity".into(),
            version: "2.0.29".into(),
            name: Some("PRIVATE_device".into()),
            observed_at: 0.0,
            about: None,
            about_observed_at: None,
        };
        tray.model.about = crate::about::AboutBlock::from_version(
            crate::about::HostFacts::default(),
            Some(&saved),
            false,
            172800,
        );
        let menu = tray.menu();
        let joined = labels(&menu).join("\n");
        assert!(joined.contains("about solstone"));
        assert!(!joined.contains("PRIVATE"));
    }

    #[test]
    fn open_journal_menu_enablement_follows_model_epoch() {
        let mut tray = tray();
        tray.model.health_state = crate::sync_health::HealthState::Connected;
        for expected in [false, true, false] {
            tray.model.open_journal_enabled = expected;
            let menu = tray.menu();
            let Some(MenuItem::Standard(open_journal)) = menu.iter().find(
                |item| matches!(item, MenuItem::Standard(value) if value.label == "open journal"),
            ) else {
                panic!("open journal row missing");
            };
            assert_eq!(open_journal.enabled, expected);
        }
    }

    #[test]
    fn unpaired_menu_names_attention_and_hides_the_journal() {
        let mut tray = tray();
        tray.model.status = TrayStatus::Waiting(SessionPresence::Unreachable);
        tray.model.health_state = crate::sync_health::HealthState::PairingRequired;
        tray.model.open_journal_enabled = true;
        assert_eq!(
            labels(&tray.menu()),
            [
                "screen needs attention",
                "not paired",
                "pause",
                "open settings",
                "about solstone",
                "quit solstone",
            ]
        );
    }

    #[test]
    fn paused_menu_uses_the_time_left_line_and_resume() {
        let mut tray = tray();
        tray.model.health_state = crate::sync_health::HealthState::Connected;
        tray.model.status = TrayStatus::Paused;
        tray.model.pause_visible = false;
        tray.model.resume_visible = true;
        tray.model.pause_seconds = 12 * 60;
        assert_eq!(
            labels(&tray.menu()),
            [
                "paused, 12 min left",
                "resume",
                "open settings",
                "open journal",
                "about solstone",
                "quit solstone",
            ]
        );
    }

    fn pixmap_from_health(health: &SyncHealth, status: TrayStatus) -> Vec<u8> {
        let mut tray = tray();
        tray.model.icon = tray_model::icon_name(status, health);
        tray.icon_pixmap()
            .into_iter()
            .next()
            .expect("one pixmap")
            .data
    }

    fn pixmap_for(facts: &SyncFacts, status: TrayStatus) -> Vec<u8> {
        pixmap_from_health(&derive_health(facts, 1_000.0, 600.0), status)
    }

    #[test]
    fn recording_health_states_select_embedded_mark_pixmaps() {
        let cases = [
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        private_state_invalid: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::ATTENTION,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Auth),
                    ..Default::default()
                },
                generated::ATTENTION,
            ),
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        pairing_required: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::ATTENTION,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    ..Default::default()
                },
                generated::ATTENTION,
            ),
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        token_persistence_failure: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::ERROR,
            ),
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        transport_unavailable: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::OFFLINE,
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Transient),
                    last_successful_contact: Some(0.0),
                    ..Default::default()
                },
                generated::OFFLINE,
            ),
            (SyncFacts::default(), generated::OFFLINE),
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        listener_ready: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::CONNECTING,
            ),
            (
                SyncFacts {
                    link: Some(LinkFactState {
                        carrier_proven: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                generated::CONNECTING,
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
                generated::HEALTHY,
            ),
            (
                SyncFacts {
                    pending_confirmed: Some(0),
                    last_successful_contact: Some(1_000.0),
                    link: Some(LinkFactState {
                        carrier_proven: true,
                        observer_registered: true,
                        ..LinkFactState::default()
                    }),
                    ..SyncFacts::default()
                },
                generated::HEALTHY,
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(
                pixmap_for(&facts, TrayStatus::Recording).as_slice(),
                expected
            );
        }
    }

    #[test]
    fn capture_status_overlays_only_healthy_and_connecting() {
        let connected = SyncFacts {
            pending_confirmed: Some(0),
            last_successful_contact: Some(1_000.0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..LinkFactState::default()
            }),
            ..SyncFacts::default()
        };
        let syncing = SyncFacts {
            in_progress: true,
            last_successful_contact: Some(1_000.0),
            link: Some(LinkFactState {
                carrier_proven: true,
                observer_registered: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let connecting = SyncFacts {
            link: Some(LinkFactState {
                carrier_proven: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let update_required = SyncFacts {
            last_error_class: Some(ErrorType::Incompatible),
            ..Default::default()
        };
        let token_persistence_failed = SyncFacts {
            link: Some(LinkFactState {
                token_persistence_failure: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let offline = SyncFacts {
            last_error_class: Some(ErrorType::Transient),
            last_successful_contact: Some(0.0),
            ..Default::default()
        };
        let cases = [
            (TrayStatus::Idle, &connected, generated::HEALTHY),
            (TrayStatus::Idle, &syncing, generated::HEALTHY),
            (TrayStatus::Paused, &connected, generated::PAUSED),
            (TrayStatus::Paused, &connecting, generated::PAUSED),
            (TrayStatus::Paused, &update_required, generated::ATTENTION),
            (
                TrayStatus::Paused,
                &token_persistence_failed,
                generated::ERROR,
            ),
            (TrayStatus::Paused, &offline, generated::OFFLINE),
            (TrayStatus::Stopped, &connected, generated::PAUSED),
            (
                TrayStatus::Stopped,
                &token_persistence_failed,
                generated::ERROR,
            ),
        ];
        for (status, facts, expected) in cases {
            assert_eq!(pixmap_for(facts, status).as_slice(), expected);
        }
    }

    #[test]
    fn embedded_pixmaps_are_pairwise_distinct() {
        let marks = [
            generated::HEALTHY,
            generated::ATTENTION,
            generated::PAUSED,
            generated::OFFLINE,
            generated::ERROR,
            generated::CONNECTING,
        ];
        for (i, left) in marks.iter().enumerate() {
            for right in &marks[i + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn unrecognized_mark_name_draws_offline_not_healthy() {
        let mut health = derive_health(&SyncFacts::default(), 1_000.0, 600.0);
        health.icon = "recording".into();
        let bytes = pixmap_from_health(&health, TrayStatus::Recording);
        assert_eq!(bytes.as_slice(), generated::OFFLINE);
        assert_ne!(bytes.as_slice(), generated::HEALTHY);
    }

    #[test]
    fn watcher_offline_returns_true_for_service_survival() {
        let test_tray = tray();
        assert!(test_tray.watcher_offline(ksni::OfflineReason::No));
    }
}

// Python tray provenance (35/35):
// test_resolve_icon_theme_path_prefers_installed: retired-by-dependency; build-generated pixmaps replace icon-theme filesystem lookup.
// test_resolve_icon_theme_path_contrib_fallback: retired-by-dependency; build-generated pixmaps replace icon-theme filesystem lookup.
// test_make_app_uses_observer_config: retired-by-wiring; Rust components are constructed separately until the sibling run-loop work lands.
// test_build_menu_creates_expected_items -> tray::tests::menu_contains_reference_top_level_structure.
// test_update_status_paused -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_update_status_idle -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_update_status_stopped_sets_attention -> tray_model::tests::stopped_status_requests_attention.
// test_update_status_recording_uses_error_icon_when_error_set: retired; the Python error field has no production writer.
// test_update_sync_signals_label_change_only_once: retired-by-dependency; ksni owns property diffing.
// test_update_sync_synced -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_syncing -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_offline -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_update_needed_sets_attention -> tray_model::tests::update_needed_uses_live_attention_icon_and_attention.
// test_update_live_stats_updates_labels -> tray_model::tests::live_stats_and_tooltip_are_rendered_from_snapshot_and_health.
// test_update_live_stats_skips_unchanged_menu_updates: retired-by-dependency; ksni owns property diffing.
// test_update_live_stats_signals_resume_countdown_change_only_once: retired-by-dependency; ksni owns property diffing.
// test_update_header_emits_label_property_update: retired-by-dependency; ksni owns property diffing.
// test_header_recording_connected -> tray_model::tests::header_matrix_all_ten_typed_rows_byte_exact.
// test_header_paused_with_timer -> tray_model::tests::header_matrix_all_ten_typed_rows_byte_exact.
// test_header_recording_offline -> tray_model::tests::header_matrix_all_ten_typed_rows_byte_exact.
// test_compute_header_label -> tray_model::tests::header_matrix_all_ten_typed_rows_byte_exact; only the
//   "weird" row is retired because the typed TrayStatus contract eliminates it by construction.
// test_build_tooltip_default -> tray_model::tests::live_stats_and_tooltip_are_rendered_from_snapshot_and_health.
// test_build_tooltip_stopped -> tray_model::tests::stopped_and_syncing_tooltips_are_byte_exact.
// test_build_tooltip_error: retired; the Python error field has no production writer.
// test_build_tooltip_sync_progress -> tray_model::tests::stopped_and_syncing_tooltips_are_byte_exact.
// test_accessible_desc_properties: retired-by-dependency; ksni has no non-standard IconAccessibleDesc.
// test_on_about_to_show_forces_recompute: retired-by-dependency; ksni owns DBusMenu and exposes no
//   app-facing AboutToShow; the ~1s anchor-driven throttle recompute is the documented equivalent.
// test_about_to_show_returns_true_and_layout_has_refreshed_labels: retired-by-dependency; it is a
//   wire-level GetGroupProperties.__wrapped__ readback.
// test_on_about_to_show_failure_keeps_tray_and_last_known_layout -> desktop_component::tests::recompute_failure_keeps_task_alive_until_terminal_loss.
// test_first_update_clears_starting_tooltip: retired-by-construction; watch always contains an
//   initialized real snapshot, so "starting…" and the initial "on"/"segment: --:--"/"cache: --"/
//   "uptime: --" placeholders are unreachable. "sync: checking..." remains sourced
//   exclusively from SURFACE_BY_STATE.
// test_update_reads_observer_state -> tray_model::tests::live_stats_and_tooltip_are_rendered_from_snapshot_and_health.
// test_update_shows_paused -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_config_paths_use_base_dir -> desktop_component::tests::component_uses_config.
// test_agent_instructions_template_uses_config_values -> clipboard::tests::instructions_use_config_values.
