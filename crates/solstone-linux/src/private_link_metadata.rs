// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::private_link::{LinkOutcome, PrivateLinkCapability};
use reqwest::StatusCode;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    pub(crate) revision: u64,
    pub(crate) reported: Option<ClientSelfReported>,
    pub(crate) owner_label: Option<String>,
    pub(crate) display_label: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) journal: Option<JournalDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClientSelfUpdateRequest {
    pub(crate) protocol_version: u32,
    pub(crate) expected_revision: u64,
    pub(crate) reported: ClientSelfReported,
}

pub(crate) fn parse_and_validate_client_self_response(
    body: &[u8],
) -> Result<ClientSelfResponse, ()> {
    let val: serde_json::Value = serde_json::from_slice(body).map_err(|_| ())?;
    let obj = val.as_object().ok_or(())?;

    let proto = obj
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .ok_or(())?;
    if proto != 1 {
        return Err(());
    }

    let revision = obj.get("revision").and_then(|v| v.as_u64()).ok_or(())?;

    let display_label = obj
        .get("display_label")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();

    let owner_label_val = obj.get("owner_label").ok_or(())?;
    let owner_label = match owner_label_val {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        _ => return Err(()),
    };

    let updated_at_val = obj.get("updated_at").ok_or(())?;
    let updated_at = match updated_at_val {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        _ => return Err(()),
    };

    let reported_val = obj.get("reported").ok_or(())?;
    let reported = match reported_val {
        serde_json::Value::Null => None,
        serde_json::Value::Object(rep_obj) => {
            let nullable = |key: &str| match rep_obj.get(key).ok_or(())? {
                serde_json::Value::Null => Ok(None),
                serde_json::Value::String(value) => Ok(Some(value.clone())),
                _ => Err(()),
            };
            Some(ClientSelfReported {
                name: nullable("name")?,
                platform: nullable("platform")?,
                device_type: nullable("device_type")?,
                app_id: nullable("app_id")?,
                app_version: nullable("app_version")?,
            })
        }
        _ => return Err(()),
    };

    let journal_val = obj.get("journal").and_then(|v| v.as_object()).ok_or(())?;
    let journal_ver = journal_val
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let journal_name_val = journal_val.get("name").ok_or(())?;
    let journal_name = match journal_name_val {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        _ => return Err(()),
    };

    Ok(ClientSelfResponse {
        protocol_version: 1,
        revision,
        reported,
        owner_label,
        display_label: Some(display_label),
        updated_at,
        journal: Some(JournalDescriptor {
            name: journal_name,
            version: Some(journal_ver),
        }),
    })
}

struct MetadataPublication {
    capability: PrivateLinkCapability,
    state_dir: PathBuf,
    association_epoch: u64,
    lease: Arc<crate::private_link::OptionalAttemptLease>,
}

impl MetadataPublication {
    async fn save(
        &self,
        version: String,
        name: Option<String>,
        preserve_name: bool,
    ) -> Result<(), ()> {
        let state_dir = self.state_dir.clone();
        let lease = Arc::clone(&self.lease);
        let epoch = self.association_epoch;
        self.capability
            .blocking_optional(move |writer, _| {
                writer.publish_metadata(
                    &lease,
                    epoch,
                    &state_dir,
                    &version,
                    name.as_deref(),
                    preserve_name,
                )
            })
            .await
    }

    async fn record(&self, response: &ClientSelfResponse) -> Result<(), ()> {
        let journal = response.journal.as_ref().ok_or(())?;
        self.save(
            journal.version.clone().ok_or(())?,
            journal.name.clone(),
            false,
        )
        .await
    }
}

pub(crate) async fn execute_metadata_sync<F>(
    capability: &PrivateLinkCapability,
    state_dir: &Path,
    _identity_key: &str,
    snapshot_source: F,
    timeout: Duration,
) -> Result<(), ()>
where
    F: Fn() -> DeviceSnapshot,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let attempt = capability.writer().metadata_attempt(deadline);
    let publication = MetadataPublication {
        capability: capability.clone(),
        state_dir: state_dir.to_path_buf(),
        association_epoch: capability.facts().association_epoch(),
        lease: Arc::clone(&attempt.lease),
    };
    tokio::time::timeout_at(deadline, async {
        let remaining = || {
            if !attempt.lease.is_current() {
                return Err(());
            }
            Ok(deadline.saturating_duration_since(tokio::time::Instant::now()))
        };
        let outcome = capability.clients_self_get(remaining()?).await;
        remaining()?;
        if matches!(
            outcome,
            LinkOutcome::LocalRejected {
                status: StatusCode::NOT_FOUND
            }
        ) {
            if let Ok(Some(version)) = capability.system_status_optional(remaining()?).await {
                remaining()?;
                publication.save(version, None, true).await?;
            }
            return Ok(());
        }
        let LinkOutcome::Success {
            status: StatusCode::OK,
            body,
        } = outcome
        else {
            return Err(());
        };
        let mut response = parse_and_validate_client_self_response(&body)?;
        publication.record(&response).await?;
        for retry in 0..2 {
            remaining()?;
            let snapshot = snapshot_source();
            if response.reported.as_ref() == Some(&ClientSelfReported::from(&snapshot)) {
                return Ok(());
            }
            let request = ClientSelfUpdateRequest {
                protocol_version: 1,
                expected_revision: response.revision,
                reported: ClientSelfReported::from(&snapshot),
            };
            let body = serde_json::to_vec(&request).map_err(|_| ())?;
            let outcome = capability.clients_self_put(body, remaining()?).await;
            remaining()?;
            match outcome {
                LinkOutcome::Success {
                    status: StatusCode::OK,
                    body,
                } => {
                    let response = parse_and_validate_client_self_response(&body)?;
                    publication.record(&response).await?;
                    return Ok(());
                }
                LinkOutcome::LocalRejected {
                    status: StatusCode::CONFLICT,
                } if retry == 0 => {
                    let outcome = capability.clients_self_get(remaining()?).await;
                    remaining()?;
                    let LinkOutcome::Success {
                        status: StatusCode::OK,
                        body,
                    } = outcome
                    else {
                        return Err(());
                    };
                    response = parse_and_validate_client_self_response(&body)?;
                    publication.record(&response).await?;
                }
                _ => return Err(()),
            }
        }
        Err(())
    })
    .await
    .unwrap_or(Err(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_reported_object_accepts_all_required_nullable_strings() {
        let body = serde_json::json!({"protocol_version":1,"revision":u64::MAX,
            "reported":{"name":null,"platform":null,"device_type":null,"app_id":null,"app_version":null},
            "owner_label":null,"updated_at":null,"display_label":"test",
            "journal":{"name":null,"version":"1.0"}});
        assert!(
            parse_and_validate_client_self_response(&serde_json::to_vec(&body).unwrap()).is_ok()
        );
        for key in ["name", "platform", "device_type", "app_id", "app_version"] {
            let mut missing = body.clone();
            missing["reported"].as_object_mut().unwrap().remove(key);
            assert!(
                parse_and_validate_client_self_response(&serde_json::to_vec(&missing).unwrap())
                    .is_err()
            );
        }
    }

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

    #[test]
    fn test_parse_and_validate_client_self_response_schema() {
        let valid_full = serde_json::json!({
            "protocol_version": 1,
            "revision": 3,
            "display_label": "My Linux",
            "owner_label": "Owner",
            "updated_at": "2026-09-07T12:00:00Z",
            "reported": {
                "name": "host-1",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.3"
            },
            "journal": {
                "name": "Journal 1",
                "version": "v1.4.0"
            }
        });
        let res =
            parse_and_validate_client_self_response(&serde_json::to_vec(&valid_full).unwrap());
        assert!(res.is_ok());

        let valid_nulls = serde_json::json!({
            "protocol_version": 1,
            "revision": 0,
            "display_label": "My Linux",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": {
                "name": null,
                "version": "v1.4.0"
            }
        });
        let res_nulls =
            parse_and_validate_client_self_response(&serde_json::to_vec(&valid_nulls).unwrap());
        assert!(res_nulls.is_ok());

        // Omitted nullable field owner_label -> error
        let omitted_owner = serde_json::json!({
            "protocol_version": 1,
            "revision": 0,
            "display_label": "My Linux",
            "updated_at": null,
            "reported": null,
            "journal": { "name": null, "version": "v1.4.0" }
        });
        assert!(
            parse_and_validate_client_self_response(&serde_json::to_vec(&omitted_owner).unwrap())
                .is_err()
        );

        // Omitted nullable field journal.name -> error
        let omitted_jname = serde_json::json!({
            "protocol_version": 1,
            "revision": 0,
            "display_label": "My Linux",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": { "version": "v1.4.0" }
        });
        assert!(
            parse_and_validate_client_self_response(&serde_json::to_vec(&omitted_jname).unwrap())
                .is_err()
        );

        // Negative revision -> error
        let neg_rev = serde_json::json!({
            "protocol_version": 1,
            "revision": -1,
            "display_label": "My Linux",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": { "name": null, "version": "v1.4.0" }
        });
        assert!(
            parse_and_validate_client_self_response(&serde_json::to_vec(&neg_rev).unwrap())
                .is_err()
        );

        // Reported with missing field -> error
        let partial_rep = serde_json::json!({
            "protocol_version": 1,
            "revision": 1,
            "display_label": "My Linux",
            "owner_label": null,
            "updated_at": null,
            "reported": {
                "name": "host-1",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux"
            },
            "journal": { "name": null, "version": "v1.4.0" }
        });
        assert!(
            parse_and_validate_client_self_response(&serde_json::to_vec(&partial_rep).unwrap())
                .is_err()
        );
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
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": {
                "name": "new-host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.3"
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
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": {
                "name": "Journal",
                "version": "v2.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get1).unwrap());

        // PUT returns 409 Conflict
        peer.enqueue_response(409, b"Conflict");

        // GET2 returns revision 2
        let get2 = serde_json::json!({
            "protocol_version": 1,
            "revision": 2,
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": {
                "name": "Journal",
                "version": "v2.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get2).unwrap());

        // PUT2 returns 200 OK
        let put2 = serde_json::json!({
            "protocol_version": 1,
            "revision": 3,
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": {
                "name": "host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.3"
            },
            "journal": {
                "name": "Journal",
                "version": "v2.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&put2).unwrap());

        let state_dir = temp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let identity_key = "test-identity-409";

        let snap = DeviceSnapshot {
            name: Some("host".to_string()),
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

        let requests = peer.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[2].method, "GET");
        assert_eq!(requests[3].method, "PUT");

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
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
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
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
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

        let put_res = serde_json::json!({
            "protocol_version": 1,
            "revision": 4,
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": {
                "name": "new-host",
                "platform": "linux",
                "device_type": "desktop",
                "app_id": "solstone-linux",
                "app_version": "1.0.3"
            },
            "journal": {
                "version": "1.0.0",
                "name": "My Journal"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&put_res).unwrap());

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

        // GET and PUT forwarded to peer
        let requests = peer.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/app/network/api/clients/self");
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[1].path, "/app/network/api/clients/self");

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
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": null,
            "journal": {
                "name": null,
                "version": "1.0.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&get_res).unwrap());

        // Hostname with control characters
        let snapshot = current_device_snapshot(|| Ok("invalid\x07hostname".to_string()));
        assert_eq!(snapshot.name, None);
        assert_eq!(snapshot.platform, Some("linux".to_string()));
        assert_eq!(snapshot.device_type, Some("desktop".to_string()));
        assert_eq!(snapshot.app_id, Some("solstone-linux".to_string()));

        let put_res = serde_json::json!({
            "protocol_version": 1,
            "revision": 2,
            "display_label": "desktop",
            "owner_label": null,
            "updated_at": null,
            "reported": {
                "name": null,
                "platform": "linux",
                "device_type": "desktop",
                "app_version": snapshot.app_version.clone(),
                "app_id": "solstone-linux",
            },
            "journal": {
                "name": null,
                "version": "1.0.0"
            }
        });
        peer.enqueue_response(200, serde_json::to_vec(&put_res).unwrap());

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
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[1].method, "PUT");
        let put_body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert_eq!(put_body["reported"]["name"], serde_json::Value::Null);
        assert_eq!(put_body["reported"]["platform"], "linux");
        assert_eq!(put_body["reported"]["device_type"], "desktop");
        assert_eq!(put_body["reported"]["app_id"], "solstone-linux");

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn stale_get_cannot_write_or_put_and_legacy_name_is_identity_bound() {
        use crate::private_link::{LinkFact, start_private_link_session};
        use crate::private_link_test_peer::PrivateLinkPeer;
        let peer = PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = start_private_link_session(temp.path(), peer.credential(), "stream")
            .await
            .unwrap();
        let cap = session.capability();
        let body = serde_json::to_vec(&serde_json::json!({"protocol_version":1,"revision":1,"reported":null,
            "owner_label":null,"updated_at":null,"display_label":"test","journal":{"name":"old","version":"1.0"}})).unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        peer.enqueue_gated_response(200, body, Arc::clone(&gate));
        let old_cap = cap.clone();
        let path = temp.path().to_path_buf();
        let old = tokio::spawn(async move {
            execute_metadata_sync(
                &old_cap,
                &path,
                "ignored",
                DeviceSnapshot::default,
                Duration::from_secs(2),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while peer.requests().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cap.writer().invalidate_optional_attempts();
        gate.notify_one();
        assert!(old.await.unwrap().is_err());
        assert_eq!(peer.requests().len(), 1);
        assert!(crate::sync_health::load_paired_journal_version(temp.path()).is_none());
        crate::sync_health::save_paired_journal_version(
            temp.path(),
            "another-pair",
            "0.1",
            Some("other name"),
        )
        .unwrap();
        peer.enqueue_response(404, b"".to_vec());
        peer.enqueue_response(200, br#"{"version":{"current":"2.0"}}"#.to_vec());
        cap.facts().publish(LinkFact::CarrierProven);
        execute_metadata_sync(
            &cap,
            temp.path(),
            "ignored",
            DeviceSnapshot::default,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        let saved = crate::sync_health::load_paired_journal_version(temp.path()).unwrap();
        assert_eq!(saved.identity_key, cap.writer().identity_key());
        assert_eq!(saved.name, None);
        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
    #[tokio::test]
    async fn full_null_clears_name_and_malformed_put_retains_valid_get() {
        for malformed in [false, true] {
            let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
            let temp = tempfile::tempdir().unwrap();
            let session = crate::private_link::start_private_link_session(
                temp.path(),
                peer.credential(),
                "stream",
            )
            .await
            .unwrap();
            let cap = session.capability();
            let mut resource = serde_json::json!({"protocol_version":1,"revision":1,"reported":null,"owner_label":null,"updated_at":null,"display_label":"test","journal":{"name":"from GET","version":"1.0"}});
            peer.enqueue_response(200, serde_json::to_vec(&resource).unwrap());
            resource["journal"]["name"] = serde_json::Value::Null;
            if malformed {
                resource.as_object_mut().unwrap().remove("owner_label");
            }
            peer.enqueue_response(200, serde_json::to_vec(&resource).unwrap());
            let result = execute_metadata_sync(
                &cap,
                temp.path(),
                "ignored",
                DeviceSnapshot::default,
                Duration::from_secs(2),
            )
            .await;
            assert_eq!(result.is_err(), malformed);
            let saved = crate::sync_health::load_paired_journal_version(temp.path()).unwrap();
            assert_eq!(
                saved.name.as_deref(),
                if malformed { Some("from GET") } else { None }
            );
            assert_eq!(peer.requests().len(), 2);
            session.shutdown().await.unwrap();
            peer.shutdown().await;
        }
    }
}
