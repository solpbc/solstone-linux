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
use solstone_linux::{
    chunking::{AudioLeg, LegBlock, SAMPLE_RATE},
    sources::{SourceDescriptor, SourceSelection, classify_sources},
    subscription::{
        MuteStatus, SubscriptionAction, SubscriptionEvent, SubscriptionOperation,
        SubscriptionState, transition,
    },
};
use tracing::{info, warn};

type AnyError = Box<dyn Error + Send + Sync>;
const PULSE_TIMEOUT: Duration = Duration::from_secs(5);

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
}

fn drive_subscription(
    mainloop: &mut Mainloop,
    context: &Context,
    device_override: &Option<String>,
    mut state: SubscriptionState,
    event: SubscriptionEvent,
) -> Result<SubscriptionOutcome, AnyError> {
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
                            device_override.as_deref(),
                        ),
                        Err(error) => Err(
                            solstone_linux::sources::SourceSelectionError::EnumerationFailed(
                                error.to_string(),
                            ),
                        ),
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
    device_override: Option<String>,
) -> Result<(), AnyError> {
    let mut mainloop = Mainloop::new().ok_or("failed to create PulseAudio threaded mainloop")?;
    let mut context = Context::new(&mainloop, "solstone audio spike")
        .ok_or("failed to create PulseAudio context")?;
    context.connect(None, ContextFlagSet::NOFLAGS, None)?;
    mainloop.start()?;
    wait_for_context(&mut mainloop, &context)?;

    let outcome = drive_subscription(
        &mut mainloop,
        &context,
        &device_override,
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
    while !stopped.load(Ordering::SeqCst) {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((facility, operation, index)) => {
                let Some(event) = map_subscription_event(facility, operation, index) else {
                    continue;
                };
                let outcome =
                    drive_subscription(&mut mainloop, &context, &device_override, state, event)?;
                state = outcome.state;
                for status in outcome.mute_changes {
                    let (muted, mute_failure) = mute_result(&status);
                    sender.send(PulseMessage::MuteChanged {
                        muted,
                        mute_failure,
                    })?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
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
    let mut sources = Vec::new();
    loop {
        match rx.recv_timeout(PULSE_TIMEOUT)? {
            Ok(Some(source)) => sources.push(source),
            Ok(None) => break,
            Err(reason) => return Err(reason.into()),
        }
    }
    drop(operation);
    Ok(sources)
}

fn query_default_sink(
    mainloop: &mut Mainloop,
    context: &Context,
) -> Result<solstone_linux::subscription::DefaultSink, String> {
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
        Ok(Some(index)) => Ok(solstone_linux::subscription::DefaultSink { index, name }),
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
        .set_read_callback(Some(Box::new(move |_| read_stream(&weak, leg, &sender))));
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

fn read_stream(weak: &Weak<Mutex<Stream>>, leg: AudioLeg, sender: &mpsc::Sender<PulseMessage>) {
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
            return;
        }
    };
    if let Err(error) = stream.discard() {
        let _ = sender.send(PulseMessage::Failed(format!(
            "record discard failed: {error}"
        )));
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
