//! The operator-readable view of adaptive state.
//!
//! This is the only serialized shape the cockpit and any future coordinator
//! surface read, for the same reason the Computer Run has exactly one
//! projection: two surfaces deriving their own view is how a GUI and an
//! external observer come to disagree about what a run is doing.
//!
//! It is redaction-safe **by construction**, not by filtering. Element labels,
//! values, geometry, evidence tokens, and the stationarity digest have no field
//! to travel in. What an operator learns is: which profile is in force, why,
//! what evidence supported it, every escalation and its cause, what the run has
//! spent, and — when it ended — exactly why it ended.
//!
//! # Unknown is a value
//!
//! Provider-reported cost fields are [`Option`] and serialize to `null` when
//! the provider reported nothing. They are never zero-filled and never
//! estimated. `costUsd` is absent from this type entirely rather than present
//! and null, because this process has no price table and a field that is always
//! null is a promise it might one day not be.

use serde::{Deserialize, Serialize};

use super::capability::{CapabilityAttribution, CapabilityEvidence};
use super::policy::ProfileReason;
use super::profile::{AdaptiveProfile, ObservationDetail, ProfileBudget, SafetyFloor};
use super::record::{
    AdaptiveLifecycle, AdaptiveRecord, CostLedger, EscalationRecord, TerminalOutcome,
};
use super::risk::TaskRisk;
use crate::gateway_config::ComputerUseTier;

/// Capability evidence as the operator reads it.
///
/// Deliberately restates rather than re-exports the internal evidence type: the
/// operator needs the *qualification story* (declared or measured, durable or
/// this-session, synthetic or live), not the internal booleans, and the two
/// should be free to diverge without silently changing what the cockpit shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvidenceProjection {
    /// Computer Use tier in force for the exact route.
    pub tier: ComputerUseTier,
    /// How the tier was attributed.
    pub attribution: CapabilityAttribution,
    /// Whether the model can emit structured tool calls at all.
    pub structured_tools: bool,
    /// Whether the model declares image input.
    pub image_input: bool,
    /// Whether a complete visual path (image input, stated byte ceiling,
    /// visual-fallback tier, durable authority) is established.
    pub qualified_visual_path: bool,
    /// Whether the authority came from the provider profile rather than this
    /// process.
    pub durable_authority: bool,
    /// Whether this process measured the route against the deterministic
    /// simulator.
    pub session_measured: bool,
    /// True when every measurement backing this run was synthetic. A synthetic
    /// PASS is real evidence that the model emits valid proposals; it is not
    /// live eligibility, and this flag is how the cockpit says so.
    pub synthetic_only: bool,
    /// Whether the host can capture a redacted screenshot for this target.
    pub host_screenshot_capture: bool,
    /// Whether a verifier independent of the proposing model is available.
    pub host_independent_verifier: bool,
    /// The highest profile this evidence can honestly support.
    pub ceiling: AdaptiveProfile,
    /// Short, operator-readable prefix of the capability generation this run
    /// was authorized under (#458). Secret-free by construction: the digest is
    /// one-way and the credential only ever entered it as a hash.
    pub generation: String,
    /// Whether local operator policy trusts declared-only capability. When
    /// false, a declared-only route may observe but never act.
    pub declared_capability_trusted: bool,
}

impl CapabilityEvidenceProjection {
    fn of(evidence: &CapabilityEvidence) -> Self {
        Self {
            tier: evidence.model.tier,
            attribution: evidence.model.attribution,
            structured_tools: evidence.model.tools,
            image_input: evidence.model.image_input,
            qualified_visual_path: evidence.model.has_qualified_visual_path(),
            durable_authority: evidence.model.durable_authority,
            session_measured: evidence.model.session_measured,
            synthetic_only: evidence.model.synthetic_only,
            host_screenshot_capture: evidence.host.screenshot_capture,
            host_independent_verifier: evidence.host.independent_verifier,
            ceiling: evidence.ceiling(),
            generation: evidence.model.generation.short().to_string(),
            declared_capability_trusted: evidence.model.declared_capability_trusted,
        }
    }
}

/// One escalation, with its operator-facing sentence resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationProjection {
    pub from: AdaptiveProfile,
    pub to: AdaptiveProfile,
    pub reason: ProfileReason,
    pub message: String,
    pub revision: u64,
}

impl EscalationProjection {
    fn of(record: &EscalationRecord) -> Self {
        Self {
            from: record.from,
            to: record.to,
            reason: record.reason,
            message: record.reason.operator_message().to_string(),
            revision: record.revision,
        }
    }
}

/// The efficiency budget in force, so the cockpit can explain what Economy
/// actually bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetProjection {
    pub observation_detail: ObservationDetail,
    pub max_observation_elements: u32,
    pub max_observation_bytes: u64,
    pub max_model_calls: u32,
    pub max_turn_millis: u64,
    pub pointer_fallback_allowed: bool,
    pub key_chord_allowed: bool,
}

impl BudgetProjection {
    const fn of(budget: ProfileBudget) -> Self {
        Self {
            observation_detail: budget.observation_detail,
            max_observation_elements: budget.max_observation_elements,
            max_observation_bytes: budget.max_observation_bytes,
            max_model_calls: budget.max_model_calls,
            max_turn_millis: budget.max_turn_millis,
            pointer_fallback_allowed: budget.allows_pointer_fallback,
            key_chord_allowed: budget.allows_key_chord,
        }
    }
}

/// What the run has spent. Host-measured fields are always present; provider
/// fields are `null` until a provider reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostProjection {
    /// Every provider attempt, including those that timed out, died in
    /// transport, returned prose, or failed schema validation. They all cost
    /// money, so they all appear here.
    pub provider_attempts: u32,
    pub accepted_attempts: u32,
    pub failed_attempts: u32,
    pub observation_bytes: u64,
    /// Structurally zero: screenshot bytes do not cross the model boundary in
    /// any profile.
    pub screenshot_bytes: u64,
    /// Provider-reported usage, preserved even for attempts whose bodies later
    /// failed to parse. `null` until a provider reports something.
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

impl CostProjection {
    const fn of(cost: CostLedger) -> Self {
        Self {
            provider_attempts: cost.provider_attempts,
            accepted_attempts: cost.accepted_attempts,
            failed_attempts: cost.failed_attempts,
            observation_bytes: cost.observation_bytes,
            screenshot_bytes: cost.screenshot_bytes,
            prompt_tokens: cost.prompt_tokens,
            completion_tokens: cost.completion_tokens,
        }
    }
}

/// How the run ended, with its reason resolved to operator text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalProjection {
    pub kind: AdaptiveLifecycle,
    pub reason: ProfileReason,
    pub message: String,
    pub profile: AdaptiveProfile,
    pub required_profile: Option<AdaptiveProfile>,
}

impl TerminalProjection {
    fn of(outcome: &TerminalOutcome) -> Self {
        Self {
            kind: outcome.lifecycle,
            reason: outcome.reason,
            message: outcome.reason.operator_message().to_string(),
            profile: outcome.profile,
            required_profile: outcome.required_profile,
        }
    }
}

/// The complete operator-readable adaptive state for one Computer Run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveProfileProjection {
    /// Always a canonical #435 name. Never an alias.
    pub profile: AdaptiveProfile,
    /// Human-readable profile name for the cockpit header.
    pub profile_display_name: String,
    /// Why this profile, in one closed code.
    pub reason: ProfileReason,
    /// The fixed operator sentence for that code.
    pub message: String,
    /// The risk class the objective and surface were classified into.
    pub risk: TaskRisk,
    pub capability: CapabilityEvidenceProjection,
    pub budget: BudgetProjection,
    /// The safety rules in force. Identical for every profile; projected so an
    /// operator can see that for themselves rather than take it on faith.
    pub safety_floor: SafetyFloor,
    pub escalations: Vec<EscalationProjection>,
    pub cost: CostProjection,
    /// How many times in a row the surface presented the same actionable state.
    pub stationary_repeats: u32,
    /// Whether the profile's element ceiling bounded the view the model saw.
    pub observation_truncated: bool,
    /// Whether this profile additionally requires an independent verifier.
    pub requires_independent_verifier: bool,
    /// Where the run's adaptive lifecycle stands. Durable, so a record found
    /// `in_flight` after a restart is a turn that was cut, not a clean stop.
    pub lifecycle: AdaptiveLifecycle,
    /// The highest risk class this run has been authorized for. A later
    /// objective above this stops the run rather than reusing its authority.
    pub risk_high_water: TaskRisk,
    /// Compare-and-swap revision. A caller echoes this to start a turn.
    pub revision: u64,
    /// Present once the run has ended.
    pub terminal: Option<TerminalProjection>,
}

/// Derives the projection from the durable record. Pure: no clock, no ambient
/// state, and given the same record it always serializes identically.
pub fn project_adaptive(record: &AdaptiveRecord) -> AdaptiveProfileProjection {
    let decision = &record.decision;
    let profile = record.profile;
    // The reason shown is the one that most recently moved the run: the last
    // escalation if there was one, otherwise the original selection.
    let reason = record
        .escalations
        .last()
        .map(|entry| entry.reason)
        .unwrap_or(decision.reason);
    AdaptiveProfileProjection {
        profile,
        profile_display_name: profile.display_name().to_string(),
        reason,
        message: reason.operator_message().to_string(),
        risk: decision.risk,
        capability: CapabilityEvidenceProjection::of(&decision.evidence),
        budget: BudgetProjection::of(profile.budget()),
        safety_floor: profile.safety_floor(),
        escalations: record
            .escalations
            .iter()
            .map(EscalationProjection::of)
            .collect(),
        cost: CostProjection::of(record.cost),
        stationary_repeats: record.stationary_repeats,
        observation_truncated: record.observation_truncated,
        requires_independent_verifier: profile.requires_independent_verifier(),
        lifecycle: record.lifecycle,
        risk_high_water: record.risk_high_water,
        revision: record.revision,
        terminal: record.terminal.as_ref().map(TerminalProjection::of),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_profile::capability::{
        CapabilityGeneration, HostCapabilityEvidence, ModelCapabilityEvidence,
        OperatorCapabilityPolicy,
    };
    use crate::computer_profile::controller::AdaptiveController;
    use crate::computer_profile::policy::{AdaptivePolicyEngine, PolicyOutcome, RuntimeSignal};
    use crate::gateway_config::ModelCapabilities;

    fn generation() -> CapabilityGeneration {
        CapabilityGeneration::compute(
            "route-1",
            &ModelCapabilities::default(),
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        )
    }

    fn evidence(image: bool, verifier: bool) -> CapabilityEvidence {
        CapabilityEvidence::new(
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
                generation: generation(),
                declared_capability_trusted: false,
            },
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: image,
                independent_verifier: verifier,
            },
        )
    }

    fn record(image: bool, verifier: bool) -> AdaptiveRecord {
        let PolicyOutcome::Proceed(decision) =
            AdaptivePolicyEngine.select(&evidence(image, verifier), TaskRisk::Routine)
        else {
            panic!("routine work proceeds");
        };
        AdaptiveRecord::new(decision, generation())
    }

    #[test]
    fn the_projection_carries_no_observed_content_or_frame_digest() {
        let mut record = record(true, true);
        record.last_frame_digest = Some("d34dbeef".repeat(8));
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller
                .begin_turn(0, &generation(), TaskRisk::Routine)
                .unwrap();
            controller.record_attempt();
            controller.record_usage(Some(120), Some(8));
            controller.record_success(4_096, false);
        }
        let wire = serde_json::to_string(&project_adaptive(&record)).unwrap();
        for needle in ["d34dbeef", "lastFrameDigest", "PASSPHRASE", "com.example"] {
            assert!(
                !wire.contains(needle),
                "projection leaked {needle:?}: {wire}"
            );
        }
    }

    #[test]
    fn the_projection_never_emits_an_alias() {
        let mut record = record(true, true);
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller.apply_signal(RuntimeSignal::MissingSemantics);
            controller.apply_signal(RuntimeSignal::AmbiguousObservation);
        }
        let wire = serde_json::to_string(&project_adaptive(&record)).unwrap();
        assert!(!wire.contains("efficient"), "{wire}");
        assert!(!wire.contains("frontier"), "{wire}");
        assert!(wire.contains("high_assurance"), "{wire}");
    }

    #[test]
    fn unknown_cost_serializes_as_null_and_failures_are_still_counted() {
        let mut record = record(true, true);
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller
                .begin_turn(0, &generation(), TaskRisk::Routine)
                .unwrap();
            controller.record_attempt();
            controller.record_failure(2_048);
        }
        let value = serde_json::to_value(project_adaptive(&record)).unwrap();
        let cost = &value["cost"];
        assert!(cost["promptTokens"].is_null(), "{cost}");
        assert!(cost["completionTokens"].is_null(), "{cost}");
        assert_eq!(cost["providerAttempts"], 1);
        assert_eq!(cost["failedAttempts"], 1);
        assert_eq!(cost["acceptedAttempts"], 0);
        assert_eq!(cost["screenshotBytes"], 0);
        let wire = value.to_string();
        assert!(!wire.contains("costUsd"), "{wire}");
        assert!(!wire.contains("usd"), "{wire}");
    }

    #[test]
    fn escalation_history_and_final_stop_reason_are_both_readable() {
        let mut record = record(true, true);
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller.apply_signal(RuntimeSignal::AmbiguousObservation);
            controller.apply_signal(RuntimeSignal::MissingSemantics);
            controller.apply_signal(RuntimeSignal::RepeatedStationarity);
        }
        let projection = project_adaptive(&record);
        assert_eq!(projection.escalations.len(), 2);
        assert_eq!(projection.escalations[0].from, AdaptiveProfile::Economy);
        assert_eq!(projection.escalations[1].to, AdaptiveProfile::HighAssurance);
        let terminal = projection.terminal.expect("stopped at the ceiling");
        assert_eq!(terminal.reason, ProfileReason::RepeatedStationarity);
        assert!(!terminal.message.is_empty());
        assert_eq!(projection.lifecycle, AdaptiveLifecycle::Stopped);
    }

    #[test]
    fn the_projected_safety_floor_is_identical_in_every_profile() {
        let mut record = record(true, true);
        let economy = project_adaptive(&record).safety_floor;
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller.apply_signal(RuntimeSignal::AmbiguousObservation);
        }
        let balanced = project_adaptive(&record).safety_floor;
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller.apply_signal(RuntimeSignal::AmbiguousObservation);
        }
        let high = project_adaptive(&record).safety_floor;
        assert_eq!(economy, balanced);
        assert_eq!(balanced, high);
        assert_eq!(economy, SafetyFloor::REQUIRED);
    }

    #[test]
    fn the_capability_generation_is_projected_but_never_the_credential() {
        let record = record(true, true);
        let projection = project_adaptive(&record);
        assert_eq!(projection.capability.generation.len(), 12);
        assert!(!projection.capability.declared_capability_trusted);
        let wire = serde_json::to_string(&projection).unwrap();
        assert!(!wire.contains("cred-1"), "{wire}");
    }

    #[test]
    fn synthetic_only_qualification_is_visible_to_the_operator() {
        let evidence = CapabilityEvidence::new(
            ModelCapabilityEvidence {
                tools: true,
                image_input: false,
                max_image_bytes: None,
                tier: ComputerUseTier::SemanticAct,
                attribution: CapabilityAttribution::Measured,
                durable_authority: false,
                session_measured: true,
                synthetic_only: true,
                generation: generation(),
                declared_capability_trusted: false,
            },
            HostCapabilityEvidence::SEMANTIC_ONLY,
        );
        let PolicyOutcome::Proceed(decision) =
            AdaptivePolicyEngine.select(&evidence, TaskRisk::Routine)
        else {
            panic!("a session-qualified model may still do routine semantic work");
        };
        let record = AdaptiveRecord::new(decision, generation());
        let projection = project_adaptive(&record);
        assert!(projection.capability.synthetic_only);
        assert!(!projection.capability.durable_authority);
        assert_eq!(projection.capability.ceiling, AdaptiveProfile::Economy);
        assert_eq!(projection.reason, ProfileReason::TextOrientedModel);
    }

    #[test]
    fn projection_is_stable_for_the_same_record() {
        let record = record(true, true);
        let first = serde_json::to_string(&project_adaptive(&record)).unwrap();
        let second = serde_json::to_string(&project_adaptive(&record)).unwrap();
        assert_eq!(first, second);
    }
}
