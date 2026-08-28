// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    audio::pulse::{PulseMessage, PulseRunError, run_pulse},
    chunking::{AudioLeg, DrainedChunk, StereoAccumulator},
    observer::{AudioCapture, MuteProbe},
};

const MAX_CONSECUTIVE_FAILURES: u8 = 3;
const REDETECT_INTERVAL: Duration = Duration::from_secs(5);
// The process is shutting down after this bound, so a wedged Pulse mainloop
// must not keep the observer alive waiting for a Rust thread join forever.
const AUDIO_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[must_use = "audio diagnostics must be reported or explicitly consumed"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioDiagnostic {
    Unavailable,
    Recovered,
    Redetect { count: u8 },
}

impl AudioDiagnostic {
    fn level(self) -> tracing::Level {
        match self {
            Self::Unavailable => tracing::Level::WARN,
            Self::Recovered | Self::Redetect { .. } => tracing::Level::INFO,
        }
    }

    fn report(self) {
        match self.level() {
            tracing::Level::WARN => tracing::warn!("{self}"),
            tracing::Level::INFO => tracing::info!("{self}"),
            _ => unreachable!("audio diagnostics only use warning and info levels"),
        }
    }
}

impl fmt::Display for AudioDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => formatter
                .write_str("Audio devices unavailable — continuing with screen capture only"),
            Self::Recovered => {
                formatter.write_str("Audio devices recovered — resuming audio capture")
            }
            Self::Redetect { count } => write!(
                formatter,
                "Re-detecting audio devices after {count} consecutive recorder failures"
            ),
        }
    }
}

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

    pub(crate) fn detected(&self) {
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
            self.detected();
            true
        } else {
            false
        }
    }
}

fn report_redetect(failures: &FailureCounter, observe: &mut dyn FnMut(AudioDiagnostic)) {
    let diagnostic = AudioDiagnostic::Redetect {
        count: failures.count(),
    };
    diagnostic.report();
    observe(diagnostic);
    failures.detected();
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
            // audio_recorder.py:47 seeds this true so an initial detection failure is
            // an unavailable edge, while a successful startup does not log recovery.
            available: true,
            muted: false,
            failures: 0,
        }
    }
}

impl CaptureState {
    fn apply(&mut self, message: PulseMessage) -> Option<AudioDiagnostic> {
        match message {
            PulseMessage::Ready {
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    tracing::warn!(%reason, "unmuted (query failed)");
                }
                self.muted = muted;
                self.set_available(true)
            }
            PulseMessage::MuteChanged {
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    tracing::warn!(%reason, "unmuted (query failed)");
                }
                self.muted = muted;
                None
            }
            PulseMessage::Block(block) => {
                self.accumulator.push(block);
                None
            }
            PulseMessage::SuccessfulRecord => {
                self.failures = 0;
                None
            }
            PulseMessage::Detected => {
                self.failures = 0;
                self.set_available(true)
            }
            PulseMessage::Degraded(reason) => {
                tracing::debug!(%reason, "audio source detection degraded");
                self.set_available(false)
            }
            PulseMessage::Failed(reason) => {
                tracing::debug!(%reason, "recoverable audio capture failure");
                self.failures = self.failures.saturating_add(1);
                if self.failures >= MAX_CONSECUTIVE_FAILURES {
                    self.set_available(false)
                } else {
                    None
                }
            }
        }
    }

    fn set_available(&mut self, available: bool) -> Option<AudioDiagnostic> {
        if self.available == available {
            return None;
        }
        self.available = available;
        if available {
            Some(AudioDiagnostic::Recovered)
        } else {
            Some(AudioDiagnostic::Unavailable)
        }
    }
}

struct Shared {
    receiver: mpsc::Receiver<PulseMessage>,
    state: CaptureState,
}

impl Shared {
    fn pump(&mut self, observe: &mut dyn FnMut(AudioDiagnostic)) {
        while let Ok(message) = self.receiver.try_recv() {
            if let Some(diagnostic) = self.state.apply(message) {
                diagnostic.report();
                observe(diagnostic);
            }
        }
    }
}

pub(crate) struct PulseAudioCapture {
    shared: Arc<Mutex<Shared>>,
    stopped: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    worker_finished: Option<mpsc::Receiver<()>>,
}

pub(crate) struct PulseMuteProbe {
    shared: Arc<Mutex<Shared>>,
}

impl PulseAudioCapture {
    pub(crate) fn spawn() -> Result<(Self, PulseMuteProbe), String> {
        let (sender, receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = Arc::clone(&stopped);
        let (worker_finished_sender, worker_finished) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("solstone-audio".into())
            .spawn(move || {
                let failures = Arc::new(FailureCounter::default());
                supervise(
                    &worker_stopped,
                    &sender,
                    |sender, stopped, failures, reset_on_detection| {
                        run_pulse(sender, stopped, failures, reset_on_detection)
                    },
                    wait_interruptibly,
                    |_| {},
                    failures,
                );
                let _ = worker_finished_sender.send(());
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
                worker_finished: Some(worker_finished),
            },
            PulseMuteProbe { shared },
        ))
    }
}

fn supervise<Run, Wait, Observe>(
    stopped: &Arc<AtomicBool>,
    sender: &mpsc::Sender<PulseMessage>,
    mut run: Run,
    mut wait: Wait,
    mut observe: Observe,
    failures: Arc<FailureCounter>,
) where
    Run: FnMut(
        mpsc::Sender<PulseMessage>,
        Arc<AtomicBool>,
        Arc<FailureCounter>,
        bool,
    ) -> Result<bool, PulseRunError>,
    Wait: FnMut(&AtomicBool, Duration) -> bool,
    Observe: FnMut(AudioDiagnostic),
{
    let mut reset_on_detection = true;
    while !stopped.load(Ordering::Acquire) {
        match run(
            sender.clone(),
            Arc::clone(stopped),
            Arc::clone(&failures),
            reset_on_detection,
        ) {
            Ok(restart_for_failures) => {
                reset_on_detection = true;
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                if restart_for_failures {
                    report_redetect(&failures, &mut observe);
                }
            }
            Err(PulseRunError::Degraded(reason)) => {
                reset_on_detection = true;
                let _ = sender.send(PulseMessage::Degraded(reason));
                if !wait(stopped, REDETECT_INTERVAL) {
                    break;
                }
            }
            Err(PulseRunError::Setup(reason)) => {
                let threshold = failures.failed();
                let _ = sender.send(PulseMessage::Failed(reason));
                if !wait(stopped, Duration::from_secs(1)) {
                    break;
                }
                if threshold {
                    report_redetect(&failures, &mut observe);
                    reset_on_detection = true;
                } else {
                    // Rebuilding Pulse after setup failure remains the same logical
                    // detect cycle as Python's record_both setup retry.
                    reset_on_detection = false;
                }
            }
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

fn worker_stopped_within(
    worker_finished: &mpsc::Receiver<()>,
    timeout: Duration,
) -> Result<(), mpsc::RecvTimeoutError> {
    worker_finished.recv_timeout(timeout)
}

impl AudioCapture for PulseAudioCapture {
    fn drain(&mut self) -> DrainedChunk {
        let mut shared = self.shared.lock().expect("audio state lock");
        let mut observe = |_| {};
        shared.pump(&mut observe);
        shared.state.accumulator.drain()
    }

    fn audio_available(&self) -> bool {
        let mut shared = self.shared.lock().expect("audio state lock");
        let mut observe = |_| {};
        shared.pump(&mut observe);
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
            let stopped = self
                .worker_finished
                .take()
                .map(|finished| worker_stopped_within(&finished, AUDIO_SHUTDOWN_TIMEOUT));
            match stopped {
                Some(Ok(())) => {}
                Some(Err(mpsc::RecvTimeoutError::Timeout)) => {
                    tracing::warn!(
                        timeout_seconds = AUDIO_SHUTDOWN_TIMEOUT.as_secs(),
                        "Audio worker did not stop before shutdown bound; detaching"
                    );
                }
                Some(Err(mpsc::RecvTimeoutError::Disconnected)) => {
                    tracing::warn!("Audio worker ended without its shutdown acknowledgement");
                }
                None => {}
            }
            // Do not join here. A join can block forever inside a wedged Pulse
            // mainloop; dropping the handle detaches this process-owned worker,
            // which the process exit immediately following observer shutdown ends.
            drop(worker);
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
        let mut observe = |_| {};
        shared.pump(&mut observe);
        Ok(shared.state.muted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::{AudioLeg, LegBlock};
    use std::sync::atomic::AtomicUsize;

    fn shared_channel() -> (mpsc::Sender<PulseMessage>, Shared) {
        let (sender, receiver) = mpsc::channel();
        (
            sender,
            Shared {
                receiver,
                state: CaptureState::default(),
            },
        )
    }

    #[test]
    fn availability_logs_once_per_transition_with_exact_copy() {
        // tests/test_audio_recorder.py::test_set_audio_available_edge_logs_once
        let (sender, mut shared) = shared_channel();
        let mut seen = Vec::new();
        sender.send(PulseMessage::Failed("one".into())).unwrap();
        sender.send(PulseMessage::Failed("two".into())).unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());

        sender.send(PulseMessage::Failed("three".into())).unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Unavailable]);
        assert!(!shared.state.available);
        seen.clear();

        sender.send(PulseMessage::Failed("four".into())).unwrap();
        sender.send(PulseMessage::Failed("five".into())).unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());

        sender
            .send(PulseMessage::Ready {
                muted: false,
                mute_failure: None,
            })
            .unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Recovered]);
        seen.clear();
        sender
            .send(PulseMessage::Ready {
                muted: false,
                mute_failure: None,
            })
            .unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
        assert!(shared.state.available);
    }

    #[test]
    fn worker_shutdown_wait_has_a_hard_timeout() {
        let (_sender, receiver) = mpsc::sync_channel(0);
        assert_eq!(
            worker_stopped_within(&receiver, Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn worker_shutdown_wait_accepts_the_completion_acknowledgement() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(()).unwrap();
        assert_eq!(
            worker_stopped_within(&receiver, Duration::from_millis(1)),
            Ok(())
        );
    }

    #[test]
    fn initial_success_messages_do_not_report_availability_edges() {
        let (sender, mut shared) = shared_channel();
        sender
            .send(PulseMessage::Ready {
                muted: true,
                mute_failure: None,
            })
            .unwrap();
        sender.send(PulseMessage::Detected).unwrap();

        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
        assert!(shared.state.available);
        assert!(shared.state.muted);
    }

    #[test]
    fn adversarial_backlog_reports_every_availability_edge_in_order() {
        let (sender, mut shared) = shared_channel();
        for _ in 0..256 {
            sender.send(PulseMessage::Failed("one".into())).unwrap();
            sender.send(PulseMessage::Failed("two".into())).unwrap();
            sender.send(PulseMessage::Failed("three".into())).unwrap();
            sender.send(PulseMessage::Detected).unwrap();
        }

        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));

        assert_eq!(seen.len(), 512);
        assert!(
            seen.chunks_exact(2)
                .all(|pair| { pair == [AudioDiagnostic::Unavailable, AudioDiagnostic::Recovered] })
        );
        assert!(shared.state.available);
        assert_eq!(shared.state.failures, 0);
    }

    #[test]
    fn ready_and_detected_each_recover_once() {
        let (sender, mut shared) = shared_channel();
        sender
            .send(PulseMessage::Degraded("unavailable".into()))
            .unwrap();
        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Unavailable]);
        seen.clear();
        for _ in 0..2 {
            sender
                .send(PulseMessage::Ready {
                    muted: false,
                    mute_failure: None,
                })
                .unwrap();
            let expected = if shared.state.available {
                &[][..]
            } else {
                &[AudioDiagnostic::Recovered][..]
            };
            shared.pump(&mut |d| seen.push(d));
            assert_eq!(seen, expected);
            seen.clear();
        }
        assert!(shared.state.available);

        let (sender, mut shared) = shared_channel();
        sender
            .send(PulseMessage::Degraded("unavailable".into()))
            .unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Unavailable]);
        seen.clear();
        sender.send(PulseMessage::Detected).unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Recovered]);
        seen.clear();
        sender.send(PulseMessage::Detected).unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
        assert!(shared.state.available);
    }

    #[test]
    fn diagnostic_levels_and_redetect_rendering_are_stable() {
        assert_eq!(AudioDiagnostic::Unavailable.level(), tracing::Level::WARN);
        assert_eq!(
            AudioDiagnostic::Unavailable.to_string(),
            "Audio devices unavailable — continuing with screen capture only"
        );
        assert_eq!(AudioDiagnostic::Recovered.level(), tracing::Level::INFO);
        assert_eq!(
            AudioDiagnostic::Recovered.to_string(),
            "Audio devices recovered — resuming audio capture"
        );
        let redetect = AudioDiagnostic::Redetect { count: 3 };
        assert_eq!(redetect.level(), tracing::Level::INFO);
        assert_eq!(
            redetect.to_string(),
            "Re-detecting audio devices after 3 consecutive recorder failures"
        );
    }

    #[test]
    fn report_redetect_observes_count_before_reset() {
        let failures = FailureCounter::default();
        assert!(!failures.failed());
        assert!(!failures.failed());
        assert!(failures.failed());
        let mut observed_count = None;

        report_redetect(&failures, &mut |_| observed_count = Some(failures.count()));

        assert_eq!(observed_count, Some(3));
        assert_eq!(failures.count(), 0);
    }

    #[test]
    fn repeated_start_failures_reconstruct_and_reach_degraded() {
        // tests/test_audio_recorder.py::test_record_both_setup_failures_trigger_redetect
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let waits = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(FailureCounter::default());
        let run_states = Arc::new(Mutex::new(Vec::new()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let observed_counts = Arc::new(Mutex::new(Vec::new()));
        supervise(
            &stopped,
            &sender,
            {
                let attempts = Arc::clone(&attempts);
                let stopped = Arc::clone(&stopped);
                let run_states = Arc::clone(&run_states);
                let order = Arc::clone(&order);
                move |_, _, failures, reset_on_detection| {
                    order.lock().unwrap().push("run");
                    run_states
                        .lock()
                        .unwrap()
                        .push((reset_on_detection, failures.count()));
                    let attempt = attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt == 4 {
                        stopped.store(true, Ordering::Release);
                        Ok(false)
                    } else {
                        Err(PulseRunError::Setup("stream start failed".into()))
                    }
                }
            },
            {
                let waits = Arc::clone(&waits);
                let order = Arc::clone(&order);
                move |_, duration| {
                    assert_eq!(duration, Duration::from_secs(1));
                    assert!(matches!(receiver.try_recv(), Ok(PulseMessage::Failed(_))));
                    order.lock().unwrap().extend(["send", "wait"]);
                    waits.fetch_add(1, Ordering::AcqRel);
                    true
                }
            },
            {
                let order = Arc::clone(&order);
                let diagnostics = Arc::clone(&diagnostics);
                let failures = Arc::clone(&failures);
                let observed_counts = Arc::clone(&observed_counts);
                move |d| {
                    order.lock().unwrap().push("observe");
                    diagnostics.lock().unwrap().push(d);
                    observed_counts.lock().unwrap().push(failures.count());
                }
            },
            Arc::clone(&failures),
        );
        assert_eq!(attempts.load(Ordering::Acquire), 4);
        assert_eq!(waits.load(Ordering::Acquire), 3);
        assert_eq!(failures.count(), 0);
        assert_eq!(*observed_counts.lock().unwrap(), [3]);
        assert_eq!(
            *run_states.lock().unwrap(),
            [(true, 0), (false, 1), (false, 2), (true, 0)]
        );
        assert_eq!(
            *order.lock().unwrap(),
            [
                "run", "send", "wait", "run", "send", "wait", "run", "send", "wait", "observe",
                "run"
            ]
        );
        assert_eq!(
            *diagnostics.lock().unwrap(),
            [AudioDiagnostic::Redetect { count: 3 }]
        );
        assert_eq!(
            diagnostics.lock().unwrap()[0].to_string(),
            "Re-detecting audio devices after 3 consecutive recorder failures"
        );
    }

    #[test]
    fn successful_run_redetect_reports_before_reset() {
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, _) = mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(FailureCounter::default());
        let run_states = Arc::new(Mutex::new(Vec::new()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let observed_counts = Arc::new(Mutex::new(Vec::new()));
        supervise(
            &stopped,
            &sender,
            {
                let attempts = Arc::clone(&attempts);
                let stopped = Arc::clone(&stopped);
                let run_states = Arc::clone(&run_states);
                let order = Arc::clone(&order);
                move |_, _, failures, reset_on_detection| {
                    order.lock().unwrap().push("run");
                    run_states
                        .lock()
                        .unwrap()
                        .push((reset_on_detection, failures.count()));
                    let attempt = attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt == 1 {
                        assert!(!failures.failed());
                        assert!(!failures.failed());
                        assert!(failures.failed());
                        Ok(true)
                    } else {
                        stopped.store(true, Ordering::Release);
                        Ok(false)
                    }
                }
            },
            |_, _| panic!("successful runs must not wait"),
            {
                let order = Arc::clone(&order);
                let diagnostics = Arc::clone(&diagnostics);
                let failures = Arc::clone(&failures);
                let observed_counts = Arc::clone(&observed_counts);
                move |diagnostic| {
                    order.lock().unwrap().push("observe");
                    diagnostics.lock().unwrap().push(diagnostic);
                    observed_counts.lock().unwrap().push(failures.count());
                }
            },
            Arc::clone(&failures),
        );

        assert_eq!(attempts.load(Ordering::Acquire), 2);
        assert_eq!(*order.lock().unwrap(), ["run", "observe", "run"]);
        assert_eq!(*observed_counts.lock().unwrap(), [3]);
        assert_eq!(failures.count(), 0);
        assert_eq!(*run_states.lock().unwrap(), [(true, 0), (true, 0)]);
        assert_eq!(
            *diagnostics.lock().unwrap(),
            [AudioDiagnostic::Redetect { count: 3 }]
        );
        assert_eq!(diagnostics.lock().unwrap()[0].level(), tracing::Level::INFO);
        assert_eq!(
            diagnostics.lock().unwrap()[0].to_string(),
            "Re-detecting audio devices after 3 consecutive recorder failures"
        );
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
                move |sender, _, _, _| {
                    let attempt = attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    if attempt == 1 {
                        Err(PulseRunError::Degraded("unavailable".into()))
                    } else {
                        sender.send(PulseMessage::Detected).unwrap();
                        stopped.store(true, Ordering::Release);
                        Ok(false)
                    }
                }
            },
            |stopped, duration| {
                assert_eq!(duration, Duration::from_secs(5));
                !stopped.load(Ordering::Acquire)
            },
            |_| {},
            Arc::new(FailureCounter::default()),
        );
        let mut shared = Shared {
            receiver,
            state: CaptureState::default(),
        };
        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(
            seen,
            [AudioDiagnostic::Unavailable, AudioDiagnostic::Recovered]
        );
        assert_eq!(attempts.load(Ordering::Acquire), 2);
        assert!(shared.state.available);
        assert_eq!(shared.state.failures, 0);
    }

    #[test]
    fn successful_record_resets_failure_counter() {
        // tests/test_audio_recorder.py::test_record_both_success_resets_counter
        let (sender, mut shared) = shared_channel();
        sender.send(PulseMessage::Failed("one".into())).unwrap();
        sender
            .send(PulseMessage::Block(LegBlock::new(
                AudioLeg::Microphone,
                vec![0.1],
            )))
            .unwrap();
        sender.send(PulseMessage::SuccessfulRecord).unwrap();
        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
        assert_eq!(shared.state.failures, 0);
        let chunk = shared.state.accumulator.drain();
        assert_eq!(chunk.report.microphone.received_samples, 1);
    }

    #[test]
    fn mute_changed_has_no_availability_diagnostic() {
        let (sender, mut shared) = shared_channel();
        sender
            .send(PulseMessage::MuteChanged {
                muted: true,
                mute_failure: Some("query failed".into()),
            })
            .unwrap();

        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
        assert!(shared.state.muted);
    }

    #[test]
    fn classification_failure_is_immediately_unavailable() {
        // tests/test_audio_recorder.py::test_detect_degrades_when_only_mic
        // tests/test_audio_recorder.py::test_detect_degrades_when_only_loopback
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let waits = Arc::new(AtomicUsize::new(0));
        supervise(
            &stopped,
            &sender,
            {
                let stopped = Arc::clone(&stopped);
                move |_, _, _, _| {
                    stopped.store(true, Ordering::Release);
                    Err(PulseRunError::Degraded("missing audio leg".into()))
                }
            },
            {
                let waits = Arc::clone(&waits);
                move |_, duration| {
                    assert_eq!(duration, Duration::from_secs(5));
                    waits.fetch_add(1, Ordering::AcqRel);
                    false
                }
            },
            |_| {},
            Arc::new(FailureCounter::default()),
        );
        let mut shared = Shared {
            receiver,
            state: CaptureState::default(),
        };
        let mut seen = Vec::new();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Unavailable]);
        assert!(!shared.state.available);
        assert_eq!(waits.load(Ordering::Acquire), 1);

        let (sender, mut shared) = shared_channel();
        sender.send(PulseMessage::Degraded("first".into())).unwrap();
        seen.clear();
        shared.pump(&mut |d| seen.push(d));
        assert_eq!(seen, [AudioDiagnostic::Unavailable]);
        seen.clear();
        sender
            .send(PulseMessage::Degraded("repeat".into()))
            .unwrap();
        shared.pump(&mut |d| seen.push(d));
        assert!(seen.is_empty());
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
