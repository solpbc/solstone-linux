// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io;
use std::path::{Path, PathBuf};
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
    pub(crate) active_pairing_id: Option<String>,
}

impl<T> Default for JobSlot<T> {
    fn default() -> Self {
        Self {
            in_flight: false,
            in_flight_val: None,
            pending: None,
            active_pairing_id: None,
        }
    }
}

pub(crate) struct OptionalJobs {
    metadata_slot: Mutex<JobSlot<DeviceSnapshot>>,
    access_slot: Mutex<JobSlot<()>>,
    meta_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    access_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    deadline: Duration,
    hostname_source: Arc<dyn Fn() -> io::Result<String> + Send + Sync>,
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
            meta_task: Mutex::new(None),
            access_task: Mutex::new(None),
            deadline,
            hostname_source,
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
        if facts_snapshot.dial_generation != 0
            && capability.is_dial_generation_suppressed(facts_snapshot.dial_generation)
        {
            return;
        }

        let current_pairing_id = capability.writer().pairing_id().to_string();

        // 1. Metadata slot trigger
        let current_snapshot = current_device_snapshot(&*self.hostname_source);
        let mut spawn_meta = false;
        {
            let mut meta = self.metadata_slot.lock().unwrap();
            if meta.active_pairing_id.as_deref() != Some(&current_pairing_id) {
                if let Some(h) = self.meta_task.lock().unwrap().take() {
                    h.abort();
                }
                meta.active_pairing_id = Some(current_pairing_id.clone());
                meta.in_flight = false;
                meta.in_flight_val = None;
                meta.pending = None;
            }
            if meta.in_flight {
                if meta.in_flight_val.as_ref() != Some(&current_snapshot) {
                    meta.pending = Some(current_snapshot);
                } else {
                    meta.pending = None;
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
            let pair_id = current_pairing_id.clone();
            let handle = tokio::spawn(async move {
                jobs.run_metadata_loop(cap, s_dir, id_key, pair_id).await;
            });
            *self.meta_task.lock().unwrap() = Some(handle);
        }

        // 2. Access slot trigger
        let mut spawn_access = false;
        {
            let mut acc = self.access_slot.lock().unwrap();
            if acc.active_pairing_id.as_deref() != Some(&current_pairing_id) {
                if let Some(h) = self.access_task.lock().unwrap().take() {
                    h.abort();
                }
                acc.active_pairing_id = Some(current_pairing_id.clone());
                acc.in_flight = false;
                acc.pending = None;
            }
            if acc.in_flight {
                acc.pending = Some(());
            } else {
                acc.in_flight = true;
                acc.pending = None;
                spawn_access = true;
            }
        }

        if spawn_access {
            let jobs = Arc::clone(self);
            let cap = capability.clone();
            let pair_id = current_pairing_id;
            let handle = tokio::spawn(async move {
                jobs.run_access_job(cap, pair_id).await;
            });
            *self.access_task.lock().unwrap() = Some(handle);
        }
    }

    pub(crate) fn shutdown(&self) {
        let mut meta = self.metadata_slot.lock().unwrap();
        meta.in_flight = false;
        meta.in_flight_val = None;
        meta.pending = None;
        if let Some(h) = self.meta_task.lock().unwrap().take() {
            h.abort();
        }
        let mut acc = self.access_slot.lock().unwrap();
        acc.in_flight = false;
        acc.pending = None;
        if let Some(h) = self.access_task.lock().unwrap().take() {
            h.abort();
        }
    }

    async fn run_metadata_loop(
        self: &Arc<Self>,
        capability: PrivateLinkCapability,
        state_dir: PathBuf,
        identity_key: String,
        pairing_id: String,
    ) {
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

        let follow_up = {
            let mut meta = self.metadata_slot.lock().unwrap();
            if meta.active_pairing_id.as_deref() != Some(&pairing_id) {
                meta.in_flight = false;
                meta.in_flight_val = None;
                meta.pending = None;
                return;
            }
            if let Some(next_snap) = meta.pending.take() {
                meta.in_flight_val = Some(next_snap);
                true
            } else {
                meta.in_flight = false;
                meta.in_flight_val = None;
                false
            }
        };

        if follow_up {
            let snap_source2 = {
                let h = Arc::clone(&self.hostname_source);
                move || current_device_snapshot(&*h)
            };
            let _ = tokio::time::timeout(
                deadline,
                execute_metadata_sync(
                    &capability,
                    &state_dir,
                    &identity_key,
                    snap_source2,
                    deadline,
                ),
            )
            .await;

            let mut meta = self.metadata_slot.lock().unwrap();
            if meta.active_pairing_id.as_deref() == Some(&pairing_id) {
                meta.in_flight = false;
                meta.in_flight_val = None;
            } else {
                meta.in_flight = false;
                meta.in_flight_val = None;
                meta.pending = None;
            }
        }
    }

    async fn run_access_job(
        self: &Arc<Self>,
        capability: PrivateLinkCapability,
        pairing_id: String,
    ) {
        let deadline = self.deadline;
        let _ = capability.retry_durable_reconciliation_if_pending();
        let _ =
            tokio::time::timeout(deadline, execute_relay_access_sync(&capability, deadline)).await;

        let follow_up = {
            let mut acc = self.access_slot.lock().unwrap();
            if acc.active_pairing_id.as_deref() != Some(&pairing_id) {
                acc.in_flight = false;
                acc.pending = None;
                return;
            }
            if acc.pending.take().is_some() {
                true
            } else {
                acc.in_flight = false;
                false
            }
        };

        if follow_up {
            let _ = capability.retry_durable_reconciliation_if_pending();
            let _ =
                tokio::time::timeout(deadline, execute_relay_access_sync(&capability, deadline))
                    .await;

            let mut acc = self.access_slot.lock().unwrap();
            if acc.active_pairing_id.as_deref() == Some(&pairing_id) {
                acc.in_flight = false;
            } else {
                acc.in_flight = false;
                acc.pending = None;
            }
        }
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

        // Clear relay transport sets suppressed dial generation to generation + 1 (= 1)
        capability.clear_relay_transport();
        assert!(capability.is_dial_generation_suppressed(1));

        let fact_state_suppressed = LinkFactState {
            dial_generation: 1,
            ..LinkFactState::default()
        };

        // Trigger with dial generation 1 -> both slots should be skipped
        jobs.trigger(&capability, temp.path(), "identity", &fact_state_suppressed);

        // slots should remain not spawned
        {
            let meta = jobs.metadata_slot.lock().unwrap();
            let acc = jobs.access_slot.lock().unwrap();
            assert!(!meta.in_flight);
            assert!(!acc.in_flight);
        }

        // A different generation (e.g. 2) is not suppressed
        let fact_state_other = LinkFactState {
            dial_generation: 2,
            ..LinkFactState::default()
        };
        jobs.trigger(&capability, temp.path(), "identity", &fact_state_other);
        {
            let meta = jobs.metadata_slot.lock().unwrap();
            let acc = jobs.access_slot.lock().unwrap();
            assert!(meta.in_flight);
            assert!(acc.in_flight);
        }

        jobs.shutdown();
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
