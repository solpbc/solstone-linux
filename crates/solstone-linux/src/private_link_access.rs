// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
pub use spl_core::relay_access::RelayAccess;

use crate::private_link::{LinkOutcome, PrivateLinkCapability};

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
    tokio::time::timeout_at(deadline, async {
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
    .unwrap_or(Err(()))
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
}
