//! Redaction-safe projection of adaptive state.
//!
//! This projection is suitable for the desktop cockpit and coordinator
//! surfaces. It contains no observation labels, values, geometry, frame
//! digests, credentials, paths, raw policy documents, or provider diagnostics.

use serde::{Deserialize, Serialize};

use super::capability::{CapabilityAttribution, CapabilityEvidence};
use super::controller::{AdaptiveController, CostLedger, EscalationRecord, TerminalOutcome};
use super::policy::ProfileReason;
use super::profile::{AdaptiveProfile, ObservationDetail, ProfileBudget, SafetyFloor};
use super::risk::TaskRisk;
use crate::gateway_config::ComputerUseTier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEvidenceProjection {
    pub tier: ComputerUseTier,
    pub attribution: CapabilityAttribution,
    pub structured_tools: bool,
    pub image_input: bool,
    pub qualified_visual_path: bool,
    pub durable_authority: bool,
    pub session_measured: bool,
    pub synthetic_only: bool,
    pub host_screenshot_capture: bool,
    pub host_independent_verifier: bool,
    pub host_isolated_guest: bool,
    pub ceiling: AdaptiveProfile,
    /// Opaque reference from the canonical #458 capability generation.
    pub capability_snapshot_reference: Option<String>,
}

impl CapabilityEvidenceProjection {
    fn from(evidence: &CapabilityEvidence) -> Self {
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
            host_isolated_guest: evidence.host.isolated_guest,
            ceiling: evidence.ceiling(),
            capability_snapshot_reference: evidence.capability_snapshot_reference(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetProjection {
    pub observation_detail: ObservationDetail,
    pub max_observation_elements: u32,
    pub max_observation_bytes: u64,
    pub max_model_calls: u32,
    pub max_repairs: u32,
    pub max_turn_millis: u64,
    pub screenshot_capture_allowed: bool,
    pub pointer_fallback_allowed: bool,
    pub key_chord_allowed: bool,
}

impl BudgetProjection {
    const fn from(budget: ProfileBudget) -> Self {
        Self {
            observation_detail: budget.observation_detail,
            max_observation_elements: budget.max_observation_elements,
            max_observation_bytes: budget.max_observation_bytes,
            max_model_calls: budget.max_model_calls,
            max_repairs: budget.max_repairs,
            max_turn_millis: budget.max_turn_millis,
            screenshot_capture_allowed: budget.allows_screenshot_capture,
            pointer_fallback_allowed: budget.allows_pointer_fallback,
            key_chord_allowed: budget.allows_key_chord,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostProjection {
    pub model_calls: u32,
    pub observation_bytes: u64,
    pub screenshot_bytes: u64,
    pub provider_attempts: u32,
    pub provider_latency_millis: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

impl From<CostLedger> for CostProjection {
    fn from(cost: CostLedger) -> Self {
        Self {
            model_calls: cost.model_calls,
            observation_bytes: cost.observation_bytes,
            screenshot_bytes: cost.screenshot_bytes,
            provider_attempts: cost.provider_attempts,
            provider_latency_millis: cost.provider_latency_millis,
            prompt_tokens: cost.prompt_tokens,
            completion_tokens: cost.completion_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationProjection {
    pub from: AdaptiveProfile,
    pub to: AdaptiveProfile,
    pub reason: ProfileReason,
    pub message: String,
    pub revision: u64,
}

impl From<&EscalationRecord> for EscalationProjection {
    fn from(record: &EscalationRecord) -> Self {
        Self {
            from: record.from,
            to: record.to,
            reason: record.reason,
            message: record.reason.operator_message().into(),
            revision: record.revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalProjection {
    pub kind: super::controller::TerminalKind,
    pub reason: ProfileReason,
    pub message: String,
    pub profile: AdaptiveProfile,
    pub required_profile: Option<AdaptiveProfile>,
}

impl From<&TerminalOutcome> for TerminalProjection {
    fn from(outcome: &TerminalOutcome) -> Self {
        Self {
            kind: outcome.kind,
            reason: outcome.reason,
            message: outcome.reason.operator_message().into(),
            profile: outcome.profile,
            required_profile: outcome.required_profile,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveProfileProjection {
    pub profile: AdaptiveProfile,
    pub profile_display_name: String,
    pub reason: ProfileReason,
    pub message: String,
    pub risk: TaskRisk,
    pub capability: CapabilityEvidenceProjection,
    pub budget: BudgetProjection,
    pub safety_floor: SafetyFloor,
    pub escalations: Vec<EscalationProjection>,
    pub cost: CostProjection,
    pub stationary_repeats: u32,
    pub observation_truncated: bool,
    pub requires_independent_verifier: bool,
    pub revision: u64,
    pub terminal: Option<TerminalProjection>,
}

pub fn project_adaptive(controller: &AdaptiveController) -> AdaptiveProfileProjection {
    let state = controller.state();
    let reason = state
        .escalations
        .last()
        .map_or(state.decision_reason, |record| record.reason);
    AdaptiveProfileProjection {
        profile: state.profile,
        profile_display_name: state.profile.display_name().into(),
        reason,
        message: reason.operator_message().into(),
        risk: state.risk,
        capability: CapabilityEvidenceProjection::from(&state.evidence),
        budget: BudgetProjection::from(state.profile.budget()),
        safety_floor: state.profile.safety_floor(),
        escalations: state
            .escalations
            .iter()
            .map(EscalationProjection::from)
            .collect(),
        cost: state.cost.clone().into(),
        stationary_repeats: state.stationary_repeats,
        observation_truncated: state.observation_truncated,
        requires_independent_verifier: state.profile.requires_independent_verifier(),
        revision: state.revision,
        terminal: state.terminal.as_ref().map(TerminalProjection::from),
    }
}

