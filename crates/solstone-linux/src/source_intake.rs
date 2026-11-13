// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! How much this computer has taken in, per source, since this counter existed.
//!
//! The file is `state/source-intake.json`, beside the pause file. It stays on
//! this computer: nothing copies it, and a new journal does not clear it.
//! Totals count finished material only. A file that is not version 1, or whose
//! totals are not whole numbers, is left byte for byte untouched.
//!
//! `started` is the local date counting began. `started_at` is that moment.
//! A terminal segment named for an earlier moment adds nothing, even when the
//! directory was renamed later. `last` moves only when that source's total grows.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Local, TimeZone};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::private_file::atomic_write_bytes;

pub const FILENAME: &str = "source-intake.json";
const LOCK_NAME: &str = "source-intake.lock";

pub(crate) fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILENAME)
}

/// Create the file when it is missing, before any source starts, so a segment
/// from before this moment is not counted. A file already there is not rewritten.
pub(crate) fn ensure(state_dir: &Path) {
    ensure_at(state_dir, &ClockStamp::now());
}

pub(crate) fn add_audio_seconds(state_dir: &Path, seconds: u64) {
    add_seconds_at(state_dir, "audio", seconds, &ClockStamp::now());
}

pub(crate) fn add_screen_seconds(state_dir: &Path, seconds: u64) {
    add_seconds_at(state_dir, "screen", seconds, &ClockStamp::now());
}

/// Whole seconds of one interleaved stereo buffer. The two channels are one
/// timeline: muted capture writes them as two files and still counts once.
pub(crate) fn audio_seconds(interleaved_len: usize, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    nearest_second(interleaved_len as f64 / 2.0 / f64::from(sample_rate))
}

pub(crate) fn nearest_second(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds < 0.5 {
        0
    } else {
        seconds.round() as u64
    }
}

/// A finished terminal segment. Activity with no terminal frames adds nothing.
/// A name with no length suffix adds nothing and does not fail the caller.
pub(crate) fn record_terminal_segment(state_dir: &Path, day: &str, segment: &str, dir: &Path) {
    record_terminal_segment_at(state_dir, day, segment, dir, &ClockStamp::now());
}

/// A screenshot taken in, by its identity. The same screenshot counts once.
pub(crate) fn add_screenshot(state_dir: &Path, id: &str) {
    add_screenshot_at(state_dir, id, &ClockStamp::now());
}

fn add_screenshot_at(state_dir: &Path, id: &str, stamp: &ClockStamp) {
    let _ = update(state_dir, stamp, |file| {
        let source = &mut file["screenshots"];
        let seen = source["seen"]
            .as_array()
            .is_some_and(|seen| seen.iter().any(|value| value.as_str() == Some(id)));
        if seen {
            return false;
        }
        let Some(total) = source.get("screenshots").and_then(Value::as_u64) else {
            return false;
        };
        source["screenshots"] = json!(total.saturating_add(1));
        source["last"] = json!(stamp.at.clone());
        if let Some(seen) = source["seen"].as_array_mut() {
            seen.push(json!(id));
        }
        true
    });
}

pub(crate) fn note_browser_batch(state_dir: &Path, batch: &Value, accepted: bool) {
    note_browser_batch_at(state_dir, batch, accepted, &ClockStamp::now());
}

pub(crate) fn attach_vault(engine: &mut solstone_vault::Engine, state_dir: PathBuf) {
    let paths = engine.known_note_paths();
    let intakes = engine.intakes();
    if keep_notes(
        &state_dir,
        Some(paths.as_slice()),
        &intakes,
        &ClockStamp::now(),
    ) {
        let through = intakes.iter().map(|intake| intake.id).max().unwrap_or(0);
        let _ = engine.ack_intakes(through);
    }
    let hooked = state_dir;
    engine.set_intake_hook(Arc::new(move |intakes| {
        keep_notes(&hooked, None, intakes, &ClockStamp::now())
    }));
}

/// Add each solstone-tmux data root's terminal total once. A missing or
/// unreadable file adds nothing and is not marked, so a later readable file
/// still counts. A root already marked adds nothing even if its total grew.
pub(crate) fn carry_tmux_roots(state_dir: &Path, roots: &[PathBuf]) {
    carry_tmux_roots_at(state_dir, roots, &ClockStamp::now());
}

fn ensure_at(state_dir: &Path, stamp: &ClockStamp) {
    let _ = update(state_dir, stamp, |_| false);
}

fn add_seconds_at(state_dir: &Path, source: &str, seconds: u64, stamp: &ClockStamp) {
    if seconds == 0 {
        let _ = update(state_dir, stamp, |_| false);
        return;
    }
    let _ = update(state_dir, stamp, |file| {
        let Some(total) = total_of(file, source) else {
            return false;
        };
        file[source]["seconds"] = json!(total.saturating_add(seconds));
        file[source]["last"] = json!(stamp.at.clone());
        true
    });
}

fn record_terminal_segment_at(
    state_dir: &Path,
    day: &str,
    segment: &str,
    dir: &Path,
    stamp: &ClockStamp,
) {
    if !directory_has_terminal_frames(dir) {
        return;
    }
    let Some(length) = length_suffix(segment) else {
        return;
    };
    let identity = format!("{day}/{segment}");
    let _ = update(state_dir, stamp, |file| {
        if seen_contains(file, &identity) {
            return false;
        }
        if before_cutoff(file, "terminal", day, segment) {
            return false;
        }
        let Some(total) = total_of(file, "terminal") else {
            return false;
        };
        file["terminal"]["seconds"] = json!(total.saturating_add(length));
        file["terminal"]["last"] = json!(stamp.at.clone());
        push_seen(file, identity);
        true
    });
}

fn note_browser_batch_at(state_dir: &Path, batch: &Value, accepted: bool, stamp: &ClockStamp) {
    if !accepted {
        return;
    }
    let Some((inst, ctx, url)) = accepted_page(batch) else {
        return;
    };
    let _ = update(state_dir, stamp, |file| {
        if tab_url(file, inst, ctx) == Some(url) {
            return false;
        }
        let Some(total) = total_of(file, "browsers") else {
            return false;
        };
        file["browsers"]["pages"] = json!(total.saturating_add(1));
        file["browsers"]["last"] = json!(stamp.at.clone());
        set_tab_url(file, inst, ctx, url);
        true
    });
}

fn keep_notes(
    state_dir: &Path,
    known_paths: Option<&[String]>,
    intakes: &[solstone_vault::Intake],
    stamp: &ClockStamp,
) -> bool {
    match update(state_dir, stamp, |file| {
        apply_notes(file, known_paths, intakes, stamp)
    }) {
        Ok(Keep::Saved | Keep::Already) => true,
        Ok(Keep::Refused) | Err(_) => false,
    }
}

fn apply_notes(
    file: &mut Value,
    known_paths: Option<&[String]>,
    intakes: &[solstone_vault::Intake],
    stamp: &ClockStamp,
) -> bool {
    let mut changed = false;
    if let Some(known_paths) = known_paths
        && file["obsidian"]["baselined"].as_bool() != Some(true)
    {
        let fresh: BTreeSet<&str> = intakes
            .iter()
            .filter(|intake| is_fresh_place(intake))
            .map(|intake| intake.path.as_str())
            .collect();
        let mut paths = path_set(file);
        for path in known_paths {
            if !fresh.contains(path.as_str()) && paths.insert(path_digest(path)) {
                changed = true;
            }
        }
        if changed {
            set_path_set(file, &paths);
        }
        file["obsidian"]["baselined"] = json!(true);
        changed = true;
    }
    let mut ordered: Vec<&solstone_vault::Intake> = intakes.iter().collect();
    ordered.sort_by_key(|intake| intake.id);
    for intake in ordered {
        if apply_intake(file, intake, stamp) {
            changed = true;
        }
    }
    changed
}

fn is_fresh_place(intake: &solstone_vault::Intake) -> bool {
    intake.first_sight
        && !intake.inventory
        && !intake.removed
        && intake.previous_path.is_none()
        && intake.replaced.is_empty()
}

/// `id` of 0 was assigned before ids started at 1. Apply it once, by the path
/// set, and do not store 0 as the cursor: the cursor's 0 means nothing applied.
fn apply_intake(file: &mut Value, intake: &solstone_vault::Intake, stamp: &ClockStamp) -> bool {
    if intake.id != 0 {
        let cursor = file["obsidian"]
            .get("intake")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if intake.id <= cursor {
            return false;
        }
        file["obsidian"]["intake"] = json!(intake.id);
    }
    let effects = apply_effects(file, intake, stamp);
    intake.id != 0 || effects
}

fn apply_effects(file: &mut Value, intake: &solstone_vault::Intake, stamp: &ClockStamp) -> bool {
    let digest = path_digest(&intake.path);
    if intake.removed {
        let mut paths = path_set(file);
        if !paths.remove(&digest) {
            return false;
        }
        set_path_set(file, &paths);
        return true;
    }
    if intake.inventory {
        let mut paths = path_set(file);
        if !paths.insert(digest) {
            return false;
        }
        set_path_set(file, &paths);
        return true;
    }
    let mut paths = path_set(file);
    let mut moved = false;
    if let Some(previous) = intake.previous_path.as_deref()
        && paths.remove(&path_digest(previous))
    {
        moved = true;
    }
    for replaced in &intake.replaced {
        if paths.remove(&path_digest(replaced)) {
            moved = true;
        }
    }
    if moved || !intake.first_sight {
        let inserted = paths.insert(digest);
        if moved || inserted {
            set_path_set(file, &paths);
            return true;
        }
        return false;
    }
    if !paths.insert(digest) {
        return false;
    }
    set_path_set(file, &paths);
    let Some(total) = total_of(file, "obsidian") else {
        return true;
    };
    file["obsidian"]["notes"] = json!(total.saturating_add(1));
    file["obsidian"]["last"] = json!(stamp.at.clone());
    file["obsidian"]["baselined"] = json!(true);
    true
}

fn carry_tmux_roots_at(state_dir: &Path, roots: &[PathBuf], stamp: &ClockStamp) {
    let _ = update(state_dir, stamp, |file| {
        let mut changed = false;
        for root in roots {
            let key = root_key(root);
            if carried_contains(file, &key) || carried_contains(file, &root.display().to_string()) {
                continue;
            }
            let Some(carried) = read_tmux_terminal(&root.join(FILENAME)) else {
                continue;
            };
            merge_terminal(file, &carried);
            push_carried(file, key);
            changed = true;
        }
        changed
    });
}

struct CarriedTerminal {
    seconds: u64,
    started: Option<String>,
    last: Option<String>,
    seen: Vec<String>,
}

fn read_tmux_terminal(path: &Path) -> Option<CarriedTerminal> {
    let bytes = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let terminal = value.get("terminal")?;
    let seconds = terminal.get("seconds").and_then(Value::as_u64)?;
    if !terminal.get("seconds").is_some_and(Value::is_u64) {
        return None;
    }
    let seen = match terminal.get("seen") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?,
        Some(_) => return None,
    };
    Some(CarriedTerminal {
        seconds,
        started: terminal
            .get("started")
            .and_then(Value::as_str)
            .map(str::to_owned),
        last: terminal
            .get("last")
            .and_then(Value::as_str)
            .map(str::to_owned),
        seen,
    })
}

fn merge_terminal(file: &mut Value, carried: &CarriedTerminal) {
    let current = file["terminal"]["seconds"].as_u64().unwrap_or(0);
    file["terminal"]["seconds"] = json!(current.saturating_add(carried.seconds));
    if let Some(started) = carried.started.as_deref()
        && file["terminal"]["started"]
            .as_str()
            .is_none_or(|existing| started < existing)
    {
        file["terminal"]["started"] = json!(started);
    }
    if let Some(last) = later_last(file["terminal"]["last"].as_str(), carried.last.as_deref()) {
        file["terminal"]["last"] = json!(last);
    }
    for identity in &carried.seen {
        push_seen(file, identity.clone());
    }
}

fn later_last(current: Option<&str>, incoming: Option<&str>) -> Option<String> {
    match (current, incoming) {
        (None, None) => None,
        (Some(current), None) => Some(current.to_owned()),
        (None, Some(incoming)) => Some(incoming.to_owned()),
        (Some(current), Some(incoming)) => {
            let left = DateTime::parse_from_rfc3339(current).ok();
            let right = DateTime::parse_from_rfc3339(incoming).ok();
            match (left, right) {
                (Some(left), Some(right)) if right > left => Some(incoming.to_owned()),
                _ => Some(current.to_owned()),
            }
        }
    }
}

fn accepted_page(batch: &Value) -> Option<(&str, &str, &str)> {
    let records = batch.get("records")?.as_array()?;
    let first = records.first()?;
    if first.get("t").and_then(Value::as_str) != Some("segment_start") {
        return None;
    }
    let url = first
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())?;
    let inst = batch
        .get("inst")
        .and_then(Value::as_str)
        .filter(|inst| !inst.is_empty())?;
    let ctx = first
        .get("ctx")
        .and_then(Value::as_str)
        .filter(|ctx| !ctx.is_empty())?;
    Some((inst, ctx, url))
}

fn directory_has_terminal_frames(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with("_screen.jsonl"))
    })
}

/// The `_LEN` suffix of a finalized segment name. Anything else is not a length.
fn length_suffix(segment: &str) -> Option<u64> {
    let (stem, suffix) = segment.rsplit_once('_')?;
    if stem.len() != 6 || !stem.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if suffix.is_empty() || suffix.len() > 6 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

fn hms_of(segment: &str) -> Option<&str> {
    let stem = segment
        .rsplit_once('_')
        .map(|(stem, _)| stem)
        .unwrap_or(segment);
    (stem.len() == 6 && stem.bytes().all(|byte| byte.is_ascii_digit())).then_some(stem)
}

fn before_cutoff(file: &Value, source: &str, day: &str, segment: &str) -> bool {
    let Some(started_at) = file[source].get("started_at").and_then(Value::as_str) else {
        return true;
    };
    let Ok(cutoff) = DateTime::parse_from_rfc3339(started_at) else {
        return true;
    };
    let Some(hms) = hms_of(segment) else {
        return true;
    };
    capture_instant(day, hms, *cutoff.offset()).is_none_or(|capture| capture < cutoff)
}

fn capture_instant(day: &str, hms: &str, offset: FixedOffset) -> Option<DateTime<FixedOffset>> {
    if day.len() != 8 || hms.len() != 6 {
        return None;
    }
    let year: i32 = day[0..4].parse().ok()?;
    let month: u32 = day[4..6].parse().ok()?;
    let day_n: u32 = day[6..8].parse().ok()?;
    let hour: u32 = hms[0..2].parse().ok()?;
    let minute: u32 = hms[2..4].parse().ok()?;
    let second: u32 = hms[4..6].parse().ok()?;
    offset
        .with_ymd_and_hms(year, month, day_n, hour, minute, second)
        .single()
}

fn total_of(file: &Value, source: &str) -> Option<u64> {
    let field = match source {
        "browsers" => "pages",
        "obsidian" => "notes",
        _ => "seconds",
    };
    file.get(source)
        .and_then(|source| source.get(field))
        .and_then(Value::as_u64)
}

fn seen_contains(file: &Value, identity: &str) -> bool {
    file["terminal"]
        .get("seen")
        .and_then(Value::as_array)
        .is_some_and(|seen| seen.iter().any(|item| item.as_str() == Some(identity)))
}

fn push_seen(file: &mut Value, identity: String) {
    if seen_contains(file, &identity) {
        return;
    }
    if !file["terminal"]["seen"].is_array() {
        file["terminal"]["seen"] = json!([]);
    }
    file["terminal"]["seen"]
        .as_array_mut()
        .expect("seen was just made an array")
        .push(json!(identity));
}

fn tab_url<'a>(file: &'a Value, inst: &str, ctx: &str) -> Option<&'a str> {
    file["browsers"]["tabs"]
        .get(inst)
        .and_then(|tabs| tabs.get(ctx))
        .and_then(Value::as_str)
}

fn set_tab_url(file: &mut Value, inst: &str, ctx: &str, url: &str) {
    if !file["browsers"]["tabs"].is_object() {
        file["browsers"]["tabs"] = json!({});
    }
    if !file["browsers"]["tabs"][inst].is_object() {
        file["browsers"]["tabs"][inst] = json!({});
    }
    file["browsers"]["tabs"][inst][ctx] = json!(url);
}

fn path_set(file: &Value) -> BTreeSet<String> {
    file["obsidian"]
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn set_path_set(file: &mut Value, paths: &BTreeSet<String>) {
    file["obsidian"]["paths"] = json!(paths.iter().cloned().collect::<Vec<_>>());
}

fn path_digest(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn carried_contains(file: &Value, key: &str) -> bool {
    file.get("carried")
        .and_then(Value::as_array)
        .is_some_and(|carried| carried.iter().any(|item| item.as_str() == Some(key)))
}

fn push_carried(file: &mut Value, key: String) {
    if carried_contains(file, &key) {
        return;
    }
    if !file.get("carried").is_some_and(Value::is_array) {
        file["carried"] = json!([]);
    }
    file["carried"]
        .as_array_mut()
        .expect("carried was just made an array")
        .push(json!(key));
}

fn root_key(root: &Path) -> String {
    fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string()
}

enum Keep {
    Saved,
    Already,
    /// The file is not version 1. It was not rewritten.
    Refused,
}

/// Load the whole file, apply `change`, and write it back. `change` returns
/// whether anything that must be saved happened. A file that cannot be read as
/// version 1 is left as it was, and `change` is not called.
fn update(
    state_dir: &Path,
    stamp: &ClockStamp,
    change: impl FnOnce(&mut Value) -> bool,
) -> io::Result<Keep> {
    let Some(_lock) = lock_state(state_dir) else {
        return Err(io::Error::other("could not lock source intake"));
    };
    let file_path = path(state_dir);
    let existing = match fs::read(&file_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut file = match existing {
        None => fresh_file(stamp),
        Some(ref bytes) => match parse_file(bytes) {
            Some(file) => file,
            None => return Ok(Keep::Refused),
        },
    };
    ensure_sources(&mut file, stamp);
    let changed = change(&mut file);
    let creating = existing.is_none();
    if !changed && !creating {
        return Ok(Keep::Already);
    }
    let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    atomic_write_bytes(&file_path, &bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(Keep::Saved)
}

fn lock_state(state_dir: &Path) -> Option<File> {
    fs::create_dir_all(state_dir).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir.join(LOCK_NAME))
        .ok()?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).ok()?;
    Some(file)
}

fn parse_file(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    if !value.get("version").is_some_and(Value::is_u64) {
        return None;
    }
    for (source, field) in [
        ("audio", "seconds"),
        ("screen", "seconds"),
        ("terminal", "seconds"),
        ("browsers", "pages"),
        ("obsidian", "notes"),
        ("screenshots", "screenshots"),
    ] {
        if !total_field_ok(&value, source, field) {
            return None;
        }
        if !started_at_ok(&value, source) {
            return None;
        }
    }
    if !string_array_ok(&value["terminal"], "seen")
        || !string_array_ok(&value["obsidian"], "paths")
        || !string_array_ok(&value["screenshots"], "seen")
    {
        return None;
    }
    if let Some(baselined) = value["obsidian"].get("baselined")
        && !baselined.is_boolean()
    {
        return None;
    }
    if let Some(intake) = value["obsidian"].get("intake")
        && !(intake.as_u64().is_some() && intake.is_u64())
    {
        return None;
    }
    if !tabs_ok(&value["browsers"]) {
        return None;
    }
    Some(value)
}

fn total_field_ok(file: &Value, source: &str, field: &str) -> bool {
    match file.get(source).and_then(|source| source.get(field)) {
        None => true,
        Some(value) => value.as_u64().is_some() && value.is_u64(),
    }
}

fn started_at_ok(file: &Value, source: &str) -> bool {
    match file.get(source).and_then(|source| source.get("started_at")) {
        None => true,
        Some(value) => value
            .as_str()
            .is_some_and(|text| DateTime::parse_from_rfc3339(text).is_ok()),
    }
}

fn string_array_ok(source: &Value, field: &str) -> bool {
    match source.get(field) {
        None => true,
        Some(Value::Array(items)) => items.iter().all(|item| item.as_str().is_some()),
        Some(_) => false,
    }
}

fn tabs_ok(browsers: &Value) -> bool {
    match browsers.get("tabs") {
        None => true,
        Some(Value::Object(instances)) => instances.iter().all(|(_, contexts)| {
            contexts
                .as_object()
                .is_some_and(|contexts| contexts.iter().all(|(_, url)| url.as_str().is_some()))
        }),
        Some(_) => false,
    }
}

fn fresh_file(stamp: &ClockStamp) -> Value {
    json!({
        "version": 1,
        "audio": source_object("seconds", stamp),
        "screen": source_object("seconds", stamp),
        "terminal": terminal_object(stamp),
        "browsers": pages_object(stamp),
        "obsidian": notes_object(stamp),
        "screenshots": screenshots_object(stamp),
        "carried": [],
    })
}

fn ensure_sources(file: &mut Value, stamp: &ClockStamp) {
    if !file.get("audio").is_some_and(Value::is_object) {
        file["audio"] = source_object("seconds", stamp);
    }
    if !file.get("screen").is_some_and(Value::is_object) {
        file["screen"] = source_object("seconds", stamp);
    }
    if !file.get("terminal").is_some_and(Value::is_object) {
        file["terminal"] = terminal_object(stamp);
    }
    if !file.get("browsers").is_some_and(Value::is_object) {
        file["browsers"] = pages_object(stamp);
    }
    if !file.get("obsidian").is_some_and(Value::is_object) {
        file["obsidian"] = notes_object(stamp);
    }
    fill_source(file, "audio", "seconds", stamp);
    fill_source(file, "screen", "seconds", stamp);
    fill_source(file, "terminal", "seconds", stamp);
    fill_source(file, "browsers", "pages", stamp);
    if !file.get("screenshots").is_some_and(Value::is_object) {
        file["screenshots"] = screenshots_object(stamp);
    }
    fill_source(file, "obsidian", "notes", stamp);
    fill_source(file, "screenshots", "screenshots", stamp);
    if !file["screenshots"]["seen"].is_array() {
        file["screenshots"]["seen"] = json!([]);
    }
    if !file["terminal"]["seen"].is_array() {
        file["terminal"]["seen"] = json!([]);
    }
    if !file["browsers"]["tabs"].is_object() {
        file["browsers"]["tabs"] = json!({});
    }
    if !file["obsidian"]["paths"].is_array() {
        file["obsidian"]["paths"] = json!([]);
    }
    if !file["obsidian"]["baselined"].is_boolean() {
        file["obsidian"]["baselined"] = json!(false);
    }
    if file["obsidian"]
        .get("intake")
        .and_then(Value::as_u64)
        .is_none()
    {
        file["obsidian"]["intake"] = json!(0);
    }
    if !file.get("carried").is_some_and(Value::is_array) {
        file["carried"] = json!([]);
    }
}

fn fill_source(file: &mut Value, source: &str, field: &str, stamp: &ClockStamp) {
    if file[source].get(field).and_then(Value::as_u64).is_none() {
        file[source][field] = json!(0);
    }
    if file[source]
        .get("started")
        .and_then(Value::as_str)
        .is_none()
    {
        file[source]["started"] = json!(stamp.started.clone());
    }
    if file[source]
        .get("started_at")
        .and_then(Value::as_str)
        .is_none()
    {
        file[source]["started_at"] = json!(stamp.at.clone());
    }
    if file[source].get("last").is_none() {
        file[source]["last"] = Value::Null;
    }
}

fn source_object(field: &str, stamp: &ClockStamp) -> Value {
    json!({
        field: 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
    })
}

fn terminal_object(stamp: &ClockStamp) -> Value {
    json!({
        "seconds": 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
        "seen": [],
    })
}

fn pages_object(stamp: &ClockStamp) -> Value {
    json!({
        "pages": 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
        "tabs": {},
    })
}

fn screenshots_object(stamp: &ClockStamp) -> Value {
    json!({
        "screenshots": 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
        "seen": [],
    })
}

fn notes_object(stamp: &ClockStamp) -> Value {
    json!({
        "notes": 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
        "paths": [],
        "baselined": false,
        "intake": 0,
    })
}

/// One source's total, as stored. `last` is absent when the field is null or missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter {
    pub total: u64,
    pub started: Option<String>,
    pub last: Option<String>,
}

/// The five counters, read and not written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeRows {
    pub audio: Counter,
    pub screen: Counter,
    pub terminal: Counter,
    pub browsers: Counter,
    pub obsidian: Counter,
    pub screenshots: Counter,
}

/// A read of `source-intake.json`. Missing stays missing. A refused file is not zeros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntakeFile {
    Missing,
    Refused,
    Rows(Box<IntakeRows>),
}

/// Parse `path` and do not create it, lock it, or rewrite it.
#[must_use]
pub fn read_rows(path: &Path) -> IntakeFile {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return IntakeFile::Missing,
        Err(_) => return IntakeFile::Refused,
    };
    let Some(value) = parse_file(&bytes) else {
        return IntakeFile::Refused;
    };
    IntakeFile::Rows(Box::new(IntakeRows {
        audio: counter_of(&value, "audio", "seconds"),
        screen: counter_of(&value, "screen", "seconds"),
        terminal: counter_of(&value, "terminal", "seconds"),
        browsers: counter_of(&value, "browsers", "pages"),
        obsidian: counter_of(&value, "obsidian", "notes"),
        screenshots: counter_of(&value, "screenshots", "screenshots"),
    }))
}

fn counter_of(value: &Value, source: &str, field: &str) -> Counter {
    let Some(source) = value.get(source) else {
        return Counter {
            total: 0,
            started: None,
            last: None,
        };
    };
    Counter {
        total: source.get(field).and_then(Value::as_u64).unwrap_or(0),
        started: source
            .get("started")
            .and_then(Value::as_str)
            .map(str::to_string),
        last: source
            .get("last")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

struct ClockStamp {
    started: String,
    at: String,
}

impl ClockStamp {
    fn now() -> Self {
        Self::from_local(Local::now())
    }

    fn from_local(now: DateTime<Local>) -> Self {
        Self {
            started: now.format("%Y-%m-%d").to_string(),
            at: now.to_rfc3339(),
        }
    }

    #[cfg(test)]
    fn fixed(
        offset_seconds: i32,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Self {
        let offset = FixedOffset::east_opt(offset_seconds).expect("offset fits a day");
        let now = offset
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .expect("stamp is a real local time");
        Self {
            started: now.format("%Y-%m-%d").to_string(),
            at: now.to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn stamp_afternoon() -> ClockStamp {
        ClockStamp::fixed(-6 * 3600, 2020, 1, 1, 15, 0, 0)
    }

    fn stamp_later() -> ClockStamp {
        ClockStamp::fixed(-6 * 3600, 2020, 1, 1, 16, 0, 0)
    }

    fn read_file(dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(path(dir)).unwrap()).unwrap()
    }

    fn write_frames(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("tmux_s1_screen.jsonl"), b"frame").unwrap();
    }

    #[test]
    fn same_day_before_the_cutoff_adds_nothing_even_when_the_directory_is_newer() {
        let temp = tempfile::tempdir().unwrap();
        let stamp = stamp_afternoon();
        ensure_at(temp.path(), &stamp);
        let morning = temp.path().join("morning");
        write_frames(&morning);
        record_terminal_segment_at(
            temp.path(),
            "20200101",
            "120000_005",
            &morning,
            &stamp_later(),
        );
        let file = read_file(temp.path());
        assert_eq!(file["terminal"]["seconds"], 0);
        assert_eq!(file["terminal"]["seen"], json!([]));
        assert!(file["terminal"]["last"].is_null());
        let kept = temp.path().join("kept");
        write_frames(&kept);
        record_terminal_segment_at(temp.path(), "20200101", "160000_005", &kept, &stamp_later());
        record_terminal_segment_at(temp.path(), "20200101", "160000_005", &kept, &stamp_later());
        let file = read_file(temp.path());
        assert_eq!(file["terminal"]["seconds"], 5);
        assert_eq!(file["terminal"]["seen"], json!(["20200101/160000_005"]));
        assert_eq!(file["terminal"]["last"], stamp_later().at);
    }

    #[test]
    fn a_name_without_a_length_adds_nothing() {
        let temp = tempfile::tempdir().unwrap();
        ensure_at(temp.path(), &stamp_afternoon());
        let before = fs::read(path(temp.path())).unwrap();
        let dir = temp.path().join("segment");
        write_frames(&dir);
        record_terminal_segment_at(temp.path(), "20200101", "160000", &dir, &stamp_afternoon());
        assert_eq!(fs::read(path(temp.path())).unwrap(), before);
    }

    #[test]
    fn activity_without_terminal_frames_adds_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("segment");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("160000_desktop_activity.jsonl"), b"row").unwrap();
        record_terminal_segment_at(
            temp.path(),
            "20200101",
            "160000_005",
            &dir,
            &stamp_afternoon(),
        );
        assert!(!path(temp.path()).exists());
    }

    #[test]
    fn audio_rounds_once_and_a_reload_keeps_every_other_source() {
        let temp = tempfile::tempdir().unwrap();
        ensure_at(temp.path(), &stamp_afternoon());
        let mut file = read_file(temp.path());
        file["terminal"]["seconds"] = json!(42);
        file["terminal"]["seen"] = json!(["20200101/010101_001"]);
        file["terminal"]["last"] = json!("2020-01-01T12:00:00-06:00");
        file["browsers"]["pages"] = json!(3);
        file["browsers"]["tabs"] = json!({"inst-a": {"ctx-1": "https://kept.example/"}});
        file["browsers"]["last"] = json!("2020-01-01T12:00:00-06:00");
        file["obsidian"]["notes"] = json!(4);
        file["obsidian"]["paths"] = json!(["abc"]);
        file["obsidian"]["baselined"] = json!(true);
        file["screen"]["seconds"] = json!(9);
        fs::write(path(temp.path()), serde_json::to_vec_pretty(&file).unwrap()).unwrap();

        assert_eq!(audio_seconds((1.6 * 16_000.0 * 2.0) as usize, 16_000), 2);
        assert_eq!(audio_seconds((0.4 * 16_000.0 * 2.0) as usize, 16_000), 0);
        add_seconds_at(temp.path(), "audio", 2, &stamp_later());
        let file = read_file(temp.path());
        assert_eq!(file["audio"]["seconds"], 2);
        assert_eq!(file["audio"]["last"], stamp_later().at);
        assert_eq!(file["terminal"]["seconds"], 42);
        assert_eq!(file["terminal"]["seen"], json!(["20200101/010101_001"]));
        assert_eq!(file["terminal"]["last"], "2020-01-01T12:00:00-06:00");
        assert_eq!(file["browsers"]["pages"], 3);
        assert_eq!(
            file["browsers"]["tabs"]["inst-a"]["ctx-1"],
            "https://kept.example/"
        );
        assert_eq!(file["obsidian"]["notes"], 4);
        assert_eq!(file["obsidian"]["paths"], json!(["abc"]));
        assert_eq!(file["screen"]["seconds"], 9);
        assert!(file["screen"]["last"].is_null());
    }

    #[test]
    fn zero_seconds_do_not_move_last() {
        let temp = tempfile::tempdir().unwrap();
        ensure_at(temp.path(), &stamp_afternoon());
        add_seconds_at(temp.path(), "audio", 0, &stamp_later());
        let file = read_file(temp.path());
        assert_eq!(file["audio"]["seconds"], 0);
        assert!(file["audio"]["last"].is_null());
    }

    #[test]
    fn two_tabs_at_the_same_url_count_separately_and_a_repeat_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let batch = |ctx: &str, url: Option<&str>, kind: &str| {
            let mut record = json!({"t": kind, "ctx": ctx, "inst": "inst-a"});
            if let Some(url) = url {
                record["url"] = json!(url);
            }
            json!({"inst": "inst-a", "records": [record]})
        };
        note_browser_batch_at(
            temp.path(),
            &batch("c1", Some("https://example/"), "segment_start"),
            true,
            &stamp_afternoon(),
        );
        note_browser_batch_at(
            temp.path(),
            &batch("c2", Some("https://example/"), "segment_start"),
            true,
            &stamp_later(),
        );
        let file = read_file(temp.path());
        assert_eq!(file["browsers"]["pages"], 2);
        assert_eq!(file["browsers"]["last"], stamp_later().at);
        let before = fs::read(path(temp.path())).unwrap();
        note_browser_batch_at(
            temp.path(),
            &batch("c1", Some("https://example/"), "segment_start"),
            true,
            &stamp_later(),
        );
        note_browser_batch_at(
            temp.path(),
            &batch("c1", None, "segment_start"),
            true,
            &stamp_later(),
        );
        note_browser_batch_at(
            temp.path(),
            &batch("c1", Some("https://other.example/"), "delta"),
            true,
            &stamp_later(),
        );
        note_browser_batch_at(
            temp.path(),
            &batch("c3", Some("https://example/"), "segment_start"),
            false,
            &stamp_later(),
        );
        assert_eq!(fs::read(path(temp.path())).unwrap(), before);
        note_browser_batch_at(
            temp.path(),
            &batch("c1", Some("https://back.example/"), "segment_start"),
            true,
            &stamp_later(),
        );
        let file = read_file(temp.path());
        assert_eq!(file["browsers"]["pages"], 3);
        assert_eq!(
            file["browsers"]["tabs"]["inst-a"]["c1"],
            "https://back.example/"
        );
        assert_eq!(file["audio"]["seconds"], 0);
    }

    #[test]
    fn a_screenshot_counts_once_and_moves_last() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        add_screenshot_at(dir, "a", &stamp_afternoon());
        add_screenshot_at(dir, "a", &stamp_later());
        add_screenshot_at(dir, "b", &stamp_later());
        let IntakeFile::Rows(rows) = read_rows(&path(dir)) else {
            panic!("rows");
        };
        assert_eq!(rows.screenshots.total, 2);
        assert_eq!(rows.screenshots.last, Some(stamp_later().at));
        assert_eq!(rows.obsidian.total, 0);
    }

    #[test]
    fn local_date_is_the_offset_day_not_utc() {
        let temp = tempfile::tempdir().unwrap();
        let stamp = ClockStamp::fixed(-6 * 3600, 2020, 1, 1, 20, 30, 0);
        ensure_at(temp.path(), &stamp);
        let file = read_file(temp.path());
        assert_eq!(file["audio"]["started"], "2020-01-01");
        assert_eq!(file["audio"]["started_at"], stamp.at);
    }

    #[test]
    fn a_file_that_is_not_version_1_or_whose_total_is_not_whole_is_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = path(temp.path());
        fs::create_dir_all(temp.path()).unwrap();
        let bad_version = br#"{"version":2,"audio":{"seconds":1,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null}}"#;
        fs::write(&file_path, bad_version).unwrap();
        add_seconds_at(temp.path(), "audio", 5, &stamp_afternoon());
        assert_eq!(fs::read(&file_path).unwrap(), bad_version);
        let bad_total = br#"{"version":1,"audio":{"seconds":1.5,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null}}"#;
        fs::write(&file_path, bad_total).unwrap();
        add_seconds_at(temp.path(), "audio", 5, &stamp_afternoon());
        assert_eq!(fs::read(&file_path).unwrap(), bad_total);
    }

    #[test]
    fn carry_is_once_per_root_and_a_missing_file_is_not_marked() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        ensure_at(&state, &stamp_afternoon());
        add_seconds_at(&state, "terminal", 3, &stamp_afternoon());
        let missing = temp.path().join("missing-tmux");
        fs::create_dir_all(&missing).unwrap();
        carry_tmux_roots_at(&state, std::slice::from_ref(&missing), &stamp_later());
        let unchanged = fs::read(path(&state)).unwrap();
        let file = read_file(&state);
        assert_eq!(file["terminal"]["seconds"], 3);
        assert_eq!(file["carried"], json!([]));

        let unreadable = temp.path().join("bad-tmux");
        fs::create_dir_all(&unreadable).unwrap();
        fs::write(unreadable.join(FILENAME), b"{").unwrap();
        carry_tmux_roots_at(&state, std::slice::from_ref(&unreadable), &stamp_later());
        assert_eq!(fs::read(path(&state)).unwrap(), unchanged);

        let first = temp.path().join("tmux-a");
        let second = temp.path().join("tmux-b");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join(FILENAME), br#"{"version":1,"terminal":{"seconds":5,"started":"2019-12-01","started_at":"2019-12-01T00:00:00-06:00","last":"2019-12-02T00:00:00-06:00","seen":["20191202/120000_005"]}}"#).unwrap();
        fs::write(second.join(FILENAME), br#"{"version":1,"terminal":{"seconds":7,"started":"2020-02-01","started_at":"2020-02-01T00:00:00-06:00","last":null,"seen":["20200201/120000_007"]}}"#).unwrap();
        carry_tmux_roots_at(&state, &[first.clone(), second.clone()], &stamp_later());
        let file = read_file(&state);
        assert_eq!(file["terminal"]["seconds"], 15);
        assert_eq!(file["terminal"]["started"], "2019-12-01");
        assert_eq!(file["terminal"]["started_at"], stamp_afternoon().at);
        assert_eq!(
            file["terminal"]["seen"],
            json!(["20191202/120000_005", "20200201/120000_007"])
        );
        assert_eq!(file["carried"].as_array().unwrap().len(), 2);
        fs::write(first.join(FILENAME), br#"{"version":1,"terminal":{"seconds":50,"started":"2019-12-01","started_at":"2019-12-01T00:00:00-06:00","last":null,"seen":[]}}"#).unwrap();
        carry_tmux_roots_at(&state, &[first, second, missing], &stamp_later());
        assert_eq!(read_file(&state)["terminal"]["seconds"], 15);
    }

    #[test]
    fn opening_scan_notes_are_not_counted_and_a_later_note_is() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("one.md"), "one").unwrap();
        fs::write(vault.join("two.md"), "two").unwrap();
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let file = read_file(&state);
        assert_eq!(file["obsidian"]["notes"], 0);
        assert_eq!(file["obsidian"]["baselined"], true);
        assert_eq!(file["obsidian"]["paths"].as_array().unwrap().len(), 2);
        assert!(engine.status(false, Some(true)).first_sync.is_some());
        fs::write(vault.join("three.md"), "three").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let file = read_file(&state);
        assert_eq!(file["obsidian"]["notes"], 1);
        assert!(file["obsidian"]["last"].is_string());
        assert_eq!(file["obsidian"]["paths"].as_array().unwrap().len(), 3);
        assert!(engine.status(false, Some(true)).first_sync.is_some());
    }

    #[test]
    fn an_empty_vaults_first_note_counts_and_a_baseline_rename_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        assert_eq!(read_file(&state)["obsidian"]["baselined"], true);
        fs::write(vault.join("first.md"), "first").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);

        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("old.md"), "same").unwrap();
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::rename(vault.join("old.md"), vault.join("new.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let file = read_file(&state);
        assert_eq!(file["obsidian"]["notes"], 0);
        assert!(file["obsidian"]["last"].is_null());
        let paths = file["obsidian"]["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], path_digest("new.md"));
    }

    fn started_engine(temp: &Path) -> (PathBuf, PathBuf, PathBuf, solstone_vault::Engine) {
        let state = temp.join("state");
        let root = temp.join("engine");
        let vault = temp.join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        (state, root, vault, engine)
    }

    #[test]
    fn an_edit_to_a_recorded_note_adds_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let before = read_file(&state);
        assert_eq!(before["obsidian"]["notes"], 0);
        assert!(before["obsidian"]["last"].is_null());
        fs::write(vault.join("one.md"), "one changed").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let after = read_file(&state);
        assert_eq!(after["obsidian"]["notes"], 0);
        assert!(after["obsidian"]["last"].is_null());
        assert_eq!(after["audio"]["seconds"], before["audio"]["seconds"]);
    }

    #[test]
    fn a_note_whose_save_never_landed_is_counted_once_on_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let mut file = read_file(&state);
        file["audio"]["seconds"] = json!(99);
        fs::write(path(&state), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        let before = fs::read(path(&state)).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o555)).unwrap();
        fs::write(vault.join("new.md"), "new").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let during = fs::read(path(&state)).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(during, before);
        assert_eq!(engine.intakes().len(), 1);
        drop(engine);

        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
        assert!(engine.intakes().is_empty());
        drop(engine);

        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn a_landed_save_whose_acknowledgement_did_not_stays_at_one() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let blocked = {
            let json = fs::read_dir(root.join("tables"))
                .unwrap()
                .find_map(|entry| {
                    let path = entry.unwrap().path();
                    (path.extension().is_some_and(|ext| ext == "json")).then_some(path)
                })
                .unwrap();
            json.with_file_name(format!(
                ".{}.tmp",
                json.file_name().unwrap().to_string_lossy()
            ))
        };
        let hooked = state.clone();
        let block_in_hook = blocked.clone();
        engine.set_intake_hook(Arc::new(move |intakes| {
            let kept = keep_notes(&hooked, None, intakes, &ClockStamp::now());
            let _ = fs::create_dir(&block_in_hook);
            kept
        }));
        fs::write(vault.join("new.md"), "new").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(engine.intakes().len(), 1);
        let _ = fs::remove_dir_all(&blocked);
        drop(engine);

        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        drop(engine);
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
    }

    #[test]
    fn removing_a_note_adds_nothing_and_a_later_note_at_that_path_can_count() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        fs::write(vault.join("old.md"), "old").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::remove_file(vault.join("old.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        assert!(read_file(&state)["obsidian"]["last"].is_null());
        fs::write(vault.join("old.md"), "old again").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
    }

    #[test]
    fn a_counted_note_removed_inside_the_window_lets_the_same_path_count_again() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        fs::write(vault.join("counted.md"), "counted").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        let last = read_file(&state)["obsidian"]["last"].clone();
        fs::remove_file(vault.join("counted.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let held = read_file(&state);
        assert_eq!(held["obsidian"]["notes"], 1);
        assert_eq!(held["obsidian"]["last"], last);
        fs::write(vault.join("counted.md"), "counted again").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 2);
    }

    #[test]
    fn a_stopped_first_walk_records_every_note_and_counts_none() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(vault.join(format!("n{index:04}.md")), "n").unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let partial = read_file(&state)["obsidian"]["paths"]
            .as_array()
            .unwrap()
            .len();
        assert!((256..300).contains(&partial), "{partial}");
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        assert_eq!(
            read_file(&state)["obsidian"]["paths"]
                .as_array()
                .unwrap()
                .len(),
            300
        );
        fs::write(vault.join("after.md"), "after").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
    }

    #[test]
    fn a_note_written_while_the_first_walk_is_open_counts_and_an_edit_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(vault.join(format!("n{index:04}.md")), "n").unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::write(vault.join("during.md"), "during").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert_eq!(counted["obsidian"]["paths"].as_array().unwrap().len(), 301);
        let last = counted["obsidian"]["last"].clone();
        assert!(last.is_string());
        fs::write(vault.join("during.md"), "during changed").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 1);
        assert_eq!(edited["obsidian"]["last"], last);
    }

    #[test]
    fn a_version_one_file_with_no_cursor_counts_once_and_keeps_audio() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let body = br#"{"version":1,"audio":{"seconds":99,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"screen":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"terminal":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"seen":[]},"browsers":{"pages":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"tabs":{}},"obsidian":{"notes":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"paths":[],"baselined":true},"carried":[]}"#;
        fs::write(path(&state), body).unwrap();
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let file = read_file(&state);
        assert_eq!(file["obsidian"]["notes"], 1);
        assert!(file["obsidian"]["intake"].is_u64());
        assert_eq!(file["audio"]["seconds"], 99);
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn a_version_one_file_with_a_bad_cursor_is_left_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        fs::create_dir_all(&state).unwrap();
        let body = br#"{"version":1,"audio":{"seconds":99,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"screen":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"terminal":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"seen":[]},"browsers":{"pages":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"tabs":{}},"obsidian":{"notes":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"paths":[],"baselined":false,"intake":"no"},"carried":[]}"#;
        fs::write(path(&state), body).unwrap();
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(fs::read(path(&state)).unwrap(), body);
        assert_eq!(engine.intakes().len(), 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn an_id_of_zero_counts_once_and_does_not_move_the_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let stamp = stamp_afternoon();
        let intake = solstone_vault::Intake {
            id: 0,
            path: "a.md".to_owned(),
            previous_path: None,
            replaced: Vec::new(),
            first_sight: true,
            removed: false,
            inventory: false,
            at_ms: 0,
        };
        assert!(keep_notes(&state, Some(&[]), &[], &stamp));
        assert!(keep_notes(
            &state,
            None,
            std::slice::from_ref(&intake),
            &stamp
        ));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["obsidian"]["intake"], 0);
        assert!(keep_notes(&state, None, &[intake], &stamp));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["obsidian"]["intake"], 0);
    }

    fn intake_of(
        id: u64,
        path: &str,
        replaced: Vec<String>,
        first_sight: bool,
        removed: bool,
        inventory: bool,
    ) -> solstone_vault::Intake {
        solstone_vault::Intake {
            id,
            path: path.to_owned(),
            previous_path: None,
            replaced,
            first_sight,
            removed,
            inventory,
            at_ms: 0,
        }
    }

    #[test]
    fn an_id_of_zero_that_is_inventory_records_the_path_and_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let stamp = stamp_afternoon();
        assert!(keep_notes(&state, Some(&[]), &[], &stamp));
        pin_audio(&state);
        let recorded = intake_of(0, "a.md", Vec::new(), true, false, true);
        assert!(keep_notes(
            &state,
            None,
            std::slice::from_ref(&recorded),
            &stamp
        ));
        let file = read_file(&state);
        assert_eq!(file["obsidian"]["notes"], 0);
        assert_eq!(file["obsidian"]["intake"], 0);
        assert!(file["obsidian"]["last"].is_null());
        assert!(path_set(&file).contains(&path_digest("a.md")));
        assert_eq!(file["audio"]["seconds"], 99);
        assert!(keep_notes(&state, None, &[recorded], &stamp));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let again = intake_of(1, "a.md", Vec::new(), true, false, false);
        assert!(keep_notes(&state, None, &[again], &stamp));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let next = intake_of(2, "b.md", Vec::new(), true, false, false);
        assert!(keep_notes(&state, None, &[next], &stamp));
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert_eq!(counted["obsidian"]["intake"], 2);
        assert_eq!(counted["audio"]["seconds"], 99);
    }

    #[test]
    fn an_id_of_zero_that_is_a_removal_drops_the_hash_and_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let stamp = stamp_afternoon();
        assert!(keep_notes(&state, Some(&[]), &[], &stamp));
        pin_audio(&state);
        let recorded = intake_of(0, "a.md", Vec::new(), true, false, true);
        assert!(keep_notes(&state, None, &[recorded], &stamp));
        let removal = intake_of(0, "a.md", Vec::new(), false, true, false);
        assert!(keep_notes(
            &state,
            None,
            std::slice::from_ref(&removal),
            &stamp
        ));
        let dropped = read_file(&state);
        assert_eq!(dropped["obsidian"]["notes"], 0);
        assert_eq!(dropped["obsidian"]["intake"], 0);
        assert!(dropped["obsidian"]["last"].is_null());
        assert!(!path_set(&dropped).contains(&path_digest("a.md")));
        assert_eq!(dropped["audio"]["seconds"], 99);
        assert!(keep_notes(&state, None, &[removal], &stamp));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let again = intake_of(1, "a.md", Vec::new(), true, false, false);
        assert!(keep_notes(&state, None, &[again], &stamp));
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert_eq!(counted["obsidian"]["intake"], 1);
        assert_eq!(counted["audio"]["seconds"], 99);
    }

    #[test]
    fn a_place_and_a_removal_in_one_batch_keep_the_count_and_drop_the_hash() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let stamp = stamp_afternoon();
        assert!(keep_notes(&state, Some(&[]), &[], &stamp));
        pin_audio(&state);
        let place = intake_of(1, "a.md", Vec::new(), true, false, false);
        let removal = intake_of(2, "a.md", Vec::new(), false, true, false);
        assert!(keep_notes(&state, None, &[place, removal], &stamp));
        let held = read_file(&state);
        assert_eq!(held["obsidian"]["notes"], 1);
        assert!(held["obsidian"]["last"].is_string());
        assert!(!path_set(&held).contains(&path_digest("a.md")));
        assert_eq!(held["audio"]["seconds"], 99);
        let later = intake_of(3, "a.md", Vec::new(), true, false, false);
        assert!(keep_notes(&state, None, &[later], &stamp));
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 2);
        assert_eq!(counted["audio"]["seconds"], 99);
    }

    #[test]
    fn a_collapsed_first_sight_moves_the_replaced_hash_and_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let stamp = stamp_afternoon();
        assert!(keep_notes(&state, Some(&[]), &[], &stamp));
        pin_audio(&state);
        let recorded = intake_of(0, "a.md", Vec::new(), true, false, true);
        assert!(keep_notes(&state, None, &[recorded], &stamp));
        let collapsed = intake_of(1, "d.md", vec!["a.md".to_owned()], true, false, false);
        assert!(keep_notes(&state, None, &[collapsed], &stamp));
        let moved = read_file(&state);
        assert_eq!(moved["obsidian"]["notes"], 0);
        assert!(moved["obsidian"]["last"].is_null());
        assert_eq!(moved["audio"]["seconds"], 99);
        let paths = path_set(&moved);
        assert!(!paths.contains(&path_digest("a.md")));
        assert!(paths.contains(&path_digest("d.md")));
    }

    #[test]
    fn re_pair_removes_the_paired_journal_file_and_not_the_counter() {
        let temp = tempfile::tempdir().unwrap();
        ensure_at(temp.path(), &stamp_afternoon());
        let counter = fs::read(path(temp.path())).unwrap();
        fs::write(
            crate::sync_health::paired_journal_path(temp.path()),
            b"{\"version\":1}",
        )
        .unwrap();
        crate::sync_health::forget_paired_journal(temp.path());
        assert!(!crate::sync_health::paired_journal_path(temp.path()).exists());
        assert_eq!(fs::read(path(temp.path())).unwrap(), counter);
    }

    #[test]
    fn about_and_the_support_report_do_not_carry_a_counter() {
        let about = crate::about::AboutBlock::unknown(crate::about::HostFacts {
            os: "ubuntu".into(),
            os_version: "24.04".into(),
            arch: "x86_64".into(),
        });
        let url = crate::support::report_url("recording", &about);
        assert!(!url.contains("source-intake"));
        assert!(!url.contains("started_at"));
        assert!(!url.contains("seconds="));
        let rendered = format!("{about:?}");
        assert!(!rendered.contains("source-intake"));
    }

    fn table_value(root: &Path) -> Value {
        let path = fs::read_dir(root.join("tables"))
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                path.extension()
                    .is_some_and(|ext| ext == "json")
                    .then_some(path)
            })
            .unwrap();
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn pin_audio(state: &Path) {
        let mut file = read_file(state);
        file["audio"]["seconds"] = json!(99);
        fs::write(path(state), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    }

    #[test]
    fn a_counted_note_renamed_inside_the_window_does_not_count_again() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        fs::write(vault.join("a.md"), "a").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        let last = counted["obsidian"]["last"].clone();
        fs::rename(vault.join("a.md"), vault.join("b.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let renamed = read_file(&state);
        assert_eq!(renamed["obsidian"]["notes"], 1);
        assert_eq!(renamed["obsidian"]["last"], last);
        assert_eq!(renamed["audio"]["seconds"], 99);
        let paths = renamed["obsidian"]["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], path_digest("b.md"));
    }

    #[test]
    fn a_renamed_note_removed_while_its_old_path_is_occupied_keeps_both_counts() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        fs::write(vault.join("live.md"), "live").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        fs::rename(vault.join("live.md"), vault.join("moved.md")).unwrap();
        fs::write(vault.join("live.md"), "fresh").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let both = read_file(&state);
        assert_eq!(both["obsidian"]["notes"], 2);
        let last = both["obsidian"]["last"].clone();
        fs::remove_file(vault.join("moved.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let held = read_file(&state);
        assert_eq!(held["obsidian"]["notes"], 2);
        assert_eq!(held["obsidian"]["last"], last);
        assert_eq!(held["audio"]["seconds"], 99);
        let held_paths = held["obsidian"]["paths"].as_array().unwrap();
        let live = json!(path_digest("live.md"));
        let moved = json!(path_digest("moved.md"));
        assert!(held_paths.iter().any(|path| path == &live));
        assert!(!held_paths.iter().any(|path| path == &moved));
        let log = fs::read_to_string(root.join("pending.log")).unwrap_or_default();
        assert!(
            !log.lines().any(|line| {
                line.contains("\"path\":\"live.md\"") && line.contains("\"change\":\"removed\"")
            }),
            "{log}"
        );
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 2);
        fs::write(vault.join("moved.md"), "again").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let added = read_file(&state);
        assert_eq!(added["obsidian"]["notes"], 3);
        assert_ne!(added["obsidian"]["last"], last);
        assert_eq!(added["audio"]["seconds"], 99);
    }

    #[test]
    fn a_missing_path_leaves_the_open_listing_and_a_later_note_there_counts() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..520 {
            fs::write(
                vault.join(format!("n{index:04}.md")),
                format!("n{index:04}"),
            )
            .unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        let cancel = std::sync::atomic::AtomicBool::new(true);
        engine.scan(&cancel).unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        pin_audio(&state);
        fs::remove_file(vault.join("n0000.md")).unwrap();
        engine.scan(&cancel).unwrap();
        let open = table_value(&root);
        assert_eq!(open["first_list_open"], true);
        let listed = open["listing_paths"].as_array().unwrap();
        assert!(!listed.iter().any(|path| path == "n0000.md"));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        // The delete is held once this scan reaches it, and the next one
        // confirms it. The note the listing recorded still adds nothing.
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::write(vault.join("n0000.md"), "new").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        let last = counted["obsidian"]["last"].clone();
        assert!(last.is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        fs::write(vault.join("n0000.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 1);
        assert_eq!(edited["obsidian"]["last"], last);
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_unread_directory_is_recorded_when_a_later_walk_can_see_it() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("seen.md"), "seen").unwrap();
        let locked = vault.join("locked");
        fs::create_dir(&locked).unwrap();
        for index in 0..300 {
            fs::write(locked.join(format!("n{index:04}.md")), "n").unwrap();
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        let first = engine.scan(&std::sync::atomic::AtomicBool::new(false));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        first.unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let partial = table_value(&root);
        assert_eq!(partial["first_list_open"], true);
        assert!(partial["listing_paths"].is_null());
        drop(engine);

        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        attach_vault(&mut engine, state.clone());
        let reloaded = table_value(&root);
        assert!(reloaded["listing_paths"].is_null());
        assert_eq!(reloaded["first_list_open"], true);
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        let snapped = table_value(&root);
        assert_eq!(snapped["first_list_open"], true);
        let listed = snapped["listing_paths"].as_array().unwrap();
        assert_eq!(listed.len(), 301);
        assert!(listed.iter().any(|path| path == "locked/n0000.md"));
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::write(vault.join("during.md"), "during").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert_eq!(counted["audio"]["seconds"], 99);
        assert_eq!(counted["obsidian"]["paths"].as_array().unwrap().len(), 302);
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn a_too_large_note_is_recorded_when_it_becomes_readable() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("small.md"), "small").unwrap();
        fs::write(
            vault.join("big.md"),
            vec![b'x'; solstone_vault::MAX_NOTE_BYTES + 1],
        )
        .unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let listed = table_value(&root);
        assert_eq!(listed["first_list_open"], false);
        let paths = listed["listing_paths"].as_array().unwrap();
        assert!(paths.iter().any(|path| path == "big.md"));
        assert!(!paths.iter().any(|path| path == "small.md"));
        fs::write(vault.join("big.md"), "hello").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let readable = read_file(&state);
        assert_eq!(readable["obsidian"]["notes"], 0);
        assert!(readable["obsidian"]["last"].is_null());
        assert_eq!(readable["audio"]["seconds"], 99);
        fs::write(vault.join("big.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
        fs::remove_file(vault.join("big.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::write(vault.join("big.md"), "fresh").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
    }

    #[test]
    fn a_too_large_note_that_disappears_does_not_block_a_new_note() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(
            vault.join("big.md"),
            vec![b'x'; solstone_vault::MAX_NOTE_BYTES + 1],
        )
        .unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::remove_file(vault.join("big.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let held = table_value(&root);
        assert!(
            held["listing_paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == "big.md")
        );
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert!(table_value(&root)["listing_paths"].is_null());
        fs::write(vault.join("big.md"), "fresh").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert_eq!(counted["audio"]["seconds"], 99);
    }

    #[test]
    fn a_too_large_note_missing_for_one_walk_does_not_count_when_it_returns() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(
            vault.join("big.md"),
            vec![b'x'; solstone_vault::MAX_NOTE_BYTES + 1],
        )
        .unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        fs::remove_file(vault.join("big.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        fs::write(vault.join("big.md"), "back").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let back = read_file(&state);
        assert_eq!(back["obsidian"]["notes"], 0);
        assert!(back["obsidian"]["last"].is_null());
        assert_eq!(back["audio"]["seconds"], 99);
        fs::write(vault.join("big.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_unreadable_note_is_recorded_when_it_can_be_read() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("plain.md"), "plain").unwrap();
        let secret = vault.join("secret.md");
        fs::write(&secret, "secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        let first = engine.scan(&std::sync::atomic::AtomicBool::new(false));
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
        first.unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let listed = table_value(&root);
        assert_eq!(listed["first_list_open"], false);
        assert!(
            listed["listing_paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == "secret.md")
        );
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let readable = read_file(&state);
        assert_eq!(readable["obsidian"]["notes"], 0);
        assert!(readable["obsidian"]["last"].is_null());
        assert_eq!(readable["audio"]["seconds"], 99);
        fs::write(&secret, "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_unread_directory_after_the_set_is_stored_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("a{index:04}.md")),
                format!("a{index:04}"),
            )
            .unwrap();
        }
        let kept = vault.join("kept");
        fs::create_dir(&kept).unwrap();
        fs::write(kept.join("secret.md"), "secret").unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        pin_audio(&state);
        let open = table_value(&root);
        assert_eq!(open["first_list_open"], true);
        assert!(
            open["listing_paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == "kept/secret.md")
        );
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o000)).unwrap();
        let locked = engine.scan(&std::sync::atomic::AtomicBool::new(false));
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o755)).unwrap();
        locked.unwrap();
        let still = table_value(&root);
        assert_eq!(still["first_list_open"], true);
        assert!(
            still["listing_paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|path| path == "kept/secret.md")
        );
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let recorded = read_file(&state);
        assert_eq!(recorded["obsidian"]["notes"], 0);
        assert!(recorded["obsidian"]["last"].is_null());
        assert_eq!(recorded["audio"]["seconds"], 99);
        fs::write(kept.join("secret.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn a_finished_listing_reloads_and_a_later_note_counts() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("one.md"), "one").unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let finished = table_value(&root);
        assert_eq!(finished["first_sync_started"], true);
        assert_eq!(finished["first_list_open"], false);
        assert!(finished["listing_paths"].is_null());
        drop(engine);

        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        let reloaded = table_value(&temp.path().join("engine"));
        assert_eq!(reloaded["first_sync_started"], true);
        assert_eq!(reloaded["first_list_open"], false);
        fs::write(vault.join("two.md"), "two").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        fs::write(vault.join("one.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 1);
        assert_eq!(edited["obsidian"]["last"], counted["obsidian"]["last"]);
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_unread_directory_after_the_listing_finished_does_not_reopen_it() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        fs::write(vault.join("one.md"), "one").unwrap();
        let kept = vault.join("kept");
        fs::create_dir(&kept).unwrap();
        fs::write(kept.join("old.md"), "old").unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o000)).unwrap();
        fs::write(vault.join("two.md"), "two").unwrap();
        let locked = engine.scan(&std::sync::atomic::AtomicBool::new(false));
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o755)).unwrap();
        locked.unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        assert_eq!(table_value(&root)["first_list_open"], false);
    }

    #[test]
    fn a_not_yet_read_note_renamed_before_resume_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("n{index:04}.md")),
                format!("n{index:04}"),
            )
            .unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        fs::rename(vault.join("n0299.md"), vault.join("moved.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let renamed = read_file(&state);
        assert_eq!(renamed["obsidian"]["notes"], 0);
        assert!(renamed["obsidian"]["last"].is_null());
        assert_eq!(renamed["audio"]["seconds"], 99);
        fs::write(vault.join("moved.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_empty_vault_records_nothing_and_a_later_note_counts() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        assert!(read_file(&state)["obsidian"]["last"].is_null());
        let finished = table_value(&root);
        assert_eq!(finished["first_sync_started"], true);
        assert_eq!(finished["first_list_open"], false);
        assert!(finished["listing_paths"].is_null());
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        fs::write(vault.join("one.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 1);
        assert_eq!(edited["obsidian"]["last"], counted["obsidian"]["last"]);
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn an_existing_notes_total_survives_an_edit_a_new_note_and_a_rename() {
        let temp = tempfile::tempdir().unwrap();
        let (state, _, vault, mut engine) = started_engine(temp.path());
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let mut file = read_file(&state);
        file["obsidian"]["notes"] = json!(4);
        file["obsidian"]["last"] = Value::Null;
        file["obsidian"]["started"] = json!("2019-05-05");
        file["audio"]["seconds"] = json!(99);
        file["audio"]["started"] = json!("2019-05-05");
        file["browsers"]["tabs"] = json!({"inst": {"c1": "https://example/"}});
        file["carried"] = json!(["root-a"]);
        file["obsidian"].as_object_mut().unwrap().remove("intake");
        fs::write(path(&state), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        fs::write(vault.join("one.md"), "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 4);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["obsidian"]["started"], "2019-05-05");
        assert_eq!(edited["audio"]["seconds"], 99);
        assert_eq!(edited["audio"]["started"], "2019-05-05");
        assert_eq!(edited["browsers"]["tabs"]["inst"]["c1"], "https://example/");
        assert_eq!(edited["carried"], json!(["root-a"]));
        fs::write(vault.join("two.md"), "two").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 5);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        assert_eq!(counted["audio"]["started"], "2019-05-05");
        assert_eq!(counted["carried"], json!(["root-a"]));
        let last = counted["obsidian"]["last"].clone();
        fs::rename(vault.join("one.md"), vault.join("moved.md")).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let renamed = read_file(&state);
        assert_eq!(renamed["obsidian"]["notes"], 5);
        assert_eq!(renamed["obsidian"]["last"], last);
        assert_eq!(renamed["audio"]["seconds"], 99);
        assert_eq!(renamed["audio"]["started"], "2019-05-05");
        assert_eq!(
            renamed["browsers"]["tabs"]["inst"]["c1"],
            "https://example/"
        );
        assert_eq!(renamed["carried"], json!(["root-a"]));
        let paths = path_set(&renamed);
        assert!(!paths.contains(&path_digest("one.md")));
        assert!(paths.contains(&path_digest("moved.md")));
    }

    #[test]
    fn an_unacked_id_of_zero_counts_and_the_next_note_counts_once() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        pin_audio(&state);
        drop(engine);
        let table_path = fs::read_dir(root.join("tables"))
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                path.extension()
                    .is_some_and(|ext| ext == "json")
                    .then_some(path)
            })
            .unwrap();
        let mut table = table_value(&root);
        table["next_intake_id"] = json!(1);
        table["intakes"] = json!([{
            "id": 0,
            "path": "zero.md",
            "previous_path": null,
            "replaced": [],
            "first_sight": true,
            "removed": false,
            "inventory": false,
            "at_ms": 0
        }]);
        fs::write(&table_path, serde_json::to_vec(&table).unwrap()).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["obsidian"]["intake"], 0);
        assert!(engine.intakes().is_empty());
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 2);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        assert!(engine.intakes().is_empty());
        drop(engine);
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 2);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn a_listed_note_missing_for_one_walk_does_not_count_when_it_returns() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("n{index:04}.md")),
                format!("n{index:04}"),
            )
            .unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        pin_audio(&state);
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        let note = vault.join("n0000.md");
        fs::remove_file(&note).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        fs::write(&note, "n0000").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let back = read_file(&state);
        assert_eq!(back["obsidian"]["notes"], 0);
        assert!(back["obsidian"]["last"].is_null());
        assert_eq!(back["audio"]["seconds"], 99);
        fs::write(&note, "edited").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let edited = read_file(&state);
        assert_eq!(edited["obsidian"]["notes"], 0);
        assert!(edited["obsidian"]["last"].is_null());
        assert_eq!(edited["audio"]["seconds"], 99);
    }

    #[test]
    fn a_failed_save_while_closing_the_listing_keeps_it_open() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("n{index:04}.md")),
                format!("n{index:04}"),
            )
            .unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        pin_audio(&state);
        let open = table_value(&root);
        assert_eq!(open["first_list_open"], true);
        let table_path = fs::read_dir(root.join("tables"))
            .unwrap()
            .find_map(|entry| {
                let path = entry.unwrap().path();
                path.extension()
                    .is_some_and(|ext| ext == "json")
                    .then_some(path)
            })
            .unwrap();
        let blocked = table_path.with_file_name(format!(
            ".{}.tmp",
            table_path.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir(&blocked).unwrap();
        let failed = engine.scan(&std::sync::atomic::AtomicBool::new(false));
        let _ = fs::remove_dir_all(&blocked);
        assert!(failed.is_err());
        let still = table_value(&root);
        assert_eq!(still["first_list_open"], true);
        assert!(still["listing_paths"].is_array());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let recorded = read_file(&state);
        assert_eq!(recorded["obsidian"]["notes"], 0);
        assert!(recorded["obsidian"]["last"].is_null());
        assert_eq!(recorded["audio"]["seconds"], 99);
    }

    #[test]
    fn a_refused_counter_file_stays_byte_for_byte_across_a_reopen() {
        for body in [
            br#"{"version":2,"audio":{"seconds":99}}"#.as_slice(),
            br#"{"version":1,"audio":{"seconds":99,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null},"screen":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null},"terminal":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"seen":[]},"browsers":{"pages":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"tabs":{}},"obsidian":{"notes":1.5,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"paths":[],"baselined":true},"carried":[]}"#.as_slice(),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (state, root, vault, mut engine) = started_engine(temp.path());
            fs::write(vault.join("one.md"), "one").unwrap();
            engine
                .scan(&std::sync::atomic::AtomicBool::new(false))
                .unwrap();
            fs::write(path(&state), body).unwrap();
            fs::write(vault.join("two.md"), "two").unwrap();
            engine
                .scan(&std::sync::atomic::AtomicBool::new(false))
                .unwrap();
            assert_eq!(fs::read(path(&state)).unwrap(), body);
            assert!(!engine.intakes().is_empty());
            drop(engine);
            let mut engine =
                solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
            attach_vault(&mut engine, state.clone());
            assert_eq!(fs::read(path(&state)).unwrap(), body);
            assert!(!engine.intakes().is_empty());
        }
    }

    #[test]
    fn a_refused_file_counts_the_waiting_note_once_it_is_valid_again() {
        let temp = tempfile::tempdir().unwrap();
        let (state, root, vault, mut engine) = started_engine(temp.path());
        fs::write(vault.join("one.md"), "one").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let refused = br#"{"version":2,"audio":{"seconds":99}}"#;
        fs::write(path(&state), refused).unwrap();
        fs::write(vault.join("two.md"), "two").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        assert_eq!(fs::read(path(&state)).unwrap(), refused);
        assert!(!engine.intakes().is_empty());
        drop(engine);
        let valid = br#"{"version":1,"audio":{"seconds":99,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"screen":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null},"terminal":{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"seen":[]},"browsers":{"pages":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"tabs":{}},"obsidian":{"notes":0,"started":"2020-01-01","started_at":"2020-01-01T15:00:00-06:00","last":null,"paths":[],"baselined":true},"carried":[]}"#;
        fs::write(path(&state), valid).unwrap();
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        attach_vault(&mut engine, state.clone());
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 1);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["audio"]["seconds"], 99);
        assert!(engine.intakes().is_empty());
        drop(engine);
        let mut engine =
            solstone_vault::Engine::open(root, Arc::new(solstone_vault::SystemClock)).unwrap();
        attach_vault(&mut engine, state.clone());
        assert_eq!(read_file(&state)["obsidian"]["notes"], 1);
        assert_eq!(read_file(&state)["audio"]["seconds"], 99);
    }

    #[test]
    fn an_existing_total_stays_while_the_first_listing_is_still_open() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let root = temp.path().join("engine");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        for index in 0..300 {
            fs::write(
                vault.join(format!("n{index:04}.md")),
                format!("n{index:04}"),
            )
            .unwrap();
        }
        let mut engine =
            solstone_vault::Engine::open(root.clone(), Arc::new(solstone_vault::SystemClock))
                .unwrap();
        engine.turn_on(&vault).unwrap();
        engine.apply_vault_list(&[]).unwrap();
        attach_vault(&mut engine, state.clone());
        engine
            .scan(&std::sync::atomic::AtomicBool::new(true))
            .unwrap();
        assert_eq!(read_file(&state)["obsidian"]["notes"], 0);
        assert_eq!(table_value(&root)["first_list_open"], true);
        let mut file = read_file(&state);
        file["obsidian"]["notes"] = json!(4);
        file["obsidian"]["last"] = Value::Null;
        file["obsidian"]["started"] = json!("2019-05-05");
        file["audio"]["seconds"] = json!(99);
        fs::write(path(&state), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let finished = read_file(&state);
        assert_eq!(finished["obsidian"]["notes"], 4);
        assert!(finished["obsidian"]["last"].is_null());
        assert_eq!(finished["obsidian"]["started"], "2019-05-05");
        assert_eq!(finished["audio"]["seconds"], 99);
        assert_eq!(table_value(&root)["first_list_open"], false);
        fs::write(vault.join("after.md"), "after").unwrap();
        engine
            .scan(&std::sync::atomic::AtomicBool::new(false))
            .unwrap();
        let counted = read_file(&state);
        assert_eq!(counted["obsidian"]["notes"], 5);
        assert!(counted["obsidian"]["last"].is_string());
        assert_eq!(counted["obsidian"]["started"], "2019-05-05");
        assert_eq!(counted["audio"]["seconds"], 99);
    }
}

#[cfg(test)]
mod read_rows_tests {
    use super::{IntakeFile, read_rows};
    use std::fs;

    fn sample(screen: u64, audio: u64, last: &str) -> String {
        format!(
            r#"{{"version":1,"audio":{{"seconds":{audio},"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null}},"screen":{{"seconds":{screen},"started":"2025-03-03","started_at":"2025-03-03T00:00:00Z","last":{last}}},"terminal":{{"seconds":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"seen":[]}},"browsers":{{"pages":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"tabs":{{}}}},"obsidian":{{"notes":0,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null,"paths":[],"baselined":false}}}}"#
        )
    }

    #[test]
    fn a_missing_file_stays_missing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source-intake.json");
        assert_eq!(read_rows(&path), IntakeFile::Missing);
        assert!(!path.exists());
    }

    #[test]
    fn a_refused_file_is_not_zeros_and_is_not_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source-intake.json");
        let body = br#"{"version":1,"screen":{"seconds":1.5,"started":"2020-01-01","started_at":"2020-01-01T00:00:00Z","last":null}}"#;
        fs::write(&path, body).unwrap();
        assert_eq!(read_rows(&path), IntakeFile::Refused);
        assert_eq!(fs::read(&path).unwrap(), body);
    }

    #[test]
    fn screen_and_audio_are_separate_and_a_null_last_stays_absent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source-intake.json");
        let body = sample(3600, 0, "null");
        fs::write(&path, &body).unwrap();
        let IntakeFile::Rows(rows) = read_rows(&path) else {
            panic!("the file is readable");
        };
        assert_eq!(rows.screen.total, 3600);
        assert_eq!(rows.audio.total, 0);
        assert_eq!(rows.screen.started.as_deref(), Some("2025-03-03"));
        assert_eq!(rows.screen.last, None);
        assert_eq!(fs::read(&path).unwrap(), body.as_bytes());
    }
}
