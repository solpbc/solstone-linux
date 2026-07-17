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
    chunking::{AudioLeg, DrainedChunk, StereoAccumulator},
    observer::{AudioCapture, MuteProbe},
};

const MAX_CONSECUTIVE_FAILURES: u8 = 3;
const REDETECT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct FailureCounter {
    consecutive: std::sync::atomic::AtomicU8,
    successful_legs: std::sync::atomic::AtomicU8,
}

impl FailureCounter {
    pub(crate) fn count(&self) -> u8 {
        self.consecutive.load(Ordering::Acquire)
    }

    pub(crate) fn failed(&self) -> bool {
        self.successful_legs.store(0, Ordering::Release);
        self.consecutive.fetch_add(1, Ordering::AcqRel) + 1 >= MAX_CONSECUTIVE_FAILURES
    }

    pub(crate) fn streams_ready(&self) {
        self.consecutive.store(0, Ordering::Release);
        self.successful_legs.store(0, Ordering::Release);
    }

    pub(crate) fn block_succeeded(&self, leg: AudioLeg) -> bool {
        let bit = match leg {
            AudioLeg::Microphone => 1,
            AudioLeg::System => 2,
        };
        let previous = self.successful_legs.fetch_or(bit, Ordering::AcqRel);
        if previous | bit == 3 {
            self.streams_ready();
            true
        } else {
            false
        }
    }
}

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
            }
            PulseMessage::SuccessfulRecord => {
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

pub(crate) struct PulseAudioCapture {
    shared: Arc<Mutex<Shared>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

pub(crate) struct PulseMuteProbe {
    shared: Arc<Mutex<Shared>>,
}

impl PulseAudioCapture {
    pub(crate) fn spawn() -> Result<(Self, PulseMuteProbe), String> {
        let (sender, receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let worker = thread::Builder::new()
            .name("solstone-audio".into())
            .spawn(move || {
                let failures = Arc::new(FailureCounter::default());
                supervise(
                    &worker_stopped,
                    &sender,
                    |sender, stopped, failures| {
                        run_pulse(sender, stopped, failures).map_err(|error| error.to_string())
                    },
                    wait_interruptibly,
                    failures,
                );
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

fn supervise<Run, Wait>(
    stopped: &Arc<AtomicBool>,
    sender: &mpsc::Sender<PulseMessage>,
    mut run: Run,
    mut wait: Wait,
    failures: Arc<FailureCounter>,
) where
    Run: FnMut(
        mpsc::Sender<PulseMessage>,
        Arc<AtomicBool>,
        Arc<FailureCounter>,
    ) -> Result<(), String>,
    Wait: FnMut(&AtomicBool, Duration) -> bool,
{
    while !stopped.load(Ordering::Acquire) {
        if let Err(reason) = run(sender.clone(), Arc::clone(stopped), Arc::clone(&failures)) {
            failures.failed();
            let _ = sender.send(PulseMessage::Failed(reason));
        }
        if !wait(stopped, REDETECT_INTERVAL) {
            break;
        }
    }
}

trait InterruptibleWait {
    fn park(&mut self, duration: Duration);
}

struct ThreadWait;
impl InterruptibleWait for ThreadWait {
    fn park(&mut self, duration: Duration) {
        thread::park_timeout(duration);
    }
}

fn wait_interruptibly(stopped: &AtomicBool, duration: Duration) -> bool {
    wait_interruptibly_with(stopped, duration, &mut ThreadWait)
}

fn wait_interruptibly_with(
    stopped: &AtomicBool,
    duration: Duration,
    waiter: &mut dyn InterruptibleWait,
) -> bool {
    let steps = duration.as_millis().div_ceil(100) as usize;
    for _ in 0..steps {
        if stopped.load(Ordering::Acquire) {
            return false;
        }
        waiter.park(Duration::from_millis(100));
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
    use std::{
        io::{self, Write},
        sync::atomic::AtomicUsize,
    };

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);
    struct LogWriter(Arc<Mutex<Vec<u8>>>);
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogWriter;
        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(Arc::clone(&self.0))
        }
    }
    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn availability_logs_once_per_transition_with_exact_copy() {
        // tests/test_audio_recorder.py::test_set_audio_available_edge_logs_once
        let output = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(output.clone())
            .finish();
        let mut state = CaptureState::default();
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..4 {
                state.apply(PulseMessage::Failed("read".into()));
            }
            for _ in 0..2 {
                state.apply(PulseMessage::Ready {
                    muted: false,
                    mute_failure: None,
                });
            }
        });
        let output = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            output
                .matches("Audio devices unavailable — continuing with screen capture only")
                .count(),
            1
        );
        assert_eq!(
            output
                .matches("Audio devices recovered — resuming audio capture")
                .count(),
            1
        );
        assert!(state.available);
    }

    #[test]
    fn third_consecutive_read_failure_requests_reconstruction() {
        // tests/test_audio_recorder.py::test_record_both_inner_record_failures_trigger_redetect
        let failures = FailureCounter::default();
        assert!(!failures.failed());
        assert!(!failures.failed());
        assert!(failures.failed());
        failures.streams_ready();
        assert!(!failures.failed());
    }

    #[test]
    fn repeated_start_failures_reconstruct_and_reach_degraded() {
        // tests/test_audio_recorder.py::test_record_both_setup_failures_trigger_redetect
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicUsize::new(0));
        supervise(
            &stopped,
            &sender,
            {
                let attempts = Arc::clone(&attempts);
                move |_, _, _| {
                    attempts.fetch_add(1, Ordering::AcqRel);
                    Err("stream start failed".into())
                }
            },
            {
                let waits = Arc::clone(&waits);
                let stopped = Arc::clone(&stopped);
                move |_, duration| {
                    assert_eq!(duration, Duration::from_secs(5));
                    let count = waits.fetch_add(1, Ordering::AcqRel) + 1;
                    if count == 3 {
                        stopped.store(true, Ordering::Release);
                        false
                    } else {
                        true
                    }
                }
            },
            Arc::new(FailureCounter::default()),
        );
        let mut shared = Shared {
            receiver,
            state: CaptureState::default(),
        };
        shared.pump();
        assert_eq!(attempts.load(Ordering::Acquire), 3);
        assert_eq!(waits.load(Ordering::Acquire), 3);
        assert!(!shared.state.available);
    }

    #[test]
    fn degraded_supervisor_recovers_without_process_restart() {
        // tests/test_audio_recorder.py::test_degraded_recorder_recovers_without_restart
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        supervise(
            &stopped,
            &sender,
            {
                let attempts = Arc::clone(&attempts);
                let stopped = Arc::clone(&stopped);
                move |sender, _, _| {
                    let attempt = attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt <= 3 {
                        Err("unavailable".into())
                    } else {
                        sender
                            .send(PulseMessage::Ready {
                                muted: false,
                                mute_failure: None,
                            })
                            .unwrap();
                        stopped.store(true, Ordering::Release);
                        Ok(())
                    }
                }
            },
            |stopped, _| !stopped.load(Ordering::Acquire),
            Arc::new(FailureCounter::default()),
        );
        let mut shared = Shared {
            receiver,
            state: CaptureState::default(),
        };
        shared.pump();
        assert_eq!(attempts.load(Ordering::Acquire), 4);
        assert!(shared.state.available);
        assert_eq!(shared.state.failures, 0);
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
        state.apply(PulseMessage::SuccessfulRecord);
        assert_eq!(state.failures, 0);
        assert!(state.muted);
    }

    #[test]
    fn production_interruptible_wait_observes_stop() {
        // tests/test_audio_recorder.py::test_sleep_interruptibly_exits_when_stopped
        struct FakeWait {
            stopped: Arc<AtomicBool>,
            parks: usize,
        }
        impl InterruptibleWait for FakeWait {
            fn park(&mut self, duration: Duration) {
                assert_eq!(duration, Duration::from_millis(100));
                self.parks += 1;
                self.stopped.store(true, Ordering::Release);
            }
        }
        let stopped = Arc::new(AtomicBool::new(false));
        let mut waiter = FakeWait {
            stopped: Arc::clone(&stopped),
            parks: 0,
        };
        assert!(!wait_interruptibly_with(
            &stopped,
            Duration::from_secs(5),
            &mut waiter
        ));
        assert_eq!(waiter.parks, 1);
    }
}
