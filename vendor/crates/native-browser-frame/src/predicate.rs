// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub fn handshake_expired(started_ms: u64, now_ms: u64, budget_ms: u64) -> bool {
    now_ms.saturating_sub(started_ms) >= budget_ms
}

pub fn state_renewal_due(last_ms: u64, now_ms: u64, interval_ms: u64) -> bool {
    now_ms.saturating_sub(last_ms) >= interval_ms
}

pub fn freshness_value_allowed(freshness_ms: u64) -> bool {
    freshness_ms <= crate::constants::FRESHNESS_MS_MAX
}

pub fn freshness_authorizes_skim(issued_ms: u64, freshness_ms: u64, now_ms: u64) -> bool {
    freshness_ms > 0
        && freshness_value_allowed(freshness_ms)
        && now_ms >= issued_ms
        && now_ms.saturating_sub(issued_ms) < freshness_ms
}

pub fn freshness_authorizes_deletion(_issued_ms: u64, _freshness_ms: u64, _now_ms: u64) -> bool {
    false
}

pub fn partial_frame_expired(first_byte_ms: u64, now_ms: u64, lifetime_ms: u64) -> bool {
    now_ms.saturating_sub(first_byte_ms) >= lifetime_ms
}

pub fn future_beyond_tolerance(observed_ms: u64, now_ms: u64, tolerance_ms: u64) -> bool {
    observed_ms.saturating_sub(now_ms) > tolerance_ms
}

pub fn queued_past_outbox_age(queued_at_ms: u64, now_ms: u64, max_age_ms: u64) -> bool {
    now_ms.saturating_sub(queued_at_ms) >= max_age_ms
}

pub fn accepted_past_min_retention(accepted_at_ms: u64, now_ms: u64, retention_ms: u64) -> bool {
    now_ms.saturating_sub(accepted_at_ms) >= retention_ms
}

pub fn connection_token_matches(live: u64, presented: u64) -> bool {
    live == presented
}

pub fn may_renew_on_connection(
    live: u64,
    presented: u64,
    last_ms: u64,
    now_ms: u64,
    interval_ms: u64,
) -> bool {
    live == presented && now_ms >= last_ms && now_ms.saturating_sub(last_ms) < interval_ms
}

pub fn capture_is_permitted(state: &serde_json::Value) -> bool {
    if !matches!(
        state.get("type").and_then(serde_json::Value::as_str),
        Some("state" | "hello_ack")
    ) || state.get("capture").and_then(serde_json::Value::as_str) != Some("permitted")
        || state
            .get("custody")
            .and_then(|custody| custody.get("full"))
            .and_then(serde_json::Value::as_bool) == Some(true)
        || state
            .get("freshness_ms")
            .and_then(crate::codec::nonnegative_integer)
            .is_none_or(|lease| lease == 0)
    {
        return false;
    }
    crate::codec::encode(state).is_ok_and(|bytes| {
        matches!(
            crate::codec::decode(&bytes, crate::frame::Direction::HostToExtension),
            crate::codec::DecodeOutcome::Accept(_)
        )
    })
}
