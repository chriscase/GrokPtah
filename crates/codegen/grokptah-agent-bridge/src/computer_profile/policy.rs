//! Pure adaptive profile selection and transition policy.
//!
//! Selection and reassessment are intentionally separate from the durable
//! controller. This module has no clock, randomness, provider access, or
//! authority mutation. It can only return a profile or a fail-closed stop.

use serde::{Deserialize, Serialize};

use super::capability::CapabilityEvidence;
use super::profile::{AdaptiveProfile, SafetyFloor};
use super::risk::TaskRisk;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileReason {
    RoutineTask,
    TextOrientedModel,
    ConsequentialIntent,
    DestructiveIntent,
    SensitiveSurface,
    AmbiguousObservation,
    MissingSemantics,
    ContradictorySemantics,
    RepeatedStationarity,
    LowConfidence,
    RepeatedUncertainty,
    VerificationFailed,
    InsufficientCapabilityForRisk,
    CapabilityRevoked,
    EscalationCeilingReached,
    BudgetExhausted,
    IndependentVerifierUnavailable,
    ModelNotQualified,
    AuthorityUnavailable,
    ProviderUncertain,
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
            Self::ContradictorySemantics => "contradictory_semantics",
            Self::RepeatedStationarity => "repeated_stationarity",
            Self::LowConfidence => "low_confidence",
            Self::RepeatedUncertainty => "repeated_uncertainty",
            Self::VerificationFailed => "verification_failed",
            Self::InsufficientCapabilityForRisk => "insufficient_capability_for_risk",
            Self::CapabilityRevoked => "capability_revoked",
            Self::EscalationCeilingReached => "escalation_ceiling_reached",
            Self::BudgetExhausted => "budget_exhausted",
            Self::IndependentVerifierUnavailable => "independent_verifier_unavailable",
            Self::ModelNotQualified => "model_not_qualified",
            Self::AuthorityUnavailable => "authority_unavailable",
            Self::ProviderUncertain => "provider_uncertain",
        }
    }

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
            Self::ContradictorySemantics => {
                "The accessibility and visual accounts of this surface disagree."
            }
            Self::RepeatedStationarity => {
                "The surface stopped changing, so repeating the last action would not be progress."
            }
            Self::LowConfidence => "The model reported confidence below the required floor.",
            Self::RepeatedUncertainty => "The model returned consecutive unusable answers.",
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
            Self::AuthorityUnavailable => {
                "Canonical principal, capability-generation, and provider-attempt authority is unavailable."
            }
            Self::ProviderUncertain => {
                "The provider attempt may have reached the provider, so the result will not be replayed automatically."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSignal {
    AmbiguousObservation,
    MissingSemantics,
    ContradictorySemantics,
    RepeatedStationarity,
    LowConfidence,
    RepeatedUncertainty,
    VerificationFailed,
    VerificationExhausted,
    CapabilityRevoked,
    BudgetExhausted,
    DestructiveIntentDetected,
    AuthorityUnavailable,
    ProviderUncertain,
}

impl RuntimeSignal {
    pub const fn reason(self) -> ProfileReason {
        match self {
            Self::AmbiguousObservation => ProfileReason::AmbiguousObservation,
            Self::MissingSemantics => ProfileReason::MissingSemantics,
            Self::ContradictorySemantics => ProfileReason::ContradictorySemantics,
            Self::RepeatedStationarity => ProfileReason::RepeatedStationarity,
            Self::LowConfidence => ProfileReason::LowConfidence,
            Self::RepeatedUncertainty => ProfileReason::RepeatedUncertainty,
            Self::VerificationFailed | Self::VerificationExhausted => {
                ProfileReason::VerificationFailed
            }
            Self::CapabilityRevoked => ProfileReason::CapabilityRevoked,
            Self::BudgetExhausted => ProfileReason::BudgetExhausted,
            Self::DestructiveIntentDetected => ProfileReason::DestructiveIntent,
            Self::AuthorityUnavailable => ProfileReason::AuthorityUnavailable,
            Self::ProviderUncertain => ProfileReason::ProviderUncertain,
        }
    }

    pub const fn always_stop(self) -> bool {
        matches!(
            self,
            Self::VerificationExhausted
                | Self::CapabilityRevoked
                | Self::BudgetExhausted
                | Self::AuthorityUnavailable
                | Self::ProviderUncertain
        )
    }
}

/// Explicit operator/task policy. `minimum_profile` is a minimum requested
/// assurance, never a request to lower the risk floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskPolicy {
    pub risk: TaskRisk,
    pub minimum_profile: Option<AdaptiveProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDecision {
    pub profile: AdaptiveProfile,
    pub reason: ProfileReason,
    pub risk: TaskRisk,
    pub ceiling: AdaptiveProfile,
    pub capability_snapshot_reference: Option<String>,
    pub evidence: CapabilityEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyStop {
    pub reason: ProfileReason,
    pub profile: AdaptiveProfile,
    pub required_profile: Option<AdaptiveProfile>,
    pub ceiling: AdaptiveProfile,
}

impl PolicyStop {
    pub const fn operator_message(&self) -> &'static str {
        self.reason.operator_message()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PolicyOutcome {
    Proceed(ProfileDecision),
    Stop(PolicyStop),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum ProfileTransition {
    Escalate {
        from: AdaptiveProfile,
        to: AdaptiveProfile,
        reason: ProfileReason,
    },
    Stop(PolicyStop),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AdaptivePolicyEngine;

impl AdaptivePolicyEngine {
    pub const fn risk_floor(risk: TaskRisk) -> AdaptiveProfile {
        match risk {
            TaskRisk::Routine => AdaptiveProfile::Economy,
            TaskRisk::Consequential => AdaptiveProfile::Balanced,
            TaskRisk::Destructive => AdaptiveProfile::HighAssurance,
        }
    }

    /// Select the least expensive profile satisfying both explicit policy and
    /// risk. Capability evidence can only cap this choice.
    pub fn select(&self, evidence: &CapabilityEvidence, policy: TaskPolicy) -> PolicyOutcome {
        let ceiling = evidence.ceiling();
        if !evidence.may_propose() {
            return PolicyOutcome::Stop(PolicyStop {
                reason: ProfileReason::ModelNotQualified,
                profile: AdaptiveProfile::Economy,
                required_profile: None,
                ceiling,
            });
        }

        let requested = policy.minimum_profile.unwrap_or(AdaptiveProfile::Economy);
        let floor = Self::risk_floor(policy.risk).max(requested);
        if floor > ceiling {
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
        let reason = match policy.risk {
            TaskRisk::Destructive => ProfileReason::DestructiveIntent,
            TaskRisk::Consequential => ProfileReason::ConsequentialIntent,
            TaskRisk::Routine if evidence.model.is_text_oriented() => {
                ProfileReason::TextOrientedModel
            }
            TaskRisk::Routine => ProfileReason::RoutineTask,
        };
        PolicyOutcome::Proceed(ProfileDecision {
            profile: floor,
            reason,
            risk: policy.risk,
            ceiling,
            capability_snapshot_reference: evidence.capability_snapshot_reference(),
            evidence: evidence.clone(),
        })
    }

    pub fn reassess(
        &self,
        current: AdaptiveProfile,
        evidence: &CapabilityEvidence,
        signal: RuntimeSignal,
    ) -> ProfileTransition {
        let ceiling = evidence.ceiling();
        let reason = signal.reason();
        if signal.always_stop() {
            return ProfileTransition::Stop(PolicyStop {
                reason,
                profile: current,
                required_profile: None,
                ceiling,
            });
        }
        let required = match signal {
            // A destructive signal is conditional on the capability ceiling.
            // It is not an unconditional terminal outcome: a qualified
            // High Assurance path must be allowed to satisfy it.
            RuntimeSignal::DestructiveIntentDetected => AdaptiveProfile::HighAssurance,
            _ => current.escalated().unwrap_or(current),
        };
        let capability_failure_reason = matches!(signal, RuntimeSignal::DestructiveIntentDetected)
            .then(|| Self::risk_floor_failure_reason(required, evidence));
        self.transition_toward(
            current,
            evidence,
            required,
            reason,
            capability_failure_reason,
        )
    }

    /// Reassess the monotonic risk floor independently of transient runtime
    /// signals. Risk classification is host-derived and may rise when the
    /// objective is unchanged, so it must retain its own reason and required
    /// profile instead of being represented as destructive intent.
    pub fn reassess_risk_floor(
        &self,
        current: AdaptiveProfile,
        evidence: &CapabilityEvidence,
        risk: TaskRisk,
    ) -> Option<ProfileTransition> {
        let required = Self::risk_floor(risk);
        if required <= current {
            return None;
        }
        let reason = match risk {
            TaskRisk::Routine => ProfileReason::RoutineTask,
            TaskRisk::Consequential => ProfileReason::ConsequentialIntent,
            TaskRisk::Destructive => ProfileReason::DestructiveIntent,
        };
        Some(self.transition_toward(
            current,
            evidence,
            required,
            reason,
            Some(Self::risk_floor_failure_reason(required, evidence)),
        ))
    }

    fn transition_toward(
        &self,
        current: AdaptiveProfile,
        evidence: &CapabilityEvidence,
        required: AdaptiveProfile,
        reason: ProfileReason,
        capability_failure_reason: Option<ProfileReason>,
    ) -> ProfileTransition {
        let ceiling = evidence.ceiling();
        if required > ceiling {
            return ProfileTransition::Stop(PolicyStop {
                reason: capability_failure_reason.unwrap_or(reason),
                profile: current,
                required_profile: Some(required),
                ceiling,
            });
        }
        let Some(next) = current.escalated() else {
            return ProfileTransition::Stop(PolicyStop {
                reason,
                profile: current,
                required_profile: Some(required),
                ceiling,
            });
        };
        if next > ceiling {
            return ProfileTransition::Stop(PolicyStop {
                reason: capability_failure_reason.unwrap_or(reason),
                profile: current,
                required_profile: Some(required),
                ceiling,
            });
        }
        ProfileTransition::Escalate {
            from: current,
            to: next,
            reason,
        }
    }

    fn risk_floor_failure_reason(
        required: AdaptiveProfile,
        evidence: &CapabilityEvidence,
    ) -> ProfileReason {
        if required == AdaptiveProfile::HighAssurance && !evidence.host.independent_verifier {
            ProfileReason::IndependentVerifierUnavailable
        } else {
            ProfileReason::InsufficientCapabilityForRisk
        }
    }

    pub const fn confidence_floor_permille() -> u16 {
        SafetyFloor::REQUIRED.min_confidence_permille
    }

    pub const fn is_low_confidence(confidence_permille: Option<u16>) -> bool {
        match confidence_permille {
            Some(value) => value < Self::confidence_floor_permille(),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_profile::capability::{
        CapabilityAttribution, HostCapabilityEvidence, ModelCapabilityEvidence,
    };
    use crate::gateway_config::ComputerUseTier;

    fn evidence(image: bool, verifier: bool, isolated: bool) -> CapabilityEvidence {
        CapabilityEvidence::with_authority(
            ModelCapabilityEvidence {
                tools: true,
                image_input: image,
                max_image_bytes: image.then_some(4 * 1024 * 1024),
                tier: if image {
                    ComputerUseTier::VisualFallbackAct
                } else {
                    ComputerUseTier::SemanticAct
                },
                attribution: CapabilityAttribution::Measured,
                durable_authority: true,
                session_measured: false,
                synthetic_only: false,
            },
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: image,
                independent_verifier: verifier,
                isolated_guest: isolated,
            },
            super::super::authority_seam::test_binding(),
        )
    }

    #[test]
    fn risk_floor_and_capability_ceiling_are_fail_closed() {
        let small = evidence(false, false, false);
        assert!(matches!(
            AdaptivePolicyEngine.select(
                &small,
                TaskPolicy {
                    risk: TaskRisk::Destructive,
                    minimum_profile: None
                }
            ),
            PolicyOutcome::Stop(PolicyStop {
                reason: ProfileReason::IndependentVerifierUnavailable,
                ..
            })
        ));
        let high = evidence(true, true, true);
        let PolicyOutcome::Proceed(decision) = AdaptivePolicyEngine.select(
            &high,
            TaskPolicy {
                risk: TaskRisk::Destructive,
                minimum_profile: None,
            },
        ) else {
            panic!("full evidence should support destructive policy");
        };
        assert_eq!(decision.profile, AdaptiveProfile::HighAssurance);
    }

    #[test]
    fn every_signal_is_escalate_or_stop() {
        let evidence = evidence(true, true, true);
        let signals = [
            RuntimeSignal::AmbiguousObservation,
            RuntimeSignal::MissingSemantics,
            RuntimeSignal::ContradictorySemantics,
            RuntimeSignal::RepeatedStationarity,
            RuntimeSignal::LowConfidence,
            RuntimeSignal::RepeatedUncertainty,
            RuntimeSignal::VerificationFailed,
            RuntimeSignal::VerificationExhausted,
            RuntimeSignal::CapabilityRevoked,
            RuntimeSignal::BudgetExhausted,
            RuntimeSignal::DestructiveIntentDetected,
            RuntimeSignal::AuthorityUnavailable,
            RuntimeSignal::ProviderUncertain,
        ];
        for signal in signals {
            match AdaptivePolicyEngine.reassess(AdaptiveProfile::Economy, &evidence, signal) {
                ProfileTransition::Escalate { from, to, .. } => {
                    assert_eq!(from.escalated(), Some(to));
                }
                ProfileTransition::Stop(stop) => assert!(!stop.operator_message().is_empty()),
            }
        }
    }

    #[test]
    fn missing_confidence_is_not_high_confidence() {
        assert!(AdaptivePolicyEngine::is_low_confidence(None));
        assert!(AdaptivePolicyEngine::is_low_confidence(Some(699)));
        assert!(!AdaptivePolicyEngine::is_low_confidence(Some(700)));
    }
}
