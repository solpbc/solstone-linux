// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Time as the note contract speaks it: an instant plus the device's offset at
//! that instant, written as RFC 3339 and floored to the journal's 300-second grid.

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
        let secs = self.ms.div_euclid(1000);
        if self.offset_s == 0 {
            let utc = chrono::DateTime::from_timestamp(secs, 0).unwrap_or_default();
            return utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        }
        let offset = chrono::FixedOffset::east_opt(self.offset_s)
            .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("zero offset is valid"));
        let local = chrono::DateTime::from_timestamp(secs, 0)
            .unwrap_or_default()
            .with_timezone(&offset);
        local.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
    }

    /// This moment's wall-clock seconds, floored to the window it falls in.
    fn window_start_wall(&self) -> i64 {
        (self.ms.div_euclid(1000) + i64::from(self.offset_s)).div_euclid(WINDOW_SECONDS)
            * WINDOW_SECONDS
    }

    pub fn window(&self) -> Window {
        let wall = chrono::DateTime::from_timestamp(self.window_start_wall(), 0)
            .unwrap_or_default()
            .naive_utc();
        Window {
            day: wall.format("%Y%m%d").to_string(),
            segment: format!("{}_300", wall.format("%H%M%S")),
        }
    }

    /// The instant this moment's window ends.
    pub fn window_end_ms(&self) -> i64 {
        (self.window_start_wall() + WINDOW_SECONDS - i64::from(self.offset_s)) * 1000
    }
}

pub const WINDOW_SECONDS: i64 = 300;

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
        chrono::Utc::now().timestamp_millis()
    }

    fn offset_s_at(&self, ms: i64) -> i32 {
        use chrono::{Local, Offset, TimeZone};
        Local
            .timestamp_millis_opt(ms)
            .single()
            .map_or(0, |moment| moment.offset().fix().local_minus_utc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
