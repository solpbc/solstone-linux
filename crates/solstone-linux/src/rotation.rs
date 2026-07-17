// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationState {
    Running,
    AwaitingEos,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationEvent {
    RotateRequested,
    EosReceived,
    TimeoutElapsed {
        stream_identity: String,
        last_pipeline_state: String,
    },
    Error(String),
    ForceStop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotationAction {
    None,
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
    use RotationState::*;
    match (state.clone(), event) {
        (Running, RotateRequested) => Transition {
            state: AwaitingEos,
            actions: vec![SendEos],
        },
        (AwaitingEos, EosReceived) => Transition {
            state: Running,
            actions: vec![FinalizeCleanlyAndRestart],
        },
        (
            AwaitingEos,
            TimeoutElapsed {
                stream_identity,
                last_pipeline_state,
            },
        ) => Transition {
            state: Running,
            actions: vec![
                ReportNotCleanlyFinalized {
                    stream_identity,
                    last_pipeline_state,
                },
                ForcePipelineNull,
                RestartAfterUncleanFinalization,
            ],
        },
        (Running | AwaitingEos, Error(error)) => Transition {
            state: Failed,
            actions: vec![ReportError(error), ForcePipelineNull],
        },
        (Running, ForceStop) => Transition {
            state: AwaitingEos,
            actions: vec![SendEos],
        },
        (AwaitingEos, ForceStop) => Transition {
            state: Stopped,
            actions: vec![ForcePipelineNull, StopCleanly],
        },
        (Stopped | Failed, _) => Transition {
            state,
            actions: vec![None],
        },
        (Running, EosReceived | TimeoutElapsed { .. }) | (AwaitingEos, RotateRequested) => {
            Transition {
                state,
                actions: vec![None],
            }
        }
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
                state: RotationState::AwaitingEos,
                actions: vec![RotationAction::SendEos]
            }
        );
    }
    #[test]
    fn eos_restarts_cleanly() {
        assert_eq!(
            transition(RotationState::AwaitingEos, RotationEvent::EosReceived).actions,
            [RotationAction::FinalizeCleanlyAndRestart]
        );
    }
    #[test]
    fn timeout_is_loud_and_distinct() {
        let actions = transition(
            RotationState::AwaitingEos,
            RotationEvent::TimeoutElapsed {
                stream_identity: "monitor-0".into(),
                last_pipeline_state: "Playing".into(),
            },
        )
        .actions;
        assert_eq!(
            actions,
            [
                RotationAction::ReportNotCleanlyFinalized {
                    stream_identity: "monitor-0".into(),
                    last_pipeline_state: "Playing".into()
                },
                RotationAction::ForcePipelineNull,
                RotationAction::RestartAfterUncleanFinalization
            ]
        );
    }
    #[test]
    fn error_is_per_stream_failure() {
        assert_eq!(
            transition(RotationState::Running, RotationEvent::Error("boom".into())).state,
            RotationState::Failed
        );
    }
    #[test]
    fn force_stop_uses_eos_path() {
        assert_eq!(
            transition(RotationState::Running, RotationEvent::ForceStop).state,
            RotationState::AwaitingEos
        );
    }
    #[test]
    fn second_force_stop_is_bounded() {
        assert_eq!(
            transition(RotationState::AwaitingEos, RotationEvent::ForceStop).state,
            RotationState::Stopped
        );
    }
    #[test]
    fn terminal_states_are_idempotent() {
        assert_eq!(
            transition(RotationState::Stopped, RotationEvent::EosReceived).actions,
            [RotationAction::None]
        );
        assert_eq!(
            transition(RotationState::Failed, RotationEvent::RotateRequested).actions,
            [RotationAction::None]
        );
    }
}
