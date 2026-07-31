// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Background segment reconciliation and sync-health persistence.
//! A later D-Bus lode hooks health-change emission beside `save_health`.

use chrono::{DateTime, Duration as ChronoDuration, Local};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, FileTimes},
    io::{self, Read},
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
    observer::{Clock, HealthBeacon},
    private_link::{LinkFactState, LinkFacts},
    segment::timestamp_parts,
    sync_health::{
        ErrorType, ProcessEpoch, SyncFacts, SyncHealth, derive_health, load_facts, save_facts,
    },
    upload::{ListingEntry, UploadClient},
};

pub const CIRCUIT_THRESHOLD_AUTH: u32 = 1;
pub const CIRCUIT_THRESHOLD_TRANSIENT: u32 = 5;
pub const CIRCUIT_COOLDOWN_INITIAL: f64 = 30.0;
pub const CIRCUIT_COOLDOWN_FACTOR: f64 = 2.0;
pub const CIRCUIT_COOLDOWN_MAX: f64 = 300.0;
pub const SYNCED_DAYS_MAX_AGE: i64 = 90;
pub const QUARANTINE_TTL_DAYS: i64 = 30;
pub const CONTACT_FLUSH_INTERVAL: f64 = 30.0;
pub const SERVER_KEY_FILENAME: &str = ".server_key";

struct LinkFactPersistence {
    state_dir: PathBuf,
    facts: Arc<Mutex<SyncFacts>>,
    last_persisted: Mutex<Option<LinkFactState>>,
    failures: Arc<AtomicUsize>,
}

impl LinkFactPersistence {
    fn persist(&self, link_facts: &LinkFacts) {
        // Lock order: persistence serialization, SyncFacts, then LinkFactState.
        let mut last_persisted = self.last_persisted.lock().unwrap();
        let mut facts = self.facts.lock().unwrap();
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
    recent_error_count: Arc<AtomicU8>,
    stale_threshold: f64,
    clock: Arc<dyn Clock + Send + Sync>,
    link_facts: LinkFacts,
    link_persistence_failures: Arc<AtomicUsize>,
    abort: tokio::task::AbortHandle,
    task: JoinHandle<()>,
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
        link_facts.install_sink(Arc::new(move |facts| persistence.persist(facts)));
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
            recent_error_count,
            stale_threshold: config.sync_stale_threshold as f64,
            clock,
            link_facts,
            link_persistence_failures,
            abort,
            task,
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
        self.running.store(false, Ordering::Release);
        self.notify.notify_one();
        // Never cancel the shared UploadClient here: the walker must finish before the
        // event sender is stopped, making walker-then-sender shutdown structural.
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

    pub fn health_beacon(&self) -> HealthBeacon {
        let facts = self.facts.lock().unwrap();
        HealthBeacon {
            last_successful_sync: facts.last_successful_sync.map(|value| value as i64),
            pending_queue_depth: facts
                .pending_confirmed
                .and_then(|value| u64::try_from(value).ok()),
            recent_error_count: self.recent_error_count.load(Ordering::Acquire).min(99),
            last_error_reason: facts.last_error_class.map(|class| {
                let class = error_name(class);
                facts
                    .last_error_code
                    .map_or_else(|| class.to_owned(), |code| format!("{class}:{code}"))
            }),
        }
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
    synced_days: HashSet<String>,
    consecutive_failures: u32,
    last_error_type: Option<ErrorType>,
    last_error_code: Option<i64>,
    circuit_open: bool,
    circuit_open_permanent: bool,
    circuit_open_since: f64,
    circuit_cooldown: f64,
    last_full_sync: f64,
    last_contact_flush: f64,
    registration_refused: bool,
    draining_shutdown: bool,
    last_recovery_generation: u64,
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
        let synced_days = load_synced_days(&config.state_dir());
        let last_recovery_generation = client.recovery_generation();
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
            synced_days,
            consecutive_failures: 0,
            last_error_type: None,
            last_error_code: None,
            circuit_open: false,
            circuit_open_permanent: false,
            circuit_open_since: 0.0,
            circuit_cooldown: CIRCUIT_COOLDOWN_INITIAL,
            last_full_sync: 0.0,
            last_contact_flush: 0.0,
            registration_refused: false,
            draining_shutdown: false,
            last_recovery_generation,
            #[cfg(test)]
            fail_next_pass: false,
        }
    }

    async fn run(&mut self) {
        self.prune_synced_days();
        loop {
            let _ = tokio::time::timeout(Duration::from_secs(60), self.notify.notified()).await;
            let completion_pending = self.pending_trigger.swap(false, Ordering::AcqRel);
            if !self.is_running() {
                // A completion can race shutdown. Give the final local segment one bounded
                // reconciliation pass before the walker exits; keyless observers stay idle.
                if completion_pending && self.client.is_registered() {
                    self.draining_shutdown = true;
                    let _ = self.execute_pass(false).await;
                    self.draining_shutdown = false;
                }
                break;
            }
            if !self.client.is_registered() {
                if !self.registration_refused {
                    tracing::warn!("Sync refused: observer is not registered");
                    self.registration_refused = true;
                }
                continue;
            }
            self.registration_refused = false;
            if self.circuit_open && !self.try_probe().await {
                continue;
            }
            let now = self.clock.wall_seconds();
            let force_full = now - self.last_full_sync > 86_400.0;
            if let Err(error) = self.execute_pass(force_full).await {
                tracing::error!(error, "Sync error");
                continue;
            }
            if force_full {
                self.last_full_sync = now;
            }
        }
    }

    async fn execute_pass(&mut self, force_full: bool) -> Result<(), &'static str> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_pass) {
            return Err("injected pass failure");
        }
        self.sync_pass(force_full).await;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn is_active(&self) -> bool {
        self.is_running() || self.draining_shutdown
    }

    async fn try_probe(&mut self) -> bool {
        if self.circuit_open_permanent {
            self.clear_progress();
            return false;
        }
        let generation = self.client.recovery_generation();
        let skip_cooldown = generation != self.last_recovery_generation;
        if skip_cooldown {
            self.last_recovery_generation = generation;
        }
        let elapsed = self.clock.monotonic_seconds() - self.circuit_open_since;
        if !skip_cooldown && elapsed < self.circuit_cooldown {
            self.set_progress(
                format!("{:.0}s until probe", self.circuit_cooldown - elapsed),
                false,
            );
            return false;
        }
        self.set_progress("probing journal...".to_owned(), true);
        // Named deviation: Python's datetime.now() is uninjected; day derivation uses the injected wall clock.
        let today = timestamp_parts(self.clock.wall_seconds()).0;
        let result = self.client.get_server_segments(&today).await;
        // Consume a recovery that completed inside this probe so it cannot skip a future breaker.
        self.last_recovery_generation = self.client.recovery_generation();
        if result.error_type.is_none() {
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
        } else {
            // Named deviation: Python's _record_failure (sync.py:496) resets the cooldown to
            // INITIAL whenever failures have reached the threshold. An open breaker always has
            // threshold failures, so Python's ladder never climbs past 60 seconds. That conflicts
            // with sync.py:14 and this lode's exponential-backoff AC. Capture the pre-probe
            // cooldown before recording the failure so the ladder can climb. Python's tests miss
            // this by setting _consecutive_failures = 0, which is unreachable for an open breaker.
            let previous_cooldown = self.circuit_cooldown;
            self.record_failure(result.error_type, result.status_code.map(i64::from));
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

    async fn sync_pass(&mut self, force_full: bool) {
        self.facts.lock().unwrap().link = self.client.link_fact_state();
        // Named deviation: Python's datetime.now() is uninjected; day derivation uses the injected wall clock.
        let today = timestamp_parts(self.clock.wall_seconds()).0;
        let segments_by_day = collect_segments(&self.config.captures_dir());
        let mut days: HashSet<String> = segments_by_day.keys().cloned().collect();
        days.insert(today.clone());
        let mut days: Vec<_> = days.into_iter().collect();
        days.sort_by(|a, b| b.cmp(a));
        self.set_progress("checking journal...".to_owned(), true);
        let mut pass_success = true;
        let mut pass_error_type = None;
        let mut pass_error_code = None;
        let mut legacy_logged = false;

        for day in days {
            if !self.is_active() || self.circuit_open {
                pass_success = false;
                break;
            }
            if day != today && self.synced_days.contains(&day) && !force_full {
                continue;
            }
            self.set_progress(format!("checking {day}..."), true);
            let query = self.client.get_server_segments(&day).await;
            let Some(items) = query.segments.as_ref() else {
                pass_success = false;
                pass_error_type = query.error_type;
                pass_error_code = query.status_code.map(i64::from);
                self.record_failure(query.error_type, pass_error_code);
                if self.circuit_open {
                    break;
                }
                continue;
            };
            if query.error_type.is_some() {
                pass_success = false;
                pass_error_type = query.error_type;
                pass_error_code = query.status_code.map(i64::from);
                self.record_failure(query.error_type, pass_error_code);
                continue;
            }
            self.record_contact(false);
            let indexed = index_entries(items);
            let key_set: HashSet<&str> = indexed.keys().map(String::as_str).collect();
            if query.legacy && !legacy_logged {
                tracing::warn!("Journal listing is pre-v2 bare array; cleanup will not delete");
                legacy_logged = true;
            }
            let mut any_needed_upload = false;
            for segment_dir in segments_by_day.get(&day).into_iter().flatten() {
                if !self.is_active() || self.circuit_open {
                    break;
                }
                let segment_key = segment_dir.file_name().unwrap().to_string_lossy();
                let held = if query.legacy {
                    key_set.contains(segment_key.as_ref())
                        || read_server_key(segment_dir)
                            .as_deref()
                            .is_some_and(|key| key_set.contains(key))
                } else if query.truncated {
                    false
                } else {
                    lookup_entry(&indexed, segment_dir)
                        .is_some_and(|entry| segment_proven_held(segment_dir, entry))
                };
                if held {
                    continue;
                }
                let files = eligible_files(segment_dir);
                if files.as_ref().is_ok_and(|files| {
                    !files.is_empty()
                        && files
                            .iter()
                            .all(|file| file.metadata().is_ok_and(|meta| meta.len() == 0))
                }) {
                    quarantine_segment(
                        self.clock.wall_seconds(),
                        segment_dir,
                        "all files zero-byte",
                    );
                    continue;
                }
                // Pinned by Python: even a successful upload delays the synced-day mark until a later pass.
                any_needed_upload = true;
                self.set_progress(format!("uploading {segment_key}"), true);
                if self.upload_segment(&day, segment_dir).await {
                    self.consecutive_failures = 0;
                    self.recent_error_count.store(0, Ordering::Release);
                    self.last_error_type = None;
                    self.last_error_code = None;
                } else {
                    pass_success = false;
                    pass_error_type = self.last_error_type;
                    // Health distinguishes a rejected 401 from revocation, so the POST status
                    // must reach both the breaker and persisted pass result.
                    pass_error_code = self.last_error_code;
                    if self.last_error_type == Some(ErrorType::Client) {
                        quarantine_segment(
                            self.clock.wall_seconds(),
                            segment_dir,
                            "server rejected (client error)",
                        );
                    }
                    self.record_failure(self.last_error_type, self.last_error_code);
                    if self.circuit_open {
                        break;
                    }
                }
            }
            if day != today && !any_needed_upload {
                self.synced_days.insert(day);
                self.save_synced_days();
            }
        }

        if pass_success && !self.circuit_open && self.is_active() {
            self.commit_pass_result(true, None, None);
        } else {
            let facts = self.facts.lock().unwrap().clone();
            self.commit_pass_result(
                false,
                pass_error_type.or(facts.last_error_class),
                pass_error_code.or(facts.last_error_code),
            );
        }
        // A final drain is a complete reconciliation pass. Running cleanup here preserves the
        // same successful-pass semantics; the outer shutdown timeout still bounds all work.
        if !self.circuit_open && self.is_active() {
            self.cleanup_synced_segments().await;
        }
        if self.is_active() {
            self.sweep_expired_quarantine();
        }
    }

    async fn upload_segment(&mut self, day: &str, segment_dir: &Path) -> bool {
        let files = match eligible_files(segment_dir) {
            Ok(files) => files,
            Err(error) => {
                tracing::warn!(%error, path = %segment_dir.display(), "Failed to enumerate segment files");
                self.last_error_type = Some(ErrorType::Transient);
                self.last_error_code = None;
                return false;
            }
        };
        if files.is_empty() {
            return true;
        }
        let key = segment_dir.file_name().unwrap().to_string_lossy();
        let result = self.client.upload_segment(day, &key, &files).await;
        if result.success {
            if let Some(stored_key) = result.stored_key.filter(|stored| stored != key.as_ref())
                && let Err(error) = write_server_key(segment_dir, &stored_key)
            {
                tracing::warn!(%error, "Failed to write server key marker");
            }
            self.record_contact(false);
            self.reset_failures();
            true
        } else {
            self.last_error_type = result.error_type;
            self.last_error_code = result.status_code.map(i64::from);
            if self.client.is_revoked() {
                // Retained deliberately: this latches revocation before the general failure
                // path runs, and removing that earlier guard is an unforced risk.
                self.circuit_open = true;
                self.circuit_open_permanent = true;
            }
            false
        }
    }

    async fn cleanup_synced_segments(&mut self) {
        let retention = self.config.cache_retention_days;
        if retention < 0 || !self.config.captures_dir().exists() {
            return;
        }
        // Named deviation: Python's datetime.now() is uninjected; day derivation uses the injected wall clock.
        let today = timestamp_parts(self.clock.wall_seconds()).0;
        // Named deviation: Python's datetime.now() is uninjected; day derivation uses the injected wall clock.
        let cutoff = local_day_minus_days(self.clock.wall_seconds(), retention);
        let Ok(day_entries) = sorted_dirs(&self.config.captures_dir()) else {
            return;
        };
        for day_dir in day_entries {
            if !self.is_active() {
                break;
            }
            let day = day_dir.file_name().unwrap().to_string_lossy().into_owned();
            if !self.synced_days.contains(&day) || (retention > 0 && day >= cutoff) || day == today
            {
                continue;
            }
            let query = self.client.get_server_segments(&day).await;
            if query.error_type.is_some() || query.segments.is_none() {
                self.record_failure(query.error_type, query.status_code.map(i64::from));
                continue;
            }
            self.record_contact(false);
            let proof_available = !query.legacy && !query.truncated;
            let indexed = query
                .segments
                .as_deref()
                .map(index_entries)
                .unwrap_or_default();
            if let Ok(streams) = sorted_dirs(&day_dir) {
                for stream in streams {
                    if let Ok(segments) = sorted_dirs(&stream) {
                        for segment in segments {
                            let name = segment.file_name().unwrap().to_string_lossy();
                            if name.ends_with(".incomplete") || name.ends_with(".failed") {
                                continue;
                            }
                            if proof_available
                                && lookup_entry(&indexed, &segment)
                                    .is_some_and(|entry| segment_proven_held(&segment, entry))
                                && let Err(error) = fs::remove_dir_all(&segment)
                            {
                                tracing::error!(%error, path = %segment.display(), "Cleanup failed");
                            }
                        }
                    }
                    remove_if_empty(&stream);
                }
            }
            remove_if_empty(&day_dir);
        }
    }

    fn sweep_expired_quarantine(&self) {
        let cutoff = self.clock.wall_seconds() - QUARANTINE_TTL_DAYS as f64 * 86_400.0;
        for segment in all_segment_dirs(&self.config.captures_dir()) {
            if !segment
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".failed"))
            {
                continue;
            }
            let mtime = segment
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs_f64());
            if let Some(mtime) = mtime.filter(|mtime| *mtime <= cutoff) {
                if let Err(error) = fs::remove_dir_all(&segment) {
                    tracing::error!(%error, path = %segment.display(), "Failed to drop quarantine");
                } else {
                    let age_days = ((self.clock.wall_seconds() - mtime) / 86_400.0)
                        .floor()
                        .max(0.0) as i64;
                    tracing::warn!(
                        path = %segment.display(),
                        age_days,
                        limit_days = QUARANTINE_TTL_DAYS,
                        "Dropping quarantined segment — unrecovered quarantined data discarded"
                    );
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
            // Decision 1: the client draws the 401-vs-revoked line before returning (403
            // latches revoked, 401 never does), so one predicate covers every failure path,
            // including segment POST failures whose caller need not interpret the status.
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
    ) {
        let mut facts = self.facts.lock().unwrap();
        facts.in_progress = false;
        facts.progress.clear();
        if success {
            let now = self.clock.wall_seconds();
            facts.last_successful_sync = Some(now);
            facts.last_successful_contact.get_or_insert(now);
            facts.last_error_class = None;
            facts.last_error_code = None;
            facts.pending_confirmed = Some(0);
            self.consecutive_failures = 0;
            self.recent_error_count.store(0, Ordering::Release);
            self.last_error_type = None;
            self.last_error_code = None;
        } else {
            facts.pending_confirmed = None;
            facts.last_error_class = error_type;
            facts.last_error_code = status_code;
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

    fn save_synced_days(&self) {
        if let Err(error) = save_synced_days(&self.config.state_dir(), &self.synced_days) {
            tracing::warn!(%error, "Failed to save synced days");
        }
    }

    fn prune_synced_days(&mut self) {
        // Named deviation: Python's datetime.now() is uninjected; day derivation uses the injected wall clock.
        let cutoff = local_day_minus_days(self.clock.wall_seconds(), SYNCED_DAYS_MAX_AGE);
        let before = self.synced_days.len();
        self.synced_days.retain(|day| day >= &cutoff);
        if self.synced_days.len() != before {
            self.save_synced_days();
        }
    }
}

fn error_name(error: ErrorType) -> &'static str {
    match error {
        ErrorType::Auth => "auth",
        ErrorType::Client => "client",
        ErrorType::Transient => "transient",
        ErrorType::Incompatible => "incompatible",
    }
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
    Ok(files)
}

fn sha256_file(path: &Path) -> io::Result<String> {
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

fn segment_proven_held(segment_dir: &Path, entry: &ListingEntry) -> bool {
    let Ok(files) = eligible_files(segment_dir) else {
        return false;
    };
    if files.is_empty() {
        return false;
    }
    files.iter().all(|local| {
        let Some(local_name) = local.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let Some(remote) = entry
            .files
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|remote| {
                remote
                    .submitted_name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .or(remote.name.as_deref())
                    == Some(local_name)
            })
        else {
            return false;
        };
        matches!(remote.status.as_deref(), Some("present" | "processed"))
            && sha256_file(local).ok().as_deref() == remote.sha256.as_deref()
    })
}

#[cfg(test)]
pub(crate) fn contract_sha256_file(path: &Path) -> io::Result<String> {
    sha256_file(path)
}

#[cfg(test)]
pub(crate) fn contract_segment_proven_held(segment_dir: &Path, entry: &ListingEntry) -> bool {
    segment_proven_held(segment_dir, entry)
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
    // Deliberately not recovery::mark_failed: its naming and return contract differ.
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(now.max(0.0));
    if let Err(error) = File::open(&failed)
        .and_then(|directory| directory.set_times(FileTimes::new().set_modified(stamp)))
    {
        tracing::warn!(%error, path = %failed.display(), "Failed to stamp quarantine time");
    }
    tracing::warn!(path = %failed.display(), reason, "Quarantined segment");
    true
}

fn local_day_minus_days(wall: f64, days: i64) -> String {
    // No 1:1 Python ancestor: proving epoch-vs-calendar behavior across DST requires
    // a timezone-database dependency or unsafe process-global TZ mutation.
    let seconds = wall.floor() as i64;
    let nanos = ((wall - wall.floor()) * 1e9) as u32;
    DateTime::from_timestamp(seconds, nanos)
        .map(|utc| utc.with_timezone(&Local).naive_local() - ChronoDuration::days(days))
        .map(|local| local.format("%Y%m%d").to_string())
        .unwrap_or_else(|| timestamp_parts(wall).0)
}

fn synced_days_path(state_dir: &Path) -> PathBuf {
    state_dir.join("synced_days.json")
}

fn load_synced_days(state_dir: &Path) -> HashSet<String> {
    fs::read_to_string(synced_days_path(state_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn save_synced_days(state_dir: &Path, days: &HashSet<String>) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let path = synced_days_path(state_dir);
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut days: Vec<_> = days.iter().collect();
    days.sort();
    let mut text = serde_json::to_string(&days).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&temp, text)?;
    fs::rename(temp, path)
}

#[cfg(test)]
pub(crate) async fn cleanup_synced_day_for_composition(
    config: Config,
    client: Arc<UploadClient>,
    clock: Arc<dyn Clock + Send + Sync>,
    day: &str,
) -> SyncFacts {
    save_synced_days(&config.state_dir(), &HashSet::from([day.to_owned()])).unwrap();
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
    worker.cleanup_synced_segments().await;
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

fn all_segment_dirs(root: &Path) -> Vec<PathBuf> {
    sorted_dirs(root)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|day| sorted_dirs(&day).unwrap_or_default())
        .flat_map(|stream| sorted_dirs(&stream).unwrap_or_default())
        .collect()
}

fn remove_if_empty(path: &Path) {
    if path.is_dir() && fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none()) {
        let _ = fs::remove_dir(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        private_link::{
            LinkFactState, ObserverState, publish_observer_registration, start_private_link_session,
        },
        private_link_test_peer::PrivateLinkPeer,
        sync_health::{HealthState, load_facts_with_liveness},
        test_support::{LinkedMockServer, MockServer, MutableClock, wait_for_requests},
        upload::ListingFile,
    };
    use serde_json::{Value, json};
    use tracing::instrument::WithSubscriber;

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
            }]),
        }
    }

    fn create_segment(temp: &tempfile::TempDir, name: &str, body: &[u8]) -> PathBuf {
        let segment = temp.path().join("captures/20260101/archon").join(name);
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), body).unwrap();
        segment
    }

    fn listing(key: &str, file_name: &str, status: Option<&str>, sha: &str) -> Value {
        let mut file = json!({"name": file_name, "sha256": sha});
        if let Some(status) = status {
            file["status"] = json!(status);
        }
        json!({"items":[{"key":key,"files":[file]}],"total":1})
    }

    async fn test_worker(
        temp: &tempfile::TempDir,
        responses: Vec<(u16, Value)>,
        retention: i64,
    ) -> (LinkedMockServer, SyncWorker) {
        let server = LinkedMockServer::new(responses).await;
        let config = Config {
            stream: "desktop".to_owned(),
            sync_retry_delays: vec![0],
            cache_retention_days: retention,
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(UploadClient::new(
            &config,
            server.capability(),
            "host",
            "linux",
            "test",
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

    async fn linked_worker(
        temp: &tempfile::TempDir,
        peer: &PrivateLinkPeer,
        clock: Arc<dyn Clock + Send + Sync>,
        retention: i64,
    ) -> (crate::private_link::PrivateLinkSession, SyncWorker) {
        let config = Config {
            stream: "desktop".to_owned(),
            sync_retry_delays: vec![0],
            cache_retention_days: retention,
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "desktop")
            .await
            .unwrap();
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "STALE-KEY-FULL".to_owned(),
                prefix: "prefix".to_owned(),
                name: "desktop".to_owned(),
                ingest_url: "/app/observer/ingest".to_owned(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability("/app/observer/ingest".to_owned()),
            "host",
            "linux",
            "test",
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
        (session, worker)
    }

    fn registration(key: &str, name: &str) -> Value {
        json!({
            "key": key,
            "name": name,
            "prefix": "prefix",
            "ingest_url": "/app/observer/ingest",
            "protocol_version": 2
        })
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
            .filter(|request| request.uri == "/app/observer/ingest")
            .count()
    }

    async fn upload_then_cleanup_keeps(remote: Value) {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, remote.clone()),
                (200, json!({"status":"ok","segment":"120000_300"})),
                (200, remote),
            ],
            7,
        )
        .await;
        worker.sync_pass(true).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert_eq!(upload_hits(&server), 1);
        assert!(segment.exists());
    }

    async fn held_then_cleanup_deletes(remote: Value) {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, remote.clone()),
                (200, remote),
            ],
            7,
        )
        .await;
        worker.sync_pass(true).await;
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
        assert!(!segment_proven_held(
            temp.path(),
            &entry("screen.webm", "present", "bad")
        ));
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
                },
                ListingFile {
                    submitted_name: None,
                    name: Some("audio.flac".to_owned()),
                    status: Some("present".to_owned()),
                    sha256: Some(sha256_file(&screen).unwrap()),
                },
            ]),
        };
        assert!(!segment_proven_held(temp.path(), &item));
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
            }]),
        };
        assert!(segment_proven_held(temp.path(), &item));
    }

    // tests/test_sync.py::test_present_status_with_mismatched_sha_uploads
    #[tokio::test]
    async fn present_status_with_mismatched_sha_uploads() {
        upload_then_cleanup_keeps(listing("120000_300", "screen.webm", Some("present"), "bad"))
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
        let remote = json!({"items":[{"key":"120000_300","files":[
            {"name":"screen.webm","status":"present","sha256":format!("{:x}",Sha256::digest(b"screen"))},
            {"name":"audio.flac","status":"processed","sha256":format!("{:x}",Sha256::digest(b"audio"))}
        ]}],"total":1});
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, remote.clone()),
                (200, remote),
            ],
            7,
        )
        .await;
        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 0);
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
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
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
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(segment.exists());
    }

    // AC: upload enumeration failures are transient and never send a partial request.
    #[tokio::test]
    async fn unreadable_segment_directory_upload_is_retried() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(eligible_files(&segment).is_err());
        let (server, mut worker) = test_worker(&temp, vec![], -1).await;
        assert!(!worker.upload_segment("20260101", &segment).await);
        fs::set_permissions(&segment, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(worker.last_error_type, Some(ErrorType::Transient));
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_truncated_envelope_uploads_and_cleanup_keeps
    #[tokio::test]
    async fn truncated_envelope_uploads_and_cleanup_keeps() {
        let sha = format!("{:x}", Sha256::digest(b"screen"));
        let remote = json!({"items":[{"key":"120000_300","files":[{"name":"screen.webm","status":"present","sha256":sha}]}],"total":2});
        upload_then_cleanup_keeps(remote).await;
    }

    // tests/test_sync.py::test_legacy_listing_logs_once_and_cleanup_keeps
    #[tokio::test]
    async fn legacy_listing_logs_once_and_cleanup_keeps() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let legacy = json!([{"key":"120000_300"}]);
        let (server, mut worker) = test_worker(
            &temp,
            vec![(200, json!([])), (200, legacy.clone()), (200, legacy)],
            7,
        )
        .await;
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        worker.sync_pass(true).with_subscriber(subscriber).await;
        assert_eq!(upload_hits(&server), 0);
        assert_eq!(server.request_count("/segments/20260101"), 2);
        assert!(segment.exists());
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("pre-v2 bare array").count(), 1);
    }

    // tests/test_sync.py::test_duplicate_marker_stops_reupload
    #[tokio::test]
    async fn duplicate_marker_stops_reupload() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let held = listing("existing_300", "screen.webm", Some("present"), &sha);
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (
                    200,
                    json!({"status":"duplicate","existing_segment":"existing_300"}),
                ),
                (200, json!({"items":[],"total":0})),
                (200, held),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert_eq!(
            fs::read_to_string(segment.join(SERVER_KEY_FILENAME)).unwrap(),
            "existing_300\n"
        );
        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 1);
        assert!(worker.synced_days.contains("20260101"));
    }

    // tests/test_sync.py::test_collision_marker_and_original_key_reconcile
    #[tokio::test]
    async fn collision_marker_and_original_key_reconcile() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let held = json!({"items":[{"key":"120000_301","original_key":"120000_300","files":[{"name":"screen.webm","status":"present","sha256":sha}]}],"total":1});
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"status":"ok","segment":"120000_301"})),
                (200, json!({"items":[],"total":0})),
                (200, held),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert_eq!(
            fs::read_to_string(segment.join(SERVER_KEY_FILENAME)).unwrap(),
            "120000_301\n"
        );
        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 1);
        assert!(worker.synced_days.contains("20260101"));
    }

    // tests/test_sync.py::test_zero_byte_segment_quarantined
    #[tokio::test]
    async fn zero_byte_segment_quarantined() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(!segment.exists());
        assert!(segment.with_file_name("120000_300.failed").exists());
    }

    // tests/test_sync.py::test_zero_byte_does_not_trigger_upload
    #[tokio::test]
    async fn zero_byte_does_not_trigger_upload() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
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
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 1);
        assert!(segment.exists());
        assert!(!segment.with_file_name("120000_300.failed").exists());
    }

    // tests/test_sync.py::test_zero_byte_day_marked_synced
    #[tokio::test]
    async fn zero_byte_day_marked_synced() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(worker.synced_days.contains("20260101"));
    }

    // tests/test_sync.py::test_client_error_quarantines_segment
    #[tokio::test]
    async fn client_error_quarantines_segment_and_walk_continues() {
        let temp = tempfile::tempdir().unwrap();
        let rejected = create_segment(&temp, "130000_300", b"bad");
        let accepted = create_segment(&temp, "120000_300", b"good");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (400, json!({})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(!rejected.exists());
        assert!(rejected.with_file_name("130000_300.failed").exists());
        assert!(accepted.exists());
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
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (400, json!({})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
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
        let mut responses = vec![
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
        ];
        responses.extend((0..10).map(|_| (500, json!({}))));
        let (_server, mut worker) = test_worker(&temp, responses, -1).await;
        worker.sync_pass(true).await;
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
        assert!(!segment_proven_held(
            temp.path(),
            &entry("screen.webm", "uploading", &sha256_file(&file).unwrap())
        ));
    }

    // AC: processed plus exact hash is terminal proof.
    #[test]
    fn processed_status_with_matching_sha_is_proof() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("screen.webm");
        fs::write(&file, b"screen").unwrap();
        assert!(segment_proven_held(
            temp.path(),
            &entry("screen.webm", "processed", &sha256_file(&file).unwrap())
        ));
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
        assert!(!segment_proven_held(
            temp.path(),
            &entry("screen.webm", "present", &sha)
        ));
    }

    // AC: zero eligible files are never proof.
    #[test]
    fn empty_segment_is_not_proven_held() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!segment_proven_held(
            temp.path(),
            &entry("x", "present", "x")
        ));
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

    // tests/test_sync.py::test_prunes_old_entries
    #[tokio::test]
    async fn prunes_old_entries_and_rewrites_sorted_file() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let server = MockServer::new(vec![]).await;
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            "host",
            "linux",
            "test",
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 0.0,
        });
        let facts = Arc::new(Mutex::new(SyncFacts::default()));
        let mut worker = SyncWorker::new(
            config.clone(),
            client,
            clock,
            SyncControl {
                notify: Arc::new(Notify::new()),
                pending_trigger: Arc::new(AtomicBool::new(false)),
                running: Arc::new(AtomicBool::new(true)),
            },
            facts,
            Arc::new(AtomicU8::new(0)),
        );
        let recent = local_day_minus_days(1_800_000_000.0, 1);
        let newer = local_day_minus_days(1_800_000_000.0, 0);
        worker.synced_days = HashSet::from(["20000101".to_owned(), newer.clone(), recent.clone()]);
        worker.prune_synced_days();
        assert_eq!(
            worker.synced_days,
            HashSet::from([recent.clone(), newer.clone()])
        );
        assert_eq!(
            fs::read_to_string(synced_days_path(&config.state_dir())).unwrap(),
            format!("[\"{recent}\",\"{newer}\"]\n")
        );
        drop(server);
    }

    // tests/test_sync.py::test_deletes_old_synced_confirmed
    // AC: cleanup performs a fresh listing after reconcile before deleting.
    #[tokio::test]
    async fn cleanup_fetches_old_day_twice_before_deleting() {
        let temp = tempfile::tempdir().unwrap();
        let segment = temp.path().join("captures/20260101/archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        let media = segment.join("screen.webm");
        fs::write(&media, b"screen").unwrap();
        let sha = sha256_file(&media).unwrap();
        let held = json!({
            "items": [{
                "key": "120000_300",
                "files": [{
                    "name": "screen.webm",
                    "status": "present",
                    "sha256": sha
                }]
            }],
            "total": 1
        });
        let server = MockServer::new(vec![
            (200, json!({"items":[],"total":0})),
            (200, held.clone()),
            (200, held),
        ])
        .await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        save_synced_days(&config.state_dir(), &HashSet::from(["20260101".to_owned()])).unwrap();
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
                mono: 100.0,
            }),
        );
        service.trigger();
        wait_for_requests(&server, 3).await;
        for _ in 0..100 {
            if !segment.exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert_eq!(server.request_count("/segments/20260101"), 2);
        assert!(!segment.exists());
    }

    // tests/test_sync_health_surfaces.py::test_health_facts_drive_all_surfaces_consistently
    // Named deviation: surface consumption belongs to the tray/CLI/D-Bus lodes.
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
                "on — connected",
                "Active",
                "connected",
                "Sync: connected — up to date (0 pending)",
                "ok",
            ),
            (
                SyncFacts {
                    last_error_class: Some(ErrorType::Incompatible),
                    last_error_code: Some(404),
                    ..SyncFacts::default()
                },
                "on — update required",
                "NeedsAttention",
                "update-required",
                "Sync: update required — update solstone-linux; pending unconfirmed",
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
            "host",
            "linux",
            "test",
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
            "Sync: connection unavailable — saving locally; restart sol; if this continues, pair this device again"
        );
        assert_eq!(sampled.doctor_severity, "fail");
        assert_eq!(
            sampled.doctor_detail,
            "sync health: connection unavailable; restart sol; if this continues, pair this device again"
        );
        assert_eq!(sampled.dbus, "transport-unavailable");
        assert_eq!(
            sampled.accessible_recording,
            "sol — on, connection unavailable, saving locally"
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
            "host",
            "linux",
            "test",
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
            "host",
            "linux",
            "test",
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
    // Named deviation: surface consumption belongs to the tray/CLI/D-Bus lodes.
    #[tokio::test]
    async fn listing_404_drives_update_needed_derived_surfaces() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(404, json!({}))], -1).await;
        worker.sync_pass(true).await;
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
        assert_eq!(health.header_recording, "on — update required");
        assert_eq!(health.sni_status, "NeedsAttention");
        assert_eq!(health.dbus, "update-required");
        assert_eq!(
            health.cli,
            "Sync: update required — update solstone-linux; pending unconfirmed"
        );
        assert_eq!(health.doctor_severity, "fail");
    }

    async fn sweep_worker(temp: &tempfile::TempDir, retention: i64, wall: f64) -> SyncWorker {
        let (_server, mut worker) = test_worker(temp, vec![], retention).await;
        worker.clock = Arc::new(FixedClock { wall, mono: 0.0 });
        worker
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

    // tests/test_sync.py::test_fresh_quarantine_survives_repeated_sweeps
    #[tokio::test]
    async fn fresh_quarantine_survives_repeated_sweeps() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300.failed", b"x");
        set_mtime(&segment, 10_000_000.0);
        let worker = sweep_worker(&temp, 7, 10_000_000.0).await;
        worker.sweep_expired_quarantine();
        worker.sweep_expired_quarantine();
        assert!(segment.exists());
    }

    // tests/test_sync.py::test_fresh_dir_mtime_beats_ancient_day_and_file_mtimes
    #[tokio::test]
    async fn fresh_dir_mtime_beats_ancient_day_and_file_mtimes() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300.failed", b"x");
        set_mtime(&segment.join("screen.webm"), 1.0);
        set_mtime(&segment, 10_000_000.0);
        sweep_worker(&temp, 7, 10_000_000.0)
            .await
            .sweep_expired_quarantine();
        assert!(segment.exists());
    }

    // tests/test_sync.py::test_aged_quarantine_deleted_without_server_query
    #[tokio::test]
    async fn aged_quarantine_deleted_without_server_query() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300.failed", b"x");
        set_mtime(&segment, 1.0);
        let (server, worker) = test_worker(&temp, vec![], 7).await;
        worker.sweep_expired_quarantine();
        assert!(!segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_aged_quarantine_deleted_with_retention_disabled
    #[tokio::test]
    async fn aged_quarantine_deleted_with_retention_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300.failed", b"x");
        set_mtime(&segment, 1.0);
        sweep_worker(&temp, -1, 10_000_000.0)
            .await
            .sweep_expired_quarantine();
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_aged_quarantine_deletes_both_name_shapes
    #[tokio::test]
    async fn aged_quarantine_deletes_both_name_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let duration = create_segment(&temp, "120000_300.failed", b"x");
        let bare = create_segment(&temp, "130000.failed", b"x");
        set_mtime(&duration, 1.0);
        set_mtime(&bare, 1.0);
        sweep_worker(&temp, 7, 10_000_000.0)
            .await
            .sweep_expired_quarantine();
        assert!(!duration.exists());
        assert!(!bare.exists());
    }

    // AC: listing-path 400 records failure and quarantines no segment.
    #[tokio::test]
    async fn listing_client_error_keeps_every_segment_unquarantined() {
        let temp = tempfile::tempdir().unwrap();
        let first = create_segment(&temp, "120000_300", b"one");
        let second = create_segment(&temp, "130000_300", b"two");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"items":[],"total":0})), (400, json!({}))],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(first.exists());
        assert!(second.exists());
        assert!(!first.with_file_name("120000_300.failed").exists());
        assert!(!second.with_file_name("130000_300.failed").exists());
        assert_eq!(
            worker.facts.lock().unwrap().last_error_class,
            Some(ErrorType::Client)
        );
    }

    // Legacy breaker criterion: a listing 401 opens a recoverable breaker and a later probe
    // resumes segment upload; this does not exercise stale-key repair.
    #[tokio::test]
    async fn listing_401_opens_breaker_and_later_probe_resumes_upload() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (401, json!({})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
            -1,
        )
        .await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();

        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 0);
        clock.set_mono(131.0);
        assert!(worker.try_probe().await);
        worker.sync_pass(true).await;
        assert!(upload_hits(&server) >= 1);
    }

    // AC 4/16: listing alone repairs a distinct key and skips the still-active breaker wait once.
    #[tokio::test]
    async fn recovery_generation_skips_breaker_wait_once_with_empty_event_queue() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("NEW-KEY", "desktop-new").to_string());
        peer.enqueue_response(200, json!({"items":[],"total":0}).to_string());
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        let (session, mut worker) = linked_worker(&temp, &peer, clock.clone(), -1).await;

        worker.sync_pass(true).await;
        assert!(worker.circuit_open);
        assert_eq!(worker.client.recovery_generation(), 2);
        assert!(clock.monotonic_seconds() < worker.circuit_open_since + worker.circuit_cooldown);
        let before = peer.requests().len();
        assert!(worker.try_probe().await);
        assert_eq!(peer.requests().len(), before + 1);
        assert_eq!(clock.monotonic_seconds(), 100.0);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 14: positive-control the sync-path record, then prove it contains prefixes but no full key.
    #[tokio::test]
    async fn sync_recovery_log_has_name_and_prefixes_without_full_keys() {
        const STALE: &str = "STALE-KEY-FULL";
        const NEW: &str = "NEW-KEY-FULL";
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration(NEW, "desktop-new").to_string());
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let (session, mut worker) = linked_worker(&temp, &peer, clock, -1).await;
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tokio::spawn(async move { worker.sync_pass(true).await }.with_subscriber(subscriber))
            .await
            .unwrap();
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(
            captured.contains("Journal identity repair completed"),
            "{captured}"
        );
        assert!(captured.contains("desktop-new"), "{captured}");
        assert!(captured.contains("STALE-KE"), "{captured}");
        assert!(captured.contains("NEW-KEY-"), "{captured}");
        assert!(!captured.contains(STALE), "{captured}");
        assert!(!captured.contains(NEW), "{captured}");
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 15: twenty sync-path rejections emit exactly one first-rejection warning.
    #[tokio::test]
    async fn first_rejection_warning_is_once_per_window() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("STALE-KEY-FULL", "desktop").to_string());
        for _ in 0..19 {
            peer.enqueue_response(401, Vec::new());
        }
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let (session, worker) = linked_worker(&temp, &peer, clock, -1).await;
        let client = Arc::clone(&worker.client);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tokio::spawn(
            async move {
                for _ in 0..20 {
                    let _ = client.get_server_segments("20260101").await;
                }
            }
            .with_subscriber(subscriber),
        )
        .await
        .unwrap();
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/observer/register")
                .count(),
            1
        );
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(
            captured
                .matches("Journal rejected the current key; attempting identity repair")
                .count(),
            1,
            "{captured}"
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // AC 15: one sync-path rejection emits exactly one first-rejection warning.
    #[tokio::test]
    async fn one_rejection_emits_one_first_rejection_warning() {
        let temp = tempfile::tempdir().unwrap();
        let peer = PrivateLinkPeer::start().await;
        peer.enqueue_response(401, Vec::new());
        peer.enqueue_response(200, registration("STALE-KEY-FULL", "desktop").to_string());
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let (session, worker) = linked_worker(&temp, &peer, clock, -1).await;
        let client = Arc::clone(&worker.client);
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Buffer(Arc::clone(&output));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tokio::spawn(
            async move {
                let _ = client.get_server_segments("20260101").await;
            }
            .with_subscriber(subscriber),
        )
        .await
        .unwrap();
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|request| request.path == "/app/observer/register")
                .count(),
            1
        );
        let captured = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(
            captured
                .matches("Journal rejected the current key; attempting identity repair")
                .count(),
            1,
            "{captured}"
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // Named deviation from the legacy breaker AC 2+5:
    // tests/test_sync.py::test_auth_opens_immediately parity: both statuses open immediately,
    // but only a revoked 403 is permanent.
    #[tokio::test]
    async fn auth_opens_immediately_but_only_403_is_permanent() {
        for status in [401, 403] {
            let temp = tempfile::tempdir().unwrap();
            let (_server, mut worker) = test_worker(&temp, vec![(status, json!({}))], -1).await;
            worker.sync_pass(true).await;
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

    // Legacy breaker criterion: an upload 401 opens a recoverable breaker and a later probe
    // permits the segment POST to be retried; this does not exercise successful stale-key repair.
    #[tokio::test]
    async fn upload_401_opens_recoverable_breaker_and_later_retries() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (401, json!({})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
            -1,
        )
        .await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();

        worker.sync_pass(true).await;
        assert_eq!(upload_hits(&server), 1);
        assert!(!worker.circuit_open_permanent);
        clock.set_mono(131.0);
        assert!(worker.try_probe().await);
        worker.sync_pass(true).await;
        assert!(upload_hits(&server) >= 2);
    }

    // AC 4: upload_403_latches_permanently pins both revocation latches, not request counts.
    #[tokio::test]
    async fn upload_403_latches_permanently() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (403, json!({})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(worker.circuit_open_permanent);
        assert!(worker.client.is_revoked());
    }

    // AC 8: upload_401_records_and_persists_status pins POST status through durable facts.
    #[tokio::test]
    async fn upload_401_records_and_persists_status() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"screen");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (401, json!({})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
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
        let (_server, mut worker) = test_worker(&temp, vec![], -1).await;
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
        let (_server, mut worker) = test_worker(&temp, vec![(404, json!({}))], -1).await;
        worker.sync_pass(true).await;
        assert!(worker.circuit_open);
        assert!(!worker.circuit_open_permanent);
        assert_eq!(worker.circuit_threshold(), 1);
    }

    async fn open_probe_worker(
        temp: &tempfile::TempDir,
        responses: Vec<(u16, Value)>,
    ) -> (LinkedMockServer, SyncWorker) {
        let (server, mut worker) = test_worker(temp, responses, -1).await;
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
            open_probe_worker(&temp, vec![(200, json!({"items":[],"total":0}))]).await;
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
            open_probe_worker(&temp, vec![(200, json!({"items":[],"total":0}))]).await;
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
        let mut responses = (0..5).map(|_| (500, json!({}))).collect::<Vec<_>>();
        responses.extend([
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
        ]);
        let (server, mut worker) = test_worker(&temp, responses, -1).await;
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 100.0));
        worker.clock = clock.clone();
        for _ in 0..5 {
            worker.sync_pass(true).await;
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
        assert_eq!(server.requests().len(), 7);
        assert_eq!(
            derive_health(&facts.lock().unwrap(), 1_800_000_000.0, 600.0).state,
            crate::sync_health::HealthState::Connected
        );
    }

    // AC 10: sustained_401_retries_stay_bounded drives only MutableClock and one wake per step.
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
            server.request_count("/app/observer/ingest/segments/") <= bound,
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
            "host",
            "linux",
            "test",
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
            "host",
            "linux",
            "test",
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
        let (_server, mut worker) = test_worker(
            &temp,
            vec![(200, json!({"items":[],"total":0})), (404, json!({}))],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        let facts = worker.facts.lock().unwrap().clone();
        assert_eq!(facts.last_error_class, Some(ErrorType::Incompatible));
        assert_eq!(facts.last_error_code, Some(404));
        assert_eq!(facts.pending_confirmed, None);
    }

    // tests/test_sync.py::test_failed_query_clears_prior_pending_zero
    #[tokio::test]
    async fn failed_query_clears_prior_pending_zero() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker) = test_worker(&temp, vec![(500, json!({}))], -1).await;
        worker.facts.lock().unwrap().pending_confirmed = Some(0);
        worker.sync_pass(true).await;
        assert_eq!(worker.facts.lock().unwrap().pending_confirmed, None);
    }

    // tests/test_sync.py::test_successful_cleanup_after_clean_pass_keeps_connected
    #[tokio::test]
    async fn successful_cleanup_after_clean_pass_keeps_connected() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let held = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, held.clone()),
                (200, held),
            ],
            7,
        )
        .await;
        worker.sync_pass(true).await;
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
        retention: i64,
        synced: bool,
    ) -> (LinkedMockServer, SyncWorker, PathBuf) {
        let segment = create_segment(temp, name, b"screen");
        let (server, mut worker) = test_worker(temp, responses, retention).await;
        if synced {
            worker.synced_days.insert("20260101".to_owned());
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
            vec![(200, json!({"items":[],"total":0}))],
            7,
            true,
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
            cleanup_worker_with_segment(&temp, "120000_300", vec![], 7, false).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_keeps_when_server_unreachable
    #[tokio::test]
    async fn keeps_when_server_unreachable() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300", vec![(500, json!({}))], 7, true).await;
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
            cache_retention_days: 7,
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host")
            .await
            .unwrap();
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".into(),
                name: "host".into(),
                ingest_url: "/app/observer/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability("/app/observer/ingest".into()),
            "host",
            "linux",
            "test",
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
        worker.synced_days.insert("20260101".into());
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
        let legacy = MockServer::new(vec![]).await;
        let peer = PrivateLinkPeer::start().await;
        let gate = Arc::new(Notify::new());
        peer.enqueue_gated_response(
            200,
            serde_json::to_vec(&json!({"items":[],"total":0})).unwrap(),
            gate.clone(),
        );
        let config = Config {
            stream: "host".into(),
            cache_retention_days: 7,
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let session = start_private_link_session(&config.config_dir, peer.credential(), "host")
            .await
            .unwrap();
        publish_observer_registration(
            &session,
            &ObserverState {
                credential_instance_id: peer.credential().instance_id,
                key: "K".to_owned(),
                prefix: "prefix".into(),
                name: "host".into(),
                ingest_url: "/app/observer/ingest".into(),
                protocol_version: 2,
            },
        )
        .unwrap();
        let clock = Arc::new(FixedClock {
            wall: 1_800_000_000.0,
            mono: 100.0,
        });
        let client = Arc::new(UploadClient::new(
            &config,
            session.capability("/app/observer/ingest".into()),
            "host",
            "linux",
            "test",
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
        worker.synced_days.insert("20260101".into());
        let mut cleanup = Box::pin(worker.cleanup_synced_segments());
        tokio::select! {
            () = &mut cleanup => panic!("slow linked response completed before release"),
            () = async {
                while peer.requests().is_empty() {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        assert!(segment.exists());
        gate.notify_one();
        cleanup.await;
        assert!(segment.exists());
        assert!(legacy.requests().is_empty());
        drop(worker);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    // tests/test_sync.py::test_never_touches_incomplete
    #[tokio::test]
    async fn never_touches_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000.incomplete", b"incomplete");
        let complete = create_segment(&temp, "140000_300", b"complete");
        let incomplete_sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let complete_sha = sha256_file(&complete.join("screen.webm")).unwrap();
        let remote = json!({"items":[
            {"key":"120000.incomplete","files":[{"name":"screen.webm","status":"present","sha256":incomplete_sha}]},
            {"key":"140000_300","files":[{"name":"screen.webm","status":"present","sha256":complete_sha}]}
        ],"total":2});
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(!complete.exists());
    }

    // tests/test_sync.py::test_retention_negative_one_keeps_forever
    #[tokio::test]
    async fn retention_negative_one_keeps_forever() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300", vec![], -1, true).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_retention_zero_deletes_immediately
    #[tokio::test]
    async fn retention_zero_deletes_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let held = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, held)], 0).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_never_cleans_today
    #[tokio::test]
    async fn never_cleans_today() {
        let temp = tempfile::tempdir().unwrap();
        let today = timestamp_parts(1_800_000_000.0).0;
        let segment = temp
            .path()
            .join("captures")
            .join(&today)
            .join("archon/120000_300");
        fs::create_dir_all(&segment).unwrap();
        fs::write(segment.join("screen.webm"), b"x").unwrap();
        let (server, mut worker) = test_worker(&temp, vec![], 0).await;
        worker.synced_days.insert(today);
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_cleans_empty_dirs
    #[tokio::test]
    async fn cleans_empty_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let day = segment.parent().unwrap().parent().unwrap().to_path_buf();
        let held = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, held)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(!day.exists());
    }

    // tests/test_sync.py::test_original_key_lookup
    #[tokio::test]
    async fn original_key_lookup_deletes() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let remote = json!({"items":[{"key":"renamed","original_key":"120000_300","files":[{"name":"screen.webm","status":"present","sha256":sha}]}],"total":1});
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(!segment.exists());
    }

    // tests/test_sync.py::test_failed_segments_kept_if_day_not_synced
    #[tokio::test]
    async fn failed_segments_kept_if_day_not_synced() {
        let temp = tempfile::tempdir().unwrap();
        let (server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300.failed", vec![], 7, false).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert!(server.requests().is_empty());
    }

    // tests/test_sync.py::test_failed_segments_kept_within_retention
    #[tokio::test]
    async fn failed_segments_kept_within_retention() {
        let temp = tempfile::tempdir().unwrap();
        let day = local_day_minus_days(1_800_000_000.0, 1);
        let segment = temp
            .path()
            .join("captures")
            .join(&day)
            .join("archon/120000_300.failed");
        fs::create_dir_all(&segment).unwrap();
        let (server, mut worker) = test_worker(&temp, vec![], 7).await;
        worker.synced_days.insert(day);
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
            vec![(200, json!({"items":[],"total":0}))],
            7,
            true,
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
            -1,
        )
        .await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        assert!(worker.upload_segment("20260101", &segment).await);
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
        let (_server, mut worker) =
            test_worker(&temp, vec![(200, json!({"items":[],"total":0}))], -1).await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        let result = worker.client.get_server_segments("20260101").await;
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
        let (_server, mut worker) = test_worker(&temp, vec![], -1).await;
        for _ in 0..4 {
            worker.record_failure(Some(ErrorType::Transient), None);
        }
        worker.commit_pass_result(true, None, None);
        assert_eq!(worker.consecutive_failures, 0);
        assert_eq!(worker.last_error_type, None);
    }

    // AC: legacy cleanup listings never authorize deletion.
    #[tokio::test]
    async fn cleanup_legacy_envelope_disables_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let legacy = json!([{"key":"120000_300","files":[{"name":"screen.webm","status":"present","sha256":sha}]}]);
        let (server, mut worker) = test_worker(&temp, vec![(200, legacy)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert_eq!(server.request_count("/segments/20260101"), 1);
    }

    // AC: truncated cleanup listings never authorize deletion.
    #[tokio::test]
    async fn cleanup_truncated_envelope_disables_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let segment = create_segment(&temp, "120000_300", b"screen");
        let sha = sha256_file(&segment.join("screen.webm")).unwrap();
        let remote = json!({"items":[{"key":"120000_300","files":[{"name":"screen.webm","status":"present","sha256":sha}]}],"total":2});
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: cleanup query failure skips the entire day.
    #[tokio::test]
    async fn cleanup_query_failure_skips_day() {
        let temp = tempfile::tempdir().unwrap();
        let (_server, mut worker, segment) =
            cleanup_worker_with_segment(&temp, "120000_300", vec![(500, json!({}))], 7, true).await;
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
        assert_eq!(
            worker.facts.lock().unwrap().last_error_class,
            Some(ErrorType::Transient)
        );
    }

    // AC: one proven segment is deleted while an unproven sibling survives.
    #[tokio::test]
    async fn cleanup_deletes_proven_sibling_only() {
        let temp = tempfile::tempdir().unwrap();
        let proven = create_segment(&temp, "120000_300", b"one");
        let unproven = create_segment(&temp, "130000_300", b"two");
        let remote = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&proven.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
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
        let remote = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, remote)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(segment.exists());
    }

    // AC: a day with an attempted upload is not marked synced in that pass.
    #[tokio::test]
    async fn pending_upload_day_is_not_marked_synced() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"x");
        let (_server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"status":"ok","segment":"120000_300"})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        assert!(!worker.synced_days.contains("20260101"));
    }

    // AC: day-name age, not directory mtime, controls positive retention.
    #[tokio::test]
    async fn retention_uses_day_name_not_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let old = create_segment(&temp, "120000_300", b"old");
        set_mtime(old.parent().unwrap().parent().unwrap(), 1_800_000_000.0);
        let held = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&old.join("screen.webm")).unwrap(),
        );
        let (_server, mut worker) = test_worker(&temp, vec![(200, held)], 7).await;
        worker.synced_days.insert("20260101".to_owned());
        worker.cleanup_synced_segments().await;
        assert!(!old.exists());
    }

    // AC: beacon fields clamp, truncate, and format error reasons.
    #[tokio::test]
    async fn health_beacon_converts_all_fields() {
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
            "host",
            "linux",
            "test",
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
        *service.facts.lock().unwrap() = SyncFacts {
            last_successful_sync: Some(100.9),
            pending_confirmed: Some(7),
            last_error_class: Some(ErrorType::Incompatible),
            last_error_code: Some(404),
            ..SyncFacts::default()
        };
        service.recent_error_count.store(120, Ordering::Release);
        let beacon = service.health_beacon();
        assert_eq!(beacon.last_successful_sync, Some(100));
        assert_eq!(beacon.pending_queue_depth, Some(7));
        assert_eq!(beacon.recent_error_count, 99);
        assert_eq!(
            beacon.last_error_reason.as_deref(),
            Some("incompatible:404")
        );
        service.facts.lock().unwrap().last_error_code = None;
        assert_eq!(
            service.health_beacon().last_error_reason.as_deref(),
            Some("incompatible")
        );
        service.facts.lock().unwrap().last_error_class = None;
        assert_eq!(service.health_beacon().last_error_reason, None);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: negative pending values persist but never enter the unsigned beacon.
    #[tokio::test]
    async fn negative_pending_round_trips_but_beacon_is_none() {
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
        let client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            "host",
            "linux",
            "test",
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let service = SyncService::start(
            config,
            client,
            Arc::new(FixedClock {
                wall: 1.0,
                mono: 0.0,
            }),
        );
        assert_eq!(service.health_beacon().pending_queue_depth, None);
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: a completion notification starts a pass.
    #[tokio::test]
    async fn completion_trigger_starts_pass() {
        let temp = tempfile::tempdir().unwrap();
        let server = MockServer::new(vec![(200, json!({"items":[],"total":0}))]).await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
        assert_eq!(server.requests().len(), 1);
    }

    // AC: the periodic timeout starts a pass without a completion trigger.
    #[tokio::test(start_paused = true)]
    async fn periodic_sixty_seconds_starts_pass() {
        let temp = tempfile::tempdir().unwrap();
        let server = MockServer::new(vec![(200, json!({"items":[],"total":0}))]).await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        wait_for_requests(&server, 1).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
    }

    // AC: full reconciliation repeats only after an injected wall day elapses.
    #[tokio::test]
    async fn daily_full_pass_rechecks_synced_day() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("captures/20260101/archon/120000_300")).unwrap();
        let responses = vec![
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
            (200, json!({"items":[],"total":0})),
        ];
        let server = MockServer::new(responses).await;
        let config = Config {
            cache_retention_days: -1,
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        save_synced_days(&config.state_dir(), &HashSet::from(["20260101".to_owned()])).unwrap();
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let clock = Arc::new(MutableClock::new(1_800_000_000.0, 0.0));
        let service = SyncService::start(config, client, clock.clone());
        service.trigger();
        wait_for_requests(&server, 2).await;
        service.trigger();
        wait_for_requests(&server, 3).await;
        assert_eq!(server.request_count("/segments/20260101"), 1);
        clock.set_wall(1_800_086_401.0);
        service.trigger();
        wait_for_requests(&server, 5).await;
        assert_eq!(server.request_count("/segments/20260101"), 2);
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
        let (server, mut worker) = test_worker(
            &temp,
            vec![
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
                (200, json!({"items":[],"total":0})),
            ],
            -1,
        )
        .await;
        worker.sync_pass(true).await;
        let uris: Vec<_> = server.requests().into_iter().map(|r| r.uri).collect();
        assert!(uris[1].ends_with("/20260101"));
        assert!(uris[2].ends_with("/20250101"));
    }

    // AC: triggers during an active request coalesce into one non-overlapping follow-up.
    #[tokio::test]
    async fn active_walk_trigger_coalesces_without_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let (server, gate) = MockServer::gated().await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
        let (server, _gate) = MockServer::gated().await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
        let (server, mut worker) =
            test_worker(&temp, vec![(200, json!({"items":[],"total":0}))], -1).await;
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
        let held = listing(
            "120000_300",
            "screen.webm",
            Some("present"),
            &sha256_file(&segment.join("screen.webm")).unwrap(),
        );
        let server = MockServer::new(vec![
            (200, json!({"items":[],"total":0})),
            (200, held.clone()),
            (500, json!({})),
            (200, json!({"items":[],"total":0})),
            (200, held),
        ])
        .await;
        let config = Config {
            base_dir: temp.path().to_path_buf(),
            config_dir: temp.path().join("config"),
            ..Config::default()
        };
        save_synced_days(&config.state_dir(), &HashSet::from(["20260101".to_owned()])).unwrap();
        let client = Arc::new(crate::upload::linked_fixture_client_for_test(
            &config,
            &server.url,
            "host",
            "linux",
            "test",
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
        wait_for_requests(&server, 3).await;
        service.trigger();
        wait_for_requests(&server, 5).await;
        service.shutdown(Duration::from_secs(1)).await.unwrap();
        assert!(server.requests().len() >= 5);
    }

    // AC: an unregistered worker logs refusal once, stays idle, and performs no HTTP.
    #[tokio::test]
    async fn unregistered_worker_refuses_once_without_http() {
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
        let (server, mut worker) = test_worker(&temp, vec![], -1).await;
        let config = worker.config.clone();
        worker.client = Arc::new(crate::upload::capability_less_client_for_test(
            &config,
            "host",
            "linux",
            "test",
            Arc::new(FixedClock {
                wall: 0.0,
                mono: 0.0,
            }),
        ));
        let notify = Arc::clone(&worker.notify);
        let running = Arc::clone(&worker.running);
        let facts = worker.facts.lock().unwrap().clone();
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
        assert_eq!(load_facts(&config.state_dir()), facts);
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches("Sync refused").count(), 1);
    }

    // AC: a completion racing shutdown drains one walker pass before join completes.
    #[tokio::test]
    async fn final_completion_trigger_drains_before_shutdown_returns() {
        let temp = tempfile::tempdir().unwrap();
        create_segment(&temp, "120000_300", b"final screen");
        let server = MockServer::new(vec![
            (200, json!({"items": [], "total": 0})),
            (200, json!({"items": [], "total": 0})),
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
            "host",
            "linux",
            "test",
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
            .find(|request| request.uri == "/app/observer/ingest")
            .expect("final segment upload");
        assert!(String::from_utf8_lossy(&upload.body).contains("final screen"));
        let facts = load_facts(&config.state_dir());
        assert_eq!(facts.pending_confirmed, Some(0));
        assert_eq!(facts.last_error_class, None);
    }
}
