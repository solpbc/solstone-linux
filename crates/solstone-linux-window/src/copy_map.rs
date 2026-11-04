// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::copy::CopyKey;
use solstone_linux::copy::{unknown_journal_body_direct, unknown_journal_seen_at};
use solstone_linux::window_control::{
    ControlModel, MarkRenderDocument, SettingsModel, adapt_graphical_link,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntakeKeys {
    pub headline: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub action: Option<CopyKey>,
}

pub fn map_fault(model: &ControlModel) -> Option<CopyKey> {
    Some(match model.fault.as_deref()? {
        "malformed-link" => CopyKey::PairingBadLink,
        "pair-failed" => CopyKey::PairingFailed,
        "unverifiable" if model.durable == "confirmed" => {
            CopyKey::PairingReplacementUnavailableDetail
        }
        "unverifiable" => CopyKey::PairingUnavailableDetail,
        "commit-outcome-unknown" => CopyKey::PairingOutcomeUnavailable,
        "commit-failed" if model.durable == "held" => CopyKey::PairingOutcomeUnavailable,
        "commit-failed" => CopyKey::PairingSaveFailed,
        "stale-answer" => CopyKey::PairingStale,
        "owner-start-failed" | "owner-stop-failed" => CopyKey::PairingConfirmedUnavailable,
        "invalid-config" => CopyKey::SettingsLoadFailed,
        "invalid-setting" => CopyKey::SettingsSaveFailed,
        "activity-write-failed" => CopyKey::ActivityWriteFailed,
        _ => CopyKey::StatusAttention,
    })
}

pub fn map_intake(model: &ControlModel) -> IntakeKeys {
    if model.capture_attention {
        return IntakeKeys {
            headline: Some(CopyKey::StatusAttention),
            detail: None,
            action: (model.capture == "recording" || model.capture == "on")
                .then_some(CopyKey::ActionPause),
        };
    }
    if model.durable == "confirmed" && model.fault.as_deref() == Some("owner-stop-failed") {
        return IntakeKeys {
            headline: Some(CopyKey::StatusAttention),
            detail: None,
            action: None,
        };
    }
    if model.capture == "recording" || model.capture == "on" {
        return IntakeKeys {
            headline: Some(CopyKey::StatusOn),
            detail: Some(CopyKey::StatusOnDetail),
            action: Some(CopyKey::ActionPause),
        };
    }
    if model.capture == "paused" {
        return IntakeKeys {
            headline: Some(CopyKey::StatusPaused),
            detail: if model.pause_bounded == Some(false) {
                // Desktop activity can keep going through the pause, by the owner's choice.
                Some(if model.activity_through_pause {
                    CopyKey::StatusPausedActivityOnDetail
                } else {
                    CopyKey::StatusPausedDetail
                })
            } else {
                None
            },
            action: Some(CopyKey::ActionResume),
        };
    }
    if model.capture == "idle" {
        return IntakeKeys {
            headline: Some(CopyKey::StatusScreenWaiting),
            detail: Some(CopyKey::StatusScreenWaitingDetail),
            action: Some(CopyKey::ActionPause),
        };
    }
    if matches!(model.capture.as_str(), "waiting" | "stopped") {
        IntakeKeys {
            headline: Some(CopyKey::StatusOff),
            detail: Some(CopyKey::StatusOffDetail),
            action: Some(CopyKey::ActionPause),
        }
    } else {
        IntakeKeys {
            headline: Some(CopyKey::StatusUnavailable),
            detail: Some(CopyKey::StatusUnavailableDetail),
            action: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalHealthKeys {
    pub status: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub action_open: Option<CopyKey>,
}

pub fn attempt_pending(model: &ControlModel) -> bool {
    model
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.state == "pending")
}

/// The mark the journal tab's card draws, if it has one to draw.
pub fn mark_to_show(model: &ControlModel) -> Option<MarkRenderDocument> {
    let unverifiable = model.fault.as_deref() == Some("unverifiable")
        || model
            .attempt
            .as_ref()
            .is_some_and(|attempt| attempt.state == "unverifiable");
    if unverifiable {
        None
    } else if attempt_pending(model) || model.durable == "held" {
        model.pending_mark.clone()
    } else {
        model.confirmed_mark.clone()
    }
}

/// The heading over the card: only the confirmed mark, never one still being asked about.
pub fn mark_heading(model: &ControlModel) -> Option<CopyKey> {
    (model.durable == "confirmed" && !attempt_pending(model) && mark_to_show(model).is_some())
        .then_some(CopyKey::JournalMarkHeading)
}

/// With no mark to draw: true when one exists and can't be shown right now,
/// false when there's no journal identity here yet.
pub fn empty_mark_is_unavailable(model: &ControlModel) -> bool {
    model.durable != "unpaired"
        || model.view == solstone_linux::window_control::SessionView::Unknown
        || model
            .attempt
            .as_ref()
            .is_some_and(|attempt| matches!(attempt.state.as_str(), "pending" | "unverifiable"))
}

/// One unknown-journal row and what its detail shows.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownJournalRow {
    pub address: Option<String>,
    pub row: String,
    pub body: String,
    pub what_answered: Option<MarkRenderDocument>,
    pub caption: CopyKey,
}

/// A row per sighting, in the tracker's order. It's a notice beside the connection's
/// own state, and never changes it.
pub fn unknown_journal_rows(model: &ControlModel) -> Vec<UnknownJournalRow> {
    model
        .unknown_journals
        .iter()
        .map(|sighting| {
            let (row, body) = match &sighting.address {
                Some(address) => (
                    unknown_journal_seen_at(address),
                    unknown_journal_body_direct(address),
                ),
                None => (
                    CopyKey::UnknownJournalSeenThroughRelay.as_str().to_string(),
                    CopyKey::UnknownJournalBodyRelay.as_str().to_string(),
                ),
            };
            UnknownJournalRow {
                address: sighting.address.clone(),
                row,
                body,
                what_answered: sighting.mark.clone(),
                caption: if sighting.mark.is_some() {
                    CopyKey::UnknownJournalClaimed
                } else {
                    CopyKey::UnknownJournalNoIdentity
                },
            }
        })
        .collect()
}

pub fn map_journal_health(model: &ControlModel) -> JournalHealthKeys {
    if model.view == solstone_linux::window_control::SessionView::Unknown {
        return JournalHealthKeys {
            status: Some(CopyKey::StatusUnavailable),
            detail: Some(CopyKey::StatusUnavailableDetail),
            action_open: None,
        };
    }
    let action_open = if model.durable == "confirmed" {
        Some(CopyKey::ActionOpenJournal)
    } else {
        None
    };

    if model.durable == "confirmed"
        && (model.owner == "failed"
            || matches!(
                model.fault.as_deref(),
                Some("owner-start-failed" | "owner-stop-failed")
            ))
    {
        return JournalHealthKeys {
            status: Some(CopyKey::StatusAttention),
            detail: Some(CopyKey::PairingConfirmedUnavailable),
            action_open,
        };
    }

    if model.durable == "confirmed" {
        if model.health == "connected" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalConnected),
                detail: None,
                action_open,
            };
        }
        // `syncing` is a send in flight or not yet confirmed. It says what this computer
        // is doing, not that your journal is answering, which a send that hangs can't know.
        if model.health == "syncing" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalSyncing),
                detail: None,
                action_open,
            };
        }
        if model.health == "offline" || model.health == "transport-unavailable" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalOffline),
                detail: Some(CopyKey::JournalOfflineDetail),
                action_open,
            };
        }
        if model.health == "journal-mark-held" {
            // Same split as a held pairing below: a question asked in the window
            // answers itself there; a mark held on disk is answered at the terminal.
            return if attempt_pending(model) {
                JournalHealthKeys {
                    status: None,
                    detail: None,
                    action_open,
                }
            } else {
                JournalHealthKeys {
                    status: Some(CopyKey::PairingHeld),
                    detail: Some(CopyKey::PairingHeldRun),
                    action_open,
                }
            };
        }
        if matches!(
            model.health.as_str(),
            "unsafe-link-state"
                | "re-pair-required"
                | "token-persistence-failed"
                | "pairing-required"
                | "update-required"
        ) {
            return JournalHealthKeys {
                status: Some(CopyKey::StatusAttention),
                detail: Some(CopyKey::JournalAttentionDetail),
                action_open,
            };
        }
    } else if model.durable == "unpaired" {
        return JournalHealthKeys {
            status: Some(CopyKey::JournalNotPaired),
            detail: None,
            action_open: None,
        };
    } else if model.durable == "held" {
        // An attempt in the window asks its own question below the card; a pairing
        // held on disk can only be answered at the terminal.
        return if attempt_pending(model) {
            JournalHealthKeys {
                status: None,
                detail: None,
                action_open: None,
            }
        } else {
            JournalHealthKeys {
                status: Some(CopyKey::PairingHeld),
                detail: Some(CopyKey::PairingHeldRun),
                action_open: None,
            }
        };
    }

    JournalHealthKeys {
        status: Some(CopyKey::StatusChecking),
        detail: None,
        action_open,
    }
}

pub fn map_open_journal_outcome(result: &str) -> Option<CopyKey> {
    match solstone_linux::window_control::decode_control_str(result) {
        Ok(model) if model.open_journal.as_deref() == Some("opened") => None,
        _ => Some(CopyKey::BrowserOpenFailed),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairingKeys {
    pub title: Option<CopyKey>,
    pub detail: Option<CopyKey>,
    pub secondary_title: Option<CopyKey>,
    pub secondary_detail: Option<CopyKey>,
    pub confirm_action: Option<CopyKey>,
    pub reject_action: Option<CopyKey>,
    pub cancel_action: Option<CopyKey>,
    pub check_status: Option<CopyKey>,
}

pub fn validate_pairing_link(link: &str) -> Result<String, CopyKey> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return Err(CopyKey::PairingEmpty);
    }
    adapt_graphical_link(trimmed).map_err(|_| CopyKey::PairingBadLink)
}

pub fn map_pairing(model: &ControlModel, attempt_outcome: Option<&str>) -> PairingKeys {
    let is_replacement = model.durable == "confirmed"
        || (model.confirmed_mark.is_some() && model.pending_mark.is_some());

    if let Some(outcome) = attempt_outcome
        .or(model.fault.as_deref())
        .or_else(|| model.attempt.as_ref().map(|attempt| attempt.state.as_str()))
    {
        match outcome {
            "confirmed"
                if model.durable == "confirmed"
                    && model.owner == "running"
                    && model.fault.is_none() =>
            {
                return PairingKeys {
                    title: Some(CopyKey::PairingSuccess),
                    ..Default::default()
                };
            }
            "stale-answer" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingStale),
                    ..Default::default()
                };
            }
            "commit-outcome-unknown" | "outcome-unknown" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingOutcomeUnavailable),
                    check_status: Some(CopyKey::PairingCheckStatus),
                    ..Default::default()
                };
            }
            "owner-start-failed" if model.durable == "confirmed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingConfirmedUnavailable),
                    ..Default::default()
                };
            }
            "commit-failed" if model.durable != "held" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingSaveFailed),
                    ..Default::default()
                };
            }
            "commit-failed" => return PairingKeys::default(),
            "malformed-link" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingBadLink),
                    ..Default::default()
                };
            }
            "unverifiable" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingUnavailableTitle),
                    detail: Some(if is_replacement {
                        CopyKey::PairingReplacementUnavailableDetail
                    } else {
                        CopyKey::PairingUnavailableDetail
                    }),
                    ..Default::default()
                };
            }
            "pair-failed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingFailed),
                    ..Default::default()
                };
            }
            "owner-stop-failed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingConfirmedUnavailable),
                    ..Default::default()
                };
            }
            _ => {}
        }
    }

    if attempt_pending(model) {
        let has_verifiable_mark = model.pending_mark.is_some();
        if has_verifiable_mark {
            return PairingKeys {
                title: Some(CopyKey::PairingHeld),
                detail: if is_replacement {
                    Some(CopyKey::PairingReplacementHeldDetail)
                } else {
                    Some(CopyKey::PairingHeldDetail)
                },
                secondary_title: Some(CopyKey::PairingQuestion),
                secondary_detail: Some(CopyKey::PairingQuestionDetail),
                confirm_action: Some(CopyKey::PairingConfirm),
                reject_action: Some(CopyKey::PairingReject),
                cancel_action: None,
                check_status: None,
            };
        } else {
            return PairingKeys {
                title: Some(CopyKey::PairingUnavailableTitle),
                detail: if is_replacement {
                    Some(CopyKey::PairingReplacementUnavailableDetail)
                } else {
                    Some(CopyKey::PairingUnavailableDetail)
                },
                secondary_title: None,
                secondary_detail: None,
                confirm_action: None,
                reject_action: Some(CopyKey::PairingReject),
                cancel_action: Some(CopyKey::PairingCancel),
                check_status: None,
            };
        }
    }

    if let Some(att) = &model.attempt {
        match att.state.as_str() {
            "rejected" => {
                return PairingKeys {
                    title: if is_replacement {
                        Some(CopyKey::PairingReplacementRejectedTitle)
                    } else {
                        Some(CopyKey::PairingRejectedTitle)
                    },
                    detail: if is_replacement {
                        Some(CopyKey::PairingReplacementRejectedDetail)
                    } else {
                        Some(CopyKey::PairingRejectedDetail)
                    },
                    ..Default::default()
                };
            }
            "cancelled" => {
                return PairingKeys {
                    title: if is_replacement {
                        Some(CopyKey::PairingReplacementCancelled)
                    } else {
                        Some(CopyKey::PairingCancelled)
                    },
                    ..Default::default()
                };
            }
            "expired" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingExpired),
                    ..Default::default()
                };
            }
            "failed" => {
                return PairingKeys {
                    title: Some(CopyKey::PairingFailed),
                    ..Default::default()
                };
            }
            _ => {}
        }
    }

    PairingKeys::default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ServicePresenceKind {
    Absent,
    Inactive,
    Failed,
    Active,
    Uncertain,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceStartKeys {
    pub action: Option<CopyKey>,
    pub disclosure: Option<CopyKey>,
    pub status: Option<CopyKey>,
    pub check_status: Option<CopyKey>,
}

pub fn map_service_start(
    presence: ServicePresenceKind,
    in_flight: bool,
    definite_failure: bool,
    deadline_hit: bool,
) -> ServiceStartKeys {
    if in_flight {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStarting),
            disclosure: None,
            status: Some(CopyKey::StatusStartWaiting),
            check_status: None,
        };
    }
    if definite_failure {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: Some(CopyKey::StatusStartDisclosure),
            status: Some(CopyKey::StatusStartFailed),
            check_status: None,
        };
    }
    if deadline_hit {
        return ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: Some(CopyKey::StatusStartDisclosure),
            status: Some(CopyKey::StatusStartUnavailable),
            check_status: Some(CopyKey::ActionCheckStatus),
        };
    }

    match presence {
        ServicePresenceKind::Absent => ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: Some(CopyKey::StatusStartDisclosure),
            status: None,
            check_status: None,
        },
        ServicePresenceKind::Inactive | ServicePresenceKind::Failed => ServiceStartKeys {
            action: Some(CopyKey::ActionStart),
            disclosure: Some(CopyKey::StatusStartDisclosure),
            status: None,
            check_status: None,
        },
        ServicePresenceKind::Active | ServicePresenceKind::Uncertain => ServiceStartKeys::default(),
    }
}

#[cfg(test)]
pub fn validate_settings_inputs(segment_str: &str) -> Result<i64, CopyKey> {
    let segment: i64 = segment_str
        .trim()
        .parse()
        .map_err(|_| CopyKey::SettingsSegmentInvalid)?;
    if segment <= 0 {
        return Err(CopyKey::SettingsSegmentInvalid);
    }

    Ok(segment)
}

pub fn validate_settings_changes(
    saved: &SettingsModel,
    segment: Option<&str>,
) -> Result<SettingsModel, CopyKey> {
    let mut requested = saved.clone();
    if let Some(text) = segment {
        let value = crate::format::parse_integer(text).ok_or(CopyKey::SettingsSegmentInvalid)?;
        if value <= 0 {
            return Err(CopyKey::SettingsSegmentInvalid);
        }
        requested.segment_interval = value;
    }
    Ok(requested)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsKeys {
    pub load_failed: Option<CopyKey>,
    pub reload: Option<CopyKey>,
    pub save_action: Option<CopyKey>,
    pub saved_banner: Option<CopyKey>,
    pub restart_needed: Option<CopyKey>,
    pub restart_detail: Option<CopyKey>,
    pub restart_action: Option<CopyKey>,
    pub restart_failed: Option<CopyKey>,
}

pub fn map_settings_state(
    saved_available: bool,
    saved_settings: &SettingsModel,
    effective_settings: Option<&SettingsModel>,
    saved_verified: bool,
    latest_owner_matches: bool,
    verified_save_restart_failed: bool,
    recovery_failed_without_save: bool,
) -> SettingsKeys {
    if !saved_available {
        return SettingsKeys {
            load_failed: Some(CopyKey::SettingsLoadFailed),
            reload: Some(CopyKey::SettingsReload),
            save_action: None,
            restart_failed: recovery_failed_without_save
                .then_some(CopyKey::SettingsRestartRecoveryFailed),
            ..Default::default()
        };
    }

    let saved_banner = if saved_verified {
        Some(CopyKey::SettingsSaved)
    } else {
        None
    };

    let differ = match effective_settings {
        Some(eff) => eff != saved_settings,
        None => false,
    };

    let (restart_needed, restart_detail, restart_action) = if differ {
        (
            Some(CopyKey::SettingsRestartNeeded),
            Some(CopyKey::SettingsRestartDetail),
            if latest_owner_matches {
                Some(CopyKey::SettingsRestart)
            } else {
                None
            },
        )
    } else {
        (None, None, None)
    };

    let restart_failed = if verified_save_restart_failed {
        Some(CopyKey::SettingsRestartFailed)
    } else if recovery_failed_without_save {
        Some(CopyKey::SettingsRestartRecoveryFailed)
    } else {
        None
    };

    SettingsKeys {
        load_failed: None,
        reload: None,
        save_action: Some(CopyKey::SettingsSave),
        saved_banner,
        restart_needed,
        restart_detail,
        restart_action,
        restart_failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_journal_connected_and_offline() {
        let response = r#"{"v":1,"durable":"confirmed","owner":"running","health":"connected","capture":"recording","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":true}"#;
        for health in [
            "connected",
            "offline",
            "transport-unavailable",
            "listener-ready",
            "connecting",
            "syncing",
            "not-reported",
        ] {
            let model = solstone_linux::window_control::decode_control_str(
                &response.replace("connected", health),
            )
            .unwrap();
            let keys = map_journal_health(&model);
            assert_eq!(keys.action_open, Some(CopyKey::ActionOpenJournal));
            match health {
                "connected" => assert_eq!(keys.status, Some(CopyKey::JournalConnected)),
                "syncing" => assert_eq!(keys.status, Some(CopyKey::JournalSyncing)),
                "offline" | "transport-unavailable" => {
                    assert_eq!(keys.status, Some(CopyKey::JournalOffline));
                    assert_eq!(keys.detail, Some(CopyKey::JournalOfflineDetail));
                }
                _ => assert_eq!(keys.status, Some(CopyKey::StatusChecking)),
            }
        }
        for health in [
            "unsafe-link-state",
            "re-pair-required",
            "token-persistence-failed",
            "pairing-required",
            "update-required",
        ] {
            let model = solstone_linux::window_control::decode_control_str(
                &response.replace("connected", health),
            )
            .unwrap();
            let keys = map_journal_health(&model);
            assert_eq!(keys.status, Some(CopyKey::StatusAttention), "{health}");
            assert_eq!(
                keys.detail,
                Some(CopyKey::JournalAttentionDetail),
                "{health}"
            );
        }
        let model = solstone_linux::window_control::decode_control_str(
            &response.replace("connected", "journal-mark-held"),
        )
        .unwrap();
        assert_eq!(
            map_journal_health(&model).status,
            Some(CopyKey::PairingHeld)
        );
        let model = solstone_linux::window_control::decode_control_str(
            &response
                .replace("confirmed", "unpaired")
                .replace("running", "absent"),
        )
        .unwrap();
        assert_eq!(
            map_journal_health(&model).status,
            Some(CopyKey::JournalNotPaired)
        );
        assert_eq!(map_journal_health(&model).action_open, None);
        assert_eq!(
            map_journal_health(&ControlModel::unknown()).status,
            Some(CopyKey::StatusUnavailable)
        );
    }

    #[test]
    fn failed_local_activation_is_not_a_claim_that_the_journal_is_offline() {
        let response = r#"{"v":1,"durable":"confirmed","owner":"failed","health":"transport-unavailable","capture":"recording","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"fault":"owner-start-failed"}"#;
        let model = solstone_linux::window_control::decode_control_str(response).unwrap();
        let keys = map_journal_health(&model);
        assert_eq!(keys.status, Some(CopyKey::StatusAttention));
        assert_eq!(keys.detail, Some(CopyKey::PairingConfirmedUnavailable));
        assert_ne!(keys.status, Some(CopyKey::JournalOffline));
    }

    #[test]
    fn held_commit_and_unverifiable_attempts_keep_their_distinct_recovery() {
        let response = r#"{"v":1,"durable":"held","owner":"absent","health":"not-reported","capture":"recording","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"fault":"commit-failed"}"#;
        let held = solstone_linux::window_control::decode_control_str(response).unwrap();
        assert_eq!(map_fault(&held), Some(CopyKey::PairingOutcomeUnavailable));
        assert_ne!(
            map_pairing(&held, None).title,
            Some(CopyKey::PairingSaveFailed)
        );
        for replacement in [false, true] {
            let json = response
                .replace("held", if replacement { "confirmed" } else { "unpaired" })
                .replace("absent", if replacement { "running" } else { "absent" })
                .replace("commit-failed", "unverifiable");
            let model = solstone_linux::window_control::decode_control_str(&json).unwrap();
            let keys = map_pairing(&model, None);
            assert_eq!(keys.title, Some(CopyKey::PairingUnavailableTitle));
            assert_eq!(
                keys.detail,
                Some(if replacement {
                    CopyKey::PairingReplacementUnavailableDetail
                } else {
                    CopyKey::PairingUnavailableDetail
                })
            );
            assert!(keys.confirm_action.is_none());
        }
    }

    #[test]
    fn validation_does_not_rewrite_untouched_hand_edited_settings() {
        let saved = SettingsModel {
            draw_cursor: false,
            start_paused: true,
            segment_interval: 1500,
            activity_source: false,
            activity_titles: false,
            activity_urls: false,
            activity_exclude_apps: Vec::new(),
            activity_exclude_titles: Vec::new(),
            activity_through_pause: false,
            activity_only: false,
        };
        assert_eq!(validate_settings_changes(&saved, None), Ok(saved.clone()));
        assert_eq!(
            validate_settings_changes(&saved, Some("0")),
            Err(CopyKey::SettingsSegmentInvalid)
        );
    }

    #[test]
    fn test_pairing_outcomes() {
        let mut model = ControlModel::unknown();
        model.durable = "confirmed".into();
        let k = map_pairing(&model, Some("owner-start-failed"));
        assert_eq!(k.title, Some(CopyKey::PairingConfirmedUnavailable));

        let mut held_model = ControlModel::unknown();
        held_model.durable = "held".into();
        let k = map_pairing(&held_model, Some("commit-failed"));
        assert_ne!(k.title, Some(CopyKey::PairingSaveFailed));

        let mut unp_model = ControlModel::unknown();
        unp_model.durable = "unpaired".into();
        let k = map_pairing(&unp_model, Some("commit-failed"));
        assert_eq!(k.title, Some(CopyKey::PairingSaveFailed));

        for unk in ["commit-outcome-unknown", "outcome-unknown"] {
            let k = map_pairing(&unp_model, Some(unk));
            assert_eq!(k.title, Some(CopyKey::PairingOutcomeUnavailable));
            assert_eq!(k.check_status, Some(CopyKey::PairingCheckStatus));
        }

        let k = map_pairing(&unp_model, Some("stale-answer"));
        assert_eq!(k.title, Some(CopyKey::PairingStale));
    }

    #[test]
    fn pending_response_exposes_explicit_mark_confirmation() {
        let payload = r##"{"v":1,"durable":"unpaired","owner":"absent","health":"not-reported","capture":"recording","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":true,"attempt":{"id":"candidate-a","pairing_id":"ceremony-a","state":"pending","mark":"afoot·unfixed"},"pending_mark":{"icon1":{"name":"line","svg":"<path d='M2 2h20'/>","color":{"name":"amber","hex":"#f59e0b"},"rot":0},"icon2":{"name":"circle","svg":"<circle cx='12' cy='12' r='8'/>","color":{"name":"blue","hex":"#3b82f6"},"rot":45},"words":["afoot","unfixed"]}}"##;
        let model = solstone_linux::window_control::decode_control_str(payload).unwrap();
        assert_eq!(
            model.view,
            solstone_linux::window_control::SessionView::Pending
        );
        let keys = map_pairing(&model, None);
        assert!(keys.confirm_action.is_some());
        assert!(keys.reject_action.is_some());
        assert!(keys.cancel_action.is_none());
        assert!(attempt_pending(&model));
        assert_eq!(mark_to_show(&model), model.pending_mark);
    }

    fn held_on_disk_model(with_mark: bool) -> ControlModel {
        let mark = if with_mark {
            r##","pending_mark":{"icon1":{"name":"line","svg":"<path d='M2 2h20'/>","color":{"name":"amber","hex":"#f59e0b"},"rot":0},"icon2":{"name":"circle","svg":"<circle cx='12' cy='12' r='8'/>","color":{"name":"blue","hex":"#3b82f6"},"rot":45},"words":["afoot","unfixed"]}"##
        } else {
            ""
        };
        let payload = format!(
            r#"{{"v":1,"durable":"held","owner":"absent","health":"journal-mark-held","capture":"recording","settings":{{"draw_cursor":true,"start_paused":false,"segment_interval":300}},"saved_available":true{mark}}}"#
        );
        solstone_linux::window_control::decode_control_str(&payload).unwrap()
    }

    #[test]
    fn pairing_held_on_disk_names_the_terminal_and_asks_nothing_here() {
        for with_mark in [true, false] {
            let model = held_on_disk_model(with_mark);
            let journal = map_journal_health(&model);
            assert_eq!(journal.status, Some(CopyKey::PairingHeld));
            assert_eq!(journal.detail, Some(CopyKey::PairingHeldRun));
            assert_eq!(map_pairing(&model, None), PairingKeys::default());
            assert_eq!(mark_to_show(&model).is_some(), with_mark);
            assert!(empty_mark_is_unavailable(&model));
        }
    }

    #[test]
    fn empty_card_after_a_finished_attempt_on_an_unpaired_computer_is_not_unavailable() {
        let base = r#"{"v":1,"durable":"unpaired","owner":"absent","health":"not-reported","capture":"recording","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":true,"attempt":{"id":"a","pairing_id":"p","state":"STATE"}}"#;
        for (state, unavailable) in [
            ("rejected", false),
            ("cancelled", false),
            ("expired", false),
            ("failed", false),
            ("unverifiable", true),
        ] {
            let model =
                solstone_linux::window_control::decode_control_str(&base.replace("STATE", state))
                    .unwrap();
            assert_eq!(empty_mark_is_unavailable(&model), unavailable, "{state}");
            assert!(mark_to_show(&model).is_none());
        }
    }

    #[test]
    fn test_intake_attention_and_states() {
        let unavailable = map_intake(&ControlModel::unknown());
        assert_eq!(unavailable.headline, Some(CopyKey::StatusUnavailable));
        assert!(unavailable.action.is_none());
        let mut model = ControlModel::unknown();
        model.capture_attention = true;
        let k = map_intake(&model);
        assert_eq!(k.headline, Some(CopyKey::StatusAttention));

        let mut doc = ControlModel::unknown();
        doc.durable = "confirmed".into();
        doc.fault = Some("owner-stop-failed".into());
        let k = map_intake(&doc);
        assert_eq!(k.headline, Some(CopyKey::StatusAttention));

        let mut rec = ControlModel::unknown();
        rec.capture = "recording".into();
        let k = map_intake(&rec);
        assert_eq!(k.headline, Some(CopyKey::StatusOn));
        assert_eq!(k.detail, Some(CopyKey::StatusOnDetail));
        assert_eq!(k.action, Some(CopyKey::ActionPause));

        let mut pau = ControlModel::unknown();
        pau.capture = "paused".into();
        pau.pause_bounded = Some(false);
        let k = map_intake(&pau);
        assert_eq!(k.headline, Some(CopyKey::StatusPaused));
        assert_eq!(k.detail, Some(CopyKey::StatusPausedDetail));
        assert_eq!(k.action, Some(CopyKey::ActionResume));

        pau.activity_through_pause = true;
        let k = map_intake(&pau);
        assert_eq!(k.headline, Some(CopyKey::StatusPaused));
        assert_eq!(k.detail, Some(CopyKey::StatusPausedActivityOnDetail));
        assert_eq!(k.action, Some(CopyKey::ActionResume));
        pau.activity_through_pause = false;

        pau.pause_bounded = Some(true);
        let k = map_intake(&pau);
        assert_eq!(k.headline, Some(CopyKey::StatusPaused));
        assert_eq!(k.detail, None);
        assert_eq!(k.action, Some(CopyKey::ActionResume));
    }

    #[test]
    fn idle_screen_never_claims_audio_intake_is_off() {
        let payload = r#"{"v":1,"durable":"unpaired","owner":"absent","health":"pairing-required","capture":"idle","settings":{"draw_cursor":true,"start_paused":false,"segment_interval":300},"saved_available":true}"#;
        let model = solstone_linux::window_control::decode_control_str(payload).unwrap();
        let keys = map_intake(&model);
        assert_eq!(keys.headline, Some(CopyKey::StatusScreenWaiting));
        assert_eq!(keys.detail, Some(CopyKey::StatusScreenWaitingDetail));
        assert_eq!(keys.action, Some(CopyKey::ActionPause));
        assert_ne!(keys.headline, Some(CopyKey::StatusOff));
    }

    #[test]
    fn test_pairing_link_validation() {
        assert_eq!(validate_pairing_link(""), Err(CopyKey::PairingEmpty));
        assert_eq!(validate_pairing_link("   "), Err(CopyKey::PairingEmpty));
        assert_eq!(
            validate_pairing_link("invalid-link"),
            Err(CopyKey::PairingBadLink)
        );
        assert!(validate_pairing_link("https://go.solstone.app/p#PAIR123").is_ok());
    }

    #[test]
    fn test_service_start_mapping() {
        let absent = map_service_start(ServicePresenceKind::Absent, false, false, false);
        assert_eq!(absent.action, Some(CopyKey::ActionStart));
        assert_eq!(absent.disclosure, Some(CopyKey::StatusStartDisclosure));

        let inactive = map_service_start(ServicePresenceKind::Inactive, false, false, false);
        assert_eq!(inactive.action, Some(CopyKey::ActionStart));
        assert_eq!(inactive.disclosure, Some(CopyKey::StatusStartDisclosure));

        let active = map_service_start(ServicePresenceKind::Active, false, false, false);
        assert_eq!(active.action, None);

        let in_flight = map_service_start(ServicePresenceKind::Inactive, true, false, false);
        assert_eq!(in_flight.action, Some(CopyKey::ActionStarting));
        assert_eq!(in_flight.status, Some(CopyKey::StatusStartWaiting));

        let d_failure = map_service_start(ServicePresenceKind::Inactive, false, true, false);
        assert_eq!(d_failure.status, Some(CopyKey::StatusStartFailed));

        let deadline = map_service_start(ServicePresenceKind::Inactive, false, false, true);
        assert_eq!(deadline.status, Some(CopyKey::StatusStartUnavailable));
        assert_eq!(deadline.check_status, Some(CopyKey::ActionCheckStatus));
    }

    #[test]
    fn test_settings_mapping() {
        let saved = SettingsModel {
            draw_cursor: true,
            start_paused: false,
            segment_interval: 10,
            activity_source: false,
            activity_titles: false,
            activity_urls: false,
            activity_exclude_apps: Vec::new(),
            activity_exclude_titles: Vec::new(),
            activity_through_pause: false,
            activity_only: false,
        };
        let keys_unavailable = map_settings_state(false, &saved, None, false, true, false, false);
        assert_eq!(
            keys_unavailable.load_failed,
            Some(CopyKey::SettingsLoadFailed)
        );
        assert_eq!(keys_unavailable.reload, Some(CopyKey::SettingsReload));
        assert_eq!(keys_unavailable.save_action, None);

        let eff_same = saved.clone();
        let keys_same = map_settings_state(true, &saved, Some(&eff_same), true, true, false, false);
        assert_eq!(keys_same.saved_banner, Some(CopyKey::SettingsSaved));
        assert_eq!(keys_same.restart_needed, None);

        let eff_diff = SettingsModel {
            segment_interval: 20,
            ..saved.clone()
        };
        let keys_diff =
            map_settings_state(true, &saved, Some(&eff_diff), false, true, false, false);
        assert_eq!(
            keys_diff.restart_needed,
            Some(CopyKey::SettingsRestartNeeded)
        );
        assert_eq!(keys_diff.restart_action, Some(CopyKey::SettingsRestart));

        assert_eq!(
            validate_settings_inputs("0"),
            Err(CopyKey::SettingsSegmentInvalid)
        );
        assert_eq!(
            validate_settings_inputs("-1"),
            Err(CopyKey::SettingsSegmentInvalid)
        );
        assert!(validate_settings_inputs("10").is_ok());
    }

    fn sighting_mark() -> MarkRenderDocument {
        let icon = |name: &str, rot| solstone_linux::window_control::MarkIconDocument {
            name: name.into(),
            svg: "<path d=\"M0 0 L10 10\"/>".into(),
            color: solstone_linux::window_control::MarkColorDocument {
                name: "gold".into(),
                hex: "#ffd700".into(),
            },
            rot,
        };
        MarkRenderDocument {
            icon1: icon("star", 0),
            icon2: icon("moon", 45),
            words: ["amber".into(), "river".into()],
        }
    }

    #[test]
    fn each_sighting_gets_its_own_row_and_the_connection_stays_as_it_was() {
        use solstone_linux::window_control::UnknownJournalDocument;

        let mut model = ControlModel::unknown();
        model.view = solstone_linux::window_control::SessionView::Confirmed;
        model.durable = "confirmed".into();
        model.owner = "running".into();
        model.health = "connected".into();
        model.confirmed_mark = Some(sighting_mark());
        let health_alone = map_journal_health(&model);
        assert!(unknown_journal_rows(&model).is_empty());

        model.unknown_journals = vec![
            UnknownJournalDocument {
                address: Some("192.168.1.100:5015".into()),
                mark: Some(sighting_mark()),
            },
            UnknownJournalDocument {
                address: Some("192.168.1.101:5015".into()),
                mark: None,
            },
            UnknownJournalDocument {
                address: None,
                mark: Some(sighting_mark()),
            },
        ];
        assert_eq!(map_journal_health(&model), health_alone);
        assert_eq!(health_alone.status, Some(CopyKey::JournalConnected));

        let rows = unknown_journal_rows(&model);
        assert_eq!(rows.len(), 3);

        assert_eq!(rows[0].address.as_deref(), Some("192.168.1.100:5015"));
        assert_eq!(
            rows[0].row,
            solstone_linux::copy::unknown_journal_seen_at("192.168.1.100:5015")
        );
        assert_eq!(
            rows[0].body,
            solstone_linux::copy::unknown_journal_body_direct("192.168.1.100:5015")
        );
        assert_eq!(rows[0].what_answered, Some(sighting_mark()));
        assert_eq!(rows[0].caption, CopyKey::UnknownJournalClaimed);

        assert_eq!(rows[1].what_answered, None);
        assert_eq!(rows[1].caption, CopyKey::UnknownJournalNoIdentity);

        assert_eq!(rows[2].address, None);
        assert_eq!(
            rows[2].row,
            CopyKey::UnknownJournalSeenThroughRelay.as_str()
        );
        assert_eq!(rows[2].body, CopyKey::UnknownJournalBodyRelay.as_str());
        assert_eq!(rows[2].caption, CopyKey::UnknownJournalClaimed);
    }

    #[test]
    fn only_the_confirmed_mark_takes_the_heading() {
        let mut model = ControlModel::unknown();
        model.durable = "confirmed".into();
        model.confirmed_mark = Some(sighting_mark());
        assert_eq!(mark_heading(&model), Some(CopyKey::JournalMarkHeading));

        // A confirmed pairing whose mark can't be read has nothing to call that.
        model.confirmed_mark = None;
        assert_eq!(mark_heading(&model), None);
        model.confirmed_mark = Some(sighting_mark());

        // A replacement still being asked about is a different journal's mark.
        model.pending_mark = Some(sighting_mark());
        model.attempt = Some(solstone_linux::window_control::AttemptModel {
            id: "attempt".into(),
            pairing_id: "pairing".into(),
            state: "pending".into(),
            mark: None,
        });
        assert_eq!(mark_heading(&model), None);

        model.attempt = None;
        model.fault = Some("unverifiable".into());
        assert_eq!(mark_heading(&model), None);
        model.fault = None;

        model.durable = "held".into();
        assert_eq!(mark_heading(&model), None);
        model.durable = "unpaired".into();
        assert_eq!(mark_heading(&model), None);
    }
}
