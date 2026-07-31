// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::sources::{SourceSelection, SourceSelectionError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionOperation {
    New,
    Changed,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultSink {
    pub index: u32,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MuteStatus {
    #[default]
    Unknown,
    Muted,
    Unmuted,
    UnmutedQueryFailed {
        reason: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubscriptionState {
    pub default_sink: Option<DefaultSink>,
    pub mute_status: MuteStatus,
    pub source_selection: Option<SourceSelection>,
    pub degraded_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionEvent {
    Started,
    SourceSubscription {
        operation: SubscriptionOperation,
        index: u32,
    },
    SinkSubscriptionChanged {
        index: u32,
    },
    ServerSubscriptionChanged,
    SourcesResolved(Result<SourceSelection, SourceSelectionError>),
    DefaultSinkResolved(Result<DefaultSink, String>),
    MuteQueryResolved(Result<bool, String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionAction {
    QuerySources,
    QueryDefaultSink,
    QueryMute { sink_name: String },
    ApplySourceSelection(SourceSelection),
    EnterDegraded { reason: String },
    ApplyMuteBoundary { status: MuteStatus },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: SubscriptionState,
    pub actions: Vec<SubscriptionAction>,
}

pub fn transition(mut state: SubscriptionState, event: SubscriptionEvent) -> Transition {
    use SubscriptionAction::*;
    let actions = match event {
        SubscriptionEvent::Started => vec![QueryDefaultSink],
        SubscriptionEvent::SourceSubscription { .. } => vec![QuerySources],
        SubscriptionEvent::SinkSubscriptionChanged { index } => state
            .default_sink
            .as_ref()
            .filter(|sink| sink.index == index)
            .map_or_else(Vec::new, |sink| {
                vec![QueryMute {
                    sink_name: sink.name.clone(),
                }]
            }),
        SubscriptionEvent::ServerSubscriptionChanged => vec![QueryDefaultSink],
        SubscriptionEvent::SourcesResolved(Ok(selection)) => {
            // The redetect backstop re-resolves sources every few seconds. Announce a
            // selection only when it actually changes, or a steady-state desktop writes
            // the same two lines to the system log every tick, forever.
            let changed = state.source_selection.as_ref() != Some(&selection);
            state.source_selection = Some(selection.clone());
            state.degraded_reason = None;
            if changed {
                vec![ApplySourceSelection(selection)]
            } else {
                Vec::new()
            }
        }
        SubscriptionEvent::SourcesResolved(Err(error)) => {
            let reason = error.to_string();
            state.degraded_reason = Some(reason.clone());
            vec![EnterDegraded { reason }]
        }
        SubscriptionEvent::DefaultSinkResolved(Ok(sink)) => {
            let sink_name = sink.name.clone();
            state.default_sink = Some(sink);
            vec![QueryMute { sink_name }, QuerySources]
        }
        SubscriptionEvent::DefaultSinkResolved(Err(reason)) => {
            state.default_sink = None;
            let status = MuteStatus::UnmutedQueryFailed { reason };
            state.mute_status = status.clone();
            vec![ApplyMuteBoundary { status }, QuerySources]
        }
        SubscriptionEvent::MuteQueryResolved(result) => {
            let status = match result {
                Ok(true) => MuteStatus::Muted,
                Ok(false) => MuteStatus::Unmuted,
                Err(reason) => MuteStatus::UnmutedQueryFailed { reason },
            };
            if state.mute_status == status {
                vec![]
            } else {
                state.mute_status = status.clone();
                vec![ApplyMuteBoundary { status }]
            }
        }
    };
    Transition { state, actions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SourceDescriptor;

    fn state_with_sink() -> SubscriptionState {
        SubscriptionState {
            default_sink: Some(DefaultSink {
                index: 7,
                name: "default-sink".into(),
            }),
            mute_status: MuteStatus::Unknown,
            source_selection: None,
            degraded_reason: None,
        }
    }

    fn selection() -> SourceSelection {
        SourceSelection {
            microphone: SourceDescriptor {
                index: 1,
                name: Some("mic".into()),
                monitor_of_sink: None,
                monitor_of_sink_name: None,
            },
            monitor: SourceDescriptor {
                index: 2,
                name: Some("monitor".into()),
                monitor_of_sink: Some(7),
                monitor_of_sink_name: Some("default-sink".into()),
            },
            monitor_matches_default_sink: Some(true),
        }
    }

    #[test]
    fn every_source_change_requests_requery() {
        for operation in [
            SubscriptionOperation::New,
            SubscriptionOperation::Changed,
            SubscriptionOperation::Removed,
        ] {
            let outcome = transition(
                SubscriptionState::default(),
                SubscriptionEvent::SourceSubscription {
                    operation,
                    index: 4,
                },
            );
            assert_eq!(outcome.actions, vec![SubscriptionAction::QuerySources]);
        }
    }

    #[test]
    fn only_default_sink_change_requests_mute() {
        let outcome = transition(
            state_with_sink(),
            SubscriptionEvent::SinkSubscriptionChanged { index: 7 },
        );
        assert_eq!(
            outcome.actions,
            vec![SubscriptionAction::QueryMute {
                sink_name: "default-sink".into()
            }]
        );
        assert!(
            transition(
                state_with_sink(),
                SubscriptionEvent::SinkSubscriptionChanged { index: 8 }
            )
            .actions
            .is_empty()
        );
    }

    #[test]
    fn server_change_requests_default_sink() {
        assert_eq!(
            transition(
                SubscriptionState::default(),
                SubscriptionEvent::ServerSubscriptionChanged
            )
            .actions,
            vec![SubscriptionAction::QueryDefaultSink]
        );
    }

    #[test]
    fn resolved_server_change_replaces_default_sink_identity() {
        let requested = transition(
            state_with_sink(),
            SubscriptionEvent::ServerSubscriptionChanged,
        );
        let resolved = transition(
            requested.state,
            SubscriptionEvent::DefaultSinkResolved(Ok(DefaultSink {
                index: 9,
                name: "replacement-sink".into(),
            })),
        );
        assert_eq!(resolved.state.default_sink.as_ref().unwrap().index, 9);
        assert_eq!(
            resolved.actions,
            vec![
                SubscriptionAction::QueryMute {
                    sink_name: "replacement-sink".into()
                },
                SubscriptionAction::QuerySources,
            ]
        );
        assert!(
            transition(
                resolved.state.clone(),
                SubscriptionEvent::SinkSubscriptionChanged { index: 7 }
            )
            .actions
            .is_empty()
        );
        assert_eq!(
            transition(
                resolved.state,
                SubscriptionEvent::SinkSubscriptionChanged { index: 9 }
            )
            .actions,
            vec![SubscriptionAction::QueryMute {
                sink_name: "replacement-sink".into()
            }]
        );
    }

    #[test]
    fn source_resolution_after_mute_change_preserves_mute_status() {
        let muted = transition(
            state_with_sink(),
            SubscriptionEvent::MuteQueryResolved(Ok(true)),
        );
        let source_event = transition(
            muted.state,
            SubscriptionEvent::SourceSubscription {
                operation: SubscriptionOperation::Changed,
                index: 2,
            },
        );
        let resolved = transition(
            source_event.state,
            SubscriptionEvent::SourcesResolved(Ok(selection())),
        );
        assert_eq!(resolved.state.mute_status, MuteStatus::Muted);
        assert_eq!(resolved.state.source_selection, Some(selection()));
    }

    #[test]
    fn mute_failure_is_unmuted_and_preserves_reason() {
        let outcome = transition(
            SubscriptionState::default(),
            SubscriptionEvent::MuteQueryResolved(Err("server unavailable".into())),
        );
        let expected = MuteStatus::UnmutedQueryFailed {
            reason: "server unavailable".into(),
        };
        assert_eq!(outcome.state.mute_status, expected);
        assert_eq!(
            outcome.actions,
            vec![SubscriptionAction::ApplyMuteBoundary { status: expected }]
        );
    }

    #[test]
    fn default_sink_failure_is_also_unmuted() {
        let outcome = transition(
            SubscriptionState::default(),
            SubscriptionEvent::DefaultSinkResolved(Err("no default sink".into())),
        );
        assert_eq!(
            outcome.state.mute_status,
            MuteStatus::UnmutedQueryFailed {
                reason: "no default sink".into()
            }
        );
    }
}
