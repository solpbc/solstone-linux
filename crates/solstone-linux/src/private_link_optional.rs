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

#[derive(Default)]
struct Lane {
    busy: bool,
    pending: bool,
    passes: u8,
    task: Option<tokio::task::JoinHandle<()>>,
}
#[derive(Default)]
struct State {
    closed: bool,
    owner: u64,
    capability: Option<PrivateLinkCapability>,
    last_trigger: Option<(u64, DeviceSnapshot)>,
    lanes: [Lane; 2],
}
pub(crate) struct OptionalJobs {
    state: Mutex<State>,
    runtime: tokio::runtime::Handle,
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
            state: Mutex::new(State::default()),
            runtime: tokio::runtime::Handle::current(),
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
        facts: &LinkFactState,
    ) {
        if facts.optional_dial {
            return;
        }
        let snapshot = current_device_snapshot(&*self.hostname_source);
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return;
        }
        let same_owner = state
            .capability
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(&old.writer(), &capability.writer()));
        if !same_owner {
            if let Some(old) = state.capability.take() {
                old.writer().invalidate_optional_attempts();
            }
            for lane in &mut state.lanes {
                if let Some(task) = lane.task.take() {
                    task.abort();
                }
                *lane = Lane::default();
            }
            state.owner += 1;
            state.capability = Some(capability.clone());
            state.last_trigger = None;
        }
        let trigger = (facts.dial_generation, snapshot);
        if state.last_trigger.as_ref() == Some(&trigger) {
            return;
        }
        state.last_trigger = Some(trigger);
        if state.lanes.iter().all(|lane| !lane.busy) {
            for lane in &mut state.lanes {
                lane.passes = 0;
            }
        }
        let owner = state.owner;
        for index in 0..2 {
            let lane = &mut state.lanes[index];
            if lane.busy || lane.passes == 2 {
                lane.pending = true;
                continue;
            }
            lane.busy = true;
            lane.pending = false;
            lane.passes += 1;
            let jobs = Arc::clone(self);
            let cap = capability.clone();
            let path = state_dir.to_path_buf();
            let identity = identity_key.to_owned();
            // Install under the same lock used by cancellation and completion.
            lane.task = Some(self.runtime.spawn(async move {
                jobs.run_lane(index, owner, cap, path, identity).await;
            }));
        }
    }
    pub(crate) fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.owner += 1;
        if let Some(cap) = state.capability.take() {
            cap.writer().invalidate_optional_attempts();
        }
        for lane in &mut state.lanes {
            if let Some(task) = lane.task.take() {
                task.abort();
            }
            *lane = Lane::default();
        }
    }
    async fn run_lane(
        self: &Arc<Self>,
        index: usize,
        owner: u64,
        capability: PrivateLinkCapability,
        state_dir: PathBuf,
        identity: String,
    ) {
        loop {
            if index == 0 {
                let source = Arc::clone(&self.hostname_source);
                let _ = execute_metadata_sync(
                    &capability,
                    &state_dir,
                    &identity,
                    move || current_device_snapshot(&*source),
                    self.deadline,
                )
                .await;
            } else {
                let _ = execute_relay_access_sync(&capability, self.deadline).await;
            }
            let mut state = self.state.lock().unwrap();
            if state.closed || state.owner != owner {
                return;
            }
            let lane = &mut state.lanes[index];
            if lane.pending && lane.passes < 2 {
                lane.pending = false;
                lane.passes += 1;
            } else {
                lane.busy = false;
                lane.task = None;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_link::start_private_link_session;
    use crate::private_link_test_peer::PrivateLinkPeer;

    #[tokio::test]
    async fn deadline_releases_jobs_and_shutdown_refuses_late_facts() {
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let cap = session.capability();
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_millis(50),
            Arc::new(|| Ok("test".into())),
        ));
        let mut facts = LinkFactState {
            carrier_proven: true,
            dial_generation: 1,
            ..LinkFactState::default()
        };
        jobs.trigger(&cap, temp.path(), "identity", &facts);
        assert!(
            jobs.state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .all(|lane| lane.busy)
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while jobs
                .state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!peer.requests().is_empty());
        facts.dial_generation += 1;
        jobs.trigger(&cap, temp.path(), "identity", &facts);
        assert!(
            jobs.state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .all(|lane| lane.busy)
        );
        jobs.shutdown();
        facts.dial_generation += 1;
        jobs.trigger(&cap, temp.path(), "identity", &facts);
        assert!(
            jobs.state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .all(|lane| !lane.busy)
        );
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn healthy_optional_dials_settle_and_real_external_dial_restarts() {
        let peer = PrivateLinkPeer::start().await;
        peer.set_route(
            "/app/network/api/clients/self",
            200,
            serde_json::to_vec(&serde_json::json!({
                "protocol_version":1,"revision":1,"reported":null,"display_label":"test",
                "owner_label":null,"updated_at":null,"journal":{"name":null,"version":"1.0"}
            }))
            .unwrap(),
        );
        peer.set_route(
            "/app/network/api/relay/access",
            200,
            br#"{"protocol_version":2,"status":"not_configured"}"#.to_vec(),
        );
        peer.set_route("/app/devices/ingest/manifest", 200, b"{}".to_vec());
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let cap = session.capability();
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_secs(1),
            Arc::new(|| Ok("test".into())),
        ));
        let sink_jobs = Arc::clone(&jobs);
        let sink_cap = cap.clone();
        let path = temp.path().to_path_buf();
        cap.facts().install_sink(Arc::new(move |facts| {
            let snapshot = facts.snapshot();
            if snapshot.carrier_proven {
                sink_jobs.trigger(&sink_cap, &path, "identity", &snapshot);
            }
        }));
        jobs.trigger(&cap, temp.path(), "identity", &LinkFactState::default());
        tokio::time::timeout(Duration::from_secs(3), async {
            while jobs
                .state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(cap.facts().snapshot().optional_dial);
        assert_eq!(peer.requests().len(), 3);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(peer.requests().len(), 3);
        let response = session
            .request(reqwest::Method::POST, "/app/devices/ingest/manifest")
            .unwrap()
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(3), async {
            while jobs
                .state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!cap.facts().snapshot().optional_dial);
        assert_eq!(peer.requests().len(), 7);
        assert_eq!(peer.accepted_carriers(), 2);
        jobs.shutdown();
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn retired_pair_cannot_clear_new_pair_job_slots() {
        let old_peer = PrivateLinkPeer::start().await;
        let new_peer = PrivateLinkPeer::start().await;
        let old_path = tempfile::tempdir().unwrap();
        let new_path = tempfile::tempdir().unwrap();
        let old_session =
            start_private_link_session(old_path.path(), old_peer.credential(), "stream")
                .await
                .unwrap();
        let new_session =
            start_private_link_session(new_path.path(), new_peer.credential(), "stream")
                .await
                .unwrap();
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_secs(2),
            Arc::new(|| Ok("test".into())),
        ));
        let old_gate = Arc::new(tokio::sync::Notify::new());
        let new_gate = Arc::new(tokio::sync::Notify::new());
        old_peer.enqueue_gated_response(200, b"{}".to_vec(), Arc::clone(&old_gate));
        new_peer.enqueue_gated_response(200, b"{}".to_vec(), Arc::clone(&new_gate));
        let facts = LinkFactState::default();
        jobs.trigger(&old_session.capability(), old_path.path(), "old", &facts);
        tokio::time::timeout(Duration::from_secs(2), async {
            while old_peer.requests().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        jobs.trigger(&new_session.capability(), new_path.path(), "new", &facts);
        tokio::time::timeout(Duration::from_secs(2), async {
            while new_peer.requests().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let owner = jobs.state.lock().unwrap().owner;
        old_gate.notify_one();
        old_session.shutdown().await.unwrap();
        assert_eq!(jobs.state.lock().unwrap().owner, owner);
        assert!(
            jobs.state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
        );
        new_gate.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            while jobs
                .state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        jobs.shutdown();
        new_session.shutdown().await.unwrap();
        old_peer.shutdown().await;
        new_peer.shutdown().await;
    }
    #[tokio::test]
    async fn changed_snapshot_during_get_is_put_and_coalesced_once() {
        let peer = PrivateLinkPeer::start().await;
        peer.set_route(
            "/app/network/api/relay/access",
            200,
            br#"{"protocol_version":2,"status":"not_configured"}"#.to_vec(),
        );
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let host = Arc::new(Mutex::new("A".to_owned()));
        let source = Arc::clone(&host);
        let jobs = Arc::new(OptionalJobs::with_deadline(
            Duration::from_secs(3),
            Arc::new(move || Ok(source.lock().unwrap().clone())),
        ));
        let gate = Arc::new(tokio::sync::Notify::new());
        let mut resource = serde_json::json!({"protocol_version":1,"revision":1,"reported":null,"display_label":"test","owner_label":null,"updated_at":null,"journal":{"name":null,"version":"1.0"}});
        peer.enqueue_gated_response(
            200,
            serde_json::to_vec(&resource).unwrap(),
            Arc::clone(&gate),
        );
        resource["reported"] =
            serde_json::to_value(crate::private_link_metadata::ClientSelfReported::from(
                &current_device_snapshot(|| Ok("B".into())),
            ))
            .unwrap();
        peer.enqueue_response(200, serde_json::to_vec(&resource).unwrap());
        peer.enqueue_response(200, serde_json::to_vec(&resource).unwrap());
        let facts = LinkFactState::default();
        jobs.trigger(&session.capability(), temp.path(), "identity", &facts);
        tokio::time::timeout(Duration::from_secs(3), async {
            while !peer
                .requests()
                .iter()
                .any(|r| r.path.ends_with("/clients/self"))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        *host.lock().unwrap() = "B".into();
        jobs.trigger(&session.capability(), temp.path(), "identity", &facts);
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(4), async {
            while jobs
                .state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .any(|lane| lane.busy)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let metadata: Vec<_> = peer
            .requests()
            .into_iter()
            .filter(|r| r.path.ends_with("/clients/self"))
            .collect();
        assert_eq!(metadata.len(), 3);
        assert_eq!(metadata[1].method, "PUT");
        let put: serde_json::Value = serde_json::from_slice(&metadata[1].body).unwrap();
        assert_eq!(put["reported"]["name"], "B");
        assert!(
            jobs.state
                .lock()
                .unwrap()
                .lanes
                .iter()
                .all(|lane| lane.passes == 2)
        );
        jobs.shutdown();
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
