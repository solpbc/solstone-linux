// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    future::Future,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use ashpd::desktop::{
    PersistMode, Session,
    screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
        StartCastOptions,
    },
};
use tracing::warn;

use crate::{
    config::Config,
    matching::{PortalStream, match_streams_to_monitors},
    observer::{StoppedStream, VideoCapture, VideoStream},
    pipeline::pipeline_description,
    positions::Monitor,
    restore_token::{load_restore_token, save_restore_token},
    streams::{is_healthy_file_size, stream_filename},
    video::{
        clamp_framerate,
        gstreamer::{CapturePipeline, PipelineFactory, stop_pipelines},
        wayland_geometry::NativeWaylandGeometry,
    },
};

type PortalFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

// Start can consume both independent interactive budgets plus the short calls.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1_270);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalOperation {
    CreateSession,
    SelectSources,
    Start,
    OpenPipeWireRemote,
    Close,
}

pub fn portal_timeout(operation: PortalOperation) -> Duration {
    match operation {
        PortalOperation::SelectSources | PortalOperation::Start => Duration::from_secs(600),
        PortalOperation::CreateSession
        | PortalOperation::OpenPipeWireRemote
        | PortalOperation::Close => Duration::from_secs(30),
    }
}

pub fn cursor_mode(draw_cursor: bool) -> u32 {
    if draw_cursor { 2 } else { 1 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalStartResult {
    pub streams: Vec<PortalStream>,
    pub restore_token: Option<String>,
}

pub trait PortalOps: Send + 'static {
    fn create_session(&mut self) -> PortalFuture<'_, ()>;
    fn select_sources(
        &mut self,
        restore_token: Option<String>,
        cursor_mode: u32,
    ) -> PortalFuture<'_, ()>;
    fn start(&mut self) -> PortalFuture<'_, PortalStartResult>;
    fn open_pipe_wire_remote(&mut self) -> PortalFuture<'_, OwnedFd>;
    fn close(&mut self) -> PortalFuture<'_, ()>;
}

pub trait TokenStore: Send + 'static {
    fn load(&self) -> Option<String>;
    fn save(&self, token: &str) -> Result<(), String>;
}

pub struct FileTokenStore {
    path: PathBuf,
}

impl FileTokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_config(config: &Config) -> Self {
        Self::new(config.restore_token_path())
    }
}

impl TokenStore for FileTokenStore {
    fn load(&self) -> Option<String> {
        load_restore_token(&self.path)
    }

    fn save(&self, token: &str) -> Result<(), String> {
        save_restore_token(&self.path, token).map_err(|error| error.to_string())
    }
}

pub trait PortalGeometry: Send + 'static {
    fn monitors(&mut self) -> Result<Vec<Monitor>, String>;
}

impl PortalGeometry for NativeWaylandGeometry {
    fn monitors(&mut self) -> Result<Vec<Monitor>, String> {
        NativeWaylandGeometry::monitors(self)
    }
}

#[derive(Debug)]
struct PortalSession {
    streams: PortalStartResult,
    remote: OwnedFd,
    monitors: Vec<Monitor>,
}

async fn close_after_error<O: PortalOps>(ops: &mut O, original: String) -> String {
    match ops.close().await {
        Ok(()) => original,
        Err(close_error) => {
            format!("{original}; additionally failed to close portal session: {close_error}")
        }
    }
}

async fn open_session<O: PortalOps, T: TokenStore, G: PortalGeometry>(
    ops: &mut O,
    tokens: &T,
    geometry: &mut G,
    draw_cursor: bool,
) -> Result<PortalSession, String> {
    ops.create_session().await?;
    if let Err(error) = ops
        .select_sources(tokens.load(), cursor_mode(draw_cursor))
        .await
    {
        return Err(close_after_error(ops, error).await);
    }
    let streams = match ops.start().await {
        Ok(result) => result,
        Err(error) => return Err(close_after_error(ops, error).await),
    };
    if let Some(token) = streams
        .restore_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        && let Err(error) = tokens.save(token)
    {
        warn!(%error, "could not save rotated portal restore token");
    }
    let remote = match ops.open_pipe_wire_remote().await {
        Ok(remote) => remote,
        Err(error) => return Err(close_after_error(ops, error).await),
    };
    let monitors = match geometry.monitors() {
        Ok(monitors) => monitors,
        Err(error) => return Err(close_after_error(ops, error).await),
    };
    Ok(PortalSession {
        streams,
        remote,
        monitors,
    })
}

pub struct AshpdPortalOps {
    proxy: Option<Screencast>,
    session: Option<Session<Screencast>>,
}

impl AshpdPortalOps {
    pub fn new() -> Self {
        Self {
            proxy: None,
            session: None,
        }
    }
}

impl Default for AshpdPortalOps {
    fn default() -> Self {
        Self::new()
    }
}

impl PortalOps for AshpdPortalOps {
    fn create_session(&mut self) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            let proxy = tokio::time::timeout(
                portal_timeout(PortalOperation::CreateSession),
                Screencast::new(),
            )
            .await
            .map_err(|_| "CreateSession timed out".to_owned())?
            .map_err(|error| error.to_string())?;
            let session = tokio::time::timeout(
                portal_timeout(PortalOperation::CreateSession),
                proxy.create_session(Default::default()),
            )
            .await
            .map_err(|_| "CreateSession timed out".to_owned())?
            .map_err(|error| error.to_string())?;
            self.proxy = Some(proxy);
            self.session = Some(session);
            Ok(())
        })
    }

    fn select_sources(
        &mut self,
        restore_token: Option<String>,
        cursor: u32,
    ) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            let proxy = self.proxy.as_ref().ok_or("portal proxy is not open")?;
            let session = self.session.as_ref().ok_or("portal session is not open")?;
            let cursor = if cursor == 2 {
                CursorMode::Embedded
            } else {
                CursorMode::Hidden
            };
            let options = SelectSourcesOptions::default()
                .set_sources(Some(SourceType::Monitor.into()))
                .set_multiple(true)
                .set_cursor_mode(cursor)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(restore_token.as_deref());
            let request = tokio::time::timeout(
                portal_timeout(PortalOperation::SelectSources),
                proxy.select_sources(session, options),
            )
            .await
            .map_err(|_| "SelectSources timed out".to_owned())?
            .map_err(|error| error.to_string())?;
            request.response().map_err(|error| error.to_string())?;
            Ok(())
        })
    }

    fn start(&mut self) -> PortalFuture<'_, PortalStartResult> {
        Box::pin(async move {
            let proxy = self.proxy.as_ref().ok_or("portal proxy is not open")?;
            let session = self.session.as_ref().ok_or("portal session is not open")?;
            let request = tokio::time::timeout(
                portal_timeout(PortalOperation::Start),
                proxy.start(session, None, StartCastOptions::default()),
            )
            .await
            .map_err(|_| "Start timed out".to_owned())?
            .map_err(|error| error.to_string())?;
            let response = request.response().map_err(|error| error.to_string())?;
            Ok(PortalStartResult {
                streams: response
                    .streams()
                    .iter()
                    .enumerate()
                    .map(|(index, stream)| PortalStream {
                        index,
                        node_id: stream.pipe_wire_node_id(),
                        position: stream.position(),
                        size: stream.size(),
                    })
                    .collect(),
                restore_token: response.restore_token().map(str::to_owned),
            })
        })
    }

    fn open_pipe_wire_remote(&mut self) -> PortalFuture<'_, OwnedFd> {
        Box::pin(async move {
            let proxy = self.proxy.as_ref().ok_or("portal proxy is not open")?;
            let session = self.session.as_ref().ok_or("portal session is not open")?;
            tokio::time::timeout(
                portal_timeout(PortalOperation::OpenPipeWireRemote),
                proxy.open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default()),
            )
            .await
            .map_err(|_| "OpenPipeWireRemote timed out".to_owned())?
            .map_err(|error| error.to_string())
        })
    }

    fn close(&mut self) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            let session = self.session.take();
            self.proxy = None;
            let Some(session) = session else {
                return Ok(());
            };
            tokio::time::timeout(portal_timeout(PortalOperation::Close), session.close())
                .await
                .map_err(|_| "Close timed out".to_owned())?
                .map_err(|error| error.to_string())
        })
    }
}

struct PortalPipeline {
    node_id: u32,
    connector: String,
    position: String,
    output: PathBuf,
    // Deliberately retain one clone per pipeline. Whether pipewiresrc duplicates
    // internally is unverified; sibling lifetime must be correct by construction.
    _remote: OwnedFd,
    pipeline: Box<dyn CapturePipeline>,
}

enum Command {
    Start {
        directory: PathBuf,
        framerate: i64,
        draw_cursor: bool,
        reply: mpsc::Sender<Result<Vec<VideoStream>, String>>,
    },
    Stop {
        reply: mpsc::Sender<Result<Vec<StoppedStream>, String>>,
    },
    Shutdown {
        reply: mpsc::Sender<()>,
    },
}

pub struct PortalVideoCapture {
    commands: mpsc::Sender<Command>,
    healthy: Arc<AtomicBool>,
}

impl PortalVideoCapture {
    pub fn spawn<O, T, G, F>(
        mut ops: O,
        tokens: T,
        mut geometry: G,
        mut factory: F,
    ) -> Result<Self, String>
    where
        O: PortalOps,
        T: TokenStore,
        G: PortalGeometry,
        F: PipelineFactory + 'static,
    {
        let (commands, receiver) = mpsc::channel();
        let healthy = Arc::new(AtomicBool::new(false));
        let worker_health = healthy.clone();
        thread::Builder::new().name("solstone-portal".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(_) => { worker_health.store(false, Ordering::Release); return; }
            };
            let mut tracked: Vec<PortalPipeline> = Vec::new();
            loop {
                let command = match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let alive = !tracked.is_empty() && tracked.iter().all(|record| record.pipeline.is_healthy());
                        worker_health.store(alive, Ordering::Release);
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match command {
                    Command::Start { directory, framerate, draw_cursor, reply } => {
                        let result = runtime.block_on(open_session(&mut ops, &tokens, &mut geometry, draw_cursor)).and_then(|session| {
                            let matched = match_streams_to_monitors(&session.streams.streams, &session.monitors);
                            let mut streams = Vec::new();
                            for stream in matched {
                                let output = directory.join(stream_filename(&stream.position_label, &stream.connector));
                                let remote = match session.remote.try_clone() { Ok(fd) => fd, Err(error) => { warn!(%error, "could not clone PipeWire fd; other streams continue"); continue; } };
                                let description = pipeline_description(remote.as_raw_fd(), stream.node_id, clamp_framerate(framerate), &output);
                                let mut pipeline = match factory.build(&description) { Ok(pipeline) => pipeline, Err(error) => { warn!(connector = %stream.connector, %error, "portal pipeline construction failed; other streams continue"); continue; } };
                                if let Err(error) = pipeline.start() { warn!(connector = %stream.connector, %error, "portal pipeline failed to enter Playing; other streams continue"); pipeline.force_stop(); continue; }
                                streams.push(VideoStream { connector: stream.connector.clone(), position: stream.position_label.clone(), file_path: output.to_string_lossy().into_owned() });
                                tracked.push(PortalPipeline { node_id: stream.node_id, connector: stream.connector, position: stream.position_label, output, _remote: remote, pipeline });
                            }
                            if streams.is_empty() { Err("No portal stream pipelines could be started".into()) } else { Ok(streams) }
                        });
                        if result.is_err() {
                            let _ = runtime.block_on(ops.close());
                            tracked.clear();
                        }
                        worker_health.store(result.is_ok(), Ordering::Release);
                        let _ = reply.send(result);
                    }
                    Command::Stop { reply } => {
                        let mut pipelines: Vec<&mut Box<dyn CapturePipeline>> = tracked.iter_mut().map(|record| &mut record.pipeline).collect();
                        stop_pipelines(&mut pipelines, Duration::from_secs(5));
                        drop(pipelines);
                        let stopped = tracked.drain(..).map(|record| {
                            let bytes = match std::fs::metadata(&record.output) { Ok(metadata) => metadata.len(), Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0, Err(error) => { warn!(path = %record.output.display(), %error, "could not stat portal stream file"); 0 } };
                            if !is_healthy_file_size(Some(bytes)) && let Err(error) = std::fs::remove_file(&record.output) && error.kind() != std::io::ErrorKind::NotFound { warn!(path = %record.output.display(), %error, "could not unlink silent portal stream file"); }
                            StoppedStream { node_id: record.node_id, connector: record.connector, position: record.position, file_bytes: bytes }
                        }).collect();
                        let close = runtime.block_on(ops.close());
                        if let Err(error) = close { warn!(%error, "failed to close portal session during stop"); }
                        worker_health.store(false, Ordering::Release);
                        let _ = reply.send(Ok(stopped));
                    }
                    Command::Shutdown { reply } => { let _ = runtime.block_on(ops.close()); worker_health.store(false, Ordering::Release); let _ = reply.send(()); break; }
                }
            }
            worker_health.store(false, Ordering::Release);
        }).map_err(|error| error.to_string())?;
        Ok(Self { commands, healthy })
    }

    fn request<T>(&self, build: impl FnOnce(mpsc::Sender<T>) -> Command) -> Result<T, String> {
        let (reply, receiver) = mpsc::channel();
        self.commands
            .send(build(reply))
            .map_err(|_| "portal worker disconnected".to_owned())?;
        receiver
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => "portal worker reply timed out".into(),
                mpsc::RecvTimeoutError::Disconnected => "portal worker disconnected".into(),
            })
    }
}

impl VideoCapture for PortalVideoCapture {
    fn start(
        &mut self,
        directory: &Path,
        framerate: i64,
        draw_cursor: bool,
    ) -> Result<Vec<VideoStream>, String> {
        self.request(|reply| Command::Start {
            directory: directory.to_owned(),
            framerate,
            draw_cursor,
            reply,
        })?
    }
    fn stop(&mut self) -> Result<Vec<StoppedStream>, String> {
        self.request(|reply| Command::Stop { reply })?
    }
    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
}

impl Drop for PortalVideoCapture {
    fn drop(&mut self) {
        let (reply, receiver) = mpsc::channel();
        if self.commands.send(Command::Shutdown { reply }).is_ok() {
            let _ = receiver.recv_timeout(Duration::from_secs(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{pipeline::PipelineDescription, positions::BoxGeometry};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Store(Arc<Mutex<Option<String>>>);
    impl TokenStore for Store {
        fn load(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
        fn save(&self, token: &str) -> Result<(), String> {
            *self.0.lock().unwrap() = Some(token.into());
            Ok(())
        }
    }
    struct Geometry(Vec<Monitor>);
    impl PortalGeometry for Geometry {
        fn monitors(&mut self) -> Result<Vec<Monitor>, String> {
            Ok(self.0.clone())
        }
    }
    struct FakeOps {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail: Option<&'static str>,
        token: Option<String>,
        streams: Vec<PortalStream>,
    }
    impl PortalOps for FakeOps {
        fn create_session(&mut self) -> PortalFuture<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("create");
                if self.fail == Some("create") {
                    Err("create failed".into())
                } else {
                    Ok(())
                }
            })
        }
        fn select_sources(&mut self, _: Option<String>, _: u32) -> PortalFuture<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("select");
                if self.fail.is_some_and(|failure| failure.contains("select")) {
                    Err("select failed".into())
                } else {
                    Ok(())
                }
            })
        }
        fn start(&mut self) -> PortalFuture<'_, PortalStartResult> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("start");
                if self.fail == Some("start") {
                    Err("start failed".into())
                } else {
                    Ok(PortalStartResult {
                        streams: self.streams.clone(),
                        restore_token: self.token.clone(),
                    })
                }
            })
        }
        fn open_pipe_wire_remote(&mut self) -> PortalFuture<'_, OwnedFd> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("open");
                if self.fail == Some("open") {
                    Err("open failed".into())
                } else {
                    Ok(std::fs::File::open("/dev/null").unwrap().into())
                }
            })
        }
        fn close(&mut self) -> PortalFuture<'_, ()> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("close");
                if self.fail.is_some_and(|failure| failure.contains("close")) {
                    Err("close failed".into())
                } else {
                    Ok(())
                }
            })
        }
    }
    #[derive(Default)]
    struct PipelineState {
        healthy: bool,
        stopped: bool,
    }
    struct FakePipeline(Arc<Mutex<PipelineState>>);
    impl CapturePipeline for FakePipeline {
        fn start(&mut self) -> Result<(), String> {
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
        fn force_stop(&mut self) {
            self.0.lock().unwrap().stopped = true;
        }
    }
    #[derive(Default)]
    struct FakeFactory {
        states: Arc<Mutex<Vec<Arc<Mutex<PipelineState>>>>>,
        descriptions: Arc<Mutex<Vec<PipelineDescription>>>,
    }
    impl PipelineFactory for FakeFactory {
        fn build(
            &mut self,
            description: &PipelineDescription,
        ) -> Result<Box<dyn CapturePipeline>, String> {
            self.descriptions.lock().unwrap().push(description.clone());
            let state = Arc::new(Mutex::new(PipelineState {
                healthy: true,
                stopped: false,
            }));
            self.states.lock().unwrap().push(state.clone());
            Ok(Box::new(FakePipeline(state)))
        }
    }
    fn monitor() -> Monitor {
        Monitor {
            id: "DP-1".into(),
            bounds: BoxGeometry {
                x1: 0,
                y1: 0,
                x2: 1920,
                y2: 1080,
            },
            position: Some("center".into()),
        }
    }

    #[test]
    fn timeout_budgets_ignore_token_presence() {
        assert_eq!(
            portal_timeout(PortalOperation::SelectSources),
            Duration::from_secs(600)
        );
        assert_eq!(
            portal_timeout(PortalOperation::Start),
            Duration::from_secs(600)
        );
        assert_eq!(
            portal_timeout(PortalOperation::CreateSession),
            Duration::from_secs(30)
        );
        assert_eq!(cursor_mode(true), 2);
        assert_eq!(cursor_mode(false), 1);
        for _token in [None, Some("present")] {
            assert_eq!(
                portal_timeout(PortalOperation::SelectSources),
                Duration::from_secs(600)
            );
            assert_eq!(
                portal_timeout(PortalOperation::Start),
                Duration::from_secs(600)
            );
        }
    }
    #[test]
    fn failures_after_create_always_close() {
        for fail in ["select", "start", "open"] {
            let calls = Arc::new(Mutex::new(vec![]));
            let mut ops = FakeOps {
                calls: calls.clone(),
                fail: Some(fail),
                token: None,
                streams: vec![],
            };
            let store = Store::default();
            let mut geometry = Geometry(vec![monitor()]);
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let error = runtime
                .block_on(open_session(&mut ops, &store, &mut geometry, true))
                .unwrap_err();
            assert!(error.contains("failed"));
            assert_eq!(calls.lock().unwrap().last(), Some(&"close"));
        }
    }
    #[test]
    fn close_failure_preserves_original_error_and_clears_session() {
        // tests/test_screencast.py::test_close_session_call_close_failure_logs_and_clears_handle
        let calls = Arc::new(Mutex::new(vec![]));
        let mut ops = FakeOps {
            calls: calls.clone(),
            fail: Some("select+close"),
            token: None,
            streams: vec![],
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(open_session(
                &mut ops,
                &Store::default(),
                &mut Geometry(vec![monitor()]),
                true,
            ))
            .unwrap_err();
        assert!(error.starts_with("select failed"));
        assert!(error.contains("additionally failed to close"));
        assert_eq!(calls.lock().unwrap().last(), Some(&"close"));
    }
    #[test]
    fn token_absent_or_blank_is_untouched_and_nonempty_overwrites() {
        for (returned, expected) in [
            (None, "old"),
            (Some("   ".into()), "old"),
            (Some("new".into()), "new"),
        ] {
            let calls = Arc::new(Mutex::new(vec![]));
            let mut ops = FakeOps {
                calls,
                fail: Some("open"),
                token: returned,
                streams: vec![],
            };
            let store = Store::default();
            *store.0.lock().unwrap() = Some("old".into());
            let mut geometry = Geometry(vec![monitor()]);
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let _ = runtime.block_on(open_session(&mut ops, &store, &mut geometry, true));
            assert_eq!(store.load().as_deref(), Some(expected));
        }
    }

    #[test]
    fn real_geometry_reaches_matching_and_unmatched_stream_falls_back() {
        let calls = Arc::new(Mutex::new(vec![]));
        let ops = FakeOps {
            calls,
            fail: None,
            token: None,
            streams: vec![
                PortalStream {
                    index: 0,
                    node_id: 10,
                    position: Some((0, 0)),
                    size: Some((1920, 1080)),
                },
                PortalStream {
                    index: 1,
                    node_id: 11,
                    position: Some((9000, 0)),
                    size: Some((800, 600)),
                },
            ],
        };
        let factory = FakeFactory::default();
        let states = factory.states.clone();
        let mut capture =
            PortalVideoCapture::spawn(ops, Store::default(), Geometry(vec![monitor()]), factory)
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let streams = capture.start(directory.path(), 1, true).unwrap();
        assert_eq!(
            streams[0].file_path,
            directory
                .path()
                .join("center_DP-1_screen.webm")
                .to_string_lossy()
        );
        assert_eq!(
            streams[1].file_path,
            directory
                .path()
                .join("unknown_monitor-1_screen.webm")
                .to_string_lossy()
        );
        assert!(capture.is_healthy());
        states.lock().unwrap()[0].lock().unwrap().healthy = false;
        thread::sleep(Duration::from_millis(150));
        assert!(!capture.is_healthy());
        let stopped = capture.stop().unwrap();
        assert_eq!(stopped.len(), 2);
    }

    #[test]
    fn one_pipeline_teardown_does_not_invalidate_sibling_state() {
        let remote: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        let first = remote.try_clone().unwrap();
        let sibling = remote.try_clone().unwrap();
        drop(first);
        assert!(sibling.try_clone().is_ok());
    }

    struct PanicGeometry;
    impl PortalGeometry for PanicGeometry {
        fn monitors(&mut self) -> Result<Vec<Monitor>, String> {
            panic!("worker died")
        }
    }
    #[test]
    fn worker_panic_is_a_disconnect_not_an_observer_unwind() {
        let ops = FakeOps {
            calls: Arc::new(Mutex::new(vec![])),
            fail: None,
            token: None,
            streams: vec![],
        };
        let mut capture =
            PortalVideoCapture::spawn(ops, Store::default(), PanicGeometry, FakeFactory::default())
                .unwrap();
        let error = capture.start(Path::new("/tmp"), 1, true).unwrap_err();
        assert!(error.contains("disconnected"));
        assert!(!capture.is_healthy());
    }
}
