//! The deterministic adaptive profile policy engine (#435).
//!
//! Two decisions live here and nowhere else:
//!
//! 1. **Selection.** Given capability evidence and task risk, which profile
//!    does this run start in — or is the honest answer that it cannot start?
//! 2. **Reassessment.** Given a runtime signal, does the run escalate, or does
//!    it stop?
//!
//! Both are pure functions of their inputs. No clock, no randomness, no ambient
//! state, no provider round-trip. The same evidence and the same signal always
//! produce the same decision, which is what makes the profile shown in the
//! operator cockpit an explanation rather than a guess.
//!
//! # The rule that shapes everything else
//!
//! Escalation may never exceed [`CapabilityEvidence::ceiling`]. When a run
//! needs more assurance than the evidence can honestly supply, the outcome is
//! [`PolicyOutcome::Stop`], never a profile the model has not earned. Issue
//! #435 calls this out directly — "unknown or underqualified models default to
//! observe-only or no Computer Use" — and it is the difference between an
//! adaptive system and one that relabels a small model as a frontier one when
//! the task gets hard.
//!
//! # Ambiguity, stationarity, and confidence do not guess
//!
//! Every signal in [`RuntimeSignal`] resolves to exactly one of escalate or
//! stop. None of them resolves to "continue anyway". A duplicate-label surface
//! that Economy cannot disambiguate escalates to a profile with geometry; if
//! there is no such profile available, the run stops and says why. The same
//! frame arriving [`SafetyFloor::max_stationary_repeats`] times in a row means
//! the last action did nothing, so repeating it is not progress.

use serde::{Deserialize, Serialize};

use super::capability::CapabilityEvidence;
use super::profile::AdaptiveProfile;
use super::risk::TaskRisk;

/// Why a profile is in force, or why a run stopped. One closed vocabulary for
/// both, so the operator reads the same code in the cockpit that the audit
/// journal recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileReason {
    /// Selected: the cheapest profile is sufficient for a routine task.
    RoutineTask,
    /// Selected: the model has no image path, so semantic observations it is.
    TextOrientedModel,
    /// Selected: a consequential objective demands more than the floor.
    ConsequentialIntent,
    /// Selected or escalated: a destructive objective demands the strongest
    /// eligible path.
    DestructiveIntent,
    /// Selected: the surface is sensitivity-flagged.
    SensitiveSurface,
    /// Escalated: several candidates match the objective equally well.
    AmbiguousObservation,
    /// Escalated: the surface offers no actionable semantics.
    MissingSemantics,
    /// Escalated or stopped: the frame stopped changing.
    RepeatedStationarity,
    /// Escalated or stopped: consecutive unusable answers.
    RepeatedUncertainty,
    /// Escalated or stopped: a postcondition was contradicted.
    VerificationFailed,
    /// Stopped: the evidence cannot support the assurance the task needs.
    InsufficientCapabilityForRisk,
    /// Stopped: the model or provider capability narrowed mid-run.
    CapabilityRevoked,
    /// Stopped: no profile above the current one exists.
    EscalationCeilingReached,
    /// Stopped: the profile's model-call or turn budget is spent.
    BudgetExhausted,
    /// Stopped: High Assurance needs a verifier independent of the proposing
    /// model and the host does not have one.
    IndependentVerifierUnavailable,
    /// Stopped: the model is not admitted to propose at all.
    ModelNotQualified,
    /// Stopped: the capability generation changed under a live decision — a
    /// same-route tier downgrade, provenance change, schema drift, credential
    /// rotation, or operator policy edit (#458).
    CapabilityGenerationChanged,
    /// Stopped or re-selected: a later objective in the same run carries a
    /// higher risk class than the one the run was authorized for.
    HigherRiskObjective,
    /// Stopped: the profile's wall-clock budget for one turn was exceeded.
    TurnBudgetExceeded,
    /// Terminal: a process restart cut the run. Nothing is replayed.
    RunInterrupted,
    /// Terminal: the durable record failed its own invariants, so it is not
    /// trusted as authority. A record that cannot be shown to be internally
    /// consistent is treated as tampered or corrupt, never as permissive.
    RecordInvalid,
}

impl ProfileReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoutineTask => "routine_task",
            Self::TextOrientedModel => "text_oriented_model",
            Self::ConsequentialIntent => "consequential_intent",
            Self::DestructiveIntent => "destructive_intent",
            Self::SensitiveSurface => "sensitive_surface",
            Self::AmbiguousObservation => "ambiguous_observation",
            Self::MissingSemantics => "missing_semantics",
            Self::RepeatedStationarity => "repeated_stationarity",
            Self::RepeatedUncertainty => "repeated_uncertainty",
            Self::VerificationFailed => "verification_failed",
            Self::InsufficientCapabilityForRisk => "insufficient_capability_for_risk",
            Self::CapabilityRevoked => "capability_revoked",
            Self::EscalationCeilingReached => "escalation_ceiling_reached",
            Self::BudgetExhausted => "budget_exhausted",
            Self::IndependentVerifierUnavailable => "independent_verifier_unavailable",
            Self::ModelNotQualified => "model_not_qualified",
            Self::CapabilityGenerationChanged => "capability_generation_changed",
            Self::HigherRiskObjective => "higher_risk_objective",
            Self::TurnBudgetExceeded => "turn_budget_exceeded",
            Self::RunInterrupted => "run_interrupted",
            Self::RecordInvalid => "record_invalid",
        }
    }

    /// Operator-facing sentence. Fixed text per reason: it is shown in the
    /// cockpit and written to the journal, and it never interpolates observed
    /// content, model prose, or host paths.
    pub const fn operator_message(self) -> &'static str {
        match self {
            Self::RoutineTask => "Routine task; the cheapest profile is sufficient.",
            Self::TextOrientedModel => {
                "The selected model has no qualified image path, so it works from semantic observations."
            }
            Self::ConsequentialIntent => {
                "The objective is externally visible or hard to reverse, so more verification is required."
            }
            Self::DestructiveIntent => {
                "The objective destroys state, so the strongest eligible path is required."
            }
            Self::SensitiveSurface => {
                "The observed surface is flagged sensitive, so more verification is required."
            }
            Self::AmbiguousObservation => {
                "Several controls match the objective equally well; more observation detail is required."
            }
            Self::MissingSemantics => {
                "The surface exposes no actionable semantics at this detail level."
            }
            Self::RepeatedStationarity => {
                "The surface stopped changing, so repeating the last action would not be progress."
            }
            Self::RepeatedUncertainty => {
                "The model returned consecutive unusable answers."
            }
            Self::VerificationFailed => {
                "The host could not confirm the expected result of the last action."
            }
            Self::InsufficientCapabilityForRisk => {
                "This task requires more assurance than the selected model and host can demonstrate."
            }
            Self::CapabilityRevoked => {
                "The model or provider capability narrowed while the run was active."
            }
            Self::EscalationCeilingReached => {
                "There is no stronger profile available, so the run stopped instead of retrying."
            }
            Self::BudgetExhausted => "The profile's model-call budget for this run is spent.",
            Self::IndependentVerifierUnavailable => {
                "High Assurance requires a verifier independent of the proposing model, which is not available."
            }
            Self::ModelNotQualified => {
                "The selected model is not qualified to propose Computer actions."
            }
            Self::CapabilityGenerationChanged => {
                "The model or provider capability changed while the run was active, so the authority it was granted under no longer applies."
            }
            Self::HigherRiskObjective => {
                "This objective is more consequential than the one this run was authorized for."
            }
            Self::TurnBudgetExceeded => {
                "The model did not answer within this profile's time budget for one step."
            }
            Self::RunInterrupted => {
                "A restart interrupted this run. Nothing was replayed; a new authorization is required."
            }
            Self::RecordInvalid => {
                "This run's adaptive record is inconsistent, so it is no longer trusted. Start a new run."
            }
        }
    }
}

/// A signal observed while a run is in flight.
///
/// Every variant here is produced by the host from something it observed
/// itself. There is deliberately no model-self-report signal: a
/// `low_confidence` variant existed in an earlier draft, but the proposal wire
/// schema carries no confidence field, so nothing could produce it. An
/// advertised signal that production cannot raise is a claim, not a control.
/// The same reasoning removed `contradictory_semantics`: detecting an AX/pixel
/// disagreement needs a pixel path this build does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSignal {
    /// More than one enabled element matches the objective equally well.
    AmbiguousObservation,
    /// The rendered observation offered no actionable element.
    MissingSemantics,
    /// The frame repeated at least [`SafetyFloor::max_stationary_repeats`]
    /// times.
    RepeatedStationarity,
    /// Consecutive unusable answers reached the floor's tolerance.
    RepeatedUncertainty,
    /// A postcondition was contradicted, and this is the first such failure.
    VerificationFailed,
    /// A postcondition was contradicted again. Never escalates.
    VerificationExhausted,
    /// Capability narrowed mid-run.
    CapabilityRevoked,
    /// The profile's budget is spent.
    BudgetExhausted,
    /// The capability generation changed under a live decision (#458).
    CapabilityGenerationChanged,
    /// A later objective in this run carries a higher risk class.
    HigherRiskObjective,
    /// One turn exceeded the profile's wall-clock budget.
    TurnBudgetExceeded,
}

impl RuntimeSignal {
    const fn reason(self) -> ProfileReason {
        match self {
            Self::AmbiguousObservation => ProfileReason::AmbiguousObservation,
            Self::MissingSemantics => ProfileReason::MissingSemantics,
            Self::RepeatedStationarity => ProfileReason::RepeatedStationarity,
            Self::RepeatedUncertainty => ProfileReason::RepeatedUncertainty,
            Self::VerificationFailed | Self::VerificationExhausted => {
                ProfileReason::VerificationFailed
            }
            Self::CapabilityRevoked => ProfileReason::CapabilityRevoked,
            Self::BudgetExhausted => ProfileReason::BudgetExhausted,
            Self::CapabilityGenerationChanged => ProfileReason::CapabilityGenerationChanged,
            Self::HigherRiskObjective => ProfileReason::HigherRiskObjective,
            Self::TurnBudgetExceeded => ProfileReason::TurnBudgetExceeded,
        }
    }

    /// Signals that terminate the run outright, whatever profile is in force.
    ///
    /// A spent budget or a withdrawn capability is not something a bigger model
    /// fixes, and a plan whose postcondition failed twice is the blind-retry
    /// loop this policy exists to prevent.
    const fn always_terminal(self) -> bool {
        matches!(
            self,
            Self::CapabilityRevoked
                | Self::CapabilityGenerationChanged
                | Self::BudgetExhausted
                | Self::VerificationExhausted
                | Self::TurnBudgetExceeded
                | Self::HigherRiskObjective
        )
    }
}

/// The profile a run starts in, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDecision {
    pub profile: AdaptiveProfile,
    pub reason: ProfileReason,
    pub risk: TaskRisk,
    pub ceiling: AdaptiveProfile,
    pub evidence: CapabilityEvidence,
}

/// Why a run cannot start or continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyStop {
    pub reason: ProfileReason,
    /// The profile in force when the run stopped.
    pub profile: AdaptiveProfile,
    /// The profile that would have been required to continue, when one exists.
    pub required_profile: Option<AdaptiveProfile>,
    pub ceiling: AdaptiveProfile,
}

impl PolicyStop {
    pub fn operator_message(&self) -> &'static str {
        self.reason.operator_message()
    }
}

/// Result of a selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PolicyOutcome {
    Proceed(ProfileDecision),
    Stop(PolicyStop),
}

/// Result of a reassessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum ProfileTransition {
    /// The run moves up one rung. `from` and `to` are always adjacent: the
    /// ladder is climbed a step at a time so every escalation is attributable
    /// to exactly one signal.
    Escalate {
        from: AdaptiveProfile,
        to: AdaptiveProfile,
        reason: ProfileReason,
    },
    /// The run terminates honestly.
    Stop(PolicyStop),
}

/// The engine. Stateless by construction — all run state lives in
/// [`super::controller::AdaptiveController`], so the rules can be tested
/// without building a run.
#[derive(Debug, Clone, Copy, Default)]
pub struct AdaptivePolicyEngine;

impl AdaptivePolicyEngine {
    /// The lowest profile a given risk class may run in.
    ///
    /// Routine work starts at Economy — that is the cost saving the whole
    /// feature exists for. Consequential and destructive work start higher
    /// because the cost of being wrong is not symmetric.
    pub const fn risk_floor(risk: TaskRisk) -> AdaptiveProfile {
        match risk {
            TaskRisk::Routine => AdaptiveProfile::Economy,
            TaskRisk::Consequential => AdaptiveProfile::Balanced,
            TaskRisk::Destructive => AdaptiveProfile::HighAssurance,
        }
    }

    const fn selection_reason(risk: TaskRisk, text_oriented: bool) -> ProfileReason {
        match risk {
            TaskRisk::Destructive => ProfileReason::DestructiveIntent,
            TaskRisk::Consequential => ProfileReason::ConsequentialIntent,
            TaskRisk::Routine if text_oriented => ProfileReason::TextOrientedModel,
            TaskRisk::Routine => ProfileReason::RoutineTask,
        }
    }

    /// Chooses the starting profile, or refuses to start.
    pub fn select(&self, evidence: &CapabilityEvidence, risk: TaskRisk) -> PolicyOutcome {
        let ceiling = evidence.ceiling();
        if !evidence.model.may_propose() {
            return PolicyOutcome::Stop(PolicyStop {
                reason: ProfileReason::ModelNotQualified,
                profile: AdaptiveProfile::Economy,
                required_profile: None,
                ceiling,
            });
        }
        let floor = Self::risk_floor(risk);
        if floor > ceiling {
            // The task needs more assurance than this model and host can
            // demonstrate. Naming the specific missing leg is more useful to
            // an operator than a generic refusal.
            let reason =
                if floor == AdaptiveProfile::HighAssurance && !evidence.host.independent_verifier {
                    ProfileReason::IndependentVerifierUnavailable
                } else {
                    ProfileReason::InsufficientCapabilityForRisk
                };
            return PolicyOutcome::Stop(PolicyStop {
                reason,
                profile: ceiling,
                required_profile: Some(floor),
                ceiling,
            });
        }
        PolicyOutcome::Proceed(ProfileDecision {
            profile: floor,
            reason: Self::selection_reason(risk, evidence.model.is_text_oriented()),
            risk,
            ceiling,
            evidence: evidence.clone(),
        })
    }

    /// Decides what a runtime signal means for a run already in flight.
    ///
    /// There is no "continue" arm. A caller reaches this function only when
    /// something already told it the current path is not working, so treating
    /// the signal as advisory would be the guess this policy exists to refuse.
    pub fn reassess(
        &self,
        current: AdaptiveProfile,
        evidence: &CapabilityEvidence,
        signal: RuntimeSignal,
    ) -> ProfileTransition {
        let ceiling = evidence.ceiling();
        let reason = signal.reason();
        if signal.always_terminal() {
            return ProfileTransition::Stop(PolicyStop {
                reason,
                profile: current,
                required_profile: None,
                ceiling,
            });
        }
        let Some(next) = current.escalated() else {
            // Already at the top of the ladder. There is nothing to buy.
            return ProfileTransition::Stop(PolicyStop {
                reason,
                profile: current,
                required_profile: None,
                ceiling,
            });
        };
        if next > ceiling {
            return ProfileTransition::Stop(PolicyStop {
                reason,
                profile: current,
                required_profile: Some(next),
                ceiling,
            });
        }
        ProfileTransition::Escalate {
            from: current,
            to: next,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_profile::capability::{
        CapabilityAttribution, CapabilityGeneration, HostCapabilityEvidence,
        ModelCapabilityEvidence, OperatorCapabilityPolicy,
    };
    use crate::gateway_config::ComputerUseTier;

    fn evidence(
        tier: ComputerUseTier,
        image: bool,
        verifier: bool,
        durable: bool,
    ) -> CapabilityEvidence {
        CapabilityEvidence::new(
            ModelCapabilityEvidence {
                tools: true,
                image_input: image,
                max_image_bytes: image.then_some(4 * 1024 * 1024),
                tier,
                attribution: CapabilityAttribution::Measured,
                durable_authority: durable,
                session_measured: !durable,
                synthetic_only: !durable,
                generation: CapabilityGeneration::compute(
                    "route-1",
                    &crate::gateway_config::ModelCapabilities::default(),
                    "cred-1",
                    &OperatorCapabilityPolicy::default(),
                ),
                declared_capability_trusted: false,
            },
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: image,
                independent_verifier: verifier,
            },
        )
    }

    fn frontier() -> CapabilityEvidence {
        evidence(ComputerUseTier::VisualFallbackAct, true, true, true)
    }

    fn small_text_model() -> CapabilityEvidence {
        evidence(ComputerUseTier::SemanticAct, false, false, true)
    }

    #[test]
    fn routine_work_starts_in_economy_even_on_a_frontier_model() {
        let outcome = AdaptivePolicyEngine.select(&frontier(), TaskRisk::Routine);
        let PolicyOutcome::Proceed(decision) = outcome else {
            panic!("routine work must proceed");
        };
        assert_eq!(decision.profile, AdaptiveProfile::Economy);
        assert_eq!(decision.reason, ProfileReason::RoutineTask);
        assert_eq!(decision.ceiling, AdaptiveProfile::HighAssurance);
    }

    #[test]
    fn destructive_work_on_a_small_text_model_stops_rather_than_pretending() {
        let outcome = AdaptivePolicyEngine.select(&small_text_model(), TaskRisk::Destructive);
        let PolicyOutcome::Stop(stop) = outcome else {
            panic!("a text-only model must not run destructive work");
        };
        assert_eq!(stop.reason, ProfileReason::IndependentVerifierUnavailable);
        assert_eq!(stop.required_profile, Some(AdaptiveProfile::HighAssurance));
        assert_eq!(stop.ceiling, AdaptiveProfile::Economy);
    }

    #[test]
    fn destructive_work_on_full_evidence_starts_at_high_assurance() {
        let PolicyOutcome::Proceed(decision) =
            AdaptivePolicyEngine.select(&frontier(), TaskRisk::Destructive)
        else {
            panic!("full evidence supports destructive work");
        };
        assert_eq!(decision.profile, AdaptiveProfile::HighAssurance);
        assert_eq!(decision.reason, ProfileReason::DestructiveIntent);
    }

    #[test]
    fn an_unqualified_model_never_selects_a_profile() {
        let mut unqualified = frontier();
        unqualified.model.tools = false;
        let PolicyOutcome::Stop(stop) =
            AdaptivePolicyEngine.select(&unqualified, TaskRisk::Routine)
        else {
            panic!("a model that cannot emit tool calls must not propose");
        };
        assert_eq!(stop.reason, ProfileReason::ModelNotQualified);
    }

    #[test]
    fn escalation_never_exceeds_the_capability_ceiling() {
        let small = small_text_model();
        assert_eq!(small.ceiling(), AdaptiveProfile::Economy);
        let transition = AdaptivePolicyEngine.reassess(
            AdaptiveProfile::Economy,
            &small,
            RuntimeSignal::AmbiguousObservation,
        );
        let ProfileTransition::Stop(stop) = transition else {
            panic!("a capped run must stop, not escalate past its ceiling");
        };
        assert_eq!(stop.reason, ProfileReason::AmbiguousObservation);
        assert_eq!(stop.required_profile, Some(AdaptiveProfile::Balanced));
    }

    #[test]
    fn escalation_climbs_one_rung_at_a_time() {
        let transition = AdaptivePolicyEngine.reassess(
            AdaptiveProfile::Economy,
            &frontier(),
            RuntimeSignal::MissingSemantics,
        );
        assert_eq!(
            transition,
            ProfileTransition::Escalate {
                from: AdaptiveProfile::Economy,
                to: AdaptiveProfile::Balanced,
                reason: ProfileReason::MissingSemantics,
            }
        );
    }

    #[test]
    fn the_top_of_the_ladder_stops_instead_of_looping() {
        let transition = AdaptivePolicyEngine.reassess(
            AdaptiveProfile::HighAssurance,
            &frontier(),
            RuntimeSignal::RepeatedStationarity,
        );
        let ProfileTransition::Stop(stop) = transition else {
            panic!("there is nothing above High Assurance to escalate to");
        };
        assert_eq!(stop.reason, ProfileReason::RepeatedStationarity);
        assert_eq!(stop.required_profile, None);
    }

    #[test]
    fn terminal_signals_never_escalate_from_any_profile() {
        for profile in AdaptiveProfile::ALL {
            for signal in [
                RuntimeSignal::CapabilityRevoked,
                RuntimeSignal::BudgetExhausted,
                RuntimeSignal::VerificationExhausted,
            ] {
                let transition = AdaptivePolicyEngine.reassess(profile, &frontier(), signal);
                assert!(
                    matches!(transition, ProfileTransition::Stop(_)),
                    "{profile} escalated on {signal:?}"
                );
            }
        }
    }

    #[test]
    fn every_signal_resolves_to_escalate_or_stop_and_never_to_continue() {
        // Exhaustive over the signal vocabulary: a new signal that forgets to
        // decide would fail to compile against this match, and one that decides
        // "continue" has nowhere to put that answer.
        let signals = [
            RuntimeSignal::AmbiguousObservation,
            RuntimeSignal::MissingSemantics,
            RuntimeSignal::RepeatedStationarity,
            RuntimeSignal::RepeatedUncertainty,
            RuntimeSignal::VerificationFailed,
            RuntimeSignal::VerificationExhausted,
            RuntimeSignal::CapabilityRevoked,
            RuntimeSignal::BudgetExhausted,
            RuntimeSignal::CapabilityGenerationChanged,
            RuntimeSignal::HigherRiskObjective,
            RuntimeSignal::TurnBudgetExceeded,
        ];
        for signal in signals {
            for profile in AdaptiveProfile::ALL {
                let transition = AdaptivePolicyEngine.reassess(profile, &frontier(), signal);
                match transition {
                    ProfileTransition::Escalate { from, to, .. } => {
                        assert!(to > from, "escalation must move up the ladder");
                        assert_eq!(from.escalated(), Some(to), "escalation skipped a rung");
                    }
                    ProfileTransition::Stop(stop) => {
                        assert!(!stop.operator_message().is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn selection_is_deterministic() {
        let evidence = frontier();
        for risk in [
            TaskRisk::Routine,
            TaskRisk::Consequential,
            TaskRisk::Destructive,
        ] {
            let first = AdaptivePolicyEngine.select(&evidence, risk);
            for _ in 0..8 {
                assert_eq!(AdaptivePolicyEngine.select(&evidence, risk), first);
            }
        }
    }

    #[test]
    fn every_reason_has_a_distinct_wire_code_and_operator_message() {
        let reasons = [
            ProfileReason::RoutineTask,
            ProfileReason::TextOrientedModel,
            ProfileReason::ConsequentialIntent,
            ProfileReason::DestructiveIntent,
            ProfileReason::SensitiveSurface,
            ProfileReason::AmbiguousObservation,
            ProfileReason::MissingSemantics,
            ProfileReason::RepeatedStationarity,
            ProfileReason::RepeatedUncertainty,
            ProfileReason::VerificationFailed,
            ProfileReason::InsufficientCapabilityForRisk,
            ProfileReason::CapabilityRevoked,
            ProfileReason::EscalationCeilingReached,
            ProfileReason::BudgetExhausted,
            ProfileReason::IndependentVerifierUnavailable,
            ProfileReason::ModelNotQualified,
            ProfileReason::CapabilityGenerationChanged,
            ProfileReason::HigherRiskObjective,
            ProfileReason::TurnBudgetExceeded,
            ProfileReason::RunInterrupted,
            ProfileReason::RecordInvalid,
        ];
        let codes: std::collections::BTreeSet<_> =
            reasons.iter().map(|reason| reason.as_str()).collect();
        assert_eq!(codes.len(), reasons.len(), "duplicate reason wire code");
        for reason in reasons {
            assert!(!reason.operator_message().is_empty());
            // Serde spelling and `as_str` must not drift apart.
            assert_eq!(
                serde_json::to_string(&reason).unwrap(),
                format!("\"{}\"", reason.as_str())
            );
        }
    }
}
