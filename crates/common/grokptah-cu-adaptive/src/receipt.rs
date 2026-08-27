//! Truthful receipts.
//!
//! A receipt is the only artifact that outlives a run, so it is the only place
//! a false claim can do lasting damage. Three things keep it honest.
//!
//! **It is derived, not written.** [`RunReceipt::build`] takes the ledger, the
//! budget, the cleanup record, and the escalation ladder and reads the numbers
//! out of them. There is no constructor that accepts a count, so a receipt
//! cannot be assembled from what someone believed happened.
//!
//! **It is re-checkable.** [`RunReceipt::reconcile`] re-derives every claim
//! from the same parts and fails on any mismatch. That is what makes a
//! deserialized receipt worth anything: a receipt that arrived from somewhere
//! else can be checked against a replayed run rather than trusted.
//!
//! **It states what it does not claim.** Every receipt carries the full
//! [`NotClaimed::MANDATORY`] set. This crate has no hardware, no virtual
//! machine, no provider, and no image model; its cost and latency units are
//! synthetic; and its approval answers come from a scripted policy rather than
//! from a person. A reader holding only the receipt still cannot mistake it
//! for a measurement of any of those things, because the receipt says so in a
//! field that [`RunReceipt::reconcile`] refuses to let it drop.
//!
//! The honesty extends to the boring cases. A run whose event tail was
//! truncated reports how many events it dropped rather than presenting the
//! tail as complete. A run that stopped without releasing its lease cannot
//! report an orderly end. A run that recorded a cancellation cannot report
//! [`StopReason::ObjectiveComplete`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::budget::{BudgetLedger, BudgetSnapshot};
use crate::cancel::{CancelSignal, CleanupLedger, Resource};
use crate::digest::{digest_canonical, domain};
use crate::escalation::{EscalationLadder, EscalationRecord};
use crate::horizon::Horizon;
use crate::ledger::RunLedger;
use crate::profile::ProfileId;
use crate::redaction::leak_scan;
use crate::tier::ModelTier;
use crate::vocabulary::{DenyReason, NotClaimed, StopReason};

/// Receipt wire version.
pub const RECEIPT_SCHEMA_VERSION: u16 = 1;

/// What the run actually ran against.
///
/// One variant, deliberately. This crate has exactly one substrate, so there
/// is no value of this field that would let a receipt from here claim to
/// describe a real machine, a real application, or a real model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Substrate {
    /// A deterministic in-process synthetic world. No hardware, no VM, no
    /// provider, no image model, no operator.
    SyntheticDeterministic,
}

/// Why a receipt was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    #[error("receipt schema version {found} is not {expected}")]
    SchemaVersion { found: u16, expected: u16 },
    #[error("receipt claims {claimed} for {field}, ledger recorded {recorded}")]
    CountMismatch {
        field: &'static str,
        claimed: u64,
        recorded: u64,
    },
    #[error("receipt refusal breakdown does not match the ledger")]
    DenialMismatch,
    #[error("receipt omits mandatory disclaimer {0:?}")]
    MissingDisclaimer(NotClaimed),
    #[error("budget snapshot is outside its own envelope")]
    BudgetOutsideEnvelope,
    #[error("receipt reports {0:?} but cleanup left resources outstanding")]
    UnreleasedResources(StopReason),
    #[error("receipt reports {0:?} but the run recorded a cancellation")]
    CancelledButClaimsCompletion(StopReason),
    #[error("receipt claims more approvals answered than were requested")]
    ApprovalAccounting,
    #[error("receipt trace digest does not match its contents")]
    DigestMismatch,
    #[error("receipt leaked forbidden content")]
    ContentLeak,
}

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunReceipt {
    pub schema_version: u16,
    pub substrate: Substrate,
    pub scenario_id: String,
    pub profile: ProfileId,
    pub base_tier: ModelTier,
    pub horizon: Horizon,
    pub steps_planned: u32,
    pub steps_committed: u32,
    pub steps_refused: u32,
    pub steps_disambiguated: u32,
    pub retries: u32,
    pub escalations: u32,
    pub approvals_requested: u32,
    pub approvals_granted: u32,
    pub approvals_refused: u32,
    pub disagreements: u32,
    pub postconditions_met: u32,
    pub postconditions_missed: u32,
    pub denials: BTreeMap<DenyReason, u32>,
    pub escalation_records: Vec<EscalationRecord>,
    pub budget: BudgetSnapshot,
    pub observations: u32,
    pub region_captures: u32,
    /// Total events, including any the bounded tail dropped.
    pub events_recorded: u64,
    /// Events the bounded tail dropped. Non-zero is normal on long horizons
    /// and is reported rather than hidden.
    pub events_dropped: u64,
    pub cleanup_complete: bool,
    pub cleanup_residue: Vec<Resource>,
    pub cancellation: Option<CancelSignal>,
    pub stop_reason: StopReason,
    /// The full mandatory disclaimer set. Reconciliation refuses a receipt
    /// that drops any of them.
    pub not_claimed: Vec<NotClaimed>,
    /// Digest over every field above.
    pub trace_digest: String,
}

/// The receipt's fields without its own digest, so the digest can be taken
/// over a stable, self-excluding view.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptBody<'a> {
    schema_version: u16,
    substrate: Substrate,
    scenario_id: &'a str,
    profile: ProfileId,
    base_tier: ModelTier,
    horizon: Horizon,
    steps_planned: u32,
    steps_committed: u32,
    steps_refused: u32,
    steps_disambiguated: u32,
    retries: u32,
    escalations: u32,
    approvals_requested: u32,
    approvals_granted: u32,
    approvals_refused: u32,
    disagreements: u32,
    postconditions_met: u32,
    postconditions_missed: u32,
    denials: &'a BTreeMap<DenyReason, u32>,
    escalation_records: &'a [EscalationRecord],
    budget: &'a BudgetSnapshot,
    observations: u32,
    region_captures: u32,
    events_recorded: u64,
    events_dropped: u64,
    cleanup_complete: bool,
    cleanup_residue: &'a [Resource],
    cancellation: Option<CancelSignal>,
    stop_reason: StopReason,
    not_claimed: &'a [NotClaimed],
}

impl RunReceipt {
    /// Derive a receipt from the parts of a finished run.
    ///
    /// Every number comes out of a ledger. There is deliberately no way to
    /// pass one in.
    #[must_use]
    pub fn build(
        scenario_id: impl Into<String>,
        profile: ProfileId,
        base_tier: ModelTier,
        horizon: Horizon,
        ledger: &RunLedger,
        budget: &BudgetLedger,
        cleanup: &CleanupLedger,
        escalation: &EscalationLadder,
        stop_reason: StopReason,
    ) -> Self {
        let mut receipt = Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            substrate: Substrate::SyntheticDeterministic,
            scenario_id: scenario_id.into(),
            profile,
            base_tier,
            horizon,
            steps_planned: ledger.planned(),
            steps_committed: ledger.committed(),
            steps_refused: ledger.refused(),
            steps_disambiguated: ledger.disambiguated(),
            retries: ledger.retried(),
            escalations: escalation.climbs(),
            approvals_requested: ledger.approvals_requested(),
            approvals_granted: ledger.approvals_granted(),
            approvals_refused: ledger.approvals_refused(),
            disagreements: ledger.disagreement_count(),
            postconditions_met: ledger.postconditions_met(),
            postconditions_missed: ledger.postconditions_missed(),
            denials: ledger.denials().clone(),
            escalation_records: escalation.records().to_vec(),
            budget: budget.snapshot(),
            observations: ledger.observations(),
            region_captures: ledger.region_captures(),
            events_recorded: ledger.events_recorded(),
            events_dropped: ledger.events_dropped(),
            cleanup_complete: cleanup.is_complete(),
            cleanup_residue: cleanup.outstanding(),
            cancellation: cleanup.cancellation(),
            stop_reason,
            not_claimed: NotClaimed::MANDATORY.to_vec(),
            trace_digest: String::new(),
        };
        receipt.trace_digest = receipt.body_digest();
        receipt
    }

    fn body_digest(&self) -> String {
        let body = ReceiptBody {
            schema_version: self.schema_version,
            substrate: self.substrate,
            scenario_id: &self.scenario_id,
            profile: self.profile,
            base_tier: self.base_tier,
            horizon: self.horizon,
            steps_planned: self.steps_planned,
            steps_committed: self.steps_committed,
            steps_refused: self.steps_refused,
            steps_disambiguated: self.steps_disambiguated,
            retries: self.retries,
            escalations: self.escalations,
            approvals_requested: self.approvals_requested,
            approvals_granted: self.approvals_granted,
            approvals_refused: self.approvals_refused,
            disagreements: self.disagreements,
            postconditions_met: self.postconditions_met,
            postconditions_missed: self.postconditions_missed,
            denials: &self.denials,
            escalation_records: &self.escalation_records,
            budget: &self.budget,
            observations: self.observations,
            region_captures: self.region_captures,
            events_recorded: self.events_recorded,
            events_dropped: self.events_dropped,
            cleanup_complete: self.cleanup_complete,
            cleanup_residue: &self.cleanup_residue,
            cancellation: self.cancellation,
            stop_reason: self.stop_reason,
            not_claimed: &self.not_claimed,
        };
        digest_canonical(domain::TRACE, &body).unwrap_or_default()
    }

    /// Re-derive every claim and fail on any mismatch.
    pub fn reconcile(
        &self,
        ledger: &RunLedger,
        budget: &BudgetLedger,
        cleanup: &CleanupLedger,
        escalation: &EscalationLadder,
    ) -> Result<(), ReceiptError> {
        if self.schema_version != RECEIPT_SCHEMA_VERSION {
            return Err(ReceiptError::SchemaVersion {
                found: self.schema_version,
                expected: RECEIPT_SCHEMA_VERSION,
            });
        }
        let checks: [(&'static str, u64, u64); 14] = [
            (
                "stepsPlanned",
                self.steps_planned.into(),
                ledger.planned().into(),
            ),
            (
                "stepsCommitted",
                self.steps_committed.into(),
                ledger.committed().into(),
            ),
            (
                "stepsRefused",
                self.steps_refused.into(),
                ledger.refused().into(),
            ),
            (
                "stepsDisambiguated",
                self.steps_disambiguated.into(),
                ledger.disambiguated().into(),
            ),
            ("retries", self.retries.into(), ledger.retried().into()),
            (
                "escalations",
                self.escalations.into(),
                escalation.climbs().into(),
            ),
            (
                "approvalsRequested",
                self.approvals_requested.into(),
                ledger.approvals_requested().into(),
            ),
            (
                "approvalsGranted",
                self.approvals_granted.into(),
                ledger.approvals_granted().into(),
            ),
            (
                "approvalsRefused",
                self.approvals_refused.into(),
                ledger.approvals_refused().into(),
            ),
            (
                "disagreements",
                self.disagreements.into(),
                ledger.disagreement_count().into(),
            ),
            (
                "observations",
                self.observations.into(),
                ledger.observations().into(),
            ),
            (
                "regionCaptures",
                self.region_captures.into(),
                ledger.region_captures().into(),
            ),
            (
                "eventsRecorded",
                self.events_recorded,
                ledger.events_recorded(),
            ),
            (
                "eventsDropped",
                self.events_dropped,
                ledger.events_dropped(),
            ),
        ];
        for (field, claimed, recorded) in checks {
            if claimed != recorded {
                return Err(ReceiptError::CountMismatch {
                    field,
                    claimed,
                    recorded,
                });
            }
        }
        if &self.denials != ledger.denials() {
            return Err(ReceiptError::DenialMismatch);
        }
        if self.denials.values().map(|n| u64::from(*n)).sum::<u64>()
            != u64::from(self.steps_refused)
        {
            return Err(ReceiptError::DenialMismatch);
        }
        if self.escalation_records.as_slice() != escalation.records() {
            return Err(ReceiptError::CountMismatch {
                field: "escalationRecords",
                claimed: self.escalation_records.len() as u64,
                recorded: escalation.records().len() as u64,
            });
        }
        if u64::from(self.approvals_granted) + u64::from(self.approvals_refused)
            > u64::from(self.approvals_requested)
        {
            return Err(ReceiptError::ApprovalAccounting);
        }
        if self.budget != budget.snapshot() || !self.budget.is_within_envelope() {
            return Err(ReceiptError::BudgetOutsideEnvelope);
        }
        for mandatory in NotClaimed::MANDATORY {
            if !self.not_claimed.contains(mandatory) {
                return Err(ReceiptError::MissingDisclaimer(*mandatory));
            }
        }
        if self.cleanup_complete != cleanup.is_complete()
            || self.cleanup_residue != cleanup.outstanding()
            || self.cancellation != cleanup.cancellation()
        {
            return Err(ReceiptError::UnreleasedResources(self.stop_reason));
        }
        if self.stop_reason.is_orderly() && !self.cleanup_complete {
            return Err(ReceiptError::UnreleasedResources(self.stop_reason));
        }
        if self.stop_reason == StopReason::ObjectiveComplete && self.cancellation.is_some() {
            return Err(ReceiptError::CancelledButClaimsCompletion(self.stop_reason));
        }
        if self.trace_digest != self.body_digest() {
            return Err(ReceiptError::DigestMismatch);
        }
        Ok(())
    }

    /// Check that nothing in the serialized receipt contains content it should
    /// not. Used by the leakage tests and by callers holding the literals a
    /// run was given.
    pub fn check_no_content(&self, forbidden: &[&str]) -> Result<(), ReceiptError> {
        let Ok(serialized) = serde_json::to_string(self) else {
            return Err(ReceiptError::ContentLeak);
        };
        if leak_scan(&serialized, forbidden).is_empty() {
            Ok(())
        } else {
            Err(ReceiptError::ContentLeak)
        }
    }

    /// True when the run reached a clean end with everything given back.
    #[must_use]
    pub fn is_orderly(&self) -> bool {
        self.stop_reason.is_orderly() && self.cleanup_complete && self.cancellation.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::BudgetEnvelope;
    use crate::cancel::CancelCause;
    use crate::lease::RunLease;
    use crate::ledger::LedgerEvent;

    struct Parts {
        ledger: RunLedger,
        budget: BudgetLedger,
        cleanup: CleanupLedger,
        escalation: EscalationLadder,
    }

    fn parts() -> Parts {
        let mut ledger = RunLedger::new();
        ledger.record(LedgerEvent::Planned { step_index: 0 });
        ledger.record(LedgerEvent::Observed { step_index: 0 });
        ledger.record(LedgerEvent::Committed { step_index: 0 });
        ledger.record(LedgerEvent::Refused {
            step_index: 1,
            reason: DenyReason::StaleFrame,
        });
        let mut cleanup = CleanupLedger::new();
        cleanup.acquire(Resource::Lease);
        cleanup.release(Resource::Lease);
        Parts {
            ledger,
            budget: BudgetLedger::new(BudgetEnvelope::for_run(
                &ProfileId::Balanced.spec(),
                ModelTier::SmallLocal,
                Horizon::Short,
            )),
            cleanup,
            escalation: EscalationLadder::new(ModelTier::SmallLocal),
        }
    }

    fn receipt(parts: &Parts, stop: StopReason) -> RunReceipt {
        RunReceipt::build(
            "scenario-1",
            ProfileId::Balanced,
            ModelTier::SmallLocal,
            Horizon::Short,
            &parts.ledger,
            &parts.budget,
            &parts.cleanup,
            &parts.escalation,
            stop,
        )
    }

    #[test]
    fn a_derived_receipt_reconciles() {
        let parts = parts();
        let receipt = receipt(&parts, StopReason::ObjectiveComplete);
        receipt
            .reconcile(
                &parts.ledger,
                &parts.budget,
                &parts.cleanup,
                &parts.escalation,
            )
            .unwrap();
        assert!(receipt.is_orderly());
    }

    #[test]
    fn an_inflated_count_is_rejected() {
        let parts = parts();
        let mut receipt = receipt(&parts, StopReason::ObjectiveComplete);
        receipt.steps_committed += 1;
        receipt.trace_digest = receipt.body_digest();
        let err = receipt
            .reconcile(
                &parts.ledger,
                &parts.budget,
                &parts.cleanup,
                &parts.escalation,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ReceiptError::CountMismatch {
                field: "stepsCommitted",
                ..
            }
        ));
    }

    #[test]
    fn a_deflated_count_is_rejected_too() {
        let parts = parts();
        let mut receipt = receipt(&parts, StopReason::ObjectiveComplete);
        receipt.steps_refused = 0;
        receipt.denials.clear();
        receipt.trace_digest = receipt.body_digest();
        let err = receipt
            .reconcile(
                &parts.ledger,
                &parts.budget,
                &parts.cleanup,
                &parts.escalation,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ReceiptError::CountMismatch {
                field: "stepsRefused",
                ..
            }
        ));
    }

    #[test]
    fn dropping_a_disclaimer_is_rejected() {
        let parts = parts();
        let mut receipt = receipt(&parts, StopReason::ObjectiveComplete);
        receipt
            .not_claimed
            .retain(|claim| *claim != NotClaimed::ProviderLatencyOrCost);
        receipt.trace_digest = receipt.body_digest();
        assert_eq!(
            receipt
                .reconcile(
                    &parts.ledger,
                    &parts.budget,
                    &parts.cleanup,
                    &parts.escalation
                )
                .unwrap_err(),
            ReceiptError::MissingDisclaimer(NotClaimed::ProviderLatencyOrCost)
        );
    }

    #[test]
    fn editing_a_receipt_without_redigesting_is_rejected() {
        let parts = parts();
        let mut receipt = receipt(&parts, StopReason::ObjectiveComplete);
        receipt.scenario_id = "something-else".into();
        assert_eq!(
            receipt
                .reconcile(
                    &parts.ledger,
                    &parts.budget,
                    &parts.cleanup,
                    &parts.escalation
                )
                .unwrap_err(),
            ReceiptError::DigestMismatch
        );
    }

    #[test]
    fn a_cancelled_run_cannot_report_completion() {
        let mut parts = parts();
        let mut lease = RunLease::new("run-1", 10_000);
        parts.cleanup.acquire(Resource::EvidenceHandles);
        parts
            .cleanup
            .cancel(&mut lease, CancelCause::OperatorRequest, 10);
        let receipt = receipt(&parts, StopReason::ObjectiveComplete);
        assert_eq!(
            receipt
                .reconcile(
                    &parts.ledger,
                    &parts.budget,
                    &parts.cleanup,
                    &parts.escalation
                )
                .unwrap_err(),
            ReceiptError::CancelledButClaimsCompletion(StopReason::ObjectiveComplete)
        );
    }

    #[test]
    fn an_orderly_end_requires_everything_to_be_given_back() {
        let mut parts = parts();
        parts.cleanup.acquire(Resource::EvidenceHandles);
        let receipt = receipt(&parts, StopReason::Abstained);
        assert!(!receipt.cleanup_complete);
        assert_eq!(receipt.cleanup_residue, vec![Resource::EvidenceHandles]);
        assert_eq!(
            receipt
                .reconcile(
                    &parts.ledger,
                    &parts.budget,
                    &parts.cleanup,
                    &parts.escalation
                )
                .unwrap_err(),
            ReceiptError::UnreleasedResources(StopReason::Abstained)
        );
    }

    #[test]
    fn a_receipt_reports_dropped_events_rather_than_the_tail_it_kept() {
        let mut parts = parts();
        for index in 0..(crate::ledger::MAX_RETAINED_EVENTS as u32 + 25) {
            parts
                .ledger
                .record(LedgerEvent::Observed { step_index: index });
        }
        let receipt = receipt(&parts, StopReason::HorizonExhausted);
        assert!(receipt.events_dropped > 0);
        assert_eq!(receipt.events_recorded, parts.ledger.events_recorded());
        receipt
            .reconcile(
                &parts.ledger,
                &parts.budget,
                &parts.cleanup,
                &parts.escalation,
            )
            .unwrap();
    }

    #[test]
    fn the_receipt_says_what_it_does_not_claim() {
        let parts = parts();
        let receipt = receipt(&parts, StopReason::ObjectiveComplete);
        assert_eq!(receipt.substrate, Substrate::SyntheticDeterministic);
        for mandatory in NotClaimed::MANDATORY {
            assert!(receipt.not_claimed.contains(mandatory));
        }
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains("synthetic_deterministic"));
        assert!(json.contains("real_hardware_timing"));
        assert!(json.contains("provider_latency_or_cost"));
        assert!(json.contains("image_model_accuracy"));
    }
}
