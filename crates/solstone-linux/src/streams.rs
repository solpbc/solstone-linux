// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

const MIN_HEALTHY_WEBM_BYTES: u64 = 2048;

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
}
