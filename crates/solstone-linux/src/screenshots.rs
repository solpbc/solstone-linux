// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The screenshots source on Linux: the screenshots the owner takes, from the
//! folder they save to, each sent to their journal at the minute it was taken.
//! Only an image the desktop's own screenshot tool marked comes in (`proof`).

mod engine;
mod folder;
mod proof;

pub use engine::{Mode, Span};

use crate::upload::{CapturedAssociation, UploadClient, UploadResult};
use engine::{Delivery, Engine, Outcome, TakenFrom};
use serde::{Deserialize, Serialize};
use solstone_vault::SystemClock;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, watch};

pub const SOURCE: &str = "screenshots";
pub const CAPABILITY: &str = "screenshots";
/// The window binary converts a type the journal refuses. It exits with this
/// when the file can't be decoded.
pub const CONVERT_ARG: &str = "--convert-screenshot";
const LOOK_EVERY: Duration = Duration::from_secs(5);
const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);
const CAPABILITY_RETRY: Duration = Duration::from_secs(60);
const CONVERT_DEADLINE: Duration = Duration::from_secs(120);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// What the app says about the source to its window and its CLI.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScreenshotsDocument {
    pub v: u32,
    pub mode: Mode,
    /// It was on before, so the chooser says S17.
    pub returning: bool,
    /// The folder's full path (S22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The folder as S4 shows it: `Pictures › Screenshots`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_label: Option<String>,
    /// The folder's own name, the last part of the label (D3, D9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    /// The owner chose it, rather than the app finding it.
    pub folder_chosen: bool,
    /// It was there, and isn't now.
    pub folder_gone: bool,
    /// The chooser's frozen count, for this span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Count>,
    pub count_failed: bool,
    /// `[done, total]` while what the owner chose is still going in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<[u64; 2]>,
    pub waiting: u64,
    pub cant_come_in: u64,
    pub cant_read: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_ms: Option<i64>,
    /// Whether the paired journal lists the capability; `None` before it's known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_lists: Option<bool>,
    /// The computer is paused.
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Count {
    pub span: Span,
    pub n: u64,
}

impl ScreenshotsDocument {
    fn encoded(&self) -> String {
        serde_json::to_string(self).expect("a status document always serializes")
    }
}

pub fn decode_document(text: &str) -> Option<ScreenshotsDocument> {
    serde_json::from_str(text)
        .ok()
        .filter(|doc: &ScreenshotsDocument| doc.v == 1)
}

/// The state word the tile and page show.
#[must_use]
pub fn word(doc: &ScreenshotsDocument) -> crate::shell_model::StateWord {
    use crate::shell_model::StateWord;
    match doc.mode {
        Mode::NotSetUp | Mode::Choosing => StateWord::ReadyToSetUp,
        Mode::Off => StateWord::Off,
        Mode::On if doc.folder_gone => StateWord::NeedsAttention,
        Mode::On if doc.paused => StateWord::Paused,
        Mode::On => StateWord::On,
    }
}

/// The tile's sub-line. `None` where the shared line (off) or nothing shows.
#[must_use]
pub fn tile_line(doc: &ScreenshotsDocument) -> Option<String> {
    use crate::copy::screenshots as copy;
    match doc.mode {
        Mode::NotSetUp | Mode::Choosing => doc.folder.is_some().then(|| copy::S11.to_owned()),
        Mode::Off => None,
        Mode::On if doc.folder_gone => Some(copy::d9(doc.folder_name.as_deref()?)),
        Mode::On if doc.paused => Some(copy::S16_COMPUTER.to_owned()),
        Mode::On => match doc.progress {
            Some([done, total]) => Some(copy::s5(&group(done), &group(total))),
            None => doc.folder_label.as_deref().map(copy::s4),
        },
    }
}

/// A count with the locale's grouping (`2,618`).
#[must_use]
pub fn group(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The folder as the page and tile name it: relative to home, with ` › `.
fn folder_label(folder: &Path, home: &Path) -> String {
    let shown = folder.strip_prefix(home).unwrap_or(folder);
    let parts = shown
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        folder.display().to_string()
    } else {
        parts.join(" › ")
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether the paired journal has said it takes screenshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Capability {
    Supported,
    NotListed,
    Unknown,
}

/// The journal, as far as the source needs it.
pub(crate) trait Transport: Send + Sync + 'static {
    fn capability(&self) -> BoxFuture<'_, Capability>;
    /// Send one file. `None` means no answer to act on.
    fn deliver<'a>(&'a self, upload: &'a Upload) -> BoxFuture<'a, Option<Outcome>>;
    fn pairing_unchanged(&self) -> bool;
}

/// One screenshot, staged for sending.
pub(crate) struct Upload {
    pub day: String,
    pub segment: String,
    pub file: PathBuf,
    pub meta: crate::upload::ScreenshotMeta,
}

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

    fn deliver<'a>(&'a self, upload: &'a Upload) -> BoxFuture<'a, Option<Outcome>> {
        Box::pin(async move {
            if !self.client.has_capability() || self.client.is_revoked() {
                return None;
            }
            let result = self
                .client
                .upload_screenshot(&upload.day, &upload.segment, &upload.file, &upload.meta)
                .await;
            Some(outcome_for(&result))
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

/// What the contract says to do with each answer.
pub(crate) fn outcome_for(result: &UploadResult) -> Outcome {
    if result.stale_capability {
        return Outcome::Hold;
    }
    if result.success {
        // `ok`, `collision` and `duplicate`: in the journal, never sent again.
        return Outcome::Delivered;
    }
    match (result.status_code, result.reason_code.as_deref()) {
        (_, Some("screenshot_invalid" | "multipart_part_too_large")) => Outcome::CantComeIn,
        // A refused upload; and the 64 MiB part limit, checked before sending.
        (Some(400 | 413), _) => Outcome::CantComeIn,
        // `screenshot_ahead`, `retryable`, `failed`, and the shared pairing answers.
        _ => Outcome::Hold,
    }
}

enum Command {
    TurnOn,
    TurnOff,
    ChooseFolder(PathBuf),
    Count(Span),
    Confirm(Span),
    Retry,
}

/// The running source, as the rest of the app talks to it.
#[derive(Clone, Default)]
pub struct ScreenshotsHandle {
    inner: Option<Arc<HandleInner>>,
}

struct HandleInner {
    commands: mpsc::Sender<(Command, oneshot::Sender<ScreenshotsDocument>)>,
    status: Arc<Mutex<ScreenshotsDocument>>,
    restart: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    stop: Arc<tokio::sync::Notify>,
}

impl ScreenshotsHandle {
    pub fn state_json(&self) -> String {
        self.inner
            .as_ref()
            .map(|inner| {
                inner
                    .status
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            })
            .unwrap_or_else(|| ScreenshotsDocument {
                v: 1,
                ..ScreenshotsDocument::default()
            })
            .encoded()
    }

    async fn call(&self, command: Command) -> String {
        let Some(inner) = &self.inner else {
            return self.state_json();
        };
        let (reply, answer) = oneshot::channel();
        if inner.commands.send((command, reply)).await.is_err() {
            return self.state_json();
        }
        match answer.await {
            Ok(document) => document.encoded(),
            Err(_) => self.state_json(),
        }
    }

    pub async fn turn_on(&self) -> String {
        self.call(Command::TurnOn).await
    }

    pub async fn turn_off(&self) -> String {
        self.call(Command::TurnOff).await
    }

    pub async fn choose_folder(&self, path: String) -> String {
        self.call(Command::ChooseFolder(PathBuf::from(path))).await
    }

    /// Count for the chooser. An unknown span counts the preselected one.
    pub async fn count(&self, span: String) -> String {
        self.call(Command::Count(
            Span::parse(&span).unwrap_or(Span::Last30Days),
        ))
        .await
    }

    pub async fn confirm(&self, span: String) -> String {
        match Span::parse(&span) {
            Some(span) => self.call(Command::Confirm(span)).await,
            None => self.state_json(),
        }
    }

    pub async fn retry(&self) -> String {
        self.call(Command::Retry).await
    }

    /// The owner paired a journal: ask how far back again. Returns at once.
    pub fn restart_setup(&self) {
        let Some(inner) = &self.inner else { return };
        inner
            .restart
            .store(true, std::sync::atomic::Ordering::Release);
        inner.wake.notify_one();
    }

    pub(crate) async fn shutdown(&self, task: tokio::task::JoinHandle<()>) {
        let Some(inner) = &self.inner else { return };
        inner.stop.notify_one();
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, task).await;
    }
}

/// Start the source's background task.
pub(crate) fn start(
    runtime: &tokio::runtime::Runtime,
    dir: PathBuf,
    client: Arc<UploadClient>,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
    state_dir: PathBuf,
) -> (ScreenshotsHandle, Option<tokio::task::JoinHandle<()>>) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return (ScreenshotsHandle::default(), None);
    };
    let transport = LinkTransport {
        client,
        asked: Mutex::new(None),
    };
    let places = folder::Places::from_env(home);
    let mut engine = Engine::open(dir.clone(), places, Arc::new(SystemClock));
    engine.set_intake_hook(Arc::new(move |id| {
        crate::source_intake::add_screenshot(&state_dir, id);
    }));
    let (handle, worker) = spawnable(
        engine,
        transport,
        paused,
        dir.join("outbox"),
        Converter::Window,
    );
    let task = runtime.spawn(worker.run());
    (handle, Some(task))
}

/// How a type the journal refuses becomes a PNG.
#[derive(Clone)]
pub(crate) enum Converter {
    /// The window binary beside this one, which carries the desktop's imaging.
    Window,
    #[cfg(test)]
    Fixed(Result<Vec<u8>, ()>),
}

impl Converter {
    fn convert(&self, source: &Path, target: &Path) -> Result<(), ()> {
        match self {
            Self::Window => {
                let window = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join("solstone-linux-window")))
                    .ok_or(())?;
                let mut command = std::process::Command::new(window);
                command.arg(CONVERT_ARG).arg(source).arg(target);
                match crate::bounded_process::run(&mut command, Some(CONVERT_DEADLINE), 16 * 1024) {
                    Ok(output) if output.status.success() && target.is_file() => Ok(()),
                    Ok(output) => {
                        tracing::warn!(
                            code = ?output.status.code(),
                            "A screenshot couldn't be converted"
                        );
                        Err(())
                    }
                    Err(error) => {
                        tracing::warn!(%error, "A screenshot couldn't be converted");
                        Err(())
                    }
                }
            }
            #[cfg(test)]
            Self::Fixed(result) => {
                let bytes = result.clone()?;
                std::fs::write(target, bytes).map_err(|_| ())
            }
        }
    }
}

fn spawnable<T: Transport>(
    engine: Engine,
    transport: T,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
    outbox: PathBuf,
    converter: Converter,
) -> (ScreenshotsHandle, Worker<T>) {
    let (commands, receiver) = mpsc::channel(8);
    let status = Arc::new(Mutex::new(ScreenshotsDocument {
        v: 1,
        ..ScreenshotsDocument::default()
    }));
    let restart = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wake = Arc::new(tokio::sync::Notify::new());
    let stop = Arc::new(tokio::sync::Notify::new());
    let handle = ScreenshotsHandle {
        inner: Some(Arc::new(HandleInner {
            commands,
            status: Arc::clone(&status),
            restart: Arc::clone(&restart),
            wake: Arc::clone(&wake),
            stop: Arc::clone(&stop),
        })),
    };
    let worker = Worker {
        engine: Some(engine),
        transport,
        status,
        paused,
        commands: receiver,
        restart,
        wake,
        stop,
        outbox,
        converter,
        capability: None,
        capability_checked: None,
        count_failed: false,
        next_look: Instant::now(),
    };
    (handle, worker)
}

struct Worker<T: Transport> {
    engine: Option<Engine>,
    transport: T,
    status: Arc<Mutex<ScreenshotsDocument>>,
    paused: watch::Receiver<crate::observer::StateSnapshot>,
    commands: mpsc::Receiver<(Command, oneshot::Sender<ScreenshotsDocument>)>,
    restart: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    stop: Arc<tokio::sync::Notify>,
    outbox: PathBuf,
    converter: Converter,
    capability: Option<bool>,
    capability_checked: Option<Instant>,
    count_failed: bool,
    next_look: Instant,
}

impl<T: Transport> Worker<T> {
    async fn with_engine<R: Send + 'static>(
        &mut self,
        call: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Option<R> {
        let mut engine = self.engine.take()?;
        match tokio::task::spawn_blocking(move || {
            let result = call(&mut engine);
            (engine, result)
        })
        .await
        {
            Ok((engine, result)) => {
                self.engine = Some(engine);
                Some(result)
            }
            Err(error) => {
                tracing::error!(%error, "The screenshots source stopped");
                None
            }
        }
    }

    fn paused(&self) -> bool {
        self.paused.borrow().paused
    }

    async fn run(mut self) {
        let paused = self.paused();
        self.with_engine(move |engine| engine.set_paused(paused))
            .await;
        self.publish();
        loop {
            if self.engine.is_none() {
                break;
            }
            if self
                .restart
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                self.with_engine(Engine::restart_setup).await;
                self.capability = None;
                self.capability_checked = None;
            }
            while let Ok((command, reply)) = self.commands.try_recv() {
                self.handle(command, reply).await;
            }
            let busy = self.step().await;
            self.publish();
            let wait = if busy {
                Duration::ZERO
            } else {
                self.next_look.saturating_duration_since(Instant::now())
            };
            tokio::select! {
                () = self.stop.notified() => break,
                () = self.wake.notified() => {}
                changed = self.paused.changed() => {
                    if changed.is_ok() {
                        let paused = self.paused();
                        self.with_engine(move |engine| engine.set_paused(paused)).await;
                    }
                }
                command = self.commands.recv() => {
                    let Some((command, reply)) = command else { break };
                    self.handle(command, reply).await;
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn document(&self) -> ScreenshotsDocument {
        let Some(engine) = &self.engine else {
            return ScreenshotsDocument {
                v: 1,
                ..ScreenshotsDocument::default()
            };
        };
        let ledger = engine.ledger();
        let folder = engine.folder();
        ScreenshotsDocument {
            v: 1,
            mode: ledger.mode,
            returning: ledger.returning,
            folder_label: folder
                .as_deref()
                .map(|folder| folder_label(folder, engine.home())),
            folder_name: folder
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned()),
            folder: folder.as_deref().map(|folder| folder.display().to_string()),
            folder_chosen: ledger.chosen_folder.is_some(),
            folder_gone: ledger.mode == Mode::On && engine.folder_gone(),
            count: engine
                .counted()
                .map(|(span, n)| Count { span, n: n as u64 }),
            count_failed: self.count_failed,
            progress: engine
                .progress()
                .map(|(done, total)| [done as u64, total as u64]),
            waiting: engine.waiting() as u64,
            cant_come_in: engine.cant_come_in() as u64,
            cant_read: engine.unreadable() as u64,
            last_checked_ms: ledger.last_checked_ms,
            journal_lists: self.capability,
            paused: self.paused(),
        }
    }

    fn publish(&self) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = self.document();
    }

    async fn handle(&mut self, command: Command, reply: oneshot::Sender<ScreenshotsDocument>) {
        let result = match command {
            Command::TurnOn => self.with_engine(Engine::turn_on).await,
            Command::TurnOff => self.with_engine(Engine::turn_off).await,
            Command::ChooseFolder(path) => {
                self.with_engine(move |engine| engine.choose_folder(path))
                    .await
            }
            Command::Count(span) => {
                let counted = self.with_engine(move |engine| engine.count(span)).await;
                self.count_failed = !matches!(counted, Some(Some(_)));
                Some(Ok(()))
            }
            Command::Confirm(span) => {
                self.count_failed = false;
                let confirmed = self.with_engine(move |engine| engine.confirm(span)).await;
                self.next_look = Instant::now();
                confirmed.map(|result| result.map(|_| ()))
            }
            Command::Retry => self.with_engine(Engine::retry).await,
        };
        if let Some(Err(error)) = result {
            tracing::warn!(%error, "Couldn't save the screenshots source");
        }
        self.publish();
        let _ = reply.send(self.document());
    }

    /// One turn. True if there's more to send right away.
    async fn step(&mut self) -> bool {
        if !self.engine.as_ref().is_some_and(Engine::is_on) {
            return false;
        }
        if Instant::now() >= self.next_look {
            self.next_look = Instant::now() + LOOK_EVERY;
            if let Some(Err(error)) = self.with_engine(Engine::tick).await {
                tracing::warn!(%error, "Couldn't save the screenshots source");
            }
        }
        if self.paused() || !self.journal_takes_screenshots().await {
            return false;
        }
        self.deliver_one().await
    }

    async fn journal_takes_screenshots(&mut self) -> bool {
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

    async fn deliver_one(&mut self) -> bool {
        let Some(delivery) = self.engine.as_ref().and_then(Engine::next_delivery) else {
            return false;
        };
        let outbox = self.outbox.clone();
        let converter = self.converter.clone();
        let staged = tokio::task::spawn_blocking(move || stage(&delivery, &outbox, &converter))
            .await
            .unwrap_or(Staged::Hold);
        let (id, outcome) = match staged {
            Staged::Ready(id, upload) => {
                let outcome = self.transport.deliver(&upload).await;
                let _ = std::fs::remove_file(&upload.file);
                match outcome {
                    Some(outcome) => (id, outcome),
                    None => return false,
                }
            }
            Staged::Done(id, outcome) => (id, outcome),
            Staged::Hold => return false,
        };
        let recorded = self
            .with_engine(move |engine| engine.record(&id, outcome))
            .await;
        if let Some(Err(error)) = recorded {
            tracing::warn!(%error, "Couldn't record a screenshot's delivery");
        }
        outcome != Outcome::Hold
    }
}

enum Staged {
    Ready(String, Upload),
    /// Settled without sending.
    Done(String, Outcome),
    Hold,
}

/// Put the screenshot in the outbox as the journal takes it: an accepted type
/// byte for byte, any other converted to PNG. The bytes are checked against what
/// was found, so a file changed since is not sent under its old identity.
fn stage(delivery: &Delivery, outbox: &Path, converter: &Converter) -> Staged {
    let id = delivery.id.clone();
    let Some(extension) = engine::extension_of(&delivery.path) else {
        return Staged::Done(id, Outcome::Gone);
    };
    let original = match std::fs::read(&delivery.path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Staged::Done(id, Outcome::Gone);
        }
        Err(_) => return Staged::Done(id, Outcome::Hold),
    };
    if engine::hash_bytes(&original) != delivery.id {
        return Staged::Done(id, Outcome::Gone);
    }
    if std::fs::create_dir_all(outbox).is_err() {
        return Staged::Hold;
    }
    let accepted = engine::ACCEPTED.contains(&extension.as_str());
    let file = outbox.join(format!(
        "screenshot_screen.{}",
        if accepted { extension.as_str() } else { "png" }
    ));
    let _ = std::fs::remove_file(&file);
    if accepted {
        if crate::private_file::atomic_write_bytes(&file, &original).is_err() {
            return Staged::Hold;
        }
    } else {
        let source = outbox.join(format!("original.{extension}"));
        if crate::private_file::atomic_write_bytes(&source, &original).is_err() {
            return Staged::Hold;
        }
        let converted = converter.convert(&source, &file);
        let _ = std::fs::remove_file(&source);
        if converted.is_err() {
            let _ = std::fs::remove_file(&file);
            return Staged::Done(id, Outcome::CantComeIn);
        }
    }
    let (day, segment, captured_at) = capture_moment(delivery.captured_ms, delivery.offset_s);
    Staged::Ready(
        id,
        Upload {
            day,
            segment,
            file,
            meta: crate::upload::ScreenshotMeta {
                v: 1,
                captured_at,
                taken_from: match delivery.taken_from {
                    TakenFrom::Platform => "platform",
                    TakenFrom::File => "file",
                },
            },
        },
    )
}

/// The envelope's day and `HHMMSS_1`, and `captured_at`, at the capture second in
/// its own offset.
pub(crate) fn capture_moment(captured_ms: i64, offset_s: i32) -> (String, String, String) {
    let offset = chrono::FixedOffset::east_opt(offset_s)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("zero offset"));
    let moment = chrono::DateTime::from_timestamp(captured_ms.div_euclid(1000), 0)
        .unwrap_or_default()
        .with_timezone(&offset);
    (
        moment.format("%Y%m%d").to_string(),
        moment.format("%H%M%S_1").to_string(),
        moment.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

#[cfg(test)]
mod tests;
