// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use native_browser_frame::*;

#[test]
fn test_predicates_boundaries() {
    // Handshake budget
    assert!(!handshake_expired(1000, 1499, 500));
    assert!(handshake_expired(1000, 1500, 500)); // pins now == start + budget
    assert!(handshake_expired(1000, 1501, 500));

    // State renewal interval
    assert!(!state_renewal_due(1000, 1499, 500));
    assert!(state_renewal_due(1000, 1500, 500)); // pins now == last + interval
    assert!(state_renewal_due(1000, 1501, 500));

    // may_renew_on_connection: live == presented && now - last < interval
    assert!(may_renew_on_connection(42, 42, 1000, 1499, 500));
    assert!(!may_renew_on_connection(42, 42, 1000, 1500, 500)); // pins now == last + interval as not renewable
    assert!(!may_renew_on_connection(42, 99, 1000, 1100, 500)); // mismatched token

    // Freshness
    assert!(freshness_value_allowed(0));
    assert!(freshness_value_allowed(15000));
    assert!(!freshness_value_allowed(15001));

    assert!(freshness_authorizes_skim(1000, 5000, 1000));
    assert!(freshness_authorizes_skim(1000, 5000, 5999));
    assert!(!freshness_authorizes_skim(1000, 5000, 6000));
    assert!(!freshness_authorizes_skim(1000, 0, 1000));
    assert!(!freshness_authorizes_skim(1000, 5000, 6001));
    assert!(!freshness_authorizes_skim(2000, 5000, 1000)); // now < issued

    assert!(!freshness_authorizes_deletion(1000, 5000, 2000)); // unconditionally false

    // Future tolerance
    assert!(!future_beyond_tolerance(1050, 1000, 60));
    assert!(future_beyond_tolerance(1061, 1000, 60));

    // Outbox age
    assert!(!queued_past_outbox_age(1000, 1099, 100));
    assert!(queued_past_outbox_age(1000, 1100, 100));

    // Accepted retention
    assert!(!accepted_past_min_retention(1000, 1099, 100));
    assert!(accepted_past_min_retention(1000, 1100, 100));

    // Connection token
    assert!(connection_token_matches(12345, 12345));
    assert!(!connection_token_matches(12345, 67890));

    // Capture permitted
    let state_ok = serde_json::json!({
        "type": "state",
        "capture": "permitted",
        "freshness_ms": 15000,
        "delivery": "delivered",
        "destination_generation": "g1",
        "period_id": "p1",
        "version": "1.0.0"
    });
    assert!(capture_is_permitted(&state_ok));

    let state_paused = serde_json::json!({
        "type": "state",
        "capture": "paused",
        "delivery": "delivered",
        "destination_generation": "g1",
        "version": "1.0.0"
    });
    assert!(!capture_is_permitted(&state_paused));
}

#[test]
fn capture_predicate_requires_a_valid_positive_lease_state() {
    let state = serde_json::json!({"type":"state","capture":"permitted","delivery":"kept_locally", "failure":"queue_full", "freshness_ms":1000, "destination_generation":"g", "period_id":"p"});
    assert!(capture_is_permitted(&state));
    for (field, value) in [
        ("freshness_ms", serde_json::json!(0)),
        ("freshness_ms", serde_json::json!(15001)),
        ("delivery", serde_json::json!("invented")),
        ("failure", serde_json::json!("invented")),
        ("type", serde_json::json!("batch")),
        ("period_id", serde_json::Value::Null),
    ] {
        let mut bad = state.clone();
        bad[field] = value;
        assert!(!capture_is_permitted(&bad));
    }
    let mut bad = state.clone();
    bad.as_object_mut().unwrap().remove("type");
    assert!(!capture_is_permitted(&bad));
    let mut bad = state;
    bad.as_object_mut().unwrap().remove("freshness_ms");
    assert!(!capture_is_permitted(&bad));
    assert!(!may_renew_on_connection(1, 1, 1000, 999, 500));
    assert!(!freshness_authorizes_skim(1000, 1, 999));
}


#[test]
fn custody_snapshots_preserve_concurrent_facts_and_gate_only_full_custody() {
    for kind in ["hello_ack", "state"] {
        let base = serde_json::json!({"type":kind,"capture":"permitted","delivery":"failed","failure":"relay_unavailable","destination_generation":"g","period_id":"p","freshness_ms":15000});
        for full in [false, true] {
            for stale in [false, true] {
                let mut state = base.clone();
                state["custody"] = serde_json::json!({"full":full,"stale":stale,"future_fact":"preserved"});
                let bytes = encode(&state).unwrap();
                match decode(&bytes, Direction::HostToExtension) {
                    DecodeOutcome::Accept(value) => assert_eq!(value, state),
                    outcome => panic!("custody snapshot refused: {outcome:?}"),
                }
                assert_eq!(capture_is_permitted(&state), !full);
            }
        }
        assert!(capture_is_permitted(&base));
        for custody in [serde_json::Value::Null, serde_json::json!([]), serde_json::json!({}), serde_json::json!({"full":false}), serde_json::json!({"stale":true}), serde_json::json!({"full":1,"stale":false}), serde_json::json!({"full":false,"stale":"true"})] {
            let mut state = base.clone();
            state["custody"] = custody;
            assert!(!capture_is_permitted(&state));
        }
    }
}
