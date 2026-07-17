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
        SubscriptionEvent::Started => vec![QuerySources, QueryDefaultSink],
        SubscriptionEvent::SourceSubscription { .. } => vec![QuerySources],
        SubscriptionEvent::SinkSubscriptionChanged { index }
            if state
                .default_sink
                .as_ref()
                .is_some_and(|sink| sink.index == index) =>
        {
            vec![QueryMute {
                sink_name: state.default_sink.as_ref().unwrap().name.clone(),
            }]
        }
        SubscriptionEvent::SinkSubscriptionChanged { .. } => vec![],
        SubscriptionEvent::ServerSubscriptionChanged => vec![QueryDefaultSink],
        SubscriptionEvent::SourcesResolved(Ok(selection)) => {
            vec![ApplySourceSelection(selection)]
        }
        SubscriptionEvent::SourcesResolved(Err(error)) => vec![EnterDegraded {
            reason: error.to_string(),
        }],
        SubscriptionEvent::DefaultSinkResolved(Ok(sink)) => {
            let sink_name = sink.name.clone();
            state.default_sink = Some(sink);
            vec![QueryMute { sink_name }]
        }
        SubscriptionEvent::DefaultSinkResolved(Err(reason)) => {
            state.default_sink = None;
            let status = MuteStatus::UnmutedQueryFailed { reason };
            state.mute_status = status.clone();
            vec![ApplyMuteBoundary { status }]
        }
        SubscriptionEvent::MuteQueryResolved(result) => {
            let status = match result {
                Ok(true) => MuteStatus::Muted,
                Ok(false) => MuteStatus::Unmuted,
                Err(reason) => MuteStatus::UnmutedQueryFailed { reason },
            };
            state.mute_status = status.clone();
            vec![ApplyMuteBoundary { status }]
        }
    };
    Transition { state, actions }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_sink() -> SubscriptionState {
        SubscriptionState {
            default_sink: Some(DefaultSink {
                index: 7,
                name: "default-sink".into(),
            }),
            mute_status: MuteStatus::Unknown,
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
