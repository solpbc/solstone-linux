// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::private_link::{LinkOutcome, PrivateLinkCapability};
use reqwest::StatusCode;
use spl_transport::client::TransportClient;
use std::time::Duration;

fn decode_char(c: u8) -> Result<u8, ()> {
    match c {
        b'A'..=b'Z' => Ok(c - b'A'),
        b'a'..=b'z' => Ok(c - b'a' + 26),
        b'0'..=b'9' => Ok(c - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(()),
    }
}

pub(crate) fn base64url_decode_no_pad(input: &str) -> Result<Vec<u8>, ()> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity((bytes.len() * 3) / 4);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let b0 = decode_char(bytes[i])? as u32;
        let b1 = decode_char(bytes[i + 1])? as u32;
        let b2 = decode_char(bytes[i + 2])? as u32;
        let b3 = decode_char(bytes[i + 3])? as u32;
        let triple = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        output.push(((triple >> 16) & 0xff) as u8);
        output.push(((triple >> 8) & 0xff) as u8);
        output.push((triple & 0xff) as u8);
        i += 4;
    }
    match bytes.len() - i {
        0 => Ok(output),
        2 => {
            let b0 = decode_char(bytes[i])? as u32;
            let b1 = decode_char(bytes[i + 1])? as u32;
            let val = (b0 << 18) | (b1 << 12);
            output.push(((val >> 16) & 0xff) as u8);
            Ok(output)
        }
        3 => {
            let b0 = decode_char(bytes[i])? as u32;
            let b1 = decode_char(bytes[i + 1])? as u32;
            let b2 = decode_char(bytes[i + 2])? as u32;
            let val = (b0 << 18) | (b1 << 12) | (b2 << 6);
            output.push(((val >> 16) & 0xff) as u8);
            output.push(((val >> 8) & 0xff) as u8);
            Ok(output)
        }
        _ => Err(()),
    }
}

#[allow(dead_code)]
pub(crate) fn base64url_encode_no_pad(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let mut index = 0;
    while index + 3 <= input.len() {
        let chunk = ((input[index] as u32) << 16)
            | ((input[index + 1] as u32) << 8)
            | input[index + 2] as u32;
        output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        output.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        output.push(TABLE[(chunk & 0x3f) as usize] as char);
        index += 3;
    }
    match input.len() - index {
        1 => {
            let chunk = (input[index] as u32) << 16;
            output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
        }
        2 => {
            let chunk = ((input[index] as u32) << 16) | ((input[index + 1] as u32) << 8);
            output.push(TABLE[((chunk >> 18) & 0x3f) as usize] as char);
            output.push(TABLE[((chunk >> 12) & 0x3f) as usize] as char);
            output.push(TABLE[((chunk >> 6) & 0x3f) as usize] as char);
        }
        _ => {}
    }
    output
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct V2Claims {
    pub(crate) iss: String,
    pub(crate) sub: String,
    pub(crate) aud: String,
    pub(crate) scope: String,
    pub(crate) ver: u64,
    pub(crate) instance_id: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    pub(crate) jti: String,
}

pub(crate) fn decode_v2_jwt_claims(token: &str) -> Result<V2Claims, ()> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(());
    }
    let claims_bytes = base64url_decode_no_pad(parts[1]).map_err(|_| ())?;
    let val: serde_json::Value = serde_json::from_slice(&claims_bytes).map_err(|_| ())?;
    let obj = val.as_object().ok_or(())?;
    const REQUIRED_KEYS: &[&str] = &[
        "iss",
        "sub",
        "aud",
        "scope",
        "ver",
        "instance_id",
        "iat",
        "exp",
        "jti",
    ];
    // Reject extra keys including device_fp, ca_fp, predecessor
    if obj.len() != REQUIRED_KEYS.len() {
        return Err(());
    }
    for key in REQUIRED_KEYS {
        if !obj.contains_key(*key) {
            return Err(());
        }
    }
    let iss = obj
        .get("iss")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let sub = obj
        .get("sub")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let aud = obj
        .get("aud")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let scope = obj
        .get("scope")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let ver = obj.get("ver").and_then(|v| v.as_u64()).ok_or(())?;
    let instance_id = obj
        .get("instance_id")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();
    let iat = obj.get("iat").and_then(|v| v.as_i64()).ok_or(())?;
    let exp = obj.get("exp").and_then(|v| v.as_i64()).ok_or(())?;
    let jti = obj
        .get("jti")
        .and_then(|v| v.as_str())
        .ok_or(())?
        .to_string();

    Ok(V2Claims {
        iss,
        sub,
        aud,
        scope,
        ver,
        instance_id,
        iat,
        exp,
        jti,
    })
}

#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct RelayAccessReady {
    pub(crate) protocol_version: u32,
    pub(crate) status: String,
    pub(crate) relay_origin: String,
    pub(crate) instance_id: String,
    pub(crate) device_token: String,
    pub(crate) expires_at: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct RelayAccessNotConfigured {
    pub(crate) protocol_version: u32,
    pub(crate) status: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum RelayAccessWireResponse {
    Ready(RelayAccessReady),
    NotConfigured(RelayAccessNotConfigured),
}

pub(crate) fn parse_relay_access_response(body: &[u8]) -> Result<RelayAccessWireResponse, ()> {
    let val: serde_json::Value = serde_json::from_slice(body).map_err(|_| ())?;
    let obj = val.as_object().ok_or(())?;
    let protocol_version = obj
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .ok_or(())?;
    if protocol_version != 2 {
        return Err(());
    }
    let status = obj.get("status").and_then(|v| v.as_str()).ok_or(())?;
    match status {
        "not_configured" => {
            if obj.len() != 2 {
                return Err(());
            }
            Ok(RelayAccessWireResponse::NotConfigured(
                RelayAccessNotConfigured {
                    protocol_version: 2,
                    status: "not_configured".to_string(),
                },
            ))
        }
        "ready" => {
            let ready: RelayAccessReady = serde_json::from_value(val).map_err(|_| ())?;
            Ok(RelayAccessWireResponse::Ready(ready))
        }
        _ => Err(()),
    }
}

pub(crate) fn validate_relay_access_ready(
    ready: &RelayAccessReady,
    paired_instance_id: &str,
    current_time: i64,
) -> Result<V2Claims, ()> {
    if ready.protocol_version != 2 || ready.status != "ready" {
        return Err(());
    }
    if ready.instance_id != paired_instance_id {
        return Err(());
    }
    let claims = decode_v2_jwt_claims(&ready.device_token)?;
    if claims.ver != 2 {
        return Err(());
    }
    if claims.aud != "spl-relay" {
        return Err(());
    }
    if claims.scope != "session.dial" {
        return Err(());
    }
    let expected_sub = format!("instance:{paired_instance_id}");
    if claims.sub != expected_sub {
        return Err(());
    }
    if claims.instance_id != paired_instance_id {
        return Err(());
    }
    if claims.iss.is_empty() {
        return Err(());
    }
    if claims.exp <= claims.iat {
        return Err(());
    }
    if claims.exp <= current_time {
        return Err(());
    }
    let parsed_dt = chrono::DateTime::parse_from_rfc3339(&ready.expires_at).map_err(|_| ())?;
    if parsed_dt.timestamp() != claims.exp {
        return Err(());
    }
    if spl_core::relay::dial_url(&ready.relay_origin, paired_instance_id).is_err() {
        return Err(());
    }
    Ok(claims)
}

pub(crate) async fn execute_relay_access_sync(
    capability: &PrivateLinkCapability,
    timeout: Duration,
) -> Result<(), ()> {
    let outcome = capability.relay_access_get(timeout).await;
    match outcome {
        LinkOutcome::Success {
            status: StatusCode::OK,
            body,
        } => {
            let wire = parse_relay_access_response(&body)?;
            let writer = capability.writer();
            let pairing_id = writer.pairing_id().to_string();
            let access_gen = writer.access_mutation_generation();
            match wire {
                RelayAccessWireResponse::NotConfigured(_) => {
                    // 1. Immediately disable live relay transport
                    capability.clear_relay_transport();
                    // 2. Commit ordered durable clear
                    let _ = writer.commit_optional_clear(&pairing_id, access_gen);
                    Ok(())
                }
                RelayAccessWireResponse::Ready(ready) => {
                    let cred = writer.current_credential();
                    let now = chrono::Utc::now().timestamp();
                    let claims = validate_relay_access_ready(&ready, &cred.instance_id, now)?;
                    let mut test_cred = cred.clone();
                    test_cred.relay_origin = Some(ready.relay_origin.clone());
                    test_cred.device_token = Some(ready.device_token.clone());
                    test_cred.device_token_expires_at = Some(claims.exp);
                    test_cred.endpoints.clear();
                    test_cred.local_endpoints = None;

                    // Ensure transport client can construct
                    if TransportClient::new_relay_only(test_cred.clone(), None).is_err() {
                        return Err(());
                    }

                    // Commit ordered relay credentials
                    if writer
                        .commit_optional_relay(
                            &pairing_id,
                            access_gen,
                            &ready.relay_origin,
                            &ready.device_token,
                            claims.exp,
                        )
                        .is_ok()
                    {
                        let new_hook = writer.create_token_hook();
                        if let Ok(new_client) =
                            TransportClient::new_relay_only(test_cred, Some(new_hook))
                        {
                            capability.replace_relay_transport(new_client);
                        }
                        Ok(())
                    } else {
                        Err(())
                    }
                }
            }
        }
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_jwt(claims_json: serde_json::Value) -> String {
        let header = base64url_encode_no_pad(b"{\"alg\":\"none\",\"typ\":\"JWT\"}");
        let claims = base64url_encode_no_pad(serde_json::to_vec(&claims_json).unwrap().as_slice());
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
        let decoded = decode_v2_jwt_claims(&token).expect("valid claims");
        assert_eq!(decoded.iss, "https://relay.example.com");
        assert_eq!(decoded.ver, 2);
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
        assert!(decode_v2_jwt_claims(&token_extra).is_err());
    }

    #[test]
    fn test_parse_relay_access_response() {
        let not_configured = br#"{"protocol_version": 2, "status": "not_configured"}"#;
        let parsed = parse_relay_access_response(not_configured).expect("parse not_configured");
        assert!(matches!(parsed, RelayAccessWireResponse::NotConfigured(_)));

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
    fn test_base64url_roundtrip() {
        let cases = [
            "",
            "f",
            "fo",
            "foo",
            "foob",
            "fooba",
            "foobar",
            "Hello, world! 1234567890-_",
        ];
        for case in cases {
            let encoded = base64url_encode_no_pad(case.as_bytes());
            assert!(!encoded.contains('='));
            assert!(!encoded.contains('+'));
            assert!(!encoded.contains('/'));
            let decoded = base64url_decode_no_pad(&encoded).expect("decode");
            assert_eq!(String::from_utf8(decoded).unwrap(), case);
        }

        // Invalid characters
        assert!(base64url_decode_no_pad("abc+").is_err());
        assert!(base64url_decode_no_pad("abc/").is_err());
        assert!(base64url_decode_no_pad("abc=").is_err());
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
        let ready = RelayAccessReady {
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
        let ready_mismatched_dt = RelayAccessReady {
            expires_at: "1970-01-01T00:33:21Z".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_mismatched_dt, "inst-123", 1500).is_err());

        // Fail when relay origin is invalid
        let ready_bad_origin = RelayAccessReady {
            relay_origin: "not a valid url".to_string(),
            ..ready.clone()
        };
        assert!(validate_relay_access_ready(&ready_bad_origin, "inst-123", 1500).is_err());
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
