//! Closed execution and provider-send state machines.
//!
//! Transitions are explicit and fail closed. Public projections are derived
//! from these facts and must never lead them.

use grokptah_agent_sdk::authority::{PublicExecutionLifecycle, PublicSendState};
use serde::{Deserialize, Serialize};

use super::authority::SpineError;

/// Closed execution lifecycle. Persist every transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionLifecycle {
    /// Caller intent received, not yet admitted.
    Requested,
    /// Host-minted admission exists.
    Admitted,
    /// Durable Queued Run with completed idempotency receipt.
    Queued,
    /// Attempt registered; worker has not acknowledged start.
    Starting,
    /// Worker acknowledged start.
    Running,
    /// Cooperative shutdown in progress.
    Stopping,
    /// Crash, stream loss, or uncertain send is being reconciled.
    Reconciling,
    /// Terminal success before finalization.
    Succeeded,
    /// Terminal failure before finalization.
    Failed,
    /// Terminal cancellation before finalization.
    Cancelled,
    /// Delivery or liveness cannot be proven.
    Uncertain,
    /// Terminal truth persisted; capacity may be released.
    Finalized,
}

impl ExecutionLifecycle {
    /// Public projection.
    pub const fn as_public(self) -> PublicExecutionLifecycle {
        match self {
            Self::Requested => PublicExecutionLifecycle::Requested,
            Self::Admitted => PublicExecutionLifecycle::Admitted,
            Self::Queued => PublicExecutionLifecycle::Queued,
            Self::Starting => PublicExecutionLifecycle::Starting,
            Self::Running => PublicExecutionLifecycle::Running,
            Self::Stopping => PublicExecutionLifecycle::Stopping,
            Self::Reconciling => PublicExecutionLifecycle::Reconciling,
            Self::Succeeded => PublicExecutionLifecycle::Succeeded,
            Self::Failed => PublicExecutionLifecycle::Failed,
            Self::Cancelled => PublicExecutionLifecycle::Cancelled,
            Self::Uncertain => PublicExecutionLifecycle::Uncertain,
            Self::Finalized => PublicExecutionLifecycle::Finalized,
        }
    }

    /// Terminal before finalization.
    pub const fn is_terminal_outcome(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Uncertain
        )
    }

    /// No further mutation of the attempt is permitted.
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Finalized)
    }
}

/// Closed provider-send lattice. Distinct from execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendState {
    /// Prepared and provably not sent.
    KnownNotSent,
    /// Physical write in progress.
    Sending,
    /// Bytes may have left; acknowledgement is missing.
    Uncertain,
    /// Provider acknowledged the request.
    Sent,
    /// Stream is being consumed.
    Streaming,
    /// Stream/result completed.
    Completed,
    /// Stream/result failed.
    Failed,
}

impl ProviderSendState {
    /// Public projection.
    pub const fn as_public(self) -> PublicSendState {
        match self {
            Self::KnownNotSent => PublicSendState::KnownNotSent,
            Self::Sending => PublicSendState::Sending,
            Self::Uncertain => PublicSendState::Uncertain,
            Self::Sent => PublicSendState::Sent,
            Self::Streaming => PublicSendState::Streaming,
            Self::Completed => PublicSendState::Completed,
            Self::Failed => PublicSendState::Failed,
        }
    }

    /// Only KnownNotSent may auto-retry.
    pub const fn may_auto_retry(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }
}

/// Compare-and-swap one execution transition.
pub fn transition_lifecycle(
    current: ExecutionLifecycle,
    next: ExecutionLifecycle,
) -> Result<ExecutionLifecycle, SpineError> {
    let allowed = matches!(
        (current, next),
        (ExecutionLifecycle::Requested, ExecutionLifecycle::Admitted)
            | (ExecutionLifecycle::Admitted, ExecutionLifecycle::Queued)
            | (ExecutionLifecycle::Queued, ExecutionLifecycle::Starting)
            | (ExecutionLifecycle::Queued, ExecutionLifecycle::Cancelled)
            | (ExecutionLifecycle::Queued, ExecutionLifecycle::Failed)
            | (ExecutionLifecycle::Starting, ExecutionLifecycle::Running)
            | (ExecutionLifecycle::Starting, ExecutionLifecycle::Stopping)
            | (ExecutionLifecycle::Starting, ExecutionLifecycle::Uncertain)
            | (ExecutionLifecycle::Starting, ExecutionLifecycle::Failed)
            | (ExecutionLifecycle::Starting, ExecutionLifecycle::Cancelled)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Stopping)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Reconciling)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Succeeded)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Failed)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Cancelled)
            | (ExecutionLifecycle::Running, ExecutionLifecycle::Uncertain)
            | (ExecutionLifecycle::Stopping, ExecutionLifecycle::Cancelled)
            | (ExecutionLifecycle::Stopping, ExecutionLifecycle::Failed)
            | (ExecutionLifecycle::Stopping, ExecutionLifecycle::Uncertain)
            | (
                ExecutionLifecycle::Stopping,
                ExecutionLifecycle::Reconciling
            )
            | (
                ExecutionLifecycle::Reconciling,
                ExecutionLifecycle::Succeeded
            )
            | (ExecutionLifecycle::Reconciling, ExecutionLifecycle::Failed)
            | (
                ExecutionLifecycle::Reconciling,
                ExecutionLifecycle::Cancelled
            )
            | (
                ExecutionLifecycle::Reconciling,
                ExecutionLifecycle::Uncertain
            )
            | (ExecutionLifecycle::Succeeded, ExecutionLifecycle::Finalized)
            | (ExecutionLifecycle::Failed, ExecutionLifecycle::Finalized)
            | (ExecutionLifecycle::Cancelled, ExecutionLifecycle::Finalized)
            | (
                ExecutionLifecycle::Uncertain,
                ExecutionLifecycle::Reconciling
            )
            | (ExecutionLifecycle::Uncertain, ExecutionLifecycle::Finalized)
    );
    if allowed {
        Ok(next)
    } else {
        Err(SpineError::TransitionForbidden)
    }
}

/// Compare-and-swap one provider-send transition.
pub fn transition_send(
    current: ProviderSendState,
    next: ProviderSendState,
) -> Result<ProviderSendState, SpineError> {
    let allowed = matches!(
        (current, next),
        (ProviderSendState::KnownNotSent, ProviderSendState::Sending)
            | (ProviderSendState::Sending, ProviderSendState::Sent)
            | (ProviderSendState::Sending, ProviderSendState::Uncertain)
            | (ProviderSendState::Sending, ProviderSendState::Failed)
            | (ProviderSendState::Sent, ProviderSendState::Streaming)
            | (ProviderSendState::Sent, ProviderSendState::Completed)
            | (ProviderSendState::Sent, ProviderSendState::Failed)
            | (ProviderSendState::Sent, ProviderSendState::Uncertain)
            | (ProviderSendState::Streaming, ProviderSendState::Completed)
            | (ProviderSendState::Streaming, ProviderSendState::Failed)
            | (ProviderSendState::Streaming, ProviderSendState::Uncertain)
            | (ProviderSendState::Uncertain, ProviderSendState::Completed)
            | (ProviderSendState::Uncertain, ProviderSendState::Failed)
            | (ProviderSendState::Uncertain, ProviderSendState::Sent)
    );
    if allowed {
        Ok(next)
    } else {
        Err(SpineError::TransitionForbidden)
    }
}

/// Crash-cut recovery for the send lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRecovery {
    /// Safe to send once under the same provider request identity.
    AutoRetryKnownNotSent,
    /// Must not create a fresh paid mutation.
    UncertainNoRetry,
    /// Already terminal.
    AlreadySettled,
}

impl ProviderSendState {
    /// Recovery after a process cut at this state.
    pub const fn recover(self) -> SendRecovery {
        match self {
            Self::KnownNotSent => SendRecovery::AutoRetryKnownNotSent,
            Self::Sending | Self::Uncertain => SendRecovery::UncertainNoRetry,
            Self::Sent | Self::Streaming | Self::Completed | Self::Failed => {
                SendRecovery::AlreadySettled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_lifecycle_order_is_closed() {
        let mut state = ExecutionLifecycle::Requested;
        for next in [
            ExecutionLifecycle::Admitted,
            ExecutionLifecycle::Queued,
            ExecutionLifecycle::Starting,
            ExecutionLifecycle::Running,
            ExecutionLifecycle::Succeeded,
            ExecutionLifecycle::Finalized,
        ] {
            state = transition_lifecycle(state, next).unwrap();
        }
        assert_eq!(
            transition_lifecycle(ExecutionLifecycle::Finalized, ExecutionLifecycle::Queued),
            Err(SpineError::TransitionForbidden)
        );
    }

    #[test]
    fn only_known_not_sent_may_auto_retry() {
        assert!(ProviderSendState::KnownNotSent.may_auto_retry());
        assert!(!ProviderSendState::Sending.may_auto_retry());
        assert!(!ProviderSendState::Uncertain.may_auto_retry());
        assert_eq!(
            ProviderSendState::Sending.recover(),
            SendRecovery::UncertainNoRetry
        );
        assert_eq!(
            transition_send(
                ProviderSendState::Completed,
                ProviderSendState::KnownNotSent
            ),
            Err(SpineError::TransitionForbidden)
        );
    }

    #[test]
    fn cancel_before_send_is_queued_to_cancelled() {
        let state = transition_lifecycle(ExecutionLifecycle::Queued, ExecutionLifecycle::Cancelled)
            .unwrap();
        assert_eq!(
            transition_lifecycle(state, ExecutionLifecycle::Finalized).unwrap(),
            ExecutionLifecycle::Finalized
        );
    }
}
