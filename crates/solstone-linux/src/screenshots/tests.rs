// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use super::*;
use crate::observer::{Mode as ObserverMode, StateSnapshot};
use crate::shell_model::StateWord;
use std::collections::VecDeque;
use std::fs;
use std::time::{Duration as StdDuration, SystemTime};

const GNOME: &[u8] = include_bytes!("../fixtures/screenshot-gnome-50.png");

/// The journal contract's golden envelopes (`x-sources.screenshots.golden`, client
/// ingest contract bundle 15.3.0), byte for byte.
const GOLDEN: [&str; 3] = [
    r#"{"day":"20261103","files":[{"submitted":"screenshot_screen.png"}],"meta":{"screenshot":{"captured_at":"2026-11-03T14:32:07-07:00","taken_from":"platform","v":1}},"segment":"143207_1","source":"screenshots"}"#,
    r#"{"day":"20261104","files":[{"submitted":"screenshot_screen.jpg"}],"meta":{"screenshot":{"captured_at":"2026-11-04T09:15:02+01:00","taken_from":"file","v":1}},"segment":"091502_1","source":"screenshots"}"#,
    r#"{"day":"20261105","files":[{"submitted":"screenshot_screen.webp"}],"meta":{"screenshot":{"captured_at":"2026-11-05T00:00:00Z","taken_from":"platform","v":1}},"segment":"000000_1","source":"screenshots"}"#,
];

fn result(success: bool, status: Option<u16>, reason: Option<&str>) -> UploadResult {
    UploadResult {
        success,
        error_type: None,
        status_code: status,
        stored_key: None,
        file_descriptors: None,
        reason_code: reason.map(str::to_owned),
        is_local_failure: false,
        captured_identity_key: None,
        captured_pairing_id: None,
        association_epoch: None,
        stale_capability: false,
    }
}

#[test]
fn each_answer_does_what_the_contract_says() {
    assert_eq!(
        outcome_for(&result(true, Some(200), None)),
        Outcome::Delivered
    );
    for (status, reason) in [
        (Some(400), Some("screenshot_invalid")),
        (Some(400), Some("multipart_part_too_large")),
        (Some(400), None),
        (Some(413), None),
    ] {
        assert_eq!(
            outcome_for(&result(false, status, reason)),
            Outcome::CantComeIn
        );
    }
    for (status, reason) in [
        (Some(409), Some("screenshot_ahead")),
        (Some(503), None),
        (Some(500), None),
        (Some(409), Some("foreign_stream_binding")),
        (Some(426), None),
        (Some(403), None),
        (None, None),
    ] {
        assert_eq!(outcome_for(&result(false, status, reason)), Outcome::Hold);
    }
    let mut stale = result(true, Some(200), None);
    stale.stale_capability = true;
    assert_eq!(outcome_for(&stale), Outcome::Hold);
}

#[test]
fn the_envelope_is_the_contracts_golden_one() {
    let cases = [
        (1_793_741_527_000, -7 * 3600, "png", "platform"),
        (1_793_780_102_000, 3600, "jpg", "file"),
        (1_793_836_800_000, 0, "webp", "platform"),
    ];
    for ((ms, offset, extension, taken_from), golden) in cases.into_iter().zip(GOLDEN) {
        let (day, segment, captured_at) = capture_moment(ms, offset);
        let meta = crate::upload::ScreenshotMeta {
            v: 1,
            captured_at,
            taken_from,
        };
        let ours: serde_json::Value = serde_json::from_str(&crate::upload::screenshot_envelope(
            &day,
            &segment,
            &format!("screenshot_screen.{extension}"),
            &meta,
        ))
        .unwrap();
        let golden: serde_json::Value = serde_json::from_str(golden).unwrap();
        assert_eq!(ours, golden);
    }
}

fn delivery(path: &Path, bytes: &[u8]) -> Delivery {
    Delivery {
        id: engine::hash_bytes(bytes),
        path: path.to_path_buf(),
        captured_ms: 1_794_571_200_000,
        offset_s: -7 * 3600,
        taken_from: TakenFrom::File,
    }
}

#[test]
fn an_accepted_type_goes_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("Screenshot From 2026-11-13 05-00-00.png");
    fs::write(&path, GNOME).unwrap();
    let staged = stage(
        &delivery(&path, GNOME),
        &temp.path().join("outbox"),
        &Converter::Fixed(Err(())),
    );
    let Staged::Ready(_, upload) = staged else {
        panic!("not staged");
    };
    assert_eq!(upload.file.file_name().unwrap(), "screenshot_screen.png");
    assert_eq!(fs::read(&upload.file).unwrap(), GNOME);
    assert_eq!(upload.day, "20261113");
    assert_eq!(upload.segment, "050000_1");
    assert_eq!(upload.meta.captured_at, "2026-11-13T05:00:00-07:00");
}

#[test]
fn a_refused_type_is_converted_and_one_that_wont_convert_reaches_its_end() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("Screenshot_20261113_050000.avif");
    fs::write(&path, b"an avif").unwrap();
    let staged = stage(
        &delivery(&path, b"an avif"),
        &temp.path().join("outbox"),
        &Converter::Fixed(Ok(GNOME.to_vec())),
    );
    let Staged::Ready(_, upload) = staged else {
        panic!("not staged");
    };
    assert_eq!(upload.file.file_name().unwrap(), "screenshot_screen.png");
    assert_eq!(fs::read(&upload.file).unwrap(), GNOME);
    assert!(
        fs::read(&path).unwrap() == b"an avif",
        "the original is left as it is"
    );
    let failed = stage(
        &delivery(&path, b"an avif"),
        &temp.path().join("outbox"),
        &Converter::Fixed(Err(())),
    );
    assert!(matches!(failed, Staged::Done(_, Outcome::CantComeIn)));
}

#[test]
fn a_file_changed_or_deleted_since_it_was_found_never_goes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("a.png");
    fs::write(&path, b"edited since").unwrap();
    let converter = Converter::Fixed(Err(()));
    let outbox = temp.path().join("outbox");
    assert!(matches!(
        stage(&delivery(&path, GNOME), &outbox, &converter),
        Staged::Done(_, Outcome::Gone)
    ));
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        stage(&delivery(&path, GNOME), &outbox, &converter),
        Staged::Done(_, Outcome::Gone)
    ));
}

#[test]
fn the_tile_reads_each_condition_in_its_own_words() {
    let on = ScreenshotsDocument {
        v: 1,
        mode: Mode::On,
        folder: Some("/home/owner/Pictures/Screenshots".to_owned()),
        folder_label: Some("Pictures › Screenshots".to_owned()),
        folder_name: Some("Screenshots".to_owned()),
        ..ScreenshotsDocument::default()
    };
    assert_eq!(word(&on), StateWord::On);
    assert_eq!(
        tile_line(&on).as_deref(),
        Some("from Pictures › Screenshots")
    );
    let bringing = ScreenshotsDocument {
        progress: Some([20, 2618]),
        ..on.clone()
    };
    assert_eq!(
        tile_line(&bringing).as_deref(),
        Some("20 of 2,618 in your journal")
    );
    let paused = ScreenshotsDocument {
        paused: true,
        ..on.clone()
    };
    assert_eq!(word(&paused), StateWord::Paused);
    assert_eq!(
        tile_line(&paused).as_deref(),
        Some("screenshots you take while it's paused don't come in.")
    );
    let gone = ScreenshotsDocument {
        folder_gone: true,
        ..on.clone()
    };
    assert_eq!(word(&gone), StateWord::NeedsAttention);
    assert_eq!(
        tile_line(&gone).as_deref(),
        Some("the Screenshots folder isn't on this computer anymore.")
    );
    let setting_up = ScreenshotsDocument {
        mode: Mode::NotSetUp,
        ..on.clone()
    };
    assert_eq!(word(&setting_up), StateWord::ReadyToSetUp);
    assert_eq!(
        tile_line(&setting_up).as_deref(),
        Some("choose how far back to start, then bring them in.")
    );
    let no_folder = ScreenshotsDocument {
        mode: Mode::NotSetUp,
        ..ScreenshotsDocument::default()
    };
    assert_eq!(tile_line(&no_folder), None);
    let off = ScreenshotsDocument {
        mode: Mode::Off,
        ..on
    };
    assert_eq!(word(&off), StateWord::Off);
    assert_eq!(tile_line(&off), None);
    assert_eq!(
        folder_label(
            Path::new("/home/owner/Pictures/Screenshots"),
            Path::new("/home/owner")
        ),
        "Pictures › Screenshots"
    );
}

struct FakeJournal {
    capability: Mutex<Capability>,
    answers: Mutex<VecDeque<Outcome>>,
    sent: Mutex<Vec<(String, String, Vec<u8>)>>,
}

impl Default for FakeJournal {
    fn default() -> Self {
        Self {
            capability: Mutex::new(Capability::Unknown),
            answers: Mutex::new(VecDeque::new()),
            sent: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for Arc<FakeJournal> {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move { *self.capability.lock().unwrap() })
    }
    fn deliver<'a>(&'a self, upload: &'a Upload) -> BoxFuture<'a, Option<Outcome>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push((
                upload.day.clone(),
                upload.segment.clone(),
                fs::read(&upload.file).unwrap(),
            ));
            Some(
                self.answers
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Outcome::Delivered),
            )
        })
    }
    /// Asked again on every turn, so a test sees an update at once.
    fn pairing_unchanged(&self) -> bool {
        false
    }
}

fn snapshot(paused: bool) -> StateSnapshot {
    StateSnapshot {
        mode: ObserverMode::Idle,
        paused,
        segment_open: false,
        captures_today: 0,
        total_size_mb: 0,
        pause_until: None,
        segment_start_mono: None,
        process_start_mono: 0.0,
        session: Default::default(),
        activity_through_pause: false,
        away: false,
    }
}

async fn settle(
    handle: &ScreenshotsHandle,
    until: impl Fn(&ScreenshotsDocument) -> bool,
) -> ScreenshotsDocument {
    for _ in 0..400 {
        let doc = decode_document(&handle.state_json()).unwrap();
        if until(&doc) {
            return doc;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    panic!("never settled: {}", handle.state_json());
}

#[tokio::test(flavor = "multi_thread")]
async fn screenshots_wait_for_a_journal_that_lists_them_then_go_once() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let shots = home.join("Pictures/Screenshots");
    fs::create_dir_all(&shots).unwrap();
    let shot = shots.join("Screenshot From 2026-11-13 05-00-00.png");
    fs::write(&shot, GNOME).unwrap();
    fs::File::options()
        .write(true)
        .open(&shot)
        .unwrap()
        .set_modified(SystemTime::now() - StdDuration::from_secs(60))
        .unwrap();
    let places = folder::Places {
        config: home.join(".config"),
        data: home.join(".local/share"),
        home: home.clone(),
        locale_dirs: Vec::new(),
        languages: Vec::new(),
    };
    let engine = Engine::open(temp.path().join("state"), places, Arc::new(SystemClock));
    let journal = Arc::new(FakeJournal {
        capability: Mutex::new(Capability::NotListed),
        ..FakeJournal::default()
    });
    let (_paused_tx, paused_rx) = watch::channel(snapshot(false));
    let (handle, worker) = spawnable(
        engine,
        Arc::clone(&journal),
        paused_rx,
        temp.path().join("outbox"),
        Converter::Fixed(Err(())),
    );
    let task = tokio::spawn(worker.run());
    let doc = decode_document(&handle.count("last_30_days".to_owned()).await).unwrap();
    assert_eq!(
        doc.count,
        Some(Count {
            span: Span::Last30Days,
            n: 1
        })
    );
    assert_eq!(word(&doc), StateWord::ReadyToSetUp);
    assert!(
        journal.sent.lock().unwrap().is_empty(),
        "nothing before the confirm"
    );
    handle.confirm("last_30_days".to_owned()).await;
    let waiting = settle(&handle, |doc| doc.journal_lists == Some(false)).await;
    assert_eq!(waiting.waiting, 1, "S21: it waits on this computer");
    assert!(journal.sent.lock().unwrap().is_empty());
    // The journal is updated and lists the capability.
    *journal.capability.lock().unwrap() = Capability::Supported;
    handle.retry().await;
    let done = settle(&handle, |doc| doc.waiting == 0 && doc.progress.is_none()).await;
    assert_eq!(done.journal_lists, Some(true));
    let sent = journal.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].2, GNOME);
    handle.shutdown(task).await;
}
