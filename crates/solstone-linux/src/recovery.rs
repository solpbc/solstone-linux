// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::segment::clamp_duration;
use claxon::{FlacReader, FlacReaderOptions};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub(crate) const METADATA_FILENAME: &str = ".metadata";
const MINIMUM_AGE_SECONDS: f64 = 120.0;

pub trait MediaDurationProbe {
    fn duration(&self, path: &Path) -> Option<f64>;
}

pub struct ClaxonMediaDurationProbe;

impl MediaDurationProbe for ClaxonMediaDurationProbe {
    fn duration(&self, path: &Path) -> Option<f64> {
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case("flac"))
        {
            return None;
        }
        let reader = FlacReader::open_ext(
            path,
            FlacReaderOptions {
                metadata_only: true,
                read_vorbis_comment: false,
            },
        )
        .ok()?;
        let info = reader.streaminfo();
        // Claxon represents an unknown total as None; that candidate is unreadable for this probe.
        stream_duration(info.samples, info.sample_rate)
    }
}

fn stream_duration(samples: Option<u64>, sample_rate: u32) -> Option<f64> {
    if sample_rate == 0 {
        return None;
    }
    Some(samples? as f64 / f64::from(sample_rate))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct SegmentProgress {
    pub has_durable_media: bool,
    pub durable_byte_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_durable_write_at: Option<f64>,
}

#[derive(Serialize)]
struct SegmentMetadataFile {
    start_timestamp: f64,
    #[serde(flatten)]
    progress: SegmentProgress,
}

pub fn write_segment_metadata(segment_dir: &Path, start_timestamp: f64, progress: SegmentProgress) {
    let document = SegmentMetadataFile {
        start_timestamp,
        progress,
    };
    let Ok(mut text) = serde_json::to_string(&document) else {
        tracing::warn!("Failed to write segment metadata");
        return;
    };
    text.push('\n');
    if let Err(error) = fs::write(segment_dir.join(METADATA_FILENAME), text) {
        tracing::warn!("Failed to write segment metadata: {error}");
    }
}

// Existence of a non-`.metadata` regular file, not byte_count > 0 — a 0-byte leftover
// is media, matching finalize_segment. Not shared with finalize_segment (boolean any-file
// after deleting `.metadata`) or recover_segment (every non-`.metadata` entry, including dirs).
pub fn scan_segment_progress(segment_dir: &Path) -> (bool, u64) {
    let Ok(entries) = fs::read_dir(segment_dir) else {
        return (false, 0);
    };
    let mut has_durable_media = false;
    let mut durable_byte_count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name == METADATA_FILENAME)
        {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        has_durable_media = true;
        durable_byte_count += fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    }
    (has_durable_media, durable_byte_count)
}

pub fn read_segment_start(segment_dir: &Path) -> Option<f64> {
    let value: Value =
        serde_json::from_str(&fs::read_to_string(segment_dir.join(METADATA_FILENAME)).ok()?)
            .ok()?;
    // Wrong-typed timestamps deliberately degrade to the filesystem fallback; Rust keeps this boundary typed.
    value.get("start_timestamp")?.as_f64()
}

fn metadata_mtime(metadata: &fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime() as f64 + metadata.mtime_nsec() as f64 / 1e9
}

fn metadata_ctime(metadata: &fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ctime() as f64 + metadata.ctime_nsec() as f64 / 1e9
}

fn filesystem_duration(mtime: f64, ctime: f64, ceiling: u64) -> u64 {
    // Pinned by Python: filesystem fallback is mtime minus ctime, never now minus ctime.
    clamp_duration(mtime - ctime, ceiling)
}

fn candidate_is_old_enough(mtime: Result<f64, ()>, now: f64) -> bool {
    // A stat failure cannot be renamed safely either; skip it so the next startup retries.
    mtime.is_ok_and(|mtime| now - mtime >= MINIMUM_AGE_SECONDS)
}

fn readable_media_duration(paths: &[PathBuf], probe: &dyn MediaDurationProbe) -> Option<f64> {
    paths
        .iter()
        .filter_map(|path| probe.duration(path))
        .reduce(f64::max)
}

pub fn recover_incomplete_segments(
    root: &Path,
    ceiling: u64,
    now: f64,
    probe: &dyn MediaDurationProbe,
) -> u64 {
    if !root.exists() {
        return 0;
    }
    let Some(days) = read_sorted_entries(root, "captures root") else {
        return 0;
    };
    let mut recovered = 0;
    for day in days {
        if !day.path().is_dir() {
            continue;
        }
        let Some(streams) = read_sorted_entries(&day.path(), "day directory") else {
            continue;
        };
        for stream in streams {
            if !stream.path().is_dir() {
                continue;
            }
            let Some(segments) = read_sorted_entries(&stream.path(), "stream directory") else {
                continue;
            };
            for segment in segments {
                if !segment.path().is_dir()
                    || !segment
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".incomplete")
                {
                    continue;
                }
                let mtime = segment
                    .metadata()
                    .map(|metadata| metadata_mtime(&metadata))
                    .map_err(|_| ());
                if !candidate_is_old_enough(mtime, now) {
                    continue;
                }
                tracing::info!(
                    "Recovering incomplete segment: {}",
                    segment.file_name().to_string_lossy()
                );
                if recover_segment(&segment.path(), ceiling, now, probe) {
                    recovered += 1;
                }
            }
        }
    }
    if recovered > 0 {
        tracing::info!("Recovered {recovered} incomplete segment(s)");
    }
    recovered
}

fn read_sorted_entries(path: &Path, label: &str) -> Option<Vec<fs::DirEntry>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!("Failed to read {label} {}: {error}", path.display());
            return None;
        }
    };
    let mut collected = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => collected.push(entry),
            Err(error) => tracing::error!(
                "Failed to read entry in {label} {}: {error}",
                path.display()
            ),
        }
    }
    collected.sort_by_key(|entry| entry.file_name());
    Some(collected)
}

fn mark_failed(segment_dir: &Path, now: f64) -> bool {
    let Some(prefix) = segment_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".incomplete"))
    else {
        return false;
    };
    let failed = segment_dir.with_file_name(format!("{prefix}.failed"));
    match fs::rename(segment_dir, &failed) {
        Ok(()) => {
            let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now.max(0.0));
            match File::open(&failed)
                .and_then(|directory| directory.set_times(FileTimes::new().set_modified(stamp)))
            {
                Ok(()) => {}
                Err(error) => tracing::warn!(
                    "Failed to stamp quarantine time for {}: {error}",
                    failed.display()
                ),
            }
            tracing::warn!(
                "Marked as failed: {} -> {}",
                segment_dir.display(),
                failed.display()
            );
        }
        Err(error) => tracing::error!(
            "Failed to mark {} as failed: {error}",
            segment_dir.display()
        ),
    }
    false
}

fn recover_segment(
    segment_dir: &Path,
    ceiling: u64,
    now: f64,
    probe: &dyn MediaDurationProbe,
) -> bool {
    let name = segment_dir.file_name().unwrap().to_string_lossy();
    let prefix = name.strip_suffix(".incomplete").unwrap();
    let mut duration = if let Some(start) = read_segment_start(segment_dir) {
        clamp_duration(now - start, ceiling)
    } else {
        let Ok(metadata) = fs::metadata(segment_dir) else {
            return mark_failed(segment_dir, now);
        };
        filesystem_duration(
            metadata_mtime(&metadata),
            metadata_ctime(&metadata),
            ceiling,
        )
    };
    let Ok(entries) = fs::read_dir(segment_dir) else {
        return mark_failed(segment_dir, now);
    };
    // Unlike observer._finalize_segment, every non-metadata entry counts, including subdirectories.
    let contents: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name != METADATA_FILENAME)
        })
        .collect();
    if contents.is_empty() {
        tracing::warn!("Empty incomplete segment: {name}");
        return mark_failed(segment_dir, now);
    }
    if let Some(readable) = readable_media_duration(&contents, probe) {
        duration = duration.min((readable as u64).max(1));
    }
    let _ = fs::remove_file(segment_dir.join(METADATA_FILENAME));
    let final_dir = segment_dir.with_file_name(format!("{prefix}_{duration}"));
    match fs::rename(segment_dir, &final_dir) {
        Ok(()) => {
            tracing::info!("Recovered: {name} -> {}", final_dir.display());
            true
        }
        Err(error) => {
            tracing::warn!("Failed to rename {name}: {error}");
            mark_failed(segment_dir, now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoMedia;
    impl MediaDurationProbe for NoMedia {
        fn duration(&self, _: &Path) -> Option<f64> {
            None
        }
    }
    struct FixedMedia(f64);
    impl MediaDurationProbe for FixedMedia {
        fn duration(&self, path: &Path) -> Option<f64> {
            path.is_file().then_some(self.0)
        }
    }

    fn incomplete(root: &Path, name: &str, now: f64) -> PathBuf {
        let path = root
            .join("20260403/archon")
            .join(format!("{name}.incomplete"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("screen.webm"), b"x").unwrap();
        let directory = File::open(&path).unwrap();
        directory
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now - 300.0)),
            )
            .unwrap();
        path
    }
    fn names(root: &Path) -> Vec<String> {
        fs::read_dir(root.join("20260403/archon"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }
    fn age(path: &Path, now: f64) {
        File::open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now - 300.0)),
            )
            .unwrap();
    }

    // tests/test_sync.py::TestRecovery::test_recovers_old_incomplete
    #[test]
    fn recovers_old() {
        let t = tempfile::tempdir().unwrap();
        incomplete(t.path(), "140000", 1000.0);
        assert_eq!(
            recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia),
            1
        );
        assert!(
            names(t.path())
                .iter()
                .any(|name| name.starts_with("140000_"))
        );
    }
    // tests/test_sync.py::TestRecovery::test_recovers_with_metadata
    #[test]
    fn metadata_duration() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        write_segment_metadata(&path, 940.0, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia);
        assert!(path.with_file_name("140000_60").exists());
    }
    // tests/test_sync.py::TestRecovery::test_recovery_metadata_duration_clamps_to_window_ceiling
    #[test]
    fn metadata_ceiling() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        write_segment_metadata(&path, 0.0, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 60, 1000.0, &NoMedia);
        assert!(path.with_file_name("140000_60").exists());
    }
    // tests/test_sync.py::TestRecovery::test_recovery_filesystem_fallback_duration_clamps_to_window_ceiling
    #[test]
    fn filesystem_ceiling() {
        assert_eq!(filesystem_duration(1000.0, 0.0, 60), 60);
    }
    // tests/test_sync.py::TestRecovery::test_recovery_bounds_duration_by_readable_flac
    #[test]
    fn real_flac_bounds_duration() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/silence-4s-16khz.flac"
            ),
            path.join("audio.flac"),
        )
        .unwrap();
        write_segment_metadata(&path, 0.0, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 300, 1000.0, &ClaxonMediaDurationProbe);
        assert!(path.with_file_name("140000_4").exists());
    }
    // tests/test_sync.py::TestRecovery::test_recovery_webm_only_uses_ceiling_clamped_elapsed
    #[test]
    fn webm_uses_ceiling() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        write_segment_metadata(&path, 0.0, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 60, 1000.0, &NoMedia);
        assert!(path.with_file_name("140000_60").exists());
    }
    // tests/test_sync.py::TestRecovery::test_skips_recent_incomplete
    #[test]
    fn skips_recent() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        File::open(&path)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(950)),
            )
            .unwrap();
        assert_eq!(
            recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia),
            0
        );
        assert!(path.exists());
    }
    // tests/test_sync.py::TestRecovery::test_marks_empty_as_failed
    #[test]
    fn empty_is_failed() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        fs::remove_file(path.join("screen.webm")).unwrap();
        age(&path, 1000.0);
        assert_eq!(
            recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia),
            0
        );
        assert!(path.with_file_name("140000.failed").exists());
    }
    // tests/test_sync.py::TestRecovery::test_mark_failed_stamps_quarantine_mtime
    #[test]
    fn failed_stamp() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        fs::remove_file(path.join("screen.webm")).unwrap();
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia);
        let stamp = metadata_mtime(&fs::metadata(path.with_file_name("140000.failed")).unwrap());
        assert!((stamp - 1000.0).abs() < 0.01);
    }
    // tests/test_sync.py::TestRecovery::test_metadata_removed_on_recovery
    #[test]
    fn metadata_removed() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        write_segment_metadata(&path, 940.0, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia);
        assert!(
            !path
                .with_file_name("140000_60")
                .join(METADATA_FILENAME)
                .exists()
        );
    }
    // tests/test_sync.py::TestRecovery::test_no_captures_dir
    #[test]
    fn no_root() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            recover_incomplete_segments(&t.path().join("missing"), 300, 1000.0, &NoMedia),
            0
        );
    }
    // AC: both duration paths and subsecond media floor at one.
    #[test]
    fn floors() {
        assert_eq!(clamp_duration(0.5, 300), 1);
        assert_eq!(filesystem_duration(0.5, 0.0, 300), 1);
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        write_segment_metadata(&path, 999.5, SegmentProgress::default());
        age(&path, 1000.0);
        recover_incomplete_segments(t.path(), 300, 1000.0, &FixedMedia(0.5));
        assert!(path.with_file_name("140000_1").exists());
    }
    // AC: failed rename quarantines that segment and scanning continues.
    #[test]
    fn rename_failure_continues() {
        let t = tempfile::tempdir().unwrap();
        let first = incomplete(t.path(), "120000", 1000.0);
        write_segment_metadata(&first, 940.0, SegmentProgress::default());
        age(&first, 1000.0);
        let collision = first.with_file_name("120000_60");
        fs::create_dir(&collision).unwrap();
        fs::write(collision.join("occupied"), b"x").unwrap();
        let second = incomplete(t.path(), "130000", 1000.0);
        write_segment_metadata(&second, 940.0, SegmentProgress::default());
        age(&second, 1000.0);
        assert_eq!(
            recover_incomplete_segments(t.path(), 300, 1000.0, &NoMedia),
            1
        );
        assert!(first.with_file_name("120000.failed").exists());
        assert!(second.with_file_name("130000_60").exists());
    }
    // AC: stat errors skip rather than quarantine.
    #[test]
    fn stat_error_skips() {
        assert!(!candidate_is_old_enough(Err(()), 1000.0));
    }
    // AC: subdirectories count as content.
    #[test]
    fn subdirectory_is_content() {
        let t = tempfile::tempdir().unwrap();
        let path = incomplete(t.path(), "140000", 1000.0);
        fs::remove_file(path.join("screen.webm")).unwrap();
        fs::create_dir(path.join("nested")).unwrap();
        write_segment_metadata(&path, 940.0, SegmentProgress::default());
        assert!(recover_segment(&path, 300, 1000.0, &NoMedia));
    }
    // AC: media candidates use max duration and swallow individual failures.
    #[test]
    fn probe_max_and_errors() {
        struct Probe;
        impl MediaDurationProbe for Probe {
            fn duration(&self, path: &Path) -> Option<f64> {
                match path.file_name()?.to_str()? {
                    "a.flac" => Some(2.0),
                    "b.flac" => Some(4.0),
                    _ => None,
                }
            }
        }
        let paths = vec!["bad.flac", "a.flac", "b.flac"]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        assert_eq!(readable_media_duration(&paths, &Probe), Some(4.0));
    }
    // AC: Claxon accepts case-insensitive FLAC suffixes and rejects directories.
    #[test]
    fn probe_file_filter() {
        let t = tempfile::tempdir().unwrap();
        let upper = t.path().join("A.FLAC");
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/silence-4s-16khz.flac"
            ),
            &upper,
        )
        .unwrap();
        assert_eq!(ClaxonMediaDurationProbe.duration(&upper), Some(4.0));
        let dir = t.path().join("D.flac");
        fs::create_dir(&dir).unwrap();
        assert_eq!(ClaxonMediaDurationProbe.duration(&dir), None);
    }
    // AC: zero sample rates are rejected by the probe contract.
    #[test]
    fn zero_rate_guard() {
        assert_eq!(stream_duration(Some(64_000), 0), None);
        assert_eq!(stream_duration(None, 16_000), None);
    }

    // Python Path.is_dir follows symlinked capture hierarchy directories.
    #[test]
    fn follows_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let t = tempfile::tempdir().unwrap();
        let captures = t.path().join("captures");
        let actual_day = t.path().join("actual-day");
        let segment = actual_day.join("archon/140000.incomplete");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), b"x").unwrap();
        write_segment_metadata(&segment, 940.0, SegmentProgress::default());
        age(&segment, 1000.0);
        fs::create_dir(&captures).unwrap();
        symlink(&actual_day, captures.join("20260403")).unwrap();

        assert_eq!(
            recover_incomplete_segments(&captures, 300, 1000.0, &NoMedia),
            1
        );
        assert!(actual_day.join("archon/140000_60").exists());
    }

    // Existence, not byte_count > 0: a 0-byte leftover is durable media (D3).
    #[test]
    fn scan_zero_byte_file_is_media() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join(METADATA_FILENAME), b"{}\n").unwrap();
        fs::write(t.path().join("leftover.bin"), b"").unwrap();
        assert_eq!(scan_segment_progress(t.path()), (true, 0));
    }

    // AC5: in-flight legacy sidecars from the previous writer still recover.
    #[test]
    fn legacy_sidecar_reads_and_recovers() {
        const LEGACY: &[u8] = b"{\"start_timestamp\":1700000000.00000000}";
        assert_eq!(LEGACY.len(), 39);

        let t = tempfile::tempdir().unwrap();
        let now = 1_700_000_060.0;
        let path = incomplete(t.path(), "140000", now);
        fs::write(path.join(METADATA_FILENAME), LEGACY).unwrap();
        age(&path, now);
        assert_eq!(read_segment_start(&path), Some(1_700_000_000.0));
        assert_eq!(recover_incomplete_segments(t.path(), 300, now, &NoMedia), 1);
        assert!(path.with_file_name("140000_60").exists());

        let extra = t.path().join("extra");
        fs::create_dir(&extra).unwrap();
        fs::write(
            extra.join(METADATA_FILENAME),
            br#"{"start_timestamp":1700000000.00000000,"unknown":true,"also":1}"#,
        )
        .unwrap();
        assert_eq!(read_segment_start(&extra), Some(1_700_000_000.0));
    }

    #[test]
    fn metadata_document_byte_shape() {
        let t = tempfile::tempdir().unwrap();
        write_segment_metadata(t.path(), 1234.5, SegmentProgress::default());
        assert_eq!(
            fs::read_to_string(t.path().join(METADATA_FILENAME)).unwrap(),
            "{\"start_timestamp\":1234.5,\"has_durable_media\":false,\"durable_byte_count\":0}\n"
        );
        write_segment_metadata(
            t.path(),
            1234.5,
            SegmentProgress {
                has_durable_media: true,
                durable_byte_count: 4,
                last_durable_write_at: Some(5678.5),
            },
        );
        assert_eq!(
            fs::read_to_string(t.path().join(METADATA_FILENAME)).unwrap(),
            "{\"start_timestamp\":1234.5,\"has_durable_media\":true,\"durable_byte_count\":4,\"last_durable_write_at\":5678.5}\n"
        );
    }
}
