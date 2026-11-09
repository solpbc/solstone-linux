// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Sync's part of moving an old terminal install in: settling how the terminal
//! history continues, then delivering what the old spool still holds.
//!
//! The old spool is read through a guard held only while it is listed, and nothing
//! is ever written inside it. A segment leaves only once the journal holds it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use solstone_terminal::legacy::{self, LegacySegment};
use spl_transport::credential::Credential;

use super::{SyncWorker, segment_custody_proven, sha256_file, verify_receipt};
use crate::private_link::{MigrationReply, compute_pairing_id};
use crate::recovery::CaptureZone;
use crate::terminal::takeover_continuity::{self as continuity, MigrationAnswer};
use crate::terminal::takeover_state::{self, Continuity, Stored};

const MIGRATION_TIMEOUT: Duration = Duration::from_secs(30);
const MIGRATION_BACKOFF_FIRST: f64 = 5.0;
const MIGRATION_BACKOFF_MAX: f64 = 300.0;
const REFUSED_SEGMENT_RETRY: f64 = 3600.0;
const DRAIN_PASS_BUDGET: Duration = Duration::from_secs(60);

impl SyncWorker {
    /// Settles continuity; called at the start of a pass, so anything it settles
    /// releases this pass's uploads.
    pub(super) async fn takeover_settle(&mut self) {
        let state_dir = self.config.state_dir();
        let Stored::Present(state) = takeover_state::load(&state_dir) else {
            return;
        };
        let config_root = self.config.config_dir.clone();
        if let continuity::LocalSettle::Migrate {
            replaces,
            caller_id,
            ours,
        } = continuity::settle_locally(&state_dir, &config_root, &state).await
        {
            self.send_migration(&replaces, &caller_id, &ours).await;
        }
        if let Stored::Present(state) = takeover_state::load(&state_dir) {
            continuity::tidy_settled(&state);
        }
    }

    /// Delivers what the old spool holds, after the pass's own uploads and within
    /// a budget, so a large backlog doesn't hold new captures back.
    pub(super) async fn takeover_drain(&mut self) {
        let state_dir = self.config.state_dir();
        let Stored::Present(state) = takeover_state::load(&state_dir) else {
            return;
        };
        if state.continuity.is_some() {
            self.drain_legacy(&state_dir).await;
        }
    }

    fn save_settled_from_journal(&self, settled: Continuity) {
        let result = takeover_state::update(&self.config.state_dir(), |state| {
            state.continuity = Some(settled);
            state.last_answer = None;
            state.finish_if_complete();
        });
        if let Err(error) = result {
            tracing::warn!(%error, "could not record how terminal history continues");
        }
    }

    async fn send_migration(&mut self, replaces: &str, caller_id: &str, ours: &Credential) {
        if self.clock.monotonic_seconds() < self.migration_next_at {
            return;
        }
        let Some(capability) = self.association.capability.clone() else {
            return;
        };
        // Only ever from the device the stored request names.
        if compute_pairing_id(&ours.client_cert_pem) != capability.writer().pairing_id() {
            return;
        }
        let state_dir = self.config.state_dir();
        let record = match takeover_state::update(&state_dir, |state| {
            continuity::stored_or_new_migration(state, replaces, caller_id)
        }) {
            Ok(Some(Ok(record))) => record,
            Ok(_) | Err(_) => return,
        };
        let reply = capability
            .clients_self_migration_put(record.body, MIGRATION_TIMEOUT)
            .await;
        let answer = match reply {
            MigrationReply::Answered { status, reason } => {
                continuity::classify_answer(status, reason.as_deref())
            }
            MigrationReply::Unreachable => MigrationAnswer::Retry("journal unreachable".to_owned()),
        };
        match answer {
            MigrationAnswer::Settled(settled) => {
                self.migration_backoff = MIGRATION_BACKOFF_FIRST;
                self.save_settled_from_journal(settled);
            }
            MigrationAnswer::Retry(text) => {
                let delay = self.migration_backoff;
                self.migration_backoff = (delay * 2.0).min(MIGRATION_BACKOFF_MAX);
                self.migration_next_at = self.clock.monotonic_seconds() + delay;
                let _ = takeover_state::update(&state_dir, |state| {
                    state.last_answer = Some(text);
                });
            }
        }
    }

    async fn drain_legacy(&mut self, state_dir: &Path) {
        let Stored::Present(state) = takeover_state::load(state_dir) else {
            return;
        };
        let deadline = Instant::now() + DRAIN_PASS_BUDGET;
        for root in &state.data_roots {
            let usable = state
                .roots
                .get(root)
                .is_some_and(|record| record.recovered && record.problem.is_none());
            if !usable {
                continue;
            }
            // The guard is dropped before anything crosses the network.
            let Ok(listing) = legacy::open(root).and_then(|guard| guard.segments()) else {
                continue;
            };
            let waiting = listing.segments.len();
            let held = listing.held.len();
            let problem_paths: Vec<PathBuf> =
                listing.problems.iter().map(|p| p.path.clone()).collect();
            let unchanged = state.roots.get(root).is_some_and(|record| {
                record.waiting == waiting
                    && record.held == held
                    && record.problem_paths == problem_paths
            });
            if !unchanged {
                let _ = takeover_state::update(state_dir, |stored| {
                    let record = stored.roots.entry(root.clone()).or_default();
                    record.waiting = waiting;
                    record.held = held;
                    record.problem_paths = problem_paths;
                });
            }
            if !self
                .deliver_legacy_listing(listing.segments, Some(deadline))
                .await
            {
                return;
            }
            self.remove_finished_ledger(root);
        }
    }

    fn remove_finished_ledger(&self, root: &Path) {
        let empty = legacy::open(root)
            .and_then(|guard| guard.segments())
            .is_ok_and(|listing| listing.segments.is_empty());
        if empty {
            let _ = fs::remove_dir_all(root.join("sync-ledger"));
        }
    }

    /// Delivers one root's finished segments, newest day first as sync does.
    /// `false` stops the pass.
    async fn deliver_legacy_listing(
        &mut self,
        segments: Vec<LegacySegment>,
        deadline: Option<Instant>,
    ) -> bool {
        let mut by_day: HashMap<String, Vec<LegacySegment>> = HashMap::new();
        for segment in segments {
            by_day.entry(segment.day.clone()).or_default().push(segment);
        }
        let mut days: Vec<String> = by_day.keys().cloned().collect();
        days.sort_by(|a, b| b.cmp(a));
        for day in days {
            let now = self.clock.wall_seconds();
            let due: Vec<LegacySegment> = by_day
                .remove(&day)
                .unwrap_or_default()
                .into_iter()
                .filter(|segment| {
                    self.legacy_retry
                        .get(&segment.dir)
                        .is_none_or(|at| now >= *at)
                })
                .collect();
            if due.is_empty() {
                continue;
            }
            if !self.is_active() || self.circuit_open {
                return false;
            }
            let custody = self
                .client
                .fetch_source_day_custody(&day, Some(crate::terminal::SOURCE))
                .await;
            if !self.association_is_current() {
                return false;
            }
            // Without the journal's list a segment can't be proven absent.
            if custody.error_type.is_some() {
                return false;
            }
            for segment in due {
                if !self.is_running() || deadline.is_some_and(|at| Instant::now() >= at) {
                    return false;
                }
                if journal_holds_all_of(&custody.items, &segment) {
                    remove_legacy_segment(&segment);
                    continue;
                }
                if !self.upload_legacy_segment(&segment).await {
                    return false;
                }
            }
        }
        true
    }

    /// `false` stops the pass; the segment is tried again on a later one.
    async fn upload_legacy_segment(&mut self, segment: &LegacySegment) -> bool {
        let files: Vec<PathBuf> = segment
            .files
            .iter()
            .map(|name| segment.dir.join(name))
            .collect();
        let mut precomputed = Vec::with_capacity(files.len());
        for (name, path) in segment.files.iter().zip(&files) {
            let (Ok(metadata), Ok(sha)) = (fs::metadata(path), sha256_file(path)) else {
                self.delay_legacy_segment(segment);
                return true;
            };
            precomputed.push((name.clone(), metadata.len(), sha));
        }
        let zone = segment.zone.as_ref().map(|time| CaptureZone {
            tz: time.tz.clone(),
            utc_offset_seconds: time.utc_offset_seconds,
        });
        self.terminal
            .set_activity(crate::terminal::SyncActivity::Working);
        let result = self
            .keep_hearing(self.client.upload_source_segment(
                &segment.day,
                &segment.segment,
                &files,
                zone.as_ref(),
                Some(crate::terminal::SOURCE),
            ))
            .await;
        self.terminal
            .set_activity(crate::terminal::SyncActivity::Idle);
        if result.stale_capability || !self.association_is_current() {
            return false;
        }
        let received = result.success
            && result
                .file_descriptors
                .as_ref()
                .is_some_and(|descriptors| verify_receipt(&files, &precomputed, descriptors));
        // The journal answered, but not with a receipt: this one waits, the rest go.
        let answered_without_receipt = result.success && !received;
        let removed_there = result.status_code == Some(500)
            && result.reason_code.as_deref() == Some("segment_removed");
        if received || removed_there {
            self.record_contact(false);
            self.reset_failures();
            remove_legacy_segment(segment);
            self.legacy_retry.remove(&segment.dir);
            return true;
        }
        let bounded = matches!(result.status_code, Some(400..=499))
            || (result.status_code.is_some_and(|status| status >= 500)
                && result.reason_code.is_some());
        if answered_without_receipt || (bounded && !self.client.is_revoked()) {
            self.delay_legacy_segment(segment);
            return true;
        }
        // Anything else is the link or the journal: try again next pass.
        false
    }

    fn delay_legacy_segment(&mut self, segment: &LegacySegment) {
        let at = self.clock.wall_seconds() + REFUSED_SEGMENT_RETRY;
        self.legacy_retry.insert(segment.dir.clone(), at);
    }
}

/// Only when one listed row accounts for every file here, byte for byte, may the
/// segment go without being sent. Anything less is sent, and the journal sorts out
/// what it already has.
fn journal_holds_all_of(items: &[crate::upload::ListingEntry], segment: &LegacySegment) -> bool {
    let mut rows = items.iter().filter(|entry| {
        entry.segment.as_deref() == Some(&segment.segment)
            || entry.key.as_deref() == Some(&segment.segment)
    });
    let (Some(row), None) = (rows.next(), rows.next()) else {
        return false;
    };
    segment_custody_proven(&segment.dir, row).unwrap_or(false)
}

fn remove_legacy_segment(segment: &LegacySegment) {
    if let Err(error) = fs::remove_dir_all(&segment.dir) {
        tracing::warn!(%error, path = %segment.dir.display(), "could not remove a delivered segment");
        return;
    }
    let _ = crate::terminal::layout::remove_zone(&segment.dir);
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_worker_for_takeover as test_worker;
    use super::super::*;
    use super::*;
    use crate::journal_mark::{is_pairing_confirmed, pairing_commit_pending, write_pairing_answer};
    use crate::private_link::{load_credential, persist_credential};
    use crate::terminal::takeover_continuity::LEGACY_CREDENTIALS_FILENAME;
    use crate::terminal::takeover_state::TakeoverState;
    use crate::terminal::takeover_state::{Phase, RootRecord};
    use serde_json::{Value, json};

    const DAY: &str = "20260101";
    const SEGMENT: &str = "101500_300";
    const MIGRATION: &str = "/app/network/api/clients/self/migration";
    const INGEST: &str = "/app/devices/ingest";

    struct Fixture {
        temp: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                temp: tempfile::tempdir().unwrap(),
            }
        }

        fn legacy_config(&self) -> PathBuf {
            self.temp.path().join("legacy-config")
        }

        fn legacy_data(&self) -> PathBuf {
            self.temp.path().join("legacy-data")
        }

        fn segment_dir(&self) -> PathBuf {
            self.legacy_data()
                .join("captures")
                .join(DAY)
                .join("host.tmux")
                .join(SEGMENT)
        }

        fn write_legacy_segment(&self) -> Value {
            let dir = self.segment_dir();
            fs::create_dir_all(&dir).unwrap();
            let body = b"terminal capture";
            fs::write(dir.join("tmux_main_screen.jsonl"), body).unwrap();
            fs::write(
                dir.with_file_name(format!("{SEGMENT}.zone.json")),
                r#"{"tz":"Asia/Kolkata","utc_offset_seconds":19800}"#,
            )
            .unwrap();
            json!({
                "status": "ok",
                "segment": SEGMENT,
                "file_descriptors": [{
                    "submitted": "tmux_main_screen.jsonl",
                    "written": "tmux_main_screen.jsonl",
                    "size": body.len(),
                    "sha256": sha256_file(&dir.join("tmux_main_screen.jsonl")).unwrap(),
                    "disposition": "written",
                }],
            })
        }

        /// An old pairing to the same journal as `app`, under another device.
        fn write_legacy_credential(&self, app: &Credential) -> Credential {
            let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
            let cert = rcgen::CertificateParams::new(Vec::<String>::new())
                .unwrap()
                .self_signed(&key)
                .unwrap();
            let credential = Credential {
                client_cert_pem: cert.pem(),
                client_key_pem: key.serialize_pem(),
                ..app.clone()
            };
            fs::create_dir_all(self.legacy_config()).unwrap();
            fs::write(
                self.legacy_config().join(LEGACY_CREDENTIALS_FILENAME),
                serde_json::to_vec(&credential).unwrap(),
            )
            .unwrap();
            credential
        }

        fn write_state(&self, worker: &SyncWorker) {
            let mut state = TakeoverState {
                phase: Phase::Recovered,
                config_roots: vec![self.legacy_config()],
                data_roots: vec![self.legacy_data()],
                settings_applied: true,
                terminal_settled: true,
                ..TakeoverState::default()
            };
            state.roots.insert(
                self.legacy_data(),
                RootRecord {
                    recovered: true,
                    ..RootRecord::default()
                },
            );
            takeover_state::save(&worker.config.state_dir(), &state).unwrap();
        }
    }

    fn pair_app(worker: &SyncWorker, credential: &Credential) {
        fs::create_dir_all(&worker.config.config_dir).unwrap();
        persist_credential(&worker.config.config_dir, credential).unwrap();
        write_pairing_answer(
            &worker.config.config_dir,
            &compute_pairing_id(&credential.client_cert_pem),
        )
        .unwrap();
    }

    fn stored(worker: &SyncWorker) -> TakeoverState {
        *match takeover_state::load(&worker.config.state_dir()) {
            Stored::Present(state) => state,
            other => panic!("{other:?}"),
        }
    }

    async fn run_takeover(worker: &mut SyncWorker) {
        worker.takeover_settle().await;
        worker.takeover_drain().await;
    }

    fn exact_requests(server: &crate::test_support::LinkedMockServer, uri: &str) -> usize {
        server
            .requests()
            .iter()
            .filter(|request| request.uri == uri)
            .count()
    }

    fn custody_empty() -> Value {
        json!({"protocol_version": 3, "total": 0, "items": []})
    }

    #[tokio::test]
    async fn the_handover_goes_first_and_then_the_waiting_segment_arrives() {
        let fixture = Fixture::new();
        let receipt = fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![
                (200, json!({"status": "ok"})),
                custody_response(custody_empty()),
                (200, receipt),
            ],
        )
        .await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);

        run_takeover(&mut worker).await;

        let uris: Vec<String> = server.requests().into_iter().map(|r| r.uri).collect();
        let migrate = uris.iter().position(|uri| uri == MIGRATION).unwrap();
        let ingest = uris.iter().position(|uri| uri == INGEST).unwrap();
        assert!(migrate < ingest, "{uris:?}");
        let upload = server
            .requests()
            .into_iter()
            .find(|request| request.uri == INGEST)
            .unwrap();
        let body = String::from_utf8_lossy(&upload.body).into_owned();
        assert!(body.contains("\"source\":\"tmux\""), "{body}");
        assert!(
            body.contains("19800"),
            "the old zone arrives as meta: {body}"
        );
        assert!(!fixture.segment_dir().exists());
        let state = stored(&worker);
        assert_eq!(state.continuity, Some(Continuity::Continued));
        assert_eq!(state.phase, Phase::Done);
        assert!(
            !fixture
                .legacy_config()
                .join(LEGACY_CREDENTIALS_FILENAME)
                .exists(),
            "the journal no longer accepts it"
        );
    }

    #[tokio::test]
    async fn an_unavailable_journal_is_retried_with_the_same_bytes_and_nothing_uploads() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let unavailable = json!({"reason_code": "migration_state_unavailable"});
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![(503, unavailable.clone()), (503, unavailable)],
        )
        .await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);

        run_takeover(&mut worker).await;
        worker.migration_next_at = f64::NEG_INFINITY;
        run_takeover(&mut worker).await;

        let puts: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|request| request.uri == MIGRATION)
            .collect();
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[0].body, puts[1].body);
        assert_eq!(exact_requests(&server, INGEST), 0);
        assert!(fixture.segment_dir().exists());
        let state = stored(&worker);
        assert_eq!(state.continuity, None);
        assert_eq!(
            state.last_answer.as_deref(),
            Some("migration_state_unavailable")
        );
        assert!(
            takeover_state::load(&worker.config.state_dir()).uploads_gated(),
            "the app's own terminal segments wait too"
        );
    }

    #[tokio::test]
    async fn a_permanent_refusal_is_recorded_and_held_captures_still_go() {
        for (reason, attention) in [
            ("migration_forbidden", false),
            ("migration_operation_conflict", true),
        ] {
            let fixture = Fixture::new();
            let receipt = fixture.write_legacy_segment();
            let (server, mut worker) = test_worker(
                &fixture.temp,
                vec![
                    (409, json!({"reason_code": reason})),
                    custody_response(custody_empty()),
                    (200, receipt),
                ],
            )
            .await;
            pair_app(&worker, &server.credential());
            fixture.write_legacy_credential(&server.credential());
            fixture.write_state(&worker);

            run_takeover(&mut worker).await;
            worker.migration_next_at = f64::NEG_INFINITY;
            run_takeover(&mut worker).await;

            assert_eq!(exact_requests(&server, MIGRATION), 1, "{reason}");
            assert_eq!(exact_requests(&server, INGEST), 1, "{reason}");
            assert_eq!(
                stored(&worker).continuity,
                Some(Continuity::NewStream {
                    reason: reason.to_owned(),
                    needs_attention: attention,
                })
            );
            assert!(
                fixture
                    .legacy_config()
                    .join(LEGACY_CREDENTIALS_FILENAME)
                    .exists(),
                "only a handed-over pairing is removed"
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_answer_is_retried_and_never_settled() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(&fixture.temp, vec![(500, json!({}))]).await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);
        run_takeover(&mut worker).await;
        let state = stored(&worker);
        assert_eq!(state.continuity, None);
        assert!(state.last_answer.is_some());
        assert_eq!(exact_requests(&server, INGEST), 0);
    }

    #[tokio::test]
    async fn an_unpaired_app_takes_on_the_old_pairing_with_no_call() {
        let fixture = Fixture::new();
        let receipt = fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![custody_response(custody_empty()), (200, receipt)],
        )
        .await;
        let old = fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);

        run_takeover(&mut worker).await;

        assert_eq!(exact_requests(&server, MIGRATION), 0);
        assert_eq!(stored(&worker).continuity, Some(Continuity::Adopted));
        let config_root = &worker.config.config_dir;
        assert_eq!(load_credential(config_root).unwrap(), Some(old.clone()));
        assert!(
            is_pairing_confirmed(config_root, &compute_pairing_id(&old.client_cert_pem)),
            "an absent old answer stands for confirmed"
        );
        assert!(!pairing_commit_pending(config_root));
        assert!(
            !fixture
                .legacy_config()
                .join(LEGACY_CREDENTIALS_FILENAME)
                .exists()
        );
    }

    #[tokio::test]
    async fn an_unconfirmed_old_answer_stays_held_after_adoption() {
        let fixture = Fixture::new();
        let (server, mut worker) = test_worker(&fixture.temp, Vec::new()).await;
        let old = fixture.write_legacy_credential(&server.credential());
        fs::write(
            fixture.legacy_config().join("pairing-answer.json"),
            r#"{"confirmed":""}"#,
        )
        .unwrap();
        fixture.write_state(&worker);
        run_takeover(&mut worker).await;
        assert!(!is_pairing_confirmed(
            &worker.config.config_dir,
            &compute_pairing_id(&old.client_cert_pem)
        ));
    }

    #[tokio::test]
    async fn a_failed_upload_goes_on_the_next_pass_and_the_old_spool_is_untouched() {
        let fixture = Fixture::new();
        let receipt = fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![
                custody_response(custody_empty()),
                (503, json!({})),
                custody_response(custody_empty()),
                (200, receipt),
            ],
        )
        .await;
        fixture.write_state(&worker);
        fs::create_dir_all(fixture.legacy_config()).unwrap();

        run_takeover(&mut worker).await;
        let names: Vec<String> = fs::read_dir(fixture.segment_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["tmux_main_screen.jsonl"]);

        run_takeover(&mut worker).await;
        assert!(!fixture.segment_dir().exists());
        assert_eq!(exact_requests(&server, INGEST), 2);
    }

    #[tokio::test]
    async fn an_unreadable_record_gates_uploads_and_drains_nothing() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(&fixture.temp, Vec::new()).await;
        fs::write(
            takeover_state::state_path(&worker.config.state_dir()),
            b"{\"phase\"",
        )
        .unwrap();
        run_takeover(&mut worker).await;
        assert!(server.requests().is_empty());
        assert!(fixture.segment_dir().exists());
        assert!(takeover_state::load(&worker.config.state_dir()).uploads_gated());
    }

    #[tokio::test]
    async fn a_different_journal_settles_as_a_new_stream_with_no_call() {
        let fixture = Fixture::new();
        let (server, mut worker) = test_worker(&fixture.temp, Vec::new()).await;
        pair_app(&worker, &server.credential());
        let mut old = fixture.write_legacy_credential(&server.credential());
        old.ca_chain_pem = Vec::new();
        old.instance_id = "another-journal".to_owned();
        old.ca_fp_prefix = vec![1, 2, 3];
        fs::write(
            fixture.legacy_config().join(LEGACY_CREDENTIALS_FILENAME),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        fixture.write_state(&worker);
        run_takeover(&mut worker).await;
        assert_eq!(exact_requests(&server, MIGRATION), 0);
        assert!(matches!(
            stored(&worker).continuity,
            Some(Continuity::NewStream { .. })
        ));
        assert!(
            fixture
                .legacy_config()
                .join(LEGACY_CREDENTIALS_FILENAME)
                .exists()
        );
    }

    #[tokio::test]
    async fn the_apps_own_terminal_segment_waits_behind_the_handover_and_a_screen_upload_does_not()
    {
        let fixture = Fixture::new();
        let captures = fixture.temp.path().join("captures").join(DAY);
        let terminal = captures
            .join(crate::terminal::STREAM_DIR)
            .join("110000_300");
        fs::create_dir_all(&terminal).unwrap();
        fs::write(terminal.join("tmux_main_screen.jsonl"), b"terminal").unwrap();
        fs::write(
            terminal.with_file_name("110000_300.zone.json"),
            r#"{"utc_offset_seconds":0}"#,
        )
        .unwrap();
        let screen = captures.join("desktop").join("120000_300");
        fs::create_dir_all(&screen).unwrap();
        fs::write(screen.join("screen.webm"), b"screen").unwrap();
        let receipt = json!({
            "status": "ok",
            "segment": "120000_300",
            "file_descriptors": [{
                "submitted": "screen.webm",
                "written": "screen.webm",
                "size": 6,
                "sha256": sha256_file(&screen.join("screen.webm")).unwrap(),
                "disposition": "written",
            }],
        });
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![
                (503, json!({"reason_code": "migration_state_unavailable"})),
                (200, receipt),
            ],
        )
        .await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);

        worker.sync_pass().await;

        let uploads: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|request| request.uri == INGEST)
            .collect();
        assert_eq!(uploads.len(), 1);
        assert!(String::from_utf8_lossy(&uploads[0].body).contains("screen.webm"));
        assert!(!screen.exists(), "the screen segment went");
        assert!(terminal.exists(), "the terminal segment waits");
    }

    #[tokio::test]
    async fn a_row_missing_one_of_the_files_does_not_remove_the_segment() {
        let fixture = Fixture::new();
        let receipt = fixture.write_legacy_segment();
        let dir = fixture.segment_dir();
        let sha = sha256_file(&dir.join("tmux_main_screen.jsonl")).unwrap();
        for (status, name) in [
            ("missing", "tmux_main_screen.jsonl"),
            ("present", "other.jsonl"),
        ] {
            let fixture_row = json!({
                "protocol_version": 3,
                "total": 1,
                "items": [{
                    "key": SEGMENT,
                    "segment": SEGMENT,
                    "stream": "host.tmux",
                    "files": [{"name": name, "size": 16, "sha256": sha, "status": status}],
                }],
            });
            let (server, mut worker) = test_worker(
                &fixture.temp,
                vec![custody_response(fixture_row), (200, receipt.clone())],
            )
            .await;
            fixture.write_state(&worker);
            fs::create_dir_all(fixture.legacy_config()).unwrap();
            run_takeover(&mut worker).await;
            assert_eq!(exact_requests(&server, INGEST), 1, "{status} {name}");
            fixture.write_legacy_segment();
        }
    }

    #[tokio::test]
    async fn a_row_that_proves_every_file_removes_the_segment_unsent() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let sha = sha256_file(&fixture.segment_dir().join("tmux_main_screen.jsonl")).unwrap();
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![custody_response(json!({
                "protocol_version": 3,
                "total": 1,
                "items": [{
                    "key": SEGMENT,
                    "segment": SEGMENT,
                    "stream": "host.tmux",
                    "files": [{
                        "name": "tmux_main_screen.jsonl",
                        "size": 16,
                        "sha256": sha,
                        "status": "present",
                    }],
                }],
            }))],
        )
        .await;
        fixture.write_state(&worker);
        fs::create_dir_all(fixture.legacy_config()).unwrap();
        run_takeover(&mut worker).await;
        assert_eq!(exact_requests(&server, INGEST), 0);
        assert!(!fixture.segment_dir().exists());
    }

    #[tokio::test]
    async fn nothing_settles_until_the_old_service_is_proven_gone() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(&fixture.temp, Vec::new()).await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);
        let state_dir = worker.config.state_dir();
        takeover_state::update(&state_dir, |state| state.phase = Phase::Stopped).unwrap();
        run_takeover(&mut worker).await;
        assert_eq!(exact_requests(&server, MIGRATION), 0);
        assert_eq!(stored(&worker).continuity, None);
        assert!(
            fixture
                .legacy_config()
                .join(LEGACY_CREDENTIALS_FILENAME)
                .exists()
        );
    }

    #[tokio::test]
    async fn a_migration_is_driven_to_its_end_after_the_old_credential_goes() {
        let fixture = Fixture::new();
        fixture.write_legacy_segment();
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![
                (503, json!({"reason_code": "migration_state_unavailable"})),
                (200, json!({"status": "ok"})),
            ],
        )
        .await;
        pair_app(&worker, &server.credential());
        fixture.write_legacy_credential(&server.credential());
        fixture.write_state(&worker);
        worker.takeover_settle().await;
        fs::remove_file(fixture.legacy_config().join(LEGACY_CREDENTIALS_FILENAME)).unwrap();
        worker.migration_next_at = f64::NEG_INFINITY;
        worker.takeover_settle().await;
        let puts: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|request| request.uri == MIGRATION)
            .collect();
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[0].body, puts[1].body);
        assert_eq!(stored(&worker).continuity, Some(Continuity::Continued));
    }

    #[tokio::test]
    async fn a_resumed_adoption_settles_as_adopted_with_no_call() {
        let fixture = Fixture::new();
        let (server, mut worker) = test_worker(&fixture.temp, Vec::new()).await;
        let old = fixture.write_legacy_credential(&server.credential());
        pair_app(&worker, &old);
        fixture.write_state(&worker);
        worker.takeover_settle().await;
        assert_eq!(exact_requests(&server, MIGRATION), 0);
        assert_eq!(stored(&worker).continuity, Some(Continuity::Adopted));
    }

    #[tokio::test]
    async fn a_segment_the_journal_will_not_take_waits_and_the_rest_go() {
        let fixture = Fixture::new();
        let receipt = fixture.write_legacy_segment();
        let later = fixture.segment_dir().with_file_name("111500_300");
        fs::create_dir_all(&later).unwrap();
        fs::write(later.join("tmux_main_screen.jsonl"), b"terminal capture").unwrap();
        let mut later_receipt = receipt.clone();
        later_receipt["segment"] = json!("111500_300");
        let (server, mut worker) = test_worker(
            &fixture.temp,
            vec![
                custody_response(custody_empty()),
                (500, json!({"reason_code": "something_else"})),
                (200, later_receipt),
            ],
        )
        .await;
        fixture.write_state(&worker);
        fs::create_dir_all(fixture.legacy_config()).unwrap();
        run_takeover(&mut worker).await;
        assert_eq!(exact_requests(&server, INGEST), 2);
        assert!(fixture.segment_dir().exists(), "the refused one waits");
        assert!(!later.exists(), "the next one still went");
    }

    fn custody_response(fixture: Value) -> (u16, Value) {
        (200, fixture)
    }
}
