// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod gstreamer;
pub mod portal;
pub mod wayland_geometry;
pub mod x11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    Portal,
    X11,
}

/// Selects how a later start follows pipeline teardown without changing the
/// `VideoCapture` contract. V1 always creates a new session, which is already
/// the wlroots mitigation: only session reuse can reconnect a new pipeline to
/// the same node and trigger "session already has a frame object". Keep this
/// seam when adding session reuse so that quirk cannot be silently re-armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectStrategy {
    NewPortalSession,
    ReusePortalSession,
}

pub fn reconnect_strategy(reuse_session: bool) -> ReconnectStrategy {
    if reuse_session {
        ReconnectStrategy::ReusePortalSession
    } else {
        ReconnectStrategy::NewPortalSession
    }
}

pub fn clamp_framerate(framerate: i64) -> u8 {
    framerate.clamp(1, 10) as u8
}

pub fn select_backend(
    session_type: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> BackendKind {
    let session_type = session_type.unwrap_or_default();
    if session_type.eq_ignore_ascii_case("x11") {
        return BackendKind::X11;
    }
    if wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.eq_ignore_ascii_case("wayland")
    {
        return BackendKind::Portal;
    }
    if display.is_some_and(|value| !value.is_empty()) {
        return BackendKind::X11;
    }
    BackendKind::Portal
}

// Python screencast test inventory (AC12; 32/32):
// tests/test_screencast.py::test_stderr_drain_consumes_flood_non_utf8_and_caps_lines
//   retired-by-dependency: Rust uses in-process GStreamer and has no subprocess stderr drain.
// tests/test_screencast.py::TestStreamMatching::test_position_based_matching
//   -> matching::tests::position_based_matching.
// tests/test_screencast.py::TestStreamMatching::test_size_based_fallback_when_no_position
//   -> matching::tests::size_based_fallback_when_no_position.
// tests/test_screencast.py::TestStreamMatching::test_position_match_skipped_when_all_zero
//   -> matching::tests::position_match_skipped_when_all_zero.
// tests/test_screencast.py::TestStreamMatching::test_ambiguous_size_assigns_in_order
//   -> matching::tests::ambiguous_size_assigns_in_order.
// tests/test_screencast.py::TestStreamMatching::test_no_monitors_falls_back_to_monitor_idx
//   -> matching::tests::no_monitors_falls_back_to_monitor_index.
// tests/test_screencast.py::TestStreamMatching::test_mixed_position_and_size_matching
//   -> matching::tests::position_based_matching plus size_based_fallback_when_no_position.
// tests/test_screencast.py::test_close_session_call_close_failure_logs_and_clears_handle
//   dbus-fast handle plumbing is retired-by-dependency; AshpdPortalOps::close takes its
//   session before awaiting Close. portal::tests::close_failure_preserves_original_error_and_attempts_close
//   covers the portable original-error/secondary-context policy.
// tests/test_screencast.py::test_start_times_out_unresolved_response_and_removes_handler
//   retired-by-dependency: ashpd owns dbus request-handler registration/removal.
// tests/test_screencast.py::test_start_times_out_method_call_and_removes_handler
//   retired-by-dependency: ashpd owns dbus request-handler registration/removal.
// tests/test_screencast.py::test_start_selects_response_timeout_from_restore_token
//   retired as the wrong rule; portal::tests::timeout_budgets_ignore_token_presence guards its replacement.
// tests/test_screencast.py::test_wayland_immediate_exit_decodes_stderr_and_closes_fd
//   retired-by-dependency: Rust uses in-process GStreamer, not gst-launch stderr.
// tests/test_screencast.py::test_wayland_command_keeps_spaced_location_as_one_token
//   retired-by-dependency: programmatic properties have no argv quoting; pipeline tests pin the value.
// tests/test_screencast.py::test_wayland_closes_pw_fd_on_spawn_failure_once
//   -> portal::tests::pipeline_construction_failure_closes_pipewire_remote_once.
// tests/test_screencast.py::TestX11Screencaster::test_connect_fails_without_display
//   retired-by-dependency: x11rb connect reports the display error directly; there is no preflight method.
// tests/test_screencast.py::TestX11Screencaster::test_connect_fails_without_gst_launch
//   retired-by-dependency: Rust uses in-process GStreamer, not gst-launch discovery.
// tests/test_screencast.py::TestX11Screencaster::test_connect_succeeds
//   retired-by-dependency: Rust has no subprocess preflight method.
// tests/test_screencast.py::TestX11Screencaster::test_start_no_monitors_raises
//   -> x11::tests::start_no_monitors_is_a_real_error (real replacement for vacuous Python test).
// tests/test_screencast.py::TestX11Screencaster::test_start_builds_one_branch_per_monitor
//   -> x11::tests::start_builds_one_real_branch_per_monitor (real replacement for vacuous Python test).
// tests/test_screencast.py::TestX11Screencaster::test_start_sets_correct_ximagesrc_region
//   -> pipeline::tests::ximagesrc_bounds_are_inclusive_and_cursor_is_configurable.
// tests/test_screencast.py::TestX11Screencaster::test_immediate_exit_decodes_non_utf8_stderr
//   retired-by-dependency: Rust uses GStreamer bus errors, not subprocess stderr.
// tests/test_screencast.py::TestX11Screencaster::test_command_keeps_spaced_location_as_one_token
//   retired-by-dependency: programmatic properties have no argv quoting.
// tests/test_screencast.py::TestX11Screencaster::test_stderr_drain_threads_join_after_stop
//   retired-by-dependency: no subprocess stderr-drain thread exists.
// tests/test_screencast.py::TestX11Screencaster::test_stop_filters_silent_streams
//   -> x11::tests::stop_reports_all_tracked_streams_and_unlinks_silent.
// tests/test_screencast.py::TestX11Screencaster::test_stop_keeps_healthy_streams
//   -> x11::tests::stop_reports_all_tracked_streams_and_unlinks_silent.
// tests/test_screencast.py::TestX11Screencaster::test_is_healthy_false_before_start
// tests/test_screencast.py::TestX11Screencaster::test_is_healthy_false_when_process_exited
// tests/test_screencast.py::TestX11Screencaster::test_is_healthy_true_when_running
//   -> x11::tests::partial_pipeline_failure_keeps_sibling_and_health_matches_python.
// tests/test_screencast_stop_filters_silent_streams.py::test_stop_partitions_healthy_and_silent
//   -> x11::tests::stop_reports_all_tracked_streams_and_unlinks_silent.
// tests/test_screencast_stop_filters_silent_streams.py::test_stop_treats_missing_file_as_silent
//   -> x11::tests::stop_reports_missing_and_unlink_error_streams.
// tests/test_screencast_stop_filters_silent_streams.py::test_stop_logs_silent_stream_dropped_prefix
//   -> streams::tests::silent_stream_log_message_matches_python_prefix plus both backend stop paths.
// tests/test_screencast_stop_filters_silent_streams.py::test_stop_handles_unlink_oserror
//   -> x11::tests::stop_reports_missing_and_unlink_error_streams.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framerate_is_clamped_to_config_range() {
        assert_eq!(clamp_framerate(-9), 1);
        assert_eq!(clamp_framerate(1), 1);
        assert_eq!(clamp_framerate(10), 10);
        assert_eq!(clamp_framerate(11), 10);
        assert_eq!(clamp_framerate(i64::MAX), 10);
    }

    #[test]
    fn backend_selection_matches_python_order() {
        // src/solstone_linux/observer.py::_create_screencaster
        let cases = [
            (
                (Some("x11"), Some("wayland-0"), Some(":0")),
                BackendKind::X11,
            ),
            ((None, Some("wayland-0"), Some(":0")), BackendKind::Portal),
            ((Some("wayland"), None, Some(":0")), BackendKind::Portal),
            ((None, None, Some(":0")), BackendKind::X11),
            ((None, None, None), BackendKind::Portal),
        ];
        for ((session, wayland, display), expected) in cases {
            assert_eq!(select_backend(session, wayland, display), expected);
        }
        assert_eq!(select_backend(Some("X11"), None, None), BackendKind::X11);
        assert_eq!(
            select_backend(Some("WaYlAnD"), None, Some(":0")),
            BackendKind::Portal
        );
    }

    #[test]
    fn reconnect_strategy_is_an_explicit_swap_point() {
        assert_eq!(
            reconnect_strategy(false),
            ReconnectStrategy::NewPortalSession
        );
        assert_eq!(
            reconnect_strategy(true),
            ReconnectStrategy::ReusePortalSession
        );
    }
}
