// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use clap::Parser;
use flac_bound::FlacEncoder;
use solstone_linux::{
    chunking::{DrainedChunk, HitGate, StereoAccumulator, to_i16},
    encoding::{AudioFileSource, AudioOutputPlan, audio_output_plan},
};
use tracing::{info, warn};

mod pulse;

use pulse::{PulseMessage, run_pulse};

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
    let error_tx = message_tx.clone();
    let pulse_thread = thread::spawn(move || {
        let result = run_pulse(message_tx, pulse_stopped, device);
        if let Err(error) = &result {
            let _ = error_tx.send(PulseMessage::Failed(error.to_string()));
        }
        result
    });

    let queried_muted = loop {
        match message_rx
            .recv()
            .map_err(|_| "PulseAudio thread exited before initialization")?
        {
            PulseMessage::Ready {
                muted,
                mute_failure,
            } => {
                if let Some(reason) = mute_failure {
                    warn!(%reason, "unmuted (query failed)");
                }
                break muted;
            }
            PulseMessage::Failed(reason) => return Err(reason.into()),
            PulseMessage::Block(_) => {}
            PulseMessage::MuteChanged { .. } => {}
        }
    };
    let mut output_muted = cli.muted.unwrap_or(queried_muted);
    let mut mute_changed_during_capture = false;
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
            Ok(PulseMessage::MuteChanged {
                muted,
                mute_failure,
            }) => {
                warn!(muted, "mute changed mid-capture");
                mute_changed_during_capture = true;
                if let Some(reason) = mute_failure {
                    warn!(%reason, "unmuted (query failed)");
                }
                if cli.muted.is_none() {
                    output_muted = muted;
                }
            }
            Ok(PulseMessage::Ready { .. }) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() >= tick {
            consume_chunk(accumulator.drain(), &mut gate, &mut segment);
            tick += CHUNK_DURATION;
        }
    }
    stopped.store(true, Ordering::SeqCst);
    pulse_thread
        .join()
        .map_err(|_| "PulseAudio control thread panicked")??;
    while let Ok(PulseMessage::Block(block)) = message_rx.try_recv() {
        accumulator.push(block);
    }
    consume_chunk(accumulator.drain(), &mut gate, &mut segment);

    if gate.should_save() {
        if mute_changed_during_capture {
            warn!(
                "mute changed mid-capture; a production observer would rotate the segment here — this spike writes one output using the final state"
            );
        }
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
