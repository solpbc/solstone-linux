// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What sync and status read from the engine's layout.
//!
//! A finished segment is `captures/<day>/_tmux/HHMMSS_LLL/` with its capture time
//! in the sibling `HHMMSS_LLL.zone.json`. The engine's finalize renames the
//! `HHMMSS.incomplete` directory first, then writes the zone file, then removes
//! `HHMMSS.incomplete.meta`; until the meta is gone a segment is not deliverable,
//! so nothing uploads without its capture time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::recovery::CaptureZone;

const ZONE_SUFFIX: &str = ".zone.json";
const INCOMPLETE_SUFFIX: &str = ".incomplete";
const META_SUFFIX: &str = ".incomplete.meta";
const FAILED_SUFFIX: &str = ".failed";

fn segment_name(segment_dir: &Path) -> Option<&str> {
    segment_dir.file_name().and_then(|name| name.to_str())
}

/// `HHMMSS` of `HHMMSS_LLL`.
fn segment_stem(name: &str) -> &str {
    name.split_once('_').map_or(name, |(stem, _)| stem)
}

/// The sibling zone file of a finished segment.
pub fn zone_path(segment_dir: &Path) -> Option<PathBuf> {
    let name = segment_name(segment_dir)?;
    Some(segment_dir.with_file_name(format!("{name}{ZONE_SUFFIX}")))
}

/// The sibling meta of the segment's unfinished form.
pub fn meta_path(segment_dir: &Path) -> Option<PathBuf> {
    let name = segment_name(segment_dir)?;
    Some(segment_dir.with_file_name(format!("{}{META_SUFFIX}", segment_stem(name))))
}

/// A finished segment whose meta still exists: its finalize has not completed.
pub fn awaiting_capture_time(segment_dir: &Path) -> bool {
    meta_path(segment_dir).is_some_and(|meta| fs::symlink_metadata(meta).is_ok())
}

/// The segment's capture time, read from its sibling zone file in the engine's
/// format. `None` when absent or unreadable; the segment then goes without `meta`.
pub fn read_zone(segment_dir: &Path) -> Option<CaptureZone> {
    let path = zone_path(segment_dir)?;
    match solstone_terminal::storage::load_capture_time(&path) {
        solstone_terminal::storage::CaptureTimeLoad::Present(time) => Some(CaptureZone {
            tz: time.tz,
            utc_offset_seconds: time.utc_offset_seconds,
        }),
        _ => None,
    }
}

/// Removes the segment's zone file; an absent one is not an error.
pub fn remove_zone(segment_dir: &Path) -> io::Result<()> {
    let Some(path) = zone_path(segment_dir) else {
        return Ok(());
    };
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Terminal segments on this computer, as `terminal status` reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalCounts {
    /// Finished and waiting to go into the journal.
    pub waiting: u64,
    /// Unfinished, waiting for the engine's start-up recovery.
    pub awaiting_recovery: u64,
    /// Kept by the engine's recovery: a retained `.incomplete`, a failed finalize
    /// or a quarantined `.failed`.
    pub held: u64,
}

/// Counts the segments under every `captures/<day>/_tmux/`. While the engine runs,
/// an `.incomplete` with its meta is the open segment and counts as nothing.
pub fn count_segments(captures_dir: &Path, engine_running: bool) -> TerminalCounts {
    let mut counts = TerminalCounts::default();
    let Ok(days) = fs::read_dir(captures_dir) else {
        return counts;
    };
    for day in days.flatten() {
        let stream = day.path().join(super::STREAM_DIR);
        let Ok(entries) = fs::read_dir(&stream) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.ends_with(FAILED_SUFFIX) {
                counts.held += 1;
            } else if let Some(stem) = name.strip_suffix(INCOMPLETE_SUFFIX) {
                let has_meta = stream.join(format!("{stem}{META_SUFFIX}")).exists();
                if !has_meta {
                    counts.held += 1;
                } else if !engine_running {
                    counts.awaiting_recovery += 1;
                }
            } else if awaiting_capture_time(&entry.path()) {
                if !engine_running {
                    counts.awaiting_recovery += 1;
                }
            } else {
                counts.waiting += 1;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(root: &Path) -> PathBuf {
        let stream = root.join("captures/20261108/_tmux");
        fs::create_dir_all(&stream).unwrap();
        stream
    }

    #[test]
    fn sibling_paths_follow_the_engine_names() {
        let segment = Path::new("/c/20261108/_tmux/101500_300");
        assert_eq!(
            zone_path(segment).unwrap(),
            PathBuf::from("/c/20261108/_tmux/101500_300.zone.json")
        );
        assert_eq!(
            meta_path(segment).unwrap(),
            PathBuf::from("/c/20261108/_tmux/101500.incomplete.meta")
        );
    }

    #[test]
    fn zone_reads_the_engine_format_and_ignores_garbage() {
        let temp = tempfile::tempdir().unwrap();
        let stream = stream(temp.path());
        let segment = stream.join("101500_300");
        fs::create_dir(&segment).unwrap();
        assert_eq!(read_zone(&segment), None);
        fs::write(
            stream.join("101500_300.zone.json"),
            br#"{"tz":"Asia/Kolkata","utc_offset_seconds":19800}"#,
        )
        .unwrap();
        assert_eq!(
            read_zone(&segment),
            Some(CaptureZone {
                tz: Some("Asia/Kolkata".into()),
                utc_offset_seconds: 19800
            })
        );
        fs::write(stream.join("101500_300.zone.json"), b"{").unwrap();
        assert_eq!(read_zone(&segment), None);
        remove_zone(&segment).unwrap();
        remove_zone(&segment).unwrap();
        assert!(!stream.join("101500_300.zone.json").exists());
    }

    #[test]
    fn counts_split_waiting_recovery_and_held() {
        let temp = tempfile::tempdir().unwrap();
        let stream = stream(temp.path());
        // Finished and deliverable.
        fs::create_dir(stream.join("100000_300")).unwrap();
        // Renamed, meta still beside it.
        fs::create_dir(stream.join("100500_300")).unwrap();
        fs::write(stream.join("100500.incomplete.meta"), b"{}").unwrap();
        // A crash: the open segment with its meta.
        fs::create_dir(stream.join("101000.incomplete")).unwrap();
        fs::write(stream.join("101000.incomplete.meta"), b"{}").unwrap();
        // Retained by recovery: no meta.
        fs::create_dir(stream.join("101500.incomplete")).unwrap();
        // Quarantined.
        fs::create_dir(stream.join("102000.failed")).unwrap();

        let stopped = count_segments(&temp.path().join("captures"), false);
        assert_eq!(
            stopped,
            TerminalCounts {
                waiting: 1,
                awaiting_recovery: 2,
                held: 2
            }
        );
        let running = count_segments(&temp.path().join("captures"), true);
        assert_eq!(
            running,
            TerminalCounts {
                waiting: 1,
                awaiting_recovery: 0,
                held: 2
            }
        );
    }
}
