// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! This device's wall clock, and the RFC 3339 text the app writes and reads.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use jiff::{SignedDuration, Timestamp, ZonedDateTime};

/// The device's local time zone, read afresh on every call so a change shows at
/// once; the same zone the Obsidian engine's clock reads.
pub fn zone() -> TimeZone {
    solstone_vault::local_zone()
}

/// Now, on the device's wall clock.
pub fn now() -> ZonedDateTime {
    Timestamp::now().to_zoned_datetime(zone())
}

/// The instant `ms` milliseconds after the epoch on the device's wall clock.
pub fn at_millis(ms: i64) -> Option<ZonedDateTime> {
    Timestamp::from_millisecond(ms)
        .ok()
        .map(|ts| ts.to_zoned_datetime(zone()))
}

/// A wall-clock time resolved in a zone, with every answer a daylight-saving
/// change allows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalResult {
    /// Exactly one instant has this wall-clock time.
    Single(ZonedDateTime),
    /// The clocks were set back: the earlier and the later instant.
    Ambiguous(ZonedDateTime, ZonedDateTime),
    /// The clocks skipped this wall-clock time.
    None,
}

impl LocalResult {
    /// The instant when there is exactly one.
    #[cfg(test)]
    pub fn single(self) -> Option<ZonedDateTime> {
        match self {
            Self::Single(zoned) => Some(zoned),
            _ => None,
        }
    }

    /// The only or earlier instant; none in a gap.
    pub fn earliest(self) -> Option<ZonedDateTime> {
        match self {
            Self::Single(zoned) | Self::Ambiguous(zoned, _) => Some(zoned),
            Self::None => None,
        }
    }
}

/// Resolve the wall-clock `dt` in `zone`.
pub fn from_local(zone: &TimeZone, dt: DateTime) -> LocalResult {
    let at = |offset: Offset| {
        offset
            .to_timestamp(dt)
            .ok()
            .map(|ts| ts.to_zoned_datetime(zone.clone()))
    };
    match zone.to_ambiguous_zoned_datetime(dt).offset() {
        AmbiguousOffset::Unambiguous { offset } => {
            at(offset).map_or(LocalResult::None, LocalResult::Single)
        }
        AmbiguousOffset::Gap { .. } => LocalResult::None,
        AmbiguousOffset::Fold { before, after } => match (at(before), at(after)) {
            (Some(earlier), Some(later)) => LocalResult::Ambiguous(earlier, later),
            (Some(only), None) | (None, Some(only)) => LocalResult::Single(only),
            (None, None) => LocalResult::None,
        },
    }
}

/// The instant of a wall-clock time on this device, the earlier one in a fold.
#[cfg(test)]
pub(crate) fn wall_for_test(
    year: i16,
    month: i8,
    day: i8,
    hour: i8,
    minute: i8,
    second: i8,
) -> Timestamp {
    let wall = jiff::civil::date(year, month, day).at(hour, minute, second, 0);
    from_local(&zone(), wall)
        .earliest()
        .expect("a real local time")
        .timestamp()
}

/// How many fraction digits written RFC 3339 text carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Precision {
    /// No fraction.
    Secs,
    /// The fewest of none, three, six or nine digits that hold the value exactly.
    AutoSi,
}

/// RFC 3339 text for `zoned` at its offset: `YYYY-MM-DDTHH:MM:SS[.fraction]`,
/// then `Z` (only when `use_z` and the offset is zero) or `±HH:MM`.
pub fn rfc3339(zoned: &ZonedDateTime, precision: Precision, use_z: bool) -> String {
    let wall = zoned.datetime();
    let mut out = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        wall.year(),
        wall.month(),
        wall.day(),
        wall.hour(),
        wall.minute(),
        wall.second()
    );
    let nanos = wall.subsec_nanosecond().max(0);
    if precision == Precision::AutoSi && nanos != 0 {
        if nanos % 1_000_000 == 0 {
            out.push_str(&format!(".{:03}", nanos / 1_000_000));
        } else if nanos % 1_000 == 0 {
            out.push_str(&format!(".{:06}", nanos / 1_000));
        } else {
            out.push_str(&format!(".{nanos:09}"));
        }
    }
    let seconds = zoned.offset().seconds();
    if use_z && seconds == 0 {
        out.push('Z');
    } else {
        let sign = if seconds < 0 { '-' } else { '+' };
        // Whole minutes, as RFC 3339 writes them; a seconds part rounds.
        let minutes = (seconds.unsigned_abs() + 30) / 60;
        out.push_str(&format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60));
    }
    out
}

/// Whole milliseconds since the epoch, rounded down.
pub fn unix_millis(ts: Timestamp) -> i64 {
    i64::try_from(ts.as_nanosecond().div_euclid(1_000_000)).expect("a timestamp's milliseconds fit")
}

/// `ts` as `YYYY-MM-DDTHH:MM:SSZ`.
#[cfg(test)]
pub fn utc_seconds(ts: Timestamp) -> String {
    rfc3339(&fixed(ts, Offset::UTC), Precision::Secs, true)
}

/// The instant `ts` read at a fixed `offset`.
pub fn fixed(ts: Timestamp, offset: Offset) -> ZonedDateTime {
    ts.to_zoned_datetime(TimeZone::fixed(offset))
}

/// Strict RFC 3339: `YYYY-MM-DD`, then `T`, `t` or a space, then
/// `HH:MM:SS[.fraction]`, then `Z`, `z` or `±HH:MM`, and nothing else. The
/// result carries the offset the text gave. A leap second (`:60`) reads as
/// the instant one second after `:59`.
pub fn parse_rfc3339(text: &str) -> Option<ZonedDateTime> {
    let mut cursor = Cursor {
        bytes: text.as_bytes(),
        at: 0,
    };
    let year = i16::try_from(cursor.digits(4)?).ok()?;
    cursor.eat(b'-')?;
    let month = cursor.digits(2)?;
    cursor.eat(b'-')?;
    let day = cursor.digits(2)?;
    let date = Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()?;
    match cursor.next()? {
        b'T' | b't' | b' ' => {}
        _ => return None,
    }
    let hour = cursor.digits(2)?;
    cursor.eat(b':')?;
    let minute = cursor.digits(2)?;
    cursor.eat(b':')?;
    let second = cursor.digits(2)?;
    let mut nanos = 0_i32;
    if cursor.eat(b'.').is_some() {
        let mut count = 0;
        while let Some(digit @ b'0'..=b'9') = cursor.peek() {
            if count < 9 {
                nanos = nanos * 10 + i32::from(digit - b'0');
            }
            count += 1;
            cursor.at += 1;
        }
        if count == 0 {
            return None;
        }
        for _ in count..9 {
            nanos *= 10;
        }
    }
    let leap = second == 60;
    let time = Time::new(
        i8::try_from(hour).ok()?,
        i8::try_from(minute).ok()?,
        i8::try_from(if leap { 59 } else { second }).ok()?,
        nanos,
    )
    .ok()?;
    let offset = match cursor.next()? {
        b'Z' | b'z' => Offset::UTC,
        sign @ (b'+' | b'-') => {
            let hours = cursor.digits(2)?;
            cursor.eat(b':')?;
            let minutes = cursor.digits(2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            Offset::from_seconds(if sign == b'-' { -seconds } else { seconds }).ok()?
        }
        _ => return None,
    };
    if cursor.at != cursor.bytes.len() {
        return None;
    }
    let ts = offset.to_timestamp(date.to_datetime(time)).ok()?;
    let ts = if leap {
        ts.checked_add(SignedDuration::from_secs(1)).ok()?
    } else {
        ts
    };
    Some(fixed(ts, offset))
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.peek() == Some(byte)).then(|| self.at += 1)
    }

    fn digits(&mut self, count: usize) -> Option<i32> {
        let mut value = 0;
        for _ in 0..count {
            let digit = self.peek().filter(u8::is_ascii_digit)?;
            value = value * 10 + i32::from(digit - b'0');
            self.at += 1;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_rfc3339_as_before() {
        let ts = Timestamp::new(1_794_156_130, 0).unwrap();
        let denver = fixed(ts, Offset::from_seconds(-7 * 3600).unwrap());
        assert_eq!(
            rfc3339(&denver, Precision::AutoSi, false),
            "2026-11-08T09:42:10-07:00"
        );
        assert_eq!(
            rfc3339(&fixed(ts, Offset::UTC), Precision::AutoSi, false),
            "2026-11-08T16:42:10+00:00"
        );
        assert_eq!(utc_seconds(ts), "2026-11-08T16:42:10Z");
        let ts = Timestamp::new(1_794_156_130, 120_000_000).unwrap();
        assert_eq!(
            rfc3339(&fixed(ts, Offset::UTC), Precision::AutoSi, false),
            "2026-11-08T16:42:10.120+00:00"
        );
        assert_eq!(utc_seconds(ts), "2026-11-08T16:42:10Z");
        let ts = Timestamp::new(1_794_156_130, 123_456_000).unwrap();
        let kolkata = fixed(ts, Offset::from_seconds(19_800).unwrap());
        assert_eq!(
            rfc3339(&kolkata, Precision::AutoSi, true),
            "2026-11-08T22:12:10.123456+05:30"
        );
        let ts = Timestamp::new(1_794_156_130, 123_456_789).unwrap();
        assert_eq!(
            rfc3339(&fixed(ts, Offset::UTC), Precision::AutoSi, true),
            "2026-11-08T16:42:10.123456789Z"
        );
    }

    #[test]
    fn reads_strict_rfc3339_as_before() {
        let read = |text: &str| {
            parse_rfc3339(text).map(|zoned| {
                (
                    zoned.timestamp().as_second(),
                    zoned.timestamp().subsec_nanosecond(),
                    zoned.offset().seconds(),
                )
            })
        };
        assert_eq!(
            read("2026-11-08T09:42:10-07:00"),
            Some((1_794_156_130, 0, -25_200))
        );
        assert_eq!(read("2026-11-08t16:42:10z"), Some((1_794_156_130, 0, 0)));
        assert_eq!(read("2026-11-08 16:42:10Z"), Some((1_794_156_130, 0, 0)));
        assert_eq!(
            read("2026-11-08T16:42:10.5+00:00"),
            Some((1_794_156_130, 500_000_000, 0))
        );
        assert_eq!(
            read("2026-11-08T16:42:10.1234567891Z"),
            Some((1_794_156_130, 123_456_789, 0))
        );
        assert_eq!(read("2026-12-31T23:59:60Z"), read("2027-01-01T00:00:00Z"));
        for refused in [
            "",
            "2026-11-08",
            "2026-11-08T16:42:10",
            "2026-11-08T16:42Z",
            "2026-11-08T16:42:10.Z",
            "2026-11-08T16:42:10+0000",
            "2026-11-08T16:42:10+24:00",
            "2026-11-08T16:42:10Z ",
            " 2026-11-08T16:42:10Z",
            "+2026-11-08T16:42:10Z",
            "2026-02-30T16:42:10Z",
            "2026-11-08T24:00:00Z",
            "2026-11-08T16:42:61Z",
            "2026-11-08_16:42:10Z",
        ] {
            assert_eq!(read(refused), None, "{refused}");
        }
    }

    #[test]
    fn local_wall_times_resolve_with_gaps_and_folds() {
        let Ok(zone) = TimeZone::get("America/Denver") else {
            return;
        };
        let stamp = |result: Option<ZonedDateTime>| result.map(|z| z.timestamp().as_second());
        let gap = jiff::civil::date(2026, 3, 8).at(2, 30, 0, 0);
        assert_eq!(from_local(&zone, gap), LocalResult::None);
        let fold = jiff::civil::date(2026, 11, 1).at(1, 30, 0, 0);
        assert_eq!(
            stamp(from_local(&zone, fold).earliest()),
            Some(1_793_518_200)
        );
        assert_eq!(from_local(&zone, fold).single(), None);
        let plain = jiff::civil::date(2026, 11, 8).at(9, 42, 10, 0);
        assert_eq!(
            stamp(from_local(&zone, plain).single()),
            Some(1_794_156_130)
        );
    }
}
