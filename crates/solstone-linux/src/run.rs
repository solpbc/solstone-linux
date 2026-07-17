// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    env,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ashpd::zbus::{
    Connection,
    fdo::{RequestNameFlags, RequestNameReply},
};
use serde_json::{Map, Value, json};

use crate::{
    audio::{backend::PulseAudioCapture, writer::FlacAudioWriter},
    config::Config,
    observer::{
        ActivityProbe, ActivityState, Backends, BackgroundCaptureStats, Clock, EventSink, Observer,
        ObserverError, SegmentCompletedEvent, StateSink, StateSnapshot, StoppedStream,
        StreamSilentEvent, VideoCapture, VideoStream, lifecycle,
    },
    recovery::{ClaxonMediaDurationProbe, recover_incomplete_segments},
    upload::UploadClient,
    video::{
        BackendKind,
        gstreamer::GstreamerPipelineFactory,
        portal::{AshpdPortalOps, FileTokenStore, PortalVideoCapture},
        select_backend,
        wayland_geometry::NativeWaylandGeometry,
        x11::X11VideoCapture,
    },
};

const BUS_NAME: &str = "org.solpbc.solstone.Observer1";
const TICK_INTERVAL: Duration = Duration::from_secs(5);

// Runtime order is a safety contract:
// 1. CLI session readiness gate completes before this module is entered.
// 2. lifecycle setup acquires the singleton bus name and performs no capture work.
// 3. recovery finalizes old incomplete segments while singleton ownership is held.
// 4. the run closure constructs audio, video, upload, and observer backends.
// 5. initialize starts the first capture, then ticks run until SIGINT or SIGTERM.
// 6. observer shutdown completes capture/audio cleanup, then event delivery is flushed.
pub fn run_observer(config: Config, host: String) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(%error, "Failed to create observer runtime");
            return 1;
        }
    };
    let _runtime_guard = runtime.enter();
    let singleton = Arc::new(Mutex::new(None::<Connection>));
    let setup_singleton = Arc::clone(&singleton);
    let run_config = config.clone();

    lifecycle(
        &config,
        || match runtime.block_on(acquire_singleton()) {
            Ok(connection) => {
                *setup_singleton.lock().expect("singleton lock") = Some(connection);
                true
            }
            Err(error) => {
                tracing::error!(%error, "Another solstone-linux observer is already running");
                false
            }
        },
        |root, ceiling| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let recovered = recover_incomplete_segments(
                root,
                ceiling.max(1) as u64,
                now,
                &ClaxonMediaDurationProbe,
            );
            if recovered != 0 {
                tracing::info!(recovered, "Recovered incomplete segments");
            }
        },
        || run_capture(&runtime, run_config, host),
        || Ok(()),
        || false,
    )
}

async fn acquire_singleton() -> Result<Connection, String> {
    let connection = Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    let reply = connection
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
        .await
        .map_err(|error| error.to_string())?;
    match reply {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(connection),
        RequestNameReply::Exists | RequestNameReply::InQueue => {
            Err(format!("{BUS_NAME} is already owned"))
        }
    }
}

fn run_capture(
    runtime: &tokio::runtime::Runtime,
    config: Config,
    host: String,
) -> Result<(), ObserverError> {
    let stopped = Arc::new(AtomicBool::new(false));
    spawn_signal_task(Arc::clone(&stopped));
    let (audio, mute) = PulseAudioCapture::spawn().map_err(ObserverError::Io)?;
    let video = VideoBackend::new(&config).map_err(ObserverError::VideoStart)?;
    let upload = UploadClient::new(&config, host.clone(), "linux", env!("CARGO_PKG_VERSION"));
    let backends = Backends {
        video,
        audio,
        activity: UnavailableActivity::default(),
        mute,
        writer: FlacAudioWriter,
        events: UploadEventSink { client: upload },
        clock: SystemClock::new(),
        stats: BackgroundCaptureStats::new(),
        states: NoopStateSink,
    };
    let mut observer = Observer::new(config, backends, host, "linux".into());
    let mut run_result = observer.initialize();
    while run_result.is_ok() && !stopped.load(Ordering::Acquire) {
        thread::park_timeout(TICK_INTERVAL);
        run_result = observer.tick();
    }
    let shutdown = observer.shutdown();
    runtime.block_on(observer.backends.events.client.stop(Duration::from_secs(5)));
    run_result.and(shutdown)
}

fn spawn_signal_task(stopped: Arc<AtomicBool>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::error!(%error, "Failed to install SIGINT handler");
                return;
            }
        };
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::error!(%error, "Failed to install SIGTERM handler");
                return;
            }
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
        tracing::info!("Received shutdown signal");
        stopped.store(true, Ordering::Release);
    });
}

enum VideoBackend {
    Portal(PortalVideoCapture),
    X11(X11VideoCapture),
}

impl VideoBackend {
    fn new(config: &Config) -> Result<Self, String> {
        match select_backend(
            env::var("XDG_SESSION_TYPE").ok().as_deref(),
            env::var("WAYLAND_DISPLAY").ok().as_deref(),
            env::var("DISPLAY").ok().as_deref(),
        ) {
            BackendKind::X11 => X11VideoCapture::new().map(Self::X11),
            BackendKind::Portal => PortalVideoCapture::spawn(
                AshpdPortalOps::new(),
                FileTokenStore::new(config.restore_token_path()),
                NativeWaylandGeometry,
                GstreamerPipelineFactory::new()?,
            )
            .map(Self::Portal),
        }
    }
}

impl VideoCapture for VideoBackend {
    fn start(
        &mut self,
        directory: &std::path::Path,
        framerate: i64,
        draw_cursor: bool,
    ) -> Result<Vec<VideoStream>, String> {
        match self {
            Self::Portal(value) => value.start(directory, framerate, draw_cursor),
            Self::X11(value) => value.start(directory, framerate, draw_cursor),
        }
    }
    fn stop(&mut self) -> Result<Vec<StoppedStream>, String> {
        match self {
            Self::Portal(value) => value.stop(),
            Self::X11(value) => value.stop(),
        }
    }
    fn is_healthy(&self) -> bool {
        match self {
            Self::Portal(value) => value.is_healthy(),
            Self::X11(value) => value.is_healthy(),
        }
    }
}

#[derive(Default)]
struct UnavailableActivity {
    logged: bool,
}

impl ActivityProbe for UnavailableActivity {
    fn probe(&mut self) -> Result<ActivityState, String> {
        let reason = "activity backend is not yet attached; see activity.py".to_owned();
        if !self.logged {
            tracing::warn!(%reason);
            self.logged = true;
        }
        Err(reason)
    }
}

struct UploadEventSink {
    client: UploadClient,
}

impl EventSink for UploadEventSink {
    fn status(&mut self, fields: Map<String, Value>) {
        self.client.enqueue_status(fields);
    }
    fn stream_silent(&mut self, event: StreamSilentEvent) {
        let mut fields = Map::new();
        fields.insert("connector".into(), json!(event.connector));
        fields.insert("position".into(), json!(event.position));
        fields.insert("node_id".into(), json!(event.node_id));
        fields.insert("file_bytes".into(), json!(event.file_bytes));
        fields.insert("segment_dir".into(), json!(event.segment_dir));
        fields.insert("duration_seconds".into(), json!(event.duration_seconds));
        fields.insert("host".into(), json!(event.host));
        fields.insert("platform".into(), json!(event.platform));
        let _ = self.client.enqueue_stream_silent(fields);
    }
    fn segment_completed(&mut self, event: SegmentCompletedEvent) {
        tracing::debug!(key = %event.key, "segment completed");
        // Attach point: sync.py::SyncService will consume this local completion signal.
    }
}

struct SystemClock {
    wall: f64,
    started: Instant,
}
impl SystemClock {
    fn new() -> Self {
        Self {
            wall: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            started: Instant::now(),
        }
    }
}
impl Clock for SystemClock {
    fn wall_seconds(&self) -> f64 {
        self.wall + self.started.elapsed().as_secs_f64()
    }
    fn monotonic_seconds(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

struct NoopStateSink;
impl StateSink for NoopStateSink {
    fn publish(&mut self, _snapshot: StateSnapshot) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn singleton_lock_precedes_recovery_exactly() {
        let config = Config::default();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let setup_calls = Rc::clone(&calls);
        let recover_calls = Rc::clone(&calls);
        let run_calls = Rc::clone(&calls);
        assert_eq!(
            lifecycle(
                &config,
                move || {
                    setup_calls.borrow_mut().push("lock");
                    true
                },
                move |_, _| recover_calls.borrow_mut().push("recover"),
                move || {
                    run_calls.borrow_mut().push("construct");
                    Ok(())
                },
                || Ok(()),
                || false,
            ),
            0
        );
        assert_eq!(&*calls.borrow(), &["lock", "recover", "construct"]);
    }

    #[test]
    fn failed_singleton_lock_prevents_recovery_and_construction() {
        let config = Config::default();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let setup_calls = Rc::clone(&calls);
        let recover_calls = Rc::clone(&calls);
        assert_eq!(
            lifecycle(
                &config,
                move || {
                    setup_calls.borrow_mut().push("lock");
                    false
                },
                move |_, _| recover_calls.borrow_mut().push("recover"),
                || Ok(()),
                || Ok(()),
                || false,
            ),
            1
        );
        assert_eq!(&*calls.borrow(), &["lock"]);
    }
}
