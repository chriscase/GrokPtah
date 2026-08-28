//! Durable per-Computer-Run adaptive state.
//!
//! The controller is stateful, but it is not a second runtime or authority.
//! It records profile decisions, bounded transitions, stationarity, spend, and
//! terminal truth inside the existing Computer Run record. Each turn is
//! compare-and-swap admitted and restart recovery never replays work.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::capability::CapabilityEvidence;
use super::policy::{
    AdaptivePolicyEngine, PolicyStop, ProfileDecision, ProfileReason, ProfileTransition,
    RuntimeSignal,
};
use super::profile::{AdaptiveProfile, ProfileBudget, SafetyFloor};
use crate::computer_use::ComputerObservation;

pub const ADAPTIVE_STATE_SCHEMA_VERSION: u32 = 1;
const MAX_ESCALATIONS: usize = 32;
const MAX_OBSERVATION_DIGESTS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveSpend {
    pub model_calls: u32,
    pub observation_bytes: u64,
    pub screenshot_bytes: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub provider_attempts: u32,
    pub provider_latency_millis: u64,
}

impl Default for AdaptiveSpend {
    fn default() -> Self {
        Self {
            model_calls: 0,
            observation_bytes: 0,
            screenshot_bytes: 0,
            prompt_tokens: None,
            completion_tokens: None,
            provider_attempts: 0,
            provider_latency_millis: 0,
        }
    }
}

impl AdaptiveSpend {
    fn add_optional(total: &mut Option<u64>, value: Option<u64>) {
        if let Some(value) = value {
            *total = Some(total.unwrap_or(0).saturating_add(value));
        }
    }

    pub fn record_provider_attempt(
        &mut self,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        latency_millis: u64,
    ) {
        self.provider_attempts = self.provider_attempts.saturating_add(1);
        self.provider_latency_millis = self.provider_latency_millis.saturating_add(latency_millis);
        Self::add_optional(&mut self.prompt_tokens, prompt_tokens);
        Self::add_optional(&mut self.completion_tokens, completion_tokens);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRecord {
    pub from: AdaptiveProfile,
    pub to: AdaptiveProfile,
    pub reason: ProfileReason,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Completed,
    Stopped,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutcome {
    pub kind: TerminalKind,
    pub reason: ProfileReason,
    pub profile: AdaptiveProfile,
    pub required_profile: Option<AdaptiveProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveRunState {
    pub schema_version: u32,
    pub revision: u64,
    pub profile: AdaptiveProfile,
    pub decision_reason: ProfileReason,
    pub risk: super::risk::TaskRisk,
    pub capability_ceiling: AdaptiveProfile,
    pub capability_snapshot_reference: Option<String>,
    pub evidence: CapabilityEvidence,
    pub escalations: Vec<EscalationRecord>,
    pub terminal: Option<TerminalOutcome>,
    pub spend: AdaptiveSpend,
    pub stationary_repeats: u32,
    pub uncertain_streak: u32,
    pub verification_failures: u32,
    pub observation_truncated: bool,
    /// Process-private structural digests are retained for replay and
    /// stationarity checks but are never copied into public projections.
    pub(crate) observation_digests: Vec<String>,
    pub turn_in_flight: bool,
}

impl AdaptiveRunState {
    pub fn validate(&self) -> bool {
        self.schema_version == ADAPTIVE_STATE_SCHEMA_VERSION
            && self.capability_ceiling >= self.profile
            && self.escalations.len() <= MAX_ESCALATIONS
            && self.observation_digests.len() <= MAX_OBSERVATION_DIGESTS
            && self.observation_digests.iter().all(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPermit {
    pub profile: AdaptiveProfile,
    pub budget: ProfileBudget,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerError {
    RevisionConflict { expected: u64, actual: u64 },
    TurnInFlight,
    Terminated { reason: ProfileReason },
    InvalidState,
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "adaptive run revision conflict: expected {expected}, current {actual}"
            ),
            Self::TurnInFlight => {
                formatter.write_str("an adaptive model turn is already in flight")
            }
            Self::Terminated { reason } => formatter.write_str(reason.operator_message()),
            Self::InvalidState => formatter.write_str("adaptive run state is invalid"),
        }
    }
}

impl std::error::Error for ControllerError {}

/// Opaque structural observation fingerprint. Its bytes have no public
/// accessor, preventing frame content from becoming a projection or log value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationFingerprint([u8; 32]);

impl ObservationFingerprint {
    pub fn of(observation: &ComputerObservation) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"grokptah.computer-use.observation.v1\0");
        hasher.update(observation.target.app_id.as_bytes());
        hasher.update([0]);
        hasher.update(observation.target.window_id.as_bytes());
        hasher.update(observation.target.generation.to_le_bytes());
        let mut elements: Vec<_> = observation.elements.iter().collect();
        elements.sort_by(|left, right| left.element_id.cmp(&right.element_id));
        for element in elements {
            hasher.update(element.element_id.as_bytes());
            hasher.update([0]);
            hasher.update(element.role.as_bytes());
            hasher.update([u8::from(element.enabled), u8::from(element.focused)]);
            hasher.update(Sha256::digest(
                element.label.as_deref().unwrap_or_default().as_bytes(),
            ));
            hasher.update(Sha256::digest(
                element.value.as_deref().unwrap_or_default().as_bytes(),
            ));
            for action in &element.actions {
                hasher.update(format!("{action:?}").as_bytes());
                hasher.update([0]);
            }
            hasher.update([1]);
        }
        Self(hasher.finalize().into())
    }

    fn hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveController {
    state: AdaptiveRunState,
}

impl AdaptiveController {
    pub fn new(run_id: &str, decision: ProfileDecision) -> Self {
        let _ = run_id;
        Self {
            state: AdaptiveRunState {
                schema_version: ADAPTIVE_STATE_SCHEMA_VERSION,
                revision: 0,
                profile: decision.profile,
                decision_reason: decision.reason,
                risk: decision.risk,
                capability_ceiling: decision.ceiling,
                capability_snapshot_reference: decision.capability_snapshot_reference,
                evidence: decision.evidence,
                escalations: Vec::new(),
                terminal: None,
                spend: AdaptiveSpend::default(),
                stationary_repeats: 0,
                uncertain_streak: 0,
                verification_failures: 0,
                observation_truncated: false,
                observation_digests: Vec::new(),
                turn_in_flight: false,
            },
        }
    }

    pub fn stopped(
        run_id: &str,
        evidence: CapabilityEvidence,
        policy: super::policy::TaskPolicy,
        stop: PolicyStop,
    ) -> Self {
        let _ = run_id;
        let decision = ProfileDecision {
            profile: stop.profile,
            reason: stop.reason,
            risk: policy.risk,
            ceiling: stop.ceiling,
            capability_snapshot_reference: evidence.capability_snapshot_reference(),
            evidence,
        };
        let mut controller = Self::new(run_id, decision);
        controller.state.terminal = Some(TerminalOutcome {
            kind: TerminalKind::Stopped,
            reason: stop.reason,
            profile: stop.profile,
            required_profile: stop.required_profile,
        });
        controller
    }

    pub fn from_state(state: AdaptiveRunState) -> Result<Self, ControllerError> {
        if !state.validate() {
            return Err(ControllerError::InvalidState);
        }
        Ok(Self { state })
    }

    pub fn state(&self) -> &AdaptiveRunState {
        &self.state
    }

    pub fn into_state(self) -> AdaptiveRunState {
        self.state
    }

    pub fn profile(&self) -> AdaptiveProfile {
        self.state.profile
    }

    pub fn budget(&self) -> ProfileBudget {
        self.profile().budget()
    }

    pub fn safety_floor(&self) -> SafetyFloor {
        self.profile().safety_floor()
    }

    pub fn decision_reason(&self) -> ProfileReason {
        self.state.decision_reason
    }

    pub fn risk(&self) -> super::risk::TaskRisk {
        self.state.risk
    }

    pub fn evidence(&self) -> &CapabilityEvidence {
        &self.state.evidence
    }

    pub(crate) fn bind_authority(
        &mut self,
        authority: super::authority::AdaptiveAuthoritySnapshot,
    ) {
        self.state.evidence.bind_authority(authority);
    }

    pub fn revision(&self) -> u64 {
        self.state.revision
    }

    pub fn spend(&self) -> AdaptiveSpend {
        self.state.spend.clone()
    }

    pub fn escalations(&self) -> &[EscalationRecord] {
        &self.state.escalations
    }

    pub fn terminal(&self) -> Option<&TerminalOutcome> {
        self.state.terminal.as_ref()
    }

    pub fn begin_turn(&mut self, expected_revision: u64) -> Result<TurnPermit, ControllerError> {
        if let Some(terminal) = &self.state.terminal {
            return Err(ControllerError::Terminated {
                reason: terminal.reason,
            });
        }
        if self.state.revision != expected_revision {
            return Err(ControllerError::RevisionConflict {
                expected: expected_revision,
                actual: self.state.revision,
            });
        }
        if self.state.turn_in_flight {
            return Err(ControllerError::TurnInFlight);
        }
        if self.state.spend.model_calls >= self.budget().max_model_calls {
            let stop = self.apply_signal(RuntimeSignal::BudgetExhausted);
            return Err(ControllerError::Terminated {
                reason: match stop {
                    ProfileTransition::Stop(stop) => stop.reason,
                    ProfileTransition::Escalate { .. } => ProfileReason::BudgetExhausted,
                },
            });
        }
        self.state.turn_in_flight = true;
        Ok(TurnPermit {
            profile: self.profile(),
            budget: self.budget(),
            revision: self.revision(),
        })
    }

    pub fn abort_turn(&mut self, provider_attempted: bool) {
        self.state.turn_in_flight = false;
        if provider_attempted {
            self.state.spend.model_calls = self.state.spend.model_calls.saturating_add(1);
            self.state.revision = self.state.revision.saturating_add(1);
        }
    }

    pub fn finish_turn(
        &mut self,
        rendered_observation_bytes: u64,
        truncated: bool,
        receipt: Option<&super::authority::ProviderAttemptReceipt>,
    ) {
        self.state.turn_in_flight = false;
        self.state.spend.model_calls = self.state.spend.model_calls.saturating_add(1);
        self.state.spend.observation_bytes = self
            .state
            .spend
            .observation_bytes
            .saturating_add(rendered_observation_bytes);
        self.state.observation_truncated = truncated;
        if let Some(receipt) = receipt {
            self.state.spend.record_provider_attempt(
                receipt.prompt_tokens,
                receipt.completion_tokens,
                receipt.latency_millis,
            );
        }
        self.state.revision = self.state.revision.saturating_add(1);
    }

    pub fn observe_frame(&mut self, fingerprint: ObservationFingerprint) -> Option<RuntimeSignal> {
        let digest = fingerprint.hex();
        let repeated = self
            .state
            .observation_digests
            .last()
            .is_some_and(|previous| previous == &digest);
        if repeated {
            self.state.stationary_repeats = self.state.stationary_repeats.saturating_add(1);
        } else {
            self.state.stationary_repeats = 0;
        }
        self.state.observation_digests.push(digest);
        if self.state.observation_digests.len() > MAX_OBSERVATION_DIGESTS {
            self.state.observation_digests.remove(0);
        }
        (self.state.stationary_repeats >= self.safety_floor().max_stationary_repeats)
            .then_some(RuntimeSignal::RepeatedStationarity)
    }

    pub fn record_uncertain_answer(&mut self) -> Option<RuntimeSignal> {
        self.state.uncertain_streak = self.state.uncertain_streak.saturating_add(1);
        (self.state.uncertain_streak >= self.safety_floor().max_consecutive_uncertain_answers)
            .then_some(RuntimeSignal::RepeatedUncertainty)
    }

    pub fn record_usable_answer(&mut self) {
        self.state.uncertain_streak = 0;
    }

    pub fn record_verification_failure(&mut self) -> RuntimeSignal {
        self.state.verification_failures = self.state.verification_failures.saturating_add(1);
        if self.state.verification_failures >= self.safety_floor().max_verification_failures {
            RuntimeSignal::VerificationExhausted
        } else {
            RuntimeSignal::VerificationFailed
        }
    }

    pub fn apply_signal(&mut self, signal: RuntimeSignal) -> ProfileTransition {
        if let Some(terminal) = &self.state.terminal {
            return ProfileTransition::Stop(PolicyStop {
                reason: terminal.reason,
                profile: terminal.profile,
                required_profile: terminal.required_profile,
                ceiling: self.state.capability_ceiling,
            });
        }
        let evidence = &self.state.evidence;
        let transition = AdaptivePolicyEngine.reassess(self.state.profile, evidence, signal);
        self.state.revision = self.state.revision.saturating_add(1);
        match &transition {
            ProfileTransition::Escalate { from, to, reason } => {
                self.state.profile = *to;
                self.state.decision_reason = *reason;
                self.state.stationary_repeats = 0;
                self.state.uncertain_streak = 0;
                self.state.observation_truncated = false;
                self.state.observation_digests.clear();
                if self.state.escalations.len() >= MAX_ESCALATIONS {
                    self.state.escalations.remove(0);
                }
                self.state.escalations.push(EscalationRecord {
                    from: *from,
                    to: *to,
                    reason: *reason,
                    revision: self.state.revision,
                });
            }
            ProfileTransition::Stop(stop) => {
                self.state.turn_in_flight = false;
                self.state.terminal = Some(TerminalOutcome {
                    kind: TerminalKind::Stopped,
                    reason: stop.reason,
                    profile: stop.profile,
                    required_profile: stop.required_profile,
                });
            }
        }
        transition
    }

    pub fn record_completed(&mut self) -> Result<(), ControllerError> {
        if self.state.terminal.is_some() {
            return Err(ControllerError::Terminated {
                reason: self.state.terminal.as_ref().unwrap().reason,
            });
        }
        self.state.turn_in_flight = false;
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.terminal = Some(TerminalOutcome {
            kind: TerminalKind::Completed,
            reason: self.state.decision_reason,
            profile: self.state.profile,
            required_profile: None,
        });
        Ok(())
    }

    pub fn recover_interrupted(&mut self) {
        self.state.turn_in_flight = false;
        self.state.observation_digests.clear();
        self.state.revision = self.state.revision.saturating_add(1);
        if self.state.terminal.is_none() {
            self.state.terminal = Some(TerminalOutcome {
                kind: TerminalKind::Interrupted,
                reason: ProfileReason::CapabilityRevoked,
                profile: self.state.profile,
                required_profile: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_profile::capability::{
        CapabilityAttribution, HostCapabilityEvidence, ModelCapabilityEvidence,
    };
    use crate::computer_profile::policy::{PolicyOutcome, TaskPolicy};
    use crate::gateway_config::ComputerUseTier;

    fn controller() -> AdaptiveController {
        let evidence = CapabilityEvidence::new(
            ModelCapabilityEvidence {
                tools: true,
                image_input: true,
                max_image_bytes: Some(4096),
                tier: ComputerUseTier::VisualFallbackAct,
                attribution: CapabilityAttribution::Measured,
                durable_authority: true,
                session_measured: false,
                synthetic_only: false,
            },
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: true,
                independent_verifier: true,
                isolated_guest: true,
            },
        );
        let PolicyOutcome::Proceed(decision) = AdaptivePolicyEngine.select(
            &evidence,
            TaskPolicy {
                risk: super::super::risk::TaskRisk::Routine,
                minimum_profile: None,
            },
        ) else {
            panic!("fixture should proceed");
        };
        AdaptiveController::new("run", decision)
    }

    fn observation(value: &str) -> ComputerObservation {
        use crate::computer_use::{
            ComputerTarget, ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
        };
        use chrono::Utc;
        use std::collections::BTreeSet;
        ComputerObservation {
            observation_id: format!("obs-{value}"),
            sequence: 1,
            target: ComputerTarget {
                app_id: "com.example.app".into(),
                window_id: "window".into(),
                generation: 1,
                display_name: "App".into(),
                sensitivity: Sensitivity::None,
            },
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "field".into(),
                role: "text_field".into(),
                label: Some(value.into()),
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::SetValue]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    #[test]
    fn revision_and_turn_admission_are_cas_bound() {
        let mut controller = controller();
        let permit = controller.begin_turn(0).unwrap();
        assert_eq!(
            controller.begin_turn(permit.revision).unwrap_err(),
            ControllerError::TurnInFlight
        );
        controller.finish_turn(100, false, None);
        assert_eq!(
            controller.begin_turn(permit.revision).unwrap_err(),
            ControllerError::RevisionConflict {
                expected: 0,
                actual: 1
            }
        );
    }

    #[test]
    fn repeated_structural_frames_escalate_without_model_calls() {
        let mut controller = controller();
        let frame = ObservationFingerprint::of(&observation("same"));
        assert!(controller.observe_frame(frame).is_none());
        assert!(controller.observe_frame(frame).is_none());
        assert_eq!(
            controller.observe_frame(frame),
            Some(RuntimeSignal::RepeatedStationarity)
        );
        assert_eq!(
            controller.apply_signal(RuntimeSignal::RepeatedStationarity),
            ProfileTransition::Escalate {
                from: AdaptiveProfile::Economy,
                to: AdaptiveProfile::Balanced,
                reason: ProfileReason::RepeatedStationarity,
            }
        );
        assert_eq!(controller.spend().model_calls, 0);
    }

    #[test]
    fn restart_is_terminal_and_never_replays() {
        let mut controller = controller();
        let _permit = controller.begin_turn(0).unwrap();
        controller.recover_interrupted();
        assert_eq!(
            controller.terminal().map(|outcome| outcome.kind),
            Some(TerminalKind::Interrupted)
        );
        assert_eq!(controller.spend().model_calls, 0);
        assert!(controller.begin_turn(0).is_err());
    }

    #[test]
    fn malformed_state_is_rejected() {
        let mut state = controller().into_state();
        state.observation_digests.push("bad".into());
        assert_eq!(
            AdaptiveController::from_state(state).unwrap_err(),
            ControllerError::InvalidState
        );
    }
}
