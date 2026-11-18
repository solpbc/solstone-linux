// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Time as the note contract speaks it: an instant plus the device's offset at
//! that instant, written as RFC 3339 and floored to the journal's 300-second grid.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone, TimeZoneDatabase};
use serde::{Deserialize, Serialize};

/// A moment on this device: the instant, and the wall clock's offset from UTC then.
/// An offset with a seconds part is not expressible in the contract's form, so such
/// a stamp is held as UTC.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Stamp {
    pub ms: i64,
    pub offset_s: i32,
}

impl Stamp {
    pub fn new(ms: i64, offset_s: i32) -> Self {
        Self {
            ms,
            offset_s: if offset_s % 60 == 0 { offset_s } else { 0 },
        }
    }

    /// `YYYY-MM-DDTHH:MM:SS` then the offset, or `Z` at UTC. Whole seconds.
    pub fn rfc3339(&self) -> String {
        let instant = instant_at(self.ms.div_euclid(1000));
        if self.offset_s == 0 {
            let utc = Offset::UTC.to_datetime(instant);
            return format!("{}Z", utc.strftime("%Y-%m-%dT%H:%M:%S"));
        }
        // An offset of a whole day or more is not one a wall clock keeps; it
        // is written as `+00:00`.
        let offset_s = if self.offset_s.unsigned_abs() < 86_400 {
            self.offset_s
        } else {
            0
        };
        let offset = Offset::from_seconds(offset_s).unwrap_or(Offset::UTC);
        let local = offset.to_datetime(instant);
        format!(
            "{}{}",
            local.strftime("%Y-%m-%dT%H:%M:%S"),
            offset_text(offset_s)
        )
    }

    /// This moment's wall-clock seconds, floored to the window it falls in.
    fn window_start_wall(&self) -> i64 {
        (self.ms.div_euclid(1000) + i64::from(self.offset_s)).div_euclid(WINDOW_SECONDS)
            * WINDOW_SECONDS
    }

    pub fn window(&self) -> Window {
        let wall = Offset::UTC.to_datetime(instant_at(self.window_start_wall()));
        Window {
            day: wall.strftime("%Y%m%d").to_string(),
            segment: format!("{}_300", wall.strftime("%H%M%S")),
        }
    }

    /// The instant this moment's window ends.
    pub fn window_end_ms(&self) -> i64 {
        (self.window_start_wall() + WINDOW_SECONDS - i64::from(self.offset_s)) * 1000
    }
}

pub const WINDOW_SECONDS: i64 = 300;

/// The instant `secs` seconds after the epoch; the epoch itself when out of range.
fn instant_at(secs: i64) -> Timestamp {
    Timestamp::new(secs, 0).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `±HH:MM` for an offset in seconds, a seconds part rounded to the nearest minute.
fn offset_text(offset_s: i32) -> String {
    let sign = if offset_s < 0 { '-' } else { '+' };
    let minutes = (offset_s.unsigned_abs() + 30) / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// One 5-minute window: `day` `YYYYMMDD`, `segment` `HHMMSS_300`.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Window {
    pub day: String,
    pub segment: String,
}

/// The device's clock, injected so tests can drive time.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    fn offset_s_at(&self, ms: i64) -> i32;

    fn stamp_at(&self, ms: i64) -> Stamp {
        Stamp::new(ms, self.offset_s_at(ms))
    }

    fn now(&self) -> Stamp {
        self.stamp_at(self.now_ms())
    }
}

/// The system clock and local time zone.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        Timestamp::now().as_millisecond()
    }

    fn offset_s_at(&self, ms: i64) -> i32 {
        Timestamp::from_millisecond(ms).map_or(0, |moment| local_zone().to_offset(moment).seconds())
    }
}

/// The device's local time zone, read afresh on every call so a change shows at
/// once: the system's zone as `TZ`, or else `/etc/localtime`, names it. A `TZ`
/// naming a zone this system no longer installs (an older alias such as
/// `Asia/Calcutta`) is read from the bundled zone database. A `TZ` that names no
/// zone at all falls back to the zone `/etc/localtime` describes, and to UTC only
/// when there is none.
pub fn local_zone() -> TimeZone {
    resolve_local_zone(
        std::env::var_os("TZ").as_deref(),
        Path::new("/etc/localtime"),
        || {
            jiff::tz::reset_system_time_zone_cache();
            TimeZone::try_system()
        },
    )
}

fn resolve_local_zone(
    tz: Option<&OsStr>,
    localtime: &Path,
    system: impl FnOnce() -> Result<TimeZone, jiff::Error>,
) -> TimeZone {
    if let Ok(zone) = system() {
        return zone;
    }
    let bundled = tz
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .and_then(|name| {
            let name = name.strip_prefix(':').unwrap_or(name);
            let name = name
                .rfind("zoneinfo/")
                .map_or(name, |at| &name[at + "zoneinfo/".len()..]);
            TimeZoneDatabase::bundled().get(name).ok()
        });
    // The zone file names no zone of its own here; only its rules are read.
    bundled
        .or_else(|| {
            let bytes = fs::read(localtime).ok()?;
            TimeZone::tzif("localtime", &bytes).ok()
        })
        .unwrap_or(TimeZone::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_golden_moment_formats_and_floors() {
        // 2026-11-08T09:42:10-07:00
        let stamp = Stamp::new(1_794_156_130_000, -7 * 3600);
        assert_eq!(stamp.rfc3339(), "2026-11-08T09:42:10-07:00");
        assert_eq!(
            stamp.window(),
            Window {
                day: "20261108".into(),
                segment: "094000_300".into()
            }
        );
        assert_eq!(stamp.window_end_ms(), 1_794_156_300_000);
    }

    #[test]
    fn an_offset_with_seconds_is_written_as_utc() {
        let stamp = Stamp::new(1_794_156_130_000, -(7 * 3600) + 17);
        assert_eq!(stamp.offset_s, 0);
        assert_eq!(stamp.rfc3339(), "2026-11-08T16:42:10Z");
    }

    #[test]
    fn whole_seconds_and_negative_milliseconds_floor() {
        let stamp = Stamp::new(1_794_156_130_999, 0);
        assert_eq!(stamp.rfc3339(), "2026-11-08T16:42:10Z");
        assert_eq!(Stamp::new(-1, 0).rfc3339(), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn east_offsets_and_day_rollover_format() {
        let stamp = Stamp::new(1_794_156_130_000, 5 * 3600 + 30 * 60);
        assert_eq!(stamp.rfc3339(), "2026-11-08T22:12:10+05:30");
        assert_eq!(
            Stamp::new(1_794_156_130_000, 14 * 3600).rfc3339(),
            "2026-11-09T06:42:10+14:00"
        );
        assert_eq!(
            Stamp::new(1_794_156_130_000, 14 * 3600).window(),
            Window {
                day: "20261109".into(),
                segment: "064000_300".into()
            }
        );
    }

    #[test]
    fn stored_offsets_outside_the_constructor_keep_their_text() {
        // A seconds part rounds to the nearest minute.
        let rounded = Stamp {
            ms: 1_794_156_130_000,
            offset_s: -(7 * 3600) + 31,
        };
        assert_eq!(rounded.rfc3339(), "2026-11-08T09:42:41-06:59");
        // A whole day or more is written at UTC with a numeric offset.
        let day = Stamp {
            ms: 1_794_156_130_000,
            offset_s: 86_400,
        };
        assert_eq!(day.rfc3339(), "2026-11-08T16:42:10+00:00");
    }

    // 2026-07-01T12:00:00Z and 2026-01-15T12:00:00Z.
    const SUMMER: i64 = 1_782_907_200;
    const WINTER: i64 = 1_768_478_400;

    fn offsets(zone: &TimeZone) -> (i32, i32) {
        let at = |unix| {
            zone.to_offset(Timestamp::from_second(unix).unwrap())
                .seconds()
        };
        (at(SUMMER), at(WINTER))
    }

    fn bundled(name: &str) -> TimeZone {
        TimeZoneDatabase::bundled().get(name).unwrap()
    }

    fn unresolved() -> Result<TimeZone, jiff::Error> {
        TimeZone::posix("not a zone")
    }

    /// A zone file with one fixed offset, as `/etc/localtime` may hold.
    fn fixed_zone_file(dir: &Path, offset_s: i32, abbreviation: &str, rule: &str) -> PathBuf {
        let mut block = Vec::new();
        let header = |block: &mut Vec<u8>, version: u8| {
            block.extend_from_slice(b"TZif");
            block.push(version);
            block.extend_from_slice(&[0; 15]);
            let chars = u32::try_from(abbreviation.len() + 1).unwrap();
            for count in [0_u32, 0, 0, 0, 1, chars] {
                block.extend_from_slice(&count.to_be_bytes());
            }
            block.extend_from_slice(&offset_s.to_be_bytes());
            block.extend_from_slice(&[0, 0]);
            block.extend_from_slice(abbreviation.as_bytes());
            block.push(0);
        };
        header(&mut block, b'2');
        header(&mut block, b'2');
        block.extend_from_slice(format!("\n{rule}\n").as_bytes());
        let path = dir.join("localtime");
        fs::write(&path, block).unwrap();
        path
    }

    #[test]
    fn tz_unset_reads_the_system_zone_and_then_localtime() {
        let temp = tempfile::tempdir().unwrap();
        let localtime = fixed_zone_file(temp.path(), 3 * 3600, "MSK", "MSK-3");
        let denver = || Ok(bundled("America/Denver"));
        assert_eq!(
            offsets(&resolve_local_zone(None, &localtime, denver)),
            (-21_600, -25_200)
        );
        let fallback = resolve_local_zone(None, &localtime, unresolved);
        assert_eq!(offsets(&fallback), (10_800, 10_800));
    }

    #[test]
    fn tz_naming_a_zone_reads_it() {
        let temp = tempfile::tempdir().unwrap();
        let localtime = fixed_zone_file(temp.path(), 3 * 3600, "MSK", "MSK-3");
        let tz = Some(OsStr::new("America/Denver"));
        let zone = resolve_local_zone(tz, &localtime, || Ok(bundled("America/Denver")));
        assert_eq!(offsets(&zone), (-21_600, -25_200));
        let utc = resolve_local_zone(Some(OsStr::new("")), &localtime, || Ok(TimeZone::UTC));
        assert_eq!(offsets(&utc), (0, 0));
    }

    #[test]
    fn tz_naming_an_alias_the_host_lacks_reads_the_bundled_zone() {
        let temp = tempfile::tempdir().unwrap();
        let localtime = fixed_zone_file(temp.path(), 3 * 3600, "MSK", "MSK-3");
        for tz in [
            "Asia/Calcutta",
            ":Asia/Calcutta",
            "/usr/share/zoneinfo/Asia/Calcutta",
        ] {
            let zone = resolve_local_zone(Some(OsStr::new(tz)), &localtime, unresolved);
            assert_eq!(offsets(&zone), (19_800, 19_800), "{tz}");
            let stamp = Stamp::new(
                1_794_156_130_000,
                zone.to_offset(Timestamp::from_second(1_794_156_130).unwrap())
                    .seconds(),
            );
            assert_eq!(stamp.rfc3339(), "2026-11-08T22:12:10+05:30");
        }
    }

    #[test]
    fn tz_naming_no_zone_falls_back_to_localtime_then_utc() {
        let temp = tempfile::tempdir().unwrap();
        let localtime = fixed_zone_file(temp.path(), 3 * 3600, "MSK", "MSK-3");
        let zone = resolve_local_zone(Some(OsStr::new("garbage!!")), &localtime, unresolved);
        assert_eq!(offsets(&zone), (10_800, 10_800));
        let stamp = Stamp::new(1_794_156_130_000, offsets(&zone).0);
        assert_eq!(stamp.rfc3339(), "2026-11-08T19:42:10+03:00");
        let missing = temp.path().join("missing");
        let zone = resolve_local_zone(Some(OsStr::new("garbage!!")), &missing, unresolved);
        assert_eq!(offsets(&zone), (0, 0));
    }

    #[test]
    fn an_instant_out_of_range_is_the_epoch() {
        assert_eq!(Stamp::new(i64::MAX, 0).rfc3339(), "1970-01-01T00:00:00Z");
    }
}
