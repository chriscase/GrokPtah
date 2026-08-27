//! Cancellation and cleanup.
//!
//! Cancellation has to be true at three different moments, and a design that
//! only handles one of them leaks:
//!
//! * **Before admission.** A cancelled run admits nothing. Easy, and the only
//!   part most implementations get right.
//! * **Between decision and dispatch.** A step decided a moment ago is
//!   dispatched now; cancellation arrived in between. The lease epoch moved,
//!   so [`crate::lease::FrameToken::admit`] refuses -- cancellation is not a
//!   flag the dispatcher has to remember to check, it is the same fence that
//!   already guards every commit.
//! * **After the last step.** Whatever the run acquired has to be given back
//!   exactly once, whether the run finished, failed, or was cut off.
//!
//! [`CleanupLedger`] handles the third. It is idempotent by construction: each
//! resource is released at most once, releasing twice is recorded as a no-op
//! rather than as a second release, and [`CleanupLedger::is_complete`] is the
//! condition a receipt is required to satisfy before it can claim an orderly
//! end. A run that stopped without releasing its lease did not finish, it
//! stopped.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lease::{EpochBump, RunLease};
use crate::vocabulary::{DenyReason, StopReason};

/// Why a run was cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelCause {
    /// A human stopped it.
    OperatorRequest,
    /// A human took the target over.
    OperatorTakeover,
    /// The surrounding session ended.
    SessionEnded,
    /// A budget line item or deadline ran out.
    BudgetExhausted,
    /// An unrecoverable refusal.
    TerminalRefusal,
}

impl CancelCause {
    pub const ALL: &'static [CancelCause] = &[
        Self::OperatorRequest,
        Self::OperatorTakeover,
        Self::SessionEnded,
        Self::BudgetExhausted,
        Self::TerminalRefusal,
    ];

    /// How a run cancelled for this cause is reported.
    #[must_use]
    pub fn stop_reason(self) -> StopReason {
        match self {
            Self::OperatorRequest | Self::OperatorTakeover | Self::SessionEnded => {
                StopReason::Cancelled
            }
            Self::BudgetExhausted => StopReason::BudgetExhausted,
            Self::TerminalRefusal => StopReason::Denied,
        }
    }
}

/// A cancellation signal bound to a lease epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelSignal {
    pub cause: CancelCause,
    /// The epoch the run moved to when it was cancelled.
    pub epoch: u64,
    pub at_millis: u64,
}

/// Things a run has to give back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// The run lease.
    Lease,
    /// The in-flight step, if any.
    InFlightStep,
    /// Region-capture handles held for evidence.
    EvidenceHandles,
    /// The pending approval prompt, if one is open.
    ApprovalPrompt,
}

impl Resource {
    pub const ALL: &'static [Resource] = &[
        Self::Lease,
        Self::InFlightStep,
        Self::EvidenceHandles,
        Self::ApprovalPrompt,
    ];
}

/// What a release attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOutcome {
    /// Released now.
    Released,
    /// Already released; nothing happened.
    AlreadyReleased,
    /// Never acquired; nothing to do.
    NotHeld,
}

/// Tracks acquisition and release so cleanup can be proved rather than
/// asserted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupLedger {
    held: BTreeMap<Resource, bool>,
    released: BTreeMap<Resource, bool>,
    cancelled: Option<CancelSignal>,
}

impl CleanupLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the run holds a resource.
    pub fn acquire(&mut self, resource: Resource) {
        self.held.insert(resource, true);
    }

    /// Release a resource. Idempotent: the second call is a recorded no-op.
    pub fn release(&mut self, resource: Resource) -> ReleaseOutcome {
        if !self.held.get(&resource).copied().unwrap_or(false) {
            return ReleaseOutcome::NotHeld;
        }
        if self.released.get(&resource).copied().unwrap_or(false) {
            return ReleaseOutcome::AlreadyReleased;
        }
        self.released.insert(resource, true);
        ReleaseOutcome::Released
    }

    /// Cancel the run: bump the lease epoch, then release everything.
    ///
    /// The epoch moves before anything is released, so a step that was decided
    /// under the old epoch cannot be dispatched during cleanup.
    pub fn cancel(
        &mut self,
        lease: &mut RunLease,
        cause: CancelCause,
        at_millis: u64,
    ) -> CancelSignal {
        if let Some(existing) = self.cancelled {
            return existing;
        }
        let bump = match cause {
            CancelCause::OperatorTakeover => EpochBump::OperatorTakeover,
            _ => EpochBump::Cancelled,
        };
        let epoch = lease.bump_epoch(bump);
        let signal = CancelSignal {
            cause,
            epoch,
            at_millis,
        };
        self.cancelled = Some(signal);
        for resource in Resource::ALL {
            let _ = self.release(*resource);
        }
        signal
    }

    #[must_use]
    pub fn cancellation(&self) -> Option<CancelSignal> {
        self.cancelled
    }

    /// Refuse admission once cancelled.
    pub fn check_admits(&self) -> Result<(), DenyReason> {
        if self.cancelled.is_some() {
            return Err(DenyReason::Cancelled);
        }
        Ok(())
    }

    /// True when everything acquired has been given back.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.held
            .iter()
            .filter(|(_, held)| **held)
            .all(|(resource, _)| self.released.get(resource).copied().unwrap_or(false))
    }

    /// Resources still outstanding, for the receipt's residue line.
    #[must_use]
    pub fn outstanding(&self) -> Vec<Resource> {
        self.held
            .iter()
            .filter(|(resource, held)| {
                **held && !self.released.get(*resource).copied().unwrap_or(false)
            })
            .map(|(resource, _)| *resource)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_holding_everything() -> CleanupLedger {
        let mut ledger = CleanupLedger::new();
        for resource in Resource::ALL {
            ledger.acquire(*resource);
        }
        ledger
    }

    #[test]
    fn cleanup_is_idempotent() {
        let mut ledger = ledger_holding_everything();
        assert_eq!(ledger.release(Resource::Lease), ReleaseOutcome::Released);
        assert_eq!(
            ledger.release(Resource::Lease),
            ReleaseOutcome::AlreadyReleased
        );
        let mut empty = CleanupLedger::new();
        assert_eq!(empty.release(Resource::Lease), ReleaseOutcome::NotHeld);
    }

    #[test]
    fn cancelling_twice_reports_the_first_signal() {
        let mut lease = RunLease::new("run-1", 10_000);
        let mut ledger = ledger_holding_everything();
        let first = ledger.cancel(&mut lease, CancelCause::OperatorRequest, 100);
        let epoch_after_first = lease.epoch;
        let second = ledger.cancel(&mut lease, CancelCause::SessionEnded, 200);
        assert_eq!(first, second);
        assert_eq!(
            lease.epoch, epoch_after_first,
            "second cancel moved the epoch"
        );
    }

    #[test]
    fn cancellation_moves_the_epoch_before_releasing() {
        let mut lease = RunLease::new("run-1", 10_000);
        let before = lease.epoch;
        let mut ledger = ledger_holding_everything();
        let signal = ledger.cancel(&mut lease, CancelCause::OperatorRequest, 5);
        assert!(signal.epoch > before);
        assert_eq!(signal.epoch, lease.epoch);
        assert!(ledger.is_complete());
        assert!(ledger.outstanding().is_empty());
    }

    #[test]
    fn a_cancelled_run_admits_nothing() {
        let mut lease = RunLease::new("run-1", 10_000);
        let mut ledger = ledger_holding_everything();
        assert!(ledger.check_admits().is_ok());
        ledger.cancel(&mut lease, CancelCause::OperatorRequest, 1);
        assert_eq!(ledger.check_admits().unwrap_err(), DenyReason::Cancelled);
    }

    #[test]
    fn a_takeover_leaves_the_operator_holding_the_lease() {
        let mut lease = RunLease::new("run-1", 10_000);
        let mut ledger = ledger_holding_everything();
        ledger.cancel(&mut lease, CancelCause::OperatorTakeover, 1);
        assert_eq!(lease.holder, crate::lease::LeaseHolder::Operator);
        assert_eq!(
            lease.check_agent_may_act(2).unwrap_err(),
            DenyReason::LeaseLost
        );
    }

    #[test]
    fn outstanding_resources_are_reported_not_hidden() {
        let mut ledger = CleanupLedger::new();
        ledger.acquire(Resource::Lease);
        ledger.acquire(Resource::EvidenceHandles);
        ledger.release(Resource::Lease);
        assert!(!ledger.is_complete());
        assert_eq!(ledger.outstanding(), vec![Resource::EvidenceHandles]);
    }

    #[test]
    fn every_cause_maps_to_a_stop_reason() {
        for cause in CancelCause::ALL {
            let reason = cause.stop_reason();
            assert!(!reason.is_orderly(), "{cause:?} reported an orderly end");
        }
    }
}
