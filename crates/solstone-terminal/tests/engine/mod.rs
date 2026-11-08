// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::ffi::OsString;
use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::support::{
    EngineHarness, ExpectedInvocation, FakeLookup, FakeTmux, FixtureRunner, OWNER_STATUS_LEFT,
    TestDirectory, nonzero, output, owner_options, permissions_are_enforced, set_mode, settle,
    write_executable,
};
use solstone_terminal::clock::{Clock, Zone, ZoneSource};
use solstone_terminal::command::{
    CommandInvocation, CommandOperation, DisplayField, TmuxOperation,
};
use solstone_terminal::config::CaptureSettings;
use solstone_terminal::indicator::{
    INDICATOR_PREFIX, OBSERVING_VALUE, SOLSTONE_OPTION, STATUS_LEFT, STATUS_LEFT_LENGTH,
};
use solstone_terminal::instance_lock::InstanceLock;
use solstone_terminal::storage::SegmentMetadata;
use solstone_terminal::tmux::TMUX_TIMEOUT;
use solstone_terminal::{
    ClosedSegment, Diagnostic, EngineConfig, EngineDependencies, EngineError, SyncActivity,
    TmuxHealth, start_with_dependencies,
};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

struct ScriptedClock {
    base_wall: OffsetDateTime,
    offset: UtcOffset,
    current_secs: AtomicU64,
}

impl ScriptedClock {
    fn new(base_wall: OffsetDateTime, offset: UtcOffset) -> Self {
        Self {
            base_wall,
            offset,
            current_secs: AtomicU64::new(0),
        }
    }

    fn advance(&self, secs: u64) {
        self.current_secs.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for ScriptedClock {
    fn wall_now(&self) -> OffsetDateTime {
        let secs = self.current_secs.load(Ordering::SeqCst);
        self.base_wall + Duration::from_secs(secs)
    }

    fn monotonic_now(&self) -> Duration {
        let secs = self.current_secs.load(Ordering::SeqCst);
        Duration::from_secs(secs)
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

fn invocation(executable: &Path, operation: TmuxOperation, args: &[&str]) -> CommandInvocation {
    CommandInvocation {
        operation: CommandOperation::Tmux(operation),
        executable: executable.to_path_buf(),
        args: args.iter().map(OsString::from).collect(),
        timeout: TMUX_TIMEOUT,
    }
}

fn create_executable(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directory for executable");
    }
    fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write executable fixture");
    fs::set_permissions(path, Permissions::from_mode(0o755)).expect("chmod executable");
}

fn setup_incomplete_recovery_fixture(stream_dir: &Path, stem: &str) {
    let incomplete_dir = stream_dir.join(format!("{stem}.incomplete"));
    fs::create_dir_all(&incomplete_dir).expect("create incomplete dir");

    let screen_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/recovery/2.0.16/tmux_synthetic_screen.jsonl");
    let meta_fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/recovery/2.0.16/164204.incomplete.meta");

    fs::copy(
        &screen_fixture,
        incomplete_dir.join("tmux_synthetic_screen.jsonl"),
    )
    .expect("copy screen fixture");

    let meta_bytes = fs::read(&meta_fixture).expect("read meta fixture");
    let mut metadata: SegmentMetadata =
        serde_json::from_slice(&meta_bytes).expect("parse metadata");
    metadata.incomplete_dir = format!("{stem}.incomplete");
    metadata.finalized_dir = format!("{stem}_001");

    let updated_meta = serde_json::to_vec(&metadata).expect("serialize updated metadata");
    fs::write(
        stream_dir.join(format!("{stem}.incomplete.meta")),
        updated_meta,
    )
    .expect("write updated meta");
}

fn standard_poll_invocations(
    tmux: &Path,
    now_unix: i64,
    content: &[u8],
) -> Vec<ExpectedInvocation> {
    vec![
        ExpectedInvocation {
            invocation: invocation(
                tmux,
                TmuxOperation::ListClients,
                &["list-clients", "-F", "#{client_session} #{client_activity}"],
            ),
            outcome: output(format!("main {now_unix}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                tmux,
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
                tmux,
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
                tmux,
                TmuxOperation::CapturePane("%0".to_owned()),
                &["capture-pane", "-p", "-e", "-t", "%0"],
            ),
            outcome: output(content.to_vec()),
        },
        ExpectedInvocation {
            invocation: invocation(
                tmux,
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
                tmux,
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
                tmux,
                TmuxOperation::DisplayMessage {
                    target: "%0".to_owned(),
                    field: DisplayField::PanePid,
                },
                &["display-message", "-p", "-t", "%0", "#{pane_pid}"],
            ),
            outcome: output(b"12345\n".to_vec()),
        },
    ]
}

#[tokio::test]
async fn prefix_already_present_widens_once_and_restores() {
    tokio::time::pause();
    let temp = TestDirectory::new("prefix-already-present");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let initial_status_left = format!("{INDICATOR_PREFIX}#[fg=green]#S#[default]");
    let owner_status_left = "#[fg=green]#S#[default]";
    let original_length = "40";
    let widened_length = "41";

    let runner = Arc::new(FixtureRunner::new([
        // Startup indicator installation
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ServerIdentity,
                &["display-message", "-p", "#{pid} #{start_time}"],
            ),
            outcome: output(b"1234 1000\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{initial_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{widened_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: output(b"observing\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &["set-option", "-g", STATUS_LEFT, &initial_status_left],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, widened_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-g", SOLSTONE_OPTION, OBSERVING_VALUE],
            ),
            outcome: output(b""),
        },
        // Teardown restore upon stop
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{initial_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &["set-option", "-g", STATUS_LEFT, owner_status_left],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{widened_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, original_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: output(b"observing\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::UnsetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-gu", SOLSTONE_OPTION],
            ),
            outcome: output(b""),
        },
    ]));

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
        settings: CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            pane_title: true,
            working_directory: true,
            executable: true,
            status_indicator: true,
            output_sessions: Vec::new(),
        },
        closed_segments: Arc::new(|_| {}),
        diagnostics: Arc::new(|_| {}),
        health: Arc::new(|_| {}),
    };

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    // Yield so spawned engine loop can run startup indicator install
    tokio::task::yield_now().await;

    // Verify after start: status-left has exactly one prefix, and length was widened once to 41
    let calls_after_start = runner.calls();
    let written_status_left = calls_after_start
        .iter()
        .find(|c| {
            c.operation == CommandOperation::Tmux(TmuxOperation::SetOption(STATUS_LEFT.to_owned()))
        })
        .expect("status-left was set on start");
    assert_eq!(
        written_status_left.args[3].to_string_lossy(),
        format!("{INDICATOR_PREFIX}{owner_status_left}")
    );
    // Ensure prefix is not duplicated
    assert_eq!(
        written_status_left.args[3]
            .to_string_lossy()
            .matches(INDICATOR_PREFIX)
            .count(),
        1
    );

    let written_length = calls_after_start
        .iter()
        .find(|c| {
            c.operation
                == CommandOperation::Tmux(TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()))
        })
        .expect("status-left-length was set on start");
    assert_eq!(written_length.args[3], widened_length);

    handle.stop().await.expect("stop engine");

    // Verify after stop: owner's original status-left and length are restored
    let all_calls = runner.calls();
    let restored_status = all_calls
        .iter()
        .rev()
        .find(|c| {
            c.operation == CommandOperation::Tmux(TmuxOperation::SetOption(STATUS_LEFT.to_owned()))
        })
        .expect("status-left was restored on stop");
    assert_eq!(restored_status.args[3], owner_status_left);

    let restored_length = all_calls
        .iter()
        .rev()
        .find(|c| {
            c.operation
                == CommandOperation::Tmux(TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()))
        })
        .expect("status-left-length was restored on stop");
    assert_eq!(restored_length.args[3], original_length);

    runner
        .assert_finished()
        .expect("all runner invocations matched");
}

#[tokio::test]
async fn prefix_absent_is_installed_and_restored() {
    tokio::time::pause();
    let temp = TestDirectory::new("prefix-absent");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let owner_status_left = "#[fg=blue]#S#[default]";
    let original_length = "40";
    let widened_length = "41";

    let runner = Arc::new(FixtureRunner::new([
        // Startup indicator installation
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ServerIdentity,
                &["display-message", "-p", "#{pid} #{start_time}"],
            ),
            outcome: output(b"1234 1000\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{original_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: nonzero(1),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &[
                    "set-option",
                    "-g",
                    STATUS_LEFT,
                    &format!("{INDICATOR_PREFIX}{owner_status_left}"),
                ],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, widened_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-g", SOLSTONE_OPTION, OBSERVING_VALUE],
            ),
            outcome: output(b""),
        },
        // Teardown restore upon stop
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{INDICATOR_PREFIX}{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &["set-option", "-g", STATUS_LEFT, owner_status_left],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{widened_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, original_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: output(b"observing\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::UnsetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-gu", SOLSTONE_OPTION],
            ),
            outcome: output(b""),
        },
    ]));

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
        settings: CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            pane_title: true,
            working_directory: true,
            executable: true,
            status_indicator: true,
            output_sessions: Vec::new(),
        },
        closed_segments: Arc::new(|_| {}),
        diagnostics: Arc::new(|_| {}),
        health: Arc::new(|_| {}),
    };

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    handle.stop().await.expect("stop engine");

    runner
        .assert_finished()
        .expect("all runner invocations completed");
}

#[tokio::test]
async fn status_indicator_off_writes_no_options() {
    tokio::time::pause();
    let temp = TestDirectory::new("indicator-off");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let runner = Arc::new(FixtureRunner::new([ExpectedInvocation {
        invocation: invocation(
            &tmux,
            TmuxOperation::ListClients,
            &["list-clients", "-F", "#{client_session} #{client_activity}"],
        ),
        outcome: output(b""),
    }]));

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
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

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    tokio::time::advance(Duration::from_secs(5)).await;
    handle.stop().await.expect("stop engine");

    for call in runner.calls() {
        assert!(
            !matches!(
                call.operation,
                CommandOperation::Tmux(
                    TmuxOperation::ShowOption(_)
                        | TmuxOperation::SetOption(_)
                        | TmuxOperation::UnsetOption(_)
                        | TmuxOperation::ServerIdentity
                )
            ),
            "no option or identity commands should be run when status_indicator is false, got: {call:?}"
        );
        for arg in &call.args {
            let s = arg.to_string_lossy();
            assert!(!s.contains("status-left"));
            assert!(!s.contains("status-left-length"));
            assert!(!s.contains("@solstone"));
        }
    }
}

#[tokio::test]
async fn tmux_missing_at_start_then_found_installs_once_without_restart() {
    tokio::time::pause();
    let temp = TestDirectory::new("tmux-missing-at-start");
    let tmux = temp.path().join("bin/tmux");
    assert!(!tmux.exists());

    let health_events = Arc::new(Mutex::new(Vec::new()));
    let health_sink = {
        let events = health_events.clone();
        Arc::new(move |h: TmuxHealth| {
            events.lock().unwrap().push(h);
        })
    };

    let owner_status_left = "#[fg=yellow]#S#[default]";
    let original_length = "40";
    let widened_length = "41";

    let runner = Arc::new(FixtureRunner::default());

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path: lock_path.clone(),
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
        settings: CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            pane_title: true,
            working_directory: true,
            executable: true,
            status_indicator: true,
            output_sessions: Vec::new(),
        },
        closed_segments: Arc::new(|_| {}),
        diagnostics: Arc::new(|_| {}),
        health: health_sink,
    };

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    // Initial state: missing, lock held
    assert_eq!(*health_events.lock().unwrap(), vec![TmuxHealth::Missing]);
    assert!(
        InstanceLock::acquire_path(&lock_path).is_err(),
        "lock must be held while missing"
    );

    // Yield so background task enters loop and completes t=0 tick
    tokio::task::yield_now().await;

    // Queue indicator installation + 1 empty poll
    runner.extend([
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ServerIdentity,
                &["display-message", "-p", "#{pid} #{start_time}"],
            ),
            outcome: output(b"1234 1000\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{original_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: nonzero(1),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &[
                    "set-option",
                    "-g",
                    STATUS_LEFT,
                    &format!("{INDICATOR_PREFIX}{owner_status_left}"),
                ],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, widened_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-g", SOLSTONE_OPTION, OBSERVING_VALUE],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ListClients,
                &["list-clients", "-F", "#{client_session} #{client_activity}"],
            ),
            outcome: output(b""),
        },
    ]);

    // Make tmux present and executable
    create_executable(&tmux);
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;

    assert_eq!(
        *health_events.lock().unwrap(),
        vec![TmuxHealth::Missing, TmuxHealth::Found]
    );

    // Remove the file mid-run -> health changes to Missing
    fs::remove_file(&tmux).expect("remove tmux mid-run");
    tokio::time::advance(Duration::from_millis(5001)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    assert_eq!(
        *health_events.lock().unwrap(),
        vec![TmuxHealth::Missing, TmuxHealth::Found, TmuxHealth::Missing]
    );

    // Restore the file mid-run -> health changes to Found
    create_executable(&tmux);
    runner.push(ExpectedInvocation {
        invocation: invocation(
            &tmux,
            TmuxOperation::ServerIdentity,
            &["display-message", "-p", "#{pid} #{start_time}"],
        ),
        outcome: output(b"1234 1000\n"),
    });
    runner.push(ExpectedInvocation {
        invocation: invocation(
            &tmux,
            TmuxOperation::ListClients,
            &["list-clients", "-F", "#{client_session} #{client_activity}"],
        ),
        outcome: output(b""),
    });
    runner.push(ExpectedInvocation {
        invocation: invocation(
            &tmux,
            TmuxOperation::ServerIdentity,
            &["display-message", "-p", "#{pid} #{start_time}"],
        ),
        outcome: output(b"1234 1000\n"),
    });

    // Queue restore invocations for stop
    runner.extend([
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{INDICATOR_PREFIX}{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &["set-option", "-g", STATUS_LEFT, owner_status_left],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{widened_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, original_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: output(b"observing\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::UnsetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-gu", SOLSTONE_OPTION],
            ),
            outcome: output(b""),
        },
    ]);

    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;

    assert_eq!(
        *health_events.lock().unwrap(),
        vec![
            TmuxHealth::Missing,
            TmuxHealth::Found,
            TmuxHealth::Missing,
            TmuxHealth::Found
        ]
    );

    handle.stop().await.expect("stop engine");
    runner.assert_finished().expect("all invocations consumed");
}

#[tokio::test]
async fn exactly_one_found_at_start() {
    tokio::time::pause();
    let temp = TestDirectory::new("exactly-one-found");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let health_events = Arc::new(Mutex::new(Vec::new()));
    let health_sink = {
        let events = health_events.clone();
        Arc::new(move |h: TmuxHealth| {
            events.lock().unwrap().push(h);
        })
    };

    let runner = Arc::new(FixtureRunner::new([
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ListClients,
                &["list-clients", "-F", "#{client_session} #{client_activity}"],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ListClients,
                &["list-clients", "-F", "#{client_session} #{client_activity}"],
            ),
            outcome: output(b""),
        },
    ]));

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
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
        health: health_sink,
    };

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    assert_eq!(*health_events.lock().unwrap(), vec![TmuxHealth::Found]);

    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(5)).await;

    // Emitted once at start and on change only: still exactly one Found
    assert_eq!(*health_events.lock().unwrap(), vec![TmuxHealth::Found]);

    handle.stop().await.expect("stop engine");
}

#[tokio::test]
async fn tmux_mode_0644_is_missing() {
    let temp = TestDirectory::new("tmux-mode-0644");
    let tmux = temp.path().join("bin/tmux");
    fs::create_dir_all(tmux.parent().unwrap()).unwrap();
    fs::write(&tmux, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&tmux, Permissions::from_mode(0o644)).unwrap();

    let health = solstone_terminal::check_tmux_health(&tmux);
    assert_eq!(health, TmuxHealth::Missing);
}

#[tokio::test]
async fn stop_while_missing_releases_the_lock() {
    tokio::time::pause();
    let temp = TestDirectory::new("stop-while-missing");
    let tmux = temp.path().join("bin/tmux");
    assert!(!tmux.exists());

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path: lock_path.clone(),
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
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

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let deps = EngineDependencies {
        runner: Arc::new(FixtureRunner::default()),
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    assert!(InstanceLock::acquire_path(&lock_path).is_err());

    let stop_result = handle.stop().await;
    assert!(stop_result.is_ok(), "stop while missing must return Ok");

    let reacquired = InstanceLock::acquire_path(&lock_path);
    assert!(
        reacquired.is_ok(),
        "lock must be re-acquirable immediately after stopping while missing"
    );
}

#[tokio::test]
async fn finalize_failure_after_rename_restores_and_unlocks() {
    tokio::time::pause();
    let temp = TestDirectory::new("finalize-failure-rename");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let owner_status_left = "#[fg=cyan]#S#[default]";
    let original_length = "40";
    let widened_length = "41";

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let mut invocations = vec![
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ServerIdentity,
                &["display-message", "-p", "#{pid} #{start_time}"],
            ),
            outcome: output(b"1234 1000\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{original_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: nonzero(1),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &[
                    "set-option",
                    "-g",
                    STATUS_LEFT,
                    &format!("{INDICATOR_PREFIX}{owner_status_left}"),
                ],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, widened_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-g", SOLSTONE_OPTION, OBSERVING_VALUE],
            ),
            outcome: output(b""),
        },
    ];

    invocations.extend(standard_poll_invocations(
        &tmux,
        t0.unix_timestamp() + 1,
        b"hello terminal\n",
    ));

    // Periodic indicator refresh on 5s tick
    invocations.push(ExpectedInvocation {
        invocation: invocation(
            &tmux,
            TmuxOperation::ServerIdentity,
            &["display-message", "-p", "#{pid} #{start_time}"],
        ),
        outcome: output(b"1234 1000\n"),
    });

    // Restore invocations on teardown
    invocations.extend([
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT.to_owned()),
                &["show-options", "-gv", STATUS_LEFT],
            ),
            outcome: output(format!("{INDICATOR_PREFIX}{owner_status_left}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT.to_owned()),
                &["set-option", "-g", STATUS_LEFT, owner_status_left],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(STATUS_LEFT_LENGTH.to_owned()),
                &["show-options", "-gv", STATUS_LEFT_LENGTH],
            ),
            outcome: output(format!("{widened_length}\n").into_bytes()),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::SetOption(STATUS_LEFT_LENGTH.to_owned()),
                &["set-option", "-g", STATUS_LEFT_LENGTH, original_length],
            ),
            outcome: output(b""),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::ShowOption(SOLSTONE_OPTION.to_owned()),
                &["show-options", "-gv", SOLSTONE_OPTION],
            ),
            outcome: output(b"observing\n"),
        },
        ExpectedInvocation {
            invocation: invocation(
                &tmux,
                TmuxOperation::UnsetOption(SOLSTONE_OPTION.to_owned()),
                &["set-option", "-gu", SOLSTONE_OPTION],
            ),
            outcome: output(b""),
        },
    ]);

    let runner = Arc::new(FixtureRunner::new(invocations));

    let root = temp.path().join("data");
    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path: lock_path.clone(),
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
        settings: CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            pane_title: true,
            working_directory: true,
            executable: true,
            status_indicator: true,
            output_sessions: Vec::new(),
        },
        closed_segments: Arc::new(|_| {}),
        diagnostics: Arc::new(|_| {}),
        health: Arc::new(|_| {}),
    };

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let pids = Arc::new(Mutex::new(Vec::new()));
    let fake_lookup = Arc::new(FakeLookup::new(pids, Ok("vim".to_owned())));

    let deps = EngineDependencies {
        runner: runner.clone(),
        clock: clock.clone(),
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: Some(fake_lookup),
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    // Let engine start and install indicator at t=0
    tokio::task::yield_now().await;

    // Advance 5s for Tick 1: 1 capture durable
    clock.advance(5);
    tokio::time::advance(Duration::from_millis(5001)).await;
    tokio::task::yield_now().await;

    // Incomplete segment directory stem is 120000, duration 5s -> finalized name 120000_005.
    // Create `120000_005.zone.json` as a directory so that `write_capture_time` fails with EISDIR
    // strictly AFTER `fs::rename(&incomplete, &finalized)` succeeds!
    let stream_dir = root.join("20260701/stream.tmux");
    let zone_path = stream_dir.join("120000_005.zone.json");
    fs::create_dir_all(&zone_path).expect("create zone collision directory");

    let stop_res = handle.stop().await;
    assert!(
        matches!(stop_res, Err(EngineError::FatalSegmentError(_))),
        "stop must fail with FatalSegmentError after rename finalize error"
    );

    // Verify lock is immediately re-acquirable
    let lock_check = InstanceLock::acquire_path(&lock_path);
    assert!(
        lock_check.is_ok(),
        "lock must be immediately re-acquirable after finalize failure"
    );

    // Verify rename had already succeeded
    assert!(
        stream_dir.join("120000_005").is_dir(),
        "finalized directory should exist from the rename that succeeded before the write_capture_time error"
    );

    // Verify indicator options were restored
    runner.assert_finished().expect("all invocations completed");
}

#[tokio::test]
async fn startup_reports_finalized_incomplete_and_skips_failed_and_empty() {
    tokio::time::pause();
    let temp = TestDirectory::new("startup-reports");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let root = temp.path().join("data");
    let stream_yesterday = root.join("20260630/stream.tmux");
    let stream_today = root.join("20260701/stream.tmux");
    fs::create_dir_all(&stream_yesterday).unwrap();
    fs::create_dir_all(&stream_today).unwrap();

    // 1. Incomplete segment yesterday
    setup_incomplete_recovery_fixture(&stream_yesterday, "140000");

    // 2. Incomplete segment today
    setup_incomplete_recovery_fixture(&stream_today, "100000");

    // 3. Already-finished segment today
    let finished_dir = stream_today.join("090000_300");
    fs::create_dir_all(&finished_dir).unwrap();
    fs::write(
        finished_dir.join("tmux_main_screen.jsonl"),
        b"{\"frame\":1}\n",
    )
    .unwrap();
    fs::write(
        stream_today.join("090000_300.zone.json"),
        b"{\"tz\":\"UTC\",\"utc_offset_seconds\":0}\n",
    )
    .unwrap();

    // 4. Failed segment today (.failed)
    let failed_dir = stream_today.join("080000_300.failed");
    fs::create_dir_all(&failed_dir).unwrap();
    fs::write(failed_dir.join("tmux_main_screen.jsonl"), b"{}\n").unwrap();
    fs::write(stream_today.join("080000_300.failed.meta"), b"{}\n").unwrap();

    let closed_segments = Arc::new(Mutex::new(Vec::new()));
    let closed_sink = {
        let c = closed_segments.clone();
        Arc::new(move |seg: ClosedSegment| {
            c.lock().unwrap().push(seg);
        })
    };

    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |day: &str| test_root.join(day).join("stream.tmux")),
        settings: CaptureSettings {
            capture_interval: 5,
            segment_interval: 300,
            pane_title: true,
            working_directory: true,
            executable: true,
            status_indicator: false,
            output_sessions: Vec::new(),
        },
        closed_segments: closed_sink,
        diagnostics: Arc::new(|_| {}),
        health: Arc::new(|_| {}),
    };

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let runner = Arc::new(FixtureRunner::default());
    let deps = EngineDependencies {
        runner,
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    // Stop immediately before capturing any frames (empty segment created during run is removed)
    handle.stop().await.expect("stop engine");

    let reported = closed_segments.lock().unwrap().clone();
    assert_eq!(
        reported.len(),
        3,
        "expected exactly 3 segments reported, got: {reported:?}"
    );

    let has_yesterday = reported
        .iter()
        .any(|s| s.day == "20260630" && s.segment == "140000_001");
    let has_today_incomplete = reported
        .iter()
        .any(|s| s.day == "20260701" && s.segment == "100000_001");
    let has_today_finished = reported
        .iter()
        .any(|s| s.day == "20260701" && s.segment == "090000_300");

    assert!(
        has_yesterday,
        "yesterday incomplete must be finalized and reported"
    );
    assert!(
        has_today_incomplete,
        "today incomplete must be finalized and reported"
    );
    assert!(has_today_finished, "today finished must be reported");

    // Failed segment must NOT be reported
    assert!(
        !reported.iter().any(|s| s.segment.contains(".failed")),
        ".failed segment must not be reported"
    );

    // Empty segment removed at stop must NOT be reported
    assert!(
        !reported
            .iter()
            .any(|s| s.segment == "120000_000" || s.segment.starts_with("120000")),
        "empty segment removed at stop must not be reported"
    );
}

#[tokio::test]
async fn sibling_stream_is_untouched() {
    tokio::time::pause();
    let temp = TestDirectory::new("sibling-stream");
    let tmux = temp.path().join("bin/tmux");
    create_executable(&tmux);

    let root = temp.path().join("data");
    let day = "20260701";
    let tmux_stream = root.join(day).join("_tmux");
    let other_stream = root.join(day).join("_other");
    fs::create_dir_all(&tmux_stream).unwrap();
    fs::create_dir_all(&other_stream).unwrap();

    // Place identical incomplete fixtures under both streams
    setup_incomplete_recovery_fixture(&tmux_stream, "164204");
    setup_incomplete_recovery_fixture(&other_stream, "164204");

    let other_screen_before =
        fs::read(other_stream.join("164204.incomplete/tmux_synthetic_screen.jsonl"))
            .expect("read other screen before");
    let other_meta_before =
        fs::read(other_stream.join("164204.incomplete.meta")).expect("read other meta before");

    let lock_path = temp.path().join("lock");
    let test_root = root.clone();
    let config = EngineConfig {
        tmux_path: tmux.clone(),
        root: root.clone(),
        lock_path,
        layout: Arc::new(move |d: &str| test_root.join(d).join("_tmux")),
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

    let t0 = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::July, 1).unwrap(),
        Time::from_hms(12, 0, 0).unwrap(),
    )
    .assume_utc();

    let clock = Arc::new(ScriptedClock::new(t0, UtcOffset::UTC));
    let zone = Zone::utc();

    let runner = Arc::new(FixtureRunner::default());
    let deps = EngineDependencies {
        runner,
        clock,
        zone_source: Box::new(StaticZoneSource(zone)),
        lookup: None,
    };

    let handle = start_with_dependencies(config, deps)
        .await
        .expect("start engine");

    handle.stop().await.expect("stop engine");

    // 1. Show _tmux stream was finalized
    assert!(
        tmux_stream.join("164204_001").is_dir(),
        "fixture under _tmux must be finalized"
    );
    assert!(
        !tmux_stream.join("164204.incomplete").exists(),
        "incomplete under _tmux must no longer exist"
    );

    // 2. Show _other sibling stream was byte-for-byte untouched
    assert!(
        other_stream.join("164204.incomplete").is_dir(),
        "sibling _other incomplete dir must remain"
    );
    assert!(
        other_stream.join("164204.incomplete.meta").is_file(),
        "sibling _other incomplete meta must remain"
    );
    assert_eq!(
        fs::read(other_stream.join("164204.incomplete/tmux_synthetic_screen.jsonl")).unwrap(),
        other_screen_before,
        "sibling screen file must be byte-for-byte unchanged"
    );
    assert_eq!(
        fs::read(other_stream.join("164204.incomplete.meta")).unwrap(),
        other_meta_before,
        "sibling meta file must be byte-for-byte unchanged"
    );
    assert!(
        !other_stream.join("164204_001").exists(),
        "sibling _other must not have finalized target"
    );
}

fn quiet_settings(capture_interval: u64, segment_interval: u64) -> CaptureSettings {
    CaptureSettings {
        capture_interval,
        segment_interval,
        status_indicator: false,
        ..EngineHarness::settings()
    }
}

fn screen_lines(segment_dir: &Path) -> usize {
    fs::read_to_string(segment_dir.join("tmux_main_screen.jsonl"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_day_without_the_layout_directory_is_skipped() {
    let mut harness = EngineHarness::new(
        "day-without-layout",
        FakeTmux::with_server(&owner_options()),
    );
    harness.stream = "_tmux".to_owned();
    let other = harness.root.join("20260630/_other");
    fs::create_dir_all(&other).expect("other stream");
    setup_incomplete_recovery_fixture(&other, "164204");
    let screen_before =
        fs::read(other.join("164204.incomplete/tmux_synthetic_screen.jsonl")).expect("screen");
    let meta_before = fs::read(other.join("164204.incomplete.meta")).expect("meta");

    let handle = harness
        .start(quiet_settings(5, 300))
        .await
        .expect("a day holding only another stream starts");
    handle.stop().await.expect("stop");

    assert!(!harness.root.join("20260630/_tmux").exists());
    assert_eq!(
        fs::read(other.join("164204.incomplete/tmux_synthetic_screen.jsonl")).expect("screen"),
        screen_before
    );
    assert_eq!(
        fs::read(other.join("164204.incomplete.meta")).expect("meta"),
        meta_before
    );
    assert!(!other.join("164204_001").exists());
}

#[tokio::test]
async fn startup_recovery_outcomes_reach_the_diagnostic_sink() {
    let harness = EngineHarness::new("startup-recovery-diagnostic", FakeTmux::default());
    // An interrupted segment without its metadata: 2.0.28's recovery retains it.
    let retained = harness.stream_dir("20260701").join("110000.incomplete");
    fs::create_dir_all(&retained).expect("retained segment");
    fs::write(
        retained.join("tmux_main_screen.jsonl"),
        b"{\"frame_id\": 1}\n",
    )
    .expect("frame");

    let handle = harness.start(quiet_settings(5, 300)).await.expect("start");
    handle.stop().await.expect("stop");

    let recovery = harness
        .diagnostics()
        .into_iter()
        .filter(|diagnostic| matches!(diagnostic, Diagnostic::Recovery(_)))
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 1, "{recovery:?}");
    assert!(
        recovery[0]
            .message()
            .contains(&retained.display().to_string())
    );
    assert!(retained.is_dir());
}

#[tokio::test]
async fn segment_warnings_are_reported_as_segment_diagnostics() {
    struct FailingZone;
    impl ZoneSource for FailingZone {
        fn read(&mut self) -> Result<Zone, String> {
            Err("no zone database".to_owned())
        }
    }
    let harness = EngineHarness::new("segment-diagnostics", FakeTmux::default());

    let handle = harness
        .start_with_zone(quiet_settings(5, 300), Box::new(FailingZone))
        .await
        .expect("start");
    handle.stop().await.expect("stop");

    let diagnostics = harness.diagnostics();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic, Diagnostic::ZoneLoadFailure(_)))
            .count(),
        1,
        "{diagnostics:?}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| matches!(diagnostic, Diagnostic::IndicatorFailure(_))),
        "{diagnostics:?}"
    );
}

#[tokio::test]
async fn no_server_at_start_installs_once_when_one_appears_and_again_after_a_restart() {
    let harness = EngineHarness::new("no-server-at-start", FakeTmux::default());
    let fake = harness.fake.clone();

    let handle = harness
        .start(EngineHarness::settings())
        .await
        .expect("start without a server");
    harness.advance(10, 5).await;
    assert_eq!(fake.writes_of(STATUS_LEFT), 0);

    fake.start_server(&owner_options());
    harness.advance(5, 5).await;
    let installed = format!("{INDICATOR_PREFIX}{OWNER_STATUS_LEFT}");
    assert_eq!(
        fake.option(STATUS_LEFT).as_deref(),
        Some(installed.as_str())
    );
    assert_eq!(fake.option(STATUS_LEFT_LENGTH).as_deref(), Some("41"));
    assert_eq!(fake.writes_of(STATUS_LEFT), 1);

    harness.advance(10, 5).await;
    assert_eq!(fake.writes_of(STATUS_LEFT), 1, "installed exactly once");

    handle
        .set_sync_activity(SyncActivity::Working)
        .await
        .expect("sync activity");
    settle().await;
    assert_eq!(fake.option(SOLSTONE_OPTION).as_deref(), Some("syncing"));

    // That server exits and another starts with the owner's own options.
    fake.stop_server();
    harness.advance(5, 5).await;
    fake.start_server(&owner_options());
    harness.advance(5, 5).await;
    assert_eq!(
        fake.option(STATUS_LEFT).as_deref(),
        Some(installed.as_str())
    );
    assert_eq!(fake.option(STATUS_LEFT_LENGTH).as_deref(), Some("41"));
    assert_eq!(fake.option(SOLSTONE_OPTION).as_deref(), Some("syncing"));
    assert_eq!(
        fake.writes_of(STATUS_LEFT),
        2,
        "reinstalled on the new server"
    );

    handle.stop().await.expect("stop");
    assert_eq!(fake.option(STATUS_LEFT).as_deref(), Some(OWNER_STATUS_LEFT));
    assert_eq!(fake.option(STATUS_LEFT_LENGTH).as_deref(), Some("40"));
    assert_eq!(fake.option(SOLSTONE_OPTION), None);
    assert_eq!(harness.health(), vec![TmuxHealth::Found]);
}

#[tokio::test]
async fn failing_polls_still_rotate_the_open_segment() {
    let harness = EngineHarness::new(
        "failing-polls-rotate",
        FakeTmux::with_server(&owner_options()),
    );
    let handle = harness.start(quiet_settings(5, 10)).await.expect("start");
    assert_eq!(harness.fake.polls(), 1);

    harness.fake.set_fail_polls(true);
    harness.advance(10, 5).await;

    let closed = harness.closed();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].segment, "120000_010");
    assert_eq!(screen_lines(&closed[0].dir), 1);
    let skips = harness
        .diagnostics()
        .into_iter()
        .filter(|diagnostic| matches!(diagnostic, Diagnostic::PollSkip(_)))
        .count();
    assert_eq!(skips, 1, "one warning for the run of failed polls");

    handle.stop().await.expect("stop");
}

#[tokio::test]
async fn missing_tmux_still_rotates_the_open_segment() {
    let harness = EngineHarness::new("missing-rotates", FakeTmux::with_server(&owner_options()));
    let handle = harness.start(quiet_settings(5, 10)).await.expect("start");
    assert_eq!(harness.fake.polls(), 1);

    fs::remove_file(&harness.tmux).expect("remove tmux");
    harness.advance(10, 5).await;

    assert_eq!(
        harness.health(),
        vec![TmuxHealth::Found, TmuxHealth::Missing]
    );
    assert_eq!(
        harness.fake.polls(),
        1,
        "nothing is captured while tmux is missing"
    );
    let closed = harness.closed();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].segment, "120000_010");
    assert_eq!(screen_lines(&closed[0].dir), 1);

    handle.stop().await.expect("stop");
    assert!(harness.lock_is_free());
}

#[tokio::test]
async fn package_manager_symlink_repoint_misses_at_most_once() {
    let mut harness =
        EngineHarness::new("symlink-repoint", FakeTmux::with_server(&owner_options()));
    let cellar_a = harness.temp.path().join("Cellar/A/bin/tmux");
    let cellar_b = harness.temp.path().join("Cellar/B/bin/tmux");
    write_executable(&cellar_a);
    write_executable(&cellar_b);
    let bin_dir = harness.temp.path().join("homebrew/bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    let tmux_link = bin_dir.join("tmux");
    symlink(&cellar_a, &tmux_link).expect("initial symlink");
    harness.tmux = tmux_link.clone();

    let handle = harness.start(quiet_settings(5, 10)).await.expect("start");

    // The package manager installs B, repoints the link and deletes A.
    let staged = bin_dir.join("tmux.tmp");
    symlink(&cellar_b, &staged).expect("staged symlink");
    fs::rename(&staged, &tmux_link).expect("repoint symlink");
    fs::remove_dir_all(harness.temp.path().join("Cellar/A")).expect("delete A");
    let polls_at_repoint = harness.fake.polls();

    // The segment open at the repoint closes at 10 s; the next one opens after it.
    harness.advance(10, 5).await;
    harness.fake.change_content();
    harness.advance(5, 5).await;
    handle.stop().await.expect("stop");

    let missing = harness
        .health()
        .iter()
        .filter(|state| **state == TmuxHealth::Missing)
        .count();
    assert!(missing <= 1, "{:?}", harness.health());
    assert!(harness.fake.polls() >= polls_at_repoint + 2);
    let closed = harness.closed();
    assert_eq!(closed.len(), 2, "{closed:?}");
    assert_eq!(closed[1].segment, "120010_005");
    assert!(
        screen_lines(&closed[1].dir) >= 1,
        "the segment opened after the repoint holds captures"
    );
}

#[tokio::test]
async fn poisoned_open_segment_restores_options_and_releases_lock() {
    if !permissions_are_enforced() {
        return;
    }
    let harness = EngineHarness::new("poisoned-segment", FakeTmux::with_server(&owner_options()));
    let fake = harness.fake.clone();
    let mut handle = harness
        .start(EngineHarness::settings())
        .await
        .expect("start");
    let stream_dir = harness.stream_dir("20260701");
    let incomplete = stream_dir.join("120000.incomplete");
    assert_eq!(
        screen_lines(&incomplete),
        1,
        "one durable frame before the fault"
    );
    let installed = format!("{INDICATOR_PREFIX}{OWNER_STATUS_LEFT}");
    assert_eq!(
        fake.option(STATUS_LEFT).as_deref(),
        Some(installed.as_str())
    );

    // The metadata write after the next append fails, and so does its rollback:
    // the segment is poisoned, which ends the engine.
    set_mode(&stream_dir, 0o555);
    fake.change_content();
    harness.advance(5, 5).await;
    let ended = tokio::time::timeout(Duration::from_secs(1), handle.wait()).await;
    set_mode(&stream_dir, 0o700);

    let result = ended.expect("the engine ends on its own");
    assert!(
        matches!(result, Err(EngineError::FatalSegmentError(_))),
        "{result:?}"
    );
    assert_eq!(fake.option(STATUS_LEFT).as_deref(), Some(OWNER_STATUS_LEFT));
    assert_eq!(fake.option(STATUS_LEFT_LENGTH).as_deref(), Some("40"));
    assert_eq!(fake.option(SOLSTONE_OPTION), None);
    assert!(harness.lock_is_free(), "the lock is free at once");
    assert!(incomplete.is_dir());
    assert!(stream_dir.join("120000.incomplete.meta").is_file());

    // A restart on the same layout recovers the poisoned segment's durable frame.
    let reported_before = harness.closed().len();
    let handle = harness
        .start(quiet_settings(5, 300))
        .await
        .expect("restart");
    let recovered = harness.closed()[reported_before..].to_vec();
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert_eq!(recovered[0].day, "20260701");
    assert!(recovered[0].segment.starts_with("120000_"), "{recovered:?}");
    assert_eq!(screen_lines(&recovered[0].dir), 1);
    assert!(!incomplete.exists());
    handle.stop().await.expect("stop restarted engine");
}

#[tokio::test]
async fn an_unwritable_layout_fails_start_and_leaves_the_owner_options() {
    if !permissions_are_enforced() {
        return;
    }
    let harness = EngineHarness::new("unwritable-layout", FakeTmux::with_server(&owner_options()));
    fs::create_dir_all(&harness.root).expect("root");
    set_mode(&harness.root, 0o555);
    let result = harness.start(EngineHarness::settings()).await;
    set_mode(&harness.root, 0o700);

    assert!(
        matches!(result, Err(EngineError::FatalSegmentError(_))),
        "{:?}",
        result.err()
    );
    assert_eq!(
        harness.fake.option(STATUS_LEFT).as_deref(),
        Some(OWNER_STATUS_LEFT)
    );
    assert_eq!(
        harness.fake.option(STATUS_LEFT_LENGTH).as_deref(),
        Some("40")
    );
    assert_eq!(harness.fake.option(SOLSTONE_OPTION), None);
    assert!(harness.lock_is_free());
}
