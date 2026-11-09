// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! How an old install's terminal history continues under this app.
//!
//! The journal names a stream from the device and the upload's source, so the
//! app's device would otherwise start a new one. Either the app takes on the old
//! pairing, which is the same device, or it asks the journal to hand the old
//! device's streams over.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use spl_transport::credential::Credential;

use super::takeover_state::{self, Continuity, JournalIdentity, Phase, TakeoverState};
use crate::journal_mark::{
    AnswerLock, PAIRING_ANSWER_FILENAME, UNVERIFIED_FILENAME, is_pairing_confirmed,
    pairing_commit_pending, pinned_ca_jid, store_answer,
};
use crate::private_file::{NoWriteFault, atomic_write_bytes};
use crate::private_link::{
    compute_pairing_id, credential_present, load_credential, persist_credential,
};

pub const LEGACY_CREDENTIALS_FILENAME: &str = "credentials.json";

/// A device id: `sha256:` and the lowercase hex of the certificate's DER.
pub fn device_id(credential: &Credential) -> Option<String> {
    let certs = spl_transport::tls::parse_certs(&credential.client_cert_pem).ok()?;
    let first = certs.into_iter().next()?;
    Some(format!(
        "sha256:{}",
        spl_core::ca::sha256_hex(first.as_ref())
    ))
}

pub fn journal_identity(credential: &Credential) -> JournalIdentity {
    JournalIdentity {
        pinned_jid: pinned_ca_jid(credential),
        instance_id: credential.instance_id.clone(),
        ca_fingerprint: credential
            .ca_fp_prefix
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

/// A random (version 4) UUID.
pub fn new_operation_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// The migration request. The journal compares these exact bytes on any retry.
pub fn migration_body(operation_id: &str, replaces: &str) -> String {
    format!(
        "{{\"protocol_version\":1,\"operation_id\":\"{operation_id}\",\"choice\":\"replace_device\",\"replaces_cid\":\"{replaces}\"}}"
    )
}

/// What the journal's answer to the migration means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationAnswer {
    Settled(Continuity),
    /// Keep the stored request and send it again later; the text is for status.
    Retry(String),
}

fn new_stream(reason: &str, needs_attention: bool) -> MigrationAnswer {
    MigrationAnswer::Settled(Continuity::NewStream {
        reason: reason.to_owned(),
        needs_attention,
    })
}

pub fn classify_answer(status: u16, reason: Option<&str>) -> MigrationAnswer {
    if (200..300).contains(&status) {
        return MigrationAnswer::Settled(Continuity::Continued);
    }
    match reason {
        Some(
            code @ ("paired_device_not_found"
            | "migration_target_conflict"
            | "migration_protocol_unsupported"
            | "migration_forbidden"
            | "migration_request_invalid"
            | "migration_replay_forbidden"
            | "migration_self_replacement"),
        ) => new_stream(code, false),
        Some(code @ "migration_operation_conflict") => new_stream(code, true),
        None if status == 404 => new_stream("journal_too_old", false),
        Some(code) => MigrationAnswer::Retry(code.to_owned()),
        None => MigrationAnswer::Retry(format!("status {status}")),
    }
}

/// What to do about continuity, given both pairings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Settle(Continuity),
    /// Take on the old pairing.
    Adopt,
    /// Wait: a pairing is being committed, or the app is paired but not connected
    /// to a confirmed journal yet.
    Wait,
    /// Ask this journal to hand the old device's streams to `caller_id`.
    Migrate {
        replaces: String,
        caller_id: String,
    },
}

/// The old pairing, as found.
pub enum LegacyPairing {
    /// None, or one that can't be read.
    Unusable,
    Conflict,
    Usable {
        device_id: String,
        journal: JournalIdentity,
    },
}

/// The app's own pairing.
pub enum AppPairing<'a> {
    None,
    CommitPending,
    Paired {
        credential: &'a Credential,
        confirmed: bool,
    },
}

pub fn plan(legacy: &LegacyPairing, app: &AppPairing<'_>) -> Plan {
    let (legacy_id, legacy_journal) = match legacy {
        LegacyPairing::Unusable => {
            return Plan::Settle(Continuity::NewStream {
                reason: "no_old_pairing".to_owned(),
                needs_attention: false,
            });
        }
        LegacyPairing::Conflict => {
            return Plan::Settle(Continuity::NewStream {
                reason: "two_old_pairings".to_owned(),
                needs_attention: false,
            });
        }
        LegacyPairing::Usable { device_id, journal } => (device_id.clone(), journal),
    };
    match app {
        AppPairing::CommitPending => Plan::Wait,
        AppPairing::None => Plan::Adopt,
        AppPairing::Paired {
            credential: ours,
            confirmed,
        } => {
            let Some(our_id) = device_id(ours) else {
                return Plan::Wait;
            };
            if our_id == legacy_id {
                return Plan::Settle(Continuity::Adopted);
            }
            if !legacy_journal.same_journal(&journal_identity(ours)) {
                return Plan::Settle(Continuity::NewStream {
                    reason: "different_journal".to_owned(),
                    needs_attention: false,
                });
            }
            if !confirmed {
                return Plan::Wait;
            }
            Plan::Migrate {
                replaces: legacy_id,
                caller_id: our_id,
            }
        }
    }
}

/// The stored request to send, keeping it for retries and replacing it only when
/// this app re-paired to the same journal under a new device.
pub fn stored_or_new_migration(
    state: &mut TakeoverState,
    replaces: &str,
    caller_id: &str,
) -> io::Result<super::takeover_state::MigrationRecord> {
    if let Some(record) = state.migration.as_ref()
        && record.caller_id == caller_id
    {
        return Ok(record.clone());
    }
    let operation_id = new_operation_id()?;
    let record = super::takeover_state::MigrationRecord {
        body: migration_body(&operation_id, replaces),
        operation_id,
        caller_id: caller_id.to_owned(),
    };
    state.migration = Some(record.clone());
    Ok(record)
}

/// Why adoption did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdoptOutcome {
    Adopted,
    /// A pairing of the app's own appeared first, or is being committed.
    AppPaired,
}

/// Takes on the old pairing: the answer first, then the credential, so a crash
/// between them is finished on the next run and never leaves a pairing with no
/// answer. The caller holds the capture-root, private-state and answer locks.
pub fn adopt(
    app_root: &Path,
    legacy_root: &Path,
    credential: &Credential,
) -> Result<AdoptOutcome, String> {
    if credential_present(app_root) || pairing_commit_pending(app_root) {
        return Ok(AdoptOutcome::AppPaired);
    }
    let answer = legacy_answer(legacy_root, credential);
    store_answer(app_root, &answer, &NoWriteFault).map_err(|error| error.to_string())?;
    copy_unverified(app_root, legacy_root)?;
    persist_credential(app_root, credential).map_err(|error| error.to_string())?;
    Ok(AdoptOutcome::Adopted)
}

/// The answer the old pairing carries. Only a file that does not exist counts as
/// confirmed, as it did for the old app; one that can't be read stays held.
fn legacy_answer(legacy_root: &Path, credential: &Credential) -> String {
    match fs::read(legacy_root.join(PAIRING_ANSWER_FILENAME)) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("confirmed")?.as_str().map(str::to_owned))
            .unwrap_or_default(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            compute_pairing_id(&credential.client_cert_pem)
        }
        Err(_) => String::new(),
    }
}

fn copy_unverified(app_root: &Path, legacy_root: &Path) -> Result<(), String> {
    let target = app_root.join(UNVERIFIED_FILENAME);
    match fs::read(legacy_root.join(UNVERIFIED_FILENAME)) {
        Ok(bytes) => atomic_write_bytes(&target, &bytes).map_err(|error| error.to_string()),
        Err(_) => match fs::remove_file(&target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}

/// Removes the old credential once the app's pairing has replaced it.
pub fn tidy_credential(legacy_root: &Path, continuity: &Continuity) -> io::Result<()> {
    if !matches!(continuity, Continuity::Adopted | Continuity::Continued) {
        return Ok(());
    }
    match fs::remove_file(legacy_root.join(LEGACY_CREDENTIALS_FILENAME)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The old pairings on disk, one per distinct device.
pub fn read_legacy_pairings(config_roots: &[PathBuf]) -> Vec<(PathBuf, Credential)> {
    let mut found: Vec<(PathBuf, Credential)> = Vec::new();
    for root in config_roots {
        let Ok(bytes) = fs::read(root.join(LEGACY_CREDENTIALS_FILENAME)) else {
            continue;
        };
        let Ok(credential) = serde_json::from_slice::<Credential>(&bytes) else {
            continue;
        };
        let id = device_id(&credential);
        if !found.iter().any(|(_, known)| device_id(known) == id) {
            found.push((root.clone(), credential));
        }
    }
    found
}

/// What is left after settling everything that needs no journal.
pub enum LocalSettle {
    Settled,
    Wait,
    Migrate {
        replaces: String,
        caller_id: String,
        ours: Box<Credential>,
    },
}

fn record_settled(state_dir: &Path, settled: Continuity) {
    let result = takeover_state::update(state_dir, |state| {
        state.continuity = Some(settled);
        state.last_answer = None;
        state.finish_if_complete();
    });
    if let Err(error) = result {
        tracing::warn!(%error, "could not record how terminal history continues");
    }
}

/// Settles continuity where no journal call is needed, which includes taking on
/// the old pairing: an app with no pairing has no link to the journal to wait for.
/// Nothing is decided until the old service is proven gone.
pub async fn settle_locally(
    state_dir: &Path,
    config_root: &Path,
    state: &TakeoverState,
) -> LocalSettle {
    if state.continuity.is_some() {
        return LocalSettle::Settled;
    }
    if state.phase < Phase::Recovered {
        return LocalSettle::Wait;
    }
    let pairings = read_legacy_pairings(&state.config_roots);
    let conflicting: Vec<PathBuf> = if pairings.len() > 1 {
        pairings
            .iter()
            .map(|(root, _)| root.join(LEGACY_CREDENTIALS_FILENAME))
            .collect()
    } else {
        Vec::new()
    };
    let found = match pairings.as_slice() {
        [(_, credential)] => device_id(credential).map(|id| (id, journal_identity(credential))),
        _ => None,
    };
    let recorded_changed = conflicting != state.conflicting_credentials
        || found.as_ref().is_some_and(|(id, journal)| {
            state.legacy_device_id.as_ref() != Some(id)
                || state.legacy_journal.as_ref() != Some(journal)
        });
    if recorded_changed {
        let update = found.clone();
        let list = conflicting.clone();
        let _ = takeover_state::update(state_dir, |stored| {
            stored.conflicting_credentials = list;
            if let Some((id, journal)) = update {
                stored.legacy_device_id = Some(id);
                stored.legacy_journal = Some(journal);
            }
        });
    }
    // A decision the journal may be holding is driven to its end, even if the old
    // credential has since gone.
    let legacy = match (&found, pairings.len()) {
        (Some((id, journal)), _) => LegacyPairing::Usable {
            device_id: id.clone(),
            journal: journal.clone(),
        },
        (None, 0) => match (
            &state.migration,
            &state.legacy_device_id,
            &state.legacy_journal,
        ) {
            (Some(_), Some(id), Some(journal)) => LegacyPairing::Usable {
                device_id: id.clone(),
                journal: journal.clone(),
            },
            _ => LegacyPairing::Unusable,
        },
        (None, 1) => LegacyPairing::Unusable,
        (None, _) => LegacyPairing::Conflict,
    };
    let ours = load_credential(config_root).ok().flatten();
    let app = if pairing_commit_pending(config_root) {
        AppPairing::CommitPending
    } else {
        match &ours {
            None => AppPairing::None,
            Some(credential) => AppPairing::Paired {
                credential,
                confirmed: is_pairing_confirmed(
                    config_root,
                    &compute_pairing_id(&credential.client_cert_pem),
                ),
            },
        }
    };
    match plan(&legacy, &app) {
        Plan::Wait => LocalSettle::Wait,
        Plan::Settle(settled) => {
            record_settled(state_dir, settled);
            LocalSettle::Settled
        }
        Plan::Adopt => {
            let [(root, credential)] = pairings.as_slice() else {
                record_settled(
                    state_dir,
                    Continuity::NewStream {
                        reason: "no_old_pairing".to_owned(),
                        needs_attention: false,
                    },
                );
                return LocalSettle::Settled;
            };
            let lock = match AnswerLock::acquire(config_root).await {
                Ok(lock) => lock,
                Err(error) => {
                    tracing::warn!(%error, "could not take the pairing answer lock");
                    return LocalSettle::Wait;
                }
            };
            let adopted = adopt(config_root, root, credential);
            drop(lock);
            match adopted {
                Ok(AdoptOutcome::Adopted) => {
                    record_settled(state_dir, Continuity::Adopted);
                    LocalSettle::Settled
                }
                Ok(AdoptOutcome::AppPaired) => LocalSettle::Wait,
                Err(error) => {
                    tracing::warn!(%error, "could not take on the old pairing");
                    LocalSettle::Wait
                }
            }
        }
        Plan::Migrate {
            replaces,
            caller_id,
        } => match ours {
            Some(ours) => LocalSettle::Migrate {
                replaces,
                caller_id,
                ours: Box::new(ours),
            },
            None => LocalSettle::Wait,
        },
    }
}

/// Removes old credentials the app no longer needs. Runs after the outcome is
/// recorded, and again on later passes if a removal failed or was interrupted.
pub fn tidy_settled(state: &TakeoverState) {
    let Some(settled) = &state.continuity else {
        return;
    };
    for root in &state.config_roots {
        if let Err(error) = tidy_credential(root, settled) {
            tracing::warn!(%error, "could not remove the old pairing");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_answers_map_as_the_takeover_needs() {
        let settled = |status, reason| match classify_answer(status, reason) {
            MigrationAnswer::Settled(continuity) => continuity,
            other => panic!("{other:?}"),
        };
        assert_eq!(settled(200, None), Continuity::Continued);
        for code in [
            "paired_device_not_found",
            "migration_target_conflict",
            "migration_protocol_unsupported",
            "migration_forbidden",
            "migration_request_invalid",
            "migration_replay_forbidden",
            "migration_self_replacement",
        ] {
            assert!(
                matches!(
                    settled(409, Some(code)),
                    Continuity::NewStream { needs_attention: false, reason } if reason == code
                ),
                "{code}"
            );
        }
        assert!(matches!(
            settled(409, Some("migration_operation_conflict")),
            Continuity::NewStream {
                needs_attention: true,
                ..
            }
        ));
        assert!(matches!(settled(404, None), Continuity::NewStream { .. }));
        for (status, reason) in [
            (503, Some("migration_state_unavailable")),
            (500, None),
            (409, Some("something_new")),
        ] {
            assert!(
                matches!(classify_answer(status, reason), MigrationAnswer::Retry(_)),
                "{status} {reason:?}"
            );
        }
    }

    #[test]
    fn the_request_body_is_exact() {
        assert_eq!(
            migration_body("op", "sha256:ab"),
            r#"{"protocol_version":1,"operation_id":"op","choice":"replace_device","replaces_cid":"sha256:ab"}"#
        );
    }

    #[test]
    fn an_operation_id_is_a_version_four_uuid() {
        let id = new_operation_id().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn a_retry_keeps_the_stored_request_and_a_new_caller_gets_a_new_one() {
        let mut state = TakeoverState::default();
        let first = stored_or_new_migration(&mut state, "sha256:old", "sha256:one").unwrap();
        let again = stored_or_new_migration(&mut state, "sha256:old", "sha256:one").unwrap();
        assert_eq!(first, again);
        let repaired = stored_or_new_migration(&mut state, "sha256:old", "sha256:two").unwrap();
        assert_ne!(first.operation_id, repaired.operation_id);
        assert_eq!(state.migration, Some(repaired));
    }

    #[test]
    fn no_old_pairing_settles_without_touching_anything() {
        assert!(matches!(
            plan(&LegacyPairing::Unusable, &AppPairing::None),
            Plan::Settle(Continuity::NewStream { .. })
        ));
        assert!(matches!(
            plan(&LegacyPairing::Conflict, &AppPairing::None),
            Plan::Settle(Continuity::NewStream { .. })
        ));
    }
}
