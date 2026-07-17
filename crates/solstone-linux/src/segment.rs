// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use chrono::{Local, LocalResult, TimeZone};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn timestamp_parts(timestamp: f64) -> (String, String) {
    let seconds = timestamp.floor() as i64;
    let nanos = ((timestamp - timestamp.floor()) * 1e9) as u32;
    let datetime = match Local.timestamp_opt(seconds, nanos) {
        LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => value,
        LocalResult::None => Local
            .timestamp_opt(seconds, 0)
            .earliest()
            .expect("Unix timestamp must be representable"),
    };
    (
        datetime.format("%Y%m%d").to_string(),
        datetime.format("%H%M%S").to_string(),
    )
}

pub fn clamp_duration(elapsed: f64, ceiling: u64) -> u64 {
    if ceiling == 0 {
        return 1;
    }
    (elapsed as i64).clamp(1, ceiling as i64) as u64
}

pub fn segment_key(time_prefix: &str, duration: u64) -> String {
    format!("{time_prefix}_{duration}")
}

pub fn finalize_segment_dir(incomplete: &Path, key: &str) -> io::Result<PathBuf> {
    let destination = incomplete.with_file_name(key);
    fs::rename(incomplete, &destination)?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::{read_segment_start, write_segment_metadata};

    // observer.py::_get_timestamp_parts shape contract.
    #[test]
    fn timestamp_shape() {
        let (date, time) = timestamp_parts(1_700_000_000.0);
        assert_eq!(date.len(), 8);
        assert_eq!(time.len(), 6);
        assert!(
            date.bytes()
                .chain(time.bytes())
                .all(|byte| byte.is_ascii_digit())
        );
    }
    // AC: duration clamps at one and at the configured ceiling.
    #[test]
    fn duration_clamps() {
        assert_eq!(clamp_duration(0.5, 300), 1);
        assert_eq!(clamp_duration(999.0, 300), 300);
        assert_eq!(clamp_duration(1.0, 0), 1);
    }
    // observer.py::_finalize_segment same-directory atomic rename.
    #[test]
    fn same_directory_finalize() {
        let t = tempfile::tempdir().unwrap();
        let incomplete = t.path().join("120000.incomplete");
        fs::create_dir(&incomplete).unwrap();
        let final_dir = finalize_segment_dir(&incomplete, "120000_5").unwrap();
        assert_eq!(final_dir, t.path().join("120000_5"));
        assert!(!incomplete.exists());
        assert!(final_dir.exists());
    }
    // observer.py::_finalize_segment unpadded duration suffix.
    #[test]
    fn unpadded_keys() {
        assert_eq!(segment_key("120000", 5), "120000_5");
        assert_eq!(segment_key("120000", 300), "120000_300");
    }
    // recovery.py metadata writer/parser compatibility.
    #[test]
    fn metadata_round_trip() {
        let t = tempfile::tempdir().unwrap();
        write_segment_metadata(t.path(), 1234.5);
        assert_eq!(read_segment_start(t.path()), Some(1234.5));
    }
}
