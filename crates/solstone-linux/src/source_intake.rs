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

pub(crate) const FILENAME: &str = "source-intake.json";
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

pub(crate) fn note_browser_batch(state_dir: &Path, batch: &Value, accepted: bool) {
    note_browser_batch_at(state_dir, batch, accepted, &ClockStamp::now());
}

pub(crate) fn vault_hook(state_dir: PathBuf) -> solstone_vault::NoteIntakeHook {
    Arc::new(move |event| note_vault_at(&state_dir, event, &ClockStamp::now()))
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

fn note_vault_at(state_dir: &Path, event: &solstone_vault::IntakeEvent, stamp: &ClockStamp) {
    let _ = update(state_dir, stamp, |file| match event {
        solstone_vault::IntakeEvent::OpeningScanFinished => {
            let already = file["obsidian"]["baselined"].as_bool() == Some(true);
            file["obsidian"]["baselined"] = json!(true);
            !already
        }
        solstone_vault::IntakeEvent::Note(note) => note_vault_path(file, note, stamp),
    });
}

fn note_vault_path(
    file: &mut Value,
    note: &solstone_vault::NoteIntake,
    stamp: &ClockStamp,
) -> bool {
    let digest = path_digest(&note.path);
    if let Some(previous) = note.previous_path.as_deref() {
        let old = path_digest(previous);
        let mut paths = path_set(file);
        let had_old = paths.remove(&old);
        let inserted = paths.insert(digest);
        set_path_set(file, &paths);
        return had_old || inserted;
    }
    let mut paths = path_set(file);
    if !paths.insert(digest) {
        return false;
    }
    set_path_set(file, &paths);
    if note.baseline {
        return true;
    }
    let Some(total) = total_of(file, "obsidian") else {
        return false;
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

/// Load the whole file, apply `change`, and write it back. `change` returns
/// whether anything that must be saved happened. A file that cannot be read as
/// version 1 is left as it was, and `change` is not called.
fn update(
    state_dir: &Path,
    stamp: &ClockStamp,
    change: impl FnOnce(&mut Value) -> bool,
) -> io::Result<()> {
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
            None => return Ok(()),
        },
    };
    ensure_sources(&mut file, stamp);
    let changed = change(&mut file);
    let creating = existing.is_none();
    if !changed && !creating {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
    atomic_write_bytes(&file_path, &bytes).map_err(|error| io::Error::other(error.to_string()))
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
    ] {
        if !total_field_ok(&value, source, field) {
            return None;
        }
        if !started_at_ok(&value, source) {
            return None;
        }
    }
    if !string_array_ok(&value["terminal"], "seen") || !string_array_ok(&value["obsidian"], "paths")
    {
        return None;
    }
    if let Some(baselined) = value["obsidian"].get("baselined")
        && !baselined.is_boolean()
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
    fill_source(file, "obsidian", "notes", stamp);
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

fn notes_object(stamp: &ClockStamp) -> Value {
    json!({
        "notes": 0,
        "started": stamp.started,
        "started_at": stamp.at,
        "last": Value::Null,
        "paths": [],
        "baselined": false,
    })
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
        engine.set_note_intake(vault_hook(state.clone()));
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
        engine.set_note_intake(vault_hook(state.clone()));
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
        engine.set_note_intake(vault_hook(state.clone()));
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
}
