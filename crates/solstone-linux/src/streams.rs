// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

const MIN_HEALTHY_WEBM_BYTES: u64 = 2048;
pub const SILENT_STREAM_LOG_MESSAGE: &str = "silent stream dropped";

fn strip_hostname(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let parts: Vec<_> = name.split('.').filter(|p| !p.is_empty()).collect();
    if parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
        parts.join("-")
    } else {
        parts[0].to_owned()
    }
}

fn is_valid_stream_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && !name.contains("..")
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

pub fn stream_name(
    host: Option<&str>,
    observer: Option<&str>,
    qualifier: Option<&str>,
) -> Result<String, String> {
    let source = host
        .filter(|s| !s.is_empty())
        .or_else(|| observer.filter(|s| !s.is_empty()))
        .ok_or_else(|| "stream_name requires host or observer".to_owned())?;
    fn normalize(s: &str) -> String {
        let mut out = String::new();
        let mut dash = false;
        for c in s.trim().to_lowercase().chars() {
            if c.is_whitespace() || c == '/' || c == '\\' {
                if !dash {
                    out.push('-');
                }
                dash = true;
            } else {
                out.push(c);
                dash = false;
            }
        }
        out
    }
    let mut name = normalize(&strip_hostname(source));
    if let Some(q) = qualifier.filter(|q| !q.is_empty()) {
        name.push('.');
        name.push_str(&normalize(q));
    }
    if is_valid_stream_name(&name) {
        Ok(name)
    } else {
        Err(format!("Invalid stream name: {name:?}"))
    }
}

pub fn stream_filename(position: &str, connector: &str) -> String {
    format!("{position}_{connector}_screen.webm")
}

pub fn is_healthy_file_size(size: Option<u64>) -> bool {
    size.is_some_and(|bytes| bytes >= MIN_HEALTHY_WEBM_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_matches_observer_contract() {
        assert_eq!(
            stream_filename("unknown", "monitor-0"),
            "unknown_monitor-0_screen.webm"
        );
    }

    #[test]
    fn silent_threshold_includes_2048() {
        assert!(!is_healthy_file_size(None));
        assert!(!is_healthy_file_size(Some(2047)));
        assert!(is_healthy_file_size(Some(2048)));
        assert!(is_healthy_file_size(Some(4096)));
    }

    // tests/test_streams.py::TestStripHostname::test_simple
    #[test]
    fn strip_simple() {
        assert_eq!(strip_hostname("archon"), "archon");
    }
    // tests/test_streams.py::TestStripHostname::test_with_domain
    #[test]
    fn strip_domain() {
        assert_eq!(strip_hostname("ja1r.local"), "ja1r");
    }
    // tests/test_streams.py::TestStripHostname::test_ip_address
    #[test]
    fn strip_ip() {
        assert_eq!(strip_hostname("192.168.1.1"), "192-168-1-1");
    }
    // tests/test_streams.py::TestStripHostname::test_fqdn
    #[test]
    fn strip_fqdn() {
        assert_eq!(strip_hostname("my.host.example.com"), "my");
    }
    // tests/test_streams.py::TestStripHostname::test_empty
    #[test]
    fn strip_empty() {
        assert_eq!(strip_hostname(""), "");
    }
    // tests/test_streams.py::TestStreamName::test_host_only
    #[test]
    fn host_only() {
        assert_eq!(stream_name(Some("archon"), None, None).unwrap(), "archon");
    }
    // tests/test_streams.py::TestStreamName::test_host_with_qualifier
    #[test]
    fn qualifier() {
        assert_eq!(
            stream_name(Some("archon"), None, Some("tmux")).unwrap(),
            "archon.tmux"
        );
    }
    // tests/test_streams.py::TestStreamName::test_host_no_qualifier
    #[test]
    fn no_qualifier() {
        assert_eq!(stream_name(Some("archon"), None, None).unwrap(), "archon");
    }
    // tests/test_streams.py::TestStreamName::test_observer
    #[test]
    fn observer() {
        assert_eq!(stream_name(None, Some("desktop"), None).unwrap(), "desktop");
    }
    // tests/test_streams.py::TestStreamName::test_rejects_empty
    #[test]
    fn rejects_empty() {
        assert!(stream_name(None, None, None).is_err());
    }
    // tests/test_streams.py::TestStreamName::test_rejects_invalid_chars
    #[test]
    fn rejects_invalid() {
        assert!(stream_name(Some("!invalid"), None, None).is_err());
    }
    #[test]
    fn numeric_double_dot_is_ip_normalized_like_python() {
        assert_eq!(stream_name(Some("1..2"), None, None).unwrap(), "1-2");
    }
    #[test]
    fn accepts_leading_digit() {
        assert_eq!(stream_name(Some("1host"), None, None).unwrap(), "1host");
    }
    #[test]
    fn normalization_collapses_runs() {
        assert_eq!(
            stream_name(Some("A /\\ B"), None, Some(" Q / X ")).unwrap(),
            "a-b.q-x"
        );
    }
}
