//! Model-budget envelopes, latency bounds, and resource limits.
//!
//! An envelope is what a run *declared* it would spend before it started. A
//! ledger is what it actually spent. Every debit is checked against the
//! envelope before it is taken, so a run cannot discover it is over budget
//! after the fact -- [`BudgetLedger::debit`] refuses first and spends second.
//!
//! Cost and latency units here are synthetic and dimensionless. They are
//! derived from [`crate::tier::DeclaredTierCapability`], which is a
//! declaration rather than a measurement, and they do not convert into tokens,
//! currency, or milliseconds on any real system. Every receipt carries
//! [`crate::vocabulary::NotClaimed::TokenAccounting`] and
//! [`crate::vocabulary::NotClaimed::ProviderLatencyOrCost`] for that reason.
//!
//! ## Why the envelope is not linear in horizon
//!
//! A run pays two kinds of cost. Per-run costs -- acquiring the lease, the
//! first plan, the closing receipt -- are paid once whatever the length. Per-
//! step costs scale with the number of steps. Scaling the whole envelope by
//! the step count would hand a 300-step run a hundred times the setup
//! allowance it needs, which is exactly the slack a runaway loop lives in. So
//! [`BudgetEnvelope::for_run`] adds a fixed base to a per-step term, and the
//! *ratio* of allowance to work therefore falls as the horizon grows. A long
//! run is held to a tighter per-step standard than a short one, which is the
//! right way round: it has more chances to amortize and more chances to drift.

use serde::{Deserialize, Serialize};

use crate::horizon::Horizon;
use crate::profile::{ExecutionProfile, ProfileId};
use crate::tier::ModelTier;
use crate::vocabulary::DenyReason;

/// The line items a run can exhaust.
///
/// Kept as an enum rather than as free-form strings so a refusal names a line
/// item from a closed set, and so a ledger cannot grow a category at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLine {
    PlannerCalls,
    ExecutorCalls,
    PlannerCostUnits,
    ExecutorCostUnits,
    Observations,
    RegionCaptures,
    CommittedActions,
    Retries,
    Escalations,
    ApprovalRequests,
    ObservationBytes,
}

impl BudgetLine {
    pub const ALL: &'static [BudgetLine] = &[
        Self::PlannerCalls,
        Self::ExecutorCalls,
        Self::PlannerCostUnits,
        Self::ExecutorCostUnits,
        Self::Observations,
        Self::RegionCaptures,
        Self::CommittedActions,
        Self::Retries,
        Self::Escalations,
        Self::ApprovalRequests,
        Self::ObservationBytes,
    ];
}

/// What a run declared it would spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetEnvelope {
    pub profile: ProfileId,
    pub tier: ModelTier,
    pub horizon: Horizon,
    pub max_planner_calls: u64,
    pub max_executor_calls: u64,
    pub max_planner_cost_units: u64,
    pub max_executor_cost_units: u64,
    pub max_observations: u64,
    pub max_region_captures: u64,
    pub max_committed_actions: u64,
    pub max_retries: u64,
    pub max_escalations: u64,
    pub max_approval_requests: u64,
    pub max_observation_bytes: u64,
    /// The longest one step may take before it is abandoned.
    pub step_deadline_millis: u64,
    /// The longest the whole run may take.
    pub run_deadline_millis: u64,
}

impl BudgetEnvelope {
    /// Build the envelope for one (profile, tier, horizon) combination.
    #[must_use]
    pub fn for_run(profile: &ExecutionProfile, tier: ModelTier, horizon: Horizon) -> Self {
        let steps = u64::from(horizon.steps());
        let declared = tier.declared();
        let planner_unit = u64::from(declared.planner_cost_units);
        let executor_unit = u64::from(declared.executor_cost_units);
        let ceiling_latency = Self::ladder_ceiling_latency(tier);

        // Per-run base plus per-step term. See the module note on why this is
        // not a straight multiply.
        let base_calls = 4;
        let planner_calls = base_calls + steps + steps / 4;
        let executor_calls = base_calls + steps * 2;

        let region_captures = match profile.region_policy {
            crate::profile::RegionPolicy::Never => 0,
            crate::profile::RegionPolicy::OnUncertainty => 2 + steps / 3,
            crate::profile::RegionPolicy::EveryStep => 2 + steps,
        };

        let observations = base_calls
            + steps
            + if profile.reobserve_before_mutation {
                steps
            } else {
                0
            }
            + if profile.verify_postcondition {
                steps
            } else {
                0
            };

        Self {
            profile: profile.id,
            tier,
            horizon,
            max_planner_calls: planner_calls,
            max_executor_calls: executor_calls,
            max_planner_cost_units: planner_calls * planner_unit,
            max_executor_cost_units: executor_calls * executor_unit,
            max_observations: observations,
            max_region_captures: region_captures,
            max_committed_actions: steps,
            max_retries: u64::from(profile.max_retries_per_run).max(steps / 8),
            max_escalations: u64::from(profile.max_escalations_per_run).max(steps / 32),
            max_approval_requests: 2 + steps / 10,
            // 4 KiB per observation is a synthetic accounting unit, not a
            // measurement of any real accessibility tree.
            max_observation_bytes: observations * 4_096,
            // Deadlines are sized for the most expensive tier the ladder can
            // reach, not for the base tier. A run that escalates is still the
            // same run: if the deadline were sized for the cheap class, the
            // first hand-off would blow it, and every escalation scenario
            // would report a timeout instead of whatever it was testing. The
            // cost lines above stay sized from the base tier, because that is
            // what the run normally pays.
            step_deadline_millis: ceiling_latency * 4,
            run_deadline_millis: ceiling_latency * 4 * (steps + base_calls),
        }
    }

    /// The per-step latency of the strongest tier reachable from `tier`.
    fn ladder_ceiling_latency(tier: ModelTier) -> u64 {
        let mut current = tier;
        let mut worst = current.declared().nominal_step_latency_millis;
        while let Some(next) = current.stronger() {
            worst = worst.max(next.declared().nominal_step_latency_millis);
            current = next;
        }
        worst
    }

    /// The same envelope, tightened to a fraction of itself.
    ///
    /// Used by the squeeze scenario. Scaling by a fraction rather than to a
    /// fixed number means "too tight" means the same thing at every horizon.
    /// Deadlines scale too: a squeezed run is not given unlimited time to
    /// spend a smaller allowance.
    ///
    /// A line that was already zero stays zero. A non-zero line never scales
    /// below one, because a budget of zero for something a run must do once
    /// makes every scenario fail identically for a reason that is not the one
    /// under test.
    #[must_use]
    pub fn scaled(self, bps: u32) -> Self {
        let scale = |value: u64| -> u64 {
            if value == 0 {
                return 0;
            }
            (value.saturating_mul(u64::from(bps)) / 10_000).max(1)
        };
        Self {
            max_planner_calls: scale(self.max_planner_calls),
            max_executor_calls: scale(self.max_executor_calls),
            max_planner_cost_units: scale(self.max_planner_cost_units),
            max_executor_cost_units: scale(self.max_executor_cost_units),
            max_observations: scale(self.max_observations),
            max_region_captures: scale(self.max_region_captures),
            max_committed_actions: scale(self.max_committed_actions),
            max_retries: scale(self.max_retries),
            max_escalations: scale(self.max_escalations),
            max_approval_requests: scale(self.max_approval_requests),
            max_observation_bytes: scale(self.max_observation_bytes),
            step_deadline_millis: self.step_deadline_millis,
            run_deadline_millis: scale(self.run_deadline_millis),
            ..self
        }
    }

    /// The ceiling for one line item.
    #[must_use]
    pub fn limit(&self, line: BudgetLine) -> u64 {
        match line {
            BudgetLine::PlannerCalls => self.max_planner_calls,
            BudgetLine::ExecutorCalls => self.max_executor_calls,
            BudgetLine::PlannerCostUnits => self.max_planner_cost_units,
            BudgetLine::ExecutorCostUnits => self.max_executor_cost_units,
            BudgetLine::Observations => self.max_observations,
            BudgetLine::RegionCaptures => self.max_region_captures,
            BudgetLine::CommittedActions => self.max_committed_actions,
            BudgetLine::Retries => self.max_retries,
            BudgetLine::Escalations => self.max_escalations,
            BudgetLine::ApprovalRequests => self.max_approval_requests,
            BudgetLine::ObservationBytes => self.max_observation_bytes,
        }
    }
}

/// What a run has actually spent.
///
/// The ledger is append-only in effect: nothing subtracts, and a refused debit
/// leaves the ledger untouched. That is what lets a receipt be reconciled
/// against it -- see [`crate::receipt::RunReceipt::reconcile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetLedger {
    envelope: BudgetEnvelope,
    spent: [u64; BudgetLedger::LINES],
    elapsed_millis: u64,
    refusals: u32,
}

impl BudgetLedger {
    const LINES: usize = 11;

    #[must_use]
    pub fn new(envelope: BudgetEnvelope) -> Self {
        debug_assert_eq!(Self::LINES, BudgetLine::ALL.len());
        Self {
            envelope,
            spent: [0; Self::LINES],
            elapsed_millis: 0,
            refusals: 0,
        }
    }

    #[must_use]
    pub fn envelope(&self) -> &BudgetEnvelope {
        &self.envelope
    }

    #[must_use]
    pub fn spent(&self, line: BudgetLine) -> u64 {
        self.spent[Self::index(line)]
    }

    #[must_use]
    pub fn remaining(&self, line: BudgetLine) -> u64 {
        self.envelope.limit(line).saturating_sub(self.spent(line))
    }

    #[must_use]
    pub fn elapsed_millis(&self) -> u64 {
        self.elapsed_millis
    }

    /// How many debits were refused. A run that ends with refusals recorded
    /// but claims a clean completion is not reconcilable.
    #[must_use]
    pub fn refusals(&self) -> u32 {
        self.refusals
    }

    /// Spend, or refuse and change nothing.
    pub fn debit(&mut self, line: BudgetLine, amount: u64) -> Result<(), DenyReason> {
        let index = Self::index(line);
        let next = self.spent[index].saturating_add(amount);
        if next > self.envelope.limit(line) {
            self.refusals = self.refusals.saturating_add(1);
            return Err(DenyReason::BudgetExhausted);
        }
        self.spent[index] = next;
        Ok(())
    }

    /// Advance the synthetic clock, refusing when a deadline is crossed.
    ///
    /// The step deadline is checked against the step's own duration and the
    /// run deadline against the total, so a run cannot pass by taking one
    /// enormous step or by taking many merely long ones.
    pub fn advance(&mut self, step_millis: u64) -> Result<(), DenyReason> {
        if step_millis > self.envelope.step_deadline_millis {
            self.refusals = self.refusals.saturating_add(1);
            return Err(DenyReason::StepDeadlineExceeded);
        }
        let next = self.elapsed_millis.saturating_add(step_millis);
        if next > self.envelope.run_deadline_millis {
            self.refusals = self.refusals.saturating_add(1);
            return Err(DenyReason::RunDeadlineExceeded);
        }
        self.elapsed_millis = next;
        Ok(())
    }

    /// A serializable snapshot for the receipt.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            envelope: self.envelope,
            spent: BudgetLine::ALL
                .iter()
                .map(|line| LineSpend {
                    line: *line,
                    spent: self.spent(*line),
                    limit: self.envelope.limit(*line),
                })
                .collect(),
            elapsed_millis: self.elapsed_millis,
            refusals: self.refusals,
        }
    }

    fn index(line: BudgetLine) -> usize {
        match line {
            BudgetLine::PlannerCalls => 0,
            BudgetLine::ExecutorCalls => 1,
            BudgetLine::PlannerCostUnits => 2,
            BudgetLine::ExecutorCostUnits => 3,
            BudgetLine::Observations => 4,
            BudgetLine::RegionCaptures => 5,
            BudgetLine::CommittedActions => 6,
            BudgetLine::Retries => 7,
            BudgetLine::Escalations => 8,
            BudgetLine::ApprovalRequests => 9,
            BudgetLine::ObservationBytes => 10,
        }
    }
}

/// One line item's final state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineSpend {
    pub line: BudgetLine,
    pub spent: u64,
    pub limit: u64,
}

/// The ledger as it appears in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    pub envelope: BudgetEnvelope,
    pub spent: Vec<LineSpend>,
    pub elapsed_millis: u64,
    pub refusals: u32,
}

impl BudgetSnapshot {
    /// True when no line item is over its limit. A snapshot that fails this is
    /// evidence of a bug in the ledger rather than of an over-budget run,
    /// since [`BudgetLedger::debit`] refuses before spending.
    #[must_use]
    pub fn is_within_envelope(&self) -> bool {
        self.spent.iter().all(|line| line.spent <= line.limit)
            && self.elapsed_millis <= self.envelope.run_deadline_millis
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(profile: ProfileId, tier: ModelTier, horizon: Horizon) -> BudgetEnvelope {
        BudgetEnvelope::for_run(&profile.spec(), tier, horizon)
    }

    #[test]
    fn a_refused_debit_changes_nothing() {
        let mut ledger = BudgetLedger::new(envelope(
            ProfileId::Economy,
            ModelTier::SmallLocal,
            Horizon::Short,
        ));
        let limit = ledger.envelope().limit(BudgetLine::CommittedActions);
        ledger.debit(BudgetLine::CommittedActions, limit).unwrap();
        let before = ledger.spent(BudgetLine::CommittedActions);
        assert_eq!(
            ledger.debit(BudgetLine::CommittedActions, 1).unwrap_err(),
            DenyReason::BudgetExhausted
        );
        assert_eq!(ledger.spent(BudgetLine::CommittedActions), before);
        assert_eq!(ledger.refusals(), 1);
    }

    #[test]
    fn every_line_item_has_a_distinct_slot() {
        let mut ledger = BudgetLedger::new(envelope(
            ProfileId::HighAssurance,
            ModelTier::StrongHosted,
            Horizon::Long,
        ));
        for (n, line) in BudgetLine::ALL.iter().enumerate() {
            ledger.debit(*line, n as u64).unwrap();
        }
        for (n, line) in BudgetLine::ALL.iter().enumerate() {
            assert_eq!(ledger.spent(*line), n as u64, "{line:?} shares a slot");
        }
    }

    #[test]
    fn allowance_per_step_tightens_as_the_horizon_grows() {
        let profile = ProfileId::Balanced.spec();
        let short = BudgetEnvelope::for_run(&profile, ModelTier::SmallLocal, Horizon::Short);
        let long = BudgetEnvelope::for_run(&profile, ModelTier::SmallLocal, Horizon::Long);
        let short_ratio = short.max_planner_calls * 1_000 / u64::from(Horizon::Short.steps());
        let long_ratio = long.max_planner_calls * 1_000 / u64::from(Horizon::Long.steps());
        assert!(
            long_ratio < short_ratio,
            "long horizon kept the short horizon's slack: {long_ratio} vs {short_ratio}"
        );
        assert!(long.max_planner_calls > short.max_planner_calls);
    }

    #[test]
    fn a_cheap_tier_gets_a_smaller_cost_envelope_than_a_strong_one() {
        let profile = ProfileId::Balanced.spec();
        let small = BudgetEnvelope::for_run(&profile, ModelTier::SmallLocal, Horizon::Medium);
        let strong = BudgetEnvelope::for_run(&profile, ModelTier::StrongHosted, Horizon::Medium);
        assert!(small.max_planner_cost_units < strong.max_planner_cost_units);
        // Call counts are a property of the profile and horizon, not of the
        // tier: a cheap model does not get to make more calls, it gets to make
        // cheaper ones.
        assert_eq!(small.max_planner_calls, strong.max_planner_calls);
    }

    #[test]
    fn a_stingy_region_policy_yields_no_region_budget_at_all() {
        let economy = BudgetEnvelope::for_run(
            &ProfileId::Economy.spec(),
            ModelTier::SmallLocal,
            Horizon::Medium,
        );
        assert_eq!(economy.max_region_captures, 0);
        let mut ledger = BudgetLedger::new(economy);
        assert_eq!(
            ledger.debit(BudgetLine::RegionCaptures, 1).unwrap_err(),
            DenyReason::BudgetExhausted
        );
    }

    #[test]
    fn scaling_tightens_every_line_without_zeroing_a_needed_one() {
        let full = envelope(ProfileId::Balanced, ModelTier::SmallLocal, Horizon::Medium);
        let squeezed = full.scaled(2_500);
        for line in BudgetLine::ALL {
            assert!(
                squeezed.limit(*line) <= full.limit(*line),
                "{line:?} grew under a squeeze"
            );
            if full.limit(*line) > 0 {
                assert!(squeezed.limit(*line) >= 1, "{line:?} was squeezed to zero");
            } else {
                assert_eq!(squeezed.limit(*line), 0);
            }
        }
        assert!(squeezed.run_deadline_millis < full.run_deadline_millis);
        // The per-step deadline is a property of the tier, not of the
        // allowance: squeezing the budget must not also make every individual
        // step time out, or the squeeze would be testing two things.
        assert_eq!(squeezed.step_deadline_millis, full.step_deadline_millis);
    }

    #[test]
    fn a_full_scale_squeeze_is_the_identity() {
        let full = envelope(
            ProfileId::HighAssurance,
            ModelTier::MidVision,
            Horizon::Long,
        );
        assert_eq!(full.scaled(10_000), full);
    }

    #[test]
    fn deadlines_survive_an_escalation_to_the_top_of_the_ladder() {
        let profile = ProfileId::Balanced.spec();
        let cheap = BudgetEnvelope::for_run(&profile, ModelTier::SmallLocal, Horizon::Medium);
        let strongest = ModelTier::StrongHosted
            .declared()
            .nominal_step_latency_millis;
        assert!(
            cheap.step_deadline_millis >= strongest,
            "a run based on the cheap tier cannot afford a step at the tier it may climb to"
        );
        // And a run that starts at the top is sized the same way.
        let strong = BudgetEnvelope::for_run(&profile, ModelTier::StrongHosted, Horizon::Medium);
        assert_eq!(cheap.step_deadline_millis, strong.step_deadline_millis);
        // Cost, unlike time, still follows the base tier.
        assert!(cheap.max_planner_cost_units < strong.max_planner_cost_units);
    }

    #[test]
    fn deadlines_catch_one_huge_step_and_many_long_ones() {
        let mut ledger = BudgetLedger::new(envelope(
            ProfileId::Balanced,
            ModelTier::SmallLocal,
            Horizon::Short,
        ));
        let step = ledger.envelope().step_deadline_millis;
        assert_eq!(
            ledger.advance(step + 1).unwrap_err(),
            DenyReason::StepDeadlineExceeded
        );
        assert_eq!(ledger.elapsed_millis(), 0);
        let mut crossed = false;
        for _ in 0..1_000 {
            if ledger.advance(step).is_err() {
                crossed = true;
                break;
            }
        }
        assert!(crossed, "run deadline was never reached");
        assert!(ledger.elapsed_millis() <= ledger.envelope().run_deadline_millis);
    }

    #[test]
    fn snapshots_report_the_full_line_set_and_stay_within_the_envelope() {
        let mut ledger = BudgetLedger::new(envelope(
            ProfileId::Balanced,
            ModelTier::MidVision,
            Horizon::Medium,
        ));
        ledger.debit(BudgetLine::Observations, 3).unwrap();
        ledger.advance(10).unwrap();
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.spent.len(), BudgetLine::ALL.len());
        assert!(snapshot.is_within_envelope());
    }
}
