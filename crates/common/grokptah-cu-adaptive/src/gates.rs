//! Mandatory human approval gates.
//!
//! A gate is a property of the *step*, never of the profile, the tier, or how
//! confident anyone is. `Economy` does not get to skip a gate that
//! `HighAssurance` would open, and a strong model does not earn its way past
//! one by being strong. That is why gates are computed by a free function over
//! the step and unioned into the verdict, rather than being another rung on
//! the disposition ladder in [`crate::confidence`] where a stricter
//! disposition could absorb them.
//!
//! Two consequences worth stating, because both are easy to get wrong:
//!
//! * **Escalation does not clear a gate.** Handing a step to a stronger model
//!   changes who proposes it, not what it does. The gate is recomputed at the
//!   new tier and comes out the same, and [`crate::escalation`] carries the
//!   pending set across the hand-off rather than resetting it.
//! * **An answered gate authorizes one step, once.** [`ApprovalDecision`]
//!   binds to a plan digest and a step index. A run cannot bank an approval
//!   and spend it on a different step, or on the same step after the frame
//!   moved.
//!
//! Nothing in this crate asks a real person anything. Gate answers in the
//! benchmark come from a scripted policy, and every receipt carries
//! [`crate::vocabulary::NotClaimed::HumanOperatorBehavior`] to say so.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::confidence::Reversibility;
use crate::redaction::{Sensitivity, TextClass};
use crate::schema::{IntentFamily, PlannedStep, StepIntent};
use crate::vocabulary::{ApprovalReason, DenyReason};

/// The gates one step opens.
///
/// A `BTreeSet` rather than a `Vec` so the set is order-independent and
/// deduplicated: a step that is both irreversible and a pointer fallback opens
/// two gates, and opening the same gate twice is not two gates.
pub type GateSet = BTreeSet<ApprovalReason>;

/// Compute the gates a step opens, given what the live frame says about the
/// element's sensitivity.
///
/// `element_sensitivity` is `None` when the step names no element. A
/// hard-denied sensitivity is not represented here at all: those steps are
/// refused by [`crate::grounding::verify`] before a gate is ever computed, and
/// a gate on a hard-denied surface would wrongly imply a human could allow it.
#[must_use]
pub fn gates_for(step: &PlannedStep, element_sensitivity: Option<Sensitivity>) -> GateSet {
    let mut gates = GateSet::new();

    if step.reversibility == Reversibility::Irreversible {
        gates.insert(ApprovalReason::IrreversibleStep);
    }

    match step.intent.family() {
        IntentFamily::PointerFallback => {
            gates.insert(ApprovalReason::PointerFallback);
        }
        IntentFamily::KeyChord => {
            gates.insert(ApprovalReason::KeyChord);
        }
        IntentFamily::TextEntry => {
            let class = match &step.intent {
                StepIntent::SetValue { text, .. } => text.class(),
                _ => TextClass::Benign,
            };
            let adjacent_text = class == TextClass::SensitiveAdjacent;
            let adjacent_surface = element_sensitivity.is_some_and(Sensitivity::requires_approval);
            if adjacent_text || adjacent_surface {
                gates.insert(ApprovalReason::SensitiveAdjacentTextEntry);
            }
        }
        IntentFamily::Semantic => {
            if element_sensitivity.is_some_and(Sensitivity::requires_approval) {
                gates.insert(ApprovalReason::SensitiveAdjacentTextEntry);
            }
        }
        IntentFamily::Ambient => {}
    }

    gates
}

/// One answer to one gate, bound to the exact step it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalDecision {
    /// Digest of the plan the approved step belongs to.
    pub plan_digest: String,
    pub step_index: u32,
    /// The exact gates this answer covers. An answer that covers fewer gates
    /// than the step opens does not authorize it.
    pub granted: Vec<ApprovalReason>,
    pub approved: bool,
    /// Lease epoch at the time the answer was given. An answer from before a
    /// takeover, pause, or cancellation is not usable afterwards.
    pub epoch: u64,
}

/// Whether the pending gates for a step are satisfied.
///
/// Returns `Ok(())` only when an answer exists that is bound to this plan,
/// this step, this epoch, and covers every open gate. Anything else refuses:
/// a missing answer with [`DenyReason::ApprovalRequired`], a refusal with
/// [`DenyReason::ApprovalDenied`], and a stale or partial answer with
/// `ApprovalRequired` -- treating a partial answer as a refusal would be
/// wrong (nobody said no), but treating it as consent would be worse.
pub fn check_gates(
    gates: &GateSet,
    plan_digest: &str,
    step_index: u32,
    epoch: u64,
    decision: Option<&ApprovalDecision>,
) -> Result<(), DenyReason> {
    if gates.is_empty() {
        return Ok(());
    }
    let Some(decision) = decision else {
        return Err(DenyReason::ApprovalRequired);
    };
    if decision.plan_digest != plan_digest
        || decision.step_index != step_index
        || decision.epoch != epoch
    {
        return Err(DenyReason::ApprovalRequired);
    }
    if !decision.approved {
        return Err(DenyReason::ApprovalDenied);
    }
    let granted: GateSet = decision.granted.iter().copied().collect();
    if !gates.is_subset(&granted) {
        return Err(DenyReason::ApprovalRequired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::AmbiguityAssessment;
    use crate::digest::{digest_str, domain};
    use crate::grounding::GroundingClaim;
    use crate::redaction::TextPayload;
    use crate::schema::{ElementRef, PointerButton, Postcondition};

    fn element() -> ElementRef {
        ElementRef::new("field-1", 1).unwrap()
    }

    fn step(intent: StepIntent, reversibility: Reversibility) -> PlannedStep {
        let grounding = match intent.element() {
            Some(element) => GroundingClaim::Semantic {
                element: element.clone(),
                role_digest: digest_str(domain::ELEMENT_ROLE, "text_field"),
            },
            None => GroundingClaim::None,
        };
        let expected = if intent.family().mutates() {
            Postcondition::FrameChanged
        } else {
            Postcondition::None
        };
        PlannedStep {
            index: 0,
            intent,
            grounding,
            ambiguity: AmbiguityAssessment::unambiguous(9_900),
            reversibility,
            expected,
        }
    }

    #[test]
    fn pointer_and_chord_steps_always_gate() {
        let pointer = step(
            StepIntent::PointerFallback {
                x: 4,
                y: 4,
                button: PointerButton::Primary,
            },
            Reversibility::Reversible,
        );
        assert!(gates_for(&pointer, None).contains(&ApprovalReason::PointerFallback));

        let chord = step(
            StepIntent::KeyChord {
                keys: vec![
                    crate::schema::ChordKey::Meta,
                    crate::schema::ChordKey::Delete,
                ],
            },
            Reversibility::Reversible,
        );
        assert!(gates_for(&chord, None).contains(&ApprovalReason::KeyChord));
    }

    #[test]
    fn irreversible_steps_gate_whatever_they_are() {
        for intent in [
            StepIntent::Invoke { element: element() },
            StepIntent::Select { element: element() },
        ] {
            let gated = step(intent, Reversibility::Irreversible);
            assert!(gates_for(&gated, None).contains(&ApprovalReason::IrreversibleStep));
        }
    }

    #[test]
    fn sensitive_adjacency_gates_from_either_side() {
        let benign_text = TextPayload::new("hello", crate::redaction::TextClass::Benign).unwrap();
        let adjacent_text =
            TextPayload::new("4111 1111", crate::redaction::TextClass::SensitiveAdjacent).unwrap();

        let benign_into_adjacent_field = step(
            StepIntent::SetValue {
                element: element(),
                text: benign_text,
            },
            Reversibility::Reversible,
        );
        assert!(
            gates_for(&benign_into_adjacent_field, Some(Sensitivity::Potential))
                .contains(&ApprovalReason::SensitiveAdjacentTextEntry)
        );

        let adjacent_into_benign_field = step(
            StepIntent::SetValue {
                element: element(),
                text: adjacent_text,
            },
            Reversibility::Reversible,
        );
        assert!(
            gates_for(&adjacent_into_benign_field, Some(Sensitivity::None))
                .contains(&ApprovalReason::SensitiveAdjacentTextEntry)
        );
    }

    #[test]
    fn ambient_steps_open_no_gate() {
        for intent in [
            StepIntent::Observe,
            StepIntent::Wait { millis: 10 },
            StepIntent::ActivateTarget,
            StepIntent::Complete,
        ] {
            assert!(gates_for(&step(intent, Reversibility::Reversible), None).is_empty());
        }
    }

    #[test]
    fn an_approval_authorizes_exactly_one_step_once() {
        let gates: GateSet = [ApprovalReason::PointerFallback].into_iter().collect();
        let decision = ApprovalDecision {
            plan_digest: "plan-a".into(),
            step_index: 2,
            granted: vec![ApprovalReason::PointerFallback],
            approved: true,
            epoch: 0,
        };
        assert!(check_gates(&gates, "plan-a", 2, 0, Some(&decision)).is_ok());
        // Different step, different plan, or a moved epoch: not authorized.
        assert_eq!(
            check_gates(&gates, "plan-a", 3, 0, Some(&decision)).unwrap_err(),
            DenyReason::ApprovalRequired
        );
        assert_eq!(
            check_gates(&gates, "plan-b", 2, 0, Some(&decision)).unwrap_err(),
            DenyReason::ApprovalRequired
        );
        assert_eq!(
            check_gates(&gates, "plan-a", 2, 1, Some(&decision)).unwrap_err(),
            DenyReason::ApprovalRequired
        );
    }

    #[test]
    fn a_partial_answer_is_not_consent() {
        let gates: GateSet = [
            ApprovalReason::PointerFallback,
            ApprovalReason::IrreversibleStep,
        ]
        .into_iter()
        .collect();
        let decision = ApprovalDecision {
            plan_digest: "plan-a".into(),
            step_index: 0,
            granted: vec![ApprovalReason::PointerFallback],
            approved: true,
            epoch: 0,
        };
        assert_eq!(
            check_gates(&gates, "plan-a", 0, 0, Some(&decision)).unwrap_err(),
            DenyReason::ApprovalRequired
        );
    }

    #[test]
    fn a_refusal_is_distinguishable_from_a_missing_answer() {
        let gates: GateSet = [ApprovalReason::IrreversibleStep].into_iter().collect();
        assert_eq!(
            check_gates(&gates, "plan-a", 0, 0, None).unwrap_err(),
            DenyReason::ApprovalRequired
        );
        let refused = ApprovalDecision {
            plan_digest: "plan-a".into(),
            step_index: 0,
            granted: vec![ApprovalReason::IrreversibleStep],
            approved: false,
            epoch: 0,
        };
        assert_eq!(
            check_gates(&gates, "plan-a", 0, 0, Some(&refused)).unwrap_err(),
            DenyReason::ApprovalDenied
        );
    }
}
