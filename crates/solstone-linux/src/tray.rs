// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::tray_model::TrayModel;
use ksni::menu::{MenuItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip, Tray};
use std::sync::mpsc::Sender;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/tray_icons.rs"));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCommand {
    Pause(u64),
    PauseIndefinite,
    Resume,
    OpenJournal,
    OpenUrl(&'static str),
    OpenConfig,
    CopyInstructions,
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
fn action(label: &str, command: TrayCommand) -> StandardItem<KsniTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |tray| {
            let _ = tray.commands.send(command);
        }),
        ..Default::default()
    }
}

impl Tray for KsniTray {
    fn id(&self) -> String {
        "solstone-observer".into()
    }
    fn category(&self) -> Category {
        Category::SystemServices
    }
    fn title(&self) -> String {
        "sol".into()
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
            "error" | "stopped" => generated::ERROR,
            "syncing" => generated::SYNCING,
            "paused" | "idle" => generated::PAUSED,
            _ => generated::RECORDING,
        };
        vec![Icon {
            width: 64,
            height: 64,
            data: data.to_vec(),
        }]
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "sol".into(),
            description: self.model.tooltip.clone(),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let pause = SubMenu {
            label: "pause".into(),
            visible: self.model.pause_visible,
            submenu: vec![
                action("15 minutes", TrayCommand::Pause(900)).into(),
                action("30 minutes", TrayCommand::Pause(1800)).into(),
                action("1 hour", TrayCommand::Pause(3600)).into(),
                action("until I resume", TrayCommand::PauseIndefinite).into(),
            ],
            ..Default::default()
        };
        let status = SubMenu {
            label: "status".into(),
            submenu: vec![
                item(self.model.header.clone(), false),
                item(self.model.sync.clone(), false),
                MenuItem::Separator,
                item(self.model.segment.clone(), false),
                item(self.model.cache.clone(), false),
                item(self.model.captures.clone(), false),
                item(self.model.uptime.clone(), false),
            ],
            ..Default::default()
        };
        vec![
            item(self.model.header.clone(), false),
            MenuItem::Separator,
            pause.into(),
            StandardItem {
                label: self.model.resume.clone(),
                visible: self.model.resume_visible,
                activate: Box::new(|t: &mut KsniTray| {
                    let _ = t.commands.send(TrayCommand::Resume);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            status.into(),
            StandardItem {
                label: "open journal".into(),
                enabled: self.model.open_journal_enabled,
                activate: Box::new(|tray: &mut KsniTray| {
                    let _ = tray.commands.send(TrayCommand::OpenJournal);
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "settings".into(),
                submenu: vec![action("open config.json", TrayCommand::OpenConfig).into()],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "about".into(),
                submenu: vec![
                    item(format!("sol v{}", env!("CARGO_PKG_VERSION")), false),
                    action(
                        "solstone.app",
                        TrayCommand::OpenUrl("https://solstone.app/observers"),
                    )
                    .into(),
                    action(
                        "source code",
                        TrayCommand::OpenUrl("https://github.com/solpbc/solstone-linux"),
                    )
                    .into(),
                    action(
                        "privacy policy",
                        TrayCommand::OpenUrl("https://solpbc.org/privacy"),
                    )
                    .into(),
                    action(
                        "copy help agent instructions",
                        TrayCommand::CopyInstructions,
                    )
                    .into(),
                    MenuItem::Separator,
                    item("© 2026 sol pbc — a public benefit corporation", false),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            item("managed via systemctl", false),
        ]
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
        observer::{Mode, StateSnapshot},
        sync_health::{SyncFacts, derive_health},
        tray_model,
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
    fn menu_contains_reference_top_level_structure() {
        let mut tray = tray();
        tray.model.open_journal_enabled = true;
        let menu = tray.menu();
        assert_eq!(menu.len(), 11);
        let labels: Vec<&str> = menu
            .iter()
            .map(|item| match item {
                MenuItem::Standard(v) => v.label.as_str(),
                MenuItem::SubMenu(v) => v.label.as_str(),
                MenuItem::Separator => "<separator>",
                _ => "<other>",
            })
            .collect();
        assert_eq!(
            labels,
            [
                tray.model.header.as_str(),
                "<separator>",
                "pause",
                "resume",
                "<separator>",
                "status",
                "open journal",
                "settings",
                "about",
                "<separator>",
                "managed via systemctl"
            ]
        );
        let status = match &menu[5] {
            MenuItem::SubMenu(value) => value,
            _ => panic!("status submenu missing"),
        };
        assert_eq!(status.submenu.len(), 7);
        assert_eq!(
            match &status.submenu[0] {
                MenuItem::Standard(value) => value.label.as_str(),
                _ => "",
            },
            tray.model.header
        );
        let pause = match &menu[2] {
            MenuItem::SubMenu(value) => value,
            _ => panic!("pause submenu missing"),
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
        if let MenuItem::Standard(value) = &menu[6] {
            assert!(value.enabled);
            (value.activate)(&mut tray);
        }
        assert_eq!(receiver.try_recv(), Ok(TrayCommand::OpenJournal));
        let about = match &menu[8] {
            MenuItem::SubMenu(value) => value,
            _ => panic!("about submenu missing"),
        };
        let expected = [
            TrayCommand::OpenUrl("https://solstone.app/observers"),
            TrayCommand::OpenUrl("https://github.com/solpbc/solstone-linux"),
            TrayCommand::OpenUrl("https://solpbc.org/privacy"),
        ];
        for (item, expected) in about.submenu[1..4].iter().zip(expected) {
            if let MenuItem::Standard(value) = item {
                (value.activate)(&mut tray);
            }
            assert_eq!(receiver.try_recv(), Ok(expected));
        }
    }

    #[test]
    fn open_journal_menu_enablement_follows_model_epoch() {
        let mut tray = tray();
        for expected in [false, true, false] {
            tray.model.open_journal_enabled = expected;
            let menu = tray.menu();
            let MenuItem::Standard(open_journal) = &menu[6] else {
                panic!("open journal item missing");
            };
            assert_eq!(open_journal.enabled, expected);
        }
    }
}

// Python tray provenance (35/35):
// test_resolve_icon_theme_path_prefers_installed: retired-by-dependency; build-generated pixmaps replace icon-theme filesystem lookup.
// test_resolve_icon_theme_path_contrib_fallback: retired-by-dependency; build-generated pixmaps replace icon-theme filesystem lookup.
// test_make_app_uses_observer_config: retired-by-wiring; Rust components are constructed separately until the sibling run-loop lode.
// test_build_menu_creates_expected_items -> tray::tests::menu_contains_reference_top_level_structure.
// test_update_status_paused -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_update_status_idle -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_update_status_stopped_sets_attention -> tray_model::tests::stopped_status_requests_attention.
// test_update_status_recording_uses_error_icon_when_error_set: retired; the Python error field has no production writer.
// test_update_sync_signals_label_change_only_once: retired-by-dependency; ksni owns property diffing.
// test_update_sync_synced -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_syncing -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_offline -> tray_model::tests::sync_labels_follow_resolved_health_surfaces.
// test_update_sync_update_needed_sets_attention -> tray_model::tests::update_needed_uses_live_error_icon_and_attention.
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
//   "today: --"/"uptime: --" placeholders are unreachable. "sync: checking..." remains sourced
//   exclusively from SURFACE_BY_STATE.
// test_update_reads_observer_state -> tray_model::tests::live_stats_and_tooltip_are_rendered_from_snapshot_and_health.
// test_update_shows_paused -> tray_model::tests::paused_and_idle_snapshots_select_typed_status.
// test_config_paths_use_base_dir -> desktop_component::tests::component_uses_config.
// test_agent_instructions_template_uses_config_values -> clipboard::tests::instructions_use_config_values.
