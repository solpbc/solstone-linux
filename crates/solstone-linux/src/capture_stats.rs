// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{ffi::OsStr, fs, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub struct CaptureStats {
    pub captures_today: u64,
    pub total_size_mb: u64,
}

#[derive(Debug, PartialEq)]
pub struct QuarantineStats {
    pub count: u64,
    pub oldest_age_seconds: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SegmentClass {
    Accepted,
    Incomplete,
    Failed,
}

fn classify_segment_name(name: &OsStr) -> SegmentClass {
    let name = name.to_string_lossy();
    if name.ends_with(".incomplete") {
        SegmentClass::Incomplete
    } else if name.ends_with(".failed") {
        SegmentClass::Failed
    } else {
        SegmentClass::Accepted
    }
}

#[derive(Debug, PartialEq)]
pub struct StatusCaptureStats {
    pub segment_count: u64,
    pub day_count: u64,
    pub size_mb: f64,
    pub incomplete_count: u64,
}

fn walk_segments(
    root: &Path,
    day: impl FnMut(&OsStr),
    segment: impl FnMut(&OsStr, SegmentClass),
    accepted_bytes: impl FnMut(u64),
) {
    walk_segments_with(root, day, segment, accepted_bytes, &|path| {
        fs::read_dir(path)
    })
}

fn walk_segments_with(
    root: &Path,
    mut day: impl FnMut(&OsStr),
    mut segment: impl FnMut(&OsStr, SegmentClass),
    mut accepted_bytes: impl FnMut(u64),
    read_dir: &dyn Fn(&Path) -> std::io::Result<fs::ReadDir>,
) {
    let _ = (|| -> std::io::Result<()> {
        if !root.exists() {
            return Ok(());
        }
        for day_entry in read_dir(root)? {
            let day_entry = day_entry?;
            if !day_entry.path().is_dir() {
                continue;
            }
            day(&day_entry.file_name());
            for stream in read_dir(&day_entry.path())? {
                let stream = stream?;
                if !stream.path().is_dir() {
                    continue;
                }
                for segment_entry in read_dir(&stream.path())? {
                    let segment_entry = segment_entry?;
                    if !segment_entry.path().is_dir() {
                        continue;
                    }
                    let class = classify_segment_name(&segment_entry.file_name());
                    segment(&day_entry.file_name(), class);
                    let mut bytes = 0;
                    if class == SegmentClass::Accepted {
                        for file in read_dir(&segment_entry.path())? {
                            let file = file?;
                            if file.path().is_file() {
                                bytes += file.metadata()?.len();
                            }
                        }
                        accepted_bytes(bytes);
                    }
                }
            }
        }
        Ok(())
    })();
}

pub fn compute_capture_stats(root: &Path, today: &str) -> CaptureStats {
    let mut captures_today = 0;
    let mut total_size = 0;
    walk_segments(
        root,
        |_| {},
        |day, class| {
            if class == SegmentClass::Accepted && day == OsStr::new(today) {
                captures_today += 1;
            }
        },
        |bytes| total_size += bytes,
    );
    CaptureStats {
        captures_today,
        total_size_mb: total_size / (1024 * 1024),
    }
}

pub fn compute_status_capture_stats(root: &Path) -> StatusCaptureStats {
    let day_count = std::cell::Cell::new(0);
    let mut stats = StatusCaptureStats {
        segment_count: 0,
        day_count: 0,
        size_mb: 0.0,
        incomplete_count: 0,
    };
    let mut total_size = 0;
    walk_segments(
        root,
        |_| day_count.set(day_count.get() + 1),
        |_, class| match class {
            SegmentClass::Accepted => {
                stats.segment_count += 1;
            }
            SegmentClass::Incomplete => stats.incomplete_count += 1,
            SegmentClass::Failed => {}
        },
        |bytes| total_size += bytes,
    );
    stats.day_count = day_count.get();
    stats.size_mb = total_size as f64 / (1024.0 * 1024.0);
    stats
}

/// A quarantined segment holding nothing but its metadata stub never captured a byte:
/// `recovery::recover_segment` marks an empty interrupted segment `.failed` by the same
/// path it uses for one carrying real media, so the suffix alone cannot tell them apart.
/// Counting the empty shape reports unsent content that does not exist, and it can never
/// drain, because there is nothing to send — it is held forever at a growing age.
/// Unreadable is deliberately not empty: a segment we cannot inspect keeps being counted
/// rather than disappearing from the owner's total on an I/O error.
fn holds_payload(segment_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(segment_dir) else {
        return true;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .file_name()
            .is_some_and(|name| name != crate::recovery::METADATA_FILENAME)
    })
}

pub fn compute_quarantine_stats(root: &Path, now: f64) -> QuarantineStats {
    let mut count = 0;
    let mut oldest_mtime: Option<f64> = None;
    let _ = (|| -> std::io::Result<()> {
        if !root.exists() {
            return Ok(());
        }
        for day in fs::read_dir(root)? {
            let day = day?;
            if !day.path().is_dir() {
                continue;
            }
            for stream in fs::read_dir(day.path())? {
                let stream = stream?;
                if !stream.path().is_dir() {
                    continue;
                }
                for segment in fs::read_dir(stream.path())? {
                    let segment = segment?;
                    if !segment.path().is_dir()
                        || classify_segment_name(&segment.file_name()) != SegmentClass::Failed
                        || !holds_payload(&segment.path())
                    {
                        continue;
                    }
                    count += 1;
                    let Ok(modified) = segment.metadata().and_then(|metadata| metadata.modified())
                    else {
                        continue;
                    };
                    let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH) else {
                        continue;
                    };
                    let mtime = elapsed.as_secs_f64();
                    oldest_mtime = Some(oldest_mtime.map_or(mtime, |oldest| oldest.min(mtime)));
                }
            }
        }
        Ok(())
    })();
    QuarantineStats {
        count,
        oldest_age_seconds: oldest_mtime.map(|mtime| (now - mtime).max(0.0)),
    }
}

/// A held segment has two possible histories and the suffix does not record which: the
/// journal declined it, or startup recovery could not finalize an interrupted one and it
/// was never sent at all. Naming either cause asserts something we cannot know — say only
/// what is true of both, which is that the segment is held and will not be retried.
pub fn format_quarantine_line(stats: &QuarantineStats) -> Option<String> {
    if stats.count == 0 {
        return None;
    }
    match stats.oldest_age_seconds {
        None => Some(format!("Held: {} segment(s) not sent", stats.count)),
        Some(age) => Some(format!(
            "Held: {} segment(s) not sent, oldest {}d",
            stats.count,
            (age / 86_400.0) as u64
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{File, FileTimes},
        time::{Duration, SystemTime},
    };

    fn segment(root: &Path, day: &str, name: &str) -> std::path::PathBuf {
        let path = root.join(day).join("archon").join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn set_mtime(path: &Path, mtime: f64) {
        File::open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(mtime)),
            )
            .unwrap();
    }

    // tests/test_capture_stats.py::test_compute_quarantine_stats_counts_both_shapes_and_oldest_age
    #[test]
    fn quarantine_shapes_and_age() {
        let t = tempfile::tempdir().unwrap();
        let a = segment(t.path(), "20260101", "120000_300.failed");
        let b = segment(t.path(), "20260101", "130000.failed");
        fs::write(a.join("audio.flac"), b"x").unwrap();
        fs::write(b.join("audio.flac"), b"x").unwrap();
        set_mtime(&a, 1_000_000.0 - 2.0 * 86_400.0);
        set_mtime(&b, 1_000_000.0 - 5.0 * 86_400.0);
        assert_eq!(
            compute_quarantine_stats(t.path(), 1_000_000.0),
            QuarantineStats {
                count: 2,
                oldest_age_seconds: Some(5.0 * 86_400.0)
            }
        );
    }
    // tests/test_capture_stats.py::test_compute_quarantine_stats_empty_or_missing_tree
    #[test]
    fn quarantine_empty_missing() {
        let t = tempfile::tempdir().unwrap();
        let expected = QuarantineStats {
            count: 0,
            oldest_age_seconds: None,
        };
        assert_eq!(
            compute_quarantine_stats(&t.path().join("missing"), 1.0),
            expected
        );
        assert_eq!(compute_quarantine_stats(t.path(), 1.0), expected);
    }
    // tests/test_dbus_service.py::TestComputeCaptureStats::test_returns_walk_counts
    #[test]
    fn capture_walk_counts() {
        let t = tempfile::tempdir().unwrap();
        let final_today = segment(t.path(), "20260102", "120000_300");
        let incomplete = segment(t.path(), "20260102", "120500.incomplete");
        let failed = segment(t.path(), "20260102", "121000_300.failed");
        let yesterday = segment(t.path(), "20260101", "130000_300");
        fs::write(final_today.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::write(incomplete.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::write(failed.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::write(yesterday.join("audio.flac"), b"x").unwrap();
        assert_eq!(
            compute_capture_stats(t.path(), "20260102"),
            CaptureStats {
                captures_today: 1,
                total_size_mb: 1
            }
        );
    }
    // tests/test_dbus_service.py::TestComputeCaptureStats::test_empty_captures
    #[test]
    fn capture_empty() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            compute_capture_stats(&t.path().join("missing"), "20260101"),
            CaptureStats {
                captures_today: 0,
                total_size_mb: 0
            }
        );
    }
    // AC: quarantine formatting shapes.
    #[test]
    fn format_empty() {
        assert_eq!(
            format_quarantine_line(&QuarantineStats {
                count: 0,
                oldest_age_seconds: None
            }),
            None
        );
    }
    #[test]
    fn format_unknown_age() {
        assert_eq!(
            format_quarantine_line(&QuarantineStats {
                count: 2,
                oldest_age_seconds: None
            })
            .as_deref(),
            Some("Held: 2 segment(s) not sent")
        );
    }
    #[test]
    fn format_days() {
        assert_eq!(
            format_quarantine_line(&QuarantineStats {
                count: 2,
                oldest_age_seconds: Some(5.9 * 86_400.0)
            })
            .as_deref(),
            Some("Held: 2 segment(s) not sent, oldest 5d")
        );
    }
    // AC: injected now clamps future mtimes to zero age.
    #[test]
    fn injected_now() {
        let t = tempfile::tempdir().unwrap();
        let failed = segment(t.path(), "20260101", "120000.failed");
        fs::write(failed.join("audio.flac"), b"x").unwrap();
        set_mtime(&failed, 200.0);
        assert_eq!(
            compute_quarantine_stats(t.path(), 100.0).oldest_age_seconds,
            Some(0.0)
        );
    }

    // AC: a failed segment holding only its metadata stub captured nothing, so it is not
    // reported as unsent content the owner could still recover.
    #[test]
    fn quarantine_skips_payload_free_failed() {
        // Far enough from the epoch that a 30-day-old fixture mtime stays positive.
        const NOW: f64 = 1_700_000_000.0;
        let t = tempfile::tempdir().unwrap();
        let empty = segment(t.path(), "20260101", "120000.failed");
        let stub = segment(t.path(), "20260101", "130000.failed");
        fs::write(stub.join(crate::recovery::METADATA_FILENAME), b"{}").unwrap();
        set_mtime(&empty, NOW - 30.0 * 86_400.0);
        set_mtime(&stub, NOW - 29.0 * 86_400.0);
        assert_eq!(
            compute_quarantine_stats(t.path(), NOW),
            QuarantineStats {
                count: 0,
                oldest_age_seconds: None
            }
        );

        // A sibling carrying real media is still held, and sets the reported age alone.
        let real = segment(t.path(), "20260101", "140000.failed");
        fs::write(real.join(crate::recovery::METADATA_FILENAME), b"{}").unwrap();
        fs::write(real.join("audio.flac"), b"x").unwrap();
        set_mtime(&real, NOW - 3.0 * 86_400.0);
        assert_eq!(
            compute_quarantine_stats(t.path(), NOW),
            QuarantineStats {
                count: 1,
                oldest_age_seconds: Some(3.0 * 86_400.0)
            }
        );
    }
    // AC: byte totals truncate rather than round.
    #[test]
    fn size_truncates() {
        let t = tempfile::tempdir().unwrap();
        let final_dir = segment(t.path(), "20260101", "120000_300");
        fs::write(final_dir.join("x"), vec![0; 1024 * 1024 - 1]).unwrap();
        assert_eq!(compute_capture_stats(t.path(), "20260101").total_size_mb, 0);
    }
    // AC: an error after accumulated work returns the partial accumulator (the walk's broad OSError contract).
    #[test]
    fn os_error_degrades_to_partial() {
        let t = tempfile::tempdir().unwrap();
        let unreadable = segment(t.path(), "20260101", "120000_300");
        let mut captures_today = 0;
        let mut total_size = 0;
        walk_segments_with(
            t.path(),
            |_| {},
            |day, class| {
                if class == SegmentClass::Accepted && day == OsStr::new("20260101") {
                    captures_today += 1;
                }
            },
            |bytes| total_size += bytes,
            &|path| {
                if path == unreadable {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "injected traversal error",
                    ))
                } else {
                    fs::read_dir(path)
                }
            },
        );
        assert_eq!(captures_today, 1);
        assert_eq!(total_size, 0);
    }

    // Python Path.is_dir follows symlinked capture hierarchy directories.
    #[test]
    fn follows_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let t = tempfile::tempdir().unwrap();
        let captures = t.path().join("captures");
        let actual_day = t.path().join("actual-day");
        let final_dir = actual_day.join("archon/120000_300");
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::create_dir(&captures).unwrap();
        symlink(&actual_day, captures.join("20260101")).unwrap();

        assert_eq!(
            compute_capture_stats(&captures, "20260101"),
            CaptureStats {
                captures_today: 1,
                total_size_mb: 1
            }
        );
    }

    // AC: status counts every direct child directory as a day, including empty and arbitrary names.
    #[test]
    fn status_counts_all_day_directories() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join("empty")).unwrap();
        fs::create_dir(t.path().join("not-a-date")).unwrap();
        fs::write(t.path().join("20260101"), b"not a directory").unwrap();
        assert_eq!(compute_status_capture_stats(t.path()).day_count, 2);
    }

    // AC: accepted, incomplete, and failed segments share one classification and sizing policy.
    #[test]
    fn status_classifies_and_sizes_segments() {
        let t = tempfile::tempdir().unwrap();
        let accepted = segment(t.path(), "20260101", "120000_300");
        let incomplete = segment(t.path(), "20260101", "120500.incomplete");
        let failed = segment(t.path(), "20260101", "121000_300.failed");
        fs::write(
            accepted.join("audio.flac"),
            vec![0; 1024 * 1024 + 512 * 1024],
        )
        .unwrap();
        fs::write(incomplete.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::write(failed.join("audio.flac"), vec![0; 1024 * 1024]).unwrap();
        fs::create_dir(accepted.join("nested")).unwrap();
        fs::write(accepted.join("nested/ignored"), vec![0; 1024 * 1024]).unwrap();
        assert_eq!(
            compute_status_capture_stats(t.path()),
            StatusCaptureStats {
                segment_count: 1,
                day_count: 1,
                size_mb: 1.5,
                incomplete_count: 1,
            }
        );
    }

    // AC: missing status trees return an empty aggregate.
    #[test]
    fn status_missing_tree_is_empty() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            compute_status_capture_stats(&t.path().join("missing")),
            StatusCaptureStats {
                segment_count: 0,
                day_count: 0,
                size_mb: 0.0,
                incomplete_count: 0,
            }
        );
    }
}
