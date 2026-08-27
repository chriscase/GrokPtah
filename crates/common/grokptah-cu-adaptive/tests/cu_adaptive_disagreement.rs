//! Planner and executor disagreement always resolves conservatively.
//!
//! The planner decided against the frame it saw; the executor decides against
//! the frame that is there. When they differ, the stricter answer wins -- in
//! both directions. A confident planner cannot talk the executor into acting,
//! and a confident executor cannot talk a cautious planner out of stopping.
//!
//! The second direction is the one that is easy to get wrong. It is tempting
//! to treat the executor as the authority because it has fresher information,
//! but "fresher" is not "more careful": a planner that has decided to stop has
//! a reason the executor cannot see, and overriding it would make the planner's
//! caution decorative.

mod common;

use common::Fixture;
use grokptah_cu_adaptive::bench::runner::{RunConfig, run};
use grokptah_cu_adaptive::bench::scenario::{Scenario, ScenarioFamily};
use grokptah_cu_adaptive::confidence::{AmbiguityAssessment, Disposition, Reversibility};
use grokptah_cu_adaptive::executor::DisagreementKind;
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::redaction::Sensitivity;
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{ApprovalReason, DenyReason, EscalationReason};

const LADDER: &[Disposition] = &[
    Disposition::Commit,
    Disposition::Disambiguate,
    Disposition::RequestApproval {
        reason: ApprovalReason::PointerFallback,
    },
    Disposition::Escalate {
        reason: EscalationReason::CapabilityGap,
    },
    Disposition::Refuse {
        reason: DenyReason::StaleFrame,
    },
];

#[test]
fn resolution_is_a_total_order_that_never_relaxes() {
    for left in LADDER {
        for right in LADDER {
            let resolved = left.resolve(*right);
            assert!(resolved.strictness() >= left.strictness());
            assert!(resolved.strictness() >= right.strictness());
            assert_eq!(
                resolved,
                right.resolve(*left),
                "resolution depends on order"
            );
        }
    }
}

#[test]
fn resolution_is_associative() {
    // Three sources of opinion resolve to the same answer whatever order they
    // are folded in, which is what lets the runner resolve incrementally.
    for a in LADDER {
        for b in LADDER {
            for c in LADDER {
                assert_eq!(a.resolve(*b).resolve(*c), a.resolve(b.resolve(*c)));
            }
        }
    }
}

#[test]
fn a_confident_planner_cannot_commit_a_step_the_live_frame_refuses() {
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            let mut fixture = Fixture::new(*profile, *tier);
            fixture.planner = Disposition::Commit;
            if let Some(live) = fixture.live_element.as_mut() {
                live.enabled = false;
            }
            let verdict = fixture.evaluate();
            assert!(!verdict.commits());
            assert_eq!(verdict.refusal(), Some(DenyReason::ElementDisabled));
            assert_eq!(
                verdict.disagreement.unwrap().kind,
                DisagreementKind::ExecutorRefusedCommit
            );
        }
    }
}

#[test]
fn a_cautious_planner_is_never_overridden_by_a_permissive_executor() {
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            for planner in [
                Disposition::Refuse {
                    reason: DenyReason::BackendUnavailable,
                },
                Disposition::Escalate {
                    reason: EscalationReason::RepeatedPostconditionMiss,
                },
                Disposition::RequestApproval {
                    reason: ApprovalReason::IrreversibleStep,
                },
                Disposition::Disambiguate,
            ] {
                let mut fixture = Fixture::new(*profile, *tier);
                fixture.planner = planner;
                let verdict = fixture.evaluate();
                assert!(
                    !verdict.commits(),
                    "{profile:?}/{tier:?} committed despite a planner that said {planner:?}"
                );
                assert!(verdict.resolved.strictness() >= planner.strictness());
            }
        }
    }
}

#[test]
fn a_conclusion_the_planners_own_evidence_denies_is_caught() {
    // The failure small models actually exhibit: the numbers are right and the
    // claim on top of them is not. The executor re-derives from the same
    // evidence and disagrees.
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.plan.steps[0].ambiguity = AmbiguityAssessment {
        candidate_count: 4,
        top_confidence_bps: 5_500,
        runner_up_confidence_bps: 5_400,
    };
    fixture.plan_digest = fixture.plan.digest().unwrap();
    fixture.planner = Disposition::Commit;
    let verdict = fixture.evaluate();
    assert!(!verdict.commits());
    assert!(matches!(
        verdict.disagreement.unwrap().kind,
        DisagreementKind::ExecutorGatedCommit
            | DisagreementKind::ExecutorDisambiguatedCommit
            | DisagreementKind::ExecutorEscalatedCommit
            | DisagreementKind::ExecutorRefusedCommit
    ));
}

#[test]
fn agreement_records_no_disagreement() {
    for profile in ProfileId::ALL {
        let fixture = Fixture::new(*profile, ModelTier::StrongHosted);
        let verdict = fixture.evaluate();
        assert_eq!(verdict.planner, verdict.executor);
        assert!(verdict.disagreement.is_none());
    }
}

#[test]
fn a_gate_the_planner_missed_is_reported_as_the_executor_gating() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    if let Some(live) = fixture.live_element.as_mut() {
        live.sensitivity = Sensitivity::Potential;
    }
    fixture.planner = Disposition::Commit;
    let verdict = fixture.evaluate();
    assert_eq!(
        verdict.disagreement.unwrap().kind,
        DisagreementKind::ExecutorGatedCommit
    );
    assert_eq!(
        verdict.gates,
        vec![ApprovalReason::SensitiveAdjacentTextEntry]
    );
}

#[test]
fn a_planner_that_is_stricter_is_labelled_as_such() {
    let mut fixture = Fixture::new(ProfileId::Balanced, ModelTier::StrongHosted);
    fixture.planner = Disposition::Disambiguate;
    let verdict = fixture.evaluate();
    assert_eq!(verdict.executor, Disposition::Commit);
    assert_eq!(verdict.resolved, Disposition::Disambiguate);
    assert_eq!(
        verdict.disagreement.unwrap().kind,
        DisagreementKind::PlannerMoreConservative
    );
}

#[test]
fn same_rung_conflicts_resolve_the_same_way_from_either_side() {
    let a = Disposition::Refuse {
        reason: DenyReason::BudgetExhausted,
    };
    let b = Disposition::Refuse {
        reason: DenyReason::SensitiveSurface,
    };
    assert_eq!(a.resolve(b), b.resolve(a));
    let c = Disposition::Escalate {
        reason: EscalationReason::PlanDepthExceeded,
    };
    let d = Disposition::Escalate {
        reason: EscalationReason::AmbiguityUnresolved,
    };
    assert_eq!(c.resolve(d), d.resolve(c));
}

#[test]
fn the_disagreement_scenario_produces_disagreements_at_every_profile() {
    for profile in ProfileId::ALL {
        for horizon in Horizon::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(ScenarioFamily::PlannerExecutorDisagreement, *horizon),
                profile: *profile,
                tier: ModelTier::StrongHosted,
            });
            outcome.reconciles().expect("receipt reconciles");
            assert!(
                outcome.receipt.disagreements > 0,
                "{} recorded no disagreement",
                outcome.label
            );
        }
    }
}

#[test]
fn a_disagreeing_run_never_commits_what_the_executor_refused() {
    for profile in ProfileId::ALL {
        let outcome = run(RunConfig {
            scenario: Scenario::new(ScenarioFamily::PlannerExecutorDisagreement, Horizon::Medium),
            profile: *profile,
            tier: ModelTier::StrongHosted,
        });
        for verdict in &outcome.verdicts {
            if let Some(disagreement) = verdict.disagreement {
                assert!(
                    verdict.resolved.strictness()
                        >= disagreement
                            .executor
                            .strictness()
                            .max(disagreement.planner.strictness()),
                    "{} resolved below both sides at step {}",
                    outcome.label,
                    verdict.step_index
                );
            }
            if verdict.commits() {
                assert_eq!(verdict.executor, Disposition::Commit);
                assert_eq!(verdict.planner, Disposition::Commit);
            }
        }
    }
}

#[test]
fn reversibility_only_ever_raises_the_bar() {
    // A step that is harder to undo is never easier to commit.
    let mut confidence = 0;
    while confidence <= 10_000 {
        for profile in ProfileId::ALL {
            let mut previous = 0;
            for reversibility in Reversibility::ALL {
                let mut fixture = Fixture::new(*profile, ModelTier::StrongHosted);
                fixture.plan.steps[0].ambiguity = AmbiguityAssessment::unambiguous(confidence);
                fixture.plan.steps[0].reversibility = *reversibility;
                fixture.plan_digest = fixture.plan.digest().unwrap();
                let strictness = fixture.evaluate().executor.strictness();
                assert!(
                    strictness >= previous,
                    "{profile:?} at {confidence} bps got more permissive at {reversibility:?}"
                );
                previous = strictness;
            }
        }
        confidence += 250;
    }
}
