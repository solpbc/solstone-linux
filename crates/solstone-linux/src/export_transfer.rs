// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Native journal export download manager for Linux.
//!
//! Streams ready journal export archives over the paired private link into a
//! sibling `.solstone-export-<id>.part` file (`0600`, `O_NOFOLLOW`), supports
//! 64-bit `Range` + `If-Range` resume, holds `SleepInhibit` while transferring
//! (pausing cleanly on `PrepareForSleep`), pauses on LAN-to-relay switch,
//! verifies full-file SHA-256 on disk before atomically renaming into the
//! owner's chosen destination, and preserves preexisting destination files on
//! failure or cancellation.

use std::{
    fs::{self, File},
    future::Future,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::{
    private_link::{
        ExportDownloadStream, ExportOpenError, PrivateLinkCapability, is_export_download_path,
    },
    sleep_inhibit::{AcquireOutcome, SleepInhibit},
    upload::UploadClient,
};

const PLATFORM: &str = "linux";
const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
const HASH_READ_BUFFER_BYTES: usize = 256 * 1024;
const FILE_MANAGER_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct OpenedExportStream {
    pub(crate) status: u16,
    pub(crate) content_range: Option<String>,
    pub(crate) etag: Option<String>,
    pub(crate) transport: &'static str,
    pub(crate) body: Box<dyn ExportByteStream>,
}

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait ExportByteStream: Send {
    fn next_chunk<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<Vec<u8>>, ()>>;

    fn read_bounded<'a>(&'a mut self, max_bytes: usize) -> BoxFuture<'a, Vec<u8>>;
}

struct LiveExportByteStream {
    inner: ExportDownloadStream,
}

impl ExportByteStream for LiveExportByteStream {
    fn next_chunk<'a>(&'a mut self) -> BoxFuture<'a, Result<Option<Vec<u8>>, ()>> {
        Box::pin(async move { self.inner.next_chunk().await })
    }

    fn read_bounded<'a>(&'a mut self, max_bytes: usize) -> BoxFuture<'a, Vec<u8>> {
        Box::pin(async move { self.inner.read_bounded(max_bytes).await })
    }
}

pub(crate) trait ExportStreamOpener: Send + Sync {
    fn current_link_token(&self) -> Option<LinkToken>;

    fn open<'a>(
        &'a self,
        expected_link: &'a LinkToken,
        download_path: &'a str,
        range_header: Option<String>,
        if_range_header: Option<String>,
    ) -> BoxFuture<'a, Result<OpenedExportStream, ExportOpenError>>;
}

#[derive(Clone)]
pub(crate) enum LinkToken {
    Live(PrivateLinkCapability),
    #[cfg(test)]
    Test(u64),
}

impl LinkToken {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Live(a), Self::Live(b)) => a.same_instance(b),
            #[cfg(test)]
            (Self::Test(a), Self::Test(b)) => a == b,
            #[cfg(test)]
            _ => false,
        }
    }
}

struct UploadExportOpener {
    upload: Arc<UploadClient>,
}

impl ExportStreamOpener for UploadExportOpener {
    fn current_link_token(&self) -> Option<LinkToken> {
        self.upload.capability().map(LinkToken::Live)
    }

    fn open<'a>(
        &'a self,
        expected_link: &'a LinkToken,
        download_path: &'a str,
        range_header: Option<String>,
        if_range_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenedExportStream, ExportOpenError>> + Send + 'a>>
    {
        Box::pin(async move {
            let Some(current) = self.upload.capability() else {
                return Err(ExportOpenError::Forbidden);
            };
            let current_token = LinkToken::Live(current.clone());
            if !current_token.same_as(expected_link) {
                return Err(ExportOpenError::Forbidden);
            }
            let stream = current
                .export_download_open(
                    download_path,
                    range_header.as_deref(),
                    if_range_header.as_deref(),
                )
                .await?;
            Ok(OpenedExportStream {
                status: stream.status.as_u16(),
                content_range: stream.content_range.clone(),
                etag: stream.etag.clone(),
                transport: stream.transport,
                body: Box::new(LiveExportByteStream { inner: stream }),
            })
        })
    }
}

struct NoopExportOpener;

impl ExportStreamOpener for NoopExportOpener {
    fn current_link_token(&self) -> Option<LinkToken> {
        None
    }

    fn open<'a>(
        &'a self,
        _expected_link: &'a LinkToken,
        _download_path: &'a str,
        _range_header: Option<String>,
        _if_range_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenedExportStream, ExportOpenError>> + Send + 'a>>
    {
        Box::pin(async { Err(ExportOpenError::TransportUnavailable) })
    }
}

type SpaceProbe = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
struct ExportStatusReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    export_id: Option<String>,
    phase: String,
    bytes_written: u64,
    total_bytes: u64,
    destination_display: Option<String>,
    pause_reason: Option<String>,
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_bytes: Option<u64>,
    can_reveal: bool,
    verified: bool,
    platform: &'static str,
}

impl ExportStatusReply {
    fn idle(id: Option<String>) -> Self {
        Self {
            id,
            export_id: None,
            phase: "idle".to_owned(),
            bytes_written: 0,
            total_bytes: 0,
            destination_display: None,
            pause_reason: None,
            error_code: None,
            required_bytes: None,
            available_bytes: None,
            can_reveal: false,
            verified: false,
            platform: PLATFORM,
        }
    }

    fn with_id(&self, id: Option<String>) -> Self {
        let mut copy = self.clone();
        copy.id = id;
        copy
    }
}

#[derive(Deserialize)]
struct StartPayload {
    #[serde(default)]
    id: Option<String>,
    export_id: String,
    download_path: String,
    #[serde(default)]
    filename: Option<String>,
    size_bytes: u64,
    sha256: String,
    etag: String,
    #[serde(default)]
    destination_path: String,
}

#[derive(Deserialize)]
struct ControlPayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    export_id: Option<String>,
}

#[derive(Deserialize)]
struct DeviceSpacePayload {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    destination_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlSignal {
    Run,
    PauseOwner,
    Cancel,
}

struct ActiveTransfer {
    generation: u64,
    export_id: String,
    download_path: String,
    size_bytes: u64,
    sha256: String,
    etag: String,
    destination_path: PathBuf,
    part_path: PathBuf,
    destination_display: String,
    link_token: LinkToken,
    initial_transport: Option<&'static str>,
    allow_relay: bool,
    control_tx: watch::Sender<ControlSignal>,
    cancel_flag: Arc<AtomicBool>,
    publish_lock: Arc<Mutex<()>>,
}

#[cfg(test)]
type VerifyChunkHook = Arc<dyn Fn(u64) + Send + Sync>;

struct ExportInner {
    opener: Arc<dyn ExportStreamOpener>,
    sleep: SleepInhibit,
    space_probe: SpaceProbe,
    state: Mutex<ExportState>,
    #[cfg(test)]
    verify_chunk_hook: Mutex<Option<VerifyChunkHook>>,
}

struct ExportState {
    next_generation: u64,
    snapshot: ExportStatusReply,
    active: Option<ActiveTransfer>,
    saved_export_id: Option<String>,
    saved_destination: Option<PathBuf>,
    saved_display: Option<String>,
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            next_generation: 0,
            snapshot: ExportStatusReply::idle(None),
            active: None,
            saved_export_id: None,
            saved_destination: None,
            saved_display: None,
        }
    }
}

#[derive(Clone)]
pub struct ExportTransferHandle {
    inner: Arc<ExportInner>,
}

impl Default for ExportTransferHandle {
    fn default() -> Self {
        Self {
            inner: Arc::new(ExportInner {
                opener: Arc::new(NoopExportOpener),
                sleep: SleepInhibit::disabled(),
                space_probe: Arc::new(statvfs_available_bytes),
                state: Mutex::new(ExportState::default()),
                #[cfg(test)]
                verify_chunk_hook: Mutex::new(None),
            }),
        }
    }
}

impl ExportTransferHandle {
    pub(crate) fn new(upload: Arc<UploadClient>, sleep: SleepInhibit) -> Self {
        Self {
            inner: Arc::new(ExportInner {
                opener: Arc::new(UploadExportOpener { upload }),
                sleep,
                space_probe: Arc::new(statvfs_available_bytes),
                state: Mutex::new(ExportState::default()),
                #[cfg(test)]
                verify_chunk_hook: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_opener_for_test(
        opener: Arc<dyn ExportStreamOpener>,
        sleep: SleepInhibit,
        space_probe: SpaceProbe,
    ) -> Self {
        Self {
            inner: Arc::new(ExportInner {
                opener,
                sleep,
                space_probe,
                state: Mutex::new(ExportState::default()),
                verify_chunk_hook: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    fn set_verify_chunk_hook(&self, hook: Option<VerifyChunkHook>) {
        *self
            .inner
            .verify_chunk_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    pub async fn start(&self, payload: &str) -> String {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return serialize_reply(&failed_reply(None, None, "invalid_request"));
        }
        let Ok(req) = serde_json::from_str::<StartPayload>(payload) else {
            return serialize_reply(&failed_reply(None, None, "invalid_request"));
        };
        if !valid_export_id(&req.export_id)
            || req.download_path != format!("/app/export/api/ready/{}/download", req.export_id)
            || !is_export_download_path(&req.download_path)
            || !valid_sha256_hex(&req.sha256)
            || req.size_bytes == 0
            || req.etag.trim().is_empty()
        {
            return serialize_reply(&failed_reply(
                req.id,
                Some(req.export_id),
                "invalid_request",
            ));
        }

        let destination_path = PathBuf::from(&req.destination_path);
        if !destination_path.is_absolute()
            || destination_path.file_name().is_none()
            || destination_path.is_dir()
        {
            return serialize_reply(&failed_reply(
                req.id,
                Some(req.export_id),
                "invalid_destination",
            ));
        }
        let Some(parent) = destination_path.parent() else {
            return serialize_reply(&failed_reply(
                req.id,
                Some(req.export_id),
                "invalid_destination",
            ));
        };
        if !parent.is_dir() {
            return serialize_reply(&failed_reply(
                req.id,
                Some(req.export_id),
                "invalid_destination",
            ));
        }

        let destination_display = destination_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .or_else(|| req.filename.clone().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "solstone-journal.zip".to_owned());

        let (part_path, existing_part_len) = {
            let state = self.lock_state();
            if let Some(prev) = &state.active
                && prev.export_id == req.export_id
                && prev.destination_path == destination_path
            {
                let len = fs::symlink_metadata(&prev.part_path)
                    .ok()
                    .filter(|meta| meta.file_type().is_file())
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                (Some(prev.part_path.clone()), len)
            } else {
                (None, 0)
            }
        };
        let remaining_needed = req.size_bytes.saturating_sub(existing_part_len);
        let required = required_device_bytes(remaining_needed);
        if let Some(available) = (self.inner.space_probe)(parent)
            && available < required
        {
            let mut fail = failed_reply(req.id, Some(req.export_id), "insufficient_device_space");
            fail.total_bytes = req.size_bytes;
            fail.required_bytes = Some(required);
            fail.available_bytes = Some(available);
            fail.destination_display = Some(destination_display);
            return serialize_reply(&fail);
        }

        let Some(link_token) = self.inner.opener.current_link_token() else {
            return serialize_reply(&failed_reply(
                req.id,
                Some(req.export_id),
                "export_requester_revoked",
            ));
        };

        let (generation, reply) = {
            let mut state = self.lock_state();
            if let Some(existing) = &state.active
                && matches!(state.snapshot.phase.as_str(), "transferring" | "verifying")
            {
                if existing.export_id == req.export_id {
                    return serialize_reply(&state.snapshot.with_id(req.id));
                }
                return serialize_reply(&failed_reply(
                    req.id,
                    Some(req.export_id),
                    "export_already_running",
                ));
            }
            let reused_part = if let Some(prev) = state.active.take() {
                prev.cancel_flag.store(true, Ordering::Release);
                let _ = prev.control_tx.send(ControlSignal::Cancel);
                if prev.export_id == req.export_id && prev.destination_path == destination_path {
                    Some(prev.part_path)
                } else {
                    let _ = fs::remove_file(&prev.part_path);
                    None
                }
            } else {
                None
            };
            state.next_generation = state.next_generation.wrapping_add(1);
            let generation = state.next_generation;
            let chosen_part_path = reused_part
                .or(part_path)
                .unwrap_or_else(|| sibling_part_path(parent, &req.export_id, generation));
            let actual_existing_len = fs::symlink_metadata(&chosen_part_path)
                .ok()
                .filter(|meta| meta.file_type().is_file())
                .map(|meta| meta.len())
                .unwrap_or(0);
            let (control_tx, _) = watch::channel(ControlSignal::Run);
            state.active = Some(ActiveTransfer {
                generation,
                export_id: req.export_id.clone(),
                download_path: req.download_path,
                size_bytes: req.size_bytes,
                sha256: req.sha256.to_ascii_lowercase(),
                etag: req.etag,
                destination_path,
                part_path: chosen_part_path,
                destination_display: destination_display.clone(),
                link_token,
                initial_transport: None,
                allow_relay: false,
                control_tx,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                publish_lock: Arc::new(Mutex::new(())),
            });
            state.snapshot = ExportStatusReply {
                id: None,
                export_id: Some(req.export_id),
                phase: "transferring".to_owned(),
                bytes_written: actual_existing_len.min(req.size_bytes),
                total_bytes: req.size_bytes,
                destination_display: Some(destination_display),
                pause_reason: None,
                error_code: None,
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            (generation, state.snapshot.with_id(req.id))
        };

        self.spawn_transfer_attempt(generation);
        serialize_reply(&reply)
    }

    pub fn status(&self, payload: &str) -> String {
        let req = serde_json::from_str::<ControlPayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone());
        let filter_export_id = req.as_ref().and_then(|r| r.export_id.clone());
        let state = self.lock_state();
        if let Some(wanted) = filter_export_id
            && state.snapshot.export_id.as_deref() != Some(wanted.as_str())
        {
            return serialize_reply(&ExportStatusReply::idle(req_id));
        }
        serialize_reply(&state.snapshot.with_id(req_id))
    }

    pub async fn pause(&self, payload: &str) -> String {
        let req = serde_json::from_str::<ControlPayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone());
        let target_id = req.as_ref().and_then(|r| r.export_id.clone());
        let reply = {
            let mut state = self.lock_state();
            if let Some(active) = &state.active
                && target_id.as_deref().is_none_or(|id| id == active.export_id)
                && matches!(state.snapshot.phase.as_str(), "transferring" | "paused")
            {
                let _ = active.control_tx.send(ControlSignal::PauseOwner);
                state.snapshot.phase = "paused".to_owned();
                state.snapshot.pause_reason = Some("owner".to_owned());
                state.snapshot.error_code = None;
            }
            state.snapshot.with_id(req_id)
        };
        serialize_reply(&reply)
    }

    pub async fn resume(&self, payload: &str) -> String {
        let req = serde_json::from_str::<ControlPayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone());
        let target_id = req.as_ref().and_then(|r| r.export_id.clone());
        let (spawn_gen, reply) = {
            let mut state = self.lock_state();
            let can_resume = state.active.as_ref().is_some_and(|active| {
                target_id.as_deref().is_none_or(|id| id == active.export_id)
                    && state.snapshot.phase == "paused"
            });
            if can_resume {
                let was_relay_pause =
                    state.snapshot.pause_reason.as_deref() == Some("relay_switched");
                let part_len = state
                    .active
                    .as_ref()
                    .and_then(|a| fs::symlink_metadata(&a.part_path).ok())
                    .filter(|m| m.file_type().is_file())
                    .map(|m| m.len())
                    .unwrap_or(state.snapshot.bytes_written);
                let next_gen = state.next_generation.wrapping_add(1);
                state.next_generation = next_gen;
                let active_size = state.active.as_mut().map(|active| {
                    active.cancel_flag.store(true, Ordering::Release);
                    let _ = active.control_tx.send(ControlSignal::Cancel);
                    active.generation = next_gen;
                    if was_relay_pause {
                        active.allow_relay = true;
                        active.initial_transport = Some("relay");
                    }
                    let (control_tx, _) = watch::channel(ControlSignal::Run);
                    active.control_tx = control_tx;
                    active.cancel_flag = Arc::new(AtomicBool::new(false));
                    active.publish_lock = Arc::new(Mutex::new(()));
                    active.size_bytes
                });
                if let Some(size_bytes) = active_size {
                    state.snapshot.phase = "transferring".to_owned();
                    state.snapshot.bytes_written = part_len.min(size_bytes);
                    state.snapshot.pause_reason = None;
                    state.snapshot.error_code = None;
                }
                (Some(next_gen), state.snapshot.with_id(req_id))
            } else {
                (None, state.snapshot.with_id(req_id))
            }
        };
        if let Some(generation) = spawn_gen {
            self.spawn_transfer_attempt(generation);
        }
        serialize_reply(&reply)
    }

    pub async fn cancel(&self, payload: &str) -> String {
        let req = serde_json::from_str::<ControlPayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone());
        let target_id = req.as_ref().and_then(|r| r.export_id.clone());

        // Signal cooperative cancellation immediately before waiting on publish_lock
        // so any blocking SHA-256 verification exits on the very next chunk and drops its fd.
        let pending = {
            let state = self.lock_state();
            state
                .active
                .as_ref()
                .filter(|active| target_id.as_deref().is_none_or(|id| id == active.export_id))
                .map(|active| {
                    active.cancel_flag.store(true, Ordering::Release);
                    let _ = active.control_tx.send(ControlSignal::Cancel);
                    (active.generation, Arc::clone(&active.publish_lock))
                })
        };

        let (part_to_remove, reply) = if let Some((gen_to_cancel, publish_lock)) = pending {
            // Lock ordering is always publish_lock -> state (matches verify_and_finalize).
            let _pub_guard = publish_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut state = self.lock_state();
            if state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == gen_to_cancel)
            {
                let active = state.active.take().expect("checked active");
                state.snapshot = ExportStatusReply {
                    id: None,
                    export_id: Some(active.export_id),
                    phase: "cancelled".to_owned(),
                    bytes_written: 0,
                    total_bytes: active.size_bytes,
                    destination_display: Some(active.destination_display),
                    pause_reason: None,
                    error_code: None,
                    required_bytes: None,
                    available_bytes: None,
                    can_reveal: false,
                    verified: false,
                    platform: PLATFORM,
                };
                (Some(active.part_path), state.snapshot.with_id(req_id))
            } else {
                // Worker already finalized before publish_lock was acquired; report truthfully.
                (None, state.snapshot.with_id(req_id))
            }
        } else {
            let state = self.lock_state();
            (None, state.snapshot.with_id(req_id))
        };
        if let Some(part_path) = part_to_remove {
            let _ = fs::remove_file(part_path);
        }
        serialize_reply(&reply)
    }

    pub async fn reveal(&self, payload: &str) -> String {
        let req = serde_json::from_str::<ControlPayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone());
        let target_id = req.as_ref().and_then(|r| r.export_id.clone());
        let (saved_path, display) = {
            let state = self.lock_state();
            let matches = target_id
                .as_deref()
                .is_none_or(|id| state.saved_export_id.as_deref() == Some(id));
            if matches {
                (state.saved_destination.clone(), state.saved_display.clone())
            } else {
                (None, None)
            }
        };
        let revealed = match saved_path {
            Some(path) if path.is_file() => reveal_saved_file(&path).await,
            _ => false,
        };
        serde_json::json!({
            "id": req_id.unwrap_or_default(),
            "revealed": revealed,
            "destination_display": display,
            "platform": PLATFORM,
        })
        .to_string()
    }

    pub fn device_space(&self, payload: &str) -> String {
        let req = serde_json::from_str::<DeviceSpacePayload>(payload).ok();
        let req_id = req.as_ref().and_then(|r| r.id.clone()).unwrap_or_default();
        let probe_path = req
            .as_ref()
            .and_then(|r| r.destination_path.as_deref())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .and_then(|p| {
                if p.is_dir() {
                    Some(p)
                } else {
                    p.parent().map(Path::to_path_buf)
                }
            })
            .filter(|p| p.is_dir())
            .or_else(default_space_probe_dir);
        let free_bytes = probe_path
            .as_deref()
            .and_then(|p| (self.inner.space_probe)(p));
        serde_json::json!({
            "id": req_id,
            "free_bytes": free_bytes,
            "platform": PLATFORM,
        })
        .to_string()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ExportState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn spawn_transfer_attempt(&self, generation: u64) {
        let handle = self.clone();
        tokio::spawn(async move {
            handle.run_transfer_attempt(generation).await;
        });
    }

    async fn run_transfer_attempt(&self, generation: u64) {
        let Some(params) = self.snapshot_active_params(generation) else {
            return;
        };

        let hold = match self
            .inner
            .sleep
            .acquire_with_reason(solstone_i18n::text!("linux.export.savingfromjournal"))
            .await
        {
            AcquireOutcome::Held(hold) => hold,
            AcquireOutcome::Sleeping => {
                self.mark_paused_if_current(generation, "sleep", None);
                return;
            }
        };

        let mut control_rx = params.control_rx.clone();
        let mut sleep_rx = self.inner.sleep.subscribe();
        let sleeping_now = *sleep_rx.borrow();
        if sleeping_now {
            self.mark_paused_if_current(generation, "sleep", None);
            hold.release().await;
            return;
        }

        let mut reset_and_retry_once = false;
        loop {
            if self.check_pairing_lost(generation, &params.link_token) {
                hold.release().await;
                return;
            }
            let control_now = *control_rx.borrow();
            match control_now {
                ControlSignal::Cancel => {
                    let _ = fs::remove_file(&params.part_path);
                    hold.release().await;
                    return;
                }
                ControlSignal::PauseOwner => {
                    self.mark_paused_if_current(generation, "owner", None);
                    hold.release().await;
                    return;
                }
                ControlSignal::Run => {}
            }

            let part_len = fs::symlink_metadata(&params.part_path)
                .ok()
                .filter(|m| m.file_type().is_file())
                .map(|m| m.len())
                .unwrap_or(0);

            if part_len >= params.size_bytes && part_len == params.size_bytes {
                // Already downloaded all bytes on disk; jump straight to verification and rename.
                self.verify_and_finalize(generation, &params).await;
                hold.release().await;
                return;
            }

            let (range_header, if_range_header, requested_offset) =
                if part_len > 0 && part_len < params.size_bytes {
                    (
                        Some(format!("bytes={part_len}-")),
                        Some(params.etag.clone()),
                        part_len,
                    )
                } else {
                    (None, None, 0)
                };

            let opened = tokio::select! {
                res = self.inner.opener.open(
                    &params.link_token,
                    &params.download_path,
                    range_header,
                    if_range_header,
                ) => res,
                _ = wait_control_stop(&mut control_rx) => {
                    let signal = *control_rx.borrow();
                    match signal {
                        ControlSignal::Cancel => {
                            let _ = fs::remove_file(&params.part_path);
                        }
                        ControlSignal::PauseOwner => {
                            self.mark_paused_if_current(generation, "owner", None);
                        }
                        ControlSignal::Run => {}
                    }
                    hold.release().await;
                    return;
                }
                _ = wait_sleep_start(&mut sleep_rx) => {
                    self.mark_paused_if_current(generation, "sleep", None);
                    hold.release().await;
                    return;
                }
            };

            let mut stream = match opened {
                Ok(stream) => stream,
                Err(ExportOpenError::Forbidden) => {
                    self.fail_and_cleanup(generation, "export_requester_revoked");
                    hold.release().await;
                    return;
                }
                Err(ExportOpenError::InvalidPath) => {
                    self.fail_and_cleanup(generation, "invalid_request");
                    hold.release().await;
                    return;
                }
                Err(ExportOpenError::TransportUnavailable) => {
                    self.mark_paused_if_current(generation, "disconnected", None);
                    hold.release().await;
                    return;
                }
            };

            // Enforce LAN-to-relay transition pause unless the owner explicitly resumed on relay.
            if self.record_transport_and_check_relay_pause(generation, stream.transport) {
                self.mark_paused_if_current(generation, "relay_switched", None);
                hold.release().await;
                return;
            }

            match stream.status {
                200 => {}
                206 => {
                    if !matches_content_range(
                        stream.content_range.as_deref(),
                        requested_offset,
                        params.size_bytes,
                    ) {
                        self.mark_paused_if_current(generation, "disconnected", None);
                        hold.release().await;
                        return;
                    }
                    if !matches_strong_etag(stream.etag.as_deref(), &params.etag) {
                        if !reset_and_retry_once {
                            reset_and_retry_once = true;
                            let _ = fs::remove_file(&params.part_path);
                            self.update_progress(generation, 0);
                            continue;
                        }
                        self.fail_and_cleanup(generation, "checksum_mismatch");
                        hold.release().await;
                        return;
                    }
                }
                416 => {
                    if !reset_and_retry_once {
                        reset_and_retry_once = true;
                        let _ = fs::remove_file(&params.part_path);
                        self.update_progress(generation, 0);
                        continue;
                    }
                    self.fail_and_cleanup(generation, "export_range_error");
                    hold.release().await;
                    return;
                }
                410 => {
                    let body = stream.body.read_bounded(MAX_ERROR_BODY_BYTES).await;
                    let code = parse_error_code(&body)
                        .unwrap_or_else(|| "export_invalidated_by_erasure".to_owned());
                    self.fail_and_cleanup(generation, &code);
                    hold.release().await;
                    return;
                }
                404 => {
                    let body = stream.body.read_bounded(MAX_ERROR_BODY_BYTES).await;
                    let code =
                        parse_error_code(&body).unwrap_or_else(|| "export_not_found".to_owned());
                    self.fail_and_cleanup(generation, &code);
                    hold.release().await;
                    return;
                }
                401 | 403 => {
                    self.fail_and_cleanup(generation, "export_requester_revoked");
                    hold.release().await;
                    return;
                }
                _ => {
                    self.mark_paused_if_current(generation, "disconnected", None);
                    hold.release().await;
                    return;
                }
            }

            let start_offset = if stream.status == 206 {
                requested_offset
            } else {
                0
            };

            let mut file = match open_part_file(&params.part_path, start_offset) {
                Ok(file) => file,
                Err(error) => {
                    let code = if is_disk_space_io_error(&error) {
                        "insufficient_device_space"
                    } else {
                        "dest_write_failed"
                    };
                    self.fail_and_cleanup(generation, code);
                    hold.release().await;
                    return;
                }
            };

            let mut written = start_offset;
            self.update_progress(generation, written);

            loop {
                if self.check_pairing_lost(generation, &params.link_token) {
                    drop(file);
                    hold.release().await;
                    return;
                }
                let next = tokio::select! {
                    biased;
                    _ = wait_control_stop(&mut control_rx) => {
                        let _ = file.flush();
                        drop(file);
                        let signal = *control_rx.borrow();
                        match signal {
                            ControlSignal::Cancel => {
                                let _ = fs::remove_file(&params.part_path);
                            }
                            ControlSignal::PauseOwner => {
                                self.mark_paused_if_current(generation, "owner", Some(written));
                            }
                            ControlSignal::Run => {}
                        }
                        hold.release().await;
                        return;
                    }
                    _ = wait_sleep_start(&mut sleep_rx) => {
                        let _ = file.flush();
                        drop(file);
                        self.mark_paused_if_current(generation, "sleep", Some(written));
                        hold.release().await;
                        return;
                    }
                    chunk = stream.body.next_chunk() => chunk,
                };

                match next {
                    Ok(Some(bytes)) => {
                        if bytes.is_empty() {
                            continue;
                        }
                        if written.saturating_add(bytes.len() as u64) > params.size_bytes {
                            drop(file);
                            self.fail_and_cleanup(generation, "export_size_mismatch");
                            hold.release().await;
                            return;
                        }
                        if let Err(error) = file.write_all(&bytes) {
                            drop(file);
                            if is_disk_space_io_error(&error) {
                                self.fail_and_cleanup(generation, "insufficient_device_space");
                            } else {
                                self.fail_and_cleanup(generation, "dest_write_failed");
                            }
                            hold.release().await;
                            return;
                        }
                        written += bytes.len() as u64;
                        self.update_progress(generation, written);
                    }
                    Ok(None) => {
                        let _ = file.flush();
                        break;
                    }
                    Err(()) => {
                        let _ = file.flush();
                        drop(file);
                        self.mark_paused_if_current(generation, "disconnected", Some(written));
                        hold.release().await;
                        return;
                    }
                }
            }

            if written < params.size_bytes {
                drop(file);
                self.mark_paused_if_current(generation, "disconnected", Some(written));
                hold.release().await;
                return;
            }
            if let Err(error) = file.sync_all() {
                drop(file);
                let code = if is_disk_space_io_error(&error) {
                    "insufficient_device_space"
                } else {
                    "dest_write_failed"
                };
                self.fail_and_cleanup(generation, code);
                hold.release().await;
                return;
            }
            drop(file);
            self.verify_and_finalize(generation, &params).await;
            hold.release().await;
            return;
        }
    }

    async fn verify_and_finalize(&self, generation: u64, params: &RunParams) {
        {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return;
            }
            state.snapshot.phase = "verifying".to_owned();
            state.snapshot.bytes_written = params.size_bytes;
            state.snapshot.pause_reason = None;
            state.snapshot.error_code = None;
        }

        let part_path = params.part_path.clone();
        let expected_size = params.size_bytes;
        let expected_sha = params.sha256.clone();
        let cancel_flag = Arc::clone(&params.cancel_flag);
        #[cfg(test)]
        let chunk_hook = self
            .inner
            .verify_chunk_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let verify_result = tokio::task::spawn_blocking(move || {
            hash_and_check_file(
                &part_path,
                expected_size,
                &expected_sha,
                &cancel_flag,
                #[cfg(test)]
                chunk_hook.as_deref(),
            )
        })
        .await;

        match verify_result {
            Ok(Ok(VerifyOutcome::Matched)) => {}
            Ok(Ok(VerifyOutcome::Cancelled)) => {
                // Attempt was cancelled or superseded; its unique part file is
                // owned only by this generation and removed without touching
                // any newer attempt's partial.
                let _ = fs::remove_file(&params.part_path);
                return;
            }
            Ok(Ok(VerifyOutcome::Mismatched)) => {
                self.fail_and_cleanup(generation, "checksum_mismatch");
                return;
            }
            Ok(Err(_)) | Err(_) => {
                if params.cancel_flag.load(Ordering::Acquire) {
                    let _ = fs::remove_file(&params.part_path);
                    return;
                }
                self.fail_and_cleanup(generation, "dest_write_failed");
                return;
            }
        }

        if self.check_pairing_lost(generation, &params.link_token) {
            return;
        }
        if params.cancel_flag.load(Ordering::Acquire)
            || *params.control_rx.borrow() == ControlSignal::Cancel
        {
            let _ = fs::remove_file(&params.part_path);
            return;
        }

        let publish_lock = Arc::clone(&params.publish_lock);
        let cancel_flag = Arc::clone(&params.cancel_flag);
        let inner_for_publish = Arc::clone(&self.inner);
        let part_path = params.part_path.clone();
        let dest_path = params.destination_path.clone();
        let publish_result = tokio::task::spawn_blocking(move || -> io::Result<bool> {
            // Lock ordering is always publish_lock -> state (matches cancel()).
            let _pub_guard = publish_lock.lock().unwrap_or_else(|e| e.into_inner());
            if cancel_flag.load(Ordering::Acquire) {
                let _ = fs::remove_file(&part_path);
                return Ok(false);
            }
            {
                let state = inner_for_publish
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if !state
                    .active
                    .as_ref()
                    .is_some_and(|a| a.generation == generation)
                {
                    let _ = fs::remove_file(&part_path);
                    return Ok(false);
                }
            }
            fs::rename(&part_path, &dest_path)?;
            let mut state = inner_for_publish
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return Ok(false);
            }
            let active = state.active.take().expect("active checked");
            state.saved_export_id = Some(active.export_id.clone());
            state.saved_destination = Some(active.destination_path);
            state.saved_display = Some(active.destination_display.clone());
            state.snapshot = ExportStatusReply {
                id: None,
                export_id: Some(active.export_id),
                phase: "saved".to_owned(),
                bytes_written: active.size_bytes,
                total_bytes: active.size_bytes,
                destination_display: Some(active.destination_display),
                pause_reason: None,
                error_code: None,
                required_bytes: None,
                available_bytes: None,
                can_reveal: true,
                verified: true,
                platform: PLATFORM,
            };
            Ok(true)
        })
        .await;
        match publish_result {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                if params.cancel_flag.load(Ordering::Acquire) {
                    let _ = fs::remove_file(&params.part_path);
                    return;
                }
                let code = if is_disk_space_io_error(&error) {
                    "insufficient_device_space"
                } else {
                    "dest_write_failed"
                };
                self.fail_and_cleanup(generation, code);
            }
            Err(_) => {
                self.fail_and_cleanup(generation, "dest_write_failed");
            }
        }
    }

    fn snapshot_active_params(&self, generation: u64) -> Option<RunParams> {
        let state = self.lock_state();
        let active = state.active.as_ref()?;
        if active.generation != generation {
            return None;
        }
        Some(RunParams {
            download_path: active.download_path.clone(),
            size_bytes: active.size_bytes,
            sha256: active.sha256.clone(),
            etag: active.etag.clone(),
            destination_path: active.destination_path.clone(),
            part_path: active.part_path.clone(),
            link_token: active.link_token.clone(),
            control_rx: active.control_tx.subscribe(),
            cancel_flag: Arc::clone(&active.cancel_flag),
            publish_lock: Arc::clone(&active.publish_lock),
        })
    }

    fn record_transport_and_check_relay_pause(
        &self,
        generation: u64,
        transport: &'static str,
    ) -> bool {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.generation != generation {
            return false;
        }
        if active.initial_transport.is_none() {
            active.initial_transport = Some(transport);
            return false;
        }
        if active.initial_transport == Some("local") && transport == "relay" && !active.allow_relay
        {
            return true;
        }
        false
    }

    fn check_pairing_lost(&self, generation: u64, expected: &LinkToken) -> bool {
        let still_valid = self
            .inner
            .opener
            .current_link_token()
            .is_some_and(|current| current.same_as(expected));
        if still_valid {
            return false;
        }
        let part_to_remove = {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return true;
            }
            let active = state.active.take().expect("active checked");
            active.cancel_flag.store(true, Ordering::Release);
            let _ = active.control_tx.send(ControlSignal::Cancel);
            state.snapshot = ExportStatusReply {
                id: None,
                export_id: Some(active.export_id),
                phase: "cancelled".to_owned(),
                bytes_written: 0,
                total_bytes: active.size_bytes,
                destination_display: Some(active.destination_display),
                pause_reason: None,
                error_code: None,
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            Some(active.part_path)
        };
        if let Some(part) = part_to_remove {
            let _ = fs::remove_file(part);
        }
        true
    }

    fn update_progress(&self, generation: u64, bytes_written: u64) {
        let mut state = self.lock_state();
        if state
            .active
            .as_ref()
            .is_some_and(|a| a.generation == generation)
            && state.snapshot.phase == "transferring"
        {
            state.snapshot.bytes_written = bytes_written;
        }
    }

    fn mark_paused_if_current(&self, generation: u64, reason: &str, bytes_written: Option<u64>) {
        let mut state = self.lock_state();
        if !state
            .active
            .as_ref()
            .is_some_and(|a| a.generation == generation)
        {
            return;
        }
        if matches!(
            state.snapshot.phase.as_str(),
            "saved" | "error" | "cancelled"
        ) {
            return;
        }
        state.snapshot.phase = "paused".to_owned();
        if let Some(written) = bytes_written {
            state.snapshot.bytes_written = written;
        }
        state.snapshot.pause_reason = Some(reason.to_owned());
        state.snapshot.error_code = None;
    }

    fn fail_and_cleanup(&self, generation: u64, error_code: &str) {
        let part_to_remove = {
            let mut state = self.lock_state();
            if !state
                .active
                .as_ref()
                .is_some_and(|a| a.generation == generation)
            {
                return;
            }
            let active = state.active.take().expect("active checked");
            state.snapshot = ExportStatusReply {
                id: None,
                export_id: Some(active.export_id),
                phase: "error".to_owned(),
                bytes_written: 0,
                total_bytes: active.size_bytes,
                destination_display: Some(active.destination_display),
                pause_reason: None,
                error_code: Some(error_code.to_owned()),
                required_bytes: None,
                available_bytes: None,
                can_reveal: false,
                verified: false,
                platform: PLATFORM,
            };
            Some(active.part_path)
        };
        if let Some(part) = part_to_remove {
            let _ = fs::remove_file(part);
        }
    }
}

struct RunParams {
    download_path: String,
    size_bytes: u64,
    sha256: String,
    etag: String,
    destination_path: PathBuf,
    part_path: PathBuf,
    link_token: LinkToken,
    control_rx: watch::Receiver<ControlSignal>,
    cancel_flag: Arc<AtomicBool>,
    publish_lock: Arc<Mutex<()>>,
}

async fn wait_control_stop(rx: &mut watch::Receiver<ControlSignal>) {
    loop {
        if *rx.borrow() != ControlSignal::Run {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn wait_sleep_start(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn open_part_file(part_path: &Path, start_offset: u64) -> io::Result<File> {
    let flags = rustix::fs::OFlags::RDWR
        | rustix::fs::OFlags::CREATE
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW;
    let mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
    let fd =
        rustix::fs::openat(rustix::fs::CWD, part_path, flags, mode).map_err(io::Error::from)?;
    let mut file = File::from(fd);
    if start_offset == 0 {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
    } else {
        file.seek(SeekFrom::Start(start_offset))?;
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerifyOutcome {
    Matched,
    Mismatched,
    Cancelled,
}

fn hash_and_check_file(
    part_path: &Path,
    expected_size: u64,
    expected_sha: &str,
    cancel_flag: &AtomicBool,
    #[cfg(test)] on_chunk: Option<&(dyn Fn(u64) + Send + Sync)>,
) -> io::Result<VerifyOutcome> {
    if cancel_flag.load(Ordering::Acquire) {
        return Ok(VerifyOutcome::Cancelled);
    }
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        part_path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(fd);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_READ_BUFFER_BYTES];
    let mut total = 0u64;
    loop {
        if cancel_flag.load(Ordering::Acquire) {
            return Ok(VerifyOutcome::Cancelled);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
        #[cfg(test)]
        if let Some(hook) = on_chunk {
            hook(total);
        }
        if cancel_flag.load(Ordering::Acquire) {
            return Ok(VerifyOutcome::Cancelled);
        }
    }
    if total != expected_size {
        return Ok(VerifyOutcome::Mismatched);
    }
    let digest = encode_hex(&hasher.finalize());
    if digest.eq_ignore_ascii_case(expected_sha) {
        Ok(VerifyOutcome::Matched)
    } else {
        Ok(VerifyOutcome::Mismatched)
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn sibling_part_path(parent: &Path, export_id: &str, generation: u64) -> PathBuf {
    parent.join(format!(".solstone-export-{export_id}-{generation}.part"))
}

pub(crate) fn required_device_bytes(remaining_bytes: u64) -> u64 {
    let headroom = (remaining_bytes / 100).clamp(1024 * 1024, 64 * 1024 * 1024);
    remaining_bytes.saturating_add(headroom)
}

fn statvfs_available_bytes(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    let bytes = (stat.f_bavail as u128).saturating_mul(stat.f_frsize as u128);
    Some(bytes.min(u64::MAX as u128) as u64)
}

fn default_space_probe_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            let tmp = std::env::temp_dir();
            tmp.is_dir().then_some(tmp)
        })
}

fn valid_export_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn matches_content_range(header: Option<&str>, expected_start: u64, expected_total: u64) -> bool {
    let Some(header) = header else {
        return false;
    };
    let Some(rest) = header.strip_prefix("bytes ") else {
        return false;
    };
    let Some((range_part, total_part)) = rest.split_once('/') else {
        return false;
    };
    let Ok(total) = total_part.trim().parse::<u64>() else {
        return false;
    };
    if total != expected_total {
        return false;
    }
    let Some((start_part, end_part)) = range_part.trim().split_once('-') else {
        return false;
    };
    let (Ok(start), Ok(end)) = (start_part.parse::<u64>(), end_part.parse::<u64>()) else {
        return false;
    };
    start == expected_start && end >= start && end < expected_total
}

fn matches_strong_etag(observed: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() || expected.starts_with("W/") {
        return false;
    }
    observed
        .map(str::trim)
        .is_some_and(|val| !val.starts_with("W/") && val == expected)
}

fn parse_error_code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("code")?.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
}

fn is_disk_space_io_error(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(28 | 122))
}

fn failed_reply(
    id: Option<String>,
    export_id: Option<String>,
    error_code: &str,
) -> ExportStatusReply {
    ExportStatusReply {
        id,
        export_id,
        phase: "error".to_owned(),
        bytes_written: 0,
        total_bytes: 0,
        destination_display: None,
        pause_reason: None,
        error_code: Some(error_code.to_owned()),
        required_bytes: None,
        available_bytes: None,
        can_reveal: false,
        verified: false,
        platform: PLATFORM,
    }
}

fn serialize_reply(reply: &ExportStatusReply) -> String {
    serde_json::to_string(reply).unwrap_or_else(|_| {
        "{\"phase\":\"error\",\"error_code\":\"serialization_error\",\"platform\":\"linux\"}"
            .to_owned()
    })
}

fn file_uri_for_path(path: &Path) -> String {
    let raw = path.as_os_str().as_encoded_bytes();
    let mut uri = String::from("file://");
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &b in raw {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            uri.push(b as char);
        } else {
            uri.push('%');
            uri.push(HEX[(b >> 4) as usize] as char);
            uri.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    uri
}

async fn reveal_saved_file(path: &Path) -> bool {
    let uri = file_uri_for_path(path);
    let dbus_result = tokio::time::timeout(FILE_MANAGER_TIMEOUT, async move {
        let connection = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.FileManager1",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
        )
        .await?;
        let uris = vec![uri];
        proxy.call::<_, _, ()>("ShowItems", &(uris, "")).await
    })
    .await;
    if matches!(dbus_result, Ok(Ok(()))) {
        return true;
    }
    if let Some(parent) = path.parent() {
        let parent = parent.to_path_buf();
        return tokio::task::spawn_blocking(move || open::that(parent).is_ok())
            .await
            .unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedStream {
        chunks: VecDeque<Result<Vec<u8>, ()>>,
    }

    impl ExportByteStream for ScriptedStream {
        fn next_chunk<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, ()>> + Send + 'a>> {
            let item = self.chunks.pop_front();
            Box::pin(async move {
                match item {
                    Some(Ok(bytes)) => Ok(Some(bytes)),
                    Some(Err(())) => Err(()),
                    None => Ok(None),
                }
            })
        }

        fn read_bounded<'a>(
            &'a mut self,
            max_bytes: usize,
        ) -> Pin<Box<dyn Future<Output = Vec<u8>> + Send + 'a>> {
            let mut out = Vec::new();
            while out.len() < max_bytes {
                match self.chunks.pop_front() {
                    Some(Ok(bytes)) => {
                        let take = bytes.len().min(max_bytes - out.len());
                        out.extend_from_slice(&bytes[..take]);
                    }
                    _ => break,
                }
            }
            Box::pin(async move { out })
        }
    }

    struct ScriptedOpener {
        link: Mutex<Option<LinkToken>>,
        seen_ranges: Mutex<Vec<(Option<String>, Option<String>)>>,
        responses: Mutex<VecDeque<Result<OpenedExportStream, ExportOpenError>>>,
        open_count: AtomicUsize,
    }

    impl ScriptedOpener {
        fn new(responses: Vec<Result<OpenedExportStream, ExportOpenError>>) -> Self {
            Self {
                link: Mutex::new(Some(LinkToken::Test(1))),
                seen_ranges: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::from(responses)),
                open_count: AtomicUsize::new(0),
            }
        }
    }

    impl ExportStreamOpener for ScriptedOpener {
        fn current_link_token(&self) -> Option<LinkToken> {
            self.link.lock().unwrap().clone()
        }

        fn open<'a>(
            &'a self,
            expected_link: &'a LinkToken,
            _download_path: &'a str,
            range_header: Option<String>,
            if_range_header: Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<OpenedExportStream, ExportOpenError>> + Send + 'a>>
        {
            self.open_count.fetch_add(1, Ordering::AcqRel);
            self.seen_ranges
                .lock()
                .unwrap()
                .push((range_header, if_range_header));
            let current = self.link.lock().unwrap().clone();
            let resp = if !current.as_ref().is_some_and(|c| c.same_as(expected_link)) {
                Err(ExportOpenError::Forbidden)
            } else {
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(ExportOpenError::TransportUnavailable))
            };
            Box::pin(async move { resp })
        }
    }

    fn sha_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        encode_hex(&h.finalize())
    }

    async fn wait_for_phase(
        handle: &ExportTransferHandle,
        export_id: &str,
        wanted: &str,
    ) -> serde_json::Value {
        for _ in 0..200 {
            let raw =
                handle.status(&serde_json::json!({"id": "st", "export_id": export_id}).to_string());
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if v["phase"].as_str() == Some(wanted) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let raw =
            handle.status(&serde_json::json!({"id": "st", "export_id": export_id}).to_string());
        panic!("timed out waiting for phase {wanted}; last status: {raw}");
    }

    #[tokio::test]
    async fn full_download_verifies_sha256_and_atomically_replaces_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("solstone-journal-2026-11-26.zip");
        fs::write(&dest, b"preexisting-file-bytes").unwrap();

        let payload: Vec<u8> = (0..(128 * 1024 + 37)).map(|i| (i % 251) as u8).collect();
        let sha = sha_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");

        let opener = Arc::new(ScriptedOpener::new(vec![Ok(OpenedExportStream {
            status: 200,
            content_range: None,
            etag: Some(etag.clone()),
            transport: "local",
            body: Box::new(ScriptedStream {
                chunks: VecDeque::from(vec![
                    Ok(payload[..65536].to_vec()),
                    Ok(payload[65536..].to_vec()),
                ]),
            }),
        })]));
        let handle = ExportTransferHandle::with_opener_for_test(
            opener,
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );

        let start_req = serde_json::json!({
            "id": "s1",
            "export_id": "exp_full_1",
            "download_path": "/app/export/api/ready/exp_full_1/download",
            "filename": "solstone-journal-2026-11-26.zip",
            "size_bytes": payload.len(),
            "sha256": sha,
            "etag": etag,
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();
        let reply: serde_json::Value =
            serde_json::from_str(&handle.start(&start_req).await).unwrap();
        assert_eq!(reply["phase"], "transferring");

        let done = wait_for_phase(&handle, "exp_full_1", "saved").await;
        assert_eq!(done["verified"], true);
        assert_eq!(done["can_reveal"], true);
        assert_eq!(done["bytes_written"], payload.len() as u64);
        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert!(!has_any_part_file(tmp.path(), "exp_full_1"));
    }

    #[tokio::test]
    async fn resume_uses_range_and_if_range_and_cancel_preserves_preexisting_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("existing.zip");
        fs::write(&dest, b"keep-me-intact").unwrap();

        let payload: Vec<u8> = (0..(192 * 1024 + 13))
            .map(|i| ((i * 7) % 256) as u8)
            .collect();
        let sha = sha_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 65536usize;

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            }),
        ]));
        let handle = ExportTransferHandle::with_opener_for_test(
            opener.clone(),
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );

        let start_req = serde_json::json!({
            "id": "s2",
            "export_id": "exp_res_1",
            "download_path": "/app/export/api/ready/exp_res_1/download",
            "size_bytes": payload.len(),
            "sha256": sha,
            "etag": etag,
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();
        let _ = handle.start(&start_req).await;
        let paused = wait_for_phase(&handle, "exp_res_1", "paused").await;
        assert_eq!(paused["pause_reason"], "disconnected");
        assert_eq!(paused["bytes_written"], split as u64);
        assert_eq!(fs::read(&dest).unwrap(), b"keep-me-intact");

        let _ = handle
            .resume(&serde_json::json!({"id": "r1", "export_id": "exp_res_1"}).to_string())
            .await;
        let saved = wait_for_phase(&handle, "exp_res_1", "saved").await;
        assert_eq!(saved["verified"], true);
        assert_eq!(fs::read(&dest).unwrap(), payload);

        let ranges = opener.seen_ranges.lock().unwrap().clone();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (None, None));
        assert_eq!(ranges[1], (Some(format!("bytes={split}-")), Some(etag)));
    }

    #[tokio::test]
    async fn lan_to_relay_switch_pauses_then_resumes_on_owner_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("relay.zip");
        let payload: Vec<u8> = (0..(96 * 1024 + 5))
            .map(|i| ((i * 3) % 256) as u8)
            .collect();
        let sha = sha_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 32768usize;

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "relay",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some(etag.clone()),
                transport: "relay",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[split..].to_vec())]),
                }),
            }),
        ]));
        let handle = ExportTransferHandle::with_opener_for_test(
            opener,
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );
        let start_req = serde_json::json!({
            "id": "s3",
            "export_id": "exp_relay_1",
            "download_path": "/app/export/api/ready/exp_relay_1/download",
            "size_bytes": payload.len(),
            "sha256": sha,
            "etag": etag,
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();
        let _ = handle.start(&start_req).await;
        let _ = wait_for_phase(&handle, "exp_relay_1", "paused").await;

        let _ = handle
            .resume(&serde_json::json!({"id": "r1", "export_id": "exp_relay_1"}).to_string())
            .await;
        let relay_paused = wait_for_phase(&handle, "exp_relay_1", "paused").await;
        assert_eq!(relay_paused["pause_reason"], "relay_switched");

        let _ = handle
            .resume(&serde_json::json!({"id": "r2", "export_id": "exp_relay_1"}).to_string())
            .await;
        let saved = wait_for_phase(&handle, "exp_relay_1", "saved").await;
        assert_eq!(saved["verified"], true);
        assert_eq!(fs::read(&dest).unwrap(), payload);
    }

    #[tokio::test]
    async fn erasure_410_and_insufficient_space_preserve_preexisting_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("important.zip");
        fs::write(&dest, b"original-archive").unwrap();

        let opener = Arc::new(ScriptedOpener::new(vec![Ok(OpenedExportStream {
            status: 410,
            content_range: None,
            etag: None,
            transport: "local",
            body: Box::new(ScriptedStream {
                chunks: VecDeque::from(vec![Ok(
                    br#"{"code":"export_invalidated_by_erasure"}"#.to_vec()
                )]),
            }),
        })]));

        // Pre-flight space refusal
        let low_space = ExportTransferHandle::with_opener_for_test(
            opener.clone(),
            SleepInhibit::disabled(),
            Arc::new(|_| Some(1024)),
        );
        let start_req = serde_json::json!({
            "id": "s4",
            "export_id": "exp_err_1",
            "download_path": "/app/export/api/ready/exp_err_1/download",
            "size_bytes": 10 * 1024 * 1024,
            "sha256": "a".repeat(64),
            "etag": "\"e1\"",
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();
        let resp: serde_json::Value =
            serde_json::from_str(&low_space.start(&start_req).await).unwrap();
        assert_eq!(resp["phase"], "error");
        assert_eq!(resp["error_code"], "insufficient_device_space");
        assert_eq!(resp["available_bytes"], 1024);
        assert_eq!(fs::read(&dest).unwrap(), b"original-archive");

        // 410 erasure refusal
        let handle = ExportTransferHandle::with_opener_for_test(
            opener,
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );
        let _ = handle.start(&start_req).await;
        let failed = wait_for_phase(&handle, "exp_err_1", "error").await;
        assert_eq!(failed["error_code"], "export_invalidated_by_erasure");
        assert_eq!(fs::read(&dest).unwrap(), b"original-archive");
        assert!(!has_any_part_file(tmp.path(), "exp_err_1"));
    }

    #[tokio::test]
    async fn resume_206_with_mismatched_etag_resets_part_instead_of_appending() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("etag.zip");
        let payload: Vec<u8> = (0..(64 * 1024 + 19)).map(|i| (i % 241) as u8).collect();
        let sha = sha_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");
        let split = 16384usize;

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload[..split].to_vec()), Err(())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 206,
                content_range: Some(format!(
                    "bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )),
                etag: Some("\"different-etag\"".to_owned()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(vec![0xee; payload.len() - split])]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload.clone())]),
                }),
            }),
        ]));
        let handle = ExportTransferHandle::with_opener_for_test(
            opener,
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );
        let start_req = serde_json::json!({
            "id": "se",
            "export_id": "exp_etag_1",
            "download_path": "/app/export/api/ready/exp_etag_1/download",
            "size_bytes": payload.len(),
            "sha256": sha,
            "etag": etag,
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();
        let _ = handle.start(&start_req).await;
        let _ = wait_for_phase(&handle, "exp_etag_1", "paused").await;
        let _ = handle
            .resume(&serde_json::json!({"id": "re", "export_id": "exp_etag_1"}).to_string())
            .await;
        let saved = wait_for_phase(&handle, "exp_etag_1", "saved").await;
        assert_eq!(saved["verified"], true);
        assert_eq!(fs::read(&dest).unwrap(), payload);
    }

    fn has_any_part_file(dir: &Path, export_id: &str) -> bool {
        let prefix = format!(".solstone-export-{export_id}");
        fs::read_dir(dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".part"))
            })
    }

    #[tokio::test]
    async fn cancel_during_verification_stops_promptly_preserves_destination_and_allows_fresh_save()
    {
        use std::sync::atomic::AtomicU64;

        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("verified-cancel.zip");
        fs::write(&dest, b"keep-original-owner-file").unwrap();

        // 16 MiB payload so verification takes multiple 256 KiB chunks.
        let payload: Vec<u8> = (0..(16 * 1024 * 1024))
            .map(|i| ((i * 29 + (i >> 8)) & 0xff) as u8)
            .collect();
        let sha = sha_hex(&payload);
        let etag = format!("\"sha256-{sha}\"");

        let opener = Arc::new(ScriptedOpener::new(vec![
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload.clone())]),
                }),
            }),
            Ok(OpenedExportStream {
                status: 200,
                content_range: None,
                etag: Some(etag.clone()),
                transport: "local",
                body: Box::new(ScriptedStream {
                    chunks: VecDeque::from(vec![Ok(payload.clone())]),
                }),
            }),
        ]));

        let handle = ExportTransferHandle::with_opener_for_test(
            opener,
            SleepInhibit::disabled(),
            Arc::new(|_| Some(500 * 1024 * 1024)),
        );

        let bytes_hashed_first = Arc::new(AtomicU64::new(0));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let bytes_ref = Arc::clone(&bytes_hashed_first);
        let entered_ref = Arc::clone(&entered_tx);
        let release_ref = Arc::clone(&release_rx);
        handle.set_verify_chunk_hook(Some(Arc::new(move |total| {
            bytes_ref.store(total, Ordering::Release);
            if let Some(tx) = entered_ref.lock().unwrap().take() {
                let _ = tx.send(());
                let _ = release_ref
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
            }
        })));

        let start_req = serde_json::json!({
            "id": "cv1",
            "export_id": "exp_vcan_1",
            "download_path": "/app/export/api/ready/exp_vcan_1/download",
            "size_bytes": payload.len(),
            "sha256": sha,
            "etag": etag,
            "destination_path": dest.to_string_lossy(),
        })
        .to_string();

        let _ = handle.start(&start_req).await;
        // Wait until verification has read the first 256 KiB chunk.
        tokio::time::timeout(Duration::from_secs(10), entered_rx)
            .await
            .expect("verification entered")
            .unwrap();

        // Cancel while verification is paused right after byte 262,144.
        let cancelled: serde_json::Value = serde_json::from_str(
            &handle
                .cancel(&serde_json::json!({"id": "c1", "export_id": "exp_vcan_1"}).to_string())
                .await,
        )
        .unwrap();
        assert_eq!(cancelled["phase"], "cancelled");

        // Unblock the hook; cooperative cancellation must exit immediately without reading the 16 MiB file.
        let _ = release_tx.send(());
        tokio::time::sleep(Duration::from_millis(80)).await;
        let hashed = bytes_hashed_first.load(Ordering::Acquire);
        assert!(
            hashed <= 512 * 1024,
            "cancelled verification must stop promptly at <= 512 KiB, read {hashed} bytes"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"keep-original-owner-file");
        assert!(!has_any_part_file(tmp.path(), "exp_vcan_1"));

        // Clear hook and immediately start a fresh save to the exact same destination; must succeed cleanly.
        handle.set_verify_chunk_hook(None);
        let _ = handle.start(&start_req).await;
        let saved = wait_for_phase(&handle, "exp_vcan_1", "saved").await;
        assert_eq!(saved["verified"], true);
        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert!(!has_any_part_file(tmp.path(), "exp_vcan_1"));
    }

    #[test]
    fn device_space_probe_reports_positive_free_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let handle = ExportTransferHandle::default();
        let raw = handle.device_space(
            &serde_json::json!({
                "id": "sp1",
                "destination_path": tmp.path().to_string_lossy(),
            })
            .to_string(),
        );
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["id"], "sp1");
        assert_eq!(v["platform"], "linux");
        assert!(v["free_bytes"].as_u64().unwrap_or(0) > 0);
    }
}
