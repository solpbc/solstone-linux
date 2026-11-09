// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The engine's rules, end to end against real files and an injected clock.

use crate::{
    Answer, Change, Clock, Delivery, Engine, Line, ListedVault, Phase, ScanOutcome, Stamp, State,
    engine::{self, TurnOnError},
    table::Table,
    wire,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const OFFSET: i32 = -7 * 3600;

/// 2026-11-08T09:30:00-07:00, the morning most tests start.
const T0: i64 = 1_794_155_400_000;

#[derive(Clone)]
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn at(ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(ms)))
    }
    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
    fn advance_s(&self, seconds: i64) {
        self.0.fetch_add(seconds * 1000, Ordering::SeqCst);
    }
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.now()
    }
    fn offset_s_at(&self, _ms: i64) -> i32 {
        OFFSET
    }
}

struct Harness {
    _temp: tempfile::TempDir,
    root: PathBuf,
    vault: PathBuf,
    clock: TestClock,
    engine: Engine,
}

impl Harness {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let vault = temp.path().join("Notes");
        fs::create_dir_all(vault.join(".obsidian")).unwrap();
        let clock = TestClock::at(T0);
        let engine = Engine::open(root.clone(), Arc::new(clock.clone())).unwrap();
        Self {
            _temp: temp,
            root,
            vault,
            clock,
            engine,
        }
    }

    fn turned_on() -> Self {
        let mut harness = Self::new();
        harness.engine.turn_on(&harness.vault).unwrap();
        harness.engine.apply_vault_list(&[]).unwrap();
        assert_eq!(harness.engine.phase(), Some(&Phase::Active));
        harness
    }

    fn write(&self, relative: &str, text: &str, mtime_ms: i64) {
        write_with_mtime(&self.vault.join(relative), text, mtime_ms);
    }

    fn scan(&mut self) -> ScanOutcome {
        self.engine.scan(&AtomicBool::new(false)).unwrap()
    }

    fn close_due(&mut self) -> usize {
        self.engine.close_due_windows(10_000).unwrap()
    }

    /// Close every due window and deliver it all as `Delivered`, returning the
    /// lines per file in delivery order.
    fn deliver_all(&mut self) -> Vec<(Delivery, Vec<Line>)> {
        self.close_due();
        let mut delivered = Vec::new();
        while let Some(delivery) = self.engine.next_delivery() {
            let lines = read_lines(&delivery.path);
            self.engine
                .record_answer(&delivery, Answer::Delivered)
                .unwrap();
            delivered.push((delivery, lines));
        }
        delivered
    }

    fn status(&self) -> engine::Status {
        self.engine.status(false, Some(true))
    }
}

fn read_lines(path: &Path) -> Vec<Line> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_with_mtime(path: &Path, text: &str, mtime_ms: i64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
    set_mtime(path, mtime_ms);
}

fn set_mtime(path: &Path, mtime_ms: i64) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_millis(mtime_ms as u64))
        .unwrap();
}

fn line_for<'a>(delivered: &'a [(Delivery, Vec<Line>)], path: &str) -> Vec<&'a Line> {
    delivered
        .iter()
        .flat_map(|(_, lines)| lines.iter())
        .filter(|line| line.path == path)
        .collect()
}

fn rfc(ms: i64) -> String {
    Stamp::new(ms, OFFSET).rfc3339()
}

#[test]
fn the_engine_reproduces_the_golden_file_from_real_notes() {
    let mut h = Harness::turned_on();
    // Settle two notes first: their first sight goes in an earlier window.
    h.write("inbox/old idea.md", "an old idea\n", T0 - 600_000);
    h.write("inbox/Alex.md", "Alex.\n", T0 - 600_000);
    h.scan();
    h.clock.set(T0 + 6 * 60_000); // 09:36, the first-sight window has closed
    let first = h.deliver_all();
    assert_eq!(first.len(), 1, "first sight of two notes in one window");

    // 09:42:33: an edit, mtime 09:42:10.
    h.clock.set(T0 + 12 * 60_000 + 33_000);
    h.write(
        "daily/2026-11-08.md",
        "---\ntags: [walk]\n---\nMet Alex by the river.\n",
        T0 + 12 * 60_000 + 10_000,
    );
    h.scan();

    // 09:43:33: a move with an edit, mtime 09:43:02.
    h.clock.set(T0 + 13 * 60_000 + 33_000);
    fs::create_dir_all(h.vault.join("people")).unwrap();
    fs::rename(
        h.vault.join("inbox/Alex.md"),
        h.vault.join("people/Alex Rivera.md"),
    )
    .unwrap();
    h.write(
        "people/Alex Rivera.md",
        "Alex likes rivers.\n",
        T0 + 13 * 60_000 + 2_000,
    );
    h.scan();

    // 09:44:33: a delete first seen; confirmed by the next scan, stamped first-seen.
    h.clock.set(T0 + 14 * 60_000 + 33_000);
    fs::remove_file(h.vault.join("inbox/old idea.md")).unwrap();
    h.scan();
    h.clock.set(T0 + 15 * 60_000 + 33_000);
    h.scan();

    // 09:46:30: the 09:40 window closes.
    h.clock.set(T0 + 16 * 60_000 + 30_000);
    h.close_due();
    let delivery = h.engine.next_delivery().expect("the golden window");
    assert_eq!(delivery.window.day, "20261108");
    assert_eq!(delivery.window.segment, "094000_300");
    assert_eq!(
        delivery.name,
        "obsidian_3e100d06fb18b4f45302cb3cf2cad1881e7183360649f0543ce4acf565b9f81e.jsonl"
    );
    let expected = concat!(
        r#"{"change":"changed","changed_at":"2026-11-08T09:42:10-07:00","content_hash":"f0b347123a86bae3e1d4d8c48a7f0430bed090b37d8ed1873a63a97c0b13dfa9","observed_at":"2026-11-08T09:42:33-07:00","path":"daily/2026-11-08.md","previous_path":null,"text":"---\ntags: [walk]\n---\nMet Alex by the river.\n","v":1,"vault":"Notes"}"#,
        "\n",
        r#"{"change":"removed","changed_at":"2026-11-08T09:44:33-07:00","content_hash":null,"observed_at":"2026-11-08T09:44:33-07:00","path":"inbox/old idea.md","previous_path":null,"text":null,"v":1,"vault":"Notes"}"#,
        "\n",
        r#"{"change":"renamed","changed_at":"2026-11-08T09:43:02-07:00","content_hash":"14fece546a747b07dec10e2658310b2d224447158a8c6e1b3a65d467b9de22a3","observed_at":"2026-11-08T09:43:33-07:00","path":"people/Alex Rivera.md","previous_path":"inbox/Alex.md","text":"Alex likes rivers.\n","v":1,"vault":"Notes"}"#,
        "\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&fs::read(&delivery.path).unwrap()),
        expected
    );
}

#[test]
fn a_first_scan_brings_in_every_note_at_its_mtime_with_progress() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.write("sub/b.MD", "beta", T0 - 7_200_000);
    h.write("skip.txt", "nope", T0 - 7_200_000);
    assert_eq!(h.scan(), ScanOutcome::Done);
    let status = h.status();
    assert_eq!(status.state, State::On);
    assert_eq!(status.waiting, 2);
    let sync = status.first_sync.expect("first sync in progress");
    assert_eq!((sync.sent, sync.found), (0, 2));
    assert!(status.last_checked_ms.is_some());

    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    assert_eq!(delivered.len(), 2, "two windows");
    let a = line_for(&delivered, "a.md");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].change, Change::Changed);
    assert_eq!(a[0].changed_at, rfc(T0 - 3_600_000));
    assert_eq!(a[0].observed_at, rfc(T0));
    assert_eq!(a[0].text.as_deref(), Some("alpha"));
    assert_eq!(
        a[0].content_hash.as_deref(),
        Some(wire::sha256_hex(b"alpha").as_str())
    );

    let status = h.status();
    assert_eq!(status.waiting, 0);
    assert!(
        status.first_sync.is_none(),
        "first sync completes once everything has gone"
    );
}

#[test]
fn delivery_is_oldest_window_first() {
    let mut h = Harness::turned_on();
    h.write("newer.md", "n", T0 - 900_000);
    h.write("oldest.md", "o", T0 - 9_000_000);
    h.write("middle.md", "m", T0 - 1_800_000);
    h.scan();
    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    let order: Vec<&str> = delivered
        .iter()
        .map(|(_, lines)| lines[0].path.as_str())
        .collect();
    assert_eq!(order, vec!["oldest.md", "middle.md", "newer.md"]);
}

#[test]
fn an_unchanged_note_is_never_read_again() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    // Make the note unreadable: a read would fail, but matching mtime+size means no read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(h.vault.join("a.md"), fs::Permissions::from_mode(0o000)).unwrap();
    }
    h.clock.advance_s(60);
    h.scan();
    assert_eq!(h.status().waiting, 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(h.vault.join("a.md"), fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn a_new_mtime_with_the_same_content_only_updates_the_row() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    h.write("a.md", "alpha", T0);
    h.clock.advance_s(60);
    h.scan();
    assert_eq!(h.status().waiting, 0, "only the row moved");
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert_eq!(table.rows["a.md"].mtime_ms, T0);
}

fn table_name(h: &Harness) -> String {
    let root = h.vault.to_str().unwrap();
    format!("{}.json", &wire::sha256_hex(root.as_bytes())[..16])
}

#[cfg(unix)]
#[test]
fn a_rename_pairs_by_file_id_even_with_new_content() {
    let mut h = Harness::turned_on();
    h.write("a.md", "before", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(600);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    // Same inode, different bytes.
    fs::write(h.vault.join("b.md"), "after").unwrap();
    h.scan();
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "b.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].change, Change::Renamed);
    assert_eq!(lines[0].previous_path.as_deref(), Some("a.md"));
    assert_eq!(lines[0].text.as_deref(), Some("after"));
    assert!(
        line_for(&delivered, "a.md").is_empty(),
        "no delete for a.md"
    );
    assert_eq!(h.status().waiting, 0);
}

#[test]
fn a_rename_that_loses_the_file_id_pairs_by_content() {
    // A sync tool's delete-then-create: a different file with the same text.
    let mut h = Harness::turned_on();
    h.write("old.md", "same words", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(600);
    let observed_time = h.clock.now();
    let moved_mtime = T0 - 3_000_000; // an arbitrary, older time from the sync tool
    fs::remove_file(h.vault.join("old.md")).unwrap();
    h.write("moved/new.md", "same words", moved_mtime);
    h.scan();
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "moved/new.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].change, Change::Renamed);
    assert_eq!(lines[0].previous_path.as_deref(), Some("old.md"));
    // Content unchanged: a rename is stamped at the observed time.
    assert_eq!(lines[0].changed_at, rfc(observed_time));
    assert!(line_for(&delivered, "old.md").is_empty());
}

/// One note to rename and one that links to it, settled by a scan at 09:39:30.
fn settled_for_a_rename() -> Harness {
    let mut h = Harness::turned_on();
    h.write("a.md", "stays the same", T0 - 3_600_000);
    h.write("links.md", "see [[a]]", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    h.clock.set(T0 + 9 * 60_000 + 30_000);
    h.scan();
    h
}

#[test]
fn a_rename_lands_at_or_before_the_edits_made_for_it() {
    let mut h = settled_for_a_rename();
    // Obsidian rewrites the link at 09:39:50, in the 09:35 window. The scan that
    // sees both comes at 09:40:20, in the 09:40 window.
    let edit = T0 + 9 * 60_000 + 50_000;
    let observed = T0 + 10 * 60_000 + 20_000;
    h.clock.set(observed);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.write("links.md", "see [[b]]", edit);
    h.scan();

    h.clock.set(T0 + 20 * 60_000);
    let delivered = h.deliver_all();
    let renamed = line_for(&delivered, "b.md");
    let linked = line_for(&delivered, "links.md");
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].change, Change::Renamed);
    assert_eq!(
        renamed[0].changed_at,
        rfc(edit),
        "at the edit, not the scan"
    );
    assert_eq!(renamed[0].observed_at, rfc(observed));
    assert!(renamed[0].changed_at <= linked[0].changed_at);
    let window = Stamp::new(edit, OFFSET).window();
    assert_eq!(window.segment, "093500_300");
    let same_file = delivered
        .iter()
        .find(|(delivery, _)| delivery.window == window)
        .expect("the earlier window");
    assert_eq!(
        same_file.1.len(),
        2,
        "rename and edit reach the journal together"
    );
}

#[test]
fn a_rename_takes_the_earliest_of_several_changed_notes() {
    let mut h = settled_for_a_rename();
    let earlier = T0 + 9 * 60_000 + 40_000;
    h.clock.set(T0 + 10 * 60_000 + 20_000);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.write("links.md", "see [[b]]", T0 + 10 * 60_000);
    h.write("new.md", "a new note", earlier);
    h.scan();
    h.clock.set(T0 + 20 * 60_000);
    let delivered = h.deliver_all();
    assert_eq!(line_for(&delivered, "b.md")[0].changed_at, rfc(earlier));
}

#[test]
fn a_rename_ignores_mtimes_not_newer_than_the_previous_scan_or_in_the_future() {
    let mut h = settled_for_a_rename();
    let observed = T0 + 10 * 60_000 + 20_000;
    h.clock.set(observed);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    // Older than the 09:39:30 scan, as a sync tool can leave it.
    h.write("links.md", "see [[b]]", T0 + 5 * 60_000);
    h.write("later.md", "from a fast clock", observed + 86_400_000);
    h.scan();
    h.clock.set(T0 + 20 * 60_000);
    let delivered = h.deliver_all();
    assert_eq!(
        line_for(&delivered, "b.md")[0].changed_at,
        rfc(observed),
        "no qualifying edit: the observed time"
    );
}

#[test]
fn the_previous_scan_time_survives_the_app_being_off() {
    let mut h = settled_for_a_rename();
    h.engine.turn_off().unwrap();
    h.engine = Engine::open(h.root.clone(), Arc::new(h.clock.clone())).unwrap();

    let edit = T0 + 60 * 60_000;
    let observed = T0 + 90 * 60_000;
    h.clock.set(observed);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.write("links.md", "see [[b]]", edit);
    h.engine.turn_on(&h.vault).unwrap();
    h.scan();
    h.clock.set(observed + 10 * 60_000);
    let delivered = h.deliver_all();
    assert_eq!(
        line_for(&delivered, "b.md")[0].changed_at,
        rfc(edit),
        "only a remembered previous scan lets the edit qualify"
    );
}

#[test]
fn a_delete_waits_one_scan_and_is_stamped_when_first_seen_missing() {
    let mut h = Harness::turned_on();
    h.write("gone.md", "bye", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(600);
    fs::remove_file(h.vault.join("gone.md")).unwrap();
    let first_missing = h.clock.now();
    h.scan();
    assert_eq!(h.status().waiting, 0, "held: nothing reported yet");

    h.clock.advance_s(60);
    h.scan();
    assert_eq!(h.status().waiting, 1, "the next scan confirms it");
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "gone.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].change, Change::Removed);
    assert_eq!(lines[0].changed_at, rfc(first_missing));
    assert_eq!(lines[0].observed_at, rfc(first_missing));
    assert_eq!(lines[0].text, None);
    assert_eq!(lines[0].content_hash, None);
}

#[test]
fn a_delete_then_create_never_reads_as_a_delete() {
    let mut h = Harness::turned_on();
    h.write("note.md", "body", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(600);
    fs::remove_file(h.vault.join("note.md")).unwrap();
    h.scan();
    // By the next scan a new path holds the same text.
    h.clock.advance_s(60);
    h.write("elsewhere/note.md", "body", h.clock.now());
    h.scan();
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    assert!(line_for(&delivered, "note.md").is_empty());
    let lines = line_for(&delivered, "elsewhere/note.md");
    assert_eq!(lines[0].change, Change::Renamed);
    assert_eq!(lines[0].previous_path.as_deref(), Some("note.md"));
}

#[test]
fn a_held_delete_returning_means_nothing_happened() {
    let mut h = Harness::turned_on();
    h.write("back.md", "same", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(600);
    fs::remove_file(h.vault.join("back.md")).unwrap();
    h.scan();
    h.clock.advance_s(60);
    h.write("back.md", "same", T0 - 3_600_000);
    h.scan();
    assert_eq!(h.status().waiting, 0);
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert!(table.held.is_empty());
}

#[test]
fn an_mtime_in_the_future_takes_the_observed_time() {
    let mut h = Harness::turned_on();
    h.write("future.md", "soon", T0 + 86_400_000);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "future.md");
    assert_eq!(lines[0].changed_at, rfc(T0));
    assert_eq!(lines[0].observed_at, rfc(T0));
}

#[test]
fn an_mtime_not_later_than_the_last_report_takes_the_observed_time() {
    let mut h = Harness::turned_on();
    h.write("a.md", "one", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    // A sync tool sets an older mtime on genuinely new content.
    h.clock.advance_s(600);
    let observed = h.clock.now();
    h.write("a.md", "two", T0 - 7_200_000);
    h.scan();
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "a.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text.as_deref(), Some("two"));
    assert_eq!(lines[0].changed_at, rfc(observed));
}

#[test]
fn a_vault_gone_reports_nothing_and_needs_attention() {
    let mut h = Harness::turned_on();
    h.write("keep.md", "still here", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    let parked = h.vault.with_extension("unmounted");
    fs::rename(&h.vault, &parked).unwrap();
    for _ in 0..3 {
        h.clock.advance_s(60);
        assert_eq!(h.scan(), ScanOutcome::Unreadable);
    }
    let status = h.status();
    assert_eq!(status.state, State::NeedsAttention);
    assert_eq!(status.attention, Some(crate::Attention::VaultUnreadable));
    assert_eq!(status.waiting, 0, "the vault's absence is never a delete");

    // It comes back: nothing was concluded while it was gone.
    fs::rename(&parked, &h.vault).unwrap();
    h.clock.advance_s(60);
    assert_eq!(h.scan(), ScanOutcome::Done);
    assert_eq!(h.status().state, State::On);
    assert_eq!(h.status().waiting, 0);

    // .obsidian alone going missing is the same as the vault being gone.
    fs::rename(h.vault.join(".obsidian"), h.vault.join(".obsidian-x")).unwrap();
    h.clock.advance_s(60);
    assert_eq!(h.scan(), ScanOutcome::Unreadable);
    assert_eq!(h.status().state, State::NeedsAttention);
}

#[test]
fn dot_folders_templates_and_other_files_are_never_reported() {
    let mut h = Harness::turned_on();
    h.write(".obsidian/app.md", "settings", T0 - 3_600_000);
    h.write(".trash/deleted.md", "x", T0 - 3_600_000);
    h.write("Templates/t.md", "x", T0 - 3_600_000);
    h.write("nested/_TEMPLATES/t.md", "x", T0 - 3_600_000);
    h.write("nested/.dotted/t.md", "x", T0 - 3_600_000);
    h.write("nested/keep.md", "x", T0 - 3_600_000);
    h.write("picture.png", "x", T0 - 3_600_000);
    h.write(".hidden.md", "x", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    let paths: Vec<&str> = delivered
        .iter()
        .flat_map(|(_, lines)| lines.iter().map(|line| line.path.as_str()))
        .collect();
    assert_eq!(paths, vec!["nested/keep.md"]);
    // Every reported path obeys the contract's path rules.
    for path in paths {
        assert!(!path.starts_with('/'));
        assert!(path.split('/').all(|segment| !segment.starts_with('.')));
    }
}

#[test]
fn saves_in_one_window_are_one_line_with_the_latest_text() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    // Inside one window (09:40-09:45), three saves, three scans.
    let base = T0 + 11 * 60_000; // 09:41
    for (index, text) in ["v1", "v2", "v3"].iter().enumerate() {
        h.clock.set(base + index as i64 * 30_000);
        h.write("a.md", text, base + index as i64 * 20_000);
        h.scan();
    }
    assert_eq!(h.status().waiting, 1, "one line, netted");
    h.clock.set(T0 + 17 * 60_000);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "a.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].change, Change::Changed);
    assert_eq!(lines[0].text.as_deref(), Some("v3"));
    assert_eq!(lines[0].changed_at, rfc(base + 2 * 20_000));
}

#[test]
fn a_rename_then_an_edit_in_a_window_nets_to_a_rename_with_the_latest_text() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    let base = T0 + 11 * 60_000;
    h.clock.set(base);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.scan();
    h.clock.set(base + 60_000);
    h.write("b.md", "v1", base + 50_000);
    h.scan();
    h.clock.set(T0 + 17 * 60_000);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "b.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].change, Change::Renamed);
    assert_eq!(lines[0].previous_path.as_deref(), Some("a.md"));
    assert_eq!(lines[0].text.as_deref(), Some("v1"));
    assert_eq!(lines[0].changed_at, rfc(base + 50_000));
}

#[test]
fn a_note_first_seen_and_removed_before_going_is_dropped() {
    let mut h = Harness::turned_on();
    let base = T0 + 11 * 60_000;
    h.clock.set(base);
    h.write("fleeting.md", "brief", base - 5_000);
    h.scan();
    assert_eq!(h.status().waiting, 1);
    assert_eq!(h.status().first_sync.unwrap().found, 1);

    h.clock.set(base + 60_000);
    fs::remove_file(h.vault.join("fleeting.md")).unwrap();
    h.scan();
    h.clock.set(base + 120_000);
    h.scan();
    assert_eq!(h.status().waiting, 0, "never reached anyone");
    assert_eq!(h.status().first_sync, None, "nothing left to bring in");
    h.clock.set(T0 + 17 * 60_000);
    assert!(h.deliver_all().is_empty());
}

#[test]
fn an_edit_then_a_delete_nets_to_the_delete_when_the_note_was_known() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    let base = T0 + 11 * 60_000;
    h.clock.set(base);
    h.write("a.md", "v1", base - 5_000);
    h.scan();
    h.clock.set(base + 60_000);
    fs::remove_file(h.vault.join("a.md")).unwrap();
    h.scan();
    h.clock.set(base + 120_000);
    h.scan();
    h.clock.set(T0 + 17 * 60_000);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "a.md");
    assert_eq!(lines.len(), 1, "one line per note per window");
    assert_eq!(lines[0].change, Change::Removed);
}

#[test]
fn a_change_to_an_already_sent_window_moves_later() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    let base = T0 + 11 * 60_000; // 09:41
    h.clock.set(base);
    h.write("a.md", "v1", base - 5_000);
    h.scan();
    h.clock.set(T0 + 17 * 60_000); // 09:47: 09:40 has gone
    let first = h.deliver_all();
    assert_eq!(first[0].1[0].text.as_deref(), Some("v1"));

    // A sync tool lands a second edit whose mtime falls in the window already sent.
    h.clock.set(T0 + 17 * 60_000 + 30_000);
    let observed = h.clock.now();
    h.write("a.md", "v2", base + 30_000);
    h.scan();
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert_eq!(
        table.rows["a.md"].sent_window.as_ref().unwrap().segment,
        "094000_300",
        "the row remembers the window that went"
    );
    h.clock.set(T0 + 26 * 60_000);
    let second = h.deliver_all();
    let lines = line_for(&second, "a.md");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text.as_deref(), Some("v2"));
    assert_eq!(lines[0].changed_at, rfc(observed));
    assert_ne!(
        Stamp::new(observed, OFFSET).window(),
        Stamp::new(base, OFFSET).window(),
        "it landed in a later window"
    );
}

#[test]
fn a_window_is_held_until_90_seconds_after_it_ends() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    let base = T0 + 11 * 60_000; // 09:41, window ends 09:45
    h.clock.set(base);
    h.write("a.md", "v1", base - 5_000);
    h.scan();
    let closes_at = T0 + 15 * 60_000 + 90_000;
    h.clock.set(closes_at - 1);
    assert_eq!(h.close_due(), 0);
    assert!(h.engine.next_delivery().is_none());
    h.clock.set(closes_at);
    assert_eq!(h.close_due(), 1);
    assert!(h.engine.next_delivery().is_some());
}

#[test]
fn pending_changes_survive_a_restart_and_close_after_it() {
    let mut h = Harness::turned_on();
    let base = T0 + 11 * 60_000;
    h.clock.set(base);
    h.write("a.md", "v0", base - 5_000);
    h.scan();
    assert_eq!(h.status().waiting, 1);

    let root = h.root.clone();
    let clock = h.clock.clone();
    h.engine = Engine::open(root, Arc::new(clock)).unwrap();
    assert_eq!(h.status().waiting, 1, "pending survived");
    assert!(h.status().first_sync.is_some());
    h.clock.set(T0 + 17 * 60_000);
    let delivered = h.deliver_all();
    assert_eq!(line_for(&delivered, "a.md").len(), 1);
}

#[test]
fn changes_made_while_off_are_found_when_turned_back_on() {
    let mut h = Harness::turned_on();
    h.write("edit.md", "v0", T0 - 3_600_000);
    h.write("rename.md", "stay", T0 - 3_600_000);
    h.write("delete.md", "bye", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    h.engine.turn_off().unwrap();
    assert_eq!(h.status().state, State::Off);

    // Time passes with the app off.
    h.clock.advance_s(3_600);
    let offline_edit = h.clock.now() - 30_000;
    h.write("edit.md", "v1", offline_edit);
    fs::rename(h.vault.join("rename.md"), h.vault.join("renamed.md")).unwrap();
    fs::remove_file(h.vault.join("delete.md")).unwrap();

    h.engine.turn_on(&h.vault).unwrap();
    assert_eq!(h.engine.phase(), Some(&Phase::Active), "no second warning");
    h.scan();
    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(500);
    let delivered = h.deliver_all();
    let edited = line_for(&delivered, "edit.md");
    assert_eq!(edited[0].text.as_deref(), Some("v1"));
    assert_eq!(edited[0].changed_at, rfc(offline_edit));
    let renamed = line_for(&delivered, "renamed.md");
    assert_eq!(renamed[0].change, Change::Renamed);
    assert_eq!(renamed[0].previous_path.as_deref(), Some("rename.md"));
    let removed = line_for(&delivered, "delete.md");
    assert_eq!(removed[0].change, Change::Removed);
}

#[test]
fn waiting_lines_stay_and_go_when_the_source_is_on_again() {
    let mut h = Harness::turned_on();
    h.write("a.md", "v0", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.close_due();
    assert_eq!(h.status().waiting, 1, "in the outbox");
    h.engine.turn_off().unwrap();
    assert_eq!(h.engine.status(false, None).waiting, 1);
    h.engine.turn_on(&h.vault).unwrap();
    assert_eq!(h.status().waiting, 1);
    let delivered = h.deliver_all();
    assert_eq!(line_for(&delivered, "a.md").len(), 1);
}

#[test]
fn a_note_over_the_size_limit_is_counted_and_never_sent() {
    let mut h = Harness::turned_on();
    h.write(
        "big.md",
        &"x".repeat(wire::MAX_NOTE_BYTES + 1),
        T0 - 3_600_000,
    );
    h.write("ok.md", "fine", T0 - 3_600_000);
    h.scan();
    assert_eq!(h.status().too_large, 1);
    assert_eq!(h.status().waiting, 1);
    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    assert!(line_for(&delivered, "big.md").is_empty());
}

#[test]
fn text_is_read_as_utf8_with_replacement_characters() {
    let mut h = Harness::turned_on();
    fs::write(h.vault.join("bad.md"), b"ab\xFFcd").unwrap();
    set_mtime(&h.vault.join("bad.md"), T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    let line = line_for(&delivered, "bad.md")[0];
    assert_eq!(line.text.as_deref(), Some("ab\u{FFFD}cd"));
    assert_eq!(
        line.content_hash.as_deref(),
        Some(wire::sha256_hex("ab\u{FFFD}cd".as_bytes()).as_str())
    );
}

#[test]
fn answers_delete_hold_set_aside_and_back_off() {
    let mut h = Harness::turned_on();
    h.write("a.md", "a", T0 - 3_600_000);
    h.write("b.md", "b", T0 - 7_200_000);
    h.scan();
    h.clock.advance_s(1);
    h.close_due();

    // Retry: nothing offered during backoff, then the same oldest file again.
    let first = h.engine.next_delivery().unwrap();
    assert_eq!(read_lines(&first.path)[0].path, "b.md");
    h.engine.record_answer(&first, Answer::Retry).unwrap();
    assert!(h.engine.next_delivery().is_none());
    h.clock.advance_s(31);
    assert_eq!(h.engine.next_delivery().unwrap(), first);

    // Ahead: held, then offered again after a while.
    h.engine.record_answer(&first, Answer::Ahead).unwrap();
    assert!(h.engine.next_delivery().is_none());
    h.clock.advance_s(5 * 60 + 1);
    assert_eq!(h.engine.next_delivery().unwrap(), first);

    // Invalid: kept aside, needs attention, the rest carry on.
    h.engine.record_answer(&first, Answer::Invalid).unwrap();
    assert!(!first.path.exists());
    assert_eq!(h.status().state, State::NeedsAttention);
    assert_eq!(h.status().attention, Some(crate::Attention::FileRefused));
    let second = h.engine.next_delivery().unwrap();
    assert_eq!(read_lines(&second.path)[0].path, "a.md");

    // Delivered, including what the journal says is segment_removed: deleted, not resent.
    assert_eq!(
        Answer::from_reply(Some("failed"), Some("segment_removed")),
        Answer::Delivered
    );
    h.engine.record_answer(&second, Answer::Delivered).unwrap();
    assert!(!second.path.exists());
    assert!(h.engine.next_delivery().is_none());
    assert_eq!(h.status().waiting, 0);
}

#[test]
fn a_segment_removed_answer_does_not_resend_the_change() {
    let mut h = Harness::turned_on();
    h.write("a.md", "a", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.close_due();
    let file = h.engine.next_delivery().unwrap();
    h.engine
        .record_answer(
            &file,
            Answer::from_reply(Some("failed"), Some("segment_removed")),
        )
        .unwrap();
    // Further scans of the unchanged note say nothing more.
    h.clock.advance_s(600);
    h.scan();
    h.close_due();
    assert!(h.engine.next_delivery().is_none());
    assert_eq!(h.status().waiting, 0);
}

#[test]
fn turning_on_with_a_vault_the_journal_has_waits_for_the_owner() {
    let mut h = Harness::new();
    h.engine.turn_on(&h.vault).unwrap();
    assert_eq!(h.status().state, State::Checking);
    assert!(!h.engine.should_scan(), "nothing happens until the check");
    h.engine
        .apply_vault_list(&[ListedVault {
            vault: "Notes".into(),
            device: Some("studio".into()),
            last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
        }])
        .unwrap();
    assert_eq!(h.status().state, State::Confirm);
    assert!(!h.engine.should_scan());
    let warning = h.status().warning.expect("the owner sees device and date");
    assert_eq!(warning.device.as_deref(), Some("studio"));

    h.engine.cancel_warning().unwrap();
    assert_eq!(h.status().state, State::Off);
    assert!(!h.engine.should_scan());
}

#[test]
fn continuing_past_the_warning_turns_it_on() {
    let mut h = Harness::new();
    h.engine.turn_on(&h.vault).unwrap();
    h.engine
        .apply_vault_list(&[ListedVault {
            vault: "Notes".into(),
            device: None,
            last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
        }])
        .unwrap();
    h.engine.continue_warning().unwrap();
    assert_eq!(h.engine.phase(), Some(&Phase::Active));
    assert!(h.engine.should_scan());
}

#[test]
fn a_different_vault_name_in_the_list_is_not_a_conflict() {
    let mut h = Harness::new();
    h.engine.turn_on(&h.vault).unwrap();
    h.engine
        .apply_vault_list(&[ListedVault {
            vault: "Other".into(),
            device: None,
            last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
        }])
        .unwrap();
    assert_eq!(h.engine.phase(), Some(&Phase::Active));
}

#[test]
fn a_folder_without_dot_obsidian_is_refused_and_leaves_the_source_off() {
    let mut h = Harness::new();
    let plain = h.vault.parent().unwrap().join("plain");
    fs::create_dir_all(&plain).unwrap();
    match h.engine.turn_on(&plain) {
        Err(TurnOnError::Refused(crate::Refusal::NotAVault)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(h.status().state, State::Off);
}

#[test]
fn turning_on_is_idempotent_while_on() {
    let mut h = Harness::turned_on();
    let other = h.vault.parent().unwrap().join("Other");
    fs::create_dir_all(other.join(".obsidian")).unwrap();
    h.engine.turn_on(&other).unwrap();
    assert_eq!(
        h.status().vault.as_deref(),
        Some("Notes"),
        "still the first"
    );
}

#[test]
fn held_material_names_no_journal() {
    // The engine is never given a journal identity, so nothing it writes can carry
    // one. This pins the shape of what it holds: only vault names, paths, text and times.
    let mut h = Harness::turned_on();
    h.write("a.md", "hello", T0 - 3_600_000);
    h.write("b.md", "world", T0 + 11 * 60_000 - 5_000);
    h.clock.set(T0 + 11 * 60_000);
    h.scan(); // a.md's window is due, b.md's is still pending
    h.clock.advance_s(1);
    h.close_due();
    assert!(h.status().waiting >= 2);

    let mut held = Vec::new();
    collect_files(&h.root, &mut held);
    assert!(held.iter().any(|path| path.ends_with("pending.log")));
    assert!(
        held.iter()
            .any(|path| path.to_string_lossy().contains("outbox"))
    );
    let identity = regex_free_identity_terms();
    for path in &held {
        let bytes = fs::read(path).unwrap();
        for term in &identity {
            assert!(
                !windows_contains(&bytes, term.as_bytes()),
                "{} carries {term}",
                path.display()
            );
        }
    }
    // The outbox files carry exactly the contract's keys.
    let delivery = h.engine.next_delivery().unwrap();
    for raw in fs::read_to_string(&delivery.path).unwrap().lines() {
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec![
                "change",
                "changed_at",
                "content_hash",
                "observed_at",
                "path",
                "previous_path",
                "text",
                "v",
                "vault"
            ]
        );
    }
}

fn regex_free_identity_terms() -> [&'static str; 6] {
    [
        "pairing",
        "journal_id",
        "instance",
        "cid",
        "certificate",
        "paired",
    ]
}

fn windows_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            collect_files(&entry.path(), out);
        } else {
            out.push(entry.path());
        }
    }
}

#[test]
fn a_full_run_leaves_every_vault_file_byte_identical() {
    let mut h = Harness::turned_on();
    h.write("daily/one.md", "first\n", T0 - 3_600_000);
    h.write("daily/two.md", "second\n", T0 - 3_000_000);
    h.write("to-rename.md", "stays\n", T0 - 2_000_000);
    h.write("to-delete.md", "goes\n", T0 - 2_000_000);
    fs::write(h.vault.join(".obsidian/config.json"), b"{\"theme\":\"x\"}").unwrap();
    h.write("Templates/t.md", "template", T0 - 2_000_000);
    h.write("picture.png", "png-bytes", T0 - 2_000_000);

    let before = snapshot(&h.vault);

    // A full run: first scan, delivery, edits and renames and deletes through scans,
    // windows closing, everything delivered.
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    h.clock.advance_s(600);
    h.scan();

    // Vault changes the *owner* makes are outside the app; the app itself makes none.
    let during = snapshot(&h.vault);
    assert_eq!(before, during, "scanning changes nothing in the vault");

    h.clock.advance_s(400);
    h.deliver_all();
    assert_eq!(before, snapshot(&h.vault));
    assert!(h.status().waiting == 0);
}

/// Every file under a directory with its bytes and mtime, plus every directory,
/// so a created, renamed, touched or rewritten entry anywhere shows up.
fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, Option<SystemTime>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if entry.file_type().unwrap().is_dir() {
                out.insert(format!("{relative}/"), (Vec::new(), None));
                stack.push(entry.path());
            } else {
                let metadata = entry.metadata().unwrap();
                out.insert(
                    relative,
                    (fs::read(entry.path()).unwrap(), metadata.modified().ok()),
                );
            }
        }
    }
    out
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Whether permissions bind here; as root they don't and the premise can't be set up.
#[cfg(unix)]
fn permissions_bind(temp: &Path) -> bool {
    let probe = temp.join("probe");
    fs::write(&probe, "x").unwrap();
    set_mode(&probe, 0o000);
    let binds = fs::read(&probe).is_err();
    set_mode(&probe, 0o600);
    fs::remove_file(&probe).unwrap();
    binds
}

#[cfg(unix)]
#[test]
fn a_folder_that_lists_but_cant_be_examined_concludes_nothing() {
    let mut h = Harness::turned_on();
    if !permissions_bind(h.vault.parent().unwrap()) {
        return;
    }
    h.write("folder/a.md", "kept", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    // Read but not execute: names list, and every lstat fails with a permission error.
    set_mode(&h.vault.join("folder"), 0o400);
    for _ in 0..3 {
        h.clock.advance_s(60);
        assert_eq!(h.scan(), ScanOutcome::Done);
    }
    let status = h.status();
    set_mode(&h.vault.join("folder"), 0o700);
    assert_eq!(status.waiting, 0, "an unreachable folder is never a delete");
    assert_eq!(status.attention, Some(crate::Attention::PartUnreadable));
    h.clock.advance_s(60);
    h.scan();
    assert_eq!(h.status().state, State::On);
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert!(table.rows.contains_key("folder/a.md"));
    assert!(table.held.is_empty());
}

#[cfg(unix)]
#[test]
fn a_delete_waits_while_an_unknown_folder_cant_be_read() {
    let mut h = Harness::turned_on();
    if !permissions_bind(h.vault.parent().unwrap()) {
        return;
    }
    h.write("a.md", "moving", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    fs::create_dir_all(h.vault.join("new")).unwrap();
    fs::rename(h.vault.join("a.md"), h.vault.join("new/a.md")).unwrap();
    set_mode(&h.vault.join("new"), 0o000);
    for _ in 0..3 {
        h.clock.advance_s(60);
        h.scan();
    }
    assert_eq!(h.status().waiting, 0, "held, not deleted");

    set_mode(&h.vault.join("new"), 0o700);
    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    assert!(line_for(&delivered, "a.md").is_empty());
    assert_eq!(line_for(&delivered, "new/a.md")[0].change, Change::Renamed);
}

#[cfg(unix)]
#[test]
fn a_failed_read_of_a_new_path_is_never_a_delete() {
    let mut h = Harness::turned_on();
    if !permissions_bind(h.vault.parent().unwrap()) {
        return;
    }
    h.write("by-id.md", "first", T0 - 3_600_000);
    h.write("by-hash.md", "second", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    fs::rename(h.vault.join("by-id.md"), h.vault.join("moved-id.md")).unwrap();
    fs::remove_file(h.vault.join("by-hash.md")).unwrap();
    h.write("moved-hash.md", "second", T0 - 3_000_000);
    set_mode(&h.vault.join("moved-id.md"), 0o000);
    set_mode(&h.vault.join("moved-hash.md"), 0o000);
    for _ in 0..3 {
        h.clock.advance_s(60);
        h.scan();
    }
    assert_eq!(h.status().waiting, 0);
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert!(table.rows.contains_key("by-id.md"), "the old row stands");
    assert!(table.rows.contains_key("by-hash.md"));
    assert!(table.held.is_empty(), "and no hold was started");

    set_mode(&h.vault.join("moved-id.md"), 0o600);
    set_mode(&h.vault.join("moved-hash.md"), 0o600);
    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    assert_eq!(
        line_for(&delivered, "moved-id.md")[0].change,
        Change::Renamed
    );
    assert_eq!(
        line_for(&delivered, "moved-hash.md")[0].change,
        Change::Renamed
    );
    assert!(line_for(&delivered, "by-id.md").is_empty());
    assert!(line_for(&delivered, "by-hash.md").is_empty());
}

#[test]
fn a_delete_after_a_rename_never_overwrites_a_live_note_at_the_old_name() {
    let mut h = Harness::turned_on();
    h.write("a.md", "original", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    // All inside the 09:40 window: a.md to b.md, a new a.md, then b.md goes.
    let base = T0 + 11 * 60_000;
    h.clock.set(base);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.scan();
    h.clock.set(base + 30_000);
    h.write("a.md", "a brand new note", base + 20_000);
    h.scan();
    h.clock.set(base + 60_000);
    fs::remove_file(h.vault.join("b.md")).unwrap();
    h.scan();
    h.clock.set(base + 90_000);
    h.scan();

    h.clock.set(T0 + 17 * 60_000);
    let delivered = h.deliver_all();
    assert!(
        line_for(&delivered, "b.md").is_empty(),
        "the rename netted away"
    );
    let lines = line_for(&delivered, "a.md");
    assert_eq!(lines.len(), 1, "no removed line for a.md");
    assert_eq!(lines[0].change, Change::Changed);
    assert_eq!(lines[0].text.as_deref(), Some("a brand new note"));
}

#[test]
fn a_read_failure_closing_a_window_keeps_it_and_only_bad_records_are_dropped() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.scan();
    h.engine.flush().unwrap();
    assert_eq!(h.status().waiting, 1);
    h.clock.advance_s(1);

    // A short log is a failed read, not a bad record: nothing is dropped.
    let log = h.root.join("pending.log");
    let bytes = fs::read(&log).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&log)
        .unwrap()
        .set_len(10)
        .unwrap();
    assert!(h.engine.close_due_windows(10).is_err());
    assert_eq!(h.status().waiting, 1, "still pending");

    // The same bytes back, and the window closes.
    fs::write(&log, &bytes).unwrap();
    assert_eq!(h.close_due(), 1);
    assert!(h.engine.next_delivery().is_some());

    // A record whose bytes no longer decode can't be sent, so the next scan reports
    // the note afresh rather than losing the change.
    let mut h = Harness::turned_on();
    h.write("b.md", "beta", T0 - 3_600_000);
    h.scan();
    h.engine.flush().unwrap();
    h.clock.advance_s(1);
    let log = h.root.join("pending.log");
    let mut bytes = fs::read(&log).unwrap();
    let text_at = bytes.windows(4).position(|w| w == b"beta").unwrap();
    // Break the JSON in place, keeping the record's length.
    bytes[text_at - 1] = b'!';
    fs::write(&log, &bytes).unwrap();
    assert_eq!(h.close_due(), 1);
    assert!(h.engine.next_delivery().is_none(), "nothing to send yet");
    assert_eq!(h.status().waiting, 0);

    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "b.md");
    assert_eq!(lines.len(), 1, "reported again");
    assert_eq!(lines[0].text.as_deref(), Some("beta"));
    assert_eq!(h.status().first_sync, None, "and the first sync can finish");
}

#[test]
fn a_note_that_grew_past_the_limit_and_moved_is_not_deleted() {
    let mut h = Harness::turned_on();
    h.write("n.md", "small", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    h.clock.advance_s(60);
    let big = "y".repeat(wire::MAX_NOTE_BYTES + 1);
    fs::write(h.vault.join("n.md"), &big).unwrap();
    h.scan();
    fs::rename(h.vault.join("n.md"), h.vault.join("moved.md")).unwrap();
    for _ in 0..3 {
        h.clock.advance_s(120);
        h.scan();
    }
    assert_eq!(h.status().too_large, 1);
    assert_eq!(h.status().waiting, 0, "not a delete");

    // Once small again it pairs up as the rename it was.
    fs::write(h.vault.join("moved.md"), "small again").unwrap();
    h.clock.advance_s(120);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    assert!(line_for(&delivered, "n.md").is_empty());
    let lines = line_for(&delivered, "moved.md");
    assert_eq!(lines[0].change, Change::Renamed);
    assert_eq!(lines[0].previous_path.as_deref(), Some("n.md"));
}

#[test]
fn a_note_that_grew_past_the_limit_and_then_went_away_is_deleted() {
    let mut h = Harness::turned_on();
    h.write("n.md", "small", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();
    h.clock.advance_s(60);
    fs::write(h.vault.join("n.md"), "y".repeat(wire::MAX_NOTE_BYTES + 1)).unwrap();
    h.scan();
    fs::remove_file(h.vault.join("n.md")).unwrap();
    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(60);
    h.scan();
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    assert_eq!(line_for(&delivered, "n.md")[0].change, Change::Removed);
}

#[test]
fn the_same_path_waiting_in_two_vaults_both_reach_the_journal() {
    let mut h = Harness::turned_on();
    let mtime = T0 - 30_000;
    h.write("Welcome.md", "vault A words", mtime);
    h.scan();
    h.engine.turn_off().unwrap();
    let other = h.vault.parent().unwrap().join("Other");
    fs::create_dir_all(other.join(".obsidian")).unwrap();
    write_with_mtime(&other.join("Welcome.md"), "vault B words", mtime);
    h.engine.turn_on(&other).unwrap();
    h.engine.apply_vault_list(&[]).unwrap();
    h.scan();
    assert_eq!(h.status().waiting, 2);
    h.clock.advance_s(400);
    let delivered = h.deliver_all();
    let mut texts: Vec<String> = delivered
        .iter()
        .flat_map(|(_, lines)| lines.iter())
        .map(|line| format!("{}: {}", line.vault, line.text.clone().unwrap()))
        .collect();
    texts.sort();
    assert_eq!(texts, ["Notes: vault A words", "Other: vault B words"]);
}

#[test]
fn a_clock_stepped_back_does_not_hold_a_change_unseen() {
    let mut h = Harness::turned_on();
    h.write("n.md", "v1", T0 - 10_000);
    h.scan();
    h.clock.advance_s(600);
    h.deliver_all();

    h.clock.set(T0 - 6 * 3_600_000);
    h.write("n.md", "v2 longer", h.clock.now() - 1_000);
    h.clock.advance_s(70);
    let observed = h.clock.now();
    h.scan();
    assert_eq!(h.status().waiting, 1, "reported, not deferred");
    h.clock.advance_s(600);
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "n.md");
    assert_eq!(lines[0].text.as_deref(), Some("v2 longer"));
    assert_eq!(lines[0].changed_at, rfc(observed));
}

#[test]
fn a_rename_is_never_stamped_before_the_notes_last_report() {
    let mut h = Harness::turned_on();
    let reported = T0 + 9 * 60_000 + 50_000;
    h.clock.set(reported + 10_000);
    h.write("a.md", "words", reported);
    h.write("links.md", "see [[a]]", T0 - 3_600_000);
    h.scan();
    // The previous scan's time is before the note's last report, and another note
    // changed between the two.
    let scan_file = h
        .root
        .join("tables")
        .join(table_name(&h))
        .with_extension("scan");
    fs::write(&scan_file, (reported - 60_000).to_string()).unwrap();
    h.clock.advance_s(20);
    fs::rename(h.vault.join("a.md"), h.vault.join("b.md")).unwrap();
    h.write("links.md", "see [[b]]", reported - 30_000);
    h.scan();
    h.clock.set(T0 + 20 * 60_000);
    let delivered = h.deliver_all();
    assert_eq!(line_for(&delivered, "b.md")[0].changed_at, rfc(reported));
}

#[cfg(unix)]
#[test]
fn an_unreadable_new_note_needs_attention_and_holds_only_notes_of_its_size() {
    let mut h = Harness::turned_on();
    if !permissions_bind(h.vault.parent().unwrap()) {
        return;
    }
    h.write("doomed.md", "this one is really gone", T0 - 3_600_000);
    h.write("same size.md", "12345", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.deliver_all();

    fs::remove_file(h.vault.join("doomed.md")).unwrap();
    fs::remove_file(h.vault.join("same size.md")).unwrap();
    h.write("locked.md", "abcde", h.clock.now() - 1_000);
    set_mode(&h.vault.join("locked.md"), 0o000);
    for _ in 0..3 {
        h.clock.advance_s(120);
        h.scan();
    }
    let status = h.status();
    set_mode(&h.vault.join("locked.md"), 0o600);
    assert_eq!(status.attention, Some(crate::Attention::PartUnreadable));
    assert_eq!(status.waiting, 1, "the other delete isn't blocked");
    let table = Table::load(&h.root.join("tables").join(table_name(&h))).unwrap();
    assert!(
        table.rows.contains_key("same size.md"),
        "still may have moved"
    );
    assert!(!table.rows.contains_key("doomed.md"));
    assert!(table.held.is_empty(), "no hold on the same-size note");
}

#[test]
fn a_set_aside_file_gets_another_try_at_startup_and_on_turn_on() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.close_due();
    let file = h.engine.next_delivery().unwrap();
    let first_lines = fs::read(&file.path).unwrap();
    h.engine.record_answer(&file, Answer::Invalid).unwrap();
    assert_eq!(h.status().state, State::NeedsAttention);

    // Startup.
    h.engine = Engine::open(h.root.clone(), Arc::new(h.clock.clone())).unwrap();
    assert_eq!(h.status().state, State::On);
    let back = h.engine.next_delivery().expect("back in the outbox");
    assert_eq!(fs::read(&back.path).unwrap(), first_lines);

    // Turn-on.
    h.engine.record_answer(&back, Answer::Invalid).unwrap();
    assert_eq!(h.status().state, State::NeedsAttention);
    h.engine.turn_off().unwrap();
    h.engine.turn_on(&h.vault).unwrap();
    assert_eq!(h.status().state, State::On);
    let again = h.engine.next_delivery().expect("back again");
    h.engine.record_answer(&again, Answer::Delivered).unwrap();
    assert_eq!(h.status().waiting, 0);
    assert!(h.status().first_sync.is_none(), "and first sync can finish");
}

#[test]
fn cancelling_a_warning_leaves_everything_else_waiting() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    h.scan();
    h.clock.advance_s(1);
    h.close_due();
    h.write("b.md", "still pending", T0);
    h.clock.advance_s(60);
    h.scan();
    let waiting = h.status().waiting;
    assert_eq!(waiting, 2, "one in the outbox, one pending");
    h.engine.turn_off().unwrap();

    let other = h.vault.parent().unwrap().join("Other");
    fs::create_dir_all(other.join(".obsidian")).unwrap();
    write_with_mtime(&other.join("c.md"), "never scanned", T0 - 3_600_000);
    h.engine.turn_on(&other).unwrap();
    h.engine
        .apply_vault_list(&[ListedVault {
            vault: "Other".into(),
            device: None,
            last_synced_at: None,
        }])
        .unwrap();
    assert_eq!(h.status().state, State::Confirm);
    assert_eq!(
        h.status().waiting,
        waiting,
        "the new vault adds nothing yet"
    );
    h.engine.cancel_warning().unwrap();
    assert_eq!(h.status().state, State::Off);
    assert_eq!(
        h.status().waiting,
        waiting,
        "and cancelling takes nothing away"
    );
}

#[test]
fn scan_intervals_stay_near_one_percent_of_a_core() {
    assert_eq!(
        engine::next_scan_delay(Duration::from_millis(200)),
        Duration::from_secs(60)
    );
    assert_eq!(
        engine::next_scan_delay(Duration::from_millis(600)),
        Duration::from_secs(60)
    );
    assert_eq!(
        engine::next_scan_delay(Duration::from_millis(601)),
        Duration::from_millis(60_100)
    );
    assert_eq!(
        engine::next_scan_delay(Duration::from_secs(2)),
        Duration::from_secs(200)
    );
    assert_eq!(
        engine::next_scan_delay(Duration::from_secs(3600)),
        Duration::from_secs(15 * 60),
        "capped"
    );
}

#[test]
fn a_scan_cancelled_by_turning_off_resumes_without_duplicate_lines() {
    let mut h = Harness::turned_on();
    // Enough notes that the walk checks the flag partway through.
    for index in 0..1500 {
        h.write(
            &format!("n{index}.md"),
            &format!("note {index}"),
            T0 - 3_600_000,
        );
    }
    let cancelled = AtomicBool::new(true);
    assert_eq!(h.engine.scan(&cancelled).unwrap(), ScanOutcome::Cancelled);
    assert_eq!(h.status().waiting, 0, "cancelled before reading anything");
    assert_eq!(h.scan(), ScanOutcome::Done);
    h.clock.advance_s(1);
    let delivered = h.deliver_all();
    let lines: Vec<&Line> = delivered.iter().flat_map(|(_, l)| l.iter()).collect();
    assert_eq!(lines.len(), 1500, "one line per note");
}

#[test]
fn a_file_gone_from_the_outbox_sends_its_notes_again_and_says_so() {
    let mut h = Harness::turned_on();
    h.write("a.md", "alpha", T0 - 3_600_000);
    assert_eq!(h.scan(), ScanOutcome::Done);
    h.clock.advance_s(10 * 60);
    assert_eq!(h.close_due(), 1);
    let delivery = h.engine.next_delivery().unwrap();
    fs::remove_file(&delivery.path).unwrap();
    h.engine.record_file_missing(&delivery).unwrap();
    assert!(h.engine.next_delivery().is_none(), "the entry is dropped");
    assert_eq!(h.status().attention, Some(crate::Attention::FileRefused));

    h.clock.advance_s(60);
    assert_eq!(h.scan(), ScanOutcome::Done);
    h.clock.advance_s(10 * 60);
    h.close_due();
    let delivered = h.deliver_all();
    let lines = line_for(&delivered, "a.md");
    assert_eq!(lines.len(), 1, "the note is reported again");
    assert_eq!(lines[0].text.as_deref(), Some("alpha"));
    assert_eq!(lines[0].change, Change::Changed);
    assert_eq!(
        h.status().attention,
        None,
        "delivered again, so nothing to flag"
    );
}
