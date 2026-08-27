//! Model-budget envelopes, latency bounds, and resource limits.
//!
//! The property that matters is *fail closed*: a run discovers it cannot
//! afford something before it spends, not after. A ledger that let a debit
//! through and then reported an overspend would be an accounting system, not a
//! limit.
//!
//! Everything below is in synthetic units. Nothing here measures a provider, a
//! token count, or a millisecond on any real machine.

mod common;

use grokptah_cu_adaptive::bench::runner::{RunConfig, run};
use grokptah_cu_adaptive::bench::scenario::{Scenario, ScenarioFamily};
use grokptah_cu_adaptive::budget::{BudgetEnvelope, BudgetLedger, BudgetLine};
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{DenyReason, StopReason};

fn envelope(profile: ProfileId, tier: ModelTier, horizon: Horizon) -> BudgetEnvelope {
    BudgetEnvelope::for_run(&profile.spec(), tier, horizon)
}

#[test]
fn every_line_item_fails_closed_at_its_edge() {
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            for horizon in Horizon::ALL {
                let envelope = envelope(*profile, *tier, *horizon);
                for line in BudgetLine::ALL {
                    let limit = envelope.limit(*line);
                    let mut ledger = BudgetLedger::new(envelope);
                    if limit > 0 {
                        ledger.debit(*line, limit).expect("the limit is affordable");
                        assert_eq!(ledger.remaining(*line), 0);
                    }
                    assert_eq!(
                        ledger.debit(*line, 1).unwrap_err(),
                        DenyReason::BudgetExhausted,
                        "{profile:?}/{tier:?}/{horizon:?} {line:?} spent past its limit"
                    );
                    assert_eq!(ledger.spent(*line), limit);
                }
            }
        }
    }
}

#[test]
fn a_single_oversized_debit_is_refused_rather_than_truncated() {
    let envelope = envelope(ProfileId::Balanced, ModelTier::SmallLocal, Horizon::Short);
    let mut ledger = BudgetLedger::new(envelope);
    let limit = envelope.limit(BudgetLine::Observations);
    assert_eq!(
        ledger
            .debit(BudgetLine::Observations, limit + 1)
            .unwrap_err(),
        DenyReason::BudgetExhausted
    );
    assert_eq!(ledger.spent(BudgetLine::Observations), 0);
    // A saturating debit cannot wrap into an affordable one either.
    assert_eq!(
        ledger
            .debit(BudgetLine::Observations, u64::MAX)
            .unwrap_err(),
        DenyReason::BudgetExhausted
    );
    assert_eq!(ledger.spent(BudgetLine::Observations), 0);
}

#[test]
fn the_allowance_per_step_tightens_as_the_horizon_grows() {
    // A long run has more chances to amortize its setup and more chances to
    // drift, so it is held to a tighter per-step standard. A linear envelope
    // would hand a 300-step run a hundred times the slack a 3-step run needs.
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            let mut previous_ratio = u64::MAX;
            for horizon in Horizon::ALL {
                let envelope = envelope(*profile, *tier, *horizon);
                let ratio = envelope.max_planner_calls * 1_000 / u64::from(horizon.steps());
                assert!(
                    ratio < previous_ratio,
                    "{profile:?}/{tier:?} kept its slack from {horizon:?} down"
                );
                previous_ratio = ratio;
            }
        }
    }
}

#[test]
fn absolute_allowances_still_grow_with_the_horizon() {
    // The complement of the test above: tighter per step, but not smaller
    // overall, or a long run could not finish at all.
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            let short = envelope(*profile, *tier, Horizon::Short);
            let long = envelope(*profile, *tier, Horizon::Long);
            for line in BudgetLine::ALL {
                assert!(
                    long.limit(*line) >= short.limit(*line),
                    "{profile:?}/{tier:?} {line:?} shrank on the long horizon"
                );
            }
            assert!(long.run_deadline_millis > short.run_deadline_millis);
        }
    }
}

#[test]
fn a_cheap_tier_costs_less_without_being_allowed_to_do_more() {
    for profile in ProfileId::ALL {
        for horizon in Horizon::ALL {
            let small = envelope(*profile, ModelTier::SmallLocal, *horizon);
            let strong = envelope(*profile, ModelTier::StrongHosted, *horizon);
            // Cost envelopes differ.
            assert!(small.max_planner_cost_units < strong.max_planner_cost_units);
            assert!(small.max_executor_cost_units < strong.max_executor_cost_units);
            // Everything that is a *permission* to act does not.
            assert_eq!(small.max_committed_actions, strong.max_committed_actions);
            assert_eq!(small.max_planner_calls, strong.max_planner_calls);
            assert_eq!(small.max_observations, strong.max_observations);
            assert_eq!(small.max_region_captures, strong.max_region_captures);
        }
    }
}

#[test]
fn a_profile_that_verifies_more_is_given_more_observations_not_more_actions() {
    for tier in ModelTier::ALL {
        for horizon in Horizon::ALL {
            let economy = envelope(ProfileId::Economy, *tier, *horizon);
            let assured = envelope(ProfileId::HighAssurance, *tier, *horizon);
            assert!(assured.max_observations > economy.max_observations);
            assert!(assured.max_region_captures > economy.max_region_captures);
            assert_eq!(assured.max_committed_actions, economy.max_committed_actions);
        }
    }
}

#[test]
fn deadlines_bound_one_long_step_and_many_ordinary_ones() {
    let envelope = envelope(ProfileId::Balanced, ModelTier::SmallLocal, Horizon::Short);
    let mut ledger = BudgetLedger::new(envelope);
    assert_eq!(
        ledger
            .advance(envelope.step_deadline_millis + 1)
            .unwrap_err(),
        DenyReason::StepDeadlineExceeded
    );
    assert_eq!(
        ledger.elapsed_millis(),
        0,
        "a refused step still spent time"
    );

    let mut ledger = BudgetLedger::new(envelope);
    let mut crossed = None;
    for _ in 0..10_000 {
        if let Err(reason) = ledger.advance(envelope.step_deadline_millis) {
            crossed = Some(reason);
            break;
        }
    }
    assert_eq!(crossed, Some(DenyReason::RunDeadlineExceeded));
    assert!(ledger.elapsed_millis() <= envelope.run_deadline_millis);
}

#[test]
fn a_squeezed_run_stops_instead_of_overspending() {
    for horizon in Horizon::ALL {
        for profile in ProfileId::ALL {
            for tier in ModelTier::ALL {
                let outcome = run(RunConfig {
                    scenario: Scenario::new(ScenarioFamily::BudgetSqueeze, *horizon),
                    profile: *profile,
                    tier: *tier,
                });
                outcome.reconciles().expect("receipt reconciles");
                assert!(
                    outcome.receipt.budget.is_within_envelope(),
                    "{} overspent",
                    outcome.label
                );
                for line in &outcome.receipt.budget.spent {
                    assert!(
                        line.spent <= line.limit,
                        "{} spent {} of {} on {:?}",
                        outcome.label,
                        line.spent,
                        line.limit,
                        line.line
                    );
                }
            }
        }
    }
}

#[test]
fn a_squeeze_is_what_actually_stops_the_squeezed_runs() {
    // Not merely "did not overspend": the squeeze has to be the binding
    // constraint, or the scenario is not testing what it says.
    let mut stopped_on_budget = 0;
    for horizon in Horizon::ALL {
        for tier in ModelTier::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(ScenarioFamily::BudgetSqueeze, *horizon),
                profile: ProfileId::HighAssurance,
                tier: *tier,
            });
            if outcome.receipt.stop_reason == StopReason::BudgetExhausted {
                stopped_on_budget += 1;
            }
        }
    }
    assert!(
        stopped_on_budget > 0,
        "no squeezed run was actually stopped by its budget"
    );
}

#[test]
fn a_latency_spike_is_refused_before_the_step_is_dispatched() {
    for horizon in Horizon::ALL {
        let outcome = run(RunConfig {
            scenario: Scenario::new(ScenarioFamily::LatencySpike, *horizon),
            profile: ProfileId::Balanced,
            tier: ModelTier::StrongHosted,
        });
        outcome.reconciles().expect("receipt reconciles");
        assert!(
            outcome.refused_for(DenyReason::StepDeadlineExceeded),
            "{} never noticed the spike",
            outcome.label
        );
        assert!(
            outcome.receipt.budget.elapsed_millis
                <= outcome.receipt.budget.envelope.run_deadline_millis
        );
    }
}

#[test]
fn observation_bytes_are_charged_and_bounded() {
    let outcome = run(RunConfig {
        scenario: Scenario::new(ScenarioFamily::Reference, Horizon::Long),
        profile: ProfileId::HighAssurance,
        tier: ModelTier::StrongHosted,
    });
    let bytes = outcome
        .receipt
        .budget
        .spent
        .iter()
        .find(|line| line.line == BudgetLine::ObservationBytes)
        .expect("observation bytes are tracked");
    assert!(
        bytes.spent > 0,
        "a 300-step run charged no observation bytes"
    );
    assert!(bytes.spent <= bytes.limit);
}

#[test]
fn scaling_never_produces_an_impossible_envelope() {
    for bps in [0, 1, 100, 2_500, 5_000, 9_999, 10_000] {
        for profile in ProfileId::ALL {
            let scaled = envelope(*profile, ModelTier::MidVision, Horizon::Medium).scaled(bps);
            let full = envelope(*profile, ModelTier::MidVision, Horizon::Medium);
            for line in BudgetLine::ALL {
                assert!(scaled.limit(*line) <= full.limit(*line));
                if full.limit(*line) > 0 {
                    assert!(
                        scaled.limit(*line) >= 1,
                        "{profile:?} {line:?} scaled to zero at {bps} bps"
                    );
                }
            }
            assert!(scaled.run_deadline_millis >= 1);
        }
    }
}
