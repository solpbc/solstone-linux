// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::Arc;
use std::time::Duration;

use crate::support::{EngineHarness, FakeTmux, owner_options, settle};
use solstone_terminal::observer::{ActivityLinger, SYNCING_LINGER, SyncActivity};
use solstone_terminal::{CaptureSettings, Diagnostic};

const INDICATOR_REFRESH: u64 = 5;

fn indicator_failures(harness: &EngineHarness) -> usize {
    harness
        .diagnostics()
        .iter()
        .filter(|diagnostic| matches!(diagnostic, Diagnostic::IndicatorFailure(_)))
        .count()
}

#[test]
fn syncing_lingers_in_the_cell_after_the_work_stops() {
    let start = std::time::Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);
    let mut linger = ActivityLinger::default();
    assert_eq!(
        linger.observe(SyncActivity::Working, at(0)),
        Some(SyncActivity::Working)
    );
    assert_eq!(linger.observe(SyncActivity::Idle, at(100)), None);
    assert_eq!(linger.ends_at(), Some(at(100) + SYNCING_LINGER));
    assert_eq!(linger.observe(SyncActivity::Working, at(900)), None);
    assert_eq!(linger.ends_at(), None);
    assert_eq!(linger.observe(SyncActivity::Idle, at(1_000)), None);
    assert_eq!(linger.expire(at(2_499)), None);
    assert_eq!(linger.expire(at(2_500)), Some(SyncActivity::Idle));
    assert_eq!(linger.expire(at(3_000)), None);
}

#[tokio::test]
async fn a_failed_indicator_update_is_a_warning_and_capture_keeps_running() {
    let harness = EngineHarness::new(
        "supervision-indicator-failure",
        FakeTmux::with_server(&owner_options()),
    );
    let fake = harness.fake.clone();
    let handle = harness
        .start(EngineHarness::settings())
        .await
        .expect("start");
    let polls_at_start = fake.polls();

    // Every ☼ write fails: the activity update, then three refreshes.
    fake.set_fail_indicator(true);
    handle
        .set_sync_activity(SyncActivity::Working)
        .await
        .expect("sync activity");
    settle().await;
    harness
        .advance(INDICATOR_REFRESH * 3, INDICATOR_REFRESH)
        .await;
    assert_eq!(indicator_failures(&harness), 1, "one warning for the run");

    // A refresh succeeds in between, then the writes fail again.
    fake.set_fail_indicator(false);
    harness.advance(INDICATOR_REFRESH, INDICATOR_REFRESH).await;
    fake.set_fail_indicator(true);
    harness
        .advance(INDICATOR_REFRESH * 2, INDICATOR_REFRESH)
        .await;
    assert_eq!(indicator_failures(&harness), 2, "one more for the next run");
    fake.set_fail_indicator(false);

    // Capture kept running throughout and the engine did not end.
    assert!(fake.polls() >= polls_at_start + 6, "{}", fake.polls());
    handle
        .set_sync_activity(SyncActivity::Idle)
        .await
        .expect("the engine is still running");
    handle.stop().await.expect("stop");
    let closed = harness.closed();
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert!(closed[0].dir.join("tmux_main_screen.jsonl").is_file());
}

#[tokio::test]
async fn shutdown_keeps_the_final_segment_for_a_later_scan() {
    let harness = EngineHarness::new("supervision-final-segment", FakeTmux::with_server(&[]));
    let settings = CaptureSettings {
        status_indicator: false,
        ..EngineHarness::settings()
    };
    let handle = harness.start(settings.clone()).await.expect("start");
    harness.advance(5, 5).await;
    handle.stop().await.expect("stop");

    let finalized = harness.stream_dir("20260701").join("120000_005");
    assert!(finalized.is_dir());
    assert!(finalized.join("tmux_main_screen.jsonl").is_file());
    assert_eq!(
        harness
            .closed()
            .iter()
            .filter(|segment| segment.dir == finalized)
            .count(),
        1
    );
    assert!(harness.lock_is_free());

    // Nobody delivered it, so the next start reports it again from the layout.
    let handle = harness.start(settings).await.expect("restart");
    assert_eq!(
        harness
            .closed()
            .iter()
            .filter(|segment| segment.dir == finalized)
            .count(),
        2
    );
    handle.stop().await.expect("stop restarted engine");
}

// What happens after capture stops is not captured time: a segment closed by stop
// is named for the time capture stopped, never for when restoring the ☼ ended.
#[tokio::test]
async fn shutdown_names_the_segment_for_when_capture_stopped() {
    let harness = EngineHarness::new(
        "supervision-shutdown-length",
        FakeTmux::with_server(&owner_options()),
    );
    let handle = harness
        .start(EngineHarness::settings())
        .await
        .expect("start");
    harness.advance(5, 5).await;
    let clock = Arc::clone(&harness.clock);
    harness.fake.on_unset(move || clock.advance(314));

    handle.stop().await.expect("stop");

    let stream_dir = harness.stream_dir("20260701");
    assert!(stream_dir.join("120000_005").is_dir());
    assert!(!stream_dir.join("120000_300").exists());
    assert!(!stream_dir.join("120000_319").exists());
}
