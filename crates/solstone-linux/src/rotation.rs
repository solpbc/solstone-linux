// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationIntent {
    Rotate,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationState {
    Running,
    AwaitingEos(RotationIntent),
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationEvent {
    RotateRequested,
    StopRequested,
    EosReceived,
    TimeoutElapsed {
        stream_identity: String,
        last_pipeline_state: String,
    },
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationAction {
    SendEos,
    FinalizeCleanlyAndRestart,
    ReportNotCleanlyFinalized {
        stream_identity: String,
        last_pipeline_state: String,
    },
    ForcePipelineNull,
    RestartAfterUncleanFinalization,
    StopCleanly,
    ReportError(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: RotationState,
    pub actions: Vec<RotationAction>,
}

pub fn transition(state: RotationState, event: RotationEvent) -> Transition {
    use RotationAction::*;
    use RotationEvent::*;
    use RotationIntent::*;
    use RotationState::*;

    match (state.clone(), event) {
        (Running, RotateRequested) => Transition {
            state: AwaitingEos(Rotate),
            actions: vec![SendEos],
        },
        (Running, StopRequested) => Transition {
            state: AwaitingEos(Stop),
            actions: vec![SendEos],
        },
        (AwaitingEos(Rotate), EosReceived) => Transition {
            state: Running,
            actions: vec![ForcePipelineNull, FinalizeCleanlyAndRestart],
        },
        (AwaitingEos(Stop), EosReceived) => Transition {
            state: Stopped,
            actions: vec![ForcePipelineNull, StopCleanly],
        },
        (
            AwaitingEos(intent),
            TimeoutElapsed {
                stream_identity,
                last_pipeline_state,
            },
        ) => unclean_transition(intent, stream_identity, last_pipeline_state),
        (AwaitingEos(intent), Error(error)) => {
            let mut transition = finish_unclean(intent);
            transition.actions.insert(0, ReportError(error));
            transition
        }
        (Running, Error(error)) => Transition {
            state: Failed,
            actions: vec![ReportError(error), ForcePipelineNull],
        },
        (Stopped | Failed, _) | (Running, EosReceived | TimeoutElapsed { .. }) => Transition {
            state,
            actions: Vec::new(),
        },
        (AwaitingEos(_), RotateRequested | StopRequested) => Transition {
            state,
            actions: Vec::new(),
        },
    }
}

fn unclean_transition(
    intent: RotationIntent,
    stream_identity: String,
    last_pipeline_state: String,
) -> Transition {
    let mut transition = finish_unclean(intent);
    transition.actions.insert(
        0,
        RotationAction::ReportNotCleanlyFinalized {
            stream_identity,
            last_pipeline_state,
        },
    );
    transition
}

fn finish_unclean(intent: RotationIntent) -> Transition {
    match intent {
        RotationIntent::Rotate => Transition {
            state: RotationState::Running,
            actions: vec![
                RotationAction::ForcePipelineNull,
                RotationAction::RestartAfterUncleanFinalization,
            ],
        },
        RotationIntent::Stop => Transition {
            state: RotationState::Stopped,
            actions: vec![
                RotationAction::ForcePipelineNull,
                RotationAction::StopCleanly,
            ],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_request_sends_eos() {
        assert_eq!(
            transition(RotationState::Running, RotationEvent::RotateRequested),
            Transition {
                state: RotationState::AwaitingEos(RotationIntent::Rotate),
                actions: vec![RotationAction::SendEos]
            }
        );
    }

    #[test]
    fn stop_request_sends_eos_without_restart_intent() {
        assert_eq!(
            transition(RotationState::Running, RotationEvent::StopRequested),
            Transition {
                state: RotationState::AwaitingEos(RotationIntent::Stop),
                actions: vec![RotationAction::SendEos]
            }
        );
    }

    #[test]
    fn rotation_eos_restarts_cleanly() {
        assert_eq!(
            transition(
                RotationState::AwaitingEos(RotationIntent::Rotate),
                RotationEvent::EosReceived
            ),
            Transition {
                state: RotationState::Running,
                actions: vec![
                    RotationAction::ForcePipelineNull,
                    RotationAction::FinalizeCleanlyAndRestart
                ]
            }
        );
    }

    #[test]
    fn stop_eos_stops_cleanly() {
        assert_eq!(
            transition(
                RotationState::AwaitingEos(RotationIntent::Stop),
                RotationEvent::EosReceived
            ),
            Transition {
                state: RotationState::Stopped,
                actions: vec![
                    RotationAction::ForcePipelineNull,
                    RotationAction::StopCleanly
                ]
            }
        );
    }

    #[test]
    fn rotation_timeout_is_loud_and_restarts() {
        let transition = transition(
            RotationState::AwaitingEos(RotationIntent::Rotate),
            timeout_event(),
        );
        assert_eq!(transition.state, RotationState::Running);
        assert_eq!(
            transition.actions,
            [
                not_cleanly_finalized(),
                RotationAction::ForcePipelineNull,
                RotationAction::RestartAfterUncleanFinalization
            ]
        );
    }

    #[test]
    fn stop_timeout_is_loud_and_never_restarts() {
        let transition = transition(
            RotationState::AwaitingEos(RotationIntent::Stop),
            timeout_event(),
        );
        assert_eq!(transition.state, RotationState::Stopped);
        assert_eq!(
            transition.actions,
            [
                not_cleanly_finalized(),
                RotationAction::ForcePipelineNull,
                RotationAction::StopCleanly
            ]
        );
    }

    #[test]
    fn errors_honor_the_pending_intent() {
        let rotate = transition(
            RotationState::AwaitingEos(RotationIntent::Rotate),
            RotationEvent::Error("boom".into()),
        );
        assert_eq!(rotate.state, RotationState::Running);
        assert_eq!(
            rotate.actions.last(),
            Some(&RotationAction::RestartAfterUncleanFinalization)
        );

        let stop = transition(
            RotationState::AwaitingEos(RotationIntent::Stop),
            RotationEvent::Error("boom".into()),
        );
        assert_eq!(stop.state, RotationState::Stopped);
        assert_eq!(stop.actions.last(), Some(&RotationAction::StopCleanly));
    }

    #[test]
    fn running_error_is_per_stream_failure() {
        assert_eq!(
            transition(RotationState::Running, RotationEvent::Error("boom".into())).state,
            RotationState::Failed
        );
    }

    #[test]
    fn terminal_states_are_idempotent() {
        assert!(
            transition(RotationState::Stopped, RotationEvent::EosReceived)
                .actions
                .is_empty()
        );
        assert!(
            transition(RotationState::Failed, RotationEvent::RotateRequested)
                .actions
                .is_empty()
        );
    }

    fn timeout_event() -> RotationEvent {
        RotationEvent::TimeoutElapsed {
            stream_identity: "monitor-0".into(),
            last_pipeline_state: "Playing".into(),
        }
    }

    fn not_cleanly_finalized() -> RotationAction {
        RotationAction::ReportNotCleanlyFinalized {
            stream_identity: "monitor-0".into(),
            last_pipeline_state: "Playing".into(),
        }
    }
}
