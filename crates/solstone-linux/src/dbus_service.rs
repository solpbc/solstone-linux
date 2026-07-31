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
    if duration_seconds > 0 {
        Some(duration_seconds as u64)
    } else {
        None
    }
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
        self.with_snapshot(|s| tray_model::status_name(tray_model::status(s)).into())
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
    pub(crate) async fn status_changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        status: &str,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    pub(crate) async fn sync_progress_changed(
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

// Python Observer1 provenance (25/25):
// TestObserverServiceStatus::test_status_recording -> dbus_service::tests::status_recording.
// TestObserverServiceStatus::test_status_idle -> dbus_service::tests::status_idle.
// TestObserverServiceStatus::test_status_paused -> dbus_service::tests::status_paused.
// TestPauseResume::test_pause_calls_observer -> dbus_service::tests::pause_calls_observer.
// TestPauseResume::test_pause_indefinite_calls_observer -> dbus_service::tests::pause_indefinite_calls_observer.
// TestPauseResume::test_resume_calls_observer -> dbus_service::tests::resume_calls_observer.
// TestAutoResume::test_auto_resume_expiry -> observer::tests::paused_finalize_saves_three_hits_clamps_and_timed_pause_resumes.
// TestSegmentTimerAndPauseRemaining::test_segment_timer_while_recording -> dbus_service::tests::segment_timer_while_recording.
// TestSegmentTimerAndPauseRemaining::test_segment_timer_zero_when_paused -> dbus_service::tests::segment_timer_zero_when_paused.
// TestSegmentTimerAndPauseRemaining::test_segment_timer_zero_when_no_segment -> dbus_service::tests::segment_timer_zero_when_no_segment.
// TestSegmentTimerAndPauseRemaining::test_pause_remaining_during_timed_pause -> dbus_service::tests::pause_remaining_during_timed_pause.
// TestSegmentTimerAndPauseRemaining::test_pause_remaining_zero_when_not_paused -> dbus_service::tests::pause_remaining_zero_when_not_paused.
// TestSegmentTimerAndPauseRemaining::test_pause_remaining_zero_for_indefinite_pause -> dbus_service::tests::pause_remaining_zero_for_indefinite_pause.
// TestComputeCaptureStats::test_returns_walk_counts -> capture_stats::tests::capture_walk_counts.
// TestComputeCaptureStats::test_empty_captures -> capture_stats::tests::capture_empty.
// TestGetStats::test_returns_cached_stats_dict -> dbus_service::tests::get_stats_returns_cached_shape.
// TestGetStats::test_empty_captures -> dbus_service::tests::get_stats_empty_captures.
// TestGetStats::test_uses_cached_today_count -> dbus_service::tests::get_stats_uses_cached_today_count.
// TestSyncStatusTracking::test_initial_status -> dbus_service::tests::fresh_sync_properties_are_not_reported_and_empty.
// TestSyncStatusTracking::test_progress_drives_syncing_status -> dbus_service::tests::in_progress_sync_properties_pass_through.
// TestSyncStatusTracking::test_progress_change_emits_signal -> desktop_component::tests::progress_change_emits_syncing_composite.
// TestObserverServiceConfig::test_capture_dir -> dbus_service::tests::config_properties_match_config.
// TestObserverServiceConfig::test_stream -> dbus_service::tests::config_properties_match_config.
// TestObserverServiceConfig::test_segment_interval -> dbus_service::tests::config_properties_match_config.

// Python introspection provenance (2/2):
// test_hyphenated_portal_property_names_parse_without_monkeypatch: retired-by-dependency;
//   it tests dbus-fast portal XML parsing, not Observer1.
// test_served_introspection_matches_legacy_baseline
//   -> dbus_service::tests::introspection_matches_authoritative_fixture for Observer1; SNI and
//      DBusMenu cases are retired-by-dependency because ksni owns those interfaces.
// Python DBusMenu provenance (8/8):
// test_default_emits_enabled_and_visible_true: retired-by-dependency; ksni owns DBusMenu properties.
// test_explicit_false_emits_false: retired-by-dependency; ksni owns DBusMenu properties.
// test_toggle_true_after_false_still_emits: retired-by-dependency; ksni owns DBusMenu properties.
// test_other_keys_still_conditional: retired-by-dependency; ksni owns DBusMenu properties.
// test_update_properties_emits_items_properties_updated: retired-by-dependency; ksni owns property diffing.
// test_update_properties_noop_when_no_names: retired-by-dependency; ksni owns property diffing.
// test_about_to_show_uses_optional_hook: retired-by-dependency; ksni exposes no AboutToShow hook.
// test_about_to_show_group_uses_optional_hook: retired-by-dependency; ksni exposes no AboutToShowGroup hook.

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use zbus::object_server::Interface;
    #[test]
    fn clamp_never_wraps() {
        assert_eq!(clamp_pause(-1), None);
        assert_eq!(clamp_pause(i32::MIN), None);
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
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum RecordedCommand {
        Pause(Option<u64>),
        Resume,
    }
    #[derive(Clone, Default)]
    struct Commands(Arc<Mutex<Vec<RecordedCommand>>>);
    impl ObserverCommands for Commands {
        fn pause(&self, duration: Option<u64>) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(RecordedCommand::Pause(duration));
        }
        fn resume(&self) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(RecordedCommand::Resume);
        }
    }
    fn snapshot(mode: crate::observer::Mode, paused: bool) -> StateSnapshot {
        StateSnapshot {
            mode,
            paused,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        }
    }
    fn not_reported_health() -> SyncHealth {
        crate::sync_health::derive_health(&Default::default(), 0.0, 600.0)
    }
    fn service(
        snapshot: StateSnapshot,
        config: Config,
        health: SyncHealth,
        progress: &str,
        now: u64,
    ) -> (
        Observer1<TestClock, Commands>,
        Commands,
        Arc<std::sync::atomic::AtomicU64>,
    ) {
        let commands = Commands::default();
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(now));
        (
            Observer1 {
                snapshot: Arc::new(Mutex::new(snapshot)),
                health: Arc::new(Mutex::new(health)),
                progress: Arc::new(Mutex::new(progress.into())),
                config,
                clock: TestClock(clock.clone()),
                commands: commands.clone(),
            },
            commands,
            clock,
        )
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
            commands: Commands::default(),
        };
        let mut xml = String::new();
        service.introspect_to_writer(&mut xml, 0);
        assert_eq!(
            normalized(&xml),
            normalized(include_str!("../testdata/introspection/observer1.xml"))
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
            commands: Commands::default(),
        };
        let first = service.segment_timer();
        now.store(101, std::sync::atomic::Ordering::Release);
        let second = service.segment_timer();
        assert!(second < first, "{second} must be less than {first}");
    }
    #[test]
    fn status_recording() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Screencast, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.current_status(), "recording");
    }
    #[test]
    fn status_idle() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.current_status(), "idle");
    }
    #[test]
    fn status_paused() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Screencast, true),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.current_status(), "paused");
    }
    #[test]
    fn pause_calls_observer() {
        let (s, c, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.pause(30), "ok");
        assert_eq!(
            *c.0.lock().unwrap_or_else(|e| e.into_inner()),
            [RecordedCommand::Pause(Some(30))]
        );
    }
    #[test]
    fn pause_indefinite_calls_observer() {
        let (s, c, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        s.pause(0);
        assert_eq!(
            *c.0.lock().unwrap_or_else(|e| e.into_inner()),
            [RecordedCommand::Pause(None)]
        );
    }
    #[test]
    fn resume_calls_observer() {
        let (s, c, _) = service(
            snapshot(crate::observer::Mode::Idle, true),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.resume(), "ok");
        assert_eq!(
            *c.0.lock().unwrap_or_else(|e| e.into_inner()),
            [RecordedCommand::Resume]
        );
    }
    #[test]
    fn segment_timer_while_recording() {
        let mut v = snapshot(crate::observer::Mode::Screencast, false);
        v.segment_open = true;
        v.segment_start_mono = Some(100.0);
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 160);
        assert_eq!(s.segment_timer(), 240);
    }
    #[test]
    fn segment_timer_zero_when_paused() {
        let mut v = snapshot(crate::observer::Mode::Screencast, true);
        v.segment_start_mono = Some(100.0);
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 160);
        assert_eq!(s.segment_timer(), 0);
    }
    #[test]
    fn segment_timer_zero_when_no_segment() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Screencast, false),
            Config::default(),
            not_reported_health(),
            "",
            160,
        );
        assert_eq!(s.segment_timer(), 0);
    }
    #[test]
    fn pause_remaining_during_timed_pause() {
        let mut v = snapshot(crate::observer::Mode::Idle, true);
        v.pause_until = Some(220.0);
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 100);
        assert_eq!(s.pause_remaining(), 120);
    }
    #[test]
    fn pause_remaining_zero_when_not_paused() {
        let mut v = snapshot(crate::observer::Mode::Idle, false);
        v.pause_until = Some(220.0);
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 100);
        assert_eq!(s.pause_remaining(), 0);
    }
    #[test]
    fn pause_remaining_zero_for_indefinite_pause() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, true),
            Config::default(),
            not_reported_health(),
            "",
            100,
        );
        assert_eq!(s.pause_remaining(), 0);
    }
    #[test]
    fn pause_remaining_decreases_between_property_reads() {
        let mut v = snapshot(crate::observer::Mode::Idle, true);
        v.pause_until = Some(220.0);
        let (s, _, clock) = service(v, Config::default(), not_reported_health(), "", 100);
        let first = s.pause_remaining();
        clock.store(101, std::sync::atomic::Ordering::Release);
        assert!(s.pause_remaining() < first);
    }
    #[test]
    fn get_stats_returns_cached_shape() {
        let mut v = snapshot(crate::observer::Mode::Idle, false);
        v.captures_today = 7;
        v.total_size_mb = 42;
        v.process_start_mono = 50.0;
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 100);
        let stats = s.get_stats();
        assert_eq!(stats.len(), 3);
        assert_eq!(
            stats.get("captures_today"),
            Some(&zbus::zvariant::OwnedValue::from(7i32))
        );
        assert_eq!(
            stats.get("total_size_mb"),
            Some(&zbus::zvariant::OwnedValue::from(42i32))
        );
        assert_eq!(
            stats.get("uptime_seconds"),
            Some(&zbus::zvariant::OwnedValue::from(50i32))
        );
    }
    #[test]
    fn get_stats_empty_captures() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        let stats = s.get_stats();
        assert_eq!(
            stats.get("captures_today"),
            Some(&zbus::zvariant::OwnedValue::from(0i32))
        );
        assert_eq!(
            stats.get("total_size_mb"),
            Some(&zbus::zvariant::OwnedValue::from(0i32))
        );
    }
    #[test]
    fn get_stats_uses_cached_today_count() {
        let mut v = snapshot(crate::observer::Mode::Idle, false);
        v.captures_today = 1;
        let (s, _, _) = service(v, Config::default(), not_reported_health(), "", 0);
        assert_eq!(
            s.get_stats().get("captures_today"),
            Some(&zbus::zvariant::OwnedValue::from(1i32))
        );
    }
    #[test]
    fn get_stats_uptime_increases_between_reads() {
        let mut v = snapshot(crate::observer::Mode::Idle, false);
        v.process_start_mono = 50.0;
        let (s, _, clock) = service(v, Config::default(), not_reported_health(), "", 100);
        let first = i32::try_from(
            s.get_stats()
                .remove("uptime_seconds")
                .unwrap_or_else(|| panic!("uptime missing")),
        )
        .unwrap_or_else(|e| panic!("invalid uptime: {e}"));
        clock.store(101, std::sync::atomic::Ordering::Release);
        let second = i32::try_from(
            s.get_stats()
                .remove("uptime_seconds")
                .unwrap_or_else(|| panic!("uptime missing")),
        )
        .unwrap_or_else(|e| panic!("invalid uptime: {e}"));
        assert!(second > first);
    }
    #[test]
    fn fresh_sync_properties_are_not_reported_and_empty() {
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.sync_status(), "not-reported");
        assert_eq!(s.current_sync_progress(), "");
    }
    #[test]
    fn in_progress_sync_properties_pass_through() {
        let h = crate::sync_health::derive_health(
            &crate::sync_health::SyncFacts {
                in_progress: true,
                progress: "uploading 120000_300".into(),
                link: Some(crate::private_link::LinkFactState {
                    observer_registered: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            0.0,
            600.0,
        );
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            Config::default(),
            h,
            "uploading 120000_300",
            0,
        );
        assert_eq!(s.sync_status(), "syncing");
        assert_eq!(s.current_sync_progress(), "uploading 120000_300");
    }
    #[test]
    fn config_properties_match_config() {
        let config = Config {
            base_dir: std::path::PathBuf::from("/tmp/observer1"),
            stream: "test-stream".into(),
            segment_interval: 300,
            ..Default::default()
        };
        let (s, _, _) = service(
            snapshot(crate::observer::Mode::Idle, false),
            config,
            not_reported_health(),
            "",
            0,
        );
        assert_eq!(s.capture_dir(), "/tmp/observer1/captures");
        assert_eq!(s.stream(), "test-stream");
        assert_eq!(s.segment_interval(), 300);
    }
}
