// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    error::Error,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use ashpd::desktop::{
    PersistMode,
    screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
        StartCastOptions,
    },
};
use clap::Parser;
use gst::prelude::*;
use gstreamer as gst;
use solstone_linux::{
    matching::{MatchedStream, PortalStream, match_streams_to_monitors},
    pipeline::{PipelineDescription, PropertyValue, pipeline_description, render_gst_launch},
    restore_token::{load_restore_token, save_restore_token},
    rotation::{RotationEvent, RotationState, transition},
    streams::{is_healthy_file_size, stream_filename},
};
use tokio::time::{Instant, timeout};
use tracing::{error, info, warn};

const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const PORTAL_INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(600);
const EOS_TIMEOUT: gst::ClockTime = gst::ClockTime::from_seconds(5);

type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(about = "Exercise xdg-desktop-portal ScreenCast through PipeWire and GStreamer")]
struct Cli {
    /// Directory where rotated segment directories and .webm files land
    #[arg(long)]
    output_dir: PathBuf,

    /// Capture framerate, clamped to 1..=10
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u64))]
    framerate: u64,

    /// Include the cursor in captured frames
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    draw_cursor: bool,

    /// Restore-token file used by this spike
    #[arg(long, default_value = ".spike-screencast-restore-token")]
    token_path: PathBuf,

    /// Rotation interval in seconds
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..))]
    rotation_interval: u64,

    /// Stop cleanly after this many seconds; otherwise run until SIGINT
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    duration: Option<u64>,
}

struct StreamPipeline {
    stream: MatchedStream,
    output: PathBuf,
    pipeline: gst::Pipeline,
    rotation_state: RotationState,
}

enum LoopEvent {
    Rotate(&'static str),
    Stop,
    InspectPipelines,
}

impl StreamPipeline {
    fn identity(&self) -> String {
        format!("{} ({})", self.stream.connector, self.stream.node_id)
    }
}

fn clamp_framerate(framerate: u64) -> u8 {
    framerate.clamp(1, 10) as u8
}

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.output_dir)?;
    info!(output_dir = %cli.output_dir.display(), framerate = clamp_framerate(cli.framerate), draw_cursor = cli.draw_cursor, "starting screencast spike");
    gst::init()?;
    run(cli).await
}

async fn run(cli: Cli) -> Result<(), AnyError> {
    let framerate = clamp_framerate(cli.framerate);
    info!("portal phase: connecting to ScreenCast portal");
    let proxy = timeout(PORTAL_CALL_TIMEOUT, Screencast::new())
        .await
        .map_err(|_| "ScreenCast proxy creation timed out")??;
    info!(portal_version = proxy.version(), "portal phase: connected");

    info!("portal phase: creating session");
    let session = timeout(
        PORTAL_CALL_TIMEOUT,
        proxy.create_session(Default::default()),
    )
    .await
    .map_err(|_| "CreateSession timed out")??;

    let restore_token = load_restore_token(&cli.token_path);
    if restore_token.is_some() {
        info!(token_path = %cli.token_path.display(), "portal phase: using restore token");
    } else {
        info!(token_path = %cli.token_path.display(), "portal phase: no usable restore token; an interactive prompt is normal");
    }
    let response_timeout = if restore_token.is_some() {
        PORTAL_CALL_TIMEOUT
    } else {
        PORTAL_INTERACTIVE_TIMEOUT
    };
    let cursor_mode = if cli.draw_cursor {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    let options = SelectSourcesOptions::default()
        .set_sources(Some(SourceType::Monitor.into()))
        .set_multiple(true)
        .set_cursor_mode(cursor_mode)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore_token.as_deref());
    info!("portal phase: selecting all monitor sources");
    let select_request = timeout(response_timeout, proxy.select_sources(&session, options))
        .await
        .map_err(|_| "SelectSources response timed out")??;
    select_request.response()?;

    info!("portal phase: starting selected sources");
    let start_request = timeout(
        response_timeout,
        proxy.start(&session, None, StartCastOptions::default()),
    )
    .await
    .map_err(|_| "Start response timed out")??;
    let start_result = start_request.response()?;

    // Portal restore tokens are single-use and rotate. Re-persist every Start result.
    if let Some(token) = start_result.restore_token()
        && let Err(error) = save_restore_token(&cli.token_path, token)
    {
        warn!(token_path = %cli.token_path.display(), %error, "could not save rotated restore token");
    }

    let portal_streams: Vec<_> = start_result
        .streams()
        .iter()
        .enumerate()
        .map(|(index, stream)| {
            info!(index, node_id = stream.pipe_wire_node_id(), position = ?stream.position(), size = ?stream.size(), "portal phase: stream received");
            PortalStream {
                index,
                node_id: stream.pipe_wire_node_id(),
                position: stream.position(),
                size: stream.size(),
            }
        })
        .collect();
    // Live connector inventory comes later from the observer's monitor discovery.
    let matched = match_streams_to_monitors(&portal_streams, &[]);

    info!("portal phase: opening PipeWire remote");
    let pipewire_fd: OwnedFd = timeout(
        PORTAL_CALL_TIMEOUT,
        proxy.open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default()),
    )
    .await
    .map_err(|_| "OpenPipeWireRemote timed out")??;
    info!(
        fd = pipewire_fd.as_raw_fd(),
        "portal phase: PipeWire remote open"
    );

    let mut generation = 0_u64;
    let mut pipelines = start_generation(
        &matched,
        pipewire_fd.as_raw_fd(),
        framerate,
        &cli.output_dir,
        generation,
    );
    if pipelines.is_empty() {
        return Err("no stream pipelines could be started".into());
    }

    let mut rotation_tick = tokio::time::interval(Duration::from_secs(cli.rotation_interval));
    rotation_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    rotation_tick.tick().await;
    let mut pipeline_tick = tokio::time::interval(Duration::from_secs(1));
    pipeline_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    pipeline_tick.tick().await;
    let deadline = cli
        .duration
        .map(|seconds| Instant::now() + Duration::from_secs(seconds));
    let mut sigusr1 =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;

    loop {
        let event = tokio::select! {
            _ = rotation_tick.tick() => LoopEvent::Rotate("rotation interval"),
            _ = pipeline_tick.tick() => LoopEvent::InspectPipelines,
            _ = tokio::signal::ctrl_c() => LoopEvent::Stop,
            _ = async {
                if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                else { std::future::pending::<()>().await; }
            } => LoopEvent::Stop,
            _ = sigusr1.recv() => LoopEvent::Rotate("SIGUSR1"),
        };

        if matches!(event, LoopEvent::InspectPipelines) {
            inspect_bus_errors(&mut pipelines);
            if pipelines.is_empty() {
                error!("all stream pipelines failed; stopping spike");
                break;
            }
            continue;
        }
        let restart = matches!(event, LoopEvent::Rotate(_));
        if let LoopEvent::Rotate(reason) = event {
            info!(reason, generation, "rotation requested");
        } else {
            info!("clean stop requested");
        }
        stop_generation(&mut pipelines, restart);
        if !restart {
            break;
        }
        generation += 1;
        pipelines = start_generation(
            &matched,
            pipewire_fd.as_raw_fd(),
            framerate,
            &cli.output_dir,
            generation,
        );
        if pipelines.is_empty() {
            error!(generation, "all stream pipelines failed; stopping spike");
            break;
        }
    }

    drop(pipewire_fd);
    info!("screencast spike stopped; PipeWire remote closed");
    Ok(())
}

fn generation_dir(output_dir: &Path, generation: u64) -> PathBuf {
    output_dir.join(format!("rotation-{generation:04}"))
}

fn start_generation(
    streams: &[MatchedStream],
    pipewire_fd: i32,
    framerate: u8,
    output_dir: &Path,
    generation: u64,
) -> Vec<StreamPipeline> {
    let directory = generation_dir(output_dir, generation);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        error!(path = %directory.display(), %error, "could not create generation directory");
        return Vec::new();
    }
    streams
        .iter()
        .filter_map(|stream| {
            let output = directory.join(stream_filename(&stream.position_label, &stream.connector));
            let description = pipeline_description(pipewire_fd, stream.node_id, framerate, &output);
            info!(stream = %stream.connector, node_id = stream.node_id, pipeline = %render_gst_launch(&description), "pipeline phase: constructing stream");
            match build_pipeline(&description) {
                Ok(pipeline) => match pipeline.set_state(gst::State::Playing) {
                    Ok(change) => {
                        info!(stream = %stream.connector, node_id = stream.node_id, state_change = ?change, "pipeline state: Playing requested");
                        Some(StreamPipeline { stream: stream.clone(), output, pipeline, rotation_state: RotationState::Running })
                    }
                    Err(error) => {
                        error!(stream = %stream.connector, node_id = stream.node_id, %error, "pipeline failed to enter Playing; other streams continue");
                        let _ = pipeline.set_state(gst::State::Null);
                        None
                    }
                },
                Err(error) => {
                    error!(stream = %stream.connector, node_id = stream.node_id, %error, "pipeline construction failed; missing plugin/factory is reported by name; other streams continue");
                    None
                }
            }
        })
        .collect()
}

fn build_pipeline(description: &PipelineDescription) -> Result<gst::Pipeline, AnyError> {
    let pipeline = gst::Pipeline::new();
    let mut elements = Vec::with_capacity(description.elements.len());
    for spec in &description.elements {
        let element = gst::ElementFactory::make(&spec.factory)
            .build()
            .map_err(|error| {
                format!(
                    "missing or unloadable GStreamer element '{}': {error}",
                    spec.factory
                )
            })?;
        for property in &spec.properties {
            match &property.value {
                PropertyValue::String(value) => {
                    element.set_property_from_str(&property.name, value)
                }
                PropertyValue::I32(value) => element.set_property(&property.name, value),
                PropertyValue::U32(value) => element.set_property(&property.name, value),
            }
        }
        if let Some(caps) = &spec.caps {
            element.set_property("caps", gst::Caps::from_str(caps)?);
        }
        pipeline.add(&element)?;
        elements.push(element);
    }
    for pair in elements.windows(2) {
        pair[0].link(&pair[1])?;
    }
    Ok(pipeline)
}

fn inspect_bus_errors(pipelines: &mut Vec<StreamPipeline>) {
    pipelines.retain_mut(|record| {
        let Some(bus) = record.pipeline.bus() else { return true; };
        let Some(message) = bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Error]) else { return true; };
        if let gst::MessageView::Error(bus_error) = message.view() {
            let source = bus_error.src().map(|source| source.name()).unwrap_or_else(|| "unknown".into());
            let detail = format!("source={source} error={} debug={:?}", bus_error.error(), bus_error.debug());
            let outcome = transition(record.rotation_state.clone(), RotationEvent::Error(detail.clone()));
            record.rotation_state = outcome.state;
            error!(stream = %record.identity(), %detail, "pipeline bus error; other streams continue");
            let _ = record.pipeline.set_state(gst::State::Null);
            report_and_remove_silent(record);
            return false;
        }
        true
    });
}

fn stop_generation(pipelines: &mut Vec<StreamPipeline>, restart: bool) {
    for record in pipelines.iter_mut() {
        let event = if restart {
            RotationEvent::RotateRequested
        } else {
            RotationEvent::ForceStop
        };
        let outcome = transition(record.rotation_state.clone(), event);
        record.rotation_state = outcome.state;
        info!(stream = %record.identity(), "pipeline state: sending EOS");
        if !record.pipeline.send_event(gst::event::Eos::new()) {
            warn!(stream = %record.identity(), "pipeline rejected EOS event");
        }

        let bus_message = record.pipeline.bus().and_then(|bus| {
            bus.timed_pop_filtered(
                EOS_TIMEOUT,
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
        });
        match bus_message.as_ref().map(|message| message.view()) {
            Some(gst::MessageView::Eos(_)) => {
                record.rotation_state =
                    transition(record.rotation_state.clone(), RotationEvent::EosReceived).state;
                info!(stream = %record.identity(), "pipeline state: EOS received; file cleanly finalized");
            }
            Some(gst::MessageView::Error(bus_error)) => {
                let detail = format!("{} debug={:?}", bus_error.error(), bus_error.debug());
                record.rotation_state = transition(
                    record.rotation_state.clone(),
                    RotationEvent::Error(detail.clone()),
                )
                .state;
                error!(stream = %record.identity(), %detail, "pipeline errored while awaiting EOS; file not cleanly finalized");
            }
            _ => {
                let last_state = format!("{:?}", record.pipeline.current_state());
                let outcome = transition(
                    record.rotation_state.clone(),
                    RotationEvent::TimeoutElapsed {
                        stream_identity: record.identity(),
                        last_pipeline_state: last_state.clone(),
                    },
                );
                record.rotation_state = outcome.state;
                error!(stream = %record.identity(), %last_state, eos_timeout_seconds = 5, "EOS TIMEOUT: stream not cleanly finalized; force-stopping pipeline");
            }
        }
        let _ = record.pipeline.set_state(gst::State::Null);
        info!(stream = %record.identity(), state = ?record.pipeline.current_state(), "pipeline state: stopped");
        report_and_remove_silent(record);
    }
    pipelines.clear();
}

fn report_and_remove_silent(record: &StreamPipeline) {
    let bytes = std::fs::metadata(&record.output)
        .ok()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    info!(stream = %record.identity(), bytes, path = %record.output.display(), "bytes written");
    if !is_healthy_file_size(Some(bytes)) {
        warn!(connector = %record.stream.connector, position = %record.stream.position_label, bytes, path = %record.output.display(), "silent stream; unlinking file");
        if let Err(error) = std::fs::remove_file(&record.output)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %record.output.display(), %error, "could not unlink silent stream file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_framerate;

    #[test]
    fn framerate_is_clamped_to_config_range() {
        assert_eq!(clamp_framerate(0), 1);
        assert_eq!(clamp_framerate(1), 1);
        assert_eq!(clamp_framerate(10), 10);
        assert_eq!(clamp_framerate(11), 10);
        assert_eq!(clamp_framerate(u64::MAX), 10);
    }
}
