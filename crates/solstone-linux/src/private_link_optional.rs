// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::private_link::{LinkFactState, PrivateLinkCapability};
use crate::private_link_access::execute_relay_access_sync;
use crate::private_link_metadata::{
    DeviceSnapshot, current_device_snapshot, execute_metadata_sync,
};

pub(crate) const DEFAULT_OPTIONAL_DEADLINE: Duration = Duration::from_secs(15);

pub(crate) struct JobSlot<T> {
    pub(crate) in_flight: bool,
    pub(crate) in_flight_val: Option<T>,
    pub(crate) pending: Option<T>,
}

impl<T> Default for JobSlot<T> {
    fn default() -> Self {
        Self {
            in_flight: false,
            in_flight_val: None,
            pending: None,
        }
    }
}

pub(crate) struct OptionalJobs {
    metadata_slot: Mutex<JobSlot<DeviceSnapshot>>,
    access_slot: Mutex<JobSlot<()>>,
    deadline: Duration,
    hostname_source: Arc<dyn Fn() -> io::Result<String> + Send + Sync>,
    self_caused_dial_generation: AtomicU64,
}

impl Default for OptionalJobs {
    fn default() -> Self {
        Self::new(DEFAULT_OPTIONAL_DEADLINE, Arc::new(crate::cli::hostname))
    }
}

impl OptionalJobs {
    pub(crate) fn new(
        deadline: Duration,
        hostname_source: Arc<dyn Fn() -> io::Result<String> + Send + Sync>,
    ) -> Self {
        Self {
            metadata_slot: Mutex::new(JobSlot::default()),
            access_slot: Mutex::new(JobSlot::default()),
            deadline,
            hostname_source,
            self_caused_dial_generation: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_deadline(
        deadline: Duration,
        hostname_source: Arc<dyn Fn() -> io::Result<String> + Send + Sync>,
    ) -> Self {
        Self::new(deadline, hostname_source)
    }

    pub(crate) fn trigger(
        self: &Arc<Self>,
        capability: &PrivateLinkCapability,
        state_dir: &Path,
        identity_key: &str,
        facts_snapshot: &LinkFactState,
    ) {
        // Retry any pending durable clear from previous not_configured failure
        let _ = capability.writer().retry_durable_clear_if_pending();

        // 1. Metadata slot trigger
        let current_snapshot = current_device_snapshot(&*self.hostname_source);
        let mut spawn_meta = false;
        {
            let mut meta = self.metadata_slot.lock().unwrap();
            if meta.in_flight {
                if meta.in_flight_val.as_ref() != Some(&current_snapshot) {
                    meta.pending = Some(current_snapshot);
                }
            } else {
                meta.in_flight = true;
                meta.in_flight_val = Some(current_snapshot);
                meta.pending = None;
                spawn_meta = true;
            }
        }

        if spawn_meta {
            let jobs = Arc::clone(self);
            let cap = capability.clone();
            let s_dir = state_dir.to_path_buf();
            let id_key = identity_key.to_string();
            tokio::spawn(async move {
                jobs.run_metadata_loop(cap, s_dir, id_key).await;
            });
        }

        // 2. Access slot trigger
        let self_caused = self.self_caused_dial_generation.load(Ordering::Acquire);
        if facts_snapshot.dial_generation != 0 && facts_snapshot.dial_generation == self_caused {
            // Skip own self-caused dial generation
            return;
        }

        let mut spawn_access = false;
        {
            let mut acc = self.access_slot.lock().unwrap();
            if !acc.in_flight {
                acc.in_flight = true;
                spawn_access = true;
            }
        }

        if spawn_access {
            let jobs = Arc::clone(self);
            let cap = capability.clone();
            tokio::spawn(async move {
                jobs.run_access_job(cap).await;
            });
        }
    }

    async fn run_metadata_loop(
        self: &Arc<Self>,
        capability: PrivateLinkCapability,
        state_dir: PathBuf,
        identity_key: String,
    ) {
        loop {
            let deadline = self.deadline;
            let snap_source = {
                let h = Arc::clone(&self.hostname_source);
                move || current_device_snapshot(&*h)
            };

            let _ = tokio::time::timeout(
                deadline,
                execute_metadata_sync(
                    &capability,
                    &state_dir,
                    &identity_key,
                    snap_source,
                    deadline,
                ),
            )
            .await;

            let mut meta = self.metadata_slot.lock().unwrap();
            if let Some(next_snap) = meta.pending.take() {
                meta.in_flight_val = Some(next_snap);
            } else {
                meta.in_flight = false;
                meta.in_flight_val = None;
                break;
            }
        }
    }

    async fn run_access_job(self: &Arc<Self>, capability: PrivateLinkCapability) {
        let deadline = self.deadline;
        let dial_gen_before = capability.facts().snapshot().dial_generation;
        let res =
            tokio::time::timeout(deadline, execute_relay_access_sync(&capability, deadline)).await;
        if matches!(res, Ok(Ok(()))) {
            let dial_gen_after = capability.facts().snapshot().dial_generation;
            if dial_gen_after != dial_gen_before {
                self.self_caused_dial_generation
                    .store(dial_gen_after, Ordering::Release);
            }
        }
        let mut acc = self.access_slot.lock().unwrap();
        acc.in_flight = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_slot_coalescing_logic() {
        let mut slot = JobSlot::<String>::default();
        assert!(!slot.in_flight);
        assert_eq!(slot.in_flight_val, None);
        assert_eq!(slot.pending, None);

        // First trigger sets in-flight
        slot.in_flight = true;
        slot.in_flight_val = Some("snap1".to_string());

        // Same trigger while in flight -> does not set pending
        if slot.in_flight_val.as_deref() != Some("snap1") {
            slot.pending = Some("snap1".to_string());
        }
        assert_eq!(slot.pending, None);

        // Changed trigger while in flight -> sets pending
        if slot.in_flight_val.as_deref() != Some("snap2") {
            slot.pending = Some("snap2".to_string());
        }
        assert_eq!(slot.pending.as_deref(), Some("snap2"));
    }

    #[tokio::test]
    async fn test_optional_jobs_self_caused_dial_suppression() {
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_millis(100),
            Arc::new(|| Ok("test-host".to_string())),
        ));

        // Set self-caused dial generation
        jobs.self_caused_dial_generation
            .store(42, Ordering::Release);

        let fact_state_same = LinkFactState {
            dial_generation: 42,
            ..LinkFactState::default()
        };

        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = crate::private_link::start_private_link_session(
            temp.path(),
            peer.credential(),
            "stream",
        )
        .await
        .unwrap();
        let capability = session.capability();

        // Trigger with dial generation 42 -> access slot should NOT be spawned
        jobs.trigger(&capability, temp.path(), "identity", &fact_state_same);

        // access slot should remain false (not spawned)
        {
            let acc = jobs.access_slot.lock().unwrap();
            assert!(!acc.in_flight);
        }

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_optional_deadline_releases_inflight_slot() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = crate::private_link::start_private_link_session(
            temp.path(),
            peer.credential(),
            "stream",
        )
        .await
        .unwrap();
        let capability = session.capability();

        // Use a short deadline (e.g. 50ms)
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_millis(50),
            Arc::new(|| Ok("test-host".to_string())),
        ));

        let fact_state = LinkFactState {
            carrier_proven: true,
            dial_generation: 1,
            ..LinkFactState::default()
        };

        // Don't enqueue any responses on peer -> requests will hang and hit 50ms deadline
        jobs.trigger(&capability, temp.path(), "ident-1", &fact_state);

        // Immediately after trigger, slots are in flight
        {
            assert!(jobs.metadata_slot.lock().unwrap().in_flight);
            assert!(jobs.access_slot.lock().unwrap().in_flight);
        }

        // Wait for deadline to expire (e.g. 150ms)
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Both in_flight flags are released after deadline
        {
            assert!(!jobs.metadata_slot.lock().unwrap().in_flight);
            assert!(!jobs.access_slot.lock().unwrap().in_flight);
        }

        // A new trigger can run
        jobs.trigger(&capability, temp.path(), "ident-1", &fact_state);
        {
            assert!(jobs.metadata_slot.lock().unwrap().in_flight);
            assert!(jobs.access_slot.lock().unwrap().in_flight);
        }

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
