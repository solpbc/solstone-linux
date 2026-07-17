// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use tracing::warn;
use x11rb::{connection::Connection, protocol::randr::ConnectionExt as _};

use crate::{
    observer::{StoppedStream, VideoCapture, VideoStream},
    pipeline::ximagesrc_pipeline_description,
    positions::{BoxGeometry, Monitor, assign_monitor_positions},
    streams::{SILENT_STREAM_LOG_MESSAGE, is_healthy_file_size, stream_filename},
    video::{
        clamp_framerate,
        gstreamer::{
            CapturePipeline, GstreamerPipelineFactory, PipelineFactory, StoppingPipeline,
            stop_pipelines,
        },
    },
};

const EOS_TIMEOUT: Duration = Duration::from_secs(5);

pub trait X11OutputProvider: Send {
    fn outputs(&mut self) -> Result<Vec<Monitor>, String>;
}

#[derive(Default)]
pub struct RandrOutputProvider;

impl X11OutputProvider for RandrOutputProvider {
    fn outputs(&mut self) -> Result<Vec<Monitor>, String> {
        let (connection, screen_index) = x11rb::connect(None).map_err(|error| error.to_string())?;
        let root = connection.setup().roots[screen_index].root;
        // Use RandR output names, not GetMonitors' monitor-object names: this
        // matches the shipping Python observer's xrandr connector identities.
        let resources = connection
            .randr_get_screen_resources_current(root)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?;
        let mut monitors = Vec::new();
        for output in resources.outputs {
            let info = connection
                .randr_get_output_info(output, resources.config_timestamp)
                .map_err(|error| error.to_string())?
                .reply()
                .map_err(|error| error.to_string())?;
            if info.crtc == x11rb::NONE {
                continue;
            }
            let crtc = connection
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .map_err(|error| error.to_string())?
                .reply()
                .map_err(|error| error.to_string())?;
            if crtc.width == 0 || crtc.height == 0 {
                continue;
            }
            let x = i32::from(crtc.x);
            let y = i32::from(crtc.y);
            let name = String::from_utf8_lossy(&info.name).into_owned();
            monitors.push(Monitor {
                id: name,
                bounds: BoxGeometry {
                    x1: x,
                    y1: y,
                    x2: x + i32::from(crtc.width),
                    y2: y + i32::from(crtc.height),
                },
                position: None,
            });
        }
        Ok(monitors)
    }
}

struct TrackedPipeline {
    node_id: u32,
    connector: String,
    position: String,
    output: PathBuf,
    pipeline: Box<dyn CapturePipeline>,
}

pub struct X11VideoCapture<P = RandrOutputProvider, F = GstreamerPipelineFactory> {
    display: String,
    outputs: P,
    pipelines: F,
    tracked: Vec<TrackedPipeline>,
    started: bool,
}

fn display_name(value: Option<String>) -> String {
    value.unwrap_or_else(|| ":0".into())
}

impl X11VideoCapture {
    pub fn new() -> Result<Self, String> {
        Ok(Self::with_dependencies(
            display_name(std::env::var("DISPLAY").ok()),
            RandrOutputProvider,
            GstreamerPipelineFactory::new()?,
        ))
    }
}

impl<P, F> X11VideoCapture<P, F> {
    pub fn with_dependencies(display: impl Into<String>, outputs: P, pipelines: F) -> Self {
        Self {
            display: display.into(),
            outputs,
            pipelines,
            tracked: Vec::new(),
            started: false,
        }
    }
}

impl<P: X11OutputProvider, F: PipelineFactory> VideoCapture for X11VideoCapture<P, F> {
    fn start(
        &mut self,
        directory: &Path,
        framerate: i64,
        draw_cursor: bool,
    ) -> Result<Vec<VideoStream>, String> {
        if self.started || !self.tracked.is_empty() {
            return Err(
                "X11 video capture is already active; stop it before starting again".into(),
            );
        }
        let monitors: Vec<_> = self.outputs.outputs()?.into_iter().filter(|monitor| {
            let usable = monitor.bounds.x1 >= 0 && monitor.bounds.y1 >= 0;
            if !usable {
                warn!(connector = %monitor.id, x = monitor.bounds.x1, y = monitor.bounds.y1, "skipping X11 monitor with negative offset; ximagesrc requires non-negative coordinates");
            }
            usable
        }).collect();
        let monitors = assign_monitor_positions(&monitors);
        if monitors.is_empty() {
            return Err("No usable monitors found for X11 capture".into());
        }
        let framerate = clamp_framerate(framerate);
        let mut streams = Vec::new();
        for (index, monitor) in monitors.into_iter().enumerate() {
            let position = monitor.position.unwrap_or_else(|| "center".into());
            let output = directory.join(stream_filename(&position, &monitor.id));
            let description = ximagesrc_pipeline_description(
                &self.display,
                &monitor.bounds,
                framerate,
                draw_cursor,
                &output,
            );
            let mut pipeline = match self.pipelines.build(&description) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    warn!(connector = %monitor.id, %error, "X11 pipeline construction failed; other streams continue");
                    continue;
                }
            };
            if let Err(error) = pipeline.start() {
                warn!(connector = %monitor.id, %error, "X11 pipeline failed to enter Playing; other streams continue");
                pipeline.force_stop();
                continue;
            }
            streams.push(VideoStream {
                connector: monitor.id.clone(),
                position: position.clone(),
                file_path: output.to_string_lossy().into_owned(),
            });
            self.tracked.push(TrackedPipeline {
                node_id: index as u32,
                connector: monitor.id,
                position,
                output,
                pipeline,
            });
        }
        if streams.is_empty() {
            return Err("No X11 monitor pipelines could be started".into());
        }
        self.started = true;
        Ok(streams)
    }

    fn stop(&mut self) -> Result<Vec<StoppedStream>, String> {
        let mut pipelines: Vec<StoppingPipeline<'_>> = self
            .tracked
            .iter_mut()
            .map(|record| StoppingPipeline {
                identity: format!("{} ({})", record.connector, record.node_id),
                pipeline: &mut record.pipeline,
            })
            .collect();
        stop_pipelines(&mut pipelines, EOS_TIMEOUT);
        drop(pipelines);

        self.started = false;
        let mut stopped = Vec::with_capacity(self.tracked.len());
        for record in self.tracked.drain(..) {
            let file_bytes = match fs::metadata(&record.output) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => {
                    warn!(path = %record.output.display(), %error, "could not stat stream file");
                    0
                }
            };
            if !is_healthy_file_size(Some(file_bytes)) {
                warn!(connector = %record.connector, position = %record.position, file_bytes, path = %record.output.display(), "{SILENT_STREAM_LOG_MESSAGE}");
                if let Err(error) = fs::remove_file(&record.output)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    warn!(path = %record.output.display(), %error, "could not unlink silent stream file");
                }
            }
            stopped.push(StoppedStream {
                node_id: record.node_id,
                connector: record.connector,
                position: record.position,
                file_bytes,
            });
        }
        Ok(stopped)
    }

    fn is_healthy(&self) -> bool {
        // Python uses `_started && process exists && process.poll() is None`.
        // Rust has one independent pipeline per output, so the equivalent shared
        // capture health is started, nonempty, and every tracked pipeline alive.
        self.started
            && !self.tracked.is_empty()
            && self.tracked.iter().all(|item| item.pipeline.is_healthy())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::pipeline::{PipelineDescription, PropertyValue};

    struct FakeOutputs(Result<Vec<Monitor>, String>);
    impl X11OutputProvider for FakeOutputs {
        fn outputs(&mut self) -> Result<Vec<Monitor>, String> {
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct FakeState {
        healthy: bool,
        started: bool,
        stopped: bool,
    }
    struct FakePipeline(Arc<Mutex<FakeState>>);
    impl CapturePipeline for FakePipeline {
        fn start(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().started = true;
            Ok(())
        }
        fn is_healthy(&self) -> bool {
            self.0.lock().unwrap().healthy
        }
        fn send_eos(&mut self) -> bool {
            true
        }
        fn poll_terminal(&mut self) -> Option<Result<(), String>> {
            Some(Ok(()))
        }
        fn state_label(&self) -> String {
            "Playing".into()
        }
        fn force_stop(&mut self) {
            self.0.lock().unwrap().stopped = true;
        }
    }
    #[derive(Default)]
    struct FakeFactory {
        descriptions: Vec<PipelineDescription>,
        states: Vec<Arc<Mutex<FakeState>>>,
        fail_at: Option<usize>,
    }
    impl PipelineFactory for FakeFactory {
        fn build(
            &mut self,
            description: &PipelineDescription,
        ) -> Result<Box<dyn CapturePipeline>, String> {
            let index = self.descriptions.len();
            self.descriptions.push(description.clone());
            if self.fail_at == Some(index) {
                return Err("missing factory".into());
            }
            let state = Arc::new(Mutex::new(FakeState {
                healthy: true,
                ..Default::default()
            }));
            self.states.push(state.clone());
            Ok(Box::new(FakePipeline(state)))
        }
    }
    fn monitor(id: &str, bounds: [i32; 4]) -> Monitor {
        Monitor {
            id: id.into(),
            bounds: BoxGeometry {
                x1: bounds[0],
                y1: bounds[1],
                x2: bounds[2],
                y2: bounds[3],
            },
            position: None,
        }
    }

    #[test]
    fn start_no_monitors_is_a_real_error() {
        // tests/test_screencast.py::TestX11Screencaster::test_start_no_monitors_raises
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(vec![])),
            FakeFactory::default(),
        );
        assert!(
            capture
                .start(Path::new("/tmp"), 1, true)
                .unwrap_err()
                .contains("No usable")
        );
    }

    #[test]
    fn missing_display_defaults_to_python_value() {
        // src/solstone_linux/screencast.py::X11Screencaster::start
        assert_eq!(display_name(None), ":0");
        assert_eq!(display_name(Some(":7".into())), ":7");
    }

    #[test]
    fn start_builds_one_real_branch_per_monitor() {
        // tests/test_screencast.py::TestX11Screencaster::test_start_builds_one_branch_per_monitor
        let monitors = vec![
            monitor("DP-1", [0, 0, 1920, 1080]),
            monitor("DP-2", [1920, 0, 3840, 1080]),
        ];
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(monitors)),
            FakeFactory::default(),
        );
        let streams = capture.start(Path::new("/tmp"), 1, false).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(capture.pipelines.descriptions.len(), 2);
        assert_eq!(
            capture.pipelines.descriptions[0].elements[0].properties[3].value,
            PropertyValue::I32(1919)
        );
        assert_eq!(
            capture.pipelines.descriptions[1].elements[0].properties[1].value,
            PropertyValue::I32(1920)
        );
        assert_eq!(
            capture.pipelines.descriptions[1].elements[0].properties[3].value,
            PropertyValue::I32(3839)
        );
    }

    #[test]
    fn all_filtered_monitors_are_a_start_error() {
        let monitors = vec![monitor("DP-1", [-1920, 0, 0, 1080])];
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(monitors)),
            FakeFactory::default(),
        );
        assert!(capture.start(Path::new("/tmp"), 1, true).is_err());
    }

    #[test]
    fn partial_pipeline_failure_keeps_sibling_and_health_matches_python() {
        let monitors = vec![
            monitor("DP-1", [0, 0, 1920, 1080]),
            monitor("DP-2", [1920, 0, 4480, 1440]),
        ];
        let factory = FakeFactory {
            fail_at: Some(0),
            ..Default::default()
        };
        let mut capture =
            X11VideoCapture::with_dependencies(":0", FakeOutputs(Ok(monitors)), factory);
        assert!(!capture.is_healthy());
        assert_eq!(capture.start(Path::new("/tmp"), -3, true).unwrap().len(), 1);
        assert!(capture.is_healthy());
        capture.pipelines.states[0].lock().unwrap().healthy = false;
        assert!(!capture.is_healthy());
        assert!(!capture.is_healthy());
    }

    #[test]
    fn start_rejects_an_already_active_capture() {
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(vec![monitor("DP-1", [0, 0, 1920, 1080])])),
            FakeFactory::default(),
        );
        capture.start(Path::new("/tmp"), 1, true).unwrap();
        assert!(
            capture
                .start(Path::new("/tmp"), 1, true)
                .unwrap_err()
                .contains("already active")
        );
        capture.stop().unwrap();
    }

    #[test]
    fn stop_reports_all_tracked_streams_and_unlinks_silent() {
        // tests/test_screencast_stop_filters_silent_streams.py::test_stop_partitions_healthy_and_silent
        // tests/test_screencast.py::TestX11Screencaster::test_stop_keeps_healthy_streams
        let monitors = vec![
            monitor("DP-1", [0, 0, 1920, 1080]),
            monitor("DP-2", [1920, 0, 3840, 1080]),
        ];
        let directory = tempfile::tempdir().unwrap();
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(monitors)),
            FakeFactory::default(),
        );
        let streams = capture.start(directory.path(), 1, true).unwrap();
        let healthy_size = (0_u64..)
            .find(|size| is_healthy_file_size(Some(*size)))
            .unwrap();
        fs::write(&streams[0].file_path, vec![0; healthy_size as usize]).unwrap();
        fs::write(&streams[1].file_path, b"silent").unwrap();
        let stopped = capture.stop().unwrap();
        assert_eq!(stopped.len(), 2);
        assert_eq!(stopped[0].file_bytes, healthy_size);
        assert_eq!(stopped[1].file_bytes, 6);
        assert!(Path::new(&streams[0].file_path).exists());
        assert!(!Path::new(&streams[1].file_path).exists());
    }

    #[test]
    fn stop_reports_missing_and_unlink_error_streams() {
        // tests/test_screencast_stop_filters_silent_streams.py::test_stop_treats_missing_file_as_silent
        // tests/test_screencast_stop_filters_silent_streams.py::test_stop_handles_unlink_oserror
        let directory = tempfile::tempdir().unwrap();
        let mut missing = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(vec![monitor("DP-1", [0, 0, 1920, 1080])])),
            FakeFactory::default(),
        );
        missing.start(directory.path(), 1, true).unwrap();
        assert_eq!(missing.stop().unwrap()[0].file_bytes, 0);
        let mut capture = X11VideoCapture::with_dependencies(
            ":0",
            FakeOutputs(Ok(vec![monitor("DP-1", [0, 0, 1920, 1080])])),
            FakeFactory::default(),
        );
        let streams = capture.start(directory.path(), 1, true).unwrap();
        fs::create_dir(&streams[0].file_path).unwrap();
        let stopped = capture.stop().unwrap();
        assert_eq!(stopped.len(), 1);
        assert!(Path::new(&streams[0].file_path).is_dir());
    }
}
