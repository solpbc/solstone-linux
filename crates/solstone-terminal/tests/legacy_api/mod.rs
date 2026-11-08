// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::support::TestDirectory;
use solstone_terminal::config::{CaptureSettings, read_lenient_legacy_segment_interval};
use solstone_terminal::instance_lock::InstanceLock;
use solstone_terminal::legacy::{HeldReason, LegacyError, LegacyRecovery, open};
use solstone_terminal::storage::{CaptureTime, SegmentMetadata, write_capture_time};

fn setup_incomplete_fixture(stream_dir: &Path, elapsed_secs: u64) {
    let incomplete_dir = stream_dir.join("164204.incomplete");
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
    metadata.elapsed_nanos = elapsed_secs * 1_000_000_000;
    metadata.finalized_dir = format!("164204_{:03}", elapsed_secs);

    let updated_meta = serde_json::to_vec(&metadata).expect("serialize updated metadata");
    fs::write(stream_dir.join("164204.incomplete.meta"), updated_meta).expect("write updated meta");
}

#[test]
fn legacy_lock_held_by_another_fd_leaves_the_tree_unchanged() {
    let temp = TestDirectory::new("legacy-lock-held");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    setup_incomplete_fixture(&stream_dir, 450);

    // Acquire lock externally
    let _lock = InstanceLock::acquire(&data_root).expect("acquire external lock");

    // Attempt to open legacy root while lock is held
    let result = open(&data_root);
    assert!(
        matches!(result, Err(LegacyError::StillRunning(_))),
        "expected StillRunning error, got: {result:?}"
    );

    // Verify tree is completely unchanged
    assert!(stream_dir.join("164204.incomplete").is_dir());
    assert!(stream_dir.join("164204.incomplete.meta").is_file());
    assert!(!stream_dir.join("164204_450").exists());
    assert!(!stream_dir.join("164204_300").exists());
}

#[test]
fn legacy_second_open_in_process_is_still_running() {
    let temp = TestDirectory::new("legacy-second-open");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).expect("create data_root");

    let first = open(&data_root).expect("first open");
    let second = open(&data_root);
    assert!(
        matches!(second, Err(LegacyError::StillRunning(_))),
        "second open must fail with StillRunning"
    );

    drop(first);

    // After dropping first guard, open succeeds
    let third = open(&data_root);
    assert!(third.is_ok(), "open should succeed after dropping guard");
}

#[test]
fn legacy_config_600_finalizes_450_as_450() {
    let temp = TestDirectory::new("legacy-config-600");
    let data_root = temp.path().join("data");
    let config_path = temp.path().join("config.json");

    // Full 2.0.28 config.json rejected by CaptureSettings
    let config_json = r#"{
        "stream": "host.tmux",
        "source": "host",
        "cache_retention_days": 14,
        "segment_interval": 600
    }"#;
    assert!(
        serde_json::from_str::<CaptureSettings>(config_json).is_err(),
        "2.0.28 config.json with extra fields must be rejected by CaptureSettings"
    );

    fs::write(&config_path, config_json).expect("write config.json");
    let interval = read_lenient_legacy_segment_interval(&config_path);
    assert_eq!(interval, 600);

    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    setup_incomplete_fixture(&stream_dir, 450);

    let guard = open(&data_root).expect("open data root");
    let recovery = guard.recover(interval).expect("recover with interval 600");
    assert_eq!(recovery, LegacyRecovery::default());

    assert!(
        stream_dir.join("164204_450").is_dir(),
        "incomplete open for 450s should finalize as _450 when interval is 600"
    );
    assert!(!stream_dir.join("164204.incomplete").exists());
}

#[test]
fn legacy_missing_config_finalizes_as_300() {
    let temp = TestDirectory::new("legacy-missing-config");
    let data_root = temp.path().join("data");
    let missing_config = temp.path().join("absent_config.json");

    let interval = read_lenient_legacy_segment_interval(&missing_config);
    assert_eq!(interval, 300);

    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    setup_incomplete_fixture(&stream_dir, 450);

    let guard = open(&data_root).expect("open data root");
    let recovery = guard.recover(interval).expect("recover with default 300");
    assert_eq!(recovery, LegacyRecovery::default());

    assert!(
        stream_dir.join("164204_300").is_dir(),
        "incomplete open for 450s should finalize as _300 when interval is 300"
    );
    assert!(!stream_dir.join("164204.incomplete").exists());
}

#[test]
fn legacy_empty_finished_directory_and_zone_sibling_are_removed() {
    let temp = TestDirectory::new("legacy-empty-dir");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    fs::create_dir_all(&stream_dir).expect("create stream_dir");

    // Empty finished directory + sibling zone file
    let empty_dir = stream_dir.join("120000_300");
    fs::create_dir_all(&empty_dir).expect("create empty dir");
    write_capture_time(
        &stream_dir,
        "120000_300",
        &CaptureTime {
            tz: Some("America/Denver".to_owned()),
            utc_offset_seconds: -21600,
        },
    )
    .expect("write zone file");

    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments().expect("scan segments");

    // Assert directory and zone file were pruned
    assert!(!empty_dir.exists(), "empty directory must be removed");
    assert!(
        !stream_dir.join("120000_300.zone.json").exists(),
        "zone sibling must be removed"
    );

    // Assert not listed in deliverable segments or held
    assert!(listing.segments.is_empty());
    assert!(listing.held.is_empty());
}

#[test]
fn legacy_ds_store_is_held_invalid_and_other_segments_are_listed() {
    let temp = TestDirectory::new("legacy-ds-store");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    fs::create_dir_all(&stream_dir).expect("create stream_dir");

    // Valid segment 120000_300
    let valid_dir = stream_dir.join("120000_300");
    fs::create_dir_all(&valid_dir).expect("create valid dir");
    fs::write(valid_dir.join("120000_desktop_activity.jsonl"), b"{}\n").expect("write activity");
    fs::write(valid_dir.join("tmux_main_screen.jsonl"), b"{}\n").expect("write screen");

    // Invalid segment 130000_300 contaminated with .DS_Store
    let invalid_dir = stream_dir.join("130000_300");
    fs::create_dir_all(&invalid_dir).expect("create invalid dir");
    fs::write(invalid_dir.join("130000_desktop_activity.jsonl"), b"{}\n").expect("write activity");
    fs::write(invalid_dir.join(".DS_Store"), b"\0\0").expect("write ds_store");

    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments().expect("scan segments");

    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].segment, "120000_300");

    assert_eq!(listing.held.len(), 1);
    assert_eq!(listing.held[0].segment, "130000_300");
    assert_eq!(listing.held[0].reason, HeldReason::Invalid);
}

#[test]
fn legacy_malformed_zone_is_deliverable_with_null() {
    let temp = TestDirectory::new("legacy-malformed-zone");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    fs::create_dir_all(&stream_dir).expect("create stream_dir");

    let seg_dir = stream_dir.join("120000_300");
    fs::create_dir_all(&seg_dir).expect("create seg dir");
    fs::write(seg_dir.join("120000_desktop_activity.jsonl"), b"{}\n").expect("write activity");
    fs::write(seg_dir.join("tmux_main_screen.jsonl"), b"{}\n").expect("write screen");

    // Malformed zone json
    fs::write(
        stream_dir.join("120000_300.zone.json"),
        b"{corrupt json syntax",
    )
    .expect("write malformed zone");

    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments().expect("scan segments");

    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].segment, "120000_300");
    assert!(
        listing.segments[0].zone.is_none(),
        "malformed zone file must yield None/null"
    );
}

#[test]
fn legacy_symlink_day_is_a_problem_and_the_good_day_is_listed() {
    let temp = TestDirectory::new("legacy-symlink-day");
    let data_root = temp.path().join("data");
    let captures = data_root.join("captures");
    fs::create_dir_all(&captures).expect("create captures");

    // Good day
    let good_day = captures.join("20260701");
    let stream_dir = good_day.join("stream.tmux");
    let seg_dir = stream_dir.join("120000_300");
    fs::create_dir_all(&seg_dir).expect("create good seg dir");
    fs::write(seg_dir.join("120000_desktop_activity.jsonl"), b"{}\n").expect("write activity");
    fs::write(seg_dir.join("tmux_main_screen.jsonl"), b"{}\n").expect("write screen");

    // Symlink day
    let target_dir = temp.path().join("outside_target");
    fs::create_dir_all(&target_dir).expect("create target dir");
    let symlink_day = captures.join("20260702");
    symlink(&target_dir, &symlink_day).expect("create symlink day");

    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments().expect("scan segments");

    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].day, "20260701");

    assert_eq!(listing.problems.len(), 1);
    assert_eq!(listing.problems[0].path, symlink_day);
}

#[test]
fn legacy_unreadable_captures_is_an_error() {
    let temp = TestDirectory::new("legacy-unreadable-captures");
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).expect("create data_root");

    // Make `captures` a regular file instead of a directory
    let captures_file = data_root.join("captures");
    fs::write(&captures_file, b"not a directory").expect("write captures file");

    let guard = open(&data_root).expect("open data root");
    let result = guard.segments();
    assert!(
        matches!(result, Err(LegacyError::UnreadableCaptures(_))),
        "expected UnreadableCaptures error, got: {result:?}"
    );
}

#[test]
fn legacy_missing_root_creates_nothing() {
    let temp = TestDirectory::new("legacy-missing-root");
    let missing_root = temp.path().join("does_not_exist");

    assert!(!missing_root.exists());

    let guard = open(&missing_root).expect("open missing root");
    let recovery = guard.recover(300).expect("recover on missing root");
    assert_eq!(recovery, LegacyRecovery::default());

    let listing = guard.segments().expect("segments on missing root");
    assert!(listing.segments.is_empty());
    assert!(listing.held.is_empty());
    assert!(listing.problems.is_empty());

    // Verify missing root was not created
    assert!(!missing_root.exists(), "missing root must not be created");
}

fn write_segment(stream_dir: &Path, name: &str) -> PathBuf {
    let stem = name.split_once('_').map_or(name, |(stem, _)| stem);
    let dir = stream_dir.join(name);
    fs::create_dir_all(&dir).expect("segment dir");
    fs::write(dir.join(format!("{stem}_desktop_activity.jsonl")), b"{}\n").expect("activity");
    fs::write(dir.join("tmux_main_screen.jsonl"), b"{}\n").expect("screen");
    dir
}

#[test]
fn legacy_recover_returns_retained_and_failed_segments_as_held() {
    let temp = TestDirectory::new("legacy-recover-held");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");

    // Without its metadata, a segment with frames is retained.
    let retained = stream_dir.join("110000.incomplete");
    fs::create_dir_all(&retained).expect("retained dir");
    fs::write(
        retained.join("tmux_main_screen.jsonl"),
        b"{\"frame_id\": 1}\n",
    )
    .expect("frame");

    // A segment whose finalized name is already taken cannot be finalized.
    setup_incomplete_fixture(&stream_dir, 300);
    let collision = stream_dir.join("164204_300");
    fs::create_dir_all(&collision).expect("collision dir");

    let guard = open(&data_root).expect("open data root");
    let mut recovery = guard.recover(300).expect("recover");
    recovery.held.sort_by(|a, b| a.segment.cmp(&b.segment));

    assert!(recovery.problems.is_empty(), "{:?}", recovery.problems);
    assert_eq!(recovery.held.len(), 2, "{:?}", recovery.held);
    assert_eq!(recovery.held[0].segment, "110000.incomplete");
    assert_eq!(recovery.held[0].dir, retained);
    assert_eq!(recovery.held[0].reason, HeldReason::Retained);
    assert_eq!(recovery.held[1].segment, "164204.incomplete");
    assert_eq!(recovery.held[1].day, "20260701");
    assert_eq!(recovery.held[1].reason, HeldReason::Failed);
    assert!(retained.is_dir());
    assert!(stream_dir.join("164204.incomplete").is_dir());
}

#[test]
fn legacy_unreadable_stream_directory_is_a_problem_and_the_rest_is_listed() {
    if !crate::support::permissions_are_enforced() {
        return;
    }
    let temp = TestDirectory::new("legacy-unreadable-stream");
    let data_root = temp.path().join("data");
    let day = data_root.join("captures/20260701");
    write_segment(&day.join("good.tmux"), "120000_300");
    let unreadable = day.join("locked.tmux");
    write_segment(&unreadable, "130000_300");

    crate::support::set_mode(&unreadable, 0o000);
    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments();
    crate::support::set_mode(&unreadable, 0o700);
    let listing = listing.expect("scan segments");

    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].segment, "120000_300");
    assert_eq!(listing.problems.len(), 1, "{:?}", listing.problems);
    assert_eq!(listing.problems[0].path, unreadable);
}

#[test]
fn legacy_segment_names_that_are_not_plain_directories_are_problems() {
    let temp = TestDirectory::new("legacy-segment-entries");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    write_segment(&stream_dir, "120000_300");
    let outside = write_segment(&temp.path().join("outside"), "130000_300");
    let linked = stream_dir.join("130000_300");
    symlink(&outside, &linked).expect("symlinked segment");
    let file = stream_dir.join("140000_300");
    fs::write(&file, b"not a directory").expect("segment-named file");

    let guard = open(&data_root).expect("open data root");
    let mut listing = guard.segments().expect("scan segments");
    listing.problems.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].segment, "120000_300");
    let paths = listing
        .problems
        .iter()
        .map(|problem| problem.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec![linked, file]);
}

#[test]
fn legacy_empty_segment_keeps_its_zone_while_the_directory_stays() {
    if !crate::support::permissions_are_enforced() {
        return;
    }
    let temp = TestDirectory::new("legacy-empty-kept");
    let data_root = temp.path().join("data");
    let stream_dir = data_root.join("captures/20260701/stream.tmux");
    let empty_dir = stream_dir.join("120000_300");
    fs::create_dir_all(&empty_dir).expect("empty dir");
    write_capture_time(
        &stream_dir,
        "120000_300",
        &CaptureTime {
            tz: Some("UTC".to_owned()),
            utc_offset_seconds: 0,
        },
    )
    .expect("zone file");

    crate::support::set_mode(&stream_dir, 0o500);
    let guard = open(&data_root).expect("open data root");
    let listing = guard.segments();
    crate::support::set_mode(&stream_dir, 0o700);
    let listing = listing.expect("scan segments");

    assert!(empty_dir.is_dir());
    assert!(stream_dir.join("120000_300.zone.json").is_file());
    assert!(listing.segments.is_empty());
    assert_eq!(listing.problems.len(), 1, "{:?}", listing.problems);
    assert_eq!(listing.problems[0].path, empty_dir);
}

#[test]
fn legacy_day_names_must_be_calendar_dates() {
    let temp = TestDirectory::new("legacy-calendar-days");
    let data_root = temp.path().join("data");
    let captures = data_root.join("captures");
    write_segment(&captures.join("20260701/stream.tmux"), "120000_300");
    let not_a_date = captures.join("20261399/stream.tmux");
    write_segment(&not_a_date, "120000_300");
    setup_incomplete_fixture(&not_a_date, 450);

    let guard = open(&data_root).expect("open data root");
    let recovery = guard.recover(300).expect("recover");
    let listing = guard.segments().expect("scan segments");

    assert_eq!(recovery, LegacyRecovery::default());
    assert!(
        not_a_date.join("164204.incomplete").is_dir(),
        "not recovered"
    );
    assert_eq!(listing.segments.len(), 1);
    assert_eq!(listing.segments[0].day, "20260701");
    assert!(listing.problems.is_empty());
}
