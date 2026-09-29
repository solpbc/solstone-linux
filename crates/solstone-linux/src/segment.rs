// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use chrono::{DateTime, Local, LocalResult, Offset, TimeZone};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const ZONE_INFO_DIRECTORIES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/share/zoneinfo",
    "/etc/zoneinfo",
    "/usr/share/lib/zoneinfo",
];

fn local_datetime(timestamp: f64) -> DateTime<Local> {
    let seconds = timestamp.floor() as i64;
    let nanos = ((timestamp - timestamp.floor()) * 1e9) as u32;
    match Local.timestamp_opt(seconds, nanos) {
        LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => value,
        LocalResult::None => Local
            .timestamp_opt(seconds, 0)
            .earliest()
            .expect("Unix timestamp must be representable"),
    }
}

pub fn timestamp_parts(timestamp: f64) -> (String, String) {
    let datetime = local_datetime(timestamp);
    (
        datetime.format("%Y%m%d").to_string(),
        datetime.format("%H%M%S").to_string(),
    )
}

pub(crate) fn local_offset_seconds(timestamp: f64) -> i32 {
    local_datetime(timestamp).offset().fix().local_minus_utc()
}

fn strip_zoneinfo_alias(mut name: &str) -> &str {
    loop {
        if let Some(rest) = name.strip_prefix("posix/") {
            name = rest;
        } else if let Some(rest) = name.strip_prefix("right/") {
            name = rest;
        } else {
            break;
        }
    }
    name
}

fn is_valid_iana_component(component: &str) -> bool {
    !component.is_empty()
        && component
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-')
}

fn iana_zone_name(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with(':') || raw.contains(',') {
        return None;
    }
    let stripped = strip_zoneinfo_alias(raw);
    let components: Vec<&str> = stripped.split('/').collect();
    if !components.iter().all(|&c| is_valid_iana_component(c)) {
        return None;
    }
    let is_slashless_allowed = stripped == "UTC" || stripped == "GMT";
    if !stripped.contains('/') && !is_slashless_allowed {
        return None;
    }
    let file_exists = ZONE_INFO_DIRECTORIES.iter().any(|&dir| {
        let path = Path::new(dir).join(stripped);
        path.is_file()
    });
    if !file_exists {
        return None;
    }
    Some(stripped.to_owned())
}

fn zone_name_from_localtime_target(target: &Path) -> Option<String> {
    let resolved = if target.is_relative() {
        Path::new("/etc").join(target)
    } else {
        target.to_path_buf()
    };
    let resolved_str = resolved.to_str()?;
    for &dir in &ZONE_INFO_DIRECTORIES {
        let prefix = if dir.ends_with('/') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };
        if let Some(remainder) = resolved_str.strip_prefix(&prefix) {
            return iana_zone_name(remainder);
        }
    }
    None
}

pub(crate) fn capture_tz_name() -> Option<String> {
    match std::env::var_os("TZ") {
        None => {
            let target = fs::read_link("/etc/localtime").ok()?;
            zone_name_from_localtime_target(&target)
        }
        Some(val) => {
            let s = val.to_str()?;
            if s.is_empty() {
                return None;
            }
            iana_zone_name(s)
        }
    }
}

pub fn clamp_duration(elapsed: f64, ceiling: u64) -> u64 {
    if ceiling == 0 {
        return 1;
    }
    let ceiling = ceiling.min(i64::MAX as u64) as i64;
    (elapsed as i64).clamp(1, ceiling) as u64
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CivilTime {
    pub day: String,
    pub hms: String,
    pub utc_offset_seconds: i32,
}

#[cfg(test)]
pub(crate) fn civil_time_in_zoneinfo(zone: &str, unix_seconds: i64) -> Option<CivilTime> {
    let stripped = strip_zoneinfo_alias(zone);
    let mut file_bytes = None;
    for &dir in &ZONE_INFO_DIRECTORIES {
        let p = Path::new(dir).join(stripped);
        if let Ok(bytes) = fs::read(&p) {
            file_bytes = Some(bytes);
            break;
        }
    }
    let bytes = file_bytes?;
    if bytes.len() < 44 || &bytes[0..4] != b"TZif" {
        return None;
    }
    let version = bytes[4];
    let parse_v1_body_len = |data: &[u8]| -> Option<usize> {
        if data.len() < 44 || &data[0..4] != b"TZif" {
            return None;
        }
        let isutcnt = u32::from_be_bytes(data[20..24].try_into().ok()?) as usize;
        let isstdcnt = u32::from_be_bytes(data[24..28].try_into().ok()?) as usize;
        let leapcnt = u32::from_be_bytes(data[28..32].try_into().ok()?) as usize;
        let timecnt = u32::from_be_bytes(data[32..36].try_into().ok()?) as usize;
        let typecnt = u32::from_be_bytes(data[36..40].try_into().ok()?) as usize;
        let charcnt = u32::from_be_bytes(data[40..44].try_into().ok()?) as usize;
        let body_len =
            timecnt * 4 + timecnt + typecnt * 6 + charcnt + leapcnt * 8 + isstdcnt + isutcnt;
        if data.len() < 44 + body_len {
            return None;
        }
        Some(body_len)
    };

    let utoff = if matches!(version, b'2' | b'3' | b'4') {
        let v1_body_len = parse_v1_body_len(&bytes)?;
        let v2_data = &bytes[44 + v1_body_len..];
        if v2_data.len() < 44 || &v2_data[0..4] != b"TZif" {
            return None;
        }
        let _isutcnt = u32::from_be_bytes(v2_data[20..24].try_into().ok()?) as usize;
        let _isstdcnt = u32::from_be_bytes(v2_data[24..28].try_into().ok()?) as usize;
        let _leapcnt = u32::from_be_bytes(v2_data[28..32].try_into().ok()?) as usize;
        let timecnt = u32::from_be_bytes(v2_data[32..36].try_into().ok()?) as usize;
        let typecnt = u32::from_be_bytes(v2_data[36..40].try_into().ok()?) as usize;
        let _charcnt = u32::from_be_bytes(v2_data[40..44].try_into().ok()?) as usize;

        let mut offset = 44;
        let times_len = timecnt * 8;
        if v2_data.len() < offset + times_len + timecnt + typecnt * 6 {
            return None;
        }
        let mut transitions = Vec::with_capacity(timecnt);
        for i in 0..timecnt {
            let start = offset + i * 8;
            let t = i64::from_be_bytes(v2_data[start..start + 8].try_into().ok()?);
            transitions.push(t);
        }
        offset += times_len;
        let type_indices = &v2_data[offset..offset + timecnt];
        offset += timecnt;

        let mut types = Vec::with_capacity(typecnt);
        for i in 0..typecnt {
            let start = offset + i * 6;
            let utoff = i32::from_be_bytes(v2_data[start..start + 4].try_into().ok()?);
            types.push(utoff);
        }

        if timecnt == 0 || unix_seconds < transitions[0] {
            if types.is_empty() {
                return None;
            }
            types[0]
        } else {
            let mut selected_idx = 0;
            for (idx, &t) in transitions.iter().enumerate() {
                if t <= unix_seconds {
                    selected_idx = idx;
                } else {
                    break;
                }
            }
            let type_idx = type_indices[selected_idx] as usize;
            if type_idx >= types.len() {
                return None;
            }
            types[type_idx]
        }
    } else {
        let _isutcnt = u32::from_be_bytes(bytes[20..24].try_into().ok()?) as usize;
        let _isstdcnt = u32::from_be_bytes(bytes[24..28].try_into().ok()?) as usize;
        let _leapcnt = u32::from_be_bytes(bytes[28..32].try_into().ok()?) as usize;
        let timecnt = u32::from_be_bytes(bytes[32..36].try_into().ok()?) as usize;
        let typecnt = u32::from_be_bytes(bytes[36..40].try_into().ok()?) as usize;
        let _charcnt = u32::from_be_bytes(bytes[40..44].try_into().ok()?) as usize;

        let mut offset = 44;
        let times_len = timecnt * 4;
        if bytes.len() < offset + times_len + timecnt + typecnt * 6 {
            return None;
        }
        let mut transitions = Vec::with_capacity(timecnt);
        for i in 0..timecnt {
            let start = offset + i * 4;
            let t = i32::from_be_bytes(bytes[start..start + 4].try_into().ok()?);
            transitions.push(t as i64);
        }
        offset += times_len;
        let type_indices = &bytes[offset..offset + timecnt];
        offset += timecnt;

        let mut types = Vec::with_capacity(typecnt);
        for i in 0..typecnt {
            let start = offset + i * 6;
            let utoff = i32::from_be_bytes(bytes[start..start + 4].try_into().ok()?);
            types.push(utoff);
        }

        if timecnt == 0 || unix_seconds < transitions[0] {
            if types.is_empty() {
                return None;
            }
            types[0]
        } else {
            let mut selected_idx = 0;
            for (idx, &t) in transitions.iter().enumerate() {
                if t <= unix_seconds {
                    selected_idx = idx;
                } else {
                    break;
                }
            }
            let type_idx = type_indices[selected_idx] as usize;
            if type_idx >= types.len() {
                return None;
            }
            types[type_idx]
        }
    };

    let naive = chrono::DateTime::from_timestamp(unix_seconds + i64::from(utoff), 0)?.naive_utc();
    Some(CivilTime {
        day: naive.format("%Y%m%d").to_string(),
        hms: naive.format("%H%M%S").to_string(),
        utc_offset_seconds: utoff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::{SegmentProgress, read_segment_start, write_segment_metadata};

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
    // duration clamps at one and at the configured ceiling.
    #[test]
    fn duration_clamps() {
        assert_eq!(clamp_duration(0.5, 300), 1);
        assert_eq!(clamp_duration(999.0, 300), 300);
        assert_eq!(clamp_duration(1.0, 0), 1);
        assert_eq!(clamp_duration(f64::MAX, u64::MAX), i64::MAX as u64);
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
        write_segment_metadata(t.path(), 1234.5, SegmentProgress::default());
        assert_eq!(read_segment_start(t.path()), Some(1234.5));
    }

    #[test]
    fn iana_zone_classifier() {
        assert_eq!(
            iana_zone_name("America/Denver"),
            Some("America/Denver".into())
        );
        assert_eq!(iana_zone_name("Asia/Kolkata"), Some("Asia/Kolkata".into()));
        assert_eq!(
            iana_zone_name("posix/America/Denver"),
            Some("America/Denver".into())
        );
        assert_eq!(
            iana_zone_name("right/America/Denver"),
            Some("America/Denver".into())
        );
        assert_eq!(iana_zone_name("UTC"), Some("UTC".into()));

        assert_eq!(iana_zone_name(""), None);
        assert_eq!(iana_zone_name(":America/Denver"), None);
        assert_eq!(iana_zone_name("MST"), None);
        assert_eq!(iana_zone_name("MST7MDT"), None);
        assert_eq!(iana_zone_name("EST5EDT,M3.2.0,M11.1.0"), None);
        assert_eq!(iana_zone_name("Japan"), None);
    }

    #[test]
    fn zone_from_localtime_target() {
        assert_eq!(
            zone_name_from_localtime_target(Path::new("/usr/share/zoneinfo/posix/America/Denver")),
            Some("America/Denver".into())
        );
        assert_eq!(
            zone_name_from_localtime_target(Path::new("/usr/share/zoneinfo/America/Denver")),
            Some("America/Denver".into())
        );
    }

    #[test]
    fn civil_time_in_zoneinfo_instants() {
        let denver_jan = civil_time_in_zoneinfo("America/Denver", 1768503600).unwrap();
        assert_eq!(denver_jan.day, "20260115");
        assert_eq!(denver_jan.hms, "120000");
        assert_eq!(denver_jan.utc_offset_seconds, -25200);

        let denver_jul = civil_time_in_zoneinfo("America/Denver", 1784138400).unwrap();
        assert_eq!(denver_jul.day, "20260715");
        assert_eq!(denver_jul.hms, "120000");
        assert_eq!(denver_jul.utc_offset_seconds, -21600);

        let kolkata_jan = civil_time_in_zoneinfo("Asia/Kolkata", 1768458600).unwrap();
        assert_eq!(kolkata_jan.day, "20260115");
        assert_eq!(kolkata_jan.hms, "120000");
        assert_eq!(kolkata_jan.utc_offset_seconds, 19800);
    }
}
