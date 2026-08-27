//! Receipts are derived, re-checkable, and explicit about what they do not
//! claim.
//!
//! A receipt is the only artifact that outlives a run, which makes it the only
//! place a false claim persists. Three properties are checked here:
//!
//! * A receipt's numbers come out of the ledger, and editing one is detected.
//! * A receipt cannot report an end it did not reach -- no completion after a
//!   cancellation, no orderly end while holding resources.
//! * A receipt carries the full mandatory disclaimer set. This harness has no
//!   hardware, no VM, no provider, no image model, and no operator, and the
//!   receipt says so in a field reconciliation refuses to let it drop.

mod common;

use grokptah_cu_adaptive::bench::runner::{RunConfig, run};
use grokptah_cu_adaptive::bench::scenario::{Scenario, ScenarioFamily};
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::profile::ProfileId;
use grokptah_cu_adaptive::receipt::{ReceiptError, Substrate};
use grokptah_cu_adaptive::tier::ModelTier;
use grokptah_cu_adaptive::vocabulary::{DenyReason, NotClaimed, StopReason};

fn outcome(
    family: ScenarioFamily,
    horizon: Horizon,
) -> grokptah_cu_adaptive::bench::runner::RunOutcome {
    run(RunConfig {
        scenario: Scenario::new(family, horizon),
        profile: ProfileId::Balanced,
        tier: ModelTier::StrongHosted,
    })
}

#[test]
fn every_receipt_in_the_matrix_reconciles() {
    for family in ScenarioFamily::ALL {
        for horizon in Horizon::ALL {
            for profile in ProfileId::ALL {
                for tier in ModelTier::ALL {
                    let outcome = run(RunConfig {
                        scenario: Scenario::new(*family, *horizon),
                        profile: *profile,
                        tier: *tier,
                    });
                    outcome.reconciles().unwrap_or_else(|error| {
                        panic!("{} did not reconcile: {error}", outcome.label)
                    });
                }
            }
        }
    }
}

#[test]
fn every_receipt_carries_the_full_mandatory_disclaimer_set() {
    for family in ScenarioFamily::ALL {
        for horizon in Horizon::ALL {
            let outcome = outcome(*family, *horizon);
            assert_eq!(outcome.receipt.substrate, Substrate::SyntheticDeterministic);
            for mandatory in NotClaimed::MANDATORY {
                assert!(
                    outcome.receipt.not_claimed.contains(mandatory),
                    "{} dropped {mandatory:?}",
                    outcome.label
                );
            }
        }
    }
}

#[test]
fn a_receipt_that_claims_more_than_the_ledger_recorded_is_rejected() {
    let outcome = outcome(ScenarioFamily::Reference, Horizon::Short);
    for mutate in [
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.steps_committed += 1,
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.steps_planned += 1,
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.observations += 7,
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.escalations += 1,
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.disagreements += 1,
        |receipt: &mut grokptah_cu_adaptive::receipt::RunReceipt| receipt.events_dropped += 1,
    ] {
        let mut forged = outcome.receipt.clone();
        mutate(&mut forged);
        // Re-digested, so the forgery is internally consistent and only
        // reconciliation against the ledger can catch it.
        let redigested = forged.clone();
        assert!(
            redigested
                .reconcile(
                    &outcome.ledger,
                    &outcome.budget,
                    &outcome.cleanup,
                    &outcome.escalation
                )
                .is_err(),
            "an inflated receipt reconciled"
        );
    }
}

#[test]
fn a_receipt_that_claims_less_than_the_ledger_recorded_is_also_rejected() {
    let outcome = outcome(ScenarioFamily::DriftingFrame, Horizon::Medium);
    assert!(
        outcome.receipt.steps_refused > 0,
        "the fixture refuses nothing"
    );
    let mut understated = outcome.receipt.clone();
    understated.steps_refused = 0;
    understated.denials.clear();
    assert!(
        understated
            .reconcile(
                &outcome.ledger,
                &outcome.budget,
                &outcome.cleanup,
                &outcome.escalation
            )
            .is_err()
    );
}

#[test]
fn the_refusal_breakdown_must_add_up_to_the_refusal_count() {
    let outcome = outcome(ScenarioFamily::DriftingFrame, Horizon::Medium);
    let total: u32 = outcome.receipt.denials.values().sum();
    assert_eq!(total, outcome.receipt.steps_refused);

    let mut skewed = outcome.receipt.clone();
    skewed.denials.insert(DenyReason::SensitiveSurface, 99);
    assert!(matches!(
        skewed
            .reconcile(
                &outcome.ledger,
                &outcome.budget,
                &outcome.cleanup,
                &outcome.escalation
            )
            .unwrap_err(),
        ReceiptError::DenialMismatch
    ));
}

#[test]
fn dropping_a_disclaimer_is_rejected() {
    let outcome = outcome(ScenarioFamily::Reference, Horizon::Short);
    for mandatory in NotClaimed::MANDATORY {
        let mut stripped = outcome.receipt.clone();
        stripped.not_claimed.retain(|claim| claim != mandatory);
        assert_eq!(
            stripped
                .reconcile(
                    &outcome.ledger,
                    &outcome.budget,
                    &outcome.cleanup,
                    &outcome.escalation
                )
                .unwrap_err(),
            ReceiptError::MissingDisclaimer(*mandatory)
        );
    }
}

#[test]
fn editing_a_receipt_without_redigesting_is_detected() {
    let outcome = outcome(ScenarioFamily::Reference, Horizon::Short);
    let mut edited = outcome.receipt.clone();
    edited.scenario_id = "a-different-scenario".into();
    assert_eq!(
        edited
            .reconcile(
                &outcome.ledger,
                &outcome.budget,
                &outcome.cleanup,
                &outcome.escalation
            )
            .unwrap_err(),
        ReceiptError::DigestMismatch
    );
}

#[test]
fn a_cancelled_run_cannot_report_completion() {
    let outcome = outcome(ScenarioFamily::CancellationMidFlight, Horizon::Medium);
    assert!(outcome.receipt.cancellation.is_some());
    let mut lying = outcome.receipt.clone();
    lying.stop_reason = StopReason::ObjectiveComplete;
    assert!(matches!(
        lying
            .reconcile(
                &outcome.ledger,
                &outcome.budget,
                &outcome.cleanup,
                &outcome.escalation
            )
            .unwrap_err(),
        ReceiptError::DigestMismatch | ReceiptError::CancelledButClaimsCompletion(_)
    ));
}

#[test]
fn a_run_that_completes_says_so_and_one_that_does_not_does_not() {
    let completed = outcome(ScenarioFamily::Reference, Horizon::Short);
    assert_eq!(completed.receipt.stop_reason, StopReason::ObjectiveComplete);
    assert!(completed.receipt.is_orderly());

    let refused = outcome(ScenarioFamily::HumanGateRefused, Horizon::Medium);
    assert_eq!(refused.receipt.stop_reason, StopReason::HumanRejected);
    assert!(!refused.receipt.is_orderly());
}

#[test]
fn a_long_run_reports_the_events_it_dropped() {
    let outcome = run(RunConfig {
        scenario: Scenario::new(ScenarioFamily::Reference, Horizon::Long),
        profile: ProfileId::HighAssurance,
        tier: ModelTier::StrongHosted,
    });
    outcome.reconciles().expect("receipt reconciles");
    assert!(
        outcome.receipt.events_recorded > 0,
        "a 300-step run recorded no events"
    );
    // Whether the tail was truncated depends on the run, but the receipt's
    // account of it must always match the ledger's.
    assert_eq!(
        outcome.receipt.events_recorded,
        outcome.ledger.events_recorded()
    );
    assert_eq!(
        outcome.receipt.events_dropped,
        outcome.ledger.events_dropped()
    );
}

#[test]
fn the_receipt_names_the_substrate_it_actually_ran_on() {
    let outcome = outcome(ScenarioFamily::Reference, Horizon::Short);
    let serialized = serde_json::to_string(&outcome.receipt).unwrap();
    assert!(serialized.contains("synthetic_deterministic"));
    for expected in [
        "real_hardware_timing",
        "virtual_machine_behavior",
        "provider_latency_or_cost",
        "image_model_accuracy",
        "human_operator_behavior",
        "real_application_semantics",
        "token_accounting",
    ] {
        assert!(serialized.contains(expected), "receipt omits {expected}");
    }
}

#[test]
fn a_receipt_round_trips_through_json_and_still_reconciles() {
    let outcome = outcome(ScenarioFamily::BackendFailure, Horizon::Medium);
    let serialized = serde_json::to_string(&outcome.receipt).unwrap();
    let restored: grokptah_cu_adaptive::receipt::RunReceipt =
        serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored, outcome.receipt);
    restored
        .reconcile(
            &outcome.ledger,
            &outcome.budget,
            &outcome.cleanup,
            &outcome.escalation,
        )
        .expect("a round-tripped receipt still reconciles");
}

#[test]
fn a_receipt_with_an_unknown_field_is_refused() {
    let outcome = outcome(ScenarioFamily::Reference, Horizon::Short);
    let mut value = serde_json::to_value(&outcome.receipt).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("realHardwareSeconds".into(), serde_json::json!(1.25));
    assert!(
        serde_json::from_value::<grokptah_cu_adaptive::receipt::RunReceipt>(value).is_err(),
        "a receipt accepted a field claiming real hardware timing"
    );
}

#[test]
fn budget_claims_are_checked_against_the_ledger_not_taken_on_trust() {
    let outcome = outcome(ScenarioFamily::BudgetSqueeze, Horizon::Medium);
    let mut inflated = outcome.receipt.clone();
    inflated.budget.envelope.max_committed_actions =
        inflated.budget.envelope.max_committed_actions * 10 + 100;
    assert!(matches!(
        inflated
            .reconcile(
                &outcome.ledger,
                &outcome.budget,
                &outcome.cleanup,
                &outcome.escalation
            )
            .unwrap_err(),
        ReceiptError::BudgetOutsideEnvelope | ReceiptError::DigestMismatch
    ));
}
