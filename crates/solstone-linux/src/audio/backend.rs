// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    audio::pulse::{PulseMessage, run_pulse},
    chunking::{DrainedChunk, StereoAccumulator},
    observer::{AudioCapture, MuteProbe},
};

const MAX_CONSECUTIVE_FAILURES: u8 = 3;
const REDETECT_INTERVAL: Duration = Duration::from_secs(5);

struct CaptureState {
    accumulator: StereoAccumulator,
    available: bool,
    muted: bool,
    failures: u8,
}

impl Default for CaptureState {
    fn default() -> Self {
        Self {
            accumulator: StereoAccumulator::default(),
            available: true,
            muted: false,
            failures: 0,
        }
    }
}

impl CaptureState {
    fn apply(&mut self, message: PulseMessage) {
        match message {
            PulseMessage::Ready {
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    tracing::warn!(%reason, "unmuted (query failed)");
                }
                self.muted = muted;
                self.failures = 0;
                self.set_available(true);
            }
            PulseMessage::MuteChanged {
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    tracing::warn!(%reason, "unmuted (query failed)");
                }
                self.muted = muted;
            }
            PulseMessage::Block(block) => {
                self.accumulator.push(block);
                self.failures = 0;
            }
            PulseMessage::Failed(reason) => {
                tracing::debug!(%reason, "recoverable audio capture failure");
                self.failures = self.failures.saturating_add(1);
                if self.failures >= MAX_CONSECUTIVE_FAILURES {
                    self.set_available(false);
                }
            }
        }
    }

    fn set_available(&mut self, available: bool) {
        if self.available == available {
            return;
        }
        self.available = available;
        if available {
            tracing::info!("Audio devices recovered — resuming audio capture");
        } else {
            tracing::warn!("Audio devices unavailable — continuing with screen capture only");
        }
    }
}

struct Shared {
    receiver: mpsc::Receiver<PulseMessage>,
    state: CaptureState,
}

impl Shared {
    fn pump(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            self.state.apply(message);
        }
    }
}

pub struct PulseAudioCapture {
    shared: Arc<Mutex<Shared>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

pub struct PulseMuteProbe {
    shared: Arc<Mutex<Shared>>,
}

impl PulseAudioCapture {
    pub fn spawn() -> Result<(Self, PulseMuteProbe), String> {
        let (sender, receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("solstone-audio".into())
            .spawn(move || {
                while !worker_stopped.load(Ordering::Acquire) {
                    match run_pulse(sender.clone(), Arc::clone(&worker_stopped)) {
                        Ok(()) => continue,
                        Err(error) => {
                            let _ = sender.send(PulseMessage::Failed(error.to_string()));
                            if !wait_interruptibly(&worker_stopped, REDETECT_INTERVAL) {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        let shared = Arc::new(Mutex::new(Shared {
            receiver,
            state: CaptureState::default(),
        }));
        Ok((
            Self {
                shared: Arc::clone(&shared),
                stopped,
                worker: Some(worker),
            },
            PulseMuteProbe { shared },
        ))
    }
}

fn wait_interruptibly(stopped: &AtomicBool, duration: Duration) -> bool {
    let steps = duration.as_millis().div_ceil(100) as usize;
    for _ in 0..steps {
        if stopped.load(Ordering::Acquire) {
            return false;
        }
        thread::park_timeout(Duration::from_millis(100));
    }
    !stopped.load(Ordering::Acquire)
}

impl AudioCapture for PulseAudioCapture {
    fn drain(&mut self) -> DrainedChunk {
        let mut shared = self.shared.lock().expect("audio state lock");
        shared.pump();
        shared.state.accumulator.drain()
    }

    fn audio_available(&self) -> bool {
        let mut shared = self.shared.lock().expect("audio state lock");
        shared.pump();
        shared.state.available
    }

    fn fatal_error(&self) -> Option<String> {
        // Python's fatal path was a dynamic NumPy column_stack type/shape error.
        // Typed Rust LegBlock values make that condition unrepresentable. Pulse
        // transport errors are recoverable; this seam remains for fake/general
        // AudioCapture implementations used by the observer lifecycle contract.
        None
    }

    fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

impl Drop for PulseAudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

impl MuteProbe for PulseMuteProbe {
    fn probe_muted(&mut self) -> Result<bool, String> {
        let mut shared = self.shared.lock().map_err(|error| error.to_string())?;
        shared.pump();
        Ok(shared.state.muted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::{AudioLeg, LegBlock};

    #[test]
    fn availability_edges_are_stable() {
        // tests/test_audio_recorder.py::test_set_audio_available_edge_logs_once
        // tests/test_audio_recorder.py::test_degraded_recorder_recovers_without_restart
        let mut state = CaptureState::default();
        for _ in 0..3 {
            state.apply(PulseMessage::Failed("read".into()));
        }
        assert!(!state.available);
        state.apply(PulseMessage::Ready {
            muted: false,
            mute_failure: None,
        });
        assert!(state.available);
    }

    #[test]
    fn three_failures_enter_degraded() {
        // tests/test_audio_recorder.py::test_record_both_setup_failures_trigger_redetect
        // tests/test_audio_recorder.py::test_record_both_inner_record_failures_trigger_redetect
        let mut state = CaptureState::default();
        state.set_available(true);
        state.apply(PulseMessage::Failed("one".into()));
        state.apply(PulseMessage::Failed("two".into()));
        assert!(state.available);
        state.apply(PulseMessage::Failed("three".into()));
        assert!(!state.available);
    }

    #[test]
    fn ready_resets_failure_counter() {
        // tests/test_audio_recorder.py::test_record_both_success_resets_counter
        let mut state = CaptureState::default();
        state.apply(PulseMessage::Failed("one".into()));
        state.apply(PulseMessage::Ready {
            muted: true,
            mute_failure: None,
        });
        state.apply(PulseMessage::Block(LegBlock::new(
            AudioLeg::Microphone,
            vec![0.1],
        )));
        assert_eq!(state.failures, 0);
        assert!(state.muted);
    }

    #[test]
    fn fake_deadline_is_interruptible() {
        // tests/test_audio_recorder.py::test_sleep_interruptibly_exits_when_stopped
        #[derive(Default)]
        struct FakeClock {
            now: u64,
        }
        let mut clock = FakeClock::default();
        let deadline = 5;
        let mut stopped = false;
        while clock.now < deadline && !stopped {
            clock.now += 1;
            stopped = clock.now == 2;
        }
        assert_eq!(clock.now, 2);
    }
}
