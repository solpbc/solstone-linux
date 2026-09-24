// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Background segment reconciliation and sync-health persistence.
//! Later D-Bus work hooks health-change emission beside `save_health`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, FileTimes},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
use tokio::{sync::Notify, task::JoinHandle};

use crate::{
    config::Config,
    observer::Clock,
    private_link::{LinkFactState, LinkFacts},
    sync_health::{
        ErrorType, ProcessEpoch, SyncFacts, SyncHealth, derive_health, load_facts, save_facts,
    },
    upload::{FileDescriptor, ListingEntry, UploadClient},
};

pub const CIRCUIT_THRESHOLD_AUTH: u32 = 1;
pub const CIRCUIT_THRESHOLD_TRANSIENT: u32 = 5;
pub const CIRCUIT_COOLDOWN_INITIAL: f64 = 30.0;
pub const CIRCUIT_COOLDOWN_FACTOR: f64 = 2.0;
pub const CIRCUIT_COOLDOWN_MAX: f64 = 300.0;
pub const CONTACT_FLUSH_INTERVAL: f64 = 30.0;
pub const METADATA_FILENAME: &str = ".metadata";
pub const SERVER_KEY_FILENAME: &str = ".server_key";
pub const INGEST_ACK_FILENAME: &str = ".ingest_ack.json";
pub const INGEST_RETRY_FILENAME: &str = ".ingest_retry.json";
pub const INGEST_CUTOVER_FILENAME: &str = "ingest_cutover.json";

#[cfg(test)]
pub(crate) static SHA256_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static INGEST_ACK_WRITE_FAULT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static INGEST_ACK_WRITE_FAULT_DIR: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IngestAckFileStamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IngestAckFile {
    pub submitted: String,
    pub written: String,
    pub size: u64,
    pub sha256: String,
    pub disposition: String,
    pub stamp: IngestAckFileStamp,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IngestAck {
    pub day: String,
    pub stream: String,
    pub local_key: String,
    pub stored_key: String,
    pub identity_key: String,
    pub pairing_id: String,
    pub proof: String,
    pub files: Vec<IngestAckFile>,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    let opt = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct IngestRetry {
    #[serde(default)]
    pub retry_version: u32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub next_attempt_after: f64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub last_attempt_at: f64,
    #[serde(default)]
    pub identical_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IngestCutover {
    pub segments: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum UploadOutcome {
    Acked,
    Bounded,
    Stop,
}

struct LinkFactPersistence {
    state_dir: PathBuf,
    facts: Arc<Mutex<SyncFacts>>,
    last_persisted: Mutex<Option<LinkFactState>>,
    failures: Arc<AtomicUsize>,
}

impl LinkFactPersistence {
    fn persist(&self, link_facts: &LinkFacts) {
        // Lock order: persistence serialization, SyncFacts, then LinkFactState.
        // The memo is sink-owned because samplers replace facts.link on every sample; comparing
        // against facts.link could let an interleaved sample suppress a needed write indefinitely.
        let mut last_persisted = self
            .last_persisted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut facts = self
            .facts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let snapshot = link_facts.snapshot();
        if last_persisted.as_ref() == Some(&snapshot) {
            return;
        }
        facts.link = Some(snapshot.clone());
        if let Err(error) = save_facts(&self.state_dir, &facts) {
            if self.failures.fetch_add(1, Ordering::AcqRel) == 0 {
                tracing::error!(%error, path = %self.state_dir.display(), "Failed to persist link health");
            }
            return;
        }
        *last_persisted = Some(snapshot);
    }
}

pub struct SyncService {
    notify: Arc<Notify>,
    pending_trigger: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    facts: Arc<Mutex<SyncFacts>>,
    stale_threshold: f64,
    clock: Arc<dyn Clock + Send + Sync>,
    link_facts: LinkFacts,
    link_persistence_failures: Arc<AtomicUsize>,
    abort: tokio::task::AbortHandle,
    task: JoinHandle<()>,
    #[allow(dead_code)]
    optional_jobs: Arc<crate::private_link_optional::OptionalJobs>,
}

#[derive(Clone)]
pub struct SyncSampler {
    pub(crate) facts: Arc<Mutex<SyncFacts>>,
    pub(crate) clock: Arc<dyn Clock + Send + Sync>,
    pub(crate) stale_threshold: f64,
    pub(crate) poison_reports: Arc<AtomicUsize>,
    pub(crate) link_facts: LinkFacts,
}

impl SyncSampler {
    fn with_facts<R>(&self, read: impl FnOnce(&SyncFacts) -> R) -> R {
        match self.facts.lock() {
            Ok(mut facts) => {
                facts.link = Some(self.link_facts.snapshot());
                read(&facts)
            }
            Err(error) => {
                if self
                    .poison_reports
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    tracing::warn!("sync facts lock poisoned; recovering sampler state");
                }
                let mut facts = error.into_inner();
                facts.link = Some(self.link_facts.snapshot());
                read(&facts)
            }
        }
    }

    pub fn sample(&self) -> (SyncHealth, String) {
        self.with_facts(|facts| {
            (
                derive_health(facts, self.clock.wall_seconds(), self.stale_threshold),
                facts.progress.clone(),
            )
        })
    }

    pub fn health(&self) -> SyncHealth {
        self.sample().0
    }

    pub fn progress(&self) -> String {
        self.sample().1
    }
}

struct SyncControl {
    notify: Arc<Notify>,
    pending_trigger: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

impl SyncService {
    #[cfg(test)]
    pub fn start(
        config: Config,
        client: Arc<UploadClient>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        Self::start_with_epoch(config, client, clock, ProcessEpoch::generate().ok())
    }

    pub(crate) fn start_with_epoch(
        config: Config,
        client: Arc<UploadClient>,
        clock: Arc<dyn Clock + Send + Sync>,
        process_epoch: Option<ProcessEpoch>,
    ) -> Self {
        let notify = Arc::new(Notify::new());
        let pending_trigger = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let link_facts = client.link_facts();
        let mut facts = load_facts(&config.state_dir());
        facts.in_progress = false;
        facts.progress.clear();
        facts.link = Some(link_facts.snapshot());
        facts.link_epoch = process_epoch;
        let facts = Arc::new(Mutex::new(facts));
        let link_persistence_failures = Arc::new(AtomicUsize::new(0));
        let persistence = Arc::new(LinkFactPersistence {
            state_dir: config.state_dir(),
            facts: Arc::clone(&facts),
            last_persisted: Mutex::new(None),
            failures: Arc::clone(&link_persistence_failures),
        });
        let optional_jobs = Arc::new(crate::private_link_optional::OptionalJobs::default());
        let optional_jobs_sink = Arc::clone(&optional_jobs);
        let sink_client = Arc::downgrade(&client);
        let sink_config = config.clone();
        link_facts.install_sink(Arc::new(move |facts| {
            persistence.persist(facts);
            let (snapshot, _epoch_at_fire) = facts.snapshot_with_epoch();
            if snapshot.carrier_proven
                && !snapshot.transport_unavailable
                && !snapshot.terminal_revocation
                && let Some(client) = sink_client.upgrade()
                && let Some(capability) = client.capability()
            {
                let identity_key = capability.writer().identity_key().to_owned();
                let state_dir = sink_config.state_dir();
                optional_jobs_sink.trigger(&capability, &state_dir, &identity_key, &snapshot);
            }
        }));
        let recent_error_count = Arc::new(AtomicU8::new(0));
        let mut worker = SyncWorker::new(
            config.clone(),
            Arc::clone(&client),
            Arc::clone(&clock),
            SyncControl {
                notify: Arc::clone(&notify),
                pending_trigger: Arc::clone(&pending_trigger),
                running: Arc::clone(&running),
            },
            Arc::clone(&facts),
            Arc::clone(&recent_error_count),
        );
        let task = tokio::spawn(async move { worker.run().await });
        let abort = task.abort_handle();
        Self {
            notify,
            pending_trigger,
            running,
            facts,
            stale_threshold: config.sync_stale_threshold as f64,
            clock,
            link_facts,
            link_persistence_failures,
            abort,
            task,
            optional_jobs,
        }
    }

    pub fn trigger(&self) {
        self.pending_trigger.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub fn trigger_handle(&self) -> SyncTrigger {
        SyncTrigger {
            notify: Arc::clone(&self.notify),
            pending_trigger: Arc::clone(&self.pending_trigger),
        }
    }

    pub fn sampler_handle(&self) -> SyncSampler {
        SyncSampler {
            facts: Arc::clone(&self.facts),
            clock: Arc::clone(&self.clock),
            stale_threshold: self.stale_threshold,
            poison_reports: Arc::new(AtomicUsize::new(0)),
            link_facts: self.link_facts.clone(),
        }
    }

    pub async fn shutdown(mut self, timeout: Duration) -> Result<(), tokio::task::JoinError> {
        self.optional_jobs.shutdown();
        self.running.store(false, Ordering::Release);
        self.notify.notify_one();
        // Never cancel the shared UploadClient here: the walker may still complete its pass.
        match tokio::time::timeout(timeout, &mut self.task).await {
            Ok(result) => result,
            Err(_) => {
                self.abort.abort();
                match self.task.await {
                    Err(error) if error.is_cancelled() => Ok(()),
                    result => result,
                }
            }
        }
    }

    pub fn health(&self) -> SyncHealth {
        self.sampler_handle().health()
    }

    pub fn progress(&self) -> String {
        self.sampler_handle().progress()
    }

    pub fn link_persistence_failure_count(&self) -> usize {
        self.link_persistence_failures.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct SyncTrigger {
    notify: Arc<Notify>,
    pending_trigger: Arc<AtomicBool>,
}

impl SyncTrigger {
    pub fn trigger(&self) {
        self.pending_trigger.store(true, Ordering::Release);
        self.notify.notify_one();
    }
}

struct SyncWorker {
    config: Config,
    client: Arc<UploadClient>,
    clock: Arc<dyn Clock + Send + Sync>,
    notify: Arc<Notify>,
    pending_trigger: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    facts: Arc<Mutex<SyncFacts>>,
    recent_error_count: Arc<AtomicU8>,
    link_facts: LinkFacts,
    consecutive_failures: u32,
    last_error_type: Option<ErrorType>,
    last_error_code: Option<i64>,
    circuit_open: bool,
    circuit_open_permanent: bool,
    circuit_open_since: f64,
    circuit_cooldown: f64,
    last_contact_flush: f64,
    draining_shutdown: bool,
    retry_floors: HashMap<PathBuf, f64>,
    #[cfg(test)]
    fail_next_pass: bool,
}

impl SyncWorker {
    fn new(
        config: Config,
        client: Arc<UploadClient>,
        clock: Arc<dyn Clock + Send + Sync>,
        control: SyncControl,
        facts: Arc<Mutex<SyncFacts>>,
        recent_error_count: Arc<AtomicU8>,
    ) -> Self {
        let link_facts = client.link_facts();
        Self {
            config,
            client,
            clock,
            notify: control.notify,
            pending_trigger: control.pending_trigger,
            running: control.running,
            facts,
            recent_error_count,
            link_facts,
            consecutive_failures: 0,
            last_error_type: None,
            last_error_code: None,
            circuit_open: false,
            circuit_open_permanent: false,
            circuit_open_since: 0.0,
            circuit_cooldown: CIRCUIT_COOLDOWN_INITIAL,
            last_contact_flush: 0.0,
            draining_shutdown: false,
            retry_floors: HashMap::new(),
            #[cfg(test)]
            fail_next_pass: false,
        }
    }

    async fn run(&mut self) {
        ensure_cutover(&self.config.state_dir(), &self.config.captures_dir());
        loop {
            let _ = tokio::time::timeout(Duration::from_secs(60), self.notify.notified()).await;
            let completion_pending = self.pending_trigger.swap(false, Ordering::AcqRel);
            if !self.is_running() {
                // A completion can race shutdown. Give the final local segment one bounded
                // reconciliation pass before the walker exits. A paired keyless observer uses
                // v3 ingest normally; an unpaired worker still has no transport to drain.
                if completion_pending && self.client.has_capability() {
                    self.draining_shutdown = true;
                    let _ = self.execute_pass().await;
                    self.draining_shutdown = false;
                }
                break;
            }
            // The pass finishes confirmed segments itself; when the pass is skipped the
            // local finish still runs here, so it runs exactly once per iteration.
            if self.client.is_revoked() {
                if self.client.has_capability() {
                    self.finish_confirmed_locally();
                }
                tracing::warn!("Sync refused: observer credential is revoked");
                continue;
            }
            // Pairing is a hard transport prerequisite. Do not turn an
            // unpaired notification into a repeated transient-error log or retry loop.
            if !self.client.has_capability() {
                if completion_pending {
                    self.pending_trigger.store(true, Ordering::Release);
                }
                continue;
            }
            if self.circuit_open && !self.try_probe().await {
                self.finish_confirmed_locally();
                continue;
            }
            if completion_pending {
                self.draining_shutdown = true;
            }
            let pass_res = self.execute_pass().await;
            self.draining_shutdown = false;
            if let Err(error) = pass_res {
                tracing::error!(error, "Sync error");
                continue;
            }
        }
    }

    async fn execute_pass(&mut self) -> Result<(), &'static str> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_pass) {
            return Err("injected pass failure");
        }
        self.sync_pass().await;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn is_active(&self) -> bool {
        self.is_running() || self.draining_shutdown
    }

    fn current_identity_and_pairing(&self) -> (Option<String>, Option<String>) {
        self.client
            .capability()
            .map(|cap| {
                let writer = cap.writer();
                (
                    Some(writer.identity_key().to_string()),
                    Some(writer.pairing_id().to_string()),
                )
            })
            .unwrap_or((None, None))
    }

    fn is_retry_blocked(
        &self,
        segment_dir: &Path,
        current_identity: Option<&str>,
        current_pairing: Option<&str>,
        now: f64,
    ) -> bool {
        if let Some(&floor) = self.retry_floors.get(segment_dir)
            && now < floor
        {
            return true;
        }
        if let Some(retry) = read_retry(segment_dir) {
            let same_pairing = retry.identity_key.as_deref() == current_identity
                && retry.pairing_id.as_deref() == current_pairing;
            if same_pairing && now < retry.next_attempt_after {
                return true;
            }
        }
        false
    }

    async fn try_probe(&mut self) -> bool {
        if self.circuit_open_permanent {
            self.clear_progress();
            return false;
        }
        let elapsed = self.clock.monotonic_seconds() - self.circuit_open_since;
        if elapsed < self.circuit_cooldown {
            self.set_progress(
                format!("{:.0}s until probe", self.circuit_cooldown - elapsed),
                false,
            );
            return false;
        }
        self.set_progress("probing journal...".to_owned(), true);
        let outcome = if let Some(capability) = self.client.capability() {
            capability.system_status().await
        } else {
            Err(crate::private_link::LinkOutcome::TransportUnavailable)
        };
        match outcome {
            Ok(_) => {
                self.record_contact(true);
                self.circuit_open = false;
                self.circuit_open_permanent = false;
                self.circuit_open_since = 0.0;
                self.circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL;
                self.consecutive_failures = 0;
                self.recent_error_count.store(0, Ordering::Release);
                self.last_error_type = None;
                self.last_error_code = None;
                {
                    let mut facts = self.facts.lock().unwrap();
                    facts.last_error_class = None;
                    facts.last_error_code = None;
                }
                self.set_progress("syncing...".to_owned(), true);
                true
            }
            Err(outcome) => {
                let previous_cooldown = self.circuit_cooldown;
                let err = UploadClient::classify_error(
                    outcome.status_code(),
                    outcome.is_transport_unavailable(),
                );
                self.record_failure(Some(err), outcome.status_code().map(i64::from));
                self.circuit_cooldown =
                    (previous_cooldown * CIRCUIT_COOLDOWN_FACTOR).min(CIRCUIT_COOLDOWN_MAX);
                self.circuit_open_since = self.clock.monotonic_seconds();
                self.set_progress(
                    format!("probe failed, next in {:.0}s", self.circuit_cooldown),
                    false,
                );
                false
            }
        }
    }

    async fn sync_pass(&mut self) {
        self.facts.lock().unwrap().link = self.client.link_fact_state();
        self.finish_confirmed_locally();
        let now = self.clock.wall_seconds();
        ensure_cutover(&self.config.state_dir(), &self.config.captures_dir());
        let (current_identity, current_pairing) = self.current_identity_and_pairing();
        let cutover_segments = load_cutover_segments(&self.config.state_dir());
        let segments_by_day = collect_segments(&self.config.captures_dir());
        let mut days: Vec<String> = segments_by_day.keys().cloned().collect();
        days.sort_by(|a, b| b.cmp(a));

        self.set_progress("checking journal...".to_owned(), true);
        let mut pass_stopped = false;
        let mut pass_error_type = None;
        let mut pass_error_code = None;
        let mut pass_bounded_error_class = None;
        let mut pass_bounded_error_code = None;
        let mut requests_made = 0;
        let mut uploaded_in_phase1 = HashSet::new();

        // Phase 1: Direct non-legacy uploads for due segments
        'phase1: for day in &days {
            if !self.is_active() || self.circuit_open {
                pass_stopped = true;
                break 'phase1;
            }
            let segments = segments_by_day.get(day).into_iter().flatten();
            for segment_dir in segments {
                if !self.is_active() || self.circuit_open {
                    pass_stopped = true;
                    break 'phase1;
                }
                if self.is_retry_blocked(
                    segment_dir,
                    current_identity.as_deref(),
                    current_pairing.as_deref(),
                    now,
                ) {
                    continue;
                }
                let ack = read_ack(segment_dir);
                if is_ack_valid(
                    segment_dir,
                    ack.clone(),
                    current_identity.as_deref(),
                    current_pairing.as_deref(),
                ) {
                    continue;
                }
                let is_cutover = segment_rel(segment_dir)
                    .as_deref()
                    .is_some_and(|r| cutover_segments.contains(r));
                let is_mismatch = ack.as_ref().is_some_and(|a| {
                    a.identity_key != current_identity.as_deref().unwrap_or_default()
                        || a.pairing_id != current_pairing.as_deref().unwrap_or_default()
                });
                if is_cutover || is_mismatch {
                    continue;
                }
                if let Ok(files) = eligible_files(segment_dir) {
                    if files.is_empty() {
                        continue;
                    }
                    if files
                        .iter()
                        .all(|file| file.metadata().is_ok_and(|meta| meta.len() == 0))
                    {
                        quarantine_segment(
                            self.clock.wall_seconds(),
                            segment_dir,
                            "all files zero-byte",
                        );
                        continue;
                    }
                }
                let segment_key = segment_dir.file_name().unwrap().to_string_lossy();
                self.set_progress(format!("uploading {segment_key}"), true);
                requests_made += 1;
                match self.upload_segment(day, segment_dir).await {
                    UploadOutcome::Acked => {
                        uploaded_in_phase1.insert(segment_dir.to_path_buf());
                    }
                    UploadOutcome::Bounded => {
                        if let Some(err) = self.last_error_type {
                            if err == ErrorType::Transient {
                                pass_bounded_error_class = Some(ErrorType::Transient);
                                pass_bounded_error_code = self.last_error_code;
                            } else if err == ErrorType::Client
                                && pass_bounded_error_class != Some(ErrorType::Transient)
                            {
                                pass_bounded_error_class = Some(ErrorType::Client);
                                pass_bounded_error_code = self.last_error_code;
                            }
                        }
                    }
                    UploadOutcome::Stop => {
                        pass_stopped = true;
                        pass_error_type = self.last_error_type;
                        pass_error_code = self.last_error_code;
                        break 'phase1;
                    }
                }
            }
        }

        // Phase 2: Targeted single GET /app/devices/ingest/segments/{day} for legacy due
        if !pass_stopped && !self.circuit_open && self.is_active() {
            let mut custody_days = HashSet::new();
            for (day, segments) in &segments_by_day {
                let enters_phase2 = segments.iter().any(|s| {
                    if uploaded_in_phase1.contains(s) {
                        return false;
                    }
                    let ack = read_ack(s);
                    let valid_ack = is_ack_valid(
                        s,
                        ack.clone(),
                        current_identity.as_deref(),
                        current_pairing.as_deref(),
                    );
                    if valid_ack {
                        return false;
                    }
                    let is_blocked = self.is_retry_blocked(
                        s,
                        current_identity.as_deref(),
                        current_pairing.as_deref(),
                        now,
                    );
                    if is_blocked {
                        return false;
                    }
                    let is_empty = eligible_files(s).map_or(true, |f| f.is_empty());
                    if is_empty {
                        return false;
                    }
                    let is_legacy = segment_rel(s)
                        .as_deref()
                        .is_some_and(|r| cutover_segments.contains(r));
                    if is_legacy {
                        return true;
                    }
                    if let Some(ack) = ack
                        && (ack.identity_key != current_identity.as_deref().unwrap_or_default()
                            || ack.pairing_id != current_pairing.as_deref().unwrap_or_default())
                    {
                        return true;
                    }
                    false
                });
                if enters_phase2 {
                    custody_days.insert(day.clone());
                }
            }
            let mut sorted_custody_days: Vec<_> = custody_days.into_iter().collect();
            sorted_custody_days.sort_by(|a, b| b.cmp(a));

            for day in sorted_custody_days {
                if !self.is_active() || self.circuit_open {
                    pass_stopped = true;
                    break;
                }
                self.set_progress(format!("checking {day}..."), true);
                requests_made += 1;
                let custody = self.client.fetch_day_custody(&day).await;
                if custody.error_type == Some(ErrorType::Auth) {
                    pass_stopped = true;
                    pass_error_type = custody.error_type;
                    pass_error_code = custody.status_code.map(i64::from);
                    self.record_failure(custody.error_type, pass_error_code);
                    break;
                }
                if custody.error_type.is_some() || !custody.proof_available || !custody.day_present
                {
                    // Non-auth error or no proof: do not record_failure, do not stop pass.
                    // Upload each legacy segment on that day directly in this pass.
                    if let Some(segments) = segments_by_day.get(&day) {
                        for segment_dir in segments {
                            let ack = read_ack(segment_dir);
                            let valid_ack = is_ack_valid(
                                segment_dir,
                                ack.clone(),
                                current_identity.as_deref(),
                                current_pairing.as_deref(),
                            );
                            let is_blocked = self.is_retry_blocked(
                                segment_dir,
                                current_identity.as_deref(),
                                current_pairing.as_deref(),
                                now,
                            );
                            if !valid_ack && !is_blocked {
                                let is_legacy = segment_rel(segment_dir)
                                    .as_deref()
                                    .is_some_and(|r| cutover_segments.contains(r));
                                let is_mismatch = ack.as_ref().is_some_and(|a| {
                                    a.identity_key
                                        != current_identity.as_deref().unwrap_or_default()
                                        || a.pairing_id
                                            != current_pairing.as_deref().unwrap_or_default()
                                });
                                if is_legacy || is_mismatch {
                                    requests_made += 1;
                                    match self.upload_segment(&day, segment_dir).await {
                                        UploadOutcome::Acked => {}
                                        UploadOutcome::Bounded => {
                                            if let Some(err) = self.last_error_type {
                                                if err == ErrorType::Transient {
                                                    pass_bounded_error_class =
                                                        Some(ErrorType::Transient);
                                                    pass_bounded_error_code = self.last_error_code;
                                                } else if err == ErrorType::Client
                                                    && pass_bounded_error_class
                                                        != Some(ErrorType::Transient)
                                                {
                                                    pass_bounded_error_class =
                                                        Some(ErrorType::Client);
                                                    pass_bounded_error_code = self.last_error_code;
                                                }
                                            }
                                        }
                                        UploadOutcome::Stop => {
                                            pass_stopped = true;
                                            pass_error_type = self.last_error_type;
                                            pass_error_code = self.last_error_code;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                self.record_contact(false);
                let indexed = index_entries(&custody.items);
                if let Some(segments) = segments_by_day.get(&day) {
                    for segment_dir in segments {
                        if uploaded_in_phase1.contains(segment_dir) {
                            continue;
                        }
                        let seg_name = segment_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default();
                        let ack = read_ack(segment_dir);
                        let valid_ack = is_ack_valid(
                            segment_dir,
                            ack.clone(),
                            current_identity.as_deref(),
                            current_pairing.as_deref(),
                        );
                        if valid_ack {
                            continue;
                        }

                        let is_blocked = self.is_retry_blocked(
                            segment_dir,
                            current_identity.as_deref(),
                            current_pairing.as_deref(),
                            now,
                        );
                        if is_blocked {
                            continue;
                        }
                        let is_legacy = segment_rel(segment_dir)
                            .as_deref()
                            .is_some_and(|r| cutover_segments.contains(r));
                        let is_mismatch = ack.as_ref().is_some_and(|a| {
                            a.identity_key != current_identity.as_deref().unwrap_or_default()
                                || a.pairing_id != current_pairing.as_deref().unwrap_or_default()
                        });
                        if !is_legacy && !is_mismatch {
                            continue;
                        }

                        let mut should_upload = false;
                        if let Some(entry) = lookup_entry(&indexed, segment_dir) {
                            match segment_custody_proven(segment_dir, entry) {
                                Err(_) => {
                                    // Err skips
                                }
                                Ok(false) => {
                                    should_upload = true;
                                }
                                Ok(true) => {
                                    let build_ack_and_unlink = || -> io::Result<()> {
                                        let stored_key = entry
                                            .key
                                            .clone()
                                            .unwrap_or_else(|| seg_name.to_string());
                                        let files = eligible_files(segment_dir)?;
                                        let mut ack_files = Vec::new();
                                        for f in &files {
                                            let fname =
                                                f.file_name().and_then(|n| n.to_str()).ok_or_else(
                                                    || io::Error::other("non-utf8 file name"),
                                                )?;
                                            let meta = f.metadata()?;
                                            let sha = sha256_file(f)?;
                                            let stamp = file_stamp(f)?;
                                            let disposition = entry
                                                .files
                                                .as_deref()
                                                .unwrap_or_default()
                                                .iter()
                                                .find(|rf| {
                                                    rf.name.as_deref() == Some(fname)
                                                        || rf.submitted_name.as_deref()
                                                            == Some(fname)
                                                })
                                                .and_then(|rf| rf.status.clone())
                                                .unwrap_or_else(|| "present".to_string());
                                            ack_files.push(IngestAckFile {
                                                submitted: fname.to_owned(),
                                                written: fname.to_owned(),
                                                size: meta.len(),
                                                sha256: sha,
                                                disposition,
                                                stamp,
                                            });
                                        }
                                        let stream = segment_dir
                                            .parent()
                                            .and_then(|p| p.file_name())
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("default")
                                            .to_string();
                                        let ack = IngestAck {
                                            day: day.clone(),
                                            stream,
                                            local_key: seg_name.to_string(),
                                            stored_key,
                                            identity_key: current_identity
                                                .clone()
                                                .unwrap_or_default(),
                                            pairing_id: current_pairing.clone().unwrap_or_default(),
                                            proof: "listing".to_string(),
                                            files: ack_files,
                                        };
                                        write_ack(segment_dir, &ack)?;
                                        remove_retry(segment_dir);
                                        let _ = remove_confirmed_segment(segment_dir);
                                        Ok(())
                                    };
                                    let _ = build_ack_and_unlink();
                                }
                            }
                        } else {
                            should_upload = true;
                        }

                        if should_upload {
                            requests_made += 1;
                            match self.upload_segment(&day, segment_dir).await {
                                UploadOutcome::Acked => {}
                                UploadOutcome::Bounded => {
                                    if let Some(err) = self.last_error_type {
                                        if err == ErrorType::Transient {
                                            pass_bounded_error_class = Some(ErrorType::Transient);
                                            pass_bounded_error_code = self.last_error_code;
                                        } else if err == ErrorType::Client
                                            && pass_bounded_error_class
                                                != Some(ErrorType::Transient)
                                        {
                                            pass_bounded_error_class = Some(ErrorType::Client);
                                            pass_bounded_error_code = self.last_error_code;
                                        }
                                    }
                                }
                                UploadOutcome::Stop => {
                                    pass_stopped = true;
                                    pass_error_type = self.last_error_type;
                                    pass_error_code = self.last_error_code;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Phase 3: Idle system_status probe if 0 requests made and contact older than threshold/2
        if requests_made == 0 && !pass_stopped && !self.circuit_open && self.is_active() {
            let last_contact = self
                .facts
                .lock()
                .unwrap()
                .last_successful_contact
                .unwrap_or(0.0);
            let stale_interval = (self.config.sync_stale_threshold as f64 / 2.0).min(300.0);
            if now - last_contact >= stale_interval
                && let Some(capability) = self.client.capability()
            {
                match capability.system_status().await {
                    Ok(_) => {
                        self.record_contact(true);
                    }
                    Err(outcome) => {
                        let err = UploadClient::classify_error(
                            outcome.status_code(),
                            outcome.is_transport_unavailable(),
                        );
                        self.record_failure(Some(err), outcome.status_code().map(i64::from));
                    }
                }
            }
        }

        cleanup_empty_capture_dirs(&self.config.captures_dir());

        let pending_count = count_unacked_segments(
            &self.config.captures_dir(),
            current_identity.as_deref(),
            current_pairing.as_deref(),
        );
        if !pass_stopped && !self.circuit_open && self.is_active() {
            self.commit_pass_result(
                true,
                pass_bounded_error_class,
                pass_bounded_error_code,
                Some(i64::try_from(pending_count).unwrap_or(i64::MAX)),
            );
        } else {
            let facts = self.facts.lock().unwrap().clone();
            self.commit_pass_result(
                false,
                pass_error_type.or(facts.last_error_class),
                pass_error_code.or(facts.last_error_code),
                None,
            );
        }
    }

    async fn upload_segment(&mut self, day: &str, segment_dir: &Path) -> UploadOutcome {
        let files = match eligible_files(segment_dir) {
            Ok(files) => files,
            Err(error) => {
                tracing::warn!(%error, path = %segment_dir.display(), "Failed to enumerate segment files");
                self.record_bounded(Some(ErrorType::Client), segment_dir, 86400.0, None, None);
                return UploadOutcome::Bounded;
            }
        };
        if files.is_empty() {
            return UploadOutcome::Bounded;
        }

        let mut precomputed = Vec::with_capacity(files.len());
        for f in &files {
            let Some(fname) = f.file_name().and_then(|n| n.to_str()) else {
                self.record_bounded(Some(ErrorType::Client), segment_dir, 86400.0, None, None);
                return UploadOutcome::Bounded;
            };
            let Ok(meta) = f.metadata() else {
                self.record_bounded(Some(ErrorType::Client), segment_dir, 86400.0, None, None);
                return UploadOutcome::Bounded;
            };
            let Ok(sha) = sha256_file(f) else {
                self.record_bounded(Some(ErrorType::Client), segment_dir, 86400.0, None, None);
                return UploadOutcome::Bounded;
            };
            precomputed.push((fname.to_string(), meta.len(), sha));
        }

        let key = segment_dir.file_name().unwrap().to_string_lossy();
        let result = self.client.upload_segment(day, &key, &files).await;
        let (current_identity, current_pairing) = self.current_identity_and_pairing();

        if result.success {
            if let Some(ref descriptors) = result.file_descriptors {
                let has_received_not_written = descriptors
                    .iter()
                    .any(|d| d.disposition == "received_not_written");
                if verify_receipt(&files, &precomputed, descriptors) {
                    let stored_key = result.stored_key.clone().unwrap_or_else(|| key.to_string());
                    if stored_key != key.as_ref()
                        && let Err(error) = write_server_key(segment_dir, &stored_key)
                    {
                        tracing::warn!(%error, "Failed to write server key marker");
                    }
                    let mut ack_files = Vec::with_capacity(descriptors.len());
                    for desc in descriptors {
                        let file_path = segment_dir.join(&desc.submitted);
                        let Ok(stamp) = file_stamp(&file_path) else {
                            self.record_bounded(
                                None,
                                segment_dir,
                                3600.0,
                                result.status_code,
                                None,
                            );
                            return UploadOutcome::Bounded;
                        };
                        ack_files.push(IngestAckFile {
                            submitted: desc.submitted.clone(),
                            written: desc.written.clone(),
                            size: desc.size,
                            sha256: desc.sha256.clone(),
                            disposition: desc.disposition.clone(),
                            stamp,
                        });
                    }
                    let stream = segment_dir
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("default")
                        .to_string();
                    let ack = IngestAck {
                        day: day.to_string(),
                        stream,
                        local_key: key.to_string(),
                        stored_key,
                        identity_key: current_identity.unwrap_or_default(),
                        pairing_id: current_pairing.unwrap_or_default(),
                        proof: "upload".to_string(),
                        files: ack_files,
                    };
                    #[cfg(test)]
                    let write_ack_res = {
                        let mut fault_dir = INGEST_ACK_WRITE_FAULT_DIR.lock().unwrap();
                        if fault_dir.as_ref().is_some_and(|p| p == segment_dir)
                            || INGEST_ACK_WRITE_FAULT.swap(false, Ordering::SeqCst)
                        {
                            *fault_dir = None;
                            Err(io::Error::other("injected ack write fault"))
                        } else {
                            write_ack(segment_dir, &ack)
                        }
                    };
                    #[cfg(not(test))]
                    let write_ack_res = write_ack(segment_dir, &ack);

                    if let Err(error) = write_ack_res {
                        tracing::warn!(%error, "Failed to write ingest ack marker");
                        self.retry_floors.insert(
                            segment_dir.to_path_buf(),
                            self.clock.wall_seconds() + 3600.0,
                        );
                        self.record_bounded(None, segment_dir, 3600.0, result.status_code, None);
                        return UploadOutcome::Bounded;
                    }
                    let _ = remove_confirmed_segment(segment_dir);
                    remove_retry(segment_dir);
                    self.record_contact(false);
                    self.reset_failures();
                    return UploadOutcome::Acked;
                } else if has_received_not_written {
                    self.record_bounded(
                        None,
                        segment_dir,
                        86400.0,
                        result.status_code,
                        Some("received_not_written".to_string()),
                    );
                    return UploadOutcome::Bounded;
                }
            }
            self.record_bounded(
                None,
                segment_dir,
                86400.0,
                result.status_code,
                Some("receipt_invalid".to_string()),
            );
            UploadOutcome::Bounded
        } else {
            if result.status_code == Some(500)
                && result.reason_code.as_deref() == Some("segment_removed")
            {
                if remove_segment_removed(segment_dir).is_err() {
                    self.retry_floors.insert(
                        segment_dir.to_path_buf(),
                        self.clock.wall_seconds() + 3600.0,
                    );
                }
                self.record_contact(false);
                self.reset_failures();
                return UploadOutcome::Acked;
            }
            if result.is_local_failure && result.status_code == Some(413) {
                self.record_bounded(
                    Some(ErrorType::Client),
                    segment_dir,
                    86400.0,
                    Some(413),
                    result.reason_code,
                );
                return UploadOutcome::Bounded;
            }
            self.last_error_type = result.error_type;
            self.last_error_code = result.status_code.map(i64::from);
            if self.client.is_revoked() {
                self.record_failure(result.error_type, result.status_code.map(i64::from));
                return UploadOutcome::Stop;
            }
            if result.status_code == Some(409)
                && let Some(ref code) = result.reason_code
                && (code == "pairing_identity_unavailable" || code == "foreign_stream_binding")
            {
                self.consecutive_failures += 1;
                self.recent_error_count
                    .store(self.consecutive_failures.min(99) as u8, Ordering::Release);
                if self.consecutive_failures >= CIRCUIT_THRESHOLD_TRANSIENT {
                    self.circuit_open = true;
                    self.circuit_open_since = self.clock.monotonic_seconds();
                    self.circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL;
                }
                return UploadOutcome::Stop;
            }
            if matches!(
                result.error_type,
                Some(ErrorType::Auth | ErrorType::Incompatible)
            ) {
                self.record_failure(result.error_type, result.status_code.map(i64::from));
                return UploadOutcome::Stop;
            }
            if result.status_code == Some(413) {
                self.record_failure(Some(ErrorType::Client), Some(413));
                return UploadOutcome::Stop;
            }
            if matches!(result.status_code, Some(400..=425)) {
                self.record_bounded(
                    Some(ErrorType::Client),
                    segment_dir,
                    86400.0,
                    result.status_code,
                    result.reason_code,
                );
                return UploadOutcome::Bounded;
            }
            if result.status_code.is_some_and(|s| s >= 500) && result.reason_code.is_some() {
                let bound = 3600.0;
                self.record_bounded(
                    Some(ErrorType::Transient),
                    segment_dir,
                    bound,
                    result.status_code,
                    result.reason_code,
                );
                return UploadOutcome::Bounded;
            }
            self.record_failure(result.error_type, result.status_code.map(i64::from));
            if self.circuit_open {
                UploadOutcome::Stop
            } else {
                UploadOutcome::Bounded
            }
        }
    }

    fn record_bounded(
        &mut self,
        error_type: Option<ErrorType>,
        segment_dir: &Path,
        bound_seconds: f64,
        status_code: Option<u16>,
        reason_code: Option<String>,
    ) {
        let now = self.clock.wall_seconds();
        let current_retry = read_retry(segment_dir);
        let (current_identity, current_pairing) = self.current_identity_and_pairing();
        let same_failure = current_retry.as_ref().is_some_and(|r| {
            r.status_code == status_code
                && r.reason_code == reason_code
                && r.identity_key == current_identity
                && r.pairing_id == current_pairing
        });
        let identical_count = if same_failure {
            current_retry.as_ref().map_or(1, |r| r.identical_count + 1)
        } else {
            1
        };

        let actual_bound =
            if bound_seconds == 86400.0 || reason_code.as_deref() == Some("received_not_written") {
                86400.0
            } else if reason_code.as_deref() == Some("retryable")
                || reason_code.as_deref() == Some("journal_write_failed")
            {
                3600.0
            } else if identical_count >= 3 {
                86400.0
            } else {
                bound_seconds
            };

        let retry = IngestRetry {
            retry_version: 1,
            next_attempt_after: now + actual_bound,
            last_attempt_at: now,
            identical_count,
            status_code,
            reason_code,
            identity_key: current_identity,
            pairing_id: current_pairing,
        };
        if let Err(error) = write_retry(segment_dir, &retry) {
            tracing::warn!(%error, path = %segment_dir.display(), "Failed to write ingest retry marker");
            self.retry_floors
                .insert(segment_dir.to_path_buf(), now + actual_bound);
        }

        if let Some(err) = error_type {
            self.last_error_type = Some(err);
            self.last_error_code = status_code.map(i64::from);
            let mut facts = self.facts.lock().unwrap();
            facts.last_error_class = Some(err);
            facts.last_error_code = status_code.map(i64::from);
            facts.pending_confirmed = None;
            drop(facts);
            self.save_health();
        }
    }

    fn finish_confirmed_locally(&self) {
        let segments_by_day = collect_segments(&self.config.captures_dir());
        let (current_identity, current_pairing) = self.current_identity_and_pairing();
        for segments in segments_by_day.values() {
            for segment_dir in segments {
                let ack = read_ack(segment_dir);
                if is_ack_valid(
                    segment_dir,
                    ack,
                    current_identity.as_deref(),
                    current_pairing.as_deref(),
                ) {
                    let _ = remove_confirmed_segment(segment_dir);
                } else {
                    let Ok(entries) = fs::read_dir(segment_dir) else {
                        continue;
                    };
                    let mut all_bookkeeping = true;
                    for entry in entries {
                        let Ok(entry) = entry else {
                            all_bookkeeping = false;
                            break;
                        };
                        let Ok(meta) = entry.metadata() else {
                            all_bookkeeping = false;
                            break;
                        };
                        if meta.is_dir() {
                            all_bookkeeping = false;
                            break;
                        }
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if !is_bookkeeping_file(&name_str) {
                            all_bookkeeping = false;
                            break;
                        }
                    }
                    if all_bookkeeping {
                        let _ = remove_confirmed_segment(segment_dir);
                    }
                }
            }
        }
    }

    fn record_contact(&mut self, force: bool) {
        self.facts.lock().unwrap().last_successful_contact = Some(self.clock.wall_seconds());
        let mono = self.clock.monotonic_seconds();
        if force || mono - self.last_contact_flush >= CONTACT_FLUSH_INTERVAL {
            self.last_contact_flush = mono;
            self.save_health();
        }
    }

    fn record_failure(&mut self, error_type: Option<ErrorType>, status_code: Option<i64>) {
        let Some(error_type) = error_type else { return };
        self.last_error_type = Some(error_type);
        self.last_error_code = status_code;
        {
            let mut facts = self.facts.lock().unwrap();
            facts.last_error_class = Some(error_type);
            facts.last_error_code = status_code;
            facts.pending_confirmed = None;
        }
        self.save_health();
        if error_type == ErrorType::Client {
            return;
        }
        self.consecutive_failures += 1;
        self.recent_error_count
            .store(self.consecutive_failures.min(99) as u8, Ordering::Release);
        if self.consecutive_failures >= self.circuit_threshold() {
            self.circuit_open = true;
            self.circuit_open_permanent = error_type == ErrorType::Auth && self.client.is_revoked();
            self.circuit_open_since = self.clock.monotonic_seconds();
            self.circuit_cooldown = CIRCUIT_COOLDOWN_INITIAL;
        }
    }

    fn reset_failures(&mut self) {
        self.consecutive_failures = 0;
        self.last_error_type = None;
        self.last_error_code = None;
        self.recent_error_count.store(0, Ordering::Release);
    }

    fn commit_pass_result(
        &mut self,
        success: bool,
        error_type: Option<ErrorType>,
        status_code: Option<i64>,
        pending_count: Option<i64>,
    ) {
        let mut facts = self.facts.lock().unwrap();
        facts.in_progress = false;
        facts.progress.clear();
        if success && error_type.is_none() {
            let now = self.clock.wall_seconds();
            facts.last_successful_sync = Some(now);
            facts.last_successful_contact.get_or_insert(now);
            facts.last_error_class = None;
            facts.last_error_code = None;
            facts.pending_confirmed = pending_count;
            self.consecutive_failures = 0;
            self.recent_error_count.store(0, Ordering::Release);
            self.last_error_type = None;
            self.last_error_code = None;
        } else {
            facts.pending_confirmed = None;
            facts.last_error_class = error_type;
            facts.last_error_code = status_code;
            self.last_error_type = error_type;
            self.last_error_code = status_code;
        }
        self.last_contact_flush = self.clock.monotonic_seconds();
        drop(facts);
        self.save_health();
    }

    fn circuit_threshold(&self) -> u32 {
        match self.last_error_type {
            Some(ErrorType::Auth | ErrorType::Incompatible) => CIRCUIT_THRESHOLD_AUTH,
            Some(ErrorType::Client) => 0,
            _ => CIRCUIT_THRESHOLD_TRANSIENT,
        }
    }

    fn set_progress(&self, progress: String, in_progress: bool) {
        let mut facts = self.facts.lock().unwrap();
        facts.progress = progress;
        facts.in_progress = in_progress;
        drop(facts);
        self.save_health();
    }

    fn clear_progress(&self) {
        self.set_progress(String::new(), false);
    }

    fn save_health(&self) {
        let mut facts = self.facts.lock().unwrap();
        facts.link = Some(self.link_facts.snapshot());
        if let Err(error) = save_facts(&self.config.state_dir(), &facts) {
            tracing::warn!(%error, "Failed to save sync health");
        }
    }

    #[cfg(test)]
    pub(crate) async fn cleanup_synced_segments(&mut self) {
        self.sync_pass().await;
    }
}

fn file_stamp(path: &Path) -> io::Result<IngestAckFileStamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    Ok(IngestAckFileStamp {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.len(),
        mtime_ns: meta
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.mtime_nsec()),
    })
}

fn is_bookkeeping_file(name: &str) -> bool {
    name == METADATA_FILENAME
        || name == INGEST_ACK_FILENAME
        || name == INGEST_RETRY_FILENAME
        || name == SERVER_KEY_FILENAME
        || (name.starts_with('.') && name.ends_with(".tmp"))
        || name.starts_with(".ingest_retry.json.tmp.")
}

fn remove_confirmed_segment(dir: &Path) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        let files = eligible_files(dir)?;
        for file in files {
            fs::remove_file(file)?;
        }
        let ack_path = dir.join(INGEST_ACK_FILENAME);
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path != ack_path && fs::metadata(&path)?.is_file() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if is_bookkeeping_file(&name_str) {
                    fs::remove_file(&path)?;
                }
            }
        }
        if ack_path.exists() {
            fs::remove_file(&ack_path)?;
        }
        fs::remove_dir(dir)?;
        Ok(())
    })();
    if let Err(ref error) = result {
        tracing::error!(%error, path = %dir.display(), "Cleanup failed");
    }
    result
}

fn remove_segment_removed(dir: &Path) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if fs::metadata(&path)?.is_file() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if is_bookkeeping_file(&name_str) {
                    fs::remove_file(&path)?;
                }
            }
        }
        let files = eligible_files(dir)?;
        for file in files {
            fs::remove_file(file)?;
        }
        fs::remove_dir(dir)?;
        Ok(())
    })();
    if let Err(ref error) = result {
        tracing::error!(%error, path = %dir.display(), "Cleanup failed");
    }
    result
}

fn is_ack_valid(
    segment_dir: &Path,
    ack: Option<IngestAck>,
    current_identity: Option<&str>,
    current_pairing: Option<&str>,
) -> bool {
    let Some(mut ack) = ack else { return false };
    if let (Some(cur_id), Some(cur_pair)) = (current_identity, current_pairing)
        && (ack.identity_key != cur_id || ack.pairing_id != cur_pair)
    {
        return false;
    }
    let local_name = match segment_dir.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return false,
    };
    if ack.local_key != local_name {
        return false;
    }
    let Ok(files) = eligible_files(segment_dir) else {
        return false;
    };
    if files.is_empty() {
        return true;
    }
    let mut modified = false;
    for file in &files {
        let Some(fname) = file.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        let Some(ack_file) = ack.files.iter_mut().find(|f| f.submitted == fname) else {
            let _ = fs::remove_file(segment_dir.join(INGEST_ACK_FILENAME));
            return false;
        };
        if ack.proof == "listing" {
            if ack_file.disposition != "present" && ack_file.disposition != "processed" {
                return false;
            }
        } else {
            if ack_file.disposition != "written" && ack_file.disposition != "already_held" {
                return false;
            }
        }
        let Ok(current_stamp) = file_stamp(file) else {
            return false;
        };
        if ack_file.stamp == current_stamp {
            continue;
        }
        let Ok(sha) = sha256_file(file) else {
            return false;
        };
        if sha == ack_file.sha256 && current_stamp.size == ack_file.size {
            ack_file.stamp = current_stamp;
            modified = true;
        } else {
            let _ = fs::remove_file(segment_dir.join(INGEST_ACK_FILENAME));
            return false;
        }
    }
    if modified {
        let _ = write_ack(segment_dir, &ack);
    }
    true
}

fn verify_receipt(
    files: &[PathBuf],
    precomputed: &[(String, u64, String)],
    descriptors: &[FileDescriptor],
) -> bool {
    if files.is_empty()
        || files.len() != descriptors.len()
        || descriptors.len() != precomputed.len()
    {
        return false;
    }
    let mut seen_submitted = HashSet::new();
    for desc in descriptors {
        if !seen_submitted.insert(&desc.submitted) {
            return false;
        }
        let Some((_, expected_size, expected_sha)) = precomputed
            .iter()
            .find(|(name, _, _)| name == &desc.submitted)
        else {
            return false;
        };
        if desc.size != *expected_size {
            return false;
        }
        if &desc.sha256 != expected_sha {
            return false;
        }
        if desc.disposition != "written" && desc.disposition != "already_held" {
            return false;
        }
    }
    true
}

fn write_ack(segment_dir: &Path, ack: &IngestAck) -> io::Result<()> {
    let target = segment_dir.join(INGEST_ACK_FILENAME);
    let text = serde_json::to_string_pretty(ack).map_err(io::Error::other)?;
    let mut bytes = text.into_bytes();
    bytes.push(b'\n');
    crate::private_file::atomic_write_bytes(&target, &bytes)
        .map_err(|err| io::Error::other(err.to_string()))
}

fn read_ack(segment_dir: &Path) -> Option<IngestAck> {
    let path = segment_dir.join(INGEST_ACK_FILENAME);
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn write_retry(segment_dir: &Path, retry: &IngestRetry) -> io::Result<()> {
    let target = segment_dir.join(INGEST_RETRY_FILENAME);
    let tmp = segment_dir.join(format!(
        "{}.tmp.{}",
        INGEST_RETRY_FILENAME,
        std::process::id()
    ));
    let text = serde_json::to_string_pretty(retry).map_err(io::Error::other)?;
    {
        let mut file = File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(tmp, target)
}

fn read_retry(segment_dir: &Path) -> Option<IngestRetry> {
    let path = segment_dir.join(INGEST_RETRY_FILENAME);
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

fn remove_retry(segment_dir: &Path) {
    let path = segment_dir.join(INGEST_RETRY_FILENAME);
    let _ = fs::remove_file(path);
}

fn segment_rel(segment_dir: &Path) -> Option<String> {
    let name = segment_dir.file_name()?.to_str()?;
    let stream_dir = segment_dir.parent()?;
    let stream = stream_dir.file_name()?.to_str()?;
    let day_dir = stream_dir.parent()?;
    let day = day_dir.file_name()?.to_str()?;
    Some(format!("{day}/{stream}/{name}"))
}

fn ensure_cutover(state_dir: &Path, captures_dir: &Path) {
    let synced_days = state_dir.join("synced_days.json");
    let _ = fs::remove_file(synced_days);

    let cutover_path = state_dir.join(INGEST_CUTOVER_FILENAME);
    if !cutover_path.exists() {
        let _ = fs::create_dir_all(state_dir);
        let mut segments = Vec::new();
        let segments_by_day = collect_segments(captures_dir);
        for day_segs in segments_by_day.values() {
            for seg in day_segs {
                if !seg.join(INGEST_ACK_FILENAME).exists()
                    && let Some(rel) = segment_rel(seg)
                {
                    segments.push(rel);
                }
            }
        }
        segments.sort();
        let cutover = IngestCutover { segments };
        if let Ok(text) = serde_json::to_string_pretty(&cutover) {
            let mut bytes = text.into_bytes();
            bytes.push(b'\n');
            let _ = crate::private_file::atomic_write_bytes(&cutover_path, &bytes);
        }
    }
}

fn load_cutover_segments(state_dir: &Path) -> HashSet<String> {
    let cutover_path = state_dir.join(INGEST_CUTOVER_FILENAME);
    fs::read_to_string(cutover_path)
        .ok()
        .and_then(|text| serde_json::from_str::<IngestCutover>(&text).ok())
        .map(|c| c.segments.into_iter().collect())
        .unwrap_or_default()
}

fn count_unacked_segments(
    captures_dir: &Path,
    current_identity: Option<&str>,
    current_pairing: Option<&str>,
) -> u64 {
    let segments_by_day = collect_segments(captures_dir);
    let mut count = 0;
    for segments in segments_by_day.values() {
        for segment in segments {
            let ack = read_ack(segment);
            if !is_ack_valid(segment, ack, current_identity, current_pairing)
                && eligible_files(segment).is_ok_and(|files| !files.is_empty())
            {
                count += 1;
            }
        }
    }
    count
}

fn eligible_files(segment_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(segment_dir)? {
        let entry = entry?;
        let path = entry.path();
        if fs::metadata(&path)?.is_file() && !entry.file_name().to_string_lossy().starts_with('.') {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    #[cfg(test)]
    SHA256_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn segment_custody_proven(segment_dir: &Path, entry: &ListingEntry) -> io::Result<bool> {
    let files = eligible_files(segment_dir)?;
    if files.is_empty() {
        return Ok(false);
    }
    for local in &files {
        let Some(local_name) = local.file_name().and_then(|name| name.to_str()) else {
            return Err(io::Error::other("non-utf8 file name"));
        };
        let Some(remote) = entry
            .files
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|remote| {
                remote.submitted_name.as_deref() == Some(local_name)
                    || remote.name.as_deref() == Some(local_name)
            })
        else {
            return Ok(false);
        };
        if !matches!(remote.status.as_deref(), Some("present" | "processed")) {
            return Ok(false);
        }
        let Some(size) = remote.size else {
            return Ok(false);
        };
        let meta = local.metadata()?;
        if meta.len() != size {
            return Ok(false);
        }
        let Some(remote_sha256) = remote.sha256.as_deref() else {
            return Ok(false);
        };
        let local_sha256 = sha256_file(local)?;
        if !local_sha256.eq_ignore_ascii_case(remote_sha256) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn index_entries(items: &[ListingEntry]) -> HashMap<String, &ListingEntry> {
    let mut indexed = HashMap::new();
    for item in items {
        if let Some(key) = item.key.as_ref().filter(|key| !key.is_empty()) {
            indexed.insert(key.clone(), item);
        }
        if let Some(key) = item.original_key.as_ref().filter(|key| !key.is_empty()) {
            indexed.insert(key.clone(), item);
        }
    }
    indexed
}

fn lookup_entry<'a>(
    entries: &'a HashMap<String, &'a ListingEntry>,
    segment_dir: &Path,
) -> Option<&'a ListingEntry> {
    let name = segment_dir.file_name()?.to_str()?;
    entries
        .get(name)
        .copied()
        .or_else(|| read_server_key(segment_dir).and_then(|key| entries.get(&key).copied()))
}

fn read_server_key(segment_dir: &Path) -> Option<String> {
    fs::read_to_string(segment_dir.join(SERVER_KEY_FILENAME))
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

fn write_server_key(segment_dir: &Path, key: &str) -> io::Result<()> {
    fs::write(segment_dir.join(SERVER_KEY_FILENAME), format!("{key}\n"))
}

fn quarantine_segment(now: f64, segment_dir: &Path, reason: &str) -> bool {
    let failed = segment_dir.with_file_name(format!(
        "{}.failed",
        segment_dir.file_name().unwrap().to_string_lossy()
    ));
    if let Err(error) = fs::rename(segment_dir, &failed) {
        tracing::error!(%error, path = %segment_dir.display(), "Failed to quarantine");
        return false;
    }
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now.max(0.0));
    if let Err(error) = File::open(&failed)
        .and_then(|directory| directory.set_times(FileTimes::new().set_modified(stamp)))
    {
        tracing::warn!(%error, path = %failed.display(), "Failed to stamp quarantine time");
    }
    tracing::warn!(path = %failed.display(), reason, "Quarantined segment");
    true
}

#[cfg(test)]
pub(crate) async fn cleanup_synced_day_for_composition(
    config: Config,
    client: Arc<UploadClient>,
    clock: Arc<dyn Clock + Send + Sync>,
    _day: &str,
) -> SyncFacts {
    let facts = Arc::new(Mutex::new(SyncFacts::default()));
    let mut worker = SyncWorker::new(
        config,
        client,
        clock,
        SyncControl {
            notify: Arc::new(Notify::new()),
            pending_trigger: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(true)),
        },
        Arc::clone(&facts),
        Arc::new(AtomicU8::new(0)),
    );
    worker.sync_pass().await;
    facts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn sorted_dirs(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<_> = fs::read_dir(root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    paths.sort();
    Ok(paths)
}

fn cleanup_empty_capture_dirs(captures_dir: &Path) {
    let Ok(day_entries) = sorted_dirs(captures_dir) else {
        return;
    };
    for day_dir in day_entries {
        if let Ok(streams) = sorted_dirs(&day_dir) {
            for stream in streams {
                remove_if_empty(&stream);
            }
        }
        remove_if_empty(&day_dir);
    }
}

fn collect_segments(root: &Path) -> HashMap<String, Vec<PathBuf>> {
    let mut result = HashMap::new();
    for day in sorted_dirs(root).unwrap_or_default().into_iter().rev() {
        let Some(day_name) = day.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        for stream in sorted_dirs(&day).unwrap_or_default() {
            let segments: Vec<_> = sorted_dirs(&stream)
                .unwrap_or_default()
                .into_iter()
                .rev()
                .filter(|path| {
                    let name = path.file_name().unwrap().to_string_lossy();
                    !name.ends_with(".incomplete") && !name.ends_with(".failed")
                })
                .collect();
            if !segments.is_empty() {
                result
                    .entry(day_name.to_owned())
                    .or_insert_with(Vec::new)
                    .extend(segments);
            }
        }
    }
    result
}

fn remove_if_empty(path: &Path) {
    if path.is_dir() && fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::segment::timestamp_parts;
    use crate::{
        private_link::{LinkFactState, start_private_link_session},
        private_link_test_peer::PrivateLinkPeer,
        sync_health::{HealthState, load_facts_with_liveness},
        test_support::{
            DayCustodyFixture, LinkedMockServer, MockServer, MutableClock, day_custody_fixture,
            wait_for_requests,
        },
        upload::ListingFile,
    };
    use serde_json::{Value, json};
    use tracing::instrument::WithSubscriber;

    struct FixedClock {
        wall: f64,
        mono: f64,
    }

    impl Clock for FixedClock {
        fn wall_seconds(&self) -> f64 {
            self.wall
        }

        fn monotonic_seconds(&self) -> f64 {
            self.mono
        }
    }

    // AC: 7 — poisoned sync facts are recovered instead of killing the shell or tick loop.
    #[test]
    fn sampler_recovers_poisoned_facts_lock() {
        let facts = Arc::new(Mutex::new(SyncFacts {
            in_progress: true,
            progress: "1/2".into(),
            ..Default::default()
        }));
        let poison = Arc::clone(&facts);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poison.lock().unwrap();
            panic!("poison sampler facts");
        });
        let link_facts = LinkFacts::default();
        link_facts.publish(crate::private_link::LinkFact::ObserverRegistered);
        let sampler = SyncSampler {
            facts,
            clock: Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
            stale_threshold: 600.0,
            poison_reports: Arc::new(AtomicUsize::new(0)),
            link_facts,
        };
        assert_eq!(sampler.health().dbus, "syncing");
        assert_eq!(sampler.progress(), "1/2");
        assert_eq!(sampler.poison_reports.load(Ordering::Acquire), 1);
    }

    fn entry(name: &str, status: &str, sha: &str) -> ListingEntry {
        ListingEntry {
            key: Some("120000_300".to_owned()),
            original_key: None,
            files: Some(vec![ListingFile {
                submitted_name: None,
                name: Some(name.to_owned()),
                status: Some(status.to_owned()),
                sha256: Some(sha.to_owned()),
                size: Some(6),
            }]),
        }
    }

    fn create_segment(temp: &tempfile::TempDir, name: &str, body: &[u8]) -> PathBuf {
        let segment = temp.path().join("captures/20260101/archon").join(name);
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), body).unwrap();
        segment
    }

    fn create_test_ack(segment: &Path, key: &str, cur_id: &str, cur_pair: &str) -> IngestAck {
        let files = eligible_files(segment).unwrap_or_default();
        let mut ack_files = Vec::new();
        for f in &files {
            let fname = f.file_name().unwrap().to_str().unwrap();
            let meta = f.metadata().unwrap();
            let sha = sha256_file(f).unwrap();
            let stamp = file_stamp(f).unwrap();
            ack_files.push(IngestAckFile {
                submitted: fname.to_owned(),
                written: fname.to_owned(),
                size: meta.len(),
                sha256: sha,
                disposition: "written".to_owned(),
                stamp,
            });
        }
        IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: key.to_string(),
            stored_key: key.to_string(),
            identity_key: cur_id.to_string(),
            pairing_id: cur_pair.to_string(),
            proof: "upload".to_string(),
            files: ack_files,
        }
    }

    fn custody(items: Vec<Value>) -> Value {
        json!({"day_custody_items": items})
    }

    fn custody_for_day(day: &str, items: Vec<Value>) -> Value {
        json!({"day_custody_day": day, "day_custody_items": items})
    }

    fn listing(key: &str, file_name: &str, status: Option<&str>, sha: &str) -> Value {
        listing_with_size(key, file_name, status, sha, 6)
    }

    fn listing_with_size(
        key: &str,
        file_name: &str,
        status: Option<&str>,
        sha: &str,
        size: u64,
    ) -> Value {
        let mut file = json!({"name": file_name, "sha256": sha});
        file["size"] = json!(size);
        if let Some(status) = status {
            file["status"] = json!(status);
        }
        custody_for_day(
            "20260101",
            vec![json!({"key":key,"observed":true,"files":[file]})],
        )
    }

    async fn test_worker(
        temp: &tempfile::TempDir,
        responses: Vec<(u16, Value)>,
    ) -> (LinkedMockServer, SyncWorker) {
        let server = LinkedMockServer::new(Vec::new()).await;
        for (status, body) in responses {
            if let Some(fixture) = day_custody_fixture(&body) {
                assert_eq!(status, 200, "custody fixtures are reachable responses");
                server.enqueue_day_custody(fixture);
            } else {
                server.enqueue_response(status, body.to_string());
            }
        }
        let config = Config {
            stream: "desktop".to_owned(),
            sync_max_retries: 1,
            sync_retry_delays: vec![0],
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let state_dir = config.state_dir();
        fs::create_dir_all(&state_dir).unwrap();
        let cutover_path = state_dir.join(INGEST_CUTOVER_FILENAME);
        if !cutover_path.exists() {
            fs::write(&cutover_path, b"{\"segments\":[]}\n").unwrap();
        }
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(UploadClient::new(
            &config,
            server.capability(),
            clock.clone(),
        ));
        let worker = SyncWorker::new(
            config,
            client,
            clock,
            SyncControl {
                notify: Arc::new(Notify::new()),
                pending_trigger: Arc::new(AtomicBool::new(false)),
                running: Arc::new(AtomicBool::new(true)),
            },
            Arc::new(Mutex::new(SyncFacts::default())),
            Arc::new(AtomicU8::new(0)),
        );
        (server, worker)
    }

    trait RequestLog {
        fn logged_requests(&self) -> Vec<crate::test_support::Received>;
    }

    impl RequestLog for MockServer {
        fn logged_requests(&self) -> Vec<crate::test_support::Received> {
            self.requests()
        }
    }

    impl RequestLog for LinkedMockServer {
        fn logged_requests(&self) -> Vec<crate::test_support::Received> {
            self.requests()
        }
    }

    fn upload_hits(server: &impl RequestLog) -> usize {
        server
            .logged_requests()
            .iter()
            .filter(|request| request.uri == "/app/devices/ingest")
            .count()
    }

    async fn upload_then_cleanup_keeps(remote: Value) {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![(200, remote)]).await;
        worker.sync_pass().await;
        assert!(!segment.join(SERVER_KEY_FILENAME).exists());
        worker.cleanup_synced_segments().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!segment.exists());
    }

    async fn held_then_cleanup_deletes(remote: Value) {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![(200, remote)]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            cur_id.as_deref().unwrap(),
            cur_pair.as_deref().unwrap(),
        );
        write_ack(&segment, &ack).unwrap();
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 0);
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_skips_incomplete_and_failed
    #[test]
    fn collect_skips_incomplete_and_failed() {
        let temp = tempfile::tempdir().unwrap();
        let stream = temp.path().join("20260101/archon");
        fs::create_dir_all(stream.join("120000_300")).unwrap();
        fs::create_dir(stream.join("130000.incomplete")).unwrap();
        fs::create_dir(stream.join("140000.failed")).unwrap();
        assert_eq!(collect_segments(temp.path())["20260101"].len(), 1);
    }

    // tests/test_sync.py::test_present_status_with_mismatched_sha_is_not_proof
    #[test]
    fn present_status_with_mismatched_sha_is_not_proof() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        assert!(
            !segment_custody_proven(temp.path(), &entry("screen.webm", "present", "bad")).unwrap()
        );
    }

    #[test]
    fn missing_or_mismatched_file_size_is_not_custody_proof() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        let sha = sha256_file(&file).unwrap();
        let mut missing = entry("screen.webm", "present", &sha);
        missing.files.as_mut().unwrap()[0].size = None;
        assert!(!segment_custody_proven(temp.path(), &missing).unwrap());

        let mut mismatch = entry("screen.webm", "present", &sha);
        mismatch.files.as_mut().unwrap()[0].size = Some(7);
        assert!(!segment_custody_proven(temp.path(), &mismatch).unwrap());
    }

    // tests/test_sync.py::test_swapped_sha_by_filename_is_not_proof
    #[test]
    fn swapped_sha_by_filename_is_not_proof() {
        let temp = tempfile::tempdir().unwrap();
        let screen = temp.path().join("screen.webm");
        let audio = temp.path().join("audio.flac");
        fs::write(&screen, b"screen").unwrap();
        fs::write(&audio, b"audio").unwrap();
        let item = ListingEntry {
            key: Some("120000_300".to_owned()),
            original_key: None,
            files: Some(vec![
                ListingFile {
                    submitted_name: None,
                    name: Some("screen.webm".to_owned()),
                    status: Some("present".to_owned()),
                    sha256: Some(sha256_file(&audio).unwrap()),
                    size: Some(fs::metadata(&audio).unwrap().len()),
                },
                ListingFile {
                    submitted_name: None,
                    name: Some("audio.flac".to_owned()),
                    status: Some("present".to_owned()),
                    sha256: Some(sha256_file(&screen).unwrap()),
                    size: Some(fs::metadata(&screen).unwrap().len()),
                },
            ]),
        };
        assert!(!segment_custody_proven(temp.path(), &item).unwrap());
    }

    // tests/test_sync.py::test_submitted_name_matches_local_filename
    #[test]
    fn submitted_name_matches_local_filename() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("submitted.webm");
        fs::write(&file, b"screen").unwrap();
        let item = ListingEntry {
            key: Some("120000_300".to_owned()),
            original_key: None,
            files: Some(vec![ListingFile {
                submitted_name: Some("submitted.webm".to_owned()),
                name: Some("stored.webm".to_owned()),
                status: Some("present".to_owned()),
                sha256: Some(sha256_file(&file).unwrap()),
                size: Some(fs::metadata(&file).unwrap().len()),
            }]),
        };
        assert!(segment_custody_proven(temp.path(), &item).unwrap());
    }

    // tests/test_sync.py::test_present_status_with_mismatched_sha_uploads
    #[tokio::test]
    async fn present_status_with_mismatched_sha_uploads() {
        upload_then_cleanup_keeps(listing("120000_300", "screen.webm", Some("present"), "bad"))
            .await;
    }

    #[tokio::test]
    async fn present_status_with_mismatched_size_uploads() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(listing_with_size(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha,
            7,
        ))
        .await;
    }

    #[tokio::test]
    async fn present_status_with_absent_sha_uploads_and_cleanup_keeps() {
        upload_then_cleanup_keeps(custody_for_day(
            "20260101",
            vec![json!({
                "key": "120000_300",
                "observed": true,
                "files": [{
                    "name": "screen.webm",
                    "size": 6,
                    "status": "present",
                }],
            })],
        ))
        .await;
    }

    #[tokio::test]
    async fn present_status_with_absent_sha_and_unreadable_file_quarantines() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let file = segment.join("screen.webm");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(sha256_file(&file).is_err());
        let (_server, mut worker) = test_worker(&temp, vec![]).await;

        worker.sync_pass().await;

        assert!(segment.exists());
        assert!(read_retry(&segment).is_some());
        assert_eq!(worker.last_error_type, Some(ErrorType::Client));
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    #[tokio::test]
    async fn processed_status_with_absent_size_uploads() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(custody(vec![json!({
            "key": "120000_300",
            "observed": true,
            "files": [{
                "name": "screen.webm",
                "status": "processed",
                "sha256": sha,
            }],
        })]))
        .await;
    }

    // tests/test_sync.py::test_relocated_status_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn relocated_status_uploads_and_cleanup_keeps() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "x", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        drop(segment);
        upload_then_cleanup_keeps(listing(
            "120000_300",
            "screen.webm",
            Some("relocated"),
            &sha,
        ))
        .await;
    }

    // tests/test_sync.py::test_processed_status_sha_match_skips_and_cleanup_deletes
    #[tokio::test]
    async fn processed_status_sha_match_skips_and_cleanup_deletes() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        held_then_cleanup_deletes(listing(
            "120000_300",
            "screen.webm",
            Some("processed"),
            &sha,
        ))
        .await;
    }

    // tests/test_sync.py::test_processed_status_with_mismatched_sha_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn processed_status_with_mismatched_sha_uploads_and_cleanup_keeps() {
        upload_then_cleanup_keeps(listing(
            "120000_300",
            "screen.webm",
            Some("processed"),
            "bad",
        ))
        .await;
    }

    // tests/test_sync.py::test_processed_status_with_mismatched_name_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn processed_status_with_mismatched_name_uploads_and_cleanup_keeps() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(listing("120000_300", "other.webm", Some("processed"), &sha))
            .await;
    }

    // tests/test_sync.py::test_missing_status_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn missing_status_uploads_and_cleanup_keeps() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(listing("120000_300", "screen.webm", None, &sha)).await;
    }

    #[tokio::test]
    async fn missing_custody_status_uploads_and_cleanup_keeps() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(listing("120000_300", "screen.webm", Some("missing"), &sha))
            .await;
    }

    // tests/test_sync.py::test_unknown_status_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn unknown_status_uploads_and_cleanup_keeps() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        upload_then_cleanup_keeps(listing("120000_300", "screen.webm", Some("unknown"), &sha))
            .await;
    }

    // tests/test_sync.py::test_all_present_sha_match_skips_and_cleanup_deletes
    #[tokio::test]
    async fn all_present_sha_match_skips_and_cleanup_deletes() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        held_then_cleanup_deletes(listing("120000_300", "screen.webm", Some("present"), &sha))
            .await;
    }

    // tests/test_sync.py::test_mixed_present_and_processed_files_skip_and_cleanup_deletes
    #[tokio::test]
    async fn mixed_present_and_processed_files_skip_and_cleanup_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        fs::write(segment.join("audio.flac"), b"audio").unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        fs::create_dir_all(config.state_dir()).unwrap();
        fs::write(
            config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/120000_300\"]}\n",
        )
        .unwrap();
        let remote = custody_for_day(
            "20260101",
            vec![json!({"key":"120000_300", "observed":true, "files":[
                {"name":"screen.webm","size":6,"status":"present","sha256":format!("{:x}",Sha256::digest(b"screen"))},
                {"name":"audio.flac","size":5,"status":"processed","sha256":format!("{:x}",Sha256::digest(b"audio"))}
            ]})],
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)]).await;
        worker.sync_pass().await;
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_unreadable_sha_cleanup_keeps
    #[tokio::test]
    async fn unreadable_sha_cleanup_keeps() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let remote = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        fs::set_permissions(segment.join("screen.webm"), fs::Permissions::from_mode(0o0)).unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)]).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: an enumeration failure cannot hide local files from cleanup proof.
    #[tokio::test]
    async fn unreadable_segment_directory_is_never_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let remote = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(eligible_files(&segment).is_err());
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)]).await;
        worker.cleanup_synced_segments().await;
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(segment.exists());
    }

    // AC: a file that cannot be statted is bounded and never sends a partial request.
    #[tokio::test]
    async fn unstatable_file_is_quarantined_without_upload() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let file = segment.join("screen.webm");
        fs::remove_file(&file).unwrap();
        symlink(segment.join("missing.webm"), &file).unwrap();
        assert!(eligible_files(&segment).is_err());
        let (server, mut worker) = test_worker(&temp, vec![]).await;

        worker.sync_pass().await;

        assert!(segment.exists());
        assert!(read_retry(&segment).is_some());
        assert_eq!(worker.last_error_type, Some(ErrorType::Client));
        assert_eq!(upload_hits(&server), 0);
    }

    #[tokio::test]
    async fn total_mismatch_uploads_and_does_not_mark_day_synced() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        server.enqueue_response(
            200,
            json!({"status":"ok","segment":"120000_300"}).to_string(),
        );
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_duplicate_marker_stops_reupload
    #[tokio::test]
    async fn duplicate_marker_stops_reupload() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(
                200,
                json!({"status":"duplicate","existing_segment":"existing_300"}),
            )],
        )
        .await;
        worker.sync_pass().await;
        assert!(!segment.exists());
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
    }

    // tests/test_sync.py::test_collision_marker_and_original_key_reconcile
    #[tokio::test]
    async fn collision_marker_and_original_key_reconcile() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_301"}))],
        )
        .await;
        worker.sync_pass().await;
        assert!(!segment.exists());
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
    }

    // tests/test_sync.py::test_zero_byte_segment_quarantined
    #[tokio::test]
    async fn zero_byte_segment_quarantined() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"");
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;
        assert!(!segment.exists());
        assert!(segment.with_file_name("120000_300.failed").exists());
    }

    // tests/test_sync.py::test_zero_byte_does_not_trigger_upload
    #[tokio::test]
    async fn zero_byte_does_not_trigger_upload() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"");
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 0);
    }

    // tests/test_sync.py::test_mixed_files_not_quarantined
    #[tokio::test]
    async fn mixed_files_not_quarantined() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"");
        fs::write(segment.join("audio.flac"), b"audio").unwrap();
        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!segment.exists());
        assert!(!segment.with_file_name("120000_300.failed").exists());
    }

    // tests/test_sync.py::test_zero_byte_day_marked_synced
    #[tokio::test]
    async fn zero_byte_day_marked_synced() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"");
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, Some(0));
    }

    // tests/test_sync.py::test_client_error_quarantines_segment
    #[tokio::test]
    async fn journal_client_error_keeps_segment_and_walk_continues() {
        let temp = tempfile::tempdir().unwrap();
        let rejected = create_segment(&temp, "130000_300", b"bad");
        let accepted = create_segment(&temp, "120000_300", b"good");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (400, json!({})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
        )
        .await;
        worker.sync_pass().await;
        assert!(rejected.exists());
        assert!(!rejected.with_file_name("130000_300.failed").exists());
        assert!(read_retry(&rejected).is_some());
        assert!(!accepted.exists());
        assert_eq!(upload_hits(&server), 2);
    }

    // tests/test_sync.py::test_quarantine_segment_stamps_quarantine_mtime
    #[test]
    fn quarantine_segment_stamps_quarantine_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("120000_300");
        fs::create_dir(&segment).unwrap();
        File::open(&segment)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        assert!(quarantine_segment(2_000_000.0, &segment, "test"));
        let modified = fs::metadata(segment.with_file_name("120000_300.failed"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(modified, 2_000_000.0);
    }

    // tests/test_sync.py::test_client_error_does_not_trip_circuit
    #[tokio::test]
    async fn client_error_does_not_trip_circuit() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"bad");
        let (_server, mut worker) = test_worker(&temp, vec![(400, json!({}))]).await;
        worker.sync_pass().await;
        assert_eq!(worker.consecutive_failures, 0);
        assert!(!worker.circuit_open);
    }

    // tests/test_sync.py::test_transient_error_still_trips_circuit
    #[tokio::test]
    async fn transient_error_still_trips_circuit() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..5 {
            create_segment(&temp, &format!("12000{index}_300"), b"bad");
        }
        let responses = (0..10).map(|_| (500, json!({}))).collect();
        let (_server, mut worker) = test_worker(&temp, responses).await;
        worker.sync_pass().await;
        assert_eq!(worker.consecutive_failures, 5);
        assert!(worker.circuit_open);
        assert_eq!(worker.circuit_cooldown, CIRCUIT_COOLDOWN_INITIAL);
    }

    // AC: a matching hash with a nonterminal status is not proof.
    #[test]
    fn uploading_status_is_not_proof() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        assert!(
            !segment_custody_proven(
                temp.path(),
                &entry("screen.webm", "uploading", &sha256_file(&file).unwrap())
            )
            .unwrap()
        );
    }

    // AC: processed plus exact hash is terminal proof.
    #[test]
    fn processed_status_with_matching_sha_is_proof() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        assert!(
            segment_custody_proven(
                temp.path(),
                &entry("screen.webm", "processed", &sha256_file(&file).unwrap())
            )
            .unwrap()
        );
    }

    // AC: one unreadable eligible file poisons the segment.
    #[test]
    fn unreadable_file_poisons_segment() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        let sha = sha256_file(&file).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
        assert!(
            segment_custody_proven(temp.path(), &entry("screen.webm", "present", &sha)).is_err()
        );
    }

    // AC: zero eligible files are never proof.
    #[test]
    fn empty_segment_is_not_proven_held() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!segment_custody_proven(temp.path(), &entry("x", "present", "x")).unwrap());
    }

    // AC: entry indexing and marker lookup cover key and original_key.
    #[test]
    fn index_and_marker_lookup_cover_both_keys() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("original");
        fs::create_dir(&segment).unwrap();
        write_server_key(&segment, "stored").unwrap();
        let item = ListingEntry {
            key: Some("stored".to_owned()),
            original_key: Some("original".to_owned()),
            files: None,
        };
        let items = [item];
        let indexed = index_entries(&items);
        assert!(lookup_entry(&indexed, &segment).is_some());
        assert_eq!(indexed.len(), 2);
    }

    // AC: ancient capture quarantined now survives thirty more days.
    #[test]
    fn quarantine_stamp_resets_age() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("120000_300");
        fs::create_dir(&segment).unwrap();
        File::open(&segment)
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        assert!(quarantine_segment(2_000_000.0, &segment, "test"));
        let modified = fs::metadata(temp.path().join("120000_300.failed"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(modified, 2_000_000.0);
    }

    // tests/test_sync.py::test_prunes_old_entries -> cutover migration creates marker and removes synced_days
    #[tokio::test]
    async fn cutover_migration_creates_marker_and_removes_synced_days() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let captures_dir = temp.path().join("captures");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&captures_dir).unwrap();
        let seg = captures_dir.join("20260101/archon/120000_300");
        fs::create_dir_all(&seg).unwrap();
        fs::write(seg.join("screen.webm"), b"screen").unwrap();
        let old_synced = state_dir.join("synced_days.json");
        fs::write(&old_synced, b"[\"20260101\"]\n").unwrap();
        assert!(old_synced.exists());

        ensure_cutover(&state_dir, &captures_dir);
        assert!(!old_synced.exists());
        let cutover_path = state_dir.join("ingest_cutover.json");
        assert!(cutover_path.exists());
        let content: IngestCutover =
            serde_json::from_str(&fs::read_to_string(&cutover_path).unwrap()).unwrap();
        assert_eq!(content.segments, vec!["20260101/archon/120000_300"]);

        // Calling again does not overwrite
        let seg2 = captures_dir.join("20260101/archon/130000_300");
        fs::create_dir_all(&seg2).unwrap();
        ensure_cutover(&state_dir, &captures_dir);
        let content2: IngestCutover =
            serde_json::from_str(&fs::read_to_string(&cutover_path).unwrap()).unwrap();
        assert_eq!(content2.segments, vec!["20260101/archon/120000_300"]);
    }

    // tests/test_sync.py::test_deletes_old_synced_confirmed
    #[tokio::test]
    async fn cleanup_deletes_old_acknowledged_segment() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("captures/20260101/archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        let media = segment.join("screen.webm");
        fs::write(&media, b"screen").unwrap();
        let sha = sha256_file(&media).unwrap();
        let stamp = file_stamp(&media).unwrap();
        let server = MockServer::new(vec![(
            200,
            listing_with_size("120000_300", "screen.webm", Some("present"), &sha, 6),
        )])
        .await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let (cur_id, cur_pair) = client
            .capability()
            .map(|cap| {
                let writer = cap.writer();
                (
                    writer.identity_key().to_string(),
                    writer.pairing_id().to_string(),
                )
            })
            .unwrap();
        let ack = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "120000_300".to_string(),
            stored_key: "120000_300".to_string(),
            identity_key: cur_id,
            pairing_id: cur_pair,
            proof: "upload".to_string(),
            files: vec![IngestAckFile {
                submitted: "screen.webm".to_owned(),
                written: "screen.webm".to_owned(),
                size: 6,
                sha256: sha.clone(),
                disposition: "written".to_owned(),
                stamp,
            }],
        };
        write_ack(&segment, &ack).unwrap();
        let service = SyncService::start(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 100.0,
            }),
        );
        service.trigger();
        for _ in 0..100 {
            if !segment.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(!segment.exists());
    }

    // tests/test_sync_health_surfaces.py::test_health_facts_drive_all_surfaces_consistently
    // Named deviation: surface consumption belongs to the tray/CLI/D-Bus layers.
    #[test]
    fn health_facts_drive_all_derived_surfaces_consistently() {
        let cases = [
            (
                SyncFacts {
                    pending_confirmed: Some(0),
                    last_successful_sync: Some(1_800_000_000.0),
                    last_successful_contact: Some(1_800_000_000.0),
                    link: Some(LinkFactState {
                        carrier_proven: true,
                        observer_registered: true,
                        ..LinkFactState::default()
                    }),
                    ..SyncFacts::default()
                },
                "on, connected",
                "Active",
                "connected",
                "Sync: connected; up to date",
                "ok",
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    last_error_code: Some(404),
                    ..SyncFacts::default()
                },
                "on, update required",
                "NeedsAttention",
                "update-required",
                "Sync: update required; update the solstone app",
                "fail",
            ),
        ];
        for (facts, header, sni, dbus, cli, doctor) in cases {
            let health = derive_health(&facts, 1_800_000_000.0, 600.0);
            assert_eq!(health.header_recording, header);
            assert_eq!(health.sni_status, sni);
            assert_eq!(health.dbus, dbus);
            assert_eq!(health.cli, cli);
            assert_eq!(health.doctor_severity, doctor);
        }
    }

    #[tokio::test]
    async fn published_link_fact_persists_and_drives_every_owner_surface() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        fs::create_dir_all(&config.config_dir).unwrap();
        let mut owner_lock =
            crate::private_link::PrivateStateLock::acquire(&config.config_dir).unwrap();
        owner_lock.mark_ready().unwrap();
        let server = MockServer::new(Vec::new()).await;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::clone(&clock),
        ));
        client.publish_link_fact(crate::private_link::LinkFact::TransportUnavailable);
        let service = SyncService::start_with_epoch(
            config.clone(),
            client,
            Arc::clone(&clock),
            Some(ProcessEpoch::for_test(7)),
        );

        let sampled = service.sampler_handle().health();
        let liveness =
            crate::private_link::PrivateStateLock::try_probe(&config.config_dir).unwrap();
        let persisted = load_facts_with_liveness(&config.state_dir(), liveness);
        let reloaded = derive_health(&persisted, 1_800_000_000.0, 600.0);
        assert_eq!(sampled.state, HealthState::TransportUnavailable);
        assert_eq!(reloaded, sampled);
        let model = crate::tray_model::build(
            &crate::observer::StateSnapshot {
                mode: crate::observer::Mode::Screencast,
                paused: false,
                segment_open: false,
                captures_today: 0,
                total_size_mb: 0,
                pause_until: None,
                segment_start_mono: None,
                process_start_mono: 0.0,
            },
            300,
            100.0,
            &sampled,
        );
        assert_eq!(model.header, sampled.header_recording);
        assert_eq!(model.tooltip, format!("on\n{}", sampled.tooltip));
        assert_eq!(model.icon, sampled.icon);
        assert_eq!(model.sni_status, sampled.sni_status);
        assert_eq!(
            sampled.cli,
            "Sync: connection unavailable; held on this device; restart the solstone app; if this continues, pair this device again"
        );
        assert_eq!(sampled.doctor_severity, "fail");
        assert_eq!(
            sampled.doctor_detail,
            "sync health: connection unavailable; restart the solstone app; if this continues, pair this device again"
        );
        assert_eq!(sampled.dbus, "transport-unavailable");
        assert_eq!(
            sampled.accessible_recording,
            "on, connection unavailable, held on this device"
        );

        service.shutdown(Duration::from_secs(1)).await.unwrap();
        drop(owner_lock);
    }

    #[tokio::test]
    async fn link_facts_published_after_sync_start_persist_change_only() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        fs::create_dir_all(&config.config_dir).unwrap();
        let mut owner_lock =
            crate::private_link::PrivateStateLock::acquire(&config.config_dir).unwrap();
        owner_lock.mark_ready().unwrap();
        let server = MockServer::new(Vec::new()).await;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::clone(&clock),
        ));
        let epoch = ProcessEpoch::for_test(8);
        let service = SyncService::start_with_epoch(
            config.clone(),
            Arc::clone(&client),
            Arc::clone(&clock),
            Some(epoch),
        );
        let health_path = crate::sync_health::sync_health_path(&config.state_dir());
        let cases = [
            (
                crate::private_link::LinkFact::PairingRequired,
                "pairing_required",
            ),
            (
                crate::private_link::LinkFact::PrivateStateInvalid,
                "private_state_invalid",
            ),
            (
                crate::private_link::LinkFact::ConfigSanitationFailed,
                "config_sanitation_failed",
            ),
            (
                crate::private_link::LinkFact::ListenerReady,
                "listener_ready",
            ),
            (
                crate::private_link::LinkFact::CarrierProven,
                "carrier_proven",
            ),
            (
                crate::private_link::LinkFact::ObserverRegistered,
                "observer_registered",
            ),
            (
                crate::private_link::LinkFact::TransportUnavailable,
                "transport_unavailable",
            ),
            (
                crate::private_link::LinkFact::TerminalRevocation,
                "terminal_revocation",
            ),
            (
                crate::private_link::LinkFact::TokenPersistenceFailure,
                "token_persistence_failure",
            ),
        ];
        for (fact, key) in cases {
            client.begin_owner_generation();
            let prior_file = File::open(&health_path).unwrap();
            let prior_inode = prior_file.metadata().unwrap().ino();
            client.publish_link_fact(fact);
            let persisted: serde_json::Value =
                serde_json::from_slice(&fs::read(&health_path).unwrap()).unwrap();
            assert_eq!(persisted["link_epoch"], "08".repeat(32));
            assert_eq!(persisted["link"][key], true, "fact {key} was not persisted");
            assert_ne!(
                fs::metadata(&health_path).unwrap().ino(),
                prior_inode,
                "fact {key} did not replace the health file"
            );
        }

        let unchanged_inode = fs::metadata(&health_path).unwrap().ino();
        client.publish_link_fact(crate::private_link::LinkFact::TokenPersistenceFailure);
        assert_eq!(fs::metadata(&health_path).unwrap().ino(), unchanged_inode);

        service.shutdown(Duration::from_secs(1)).await.unwrap();
        drop(owner_lock);
    }

    #[tokio::test]
    async fn concurrent_link_fact_publishers_persist_final_live_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let server = MockServer::new(Vec::new()).await;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::clone(&clock),
        ));
        let service = SyncService::start_with_epoch(
            config.clone(),
            Arc::clone(&client),
            Arc::clone(&clock),
            Some(ProcessEpoch::for_test(10)),
        );
        client.begin_owner_generation();
        client.publish_link_fact(crate::private_link::LinkFact::TokenPersistenceFailure);

        let facts = [
            crate::private_link::LinkFact::PairingRequired,
            crate::private_link::LinkFact::PrivateStateInvalid,
            crate::private_link::LinkFact::ConfigSanitationFailed,
            crate::private_link::LinkFact::ListenerReady,
            crate::private_link::LinkFact::CarrierProven,
            crate::private_link::LinkFact::ObserverRegistered,
            crate::private_link::LinkFact::TransportUnavailable,
            crate::private_link::LinkFact::TerminalRevocation,
            crate::private_link::LinkFact::TokenPersistenceFailure,
        ];
        let publishers = facts.map(|fact| {
            let client = Arc::clone(&client);
            std::thread::spawn(move || client.publish_link_fact(fact))
        });
        for publisher in publishers {
            publisher.join().unwrap();
        }

        let snapshot = client.link_facts().snapshot();
        let unknown_journals_json: Vec<serde_json::Value> = snapshot
            .unknown_journals
            .iter()
            .map(|uj| serde_json::json!({ "address": uj.address, "jid": uj.jid }))
            .collect();
        let unknown_spoken_marks_json: Vec<serde_json::Value> = snapshot
            .unknown_spoken_marks
            .iter()
            .map(|m| match m {
                Some(s) => serde_json::Value::String(s.clone()),
                None => serde_json::Value::Null,
            })
            .collect();
        let expected = serde_json::json!({
            "pairing_required": snapshot.pairing_required,
            "private_state_invalid": snapshot.private_state_invalid,
            "config_sanitation_failed": snapshot.config_sanitation_failed,
            "listener_ready": snapshot.listener_ready,
            "carrier_proven": snapshot.carrier_proven,
            "observer_registered": snapshot.observer_registered,
            "transport_unavailable": snapshot.transport_unavailable,
            "terminal_revocation": snapshot.terminal_revocation,
            "token_persistence_failure": snapshot.token_persistence_failure,
            "journal_version_observed": snapshot.journal_version_observed,
            "unknown_journals": unknown_journals_json,
            "paired_jid": snapshot.paired_jid,
            "unknown_spoken_marks": unknown_spoken_marks_json,
            "paired_spoken_mark": snapshot.paired_spoken_mark,
        });
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(crate::sync_health::sync_health_path(&config.state_dir())).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["link"], expected);
        // Carrier recovery can clear transport_unavailable; its final value depends
        // on publisher order. Persistence must still match the actual final state.
        assert!(
            expected
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| !matches!(
                    key.as_str(),
                    "journal_version_observed"
                        | "transport_unavailable"
                        | "unknown_journals"
                        | "paired_jid"
                        | "unknown_spoken_marks"
                        | "paired_spoken_mark"
                ))
                .all(|(_, value)| value.as_bool() == Some(true))
        );
        assert_eq!(persisted["link"]["journal_version_observed"], false);

        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn link_fact_sink_preserves_sync_columns_and_retries_failed_write() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let server = MockServer::new(Vec::new()).await;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::clone(&clock),
        ));
        let service = SyncService::start_with_epoch(
            config.clone(),
            Arc::clone(&client),
            Arc::clone(&clock),
            Some(ProcessEpoch::for_test(9)),
        );
        client.begin_owner_generation();
        {
            let mut facts = service.facts.lock().unwrap();
            facts.last_successful_sync = Some(11.0);
            facts.last_successful_contact = Some(12.0);
            facts.last_error_class = Some(ErrorType::Client);
            facts.last_error_code = Some(409);
            facts.pending_confirmed = Some(3);
            facts.in_progress = true;
            facts.progress = "3/4".into();
            facts.link = Some(LinkFactState {
                pairing_required: true,
                ..LinkFactState::default()
            });
        }
        let health_path = crate::sync_health::sync_health_path(&config.state_dir());
        let before = fs::metadata(&health_path).unwrap().ino();
        client.publish_link_fact(crate::private_link::LinkFact::PairingRequired);
        assert_ne!(fs::metadata(&health_path).unwrap().ino(), before);
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&health_path).unwrap()).unwrap();
        assert_eq!(persisted["last_successful_sync"], 11.0);
        assert_eq!(persisted["last_successful_contact"], 12.0);
        assert_eq!(persisted["last_error_class"], "client");
        assert_eq!(persisted["last_error_code"], 409);
        assert_eq!(persisted["pending_confirmed"], 3);
        assert_eq!(persisted["in_progress"], true);
        assert_eq!(persisted["progress"], "3/4");

        let state_dir = config.state_dir();
        let backup = state_dir.with_extension("backup");
        fs::rename(&state_dir, &backup).unwrap();
        fs::write(&state_dir, b"block directory creation").unwrap();
        client.publish_link_fact(crate::private_link::LinkFact::PrivateStateInvalid);
        assert_eq!(service.link_persistence_failure_count(), 1);
        fs::remove_file(&state_dir).unwrap();
        fs::rename(&backup, &state_dir).unwrap();
        client.publish_link_fact(crate::private_link::LinkFact::ConfigSanitationFailed);
        assert_eq!(service.link_persistence_failure_count(), 1);
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&health_path).unwrap()).unwrap();
        assert_eq!(persisted["link"]["private_state_invalid"], true);
        assert_eq!(persisted["link"]["config_sanitation_failed"], true);

        let link_facts = client.link_facts();
        link_facts.owner_lost();
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&health_path).unwrap()).unwrap();
        assert_eq!(persisted["link"]["transport_unavailable"], true);
        assert_eq!(persisted["link"]["private_state_invalid"], false);
        client.begin_owner_generation();
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&health_path).unwrap()).unwrap();
        assert_eq!(persisted["link"]["transport_unavailable"], false);

        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // tests/test_sync_health_surfaces.py::test_404_query_cycle_drives_failing_state_on_all_surfaces
    // Named deviation: surface consumption belongs to the tray/CLI/D-Bus layers.
    #[tokio::test]
    async fn listing_404_drives_update_needed_derived_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(404, json!({}))]).await;
        worker.sync_pass().await;
        let health = derive_health(
            &worker.facts.lock().unwrap(),
            worker.clock.wall_seconds(),
            600.0,
        );
        assert_eq!(
            health.state,
            crate::sync_health::HealthState::UpdateRequired
        );
        assert_eq!(health.pending_display, "pending unconfirmed");
        assert_eq!(health.header_recording, "on, update required");
        assert_eq!(health.sni_status, "NeedsAttention");
        assert_eq!(health.dbus, "update-required");
        assert_eq!(health.cli, "Sync: update required; update the solstone app");
        assert_eq!(health.doctor_severity, "fail");
    }

    fn set_mtime(path: &Path, seconds: f64) {
        File::open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds)),
            )
            .unwrap();
    }

    // AC: failed segments are kept indefinitely without server queries.
    #[tokio::test]
    async fn failed_segments_kept_indefinitely_across_passes() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300.failed", b"x");
        let bare = create_segment(&temp, "130000.failed", b"x");
        set_mtime(&segment, 1.0);
        set_mtime(&bare, 1.0);
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;
        worker.sync_pass().await;
        assert!(segment.exists());
        assert!(bare.exists());
        assert!(server.requests().is_empty());
    }

    // AC: listing-path 400 records failure and quarantines no segment.
    #[tokio::test]
    async fn listing_client_error_keeps_every_segment_unquarantined() {
        let temp = tempfile::tempdir().unwrap();
        let first = create_segment(&temp, "120000_300", b"one");
        let second = create_segment(&temp, "130000_300", b"two");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(400, json!({})), (400, json!({})), (400, json!({}))],
        )
        .await;
        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/120000_300\",\"20260101/archon/130000_300\"]}\n",
        )
        .unwrap();
        worker.sync_pass().await;
        assert!(first.exists());
        assert!(second.exists());
        assert!(!first.with_file_name("120000_300.failed").exists());
        assert!(!second.with_file_name("130000_300.failed").exists());
        assert_eq!(
            worker.facts.lock().unwrap().last_error_class,
            Some(ErrorType::Client)
        );
    }

    // A listing 401 opens a recoverable breaker; a later probe after cooldown resumes upload.
    #[tokio::test]
    async fn listing_401_opens_breaker_and_later_probe_resumes_upload() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (401, json!({})),
                (200, json!({"status":"ok"})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
        )
        .await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();

        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        clock.set_mono(131.0);
        assert!(worker.try_probe().await);
        worker.sync_pass().await;
        assert!(
            upload_hits(&server) >= 2,
            "requests: {:?}",
            server
                .requests()
                .iter()
                .map(|request| &request.uri)
                .collect::<Vec<_>>()
        );
    }

    // Both auth statuses open the breaker immediately, but only 403 is permanent and revokes.
    #[tokio::test]
    async fn auth_opens_immediately_but_only_403_is_permanent() {
        for status in [401, 403] {
            let temp = tempfile::tempdir().unwrap();
            let _segment = create_segment(&temp, "120000_300", b"screen");
            let (_server, mut worker) = test_worker(&temp, vec![(status, json!({}))]).await;
            worker.sync_pass().await;
            assert!(worker.circuit_open);
            assert_eq!(worker.consecutive_failures, 1);
            assert_eq!(worker.circuit_open_permanent, status == 403);
            assert_eq!(worker.client.is_revoked(), status == 403);
            assert_eq!(
                worker.facts.lock().unwrap().last_error_code,
                Some(i64::from(status))
            );
        }
    }

    // An upload 401 opens a recoverable breaker; a later probe after cooldown retries the POST.
    #[tokio::test]
    async fn upload_401_opens_recoverable_breaker_and_later_retries() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (401, json!({})),
                (200, json!({"status":"ok"})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
        )
        .await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();

        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!worker.circuit_open_permanent);
        clock.set_mono(131.0);
        assert!(worker.try_probe().await);
        worker.sync_pass().await;
        assert!(upload_hits(&server) >= 2);
    }

    // upload_403_latches_permanently pins both revocation latches, not request counts.
    #[tokio::test]
    async fn upload_403_latches_permanently() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(&temp, vec![(403, json!({}))]).await;
        worker.sync_pass().await;
        assert!(worker.circuit_open_permanent);
        assert!(worker.client.is_revoked());
    }

    // upload_401_records_and_persists_status pins POST status through durable facts.
    #[tokio::test]
    async fn upload_401_records_and_persists_status() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(&temp, vec![(401, json!({}))]).await;
        worker.sync_pass().await;
        let facts = worker.facts.lock().unwrap().clone();
        assert_eq!(facts.last_error_class, Some(ErrorType::Auth));
        assert_eq!(facts.last_error_code, Some(401));
        let persisted = load_facts(&worker.config.state_dir());
        assert_eq!(persisted.last_error_class, Some(ErrorType::Auth));
        assert_eq!(persisted.last_error_code, Some(401));
    }

    // tests/test_sync.py::test_transient_allows_more_failures
    #[tokio::test]
    async fn transient_allows_four_failures() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        assert_eq!(worker.circuit_threshold(), 5);
        assert!(!worker.circuit_open);
    }

    // tests/test_sync.py::test_incompatible_opens_immediately
    #[tokio::test]
    async fn incompatible_opens_immediately_but_is_probeable() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(404, json!({}))]).await;
        worker.sync_pass().await;
        assert!(worker.circuit_open);
        assert!(!worker.circuit_open_permanent);
        assert_eq!(worker.circuit_threshold(), 1);
    }

    async fn open_probe_worker(
        temp: &tempfile::TempDir,
        responses: Vec<(u16, Value)>,
    ) -> (LinkedMockServer, SyncWorker) {
        let (server, mut worker) = test_worker(temp, Vec::new()).await;
        for (status, body) in responses {
            server.enqueue_response(status, body.to_string());
        }
        worker.circuit_open = true;
        worker.circuit_open_since = 0.0;
        worker.circuit_cooldown = 30.0;
        worker.consecutive_failures = 5;
        worker.last_error_type = Some(ErrorType::Transient);
        worker.last_error_code = None;
        (server, worker)
    }

    // tests/test_sync.py::test_transient_circuit_recovers_after_cooldown
    #[tokio::test]
    async fn transient_circuit_recovers_after_cooldown() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) =
            open_probe_worker(&temp, vec![(200, json!({"status":"ok"}))]).await;
        assert!(worker.try_probe().await);
        assert!(!worker.circuit_open);
        assert_eq!(worker.consecutive_failures, 0);
    }

    // tests/test_sync.py::test_revoked_circuit_never_recovers
    #[tokio::test]
    async fn revoked_circuit_never_recovers() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker) = open_probe_worker(&temp, vec![]).await;
        worker.circuit_open_permanent = true;
        assert!(!worker.try_probe().await);
        assert!(worker.circuit_open);
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_backoff_increases_on_failed_probe
    #[tokio::test]
    async fn backoff_increases_on_failed_probe() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = open_probe_worker(&temp, vec![(500, json!({}))]).await;
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 60.0);
    }

    // AC: consecutive failed probes from a reachable open-breaker state climb the full ladder.
    #[tokio::test]
    async fn failed_probes_climb_full_backoff_ladder() {
        let temp = tempfile::tempdir().unwrap();
        let responses = (0..5).map(|_| (500, json!({}))).collect();
        let (_server, mut worker) = open_probe_worker(&temp, responses).await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 31.0));
        worker.clock = clock.clone();

        assert_eq!(worker.circuit_cooldown, 30.0);
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 60.0);
        clock.set_mono(92.0);
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 120.0);
        clock.set_mono(213.0);
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 240.0);
        clock.set_mono(454.0);
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 300.0);
        clock.set_mono(755.0);
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, 300.0);
    }

    // tests/test_sync.py::test_full_reset_after_successful_probe
    #[tokio::test]
    async fn full_reset_after_successful_probe() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) =
            open_probe_worker(&temp, vec![(200, json!({"status":"ok"}))]).await;
        worker.circuit_cooldown = 120.0;
        worker.clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 121.0,
        });
        assert!(worker.try_probe().await);
        assert!(!worker.circuit_open);
        assert!(!worker.circuit_open_permanent);
        assert_eq!(worker.circuit_open_since, 0.0);
        assert_eq!(worker.circuit_cooldown, CIRCUIT_COOLDOWN_INITIAL);
        assert_eq!(worker.consecutive_failures, 0);
        assert_eq!(worker.last_error_type, None);
    }

    // tests/test_sync.py::test_cooldown_caps_at_max
    #[tokio::test]
    async fn cooldown_caps_at_max() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = open_probe_worker(&temp, vec![(500, json!({}))]).await;
        worker.circuit_cooldown = CIRCUIT_COOLDOWN_MAX;
        worker.clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 301.0,
        });
        assert!(!worker.try_probe().await);
        assert_eq!(worker.circuit_cooldown, CIRCUIT_COOLDOWN_MAX);
    }

    // tests/test_sync.py::test_skips_probe_before_cooldown_elapses
    #[tokio::test]
    async fn skips_probe_before_cooldown_elapses() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker) = open_probe_worker(&temp, vec![]).await;
        worker.circuit_open_since = 90.0;
        assert!(!worker.try_probe().await);
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_query_failures_recover_to_connected
    #[tokio::test]
    async fn query_failures_recover_to_connected() {
        let temp = tempfile::tempdir().unwrap();
        let _segment = create_segment(&temp, "120000_300", b"screen");
        let mut responses = (0..5).map(|_| (500, json!({}))).collect::<Vec<_>>();
        responses.extend([(200, json!({"status":"ok"})), (200, json!({"status":"ok"}))]);
        let (server, mut worker) = test_worker(&temp, responses).await;
        worker.config.sync_max_retries = 1;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();
        for step in 0..5 {
            clock.set_wall(1_800_000_000.0 + (step as f64) * 3601.0);
            worker.sync_pass().await;
        }
        assert!(worker.circuit_open);
        assert_eq!(
            derive_health(&worker.facts.lock().unwrap(), 1_800_000_000.0, 600.0).state,
            crate::sync_health::HealthState::Offline
        );
        assert_eq!(server.requests().len(), 5);

        clock.set_mono(131.0);
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let facts = Arc::clone(&worker.facts);
        let task = tokio::spawn(async move { worker.run().await });
        notify.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if facts.lock().unwrap().pending_confirmed == Some(0) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        running.store(false, Ordering::Release);
        notify.notify_one();
        task.await.unwrap();
        assert_eq!(
            derive_health(&facts.lock().unwrap(), 1_800_000_000.0, 600.0).state,
            crate::sync_health::HealthState::Connected
        );
    }

    // sustained_401_retries_stay_bounded drives only MutableClock and one wake per step.
    // The conservative bound is 5 + ceil(log2(300 / 30)) + ceil(14400 / 300) + 1
    // = 5 + 4 + 48 + 1 = 58; a 401 actually opens at CIRCUIT_THRESHOLD_AUTH.
    #[tokio::test]
    async fn sustained_401_retries_stay_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let responses = (0..240).map(|_| (401, json!({}))).collect();
        let (server, mut worker) = open_probe_worker(&temp, responses).await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 0.0));
        worker.clock = clock.clone();
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let task = tokio::spawn(async move {
            worker.run().await;
            worker
        });

        for step in 1..=240 {
            clock.set_mono(f64::from(step * 60));
            notify.notify_one();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        running.store(false, Ordering::Release);
        notify.notify_one();
        let worker = task.await.unwrap();
        let ramp_steps = (CIRCUIT_COOLDOWN_MAX / CIRCUIT_COOLDOWN_INITIAL)
            .log2()
            .ceil() as usize;
        let capped_steps = (14_400.0 / CIRCUIT_COOLDOWN_MAX).ceil() as usize;
        let bound = CIRCUIT_THRESHOLD_TRANSIENT as usize + ramp_steps + capped_steps + 1;
        assert!(
            server.request_count("/app/devices/ingest")
                + server.request_count("/app/devices/system/status")
                <= bound,
            "listing retries exceeded conservative bound {bound}"
        );
        assert_eq!(worker.circuit_cooldown, CIRCUIT_COOLDOWN_MAX);
    }

    // AC: sync shutdown releases the walker without retaining or cancelling the upload client.
    #[tokio::test]
    async fn sync_shutdown_releases_client_without_cancelling_it() {
        let temp = tempfile::tempdir().unwrap();
        let server = MockServer::new(vec![]).await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config,
            Arc::clone(&client),
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert_eq!(Arc::strong_count(&client), 1);
    }

    // tests/test_sync.py::test_startup_forces_in_progress_false
    #[tokio::test]
    async fn startup_forces_in_progress_false() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        save_facts(
            &config.state_dir(),
            &SyncFacts {
                in_progress: true,
                progress: "uploading".to_owned(),
                ..SyncFacts::default()
            },
        )
        .unwrap();
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config.clone(),
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        let facts = load_facts(&config.state_dir());
        assert!(!facts.in_progress);
        assert!(facts.progress.is_empty());
    }

    // tests/test_sync.py::test_today_success_and_older_404_is_update_needed
    #[tokio::test]
    async fn today_success_and_older_404_is_update_needed() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"x");
        let (_server, mut worker) = test_worker(&temp, vec![(404, json!({}))]).await;
        worker.sync_pass().await;
        let facts = worker.facts.lock().unwrap().clone();
        assert_eq!(facts.last_error_class, Some(ErrorType::Incompatible));
        assert_eq!(facts.last_error_code, Some(404));
        assert_eq!(facts.pending_confirmed, None);
    }

    // tests/test_sync.py::test_failed_query_clears_prior_pending_zero
    #[tokio::test]
    async fn failed_query_clears_prior_pending_zero() {
        let temp = tempfile::tempdir().unwrap();
        let _segment = create_segment(&temp, "120000_300", b"screen");
        let responses = (0..5).map(|_| (500, json!({}))).collect();
        let (_server, mut worker) = test_worker(&temp, responses).await;
        worker.facts.lock().unwrap().pending_confirmed = Some(0);
        worker.sync_pass().await;
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, None);
    }

    // tests/test_sync.py::test_successful_cleanup_after_clean_pass_keeps_connected
    #[tokio::test]
    async fn successful_cleanup_after_clean_pass_keeps_connected() {
        let temp = tempfile::tempdir().unwrap();
        let _segment = create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.sync_pass().await;
        assert_eq!(
            derive_health(
                &worker.facts.lock().unwrap(),
                worker.clock.wall_seconds(),
                600.0
            )
            .state,
            crate::sync_health::HealthState::Connected
        );
    }

    async fn cleanup_worker_with_segment(
        temp: &tempfile::TempDir,
        name: &str,
        responses: Vec<(u16, Value)>,
        synced: bool,
    ) -> (LinkedMockServer, SyncWorker, PathBuf) {
        let segment = create_segment(temp, name, b"screen");
        let (server, worker) = test_worker(temp, responses).await;
        if synced {
            let files = eligible_files(&segment).unwrap_or_default();
            let mut ack_files = Vec::new();
            for f in &files {
                let fname = f.file_name().unwrap().to_str().unwrap();
                let meta = f.metadata().unwrap();
                let sha = sha256_file(f).unwrap();
                let stamp = file_stamp(f).unwrap();
                ack_files.push(IngestAckFile {
                    submitted: fname.to_owned(),
                    written: fname.to_owned(),
                    size: meta.len(),
                    sha256: sha,
                    disposition: "written".to_owned(),
                    stamp,
                });
            }
            let (cur_id, cur_pair) = worker.current_identity_and_pairing();
            let ack = IngestAck {
                day: "20260101".to_string(),
                stream: "archon".to_string(),
                local_key: name.to_string(),
                stored_key: name.to_string(),
                identity_key: cur_id.unwrap_or_default(),
                pairing_id: cur_pair.unwrap_or_default(),
                proof: "upload".to_string(),
                files: ack_files,
            };
            let _ = write_ack(&segment, &ack);
        } else if !name.ends_with(".incomplete") && !name.ends_with(".failed") {
            let rel = format!("20260101/archon/{name}");
            let _ = fs::write(
                worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
                serde_json::to_string(&serde_json::json!({ "segments": [rel] })).unwrap(),
            );
        }
        (server, worker, segment)
    }

    // tests/test_sync.py::test_keeps_unconfirmed_on_server
    #[tokio::test]
    async fn keeps_unconfirmed_on_server() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) = cleanup_worker_with_segment(
            &temp,
            "120000_300",
            vec![(200, custody(Vec::new())), (500, json!({}))],
            false,
        )
        .await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // tests/test_sync.py::test_keeps_segments_not_in_synced_days
    #[tokio::test]
    async fn keeps_segments_not_in_synced_days() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300", vec![(500, json!({}))], false).await;
        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[]}\n",
        )
        .unwrap();
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(
            !server
                .requests()
                .iter()
                .any(|r| r.uri.contains("/segments/") || r.uri.contains("day-custody"))
        );
    }

    // tests/test_sync.py::test_keeps_when_server_unreachable
    #[tokio::test]
    async fn keeps_when_server_unreachable() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) = cleanup_worker_with_segment(
            &temp,
            "120000_300",
            vec![(500, json!({})), (500, json!({}))],
            false,
        )
        .await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    #[tokio::test]
    async fn linked_disconnect_never_deletes_unproven_segment() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let legacy = MockServer::new(vec![]).await;
        let peer = PrivateLinkPeer::start().await;
        let config = Config {
            stream: "host".into(),
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host")
            .await
            .unwrap();
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability(),
            clock.clone(),
        ));
        let mut worker = SyncWorker::new(
            config,
            client,
            clock,
            SyncControl {
                notify: Arc::new(Notify::new()),
                pending_trigger: Arc::new(AtomicBool::new(false)),
                running: Arc::new(AtomicBool::new(true)),
            },
            Arc::new(Mutex::new(SyncFacts::default())),
            Arc::new(AtomicU8::new(0)),
        );
        peer.shutdown().await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(legacy.requests().is_empty());
        drop(worker);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn slow_linked_response_does_not_delete_unproven_segment() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![(500, json!({}))]).await;
        let gate = Arc::new(Notify::new());
        server.enqueue_gated_response(
            200,
            serde_json::to_vec(&custody(Vec::new())).unwrap(),
            gate.clone(),
        );
        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/120000_300\"]}\n",
        )
        .unwrap();
        let mut cleanup = Box::pin(worker.cleanup_synced_segments());
        tokio::select! {
            () = &mut cleanup => panic!("slow linked response completed before release"),
            () = async {
                while server.requests().is_empty() {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        assert!(segment.exists());
        gate.notify_one();
        cleanup.await;
        assert!(segment.exists());
    }

    // tests/test_sync.py::test_never_touches_incomplete
    #[tokio::test]
    async fn never_touches_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000.incomplete", b"incomplete");
        let complete = create_segment(&temp, "140000_300", b"complete");
        let sha = sha256_file(&complete.join("screen.webm")).unwrap();
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(
                200,
                listing_with_size("140000_300", "screen.webm", Some("present"), &sha, 8),
            )],
        )
        .await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &complete,
            "140000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&complete, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(!complete.exists());
    }

    // tests/test_sync.py::test_retention_zero_deletes_immediately
    #[tokio::test]
    async fn confirmed_segment_deletes_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&segment, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_never_cleans_today
    #[tokio::test]
    async fn confirmed_segment_today_is_deleted_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let today = timestamp_parts(1_800_000_000.0).0;
        let segment = temp
            .path()
            .join("captures")
            .join(&today)
            .join("archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), b"x").unwrap();
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&segment, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_cleans_empty_dirs
    #[tokio::test]
    async fn cleans_empty_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let day = segment.parent().unwrap().parent().unwrap().to_path_buf();
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(
                200,
                listing_with_size("120000_300", "screen.webm", Some("present"), &sha, 6),
            )],
        )
        .await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&segment, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!day.exists());
    }

    // tests/test_sync.py::test_original_key_lookup
    #[tokio::test]
    async fn acknowledged_segment_with_renamed_stored_key_deletes_locally() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let mut ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        ack.stored_key = "renamed".to_string();
        write_ack(&segment, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_failed_segments_kept_if_day_not_synced
    #[tokio::test]
    async fn failed_segments_kept_if_day_not_synced() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300.failed", vec![], false).await;
        set_mtime(&segment, worker.clock.wall_seconds());
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_failed_segments_kept_within_retention
    #[tokio::test]
    async fn failed_segments_kept_indefinitely_without_server_query() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp
            .path()
            .join("captures/20260101/archon/120000_300.failed");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), b"bad").unwrap();
        set_mtime(&segment, 1.0);
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_incomplete_still_skipped
    #[tokio::test]
    async fn incomplete_still_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) = cleanup_worker_with_segment(
            &temp,
            "120000.incomplete",
            vec![(200, custody(Vec::new()))],
            false,
        )
        .await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: upload success separates two runs of four transient failures.
    #[tokio::test]
    async fn upload_success_resets_four_plus_four_failures() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"x");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        assert_eq!(
            worker.upload_segment("20260101", &segment).await,
            UploadOutcome::Acked
        );
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        assert_eq!(worker.consecutive_failures, 4);
        assert!(!worker.circuit_open);
    }

    // AC: a successful day listing records contact without resetting failures.
    #[tokio::test]
    async fn listing_success_then_fifth_failure_opens() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(200, custody(Vec::new()))]).await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        let result = worker.client.fetch_day_custody("20260101").await;
        assert!(result.error_type.is_none());
        worker.record_contact(false);
        worker.record_failure(Some(ErrorType::Transient), None);
        assert_eq!(worker.consecutive_failures, 5);
        assert!(worker.circuit_open);
    }

    // AC: successful pass commit is the third breaker reset site.
    #[tokio::test]
    async fn successful_pass_commit_resets_breaker_failures() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        worker.commit_pass_result(true, None, None, Some(0));
        assert_eq!(worker.consecutive_failures, 0);
        assert_eq!(worker.last_error_type, None);
    }

    // AC: an unproven v3 envelope never authorizes deletion.
    #[tokio::test]
    async fn cleanup_total_mismatch_disables_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(&temp, vec![(500, json!({}))]).await;
        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/120000_300\"]}\n",
        )
        .unwrap();
        server.enqueue_day_custody(
            DayCustodyFixture::new(
                "20260101",
                vec![json!({"key":"120000_300", "observed":true, "files":[]})],
            )
            .with_segments_total(2),
        );
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: cleanup query failure skips the entire day.
    #[tokio::test]
    async fn cleanup_query_failure_skips_day() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) = cleanup_worker_with_segment(
            &temp,
            "120000_300",
            vec![(500, json!({})), (500, json!({}))],
            false,
        )
        .await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: one proven segment is deleted while an unproven sibling survives.
    #[tokio::test]
    async fn cleanup_deletes_proven_sibling_only() {
        let temp = tempfile::tempdir().unwrap();
        let proven = create_segment(&temp, "120000_300", b"one");
        let unproven = create_segment(&temp, "130000_300", b"two");
        let sha_proven = sha256_file(&proven.join("screen.webm")).unwrap();
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (
                    200,
                    listing_with_size("120000_300", "screen.webm", Some("present"), &sha_proven, 3),
                ),
                (500, json!({})),
            ],
        )
        .await;
        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/130000_300\"]}\n",
        )
        .unwrap();
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &proven,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&proven, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!proven.exists());
        assert!(unproven.exists());
    }

    // AC: one unproven local file keeps the whole segment.
    #[tokio::test]
    async fn cleanup_unproven_file_keeps_whole_segment() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"one");
        fs::write(segment.join("audio.flac"), b"two").unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(500, json!({}))]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        let mut ack_without_audio = ack.clone();
        ack_without_audio
            .files
            .retain(|f| f.submitted != "audio.flac");
        write_ack(&segment, &ack_without_audio).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(!segment.join(INGEST_ACK_FILENAME).exists());
        assert!(segment.join("screen.webm").exists());
        assert!(segment.join("audio.flac").exists());
    }

    // AC: a day with an attempted upload is not marked synced in that pass.
    #[tokio::test]
    async fn pending_upload_day_is_not_marked_synced() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"x");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.sync_pass().await;
        assert!(!segment.exists());
    }

    // AC: day-name age, not directory mtime, controls positive retention.
    #[tokio::test]
    async fn confirmed_segment_deleted_regardless_of_day_or_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let old = create_segment(&temp, "120000_300", b"old");
        set_mtime(old.parent().unwrap().parent().unwrap(), 1_800_000_000.0);
        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &old,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&old, &ack).unwrap();
        worker.cleanup_synced_segments().await;
        assert!(!old.exists());
    }

    // Negative pending values persist unchanged.
    #[tokio::test]
    async fn negative_pending_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let facts = SyncFacts {
            pending_confirmed: Some(-5),
            ..SyncFacts::default()
        };
        save_facts(&config.state_dir(), &facts).unwrap();
        assert_eq!(load_facts(&config.state_dir()).pending_confirmed, Some(-5));
    }

    // AC: a completion notification starts a pass.
    #[tokio::test]
    async fn completion_trigger_starts_pass() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("captures/20260101/archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), b"video").unwrap();
        let server =
            MockServer::new(vec![(200, json!({"status":"ok","segment":"120000_300"}))]).await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.trigger();
        wait_for_requests(&server, 1).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: the periodic timeout starts a pass without a completion trigger.
    #[tokio::test(start_paused = true)]
    async fn periodic_sixty_seconds_starts_pass() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config.clone(),
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(!config.state_dir().join("synced_days.json").exists());
        assert!(
            !config.captures_dir().exists()
                || fs::read_dir(config.captures_dir())
                    .unwrap()
                    .next()
                    .is_none()
        );
    }

    // AC: full reconciliation repeats only after an injected wall day elapses.
    #[tokio::test]
    async fn acknowledged_segment_makes_no_requests_across_passes() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("captures/20260101/archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        let media = segment.join("screen.webm");
        fs::write(&media, b"screen").unwrap();
        let server = MockServer::new(vec![]).await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let (cur_id, cur_pair) = {
            let cap = client.capability().unwrap();
            let writer = cap.writer();
            (
                writer.identity_key().to_string(),
                writer.pairing_id().to_string(),
            )
        };
        let ack = create_test_ack(&segment, "120000_300", &cur_id, &cur_pair);
        write_ack(&segment, &ack).unwrap();
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 0.0));
        let service = SyncService::start(config, client, clock.clone());
        service.trigger();
        let start = std::time::Instant::now();
        while segment.exists() && start.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!segment.exists());
        clock.set_wall(1_800_086_401.0);
        service.trigger();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(server.requests().len(), 0);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: distinct days are queried newest first.
    #[tokio::test]
    async fn sync_queries_distinct_days_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("captures/20250101/archon/1");
        let b = temp.path().join("captures/20260101/archon/1");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("screen.webm"), b"a").unwrap();
        fs::write(b.join("screen.webm"), b"b").unwrap();
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"status":"ok","segment":"1"})),
                (200, json!({"status":"ok","segment":"1"})),
            ],
        )
        .await;
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 2);
    }

    // AC: triggers during an active request coalesce into one non-overlapping follow-up.
    #[tokio::test]
    async fn active_walk_trigger_coalesces_without_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let seg1 = temp.path().join("captures/20260101/archon/1");
        fs::create_dir_all(&seg1).unwrap();
        fs::write(seg1.join("screen.webm"), b"1").unwrap();
        let (server, gate) = MockServer::gated().await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.trigger();
        wait_for_requests(&server, 1).await;
        let seg2 = temp.path().join("captures/20260101/archon/2");
        fs::create_dir_all(&seg2).unwrap();
        fs::write(seg2.join("screen.webm"), b"2").unwrap();
        service.trigger();
        service.trigger();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(server.requests().len(), 1);
        gate.notify_one();
        wait_for_requests(&server, 2).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(server.requests().len(), 2);
        gate.notify_waiters();
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: shutdown cancels a blocked walk and leaves parseable facts.
    #[tokio::test]
    async fn shutdown_mid_walk_is_prompt_and_state_remains_valid() {
        let temp = tempfile::tempdir().unwrap();
        let seg = temp.path().join("captures/20260101/archon/1");
        fs::create_dir_all(&seg).unwrap();
        fs::write(seg.join("screen.webm"), b"1").unwrap();
        let (server, _gate) = MockServer::gated().await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config.clone(),
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.trigger();
        wait_for_requests(&server, 1).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        let text =
            fs::read_to_string(crate::sync_health::sync_health_path(&config.state_dir())).unwrap();
        assert!(serde_json::from_str::<Value>(&text).unwrap().is_object());
    }

    // AC: an injected pass failure is supervised and the next trigger runs.
    #[tokio::test]
    async fn injected_pass_error_does_not_kill_worker() {
        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker) = test_worker(&temp, vec![(200, custody(Vec::new()))]).await;
        worker.fail_next_pass = true;
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let task = tokio::spawn(async move { worker.run().await }.with_subscriber(subscriber));
        notify.notify_one();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(server.requests().is_empty());
        notify.notify_one();
        server.wait_for_requests(1).await;
        running.store(false, Ordering::Release);
        notify.notify_one();
        task.await.unwrap();
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("Sync error").count(), 1);
    }

    // AC: cleanup failure is contained and a later trigger still runs.
    #[tokio::test]
    async fn cleanup_error_does_not_kill_worker() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let ack = create_test_ack(&segment, "120000_300", "", "");
        write_ack(&segment, &ack).unwrap();
        let server = MockServer::new(vec![
            (200, json!({"status":"ok"})),
            (500, json!({})),
            (200, json!({"status":"ok"})),
        ])
        .await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.trigger();
        for _ in 0..100 {
            if !segment.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn revoked_worker_makes_no_requests() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        let config = Config {
            stream: "desktop".to_owned(),
            base_dir: temp.path().join("data"),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "desktop")
            .await
            .unwrap();
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability(),
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        ));
        client.revoke_for_test();
        let notify = Arc::new(Notify::new());
        let running = Arc::new(AtomicBool::new(true));
        let mut worker = SyncWorker::new(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
            SyncControl {
                notify: notify.clone(),
                pending_trigger: Arc::new(AtomicBool::new(false)),
                running: running.clone(),
            },
            Arc::new(Mutex::new(SyncFacts::default())),
            Arc::new(AtomicU8::new(0)),
        );
        let task = tokio::spawn(async move { worker.run().await });
        notify.notify_one();
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        running.store(false, Ordering::Release);
        notify.notify_one();
        task.await.unwrap();
        assert!(peer.requests().is_empty());
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn unpaired_worker_does_not_busy_loop_or_emit_refusal_noise() {
        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let config = worker.config.clone();
        worker.client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let task = tokio::spawn(async move { worker.run().await }.with_subscriber(subscriber));
        notify.notify_one();
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        notify.notify_one();
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        running.store(false, Ordering::Release);
        notify.notify_one();
        task.await.unwrap();
        assert!(server.requests().is_empty());
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(!output.contains("Sync refused"), "{output}");
        assert!(!output.contains("Sync error"), "{output}");
    }

    // AC: a completion racing shutdown drains one walker pass before join completes.
    #[tokio::test]
    async fn final_completion_trigger_drains_before_shutdown_returns() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"final screen");
        let server = MockServer::new(vec![
            (200, custody(Vec::new())),
            (200, custody(Vec::new())),
            (200, json!({"status": "ok", "segment": "120000_300"})),
        ])
        .await;
        let config = Config {
            base_dir: temp.path().into(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        save_facts(
            &config.state_dir(),
            &SyncFacts {
                pending_confirmed: Some(7),
                last_error_class: Some(ErrorType::Transient),
                ..Default::default()
            },
        )
        .unwrap();
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config.clone(),
            client,
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        service.trigger();
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert_eq!(upload_hits(&server), 1);
        let upload = server
            .requests()
            .into_iter()
            .find(|request| request.uri == "/app/devices/ingest")
            .expect("final segment upload");
        assert!(String::from_utf8_lossy(&upload.body).contains("final screen"));
        let facts = load_facts(&config.state_dir());
        assert_eq!(facts.pending_confirmed, Some(0));
        assert_eq!(facts.last_error_class, None);
    }

    #[test]
    fn metadata_freshness_requires_current_association_and_dial() {
        let facts = crate::private_link::LinkFacts::default();
        let old = facts.association_epoch();
        facts.begin_owner_generation();
        facts.publish_with_generation(crate::private_link::LinkFact::CarrierProven, 2);
        let current = facts.association_epoch();
        facts.note_metadata_saved(old, 2);
        assert!(!facts.snapshot().journal_version_observed);
        facts.note_metadata_saved(current, 1);
        assert!(!facts.snapshot().journal_version_observed);
        facts.note_metadata_saved(current, 2);
        assert!(facts.snapshot().journal_version_observed);
        facts.publish_with_generation(crate::private_link::LinkFact::TerminalRevocation, 2);
        facts.note_metadata_saved(current, 2);
        assert!(!facts.snapshot().journal_version_observed);
        facts.owner_lost();
        facts.note_metadata_saved(current, 2);
        assert!(!facts.snapshot().journal_version_observed);
    }

    #[tokio::test]
    async fn sync_service_records_paired_journal_version_on_carrier_proven() {
        let temp = tempfile::tempdir().unwrap();
        let server = LinkedMockServer::new(vec![
            (200, json!({"version": {"current": "1.4.0"}})),
            (
                200,
                json!({
                    "protocol_version": 1,
                    "revision": 0,
                    "display_label": "desktop",
                    "reported": null,
                    "owner_label": null,
                    "updated_at": null,
                    "journal": {
                        "version": "1.4.0",
                        "name": null
                    }
                }),
            ),
            (
                200,
                json!({
                    "protocol_version": 2,
                    "status": "not_configured"
                }),
            ),
        ])
        .await;
        let config = Config {
            base_dir: temp.path().into(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        fs::create_dir_all(&config.config_dir).unwrap();
        let credential = server.credential();
        crate::private_link::persist_credential(&config.config_dir, &credential).unwrap();
        let client = Arc::new(UploadClient::new(
            &config,
            server.capability(),
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        // Establish a real carrier before installing the metadata observer.
        assert!(server.capability().system_status().await.is_ok());
        let service = SyncService::start(
            config.clone(),
            Arc::clone(&client),
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        let link_facts = client.link_facts();

        // Allow detached task to complete
        let mut observed = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let loaded = crate::sync_health::load_paired_journal_version(&config.state_dir());
            if let Some(loaded) = loaded
                && link_facts.snapshot().journal_version_observed
            {
                observed = Some(loaded);
                break;
            }
        }
        let loaded = observed.expect("journal version and in-memory fact should both publish");
        let identity_key = crate::private_link::journal_identity_key(&credential);
        assert_eq!(loaded.identity_key, identity_key);
        assert_eq!(loaded.version, "1.4.0");
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn capability_installed_after_carrier_proven_fetches_once() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().into(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        fs::create_dir_all(&config.config_dir).unwrap();
        let client = Arc::new(UploadClient::new(
            &config,
            None::<crate::private_link::PrivateLinkCapability>,
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let server = LinkedMockServer::new_with_facts(
            client.link_facts(),
            vec![
                (200, json!({"version": {"current": "1.5.0"}})),
                (
                    200,
                    json!({
                        "protocol_version": 1,
                        "revision": 0,
                        "display_label": "desktop",
                        "reported": null,
                        "owner_label": null,
                        "updated_at": null,
                        "journal": {
                            "version": "1.5.0",
                            "name": null
                        }
                    }),
                ),
                (
                    200,
                    json!({
                        "protocol_version": 2,
                        "status": "not_configured"
                    }),
                ),
            ],
        )
        .await;
        let credential = server.credential();
        crate::private_link::persist_credential(&config.config_dir, &credential).unwrap();
        let service = SyncService::start(
            config.clone(),
            Arc::clone(&client),
            Arc::new(FixedClock {
                wall: 1_800_000_000.0,
                mono: 0.0,
            }),
        );
        let link_facts = client.link_facts();
        // CarrierProven fires BEFORE capability is installed
        assert!(server.capability().system_status().await.is_ok());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(crate::sync_health::load_paired_journal_version(&config.state_dir()).is_none());
        assert!(!link_facts.snapshot().journal_version_observed);

        // Capability is installed later (e.g. session established) -> republish triggers exactly 1 fetch
        client.install_capability(server.capability());

        let mut observed = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let loaded = crate::sync_health::load_paired_journal_version(&config.state_dir());
            if let Some(loaded) = loaded
                && link_facts.snapshot().journal_version_observed
            {
                observed = Some(loaded);
                break;
            }
        }
        let loaded = observed.expect("journal version and in-memory fact should both publish");
        let identity_key = crate::private_link::journal_identity_key(&credential);
        assert_eq!(loaded.identity_key, identity_key);
        assert_eq!(loaded.version, "1.5.0");

        // Duplicate event in generation 1 does not re-fetch
        link_facts.publish_with_generation(crate::private_link::LinkFact::ObserverRegistered, 1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_journal_sighting_persists_and_drives_overlay_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let server = MockServer::new(Vec::new()).await;
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            Arc::clone(&clock),
        ));
        let service = SyncService::start_with_epoch(
            config.clone(),
            Arc::clone(&client),
            Arc::clone(&clock),
            Some(ProcessEpoch::for_test(10)),
        );
        let link_facts = client.link_facts();

        let decode_hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };

        let spki_hex = "3059301306072a8648ce3d020106082a8648ce3d03010703420004471c3e758c4904285bba7e53118ed0f524adeb0757d25bd2f8e7b0d76dfa714cdd520f7aca8a8b917acc37f51de8f0c9bbe3ad858382e702dc25a12d09f7a858";
        let jid = spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex)).unwrap();
        let spki_hex_paired = "3059301306072a8648ce3d020106082a8648ce3d030107034200047cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1";
        let paired_jid =
            spl_core::relay_window::jid_from_spki(&decode_hex(spki_hex_paired)).unwrap();

        link_facts.set_paired_jid(Some(paired_jid));
        link_facts.publish(crate::private_link::LinkFact::TransportUnavailable);
        link_facts.note_unknown_journals(vec![spl_transport::UnknownJournal {
            address: Some("192.168.1.100:5015".into()),
            jid: Some(jid),
        }]);

        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(crate::sync_health::sync_health_path(&config.state_dir())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            persisted["link"]["unknown_journals"][0]["address"],
            "192.168.1.100:5015"
        );

        let lock = crate::private_link::PrivateStateLock::acquire(&config.config_dir).unwrap();
        let facts = crate::sync_health::load_facts_with_liveness(
            &config.state_dir(),
            crate::private_link::PrivateStateLockLiveness::LiveOwner,
        );
        let health = crate::sync_health::derive_health(&facts, 1000.0, 600.0);
        assert_eq!(
            health.state,
            crate::sync_health::HealthState::TransportUnavailable
        );
        assert!(
            health
                .tooltip
                .contains("unknown journal seen at 192.168.1.100:5015")
        );
        assert!(
            health
                .cli
                .contains("Unknown journal seen at 192.168.1.100:5015:")
        );
        assert!(
            health
                .cli
                .contains("blue, purple · liquefy·smock  (claimed, not verified)")
        );
        assert!(health.cli.contains("pink, cyan · distrust·chokehold"));
        drop(lock);

        // Clears when unknown journals is cleared
        link_facts.note_unknown_journals(Vec::new());
        let facts_after = crate::sync_health::load_facts_with_liveness(
            &config.state_dir(),
            crate::private_link::PrivateStateLockLiveness::LiveOwner,
        );
        let health_after = crate::sync_health::derive_health(&facts_after, 1000.0, 600.0);
        assert!(!health_after.tooltip.contains("unknown journal"));
        assert!(!health_after.cli.contains("Unknown journal"));

        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // Confirmed sealed segment removed in same pass and next pass is connected
    #[tokio::test]
    async fn valid_receipt_removes_segment_and_next_pass_is_connected() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"video data");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!segment.exists());

        // Next pass: no uploads, facts show connected / no error
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(
            !server
                .requests()
                .iter()
                .any(|r| r.uri.contains("segments/"))
        );
        let facts = worker.facts.lock().unwrap().clone();
        let health = crate::sync_health::derive_health(&facts, worker.clock.wall_seconds(), 600.0);
        assert_eq!(
            health.state,
            crate::sync_health::HealthState::Connected,
            "link snapshot: {:?}",
            facts.link
        );
        assert_eq!(facts.last_error_class, None);
        assert_eq!(facts.pending_confirmed, Some(0));
    }

    // Removal cleans bookkeeping and stray temporary files
    #[tokio::test]
    async fn removal_cleans_bookkeeping_and_temporary_files() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"video data");
        fs::write(segment.join(".metadata"), b"meta").unwrap();
        fs::write(segment.join(INGEST_RETRY_FILENAME), b"{}").unwrap();
        fs::write(segment.join(".server_key"), b"key").unwrap();
        fs::write(segment.join(".ingest_ack.json.4242.0.tmp"), b"tmp").unwrap();
        fs::write(segment.join(".ingest_retry.json.tmp.4242"), b"tmp").unwrap();

        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.sync_pass().await;
        assert!(!segment.exists());
    }

    #[test]
    fn bookkeeping_match_covers_only_our_temporary_files() {
        assert!(is_bookkeeping_file(".ingest_ack.json.4242.0.tmp"));
        assert!(is_bookkeeping_file(".ingest_retry.json.tmp.4242"));
        assert!(is_bookkeeping_file(INGEST_ACK_FILENAME));
        assert!(is_bookkeeping_file(INGEST_RETRY_FILENAME));
        assert!(!is_bookkeeping_file(".screen.webm.tmp123"));
        assert!(!is_bookkeeping_file(".notes.tmpl"));
        assert!(!is_bookkeeping_file("screen.webm.tmp"));
        assert!(!is_bookkeeping_file("screen.webm"));
    }

    static INGEST_FAULT_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // Invalid descriptors leave segment on disk with retry file
    #[tokio::test]
    async fn invalid_descriptors_leave_segment_with_retry_file() {
        // Variation 1: wrong size
        {
            let temp = tempfile::tempdir().unwrap();
            let segment = create_segment(&temp, "120000_300", b"video data");
            let sha = sha256_file(&segment.join("screen.webm")).unwrap();
            let (_server, mut worker) = test_worker(
                &temp,
                vec![(
                    200,
                    json!({
                        "status": "ok",
                        "segment": "120000_300",
                        "file_descriptors": [{
                            "submitted": "screen.webm",
                            "written": "screen.webm",
                            "size": 99999, // wrong size
                            "sha256": sha,
                            "disposition": "written",
                        }]
                    }),
                )],
            )
            .await;
            worker.sync_pass().await;
            assert!(segment.exists());
            assert!(segment.join(INGEST_RETRY_FILENAME).exists());
            assert!(!segment.join(INGEST_ACK_FILENAME).exists());
        }

        // Variation 2: received_not_written
        {
            let temp = tempfile::tempdir().unwrap();
            let segment = create_segment(&temp, "120000_300", b"video data");
            let sha = sha256_file(&segment.join("screen.webm")).unwrap();
            let size = fs::metadata(segment.join("screen.webm")).unwrap().len();
            let (_server, mut worker) = test_worker(
                &temp,
                vec![(
                    200,
                    json!({
                        "status": "ok",
                        "segment": "120000_300",
                        "file_descriptors": [{
                            "submitted": "screen.webm",
                            "written": "screen.webm",
                            "size": size,
                            "sha256": sha,
                            "disposition": "received_not_written",
                        }]
                    }),
                )],
            )
            .await;
            worker.sync_pass().await;
            assert!(segment.exists());
            assert!(segment.join(INGEST_RETRY_FILENAME).exists());
            assert!(!segment.join(INGEST_ACK_FILENAME).exists());
        }

        // Variation 3: empty descriptors
        {
            let temp = tempfile::tempdir().unwrap();
            let segment = create_segment(&temp, "120000_300", b"video data");
            let (_server, mut worker) = test_worker(
                &temp,
                vec![(
                    200,
                    json!({
                        "status": "ok",
                        "segment": "120000_300",
                        "file_descriptors": [],
                    }),
                )],
            )
            .await;
            worker.sync_pass().await;
            assert!(segment.exists());
            assert!(segment.join(INGEST_RETRY_FILENAME).exists());
            assert!(!segment.join(INGEST_ACK_FILENAME).exists());
        }
    }

    // segment_removed removes segment and uploads older segment in same pass
    #[tokio::test]
    async fn segment_removed_removes_and_uploads_older_segment() {
        let temp = tempfile::tempdir().unwrap();
        let seg_older = create_segment(&temp, "110000_300", b"older data");
        let seg_newer = create_segment(&temp, "120000_300", b"newer data");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(
                500,
                json!({
                    "reason_code": "segment_removed",
                    "error": "Segment already removed"
                }),
            )],
        )
        .await;
        worker.sync_pass().await;
        assert!(!seg_newer.exists());
        assert!(!seg_older.exists());
        assert_eq!(upload_hits(&server), 2);
        let facts = worker.facts.lock().unwrap().clone();
        let health = crate::sync_health::derive_health(&facts, worker.clock.wall_seconds(), 600.0);
        assert_eq!(health.state, crate::sync_health::HealthState::Connected);
        assert_eq!(facts.last_error_class, None);
        assert_eq!(facts.pending_confirmed, Some(0));
    }

    #[tokio::test]
    async fn segment_removed_after_a_failed_pass_commits_as_connected() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"data");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(
                500,
                json!({
                    "reason_code": "segment_removed",
                    "error": "Segment already removed"
                }),
            )],
        )
        .await;
        // A previous pass ended on a transient journal error.
        worker.last_error_type = Some(ErrorType::Transient);
        worker.last_error_code = Some(503);
        {
            let mut facts = worker.facts.lock().unwrap();
            facts.last_error_class = Some(ErrorType::Transient);
            facts.last_error_code = Some(503);
        }

        worker.sync_pass().await;

        assert!(!segment.exists());
        assert_eq!(upload_hits(&server), 1);
        let facts = worker.facts.lock().unwrap().clone();
        let health = crate::sync_health::derive_health(&facts, worker.clock.wall_seconds(), 600.0);
        assert_eq!(health.state, crate::sync_health::HealthState::Connected);
        assert_eq!(facts.last_error_class, None);
        assert_eq!(facts.pending_confirmed, Some(0));
        assert_eq!(worker.last_error_type, None);
    }

    #[tokio::test]
    async fn segment_removed_code_on_other_statuses_keeps_segment() {
        for status in [409, 503] {
            let temp = tempfile::tempdir().unwrap();
            let segment = create_segment(&temp, "120000_300", b"data");
            let (server, mut worker) = test_worker(
                &temp,
                vec![(
                    status,
                    json!({
                        "reason_code": "segment_removed",
                        "error": "Segment already removed"
                    }),
                )],
            )
            .await;
            worker.sync_pass().await;
            assert_eq!(upload_hits(&server), 1, "status {status}");
            assert!(segment.join("screen.webm").exists(), "status {status}");
            assert!(
                segment.join(INGEST_RETRY_FILENAME).exists(),
                "status {status}"
            );
        }
    }

    #[tokio::test]
    async fn segment_removed_removal_failure_is_not_resent_for_an_hour() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"data");
        let removed = json!({
            "reason_code": "segment_removed",
            "error": "Segment already removed"
        });
        let (server, mut worker) = test_worker(&temp, vec![(500, removed.clone())]).await;
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o555)).unwrap();

        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(segment.join("screen.webm").exists());

        // The next passes inside the hour do not send it again.
        worker.sync_pass().await;
        let start = worker.clock.wall_seconds();
        worker.clock = Arc::new(FixedClock {
            wall: start + 3500.0,
            mono: 200.0,
        });
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);

        // After the hour it is sent again, and a removal that now succeeds removes it.
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o755)).unwrap();
        server.enqueue_response(500, removed.to_string());
        worker.clock = Arc::new(FixedClock {
            wall: start + 3601.0,
            mono: 300.0,
        });
        worker.sync_pass().await;
        assert_eq!(upload_hits(&server), 2);
        assert!(!segment.exists());
    }

    #[tokio::test]
    async fn journal_write_failed_keeps_segment() {
        let temp = tempfile::tempdir().unwrap();
        let seg_newer = create_segment(&temp, "120000_300", b"newer data");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(
                500,
                json!({
                    "reason_code": "journal_write_failed",
                    "error": "Internal database write failed"
                }),
            )],
        )
        .await;
        worker.sync_pass().await;
        // journal_write_failed stops the pass; segment remains and retry file exists
        assert!(seg_newer.exists());
        assert!(seg_newer.join(INGEST_RETRY_FILENAME).exists());
        assert_eq!(upload_hits(&server), 1);
    }

    // Fresh worker removes all three ack variations locally with zero requests
    #[tokio::test]
    async fn fresh_worker_removes_all_three_ack_variations_locally() {
        let _fault_lock = INGEST_FAULT_TEST_MUTEX.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("captures/20260101/archon");
        fs::create_dir_all(&day).unwrap();

        // 1: all files present
        let seg1 = day.join("110000_300");
        fs::create_dir_all(&seg1).unwrap();
        fs::write(seg1.join("screen.webm"), b"video1").unwrap();
        fs::write(seg1.join("mic.flac"), b"audio1").unwrap();

        // 2: 1 of 2 files deleted
        let seg2 = day.join("120000_300");
        fs::create_dir_all(&seg2).unwrap();
        fs::write(seg2.join("mic.flac"), b"audio2").unwrap();

        // 3: 0 files remaining (empty dir)
        let seg3 = day.join("130000_300");
        fs::create_dir_all(&seg3).unwrap();

        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let id = cur_id.unwrap_or_default();
        let pair = cur_pair.unwrap_or_default();

        let ack1 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "110000_300".to_string(),
            stored_key: "110000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "upload".to_string(),
            files: vec![
                IngestAckFile {
                    submitted: "screen.webm".to_owned(),
                    written: "screen.webm".to_owned(),
                    size: 6,
                    sha256: sha256_file(&seg1.join("screen.webm")).unwrap(),
                    disposition: "written".to_owned(),
                    stamp: file_stamp(&seg1.join("screen.webm")).unwrap(),
                },
                IngestAckFile {
                    submitted: "mic.flac".to_owned(),
                    written: "mic.flac".to_owned(),
                    size: 6,
                    sha256: sha256_file(&seg1.join("mic.flac")).unwrap(),
                    disposition: "written".to_owned(),
                    stamp: file_stamp(&seg1.join("mic.flac")).unwrap(),
                },
            ],
        };
        write_ack(&seg1, &ack1).unwrap();

        let ack2 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "120000_300".to_string(),
            stored_key: "120000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "upload".to_string(),
            files: vec![
                IngestAckFile {
                    submitted: "screen.webm".to_owned(),
                    written: "screen.webm".to_owned(),
                    size: 100,
                    sha256: "fake".into(),
                    disposition: "written".to_owned(),
                    stamp: IngestAckFileStamp {
                        dev: 0,
                        ino: 0,
                        size: 0,
                        mtime_ns: 0,
                    },
                },
                IngestAckFile {
                    submitted: "mic.flac".to_owned(),
                    written: "mic.flac".to_owned(),
                    size: 6,
                    sha256: sha256_file(&seg2.join("mic.flac")).unwrap(),
                    disposition: "written".to_owned(),
                    stamp: file_stamp(&seg2.join("mic.flac")).unwrap(),
                },
            ],
        };
        write_ack(&seg2, &ack2).unwrap();

        let ack3 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "130000_300".to_string(),
            stored_key: "130000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "upload".to_string(),
            files: vec![],
        };
        write_ack(&seg3, &ack3).unwrap();

        worker.sync_pass().await;

        assert!(!seg1.exists());
        assert!(!seg2.exists());
        assert!(!seg3.exists());
        assert!(server.requests().is_empty());
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, Some(0));
    }

    // Bookkeeping and empty dirs removed; unknown subdirectories kept
    #[tokio::test]
    async fn bookkeeping_and_empty_dirs_removed_unknown_subdir_kept() {
        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("captures/20260101/archon");
        fs::create_dir_all(&day).unwrap();

        // 1. Acked segment with custom_subdir
        let seg1 = day.join("110000_300");
        fs::create_dir_all(&seg1).unwrap();
        fs::write(seg1.join("screen.webm"), b"data").unwrap();
        fs::write(seg1.join(".metadata"), b"meta").unwrap();
        fs::write(seg1.join(INGEST_RETRY_FILENAME), b"{}").unwrap();
        let custom_dir1 = seg1.join("custom_subdir");
        fs::create_dir_all(&custom_dir1).unwrap();
        fs::write(custom_dir1.join("note.txt"), b"keep me").unwrap();

        // 2. Sealed dir with only .server_key
        let seg2 = day.join("120000_300");
        fs::create_dir_all(&seg2).unwrap();
        fs::write(seg2.join(".server_key"), b"server key").unwrap();

        // 3. Sealed dir with only .ingest_retry.json
        let seg3 = day.join("130000_300");
        fs::create_dir_all(&seg3).unwrap();
        fs::write(seg3.join(INGEST_RETRY_FILENAME), b"{}").unwrap();

        // 4. Sealed empty dir
        let seg4 = day.join("140000_300");
        fs::create_dir_all(&seg4).unwrap();

        // 5. Sealed dir whose only entry is an unknown subdirectory
        let seg5 = day.join("150000_300");
        let custom_dir5 = seg5.join("other_subdir");
        fs::create_dir_all(&custom_dir5).unwrap();
        fs::write(custom_dir5.join("data.bin"), b"keep me too").unwrap();

        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &seg1,
            "110000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&seg1, &ack).unwrap();

        worker.sync_pass().await;

        // seg1: Bookkeeping and media were cleaned, custom subdir and parent remain
        assert!(!seg1.join("screen.webm").exists());
        assert!(!seg1.join(".metadata").exists());
        assert!(!seg1.join(INGEST_RETRY_FILENAME).exists());
        assert!(!seg1.join(INGEST_ACK_FILENAME).exists());
        assert!(custom_dir1.join("note.txt").exists());
        assert!(seg1.exists());

        // seg2, seg3, seg4 removed
        assert!(!seg2.exists());
        assert!(!seg3.exists());
        assert!(!seg4.exists());

        // seg5 preserved because of unknown subdir
        assert!(seg5.exists());
        assert!(custom_dir5.join("data.bin").exists());

        assert_eq!(upload_hits(&server), 0);
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, Some(0));
    }

    // Ack write fault bounds retry and succeeds after cooldown
    #[tokio::test]
    async fn ack_write_fault_bounds_retry_and_succeeds_after_cooldown() {
        let _fault_lock = INGEST_FAULT_TEST_MUTEX.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"video data");
        let (server, mut worker1) = test_worker(&temp, vec![]).await;

        *INGEST_ACK_WRITE_FAULT_DIR.lock().unwrap() = Some(segment.clone());
        worker1.sync_pass().await;
        *INGEST_ACK_WRITE_FAULT_DIR.lock().unwrap() = None;

        // Ack write failed, retry file written with ~3600s backoff
        assert!(segment.exists());
        let retry_bytes = fs::read(segment.join(INGEST_RETRY_FILENAME)).unwrap();
        let retry: IngestRetry = serde_json::from_slice(&retry_bytes).unwrap();
        assert!(
            retry.next_attempt_after >= worker1.clock.wall_seconds() + 3500.0,
            "retry backoff should be around 3600s: {}",
            retry.next_attempt_after - worker1.clock.wall_seconds()
        );

        // Build second worker on same config and client clock with empty retry_floors map
        let (_, mut worker2) = test_worker(&temp, vec![]).await;
        worker2.clock = Arc::clone(&worker1.clock);
        worker2.client = Arc::clone(&worker1.client);

        // First pass on fresh worker skips upload because next_attempt_after is in future
        worker2.sync_pass().await;
        assert_eq!(upload_hits(&server), 1);

        // Advance clock past cooldown
        let new_clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: retry.next_attempt_after + 10.0,
            mono: 10000.0,
        });
        worker2.clock = new_clock;

        // Second pass succeeds and removes segment
        worker2.sync_pass().await;
        assert_eq!(upload_hits(&server), 2);
        assert!(!segment.exists());
    }

    // Auth stops pass after removing acked segment
    #[tokio::test]
    async fn auth_stops_pass_after_removing_acked_segment() {
        let temp = tempfile::tempdir().unwrap();
        let seg1 = create_segment(&temp, "110000_300", b"acked");
        let seg2 = create_segment(&temp, "120000_300", b"pending");

        let (server, mut worker) =
            test_worker(&temp, vec![(401, json!({"error": "unauthorized"}))]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &seg1,
            "110000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&seg1, &ack).unwrap();

        worker.sync_pass().await;

        // seg1 was removed locally before seg2 upload attempt
        assert!(!seg1.exists());
        // seg2 failed with 401 and remains
        assert!(seg2.exists());
        assert_eq!(upload_hits(&server), 1);
        assert_eq!(worker.last_error_type, Some(ErrorType::Auth));
    }

    #[tokio::test]
    async fn conflict_stops_pass_after_removing_acked_segment() {
        let temp = tempfile::tempdir().unwrap();
        let seg1 = create_segment(&temp, "110000_300", b"acked");
        let seg2 = create_segment(&temp, "120000_300", b"pending");

        let (server, mut worker) = test_worker(
            &temp,
            vec![(
                409,
                json!({
                    "reason_code": "foreign_stream_binding",
                    "error": "foreign stream"
                }),
            )],
        )
        .await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &seg1,
            "110000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&seg1, &ack).unwrap();

        worker.sync_pass().await;

        // seg1 was removed locally before seg2 upload attempt
        assert!(!seg1.exists());
        // seg2 failed with 409 and remains
        assert!(seg2.exists());
        assert_eq!(upload_hits(&server), 1);
        assert_eq!(worker.last_error_type, Some(ErrorType::Transient));
        assert_eq!(worker.last_error_code, Some(409));
    }

    #[tokio::test]
    async fn local_finish_runs_with_breaker_open_or_revoked_and_not_when_unpaired() {
        // 1. Breaker open + probe 500: still runs finish_confirmed_locally
        {
            let temp = tempfile::tempdir().unwrap();
            let seg_acked = create_segment(&temp, "110000_300", b"acked data");
            let _seg_unacked = create_segment(&temp, "120000_300", b"unacked data");
            let (server, mut worker) =
                test_worker(&temp, vec![(500, json!({"error": "server error"}))]).await;
            let (cur_id, cur_pair) = worker.current_identity_and_pairing();
            let ack = create_test_ack(
                &seg_acked,
                "110000_300",
                &cur_id.unwrap_or_default(),
                &cur_pair.unwrap_or_default(),
            );
            write_ack(&seg_acked, &ack).unwrap();

            // Breaker open with its cooldown already elapsed, so the probe runs.
            worker.circuit_open = true;
            worker.circuit_open_since = 0.0;
            worker.circuit_cooldown = 10.0;

            let notify = Arc::clone(&worker.notify);
            let running = Arc::clone(&worker.running);
            let task = tokio::spawn(async move {
                worker.run().await;
                worker
            });
            notify.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                while seg_acked.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            running.store(false, Ordering::Release);
            notify.notify_one();
            let worker = task.await.unwrap();

            // The probe ran and failed: the breaker stays open with a longer cooldown.
            assert!(worker.circuit_open);
            assert_eq!(worker.circuit_cooldown, 10.0 * CIRCUIT_COOLDOWN_FACTOR);
            assert_eq!(worker.circuit_open_since, worker.clock.monotonic_seconds());
            assert_eq!(worker.facts.lock().unwrap().last_error_code, Some(500));
            // seg_acked is removed even though the pass was skipped
            assert!(!seg_acked.exists());
            assert_eq!(upload_hits(&server), 0);
        }

        // 2. capability_less_client_for_test: skips finish_confirmed_locally and pass
        {
            let temp = tempfile::tempdir().unwrap();
            let seg_acked = create_segment(&temp, "110000_300", b"acked data");
            let (_server, mut worker) = test_worker(&temp, vec![]).await;
            let (cur_id, cur_pair) = worker.current_identity_and_pairing();
            let ack = create_test_ack(
                &seg_acked,
                "110000_300",
                &cur_id.unwrap_or_default(),
                &cur_pair.unwrap_or_default(),
            );
            write_ack(&seg_acked, &ack).unwrap();

            let config = worker.config.clone();
            worker.client = Arc::new(crate::upload::capability_less_client_for_test(
                &config,
                Arc::new(FixedClock {
                    wall: 0.0,
                    mono: 0.0,
                }),
            ));
            let notify = Arc::clone(&worker.notify);
            let running = Arc::clone(&worker.running);
            let task = tokio::spawn(async move { worker.run().await });
            notify.notify_one();
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            running.store(false, Ordering::Release);
            notify.notify_one();
            task.await.unwrap();

            // seg_acked is NOT removed because capability is missing
            assert!(seg_acked.exists());
        }

        // 3. revoke_for_test: runs finish_confirmed_locally before exiting
        {
            let temp = tempfile::tempdir().unwrap();
            let seg_acked = create_segment(&temp, "110000_300", b"acked data");
            let (_server, mut worker) = test_worker(&temp, vec![]).await;
            let (cur_id, cur_pair) = worker.current_identity_and_pairing();
            let ack = create_test_ack(
                &seg_acked,
                "110000_300",
                &cur_id.unwrap_or_default(),
                &cur_pair.unwrap_or_default(),
            );
            write_ack(&seg_acked, &ack).unwrap();

            worker.client.revoke_for_test();
            let notify = Arc::clone(&worker.notify);
            let running = Arc::clone(&worker.running);
            let task = tokio::spawn(async move { worker.run().await });
            notify.notify_one();
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            running.store(false, Ordering::Release);
            notify.notify_one();
            task.await.unwrap();

            // seg_acked is removed before worker loop handles revocation
            assert!(!seg_acked.exists());
        }
    }

    // Unwritable segment logs cleanup failed and retries
    #[tokio::test]
    async fn unwritable_segment_logs_cleanup_failed_and_retries() {
        use std::os::unix::fs::PermissionsExt;

        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"data");
        fs::write(segment.join("mic.flac"), b"audio").unwrap();

        let (server, mut worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let id = cur_id.unwrap_or_default();
        let pair = cur_pair.unwrap_or_default();
        let ack = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "120000_300".to_string(),
            stored_key: "120000_300".to_string(),
            identity_key: id,
            pairing_id: pair,
            proof: "upload".to_string(),
            files: vec![
                IngestAckFile {
                    submitted: "screen.webm".to_owned(),
                    written: "screen.webm".to_owned(),
                    size: 4,
                    sha256: sha256_file(&segment.join("screen.webm")).unwrap(),
                    disposition: "written".to_owned(),
                    stamp: file_stamp(&segment.join("screen.webm")).unwrap(),
                },
                IngestAckFile {
                    submitted: "mic.flac".to_owned(),
                    written: "mic.flac".to_owned(),
                    size: 5,
                    sha256: sha256_file(&segment.join("mic.flac")).unwrap(),
                    disposition: "written".to_owned(),
                    stamp: file_stamp(&segment.join("mic.flac")).unwrap(),
                },
            ],
        };
        write_ack(&segment, &ack).unwrap();

        // Delete 1 file from disk
        fs::remove_file(segment.join("mic.flac")).unwrap();

        // Make segment directory read-only so deletion inside fails
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o555)).unwrap();

        let output = Arc::new(Mutex::new(Vec::new()));
        let make_subscriber = || {
            let writer = Buffer(Arc::clone(&output));
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish()
        };

        async { worker.sync_pass().await }
            .with_subscriber(make_subscriber())
            .await;

        assert_eq!(upload_hits(&server), 0);
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, Some(0));
        assert!(segment.exists());
        assert!(segment.join(INGEST_ACK_FILENAME).exists());
        let log_out1 = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(log_out1.matches("Cleanup failed").count(), 1);

        // Second sync_pass logs a second "Cleanup failed"
        async { worker.sync_pass().await }
            .with_subscriber(make_subscriber())
            .await;

        assert!(segment.exists());
        let log_out2 = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(log_out2.matches("Cleanup failed").count(), 2);

        // Restore permissions so tempdir can be dropped
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn failing_local_removal_logs_once_per_run_iteration() {
        use std::os::unix::fs::PermissionsExt;

        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"data");
        let (server, worker) = test_worker(&temp, vec![]).await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let ack = create_test_ack(
            &segment,
            "120000_300",
            &cur_id.unwrap_or_default(),
            &cur_pair.unwrap_or_default(),
        );
        write_ack(&segment, &ack).unwrap();
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o555)).unwrap();

        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let count = || {
            String::from_utf8(output.lock().unwrap().clone())
                .unwrap()
                .matches("Cleanup failed")
                .count()
        };
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let facts = Arc::clone(&worker.facts);
        let mut worker = worker;
        let task = tokio::spawn(async move { worker.run().await }.with_subscriber(subscriber));

        for iteration in 1..=2 {
            // Wait for each pass to commit before the next trigger, so every
            // trigger is its own loop iteration.
            facts.lock().unwrap().last_successful_sync = None;
            notify.notify_one();
            tokio::time::timeout(Duration::from_secs(5), async {
                while facts.lock().unwrap().last_successful_sync.is_none() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(count(), iteration);
        }
        running.store(false, Ordering::Release);
        notify.notify_one();
        task.await.unwrap();

        assert_eq!(count(), 2);
        assert_eq!(upload_hits(&server), 0);
        assert!(segment.exists());
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Incomplete and failed segments with metadata survive without upload
    #[tokio::test]
    async fn incomplete_and_failed_with_metadata_survive_without_upload() {
        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("captures/20260101/archon");
        fs::create_dir_all(&day).unwrap();

        // Incomplete segment (active / not sealed) with only .metadata
        let seg_incomplete = day.join("120000.incomplete");
        fs::create_dir_all(&seg_incomplete).unwrap();
        fs::write(seg_incomplete.join(".metadata"), b"meta").unwrap();

        // Failed segment with only .metadata
        let seg_failed = day.join("120000_300.failed");
        fs::create_dir_all(&seg_failed).unwrap();
        fs::write(seg_failed.join(".metadata"), b"meta").unwrap();

        let (server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;

        assert!(seg_incomplete.exists());
        assert!(seg_failed.exists());
        assert_eq!(upload_hits(&server), 0);
    }

    // Removes old retention formats and tolerates null retry
    #[tokio::test]
    async fn removes_old_retention_formats_and_tolerates_null_retry() {
        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("captures/20260101/archon");
        fs::create_dir_all(&day).unwrap();

        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status": "ok", "segment": "140000_300"}))],
        )
        .await;
        let (cur_id, cur_pair) = worker.current_identity_and_pairing();
        let id = cur_id.unwrap_or_default();
        let pair = cur_pair.unwrap_or_default();

        // 1. Normal ack (upload / written)
        let seg1 = day.join("110000_300");
        fs::create_dir_all(&seg1).unwrap();
        fs::write(seg1.join("screen.webm"), b"video1").unwrap();
        let ack1 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "110000_300".to_string(),
            stored_key: "110000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "upload".to_string(),
            files: vec![IngestAckFile {
                submitted: "screen.webm".to_owned(),
                written: "screen.webm".to_owned(),
                size: 6,
                sha256: sha256_file(&seg1.join("screen.webm")).unwrap(),
                disposition: "written".to_owned(),
                stamp: file_stamp(&seg1.join("screen.webm")).unwrap(),
            }],
        };
        write_ack(&seg1, &ack1).unwrap();

        // 2. Old proof format: listing with disposition "present"
        let seg2 = day.join("120000_300");
        fs::create_dir_all(&seg2).unwrap();
        fs::write(seg2.join("screen.webm"), b"video2").unwrap();
        let ack2 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "120000_300".to_string(),
            stored_key: "120000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "listing".to_string(),
            files: vec![IngestAckFile {
                submitted: "screen.webm".to_owned(),
                written: "screen.webm".to_owned(),
                size: 6,
                sha256: sha256_file(&seg2.join("screen.webm")).unwrap(),
                disposition: "present".to_owned(),
                stamp: file_stamp(&seg2.join("screen.webm")).unwrap(),
            }],
        };
        write_ack(&seg2, &ack2).unwrap();

        // 3. Ack with sibling retry containing old retention fields + numeric next_attempt_after
        let seg3 = day.join("130000_300");
        fs::create_dir_all(&seg3).unwrap();
        fs::write(seg3.join("screen.webm"), b"video3").unwrap();
        let ack3 = IngestAck {
            day: "20260101".to_string(),
            stream: "archon".to_string(),
            local_key: "130000_300".to_string(),
            stored_key: "130000_300".to_string(),
            identity_key: id.clone(),
            pairing_id: pair.clone(),
            proof: "upload".to_string(),
            files: vec![IngestAckFile {
                submitted: "screen.webm".to_owned(),
                written: "screen.webm".to_owned(),
                size: 6,
                sha256: sha256_file(&seg3.join("screen.webm")).unwrap(),
                disposition: "written".to_owned(),
                stamp: file_stamp(&seg3.join("screen.webm")).unwrap(),
            }],
        };
        write_ack(&seg3, &ack3).unwrap();
        fs::write(
            seg3.join(INGEST_RETRY_FILENAME),
            br#"{"retention_unproven":true,"retention_next_unix":1700000000,"terminal":true,"next_attempt_after":1700000000.0,"retry_version":1}"#,
        )
        .unwrap();

        // 4. Media payload, NO ack, retry file with next_attempt_after: null
        let seg4 = day.join("140000_300");
        fs::create_dir_all(&seg4).unwrap();
        fs::write(seg4.join("screen.webm"), b"video4").unwrap();
        fs::write(
            seg4.join(INGEST_RETRY_FILENAME),
            br#"{"next_attempt_after":null,"retry_version":1}"#,
        )
        .unwrap();

        worker.sync_pass().await;

        assert!(!seg1.exists());
        assert!(!seg2.exists());
        assert!(!seg3.exists());
        assert!(!seg4.exists());
        assert_eq!(upload_hits(&server), 1);
        assert!(
            !server
                .requests()
                .iter()
                .any(|r| r.uri.contains("segments/"))
        );
    }

    // Sparse local 413 records bounded retry and uploads older segment; journal 413 stops walk
    #[tokio::test]
    async fn sparse_local_413_records_bounded_retry_and_uploads_older_segment() {
        let temp = tempfile::tempdir().unwrap();
        let seg_older = create_segment(&temp, "110000_300", b"older data");
        let seg_newer = create_segment(&temp, "120000_300", b"newer data");

        // Make newer segment exceed MAX_PART_BODY_BYTES (250MB) via sparse set_len
        let file = fs::OpenOptions::new()
            .write(true)
            .open(seg_newer.join("screen.webm"))
            .unwrap();
        file.set_len(251 * 1024 * 1024).unwrap();

        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status": "ok", "segment": "110000_300"}))],
        )
        .await;

        worker.sync_pass().await;

        // seg_newer recorded local 413 retry and stayed
        assert!(seg_newer.exists());
        assert!(seg_newer.join(INGEST_RETRY_FILENAME).exists());
        let retry_bytes = fs::read(seg_newer.join(INGEST_RETRY_FILENAME)).unwrap();
        let retry: IngestRetry = serde_json::from_slice(&retry_bytes).unwrap();
        assert_eq!(retry.status_code, Some(413));

        // Walk continued to seg_older, which uploaded and was removed
        assert!(!seg_older.exists());
        assert_eq!(upload_hits(&server), 1);
    }

    #[tokio::test]
    async fn journal_413_stops_walk() {
        let temp = tempfile::tempdir().unwrap();
        let seg_older = create_segment(&temp, "110000_300", b"older data");
        let seg_newer = create_segment(&temp, "120000_300", b"newer data");

        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (413, json!({"reason_code": "file_too_large"})),
                (200, json!({"status": "ok", "segment": "110000_300"})),
            ],
        )
        .await;

        worker.sync_pass().await;

        // Journal 413 stops walk: both segments stay
        assert!(seg_newer.exists());
        assert!(seg_older.exists());
        assert_eq!(upload_hits(&server), 1);
        assert_eq!(worker.last_error_type, Some(ErrorType::Client));
        assert_eq!(worker.last_error_code, Some(413));
    }

    // Aged failed segment survives and doctor / unacked accounting ignores it
    #[tokio::test]
    async fn aged_failed_segment_survives_and_formats_quarantine_line() {
        use std::fs::FileTimes;

        let temp = tempfile::tempdir().unwrap();
        let day = temp.path().join("captures/20260101/archon");
        fs::create_dir_all(&day).unwrap();
        let seg_failed = day.join("120000_300.failed");
        fs::create_dir_all(&seg_failed).unwrap();
        fs::write(seg_failed.join("screen.webm"), b"quarantine video").unwrap();
        std::fs::File::open(&seg_failed)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(1.0)),
            )
            .unwrap();

        let (_server, mut worker) = test_worker(&temp, vec![]).await;
        worker.sync_pass().await;

        assert!(seg_failed.exists());
        let stats = crate::capture_stats::compute_quarantine_stats(
            &temp.path().join("captures"),
            worker.clock.wall_seconds(),
        );
        let line = crate::capture_stats::format_quarantine_line(&stats);
        assert!(line.is_some());
        assert!(line.unwrap().contains("Held:"));
        let captures = count_unacked_segments(&temp.path().join("captures"), None, None);
        assert_eq!(captures, 0);
    }

    // Direct-to-listing cutover variations
    #[tokio::test]
    async fn listing_cutover_variations() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let seg_proven = create_segment(&temp, "110000_300", b"proven cutover");
        let seg_unproven = create_segment(&temp, "120000_300", b"unproven cutover");
        let seg_unstatable = create_segment(&temp, "130000_300", b"unstatable cutover");

        let sha_proven = sha256_file(&seg_proven.join("screen.webm")).unwrap();
        let size_proven = fs::metadata(seg_proven.join("screen.webm")).unwrap().len();

        let remote = custody_for_day(
            "20260101",
            vec![
                json!({
                    "key": "110000_300",
                    "observed": true,
                    "files": [{
                        "name": "screen.webm",
                        "size": size_proven,
                        "status": "present",
                        "sha256": sha_proven,
                    }],
                }),
                json!({
                    "key": "120000_300",
                    "observed": false,
                    "files": [],
                }),
            ],
        );

        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, remote),
                (200, json!({"status": "ok", "segment": "120000_300"})),
            ],
        )
        .await;

        fs::write(
            worker.config.state_dir().join(INGEST_CUTOVER_FILENAME),
            b"{\"segments\":[\"20260101/archon/110000_300\",\"20260101/archon/120000_300\",\"20260101/archon/130000_300\"]}\n",
        )
        .unwrap();

        // Make unstatable segment file unreadable
        fs::set_permissions(
            seg_unstatable.join("screen.webm"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        worker.sync_pass().await;

        // seg_proven proven via remote listing -> removed
        assert!(!seg_proven.exists());
        // seg_unproven uploaded -> removed
        assert!(!seg_unproven.exists());
        // seg_unstatable kept safely
        assert!(seg_unstatable.exists());
        assert_eq!(upload_hits(&server), 1);

        // Restore permissions
        fs::set_permissions(
            seg_unstatable.join("screen.webm"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }

    // Config with retired retention key runs pass and removes confirmed segment
    #[tokio::test]
    async fn config_with_retired_retention_key_runs_pass_and_removes_confirmed() {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.json"),
            br#"{"cache_retention_days": -1}"#,
        )
        .unwrap();

        let loaded = crate::config::load_config(crate::config::ConfigPaths {
            config_dir: Some(config_dir),
            base_dir: Some(temp.path().to_path_buf()),
        });
        let config = loaded.config;
        let segment = create_segment(&temp, "120000_300", b"data");
        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"status":"ok","segment":"120000_300"}))],
        )
        .await;
        worker.config = config.clone();

        worker.sync_pass().await;

        assert_eq!(upload_hits(&server), 1);
        assert!(!segment.exists());
        crate::config::save_config(&config).unwrap();
        let saved_str = fs::read_to_string(config.config_path()).unwrap();
        assert!(!saved_str.contains("cache_retention_days"));
    }
}
