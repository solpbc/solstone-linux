// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
pub use spl_core::relay_access::RelayAccess;
use spl_transport::credential::EndpointAddr;

use crate::private_link::{LinkOutcome, PrivateLinkCapability};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListedEndpoints {
    Unusable,
    Ignore,
    Listed(Vec<EndpointAddr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointPath {
    Unknown,
    #[allow(dead_code)]
    ProvenRelay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndpointCommit {
    Unchanged,
    Committed,
    Stale,
    Failed,
}

pub(crate) fn parse_listed_endpoints(body: &[u8]) -> ListedEndpoints {
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) else {
        return ListedEndpoints::Unusable;
    };
    let Some(obj) = val.as_object() else {
        return ListedEndpoints::Unusable;
    };
    let Some(v) = obj.get("v").and_then(|v| v.as_f64()) else {
        return ListedEndpoints::Ignore;
    };
    if !v.is_finite() || v < 2.0 {
        return ListedEndpoints::Ignore;
    }
    let Some(endpoints) = obj.get("endpoints").and_then(|e| e.as_array()) else {
        return ListedEndpoints::Unusable;
    };

    let mut parsed = Vec::new();
    for entry in endpoints {
        let Some(entry_obj) = entry.as_object() else {
            continue;
        };
        let Some(ip_str) = entry_obj.get("ip").and_then(|ip| ip.as_str()) else {
            continue;
        };
        let Some(port_u64) = entry_obj.get("port").and_then(|p| p.as_u64()) else {
            continue;
        };
        if port_u64 == 0 || port_u64 > 65535 {
            continue;
        }
        let port = port_u64 as u16;
        let parsed_ip = if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
            Some(ip)
        } else if ip_str.starts_with('[') && ip_str.ends_with(']') && ip_str.len() >= 2 {
            let inner = &ip_str[1..ip_str.len() - 1];
            if !inner.starts_with('[') && !inner.ends_with(']') {
                inner.parse::<std::net::IpAddr>().ok()
            } else {
                None
            }
        } else {
            None
        };
        let Some(ip_addr) = parsed_ip else {
            continue;
        };
        parsed.push((ip_addr, port));
    }

    let mut deduped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (ip, port) in parsed {
        if seen.insert((ip, port)) {
            deduped.push(EndpointAddr {
                host: ip.to_string(),
                port,
            });
        }
    }
    if deduped.is_empty() {
        return ListedEndpoints::Ignore;
    }
    ListedEndpoints::Listed(deduped)
}

pub(crate) fn merge_dial_endpoints(
    stored: &[EndpointAddr],
    listed: &[EndpointAddr],
    path: EndpointPath,
) -> Vec<EndpointAddr> {
    match path {
        EndpointPath::ProvenRelay => listed.to_vec(),
        EndpointPath::Unknown => {
            let mut result = listed.to_vec();
            let mut listed_set = std::collections::HashSet::new();
            for ep in listed {
                if let Ok(ip) = ep.host.parse::<std::net::IpAddr>() {
                    listed_set.insert((ip, ep.port));
                }
            }
            let mut kept_count = 0;
            for stored_ep in stored {
                if kept_count == 2 {
                    break;
                }
                let is_member = if let Ok(ip) = stored_ep.host.parse::<std::net::IpAddr>() {
                    listed_set.contains(&(ip, stored_ep.port))
                } else {
                    false
                };
                if !is_member {
                    result.push(stored_ep.clone());
                    kept_count += 1;
                }
            }
            result
        }
    }
}

pub(crate) async fn refresh_dial_endpoints(
    capability: &PrivateLinkCapability,
    attempt: &crate::private_link::OptionalAttempt,
) {
    let Some(timeout) = attempt.lease.time_left() else {
        return;
    };
    let current_endpoints = capability.writer().current_credential().endpoints;
    if !current_endpoints.is_empty()
        && current_endpoints.iter().all(|ep| {
            ep.host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
    {
        return;
    }
    let outcome = capability.local_endpoints_get(timeout).await;
    let body = match outcome {
        LinkOutcome::Success {
            status: reqwest::StatusCode::OK,
            body,
        } => body,
        _ => {
            tracing::warn!("dial endpoint refresh failed");
            return;
        }
    };
    let listed = match parse_listed_endpoints(&body) {
        ListedEndpoints::Unusable => {
            tracing::warn!("dial endpoint refresh failed");
            return;
        }
        ListedEndpoints::Ignore => return,
        ListedEndpoints::Listed(listed) => listed,
    };

    let lease = Arc::clone(&attempt.lease);
    let caller_pairing_id = capability.writer().pairing_id().to_owned();
    let _ = capability
        .blocking_optional(move |writer, opener| {
            let commit = writer.commit_dial_endpoints_with_attempt(
                &lease,
                &caller_pairing_id,
                listed,
                EndpointPath::Unknown,
            );
            match commit {
                EndpointCommit::Committed => {
                    let credential = writer.current_credential();
                    if let Some(lan) = crate::private_link::lan_dial_credential(&credential) {
                        match spl_transport::TransportClient::new(lan, None) {
                            Ok(client) => opener.replace_lan_transport(client),
                            Err(_) => tracing::warn!("dial endpoint refresh failed"),
                        }
                    }
                    Ok(())
                }
                EndpointCommit::Failed => {
                    tracing::warn!("dial endpoint refresh failed");
                    Err(())
                }
                EndpointCommit::Unchanged | EndpointCommit::Stale => Ok(()),
            }
        })
        .await;
}

#[derive(Debug)]
pub(crate) enum RelayAccessWireResponse {
    NotConfigured,
    Ready(RelayAccess),
}

pub(crate) fn parse_relay_access_response(bytes: &[u8]) -> Result<RelayAccessWireResponse, ()> {
    let val: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let obj = val.as_object().ok_or(())?;
    let pv = obj
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .ok_or(())?;
    if pv != 2 {
        return Err(());
    }
    let status = obj.get("status").and_then(|v| v.as_str()).ok_or(())?;
    match status {
        "not_configured" if obj.len() == 2 => Ok(RelayAccessWireResponse::NotConfigured),
        "ready" => {
            let ready: RelayAccess = serde_json::from_value(val).map_err(|_| ())?;
            Ok(RelayAccessWireResponse::Ready(ready))
        }
        _ => Err(()),
    }
}

pub(crate) fn validate_relay_access_ready(
    ready: &RelayAccess,
    instance_id: &str,
    now: i64,
) -> Result<spl_core::jwt::JwtClaims, ()> {
    spl_transport::validate_relay_origin(&ready.relay_origin).map_err(|_| ())?;
    ready.claims(instance_id, now).ok_or(())
}

pub(crate) async fn execute_relay_access_sync(
    capability: &PrivateLinkCapability,
    timeout: Duration,
) -> Result<(), ()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let writer = capability.writer();
    let attempt = writer.access_attempt(deadline);
    let relay_result = tokio::time::timeout_at(deadline, async {
        let _ = capability
            .retry_optional_reconciliation(Arc::clone(&attempt.lease))
            .await;
        let pairing_id = writer.pairing_id().to_string();
        let access_gen = writer.access_mutation_generation();
        let opener_incarnation = capability.opener_relay_incarnation();
        if !attempt.lease.is_current() {
            return Err(());
        }
        let outcome = capability
            .relay_access_get(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
        let LinkOutcome::Success {
            status: StatusCode::OK,
            body,
        } = outcome
        else {
            return Err(());
        };
        let wire = parse_relay_access_response(&body)?;
        let lease = Arc::clone(&attempt.lease);
        capability
            .blocking_optional(move |writer, opener| {
                if !lease.is_current() {
                    return Err(());
                }
                match wire {
                    RelayAccessWireResponse::NotConfigured => writer
                        .commit_optional_clear_with_attempt(
                            &opener,
                            &pairing_id,
                            access_gen,
                            opener_incarnation,
                            Some(&lease),
                        ),
                    RelayAccessWireResponse::Ready(ready) => {
                        let claims = validate_relay_access_ready(
                            &ready,
                            writer.instance_id(),
                            chrono::Utc::now().timestamp(),
                        )?;
                        writer.commit_optional_relay_with_attempt(
                            &opener,
                            &pairing_id,
                            access_gen,
                            opener_incarnation,
                            &ready.relay_origin,
                            &ready.device_token,
                            claims.exp,
                            Some(&lease),
                        )
                    }
                }
            })
            .await
    })
    .await
    .unwrap_or(Err(()));

    refresh_dial_endpoints(capability, &attempt).await;
    relay_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_link::tests::base64url_no_pad;
    use spl_core::relay_access::instance_claims;

    fn make_test_jwt(claims_json: serde_json::Value) -> String {
        let header = base64url_no_pad(b"{\"alg\":\"none\",\"typ\":\"JWT\"}");
        let claims = base64url_no_pad(serde_json::to_vec(&claims_json).unwrap().as_slice());
        format!("{header}.{claims}.signature")
    }

    #[test]
    fn test_v2_jwt_decode_exact_keys() {
        let claims_json = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jwt-id-1",
        });
        let token = make_test_jwt(claims_json);
        let decoded = instance_claims(&token, "inst-123", 1500).expect("valid claims");
        assert_eq!(decoded.exp, 2000);

        // Reject extra keys
        let claims_extra = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jwt-id-1",
            "device_fp": "extra-fp",
        });
        let token_extra = make_test_jwt(claims_extra);
        assert!(instance_claims(&token_extra, "inst-123", 1500).is_none());
    }

    #[test]
    fn test_parse_relay_access_response() {
        let not_configured = br#"{"protocol_version": 2, "status": "not_configured"}"#;
        let parsed = parse_relay_access_response(not_configured).expect("parse not_configured");
        assert!(matches!(parsed, RelayAccessWireResponse::NotConfigured));

        let ready_json = serde_json::json!({
            "protocol_version": 2,
            "status": "ready",
            "relay_origin": "https://relay.example.com",
            "instance_id": "inst-123",
            "device_token": "a.b.c",
            "expires_at": "2026-09-08T12:00:00Z"
        });
        let ready_bytes = serde_json::to_vec(&ready_json).unwrap();
        let parsed_ready = parse_relay_access_response(&ready_bytes).expect("parse ready");
        assert!(matches!(parsed_ready, RelayAccessWireResponse::Ready(_)));
    }

    #[test]
    fn test_validate_relay_access_ready_edges() {
        let claims_json = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jwt-id-1",
        });
        let token = make_test_jwt(claims_json);
        let ready = RelayAccess {
            protocol_version: 2,
            status: "ready".to_string(),
            relay_origin: "https://relay.example.com".to_string(),
            instance_id: "inst-123".to_string(),
            device_token: token.clone(),
            expires_at: "1970-01-01T00:33:20Z".to_string(), // timestamp 2000
        };

        // Success when current_time < exp
        assert!(validate_relay_access_ready(&ready, "inst-123", 1500).is_ok());

        // Fail when expired
        assert!(validate_relay_access_ready(&ready, "inst-123", 2000).is_err());
        assert!(validate_relay_access_ready(&ready, "inst-123", 2001).is_err());

        // Fail when paired instance_id mismatch
        assert!(validate_relay_access_ready(&ready, "inst-other", 1500).is_err());

        // Fail when expires_at timestamp mismatches jwt exp
        let ready_mismatched_dt = RelayAccess {
            expires_at: "1970-01-01T00:33:21Z".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_mismatched_dt, "inst-123", 1500).is_err());

        // Fractional RFC3339 expires_at must be rejected (nanosecond != 0)
        let ready_fractional = RelayAccess {
            expires_at: "1970-01-01T00:33:20.500Z".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_fractional, "inst-123", 1500).is_err());

        // Fail when relay origin has userinfo
        let ready_userinfo = RelayAccess {
            relay_origin: "https://user@relay.example.com".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_userinfo, "inst-123", 1500).is_err());

        // Fail when relay origin is invalid
        let ready_bad_origin = RelayAccess {
            relay_origin: "not a valid url".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_bad_origin, "inst-123", 1500).is_err());
    }

    #[test]
    fn test_jwt_boundary_validation() {
        let make_ready = |token: String| RelayAccess {
            protocol_version: 2,
            status: "ready".to_string(),
            relay_origin: "https://relay.example.com".to_string(),
            instance_id: "inst-123".to_string(),
            device_token: token,
            expires_at: "1970-01-01T00:33:20Z".to_string(),
        };

        // 1. Extra / empty JWT segments
        assert!(validate_relay_access_ready(&make_ready("a.b".into()), "inst-123", 1000).is_err());
        assert!(
            validate_relay_access_ready(&make_ready("a.b.c.d".into()), "inst-123", 1000).is_err()
        );
        assert!(validate_relay_access_ready(&make_ready("a..c".into()), "inst-123", 1000).is_err());
        assert!(instance_claims("a.b", "inst-123", 1000).is_none());
        assert!(instance_claims("a.b.c.d", "inst-123", 1000).is_none());
        assert!(instance_claims("a..c", "inst-123", 1000).is_none());

        // 2. Empty jti
        let empty_jti = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "",
        });
        let token_empty_jti = make_test_jwt(empty_jti);
        assert!(
            validate_relay_access_ready(&make_ready(token_empty_jti.clone()), "inst-123", 1500)
                .is_err()
        );
        assert!(instance_claims(&token_empty_jti, "inst-123", 1500).is_none());

        // 3. iat > now + 60 reject vs iat == now + 60 accept
        let iat_future_reject = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1061,
            "exp": 2000,
            "jti": "jti-1",
        });
        let token_iat_future_reject = make_test_jwt(iat_future_reject);
        assert!(
            validate_relay_access_ready(
                &make_ready(token_iat_future_reject.clone()),
                "inst-123",
                1000
            )
            .is_err()
        );
        assert!(instance_claims(&token_iat_future_reject, "inst-123", 1000).is_none());

        let iat_future_accept = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1060,
            "exp": 2000,
            "jti": "jti-1",
        });
        let token_iat_future_accept = make_test_jwt(iat_future_accept);
        assert!(
            validate_relay_access_ready(
                &make_ready(token_iat_future_accept.clone()),
                "inst-123",
                1000
            )
            .is_ok()
        );
        assert!(instance_claims(&token_iat_future_accept, "inst-123", 1000).is_some());

        // 4. Wrong aud / scope / sub / iss
        let wrong_aud = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "wrong-aud",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jti-1",
        });
        let token_wrong_aud = make_test_jwt(wrong_aud);
        assert!(
            validate_relay_access_ready(&make_ready(token_wrong_aud.clone()), "inst-123", 1500)
                .is_err()
        );
        assert!(instance_claims(&token_wrong_aud, "inst-123", 1500).is_none());

        let wrong_scope = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "instance:inst-123",
            "aud": "spl-relay",
            "scope": "wrong-scope",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jti-1",
        });
        let token_wrong_scope = make_test_jwt(wrong_scope);
        assert!(
            validate_relay_access_ready(&make_ready(token_wrong_scope.clone()), "inst-123", 1500)
                .is_err()
        );
        assert!(instance_claims(&token_wrong_scope, "inst-123", 1500).is_none());

        let wrong_sub = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": "inst-123", // missing instance: prefix
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": "inst-123",
            "iat": 1000,
            "exp": 2000,
            "jti": "jti-1",
        });
        let token_wrong_sub = make_test_jwt(wrong_sub);
        assert!(
            validate_relay_access_ready(&make_ready(token_wrong_sub.clone()), "inst-123", 1500)
                .is_err()
        );
        assert!(instance_claims(&token_wrong_sub, "inst-123", 1500).is_none());
    }

    #[tokio::test]
    async fn test_execute_relay_access_sync_not_configured() {
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

        // Enqueue not_configured response
        let not_configured = serde_json::json!({
            "protocol_version": 2,
            "status": "not_configured"
        });
        peer.enqueue_response(200, serde_json::to_vec(&not_configured).unwrap());

        let res = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert!(res.is_ok());

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_execute_relay_access_sync_ready() {
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

        let inst_id = peer.credential().instance_id;
        let future_exp = chrono::Utc::now().timestamp() + 3600;
        let expires_at_str = chrono::DateTime::from_timestamp(future_exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let claims_json = serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": format!("instance:{inst_id}"),
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": inst_id,
            "iat": future_exp - 7200,
            "exp": future_exp,
            "jti": "jwt-test-ready-1",
        });
        let token = make_test_jwt(claims_json);

        let ready_json = serde_json::json!({
            "protocol_version": 2,
            "status": "ready",
            "relay_origin": "https://relay.example.com",
            "instance_id": inst_id,
            "device_token": token,
            "expires_at": expires_at_str,
        });
        peer.enqueue_response(200, serde_json::to_vec(&ready_json).unwrap());

        let res = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert!(res.is_ok());

        // Credential writer has updated relay info
        let writer = capability.writer();
        let cred = writer.current_credential();
        assert_eq!(
            cred.relay_origin.as_deref(),
            Some("https://relay.example.com")
        );
        assert_eq!(cred.device_token_expires_at, Some(future_exp));

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[test]
    fn test_parse_listed_endpoints_version_gate() {
        // Missing v -> Ignore
        let missing_v = br#"{"endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(parse_listed_endpoints(missing_v), ListedEndpoints::Ignore);

        // String v -> Ignore
        let string_v = br#"{"v":"2","endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(parse_listed_endpoints(string_v), ListedEndpoints::Ignore);

        // v: 1 -> Ignore even with changed list
        let v1 = br#"{"v":1,"endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(parse_listed_endpoints(v1), ListedEndpoints::Ignore);

        // v: 1.5 -> Ignore
        let v1_5 = br#"{"v":1.5,"endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(parse_listed_endpoints(v1_5), ListedEndpoints::Ignore);

        // v: 2.0 -> Listed
        let v2_0 = br#"{"v":2.0,"endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(
            parse_listed_endpoints(v2_0),
            ListedEndpoints::Listed(vec![EndpointAddr {
                host: "192.0.2.1".into(),
                port: 7657,
            }])
        );

        // v: 2.5 -> Listed
        let v2_5 = br#"{"v":2.5,"endpoints":[{"ip":"192.0.2.1","port":7657,"scope":"lan"}]}"#;
        assert_eq!(
            parse_listed_endpoints(v2_5),
            ListedEndpoints::Listed(vec![EndpointAddr {
                host: "192.0.2.1".into(),
                port: 7657,
            }])
        );

        // v >= 2 with endpoints not an array -> Unusable
        let not_array = br#"{"v":2,"endpoints":"invalid"}"#;
        assert_eq!(parse_listed_endpoints(not_array), ListedEndpoints::Unusable);

        // Non-object body -> Unusable
        assert_eq!(parse_listed_endpoints(b"[]"), ListedEndpoints::Unusable);
        assert_eq!(
            parse_listed_endpoints(b"not json"),
            ListedEndpoints::Unusable
        );
    }

    #[test]
    fn test_parse_listed_endpoints_validation() {
        // Skip bad ip, port 0, non-integer port; missing scope parses; ttl_s and generated_at ignored
        let json = serde_json::json!({
            "v": 2,
            "endpoints": [
                {"ip": "invalid-ip", "port": 7657, "scope": "lan"},
                {"ip": "192.0.2.1", "port": 0, "scope": "lan"},
                {"ip": "192.0.2.1", "port": "7657", "scope": "lan"},
                {"ip": "192.0.2.2", "port": 7657},
                {"ip": "192.0.2.3", "port": 7658, "scope": "lan", "ttl_s": 300, "generated_at": "2026-04-01T00:00:00Z"}
            ]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let parsed = parse_listed_endpoints(&bytes);
        assert_eq!(
            parsed,
            ListedEndpoints::Listed(vec![
                EndpointAddr {
                    host: "192.0.2.2".into(),
                    port: 7657,
                },
                EndpointAddr {
                    host: "192.0.2.3".into(),
                    port: 7658,
                },
            ])
        );
    }

    #[test]
    fn test_parse_listed_endpoints_ipv6_and_dedup() {
        // Bracketed IPv6 stores bare display; dedupe first-wins
        let json = serde_json::json!({
            "v": 2,
            "endpoints": [
                {"ip": "[2001:db8::1]", "port": 7657, "scope": "lan"},
                {"ip": "2001:db8::1", "port": 7657, "scope": "lan"},
                {"ip": "192.0.2.1", "port": 7657, "scope": "lan"},
                {"ip": "192.0.2.1", "port": 7657, "scope": "wan"}
            ]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let parsed = parse_listed_endpoints(&bytes);
        assert_eq!(
            parsed,
            ListedEndpoints::Listed(vec![
                EndpointAddr {
                    host: "2001:db8::1".into(),
                    port: 7657,
                },
                EndpointAddr {
                    host: "192.0.2.1".into(),
                    port: 7657,
                },
            ])
        );

        // All entries skipped -> Ignore
        let all_skipped = serde_json::json!({
            "v": 2,
            "endpoints": [
                {"ip": "invalid-ip", "port": 7657}
            ]
        });
        assert_eq!(
            parse_listed_endpoints(&serde_json::to_vec(&all_skipped).unwrap()),
            ListedEndpoints::Ignore
        );
    }

    #[test]
    fn test_merge_dial_endpoints_proven_relay() {
        let stored = vec![EndpointAddr {
            host: "127.0.0.1".into(),
            port: 1,
        }];
        let listed = vec![
            EndpointAddr {
                host: "10.0.0.1".into(),
                port: 2,
            },
            EndpointAddr {
                host: "10.0.0.2".into(),
                port: 3,
            },
        ];
        assert_eq!(
            merge_dial_endpoints(&stored, &listed, EndpointPath::ProvenRelay),
            listed
        );
    }

    #[test]
    fn test_merge_dial_endpoints_unknown_walk() {
        let o1 = EndpointAddr {
            host: "10.0.0.1".into(),
            port: 1,
        };
        let n1 = EndpointAddr {
            host: "10.0.0.2".into(),
            port: 2,
        };
        let n2 = EndpointAddr {
            host: "10.0.0.3".into(),
            port: 3,
        };
        let n3 = EndpointAddr {
            host: "10.0.0.4".into(),
            port: 4,
        };
        let n4 = EndpointAddr {
            host: "10.0.0.5".into(),
            port: 5,
        };

        // stored [O1], list [N1, N2] -> [N1, N2, O1]
        let walk1 = merge_dial_endpoints(
            std::slice::from_ref(&o1),
            &[n1.clone(), n2.clone()],
            EndpointPath::Unknown,
        );
        assert_eq!(walk1, vec![n1.clone(), n2.clone(), o1.clone()]);

        // same list again -> [N1, N2, O1]
        let walk2 = merge_dial_endpoints(&walk1, &[n1.clone(), n2.clone()], EndpointPath::Unknown);
        assert_eq!(walk2, vec![n1.clone(), n2.clone(), o1.clone()]);

        // stored [N1, N2, O1], list [N3, N4] -> [N3, N4, N1, N2]
        let walk3 = merge_dial_endpoints(&walk2, &[n3.clone(), n4.clone()], EndpointPath::Unknown);
        assert_eq!(walk3, vec![n3, n4, n1, n2]);
    }

    fn mixed_credential(
        peer: &crate::private_link_test_peer::PrivateLinkPeer,
    ) -> spl_transport::credential::Credential {
        let mut cred = peer.credential();
        cred.endpoints.push(EndpointAddr {
            host: "192.0.2.9".into(),
            port: 7657,
        });
        cred.local_endpoints = Some(serde_json::json!([{"ip": "10.0.0.1", "port": 7657}]));
        cred
    }

    type SetupCase = Box<dyn Fn(&crate::private_link_test_peer::PrivateLinkPeer) + Send + Sync>;

    #[tokio::test]
    async fn test_session_local_endpoints_http_errors_and_invalid_bodies() {
        let not_configured = serde_json::to_vec(&serde_json::json!({
            "protocol_version": 2,
            "status": "not_configured"
        }))
        .unwrap();

        let cases: Vec<SetupCase> = vec![
            Box::new(|peer| {
                peer.set_route_headers(
                    "/app/network/local-endpoints",
                    500,
                    vec![("content-type".into(), "text/plain".into())],
                    b"internal error".to_vec(),
                );
            }),
            Box::new(|peer| {
                peer.set_route("/app/network/local-endpoints", 302, Vec::new());
            }),
            Box::new(|peer| {
                peer.set_route("/app/network/local-endpoints", 404, Vec::new());
            }),
            Box::new(|peer| {
                peer.set_route("/app/network/local-endpoints", 200, b"not json".to_vec());
            }),
            Box::new(|peer| {
                peer.set_route("/app/network/local-endpoints", 200, b"[]".to_vec());
            }),
        ];

        for setup_case in cases {
            let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
            peer.set_route("/app/network/api/relay/access", 200, not_configured.clone());
            setup_case(&peer);

            let temp = tempfile::tempdir().unwrap();
            let cred = mixed_credential(&peer);
            crate::private_link::persist_credential(temp.path(), &cred).unwrap();
            let session = crate::private_link::start_private_link_session(
                temp.path(),
                cred.clone(),
                "stream",
            )
            .await
            .unwrap();
            let capability = session.capability();

            let cred_path = temp.path().join(crate::private_link::CREDENTIALS_FILENAME);
            let initial_bytes = std::fs::read(&cred_path).unwrap();
            let initial_endpoints = capability.writer().current_credential().endpoints;
            let initial_local_endpoints = capability.writer().current_credential().local_endpoints;

            let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;

            let get_count = peer
                .requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count();
            assert_eq!(get_count, 1);

            let current_bytes = std::fs::read(&cred_path).unwrap();
            assert_eq!(current_bytes, initial_bytes);
            assert_eq!(
                capability.writer().current_credential().endpoints,
                initial_endpoints
            );
            assert_eq!(
                capability.writer().current_credential().local_endpoints,
                initial_local_endpoints
            );

            session.shutdown().await.unwrap();
            peer.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_session_local_endpoints_version_gating() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let not_configured = serde_json::to_vec(&serde_json::json!({
            "protocol_version": 2,
            "status": "not_configured"
        }))
        .unwrap();
        peer.set_route("/app/network/api/relay/access", 200, not_configured);

        let temp = tempfile::tempdir().unwrap();
        let cred = mixed_credential(&peer);
        crate::private_link::persist_credential(temp.path(), &cred).unwrap();
        let session =
            crate::private_link::start_private_link_session(temp.path(), cred.clone(), "stream")
                .await
                .unwrap();
        let capability = session.capability();
        let cred_path = temp.path().join(crate::private_link::CREDENTIALS_FILENAME);
        let initial_bytes = std::fs::read(&cred_path).unwrap();

        // 1. v: 1 with changed list: one GET, nothing changed
        let v1_json = serde_json::to_vec(&serde_json::json!({
            "v": 1,
            "endpoints": [{"ip": "192.0.2.10", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, v1_json);
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), initial_bytes);
        assert_eq!(
            capability.writer().current_credential().endpoints,
            cred.endpoints
        );

        // 2. missing v with changed list: one GET, nothing changed
        let missing_v_json = serde_json::to_vec(&serde_json::json!({
            "endpoints": [{"ip": "192.0.2.10", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, missing_v_json);
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            2
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), initial_bytes);
        assert_eq!(
            capability.writer().current_credential().endpoints,
            cred.endpoints
        );

        // 3. same list at v: 2 replaces the set
        let v2_json = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [{"ip": "192.0.2.10", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, v2_json);
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            3
        );
        assert_ne!(std::fs::read(&cred_path).unwrap(), initial_bytes);
        let expected = vec![
            EndpointAddr {
                host: "192.0.2.10".into(),
                port: 7657,
            },
            cred.endpoints[0].clone(),
            cred.endpoints[1].clone(),
        ];
        assert_eq!(capability.writer().current_credential().endpoints, expected);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_local_endpoints_loopback_vs_mixed_skip() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let not_configured = serde_json::to_vec(&serde_json::json!({
            "protocol_version": 2,
            "status": "not_configured"
        }))
        .unwrap();
        peer.set_route("/app/network/api/relay/access", 200, not_configured);

        // Loopback-only: zero GETs
        let temp1 = tempfile::tempdir().unwrap();
        let session1 = crate::private_link::start_private_link_session(
            temp1.path(),
            peer.credential(),
            "stream",
        )
        .await
        .unwrap();
        let _ = execute_relay_access_sync(&session1.capability(), Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            0
        );
        session1.shutdown().await.unwrap();

        // Mixed: one GET
        let temp2 = tempfile::tempdir().unwrap();
        let session2 = crate::private_link::start_private_link_session(
            temp2.path(),
            mixed_credential(&peer),
            "stream",
        )
        .await
        .unwrap();
        let _ = execute_relay_access_sync(&session2.capability(), Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );
        session2.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_local_endpoints_relay_outcomes() {
        let endpoints_v2 = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [{"ip": "192.0.2.20", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();

        // 4 relay variants: 404, 500, not-configured, ready token equal to stored token
        for outcome in ["404", "500", "not_configured", "ready_equal"] {
            let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
            peer.set_route("/app/network/local-endpoints", 200, endpoints_v2.clone());

            let mut cred = mixed_credential(&peer);
            match outcome {
                "404" => {
                    peer.set_route("/app/network/api/relay/access", 404, Vec::new());
                }
                "500" => {
                    peer.set_route("/app/network/api/relay/access", 500, Vec::new());
                }
                "not_configured" => {
                    peer.set_route(
                        "/app/network/api/relay/access",
                        200,
                        serde_json::to_vec(&serde_json::json!({
                            "protocol_version": 2,
                            "status": "not_configured"
                        }))
                        .unwrap(),
                    );
                }
                "ready_equal" => {
                    let inst_id = peer.credential().instance_id;
                    let future_exp = chrono::Utc::now().timestamp() + 3600;
                    let expires_at_str = chrono::DateTime::from_timestamp(future_exp, 0)
                        .unwrap()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    let token = make_test_jwt(serde_json::json!({
                        "iss": "https://relay.example.com",
                        "sub": format!("instance:{inst_id}"),
                        "aud": "spl-relay",
                        "scope": "session.dial",
                        "ver": 2,
                        "instance_id": inst_id,
                        "iat": future_exp - 7200,
                        "exp": future_exp,
                        "jti": "jwt-equal",
                    }));
                    cred.relay_origin = Some("https://relay.example.com".into());
                    cred.device_token = Some(token.clone());
                    cred.device_token_expires_at = Some(future_exp);
                    peer.set_route(
                        "/app/network/api/relay/access",
                        200,
                        serde_json::to_vec(&serde_json::json!({
                            "protocol_version": 2,
                            "status": "ready",
                            "relay_origin": "https://relay.example.com",
                            "instance_id": inst_id,
                            "device_token": token,
                            "expires_at": expires_at_str,
                        }))
                        .unwrap(),
                    );
                }
                _ => unreachable!(),
            }

            let temp = tempfile::tempdir().unwrap();
            let session =
                crate::private_link::start_private_link_session(temp.path(), cred, "stream")
                    .await
                    .unwrap();
            let capability = session.capability();

            let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;

            assert_eq!(
                peer.requests()
                    .iter()
                    .filter(|r| r.path == "/app/network/local-endpoints")
                    .count(),
                1
            );
            assert!(
                capability
                    .writer()
                    .current_credential()
                    .endpoints
                    .iter()
                    .any(|ep| ep.host == "192.0.2.20" && ep.port == 7657)
            );

            session.shutdown().await.unwrap();
            peer.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_session_local_endpoints_relay_ready_and_address_commit() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let inst_id = peer.credential().instance_id;
        let future_exp = chrono::Utc::now().timestamp() + 3600;
        let expires_at_str = chrono::DateTime::from_timestamp(future_exp, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let new_token = make_test_jwt(serde_json::json!({
            "iss": "https://relay.example.com",
            "sub": format!("instance:{inst_id}"),
            "aud": "spl-relay",
            "scope": "session.dial",
            "ver": 2,
            "instance_id": inst_id,
            "iat": future_exp - 7200,
            "exp": future_exp,
            "jti": "jwt-new-token",
        }));

        peer.set_route(
            "/app/network/api/relay/access",
            200,
            serde_json::to_vec(&serde_json::json!({
                "protocol_version": 2,
                "status": "ready",
                "relay_origin": "https://relay.example.com",
                "instance_id": inst_id,
                "device_token": new_token,
                "expires_at": expires_at_str,
            }))
            .unwrap(),
        );

        peer.set_route(
            "/app/network/local-endpoints",
            200,
            serde_json::to_vec(&serde_json::json!({
                "v": 2,
                "endpoints": [{"ip": "192.0.2.30", "port": 7657, "scope": "lan"}]
            }))
            .unwrap(),
        );

        let temp = tempfile::tempdir().unwrap();
        let session = crate::private_link::start_private_link_session(
            temp.path(),
            mixed_credential(&peer),
            "stream",
        )
        .await
        .unwrap();
        let capability = session.capability();

        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;

        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );

        let disk_cred = crate::private_link::load_credential(temp.path())
            .unwrap()
            .unwrap();
        let mem_cred = capability.writer().current_credential();

        assert_eq!(disk_cred.device_token.as_deref(), Some(new_token.as_str()));
        assert_eq!(mem_cred.device_token.as_deref(), Some(new_token.as_str()));
        assert!(
            disk_cred
                .endpoints
                .iter()
                .any(|ep| ep.host == "192.0.2.30" && ep.port == 7657)
        );
        assert!(
            mem_cred
                .endpoints
                .iter()
                .any(|ep| ep.host == "192.0.2.30" && ep.port == 7657)
        );

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_local_endpoints_persisted_unknown_merge() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        peer.set_route(
            "/app/network/api/relay/access",
            200,
            serde_json::to_vec(&serde_json::json!({
                "protocol_version": 2,
                "status": "not_configured"
            }))
            .unwrap(),
        );

        let temp = tempfile::tempdir().unwrap();
        let cred = mixed_credential(&peer);
        let sentinel = cred.local_endpoints.clone();
        let peer_loopback = cred.endpoints[0].clone();
        let peer_mixed = cred.endpoints[1].clone();

        let session = crate::private_link::start_private_link_session(temp.path(), cred, "stream")
            .await
            .unwrap();
        let capability = session.capability();
        let cred_path = temp.path().join(crate::private_link::CREDENTIALS_FILENAME);

        // Pass 1: v: 2 with two new addresses (one bracketed IPv6)
        let pass1_json = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [
                {"ip": "[2001:db8::1]", "port": 7657, "scope": "lan"},
                {"ip": "192.0.2.10", "port": 7657, "scope": "lan"}
            ]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, pass1_json.clone());
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );

        let ep_ipv6 = EndpointAddr {
            host: "2001:db8::1".into(),
            port: 7657,
        };
        let ep_10 = EndpointAddr {
            host: "192.0.2.10".into(),
            port: 7657,
        };
        let expected_pass1 = vec![
            ep_ipv6.clone(),
            ep_10.clone(),
            peer_loopback.clone(),
            peer_mixed.clone(),
        ];

        let disk_cred1 = crate::private_link::load_credential(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(disk_cred1.endpoints, expected_pass1);
        assert_eq!(
            capability.writer().current_credential().endpoints,
            expected_pass1
        );
        assert_eq!(disk_cred1.local_endpoints, sentinel);
        let pass1_bytes = std::fs::read(&cred_path).unwrap();

        // Pass 2: second identical refresh -> one more GET, endpoints unchanged, file bytes identical
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            2
        );
        let disk_cred2 = crate::private_link::load_credential(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(disk_cred2.endpoints, expected_pass1);
        assert_eq!(
            capability.writer().current_credential().endpoints,
            expected_pass1
        );
        assert_eq!(std::fs::read(&cred_path).unwrap(), pass1_bytes);
        assert_eq!(disk_cred2.local_endpoints, sentinel);

        // Pass 3: third list of two newer addresses -> list plus first two of prior vector, last is gone
        let pass3_json = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [
                {"ip": "192.0.2.11", "port": 7657, "scope": "lan"},
                {"ip": "192.0.2.12", "port": 7657, "scope": "lan"}
            ]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, pass3_json);
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            3
        );

        let ep_11 = EndpointAddr {
            host: "192.0.2.11".into(),
            port: 7657,
        };
        let ep_12 = EndpointAddr {
            host: "192.0.2.12".into(),
            port: 7657,
        };
        let expected_pass3 = vec![ep_11, ep_12, ep_ipv6, ep_10];

        let disk_cred3 = crate::private_link::load_credential(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(disk_cred3.endpoints, expected_pass3);
        assert_eq!(
            capability.writer().current_credential().endpoints,
            expected_pass3
        );
        assert_eq!(disk_cred3.local_endpoints, sentinel);

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_local_endpoints_unchanged_canonical_set() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        peer.set_route(
            "/app/network/api/relay/access",
            200,
            serde_json::to_vec(&serde_json::json!({
                "protocol_version": 2,
                "status": "not_configured"
            }))
            .unwrap(),
        );
        let endpoints_json = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [{"ip": "192.0.2.50", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, endpoints_json);

        let temp = tempfile::tempdir().unwrap();
        let session = crate::private_link::start_private_link_session(
            temp.path(),
            mixed_credential(&peer),
            "stream",
        )
        .await
        .unwrap();
        let capability = session.capability();

        // First refresh commits changed endpoints and replaces lan_transport
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );

        let cred_path = temp.path().join(crate::private_link::CREDENTIALS_FILENAME);
        let bytes_before = std::fs::read(&cred_path).unwrap();
        let lan_before = capability.opener().cloned_lan_transport().unwrap();

        // Second refresh with unchanged canonical set
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            2
        );
        let bytes_after = std::fs::read(&cred_path).unwrap();
        assert_eq!(bytes_before, bytes_after);

        let lan_after = capability.opener().cloned_lan_transport().unwrap();
        assert!(Arc::ptr_eq(&lan_before, &lan_after));

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }

    #[tokio::test]
    async fn test_session_local_endpoints_deadline() {
        let peer = crate::private_link_test_peer::PrivateLinkPeer::start().await;
        let temp = tempfile::tempdir().unwrap();
        let session = crate::private_link::start_private_link_session(
            temp.path(),
            mixed_credential(&peer),
            "stream",
        )
        .await
        .unwrap();
        let capability = session.capability();

        let gate = Arc::new(tokio::sync::Notify::new());
        peer.enqueue_gated_response(200, b"{}", gate.clone());

        // Call with 1s timeout: relay path times out, gate unnotified, zero address GETs
        let _ = execute_relay_access_sync(&capability, Duration::from_secs(1)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            0
        );

        // Fast relay 404, valid v: 2 endpoints, notify gate, normal timeout
        peer.set_route("/app/network/api/relay/access", 404, Vec::new());
        let new_ep_json = serde_json::to_vec(&serde_json::json!({
            "v": 2,
            "endpoints": [{"ip": "192.0.2.60", "port": 7657, "scope": "lan"}]
        }))
        .unwrap();
        peer.set_route("/app/network/local-endpoints", 200, new_ep_json);
        gate.notify_waiters();

        let _ = execute_relay_access_sync(&capability, Duration::from_secs(5)).await;
        assert_eq!(
            peer.requests()
                .iter()
                .filter(|r| r.path == "/app/network/local-endpoints")
                .count(),
            1
        );
        assert!(
            capability
                .writer()
                .current_credential()
                .endpoints
                .iter()
                .any(|ep| ep.host == "192.0.2.60" && ep.port == 7657)
        );

        session.shutdown().await.unwrap();
        peer.shutdown().await;
    }
}
