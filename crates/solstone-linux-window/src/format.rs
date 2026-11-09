// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use gtk::glib;
use num_format::{Buffer, Locale, SystemLocale, ToFormattedStr};

pub fn format_integer<T: ToFormattedStr>(n: T) -> String {
    if let Ok(locale) = SystemLocale::default() {
        let mut buf = Buffer::default();
        buf.write_formatted(&n, &locale);
        buf.to_string()
    } else {
        let mut buf = Buffer::default();
        buf.write_formatted(&n, &Locale::en);
        buf.to_string()
    }
}

pub fn format_timestamp(epoch_secs: f64) -> String {
    if let Ok(dt) = glib::DateTime::from_unix_local(epoch_secs as i64)
        && let Ok(s) = dt.format("%c")
    {
        return s.to_string();
    }
    format_integer(epoch_secs as i64)
}

/// An RFC 3339 time as a date alone in local time, such as `Oct 20, 2026`.
pub fn format_date(rfc3339: &str) -> Option<String> {
    let moment = glib::DateTime::from_iso8601(rfc3339, None).ok()?;
    let local = moment.to_local().ok()?;
    local.format("%b %-d, %Y").ok().map(|date| date.to_string())
}

pub fn parse_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Ok(value) = text.parse() {
        return Some(value);
    }
    let locale = SystemLocale::default().ok()?;
    let separator = locale.separator();
    if separator.is_empty() {
        return None;
    }
    let value = text.replace(separator, "").parse::<i64>().ok()?;
    (format_integer(value) == text).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_integer_fallback_does_not_panic() {
        let s1 = format_integer(1000);
        assert!(!s1.is_empty());
        let s2 = format_integer(0);
        assert_eq!(s2, "0");
        let s3 = format_integer(-42);
        assert!(s3.contains("42"));
    }

    #[test]
    fn a_date_is_only_a_date() {
        let date = format_date("2026-10-20T12:00:00-07:00").unwrap();
        assert!(date.ends_with(", 2026"), "{date}");
        assert!(!date.contains(':'), "{date}");
        assert_eq!(format_date("not a time"), None);
        assert_eq!(format_date(""), None);
    }

    #[test]
    fn format_timestamp_does_not_panic() {
        let s = format_timestamp(1700000000.0);
        assert!(!s.is_empty());
    }
}
