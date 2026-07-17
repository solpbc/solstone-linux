// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub mod backend;
pub mod pulse;
pub mod writer;

// Python audio test inventory (tests/test_audio_mute.py does not exist; mute
// coverage below is new Rust coverage of audio_mute.py's observable contract):
// tests/test_audio_recorder.py::test_record_both_stereo_layout_mic_left_sys_right
//   -> chunking::tests::retained_tail_pairs_on_next_drain_without_drift.
// tests/test_audio_recorder.py::test_create_flac_and_mono_flac_bytes_nonempty
//   -> audio::writer::tests::writes_stereo_and_split_mono_flac.
// tests/test_audio_recorder.py::test_set_audio_available_edge_logs_once
//   -> audio::backend::tests::availability_edges_are_stable.
// tests/test_audio_recorder.py::test_degraded_recorder_recovers_without_restart
//   -> audio::backend::tests::availability_edges_are_stable.
// tests/test_audio_recorder.py::test_detect_degrades_when_only_mic
// tests/test_audio_recorder.py::test_detect_degrades_when_only_loopback
//   -> sources::tests::missing_legs_are_explicit.
// tests/test_audio_recorder.py::test_record_both_setup_failures_trigger_redetect
// tests/test_audio_recorder.py::test_record_both_inner_record_failures_trigger_redetect
//   -> audio::backend::tests::three_failures_enter_degraded.
// tests/test_audio_recorder.py::test_record_both_success_resets_counter
//   -> audio::backend::tests::ready_resets_failure_counter.
// tests/test_audio_recorder.py::test_sleep_interruptibly_exits_when_stopped
//   -> audio::backend::tests::fake_deadline_is_interruptible.
// tests/test_audio_recorder.py::test_fatal_format_error_untouched_by_counter
//   retired-by-language: typed Rust leg blocks make NumPy column_stack failures unrepresentable.
// tests/test_audio_detect.py::test_input_detect_detects_both_legs_without_any_signal
//   -> sources::tests::selects_first_mic_and_first_monitor_structurally.
// tests/test_audio_detect.py::test_input_detect_never_plays_tone
//   retired-by-dependency: native Pulse source metadata never plays a detection tone.
// tests/test_audio_detect.py::test_input_detect_hung_device_treated_absent_within_bound
//   -> audio::pulse::tests::metadata_deadline_keeps_earlier_sources.
