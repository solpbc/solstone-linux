// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The running app's side of the browser path: a same-user unix socket that host-mode
//! processes connect to, one session per connected browser.
//!
//! The trust boundary is the OS user (any process of the same user could launch the
//! host itself), so the controls are the endpoint's ownership and mode and the peer
//! uid, and nothing more.

use super::custody::{Custody, Facts, Layout, Outcome};
use crate::observer::StateSnapshot;
use chrono::Local;
use native_browser_frame::{
    Assembler, CONTROL_MAX, Chunk, DecodeOutcome, Direction, FRESHNESS_MS_MAX, HANDSHAKE_MS_BUDGET,
    STATE_RENEWAL_MS_INTERVAL, Step, build_reply, decode, encode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{broadcast, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};

const LOCAL_HELLO_MAX: usize = 512;
const MAX_SESSIONS: usize = 16;

#[derive(Clone, Debug)]
enum Event {
    Publish,
    Boundary {
        generation: String,
        period_id: String,
    },
    Bye(&'static str),
}

#[derive(Clone, Debug, Serialize)]
struct Connected {
    brand: String,
    since_ms: i64,
    #[serde(skip)]
    id: u64,
}

struct Shared {
    custody: Mutex<Custody>,
    /// The latest facts, refreshed after every change to custody, so a state renewal
    /// never waits behind a disk write.
    view: Mutex<Facts>,
    active: std::sync::atomic::AtomicUsize,
    layout: Layout,
    events: broadcast::Sender<Event>,
    paused: watch::Receiver<StateSnapshot>,
    connected: Mutex<Vec<Connected>>,
    next_id: Mutex<u64>,
    dev_enabled: bool,
    uid: u32,
    on_period_finished: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn paused(&self) -> bool {
        self.paused.borrow().paused
    }

    fn facts(&self) -> Facts {
        let mut facts = self.view.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if self.paused() && facts.generation.is_some() {
            facts.capture = "paused";
        }
        facts
    }

    /// Run `change` on custody off the async workers, then refresh the view.
    async fn with_custody<T: Send + 'static>(
        self: &Arc<Self>,
        change: impl FnOnce(&mut Custody) -> T + Send + 'static,
    ) -> io::Result<T> {
        let shared = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let mut custody = shared.custody.lock().unwrap_or_else(|e| e.into_inner());
            let result = change(&mut custody);
            *shared.view.lock().unwrap_or_else(|e| e.into_inner()) = custody.facts(false);
            result
        })
        .await
        .map_err(io::Error::other)
    }

    fn write_status(&self) {
        let facts = self.facts();
        let connected = self
            .connected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let status = super::status::Running {
            updated_at_ms: Local::now().timestamp_millis(),
            capture: facts.capture.to_owned(),
            delivery: facts.delivery.to_owned(),
            failure: facts.failure.map(str::to_owned),
            held_bytes: facts.held_bytes,
            full: facts.full,
            browsers: connected.iter().map(|c| c.brand.clone()).collect(),
        };
        if let Err(error) = super::status::write_running(&self.layout, &status) {
            tracing::debug!(%error, "Could not write browser status");
        }
    }
}

pub struct ServerHandle {
    shared: Arc<Shared>,
    endpoint: PathBuf,
    tasks: Vec<JoinHandle<()>>,
}

impl ServerHandle {
    /// Tell every connected browser the app is going away, then close the endpoint.
    pub async fn shutdown(self) {
        let _ = self.shared.events.send(Event::Bye("shutdown"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        for task in &self.tasks {
            task.abort();
        }
        let _ = fs::remove_file(&self.endpoint);
        super::status::clear_running(&self.shared.layout);
    }
}

pub struct ServerConfig {
    pub base_dir: PathBuf,
    pub endpoint: PathBuf,
    pub journal: Option<String>,
    pub dev_enabled: bool,
    pub paused: watch::Receiver<StateSnapshot>,
    pub on_period_finished: Arc<dyn Fn() + Send + Sync>,
}

/// Open custody and publish the endpoint. Must be called inside a tokio runtime.
pub fn start(config: ServerConfig) -> io::Result<ServerHandle> {
    let layout = Layout::new(&config.base_dir);
    let custody = Custody::open(layout.clone(), config.journal.as_deref(), Local::now())?;
    let uid = rustix::process::geteuid().as_raw();
    let listener = bind(&config.endpoint, uid)?;
    let (events, _) = broadcast::channel(64);
    let view = custody.facts(false);
    let shared = Arc::new(Shared {
        view: Mutex::new(view),
        active: std::sync::atomic::AtomicUsize::new(0),
        custody: Mutex::new(custody),
        layout,
        events,
        paused: config.paused,
        connected: Mutex::new(Vec::new()),
        next_id: Mutex::new(0),
        dev_enabled: config.dev_enabled,
        uid,
        on_period_finished: config.on_period_finished,
    });
    shared.write_status();
    let tasks = vec![
        tokio::spawn(accept_loop(Arc::clone(&shared), listener)),
        tokio::spawn(clock_loop(Arc::clone(&shared))),
        tokio::spawn(pause_loop(Arc::clone(&shared))),
    ];
    Ok(ServerHandle {
        shared,
        endpoint: config.endpoint,
        tasks,
    })
}

/// Publish the endpoint in a directory only this user can enter. A file already at the
/// endpoint path is never connected to: the app holds the single-instance lock, so
/// anything there is stale.
fn bind(endpoint: &Path, uid: u32) -> io::Result<UnixListener> {
    let directory = endpoint
        .parent()
        .ok_or_else(|| io::Error::other("browser endpoint has no directory"))?;
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(directory)?;
    if !meta.is_dir() || meta.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "browser endpoint directory is not this user's",
        ));
    }
    if meta.mode() & 0o777 != 0o700 {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    // Shorter than the endpoint's own name, so it always fits in sun_path.
    let staging = directory.join(format!(".bh{}", std::process::id()));
    let _ = fs::remove_file(&staging);
    let listener = std::os::unix::net::UnixListener::bind(&staging)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
    fs::rename(&staging, endpoint)?;
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener)
}

async fn accept_loop(shared: Arc<Shared>, listener: UnixListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        if stream
            .peer_cred()
            .map_or(true, |credentials| credentials.uid() != shared.uid)
        {
            continue;
        }
        use std::sync::atomic::Ordering;
        if shared.active.fetch_add(1, Ordering::AcqRel) >= MAX_SESSIONS {
            shared.active.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let worker = Arc::clone(&shared);
        tokio::spawn(async move {
            session(Arc::clone(&worker), stream).await;
            worker.active.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

async fn clock_loop(shared: Arc<Shared>) {
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    let mut since_status = 0_u32;
    loop {
        ticks.tick().await;
        since_status += 1;
        let refresh = since_status >= 5;
        if refresh {
            since_status = 0;
        }
        let Ok(rotated) = shared
            .with_custody(move |custody| {
                let new_period = custody.tick(Local::now());
                if refresh {
                    custody.refresh_held();
                }
                new_period.zip(custody.generation().map(str::to_owned))
            })
            .await
        else {
            continue;
        };
        if refresh {
            let status = Arc::clone(&shared);
            let _ = tokio::task::spawn_blocking(move || status.write_status()).await;
        }
        if let Some((period_id, generation)) = rotated {
            let _ = shared.events.send(Event::Boundary {
                generation,
                period_id,
            });
            (shared.on_period_finished)();
        }
    }
}

async fn pause_loop(shared: Arc<Shared>) {
    let mut paused = shared.paused.clone();
    let mut last = paused.borrow().paused;
    while paused.changed().await.is_ok() {
        let now = paused.borrow().paused;
        if now != last {
            last = now;
            let _ = shared.events.send(Event::Publish);
            shared.write_status();
        }
    }
}

struct Session {
    stream: UnixStream,
    assembler: Assembler,
    pending: std::collections::VecDeque<Vec<u8>>,
}

impl Session {
    /// Read the next whole frame, or `None` at end of input or on a framing error.
    async fn next(&mut self) -> Option<Vec<u8>> {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(frame);
            }
            let count = self.stream.read(&mut buffer).await.ok()?;
            let chunk = if count == 0 {
                Chunk::Eof
            } else {
                Chunk::Data(&buffer[..count])
            };
            for step in self.assembler.feed(chunk).ok()? {
                match step {
                    Step::Message(body) => self.pending.push_back(body),
                    Step::CleanEof => return None,
                    Step::EmptyRead | Step::NeedMore => {}
                }
            }
        }
    }

    async fn send(&mut self, message: &Value) -> io::Result<()> {
        let body = encode(message).map_err(io::Error::other)?;
        // Never put a message on the wire the extension's own codec would refuse.
        if !matches!(
            decode(&body, Direction::HostToExtension),
            DecodeOutcome::Accept(_)
        ) {
            return Err(io::Error::other("refusing to send an invalid message"));
        }
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        frame.extend_from_slice(&body);
        timeout(
            Duration::from_millis(HANDSHAKE_MS_BUDGET),
            self.stream.write_all(&frame),
        )
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }
}

pub fn state_message(kind: &str, facts: &Facts) -> Value {
    let mut message = json!({
        "type": kind,
        "capture": facts.capture,
        "delivery": facts.delivery,
        "freshness_ms": FRESHNESS_MS_MAX,
        "destination_generation": facts.generation,
        "period_id": facts.period_id,
        "custody": {"full": facts.full, "stale": false},
    });
    if let Some(failure) = facts.failure {
        message["failure"] = json!(failure);
    }
    message
}

struct Hello {
    brand: String,
    inst: String,
}

/// The handshake: the host's `local_hello`, then the extension's `hello`, both within
/// one budget. Anything unexpected closes the session without a reply.
async fn handshake(shared: &Shared, session: &mut Session) -> Option<Hello> {
    let deadline = Instant::now() + Duration::from_millis(HANDSHAKE_MS_BUDGET);
    let local = tokio::time::timeout_at(deadline, session.next())
        .await
        .ok()??;
    if local.len() > LOCAL_HELLO_MAX {
        return None;
    }
    let local: Value = serde_json::from_slice(&local).ok()?;
    if local.get("type")?.as_str()? != "local_hello" {
        return None;
    }
    let launched_firefox = match local.get("brand")?.as_str()? {
        "chromium" => false,
        "firefox" => true,
        _ => return None,
    };
    match local.get("mode")?.as_str()? {
        "production" => {}
        "development" if shared.dev_enabled => {}
        _ => return None,
    }
    let hello = tokio::time::timeout_at(deadline, session.next())
        .await
        .ok()??;
    if hello.len() > CONTROL_MAX {
        return None;
    }
    match decode(&hello, Direction::ExtensionToHost) {
        DecodeOutcome::Accept(value) if value["type"] == "hello" => {
            let brand = value["brand"].as_str()?.to_owned();
            if (brand == "firefox") != launched_firefox {
                return None;
            }
            Some(Hello {
                brand,
                inst: value["inst"].as_str()?.to_owned(),
            })
        }
        DecodeOutcome::Unsupported {
            protocol, behind, ..
        } => {
            let _ = session
                .send(&json!({"type": "unsupported", "protocol": protocol, "behind": behind}))
                .await;
            None
        }
        _ => None,
    }
}

async fn session(shared: Arc<Shared>, stream: UnixStream) {
    let mut events = shared.events.subscribe();
    // Handshake frames are small; only batches may be large.
    let mut session = Session {
        stream,
        assembler: Assembler::new(Direction::HostToExtension),
        pending: Default::default(),
    };
    let Some(hello) = handshake(&shared, &mut session).await else {
        return;
    };
    if session.assembler.retained() != 0 {
        return;
    }
    session.assembler = Assembler::new(Direction::ExtensionToHost);
    let id = {
        let mut next = shared.next_id.lock().unwrap_or_else(|e| e.into_inner());
        *next += 1;
        *next
    };
    shared
        .connected
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Connected {
            brand: hello.brand.clone(),
            since_ms: Local::now().timestamp_millis(),
            id,
        });
    shared.write_status();
    let _ = run_session(&shared, &mut session, &hello, &mut events).await;
    shared
        .connected
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|connected| connected.id != id);
    shared.write_status();
}

async fn run_session(
    shared: &Arc<Shared>,
    session: &mut Session,
    hello: &Hello,
    events: &mut broadcast::Receiver<Event>,
) -> io::Result<()> {
    let facts = shared.facts();
    let generation = facts.generation.clone();
    session.send(&state_message("hello_ack", &facts)).await?;
    session.send(&state_message("state", &facts)).await?;
    let mut renewal = tokio::time::interval(Duration::from_millis(STATE_RENEWAL_MS_INTERVAL));
    renewal.tick().await;
    loop {
        tokio::select! {
            frame = session.next() => {
                let Some(frame) = frame else { return Ok(()) };
                let inst = hello.inst.clone();
                let handled = shared
                    .with_custody(move |custody| {
                        let DecodeOutcome::Accept(batch) = decode(&frame, Direction::ExtensionToHost)
                        else {
                            return None;
                        };
                        if batch["type"] != "batch" || batch["inst"].as_str() != Some(inst.as_str()) {
                            return None;
                        }
                        let ids = json!({
                            "destination_generation": batch["destination_generation"],
                            "inst": batch["inst"],
                            "batch_id": batch["batch_id"],
                        });
                        let (outcome, rotated) = custody.accept(&batch, Local::now());
                        Some((ids, outcome, rotated))
                    })
                    .await?;
                // Anything but a valid batch from this browser ends the session.
                let Some((batch, outcome, rotated)) = handled else {
                    return Ok(());
                };
                if let Some(period_id) = rotated
                    && let Some(generation) = &generation
                {
                    let _ = shared.events.send(Event::Boundary {
                        generation: generation.clone(),
                        period_id,
                    });
                    (shared.on_period_finished)();
                }
                session.send(&reply(&batch, outcome)?).await?;
                session.send(&state_message("state", &shared.facts())).await?;
            }
            _ = renewal.tick() => {
                session.send(&state_message("state", &shared.facts())).await?;
            }
            event = events.recv() => match event {
                Ok(Event::Publish) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    session.send(&state_message("state", &shared.facts())).await?;
                }
                Ok(Event::Boundary { generation: changed, period_id }) => {
                    if generation.as_deref() == Some(changed.as_str()) {
                        session
                            .send(&json!({"type": "boundary", "destination_generation": changed, "period_id": period_id}))
                            .await?;
                    }
                    session.send(&state_message("state", &shared.facts())).await?;
                }
                Ok(Event::Bye(reason)) => {
                    let _ = session.send(&json!({"type": "bye", "reason": reason})).await;
                    return Ok(());
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

fn reply(ids: &Value, outcome: Outcome) -> io::Result<Value> {
    let mut receipt = ids.clone();
    match outcome {
        Outcome::Accepted { period_id } => {
            receipt["result"] = json!("accepted");
            receipt["period_id"] = json!(period_id);
        }
        Outcome::Duplicate { period_id } => {
            receipt["result"] = json!("duplicate");
            receipt["period_id"] = json!(period_id);
        }
        Outcome::Rejected { reason } => {
            receipt["result"] = json!("rejected");
            receipt["reason"] = json!(reason);
        }
    }
    build_reply(&receipt).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::host::write_frame;
    use crate::observer::Mode;
    use tokio::io::AsyncReadExt;

    fn snapshot(paused: bool) -> StateSnapshot {
        StateSnapshot {
            mode: Mode::Idle,
            paused,
            segment_open: false,
            captures_today: 0,
            total_size_mb: 0,
            pause_until: None,
            segment_start_mono: None,
            process_start_mono: 0.0,
        }
    }

    struct Rig {
        _temp: tempfile::TempDir,
        endpoint: PathBuf,
        pause: watch::Sender<StateSnapshot>,
        handle: ServerHandle,
    }

    fn rig(journal: Option<&str>, dev_enabled: bool) -> Rig {
        let temp = tempfile::tempdir().unwrap();
        let endpoint = temp.path().join("run/solstone-linux/browser-host.sock");
        fs::create_dir_all(endpoint.parent().unwrap().parent().unwrap()).unwrap();
        let (pause, paused) = watch::channel(snapshot(false));
        let handle = start(ServerConfig {
            base_dir: temp.path().join("data"),
            endpoint: endpoint.clone(),
            journal: journal.map(str::to_owned),
            dev_enabled,
            paused,
            on_period_finished: Arc::new(|| {}),
        })
        .unwrap();
        Rig {
            _temp: temp,
            endpoint,
            pause,
            handle,
        }
    }

    async fn frame(stream: &mut UnixStream, body: &[u8]) {
        let mut out = Vec::new();
        write_frame(&mut out, body).unwrap();
        stream.write_all(&out).await.unwrap();
    }

    async fn read(stream: &mut UnixStream) -> Value {
        let mut length = [0_u8; 4];
        timeout(Duration::from_secs(10), stream.read_exact(&mut length))
            .await
            .unwrap()
            .unwrap();
        let mut body = vec![0_u8; u32::from_ne_bytes(length) as usize];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn read_type(stream: &mut UnixStream, kind: &str) -> Value {
        loop {
            let message = read(stream).await;
            if message["type"] == kind {
                return message;
            }
        }
    }

    async fn connect(rig: &Rig, brand: &str, mode: &str, hello_brand: &str) -> UnixStream {
        let mut stream = UnixStream::connect(&rig.endpoint).await.unwrap();
        frame(
            &mut stream,
            format!(r#"{{"brand":"{brand}","mode":"{mode}","type":"local_hello"}}"#).as_bytes(),
        )
        .await;
        frame(
            &mut stream,
            format!(r#"{{"type":"hello","protocol":1,"version":"0.2.0","brand":"{hello_brand}","inst":"inst-a"}}"#)
                .as_bytes(),
        )
        .await;
        stream
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_paired_app_permits_capture_accepts_batches_and_answers_replays() {
        let rig = rig(Some("journal-a"), false);
        let meta = fs::metadata(rig.endpoint.parent().unwrap()).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o700);
        let mut stream = connect(&rig, "chromium", "production", "edge").await;
        let ack = read(&mut stream).await;
        assert_eq!(ack["type"], "hello_ack");
        assert_eq!(ack["capture"], "permitted");
        assert_eq!(ack["freshness_ms"], 15000);
        let generation = ack["destination_generation"].as_str().unwrap().to_owned();
        assert_eq!(read(&mut stream).await["type"], "state");
        let batch = json!({
            "type": "batch", "destination_generation": generation, "inst": "inst-a",
            "batch_id": "0123456789abcdef0123456789abcdef",
            "queued_at_ms": Local::now().timestamp_millis(),
            "records": [{"t": "segment_start", "ts": 1, "ctx": "c1", "inst": "inst-a",
                         "blocks": [{"id": "b", "text": "marker"}]}],
        });
        let bytes = encode(&batch).unwrap();
        frame(&mut stream, &bytes).await;
        let first = read_type(&mut stream, "accepted").await;
        assert_eq!(first["result"], "accepted");
        frame(&mut stream, &bytes).await;
        let replay = read_type(&mut stream, "accepted").await;
        assert_eq!(replay["result"], "duplicate");
        assert_eq!(replay["period_id"], first["period_id"]);

        rig.pause.send_replace(snapshot(true));
        let paused = read_type(&mut stream, "state").await;
        let paused = if paused["capture"] == "paused" {
            paused
        } else {
            read_type(&mut stream, "state").await
        };
        assert_eq!(paused["capture"], "paused");
        rig.pause.send_replace(snapshot(false));

        rig.handle.shutdown().await;
        assert_eq!(read_type(&mut stream, "bye").await["reason"], "shutdown");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unpaired_app_says_so() {
        let rig = rig(None, false);
        let mut stream = connect(&rig, "firefox", "production", "firefox").await;
        let ack = read(&mut stream).await;
        assert_eq!(ack["capture"], "not_paired");
        assert_eq!(ack["destination_generation"], Value::Null);
        rig.handle.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_development_launch_is_closed_unless_the_build_admits_it() {
        let rig = rig(Some("journal-a"), false);
        let mut stream = connect(&rig, "chromium", "development", "chrome").await;
        let mut byte = [0_u8; 1];
        assert_eq!(
            timeout(Duration::from_secs(10), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        rig.handle.shutdown().await;
        let dev = super::tests::rig(Some("journal-a"), true);
        let mut stream = connect(&dev, "chromium", "development", "chrome").await;
        assert_eq!(read(&mut stream).await["type"], "hello_ack");
        dev.handle.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_brand_that_contradicts_the_launch_is_closed() {
        let rig = rig(Some("journal-a"), false);
        let mut stream = connect(&rig, "chromium", "production", "firefox").await;
        let mut byte = [0_u8; 1];
        assert_eq!(
            timeout(Duration::from_secs(10), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        rig.handle.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_newer_protocol_is_told_the_app_is_behind() {
        let rig = rig(Some("journal-a"), false);
        let mut stream = UnixStream::connect(&rig.endpoint).await.unwrap();
        frame(
            &mut stream,
            br#"{"brand":"firefox","mode":"production","type":"local_hello"}"#,
        )
        .await;
        frame(
            &mut stream,
            br#"{"type":"hello","protocol":2,"version":"9.0.0","brand":"firefox","inst":"i"}"#,
        )
        .await;
        let message = read(&mut stream).await;
        assert_eq!(message["type"], "unsupported");
        assert_eq!(message["behind"], "app");
        rig.handle.shutdown().await;
    }
}
