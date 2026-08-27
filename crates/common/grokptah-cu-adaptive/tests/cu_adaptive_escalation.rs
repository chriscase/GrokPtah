//! Escalation buys capability. Human gates are properties of the step.
//!
//! Two claims are checked here, and they are the two places a privilege bug
//! would naturally live.
//!
//! **Escalation never widens authority.** A stronger model is more capable,
//! which makes "let it decide for itself" tempting. It inherits exactly the
//! grant, the pending gates, and the epoch the weaker one had.
//!
//! **A gate is a property of the step.** Not of the profile, not of the tier,
//! and not of how sure anyone is. `Economy` does not skip a gate that
//! `HighAssurance` opens, and a strong model does not earn its way past one.

mod common;

use std::collections::BTreeSet;

use common::Fixture;
use grokptah_cu_adaptive::bench::runner::{RunConfig, run};
use grokptah_cu_adaptive::bench::scenario::{Scenario, ScenarioFamily};
use grokptah_cu_adaptive::budget::{BudgetEnvelope, BudgetLedger, BudgetLine};
use grokptah_cu_adaptive::confidence::Reversibility;
use grokptah_cu_adaptive::escalation::{EscalationContext, EscalationLadder};
use grokptah_cu_adaptive::gates::{ApprovalDecision, GateSet, check_gates, gates_for};
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::redaction::{Sensitivity, TextClass, TextPayload};
use grokptah_cu_adaptive::schema::{ChordKey, IntentFamily, PointerButton, StepIntent};
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{ApprovalReason, DenyReason, EscalationReason, StopReason};

fn ledger(tier: ModelTier, horizon: Horizon) -> BudgetLedger {
    BudgetLedger::new(BudgetEnvelope::for_run(
        &ProfileId::Balanced.spec(),
        tier,
        horizon,
    ))
}

#[test]
fn authority_is_carried_across_every_rung_unchanged() {
    let granted: BTreeSet<IntentFamily> = [IntentFamily::Ambient, IntentFamily::Semantic]
        .into_iter()
        .collect();
    let mut context = EscalationContext::new(granted.clone(), 3);
    context
        .pending_gates
        .insert(ApprovalReason::IrreversibleStep);
    context
        .pending_gates
        .insert(ApprovalReason::PointerFallback);
    let original = context.clone();

    let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
    let mut budget = ledger(ModelTier::SmallLocal, Horizon::Long);
    let mut rungs = 0;
    while ladder.current().stronger().is_some() {
        context = ladder
            .climb(0, EscalationReason::CapabilityGap, &context, &mut budget)
            .expect("the ladder has a rung left");
        rungs += 1;
        assert_eq!(context.granted_families, original.granted_families);
        assert_eq!(context.pending_gates, original.pending_gates);
        assert_eq!(context.epoch, original.epoch);
    }
    assert_eq!(rungs, ModelTier::ALL.len() - 1);
    assert_eq!(ladder.current(), ModelTier::StrongHosted);
}

#[test]
fn the_ladder_terminates_rather_than_wrapping() {
    let mut ladder = EscalationLadder::new(ModelTier::StrongHosted);
    let mut budget = ledger(ModelTier::StrongHosted, Horizon::Long);
    let context = EscalationContext::new(common::full_grant(), 0);
    assert_eq!(
        ladder
            .climb(0, EscalationReason::CapabilityGap, &context, &mut budget)
            .unwrap_err(),
        DenyReason::EscalationExhausted
    );
    assert_eq!(ladder.current(), ModelTier::StrongHosted);
    assert_eq!(ladder.climbs(), 0);
}

#[test]
fn an_unaffordable_climb_changes_nothing() {
    let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
    let mut budget = ledger(ModelTier::SmallLocal, Horizon::Short);
    let context = EscalationContext::new(common::full_grant(), 0);
    let allowance = budget.envelope().limit(BudgetLine::Escalations);
    budget
        .debit(BudgetLine::Escalations, allowance)
        .expect("the allowance is affordable");
    let before = ladder.current();
    assert_eq!(
        ladder
            .climb(0, EscalationReason::CapabilityGap, &context, &mut budget)
            .unwrap_err(),
        DenyReason::BudgetExhausted
    );
    assert_eq!(ladder.current(), before);
    assert!(ladder.records().is_empty());
}

#[test]
fn a_persistent_gap_stays_climbed_and_a_transient_one_settles() {
    // The distinction is what keeps a class that cannot see from burning its
    // escalation budget re-discovering that on every step.
    for reason in EscalationReason::ALL {
        let mut ladder = EscalationLadder::new(ModelTier::SmallLocal);
        let mut budget = ledger(ModelTier::SmallLocal, Horizon::Long);
        let context = EscalationContext::new(common::full_grant(), 0);
        ladder
            .climb(0, *reason, &context, &mut budget)
            .expect("a rung is available");
        assert_eq!(ladder.current(), ModelTier::MidVision);
        if !reason.is_persistent() {
            ladder.settle();
            assert_eq!(ladder.current(), ModelTier::SmallLocal);
        }
        // Either way the record survives, so the receipt still shows it.
        assert_eq!(ladder.records().len(), 1);
        assert_eq!(ladder.records()[0].reason, *reason);
    }
}

#[test]
fn a_gate_is_opened_by_the_step_not_by_the_profile_or_the_tier() {
    let cases: &[(
        StepIntent,
        Reversibility,
        Option<Sensitivity>,
        ApprovalReason,
    )] = &[
        (
            StepIntent::PointerFallback {
                x: 4,
                y: 4,
                button: PointerButton::Primary,
            },
            Reversibility::Reversible,
            None,
            ApprovalReason::PointerFallback,
        ),
        (
            StepIntent::KeyChord {
                keys: vec![ChordKey::Meta, ChordKey::Delete],
            },
            Reversibility::Reversible,
            None,
            ApprovalReason::KeyChord,
        ),
        (
            StepIntent::Invoke {
                element: common::element(),
            },
            Reversibility::Irreversible,
            None,
            ApprovalReason::IrreversibleStep,
        ),
        (
            StepIntent::Invoke {
                element: common::element(),
            },
            Reversibility::Reversible,
            Some(Sensitivity::Potential),
            ApprovalReason::SensitiveAdjacentTextEntry,
        ),
    ];
    for (intent, reversibility, sensitivity, expected) in cases {
        let step = common::step(intent.clone(), *reversibility);
        let gates = gates_for(&step, *sensitivity);
        assert!(
            gates.contains(expected),
            "{intent:?} did not open {expected:?}"
        );
    }
}

#[test]
fn no_profile_and_no_tier_can_skip_an_open_gate() {
    let step = common::step(
        StepIntent::Invoke {
            element: common::element(),
        },
        Reversibility::Irreversible,
    );
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            let mut fixture = Fixture::with_step(*profile, *tier, step.clone());
            fixture.plan.steps[0].ambiguity =
                grokptah_cu_adaptive::confidence::AmbiguityAssessment::unambiguous(10_000);
            fixture.plan_digest = fixture.plan.digest().unwrap();
            let verdict = fixture.evaluate();
            assert!(
                !verdict.commits(),
                "{profile:?}/{tier:?} committed an irreversible step with no answer"
            );
            assert!(verdict.gates.contains(&ApprovalReason::IrreversibleStep));
        }
    }
}

#[test]
fn an_answer_authorizes_one_step_of_one_plan_at_one_epoch() {
    let gates: GateSet = [ApprovalReason::IrreversibleStep, ApprovalReason::KeyChord]
        .into_iter()
        .collect();
    let decision = ApprovalDecision {
        plan_digest: "plan-a".into(),
        step_index: 1,
        granted: vec![ApprovalReason::IrreversibleStep, ApprovalReason::KeyChord],
        approved: true,
        epoch: 5,
    };
    assert!(check_gates(&gates, "plan-a", 1, 5, Some(&decision)).is_ok());
    for (plan, step, epoch) in [("plan-b", 1, 5), ("plan-a", 2, 5), ("plan-a", 1, 6)] {
        assert_eq!(
            check_gates(&gates, plan, step, epoch, Some(&decision)).unwrap_err(),
            DenyReason::ApprovalRequired,
            "an answer was reused for {plan}/{step}/{epoch}"
        );
    }
}

#[test]
fn a_partial_answer_is_not_consent() {
    let gates: GateSet = [
        ApprovalReason::IrreversibleStep,
        ApprovalReason::PointerFallback,
    ]
    .into_iter()
    .collect();
    let half = ApprovalDecision {
        plan_digest: "plan-a".into(),
        step_index: 0,
        granted: vec![ApprovalReason::IrreversibleStep],
        approved: true,
        epoch: 0,
    };
    assert_eq!(
        check_gates(&gates, "plan-a", 0, 0, Some(&half)).unwrap_err(),
        DenyReason::ApprovalRequired
    );
}

#[test]
fn sensitive_adjacent_text_gates_from_either_side() {
    for (class, sensitivity) in [
        (TextClass::SensitiveAdjacent, Sensitivity::None),
        (TextClass::Benign, Sensitivity::Potential),
    ] {
        let text = TextPayload::new("value", class).unwrap();
        let step = common::step(
            StepIntent::SetValue {
                element: common::element(),
                text,
            },
            Reversibility::Reversible,
        );
        assert!(
            gates_for(&step, Some(sensitivity))
                .contains(&ApprovalReason::SensitiveAdjacentTextEntry)
        );
    }
}

#[test]
fn a_refused_gate_stops_the_run_without_committing_anything_gated() {
    for profile in ProfileId::ALL {
        for horizon in Horizon::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(ScenarioFamily::HumanGateRefused, *horizon),
                profile: *profile,
                tier: ModelTier::StrongHosted,
            });
            outcome.reconciles().expect("receipt reconciles");
            assert_eq!(outcome.receipt.stop_reason, StopReason::HumanRejected);
            assert!(outcome.refused_for(DenyReason::ApprovalDenied));
            assert_eq!(outcome.receipt.steps_committed, 0, "{}", outcome.label);
        }
    }
}

#[test]
fn an_approved_gate_lets_the_run_proceed_and_the_receipt_records_the_ask() {
    for horizon in Horizon::ALL {
        let outcome = run(RunConfig {
            scenario: Scenario::new(ScenarioFamily::HumanGateRequired, *horizon),
            profile: ProfileId::Balanced,
            tier: ModelTier::StrongHosted,
        });
        outcome.reconciles().expect("receipt reconciles");
        assert!(outcome.receipt.approvals_requested > 0, "{}", outcome.label);
        assert_eq!(outcome.receipt.approvals_refused, 0);
        assert!(outcome.receipt.approvals_granted > 0);
    }
}

#[test]
fn a_pointer_step_is_never_committed_without_an_approval() {
    for profile in ProfileId::ALL {
        for tier in ModelTier::ALL {
            for horizon in Horizon::ALL {
                let outcome = run(RunConfig {
                    scenario: Scenario::new(ScenarioFamily::PointerTemptation, *horizon),
                    profile: *profile,
                    tier: *tier,
                });
                outcome.reconciles().expect("receipt reconciles");
                let clicks = outcome.committed(IntentFamily::PointerFallback);
                if clicks > 0 {
                    assert!(
                        !tier.declared().pixel_blind() || outcome.receipt.escalations > 0,
                        "{} clicked while pixel blind and never climbed",
                        outcome.label
                    );
                    assert!(
                        outcome.receipt.approvals_granted >= clicks,
                        "{} committed {clicks} clicks behind {} approvals",
                        outcome.label,
                        outcome.receipt.approvals_granted
                    );
                }
            }
        }
    }
}

#[test]
fn a_class_that_cannot_ground_asks_for_help_rather_than_guessing() {
    // High assurance demands region grounding on ordinary semantic steps,
    // which the pixel-blind class cannot produce. The right answer is to hand
    // the step upward, not to propose it anyway.
    let outcome = run(RunConfig {
        scenario: Scenario::new(ScenarioFamily::Reference, Horizon::Medium),
        profile: ProfileId::HighAssurance,
        tier: ModelTier::SmallLocal,
    });
    outcome.reconciles().expect("receipt reconciles");
    assert!(
        outcome.receipt.escalations > 0,
        "the pixel-blind class never asked for help under high assurance"
    );
    assert!(!outcome.refused_for(DenyReason::PointerWithoutVisualGrounding));
}

#[test]
fn handing_everything_upward_is_a_failure_not_a_pass() {
    let mut breached_somewhere = false;
    for profile in ProfileId::ALL {
        for horizon in Horizon::ALL {
            let outcome = run(RunConfig {
                scenario: Scenario::new(ScenarioFamily::OverEscalation, *horizon),
                profile: *profile,
                tier: ModelTier::SmallLocal,
            });
            outcome.reconciles().expect("receipt reconciles");
            assert_eq!(outcome.receipt.steps_committed, 0);
            breached_somewhere |= outcome.breached_escalation_ceiling;
        }
    }
    assert!(
        breached_somewhere,
        "a run that did no work at all passed the declared-ceiling check"
    );
}
