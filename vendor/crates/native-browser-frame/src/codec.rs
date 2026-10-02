// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::constants::*;
use crate::frame::Direction;
use serde_json::Value;

pub const ENVELOPE_SCHEMA_STR: &str =
    include_str!("../../../contracts/native-browser/envelope.schema.json");
pub const JOURNAL_SCHEMA_STR: &str =
    include_str!("../../../contracts/native-browser/browser.schema.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeOutcome {
    Accept(Value),
    Unsupported {
        protocol: u64,
        version: String,
        behind: String,
    },
    Refuse(DecodeError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    pub code: String,
    pub class: Option<String>,
    pub row: Option<usize>,
    pub field: Option<String>,
    pub cause: Option<String>,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code)?;
        if let Some(ref cause) = self.cause {
            write!(f, " cause: {}", cause)?;
        }
        if let Some(row) = self.row {
            write!(f, " row: {}", row)?;
        }
        if let Some(ref field) = self.field {
            write!(f, " field: {}", field)?;
        }
        Ok(())
    }
}

impl std::error::Error for DecodeError {}

impl DecodeError {
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_string(),
            class: None,
            row: None,
            field: None,
            cause: None,
        }
    }

    pub fn with_record_error(code: &str, row: usize, field: &str, cause: &str) -> Self {
        Self {
            code: code.to_string(),
            class: None,
            row: Some(row),
            field: Some(field.to_string()),
            cause: Some(cause.to_string()),
        }
    }
}

use std::sync::OnceLock;

static VALIDATOR: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();

pub fn get_validator() -> Result<&'static jsonschema::Validator, DecodeError> {
    let res = VALIDATOR.get_or_init(|| {
        let journal_value: Value = serde_json::from_str(JOURNAL_SCHEMA_STR)
            .map_err(|e| format!("journal schema parse error: {}", e))?;
        let envelope_value: Value = serde_json::from_str(ENVELOPE_SCHEMA_STR)
            .map_err(|e| format!("envelope schema parse error: {}", e))?;
        let registry = jsonschema::Registry::new()
            .add("solstone-journal-format:browser-jsonl", journal_value)
            .map_err(|e| format!("registry add error: {}", e))?
            .prepare()
            .map_err(|e| format!("registry prepare error: {}", e))?;
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .offline()
            .with_registry(&registry)
            .build(&envelope_value)
            .map_err(|e| format!("validator build error: {}", e))?;
        Ok(validator)
    });
    match res {
        Ok(v) => Ok(v),
        Err(_) => Err(DecodeError::new("bad_json")),
    }
}

/// JSON Schema integers may arrive as integral decimal/exponent values.
pub fn nonnegative_integer(value: &Value) -> Option<u64> {
    if let Some(number) = value.as_u64() {
        return (number <= TIMESTAMP_MAX).then_some(number);
    }
    let number = value.as_f64()?;
    (number.is_finite() && number >= 0.0 && number <= TIMESTAMP_MAX as f64 && number.fract() == 0.0)
        .then_some(number as u64)
}

static RECORD_VALIDATOR: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();
fn record_validator() -> Result<&'static jsonschema::Validator, DecodeError> {
    RECORD_VALIDATOR
        .get_or_init(|| {
            let schema: Value =
                serde_json::from_str(JOURNAL_SCHEMA_STR).map_err(|_| "schema".to_string())?;
            jsonschema::options()
                .with_draft(jsonschema::Draft::Draft202012)
                .offline()
                .build(&schema)
                .map_err(|_| "schema".to_string())
        })
        .as_ref()
        .map_err(|_| DecodeError::new("bad_json"))
}

fn block_id_error(records: &[Value]) -> Option<DecodeError> {
    for (row, record) in records.iter().enumerate() {
        let blocks: Vec<&Value> =
            if record.get("t").and_then(Value::as_str) == Some("segment_start") {
                record
                    .get("blocks")
                    .and_then(Value::as_array)
                    .map(|blocks| blocks.iter().collect())
                    .unwrap_or_default()
            } else {
                record.get("block").into_iter().collect()
            };
        for block in blocks {
            let cause = match block.get("id") {
                None => Some("missing"),
                Some(Value::String(id)) if id.is_empty() => Some("empty"),
                Some(Value::String(id)) if id.chars().count() > ID_STRING_MAX => Some("too_long"),
                Some(Value::String(_)) => None,
                Some(_) => Some("type"),
            };
            if let Some(cause) = cause {
                return Some(DecodeError::with_record_error(
                    "bad_record",
                    row,
                    "id",
                    cause,
                ));
            }
        }
    }
    None
}

fn state_ids_allowed(value: &Value) -> bool {
    let generation = value.get("destination_generation").filter(|v| !v.is_null());
    let period = value.get("period_id").filter(|v| !v.is_null());
    let nonempty = |v: Option<&Value>| v.and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    match value.get("capture").and_then(Value::as_str) {
        Some("unavailable" | "not_paired") => generation.is_none() && period.is_none(),
        Some("permitted") => nonempty(generation) && nonempty(period),
        Some("paused" | "intake_off") => {
            nonempty(generation) && (period.is_none() || nonempty(period))
        }
        _ => false,
    }
}

// Map only bounded machine fields; never stringify a schema error or input value.
fn schema_error(value: &Value, message_type: &str) -> DecodeError {
    if message_type == "accepted" {
        return DecodeError::new("invalid_receipt");
    }
    static ENVELOPE: OnceLock<Value> = OnceLock::new();
    let schema = ENVELOPE
        .get_or_init(|| serde_json::from_str(ENVELOPE_SCHEMA_STR).expect("embedded schema"));
    if schema["$defs"][message_type]["required"]
        .as_array()
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|f| value.get(f.as_str().unwrap()).is_none())
        })
    {
        return DecodeError::new("missing_field");
    }
    for field in ["protocol", "queued_at_ms"] {
        if value
            .get(field)
            .is_some_and(|v| nonnegative_integer(v).is_none())
        {
            return DecodeError::new("bad_number");
        }
    }
    if matches!(message_type, "state" | "hello_ack") {
        if value
            .get("freshness_ms")
            .is_some_and(|v| nonnegative_integer(v).is_none_or(|v| v > FRESHNESS_MS_MAX))
        {
            return DecodeError::new("freshness_range");
        }
        for (field, allowed) in [
            ("capture", CAPTURE_ENUM),
            ("delivery", DELIVERY_ENUM),
            ("failure", FAILURE_ENUM),
        ] {
            if value
                .get(field)
                .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !allowed.contains(&s)))
            {
                return DecodeError::new("invalid_enum");
            }
        }
        if !state_ids_allowed(value) {
            return DecodeError::new("bad_state_ids");
        }
    }
    for (field, allowed) in [
        ("brand", BRAND_ENUM),
        ("behind", BEHIND_ENUM),
        ("reason", BYE_REASON_ENUM),
    ] {
        if value
            .get(field)
            .is_some_and(|v| v.as_str().is_none_or(|s| !allowed.contains(&s)))
        {
            return DecodeError::new("invalid_enum");
        }
    }
    if message_type == "batch" {
        if value
            .get("batch_id")
            .and_then(Value::as_str)
            .is_none_or(|id| {
                id.len() != BATCH_ID_HEX_LEN
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            return DecodeError::new("bad_batch_id");
        }
        if let Some(records) = value.get("records").and_then(Value::as_array) {
            if records.len() > DELTA_RECORDS_MAX {
                return DecodeError::new("too_many_deltas");
            }
            if records.is_empty() {
                return DecodeError::new("bad_record");
            }
            if record_validator()
                .is_ok_and(|validator| records.iter().any(|record| !validator.is_valid(record)))
            {
                return block_id_error(records).unwrap_or_else(|| DecodeError::new("bad_record"));
            }
        } else {
            return DecodeError::new("bad_record");
        }
    }
    DecodeError::new("missing_field")
}

// --- Canonical JSON Encoder ---

fn escape_canonical_string(s: &str, out: &mut String) -> Result<(), &'static str> {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x00'..='\x1f' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

fn depth_allowed(value: &Value) -> bool {
    let mut pending = vec![(value, 0)];
    while let Some((value, parent_depth)) = pending.pop() {
        match value {
            Value::Array(items) => {
                if parent_depth >= JSON_MAX_DEPTH {
                    return false;
                }
                pending.extend(items.iter().map(|item| (item, parent_depth + 1)));
            }
            Value::Object(items) => {
                if parent_depth >= JSON_MAX_DEPTH {
                    return false;
                }
                pending.extend(items.values().map(|item| (item, parent_depth + 1)));
            }
            _ => {}
        }
    }
    true
}

pub fn canonical_stringify(val: &Value, out: &mut String) -> Result<(), &'static str> {
    if !depth_allowed(val) {
        return Err("bad_json");
    }
    write_canonical(val, out)
}

fn write_canonical(val: &Value, out: &mut String) -> Result<(), &'static str> {
    match val {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape_canonical_string(s, out)?,
        Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            static KEY_ORDER: OnceLock<std::collections::BTreeMap<String, Vec<String>>> =
                OnceLock::new();
            let key_order = KEY_ORDER.get_or_init(|| {
                serde_json::from_str(CANONICAL_KEY_ORDER_JSON).expect("generated key order")
            });
            let known_type = map
                .get("type")
                .and_then(Value::as_str)
                .filter(|kind| key_order.contains_key(*kind));
            let record_type = map
                .get("t")
                .and_then(Value::as_str)
                .and_then(|kind| match kind {
                    "segment_start" => Some("snapshot_record"),
                    "delta" => Some("delta_record"),
                    _ => None,
                });
            let kind = known_type.or(record_type).or_else(|| {
                if map.contains_key("id") || map.contains_key("text") {
                    Some("block")
                } else if map.contains_key("label")
                    || map.contains_key("level")
                    || map.contains_key("linkHost")
                {
                    Some("block_attrs")
                } else {
                    None
                }
            });
            let known_order: Vec<&str> = kind
                .and_then(|kind| key_order.get(kind))
                .map(|keys| keys.iter().map(String::as_str).collect())
                .unwrap_or_default();

            let mut ordered_keys: Vec<&str> = Vec::new();
            for &k in &known_order {
                if map.contains_key(k) {
                    ordered_keys.push(k);
                }
            }

            let mut unknown_keys: Vec<&str> = map
                .keys()
                .map(|s| s.as_str())
                .filter(|k| !known_order.contains(k))
                .collect();
            unknown_keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));

            ordered_keys.extend(unknown_keys);

            out.push('{');
            for (i, &k) in ordered_keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                escape_canonical_string(k, out)?;
                out.push(':');
                write_canonical(&map[k], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

pub fn encode(val: &Value) -> Result<Vec<u8>, DecodeError> {
    let mut out = String::new();
    canonical_stringify(val, &mut out).map_err(|e| DecodeError::new(e))?;
    Ok(out.into_bytes())
}

// --- Decoder ---

pub fn decode(bytes: &[u8], direction: Direction) -> DecodeOutcome {
    if bytes.is_empty() {
        return DecodeOutcome::Refuse(DecodeError::new("empty_payload"));
    }
    if bytes.len() > direction.socket_cap() {
        return DecodeOutcome::Refuse(DecodeError::new("oversize"));
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return DecodeOutcome::Refuse(DecodeError::new("bad_utf8")),
    };
    if text.trim().is_empty() {
        return DecodeOutcome::Refuse(DecodeError::new("empty_payload"));
    }
    // Parse the actual payload first: never repair number lexemes or scan through escaped strings.
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            let description = error.to_string();
            let code = if description.starts_with("lone leading surrogate")
                || description.starts_with("unexpected end of hex escape")
            {
                "lone_surrogate"
            } else if description.starts_with("number out of range") {
                "bad_number"
            } else {
                "bad_json"
            };
            return DecodeOutcome::Refuse(DecodeError::new(code));
        }
    };
    if !depth_allowed(&value) {
        return DecodeOutcome::Refuse(DecodeError::new("bad_json"));
    }
    let Some(object) = value.as_object() else {
        return DecodeOutcome::Refuse(DecodeError::new("bad_json"));
    };
    let Some(message_type) = object.get("type").and_then(Value::as_str) else {
        return DecodeOutcome::Refuse(DecodeError::new("missing_field"));
    };
    let expected_direction = match message_type {
        "hello" | "batch" => Direction::ExtensionToHost,
        "hello_ack" | "unsupported" | "state" | "boundary" | "accepted" | "bye" => {
            Direction::HostToExtension
        }
        _ => return DecodeOutcome::Refuse(DecodeError::new("bad_type")),
    };
    if direction != expected_direction {
        return DecodeOutcome::Refuse(DecodeError::new("bad_direction"));
    }
    if message_type != "batch" && bytes.len() > CONTROL_MAX {
        return DecodeOutcome::Refuse(DecodeError::new("oversize"));
    }
    let validator = match get_validator() {
        Ok(v) => v,
        Err(error) => return DecodeOutcome::Refuse(error),
    };
    if !validator.is_valid(&value) {
        return DecodeOutcome::Refuse(schema_error(&value, message_type));
    }
    if message_type == "hello" {
        let Some(protocol) = value.get("protocol").and_then(nonnegative_integer) else {
            return DecodeOutcome::Refuse(DecodeError::new("bad_number"));
        };
        if protocol != u64::from(WIRE_PROTOCOL) {
            return DecodeOutcome::Unsupported {
                protocol,
                version: value["version"].as_str().unwrap().to_owned(),
                behind: if protocol > u64::from(WIRE_PROTOCOL) {
                    "app"
                } else {
                    "extension"
                }
                .to_owned(),
            };
        }
    }
    if message_type == "batch" {
        let records = value["records"].as_array().expect("validated records");
        let snapshot = records[0]["t"] == "segment_start";
        if snapshot && records.len() != 1 {
            return DecodeOutcome::Refuse(DecodeError::new("bad_record"));
        }
        for (row, record) in records.iter().enumerate() {
            let cause = match record.get("ctx").and_then(Value::as_str) {
                None => Some("missing"),
                Some("") => Some("empty"),
                Some(_) => None,
            };
            if let Some(cause) = cause {
                return DecodeOutcome::Refuse(DecodeError::with_record_error(
                    "bad_record",
                    row,
                    "ctx",
                    cause,
                ));
            }
        }
        let context = records[0]["ctx"]
            .as_str()
            .expect("validated native context");
        for (row, record) in records.iter().enumerate() {
            if record["t"] != if snapshot { "segment_start" } else { "delta" } {
                return DecodeOutcome::Refuse(DecodeError::new("bad_record"));
            }
            if record.get("ctx").and_then(Value::as_str) != Some(context) {
                return DecodeOutcome::Refuse(DecodeError::new("mixed_context"));
            }
            if record
                .get("inst")
                .is_some_and(|inst| inst != &value["inst"])
            {
                return DecodeOutcome::Refuse(DecodeError::with_record_error(
                    "bad_record",
                    row,
                    "inst",
                    "mismatch",
                ));
            }
            if record
                .get("snapshot_reason")
                .is_some_and(|reason| reason.as_str() != Some("delivery_recovery"))
            {
                return DecodeOutcome::Refuse(DecodeError::new("invalid_enum"));
            }
        }
        if let Some(error) = block_id_error(records) {
            return DecodeOutcome::Refuse(error);
        }
    }
    DecodeOutcome::Accept(value)
}

// --- Recipe Builder in Rust ---

pub fn build_recipe(recipe_id: &str) -> Result<Vec<u8>, &'static str> {
    let mut base_obj = match recipe_id {
        "extension_to_host_batch_max" | "extension_to_host_batch_oversize" => {
            serde_json::json!({
                "type": "batch",
                "destination_generation": "g",
                "inst": "i",
                "batch_id": "0123456789abcdef0123456789abcdef",
                "queued_at_ms": 0,
                "records": [{
                    "t": "segment_start",
                    "ts": 0,
                    "ctx": "c",
                    "blocks": [{"id": "b", "text": "x"}]
                }],
                "pad": ""
            })
        }
        "control_payload_max" | "control_payload_oversize" => {
            serde_json::json!({
                "type": "hello",
                "protocol": 1,
                "version": "1.0.0",
                "brand": "chrome",
                "inst": "inst1",
                "pad": ""
            })
        }
        "batch_delta_cap_3000" => {
            let mut deltas = Vec::new();
            for i in 0..1500 {
                deltas.push(serde_json::json!({"t": "delta", "ts": 0, "ctx": "c", "op": "add", "block": {"id": format!("a{}", i), "text": "t"}}));
                deltas.push(serde_json::json!({"t": "delta", "ts": 0, "ctx": "c", "op": "remove", "block": {"id": format!("r{}", i)}}));
            }
            let obj = serde_json::json!({
                "type": "batch",
                "destination_generation": "g",
                "inst": "i",
                "batch_id": "0123456789abcdef0123456789abcdef",
                "queued_at_ms": 0,
                "records": deltas
            });
            return encode(&obj).map_err(|_| "encode error");
        }
        "batch_delta_oversize_3001" => {
            let mut deltas = Vec::new();
            for i in 0..1500 {
                deltas.push(serde_json::json!({"t": "delta", "ts": 0, "ctx": "c", "op": "add", "block": {"id": format!("a{}", i), "text": "t"}}));
                deltas.push(serde_json::json!({"t": "delta", "ts": 0, "ctx": "c", "op": "remove", "block": {"id": format!("r{}", i)}}));
            }
            deltas.push(serde_json::json!({"t": "delta", "ts": 0, "ctx": "c", "op": "add", "block": {"id": "extra", "text": "t"}}));
            let obj = serde_json::json!({
                "type": "batch",
                "destination_generation": "g",
                "inst": "i",
                "batch_id": "0123456789abcdef0123456789abcdef",
                "queued_at_ms": 0,
                "records": deltas
            });
            return encode(&obj).map_err(|_| "encode error");
        }
        _ => return Err("unknown recipe"),
    };

    let target_len = match recipe_id {
        "extension_to_host_batch_max" => EXTENSION_TO_HOST_MAX,
        "extension_to_host_batch_oversize" => EXTENSION_TO_HOST_MAX + 1,
        "control_payload_max" => CONTROL_MAX,
        "control_payload_oversize" => CONTROL_MAX + 1,
        _ => return Err("unknown target length"),
    };

    let empty_bytes = encode(&base_obj).map_err(|_| "encode error")?;
    let pad_len = target_len - empty_bytes.len();
    base_obj["pad"] = Value::String("a".repeat(pad_len));

    let final_bytes = encode(&base_obj).map_err(|_| "encode error")?;
    if final_bytes.len() != target_len {
        return Err("recipe length mismatch");
    }

    Ok(final_bytes)
}

pub fn build_reply(receipt: &Value) -> Result<Value, &'static str> {
    let result = receipt
        .get("result")
        .or_else(|| receipt.get("outcome"))
        .and_then(Value::as_str)
        .unwrap_or(if receipt.get("reason").is_some() {
            "rejected"
        } else {
            ""
        });
    let result = match result {
        "accepted" if receipt.get("duplicate").and_then(Value::as_bool) == Some(true) => {
            "duplicate"
        }
        "accepted" | "duplicate" => result,
        "rejected" | "backpressure" | "loss" => "rejected",
        _ => return Err("invalid_receipt"),
    };
    let mut reply = serde_json::Map::new();
    reply.insert("type".to_owned(), Value::String("accepted".to_owned()));
    reply.insert("result".to_owned(), Value::String(result.to_owned()));
    for field in ["destination_generation", "inst", "batch_id"] {
        reply.insert(
            field.to_owned(),
            receipt.get(field).cloned().ok_or("invalid_receipt")?,
        );
    }
    if result == "rejected" {
        let reason = receipt
            .get("reason")
            .and_then(Value::as_str)
            .ok_or("invalid_receipt")?;
        let class = if RETRYABLE_REASONS.contains(&reason) {
            "retryable"
        } else if PERMANENT_REASONS.contains(&reason) {
            "permanent"
        } else {
            return Err("invalid_receipt");
        };
        if receipt
            .get("class")
            .is_some_and(|value| value.as_str() != Some(class))
        {
            return Err("invalid_receipt");
        }
        if receipt.get("period_id").is_some() {
            return Err("invalid_receipt");
        }
        reply.insert("reason".to_owned(), Value::String(reason.to_owned()));
        reply.insert("class".to_owned(), Value::String(class.to_owned()));
    } else {
        if receipt.get("reason").is_some() || receipt.get("class").is_some() {
            return Err("invalid_receipt");
        }
        reply.insert(
            "period_id".to_owned(),
            receipt.get("period_id").cloned().ok_or("invalid_receipt")?,
        );
    }
    let reply = Value::Object(reply);
    let encoded = encode(&reply).map_err(|_| "invalid_receipt")?;
    if !matches!(
        decode(&encoded, Direction::HostToExtension),
        DecodeOutcome::Accept(_)
    ) {
        return Err("invalid_receipt");
    }
    Ok(reply)
}
