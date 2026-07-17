// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    config::Config,
    observer::{Clock, StateSnapshot},
    sync_health::SyncHealth,
    tray_model,
};
use std::sync::{Arc, Mutex};
use zbus::interface;

pub fn clamp_pause(duration_seconds: i32) -> Option<u64> {
    (duration_seconds > 0).then_some(duration_seconds as u64)
}

pub trait ObserverCommands: Send + Sync {
    fn pause(&self, duration: Option<u64>);
    fn resume(&self);
}

pub struct Observer1<C: Clock, O: ObserverCommands> {
    pub snapshot: Arc<Mutex<StateSnapshot>>,
    pub health: Arc<Mutex<SyncHealth>>,
    pub progress: Arc<Mutex<String>>,
    pub config: Config,
    pub clock: C,
    pub commands: O,
}
impl<C: Clock, O: ObserverCommands> Observer1<C, O> {
    fn with_snapshot<R>(&self, f: impl FnOnce(&StateSnapshot) -> R) -> R {
        match self.snapshot.lock() {
            Ok(s) => f(&s),
            Err(e) => f(&e.into_inner()),
        }
    }
    fn now(&self) -> f64 {
        self.clock.monotonic_seconds()
    }
}

#[interface(name = "org.solpbc.solstone.Observer1")]
impl<C: Clock + Send + Sync + 'static, O: ObserverCommands + 'static> Observer1<C, O> {
    #[zbus(property(emits_changed_signal = "false"), name = "Status")]
    fn current_status(&self) -> String {
        self.with_snapshot(|s| {
            match tray_model::status(s) {
                tray_model::TrayStatus::Recording => "recording",
                tray_model::TrayStatus::Idle => "idle",
                tray_model::TrayStatus::Paused => "paused",
                tray_model::TrayStatus::Stopped => "stopped",
            }
            .into()
        })
    }
    #[zbus(property)]
    fn sync_status(&self) -> String {
        match self.health.lock() {
            Ok(h) => h.dbus.clone(),
            Err(e) => e.into_inner().dbus.clone(),
        }
    }
    #[zbus(property(emits_changed_signal = "false"), name = "SyncProgress")]
    fn current_sync_progress(&self) -> String {
        match self.progress.lock() {
            Ok(p) => p.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }
    #[zbus(property)]
    fn capture_dir(&self) -> String {
        self.config.captures_dir().display().to_string()
    }
    #[zbus(property)]
    fn segment_timer(&self) -> i32 {
        let now = self.now();
        self.with_snapshot(|s| tray_model::segment_remaining(s, self.config.segment_interval, now))
    }
    #[zbus(property)]
    fn pause_remaining(&self) -> i32 {
        let now = self.now();
        self.with_snapshot(|s| tray_model::pause_remaining(s, now))
    }
    #[zbus(property)]
    fn error(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn server_url(&self) -> String {
        self.config.server_url.clone()
    }
    #[zbus(property)]
    fn stream(&self) -> String {
        self.config.stream.clone()
    }
    #[zbus(property)]
    fn segment_interval(&self) -> i32 {
        self.config.segment_interval as i32
    }
    fn pause(&self, duration_seconds: i32) -> String {
        self.commands.pause(clamp_pause(duration_seconds));
        "ok".into()
    }
    fn resume(&self) -> String {
        self.commands.resume();
        "ok".into()
    }
    fn get_stats(&self) -> std::collections::HashMap<String, zbus::zvariant::OwnedValue> {
        let now = self.now();
        self.with_snapshot(|s| {
            std::collections::HashMap::from([
                (
                    "captures_today".into(),
                    zbus::zvariant::OwnedValue::from(s.captures_today as i32),
                ),
                (
                    "total_size_mb".into(),
                    zbus::zvariant::OwnedValue::from(s.total_size_mb as i32),
                ),
                (
                    "uptime_seconds".into(),
                    zbus::zvariant::OwnedValue::from(tray_model::uptime(s, now)),
                ),
            ])
        })
    }
    #[zbus(signal)]
    async fn status_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        status: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn sync_progress_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        progress: &str,
    ) -> zbus::Result<()>;
    // Reference parity: declared for introspection, but the dead Error field has zero emit sites.
    #[zbus(signal)]
    async fn error_occurred(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        message: &str,
    ) -> zbus::Result<()>;
}

// Python Observer1 provenance (25/25): all ported.
// test_status_recording, test_status_idle, test_status_paused -> status property tests.
// test_pause_calls_observer, test_pause_indefinite_calls_observer, test_resume_calls_observer -> command/clamp tests.
// test_auto_resume_expiry -> observer auto-resume plus desktop signal-state tests.
// test_segment_timer_while_recording, test_segment_timer_zero_when_paused,
// test_segment_timer_zero_when_no_segment, test_pause_remaining_during_timed_pause,
// test_pause_remaining_zero_when_not_paused, test_pause_remaining_zero_for_indefinite_pause
//   -> read-time countdown tests.
// test_returns_walk_counts, test_empty_captures (capture-stats class), test_returns_cached_stats_dict,
// test_empty_captures (GetStats class), test_uses_cached_today_count -> GetStats/snapshot tests.
// test_initial_status, test_progress_drives_syncing_status, test_progress_change_emits_signal
//   -> sync properties and desktop_component::SignalState tests.
// test_capture_dir, test_server_url, test_stream, test_segment_interval -> config property tests.

// Python introspection provenance (2/2):
// test_hyphenated_portal_property_names_parse_without_monkeypatch: retired-by-dependency;
//   it tests dbus-fast portal XML parsing, not Observer1.
// test_served_introspection_matches_legacy_baseline: Observer1 ports below; SNI and DBusMenu
//   cases are retired-by-dependency because ksni owns those interfaces.
// Python DBusMenu provenance (8/8): test_default_emits_enabled_and_visible_true,
// test_explicit_false_emits_false, test_toggle_true_after_false_still_emits,
// test_other_keys_still_conditional, test_update_properties_emits_items_properties_updated,
// test_update_properties_noop_when_no_names, test_about_to_show_uses_optional_hook,
// test_about_to_show_group_uses_optional_hook are retired-by-dependency: ksni owns DBusMenu
// layout, property diffing, and wire behavior.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use zbus::object_server::Interface;
    #[test]
    fn clamp_never_wraps() {
        assert_eq!(clamp_pause(-1), None);
        assert_eq!(clamp_pause(0), None);
        assert_eq!(clamp_pause(1), Some(1));
        assert_eq!(clamp_pause(i32::MAX), Some(i32::MAX as u64));
    }
    #[derive(Clone)]
    struct TestClock(Arc<std::sync::atomic::AtomicU64>);
    impl Clock for TestClock {
        fn wall_seconds(&self) -> f64 {
            0.0
        }
        fn monotonic_seconds(&self) -> f64 {
            self.0.load(std::sync::atomic::Ordering::Acquire) as f64
        }
    }
    struct Commands;
    impl ObserverCommands for Commands {
        fn pause(&self, _: Option<u64>) {}
        fn resume(&self) {}
    }
    type Normalized = (
        BTreeMap<String, Vec<(String, String)>>,
        BTreeMap<String, Vec<(String, String)>>,
        BTreeMap<String, (String, String)>,
    );
    fn normalized(xml: &str) -> Normalized {
        let xml = xml
            .find("<node")
            .or_else(|| xml.find("<interface"))
            .map_or(xml, |start| &xml[start..]);
        let doc =
            roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("invalid test XML: {e}"));
        let iface = doc
            .descendants()
            .find(|n| {
                n.has_tag_name("interface")
                    && n.attribute("name") == Some("org.solpbc.solstone.Observer1")
            })
            .unwrap_or_else(|| panic!("Observer1 missing"));
        let args = |member: roxmltree::Node<'_, '_>, signal: bool| {
            member
                .children()
                .filter(|n| n.has_tag_name("arg"))
                .map(|a| {
                    let direction = a
                        .attribute("direction")
                        .unwrap_or(if signal { "out" } else { "" })
                        .to_owned();
                    (direction, a.attribute("type").unwrap_or("").to_owned())
                })
                .collect()
        };
        let methods = iface
            .children()
            .filter(|n| n.has_tag_name("method"))
            .map(|n| (n.attribute("name").unwrap_or("").to_owned(), args(n, false)))
            .collect();
        let signals = iface
            .children()
            .filter(|n| n.has_tag_name("signal"))
            .map(|n| (n.attribute("name").unwrap_or("").to_owned(), args(n, true)))
            .collect();
        let properties = iface
            .children()
            .filter(|n| n.has_tag_name("property"))
            .map(|n| {
                (
                    n.attribute("name").unwrap_or("").to_owned(),
                    (
                        n.attribute("type").unwrap_or("").to_owned(),
                        n.attribute("access").unwrap_or("").to_owned(),
                    ),
                )
            })
            .collect();
        (methods, signals, properties)
    }
    #[test]
    fn introspection_matches_authoritative_fixture() {
        let config = Config::default();
        let snapshot = StateSnapshot {
            mode: crate::observer::Mode::Idle,
            paused: false,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        };
        let health = crate::sync_health::derive_health(&Default::default(), 0.0, 600.0);
        let service = Observer1 {
            snapshot: Arc::new(Mutex::new(snapshot)),
            health: Arc::new(Mutex::new(health)),
            progress: Arc::new(Mutex::new(String::new())),
            config,
            clock: TestClock(Arc::new(std::sync::atomic::AtomicU64::new(0))),
            commands: Commands,
        };
        let mut xml = String::new();
        service.introspect_to_writer(&mut xml, 0);
        assert_eq!(
            normalized(&xml),
            normalized(include_str!(
                "../../../tests/fixtures/introspection/observer1.xml"
            ))
        );
    }
    #[test]
    fn segment_timer_decreases_between_property_reads() {
        let now = Arc::new(std::sync::atomic::AtomicU64::new(100));
        let snapshot = StateSnapshot {
            mode: crate::observer::Mode::Screencast,
            paused: false,
            segment_open: true,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: Some(100.0),
            process_start_mono: 50.0,
        };
        let service = Observer1 {
            snapshot: Arc::new(Mutex::new(snapshot)),
            health: Arc::new(Mutex::new(crate::sync_health::derive_health(
                &Default::default(),
                0.0,
                600.0,
            ))),
            progress: Arc::new(Mutex::new(String::new())),
            config: Config::default(),
            clock: TestClock(now.clone()),
            commands: Commands,
        };
        let first = service.segment_timer();
        now.store(101, std::sync::atomic::Ordering::Release);
        let second = service.segment_timer();
        assert!(second < first, "{second} must be less than {first}");
    }
}
