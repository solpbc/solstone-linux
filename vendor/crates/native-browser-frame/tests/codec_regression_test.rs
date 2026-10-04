// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use native_browser_frame::{codec::nonnegative_integer, *};
use serde_json::{json, Value};

fn batch() -> Value {
    json!({"type":"batch","destination_generation":"g","inst":"i","batch_id":"0123456789abcdef0123456789abcdef","queued_at_ms":0,
        "records":[{"t":"segment_start","ts":0,"ctx":"c","blocks":[{"id":"b","text":"text"}]}]})
}
fn decode_value(value: &Value, direction: Direction) -> DecodeOutcome {
    decode(&serde_json::to_vec(value).unwrap(), direction)
}
fn refused(value: &Value) {
    assert!(
        matches!(
            decode_value(value, Direction::ExtensionToHost),
            DecodeOutcome::Refuse(_)
        ),
        "invalid batch accepted"
    );
}
#[test]
fn canonical_record_and_envelope_schema_are_enforced() {
    assert!(matches!(
        decode_value(&batch(), Direction::ExtensionToHost),
        DecodeOutcome::Accept(_)
    ));
    let mut cases = Vec::new();
    let mut b = batch();
    b["records"][0].as_object_mut().unwrap().remove("ts");
    cases.push(b);
    for age in [
        Value::Null,
        json!("0"),
        json!({}),
        json!(-1),
        json!(9007199254740992_u64),
    ] {
        let mut b = batch();
        b["queued_at_ms"] = age;
        cases.push(b);
    }
    let mut b = batch();
    b["records"][0]["title"] = json!("t".repeat(8193));
    cases.push(b);
    let mut b = batch();
    b["records"][0]["blocks"][0]["text"] = json!("t".repeat(2002));
    cases.push(b);
    let mut b = batch();
    b["records"][0]["blocks"][0]
        .as_object_mut()
        .unwrap()
        .remove("text");
    cases.push(b);
    let mut b = batch();
    b["records"][0]["blocks"] = json!(vec![json!({"id":"b","text":"t"}); 1501]);
    cases.push(b);
    let mut b = batch();
    b["records"][0]["blocks"][0]["depth"] = json!(4097);
    cases.push(b);
    let mut b = batch();
    b["records"][0]["inst"] = json!(42);
    cases.push(b);
    let mut b = batch();
    b["records"][0]["ctx"] = json!({});
    cases.push(b);
    let mut b = batch();
    b["destination_generation"] = json!("g".repeat(129));
    cases.push(b);
    for case in &cases {
        refused(case);
    }
}
#[test]
fn strict_json_preserves_escaped_text_additive_values_and_numbers() {
    let mut b = batch();
    b["records"][0]["blocks"][0]["text"] = json!(r#"literal \ud800 \udfff and escaped "ts":-1"#);
    b["extra"] = json!({"ts":-1,"depth":1.25,"huge":1e20,"nested":{"queued_at_ms":-4}});
    let bytes = serde_json::to_vec(&b).unwrap();
    let DecodeOutcome::Accept(value) = decode(&bytes, Direction::ExtensionToHost) else {
        panic!("valid escaped/additive data refused")
    };
    assert_eq!(value, b);
    let roundtrip: Value = serde_json::from_slice(&encode(&value).unwrap()).unwrap();
    assert_eq!(roundtrip, b, "encoder changed valid numeric/additive data");
    let raw = serde_json::to_string(&batch()).unwrap();
    for lexeme in ["1.e3", "0.e3", "01", "+1"] {
        let bad = raw.replace("\"queued_at_ms\":0", &format!("\"queued_at_ms\":{lexeme}"));
        assert!(serde_json::from_str::<Value>(&bad).is_err());
        assert!(matches!(
            decode(bad.as_bytes(), Direction::ExtensionToHost),
            DecodeOutcome::Refuse(_)
        ));
    }
    for lexeme in ["-0", "-0.0", "1000.0", "1e3", "9007199254740991.0"] {
        let good = raw.replace("\"queued_at_ms\":0", &format!("\"queued_at_ms\":{lexeme}"));
        let DecodeOutcome::Accept(value) = decode(good.as_bytes(), Direction::ExtensionToHost)
        else {
            panic!("integral number refused: {lexeme}")
        };
        assert!(nonnegative_integer(&value["queued_at_ms"]).is_some());
    }
    for field in ["text", "extra"] {
        let raw = if field == "text" {
            raw.replace("\"text\":\"text\"", "\"text\":\"\\uD800\"")
        } else {
            raw.replace(
                "\"type\":\"batch\"",
                "\"type\":\"batch\",\"extra\":\"\\uDFFF\"",
            )
        };
        assert!(matches!(
            decode(raw.as_bytes(), Direction::ExtensionToHost),
            DecodeOutcome::Refuse(_)
        ));
    }
}
#[test]
fn hello_limits_are_scalar_and_unsupported_still_checks_caps_and_direction() {
    let hello = json!({"type":"hello","protocol":1,"version":"é".repeat(64),"brand":"chrome","inst":"é".repeat(128)});
    assert!(matches!(
        decode_value(&hello, Direction::ExtensionToHost),
        DecodeOutcome::Accept(_)
    ));
    let mut future = hello.clone();
    future["protocol"] = json!(2.0);
    assert!(matches!(
        decode_value(&future, Direction::ExtensionToHost),
        DecodeOutcome::Unsupported { protocol: 2, .. }
    ));
    assert!(matches!(
        decode_value(&future, Direction::HostToExtension),
        DecodeOutcome::Refuse(_)
    ));
    future["pad"] = json!("x".repeat(CONTROL_MAX));
    assert!(matches!(
        decode_value(&future, Direction::ExtensionToHost),
        DecodeOutcome::Refuse(_)
    ));
    future.as_object_mut().unwrap().remove("pad");
    future["inst"] = json!("é".repeat(129));
    assert!(matches!(
        decode_value(&future, Direction::ExtensionToHost),
        DecodeOutcome::Refuse(_)
    ));
}
#[test]
fn native_context_presence_is_required_in_both_orders() {
    for absent in [true, false] {
        let mut b = batch();
        if absent {
            b["records"][0].as_object_mut().unwrap().remove("ctx");
        } else {
            b["records"][0]["ctx"] = json!("");
        }
        refused(&b);
    }
    let valid = json!({"t":"delta","ts":0,"ctx":"c","op":"remove","block":{"id":"b"}});
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("ctx");
    for records in [
        vec![missing.clone(), valid.clone()],
        vec![valid.clone(), missing],
    ] {
        let mut b = batch();
        b["records"] = json!(records);
        refused(&b);
    }
    let mut b = batch();
    let mut other = valid.clone();
    other["ctx"] = json!("different");
    b["records"] = json!([valid, other]);
    refused(&b);
}
#[test]
fn actual_receipt_builders_roundtrip_every_result_and_reason() {
    let identity = json!({"destination_generation":"g","inst":"i","batch_id":"0123456789abcdef0123456789abcdef"});
    for result in ["accepted", "duplicate"] {
        let mut r = identity.clone();
        r["result"] = json!(result);
        r["period_id"] = json!("original-period");
        let reply = build_reply(&r).unwrap();
        assert_eq!(reply["result"], result);
        assert!(matches!(
            decode(&encode(&reply).unwrap(), Direction::HostToExtension),
            DecodeOutcome::Accept(_)
        ));
    }
    for (reasons, class) in [
        (RETRYABLE_REASONS, "retryable"),
        (PERMANENT_REASONS, "permanent"),
    ] {
        for reason in reasons {
            let mut r = identity.clone();
            r["result"] = json!("rejected");
            r["reason"] = json!(reason);
            let reply = build_reply(&r).unwrap();
            assert_eq!(reply["class"], class);
            assert!(reply.get("period_id").is_none());
            assert!(matches!(
                decode(&encode(&reply).unwrap(), Direction::HostToExtension),
                DecodeOutcome::Accept(_)
            ));
            r["class"] = json!(if class == "retryable" {
                "permanent"
            } else {
                "retryable"
            });
            assert!(build_reply(&r).is_err());
        }
    }
    for reason in LEGACY_PERMANENT_REASONS {
        let mut permanent = json!({"type":"accepted","result":"rejected","reason":reason,"class":"permanent"});
        permanent["destination_generation"] = identity["destination_generation"].clone();
        permanent["inst"] = identity["inst"].clone();
        permanent["batch_id"] = identity["batch_id"].clone();
        assert!(matches!(
            decode(&encode(&permanent).unwrap(), Direction::HostToExtension),
            DecodeOutcome::Accept(_)
        ));
        let mut retryable = permanent.clone();
        retryable["class"] = json!("retryable");
        refused(&retryable);

        let mut request = identity.clone();
        request["result"] = json!("rejected");
        request["reason"] = json!(reason);
        request["class"] = json!("permanent");
        assert_eq!(build_reply(&request), Err("invalid_receipt"));
    }
    assert!(build_reply(&json!({"result":"accepted","period_id":"p"})).is_err());
    let mut rejected = identity;
    rejected["result"] = json!("rejected");
    rejected["reason"] = json!("snapshot_required");
    rejected["period_id"] = json!("p");
    assert!(build_reply(&rejected).is_err());
}

#[test]
fn nonfinite_json_number_is_refused_without_value_echo() {
    let raw =
        br#"{"type":"hello","protocol":1,"version":"v","brand":"chrome","inst":"i","extra":1e400}"#;
    let DecodeOutcome::Refuse(error) = decode(raw, Direction::ExtensionToHost) else {
        panic!("nonfinite additive number accepted")
    };
    assert_eq!(error.code, "bad_number");
    assert!(!error.to_string().contains("1e400"));
}

#[test]
fn depth_cap_counts_root_and_applies_to_decode_and_encode() {
    assert_eq!(JSON_MAX_DEPTH, 127);
    let nested = |count: usize| {
        let mut value = json!(0);
        for _ in 0..count {
            value = json!([value]);
        }
        value
    };
    let mut value = json!({"type":"hello","protocol":1,"version":"v","brand":"chrome","inst":"i"});
    value["extra"] = nested(JSON_MAX_DEPTH - 1);
    let bytes = encode(&value).expect("127 total container levels allowed");
    assert!(matches!(
        decode(&bytes, Direction::ExtensionToHost),
        DecodeOutcome::Accept(_)
    ));
    value["extra"] = nested(JSON_MAX_DEPTH);
    assert_eq!(encode(&value).unwrap_err().code, "bad_json");
    let raw = serde_json::to_vec(&value).unwrap();
    let DecodeOutcome::Refuse(error) = decode(&raw, Direction::ExtensionToHost) else {
        panic!("128 levels accepted")
    };
    assert_eq!(error.code, "bad_json");
    let mut output = String::new();
    assert!(canonical_stringify(&value, &mut output).is_err());
    assert!(output.is_empty());
}

#[test]
fn receipt_and_typed_block_key_order_matches_authority() {
    let accepted = json!({"type":"accepted","result":"accepted","destination_generation":"g","inst":"i","batch_id":"0123456789abcdef0123456789abcdef","period_id":"p"});
    assert_eq!(
        String::from_utf8(encode(&accepted).unwrap()).unwrap(),
        r#"{"type":"accepted","result":"accepted","destination_generation":"g","inst":"i","batch_id":"0123456789abcdef0123456789abcdef","period_id":"p"}"#
    );
    let block = json!({"type":"paragraph","id":"b","text":"t","depth":0,"attrs":{"level":"2","label":"L","linkHost":"example.invalid"}});
    assert_eq!(
        String::from_utf8(encode(&block).unwrap()).unwrap(),
        r#"{"id":"b","text":"t","type":"paragraph","depth":0,"attrs":{"label":"L","level":"2","linkHost":"example.invalid"}}"#
    );
    let unicode = json!({"\u{e000}":0,"\u{10000}":1});
    assert_eq!(
        String::from_utf8(encode(&unicode).unwrap()).unwrap(),
        "{\"\u{10000}\":1,\"\u{e000}\":0}"
    );
}
