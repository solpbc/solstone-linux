// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::support::{
    ExpectedInvocation, FakeLookup, FixtureRunner, TestDirectory, output, write_executable,
};
use solstone_terminal::clock::{Clock, Zone, ZoneSource};
use solstone_terminal::command::{
    CommandInvocation, CommandOperation, DisplayField, TmuxOperation,
};
use solstone_terminal::config::CaptureSettings;
use solstone_terminal::foreground::ExeFailure;
use solstone_terminal::legacy::{HeldReason, open};
use solstone_terminal::tmux::TMUX_TIMEOUT;
use solstone_terminal::{EngineConfig, EngineDependencies, LayoutFn, start_with_dependencies};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// Waits, without letting time pass, until the engine has used every fixture
/// invocation; fails the test if it does not.
async fn wait_for_calls(runner: &FixtureRunner, count: usize) {
    for _ in 0..10_000 {
        if runner.calls().len() >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "the engine made {} of {count} fixture invocations",
        runner.calls().len()
    );
}

struct ScriptedClock {
    wall_reads: AtomicUsize,
    mono_reads: AtomicUsize,
    t0: OffsetDateTime,
    t_poll: OffsetDateTime,
    t_stop: OffsetDateTime,
    offset: UtcOffset,
}

impl ScriptedClock {
    fn new(t0: OffsetDateTime, offset: UtcOffset) -> Self {
        Self {
            wall_reads: AtomicUsize::new(0),
            mono_reads: AtomicUsize::new(0),
            t0,
            t_poll: t0 + Duration::from_secs(1),
            t_stop: t0 + Duration::from_secs(5),
            offset,
        }
    }
}

impl Clock for ScriptedClock {
    fn wall_now(&self) -> OffsetDateTime {
        let count = self.wall_reads.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            self.t0
        } else if count == 1 {
            self.t_poll
        } else {
            self.t_stop
        }
    }

    fn monotonic_now(&self) -> Duration {
        let count = self.mono_reads.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            Duration::ZERO
        } else if count == 1 {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        }
    }

    fn offset_at(&self, _instant: OffsetDateTime) -> UtcOffset {
        self.offset
    }
}

struct StaticZoneSource(Zone);

impl ZoneSource for StaticZoneSource {
    fn read(&mut self) -> Result<Zone, String> {
        Ok(self.0.clone())
    }
}

fn invocation(tmux: &Path, operation: TmuxOperation, args: &[&str]) -> CommandInvocation {
    CommandInvocation {
        operation: CommandOperation::Tmux(operation),
        executable: tmux.to_path_buf(),
        args: args.iter().map(OsString::from).collect(),
        timeout: TMUX_TIMEOUT,
    }
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst");
    for entry in fs::read_dir(src).expect("read src") {
        let entry = entry.expect("entry");
        let file_type = entry.file_type().expect("file type");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

fn assert_trees_byte_equal(actual: &Path, expected: &Path) {
    let mut actual_entries = fs::read_dir(actual)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", actual.display()))
        .map(|res| res.expect("entry").file_name())
        .collect::<Vec<_>>();
    let mut expected_entries = fs::read_dir(expected)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", expected.display()))
        .map(|res| res.expect("entry").file_name())
        .collect::<Vec<_>>();
    actual_entries.sort();
    expected_entries.sort();

    assert_eq!(
        actual_entries,
        expected_entries,
        "entry mismatch between actual {} and expected {}",
        actual.display(),
        expected.display()
    );

    for name in actual_entries {
        if name == ".solstone-tmux.lock" {
            assert!(actual.join(&name).is_file());
            assert!(expected.join(&name).is_file());
            continue;
        }
        let actual_path = actual.join(&name);
        let expected_path = expected.join(&name);
        let actual_meta = fs::symlink_metadata(&actual_path).expect("actual meta");
        let expected_meta = fs::symlink_metadata(&expected_path).expect("expected meta");
        assert_eq!(
            actual_meta.is_dir(),
            expected_meta.is_dir(),
            "type mismatch for {}",
            actual_path.display()
        );
        if actual_meta.is_dir() {
            assert_trees_byte_equal(&actual_path, &expected_path);
        } else {
            let actual_bytes = fs::read(&actual_path).expect("read actual");
            let expected_bytes = fs::read(&expected_path).expect("read expected");
            assert_eq!(
                actual_bytes,
                expected_bytes,
                "byte mismatch in file {}",
                actual_path.display()
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn engine_golden_populated_matches_both_layouts() {
    let denver_bytes =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/america-denver.tzif"))
            .expect("read denver tzif");
    let zone = Zone::from_tzif("America/Denver", &denver_bytes).expect("denver zone");

    let golden_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/goldens/engine/populated");

    for with_tmux_suffix in [true, false] {
        let temp = TestDirectory::new("golden-populated-engine");
        let tmux = temp.path().join("bin/tmux");
        write_executable(&tmux);
        let test_root = temp.path().join("root");
        fs::create_dir_all(&test_root).expect("create test_root");

        let layout: LayoutFn = if with_tmux_suffix {
            let root = test_root.clone();
            Arc::new(move |day: &str| root.join(day).join("_tmux"))
        } else {
            let root = test_root.clone();
            Arc::new(move |day: &str| root.join(day))
        };

        let t0 = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::July, 1).unwrap(),
            Time::from_hms(18, 0, 0).unwrap(),
        )
        .assume_utc();

        let clock = Arc::new(ScriptedClock::new(
            t0,
            UtcOffset::from_hms(-6, 0, 0).unwrap(),
        ));

        let t_poll = t0 + Duration::from_secs(1);
        let runner = Arc::new(FixtureRunner::new([
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListClients,
                    &["list-clients", "-F", "#{client_session} #{client_activity}"],
                ),
                outcome: output(format!("main {}\n", t_poll.unix_timestamp()).into_bytes()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListWindows("main".to_owned()),
                    &[
                        "list-windows",
                        "-t",
                        "main",
                        "-F",
                        "#{window_active} #{window_id} #{window_index} #{window_name}",
                    ],
                ),
                outcome: output(b"1 @0 0 active\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListPanes("@0".to_owned()),
                    &[
                        "list-panes",
                        "-t",
                        "@0",
                        "-F",
                        "#{pane_id} #{pane_index} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{pane_active} #{cursor_x} #{cursor_y} #{cursor_flag} #{alternate_on} #{pane_in_mode}",
                    ],
                ),
                outcome: output(b"%0 0 0 0 80 24 1 0 0 1 0 0\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::CapturePane("%0".to_owned()),
                    &["capture-pane", "-p", "-e", "-t", "%0"],
                ),
                outcome: output(b"hello terminal\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PaneTitle,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_title}"],
                ),
                outcome: output(b"editor\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PaneCurrentPath,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_current_path}"],
                ),
                outcome: output(b"/home/user/project\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PanePid,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_pid}"],
                ),
                outcome: output(b"12345\n".to_vec()),
            },
        ]));

        let pids = Arc::new(Mutex::new(Vec::new()));
        let fake_lookup = Arc::new(FakeLookup::new(pids, Ok("vim".to_owned())));

        let config = EngineConfig {
            tmux_path: tmux.clone(),
            root: test_root.clone(),
            lock_path: temp.path().join("lock"),
            layout,
            settings: CaptureSettings {
                capture_interval: 5,
                segment_interval: 300,
                pane_title: true,
                working_directory: true,
                executable: true,
                status_indicator: false,
                output_sessions: Vec::new(),
            },
            closed_segments: Arc::new(|_| {}),
            diagnostics: Arc::new(|_| {}),
            health: Arc::new(|_| {}),
        };

        let deps = EngineDependencies {
            runner: runner.clone(),
            clock,
            zone_source: Box::new(StaticZoneSource(zone.clone())),
            lookup: Some(fake_lookup),
        };

        let handle = start_with_dependencies(config, deps)
            .await
            .expect("start engine");

        // The first tick polls once; time never reaches the next.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_for_calls(&runner, 7).await;
        runner.assert_finished().expect("all runner calls used");
        handle.stop().await.expect("stop engine");

        let stream_dir = if with_tmux_suffix {
            test_root.join("20260701/_tmux")
        } else {
            test_root.join("20260701")
        };

        assert_trees_byte_equal(&stream_dir, &golden_dir);
    }
}

#[tokio::test(start_paused = true)]
async fn engine_golden_availability_matches_both_layouts() {
    let zone = Zone::utc();
    let golden_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/goldens/engine/availability");

    for with_tmux_suffix in [true, false] {
        let temp = TestDirectory::new("golden-availability-engine");
        let tmux = temp.path().join("bin/tmux");
        write_executable(&tmux);
        let test_root = temp.path().join("root");
        fs::create_dir_all(&test_root).expect("create test_root");

        let layout: LayoutFn = if with_tmux_suffix {
            let root = test_root.clone();
            Arc::new(move |day: &str| root.join(day).join("_tmux"))
        } else {
            let root = test_root.clone();
            Arc::new(move |day: &str| root.join(day))
        };

        let t0 = PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::July, 1).unwrap(),
            Time::from_hms(18, 0, 0).unwrap(),
        )
        .assume_utc();

        let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));

        let t_poll = t0 + Duration::from_secs(1);
        let runner = Arc::new(FixtureRunner::new([
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListClients,
                    &["list-clients", "-F", "#{client_session} #{client_activity}"],
                ),
                outcome: output(format!("main {}\n", t_poll.unix_timestamp()).into_bytes()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListWindows("main".to_owned()),
                    &[
                        "list-windows",
                        "-t",
                        "main",
                        "-F",
                        "#{window_active} #{window_id} #{window_index} #{window_name}",
                    ],
                ),
                outcome: output(b"1 @0 0 active\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::ListPanes("@0".to_owned()),
                    &[
                        "list-panes",
                        "-t",
                        "@0",
                        "-F",
                        "#{pane_id} #{pane_index} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{pane_active} #{cursor_x} #{cursor_y} #{cursor_flag} #{alternate_on} #{pane_in_mode}",
                    ],
                ),
                outcome: output(b"%0 0 0 0 80 24 1 0 0 1 0 0\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::CapturePane("%0".to_owned()),
                    &["capture-pane", "-p", "-e", "-t", "%0"],
                ),
                outcome: output(b"hello terminal\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PaneTitle,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_title}"],
                ),
                outcome: output(b"editor\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PaneCurrentPath,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_current_path}"],
                ),
                outcome: output(b"/home/user/project\n".to_vec()),
            },
            ExpectedInvocation {
                invocation: invocation(
                    &tmux,
                    TmuxOperation::DisplayMessage {
                        target: "%0".to_owned(),
                        field: DisplayField::PanePid,
                    },
                    &["display-message", "-p", "-t", "%0", "#{pane_pid}"],
                ),
                outcome: output(b"12345\n".to_vec()),
            },
        ]));

        let pids = Arc::new(Mutex::new(Vec::new()));
        let fake_lookup = Arc::new(FakeLookup::new(pids, Err(ExeFailure::Unavailable)));

        let config = EngineConfig {
            tmux_path: tmux.clone(),
            root: test_root.clone(),
            lock_path: temp.path().join("lock"),
            layout,
            settings: CaptureSettings {
                capture_interval: 5,
                segment_interval: 300,
                pane_title: true,
                working_directory: true,
                executable: true,
                status_indicator: false,
                output_sessions: Vec::new(),
            },
            closed_segments: Arc::new(|_| {}),
            diagnostics: Arc::new(|_| {}),
            health: Arc::new(|_| {}),
        };

        let deps = EngineDependencies {
            runner: runner.clone(),
            clock,
            zone_source: Box::new(StaticZoneSource(zone.clone())),
            lookup: Some(fake_lookup),
        };

        let handle = start_with_dependencies(config, deps)
            .await
            .expect("start engine");

        // The first tick polls once; time never reaches the next.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_for_calls(&runner, 7).await;
        runner.assert_finished().expect("all runner calls used");
        handle.stop().await.expect("stop engine");

        let stream_dir = if with_tmux_suffix {
            test_root.join("20260701/_tmux")
        } else {
            test_root.join("20260701")
        };

        assert_trees_byte_equal(&stream_dir, &golden_dir);
    }
}

#[test]
fn legacy_recover_and_segments_match_unmodified_golden_tree() {
    let golden_pre =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/goldens/legacy/pre-recovery");
    let golden_rec =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/goldens/legacy/recovered");

    let temp = TestDirectory::new("golden-legacy-recovery");
    let data_root = temp.path().join("data");
    copy_dir_all(&golden_pre, &data_root);

    let guard = open(&data_root).expect("open legacy root");
    let recovery = guard.recover(300).expect("recover legacy");

    assert!(
        recovery.problems.is_empty() && recovery.held.is_empty(),
        "unexpected recovery outcome: {recovery:?}"
    );

    // Byte-compare entire data directory to recovered golden tree
    assert_trees_byte_equal(&data_root, &golden_rec);

    let mut listing = guard.segments().expect("legacy segments listing");
    listing.segments.sort_by(|a, b| a.segment.cmp(&b.segment));
    listing.held.sort_by(|a, b| a.segment.cmp(&b.segment));

    assert_eq!(
        listing.segments.len(),
        4,
        "expected exactly 4 deliverable segments"
    );
    assert_eq!(listing.held.len(), 2, "expected exactly 2 held directories");

    // Check deliverable segments
    // 1. 120000_300
    assert_eq!(listing.segments[0].segment, "120000_300");
    let zone0 = listing.segments[0].zone.as_ref().expect("seg0 zone");
    assert_eq!(zone0.tz.as_deref(), Some("America/Denver"));
    assert_eq!(zone0.utc_offset_seconds, -21600);

    // 2. 120500_1200
    assert_eq!(listing.segments[1].segment, "120500_1200");
    let zone1 = listing.segments[1].zone.as_ref().expect("seg1 zone");
    assert_eq!(zone1.tz.as_deref(), Some("America/Denver"));
    assert_eq!(zone1.utc_offset_seconds, -21600);

    // 3. 130000_300 (no zone file)
    assert_eq!(listing.segments[2].segment, "130000_300");
    assert!(
        listing.segments[2].zone.is_none(),
        "zone-less segment must have zone null"
    );

    // 4. 164204_001 (recovered from incomplete)
    assert_eq!(listing.segments[3].segment, "164204_001");
    let zone3 = listing.segments[3].zone.as_ref().expect("seg3 zone");
    assert_eq!(zone3.tz.as_deref(), Some("Etc/UTC"));
    assert_eq!(zone3.utc_offset_seconds, 0);

    // Check held entries
    assert_eq!(listing.held[0].segment, "122500_300.failed");
    assert_eq!(listing.held[0].reason, HeldReason::Failed);

    assert_eq!(listing.held[1].segment, "132500_300.1.failed");
    assert_eq!(listing.held[1].reason, HeldReason::Failed);

    // Assert that no .meta files are in held
    for held in &listing.held {
        assert!(
            !held.segment.ends_with(".meta"),
            ".meta files must not be listed as held"
        );
    }
}
