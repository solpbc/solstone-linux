// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Release-record time: a UTC instant written and read as `YYYY-MM-DDTHH:MM:SSZ`.

use jiff::civil::Date;
use jiff::tz::Offset;
use jiff::{SignedDuration, Timestamp};

/// `ts` as `YYYY-MM-DDTHH:MM:SSZ`, any part of a second dropped.
pub(crate) fn utc_seconds(ts: Timestamp) -> String {
    let wall = Offset::UTC.to_datetime(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        wall.year(),
        wall.month(),
        wall.day(),
        wall.hour(),
        wall.minute(),
        wall.second()
    )
}

/// The instant `text` names when it is exactly `YYYY-MM-DDTHH:MM:SSZ` with a real
/// date and time. A leap second (`:60`) reads as the instant one second after `:59`.
pub(crate) fn parse_utc_seconds(text: &str) -> Option<Timestamp> {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        });
    if !shaped {
        return None;
    }
    let number = |from: usize, to: usize| {
        bytes[from..to]
            .iter()
            .fold(0_i16, |value, digit| value * 10 + i16::from(digit - b'0'))
    };
    let field = |from: usize| i8::try_from(number(from, from + 2)).ok();
    let date = Date::new(number(0, 4), field(5)?, field(8)?).ok()?;
    let (hour, minute, second) = (field(11)?, field(14)?, field(17)?);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let leap = second == 60;
    let wall = date.at(hour, minute, if leap { 59 } else { second }, 0);
    let ts = Offset::UTC.to_timestamp(wall).ok()?;
    if leap {
        ts.checked_add(SignedDuration::from_secs(1)).ok()
    } else {
        Some(ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_whole_utc_seconds_with_z() {
        let ts = Timestamp::new(1_784_678_400, 987_654_321).unwrap();
        assert_eq!(utc_seconds(ts), "2026-07-22T00:00:00Z");
        let ts = Timestamp::new(1_785_414_896, 0).unwrap();
        assert_eq!(utc_seconds(ts), "2026-07-30T12:34:56Z");
        assert_eq!(utc_seconds(Timestamp::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn reads_exactly_the_canonical_form() {
        let ts = parse_utc_seconds("2026-07-30T12:34:56Z").unwrap();
        assert_eq!(ts.as_second(), 1_785_414_896);
        assert_eq!(utc_seconds(ts), "2026-07-30T12:34:56Z");
        assert_eq!(
            parse_utc_seconds("2026-12-31T23:59:60Z")
                .unwrap()
                .as_second(),
            parse_utc_seconds("2027-01-01T00:00:00Z")
                .unwrap()
                .as_second()
        );
        for refused in [
            "2026-07-30T12:34:56+00:00",
            "2026-07-30T12:34:56.5Z",
            "2026-07-30 12:34:56Z",
            "2026-07-30t12:34:56Z",
            "2026-07-30T12:34:56z",
            "2026-02-30T12:34:56Z",
            "2026-13-01T00:00:00Z",
            "2026-07-30T24:00:00Z",
            "2026-07-30T12:60:00Z",
            "2026-07-30T12:34:61Z",
            "+026-07-30T12:34:56Z",
            " 2026-07-30T12:34:56Z",
            "2026-7-30T12:34:56Z",
            "",
        ] {
            assert_eq!(parse_utc_seconds(refused), None, "{refused}");
        }
    }
}
