// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime};

const GNOME: &[u8] = include_bytes!("../../fixtures/screenshot-gnome-50.png");
const SPECTACLE: &[u8] = include_bytes!("../../fixtures/screenshot-spectacle-6.png");
/// 2026-11-13 12:00:00 UTC.
const NOW: i64 = 1_794_571_200_000;
const HOUR: i64 = 3_600_000;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn offset_s_at(&self, _ms: i64) -> i32 {
        -7 * 3600
    }
}

struct Desk {
    _temp: tempfile::TempDir,
    home: PathBuf,
    shots: PathBuf,
    state: PathBuf,
    clock: Arc<TestClock>,
    taken_in: Arc<Mutex<Vec<String>>>,
}

impl Desk {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let shots = home.join("Pictures/Screenshots");
        fs::create_dir_all(&shots).unwrap();
        fs::create_dir_all(home.join(".config")).unwrap();
        fs::create_dir_all(home.join(".local/share")).unwrap();
        Self {
            state: temp.path().join("state"),
            _temp: temp,
            home,
            shots,
            clock: Arc::new(TestClock(AtomicI64::new(NOW))),
            taken_in: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn engine(&self) -> Engine {
        let places = Places {
            config: self.home.join(".config"),
            data: self.home.join(".local/share"),
            home: self.home.clone(),
            locale_dirs: Vec::new(),
            languages: Vec::new(),
        };
        let mut engine = Engine::open(self.state.clone(), places, self.clock.clone());
        let taken_in = Arc::clone(&self.taken_in);
        engine.set_intake_hook(Arc::new(move |id| {
            taken_in.lock().unwrap().push(id.to_owned())
        }));
        engine
    }

    fn at(&self, ms: i64) {
        self.clock.0.store(ms, Ordering::SeqCst);
    }

    /// A GNOME screenshot taken at `ms`, made unique by a trailing chunk-free byte run.
    fn gnome(&self, name: &str, ms: i64) -> PathBuf {
        let mut bytes = GNOME.to_vec();
        bytes.extend_from_slice(name.as_bytes());
        self.write(name, &bytes, ms)
    }

    fn photo(&self, name: &str, ms: i64) -> PathBuf {
        let mut bytes = SPECTACLE.to_vec();
        bytes.extend_from_slice(name.as_bytes());
        self.write(name, &bytes, ms)
    }

    /// A Spectacle screenshot: nothing in the file, and the desktop's record naming it.
    fn spectacle(&self, name: &str, ms: i64) -> PathBuf {
        let path = self.photo(name, ms);
        let recents = self.home.join(".local/share/recently-used.xbel");
        let mut text = fs::read_to_string(&recents).unwrap_or_else(|_| {
            "<?xml version=\"1.0\"?>\n<xbel version=\"1.0\" xmlns:bookmark=\"http://www.freedesktop.org/standards/desktop-bookmarks\">\n</xbel>\n".to_owned()
        });
        let added = chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339();
        let href = url::Url::from_file_path(&path).unwrap();
        let entry = format!(
            "  <bookmark href=\"{href}\" added=\"{added}\"><info><metadata owner=\"http://freedesktop.org\"><bookmark:applications><bookmark:application name=\"org.kde.spectacle\" exec=\"spectacle %f\" count=\"1\"/></bookmark:applications></metadata></info></bookmark>\n"
        );
        text = text.replace("</xbel>", &format!("{entry}</xbel>"));
        fs::write(&recents, text).unwrap();
        path
    }

    fn write(&self, name: &str, bytes: &[u8], ms: i64) -> PathBuf {
        let path = self.shots.join(name);
        fs::write(&path, bytes).unwrap();
        let time = SystemTime::UNIX_EPOCH + Duration::from_millis(ms as u64);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
        path
    }
}

fn names(engine: &Engine, state: ItemState) -> Vec<String> {
    let mut names = engine
        .ledger()
        .items
        .values()
        .filter(|item| item.state == state)
        .map(|item| {
            item.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn deliver_all(engine: &mut Engine) -> Vec<String> {
    let mut sent = Vec::new();
    while let Some(delivery) = engine.next_delivery() {
        sent.push(
            delivery
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        engine.record(&delivery.id, Outcome::Delivered).unwrap();
    }
    sent.sort();
    sent
}

#[test]
fn from_a_folder_of_photos_only_the_screenshots_are_counted_and_come_in() {
    let desk = Desk::new();
    desk.gnome("Screenshot From 2026-11-13 04-00-00.png", NOW - HOUR);
    desk.spectacle("Screenshot_20261113_040100.png", NOW - HOUR);
    desk.photo("IMG_2041.png", NOW - HOUR);
    desk.photo("Screenshot_20261113_040200.png", NOW - HOUR);
    desk.write("holiday.jpg", b"\xff\xd8\xff\xe0 photo", NOW - HOUR);
    desk.write("notes.txt", b"text", NOW - HOUR);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    assert_eq!(engine.count(Span::Last30Days), Some(2));
    assert!(
        engine.next_delivery().is_none(),
        "nothing goes before the confirm"
    );
    assert!(engine.confirm(Span::Last30Days).unwrap());
    assert_eq!(
        deliver_all(&mut engine),
        vec![
            "Screenshot From 2026-11-13 04-00-00.png",
            "Screenshot_20261113_040100.png"
        ]
    );
    assert_eq!(desk.taken_in.lock().unwrap().len(), 2);
}

#[test]
fn a_spectacle_screenshot_keeps_the_moment_the_desktop_recorded() {
    let desk = Desk::new();
    let saved = NOW - 3 * HOUR + 224;
    desk.spectacle("Screenshot_20261113_020000.png", saved);
    // The file's own time moved later, as a copy would move it.
    fs::File::options()
        .write(true)
        .open(desk.shots.join("Screenshot_20261113_020000.png"))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_millis((NOW - HOUR) as u64))
        .unwrap();
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    let delivery = engine.next_delivery().unwrap();
    assert_eq!(delivery.captured_ms, saved);
    assert_eq!(delivery.taken_from, TakenFrom::Platform);
    assert_eq!(delivery.offset_s, -7 * 3600);
}

#[test]
fn screenshots_from_before_the_chosen_span_stay_out_after_the_confirm() {
    let desk = Desk::new();
    desk.gnome("recent.png", NOW - 2 * DAY_MS);
    desk.gnome("old.png", NOW - 40 * DAY_MS);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    assert_eq!(engine.count(Span::Last30Days), Some(1));
    assert_eq!(engine.count(Span::All), Some(2));
    assert_eq!(engine.count(Span::Last30Days), Some(1));
    engine.confirm(Span::Last30Days).unwrap();
    engine.tick().unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["recent.png"]);
    desk.at(NOW + HOUR);
    desk.gnome("new.png", NOW + HOUR - 10_000);
    engine.tick().unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["new.png"]);
}

#[test]
fn a_pause_skips_and_a_resume_starts_from_the_moment_of_resume() {
    let desk = Desk::new();
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    desk.at(NOW + 10 * 60_000);
    engine.set_paused(true).unwrap();
    desk.gnome("while-paused.png", NOW + 11 * 60_000);
    desk.at(NOW + 12 * 60_000);
    engine.tick().unwrap();
    engine.set_paused(false).unwrap();
    desk.gnome("after-resume.png", NOW + 12 * 60_000 + 1_000);
    desk.at(NOW + 13 * 60_000);
    engine.tick().unwrap();
    // The app quits and opens again: the paused window is still skipped.
    let mut engine = desk.engine();
    engine.tick().unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["after-resume.png"]);
    assert_eq!(desk.taken_in.lock().unwrap().len(), 1);
}

/// Each write of the record replaces the file, so a new inode means a write.
fn record_inode(desk: &Desk) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(desk.state.join(LEDGER)).unwrap().ino()
}

#[test]
fn looks_that_find_nothing_new_leave_the_record_alone() {
    let desk = Desk::new();
    desk.gnome("one.png", NOW - HOUR);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    deliver_all(&mut engine);
    engine.tick().unwrap();
    let before = record_inode(&desk);
    for look in 1..=100 {
        desk.at(NOW + look * 5_000);
        engine.tick().unwrap();
    }
    assert_eq!(
        record_inode(&desk),
        before,
        "100 unchanged looks wrote the record"
    );
    assert_eq!(engine.ledger().last_checked_ms, Some(NOW + 100 * 5_000));
    // A new screenshot still lands in the record.
    desk.gnome("two.png", NOW + 400_000);
    desk.at(NOW + 101 * 5_000);
    engine.tick().unwrap();
    assert_ne!(record_inode(&desk), before);
    let reopened = desk.engine();
    assert_eq!(names(&reopened, ItemState::Waiting), vec!["two.png"]);
}

#[test]
fn what_was_taken_while_the_app_was_not_running_comes_in_once() {
    let desk = Desk::new();
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    drop(engine);
    desk.gnome("while-quit.png", NOW + 60_000);
    desk.at(NOW + 5 * 60_000);
    let mut engine = desk.engine();
    engine.tick().unwrap();
    engine.tick().unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["while-quit.png"]);
    engine.tick().unwrap();
    assert!(engine.next_delivery().is_none());
}

#[test]
fn a_second_backfill_counts_only_its_own_screenshots() {
    let desk = Desk::new();
    for day in 1..=3 {
        desk.gnome(&format!("d{day}.png"), NOW - day * DAY_MS);
    }
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Last30Days);
    engine.confirm(Span::Last30Days).unwrap();
    deliver_all(&mut engine);
    assert_eq!(engine.progress(), None);
    engine.turn_off().unwrap();
    desk.gnome("d40.png", NOW - 40 * DAY_MS);
    engine.turn_on().unwrap();
    assert!(engine.ledger().returning);
    assert_eq!(engine.count(Span::All), Some(4));
    engine.confirm(Span::All).unwrap();
    assert_eq!(engine.progress(), Some((0, 4)));
    let first = engine.next_delivery().unwrap();
    engine.record(&first.id, Outcome::Delivered).unwrap();
    assert_eq!(engine.progress(), Some((1, 4)));
}

#[test]
fn a_new_pairing_asks_again_and_resends_what_the_first_journal_has() {
    let desk = Desk::new();
    desk.gnome("a.png", NOW - DAY_MS);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Last30Days);
    engine.confirm(Span::Last30Days).unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["a.png"]);
    engine.restart_setup().unwrap();
    assert_eq!(engine.ledger().mode, Mode::Choosing);
    desk.gnome("b.png", NOW - 60_000);
    engine.tick().unwrap();
    assert!(
        engine.next_delivery().is_none(),
        "nothing goes until the confirm"
    );
    assert_eq!(engine.count(Span::Last30Days), Some(2));
    engine.confirm(Span::Last30Days).unwrap();
    assert_eq!(deliver_all(&mut engine), vec!["a.png", "b.png"]);
}

#[test]
fn a_new_pairing_leaves_a_source_that_is_off_alone() {
    let desk = Desk::new();
    let mut engine = desk.engine();
    engine.restart_setup().unwrap();
    assert_eq!(engine.ledger().mode, Mode::NotSetUp);
}

#[test]
fn a_screenshot_deleted_before_it_was_sent_never_goes_and_the_count_completes() {
    let desk = Desk::new();
    desk.gnome("kept.png", NOW - DAY_MS);
    let gone = desk.gnome("deleted.png", NOW - DAY_MS);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Last30Days);
    engine.confirm(Span::Last30Days).unwrap();
    fs::remove_file(gone).unwrap();
    engine.tick().unwrap();
    assert_eq!(engine.progress(), Some((1, 2)));
    assert_eq!(deliver_all(&mut engine), vec!["kept.png"]);
    assert_eq!(engine.progress(), None);
    assert_eq!(names(&engine, ItemState::Gone), vec!["deleted.png"]);
}

#[test]
fn turning_back_on_with_a_shorter_span_leaves_older_held_screenshots_out() {
    let desk = Desk::new();
    desk.gnome("old.png", NOW - 10 * DAY_MS);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Last30Days);
    engine.confirm(Span::Last30Days).unwrap();
    // The journal never took it, and the owner turns the source off and on.
    engine.turn_off().unwrap();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    assert!(engine.next_delivery().is_none());
    assert_eq!(names(&engine, ItemState::Left), vec!["old.png"]);
}

#[test]
fn a_file_still_being_written_waits_for_the_next_look() {
    let desk = Desk::new();
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    desk.gnome("fresh.png", NOW - 500);
    engine.tick().unwrap();
    assert!(engine.next_delivery().is_none());
    desk.at(NOW + 5_000);
    engine.tick().unwrap();
    assert!(engine.next_delivery().is_some());
}

#[test]
fn a_held_screenshot_waits_longer_each_time() {
    let desk = Desk::new();
    desk.gnome("a.png", NOW - HOUR);
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    engine.count(Span::Today);
    engine.confirm(Span::Today).unwrap();
    let delivery = engine.next_delivery().unwrap();
    engine.record(&delivery.id, Outcome::Hold).unwrap();
    assert!(engine.next_delivery().is_none());
    desk.at(NOW + 31_000);
    assert!(engine.next_delivery().is_some());
    engine.record(&delivery.id, Outcome::Hold).unwrap();
    desk.at(NOW + 62_000);
    assert!(engine.next_delivery().is_none());
    engine.retry().unwrap();
    assert!(engine.next_delivery().is_some());
}

#[test]
fn the_folder_going_after_it_was_there_is_a_fault_and_never_being_there_is_not() {
    let desk = Desk::new();
    fs::remove_dir(&desk.shots).unwrap();
    let mut engine = desk.engine();
    engine.turn_on().unwrap();
    assert_eq!(engine.count(Span::Today), Some(0));
    engine.confirm(Span::Today).unwrap();
    engine.tick().unwrap();
    assert!(!engine.folder_gone());
    fs::create_dir_all(&desk.shots).unwrap();
    engine.tick().unwrap();
    fs::remove_dir(&desk.shots).unwrap();
    engine.tick().unwrap();
    assert!(engine.folder_gone());
}

#[test]
fn each_span_is_named_as_the_window_names_it() {
    for span in Span::ALL {
        assert_eq!(Span::parse(span.as_str()), Some(span));
    }
}

#[test]
fn an_unreadable_record_starts_over_at_set_up() {
    let desk = Desk::new();
    fs::create_dir_all(&desk.state).unwrap();
    fs::write(desk.state.join(LEDGER), b"{not json").unwrap();
    assert_eq!(desk.engine().ledger().mode, Mode::NotSetUp);
}
