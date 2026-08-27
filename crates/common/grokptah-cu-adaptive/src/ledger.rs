//! The run ledger: what actually happened.
//!
//! A receipt is only as truthful as the thing it summarizes, so the ledger is
//! built to be reconcilable rather than merely informative. Two rules do that
//! work:
//!
//! * **Counters are exact and unbounded.** The event list is capped (a
//!   300-step run with retries produces a lot of events, and an uncapped list
//!   is a memory bug waiting for the long horizon), but the counters are not.
//!   When the list is truncated the ledger says so, in
//!   [`RunLedger::events_dropped`], and the receipt carries that number rather
//!   than quietly reporting the tail it happens to still hold.
//! * **Nothing is recorded twice and nothing is inferred.** Every counter is
//!   incremented at exactly one call site, so
//!   [`crate::receipt::RunReceipt::reconcile`] can compare the receipt's
//!   claims against the ledger's counters and fail on any mismatch. A receipt
//!   that claims more commits than the ledger recorded is rejected, and so is
//!   one that claims fewer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::executor::DisagreementKind;
use crate::schema::PostconditionOutcome;
use crate::vocabulary::{ApprovalReason, DenyReason, EscalationReason};

/// How many events the ledger keeps. Beyond this, the count is still exact and
/// the tail is dropped.
pub const MAX_RETAINED_EVENTS: usize = 2_048;

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LedgerEvent {
    Observed {
        step_index: u32,
    },
    RegionCaptured {
        step_index: u32,
    },
    Planned {
        step_index: u32,
    },
    Committed {
        step_index: u32,
    },
    Refused {
        step_index: u32,
        reason: DenyReason,
    },
    Disambiguated {
        step_index: u32,
    },
    ApprovalRequested {
        step_index: u32,
        reason: ApprovalReason,
    },
    ApprovalAnswered {
        step_index: u32,
        approved: bool,
    },
    Escalated {
        step_index: u32,
        reason: EscalationReason,
    },
    Retried {
        step_index: u32,
    },
    Postcondition {
        step_index: u32,
        outcome: PostconditionOutcome,
    },
    Disagreed {
        step_index: u32,
        kind: DisagreementKind,
    },
}

/// Exact counters plus a bounded event tail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLedger {
    events: Vec<LedgerEvent>,
    events_recorded: u64,
    events_dropped: u64,
    observations: u32,
    region_captures: u32,
    planned: u32,
    committed: u32,
    refused: u32,
    disambiguated: u32,
    retried: u32,
    approvals_requested: u32,
    approvals_granted: u32,
    approvals_refused: u32,
    escalated: u32,
    postconditions_met: u32,
    postconditions_missed: u32,
    postconditions_unchecked: u32,
    denials: BTreeMap<DenyReason, u32>,
    disagreements: BTreeMap<DisagreementKind, u32>,
}

impl RunLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an event and update the exact counters.
    pub fn record(&mut self, event: LedgerEvent) {
        match &event {
            LedgerEvent::Observed { .. } => self.observations += 1,
            LedgerEvent::RegionCaptured { .. } => self.region_captures += 1,
            LedgerEvent::Planned { .. } => self.planned += 1,
            LedgerEvent::Committed { .. } => self.committed += 1,
            LedgerEvent::Refused { reason, .. } => {
                self.refused += 1;
                *self.denials.entry(*reason).or_default() += 1;
            }
            LedgerEvent::Disambiguated { .. } => self.disambiguated += 1,
            LedgerEvent::ApprovalRequested { .. } => self.approvals_requested += 1,
            LedgerEvent::ApprovalAnswered { approved, .. } => {
                if *approved {
                    self.approvals_granted += 1;
                } else {
                    self.approvals_refused += 1;
                }
            }
            LedgerEvent::Escalated { .. } => self.escalated += 1,
            LedgerEvent::Retried { .. } => self.retried += 1,
            LedgerEvent::Postcondition { outcome, .. } => match outcome {
                PostconditionOutcome::Met => self.postconditions_met += 1,
                PostconditionOutcome::Missed => self.postconditions_missed += 1,
                PostconditionOutcome::NotChecked | PostconditionOutcome::NotApplicable => {
                    self.postconditions_unchecked += 1
                }
            },
            LedgerEvent::Disagreed { kind, .. } => {
                *self.disagreements.entry(*kind).or_default() += 1;
            }
        }
        self.events_recorded += 1;
        if self.events.len() < MAX_RETAINED_EVENTS {
            self.events.push(event);
        } else {
            self.events_dropped += 1;
        }
    }

    #[must_use]
    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }

    /// Total events, including any the tail no longer holds.
    #[must_use]
    pub fn events_recorded(&self) -> u64 {
        self.events_recorded
    }

    /// Events the bounded tail dropped. A receipt reports this rather than
    /// implying the tail is the whole story.
    #[must_use]
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped
    }

    #[must_use]
    pub fn observations(&self) -> u32 {
        self.observations
    }

    #[must_use]
    pub fn region_captures(&self) -> u32 {
        self.region_captures
    }

    #[must_use]
    pub fn planned(&self) -> u32 {
        self.planned
    }

    #[must_use]
    pub fn committed(&self) -> u32 {
        self.committed
    }

    #[must_use]
    pub fn refused(&self) -> u32 {
        self.refused
    }

    #[must_use]
    pub fn disambiguated(&self) -> u32 {
        self.disambiguated
    }

    #[must_use]
    pub fn retried(&self) -> u32 {
        self.retried
    }

    #[must_use]
    pub fn approvals_requested(&self) -> u32 {
        self.approvals_requested
    }

    #[must_use]
    pub fn approvals_granted(&self) -> u32 {
        self.approvals_granted
    }

    #[must_use]
    pub fn approvals_refused(&self) -> u32 {
        self.approvals_refused
    }

    #[must_use]
    pub fn escalated(&self) -> u32 {
        self.escalated
    }

    #[must_use]
    pub fn postconditions_met(&self) -> u32 {
        self.postconditions_met
    }

    #[must_use]
    pub fn postconditions_missed(&self) -> u32 {
        self.postconditions_missed
    }

    #[must_use]
    pub fn denials(&self) -> &BTreeMap<DenyReason, u32> {
        &self.denials
    }

    #[must_use]
    pub fn disagreements(&self) -> &BTreeMap<DisagreementKind, u32> {
        &self.disagreements
    }

    /// Total disagreements across all kinds.
    #[must_use]
    pub fn disagreement_count(&self) -> u32 {
        self.disagreements.values().copied().sum()
    }

    /// Steps the run actually tried to do something about, as opposed to
    /// refusing outright. This is the denominator for the "too timid" checks
    /// in [`crate::escalation::EscalationLadder::breaches_declared_ceiling`].
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.planned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_stay_exact_after_the_tail_is_capped() {
        let mut ledger = RunLedger::new();
        let total = MAX_RETAINED_EVENTS + 500;
        for index in 0..total {
            ledger.record(LedgerEvent::Observed {
                step_index: index as u32,
            });
        }
        assert_eq!(ledger.observations() as usize, total);
        assert_eq!(ledger.events().len(), MAX_RETAINED_EVENTS);
        assert_eq!(ledger.events_recorded() as usize, total);
        assert_eq!(
            ledger.events_dropped() as usize,
            total - MAX_RETAINED_EVENTS
        );
    }

    #[test]
    fn every_refusal_lands_in_exactly_one_bucket() {
        let mut ledger = RunLedger::new();
        ledger.record(LedgerEvent::Refused {
            step_index: 0,
            reason: DenyReason::StaleFrame,
        });
        ledger.record(LedgerEvent::Refused {
            step_index: 1,
            reason: DenyReason::StaleFrame,
        });
        ledger.record(LedgerEvent::Refused {
            step_index: 2,
            reason: DenyReason::BudgetExhausted,
        });
        assert_eq!(ledger.refused(), 3);
        assert_eq!(ledger.denials()[&DenyReason::StaleFrame], 2);
        assert_eq!(ledger.denials()[&DenyReason::BudgetExhausted], 1);
        assert_eq!(ledger.denials().values().sum::<u32>(), ledger.refused());
    }

    #[test]
    fn approvals_split_into_granted_and_refused() {
        let mut ledger = RunLedger::new();
        ledger.record(LedgerEvent::ApprovalRequested {
            step_index: 0,
            reason: ApprovalReason::IrreversibleStep,
        });
        ledger.record(LedgerEvent::ApprovalAnswered {
            step_index: 0,
            approved: true,
        });
        ledger.record(LedgerEvent::ApprovalRequested {
            step_index: 1,
            reason: ApprovalReason::PointerFallback,
        });
        ledger.record(LedgerEvent::ApprovalAnswered {
            step_index: 1,
            approved: false,
        });
        assert_eq!(ledger.approvals_requested(), 2);
        assert_eq!(ledger.approvals_granted(), 1);
        assert_eq!(ledger.approvals_refused(), 1);
    }

    #[test]
    fn disagreements_are_counted_by_kind() {
        let mut ledger = RunLedger::new();
        ledger.record(LedgerEvent::Disagreed {
            step_index: 0,
            kind: DisagreementKind::ExecutorRefusedCommit,
        });
        ledger.record(LedgerEvent::Disagreed {
            step_index: 1,
            kind: DisagreementKind::ExecutorRefusedCommit,
        });
        ledger.record(LedgerEvent::Disagreed {
            step_index: 2,
            kind: DisagreementKind::PlannerMoreConservative,
        });
        assert_eq!(ledger.disagreement_count(), 3);
        assert_eq!(
            ledger.disagreements()[&DisagreementKind::ExecutorRefusedCommit],
            2
        );
    }
}
