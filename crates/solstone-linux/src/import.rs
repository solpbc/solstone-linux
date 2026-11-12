// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! File custody belongs to the service, independently of the window and source file.

use crate::private_file::{atomic_write_bytes, ensure_private_directory};
use crate::private_link::{LinkOutcome, PrivateLinkCapability};
use crate::upload::{CapturedAssociation, UploadClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
pub const EXPLANATION: &str = "adds PDFs, audio, images and text you choose here.";
pub const STORE_ERROR: &str = "couldn't read imports on this device. nothing has been removed. restart the solstone app to try again.";
pub const STEP_ERROR: &str = "this step couldn't finish. try again.";
pub const REMOVE_ERROR: &str = "couldn't remove this. try again.";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Copying,
    Pending,
    Refused,
    Delivered,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Saved {
    binding: String,
    path: String,
    timestamp: String,
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
    pub adding: bool,
    pub interrupted: bool,
    pub store_error: bool,
    pub waiting: u64,
    pub held: bool,
    pub sending: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImportSummary {
    pub waiting: u64,
    pub held: bool,
    pub sending: bool,
    pub recent: String,
}

impl From<ImportDocument> for ImportSummary {
    fn from(document: ImportDocument) -> Self {
        Self {
            waiting: document.waiting,
            held: document.held,
            sending: document.sending,
            recent: document.recent,
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
    addition: Option<String>,
    interrupted: bool,
    cursor: String,
    auth_status: Option<u16>,
}

#[derive(Clone, Default)]
pub struct ImportHandle {
    root: Option<PathBuf>,
    state: Arc<Mutex<Store>>,
    adding: Arc<AtomicBool>,
    client: Weak<UploadClient>,
    trigger: Option<crate::sync::SyncTrigger>,
    status_sink: Option<Arc<dyn Fn(ImportSummary) + Send + Sync>>,
    status_lock: Arc<Mutex<()>>,
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
                state.delivered_journal = value["journal"].as_str().map(str::to_owned);
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
                    item.refusal = Some(local_refusal("this couldn't be read. add it again"));
                    self.persist(&item)?;
                } else if (item.phase == Phase::Pending || item.may_have_saved)
                    && regular_file(&entry.path().join("payload")).is_err()
                {
                    item.phase = Phase::Refused;
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
        if self.root.is_none() || self.adding.swap(true, Ordering::AcqRel) {
            return false;
        }
        let handle = self.clone();
        let result = tokio::task::spawn_blocking(move || {
            handle.restore()?;
            let total = paths.len();
            let mut accepted = 0;
            for (index, path) in paths.into_iter().enumerate() {
                handle
                    .state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .addition = Some(format!("saving on this device: {} of {total}", index + 1));
                if handle.copy_one(Path::new(&path))? {
                    accepted += 1;
                }
            }
            handle
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .addition = Some(if accepted == 0 {
                "nothing was saved on this device.".to_owned()
            } else {
                format!("saved on this device: {accepted}")
            });
            Ok::<_, io::Error>(())
        })
        .await;
        self.adding.store(false, Ordering::Release);
        self.wake();
        matches!(result, Ok(Ok(())))
    }

    fn copy_one(&self, path: &Path) -> io::Result<bool> {
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        let id = new_id()?;
        let dir = root.join(&id);
        ensure_private_directory(&dir).map_err(ioe)?;
        let mut item = Item {
            id,
            name: path
                .file_name()
                .map_or_else(|| "file".to_owned(), |s| s.to_string_lossy().into_owned()),
            mime: file_mime(path)
                .unwrap_or("application/octet-stream")
                .to_owned(),
            size: 0,
            phase: Phase::Copying,
            may_have_saved: false,
            saved: None,
            refusal: None,
        };
        self.publish(item.clone())?;
        let copied = (|| {
            if file_mime(path).is_none() {
                return Err(ioe("unsupported"));
            }
            let mut source = regular_file(path)?;
            let size = source.metadata()?.len();
            if size > FILE_LIMIT {
                return Err(ioe("too_large"));
            }
            if available_space(&dir)? < size.saturating_add(256 * 1024 * 1024) {
                return Err(io::Error::from_raw_os_error(28));
            }
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join("payload.part"))?;
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
            fs::rename(dir.join("payload.part"), dir.join("payload"))?;
            File::open(&dir)?.sync_all()?;
            Ok::<_, io::Error>(copied)
        })();
        let accepted = copied.is_ok();
        match copied {
            Ok(size) => {
                item.size = size;
                item.phase = Phase::Pending;
            }
            Err(error) => {
                item.phase = Phase::Refused;
                let reason = match error.to_string().as_str() {
                    "unsupported" => "your journal can't take this kind of file",
                    "too_large" => "it's larger than your journal takes",
                    _ if error.raw_os_error() == Some(28) => {
                        "there isn't enough room on this device"
                    }
                    _ => "this couldn't be read. add it again",
                };
                item.refusal = Some(local_refusal(reason));
            }
        }
        self.publish(item)?;
        Ok(accepted)
    }

    pub fn document(&self) -> ImportDocument {
        if self.root.is_none() {
            return ImportDocument {
                v: 1,
                recent: "nothing yet.".to_owned(),
                ..ImportDocument::default()
            };
        }
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
        let mut document = ImportDocument {
            v: 1,
            addition: state.addition.clone(),
            adding: self.adding.load(Ordering::Acquire),
            interrupted: state.interrupted,
            store_error: state.failed,
            ..ImportDocument::default()
        };
        if state.failed {
            document.held = true;
            document.interrupted = false;
            document.recent = STORE_ERROR.to_owned();
            return document;
        }
        let current_binding = capability.as_ref().map(binding);
        for item in state.items.values().filter(|i| i.phase != Phase::Delivered) {
            let sending = state.busy.contains(&item.id);
            let refusal = item.refusal.as_ref().filter(|r| {
                r.binding.is_none()
                    || (r.binding == current_binding && r.version.as_deref() == version)
            });
            let attention = refusal.is_some();
            if ready && supported && !attention && item.phase != Phase::Copying {
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
            });
        }
        document.held =
            state.failed || !state.loaded || !document.rows.is_empty() || document.adding;
        document.recent = if capability.is_none() {
            "imports wait on this device until you connect a journal."
        } else if facts.as_ref().is_some_and(|f| f.journal_mark_held) {
            "imports wait until you confirm your journal's mark."
        } else if !ready {
            "your journal stopped accepting this device. imports wait here until you pair again."
        } else if !document.rows.is_empty() && !supported {
            "imports wait until your journal is updated."
        } else if document.sending {
            "sending to your journal."
        } else if document.waiting > 0
            || document.adding
            || state.items.values().any(|i| i.phase == Phase::Copying)
        {
            "waiting to go to your journal."
        } else if !document.rows.is_empty() {
            "imports here won't go on their own. each one says why."
        } else if capability
            .as_ref()
            .is_some_and(|c| state.delivered_journal.as_deref() == Some(c.writer().identity_key()))
        {
            "in your journal."
        } else {
            "nothing yet."
        }
        .to_owned();
        document
    }

    pub fn state_json(&self) -> String {
        serde_json::to_string(&self.document()).expect("import document serializes")
    }

    pub async fn retry(&self, id: String) -> bool {
        let handle = self.clone();
        let ok = tokio::task::spawn_blocking(move || {
            let mut state = handle.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.busy.contains(&id) || state.confirmations.contains_key(&id) || state.failed {
                return Ok::<_, io::Error>(false);
            }
            let Some(item) = state.items.get_mut(&id) else {
                return Ok(false);
            };
            if !item.refusal.as_ref().is_some_and(|r| r.retryable) {
                return Ok(false);
            }
            let mut next = item.clone();
            next.refusal = None;
            next.phase = Phase::Pending;
            handle.persist(&next)?;
            *item = next;
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
            may_have_saved: item.may_have_saved,
            missing: self
                .root
                .as_ref()
                .is_none_or(|root| regular_file(&root.join(id).join("payload")).is_err()),
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

    pub(crate) async fn deliver(&self, client: &Arc<UploadClient>, running: &AtomicBool) {
        if self.root.is_none() || !running.load(Ordering::Acquire) || client.is_revoked() {
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
        if self.document().rows.is_empty() {
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
        let Some(version_result) =
            guarded(client, &association, running, capability.system_status()).await
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
        let mut attempted = 0;
        for id in ids {
            if attempted >= BATCH
                || !running.load(Ordering::Acquire)
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
            let item = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let Some(item) = state.items.get(&id) else {
                    continue;
                };
                if matches!(item.phase, Phase::Copying | Phase::Delivered)
                    || state.busy.contains(&id)
                    || state.confirmations.contains_key(&id)
                    || item.refusal.as_ref().is_some_and(|r| {
                        r.binding.is_none()
                            || (r.binding.as_deref() == Some(&binding) && r.version == version)
                    })
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
                item.refusal = Some(local_refusal("the copy on this device is missing"));
                return self.publish(item);
            }
        };
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
            item.saved = None;
            self.publish(item.clone())?;
            let file = tokio::fs::File::from_std(payload);
            let part = reqwest::multipart::Part::stream_with_length(
                reqwest::Body::wrap_stream(ReaderStream::new(file)),
                item.size,
            )
            .file_name(item.name.clone())
            .mime_str(&item.mime)
            .map_err(ioe)?;
            let form = reqwest::multipart::Form::new()
                .text("client_item_id", item.id.clone())
                .text("imported_via", "desktop")
                .part("file", part);
            let Some(outcome) =
                guarded(client, association, running, capability.import_save(form)).await
            else {
                return Ok(());
            };
            self.auth_answer(client, association, &outcome);
            match parse_reply(outcome, false) {
                Answer::Saved { path, timestamp } => {
                    item.saved = Some(Saved {
                        binding: binding.to_owned(),
                        path,
                        timestamp,
                    });
                    self.publish(item.clone())?;
                    self.state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .saved_current
                        .insert(item.id.clone(), association.clone());
                }
                Answer::Delivered => {
                    return self.delivered(item, capability.writer().identity_key());
                }
                other => return self.retain_answer(item, other, binding, version),
            }
        }
        if !client.association_is_current(association) || !running.load(Ordering::Acquire) {
            return Ok(());
        }
        let saved = item
            .saved
            .as_ref()
            .ok_or_else(|| ioe("missing import save progress"))?;
        let Some(outcome) = guarded(
            client,
            association,
            running,
            capability.import_start(&saved.path, &saved.timestamp),
        )
        .await
        else {
            return Ok(());
        };
        self.auth_answer(client, association, &outcome);
        match parse_reply(outcome, true) {
            Answer::Delivered => self.delivered(item, capability.writer().identity_key()),
            other => self.retain_answer(item, other, binding, version),
        }
    }

    fn retain_answer(
        &self,
        mut item: Item,
        answer: Answer,
        binding: &str,
        version: Option<&str>,
    ) -> io::Result<()> {
        if let Answer::Refused(mut refusal) = answer {
            refusal.binding = Some(binding.to_owned());
            refusal.version = version.map(str::to_owned);
            item.phase = Phase::Refused;
            item.refusal = Some(refusal);
            self.publish(item)?;
        }
        Ok(())
    }

    fn delivered(&self, mut item: Item, journal: &str) -> io::Result<()> {
        item.phase = Phase::Delivered;
        self.publish(item.clone())?;
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| ioe("import store unavailable"))?;
        atomic_write_bytes(
            &root.join("recent.json"),
            &encode(&serde_json::json!({"journal":journal}))?,
        )
        .map_err(ioe)?;
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.delivered_journal = Some(journal.to_owned());
            state.items.remove(&item.id);
            state.saved_current.remove(&item.id);
        }
        self.cleanup(&item.id)
    }
}

struct Sending {
    handle: ImportHandle,
    id: String,
}
impl Drop for Sending {
    fn drop(&mut self) {
        self.handle
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .busy
            .remove(&self.id);
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
    future: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            // A completed delivery answer may end custody even after the association retired.
            biased;
            answer = &mut future => return Some(answer),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if !running.load(Ordering::Acquire) || !client.association_is_current(association) { return None }
            }
        }
    }
}

#[derive(Debug)]
enum Answer {
    Saved { path: String, timestamp: String },
    Delivered,
    Retry,
    Refused(Refusal),
}

fn parse_reply(outcome: LinkOutcome, start: bool) -> Answer {
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
        return Answer::Delivered;
    }
    if status.as_u16() == 200 {
        if start
            && root["status"] == "ok"
            && string("task_id").is_some()
            && string("import_id").is_some()
        {
            return Answer::Delivered;
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
        Some("client_item_conflict" | "nothing_to_import")
    );
    Answer::Refused(Refusal {
        reason: string("error").unwrap_or_else(|| "your journal couldn't accept this.".to_owned()),
        reason_code,
        kind,
        binding: None,
        version: None,
        retryable,
    })
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
                    start
                ),
                Answer::Delivered
            ));
            for status in [200, 409, 500] {
                assert!(!matches!(
                    parse_reply(
                        LinkOutcome::Success {
                            status: StatusCode::from_u16(status).unwrap(),
                            body: replay.to_vec()
                        },
                        start
                    ),
                    Answer::Delivered
                ));
            }
            assert!(matches!(
                parse_reply(
                    LinkOutcome::Success {
                        status: StatusCode::OK,
                        body: b"{\"status\":\"ok\"}".to_vec()
                    },
                    start
                ),
                Answer::Retry
            ));
        }
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
        assert!(reopened.document().interrupted);
        assert_eq!(
            reopened.state.lock().unwrap().items[&interrupted.id].phase,
            Phase::Refused
        );
        assert_eq!(reopened.remove(items[1].id.clone(), false).await, "removed");
        assert!(!root.join(&items[1].id).exists());
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
                text.contains(&id) && text.contains("client_item_id") && text.contains("desktop")
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
    }
}
