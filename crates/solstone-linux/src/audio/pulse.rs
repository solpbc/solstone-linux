// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    chunking::{AudioLeg, LegBlock, SAMPLE_RATE},
    sources::{SourceDescriptor, SourceSelection, classify_sources},
    subscription::{
        MuteStatus, SubscriptionAction, SubscriptionEvent, SubscriptionOperation,
        SubscriptionState, transition,
    },
};
use libpulse_binding as pulse;
use pulse::{
    callbacks::ListResult,
    context::{
        Context, FlagSet as ContextFlagSet, State as ContextState,
        subscribe::{Facility, InterestMaskSet, Operation},
    },
    mainloop::threaded::Mainloop,
    sample::{Format, Spec},
    stream::{FlagSet as StreamFlagSet, PeekResult, State as StreamState, Stream},
};
use tracing::{info, warn};

type AnyError = Box<dyn Error + Send + Sync>;
const PULSE_TIMEOUT: Duration = Duration::from_secs(5);
const SOURCE_METADATA_TIMEOUT: Duration = Duration::from_secs(3);
const REDETECT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct RedetectSchedule {
    deadline: Duration,
}

impl RedetectSchedule {
    fn new(now: Duration) -> Self {
        Self {
            deadline: now + REDETECT_INTERVAL,
        }
    }

    fn remaining(self, now: Duration) -> Duration {
        self.deadline.saturating_sub(now)
    }

    fn due(self, now: Duration) -> bool {
        now >= self.deadline
    }

    fn reset(&mut self, now: Duration) {
        self.deadline = now + REDETECT_INTERVAL;
    }
}

pub(crate) enum PulseMessage {
    Ready {
        muted: bool,
        mute_failure: Option<String>,
    },
    MuteChanged {
        muted: bool,
        mute_failure: Option<String>,
    },
    Block(LegBlock),
    Failed(String),
}

fn report_selection(selection: &SourceSelection) {
    info!(
        microphone = ?selection.microphone.name,
        monitor = ?selection.monitor.name,
        "selected first microphone and monitor sources"
    );
    match selection.monitor_matches_default_sink {
        Some(true) => {}
        Some(false) => warn!("chosen monitor is not the default sink monitor"),
        None => warn!("chosen monitor has no sink name; default-sink comparison unavailable"),
    }
}

struct SubscriptionOutcome {
    state: SubscriptionState,
    mute_changes: Vec<MuteStatus>,
    source_changed: bool,
}

fn drive_subscription(
    mainloop: &mut Mainloop,
    context: &Context,
    mut state: SubscriptionState,
    event: SubscriptionEvent,
) -> Result<SubscriptionOutcome, AnyError> {
    let previous_selection = state.source_selection.clone();
    let mut events = VecDeque::from([event]);
    let mut mute_changes = Vec::new();
    while let Some(event) = events.pop_front() {
        let outcome = transition(state, event);
        state = outcome.state;
        for action in outcome.actions {
            match action {
                SubscriptionAction::QuerySources => {
                    let result = match query_sources(mainloop, context) {
                        Ok(sources) => classify_sources(
                            &sources,
                            state.default_sink.as_ref().map(|sink| sink.name.as_str()),
                            None,
                        ),
                        Err(error) => Err(crate::sources::SourceSelectionError::EnumerationFailed(
                            error.to_string(),
                        )),
                    };
                    events.push_back(SubscriptionEvent::SourcesResolved(result));
                }
                SubscriptionAction::QueryDefaultSink => events.push_back(
                    SubscriptionEvent::DefaultSinkResolved(query_default_sink(mainloop, context)),
                ),
                SubscriptionAction::QueryMute { sink_name } => {
                    events.push_back(SubscriptionEvent::MuteQueryResolved(query_sink_mute(
                        mainloop, context, &sink_name,
                    )))
                }
                SubscriptionAction::ApplySourceSelection(selection) => {
                    report_selection(&selection);
                }
                SubscriptionAction::EnterDegraded { reason } => {
                    warn!(%reason, "audio source enumeration degraded; keeping existing streams");
                }
                SubscriptionAction::ApplyMuteBoundary { status } => mute_changes.push(status),
            }
        }
    }
    Ok(SubscriptionOutcome {
        source_changed: previous_selection != state.source_selection,
        state,
        mute_changes,
    })
}

fn map_subscription_event(
    facility: Option<Facility>,
    operation: Option<Operation>,
    index: u32,
) -> Option<SubscriptionEvent> {
    let operation = match operation {
        Some(Operation::New) => SubscriptionOperation::New,
        Some(Operation::Changed) => SubscriptionOperation::Changed,
        Some(Operation::Removed) => SubscriptionOperation::Removed,
        None => return None,
    };
    match facility {
        Some(Facility::Source) => Some(SubscriptionEvent::SourceSubscription { operation, index }),
        Some(Facility::Sink) if operation == SubscriptionOperation::Changed => {
            Some(SubscriptionEvent::SinkSubscriptionChanged { index })
        }
        Some(Facility::Server) if operation == SubscriptionOperation::Changed => {
            Some(SubscriptionEvent::ServerSubscriptionChanged)
        }
        _ => None,
    }
}

fn mute_result(status: &MuteStatus) -> (bool, Option<String>) {
    match status {
        MuteStatus::Muted => (true, None),
        MuteStatus::Unknown | MuteStatus::Unmuted => (false, None),
        MuteStatus::UnmutedQueryFailed { reason } => (false, Some(reason.clone())),
    }
}

pub(crate) fn run_pulse(
    sender: mpsc::Sender<PulseMessage>,
    stopped: Arc<AtomicBool>,
) -> Result<(), AnyError> {
    let mut mainloop = Mainloop::new().ok_or("failed to create PulseAudio threaded mainloop")?;
    let mut context =
        Context::new(&mainloop, "solstone-linux").ok_or("failed to create PulseAudio context")?;
    context.connect(None, ContextFlagSet::NOFLAGS, None)?;
    mainloop.start()?;
    wait_for_context(&mut mainloop, &context)?;

    let outcome = drive_subscription(
        &mut mainloop,
        &context,
        SubscriptionState::default(),
        SubscriptionEvent::Started,
    )?;
    let state = outcome.state;
    let selection = state.source_selection.clone().ok_or_else(|| {
        format!(
            "audio degraded: {}",
            state
                .degraded_reason
                .as_deref()
                .unwrap_or("source selection was not resolved")
        )
    })?;
    let (muted, mute_failure) = mute_result(&state.mute_status);
    sender.send(PulseMessage::Ready {
        muted,
        mute_failure,
    })?;

    // Pulse streams are long-lived; Python's 1000-block rebuild is soundcard-library hygiene.
    let restart = Arc::new(AtomicBool::new(false));
    let microphone = start_stream(
        &mut mainloop,
        &mut context,
        selection
            .microphone
            .name
            .as_deref()
            .ok_or("microphone has no name")?,
        AudioLeg::Microphone,
        sender.clone(),
        Arc::clone(&restart),
    )?;
    let system = start_stream(
        &mut mainloop,
        &mut context,
        selection
            .monitor
            .name
            .as_deref()
            .ok_or("monitor has no name")?,
        AudioLeg::System,
        sender.clone(),
        Arc::clone(&restart),
    )?;
    info!(default_sink = ?state.default_sink, "PulseAudio capture ready");

    let (event_tx, event_rx) = mpsc::channel();
    context.set_subscribe_callback(Some(Box::new(move |facility, operation, index| {
        let _ = event_tx.send((facility, operation, index));
    })));
    mainloop.lock();
    let subscription = context.subscribe(
        InterestMaskSet::SOURCE | InterestMaskSet::SINK | InterestMaskSet::SERVER,
        |_| {},
    );
    mainloop.unlock();
    let mut state = state;
    let schedule_started = Instant::now();
    let mut redetect = RedetectSchedule::new(schedule_started.elapsed());
    while !stopped.load(Ordering::SeqCst) && !restart.load(Ordering::Acquire) {
        let wait = redetect
            .remaining(schedule_started.elapsed())
            .min(Duration::from_millis(100));
        match event_rx.recv_timeout(wait) {
            Ok((facility, operation, index)) => {
                let Some(event) = map_subscription_event(facility, operation, index) else {
                    continue;
                };
                let outcome = drive_subscription(&mut mainloop, &context, state, event)?;
                let source_changed = outcome.source_changed;
                state = outcome.state;
                for status in outcome.mute_changes {
                    let (muted, mute_failure) = mute_result(&status);
                    sender.send(PulseMessage::MuteChanged {
                        muted,
                        mute_failure,
                    })?;
                }
                if source_changed {
                    restart.store(true, Ordering::Release);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if redetect.due(schedule_started.elapsed()) {
            let outcome =
                drive_subscription(&mut mainloop, &context, state, SubscriptionEvent::Started)?;
            let source_changed = outcome.source_changed;
            state = outcome.state;
            for status in outcome.mute_changes {
                let (muted, mute_failure) = mute_result(&status);
                sender.send(PulseMessage::MuteChanged {
                    muted,
                    mute_failure,
                })?;
            }
            if source_changed {
                restart.store(true, Ordering::Release);
            }
            redetect.reset(schedule_started.elapsed());
        }
    }
    drop(subscription);
    mainloop.lock();
    let _ = microphone
        .lock()
        .expect("microphone stream lock")
        .disconnect();
    let _ = system.lock().expect("system stream lock").disconnect();
    context.disconnect();
    mainloop.unlock();
    mainloop.stop();
    Ok(())
}

fn wait_for_context(mainloop: &mut Mainloop, context: &Context) -> Result<(), AnyError> {
    let deadline = Instant::now() + PULSE_TIMEOUT;
    loop {
        mainloop.lock();
        let state = context.get_state();
        mainloop.unlock();
        match state {
            ContextState::Ready => return Ok(()),
            ContextState::Failed | ContextState::Terminated => {
                return Err(format!("PulseAudio context failed: {state:?}").into());
            }
            _ if Instant::now() >= deadline => {
                return Err("PulseAudio context readiness timed out after 5 seconds".into());
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn query_sources(
    mainloop: &mut Mainloop,
    context: &Context,
) -> Result<Vec<SourceDescriptor>, AnyError> {
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context.introspect().get_source_info_list(move |result| {
        let mapped = match result {
            ListResult::Item(source) => Ok(Some(SourceDescriptor {
                index: source.index,
                name: source.name.as_deref().map(Into::into),
                monitor_of_sink: source.monitor_of_sink,
                monitor_of_sink_name: source.monitor_of_sink_name.as_deref().map(Into::into),
            })),
            ListResult::End => Ok(None),
            ListResult::Error => Err("PulseAudio source enumeration failed"),
        };
        let _ = tx.send(mapped);
    });
    mainloop.unlock();
    struct ChannelEvents(mpsc::Receiver<Result<Option<SourceDescriptor>, &'static str>>);
    impl SourceEventSource for ChannelEvents {
        fn next(&mut self, timeout: Duration) -> SourceEvent {
            match self.0.recv_timeout(timeout) {
                Ok(Ok(Some(source))) => SourceEvent::Item(source),
                Ok(Ok(None)) => SourceEvent::End,
                Ok(Err(reason)) => SourceEvent::Failed(reason.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => SourceEvent::Pending,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    SourceEvent::Failed("PulseAudio source enumeration disconnected".into())
                }
            }
        }
    }
    struct RealClock(Instant);
    impl MetadataClock for RealClock {
        fn elapsed(&self) -> Duration {
            self.0.elapsed()
        }
    }
    let mut events = ChannelEvents(rx);
    let clock = RealClock(Instant::now());
    let sources = collect_source_metadata(&mut events, &clock)?;
    drop(operation);
    Ok(sources)
}

enum SourceEvent {
    Item(SourceDescriptor),
    End,
    Pending,
    Failed(String),
}

trait SourceEventSource {
    fn next(&mut self, timeout: Duration) -> SourceEvent;
}

trait MetadataClock {
    fn elapsed(&self) -> Duration;
}

fn collect_source_metadata(
    events: &mut dyn SourceEventSource,
    clock: &dyn MetadataClock,
) -> Result<Vec<SourceDescriptor>, AnyError> {
    let mut sources = Vec::new();
    // Pulse emits one callback per source and then End. Preserve descriptors that
    // arrived before a wedged operation omits End; the fixed three-second deadline
    // is the native analogue of abandoning one hung Python device-metadata call.
    while clock.elapsed() < SOURCE_METADATA_TIMEOUT {
        let remaining = SOURCE_METADATA_TIMEOUT.saturating_sub(clock.elapsed());
        if remaining.is_zero() {
            break;
        }
        match events.next(remaining) {
            SourceEvent::Item(source) => sources.push(source),
            SourceEvent::End => break,
            SourceEvent::Pending => continue,
            SourceEvent::Failed(reason) => return Err(reason.into()),
        }
    }
    Ok(sources)
}

fn query_default_sink(
    mainloop: &mut Mainloop,
    context: &Context,
) -> Result<crate::subscription::DefaultSink, String> {
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context.introspect().get_server_info(move |server| {
        let _ = tx.send(server.default_sink_name.as_deref().map(str::to_owned));
    });
    mainloop.unlock();
    let name = match rx.recv_timeout(PULSE_TIMEOUT) {
        Ok(Some(name)) => name,
        Ok(None) => return Err("server has no default sink".into()),
        Err(error) => return Err(format!("default sink query failed: {error}")),
    };
    drop(operation);
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context
        .introspect()
        .get_sink_info_by_name(&name, move |result| {
            let value = match result {
                ListResult::Item(sink) => Some(sink.index),
                ListResult::End | ListResult::Error => None,
            };
            let _ = tx.send(value);
        });
    mainloop.unlock();
    let result = rx.recv_timeout(PULSE_TIMEOUT);
    drop(operation);
    match result {
        Ok(Some(index)) => Ok(crate::subscription::DefaultSink { index, name }),
        Ok(None) => Err("default sink lookup failed".into()),
        Err(error) => Err(format!("default sink lookup failed: {error}")),
    }
}

fn query_sink_mute(
    mainloop: &mut Mainloop,
    context: &Context,
    sink_name: &str,
) -> Result<bool, String> {
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context
        .introspect()
        .get_sink_info_by_name(sink_name, move |result| {
            let value = match result {
                ListResult::Item(sink) => Some(sink.mute),
                ListResult::End | ListResult::Error => None,
            };
            let _ = tx.send(value);
        });
    mainloop.unlock();
    let result = rx.recv_timeout(PULSE_TIMEOUT);
    drop(operation);
    match result {
        Ok(Some(muted)) => Ok(muted),
        Ok(None) => Err("default sink mute lookup failed".into()),
        Err(error) => Err(format!("mute query failed: {error}")),
    }
}

fn start_stream(
    mainloop: &mut Mainloop,
    context: &mut Context,
    source_name: &str,
    leg: AudioLeg,
    sender: mpsc::Sender<PulseMessage>,
    restart: Arc<AtomicBool>,
) -> Result<Arc<Mutex<Stream>>, AnyError> {
    let spec = Spec {
        format: Format::F32le,
        channels: 1,
        rate: SAMPLE_RATE,
    };
    let stream = Arc::new(Mutex::new(
        Stream::new(context, "solstone audio leg", &spec, None)
            .ok_or("failed to create PulseAudio record stream")?,
    ));
    let weak: Weak<Mutex<Stream>> = Arc::downgrade(&stream);
    stream
        .lock()
        .expect("stream lock")
        .set_read_callback(Some(Box::new(move |_| {
            read_stream(&weak, leg, &sender, &restart)
        })));
    mainloop.lock();
    let connect_result = stream.lock().expect("stream lock").connect_record(
        Some(source_name),
        None,
        StreamFlagSet::ADJUST_LATENCY,
    );
    mainloop.unlock();
    connect_result?;
    let deadline = Instant::now() + PULSE_TIMEOUT;
    loop {
        mainloop.lock();
        let state = stream.lock().expect("stream lock").get_state();
        mainloop.unlock();
        match state {
            StreamState::Ready => break,
            StreamState::Failed | StreamState::Terminated => {
                return Err(format!("record stream failed: {state:?}").into());
            }
            _ if Instant::now() >= deadline => {
                return Err("PulseAudio stream readiness timed out after 5 seconds".into());
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }
    mainloop.lock();
    let attributes = stream
        .lock()
        .expect("stream lock")
        .get_buffer_attr()
        .copied();
    mainloop.unlock();
    info!(?leg, ?attributes, "negotiated PulseAudio buffer attributes");
    Ok(stream)
}

fn read_stream(
    weak: &Weak<Mutex<Stream>>,
    leg: AudioLeg,
    sender: &mpsc::Sender<PulseMessage>,
    restart: &AtomicBool,
) {
    let Some(stream) = weak.upgrade() else { return };
    let mut stream = stream.lock().expect("stream callback lock");
    let bytes = match stream.peek() {
        Ok(PeekResult::Data(data)) => Some(data.to_vec()),
        Ok(PeekResult::Hole(length)) => {
            warn!(?leg, length, "PulseAudio record hole");
            None
        }
        Ok(PeekResult::Empty) => return,
        Err(error) => {
            let _ = sender.send(PulseMessage::Failed(format!("record peek failed: {error}")));
            restart.store(true, Ordering::Release);
            return;
        }
    };
    if let Err(error) = stream.discard() {
        let _ = sender.send(PulseMessage::Failed(format!(
            "record discard failed: {error}"
        )));
        restart.store(true, Ordering::Release);
        return;
    }
    drop(stream);
    if let Some(bytes) = bytes {
        let remainder = bytes.len() % size_of::<f32>();
        if remainder != 0 {
            warn!(?leg, remainder, "float32 record buffer has trailing bytes");
        }
        let samples = bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte float")))
            .collect();
        let _ = sender.send(PulseMessage::Block(LegBlock::new(leg, samples)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque, rc::Rc};

    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<Duration>>);
    impl MetadataClock for FakeClock {
        fn elapsed(&self) -> Duration {
            self.0.get()
        }
    }

    struct FakeEvents {
        clock: FakeClock,
        ready: VecDeque<SourceDescriptor>,
    }
    impl SourceEventSource for FakeEvents {
        fn next(&mut self, timeout: Duration) -> SourceEvent {
            if let Some(source) = self.ready.pop_front() {
                return SourceEvent::Item(source);
            }
            self.clock.0.set(self.clock.elapsed() + timeout);
            SourceEvent::Pending
        }
    }

    #[test]
    fn metadata_deadline_keeps_earlier_sources() {
        // tests/test_audio_detect.py::test_input_detect_hung_device_treated_absent_within_bound
        let clock = FakeClock::default();
        let mut events = FakeEvents {
            clock: clock.clone(),
            ready: VecDeque::from([
                SourceDescriptor {
                    index: 1,
                    name: Some("mic".into()),
                    monitor_of_sink: None,
                    monitor_of_sink_name: None,
                },
                SourceDescriptor {
                    index: 2,
                    name: Some("sink.monitor".into()),
                    monitor_of_sink: Some(7),
                    monitor_of_sink_name: Some("sink".into()),
                },
            ]),
        };
        let sources = collect_source_metadata(&mut events, &clock).unwrap();
        assert_eq!(clock.elapsed(), Duration::from_secs(3));
        let selection = classify_sources(&sources, Some("sink"), None).unwrap();
        assert_eq!(selection.microphone.name.as_deref(), Some("mic"));
        assert_eq!(selection.monitor.name.as_deref(), Some("sink.monitor"));
    }

    #[test]
    fn fake_clock_drives_five_second_backstop() {
        let clock = FakeClock::default();
        let mut schedule = RedetectSchedule::new(clock.elapsed());
        clock.0.set(Duration::from_secs(4));
        assert!(!schedule.due(clock.elapsed()));
        clock.0.set(Duration::from_secs(5));
        assert!(schedule.due(clock.elapsed()));
        schedule.reset(clock.elapsed());
        assert_eq!(schedule.remaining(clock.elapsed()), Duration::from_secs(5));
    }

    #[test]
    fn fake_subscription_event_triggers_existing_redetect_transition() {
        let event = map_subscription_event(Some(Facility::Source), Some(Operation::Changed), 9)
            .expect("source event");
        let outcome = transition(SubscriptionState::default(), event);
        assert_eq!(outcome.actions, vec![SubscriptionAction::QuerySources]);
    }
}
