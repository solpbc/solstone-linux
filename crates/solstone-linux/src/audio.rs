// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub(crate) mod backend;
pub(crate) mod pulse;
pub(crate) mod writer;

// Python audio test inventory (tests/test_audio_mute.py does not exist; mute
// coverage below is new Rust coverage of audio_mute.py's observable contract):
// tests/test_audio_recorder.py::test_record_both_stereo_layout_mic_left_sys_right
//   -> chunking::tests::retained_tail_pairs_on_next_drain_without_drift.
// tests/test_audio_recorder.py::test_create_flac_and_mono_flac_bytes_nonempty
//   -> audio::writer::tests::writes_stereo_and_split_mono_flac.
// tests/test_audio_recorder.py::test_set_audio_available_edge_logs_once
//   -> audio::backend::tests::availability_logs_once_per_transition_with_exact_copy.
// tests/test_audio_recorder.py::test_degraded_recorder_recovers_without_restart
//   -> audio::backend::tests::degraded_supervisor_recovers_without_process_restart.
// tests/test_audio_recorder.py::test_detect_degrades_when_only_mic
// tests/test_audio_recorder.py::test_detect_degrades_when_only_loopback
//   -> sources::tests::missing_legs_are_explicit (classification) and
//      audio::backend::tests::classification_failure_is_immediately_unavailable.
// tests/test_audio_recorder.py::test_record_both_setup_failures_trigger_redetect
//   -> audio::pulse::tests::failed_stream_setup_never_publishes_ready and
//      audio::backend::tests::repeated_start_failures_reconstruct_and_reach_degraded.
// tests/test_audio_recorder.py::test_record_both_inner_record_failures_trigger_redetect
//   -> audio::pulse::tests::third_read_failure_requests_reconstruction. Python's
//      0.5-second retry sleep is retired-by-dependency: Pulse callbacks do not hot-spin.
// tests/test_audio_recorder.py::test_record_both_success_resets_counter
//   -> audio::backend::tests::successful_record_resets_failure_counter and
//      audio::pulse::tests::stream_setup_success_does_not_reset_failures.
// tests/test_audio_recorder.py::test_sleep_interruptibly_exits_when_stopped
//   -> audio::backend::tests::production_interruptible_wait_observes_stop.
// tests/test_audio_recorder.py::test_fatal_format_error_untouched_by_counter
//   retired-by-language: typed Rust leg blocks make NumPy column_stack failures unrepresentable.
// tests/test_audio_detect.py::test_input_detect_detects_both_legs_without_any_signal
//   -> sources::tests::selects_first_mic_and_first_monitor_structurally.
// tests/test_audio_detect.py::test_input_detect_never_plays_tone
//   retired-by-dependency: native Pulse source metadata never plays a detection tone.
// tests/test_audio_detect.py::test_input_detect_hung_device_treated_absent_within_bound
//   -> audio::pulse::tests::metadata_deadline_keeps_earlier_sources.
// Empty-input assertions in test_create_flac_and_mono_flac_bytes_nonempty are
// retired-by-call-contract: the observer hit gate never invokes AudioWriter with empty frames.
