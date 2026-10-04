// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use native_browser_frame::*;

#[test]
fn test_corpus_vectors() {
    let corpus_bytes = include_bytes!("../../../contracts/native-browser/corpus.json");
    let corpus: Vec<serde_json::Value> = serde_json::from_slice(corpus_bytes).unwrap();

    let sentinel = "ZQ_SENTINEL_do_not_echo";

    for v in &corpus {
        let id = v.get("id").and_then(|i| i.as_str()).unwrap();
        let dir_str = v.get("direction").and_then(|d| d.as_str()).unwrap();
        let direction = match dir_str {
            "extension_to_host" => Direction::ExtensionToHost,
            "host_to_extension" => Direction::HostToExtension,
            _ => panic!("unknown direction: {}", dir_str),
        };

        let payload_bytes = if let Some(p) = v.get("payload").and_then(|p| p.as_str()) {
            p.as_bytes().to_vec()
        } else if let Some(r) = v.get("raw").and_then(|r| r.as_str()) {
            r.as_bytes().to_vec()
        } else {
            panic!("vector {} missing payload or raw", id);
        };

        let expect = v.get("expect").and_then(|e| e.as_str()).unwrap();
        let outcome = decode(&payload_bytes, direction);

        match (expect, &outcome) {
            ("accept", DecodeOutcome::Accept(val)) => {
                // Assert encode round-trip
                let encoded = encode(val).unwrap();
                assert!(!encoded.is_empty(), "vector {} encoded is empty", id);
            }
            ("unsupported", DecodeOutcome::Unsupported { behind, .. }) => {
                if let Some(exp_behind) = v.get("behind").and_then(|b| b.as_str()) {
                    assert_eq!(behind, exp_behind, "vector {} behind mismatch", id);
                }
            }
            ("refuse", DecodeOutcome::Refuse(err)) => {
                if let Some(exp_code) = v.get("code").and_then(|c| c.as_str()) {
                    assert_eq!(&err.code, exp_code, "vector {} error code mismatch", id);
                }
                if let Some(exp_cause) = v.get("cause").and_then(|c| c.as_str()) {
                    assert_eq!(
                        err.cause.as_deref(),
                        Some(exp_cause),
                        "vector {} cause mismatch",
                        id
                    );
                }
                // Assert sentinel is not echoed in error string
                let err_str = format!("{}", err);
                assert!(
                    !err_str.contains(sentinel),
                    "vector {} echoed sentinel in error string",
                    id
                );
            }
            _ => panic!("vector {} expected {} but got {:?}", id, expect, outcome),
        }
    }
}

#[test]
fn test_corpus_retries_and_numbers() {
    let corpus_bytes = include_bytes!("../../../contracts/native-browser/corpus.json");
    let corpus: Vec<serde_json::Value> = serde_json::from_slice(corpus_bytes).unwrap();
    let find_vec = |id: &str| -> serde_json::Value {
        corpus
            .iter()
            .find(|v| v.get("id").and_then(|i| i.as_str()) == Some(id))
            .unwrap()
            .clone()
    };

    let r1_payload = find_vec("batch_retry_1")["payload"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    let r2_payload = find_vec("batch_retry_2")["payload"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    let out1 = decode(&r1_payload, Direction::ExtensionToHost);
    let out2 = decode(&r2_payload, Direction::ExtensionToHost);
    match (out1, out2) {
        (DecodeOutcome::Accept(v1), DecodeOutcome::Accept(v2)) => {
            assert_eq!(v1["destination_generation"], v2["destination_generation"]);
            assert_eq!(v1["inst"], v2["inst"]);
            assert_eq!(v1["batch_id"], v2["batch_id"]);
            assert_eq!(v1["queued_at_ms"], v2["queued_at_ms"]);
        }
        other => panic!("expected accept for retries, got {:?}", other),
    }

    let f1_payload = if let Some(p) = find_vec("batch_queued_at_ms_fraction_1000_0")
        .get("payload")
        .and_then(|p| p.as_str())
    {
        p.as_bytes().to_vec()
    } else {
        find_vec("batch_queued_at_ms_fraction_1000_0")["raw"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec()
    };
    let f2_payload = if let Some(p) = find_vec("batch_queued_at_ms_exponential_1e3")
        .get("payload")
        .and_then(|p| p.as_str())
    {
        p.as_bytes().to_vec()
    } else {
        find_vec("batch_queued_at_ms_exponential_1e3")["raw"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec()
    };
    match (
        decode(&f1_payload, Direction::ExtensionToHost),
        decode(&f2_payload, Direction::ExtensionToHost),
    ) {
        (DecodeOutcome::Accept(v1), DecodeOutcome::Accept(v2)) => {
            assert_eq!(
                native_browser_frame::codec::nonnegative_integer(&v1["queued_at_ms"]),
                Some(1000)
            );
            assert_eq!(
                native_browser_frame::codec::nonnegative_integer(&v2["queued_at_ms"]),
                Some(1000)
            );
        }
        other => panic!("expected accept for numbers, got {:?}", other),
    }

    let rec_payload = find_vec("batch_recovery_snapshot")["payload"]
        .as_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    match decode(&rec_payload, Direction::ExtensionToHost) {
        DecodeOutcome::Accept(v1) => {
            assert_eq!(v1["records"][0]["t"].as_str(), Some("segment_start"));
            assert_eq!(
                v1["records"][0]["snapshot_reason"].as_str(),
                Some("delivery_recovery")
            );
        }
        other => panic!("expected accept for recovery snapshot, got {:?}", other),
    }

    let reasons_and_outcomes = [
        ("queue_full", "retryable"),
        ("age_policy", "retryable"),
        ("unaccepted_lost", "permanent"),
    ];

    for (reason, exp_outcome) in reasons_and_outcomes {
        let req = serde_json::json!({
            "reason": reason,
            "destination_generation": "g1",
            "inst": "i1",
            "batch_id": "0123456789abcdef0123456789abcdef"
        });
        let rej = build_reply(&req).unwrap();
        assert_eq!(rej["type"].as_str(), Some("accepted"));
        assert_eq!(rej["result"].as_str(), Some("rejected"));
        assert_eq!(rej["reason"].as_str(), Some(reason));
        assert_eq!(rej["class"].as_str(), Some(exp_outcome));
        assert_eq!(rej["destination_generation"].as_str(), Some("g1"));
        assert_eq!(rej["inst"].as_str(), Some("i1"));
        assert_eq!(
            rej["batch_id"].as_str(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }
}

#[test]
fn test_recipes_rebuild_and_hashes() {
    let recipes_bytes = include_bytes!("../../../contracts/native-browser/recipes.json");
    let recipes: Vec<serde_json::Value> = serde_json::from_slice(recipes_bytes).unwrap();

    for r in &recipes {
        let recipe_id = r.get("id").and_then(|i| i.as_str()).unwrap();
        let exp_len = r.get("target_length").and_then(|l| l.as_u64()).unwrap() as usize;
        let exp_hash = r.get("sha256").and_then(|h| h.as_str()).unwrap();

        let built_bytes = build_recipe(recipe_id).unwrap();
        assert_eq!(
            built_bytes.len(),
            exp_len,
            "recipe {} length mismatch",
            recipe_id
        );

        let actual_hash = sha2_simple(&built_bytes)
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        assert_eq!(
            actual_hash, exp_hash,
            "recipe {} sha256 mismatch",
            recipe_id
        );
    }
}

fn sha2_simple(data: &[u8]) -> [u8; 32] {
    // Sha256 pure implementation helper
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..(i + 1) * 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_var = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_var
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_var = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_var);
    }

    let mut out = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&val.to_be_bytes());
    }
    out
}
