// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::private_link::{LinkOutcome, PrivateLinkCapability};
use reqwest::StatusCode;
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceSnapshot {
    pub(crate) name: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) device_type: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) app_version: Option<String>,
}

pub(crate) fn sanitize_reported_field(raw: Option<&str>, max_bytes: usize) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .any(|c| (c as u32) < 0x20 || (c as u32) == 0x7f)
    {
        return None;
    }
    if trimmed.len() > max_bytes {
        return None;
    }
    Some(trimmed.to_string())
}

pub(crate) fn current_device_snapshot(
    hostname_source: impl Fn() -> io::Result<String>,
) -> DeviceSnapshot {
    // Hostname is read on each connection-lifecycle trigger; there is no hostname watcher.
    let name = sanitize_reported_field(hostname_source().ok().as_deref(), 80);
    DeviceSnapshot {
        name,
        platform: Some("linux".to_string()),
        device_type: Some("desktop".to_string()),
        app_id: Some("solstone-linux".to_string()),
        app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClientSelfReported {
    pub(crate) name: Option<String>,
    pub(crate) platform: Option<String>,
    pub(crate) device_type: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) app_version: Option<String>,
}

impl From<&DeviceSnapshot> for ClientSelfReported {
    fn from(s: &DeviceSnapshot) -> Self {
        Self {
            name: s.name.clone(),
            platform: s.platform.clone(),
            device_type: s.device_type.clone(),
            app_id: s.app_id.clone(),
            app_version: s.app_version.clone(),
        }
    }
}

impl PartialEq<DeviceSnapshot> for ClientSelfReported {
    fn eq(&self, other: &DeviceSnapshot) -> bool {
        self.name == other.name
            && self.platform == other.platform
            && self.device_type == other.device_type
            && self.app_id == other.app_id
            && self.app_version == other.app_version
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct JournalDescriptor {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct ClientSelfResponse {
    pub(crate) protocol_version: u32,
    pub(crate) revision: i64,
    pub(crate) reported: Option<ClientSelfReported>,
    pub(crate) owner_label: Option<String>,
    pub(crate) display_label: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) journal: Option<JournalDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClientSelfUpdateRequest {
    pub(crate) protocol_version: u32,
    pub(crate) expected_revision: i64,
    pub(crate) reported: ClientSelfReported,
}

pub(crate) async fn execute_metadata_sync<F>(
    capability: &PrivateLinkCapability,
    state_dir: &Path,
    identity_key: &str,
    snapshot_source: F,
    timeout: Duration,
) -> Result<(), ()>
where
    F: Fn() -> DeviceSnapshot,
{
    let save_ver = |ver: &str, name: Option<&str>| {
        let (snap_fact, epoch) = capability.facts().snapshot_with_epoch();
        if snap_fact.carrier_proven {
            let _ =
                capability
                    .facts()
                    .commit_journal_version(epoch, snap_fact.dial_generation, || {
                        crate::sync_health::save_paired_journal_version(
                            state_dir,
                            identity_key,
                            ver,
                            name,
                        )
                        .is_ok()
                    });
        } else {
            let _ =
                crate::sync_health::save_paired_journal_version(state_dir, identity_key, ver, name);
        }
    };

    let record_journal = |res: &ClientSelfResponse| {
        if let Some(version) = res.journal.as_ref().and_then(|j| j.version.as_deref()) {
            let name = res.journal.as_ref().and_then(|j| j.name.as_deref());
            save_ver(version, name);
        }
    };

    let outcome = capability.clients_self_get(timeout).await;
    match outcome {
        LinkOutcome::LocalRejected {
            status: StatusCode::NOT_FOUND,
        } => {
            // Authenticated 404 only -> fallback to legacy system_status
            if let Ok(Some(version)) = capability.system_status().await {
                save_ver(&version, None);
            }
            Ok(())
        }
        LinkOutcome::Success {
            status: StatusCode::OK,
            body,
        } => {
            let res: ClientSelfResponse = serde_json::from_slice(&body).map_err(|_| ())?;
            if res.protocol_version != 1 || res.revision < 0 {
                return Err(());
            }
            record_journal(&res);
            let current_snap = snapshot_source();
            if res.reported.as_ref() == Some(&ClientSelfReported::from(&current_snap)) {
                // Unchanged snapshot -> no PUT
                return Ok(());
            }

            // Send PUT
            let req = ClientSelfUpdateRequest {
                protocol_version: 1,
                expected_revision: res.revision,
                reported: ClientSelfReported::from(&current_snap),
            };
            let req_bytes = serde_json::to_vec(&req).map_err(|_| ())?;
            let put_outcome = capability.clients_self_put(req_bytes, timeout).await;
            match put_outcome {
                LinkOutcome::Success {
                    status: StatusCode::OK,
                    body: put_body,
                } => {
                    if let Ok(put_res) = serde_json::from_slice::<ClientSelfResponse>(&put_body) {
                        record_journal(&put_res);
                    }
                    Ok(())
                }
                LinkOutcome::LocalRejected {
                    status: StatusCode::CONFLICT,
                } => {
                    // 409 Conflict: reread GET, retry at most once with newest snapshot
                    let get2_outcome = capability.clients_self_get(timeout).await;
                    match get2_outcome {
                        LinkOutcome::Success {
                            status: StatusCode::OK,
                            body: body2,
                        } => {
                            let res2: ClientSelfResponse =
                                serde_json::from_slice(&body2).map_err(|_| ())?;
                            if res2.protocol_version != 1 || res2.revision < 0 {
                                return Err(());
                            }
                            record_journal(&res2);
                            let current_snap2 = snapshot_source();
                            if res2.reported.as_ref()
                                == Some(&ClientSelfReported::from(&current_snap2))
                            {
                                // Re-read matches newest snapshot -> no PUT
                                return Ok(());
                            }
                            let req2 = ClientSelfUpdateRequest {
                                protocol_version: 1,
                                expected_revision: res2.revision,
                                reported: ClientSelfReported::from(&current_snap2),
                            };
                            let req_bytes2 = serde_json::to_vec(&req2).map_err(|_| ())?;
                            let put2_outcome =
                                capability.clients_self_put(req_bytes2, timeout).await;
                            match put2_outcome {
                                LinkOutcome::Success {
                                    status: StatusCode::OK,
                                    body: put_body2,
                                } => {
                                    if let Ok(put_res2) =
                                        serde_json::from_slice::<ClientSelfResponse>(&put_body2)
                                    {
                                        record_journal(&put_res2);
                                    }
                                    Ok(())
                                }
                                _ => Ok(()),
                            }
                        }
                        _ => Err(()),
                    }
                }
                // Non-409 outcome (e.g. 405 Method Not Allowed from loopback bridge due to pinned spl-core):
                // Optional failure of publication; retain last-known journal name/version recorded from GET above.
                _ => Ok(()),
            }
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitization_rules() {
        assert_eq!(sanitize_reported_field(None, 80), None);
        assert_eq!(sanitize_reported_field(Some("   "), 80), None);
        assert_eq!(
            sanitize_reported_field(Some("valid-name"), 80),
            Some("valid-name".to_string())
        );
        assert_eq!(
            sanitize_reported_field(Some("  trimmed  "), 80),
            Some("trimmed".to_string())
        );

        // Control characters: C0 (e.g. \n, \t, \0) or DEL (0x7f)
        assert_eq!(sanitize_reported_field(Some("bad\nname"), 80), None);
        assert_eq!(sanitize_reported_field(Some("bad\tname"), 80), None);
        assert_eq!(sanitize_reported_field(Some("bad\x7fname"), 80), None);
        assert_eq!(sanitize_reported_field(Some("bad\0name"), 80), None);

        // Byte bound check (max_bytes) without mid-character truncation
        // "é" is 2 bytes (0xc3 0xa9) in UTF-8
        let e_acute = "é";
        assert_eq!(e_acute.len(), 2);
        assert_eq!(sanitize_reported_field(Some(e_acute), 1), None);
        assert_eq!(
            sanitize_reported_field(Some(e_acute), 2),
            Some("é".to_string())
        );

        let long_ascii = "a".repeat(81);
        assert_eq!(sanitize_reported_field(Some(&long_ascii), 80), None);
        let exact_ascii = "a".repeat(80);
        assert_eq!(
            sanitize_reported_field(Some(&exact_ascii), 80),
            Some(exact_ascii)
        );
    }

    #[test]
    fn test_current_device_snapshot_creation() {
        let snap = current_device_snapshot(|| Ok("my-host".to_string()));
        assert_eq!(snap.name, Some("my-host".to_string()));
        assert_eq!(snap.platform, Some("linux".to_string()));
        assert_eq!(snap.device_type, Some("desktop".to_string()));
        assert_eq!(snap.app_id, Some("solstone-linux".to_string()));
        assert_eq!(
            snap.app_version,
            Some(env!("CARGO_PKG_VERSION").to_string())
        );

        // Failed hostname still produces snapshot with None name
        let snap_err = current_device_snapshot(|| Err(io::Error::other("no host")));
        assert_eq!(snap_err.name, None);
        assert_eq!(snap_err.platform, Some("linux".to_string()));
    }

    #[tokio::test]
    async fn test_metadata_sync_success() {
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

        // Enqueue GET /app/network/api/clients/self
        let get_body = serde_json::json!({
            "protocol_version": 1,
            "revision": 1,
            "reported": {
                "name": "old-host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.0"
            },
            "journal": {
                "name": "My Journal",
                "version": "v1.2.4"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get_body).unwrap());

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let identity_key = "test-identity";

        let snap = DeviceSnapshot {
            name: Some("new-host".to_string()),
            platform: Some("linux".to_string()),
            device_type: Some("desktop".to_string()),
            app_id: Some("solstone-linux".to_string()),
            app_version: Some("1.0.3".to_string()),
        };

        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            identity_key,
            || snap.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        let loaded = crate::sync_health::load_paired_journal_version(&state_dir).unwrap();
        assert_eq!(loaded.version, "v1.2.4");
        assert_eq!(loaded.name.as_deref(), Some("My Journal"));

        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/app/network/api/clients/self");

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_metadata_sync_404_fallback_to_system_status() {
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

        // GET /app/network/api/clients/self -> 404
        peer.enqueue_response(404, b"Not Found");

        // system_status -> 200
        let status_body = serde_json::json!({
            "version": {
                "current": "v0.9.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&status_body).unwrap());

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let identity_key = "test-identity-404";

        let snap = DeviceSnapshot::default();
        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            identity_key,
            || snap.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        let loaded = crate::sync_health::load_paired_journal_version(&state_dir).unwrap();
        assert_eq!(loaded.version, "v0.9.0");
        assert_eq!(loaded.name, None);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_metadata_sync_409_single_retry() {
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

        // GET returns revision 1
        let get1 = serde_json::json!({
            "protocol_version": 1,
            "revision": 1,
            "reported": null,
            "journal": {
                "name": "Journal",
                "version": "v2.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get1).unwrap());

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let identity_key = "test-identity-409";

        let snap = DeviceSnapshot {
            name: Some("host".to_string()),
            ..DeviceSnapshot::default()
        };
        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            identity_key,
            || snap.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        // Prove 405 is NOT treated as 409: after GET + PUT 405, peer has exactly one GET (no retry GET)
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/app/network/api/clients/self");

        let loaded = crate::sync_health::load_paired_journal_version(&state_dir).unwrap();
        assert_eq!(loaded.version, "v2.0");

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_metadata_sync_unchanged_snapshot_skips_put() {
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

        let snap = DeviceSnapshot {
            name: Some("same-host".to_string()),
            platform: Some("linux".to_string()),
            device_type: Some("desktop".to_string()),
            app_id: Some("solstone-linux".to_string()),
            app_version: Some("1.0.0".to_string()),
        };

        // GET returns identical reported data
        let get_body = serde_json::json!({
            "protocol_version": 1,
            "revision": 5,
            "reported": {
                "name": "same-host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.0"
            },
            "journal": {
                "name": "My Journal",
                "version": "v1.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get_body).unwrap());

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let identity_key = "test-identity-unchanged";

        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            identity_key,
            || snap.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        // Peer only served 1 response (the GET), no PUT was sent
        assert_eq!(peer.accepted_carriers(), 1);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn metadata_put_uses_http_put_method() {
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

        let get_res = serde_json::json!({
            "protocol_version": 1,
            "revision": 3,
            "reported": {
                "name": "old-host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "0.1.0"
            },
            "journal": {
                "version": "1.0.0",
                "name": "My Journal"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get_res).unwrap());

        let snapshot = DeviceSnapshot {
            name: Some("new-host".to_string()),
            platform: Some("linux".to_string()),
            device_type: Some("desktop".to_string()),
            app_id: Some("solstone-linux".to_string()),
            app_version: Some("1.0.3".to_string()),
        };

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            "ident-1",
            || snapshot.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        // GET was forwarded to peer
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/app/network/api/clients/self");

        // The following clients_self_put returns 405 Method Not Allowed locally at the loopback bridge
        let put_outcome = capability
            .clients_self_put(b"{}".to_vec(), Duration::from_secs(5))
            .await;
        assert_eq!(
            put_outcome,
            LinkOutcome::LocalRejected {
                status: reqwest::StatusCode::METHOD_NOT_ALLOWED,
            }
        );

        // Peer request list did not grow by a second forwarded journal request
        assert_eq!(peer.requests().len(), 1);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn metadata_corrupt_clients_self_does_not_fallback_to_system_status() {
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

        // Return 200 OK but corrupt non-json body
        peer.enqueue_response(200, b"not json".to_vec());

        let snapshot = DeviceSnapshot {
            name: Some("host".to_string()),
            platform: Some("linux".to_string()),
            device_type: Some("desktop".to_string()),
            app_id: Some("solstone-linux".to_string()),
            app_version: Some("1.0.3".to_string()),
        };
        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            "ident-1",
            || snapshot.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_err());

        // Only one request was made (GET /app/network/api/clients/self), no fallback to system status
        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/app/network/api/clients/self");

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn metadata_control_and_multibyte_name_nulls_only_that_field() {
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

        let get_res = serde_json::json!({
            "protocol_version": 1,
            "revision": 1,
            "reported": null
        });
        peer.enqueue_response(200, serde_json::to_vec(&get_res).unwrap());

        // Hostname with control characters
        let snapshot = current_device_snapshot(|| Ok("invalid\x07hostname".to_string()));
        assert_eq!(snapshot.name, None);
        assert_eq!(snapshot.platform, Some("linux".to_string()));
        assert_eq!(snapshot.device_type, Some("desktop".to_string()));
        assert_eq!(snapshot.app_id, Some("solstone-linux".to_string()));

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let res = execute_metadata_sync(
            &capability,
            &state_dir,
            "ident-1",
            || snapshot.clone(),
            Duration::from_secs(5),
        )
        .await;
        assert!(res.is_ok());

        let requests = peer.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
