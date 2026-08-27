//! The escalation ladder.
//!
//! Escalation buys capability and nothing else. When a step is handed from a
//! small local model to a stronger one, exactly three things change: which
//! class proposes, what it costs, and how deep a plan it may hold. Everything
//! that constitutes authority is carried across unchanged --
//! [`EscalationContext`] holds the grant's action classes, the pending
//! approval gates, and the redaction posture, and [`EscalationLadder::climb`]
//! copies them forward rather than recomputing them at the new tier.
//!
//! The reason that matters: escalation is the natural place for a privilege
//! bug. A stronger model is more capable, which makes "let it decide for
//! itself" tempting, and a step that was gated at the weak tier would quietly
//! stop being gated at the strong one. So the ladder is written so the only
//! way to *widen* authority is to construct a new context, and
//! `tests/cu_adaptive_escalation.rs` asserts the carried-forward set is
//! identical at every rung.
//!
//! Escalation is also bounded in both directions. It is budgeted (a run that
//! escalates every step exhausts [`crate::budget::BudgetLine::Escalations`]
//! and stops), and it is floored (a class that hands up more than
//! [`crate::tier::DeclaredTierCapability::max_escalation_bps`] of its steps
//! has breached its own declaration, which the suite gate treats as a
//! failure). A model that refuses everything is not safe, it is not working.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::budget::{BudgetLedger, BudgetLine};
use crate::gates::GateSet;
use crate::schema::IntentFamily;
use crate::tier::ModelTier;
use crate::vocabulary::{ApprovalReason, DenyReason, EscalationReason};

/// The authority a step carries, independent of who is proposing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EscalationContext {
    /// Which intent families the grant authorizes. Never widened by climbing.
    pub granted_families: BTreeSet<IntentFamily>,
    /// Gates opened and not yet answered. Carried across the hand-off.
    pub pending_gates: GateSet,
    /// The lease epoch the context was created under.
    pub epoch: u64,
}

impl EscalationContext {
    #[must_use]
    pub fn new(granted_families: BTreeSet<IntentFamily>, epoch: u64) -> Self {
        Self {
            granted_families,
            pending_gates: GateSet::new(),
            epoch,
        }
    }

    /// True when the grant authorizes this family.
    #[must_use]
    pub fn authorizes(&self, family: IntentFamily) -> bool {
        self.granted_families.contains(&family)
    }

    /// Refuse a step whose family is outside the grant.
    pub fn check_family(&self, family: IntentFamily) -> Result<(), DenyReason> {
        if self.authorizes(family) {
            Ok(())
        } else {
            Err(DenyReason::ClassOutsideGrant)
        }
    }
}

/// One rung climbed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EscalationRecord {
    pub step_index: u32,
    pub from: ModelTier,
    pub to: ModelTier,
    pub reason: EscalationReason,
    /// Gates that were pending at hand-off and remain pending after it.
    pub carried_gates: Vec<ApprovalReason>,
}

/// The ladder for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationLadder {
    current: ModelTier,
    base: ModelTier,
    records: Vec<EscalationRecord>,
}

impl EscalationLadder {
    #[must_use]
    pub fn new(base: ModelTier) -> Self {
        Self {
            current: base,
            base,
            records: Vec::new(),
        }
    }

    #[must_use]
    pub fn current(&self) -> ModelTier {
        self.current
    }

    #[must_use]
    pub fn base(&self) -> ModelTier {
        self.base
    }

    #[must_use]
    pub fn records(&self) -> &[EscalationRecord] {
        &self.records
    }

    #[must_use]
    pub fn climbs(&self) -> u32 {
        self.records.len() as u32
    }

    /// Hand one step to the next tier up.
    ///
    /// Debits the escalation budget first: a run that cannot afford the
    /// hand-off is refused before its tier changes, so the ledger and the
    /// ladder never disagree about what happened. Authority is carried
    /// forward, never recomputed.
    pub fn climb(
        &mut self,
        step_index: u32,
        reason: EscalationReason,
        context: &EscalationContext,
        ledger: &mut BudgetLedger,
    ) -> Result<EscalationContext, DenyReason> {
        let Some(next) = self.current.stronger() else {
            return Err(DenyReason::EscalationExhausted);
        };
        ledger.debit(BudgetLine::Escalations, 1)?;
        self.records.push(EscalationRecord {
            step_index,
            from: self.current,
            to: next,
            reason,
            carried_gates: context.pending_gates.iter().copied().collect(),
        });
        self.current = next;
        // The whole point: the new context is the old context. Nothing about
        // being stronger widens what the step may do.
        Ok(context.clone())
    }

    /// Return to the base tier for the next step.
    ///
    /// Escalation is per step, not per run: a run that needed a strong model
    /// once should not pay for one on every subsequent step. The records stay,
    /// so the receipt still shows what happened.
    pub fn settle(&mut self) {
        self.current = self.base;
    }

    /// True when the run handed up more of its steps than the base class
    /// declared it would.
    ///
    /// This is the "too timid" side of the envelope. A class that escalates
    /// everything scores perfectly on safety while doing no work, so exceeding
    /// the declared ceiling is a breach in the same way recklessness is.
    #[must_use]
    pub fn breaches_declared_ceiling(&self, steps_attempted: u32) -> bool {
        if steps_attempted == 0 {
            return false;
        }
        let ceiling = self.base.declared().max_escalation_bps;
        let observed = (u64::from(self.climbs()) * 10_000) / u64::from(steps_attempted);
        observed > u64::from(ceiling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::BudgetEnvelope;
    use crate::horizon::Horizon;
    use crate::profile::ProfileId;

    fn context() -> EscalationContext {
        let families: BTreeSet<IntentFamily> = [IntentFamily::Ambient, IntentFamily::Semantic]
            .into_iter()
            .collect();
        let mut context = EscalationContext::new(families, 0);
        context
            .pending_gates
            .insert(ApprovalReason::IrreversibleStep);
        context
    }

    fn ledger(tier: ModelTier) -> BudgetLedger {
        ledger_at(tier, Horizon::Medium)
    }

    fn ledger_at(tier: ModelTier, horizon: Horizon) -> BudgetLedger {
        BudgetLedger::new(BudgetEnvelope::for_run(
            &ProfileId::Balanced.spec(),
            tier,
            horizon,
        ))
    }

    #[test]
    fn climbing_never_widens_authority() {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        let mut ledger = ledger(ModelTier::SmallLocal);
        let before = context();
        let after = ladder
            .climb(0, EscalationReason::CapabilityGap, &before, &mut ledger)
            .unwrap();
        assert_eq!(after.granted_families, before.granted_families);
        assert_eq!(after.pending_gates, before.pending_gates);
        assert_eq!(after.epoch, before.epoch);
        assert_eq!(ladder.current(), ModelTier::MidVision);
        // The step that was gated before the hand-off is still gated after it.
        assert!(
            after
                .pending_gates
                .contains(&ApprovalReason::IrreversibleStep)
        );
    }

    #[test]
    fn a_pointer_family_outside_the_grant_stays_outside_it_at_every_rung() {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        let mut ledger = ledger(ModelTier::SmallLocal);
        let mut context = context();
        assert_eq!(
            context
                .check_family(IntentFamily::PointerFallback)
                .unwrap_err(),
            DenyReason::ClassOutsideGrant
        );
        while ladder.current().stronger().is_some() {
            context = ladder
                .climb(0, EscalationReason::CapabilityGap, &context, &mut ledger)
                .unwrap();
            assert_eq!(
                context
                    .check_family(IntentFamily::PointerFallback)
                    .unwrap_err(),
                DenyReason::ClassOutsideGrant
            );
        }
        assert_eq!(ladder.current(), ModelTier::StrongHosted);
    }

    #[test]
    fn the_ladder_runs_out_rather_than_looping() {
        let mut ladder = EscalationLadder::new(ModelTier::StrongHosted);
        let mut ledger = ledger(ModelTier::StrongHosted);
        assert_eq!(
            ladder
                .climb(0, EscalationReason::CapabilityGap, &context(), &mut ledger)
                .unwrap_err(),
            DenyReason::EscalationExhausted
        );
    }

    #[test]
    fn an_unaffordable_hand_off_leaves_the_tier_alone() {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        let mut ledger = ledger(ModelTier::SmallLocal);
        let allowance = ledger.envelope().max_escalations;
        for _ in 0..allowance {
            ladder.settle();
            let _ = ladder.climb(0, EscalationReason::CapabilityGap, &context(), &mut ledger);
        }
        ladder.settle();
        let before = ladder.current();
        assert_eq!(
            ladder
                .climb(0, EscalationReason::CapabilityGap, &context(), &mut ledger)
                .unwrap_err(),
            DenyReason::BudgetExhausted
        );
        assert_eq!(ladder.current(), before);
        assert_eq!(ladder.climbs() as u64, allowance);
    }

    #[test]
    fn handing_up_everything_breaches_the_declared_ceiling() {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        // A long horizon so the ceiling under test is the declared one rather
        // than the budget line.
        let mut ledger = ledger_at(ModelTier::SmallLocal, Horizon::Long);
        for step in 0..4 {
            ladder.settle();
            ladder
                .climb(
                    step,
                    EscalationReason::CapabilityGap,
                    &context(),
                    &mut ledger,
                )
                .unwrap();
        }
        // Four hand-offs across four steps is 10_000 bps, well past the small
        // class's declared 6_000.
        assert!(ladder.breaches_declared_ceiling(4));
        // The same four across forty steps is not.
        assert!(!ladder.breaches_declared_ceiling(40));
        assert!(!ladder.breaches_declared_ceiling(0));
    }

    #[test]
    fn settling_returns_to_base_but_keeps_the_record() {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        let mut ledger = ledger(ModelTier::SmallLocal);
        ladder
            .climb(
                3,
                EscalationReason::AmbiguityUnresolved,
                &context(),
                &mut ledger,
            )
            .unwrap();
        ladder.settle();
        assert_eq!(ladder.current(), ModelTier::SmallLocal);
        assert_eq!(ladder.records().len(), 1);
        assert_eq!(ladder.records()[0].step_index, 3);
        assert_eq!(ladder.records()[0].to, ModelTier::MidVision);
    }
}
