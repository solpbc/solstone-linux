// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use chrono::{DateTime, Local, LocalResult, Offset, TimeZone};
use std::{
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
};

pub(crate) const ZONE_INFO_DIRECTORIES: [&str; 4] = [
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

#[cfg(test)]
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

fn is_valid_iana_shape(raw: &str) -> Option<&str> {
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
    Some(stripped)
}

#[cfg(test)]
fn iana_zone_name(raw: &str) -> Option<String> {
    let stripped = is_valid_iana_shape(raw)?;
    let file_exists = ZONE_INFO_DIRECTORIES.iter().any(|&dir| {
        let path = Path::new(dir).join(stripped);
        path.is_file()
    });
    if !file_exists {
        return None;
    }
    Some(stripped.to_owned())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = components.last() {
                    components.pop();
                } else {
                    components.push(comp);
                }
            }
            _ => components.push(comp),
        }
    }
    components.iter().collect()
}

#[cfg(test)]
fn zone_name_from_localtime_target(target: &Path) -> Option<String> {
    let resolved = if target.is_relative() {
        normalize_path(&Path::new("/etc").join(target))
    } else {
        normalize_path(target)
    };
    for &dir in &ZONE_INFO_DIRECTORIES {
        let norm_root = normalize_path(Path::new(dir));
        if let Ok(rel) = resolved.strip_prefix(&norm_root)
            && let Some(rel_str) = rel.to_str()
            && let Some(name) = is_valid_iana_shape(rel_str)
        {
            return Some(name.to_owned());
        }
    }
    None
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZoneReading {
    pub day: String,
    pub hms: String,
    pub utc_offset_seconds: i32,
    pub tz: Option<String>,
}

pub(crate) trait ZoneSource {
    fn read_zone(&mut self, wall_seconds: f64) -> ZoneReading;
}

pub(crate) struct DeviceZoneReader {
    tz_env: Option<OsString>,
    localtime: PathBuf,
    roots: Vec<PathBuf>,
    resolved_name: bool,
    warned: bool,
}

impl DeviceZoneReader {
    pub(crate) fn new(tz_env: Option<OsString>, localtime: PathBuf, roots: Vec<PathBuf>) -> Self {
        Self {
            tz_env,
            localtime,
            roots,
            resolved_name: false,
            warned: false,
        }
    }

    fn warn_fallback(&mut self) {
        if !self.warned {
            tracing::warn!("capture zone fallback");
            self.warned = true;
        }
    }
}

impl ZoneSource for DeviceZoneReader {
    fn read_zone(&mut self, wall_seconds: f64) -> ZoneReading {
        let unix_secs = wall_seconds.floor() as i64;

        let make_reading =
            |tz: &jiff::tz::TimeZone, tz_name: Option<String>| -> Option<ZoneReading> {
                let ts = jiff::Timestamp::from_second(unix_secs).ok()?;
                let dt = tz.to_datetime(ts);
                let offset = tz.to_offset(ts);
                Some(ZoneReading {
                    day: format!("{:04}{:02}{:02}", dt.year(), dt.month(), dt.day()),
                    hms: format!("{:02}{:02}{:02}", dt.hour(), dt.minute(), dt.second()),
                    utc_offset_seconds: offset.seconds(),
                    tz: tz_name,
                })
            };

        if let Some(ref val) = self.tz_env {
            // Rows 1-3. Never consult localtime.
            let Some(raw_str) = val.to_str() else {
                if self.resolved_name {
                    self.warn_fallback();
                }
                let dt = local_datetime(wall_seconds);
                return ZoneReading {
                    day: dt.format("%Y%m%d").to_string(),
                    hms: dt.format("%H%M%S").to_string(),
                    utc_offset_seconds: dt.offset().fix().local_minus_utc(),
                    tz: None,
                };
            };

            if raw_str.is_empty() {
                if self.resolved_name {
                    self.warn_fallback();
                }
                let dt = local_datetime(wall_seconds);
                return ZoneReading {
                    day: dt.format("%Y%m%d").to_string(),
                    hms: dt.format("%H%M%S").to_string(),
                    utc_offset_seconds: dt.offset().fix().local_minus_utc(),
                    tz: None,
                };
            }

            if let Some(shape_name) = is_valid_iana_shape(raw_str) {
                for root in &self.roots {
                    let p = root.join(shape_name);
                    if p.is_file() {
                        if let Ok(bytes) = fs::read(&p)
                            && let Ok(tz) = jiff::tz::TimeZone::tzif(shape_name, &bytes)
                            && let Some(reading) = make_reading(&tz, Some(shape_name.to_owned()))
                        {
                            self.resolved_name = true;
                            return reading;
                        }
                        // File exists but tzif failed
                        self.resolved_name = true;
                        self.warn_fallback();
                        let dt = local_datetime(wall_seconds);
                        return ZoneReading {
                            day: dt.format("%Y%m%d").to_string(),
                            hms: dt.format("%H%M%S").to_string(),
                            utc_offset_seconds: dt.offset().fix().local_minus_utc(),
                            tz: None,
                        };
                    }
                }
                // Shape-valid but file does not exist
                if self.resolved_name {
                    self.warn_fallback();
                }
                let dt = local_datetime(wall_seconds);
                return ZoneReading {
                    day: dt.format("%Y%m%d").to_string(),
                    hms: dt.format("%H%M%S").to_string(),
                    utc_offset_seconds: dt.offset().fix().local_minus_utc(),
                    tz: None,
                };
            }

            // Not a shape-valid IANA name. Try POSIX
            if let Ok(tz) = jiff::tz::TimeZone::posix(raw_str)
                && let Some(reading) = make_reading(&tz, None)
            {
                return reading;
            }

            // POSIX rejected
            if self.resolved_name {
                self.warn_fallback();
            }
            let dt = local_datetime(wall_seconds);
            return ZoneReading {
                day: dt.format("%Y%m%d").to_string(),
                hms: dt.format("%H%M%S").to_string(),
                utc_offset_seconds: dt.offset().fix().local_minus_utc(),
                tz: None,
            };
        }

        // TZ unset: Rows 4-6
        let mut link_resolved_name: Option<(String, PathBuf)> = None;
        if let Ok(target) = fs::read_link(&self.localtime) {
            let resolved = if target.is_relative() {
                let parent = self.localtime.parent().unwrap_or_else(|| Path::new(""));
                normalize_path(&parent.join(target))
            } else {
                normalize_path(&target)
            };

            for root in &self.roots {
                let normalized_root = normalize_path(root);
                if let Ok(rel) = resolved.strip_prefix(&normalized_root)
                    && let Some(rel_str) = rel.to_str()
                    && let Some(shape_name) = is_valid_iana_shape(rel_str)
                {
                    link_resolved_name = Some((shape_name.to_owned(), resolved.clone()));
                    break;
                }
            }
        }

        if let Some((name, resolved_path)) = link_resolved_name {
            if resolved_path.is_file()
                && let Ok(bytes) = fs::read(&resolved_path)
                && let Ok(tz) = jiff::tz::TimeZone::tzif(&name, &bytes)
                && let Some(reading) = make_reading(&tz, Some(name.clone()))
            {
                self.resolved_name = true;
                return reading;
            }
            // Name matched but tzif failed
            self.resolved_name = true;
        }

        // Try reading localtime path directly (Row 5)
        if let Ok(bytes) = fs::read(&self.localtime)
            && let Ok(tz) = jiff::tz::TimeZone::tzif("localtime", &bytes)
            && let Some(reading) = make_reading(&tz, None)
        {
            if self.resolved_name {
                self.warn_fallback();
            }
            return reading;
        }

        // Row 6
        self.warn_fallback();
        let dt = local_datetime(wall_seconds);
        ZoneReading {
            day: dt.format("%Y%m%d").to_string(),
            hms: dt.format("%H%M%S").to_string(),
            utc_offset_seconds: dt.offset().fix().local_minus_utc(),
            tz: None,
        }
    }
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
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);
    impl io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture_logs<F, R>(f: F) -> (R, Vec<String>)
    where
        F: FnOnce() -> R,
    {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer = LogBuffer(Arc::clone(&buffer));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let result = tracing::subscriber::with_default(subscriber, f);
        let raw = buffer.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&raw).to_string();
        let lines: Vec<String> = text
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        (result, lines)
    }

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

    fn testdata_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/zoneinfo")
    }

    fn check_utc_host() {
        if local_offset_seconds(1790706000.0) == 0 {
            panic!("host is UTC; can't tell the fallback from Etc/Unknown");
        }
    }

    #[test]
    fn berlin_slim_dst_instants() {
        let root = testdata_root();
        let bytes = fs::read(root.join("Europe/Berlin")).unwrap();
        let tz = jiff::tz::TimeZone::tzif("Europe/Berlin", &bytes).unwrap();

        let ts1 = jiff::Timestamp::from_second(1792888200).unwrap();
        let dt1 = tz.to_datetime(ts1);
        let off1 = tz.to_offset(ts1).seconds();
        assert_eq!(
            format!("{:04}{:02}{:02}", dt1.year(), dt1.month(), dt1.day()),
            "20261025"
        );
        assert_eq!(
            format!("{:02}{:02}{:02}", dt1.hour(), dt1.minute(), dt1.second()),
            "023000"
        );
        assert_eq!(off1, 7200);

        let ts2 = jiff::Timestamp::from_second(1792891800).unwrap();
        let dt2 = tz.to_datetime(ts2);
        let off2 = tz.to_offset(ts2).seconds();
        assert_eq!(
            format!("{:04}{:02}{:02}", dt2.year(), dt2.month(), dt2.day()),
            "20261025"
        );
        assert_eq!(
            format!("{:02}{:02}{:02}", dt2.hour(), dt2.minute(), dt2.second()),
            "023000"
        );
        assert_eq!(off2, 3600);
    }

    #[test]
    fn zone_table_row_1_tz_set_and_loads() {
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(root.join("Pacific/Auckland"), &link).unwrap();

        let mut reader =
            DeviceZoneReader::new(Some(OsString::from("Asia/Kolkata")), link, vec![root]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading.day, "20260929");
        assert_eq!(reading.hms, "235000");
        assert_eq!(reading.utc_offset_seconds, 19800);
        assert_eq!(reading.tz, Some("Asia/Kolkata".into()));
        assert!(lines.is_empty());
    }

    #[test]
    fn zone_table_row_1_file_is_the_roots() {
        let temp = tempfile::tempdir().unwrap();
        let temp_root = temp.path().join("zoneinfo");
        fs::create_dir_all(temp_root.join("Pacific")).unwrap();
        let root = testdata_root();
        fs::copy(
            root.join("Asia/Kathmandu"),
            temp_root.join("Pacific/Auckland"),
        )
        .unwrap();

        let link = temp.path().join("localtime");
        let mut reader = DeviceZoneReader::new(
            Some(OsString::from("Pacific/Auckland")),
            link,
            vec![temp_root],
        );
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading.day, "20260930");
        assert_eq!(reading.hms, "000500");
        assert_eq!(reading.utc_offset_seconds, 20700);
        assert_eq!(reading.tz, Some("Pacific/Auckland".into()));
        assert!(lines.is_empty());
    }

    #[test]
    fn zone_table_row_2_posix_string() {
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let link = temp.path().join("localtime");

        let mut reader = DeviceZoneReader::new(Some(OsString::from("IST-5:30")), link, vec![root]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading.day, "20260929");
        assert_eq!(reading.hms, "235000");
        assert_eq!(reading.utc_offset_seconds, 19800);
        assert_eq!(reading.tz, None);
        assert!(lines.is_empty());
    }

    #[test]
    fn zone_table_row_3_unparseable_tz() {
        check_utc_host();
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(root.join("Pacific/Auckland"), &link).unwrap();

        let mut reader =
            DeviceZoneReader::new(Some(OsString::from("not a zone")), link, vec![root]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        let dt = local_datetime(1790706000.0);
        assert_eq!(reading.day, dt.format("%Y%m%d").to_string());
        assert_eq!(reading.hms, dt.format("%H%M%S").to_string());
        assert_eq!(
            reading.utc_offset_seconds,
            dt.offset().fix().local_minus_utc()
        );
        assert_ne!(reading.utc_offset_seconds, 46800);
        assert_eq!(reading.tz, None);
        assert!(
            !lines.iter().any(|l| l.contains("capture zone fallback")),
            "unexpected warn: {lines:?}"
        );
    }

    #[test]
    fn zone_table_row_3_file_corrupt() {
        check_utc_host();
        let temp = tempfile::tempdir().unwrap();
        let temp_root = temp.path().join("zoneinfo");
        fs::create_dir_all(temp_root.join("Asia")).unwrap();
        fs::write(temp_root.join("Asia/Kolkata"), b"garbage_bytes").unwrap();

        let root = testdata_root();
        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(root.join("Pacific/Auckland"), &link).unwrap();

        let mut reader =
            DeviceZoneReader::new(Some(OsString::from("Asia/Kolkata")), link, vec![temp_root]);
        let (reading1, lines1) = capture_logs(|| reader.read_zone(1790706000.0));
        let dt = local_datetime(1790706000.0);
        assert_eq!(reading1.day, dt.format("%Y%m%d").to_string());
        assert_eq!(reading1.hms, dt.format("%H%M%S").to_string());
        assert_eq!(
            reading1.utc_offset_seconds,
            dt.offset().fix().local_minus_utc()
        );
        assert_ne!(reading1.utc_offset_seconds, 46800);
        assert_eq!(reading1.tz, None);
        assert_eq!(
            lines1
                .iter()
                .filter(|l| l.contains("capture zone fallback"))
                .count(),
            1
        );

        let (_reading2, lines2) = capture_logs(|| reader.read_zone(1790706000.0));
        assert!(
            !lines2.iter().any(|l| l.contains("capture zone fallback")),
            "second read logged warn again"
        );
    }

    #[test]
    fn zone_table_row_4_link_moves_between_reads() {
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(root.join("Asia/Kolkata"), &link).unwrap();

        let mut reader = DeviceZoneReader::new(None, link.clone(), vec![root.clone()]);
        let (reading1, lines1) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading1.day, "20260929");
        assert_eq!(reading1.hms, "235000");
        assert_eq!(reading1.utc_offset_seconds, 19800);
        assert_eq!(reading1.tz, Some("Asia/Kolkata".into()));
        assert!(lines1.is_empty());

        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(root.join("Pacific/Auckland"), &link).unwrap();

        let (reading2, lines2) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading2.day, "20260930");
        assert_eq!(reading2.hms, "072000");
        assert_eq!(reading2.utc_offset_seconds, 46800);
        assert_eq!(reading2.tz, Some("Pacific/Auckland".into()));
        assert!(lines2.is_empty());
    }

    #[test]
    fn zone_table_row_4_relative_link() {
        let temp = tempfile::tempdir().unwrap();
        let fixture_root = temp.path();
        let etc_dir = fixture_root.join("etc");
        let zoneinfo_dir = fixture_root.join("usr/share/zoneinfo");
        fs::create_dir_all(&etc_dir).unwrap();
        fs::create_dir_all(zoneinfo_dir.join("Asia")).unwrap();

        let root = testdata_root();
        fs::copy(root.join("Asia/Kolkata"), zoneinfo_dir.join("Asia/Kolkata")).unwrap();

        let link = etc_dir.join("localtime");
        std::os::unix::fs::symlink("../usr/share/zoneinfo/Asia/Kolkata", &link).unwrap();

        let mut reader = DeviceZoneReader::new(None, link, vec![zoneinfo_dir]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading.day, "20260929");
        assert_eq!(reading.hms, "235000");
        assert_eq!(reading.utc_offset_seconds, 19800);
        assert_eq!(reading.tz, Some("Asia/Kolkata".into()));
        assert!(lines.is_empty());
    }

    #[test]
    fn zone_table_row_5_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let localtime = temp.path().join("localtime");
        fs::copy(root.join("Asia/Kathmandu"), &localtime).unwrap();

        let mut reader = DeviceZoneReader::new(None, localtime, vec![root]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        assert_eq!(reading.day, "20260930");
        assert_eq!(reading.hms, "000500");
        assert_eq!(reading.utc_offset_seconds, 20700);
        assert_eq!(reading.tz, None);
        assert!(lines.is_empty());
    }

    #[test]
    fn zone_table_row_5_corrupt_resolved_link() {
        check_utc_host();
        let temp = tempfile::tempdir().unwrap();
        let temp_root = temp.path().join("zoneinfo");
        fs::create_dir_all(temp_root.join("Asia")).unwrap();
        let corrupt_target = temp_root.join("Asia/Kolkata");
        fs::write(&corrupt_target, b"corrupt").unwrap();

        let link = temp.path().join("localtime");
        std::os::unix::fs::symlink(&corrupt_target, &link).unwrap();

        let mut reader = DeviceZoneReader::new(None, link, vec![temp_root]);
        let (reading, lines) = capture_logs(|| reader.read_zone(1790706000.0));
        let dt = local_datetime(1790706000.0);
        assert_eq!(reading.day, dt.format("%Y%m%d").to_string());
        assert_eq!(reading.hms, dt.format("%H%M%S").to_string());
        assert_eq!(
            reading.utc_offset_seconds,
            dt.offset().fix().local_minus_utc()
        );
        assert_eq!(reading.tz, None);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("capture zone fallback"))
                .count(),
            1
        );
    }

    #[test]
    fn zone_table_row_6_missing_localtime() {
        check_utc_host();
        let temp = tempfile::tempdir().unwrap();
        let root = testdata_root();
        let link = temp.path().join("nonexistent_localtime");

        let mut reader = DeviceZoneReader::new(None, link, vec![root]);
        let (reading1, lines1) = capture_logs(|| reader.read_zone(1790706000.0));
        let dt = local_datetime(1790706000.0);
        assert_eq!(reading1.day, dt.format("%Y%m%d").to_string());
        assert_eq!(reading1.hms, dt.format("%H%M%S").to_string());
        assert_eq!(
            reading1.utc_offset_seconds,
            dt.offset().fix().local_minus_utc()
        );
        assert_eq!(reading1.tz, None);
        assert_eq!(
            lines1
                .iter()
                .filter(|l| l.contains("capture zone fallback"))
                .count(),
            1
        );

        let (_reading2, lines2) = capture_logs(|| reader.read_zone(1790706000.0));
        assert!(
            !lines2.iter().any(|l| l.contains("capture zone fallback")),
            "second read logged warn again"
        );
    }
}
