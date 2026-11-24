// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! File custody belongs to the service, independently of the window and source file.

use crate::private_file::{atomic_write_bytes, ensure_private_directory};
use crate::private_link::{LinkOutcome, PrivateLinkCapability};
use crate::upload::{CapturedAssociation, UploadClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use solstone_i18n::{message, text};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::io::ReaderStream;

const FILE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const BATCH: usize = 16;
const RECENT_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const RECENT_MAX_ITEMS: usize = 5;
/// The window binary converts a HEIC/HEIF photo into a JPEG with the desktop's
/// own gdk-pixbuf loaders, preserving EXIF metadata.
pub const CONVERT_PHOTO_ARG: &str = "--convert-photo";
const CONVERT_DEADLINE: Duration = Duration::from_secs(120);
pub fn explanation() -> &'static str {
    text!("linux.import.explanation")
}
pub fn explanation_no_drop() -> &'static str {
    text!("linux.import.explanationnodrop")
}
pub fn store_error() -> &'static str {
    text!("linux.import.storeerror")
}
pub fn step_error() -> &'static str {
    text!("linux.import.steperror")
}
pub fn remove_error() -> &'static str {
    text!("linux.import.removeerror")
}

/// Strict ASCII `YYYYMMDD_HHMMSS` import timestamp validation (never Unicode digits).
pub fn valid_import_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() == 15
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'_'
        && bytes[9..].iter().all(u8::is_ascii_digit)
}

fn is_confirmation_required_kind(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some("body_confirmation_required" | "confirmation_required")
    ) || kind.is_some_and(|k| k.ends_with("_confirmation_required"))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Copying,
    Pending,
    ReadyToReview,
    Refused,
    Delivered,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Saved {
    binding: String,
    path: String,
    timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Review {
    import_id: String,
    reason_code: String,
    kind: String,
    instance_id: String,
    #[serde(default)]
    expired: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Refusal {
    reason: String,
    reason_code: Option<String>,
    kind: Option<String>,
    binding: Option<String>,
    version: Option<String>,
    retryable: bool,
}

impl Refusal {
    fn applies_to(&self, current_binding: Option<&str>, current_version: Option<&str>) -> bool {
        if self.binding.is_none() {
            return true;
        }
        if self.binding.as_deref() != current_binding {
            return false;
        }
        if self.reason_code.as_deref() == Some("import_preview_required") {
            return true;
        }
        self.version.as_deref() == current_version
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Item {
    id: String,
    name: String,
    mime: String,
    size: u64,
    phase: Phase,
    may_have_saved: bool,
    saved: Option<Saved>,
    refusal: Option<Refusal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    review: Option<Review>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    handoff: Option<Handoff>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RecentReceipt {
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    pub instance_id: String,
    pub delivered_at_epoch_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RecentFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal: Option<String>,
    #[serde(default)]
    receipts: Vec<RecentReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Handoff {
    pub area: String,
    pub context: serde_json::Map<String, Value>,
}

enum PickedFile {
    Path(String),
    Open(String, File),
}

impl PickedFile {
    fn name(&self) -> &Path {
        match self {
            Self::Path(path) | Self::Open(path, _) => Path::new(path),
        }
    }
    fn open(self) -> io::Result<File> {
        match self {
            Self::Path(path) => regular_file(Path::new(&path)),
            Self::Open(_, file) if file.metadata()?.is_file() => Ok(file),
            Self::Open(..) => Err(ioe("not a regular file")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct ImportRow {
    pub id: String,
    pub name: String,
    pub state: String,
    pub reason: Option<String>,
    pub can_retry: bool,
    pub may_have_saved: bool,
    pub missing: bool,
    #[serde(default)]
    pub can_remove: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_kind: Option<String>,
    #[serde(default)]
    pub review_expired: bool,
    #[serde(default)]
    pub size_bytes: u64,
}

/// A file the app didn't take at intake. Nothing of it is kept on this device, so it is
/// named under the result of the addition instead of listed with what is.
#[derive(Clone, Debug, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct IntakeRefusal {
    pub name: String,
    pub reason: String,
}

impl IntakeRefusal {
    pub fn line(&self) -> String {
        message!("linux.import.namedrefusal", "name" => self.name, "reason" => self.reason)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoveConfirmation {
    pub token: String,
    pub row: ImportRow,
}

struct ConfirmationOwner {
    owner: String,
    token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct ImportDocument {
    pub v: u32,
    pub rows: Vec<ImportRow>,
    pub recent: String,
    pub addition: Option<String>,
    #[serde(default)]
    pub intake_refused: Vec<IntakeRefusal>,
    pub adding: bool,
    pub interrupted: bool,
    pub store_error: bool,
    pub waiting: u64,
    pub held: bool,
    pub sending: bool,
    #[serde(default)]
    pub ready_to_review: u64,
    #[serde(default)]
    pub needs_attention: u64,
    #[serde(default)]
    pub paired_confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_hold: Option<String>,
    #[serde(default)]
    pub recent_receipts: Vec<RecentReceipt>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImportSummary {
    pub waiting: u64,
    pub held: bool,
    pub sending: bool,
    pub recent: String,
    #[serde(default)]
    pub ready_to_review: u64,
    #[serde(default)]
    pub needs_attention: u64,
    #[serde(default)]
    pub store_error: bool,
}

impl From<ImportDocument> for ImportSummary {
    fn from(document: ImportDocument) -> Self {
        Self {
            waiting: document.waiting,
            held: document.held,
            sending: document.sending,
            recent: document.recent,
            ready_to_review: document.ready_to_review,
            needs_attention: document.needs_attention,
            store_error: document.store_error,
        }
    }
}

#[derive(Default)]
struct Store {
    loaded: bool,
    failed: bool,
    items: BTreeMap<String, Item>,
    busy: HashSet<String>,
    confirmations: BTreeMap<String, ConfirmationOwner>,
    saved_current: BTreeMap<String, CapturedAssociation>,
    observed: Option<CapturedAssociation>,
    version: Option<String>,
    delivered_journal: Option<String>,
    recent_receipts: Vec<RecentReceipt>,
    receipts: std::collections::VecDeque<(String, Option<String>)>,
    sent_bytes: BTreeMap<String, u64>,
    addition: Option<String>,
    intake_refused: Vec<IntakeRefusal>,
    interrupted: bool,
    cursor: String,
    auth_status: Option<u16>,
}

#[derive(Clone, Default)]
enum PhotoConverter {
    #[default]
    Window,
    #[cfg(all(test, feature = "full-tests"))]
    Fixed(Result<Vec<u8>, ()>),
}

impl PhotoConverter {
    fn convert(&self, source: &Path, target: &Path) -> Result<(), ()> {
        match self {
            Self::Window => {
                let window = crate::service::current_executable()
                    .ok()
                    .and_then(|exe| exe.parent().map(|dir| dir.join("solstone-linux-window")))
                    .ok_or(())?;
                let mut command = std::process::Command::new(window);
                command.arg(CONVERT_PHOTO_ARG).arg(source).arg(target);
                match crate::bounded_process::run(&mut command, Some(CONVERT_DEADLINE), 16 * 1024) {
                    Ok(output) if output.status.success() && target.is_file() => Ok(()),
                    Ok(output) => {
                        tracing::warn!(
                            code = ?output.status.code(),
                            "A photo couldn't be converted"
                        );
                        Err(())
                    }
                    Err(error) => {
                        tracing::warn!(%error, "A photo couldn't be converted");
                        Err(())
                    }
                }
            }
            #[cfg(all(test, feature = "full-tests"))]
            Self::Fixed(result) => {
                let bytes = result.clone()?;
                std::fs::write(target, bytes).map_err(|_| ())
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct ImportHandle {
    root: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    state: Arc<Mutex<Store>>,
    adding: Arc<AtomicBool>,
    client: Weak<UploadClient>,
    trigger: Option<crate::sync::SyncTrigger>,
    sleep: crate::sleep_inhibit::SleepInhibit,
    status_sink: Option<Arc<dyn Fn(ImportSummary) + Send + Sync>>,
    status_lock: Arc<Mutex<()>>,
    recent_lock: Arc<Mutex<()>>,
    photo_converter: PhotoConverter,
    #[cfg(all(test, feature = "full-tests"))]
    clock_ms: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

fn ioe(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn encode(value: &impl Serialize) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(ioe)
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn regular_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ioe("not a regular import file"));
    }
    Ok(file)
}

fn new_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn supports_import(version: Option<&str>) -> bool {
    let Some(version) = version else { return false };
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = version.split('.');
    let parsed = (|| {
        Some((
            parts.next()?.parse::<u64>().ok()?,
            parts.next()?.parse::<u64>().ok()?,
            parts.next()?.parse::<u64>().ok()?,
        ))
    })();
    parts.next().is_none() && parsed.is_some_and(|v| v >= (2, 0, 52))
}

pub fn file_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "text" | "md" | "csv" | "log" => "text/plain",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "aif" | "aiff" => "audio/aiff",
        _ => return None,
    })
}

pub fn is_convertible_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "heic" | "heif"))
}

pub fn converted_jpeg_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "photo".to_owned());
    format!("{stem}.jpg")
}

fn binding(capability: &PrivateLinkCapability) -> String {
    let writer = capability.writer();
    format!("{}:{}", writer.identity_key(), writer.pairing_id())
}

impl ImportHandle {
    pub(crate) fn with_status_sink(
        mut self,
        sink: Arc<dyn Fn(ImportSummary) + Send + Sync>,
    ) -> Self {
        self.status_sink = Some(sink);
        self
    }

    pub(crate) fn with_state_dir(mut self, state_dir: PathBuf) -> Self {
        self.state_dir = Some(state_dir);
        self
    }

    pub(crate) fn with_sleep_inhibit(mut self, sleep: crate::sleep_inhibit::SleepInhibit) -> Self {
        self.sleep = sleep;
        self
    }

    pub(crate) fn has_deliverable_items(&self) -> bool {
        if self.root.is_none() || self.sleep.is_sleeping() {
            return false;
        }
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        !state.failed
            && state.items.values().any(|item| {
                item.phase == Phase::Pending && !state.confirmations.contains_key(&item.id)
            })
    }

    #[cfg(all(test, feature = "full-tests"))]
    pub(crate) fn with_clock_ms(mut self, clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.clock_ms = Some(clock);
        self
    }

    fn now_ms(&self) -> i64 {
        #[cfg(all(test, feature = "full-tests"))]
        if let Some(clock) = &self.clock_ms {
            return clock();
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    pub(crate) fn has_review_items(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.items.values().any(|item| {
            item.phase == Phase::ReadyToReview
                && item
                    .review
                    .as_ref()
                    .is_some_and(|r| !r.expired && valid_import_id(&r.import_id))
        })
    }

    pub(crate) fn request_reconcile(&self) {
        if self.has_review_items()
            && let Some(trigger) = &self.trigger
        {
            trigger.trigger();
        }
    }

    pub(crate) fn publish_status(&self) {
        if let Some(sink) = &self.status_sink {
            let _order = self.status_lock.lock().unwrap_or_else(|p| p.into_inner());
            sink(self.document().into());
        }
    }
    pub(crate) fn new(
        root: PathBuf,
        client: &Arc<UploadClient>,
        trigger: crate::sync::SyncTrigger,
    ) -> Self {
        Self {
            root: Some(root),
            client: Arc::downgrade(client),
            trigger: Some(trigger),
            ..Self::default()
        }
    }

    fn persist(&self, item: &Item) -> io::Result<()> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        atomic_write_bytes(&root.join(&item.id).join("item.json"), &encode(item)?).map_err(ioe)
    }

    fn publish(&self, item: Item) -> io::Result<()> {
        self.persist(&item)?;
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .items
            .insert(item.id.clone(), item);
        self.publish_status();
        Ok(())
    }

    pub(crate) fn restore(&self) -> io::Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let now_ms = self.now_ms();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.loaded {
            return if state.failed {
                Err(ioe("import store unavailable"))
            } else {
                Ok(())
            };
        }
        let mut delivered = Vec::new();
        let mut retired = Vec::new();
        let result = (|| {
            ensure_private_directory(root).map_err(ioe)?;
            let recent = root.join("recent.json");
            if recent.exists() {
                let value: Value = serde_json::from_reader(regular_file(&recent)?).map_err(ioe)?;
                let parsed = parse_recent_file(value, now_ms)?;
                state.delivered_journal = parsed.journal;
                state.recent_receipts = parsed.receipts;
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let id = entry.file_name().to_string_lossy().into_owned();
                if id.strip_prefix(".removed-").is_some_and(valid_id) {
                    retired.push(entry.path());
                    continue;
                }
                if !valid_id(&id) {
                    continue;
                }
                if !entry.file_type()?.is_dir() {
                    return Err(ioe("invalid import directory"));
                }
                let mut item: Item = match regular_file(&entry.path().join("item.json"))
                    .and_then(|file| serde_json::from_reader(file).map_err(ioe))
                {
                    Ok(item) => item,
                    Err(_) => Item {
                        id: id.clone(),
                        name: id.clone(),
                        mime: String::new(),
                        size: 0,
                        phase: Phase::Refused,
                        may_have_saved: true,
                        saved: None,
                        handoff: None,
                        review: None,
                        refusal: Some(local_refusal("this couldn't be read. add it again")),
                    },
                };
                if item.id != id {
                    return Err(ioe("invalid import identity"));
                }
                if item.phase == Phase::Delivered {
                    // The receipt preceded cleanup; never make it pending again.
                    delivered.push(id);
                    continue;
                }
                if item.phase == Phase::Copying {
                    state.interrupted = true;
                    item.phase = Phase::Refused;
                    item.review = None;
                    item.refusal = Some(local_refusal("this couldn't be read. add it again"));
                    self.persist(&item)?;
                } else if item.phase == Phase::ReadyToReview
                    && item
                        .review
                        .as_ref()
                        .is_none_or(|r| !valid_import_id(&r.import_id) || r.instance_id.is_empty())
                {
                    item.phase = Phase::Refused;
                    item.review = None;
                    item.refusal = Some(local_refusal("this couldn't be read. add it again"));
                    self.persist(&item)?;
                } else if (item.phase == Phase::Pending
                    || item.phase == Phase::ReadyToReview
                    || item.may_have_saved)
                    && regular_file(&entry.path().join("payload")).is_err()
                {
                    item.phase = Phase::Refused;
                    item.review = None;
                    item.refusal = Some(local_refusal("the copy on this device is missing"));
                    self.persist(&item)?;
                }
                state.items.insert(id, item);
            }
            Ok(())
        })();
        state.loaded = true;
        state.failed = result.is_err();
        drop(state);
        if result.is_ok() {
            for id in delivered {
                let _ = self.cleanup(&id);
            }
            for path in retired {
                let _ = self.cleanup_retired(&path);
            }
        }
        self.publish_status();
        result
    }

    fn cleanup(&self, id: &str) -> io::Result<()> {
        if !valid_id(id) {
            return Err(ioe("invalid import identity"));
        }
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        let dir = root.join(id);
        let retired = root.join(format!(".removed-{id}"));
        match fs::rename(&dir, &retired) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        File::open(root)?.sync_all()?;
        self.cleanup_retired(&retired)?;
        File::open(root)?.sync_all()
    }

    fn cleanup_retired(&self, dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Err(ioe("invalid retired import directory"));
        }
        // Only our fixed files are removed. An unexpected entry is retained for recovery.
        for name in ["payload", "payload.part", "item.json"] {
            match fs::remove_file(dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        match fs::remove_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }

    fn wake(&self) {
        self.publish_status();
        if let Some(trigger) = &self.trigger {
            trigger.trigger();
        }
    }

    pub async fn add_files(&self, paths: Vec<String>) -> bool {
        let result = self
            .add_selected(paths.into_iter().map(PickedFile::Path).collect(), None)
            .await;
        matches!(result["outcome"].as_str(), Some("queued" | "refused"))
            && result["store_error"] != true
    }

    pub(crate) async fn add_open_files(
        &self,
        files: Vec<(String, File)>,
        handoff: Handoff,
    ) -> Value {
        self.add_selected(
            files
                .into_iter()
                .map(|(name, file)| PickedFile::Open(name, file))
                .collect(),
            Some(handoff),
        )
        .await
    }

    async fn add_selected(&self, files: Vec<PickedFile>, handoff: Option<Handoff>) -> Value {
        if self.root.is_none() {
            return json!({"outcome":"refused","reason":store_error(),"store_error":true});
        }
        if self.adding.swap(true, Ordering::AcqRel) {
            return json!({"outcome":"busy"});
        }
        let handle = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut accepted = Vec::new();
            let mut refused = Vec::new();
            let mut store_failed = false;
            let restored = handle.restore().is_ok();
            let total = files.len();
            handle
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .intake_refused
                .clear();
            for (index, file) in files.into_iter().enumerate() {
                let name = file
                    .name()
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                handle
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .addition =
                    Some(message!("linux.import.saving", "saved" => index + 1, "total" => total));
                let result = if restored {
                    handle.copy_one(file, handoff.clone())
                } else {
                    Err(ioe("store unavailable"))
                };
                match result {
                    Ok(Ok(item)) => {
                        accepted.push(json!({"item":item.id,"name":item.name,"size":item.size}))
                    }
                    Ok(Err(reason)) => refused.push(IntakeRefusal { name, reason }),
                    Err(_) => {
                        store_failed = true;
                        refused.push(IntakeRefusal {
                            name,
                            reason: store_error().to_owned(),
                        });
                    }
                }
            }
            {
                let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
                state.intake_refused.clone_from(&refused);
                state.addition = Some(if accepted.is_empty() {
                    text!("linux.import.nothingwassaved").to_owned()
                } else {
                    message!("linux.import.saved", "count" => accepted.len())
                });
            }
            handle.adding.store(false, Ordering::Release);
            handle.wake();
            json!({"outcome":if accepted.is_empty() {"refused"} else {"queued"},
                   "items":accepted,"refused":refused,"store_error":store_failed})
        })
        .await;
        result.unwrap_or_else(|_| {
            self.adding.store(false, Ordering::Release);
            json!({"outcome":"refused","reason":store_error(),"store_error":true})
        })
    }

    pub(crate) fn handoff_status(&self, id: &str) -> Value {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, row)) = state.receipts.iter().find(|(item, _)| item == id) {
            return json!({"state":"delivered","import_id":row});
        }
        let Some(item) = state.items.get(id) else {
            return json!({"state":"gone"});
        };
        if item.phase == Phase::ReadyToReview
            && let Some(review) = &item.review
        {
            if review.expired {
                return json!({
                    "state": "attention",
                    "review_required": false,
                    "review_expired": true,
                    "reason_code": review.reason_code,
                    "kind": review.kind,
                    "import_id": review.import_id,
                    "retryable": true,
                    "reason": text!("linux.import.reviewexpireddetail"),
                    "device_noun": "computer",
                    "size_bytes": item.size,
                });
            }
            return json!({
                "state": "attention",
                "review_required": true,
                "review_expired": false,
                "reason_code": review.reason_code,
                "kind": review.kind,
                "import_id": review.import_id,
                "retryable": false,
                "reason": text!("linux.import.reviewdetail"),
                "device_noun": "computer",
                "size_bytes": item.size,
            });
        }
        if let Some(refusal) = &item.refusal {
            return json!({
                "state": "attention",
                "review_required": false,
                "review_expired": false,
                "reason_code": refusal.reason_code,
                "kind": refusal.kind,
                "retryable": refusal.retryable,
                "reason": refusal.reason,
                "import_id": item.saved.as_ref().map(|saved| &saved.timestamp),
                "device_noun": "computer",
                "size_bytes": item.size,
            });
        }
        if state.busy.contains(id) {
            return json!({
                "state": "uploading",
                "bytes": state.sent_bytes.get(id).copied().unwrap_or(0),
                "size": item.size,
                "size_bytes": item.size,
                "device_noun": "computer",
            });
        }
        json!({
            "state": "waiting",
            "size_bytes": item.size,
            "device_noun": "computer",
        })
    }

    fn copy_one(
        &self,
        selected: PickedFile,
        handoff: Option<Handoff>,
    ) -> io::Result<Result<Item, String>> {
        let path_buf = selected.name().to_owned();
        let path = path_buf.as_path();
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        let id = new_id()?;
        let dir = root.join(&id);
        ensure_private_directory(&dir).map_err(ioe)?;
        let convertible = is_convertible_photo(path);
        let original_name = path
            .file_name()
            .map_or_else(|| "file".to_owned(), |s| s.to_string_lossy().into_owned());
        let mut item = Item {
            id,
            name: original_name.clone(),
            mime: if convertible {
                "image/jpeg".to_owned()
            } else {
                file_mime(path)
                    .unwrap_or("application/octet-stream")
                    .to_owned()
            },
            size: 0,
            phase: Phase::Copying,
            may_have_saved: false,
            saved: None,
            refusal: None,
            review: None,
            handoff,
        };
        self.publish(item.clone())?;
        let copied = (|| {
            let mut source = selected.open()?;
            let size = source.metadata()?.len();
            if size > FILE_LIMIT {
                return Err(ioe("too_large"));
            }
            if available_space(&dir)? < size.saturating_add(256 * 1024 * 1024) {
                return Err(io::Error::from_raw_os_error(28));
            }
            let copy_target = if convertible {
                dir.join("source.part")
            } else {
                dir.join("payload.part")
            };
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&copy_target)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                let n = source.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > FILE_LIMIT {
                    return Err(ioe("too_large"));
                }
                if copied % (1024 * 1024) < n as u64
                    && available_space(&dir)? < 256 * 1024 * 1024 + n as u64
                {
                    return Err(io::Error::from_raw_os_error(28));
                }
                out.write_all(&buffer[..n])?;
            }
            out.sync_all()?;
            drop(out);
            drop(source);

            let final_size = if convertible {
                let payload_part = dir.join("payload.part");
                let converted = self.photo_converter.convert(&copy_target, &payload_part);
                let _ = fs::remove_file(&copy_target);
                if converted.is_err() {
                    let _ = fs::remove_file(&payload_part);
                    return Err(ioe("cannot_convert"));
                }
                let converted_file = OpenOptions::new().read(true).open(&payload_part)?;
                let converted_size = converted_file.metadata()?.len();
                if converted_size > FILE_LIMIT {
                    let _ = fs::remove_file(&payload_part);
                    return Err(ioe("too_large"));
                }
                converted_file.sync_all()?;
                drop(converted_file);
                converted_size
            } else {
                copied
            };
            fs::rename(dir.join("payload.part"), dir.join("payload"))?;
            File::open(&dir)?.sync_all()?;
            Ok::<_, io::Error>(final_size)
        })();
        match copied {
            Ok(size) => {
                if convertible {
                    item.name = converted_jpeg_name(path);
                    item.mime = "image/jpeg".to_owned();
                }
                item.size = size;
                item.phase = Phase::Pending;
                self.publish(item.clone())?;
                if let Some(state_dir) = &self.state_dir {
                    crate::source_intake::add_import(state_dir, &item.id);
                }
                Ok(Ok(item))
            }
            Err(error) => {
                let reason = match error.to_string().as_str() {
                    "unsupported" => text!("linux.import.filekind"),
                    "cannot_convert" => text!("linux.import.cannotconvert"),
                    "too_large" => text!("linux.import.filetoolarge"),
                    _ if error.raw_os_error() == Some(28) => {
                        text!("linux.import.noroom")
                    }
                    _ => "this couldn't be read. add it again",
                };
                // Nothing was taken, so the pre-copy record goes the way a removal does:
                // a tombstone first, so a crash during cleanup never brings it back.
                let id = item.id.clone();
                item.phase = Phase::Delivered;
                self.publish(item)?;
                self.cleanup(&id)?;
                self.state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .items
                    .remove(&id);
                Ok(Err(reason.to_owned()))
            }
        }
    }

    pub fn document(&self) -> ImportDocument {
        if self.root.is_none() {
            return ImportDocument {
                v: 1,
                recent: text!("linux.import.nothingyet").to_owned(),
                ..ImportDocument::default()
            };
        }
        let now_ms = self.now_ms();
        let client = self.client.upgrade();
        let capability = client.as_ref().and_then(|c| c.capability());
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let current = client.as_ref().is_some_and(|c| {
            state
                .observed
                .as_ref()
                .is_some_and(|a| c.association_is_current(a))
        });
        let version = current.then_some(state.version.as_deref()).flatten();
        let supported = supports_import(version);
        let facts = capability.as_ref().map(|c| c.facts().snapshot());
        let ready = capability.is_some()
            && !facts.as_ref().is_some_and(|f| {
                f.journal_mark_held
                    || f.terminal_revocation
                    || f.private_state_invalid
                    || f.config_sanitation_failed
                    || f.pairing_required
            })
            && !client.as_ref().is_some_and(|c| c.is_revoked());
        let current_instance_id = capability
            .as_ref()
            .map(|c| c.writer().identity_key().to_owned());
        let mut document = ImportDocument {
            v: 1,
            addition: state.addition.clone(),
            adding: self.adding.load(Ordering::Acquire),
            interrupted: state.interrupted,
            store_error: state.failed,
            paired_confirmed: ready,
            ..ImportDocument::default()
        };
        if state.failed {
            document.held = true;
            document.interrupted = false;
            document.recent = store_error().to_owned();
            return document;
        }
        document.intake_refused = state.intake_refused.clone();
        let current_binding = capability.as_ref().map(binding);
        for item in state.items.values().filter(|i| i.phase != Phase::Delivered) {
            let sending = state.busy.contains(&item.id);
            let matching_review = if item.phase == Phase::ReadyToReview {
                item.review.as_ref().filter(|r| {
                    ready
                        && current_instance_id.as_deref() == Some(r.instance_id.as_str())
                        && valid_import_id(&r.import_id)
                })
            } else {
                None
            };
            if let Some(review) = matching_review {
                document.ready_to_review += 1;
                let row_state = if review.expired {
                    "review expired"
                } else {
                    "ready to review"
                };
                document.rows.push(ImportRow {
                    id: item.id.clone(),
                    name: item.name.clone(),
                    state: row_state.to_owned(),
                    reason: review
                        .expired
                        .then(|| text!("linux.import.reviewexpireddetail").to_owned()),
                    can_retry: review.expired
                        && !sending
                        && !state.confirmations.contains_key(&item.id),
                    may_have_saved: true,
                    missing: false,
                    can_remove: !sending && !state.confirmations.contains_key(&item.id),
                    import_id: Some(review.import_id.clone()),
                    reason_code: Some(review.reason_code.clone()),
                    review_kind: Some(review.kind.clone()),
                    review_expired: review.expired,
                    size_bytes: item.size,
                });
                continue;
            }
            let refusal = item
                .refusal
                .as_ref()
                .filter(|r| r.applies_to(current_binding.as_deref(), version));
            let attention = refusal.is_some();
            if attention {
                document.needs_attention += 1;
            } else if ready && supported && item.phase != Phase::Copying {
                document.waiting += 1;
            }
            document.sending |= sending;
            document.rows.push(ImportRow {
                id: item.id.clone(),
                name: item.name.clone(),
                state: if sending {
                    "sending"
                } else if attention {
                    "needs attention"
                } else {
                    "waiting"
                }
                .to_owned(),
                reason: refusal.map(|r| r.reason.clone()),
                can_retry: !sending
                    && !state.confirmations.contains_key(&item.id)
                    && refusal.is_some_and(|r| r.retryable),
                may_have_saved: item.may_have_saved,
                missing: refusal.is_some_and(|r| r.reason == "the copy on this device is missing"),
                can_remove: !sending
                    && item.phase != Phase::Copying
                    && !state.confirmations.contains_key(&item.id),
                import_id: item
                    .saved
                    .as_ref()
                    .map(|s| s.timestamp.clone())
                    .filter(|ts| valid_import_id(ts)),
                reason_code: refusal.and_then(|r| r.reason_code.clone()),
                review_kind: refusal.and_then(|r| r.kind.clone()),
                review_expired: false,
                size_bytes: item.size,
            });
        }
        if ready && let Some(inst) = current_instance_id.as_deref() {
            let mut filtered: Vec<RecentReceipt> = state
                .recent_receipts
                .iter()
                .filter(|r| r.instance_id == inst)
                .cloned()
                .collect();
            prune_recent_receipts(&mut filtered, now_ms);
            document.recent_receipts = filtered;
        }
        document.queue_hold = if capability.is_none() {
            Some(text!("linux.import.waitforjournal").to_owned())
        } else if facts.as_ref().is_some_and(|f| f.journal_mark_held) {
            Some(text!("linux.import.waitformark").to_owned())
        } else if !ready {
            Some(text!("linux.import.pairagain").to_owned())
        } else if current && !supported {
            Some(text!("linux.import.waitforupdate").to_owned())
        } else {
            None
        };
        document.held =
            state.failed || !state.loaded || !document.rows.is_empty() || document.adding;
        document.recent = if capability.is_none() {
            text!("linux.import.waitforjournal").to_owned()
        } else if facts.as_ref().is_some_and(|f| f.journal_mark_held) {
            text!("linux.import.waitformark").to_owned()
        } else if !ready {
            text!("linux.import.pairagain").to_owned()
        } else if !document.rows.is_empty() && !supported {
            text!("linux.import.waitforupdate").to_owned()
        } else if document.sending {
            text!("linux.import.sendingtojournal").to_owned()
        } else if document.waiting > 0
            || document.adding
            || state.items.values().any(|i| i.phase == Phase::Copying)
        {
            text!("linux.import.waitingforjournal").to_owned()
        } else if document.needs_attention > 0 {
            text!("linux.import.held").to_owned()
        } else if document.ready_to_review > 0 {
            message!(
                "linux.import.readytoreviewcount",
                "count" => document.ready_to_review
            )
        } else if !document.rows.is_empty() {
            text!("linux.import.held").to_owned()
        } else if !document.recent_receipts.is_empty()
            || capability.as_ref().is_some_and(|c| {
                state.delivered_journal.as_deref() == Some(c.writer().identity_key())
            })
        {
            text!("linux.import.delivered").to_owned()
        } else {
            text!("linux.import.nothingyet").to_owned()
        };
        document
    }

    pub fn state_json(&self) -> String {
        serde_json::to_string(&self.document()).expect("import document serializes")
    }

    pub async fn send_again(&self, id: String) -> bool {
        let handle = self.clone();
        let ok = tokio::task::spawn_blocking(move || {
            let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.busy.contains(&id) || state.confirmations.contains_key(&id) || state.failed {
                return Ok::<_, io::Error>(false);
            }
            let Some(item) = state.items.get(&id) else {
                return Ok(false);
            };
            if item.phase != Phase::ReadyToReview
                || !item.review.as_ref().is_some_and(|r| r.expired)
            {
                return Ok(false);
            }
            let mut next = item.clone();
            next.saved = None;
            next.review = None;
            next.refusal = None;
            next.phase = Phase::Pending;
            handle.persist(&next)?;
            state.items.insert(id.clone(), next);
            state.saved_current.remove(&id);
            Ok(true)
        })
        .await;
        self.wake();
        matches!(ok, Ok(Ok(true)))
    }

    pub async fn retry(&self, id: String) -> bool {
        let handle = self.clone();
        let ok = tokio::task::spawn_blocking(move || {
            let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.busy.contains(&id) || state.confirmations.contains_key(&id) || state.failed {
                return Ok::<_, io::Error>(false);
            }
            let Some(item) = state.items.get(&id) else {
                return Ok(false);
            };
            let is_expired_review = item.phase == Phase::ReadyToReview
                && item.review.as_ref().is_some_and(|r| r.expired);
            let is_retryable_refusal =
                item.phase == Phase::Refused && item.refusal.as_ref().is_some_and(|r| r.retryable);
            if !is_expired_review && !is_retryable_refusal {
                return Ok(false);
            }
            let mut next = item.clone();
            if is_expired_review
                || next.refusal.as_ref().is_some_and(|refusal| {
                    matches!(
                        refusal.reason_code.as_deref(),
                        Some("import_preview_required" | "import_not_found")
                    )
                })
            {
                next.saved = None;
            }
            next.review = None;
            next.refusal = None;
            next.phase = Phase::Pending;
            handle.persist(&next)?;
            state.items.insert(id.clone(), next);
            state.saved_current.remove(&id);
            Ok(true)
        })
        .await;
        self.wake();
        matches!(ok, Ok(Ok(true)))
    }

    pub(crate) fn begin_remove(
        &self,
        id: &str,
        owner: &str,
    ) -> io::Result<Option<RemoveConfirmation>> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.failed || state.busy.contains(id) || state.confirmations.contains_key(id) {
            return Ok(None);
        }
        let Some(item) = state
            .items
            .get(id)
            .filter(|i| !matches!(i.phase, Phase::Copying | Phase::Delivered))
        else {
            return Ok(None);
        };
        let row = ImportRow {
            id: item.id.clone(),
            name: item.name.clone(),
            state: match item.phase {
                Phase::ReadyToReview if item.review.as_ref().is_some_and(|r| r.expired) => {
                    "review expired"
                }
                Phase::ReadyToReview => "ready to review",
                Phase::Refused => "needs attention",
                _ => "waiting",
            }
            .to_owned(),
            may_have_saved: item.may_have_saved,
            missing: self
                .root
                .as_ref()
                .is_none_or(|root| regular_file(&root.join(id).join("payload")).is_err()),
            import_id: item.review.as_ref().map(|r| r.import_id.clone()),
            review_kind: item.review.as_ref().map(|r| r.kind.clone()),
            review_expired: item.review.as_ref().is_some_and(|r| r.expired),
            size_bytes: item.size,
            ..ImportRow::default()
        };
        let token = new_id()?;
        state.confirmations.insert(
            id.to_owned(),
            ConfirmationOwner {
                owner: owner.to_owned(),
                token: token.clone(),
            },
        );
        Ok(Some(RemoveConfirmation { token, row }))
    }

    pub(crate) fn cancel_remove(&self, id: &str, token: &str, owner: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state
            .confirmations
            .get(id)
            .is_some_and(|c| c.owner == owner && c.token == token)
        {
            state.confirmations.remove(id);
        }
        drop(state);
        self.wake();
    }

    pub(crate) fn cancel_remove_owner(&self, owner: &str) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .confirmations
            .retain(|_, c| c.owner != owner);
        self.wake();
    }

    pub(crate) async fn remove_confirmed(
        &self,
        id: String,
        token: String,
        owner: String,
        may_have_saved: bool,
    ) -> &'static str {
        self.remove_inner(id, may_have_saved, Some((token, owner)))
            .await
    }

    #[cfg(test)]
    pub async fn remove(&self, id: String, may_have_saved: bool) -> &'static str {
        self.remove_inner(id, may_have_saved, None).await
    }

    /// A confirmation owns the item until it closes or its D-Bus connection disappears.
    async fn remove_inner(
        &self,
        id: String,
        may_have_saved: bool,
        confirmation: Option<(String, String)>,
    ) -> &'static str {
        let handle = self.clone();
        let result = match tokio::task::spawn_blocking(move || {
            let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
            let owns_confirmation = match confirmation {
                Some((ref token, ref owner)) => state
                    .confirmations
                    .get(&id)
                    .is_some_and(|c| c.token == *token && c.owner == *owner),
                None => !state.confirmations.contains_key(&id),
            };
            if !owns_confirmation
                || state.failed
                || state.busy.contains(&id)
                || state
                    .items
                    .get(&id)
                    .is_some_and(|i| i.phase == Phase::Copying)
            {
                return Ok::<_, io::Error>("busy");
            }
            let Some(item) = state.items.get(&id) else {
                drop(state);
                handle.cleanup(&id)?;
                return Ok("removed");
            };
            if item.may_have_saved != may_have_saved {
                return Ok("changed");
            }
            let mut tombstone = item.clone();
            tombstone.phase = Phase::Delivered;
            if item.phase != Phase::Delivered {
                handle.persist(&tombstone)?;
            }
            state.items.insert(id.clone(), tombstone);
            drop(state);
            handle.cleanup(&id)?;
            let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
            state.items.remove(&id);
            state.saved_current.remove(&id);
            state.confirmations.remove(&id);
            Ok("removed")
        })
        .await
        {
            Ok(Ok(result)) => result,
            _ => "failed",
        };
        self.wake();
        result
    }

    pub fn dismiss_interrupted(&self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .interrupted = false;
    }

    pub(crate) fn take_auth_status(&self) -> Option<u16> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .auth_status
            .take()
    }

    fn auth_answer(
        &self,
        client: &UploadClient,
        association: &CapturedAssociation,
        outcome: &LinkOutcome,
    ) {
        if let Some(status) = client.import_auth_failure(association, outcome) {
            self.state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .auth_status = Some(status);
        }
    }

    fn reset_foreign_reviews(&self, current_instance_id: &str) -> io::Result<()> {
        let mut changed = false;
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let foreign_ids: Vec<String> = state
                .items
                .iter()
                .filter(|(id, item)| {
                    item.phase == Phase::ReadyToReview
                        && !state.busy.contains(*id)
                        && !state.confirmations.contains_key(*id)
                        && item
                            .review
                            .as_ref()
                            .is_none_or(|r| r.instance_id != current_instance_id)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in foreign_ids {
                if let Some(item) = state.items.get(&id) {
                    let mut next = item.clone();
                    next.phase = Phase::Pending;
                    next.saved = None;
                    next.review = None;
                    next.refusal = None;
                    next.may_have_saved = false;
                    self.persist(&next)?;
                    state.items.insert(id.clone(), next);
                    state.saved_current.remove(&id);
                    changed = true;
                }
            }
        }
        if changed {
            self.publish_status();
        }
        Ok(())
    }

    pub(crate) async fn reconcile_reviews(&self, client: &Arc<UploadClient>, running: &AtomicBool) {
        if self.root.is_none()
            || !running.load(Ordering::Acquire)
            || self.sleep.is_sleeping()
            || client.is_revoked()
        {
            return;
        }
        let association = client.capture_association();
        let Some(capability) = association.capability.clone() else {
            return;
        };
        let facts = capability.facts().snapshot();
        if facts.journal_mark_held
            || facts.terminal_revocation
            || facts.private_state_invalid
            || facts.config_sanitation_failed
            || facts.pairing_required
        {
            return;
        }
        let current_instance_id = capability.writer().identity_key().to_owned();
        if self.reset_foreign_reviews(&current_instance_id).is_err() {
            return;
        }
        let candidates: Vec<(String, String, String)> = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.failed {
                return;
            }
            state
                .items
                .values()
                .filter_map(|item| {
                    if item.phase != Phase::ReadyToReview
                        || state.busy.contains(&item.id)
                        || state.confirmations.contains_key(&item.id)
                    {
                        return None;
                    }
                    let review = item.review.as_ref()?;
                    if review.expired
                        || review.instance_id != current_instance_id
                        || !valid_import_id(&review.import_id)
                    {
                        return None;
                    }
                    Some((
                        item.id.clone(),
                        review.import_id.clone(),
                        review.instance_id.clone(),
                    ))
                })
                .collect()
        };
        for (item_id, saved_import_id, saved_instance_id) in candidates {
            if !running.load(Ordering::Acquire)
                || self.sleep.is_sleeping()
                || client.is_revoked()
                || !client.association_is_current(&association)
                || self
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .auth_status
                    .is_some()
            {
                break;
            }
            let Some(outcome) = guarded(
                client,
                &association,
                running,
                &self.sleep,
                capability.import_status(&saved_import_id),
            )
            .await
            else {
                break;
            };
            if outcome
                .status_code()
                .is_some_and(|code| matches!(code, 401 | 403))
            {
                self.auth_answer(client, &association, &outcome);
                break;
            }
            let LinkOutcome::Success { status, body } = outcome else {
                continue;
            };
            match status.as_u16() {
                404 => {
                    let updated = {
                        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                        let matches = state.items.get(&item_id).is_some_and(|item| {
                            !state.busy.contains(&item_id)
                                && !state.confirmations.contains_key(&item_id)
                                && item.phase == Phase::ReadyToReview
                                && item.review.as_ref().is_some_and(|r| {
                                    !r.expired
                                        && r.import_id == saved_import_id
                                        && r.instance_id == saved_instance_id
                                })
                        });
                        if matches {
                            let mut next = state.items[&item_id].clone();
                            if let Some(review) = &mut next.review {
                                review.expired = true;
                            }
                            if self.persist(&next).is_ok() {
                                state.items.insert(item_id.clone(), next);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    };
                    if updated {
                        self.publish_status();
                    }
                }
                200 => {
                    let Ok(root) = serde_json::from_slice::<Value>(&body) else {
                        continue;
                    };
                    if classify_reconcile_200(&root) == ReconcileDecision::Confirmed
                        && let Err(error) = self.confirm_reconciled_delivery(
                            &item_id,
                            &saved_import_id,
                            &saved_instance_id,
                        )
                    {
                        tracing::warn!(%error, "import reconciliation retained after local storage failure");
                    }
                }
                _ => {}
            }
        }
    }

    fn confirm_reconciled_delivery(
        &self,
        item_id: &str,
        saved_import_id: &str,
        saved_instance_id: &str,
    ) -> io::Result<()> {
        let item = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.busy.contains(item_id) || state.confirmations.contains_key(item_id) {
                return Ok(());
            }
            let Some(item) = state.items.get(item_id) else {
                return Ok(());
            };
            if item.phase != Phase::ReadyToReview
                || !item.review.as_ref().is_some_and(|r| {
                    r.import_id == saved_import_id && r.instance_id == saved_instance_id
                })
            {
                return Ok(());
            }
            let cloned = item.clone();
            state.busy.insert(item_id.to_owned());
            cloned
        };
        let _reservation = Sending {
            handle: self.clone(),
            id: item_id.to_owned(),
        };
        self.delivered(item, saved_instance_id, Some(saved_import_id.to_owned()))
    }

    pub(crate) async fn deliver(&self, client: &Arc<UploadClient>, running: &AtomicBool) {
        if self.root.is_none()
            || !running.load(Ordering::Acquire)
            || self.sleep.is_sleeping()
            || client.is_revoked()
        {
            return;
        }
        let association = client.capture_association();
        let Some(capability) = association.capability.clone() else {
            return;
        };
        let facts = capability.facts().snapshot();
        if facts.journal_mark_held
            || facts.terminal_revocation
            || facts.private_state_invalid
            || facts.config_sanitation_failed
            || facts.pairing_required
        {
            return;
        }
        if self
            .reset_foreign_reviews(capability.writer().identity_key())
            .is_err()
        {
            return;
        }
        self.reconcile_reviews(client, running).await;
        if !running.load(Ordering::Acquire)
            || self.sleep.is_sleeping()
            || client.is_revoked()
            || !client.association_is_current(&association)
            || self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .auth_status
                .is_some()
        {
            return;
        }
        let has_deliverable = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            !state.failed
                && state
                    .items
                    .values()
                    .any(|item| !matches!(item.phase, Phase::Delivered | Phase::ReadyToReview))
        };
        if !has_deliverable {
            return;
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state
                .observed
                .as_ref()
                .is_some_and(|old| !client.association_is_current(old))
            {
                for item in state.items.values_mut() {
                    if item.refusal.as_ref().is_some_and(|r| r.binding.is_some()) {
                        let mut next = item.clone();
                        next.refusal = None;
                        next.phase = Phase::Pending;
                        if self.persist(&next).is_err() {
                            return;
                        }
                        *item = next;
                    }
                }
                state.saved_current.clear();
                state.version = None;
            }
        }
        let Some(version_result) = guarded(
            client,
            &association,
            running,
            &self.sleep,
            capability.system_status(),
        )
        .await
        else {
            return;
        };
        let version = match version_result {
            Ok(version) => version,
            Err(outcome) => {
                self.auth_answer(client, &association, &outcome);
                return;
            }
        };
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.observed = Some(association.clone());
            state.version = version.clone();
        }
        self.publish_status();
        if !supports_import(version.as_deref()) {
            return;
        }
        let binding = binding(&capability);
        let ids = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let mut ids: Vec<_> = state.items.keys().cloned().collect();
            let split = ids.partition_point(|id| id <= &state.cursor);
            ids.rotate_left(split);
            ids
        };
        let mut sleep_hold = None;
        let mut attempted = 0;
        for id in ids {
            if attempted >= BATCH
                || !running.load(Ordering::Acquire)
                || self.sleep.is_sleeping()
                || client.is_revoked()
                || !client.association_is_current(&association)
                || self
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .auth_status
                    .is_some()
            {
                break;
            }
            let deliverable = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                state.items.get(&id).is_some_and(|item| {
                    !matches!(
                        item.phase,
                        Phase::Copying | Phase::Delivered | Phase::ReadyToReview
                    ) && !state.busy.contains(&id)
                        && !state.confirmations.contains_key(&id)
                        && !item
                            .refusal
                            .as_ref()
                            .is_some_and(|r| r.applies_to(Some(&binding), version.as_deref()))
                })
            };
            if !deliverable {
                continue;
            }
            if sleep_hold.is_none() {
                match self.sleep.acquire().await {
                    crate::sleep_inhibit::AcquireOutcome::Held(hold) => sleep_hold = Some(hold),
                    crate::sleep_inhibit::AcquireOutcome::Sleeping => break,
                }
            }
            let item = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let Some(item) = state.items.get(&id) else {
                    continue;
                };
                if matches!(
                    item.phase,
                    Phase::Copying | Phase::Delivered | Phase::ReadyToReview
                ) || state.busy.contains(&id)
                    || state.confirmations.contains_key(&id)
                    || item
                        .refusal
                        .as_ref()
                        .is_some_and(|r| r.applies_to(Some(&binding), version.as_deref()))
                {
                    continue;
                }
                let item = item.clone();
                state.busy.insert(id.clone());
                state.cursor = id.clone();
                item
            };
            attempted += 1;
            let _reservation = Sending {
                handle: self.clone(),
                id: id.clone(),
            };
            self.publish_status();
            if let Err(error) = self
                .send_item(
                    item,
                    client,
                    &association,
                    &capability,
                    &binding,
                    version.as_deref(),
                    running,
                )
                .await
            {
                tracing::warn!(%error, "import retained after local storage failure");
            }
        }
        drop(sleep_hold);
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_item(
        &self,
        mut item: Item,
        client: &Arc<UploadClient>,
        association: &CapturedAssociation,
        capability: &PrivateLinkCapability,
        binding: &str,
        version: Option<&str>,
        running: &AtomicBool,
    ) -> io::Result<()> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        let payload = match regular_file(&root.join(&item.id).join("payload")) {
            Ok(file) if file.metadata()?.len() == item.size => file,
            _ => {
                item.phase = Phase::Refused;
                item.review = None;
                item.refusal = Some(local_refusal("the copy on this device is missing"));
                return self.publish(item);
            }
        };
        let instance_id = capability.writer().identity_key().to_owned();
        let reuse = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .saved_current
            .get(&item.id)
            .is_some_and(|a| client.association_is_current(a));
        if !reuse || item.saved.as_ref().is_none_or(|s| s.binding != binding) {
            item.may_have_saved = true;
            item.phase = Phase::Pending;
            item.refusal = None;
            item.review = None;
            item.saved = None;
            self.publish(item.clone())?;
            let file = tokio::fs::File::from_std(payload);
            let state = self.state.clone();
            let upload_id = item.id.clone();
            let mut sent = 0;
            let stream =
                futures_util::TryStreamExt::inspect_ok(ReaderStream::new(file), move |chunk| {
                    sent += chunk.len() as u64;
                    state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .sent_bytes
                        .insert(upload_id.clone(), sent);
                });
            let part = reqwest::multipart::Part::stream_with_length(
                reqwest::Body::wrap_stream(stream),
                item.size,
            )
            .file_name(item.name.clone())
            .mime_str(&item.mime)
            .map_err(ioe)?;
            let mut form = reqwest::multipart::Form::new()
                .text("client_item_id", item.id.clone())
                .text("imported_via", "linux")
                .part("file", part);
            if let Some(handoff) = &item.handoff {
                form = form.text("area", handoff.area.clone());
                if !matches!(handoff.area.as_str(), "quick" | "recording") {
                    form = form.text("source_hint", handoff.area.clone());
                }
                form = form.text(
                    "context",
                    serde_json::to_string(&handoff.context).map_err(ioe)?,
                );
            }
            let Some(outcome) = guarded(
                client,
                association,
                running,
                &self.sleep,
                capability.import_save(form),
            )
            .await
            else {
                return Ok(());
            };
            self.auth_answer(client, association, &outcome);
            match parse_reply(outcome, false, None) {
                Answer::Saved { path, timestamp } => {
                    item.saved = Some(Saved {
                        binding: binding.to_owned(),
                        path,
                        timestamp,
                        instance_id: Some(instance_id.clone()),
                    });
                    self.publish(item.clone())?;
                    self.state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .saved_current
                        .insert(item.id.clone(), association.clone());
                }
                Answer::Delivered(row) => {
                    return self.delivered(item, &instance_id, row);
                }
                other => return self.retain_answer(item, other, binding, version, &instance_id),
            }
        }
        if !client.association_is_current(association)
            || !running.load(Ordering::Acquire)
            || self.sleep.is_sleeping()
        {
            return Ok(());
        }
        let saved = item
            .saved
            .as_ref()
            .ok_or_else(|| ioe("missing import save progress"))?
            .clone();
        let Some(outcome) = guarded(
            client,
            association,
            running,
            &self.sleep,
            capability.import_start(&saved.path, &saved.timestamp),
        )
        .await
        else {
            return Ok(());
        };
        self.auth_answer(client, association, &outcome);
        match parse_reply(outcome, true, Some(&saved.timestamp)) {
            Answer::Delivered(row) => self.delivered(item, &instance_id, row),
            other => self.retain_answer(item, other, binding, version, &instance_id),
        }
    }

    fn retain_answer(
        &self,
        mut item: Item,
        answer: Answer,
        binding: &str,
        version: Option<&str>,
        instance_id: &str,
    ) -> io::Result<()> {
        match answer {
            Answer::ReadyToReview {
                import_id,
                reason_code,
                kind,
            } => {
                item.phase = Phase::ReadyToReview;
                item.may_have_saved = true;
                item.refusal = None;
                item.review = Some(Review {
                    import_id,
                    reason_code,
                    kind,
                    instance_id: instance_id.to_owned(),
                    expired: false,
                });
                self.publish(item)?;
            }
            Answer::Refused(mut refusal) => {
                refusal.binding = Some(binding.to_owned());
                refusal.version = version.map(str::to_owned);
                item.phase = Phase::Refused;
                item.review = None;
                item.refusal = Some(refusal);
                self.publish(item)?;
            }
            Answer::Saved { .. } | Answer::Delivered(_) | Answer::Retry => {}
        }
        Ok(())
    }

    fn delivered(&self, mut item: Item, journal: &str, row: Option<String>) -> io::Result<()> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        let now_ms = self.now_ms();
        let resolved_row = row.or_else(|| item.saved.as_ref().map(|saved| saved.timestamp.clone()));
        let valid_row = resolved_row
            .as_ref()
            .filter(|id| valid_import_id(id))
            .cloned();

        // Persist the 24h recent receipt durably BEFORE deleting local payload bytes.
        let updated_receipts = {
            let _recent_guard = self.recent_lock.lock().unwrap_or_else(|p| p.into_inner());
            let mut receipts = self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .recent_receipts
                .clone();
            receipts.insert(
                0,
                RecentReceipt {
                    filename: item.name.clone(),
                    import_id: valid_row.clone(),
                    instance_id: journal.to_owned(),
                    delivered_at_epoch_ms: now_ms,
                },
            );
            prune_recent_receipts(&mut receipts, now_ms);
            let file = RecentFile {
                journal: Some(journal.to_owned()),
                receipts: receipts.clone(),
            };
            atomic_write_bytes(&root.join("recent.json"), &encode(&file)?).map_err(ioe)?;
            receipts
        };

        // Tombstone item.json so a crash before cleanup never resurrects it.
        item.phase = Phase::Delivered;
        item.review = None;
        self.persist(&item)?;
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.delivered_journal = Some(journal.to_owned());
            state.recent_receipts = updated_receipts;
            if item.handoff.is_some() {
                if state.receipts.len() >= 1024 {
                    state.receipts.pop_front();
                }
                state.receipts.push_back((item.id.clone(), resolved_row));
            }
            state.sent_bytes.remove(&item.id);
            state.items.remove(&item.id);
            state.saved_current.remove(&item.id);
        }
        self.cleanup(&item.id)?;
        self.publish_status();
        Ok(())
    }
}

fn parse_recent_file(value: Value, now_ms: i64) -> io::Result<RecentFile> {
    let mut file = match value {
        Value::String(journal) => RecentFile {
            journal: (!journal.is_empty()).then_some(journal),
            receipts: Vec::new(),
        },
        Value::Object(_) => serde_json::from_value::<RecentFile>(value).map_err(ioe)?,
        _ => return Err(ioe("invalid recent.json")),
    };
    prune_recent_receipts(&mut file.receipts, now_ms);
    Ok(file)
}

fn prune_recent_receipts(receipts: &mut Vec<RecentReceipt>, now_ms: i64) {
    receipts.retain(|r| {
        !r.filename.is_empty()
            && !r.instance_id.is_empty()
            && now_ms.saturating_sub(r.delivered_at_epoch_ms) <= RECENT_WINDOW_MS
    });
    for receipt in receipts.iter_mut() {
        receipt.import_id = receipt.import_id.take().filter(|id| valid_import_id(id));
    }
    receipts.sort_by_key(|r| std::cmp::Reverse(r.delivered_at_epoch_ms));
    receipts.truncate(RECENT_MAX_ITEMS);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconcileDecision {
    Confirmed,
    Keep,
}

fn classify_reconcile_200(root: &Value) -> ReconcileDecision {
    if matches!(
        root.get("status").and_then(Value::as_str),
        Some("running" | "success" | "failed" | "unconfirmed" | "noted" | "unavailable")
    ) {
        return ReconcileDecision::Confirmed;
    }
    if let Some(saved_start) = root.get("saved_start") {
        if saved_start.get("active").and_then(Value::as_bool) == Some(true) {
            return ReconcileDecision::Confirmed;
        }
        if saved_start.get("state").and_then(Value::as_str) == Some("consumed") {
            return ReconcileDecision::Confirmed;
        }
    }
    ReconcileDecision::Keep
}

struct Sending {
    handle: ImportHandle,
    id: String,
}
impl Drop for Sending {
    fn drop(&mut self) {
        {
            let mut state = self.handle.state.lock().unwrap_or_else(|p| p.into_inner());
            state.busy.remove(&self.id);
            state.sent_bytes.remove(&self.id);
        }
        self.handle.publish_status();
    }
}

fn local_refusal(reason: &str) -> Refusal {
    Refusal {
        reason: reason.to_owned(),
        reason_code: None,
        kind: None,
        binding: None,
        version: None,
        retryable: false,
    }
}

fn available_space(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

async fn guarded<T>(
    client: &UploadClient,
    association: &CapturedAssociation,
    running: &AtomicBool,
    sleep: &crate::sleep_inhibit::SleepInhibit,
    future: impl std::future::Future<Output = T>,
) -> Option<T> {
    if !running.load(Ordering::Acquire)
        || !client.association_is_current(association)
        || sleep.is_sleeping()
    {
        return None;
    }
    let mut sleep_rx = sleep.subscribe();
    if *sleep_rx.borrow_and_update() {
        return None;
    }
    let mut watch_open = true;
    tokio::pin!(future);
    loop {
        tokio::select! {
            // A completed delivery answer may end custody even after the association retired.
            biased;
            answer = &mut future => return Some(answer),
            changed = sleep_rx.changed(), if watch_open => {
                match changed {
                    Ok(()) if *sleep_rx.borrow_and_update() => return None,
                    Ok(()) => {}
                    Err(_) => watch_open = false,
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if !running.load(Ordering::Acquire)
                    || !client.association_is_current(association)
                    || sleep.is_sleeping()
                {
                    return None;
                }
            }
        }
    }
}

#[derive(Debug)]
enum Answer {
    Saved {
        path: String,
        timestamp: String,
    },
    Delivered(Option<String>),
    ReadyToReview {
        import_id: String,
        reason_code: String,
        kind: String,
    },
    Retry,
    Refused(Refusal),
}

fn parse_reply(outcome: LinkOutcome, start: bool, fallback_timestamp: Option<&str>) -> Answer {
    let LinkOutcome::Success { status, body } = outcome else {
        return Answer::Retry;
    };
    if status.as_u16() >= 500 || matches!(status.as_u16(), 401 | 403) {
        return Answer::Retry;
    }
    let root: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let string = |key: &str| {
        root[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    if status.as_u16() == 400
        && root["reason_code"] == "invalid_operation_for_state"
        && root["kind"] == "already_imported"
    {
        return Answer::Delivered(string("import_id"));
    }
    if status.as_u16() == 200 {
        if start
            && root["status"] == "ok"
            && string("task_id").is_some()
            && string("import_id").is_some()
        {
            return Answer::Delivered(string("import_id"));
        }
        if !start
            && root["status"] == "staged"
            && let (Some(path), Some(timestamp)) = (string("path"), string("timestamp"))
        {
            return Answer::Saved { path, timestamp };
        }
        return Answer::Retry;
    }
    if status.is_success() {
        return Answer::Retry;
    }
    let reason_code = string("reason_code");
    let kind = string("kind");
    if start && status.as_u16() == 409 && reason_code.as_deref() == Some("import_preview_required")
    {
        let candidate_id = string("import_id").or_else(|| fallback_timestamp.map(str::to_owned));
        if is_confirmation_required_kind(kind.as_deref())
            && let Some(import_id) = candidate_id.filter(|id| valid_import_id(id))
        {
            return Answer::ReadyToReview {
                import_id,
                reason_code: "import_preview_required".to_owned(),
                kind: kind.unwrap_or_default(),
            };
        }
        let retryable = matches!(kind.as_deref(), Some("source_changed" | "already_running"));
        return Answer::Refused(Refusal {
            reason: string("error")
                .unwrap_or_else(|| text!("linux.import.journalrefusal").to_owned()),
            reason_code,
            kind,
            binding: None,
            version: None,
            retryable,
        });
    }
    let retryable = !matches!(
        reason_code.as_deref(),
        Some(
            "multipart_part_too_large"
                | "unsupported_type"
                | "unsupported_file_type"
                | "file_too_large"
                | "invalid_file"
        )
    ) && !matches!(
        kind.as_deref(),
        Some(
            "client_item_conflict"
                | "nothing_to_import"
                | "plan_failed"
                | "invalid_request"
                | "not_found"
        )
    );
    Answer::Refused(Refusal {
        reason: string("error").unwrap_or_else(|| text!("linux.import.journalrefusal").to_owned()),
        reason_code,
        kind,
        binding: None,
        version: None,
        retryable,
    })
}

/// Render the app's stable import status and reasons from older saved items.
pub fn display_import_status(value: &str) -> &str {
    match value {
        "sending" => text!("linux.import.sending"),
        "waiting" => text!("linux.import.waiting"),
        "ready to review" => text!("linux.import.readytoreview"),
        "review expired" => text!("linux.import.expired"),
        "needs attention" => text!("linux.import.attention"),
        "this couldn't be read. add it again" => text!("linux.import.filenotread"),
        "the copy on this device is missing" => text!("linux.import.filemissing"),
        reason => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    #[test]
    fn floor_and_wire_answers_fail_closed() {
        for version in [
            None,
            Some("2.0.51"),
            Some("unknown"),
            Some("2.0.52-extra"),
            Some("2.0.52.1"),
        ] {
            assert!(!supports_import(version));
        }
        for version in ["2.0.52", "v2.0.75", "2.1.0", "3.0.0"] {
            assert!(supports_import(Some(version)));
        }
        for start in [false, true] {
            let replay =
                br#"{"reason_code":"invalid_operation_for_state","kind":"already_imported"}"#;
            assert!(matches!(
                parse_reply(
                    LinkOutcome::Success {
                        status: StatusCode::BAD_REQUEST,
                        body: replay.to_vec()
                    },
                    start,
                    None,
                ),
                Answer::Delivered(_)
            ));
            for status in [200, 409, 500] {
                assert!(!matches!(
                    parse_reply(
                        LinkOutcome::Success {
                            status: StatusCode::from_u16(status).unwrap(),
                            body: replay.to_vec()
                        },
                        start,
                        None,
                    ),
                    Answer::Delivered(_)
                ));
            }
            assert!(matches!(
                parse_reply(
                    LinkOutcome::Success {
                        status: StatusCode::OK,
                        body: b"{\"status\":\"ok\"}".to_vec()
                    },
                    start,
                    None,
                ),
                Answer::Retry
            ));
        }
    }

    #[test]
    fn valid_import_id_enforces_strict_ascii_timestamp() {
        assert!(valid_import_id("20261121_181500"));
        assert!(valid_import_id("00000000_000000"));
        for bad in [
            "",
            "20261121_18150",
            "20261121_1815000",
            "20261121-181500",
            "not-a-timestamp",
            "../settings",
            "20261121_181500?x=1",
            "20261121_181500#frag",
            "２０２６１１２１_１８１５００",
        ] {
            assert!(!valid_import_id(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn preview_required_classification_allows_only_explicit_confirmation_kinds() {
        for kind in [
            "body_confirmation_required",
            "confirmation_required",
            "evernote_confirmation_required",
        ] {
            let body = serde_json::json!({
                "reason_code": "import_preview_required",
                "kind": kind,
                "import_id": "20261121_181500",
                "error": "preview required"
            })
            .to_string();
            assert!(
                matches!(
                    parse_reply(
                        LinkOutcome::Success {
                            status: StatusCode::CONFLICT,
                            body: body.into_bytes(),
                        },
                        true,
                        None,
                    ),
                    Answer::ReadyToReview { import_id, .. } if import_id == "20261121_181500"
                ),
                "expected ReadyToReview for {kind}"
            );
        }
        // Fallback to persisted /save timestamp when response omits import_id.
        let fallback_body = serde_json::json!({
            "reason_code": "import_preview_required",
            "kind": "body_confirmation_required",
        })
        .to_string();
        assert!(matches!(
            parse_reply(
                LinkOutcome::Success {
                    status: StatusCode::CONFLICT,
                    body: fallback_body.into_bytes(),
                },
                true,
                Some("20261121_181501"),
            ),
            Answer::ReadyToReview { import_id, .. } if import_id == "20261121_181501"
        ));
        // Invalid import_id (even with valid fallback) or missing valid id must NOT enter ReadyToReview.
        let bad_id_body = serde_json::json!({
            "reason_code": "import_preview_required",
            "kind": "body_confirmation_required",
            "import_id": "../bad"
        })
        .to_string();
        assert!(matches!(
            parse_reply(
                LinkOutcome::Success {
                    status: StatusCode::CONFLICT,
                    body: bad_id_body.into_bytes(),
                },
                true,
                Some("20261121_181501"),
            ),
            Answer::Refused(Refusal {
                retryable: false,
                ..
            })
        ));
        // All non-confirmation kinds enter Refused, never ReadyToReview.
        for (kind, expect_retryable) in [
            (Some("plan_failed"), false),
            (Some("nothing_to_import"), false),
            (Some("invalid_request"), false),
            (Some("not_found"), false),
            (Some("source_changed"), true),
            (Some("already_running"), true),
            (Some("unknown_kind"), false),
            (None, false),
        ] {
            let mut obj = serde_json::json!({
                "reason_code": "import_preview_required",
                "import_id": "20261121_181500",
                "error": "refused"
            });
            if let Some(k) = kind {
                obj["kind"] = json!(k);
            }
            match parse_reply(
                LinkOutcome::Success {
                    status: StatusCode::CONFLICT,
                    body: obj.to_string().into_bytes(),
                },
                true,
                Some("20261121_181500"),
            ) {
                Answer::Refused(r) => {
                    assert_eq!(
                        r.retryable, expect_retryable,
                        "retryable mismatch for {kind:?}"
                    );
                }
                other => panic!("expected Refused for {kind:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn reconcile_200_classification_is_conservative_and_exact() {
        for status in [
            "running",
            "success",
            "failed",
            "unconfirmed",
            "noted",
            "unavailable",
        ] {
            assert_eq!(
                classify_reconcile_200(&json!({"status": status})),
                ReconcileDecision::Confirmed,
                "expected Confirmed for status {status}"
            );
        }
        assert_eq!(
            classify_reconcile_200(
                &json!({"status": "pending", "saved_start": {"active": true, "state": "eligible"}})
            ),
            ReconcileDecision::Confirmed
        );
        assert_eq!(
            classify_reconcile_200(
                &json!({"status": "pending", "saved_start": {"active": false, "state": "consumed"}})
            ),
            ReconcileDecision::Confirmed
        );
        for keep in [
            json!({"status": "pending", "saved_start": {"active": false, "state": "eligible"}}),
            json!({"status": "pending", "saved_start": {"active": false, "state": "unknown"}}),
            json!({"status": "pending", "saved_start": null}),
            json!({"status": "pending"}),
            json!({}),
        ] {
            assert_eq!(
                classify_reconcile_200(&keep),
                ReconcileDecision::Keep,
                "must keep for {keep}"
            );
        }
    }

    #[test]
    fn recent_json_decodes_legacy_string_and_prunes_24h_cap_5() {
        let now = 2_000_000_000_000i64;
        // Legacy bare JSON string format
        let bare = parse_recent_file(json!("inst-a:abcd"), now).unwrap();
        assert_eq!(bare.journal.as_deref(), Some("inst-a:abcd"));
        assert!(bare.receipts.is_empty());

        // Legacy object without receipts
        let legacy_obj = parse_recent_file(json!({"journal": "inst-a:abcd"}), now).unwrap();
        assert_eq!(legacy_obj.journal.as_deref(), Some("inst-a:abcd"));
        assert!(legacy_obj.receipts.is_empty());

        // Pruning >24h, capping to 5 newest, sanitizing invalid import_id
        let mut list = vec![RecentReceipt {
            filename: "expired.zip".into(),
            import_id: Some("20261120_100000".into()),
            instance_id: "inst-a".into(),
            delivered_at_epoch_ms: now - RECENT_WINDOW_MS - 1,
        }];
        for i in 0..7 {
            list.push(RecentReceipt {
                filename: format!("file-{i}.zip"),
                import_id: if i == 6 {
                    Some("not-valid".into())
                } else {
                    Some(format!("20261121_12000{i}"))
                },
                instance_id: "inst-a".into(),
                delivered_at_epoch_ms: now - (7 - i as i64) * 1000,
            });
        }
        prune_recent_receipts(&mut list, now);
        assert_eq!(list.len(), 5);
        assert_eq!(list[0].filename, "file-6.zip");
        assert_eq!(
            list[0].import_id, None,
            "invalid import_id must be sanitized"
        );
        assert_eq!(list[4].filename, "file-2.zip");
        assert_eq!(list[4].import_id.as_deref(), Some("20261121_120002"));
    }

    #[cfg(feature = "full-tests")]
    fn store_entries(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[tokio::test]
    async fn arbitrary_files_reach_custody_with_their_original_name() {
        let temp = tempfile::tempdir().unwrap();
        let handle = ImportHandle {
            root: Some(temp.path().join("import")),
            ..ImportHandle::default()
        };
        let source = temp.path().join("extensionless");
        fs::write(&source, b"file bytes").unwrap();
        assert!(handle.add_files(vec![source.display().to_string()]).await);
        let state = handle.state.lock().unwrap();
        let item = state.items.values().next().unwrap();
        assert_eq!(item.name, "extensionless");
        assert_eq!(item.mime, "application/octet-stream");
        assert_eq!(item.phase, Phase::Pending);
    }

    #[tokio::test]
    async fn handoff_owns_selected_descriptor_and_preserves_context_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("import");
        let handle = ImportHandle {
            root: Some(root.clone()),
            ..ImportHandle::default()
        };
        let file = temp.path().join("source");
        fs::write(&file, b"selected bytes").unwrap();
        let selected = File::open(&file).unwrap();
        fs::remove_file(&file).unwrap();
        let handoff: Handoff =
            serde_json::from_value(json!({"area":"document","context":{"future":{"nested":true}}}))
                .unwrap();
        let result = handle
            .add_open_files(vec![("notes.txt".into(), selected)], handoff)
            .await;
        assert_eq!(result["outcome"], "queued", "{result}");
        let id = result["items"][0]["item"].as_str().unwrap();
        assert_eq!(
            fs::read(root.join(id).join("payload")).unwrap(),
            b"selected bytes"
        );
        let restored = ImportHandle {
            root: Some(root),
            ..ImportHandle::default()
        };
        restored.restore().unwrap();
        assert_eq!(restored.handoff_status(id)["state"], "waiting");
        let state = restored.state.lock().unwrap();
        let kept = state.items[id].handoff.as_ref().unwrap();
        assert_eq!(kept.area, "document");
        assert_eq!(kept.context["future"]["nested"], true);
    }

    #[test]
    fn documents_without_intake_refusals_still_read() {
        let document: ImportDocument = serde_json::from_value(serde_json::json!({
            "v": 1, "rows": [], "recent": "nothing yet.", "addition": null, "adding": false,
            "interrupted": false, "store_error": false, "waiting": 0, "held": false,
            "sending": false
        }))
        .unwrap();
        assert!(document.intake_refused.is_empty());
    }

    // Copying a file checks free space on the host.
    #[cfg(feature = "full-tests")]
    #[tokio::test]
    async fn mixed_addition_keeps_the_copy_and_names_the_refusal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("import");
        let handle = ImportHandle {
            root: Some(root.clone()),
            ..ImportHandle::default()
        };
        let large = temp.path().join("large.pdf");
        File::create(&large)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        let note = temp.path().join("note.txt");
        fs::write(&note, b"kept bytes").unwrap();
        assert!(
            handle
                .add_files(vec![
                    large.display().to_string(),
                    note.display().to_string()
                ])
                .await
        );
        let items: Vec<_> = handle
            .state
            .lock()
            .unwrap()
            .items
            .values()
            .cloned()
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "note.txt");
        assert_eq!(items[0].phase, Phase::Pending);
        assert_eq!(store_entries(&root), vec![items[0].id.clone()]);
        let document = handle.document();
        assert_eq!(document.rows.len(), 1);
        assert_eq!(document.rows[0].name, "note.txt");
        assert_eq!(
            document.addition.as_deref(),
            Some("saved on this device: 1")
        );
        assert_eq!(
            document
                .intake_refused
                .iter()
                .map(IntakeRefusal::line)
                .collect::<Vec<_>>(),
            vec!["large.pdf: it's larger than your journal takes"]
        );
    }

    #[cfg(feature = "full-tests")]
    #[tokio::test]
    async fn copies_survive_source_changes_and_incomplete_restart() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("import");
        let handle = ImportHandle {
            root: Some(root.clone()),
            ..ImportHandle::default()
        };
        let source = temp.path().join("note.txt");
        fs::write(&source, b"selected bytes").unwrap();
        assert!(
            handle
                .add_files(vec![
                    source.display().to_string(),
                    source.display().to_string()
                ])
                .await
        );
        fs::write(&source, b"changed bytes").unwrap();
        let items: Vec<_> = handle
            .state
            .lock()
            .unwrap()
            .items
            .values()
            .cloned()
            .collect();
        assert_eq!(items.len(), 2);
        assert_ne!(items[0].id, items[1].id);
        for item in &items {
            assert_eq!(
                fs::read(root.join(&item.id).join("payload")).unwrap(),
                b"selected bytes"
            );
        }
        let mut interrupted = items[0].clone();
        interrupted.phase = Phase::Copying;
        handle.persist(&interrupted).unwrap();
        let reopened = ImportHandle {
            root: Some(root.clone()),
            ..ImportHandle::default()
        };
        reopened.restore().unwrap();
        let document = reopened.document();
        assert!(document.interrupted);
        assert!(document.intake_refused.is_empty());
        assert!(
            document
                .rows
                .iter()
                .any(|r| r.id == interrupted.id && r.state == "needs attention")
        );
        assert_eq!(
            reopened.state.lock().unwrap().items[&interrupted.id].phase,
            Phase::Refused
        );
        assert_eq!(reopened.remove(items[1].id.clone(), false).await, "removed");
        assert!(!root.join(&items[1].id).exists());
    }

    #[test]
    fn convertible_photo_extensions_and_jpeg_names() {
        assert!(is_convertible_photo(Path::new("IMG_0412.HEIC")));
        assert!(is_convertible_photo(Path::new("shot.heif")));
        assert!(!is_convertible_photo(Path::new("photo.jpg")));
        assert_eq!(
            converted_jpeg_name(Path::new("/tmp/IMG_0412.HEIC")),
            "IMG_0412.jpg"
        );
    }

    #[cfg(feature = "full-tests")]
    #[tokio::test]
    async fn heic_photo_converts_to_jpeg_in_custody_and_refuses_when_undecodable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("import");
        let heic = temp.path().join("IMG_0412.HEIC");
        fs::write(&heic, b"fake-heic").unwrap();

        let ok_handle = ImportHandle {
            root: Some(root.clone()),
            photo_converter: PhotoConverter::Fixed(Ok(b"\xff\xd8jpeg-out\xff\xd9".to_vec())),
            ..ImportHandle::default()
        };
        assert!(ok_handle.add_files(vec![heic.display().to_string()]).await);
        let items: Vec<_> = ok_handle
            .state
            .lock()
            .unwrap()
            .items
            .values()
            .cloned()
            .collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "IMG_0412.jpg");
        assert_eq!(items[0].mime, "image/jpeg");
        assert_eq!(
            fs::read(root.join(&items[0].id).join("payload")).unwrap(),
            b"\xff\xd8jpeg-out\xff\xd9"
        );

        let root_fail = temp.path().join("import-fail");
        let bad = temp.path().join("IMG_0413.heif");
        fs::write(&bad, b"bad-heif").unwrap();
        let fail_handle = ImportHandle {
            root: Some(root_fail.clone()),
            photo_converter: PhotoConverter::Fixed(Err(())),
            ..ImportHandle::default()
        };
        assert!(fail_handle.add_files(vec![bad.display().to_string()]).await);
        assert!(fail_handle.state.lock().unwrap().items.is_empty());
        assert!(store_entries(&root_fail).is_empty());
        let doc = fail_handle.document();
        assert_eq!(doc.intake_refused.len(), 1);
        assert_eq!(doc.intake_refused[0].name, "IMG_0413.heif");
        assert!(!doc.intake_refused[0].reason.is_empty());
    }

    #[cfg(feature = "full-tests")]
    mod delivery {
        use super::*;
        use crate::{
            config::Config,
            private_link::{PrivateLinkSession, start_private_link_session},
            private_link_test_peer::PrivateLinkPeer,
            sync::SyncTrigger,
            test_support::MutableClock,
        };
        use serde_json::json;

        struct Harness {
            temp: tempfile::TempDir,
            peer: PrivateLinkPeer,
            session: PrivateLinkSession,
            client: Arc<UploadClient>,
            handle: ImportHandle,
            running: Arc<AtomicBool>,
        }

        impl Harness {
            async fn new() -> Self {
                let temp = tempfile::tempdir().unwrap();
                let peer = PrivateLinkPeer::start().await;
                peer.set_route(
                    "/api/system/status",
                    200,
                    json!({"version":{"current":"2.0.52"}})
                        .to_string()
                        .into_bytes(),
                );
                peer.set_route(
                    "/app/import/api/save",
                    200,
                    br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                        .to_vec(),
                );
                peer.set_route(
                    "/app/import/api/start",
                    200,
                    br#"{"status":"ok","task_id":"task","import_id":"saved"}"#.to_vec(),
                );
                let session = start_private_link_session(
                    &temp.path().join("link"),
                    peer.credential(),
                    "desktop",
                )
                .await
                .unwrap();
                let config = Config {
                    base_dir: temp.path().join("data"),
                    config_dir: temp.path().join("config"),
                    ..Config::default()
                };
                let client = Arc::new(UploadClient::new(
                    &config,
                    session.capability(),
                    Arc::new(MutableClock::new(1_800_000_000.0, 100.0)),
                ));
                let handle = ImportHandle::new(
                    config.base_dir.join("import"),
                    &client,
                    SyncTrigger::default(),
                );
                Self {
                    temp,
                    peer,
                    session,
                    client,
                    handle,
                    running: Arc::new(AtomicBool::new(true)),
                }
            }
            async fn add(&self, bytes: &[u8]) -> String {
                let path = self.temp.path().join("note.txt");
                fs::write(&path, bytes).unwrap();
                let before: HashSet<_> = self
                    .handle
                    .document()
                    .rows
                    .into_iter()
                    .map(|r| r.id)
                    .collect();
                assert!(
                    self.handle
                        .add_files(vec![path.display().to_string()])
                        .await
                );
                self.handle
                    .document()
                    .rows
                    .into_iter()
                    .find(|r| !before.contains(&r.id))
                    .unwrap()
                    .id
            }
            async fn pass(&self) {
                self.handle.deliver(&self.client, &self.running).await;
            }
            async fn wait_save(&self) {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while !self
                        .peer
                        .requests()
                        .iter()
                        .any(|r| r.path == "/app/import/api/save")
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
            fn saves(&self) -> usize {
                self.peer
                    .requests()
                    .iter()
                    .filter(|r| r.path == "/app/import/api/save")
                    .count()
            }
            fn starts(&self) -> usize {
                self.peer
                    .requests()
                    .iter()
                    .filter(|r| r.path == "/app/import/api/start")
                    .count()
            }
            async fn close(self) {
                let _ = self.session.shutdown().await;
                self.peer.shutdown().await;
            }
        }

        #[tokio::test]
        async fn streamed_copy_and_both_replay_steps_finish_custody() {
            let h = Harness::new().await;
            let bytes = vec![b'x'; 1024 * 1024 + 17];
            let id = h.add(&bytes).await;
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert!(!h.handle.root.as_ref().unwrap().join(&id).exists());
            let requests = h.peer.requests();
            let save = requests
                .iter()
                .find(|r| r.path == "/app/import/api/save")
                .unwrap();
            assert_eq!(save.method, "POST");
            assert!(save.body.windows(bytes.len()).any(|window| window == bytes));
            let text = String::from_utf8_lossy(&save.body);
            assert!(
                text.contains(&id) && text.contains("client_item_id") && text.contains("linux")
            );
            let start = requests
                .iter()
                .find(|r| r.path == "/app/import/api/start")
                .unwrap();
            let start: Value = serde_json::from_slice(&start.body).unwrap();
            assert_eq!(
                start,
                json!({"path":"/imports/saved/note.txt","timestamp":"saved"})
            );
            for at_save in [true, false] {
                let id = h
                    .add(if at_save {
                        b"save replay"
                    } else {
                        b"start replay"
                    })
                    .await;
                let replay =
                    br#"{"reason_code":"invalid_operation_for_state","kind":"already_imported"}"#
                        .to_vec();
                h.peer.set_route(
                    if at_save {
                        "/app/import/api/save"
                    } else {
                        "/app/import/api/start"
                    },
                    400,
                    replay,
                );
                if !at_save {
                    h.peer.set_route("/app/import/api/save", 200, br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#.to_vec());
                }
                let starts = h.starts();
                h.pass().await;
                assert!(h.handle.document().rows.is_empty());
                assert!(!h.handle.root.as_ref().unwrap().join(&id).exists());
                assert_eq!(h.starts(), starts + usize::from(!at_save));
            }
            h.close().await;
        }

        #[tokio::test]
        async fn old_unknown_and_refused_items_keep_bytes_until_new_evidence() {
            let h = Harness::new().await;
            let id = h.add(b"held bytes").await;
            for status in [
                json!({"version":{"current":"2.0.51"}}),
                json!({"version":{}}),
            ] {
                h.peer
                    .set_route("/api/system/status", 200, status.to_string().into_bytes());
                h.pass().await;
                let doc = h.handle.document();
                assert_eq!(doc.waiting, 0);
                assert!(doc.held);
                assert_eq!(h.saves(), 0);
                assert_eq!(h.starts(), 0);
            }
            h.peer.set_route(
                "/api/system/status",
                200,
                json!({"version":{"current":"2.0.52"}})
                    .to_string()
                    .into_bytes(),
            );
            h.peer.set_route("/app/import/api/save", 413, br#"{"error":"it's larger than your journal takes","reason_code":"multipart_part_too_large"}"#.to_vec());
            h.pass().await;
            let doc = h.handle.document();
            assert_eq!(doc.waiting, 0);
            assert!(doc.held);
            assert!(!doc.rows[0].can_retry);
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id)
                    .join("payload")
                    .exists()
            );
            h.pass().await;
            assert_eq!(h.saves(), 1);
            h.peer.set_route(
                "/api/system/status",
                200,
                json!({"version":{"current":"2.0.53"}})
                    .to_string()
                    .into_bytes(),
            );
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
            );
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(h.saves(), 2);
            h.close().await;
        }

        #[tokio::test]
        async fn cancellation_retains_ambiguous_save_and_rechecks_removal_warning() {
            let h = Harness::new().await;
            let id = h.add(b"ambiguous save").await;
            let gate = Arc::new(AtomicBool::new(false));
            h.peer.set_pending_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
                gate,
                false,
            );
            let handle = h.handle.clone();
            let client = h.client.clone();
            let running = h.running.clone();
            let task = tokio::spawn(async move {
                handle.deliver(&client, &running).await;
            });
            h.wait_save().await;
            assert_eq!(h.handle.remove(id.clone(), false).await, "busy");
            assert!(h.handle.document().rows[0].may_have_saved);
            h.running.store(false, Ordering::Release);
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(h.starts(), 0);
            assert_eq!(h.handle.remove(id.clone(), false).await, "changed");
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id)
                    .join("payload")
                    .exists()
            );
            assert_eq!(h.handle.remove(id, true).await, "removed");
            h.close().await;
        }

        #[tokio::test]
        async fn association_aba_cannot_start_a_late_saved_path() {
            let h = Harness::new().await;
            let id = h.add(b"same stable client item").await;
            let gate = Arc::new(AtomicBool::new(false));
            h.peer.set_pending_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
                gate.clone(),
                false,
            );
            let handle = h.handle.clone();
            let client = h.client.clone();
            let running = h.running.clone();
            let task = tokio::spawn(async move {
                handle.deliver(&client, &running).await;
            });
            h.wait_save().await;
            let old = h.client.capability().unwrap();
            let other = PrivateLinkPeer::start().await;
            let other_session = start_private_link_session(
                &h.temp.path().join("other-link"),
                other.credential(),
                "desktop",
            )
            .await
            .unwrap();
            h.client.prepare_new_owner();
            h.client.install_capability(other_session.capability());
            h.client.prepare_new_owner();
            h.client.install_capability(old);
            gate.store(true, Ordering::Release);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(h.starts(), 0);
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
            );
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(h.saves(), 2);
            assert_eq!(h.starts(), 1);
            for request in h
                .peer
                .requests()
                .iter()
                .filter(|r| r.path == "/app/import/api/save")
            {
                assert!(String::from_utf8_lossy(&request.body).contains(&id));
            }
            h.close().await;
            let _ = other_session.shutdown().await;
            other.shutdown().await;
        }

        #[tokio::test]
        async fn restart_after_save_resends_same_id_without_losing_warning() {
            let mut h = Harness::new().await;
            let id = h.add(b"restart bytes").await;
            h.peer
                .set_route("/app/import/api/start", 500, b"{}".to_vec());
            h.pass().await;
            assert!(h.handle.state.lock().unwrap().items[&id].saved.is_some());
            h.handle = ImportHandle::new(
                h.handle.root.clone().unwrap(),
                &h.client,
                SyncTrigger::default(),
            );
            h.handle.restore().unwrap();
            assert!(h.handle.document().rows[0].may_have_saved);
            h.peer.set_route(
                "/app/import/api/start",
                200,
                br#"{"status":"ok","task_id":"task","import_id":"saved"}"#.to_vec(),
            );
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(h.saves(), 2);
            for request in h
                .peer
                .requests()
                .iter()
                .filter(|r| r.path == "/app/import/api/save")
            {
                assert!(String::from_utf8_lossy(&request.body).contains(&id));
            }
            h.close().await;
        }

        #[tokio::test]
        async fn delivered_marker_and_store_failure_do_not_resurrect_or_delete_held_data() {
            let h = Harness::new().await;
            let id = h.add(b"delivered marker").await;
            let mut item = h.handle.state.lock().unwrap().items[&id].clone();
            item.phase = Phase::Delivered;
            h.handle.persist(&item).unwrap();
            let root = h.handle.root.clone().unwrap();
            let bad = root.join("f".repeat(32));
            std::os::unix::fs::symlink(h.temp.path(), &bad).unwrap();
            let failed = ImportHandle::new(root.clone(), &h.client, SyncTrigger::default());
            assert!(failed.restore().is_err());
            assert!(root.join(&id).join("payload").exists());
            assert!(failed.document().store_error);
            fs::remove_file(bad).unwrap();
            let recovered = ImportHandle::new(root.clone(), &h.client, SyncTrigger::default());
            recovered.restore().unwrap();
            assert!(recovered.document().rows.is_empty());
            assert!(!root.join(&id).exists());
            h.close().await;
        }

        #[tokio::test]
        async fn bounded_pass_rotates_past_transient_failures() {
            let h = Harness::new().await;
            for _ in 0..20 {
                h.add(b"pending").await;
            }
            h.peer
                .set_route("/app/import/api/save", 500, b"{}".to_vec());
            h.pass().await;
            assert_eq!(h.saves(), 16);
            assert_eq!(h.handle.document().waiting, 20);
            h.pass().await;
            assert_eq!(h.saves(), 32);
            let mut seen = HashSet::new();
            for request in h
                .peer
                .requests()
                .iter()
                .filter(|r| r.path == "/app/import/api/save")
            {
                for id in h.handle.state.lock().unwrap().items.keys() {
                    if String::from_utf8_lossy(&request.body).contains(id) {
                        seen.insert(id.clone());
                    }
                }
            }
            assert_eq!(seen.len(), 20);
            h.close().await;
        }

        #[tokio::test]
        async fn confirmation_reserves_delivery_until_cancel_or_owner_loss() {
            let h = Harness::new().await;
            let id = h.add(b"confirmation holds this copy").await;
            let confirmation = h.handle.begin_remove(&id, ":1.4").unwrap().unwrap();
            assert!(!confirmation.row.may_have_saved);
            h.pass().await;
            assert_eq!(h.saves(), 0);
            assert_eq!(h.starts(), 0);
            assert_eq!(
                h.handle
                    .remove_confirmed(
                        id.clone(),
                        confirmation.token.clone(),
                        ":1.5".to_owned(),
                        false
                    )
                    .await,
                "busy"
            );
            assert_eq!(h.handle.remove(id.clone(), false).await, "busy");
            h.handle.cancel_remove(&id, &confirmation.token, ":1.4");
            let _confirmation = h.handle.begin_remove(&id, ":1.6").unwrap().unwrap();
            h.pass().await;
            assert_eq!(h.saves(), 0);
            h.handle.cancel_remove_owner(":1.6");
            h.pass().await;
            assert_eq!(h.saves(), 1);
            assert!(h.handle.document().rows.is_empty());
            h.close().await;
        }

        #[tokio::test]
        async fn confirmed_remove_retries_real_cleanup_after_failure() {
            let h = Harness::new().await;
            let id = h.add(b"owner removed these bytes").await;
            let root = h.handle.root.as_ref().unwrap();
            fs::write(
                root.join(&id).join("unexpected"),
                b"keep unexpected entries",
            )
            .unwrap();
            let confirmation = h.handle.begin_remove(&id, ":1.4").unwrap().unwrap();
            assert_eq!(
                h.handle
                    .remove_confirmed(
                        id.clone(),
                        confirmation.token.clone(),
                        ":1.4".to_owned(),
                        false
                    )
                    .await,
                "failed"
            );
            let retired = root.join(format!(".removed-{id}"));
            assert!(retired.exists());
            h.pass().await;
            assert_eq!(h.saves(), 0);
            fs::remove_file(retired.join("unexpected")).unwrap();
            assert_eq!(
                h.handle
                    .remove_confirmed(id, confirmation.token, ":1.4".to_owned(), false)
                    .await,
                "removed"
            );
            assert!(!retired.exists());
            assert!(h.handle.document().rows.is_empty());
            h.close().await;
        }

        #[tokio::test]
        async fn forbidden_latches_existing_auth_handling_and_keeps_custody() {
            let h = Harness::new().await;
            let id = h.add(b"do not discard on auth refusal").await;
            h.peer
                .set_route("/app/import/api/save", 403, b"{}".to_vec());
            h.pass().await;
            assert!(h.client.is_revoked());
            assert_eq!(h.handle.take_auth_status(), Some(403));
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(id)
                    .join("payload")
                    .exists()
            );
            let count = h.saves();
            h.pass().await;
            assert_eq!(h.saves(), count);
            h.close().await;
        }

        #[tokio::test]
        async fn ready_to_review_reconciles_expires_send_again_and_resets_on_repair() {
            let now = Arc::new(std::sync::atomic::AtomicI64::new(1_800_000_000_000));
            let mut h = Harness::new().await;
            let state_dir = h.temp.path().join("state");
            let now_read = Arc::clone(&now);
            h.handle = h
                .handle
                .with_state_dir(state_dir.clone())
                .with_clock_ms(Arc::new(move || now_read.load(Ordering::Acquire)));

            // 1. Apple Health confirmation-required 409 enters ready_to_review, keeps bytes, not retried on version bump.
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_181500/export.zip","timestamp":"20261121_181500"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                409,
                br#"{"reason_code":"import_preview_required","kind":"body_confirmation_required","import_id":"20261121_181500","error":"confirm body import"}"#
                    .to_vec(),
            );
            let handoff: Handoff =
                serde_json::from_value(json!({"area":"apple_health","context":{}})).unwrap();
            let src = h.temp.path().join("export.zip");
            fs::write(&src, b"apple health bytes").unwrap();
            let opened = File::open(&src).unwrap();
            let added = h
                .handle
                .add_open_files(vec![("export.zip".into(), opened)], handoff)
                .await;
            let id = added["items"][0]["item"].as_str().unwrap().to_owned();
            // Counter incremented once on intake.
            let crate::source_intake::IntakeFile::Rows(intake) =
                crate::source_intake::read_rows(&crate::source_intake::path(&state_dir))
            else {
                panic!("intake rows expected");
            };
            assert_eq!(intake.imports.total, 1);

            h.pass().await;
            let doc = h.handle.document();
            assert_eq!(doc.waiting, 0);
            assert_eq!(doc.ready_to_review, 1);
            assert_eq!(doc.needs_attention, 0);
            assert_eq!(doc.rows.len(), 1);
            assert_eq!(doc.rows[0].state, "ready to review");
            assert!(!doc.rows[0].can_retry);
            assert_eq!(doc.rows[0].import_id.as_deref(), Some("20261121_181500"));
            assert_eq!(
                doc.rows[0].review_kind.as_deref(),
                Some("body_confirmation_required")
            );
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id)
                    .join("payload")
                    .exists()
            );

            // Handoff bridge emits additive v2 fields.
            let bridge = h.handle.handoff_status(&id);
            assert_eq!(bridge["state"], "attention");
            assert_eq!(bridge["review_required"], true);
            assert_eq!(bridge["review_expired"], false);
            assert_eq!(bridge["reason_code"], "import_preview_required");
            assert_eq!(bridge["kind"], "body_confirmation_required");
            assert_eq!(bridge["import_id"], "20261121_181500");
            assert_eq!(bridge["retryable"], false);
            assert_eq!(bridge["device_noun"], "computer");
            assert_eq!(bridge["size_bytes"], 18);

            // Version bump does NOT auto-retry ready_to_review, and pending+unknown/eligible preserves bytes.
            h.peer.set_route(
                "/api/system/status",
                200,
                json!({"version":{"current":"2.0.55"}})
                    .to_string()
                    .into_bytes(),
            );
            for keep_body in [
                br#"{"status":"pending","saved_start":{"active":false,"state":"eligible"}}"#
                    .to_vec(),
                br#"{"status":"pending","saved_start":{"active":false,"state":"unknown"}}"#
                    .to_vec(),
                br#"{"status":"pending"}"#.to_vec(),
                b"not-json".to_vec(),
            ] {
                h.peer
                    .set_route("/app/import/api/20261121_181500", 200, keep_body);
                h.pass().await;
                assert_eq!(h.saves(), 1, "ready_to_review must never auto-reupload");
                assert_eq!(h.handle.document().ready_to_review, 1);
                assert!(
                    h.handle
                        .root
                        .as_ref()
                        .unwrap()
                        .join(&id)
                        .join("payload")
                        .exists()
                );
            }
            for transient in [400u16, 429, 500, 503] {
                h.peer
                    .set_route("/app/import/api/20261121_181500", transient, b"{}".to_vec());
                h.pass().await;
                assert_eq!(h.handle.document().ready_to_review, 1);
                assert!(!h.handle.document().rows[0].review_expired);
            }

            // 2. 404 marks review expired keeping local bytes; send_again clears saved state & re-uploads without double-counting.
            h.peer
                .set_route("/app/import/api/20261121_181500", 404, b"{}".to_vec());
            h.pass().await;
            let expired_doc = h.handle.document();
            assert_eq!(expired_doc.rows[0].state, "review expired");
            assert!(expired_doc.rows[0].review_expired);
            assert!(expired_doc.rows[0].can_retry);
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id)
                    .join("payload")
                    .exists()
            );
            let expired_bridge = h.handle.handoff_status(&id);
            assert_eq!(expired_bridge["state"], "attention");
            assert_eq!(expired_bridge["review_required"], false);
            assert_eq!(expired_bridge["review_expired"], true);
            assert_eq!(expired_bridge["retryable"], true);

            assert!(h.handle.send_again(id.clone()).await);
            // Counter must NOT double-count on send_again!
            let crate::source_intake::IntakeFile::Rows(intake_after) =
                crate::source_intake::read_rows(&crate::source_intake::path(&state_dir))
            else {
                panic!("intake rows expected");
            };
            assert_eq!(intake_after.imports.total, 1);

            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_190000/export.zip","timestamp":"20261121_190000"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                409,
                br#"{"reason_code":"import_preview_required","kind":"body_confirmation_required","import_id":"20261121_190000"}"#
                    .to_vec(),
            );
            h.pass().await;
            assert_eq!(h.saves(), 2);
            assert_eq!(
                h.handle.document().rows[0].import_id.as_deref(),
                Some("20261121_190000")
            );

            // 3. Reconcile 200 running deletes local bytes, writes durable 24h receipt, and updates handoff lookup to delivered.
            h.peer.set_route(
                "/app/import/api/20261121_190000",
                200,
                br#"{"status":"running","saved_start":{"active":true,"state":"consumed"}}"#
                    .to_vec(),
            );
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert!(!h.handle.root.as_ref().unwrap().join(&id).exists());
            assert_eq!(h.handle.handoff_status(&id)["state"], "delivered");
            assert_eq!(h.handle.handoff_status(&id)["import_id"], "20261121_190000");
            let doc = h.handle.document();
            assert_eq!(doc.recent_receipts.len(), 1);
            assert_eq!(doc.recent_receipts[0].filename, "export.zip");
            assert_eq!(
                doc.recent_receipts[0].import_id.as_deref(),
                Some("20261121_190000")
            );

            // Persists across reload and expires after 24h.
            let reloaded = ImportHandle::new(
                h.handle.root.clone().unwrap(),
                &h.client,
                SyncTrigger::default(),
            )
            .with_clock_ms({
                let now_read = Arc::clone(&now);
                Arc::new(move || now_read.load(Ordering::Acquire))
            });
            reloaded.restore().unwrap();
            assert_eq!(reloaded.document().recent_receipts.len(), 1);
            now.fetch_add(RECENT_WINDOW_MS + 1000, Ordering::AcqRel);
            assert!(reloaded.document().recent_receipts.is_empty());

            // 4. plan_failed enters non-retryable needs_attention and is NEVER retried on version bump.
            let bad_id = h.add(b"unknown binary").await;
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_200000/note.txt","timestamp":"20261121_200000"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                409,
                br#"{"reason_code":"import_preview_required","kind":"plan_failed","import_id":"20261121_200000","error":"could not read file"}"#
                    .to_vec(),
            );
            h.pass().await;
            let doc = h.handle.document();
            assert_eq!(doc.ready_to_review, 0);
            assert_eq!(doc.needs_attention, 1);
            assert_eq!(doc.rows[0].state, "needs attention");
            assert!(!doc.rows[0].can_retry);
            let saves_before_bump = h.saves();
            h.peer.set_route(
                "/api/system/status",
                200,
                json!({"version":{"current":"2.0.99"}})
                    .to_string()
                    .into_bytes(),
            );
            h.pass().await;
            assert_eq!(
                h.saves(),
                saves_before_bump,
                "plan_failed must not retry on version bump"
            );
            assert_eq!(h.handle.remove(bad_id, true).await, "removed");

            // 5. Re-pair to Journal B resets Journal A ready_to_review item to Pending & never queries Journal B with Journal A's import_id.
            let repair_id = h.add(b"cross journal").await;
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_210000/note.txt","timestamp":"20261121_210000"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                409,
                br#"{"reason_code":"import_preview_required","kind":"body_confirmation_required","import_id":"20261121_210000"}"#
                    .to_vec(),
            );
            h.pass().await;
            assert_eq!(h.handle.document().ready_to_review, 1);

            let peer_b = PrivateLinkPeer::start().await;
            peer_b.set_route(
                "/api/system/status",
                200,
                json!({"version":{"current":"2.0.55"}})
                    .to_string()
                    .into_bytes(),
            );
            peer_b.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_210500/note.txt","timestamp":"20261121_210500"}"#
                    .to_vec(),
            );
            peer_b.set_route(
                "/app/import/api/start",
                200,
                br#"{"status":"ok","task_id":"t2","import_id":"20261121_210500"}"#.to_vec(),
            );
            let session_b = start_private_link_session(
                &h.temp.path().join("link-b"),
                peer_b.credential(),
                "desktop",
            )
            .await
            .unwrap();
            h.client.prepare_new_owner();
            h.client.install_capability(session_b.capability());
            // Old Journal A receipts must be hidden on Journal B.
            assert!(h.handle.document().recent_receipts.is_empty());
            h.pass().await;
            assert!(
                !peer_b
                    .requests()
                    .iter()
                    .any(|r| r.path == "/app/import/api/20261121_210000"),
                "must NEVER query Journal B with Journal A's import_id"
            );
            assert!(h.handle.document().rows.is_empty());
            assert!(!h.handle.root.as_ref().unwrap().join(&repair_id).exists());
            assert_eq!(h.handle.document().recent_receipts.len(), 1);
            assert_eq!(
                h.handle.document().recent_receipts[0].import_id.as_deref(),
                Some("20261121_210500")
            );

            let _ = session_b.shutdown().await;
            peer_b.shutdown().await;
            h.close().await;
        }

        #[tokio::test]
        async fn reconcile_403_halts_batch_records_refusal_and_keeps_bytes() {
            let h = Harness::new().await;
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_220000/note.txt","timestamp":"20261121_220000"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                409,
                br#"{"reason_code":"import_preview_required","kind":"confirmation_required","import_id":"20261121_220000"}"#
                    .to_vec(),
            );
            let id1 = h.add(b"first").await;
            let id2 = h.add(b"second").await;
            h.pass().await;
            assert_eq!(h.handle.document().ready_to_review, 2);

            h.peer
                .set_route("/app/import/api/20261121_220000", 403, b"{}".to_vec());
            h.handle.reconcile_reviews(&h.client, &h.running).await;
            assert!(h.client.is_revoked());
            assert_eq!(h.handle.take_auth_status(), Some(403));
            let gets = h
                .peer
                .requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path == "/app/import/api/20261121_220000")
                .count();
            assert_eq!(gets, 1, "403 must halt reconcile batch on first refusal");
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id1)
                    .join("payload")
                    .exists()
            );
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id2)
                    .join("payload")
                    .exists()
            );
            h.close().await;
        }

        #[tokio::test]
        async fn high_cap_handoff_lookup_survives_batch_above_ui_recent_cap() {
            let h = Harness::new().await;
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/20261121_230000/f.txt","timestamp":"20261121_230000"}"#
                    .to_vec(),
            );
            h.peer.set_route(
                "/app/import/api/start",
                200,
                br#"{"status":"ok","task_id":"task","import_id":"20261121_230000"}"#.to_vec(),
            );
            let mut ids = Vec::new();
            for i in 0..8 {
                let p = h.temp.path().join(format!("batch-{i}.txt"));
                fs::write(&p, format!("bytes-{i}")).unwrap();
                let handoff: Handoff =
                    serde_json::from_value(json!({"area":"quick","context":{}})).unwrap();
                let res = h
                    .handle
                    .add_open_files(
                        vec![(format!("batch-{i}.txt"), File::open(&p).unwrap())],
                        handoff,
                    )
                    .await;
                ids.push(res["items"][0]["item"].as_str().unwrap().to_owned());
            }
            h.pass().await;
            // UI recent receipts capped at 5 newest.
            assert_eq!(h.handle.document().recent_receipts.len(), 5);
            // All 8 handoff items still resolve as delivered (never gone).
            for id in &ids {
                assert_eq!(h.handle.handoff_status(id)["state"], "delivered");
            }
            h.close().await;
        }

        #[tokio::test]
        async fn active_delivery_holds_sleep_inhibitor_and_releases_on_every_exit() {
            use crate::sleep_inhibit::{
                LockKind, SleepInhibit,
                tests::{FakeBackend, FakeOutcome},
            };

            let mut h = Harness::new().await;
            let backend = FakeBackend::new(|_| FakeOutcome::Allow);
            let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));
            h.handle = h.handle.with_sleep_inhibit(sleep);

            // Idle pass with no items takes no inhibitor locks.
            h.pass().await;
            assert!(backend.called_kinds().is_empty());
            assert_eq!(backend.active_count(), 0);

            // Active delivery holds both delay and sleep:idle block while /save is in flight.
            let _id = h.add(b"sleep hold bytes").await;
            let gate = Arc::new(AtomicBool::new(false));
            h.peer.set_pending_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
                gate.clone(),
                false,
            );
            let handle = h.handle.clone();
            let client = h.client.clone();
            let running = h.running.clone();
            let task = tokio::spawn(async move {
                handle.deliver(&client, &running).await;
            });
            h.wait_save().await;
            assert_eq!(
                backend.active_kinds(),
                vec![LockKind::SleepDelay, LockKind::SleepIdleBlock]
            );
            gate.store(true, Ordering::Release);
            h.peer.notify_response_gates();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(backend.active_count(), 0);

            // Transient failure and client refusal both release their locks on exit.
            let _id2 = h.add(b"fail exit bytes").await;
            h.peer
                .set_route("/app/import/api/save", 500, br#"{"error":"busy"}"#.to_vec());
            h.pass().await;
            assert_eq!(backend.active_count(), 0);

            h.peer.set_route(
                "/app/import/api/save",
                400,
                br#"{"reason_code":"unsupported_type","error":"no"}"#.to_vec(),
            );
            h.pass().await;
            assert_eq!(backend.active_count(), 0);
            h.close().await;
        }

        #[tokio::test]
        async fn prepare_for_sleep_pauses_in_flight_delivery_releases_holds_and_resumes_on_wake() {
            use crate::sleep_inhibit::{
                LockKind, SleepInhibit,
                tests::{FakeBackend, FakeOutcome},
            };

            let mut h = Harness::new().await;
            let backend = FakeBackend::new(|_| FakeOutcome::Allow);
            let sleep = SleepInhibit::with_backend(backend.clone(), Duration::from_secs(1));
            h.handle = h.handle.with_sleep_inhibit(sleep.clone());

            let id = h.add(b"pause across sleep").await;
            let gate = Arc::new(AtomicBool::new(false));
            h.peer.set_pending_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
                gate.clone(),
                false,
            );
            let handle = h.handle.clone();
            let client = h.client.clone();
            let running = h.running.clone();
            let task = tokio::spawn(async move {
                handle.deliver(&client, &running).await;
            });
            h.wait_save().await;
            assert!(h.handle.document().sending);
            assert_eq!(
                backend.active_kinds(),
                vec![LockKind::SleepDelay, LockKind::SleepIdleBlock]
            );

            // Owner initiates sleep: delivery pauses, drops its locks, keeps item waiting.
            sleep.on_prepare_for_sleep(true, || {}).await;
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(backend.active_count(), 0);
            assert_eq!(h.starts(), 0);
            let paused_doc = h.handle.document();
            assert!(!paused_doc.sending);
            assert_eq!(paused_doc.needs_attention, 0);
            assert_eq!(paused_doc.rows.len(), 1);
            assert_eq!(paused_doc.rows[0].state, "waiting");
            assert!(
                h.handle
                    .root
                    .as_ref()
                    .unwrap()
                    .join(&id)
                    .join("payload")
                    .exists()
            );

            // While sleeping, another pass neither sends nor acquires locks.
            let saves_while_paused = h.saves();
            let calls_while_paused = backend.called_kinds().len();
            h.pass().await;
            assert_eq!(h.saves(), saves_while_paused);
            assert_eq!(backend.called_kinds().len(), calls_while_paused);

            // Release the paused route and wake: delivery resumes cleanly and finishes.
            gate.store(true, Ordering::Release);
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
            );
            let woke = Arc::new(AtomicBool::new(false));
            let woke_flag = woke.clone();
            sleep
                .on_prepare_for_sleep(false, move || woke_flag.store(true, Ordering::Release))
                .await;
            assert!(woke.load(Ordering::Acquire));

            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(h.starts(), 1);
            assert_eq!(backend.active_count(), 0);
            h.close().await;
        }

        #[tokio::test]
        async fn polkit_refusal_degrades_cleanly_during_real_import_delivery() {
            use crate::sleep_inhibit::{
                LockKind, SleepInhibit,
                tests::{FakeBackend, FakeOutcome},
            };

            let mut h = Harness::new().await;
            let fallback_backend = FakeBackend::new(|kind| match kind {
                LockKind::SleepIdleBlock => FakeOutcome::Refuse("AccessDenied"),
                _ => FakeOutcome::Allow,
            });
            h.handle = h.handle.with_sleep_inhibit(SleepInhibit::with_backend(
                fallback_backend.clone(),
                Duration::from_secs(1),
            ));
            let _ = h.add(b"fallback idle lock").await;
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(
                fallback_backend.called_kinds(),
                vec![
                    LockKind::SleepDelay,
                    LockKind::SleepIdleBlock,
                    LockKind::IdleBlock,
                ]
            );
            assert_eq!(fallback_backend.active_count(), 0);

            let denied_backend = FakeBackend::new(|_| FakeOutcome::Refuse("AccessDenied"));
            h.handle = h.handle.with_sleep_inhibit(SleepInhibit::with_backend(
                denied_backend.clone(),
                Duration::from_secs(1),
            ));
            let _ = h.add(b"all locks denied").await;
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(denied_backend.active_count(), 0);
            h.close().await;
        }

        #[tokio::test]
        async fn live_logind_holds_during_import_delivery_and_releases_on_prepare_for_sleep() {
            use crate::sleep_inhibit::{
                SleepInhibit,
                tests::{
                    list_logind_inhibitors_for_pid, lock_real_logind_test,
                    wait_logind_inhibitors_cleared,
                },
            };

            let _guard = lock_real_logind_test().await;
            let pid = std::process::id();
            wait_logind_inhibitors_cleared(pid).await;

            let mut h = Harness::new().await;
            let sleep = SleepInhibit::logind();
            h.handle = h.handle.with_sleep_inhibit(sleep.clone());

            let _id = h.add(b"live logind delivery bytes").await;
            let gate = Arc::new(AtomicBool::new(false));
            h.peer.set_pending_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
                gate.clone(),
                false,
            );
            let handle = h.handle.clone();
            let client = h.client.clone();
            let running = h.running.clone();
            let task = tokio::spawn(async move {
                handle.deliver(&client, &running).await;
            });
            h.wait_save().await;

            let during = list_logind_inhibitors_for_pid(pid).await;
            println!("live import delivery logind inhibitors (pid {pid}): {during:?}");
            assert!(
                during
                    .iter()
                    .any(|(what, who, _, mode, _, _)| what == "sleep"
                        && who == text!("linux.metadata.name")
                        && mode == "delay"),
                "expected sleep delay lock during live import delivery, got {during:?}"
            );
            assert!(
                during.iter().any(|(what, who, _, mode, _, _)| {
                    (what == "sleep:idle" || what == "idle")
                        && who == text!("linux.metadata.name")
                        && mode == "block"
                }),
                "expected sleep:idle or idle block lock during live import delivery, got {during:?}"
            );

            // Simulate PrepareForSleep(true) while /save is still gated: delivery pauses
            // and logind clears every held lock immediately.
            sleep.on_prepare_for_sleep(true, || {}).await;
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            wait_logind_inhibitors_cleared(pid).await;
            let paused = h.handle.document();
            assert_eq!(paused.rows.len(), 1);
            assert_eq!(paused.rows[0].state, "waiting");

            // Simulate PrepareForSleep(false) and let delivery complete cleanly.
            gate.store(true, Ordering::Release);
            h.peer.notify_response_gates();
            h.peer.set_route(
                "/app/import/api/save",
                200,
                br#"{"status":"staged","path":"/imports/saved/note.txt","timestamp":"saved"}"#
                    .to_vec(),
            );
            sleep.on_prepare_for_sleep(false, || {}).await;
            h.pass().await;
            assert!(h.handle.document().rows.is_empty());
            assert_eq!(h.starts(), 1);
            wait_logind_inhibitors_cleared(pid).await;
            h.close().await;
        }
    }
}
