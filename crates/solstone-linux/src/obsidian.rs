// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The Obsidian vault source on Linux: the shared engine run as a background task,
//! delivering over the app's paired upload path as the `obsidian` source.

pub use solstone_vault::{FoundVault, Refusal, State, Status};

use crate::upload::{CapturedAssociation, UploadClient, UploadResult};
use serde::{Deserialize, Serialize};
use solstone_vault::{Answer, Delivery, Engine, ListedVault, Phase, ScanOutcome, SystemClock};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};

pub const SOURCE: &str = "obsidian";
pub const CAPABILITY: &str = "obsidian_notes";
pub(crate) const INVALID_REASON: &str = "obsidian_note_invalid";

const TICK: Duration = Duration::from_secs(2);
const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);
const CAPABILITY_RETRY: Duration = Duration::from_secs(30);
const VAULT_CHECK_RETRY: Duration = Duration::from_secs(30);
const CLOSE_BATCH: usize = 500;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// What the app says about the source to its window.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObsidianDocument {
    pub v: u32,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

impl ObsidianDocument {
    fn new(status: Status, refusal: Option<Refusal>) -> Self {
        Self {
            v: 1,
            status,
            refusal,
        }
    }

    fn encoded(&self) -> String {
        serde_json::to_string(self).expect("a status document always serializes")
    }
}

pub fn decode_document(text: &str) -> Option<ObsidianDocument> {
    serde_json::from_str(text)
        .ok()
        .filter(|doc: &ObsidianDocument| doc.v == 1)
}

fn off_status() -> Status {
    Status {
        state: State::Off,
        vault: None,
        warning: None,
        last_checked_ms: None,
        waiting: 0,
        first_sync: None,
        not_on_device: 0,
        too_large: 0,
        attention: None,
    }
}

/// The vaults Obsidian itself lists on this computer.
pub fn found_vaults() -> Vec<FoundVault> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    solstone_vault::found_vaults(&solstone_vault::linux_candidates(&home))
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether the paired journal has said it accepts note changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Capability {
    Supported,
    NotListed,
    /// Not paired, or no answer.
    Unknown,
}

pub(crate) enum VaultList {
    Listed(Vec<ListedVault>),
    Unavailable,
}

/// The journal, as far as the source needs it.
pub(crate) trait Transport: Send + Sync + 'static {
    fn capability(&self) -> BoxFuture<'_, Capability>;
    fn vault_list(&self) -> BoxFuture<'_, VaultList>;
    /// Send one file. `None` means no answer from the journal to act on.
    fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, Option<Answer>>;
    /// Whether the journal this was last asked of is still the paired one.
    fn pairing_unchanged(&self) -> bool;
}

/// The real journal over the paired upload path.
struct LinkTransport {
    client: Arc<UploadClient>,
    asked: Mutex<Option<CapturedAssociation>>,
}

impl Transport for LinkTransport {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move {
            *self.asked.lock().unwrap_or_else(|p| p.into_inner()) =
                Some(self.client.capture_association());
            let Some(capability) = self.client.capability() else {
                return Capability::Unknown;
            };
            match capability.system_capabilities().await {
                Ok(listed) if listed.iter().any(|name| name == CAPABILITY) => Capability::Supported,
                Ok(_) => Capability::NotListed,
                Err(_) => Capability::Unknown,
            }
        })
    }

    fn vault_list(&self) -> BoxFuture<'_, VaultList> {
        Box::pin(async move {
            let Some(capability) = self.client.capability() else {
                return VaultList::Unavailable;
            };
            use crate::private_link::LinkOutcome;
            match capability.obsidian_vaults().await {
                LinkOutcome::Success { status, body } if status == reqwest::StatusCode::OK => {
                    parse_vault_list(&body).map_or(VaultList::Unavailable, VaultList::Listed)
                }
                // An older journal without the route: no vault is known from elsewhere.
                LinkOutcome::LocalRejected { status }
                    if status == reqwest::StatusCode::NOT_FOUND =>
                {
                    VaultList::Listed(Vec::new())
                }
                _ => VaultList::Unavailable,
            }
        })
    }

    fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, Option<Answer>> {
        Box::pin(async move {
            if !self.client.has_capability() || self.client.is_revoked() {
                return None;
            }
            let result = self
                .client
                .upload_source_segment(
                    &delivery.window.day,
                    &delivery.window.segment,
                    std::slice::from_ref(&delivery.path),
                    None,
                    Some(SOURCE),
                )
                .await;
            Some(answer_for(&result))
        })
    }

    fn pairing_unchanged(&self) -> bool {
        self.asked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|asked| self.client.association_is_current(asked))
    }
}

/// How the app takes what the upload client got back.
pub(crate) fn answer_for(result: &UploadResult) -> Answer {
    // An answer from a journal no longer paired means nothing to the current one.
    if result.stale_capability {
        return Answer::Retry;
    }
    if result.success {
        return Answer::Delivered;
    }
    Answer::from_reply(None, result.reason_code.as_deref())
}

pub(crate) fn parse_vault_list(body: &[u8]) -> Option<Vec<ListedVault>> {
    #[derive(Deserialize)]
    struct Listing {
        vaults: Vec<ListedVault>,
    }
    serde_json::from_slice::<Listing>(body)
        .ok()
        .map(|listing| listing.vaults)
}

enum Command {
    TurnOn {
        path: String,
        reply: oneshot::Sender<ObsidianDocument>,
    },
    TurnOff(oneshot::Sender<ObsidianDocument>),
    Continue(oneshot::Sender<ObsidianDocument>),
    Cancel(oneshot::Sender<ObsidianDocument>),
}

/// The running source, as the rest of the app talks to it. Default is a source
/// that is not available.
#[derive(Clone, Default)]
pub struct ObsidianHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<Command>,
    status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    stop_notify: Arc<tokio::sync::Notify>,
}

impl ObsidianHandle {
    pub fn state_json(&self) -> String {
        let status = self.inner.as_ref().map_or_else(off_status, |inner| {
            inner
                .status
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        });
        ObsidianDocument::new(status, None).encoded()
    }

    async fn call(
        &self,
        build: impl FnOnce(oneshot::Sender<ObsidianDocument>) -> Command,
    ) -> String {
        let Some(inner) = &self.inner else {
            return self.state_json();
        };
        let (reply, answer) = oneshot::channel();
        if inner.commands.send(build(reply)).await.is_err() {
            return self.state_json();
        }
        match answer.await {
            Ok(document) => document.encoded(),
            Err(_) => self.state_json(),
        }
    }

    pub async fn turn_on(&self, path: String) -> String {
        self.call(|reply| Command::TurnOn { path, reply }).await
    }

    pub async fn turn_off(&self) -> String {
        if let Some(inner) = &self.inner {
            inner.cancel.store(true, Ordering::Release);
            inner.cancel_notify.notify_one();
        }
        self.call(Command::TurnOff).await
    }

    pub async fn continue_warning(&self) -> String {
        self.call(Command::Continue).await
    }

    pub async fn cancel_warning(&self) -> String {
        self.call(Command::Cancel).await
    }

    /// Stop the source and wait for it to save what it holds.
    pub(crate) async fn shutdown(&self, task: tokio::task::JoinHandle<()>) {
        let Some(inner) = &self.inner else {
            return;
        };
        inner.cancel.store(true, Ordering::Release);
        inner.cancel_notify.notify_one();
        inner.stop.store(true, Ordering::Release);
        inner.stop_notify.notify_one();
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, task).await;
    }
}

/// Start the source's background task. A failure to open leaves the rest of the app
/// running, with this source unavailable.
pub(crate) fn start(
    runtime: &tokio::runtime::Runtime,
    root: PathBuf,
    client: Arc<UploadClient>,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
) -> (ObsidianHandle, Option<tokio::task::JoinHandle<()>>) {
    let transport = LinkTransport {
        client,
        asked: Mutex::new(None),
    };
    let (handle, mut worker) = spawnable(None, transport, paused);
    // The first read of what's held can be long, so it happens with the source's
    // own work, never in the way of the app starting or of the window.
    let task = runtime.spawn(async move {
        match tokio::task::spawn_blocking(move || Engine::open(root, Arc::new(SystemClock))).await {
            Ok(Ok(engine)) => {
                worker.engine = Some(engine);
                worker.publish();
                worker.run().await;
            }
            Ok(Err(error)) => tracing::warn!(%error, "Obsidian vault source is unavailable"),
            Err(error) => tracing::warn!(%error, "Obsidian vault source is unavailable"),
        }
    });
    (handle, Some(task))
}

fn spawnable<T: Transport>(
    engine: Option<Engine>,
    transport: T,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
) -> (ObsidianHandle, Worker<T>) {
    let (commands, receiver) = mpsc::channel(8);
    let initial = engine
        .as_ref()
        .map_or_else(off_status, |engine| engine.status(false, None));
    let status = Arc::new(Mutex::new(initial));
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_notify = Arc::new(tokio::sync::Notify::new());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_notify = Arc::new(tokio::sync::Notify::new());
    let handle = ObsidianHandle {
        inner: Some(Arc::new(HandleInner {
            commands,
            status: Arc::clone(&status),
            cancel: Arc::clone(&cancel),
            cancel_notify: Arc::clone(&cancel_notify),
            stop: Arc::clone(&stop),
            stop_notify: Arc::clone(&stop_notify),
        })),
    };
    let worker = Worker {
        engine,
        transport,
        status,
        paused,
        commands: receiver,
        cancel,
        cancel_notify,
        stop,
        stop_notify,
        next_scan: Instant::now(),
        next_vault_check: Instant::now(),
        capability: None,
        capability_checked: None,
    };
    (handle, worker)
}

struct Worker<T: Transport> {
    engine: Option<Engine>,
    transport: T,
    status: Arc<Mutex<Status>>,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
    commands: mpsc::Receiver<Command>,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    stop_notify: Arc<tokio::sync::Notify>,
    next_scan: Instant,
    next_vault_check: Instant,
    capability: Option<bool>,
    capability_checked: Option<Instant>,
}

impl<T: Transport> Worker<T> {
    /// Run the engine off the async runtime.
    async fn with_engine<R: Send + 'static>(
        &mut self,
        call: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Option<R> {
        let mut engine = self.engine.take()?;
        let result = tokio::task::spawn_blocking(move || {
            let result = call(&mut engine);
            (engine, result)
        })
        .await;
        match result {
            Ok((engine, result)) => {
                self.engine = Some(engine);
                Some(result)
            }
            Err(error) => {
                tracing::error!(%error, "Obsidian vault source stopped");
                None
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused.borrow().paused
    }

    async fn run(mut self) {
        loop {
            if self.stop.load(Ordering::Acquire) || self.engine.is_none() {
                break;
            }
            while let Ok(command) = self.commands.try_recv() {
                self.handle(command).await;
            }
            let busy = self.step().await;
            self.publish();
            if self.stop.load(Ordering::Acquire) || self.engine.is_none() {
                break;
            }
            let wait = if busy { Duration::ZERO } else { TICK };
            tokio::select! {
                () = self.stop_notify.notified() => break,
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    self.handle(command).await;
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
        if let Some(mut engine) = self.engine.take() {
            let _ = tokio::task::spawn_blocking(move || engine.flush()).await;
        }
    }

    fn publish(&self) {
        let Some(engine) = &self.engine else { return };
        let status = engine.status(self.paused(), self.capability);
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = status;
    }

    fn document(&self, refusal: Option<Refusal>) -> ObsidianDocument {
        self.publish();
        ObsidianDocument::new(
            self.status
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            refusal,
        )
    }

    async fn handle(&mut self, command: Command) {
        self.cancel.store(false, Ordering::Release);
        match command {
            Command::TurnOn { path, reply } => {
                let result = self
                    .with_engine(move |engine| engine.turn_on(std::path::Path::new(&path)))
                    .await;
                let refusal = match result {
                    Some(Err(solstone_vault::engine::TurnOnError::Refused(refusal))) => {
                        Some(refusal)
                    }
                    Some(Err(solstone_vault::engine::TurnOnError::Io(error))) => {
                        tracing::warn!(%error, "Couldn't turn on the Obsidian source");
                        None
                    }
                    _ => None,
                };
                if refusal.is_none() {
                    self.next_scan = Instant::now();
                    self.next_vault_check = Instant::now();
                    self.capability = None;
                    self.capability_checked = None;
                }
                let _ = reply.send(self.document(refusal));
            }
            Command::TurnOff(reply) => {
                let _ = self.with_engine(|engine| engine.turn_off()).await;
                let _ = reply.send(self.document(None));
            }
            Command::Continue(reply) => {
                let _ = self.with_engine(|engine| engine.continue_warning()).await;
                self.next_scan = Instant::now();
                let _ = reply.send(self.document(None));
            }
            Command::Cancel(reply) => {
                let _ = self.with_engine(|engine| engine.cancel_warning()).await;
                let _ = reply.send(self.document(None));
            }
        }
    }

    /// One turn of the source. Returns whether there's more to do right away.
    async fn step(&mut self) -> bool {
        if self.engine.as_ref().is_none_or(|e| !e.is_on()) {
            return false;
        }
        if self.engine.as_ref().and_then(Engine::phase) == Some(&Phase::Checking) {
            return self.check_vault_list().await;
        }
        if !self.engine.as_ref().is_some_and(Engine::should_scan) {
            return false;
        }
        let mut busy = false;
        if Instant::now() >= self.next_scan {
            let cancel = Arc::clone(&self.cancel);
            let outcome = self.with_engine(move |engine| engine.scan(&cancel)).await;
            match outcome {
                Some(Ok(ScanOutcome::Cancelled)) => self.cancel.store(false, Ordering::Release),
                Some(Err(error)) => tracing::warn!(%error, "Obsidian scan failed"),
                _ => {}
            }
            let duration = self
                .engine
                .as_ref()
                .map_or(Duration::ZERO, Engine::last_scan_duration);
            self.next_scan = Instant::now() + solstone_vault::next_scan_delay(duration);
        }
        if !self.paused() {
            let closed = self
                .with_engine(|engine| engine.close_due_windows(CLOSE_BATCH))
                .await;
            match closed {
                Some(Ok(count)) if count == CLOSE_BATCH => busy = true,
                Some(Err(error)) => tracing::warn!(%error, "Obsidian window close failed"),
                _ => {}
            }
            if self.journal_supports_notes().await {
                busy |= self.deliver_one().await;
            }
        }
        busy
    }

    async fn check_vault_list(&mut self) -> bool {
        if Instant::now() < self.next_vault_check {
            return false;
        }
        self.next_vault_check = Instant::now() + VAULT_CHECK_RETRY;
        if let VaultList::Listed(listed) = self.transport.vault_list().await {
            if let Some(Err(error)) = self
                .with_engine(move |engine| engine.apply_vault_list(&listed))
                .await
            {
                tracing::warn!(%error, "Couldn't save the Obsidian vault check");
            }
            self.next_scan = Instant::now();
            return true;
        }
        false
    }

    /// Whether the paired journal takes note changes, re-asking now and then or
    /// when the pairing has changed.
    async fn journal_supports_notes(&mut self) -> bool {
        let stale = match self.capability_checked {
            None => true,
            Some(_) if !self.transport.pairing_unchanged() => true,
            Some(checked) => {
                let ttl = if self.capability == Some(true) {
                    CAPABILITY_TTL
                } else {
                    CAPABILITY_RETRY
                };
                checked.elapsed() >= ttl
            }
        };
        if stale {
            self.capability = match self.transport.capability().await {
                Capability::Supported => Some(true),
                Capability::NotListed => Some(false),
                Capability::Unknown => None,
            };
            self.capability_checked = Some(Instant::now());
        }
        self.capability == Some(true)
    }

    /// Deliver one file if the journal allows it. True if one went.
    async fn deliver_one(&mut self) -> bool {
        if self.cancel.load(Ordering::Acquire) {
            return false;
        }
        let Some(delivery) = self.engine.as_ref().and_then(Engine::next_delivery) else {
            return false;
        };
        // Turning off doesn't wait on the journal. The file stays in the outbox, and
        // if the journal did take it, sending it again answers `duplicate`.
        let answer = tokio::select! {
            answer = self.transport.deliver(&delivery) => answer,
            () = self.cancel_notify.notified() => return false,
        };
        let Some(answer) = answer else {
            return false;
        };
        let recorded = self
            .with_engine(move |engine| engine.record_answer(&delivery, answer))
            .await;
        if let Some(Err(error)) = recorded {
            tracing::warn!(%error, "Couldn't record an Obsidian delivery");
        }
        matches!(answer, Answer::Delivered | Answer::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::{Mode, StateSnapshot};
    use solstone_vault::Clock;
    use std::{collections::VecDeque, fs, path::Path, sync::atomic::AtomicI64};

    const T0: i64 = 1_794_155_400_000;

    #[derive(Clone)]
    struct TestClock(Arc<AtomicI64>);
    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
        fn offset_s_at(&self, _ms: i64) -> i32 {
            -7 * 3600
        }
    }

    #[derive(Default)]
    struct FakeJournal {
        capability: Mutex<VecDeque<Capability>>,
        current_capability: Mutex<Option<Capability>>,
        vaults: Mutex<Option<VaultList>>,
        answers: Mutex<VecDeque<Answer>>,
        delivered: Mutex<Vec<Delivery>>,
        capability_calls: Mutex<u32>,
        pairing_changed: AtomicBool,
        /// Runs as an upload starts, which then never finishes.
        hang_after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl Transport for Arc<FakeJournal> {
        fn capability(&self) -> BoxFuture<'_, Capability> {
            Box::pin(async move {
                *self.capability_calls.lock().unwrap() += 1;
                let next = self.capability.lock().unwrap().pop_front();
                if let Some(next) = next {
                    *self.current_capability.lock().unwrap() = Some(next);
                }
                self.current_capability
                    .lock()
                    .unwrap()
                    .unwrap_or(Capability::Supported)
            })
        }
        fn vault_list(&self) -> BoxFuture<'_, VaultList> {
            Box::pin(async move {
                self.vaults
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or(VaultList::Listed(Vec::new()))
            })
        }
        fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, Option<Answer>> {
            Box::pin(async move {
                self.delivered.lock().unwrap().push(delivery.clone());
                let hang = self.hang_after.lock().unwrap().take();
                if let Some(started) = hang {
                    started();
                    std::future::pending::<()>().await;
                }
                Some(
                    self.answers
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or(Answer::Delivered),
                )
            })
        }
        fn pairing_unchanged(&self) -> bool {
            !self.pairing_changed.swap(false, Ordering::AcqRel)
        }
    }

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
            session: Default::default(),
            activity_through_pause: false,
            away: false,
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        vault: PathBuf,
        journal: Arc<FakeJournal>,
        worker: Worker<Arc<FakeJournal>>,
        handle: ObsidianHandle,
        paused: watch::Sender<StateSnapshot>,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let clock = TestClock(Arc::new(AtomicI64::new(T0)));
        let engine = Engine::open(temp.path().join("state"), Arc::new(clock)).unwrap();
        let journal = Arc::new(FakeJournal::default());
        let (paused, paused_rx) = watch::channel(snapshot(false));
        let (handle, worker) = spawnable(Some(engine), journal.clone(), paused_rx);
        Fixture {
            _temp: temp,
            vault,
            journal,
            worker,
            handle,
            paused,
        }
    }

    fn write_note(vault: &Path, name: &str, text: &str, mtime_ms: i64) {
        let path = vault.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(mtime_ms as u64))
            .unwrap();
    }

    /// Drive the worker through a turn-on: the vault check, then the first scan.
    async fn turn_on(f: &mut Fixture) {
        let (reply, answer) = oneshot::channel();
        f.worker
            .handle(Command::TurnOn {
                path: f.vault.to_str().unwrap().to_owned(),
                reply,
            })
            .await;
        answer.await.unwrap();
        f.worker.step().await;
        f.worker.step().await;
    }

    async fn steps_until_quiet(f: &mut Fixture) {
        for _ in 0..50 {
            f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
            if !f.worker.step().await {
                return;
            }
        }
        panic!("the source never settled");
    }

    fn state(f: &Fixture) -> ObsidianDocument {
        f.worker.publish();
        decode_document(&f.handle.state_json()).unwrap()
    }

    #[tokio::test]
    async fn a_journal_without_the_capability_holds_and_delivers_later() {
        let mut f = fixture();
        *f.journal.current_capability.lock().unwrap() = Some(Capability::NotListed);
        write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
        turn_on(&mut f).await;
        // Past the capability retry.
        f.worker.step().await;
        assert!(
            f.journal.delivered.lock().unwrap().is_empty(),
            "nothing sent"
        );
        let doc = state(&f);
        assert_eq!(doc.status.state, State::WaitingForJournalUpdate);
        assert_eq!(doc.status.waiting, 1);

        // The journal is updated.
        *f.journal.current_capability.lock().unwrap() = Some(Capability::Supported);
        f.worker.capability_checked = None;
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
        assert_eq!(
            f.journal.delivered.lock().unwrap().len(),
            1,
            "delivered now"
        );
        assert_eq!(state(&f).status.state, State::On);
    }

    #[tokio::test]
    async fn pause_holds_windows_and_delivery_and_resume_delivers() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
        f.paused.send(snapshot(true)).unwrap();
        turn_on(&mut f).await;
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
        assert!(f.journal.delivered.lock().unwrap().is_empty());
        let doc = state(&f);
        assert_eq!(doc.status.state, State::Paused);
        assert_eq!(doc.status.waiting, 1, "scanned but held");

        f.paused.send(snapshot(false)).unwrap();
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_vault_listed_by_the_journal_sends_nothing_until_the_owner_continues() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
        *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
            vault: "Notes".into(),
            device: Some("studio".into()),
            last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
        }]));
        turn_on(&mut f).await;
        steps_until_quiet(&mut f).await;
        assert!(f.journal.delivered.lock().unwrap().is_empty());
        let doc = state(&f);
        assert_eq!(doc.status.state, State::Confirm);
        assert_eq!(
            doc.status.warning.unwrap().device.as_deref(),
            Some("studio")
        );

        let (reply, _answer) = oneshot::channel();
        f.worker.handle(Command::Continue(reply)).await;
        f.worker.step().await;
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1);
        assert_eq!(state(&f).status.state, State::On);
    }

    #[tokio::test]
    async fn cancelling_the_warning_leaves_the_source_off() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
        *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
            vault: "Notes".into(),
            device: None,
            last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
        }]));
        turn_on(&mut f).await;
        steps_until_quiet(&mut f).await;
        let (reply, answer) = oneshot::channel();
        f.worker.handle(Command::Cancel(reply)).await;
        assert_eq!(answer.await.unwrap().status.state, State::Off);
        assert!(f.journal.delivered.lock().unwrap().is_empty());
        assert_eq!(state(&f).status.waiting, 0);
    }

    #[tokio::test]
    async fn a_new_pairing_re_asks_the_journal_before_sending() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
        turn_on(&mut f).await;
        steps_until_quiet(&mut f).await;
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1);
        let asks = *f.journal.capability_calls.lock().unwrap();

        // A new file waits, and the pairing changed to a journal that doesn't take notes.
        write_note(&f.vault, "b.md", "bbb", T0 - 3_600_000);
        f.worker.next_scan = Instant::now();
        f.journal.pairing_changed.store(true, Ordering::Release);
        *f.journal.current_capability.lock().unwrap() = Some(Capability::NotListed);
        f.worker.step().await;
        assert_eq!(
            *f.journal.capability_calls.lock().unwrap(),
            asks + 1,
            "the new pairing is asked first"
        );
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1, "nothing went");
        assert_eq!(state(&f).status.state, State::WaitingForJournalUpdate);
    }

    #[tokio::test]
    async fn turning_off_does_not_wait_on_an_upload() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
        let cancel = Arc::clone(&f.worker.cancel);
        let notify = Arc::clone(&f.worker.cancel_notify);
        *f.journal.hang_after.lock().unwrap() = Some(Box::new(move || {
            cancel.store(true, Ordering::Release);
            notify.notify_one();
        }));
        // Reaches the upload, which hangs until the cancel wins the race.
        turn_on(&mut f).await;
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1, "it started");
        assert_eq!(state(&f).status.waiting, 1, "and the file is still held");

        // Cancelled before an upload starts, nothing is attempted.
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
        assert_eq!(f.journal.delivered.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_folder_is_reported_and_leaves_the_source_off() {
        let mut f = fixture();
        let plain = f.vault.parent().unwrap().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let (reply, answer) = oneshot::channel();
        f.worker
            .handle(Command::TurnOn {
                path: plain.to_str().unwrap().to_owned(),
                reply,
            })
            .await;
        let doc = answer.await.unwrap();
        assert_eq!(doc.refusal, Some(Refusal::NotAVault));
        assert_eq!(doc.status.state, State::Off);
    }

    #[tokio::test]
    async fn the_answers_table_is_honoured_end_to_end() {
        let mut f = fixture();
        write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
        write_note(&f.vault, "b.md", "bbb", T0 - 3_600_000);
        // a.md's file: invalid, set aside. b.md's: segment_removed, so delivered.
        f.journal
            .answers
            .lock()
            .unwrap()
            .extend([Answer::Invalid, Answer::Delivered]);
        turn_on(&mut f).await;
        steps_until_quiet(&mut f).await;
        let delivered = f.journal.delivered.lock().unwrap().clone();
        assert_eq!(delivered.len(), 2, "the bad file doesn't stop the rest");
        let doc = state(&f);
        assert_eq!(doc.status.state, State::NeedsAttention);
        assert_eq!(doc.status.waiting, 0);
    }

    #[test]
    fn upload_results_map_onto_answers() {
        let base = UploadResult {
            success: false,
            error_type: None,
            status_code: Some(409),
            stored_key: None,
            file_descriptors: None,
            reason_code: None,
            is_local_failure: false,
            captured_identity_key: None,
            captured_pairing_id: None,
            association_epoch: None,
            stale_capability: false,
        };
        assert_eq!(
            answer_for(&UploadResult {
                success: true,
                ..base.clone()
            }),
            Answer::Delivered
        );
        assert_eq!(
            answer_for(&UploadResult {
                reason_code: Some("obsidian_note_ahead".into()),
                ..base.clone()
            }),
            Answer::Ahead
        );
        assert_eq!(
            answer_for(&UploadResult {
                reason_code: Some("obsidian_note_invalid".into()),
                ..base.clone()
            }),
            Answer::Invalid
        );
        assert_eq!(
            answer_for(&UploadResult {
                reason_code: Some("segment_removed".into()),
                ..base.clone()
            }),
            Answer::Delivered
        );
        assert_eq!(answer_for(&base), Answer::Retry);
        // Even a clean success from a journal no longer paired is not an answer.
        assert_eq!(
            answer_for(&UploadResult {
                success: true,
                stale_capability: true,
                ..base
            }),
            Answer::Retry
        );
    }

    #[test]
    fn the_status_document_and_vault_list_parse() {
        assert_eq!(
            crate::private_link::capabilities_from_status(&serde_json::json!({
                "capabilities": ["calendar_notes", "obsidian_notes", 7]
            })),
            vec!["calendar_notes", "obsidian_notes"]
        );
        assert!(crate::private_link::capabilities_from_status(&serde_json::json!({})).is_empty());
        let list =
            parse_vault_list(br#"{"vaults":[{"vault":"Notes","device":null,"last_synced_at":"2026-11-08T09:44:33-07:00"}]}"#)
                .unwrap();
        assert_eq!(list[0].device, None);
        assert!(parse_vault_list(b"nope").is_none());
    }

    /// The real upload path: the envelope carries the source, the file part is named
    /// by its hash, and the journal's refusal reasons come through.
    #[cfg(feature = "full-tests")]
    mod over_a_real_link {
        use super::*;
        use crate::config::Config;

        #[tokio::test]
        async fn uploads_go_as_the_obsidian_source_and_refusals_keep_their_reason() {
            let temp = tempfile::tempdir().unwrap();
            let server = crate::test_support::LinkedMockServer::new(Vec::new()).await;
            let config = Config {
                sync_max_retries: 1,
                sync_retry_delays: vec![0],
                base_dir: temp.path().to_path_buf(),
                config_dir: temp.path().join("config"),
                ..Config::default()
            };
            let clock = Arc::new(crate::run::SystemClock::new());
            let client = Arc::new(UploadClient::new(&config, server.capability(), clock));

            let file = temp
                .path()
                .join(format!("obsidian_{}.jsonl", "a".repeat(64)));
            fs::write(&file, b"{}\n").unwrap();
            let delivery = Delivery {
                window: solstone_vault::Window {
                    day: "20261108".into(),
                    segment: "094000_300".into(),
                },
                name: file.file_name().unwrap().to_string_lossy().into_owned(),
                path: file.clone(),
                lines: 1,
                first_sight: 1,
            };
            let transport = LinkTransport {
                client,
                asked: Mutex::new(None),
            };

            // The journal's own answers, which name its transcript rather than this
            // file, or nothing at all on a resend.
            server.enqueue_response(
                200,
                r#"{"status":"duplicate","existing_segment":"094000_300","message":"Nothing new in this file","file_descriptors":[],"meta":{}}"#,
            );
            assert_eq!(transport.deliver(&delivery).await, Some(Answer::Delivered));
            server.enqueue_response(
                200,
                r#"{"status":"ok","segment":"094000_300","files":["note_transcript.md"],"bytes":60,"file_descriptors":[{"submitted":"note_transcript.md","written":"note_transcript.md","size":60,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","disposition":"written"}],"meta":{}}"#,
            );
            assert_eq!(transport.deliver(&delivery).await, Some(Answer::Delivered));
            let requests = server.requests();
            let upload = requests.last().unwrap();
            assert_eq!(upload.uri, "/app/devices/ingest");
            let body = String::from_utf8_lossy(&upload.body);
            assert!(body.contains("\"source\":\"obsidian\""), "{body}");
            assert!(body.contains("\"day\":\"20261108\""), "{body}");
            assert!(body.contains("\"segment\":\"094000_300\""), "{body}");
            assert!(
                body.contains(&format!("obsidian_{}.jsonl", "a".repeat(64))),
                "{body}"
            );

            server.enqueue_response(
                400,
                serde_json::json!({"reason_code": INVALID_REASON}).to_string(),
            );
            assert_eq!(transport.deliver(&delivery).await, Some(Answer::Invalid));

            server.enqueue_response(
                409,
                serde_json::json!({"reason_code": "obsidian_note_ahead"}).to_string(),
            );
            assert_eq!(transport.deliver(&delivery).await, Some(Answer::Ahead));

            server.enqueue_response(
                500,
                serde_json::json!({"status": "failed", "reason_code": "segment_removed"})
                    .to_string(),
            );
            assert_eq!(transport.deliver(&delivery).await, Some(Answer::Delivered));

            server.enqueue_response(404, "");
            assert!(matches!(
                transport.vault_list().await,
                VaultList::Listed(list) if list.is_empty()
            ));
            server.enqueue_response(
                200,
                serde_json::json!({"vaults": [{"vault": "Notes", "device": "studio", "last_synced_at": "2026-11-08T09:44:33-07:00"}]})
                    .to_string(),
            );
            assert!(matches!(
                transport.vault_list().await,
                VaultList::Listed(list) if list.len() == 1 && list[0].vault == "Notes"
            ));

            // `/api/system/status` is answered by the peer's fixture route.
            assert_eq!(transport.capability().await, Capability::NotListed);
        }
    }
}
