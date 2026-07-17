// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{fs, path::Path};

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

pub fn compute_capture_stats(root: &Path, today: &str) -> CaptureStats {
    let mut captures_today = 0;
    let mut total_size = 0;
    let walk_result = (|| -> std::io::Result<()> {
        if !root.exists() {
            return Ok(());
        }
        for day in fs::read_dir(root)? {
            let day = day?;
            if !day.file_type()?.is_dir() {
                continue;
            }
            for stream in fs::read_dir(day.path())? {
                let stream = stream?;
                if !stream.file_type()?.is_dir() {
                    continue;
                }
                for segment in fs::read_dir(stream.path())? {
                    let segment = segment?;
                    if !segment.file_type()?.is_dir() {
                        continue;
                    }
                    let name = segment.file_name();
                    let name = name.to_string_lossy();
                    if name.ends_with(".incomplete") || name.ends_with(".failed") {
                        continue;
                    }
                    if day.file_name() == today {
                        captures_today += 1;
                    }
                    for file in fs::read_dir(segment.path())? {
                        let file = file?;
                        if file.file_type()?.is_file() {
                            total_size += file.metadata()?.len();
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    capture_stats_after_walk(captures_today, total_size, walk_result)
}

fn capture_stats_after_walk(
    captures_today: u64,
    total_size: u64,
    _walk_result: std::io::Result<()>,
) -> CaptureStats {
    CaptureStats {
        captures_today,
        total_size_mb: total_size / (1024 * 1024),
    }
}

pub fn compute_quarantine_stats(root: &Path, now: f64) -> QuarantineStats {
    let mut count = 0;
    let mut oldest_mtime: Option<f64> = None;
    let walk_result = (|| -> std::io::Result<()> {
        if !root.exists() {
            return Ok(());
        }
        for day in fs::read_dir(root)? {
            let day = day?;
            if !day.file_type()?.is_dir() {
                continue;
            }
            for stream in fs::read_dir(day.path())? {
                let stream = stream?;
                if !stream.file_type()?.is_dir() {
                    continue;
                }
                for segment in fs::read_dir(stream.path())? {
                    let segment = segment?;
                    if !segment.file_type()?.is_dir()
                        || !segment.file_name().to_string_lossy().ends_with(".failed")
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
    quarantine_stats_after_walk(count, oldest_mtime, now, walk_result)
}

fn quarantine_stats_after_walk(
    count: u64,
    oldest_mtime: Option<f64>,
    now: f64,
    _walk_result: std::io::Result<()>,
) -> QuarantineStats {
    QuarantineStats {
        count,
        oldest_age_seconds: oldest_mtime.map(|mtime| (now - mtime).max(0.0)),
    }
}

pub fn format_quarantine_line(stats: &QuarantineStats) -> Option<String> {
    if stats.count == 0 {
        return None;
    }
    match stats.oldest_age_seconds {
        None => Some(format!(
            "Quarantine: {} rejected segment(s) held",
            stats.count
        )),
        Some(age) => Some(format!(
            "Quarantine: {} rejected segment(s) held, oldest {}d",
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
            Some("Quarantine: 2 rejected segment(s) held")
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
            Some("Quarantine: 2 rejected segment(s) held, oldest 5d")
        );
    }
    // AC: injected now clamps future mtimes to zero age.
    #[test]
    fn injected_now() {
        let t = tempfile::tempdir().unwrap();
        let failed = segment(t.path(), "20260101", "120000.failed");
        set_mtime(&failed, 200.0);
        assert_eq!(
            compute_quarantine_stats(t.path(), 100.0).oldest_age_seconds,
            Some(0.0)
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
        assert_eq!(
            capture_stats_after_walk(1, 2 * 1024 * 1024, Err(std::io::Error::other("mid-walk"))),
            CaptureStats {
                captures_today: 1,
                total_size_mb: 2
            }
        );
        assert_eq!(
            quarantine_stats_after_walk(
                2,
                Some(500.0),
                1000.0,
                Err(std::io::Error::other("mid-walk"))
            ),
            QuarantineStats {
                count: 2,
                oldest_age_seconds: Some(500.0)
            }
        );
    }
}
