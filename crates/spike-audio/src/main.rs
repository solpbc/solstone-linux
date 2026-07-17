// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use clap::Parser;
use flac_bound::FlacEncoder;
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
    chunking::{AudioLeg, DrainedChunk, HitGate, LegBlock, SAMPLE_RATE, StereoAccumulator, to_i16},
    encoding::{AudioFileSource, AudioOutputPlan, audio_output_plan},
    sources::{SourceDescriptor, SourceSelection, classify_sources},
};
use tracing::{info, warn};

type AnyError = Box<dyn Error + Send + Sync>;
const CHUNK_DURATION: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(about = "Exercise PulseAudio microphone and system-audio capture with FLAC output")]
struct Cli {
    /// Directory where FLAC output files are written
    #[arg(long)]
    output_dir: PathBuf,

    /// Stop cleanly after this many seconds; otherwise run until SIGINT
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    duration: Option<u64>,

    /// Pin output shape: true writes dual mono, false writes stereo; omitted follows live mute state
    #[arg(long, action = clap::ArgAction::Set)]
    muted: Option<bool>,

    /// Exact PulseAudio source name to use for the microphone leg
    #[arg(long)]
    device: Option<String>,
}

enum PulseMessage {
    Ready {
        selection: SourceSelection,
        muted: bool,
        mute_failure: Option<String>,
    },
    Block(LegBlock),
    Failed(String),
}

fn main() -> Result<(), AnyError> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.output_dir)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let signal_stopped = Arc::clone(&stopped);
    ctrlc::set_handler(move || signal_stopped.store(true, Ordering::SeqCst))?;

    let (message_tx, message_rx) = mpsc::channel();
    let pulse_stopped = Arc::clone(&stopped);
    let device = cli.device.clone();
    let pulse_thread = thread::spawn(move || run_pulse(message_tx, pulse_stopped, device));

    let (selection, queried_muted) = loop {
        match message_rx.recv()? {
            PulseMessage::Ready {
                selection,
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    warn!(%reason, "unmuted (query failed)");
                }
                break (selection, muted);
            }
            PulseMessage::Failed(reason) => return Err(reason.into()),
            PulseMessage::Block(_) => {}
        }
    };
    report_selection(&selection);
    let mut output_muted = cli.muted.unwrap_or(queried_muted);
    info!(
        muted = output_muted,
        pinned = cli.muted.is_some(),
        "output shape selected"
    );

    let deadline = cli
        .duration
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let mut tick = Instant::now() + CHUNK_DURATION;
    let mut accumulator = StereoAccumulator::default();
    let mut gate = HitGate::default();
    let mut segment = Vec::new();
    while !stopped.load(Ordering::SeqCst) && deadline.is_none_or(|end| Instant::now() < end) {
        let next = deadline.map_or(tick, |end| tick.min(end));
        let timeout = next.saturating_duration_since(Instant::now());
        match message_rx.recv_timeout(timeout) {
            Ok(PulseMessage::Block(block)) => accumulator.push(block),
            Ok(PulseMessage::Failed(reason)) => return Err(reason.into()),
            Ok(PulseMessage::Ready {
                muted,
                mute_failure,
                ..
            }) => {
                info!(muted, "live mute event observed");
                if let Some(reason) = mute_failure {
                    warn!(%reason, "unmuted (query failed)");
                }
                if cli.muted.is_none() {
                    output_muted = muted;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() >= tick {
            consume_chunk(accumulator.drain(), &mut gate, &mut segment);
            tick += CHUNK_DURATION;
        }
    }
    stopped.store(true, Ordering::SeqCst);
    while let Ok(PulseMessage::Block(block)) = message_rx.try_recv() {
        accumulator.push(block);
    }
    consume_chunk(accumulator.drain(), &mut gate, &mut segment);
    pulse_thread
        .join()
        .map_err(|_| "PulseAudio control thread panicked")??;

    if gate.should_save() {
        write_output(&cli.output_dir, &audio_output_plan(output_muted), &segment)?;
        info!(hits = gate.hits(), "saved audio output");
    } else {
        info!(
            hits = gate.hits(),
            "skipping audio output below hit threshold"
        );
    }
    Ok(())
}

fn consume_chunk(chunk: DrainedChunk, gate: &mut HitGate, segment: &mut Vec<f32>) {
    if chunk.report.is_imbalanced() {
        warn!(?chunk.report, "audio leg imbalance");
    }
    let rms = chunk.rms();
    let hit = gate.observe(&chunk);
    info!(
        rms,
        hit,
        hits = gate.hits(),
        frames = chunk.frames(),
        "drained audio chunk"
    );
    segment.extend_from_slice(chunk.interleaved());
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

fn run_pulse(
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

    let (default_sink_name, default_sink_index, muted, mute_failure) =
        query_default_sink(&mut mainloop, &context);
    let sources = query_sources(&mut mainloop, &context)?;
    let selection = classify_sources(
        &sources,
        default_sink_name.as_deref(),
        device_override.as_deref(),
    )
    .map_err(|error| format!("audio degraded: {error}"))?;
    sender.send(PulseMessage::Ready {
        selection: selection.clone(),
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
    info!(?default_sink_index, "PulseAudio capture ready");

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
    while !stopped.load(Ordering::SeqCst) {
        match event_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((
                Some(Facility::Source),
                Some(Operation::New | Operation::Changed | Operation::Removed),
                _,
            )) => {
                let refreshed = query_sources(&mut mainloop, &context)?;
                let selection = classify_sources(
                    &refreshed,
                    default_sink_name.as_deref(),
                    device_override.as_deref(),
                )
                .map_err(|error| format!("audio degraded after source change: {error}"))?;
                sender.send(PulseMessage::Ready {
                    selection,
                    muted,
                    mute_failure: None,
                })?;
            }
            Ok((Some(Facility::Sink), Some(Operation::Changed), index))
                if Some(index) == default_sink_index =>
            {
                let (_, _, live_muted, failure) = query_default_sink(&mut mainloop, &context);
                sender.send(PulseMessage::Ready {
                    selection: selection.clone(),
                    muted: live_muted,
                    mute_failure: failure,
                })?;
            }
            Ok((Some(Facility::Server), Some(Operation::Changed), _)) => {
                let (_, _, live_muted, failure) = query_default_sink(&mut mainloop, &context);
                sender.send(PulseMessage::Ready {
                    selection: selection.clone(),
                    muted: live_muted,
                    mute_failure: failure,
                })?;
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
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
    loop {
        mainloop.lock();
        let state = context.get_state();
        mainloop.unlock();
        match state {
            ContextState::Ready => return Ok(()),
            ContextState::Failed | ContextState::Terminated => {
                return Err(format!("PulseAudio context failed: {state:?}").into());
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
            ListResult::Item(source) => Some(SourceDescriptor {
                index: source.index,
                name: source.name.as_deref().map(Into::into),
                monitor_of_sink: source.monitor_of_sink,
                monitor_of_sink_name: source.monitor_of_sink_name.as_deref().map(Into::into),
            }),
            ListResult::End | ListResult::Error => None,
        };
        let _ = tx.send((
            mapped,
            matches!(result, ListResult::End | ListResult::Error),
        ));
    });
    mainloop.unlock();
    let mut sources = Vec::new();
    loop {
        let (source, done) = rx.recv_timeout(Duration::from_secs(5))?;
        if let Some(source) = source {
            sources.push(source);
        }
        if done {
            break;
        }
    }
    drop(operation);
    Ok(sources)
}

fn query_default_sink(
    mainloop: &mut Mainloop,
    context: &Context,
) -> (Option<String>, Option<u32>, bool, Option<String>) {
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context.introspect().get_server_info(move |server| {
        let _ = tx.send(server.default_sink_name.as_deref().map(str::to_owned));
    });
    mainloop.unlock();
    let name = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Some(name)) => name,
        Ok(None) => return (None, None, false, Some("server has no default sink".into())),
        Err(error) => {
            return (
                None,
                None,
                false,
                Some(format!("default sink query failed: {error}")),
            );
        }
    };
    drop(operation);
    let (tx, rx) = mpsc::channel();
    mainloop.lock();
    let operation = context
        .introspect()
        .get_sink_info_by_name(&name, move |result| {
            let value = match result {
                ListResult::Item(sink) => Some((sink.index, sink.mute)),
                ListResult::End | ListResult::Error => None,
            };
            let _ = tx.send(value);
        });
    mainloop.unlock();
    let result = rx.recv_timeout(Duration::from_secs(5));
    drop(operation);
    match result {
        Ok(Some((index, muted))) => (Some(name), Some(index), muted, None),
        Ok(None) => (
            Some(name),
            None,
            false,
            Some("default sink lookup failed".into()),
        ),
        Err(error) => (
            Some(name),
            None,
            false,
            Some(format!("mute query failed: {error}")),
        ),
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
    stream.lock().expect("stream lock").connect_record(
        Some(source_name),
        None,
        StreamFlagSet::ADJUST_LATENCY,
    )?;
    mainloop.unlock();
    loop {
        mainloop.lock();
        let state = stream.lock().expect("stream lock").get_state();
        mainloop.unlock();
        match state {
            StreamState::Ready => break,
            StreamState::Failed | StreamState::Terminated => {
                return Err(format!("record stream failed: {state:?}").into());
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
        let samples = bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte float")))
            .collect();
        let _ = sender.send(PulseMessage::Block(LegBlock::new(leg, samples)));
    }
}

fn write_output(directory: &Path, plan: &AudioOutputPlan, stereo: &[f32]) -> Result<(), AnyError> {
    for file in &plan.files {
        let samples: Vec<i32> = match file.source {
            AudioFileSource::StereoInterleaved => stereo
                .iter()
                .map(|&sample| i32::from(to_i16(sample)))
                .collect(),
            AudioFileSource::MicrophoneMono => stereo
                .chunks_exact(2)
                .map(|frame| i32::from(to_i16(frame[0])))
                .collect(),
            AudioFileSource::SystemMono => stereo
                .chunks_exact(2)
                .map(|frame| i32::from(to_i16(frame[1])))
                .collect(),
        };
        let mut encoder = FlacEncoder::new()
            .ok_or("failed to allocate FLAC encoder")?
            .sample_rate(plan.sample_rate)
            .bits_per_sample(plan.bits_per_sample)
            .channels(file.channels)
            .init_file(&directory.join(file.filename))
            .map_err(|error| format!("failed to initialize FLAC encoder: {error:?}"))?;
        let frames = u32::try_from(samples.len() / file.channels as usize)?;
        encoder
            .process_interleaved(&samples, frames)
            .map_err(|()| "FLAC encoding failed")?;
        encoder.finish().map_err(|_| "FLAC finalization failed")?;
    }
    Ok(())
}
