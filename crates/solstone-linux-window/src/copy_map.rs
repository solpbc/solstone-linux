// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::copy::CopyKey;
use solstone_linux::window_control::{ControlModel, SettingsModel, adapt_graphical_link};

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
        if model.health == "offline" || model.health == "transport-unavailable" {
            return JournalHealthKeys {
                status: Some(CopyKey::JournalOffline),
                detail: Some(CopyKey::JournalOfflineDetail),
                action_open,
            };
        }
    } else if model.durable == "unpaired" {
        return JournalHealthKeys {
            status: Some(CopyKey::JournalNotPaired),
            detail: None,
            action_open: None,
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

    if model.durable == "held"
        || model.attempt.as_ref().map(|a| a.state.as_str()) == Some("pending")
    {
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
                cancel_action: Some(CopyKey::PairingCancel),
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
                "offline" | "transport-unavailable" => {
                    assert_eq!(keys.status, Some(CopyKey::JournalOffline));
                    assert_eq!(keys.detail, Some(CopyKey::JournalOfflineDetail));
                }
                _ => assert_ne!(keys.status, Some(CopyKey::JournalConnected)),
            }
        }
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
        assert!(keys.cancel_action.is_some());
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
}
