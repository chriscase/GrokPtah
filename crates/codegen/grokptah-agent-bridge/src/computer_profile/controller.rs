//! Transitions over the durable adaptive record.
//!
//! Everything here borrows an [`AdaptiveRecord`] and mutates it in place. There
//! is no owned state, no map, no cache: the record lives on the Computer Run,
//! and the service persists it. That is what makes the operator's account of a
//! run survive a crash, and what stops a second run in the same session from
//! inheriting the first run's profile, spend, or escalation history.
//!
//! # Admission is where authority is rechecked, not where it is assumed
//!
//! [`AdaptiveController::begin_turn`] is the only door into a model call, and
//! it re-derives three things every single time:
//!
//! 1. the compare-and-swap revision, so a stale or duplicate caller cannot act;
//! 2. the **capability generation** (#458), so a same-route tier downgrade,
//!    provenance change, schema drift, credential rotation, or operator policy
//!    edit stops the run instead of reusing authority granted under other
//!    facts;
//! 3. the **task risk**, so a later, more consequential objective in the same
//!    run cannot quietly ride the authorization the run got for routine work.
//!
//! None of those is cached from the decision. A record that was correct one
//! turn ago is not evidence that it is correct now, which is precisely the
//! defect this rewrite exists to remove.
//!
//! # Nothing observed leaves this type
//!
//! Stationarity is tracked as an opaque digest of *structure*: element
//! identity, role, enabled/focused state, and hashes of label and value. The
//! digest is stored, never projected, never logged, and never sent to a model.
//! What an operator sees is a repeat count.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::capability::CapabilityGeneration;
use super::policy::{
    AdaptivePolicyEngine, PolicyStop, ProfileReason, ProfileTransition, RuntimeSignal,
};
use super::profile::{AdaptiveProfile, ProfileBudget, SafetyFloor};
use super::record::{AdaptiveLifecycle, AdaptiveRecord, EscalationRecord, TerminalOutcome};
use super::risk::TaskRisk;
use crate::computer_use::ComputerObservation;

/// Why a turn was refused before any provider attempt happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ControllerError {
    /// The caller's revision is not the record's. A stale or duplicate
    /// request; nothing was spent.
    RevisionConflict { expected: u64, actual: u64 },
    /// A turn is already in flight for this run.
    TurnInFlight,
    /// The run already ended.
    Terminated { reason: ProfileReason },
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                f,
                "the Computer Run advanced while this request was preparing (expected revision {expected}, found {actual})"
            ),
            Self::TurnInFlight => {
                f.write_str("a Computer model request is already in flight for this run")
            }
            Self::Terminated { reason } => f.write_str(reason.operator_message()),
        }
    }
}

impl std::error::Error for ControllerError {}

/// An opaque structural digest of one observation.
///
/// Two frames with the same digest present the same actionable surface even
/// though their observation ids differ. Label and value are hashed rather than
/// absorbed, so a long document body costs the same as an empty one and
/// nothing reconstructible was ever in the digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationFingerprint(String);

impl ObservationFingerprint {
    pub fn of(observation: &ComputerObservation) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"grokptah.cu.fingerprint.v1");
        hasher.update(observation.target.app_id.as_bytes());
        hasher.update([0]);
        hasher.update(observation.target.window_id.as_bytes());
        hasher.update([0]);
        hasher.update(observation.target.generation.to_le_bytes());
        // Elements arrive host-ordered; sort by id so a backend that reorders a
        // stable surface does not read as movement.
        let mut sorted: Vec<&crate::computer_use::SemanticElement> =
            observation.elements.iter().collect();
        sorted.sort_by(|left, right| left.element_id.cmp(&right.element_id));
        for element in sorted {
            hasher.update(element.element_id.as_bytes());
            hasher.update([0]);
            hasher.update(element.role.as_bytes());
            hasher.update([0]);
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
        Self(format!("{:x}", hasher.finalize()))
    }
}

/// A turn admitted by the controller.
///
/// Carries the profile and budget in force so a caller cannot render an
/// observation against a budget the run has since escalated past, and the
/// revision that admitted it so the outcome can be applied under
/// compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnPermit {
    pub profile: AdaptiveProfile,
    pub budget: ProfileBudget,
    pub revision: u64,
    /// Opaque identity of the admitted turn, minted by [`AdaptiveController::begin_turn`]
    /// and written to the durable record as `active_permit`.
    ///
    /// A sealed proposal binds this, so authority granted for one turn cannot
    /// be spent on another. Holding the value is not itself an authority: it
    /// means something only while it still equals the live record's
    /// `active_permit`, which nothing outside `begin_turn` can write.
    permit_id: String,
    /// The capability generation in force when the turn was admitted. Bound
    /// into the seal so a same-route tier downgrade, credential rotation,
    /// schema drift, or operator policy edit *during inference* is refused
    /// when the answer comes back (#458).
    generation: CapabilityGeneration,
}

impl TurnPermit {
    /// A permit bound to no durable run.
    ///
    /// The opt-in live provider proof exercises the provider round-trip against
    /// the deterministic simulator and reaches no Computer Run, so there is no
    /// record to admit a turn against. This carries a profile and a budget,
    /// which is all that is needed to *bound what is sent*, and it grants no
    /// authority whatsoever: its `permit_id` matches no record's
    /// `active_permit` and its generation is
    /// [`CapabilityGeneration::unbound`], which no computed generation can
    /// equal. `ModelProposalContext::from_run_with_permit` therefore refuses
    /// it, so nothing produced under it can be applied to a run.
    pub fn unbound(profile: AdaptiveProfile) -> Self {
        Self {
            profile,
            budget: profile.budget(),
            revision: 0,
            permit_id: "unbound".to_string(),
            generation: CapabilityGeneration::unbound(),
        }
    }

    pub fn permit_id(&self) -> &str {
        &self.permit_id
    }

    pub fn generation(&self) -> &CapabilityGeneration {
        &self.generation
    }

    /// The wall-clock budget for this turn, as a duration the caller can hand
    /// straight to a timeout. Advertising `maxTurnMillis` and not enforcing it
    /// would be a claim rather than a control, so this is the only way the
    /// number is read.
    pub fn turn_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.budget.max_turn_millis)
    }
}

/// Borrowed view over a run's durable adaptive record.
#[derive(Debug)]
pub struct AdaptiveController<'a> {
    record: &'a mut AdaptiveRecord,
}

impl<'a> AdaptiveController<'a> {
    pub fn new(record: &'a mut AdaptiveRecord) -> Self {
        Self { record }
    }

    pub fn record(&self) -> &AdaptiveRecord {
        self.record
    }

    pub fn profile(&self) -> AdaptiveProfile {
        self.record.profile
    }

    pub fn budget(&self) -> ProfileBudget {
        self.record.profile.budget()
    }

    pub fn safety_floor(&self) -> SafetyFloor {
        self.record.profile.safety_floor()
    }

    /// Admits one turn, or refuses. A refusal spends nothing.
    ///
    /// `generation` and `risk` are the values the host just re-derived from the
    /// live route and the current objective — not the ones stored on the
    /// record. Comparing the two is the whole point.
    pub fn begin_turn(
        &mut self,
        expected_revision: u64,
        generation: &CapabilityGeneration,
        risk: TaskRisk,
    ) -> Result<TurnPermit, ControllerError> {
        if let Some(terminal) = &self.record.terminal {
            return Err(ControllerError::Terminated {
                reason: terminal.reason,
            });
        }
        if expected_revision != self.record.revision {
            return Err(ControllerError::RevisionConflict {
                expected: expected_revision,
                actual: self.record.revision,
            });
        }
        if self.record.lifecycle == AdaptiveLifecycle::InFlight {
            return Err(ControllerError::TurnInFlight);
        }
        // (#458) The authority this record was opened under must still be the
        // authority in force. Anything else is a downgrade until proven
        // otherwise, and a downgrade is never a reuse.
        if &self.record.generation != generation {
            let stop = self.stop(RuntimeSignal::CapabilityGenerationChanged);
            return Err(ControllerError::Terminated {
                reason: stop.reason,
            });
        }
        // A later objective may be more consequential than the one this run was
        // authorized for. Serving it under the old decision would be exactly
        // the "reuse occupied state" defect.
        if risk > self.record.risk_high_water {
            let stop = self.stop(RuntimeSignal::HigherRiskObjective);
            return Err(ControllerError::Terminated {
                reason: stop.reason,
            });
        }
        if self.record.cost.provider_attempts >= self.budget().max_model_calls {
            let stop = self.stop(RuntimeSignal::BudgetExhausted);
            return Err(ControllerError::Terminated {
                reason: stop.reason,
            });
        }
        // Mint the turn's opaque identity and write it to the durable record.
        // Everything that ends or moves the run clears or replaces it, so a
        // proposal sealed against this permit stops being applicable the
        // moment the run is no longer in the state that admitted it.
        let permit_id = format!("permit-{}", uuid::Uuid::new_v4());
        self.record.active_permit = Some(permit_id.clone());
        self.record.lifecycle = AdaptiveLifecycle::InFlight;
        Ok(TurnPermit {
            profile: self.record.profile,
            budget: self.budget(),
            revision: self.record.revision,
            permit_id,
            generation: self.record.generation.clone(),
        })
    }

    /// Releases an admitted turn without advancing the run. Used when a turn
    /// failed for a reason the adaptive layer does not own — an operator
    /// cancellation — so a retry is not blocked by a phantom in-flight flag.
    pub fn abort_turn(&mut self) {
        if self.record.lifecycle == AdaptiveLifecycle::InFlight {
            self.record.lifecycle = AdaptiveLifecycle::Idle;
        }
    }

    /// Counts one provider attempt. Called **before** the request leaves, so
    /// an attempt that times out, dies in transport, returns prose, or fails
    /// schema validation still counts against the budget — because it still
    /// cost money and still consumed the run's allowance.
    pub fn record_attempt(&mut self) {
        self.record.cost.provider_attempts = self.record.cost.provider_attempts.saturating_add(1);
        self.record.bump();
    }

    /// Records provider-reported usage regardless of whether the response was
    /// usable. A body that arrived and then failed to parse was still billed.
    pub fn record_usage(&mut self, prompt_tokens: Option<u64>, completion_tokens: Option<u64>) {
        self.record.cost.add_usage(prompt_tokens, completion_tokens);
    }

    /// Records a usable proposal and closes the turn.
    pub fn record_success(&mut self, observation_bytes: u64, truncated: bool) {
        self.record.cost.accepted_attempts = self.record.cost.accepted_attempts.saturating_add(1);
        self.record.cost.observation_bytes = self
            .record
            .cost
            .observation_bytes
            .saturating_add(observation_bytes);
        self.record.observation_truncated = truncated;
        self.record.uncertain_streak = 0;
        self.record.lifecycle = AdaptiveLifecycle::Idle;
        self.record.bump();
    }

    /// Records an unusable answer and closes the turn, reporting the signal
    /// once consecutive unusable answers reach the floor's tolerance.
    pub fn record_failure(&mut self, observation_bytes: u64) -> Option<RuntimeSignal> {
        self.record.cost.failed_attempts = self.record.cost.failed_attempts.saturating_add(1);
        self.record.cost.observation_bytes = self
            .record
            .cost
            .observation_bytes
            .saturating_add(observation_bytes);
        self.record.uncertain_streak = self.record.uncertain_streak.saturating_add(1);
        self.record.lifecycle = AdaptiveLifecycle::Idle;
        self.record.bump();
        (self.record.uncertain_streak >= self.safety_floor().max_consecutive_uncertain_answers)
            .then_some(RuntimeSignal::RepeatedUncertainty)
    }

    /// Feeds one frame into the stationarity window.
    ///
    /// Returns [`RuntimeSignal::RepeatedStationarity`] once the same structural
    /// digest has arrived [`SafetyFloor::max_stationary_repeats`] times in a
    /// row. Observing alone never changes the profile; the caller decides by
    /// passing the signal to [`Self::apply_signal`].
    pub fn observe_frame(&mut self, fingerprint: &ObservationFingerprint) -> Option<RuntimeSignal> {
        let floor = self.safety_floor();
        if self.record.last_frame_digest.as_deref() == Some(fingerprint.0.as_str()) {
            self.record.stationary_repeats = self.record.stationary_repeats.saturating_add(1);
        } else {
            self.record.stationary_repeats = 0;
        }
        self.record.last_frame_digest = Some(fingerprint.0.clone());
        (self.record.stationary_repeats >= floor.max_stationary_repeats)
            .then_some(RuntimeSignal::RepeatedStationarity)
    }

    /// Records a dispatch whose postcondition the host could not verify on the
    /// verifying frame. The first failure may escalate; the second is terminal,
    /// because re-running a move that already failed twice is the blind-retry
    /// loop this layer exists to prevent.
    pub fn record_verification_failure(&mut self) -> RuntimeSignal {
        self.record.verification_failures = self.record.verification_failures.saturating_add(1);
        self.record.bump();
        if self.record.verification_failures >= self.safety_floor().max_verification_failures {
            RuntimeSignal::VerificationExhausted
        } else {
            RuntimeSignal::VerificationFailed
        }
    }

    /// Applies a runtime signal through the policy engine, mutating the record.
    ///
    /// This is the only method that changes the profile, and it always records
    /// what happened: an escalation appends to the durable history, a stop
    /// terminates the run. Both advance the revision so anything in flight is
    /// invalidated.
    pub fn apply_signal(&mut self, signal: RuntimeSignal) -> ProfileTransition {
        if let Some(terminal) = &self.record.terminal {
            return ProfileTransition::Stop(PolicyStop {
                reason: terminal.reason,
                profile: terminal.profile,
                required_profile: terminal.required_profile,
                ceiling: self.record.decision.ceiling,
            });
        }
        let transition = AdaptivePolicyEngine.reassess(
            self.record.profile,
            &self.record.decision.evidence,
            signal,
        );
        self.record.bump();
        match &transition {
            ProfileTransition::Escalate { from, to, reason } => {
                self.record.profile = *to;
                self.record.reset_for_new_profile();
                self.record.lifecycle = AdaptiveLifecycle::Idle;
                // The turn in flight was admitted under the old profile, so
                // its answer may not be applied under the new one.
                self.record.active_permit = None;
                let revision = self.record.revision;
                self.record.push_escalation(EscalationRecord {
                    from: *from,
                    to: *to,
                    reason: *reason,
                    revision,
                });
            }
            ProfileTransition::Stop(stop) => {
                self.record.lifecycle = AdaptiveLifecycle::Stopped;
                self.record.active_permit = None;
                self.record.terminal = Some(TerminalOutcome {
                    lifecycle: AdaptiveLifecycle::Stopped,
                    reason: stop.reason,
                    profile: stop.profile,
                    required_profile: stop.required_profile,
                });
            }
        }
        transition
    }

    /// Terminates the run with a stop derived from `signal`.
    pub fn stop(&mut self, signal: RuntimeSignal) -> PolicyStop {
        match self.apply_signal(signal) {
            ProfileTransition::Stop(stop) => stop,
            ProfileTransition::Escalate { .. } => {
                // `signal` was escalatable but the caller asked to stop. Honor
                // the caller and record the stop at the escalated profile.
                let stop = PolicyStop {
                    reason: ProfileReason::EscalationCeilingReached,
                    profile: self.record.profile,
                    required_profile: None,
                    ceiling: self.record.decision.ceiling,
                };
                self.record.lifecycle = AdaptiveLifecycle::Stopped;
                self.record.active_permit = None;
                self.record.terminal = Some(TerminalOutcome {
                    lifecycle: AdaptiveLifecycle::Stopped,
                    reason: stop.reason,
                    profile: stop.profile,
                    required_profile: None,
                });
                stop
            }
        }
    }

    /// Marks the run completed. Callers must already hold the host-issued
    /// completion proof; the controller records the outcome, it does not
    /// decide it.
    pub fn record_completed(&mut self) {
        if self.record.terminal.is_some() {
            return;
        }
        self.record.lifecycle = AdaptiveLifecycle::Completed;
        self.record.active_permit = None;
        self.record.bump();
        self.record.terminal = Some(TerminalOutcome {
            lifecycle: AdaptiveLifecycle::Completed,
            reason: ProfileReason::RoutineTask,
            profile: self.record.profile,
            required_profile: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Utc;

    use super::*;
    use crate::computer_profile::capability::{
        CapabilityAttribution, CapabilityEvidence, HostCapabilityEvidence, ModelCapabilityEvidence,
        OperatorCapabilityPolicy,
    };
    use crate::computer_profile::policy::PolicyOutcome;
    use crate::computer_use::{
        ComputerTarget, ObservationGeometry, SemanticAction, SemanticElement, Sensitivity,
    };
    use crate::gateway_config::{CapabilitySource, ComputerUseTier, ModelCapabilities};

    fn capabilities(image: bool) -> ModelCapabilities {
        ModelCapabilities {
            tools: true,
            image_input: image,
            max_image_bytes: image.then_some(4 * 1024 * 1024),
            computer_use_tier: if image {
                ComputerUseTier::VisualFallbackAct
            } else {
                ComputerUseTier::SemanticAct
            },
            computer_capability_source: CapabilitySource::Measured,
            ..Default::default()
        }
    }

    fn generation(image: bool) -> CapabilityGeneration {
        CapabilityGeneration::compute(
            "route-1",
            &capabilities(image),
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
                generation: generation(image),
                declared_capability_trusted: false,
            },
            HostCapabilityEvidence {
                semantic_observation: true,
                screenshot_capture: image,
                independent_verifier: verifier,
            },
        )
    }

    fn record(image: bool, verifier: bool, risk: TaskRisk) -> AdaptiveRecord {
        let PolicyOutcome::Proceed(decision) =
            AdaptivePolicyEngine.select(&evidence(image, verifier), risk)
        else {
            panic!("expected the policy to proceed");
        };
        AdaptiveRecord::new(decision, generation(image))
    }

    fn observation(value: &str) -> ComputerObservation {
        ComputerObservation {
            observation_id: format!("obs-{value}"),
            sequence: 1,
            target: ComputerTarget {
                app_id: "com.example.demo".into(),
                window_id: "w1".into(),
                generation: 1,
                display_name: "Demo".into(),
                sensitivity: Sensitivity::None,
            },
            captured_at: Utc::now(),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 2.0,
            },
            screenshot: None,
            elements: vec![SemanticElement {
                element_id: "field".into(),
                role: "text_field".into(),
                label: Some("Name".into()),
                value: Some(value.into()),
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
    fn a_changed_capability_generation_stops_the_run_before_any_attempt() {
        let mut record = record(true, true, TaskRisk::Routine);
        let mut controller = AdaptiveController::new(&mut record);
        // Same route, downgraded tier: the digest moves even though the
        // endpoint, model, and dialect are untouched.
        let downgraded = CapabilityGeneration::compute(
            "route-1",
            &ModelCapabilities {
                computer_use_tier: ComputerUseTier::Observe,
                ..capabilities(true)
            },
            "cred-1",
            &OperatorCapabilityPolicy::default(),
        );
        let error = controller
            .begin_turn(0, &downgraded, TaskRisk::Routine)
            .unwrap_err();
        assert_eq!(
            error,
            ControllerError::Terminated {
                reason: ProfileReason::CapabilityGenerationChanged
            }
        );
        assert_eq!(record.cost.provider_attempts, 0, "nothing was spent");
        assert_eq!(record.lifecycle, AdaptiveLifecycle::Stopped);
    }

    #[test]
    fn credential_rotation_alone_changes_the_generation() {
        let before = generation(true);
        let after = CapabilityGeneration::compute(
            "route-1",
            &capabilities(true),
            "cred-2",
            &OperatorCapabilityPolicy::default(),
        );
        assert_ne!(before, after);
    }

    #[test]
    fn an_operator_policy_edit_alone_changes_the_generation() {
        let before = generation(true);
        let after = CapabilityGeneration::compute(
            "route-1",
            &capabilities(true),
            "cred-1",
            &OperatorCapabilityPolicy {
                trust_declared_capability: true,
                policy_generation: "operator/v2".into(),
            },
        );
        assert_ne!(before, after);
    }

    #[test]
    fn a_later_higher_risk_objective_stops_rather_than_reusing_authority() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller
                .begin_turn(0, &generation, TaskRisk::Routine)
                .unwrap();
            controller.record_attempt();
            controller.record_success(1_024, false);
        }
        let revision = record.revision;
        let mut controller = AdaptiveController::new(&mut record);
        let error = controller
            .begin_turn(revision, &generation, TaskRisk::Destructive)
            .unwrap_err();
        assert_eq!(
            error,
            ControllerError::Terminated {
                reason: ProfileReason::HigherRiskObjective
            }
        );
        assert_eq!(record.lifecycle, AdaptiveLifecycle::Stopped);
    }

    #[test]
    fn a_lower_risk_follow_up_is_allowed_under_the_high_water_mark() {
        let mut record = record(true, true, TaskRisk::Consequential);
        let generation = generation(true);
        let mut controller = AdaptiveController::new(&mut record);
        assert!(controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .is_ok());
    }

    #[test]
    fn every_attempt_counts_including_the_ones_that_failed() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let mut controller = AdaptiveController::new(&mut record);
        controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .unwrap();
        controller.record_attempt();
        // A body arrived, reported usage, and then failed to parse.
        controller.record_usage(Some(400), Some(12));
        controller.record_failure(2_048);
        assert_eq!(record.cost.provider_attempts, 1);
        assert_eq!(record.cost.failed_attempts, 1);
        assert_eq!(record.cost.accepted_attempts, 0);
        assert_eq!(
            record.cost.prompt_tokens,
            Some(400),
            "usage survives a parse failure"
        );
        assert_eq!(record.cost.completion_tokens, Some(12));
        assert_eq!(
            record.cost.observation_bytes, 2_048,
            "a failed attempt still sent an observation"
        );
    }

    #[test]
    fn a_failed_attempt_consumes_the_model_call_budget() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let budget = record.profile.budget().max_model_calls;
        for _ in 0..budget {
            let revision = record.revision;
            let mut controller = AdaptiveController::new(&mut record);
            controller
                .begin_turn(revision, &generation, TaskRisk::Routine)
                .unwrap();
            controller.record_attempt();
            controller.record_failure(16);
            // Clear the uncertainty streak so budget exhaustion is what ends
            // this run rather than repeated uncertainty.
            record.uncertain_streak = 0;
        }
        let revision = record.revision;
        let mut controller = AdaptiveController::new(&mut record);
        let error = controller
            .begin_turn(revision, &generation, TaskRisk::Routine)
            .unwrap_err();
        assert_eq!(
            error,
            ControllerError::Terminated {
                reason: ProfileReason::BudgetExhausted
            }
        );
        assert_eq!(record.cost.provider_attempts, budget);
        assert_eq!(record.cost.failed_attempts, budget);
    }

    /// The permit is opaque and unique per admitted turn, and the durable
    /// record agrees with it. That pairing is what makes a seal bound to one
    /// turn unusable on another.
    #[test]
    fn each_admitted_turn_mints_a_distinct_permit_the_record_agrees_with() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let first = {
            let mut controller = AdaptiveController::new(&mut record);
            let permit = controller
                .begin_turn(0, &generation, TaskRisk::Routine)
                .unwrap();
            assert_eq!(
                controller.record().active_permit.as_deref(),
                Some(permit.permit_id())
            );
            assert_eq!(permit.generation(), &generation);
            permit.permit_id().to_string()
        };
        let mut controller = AdaptiveController::new(&mut record);
        controller.record_success(128, false);
        let revision = controller.record().revision;
        let second = controller
            .begin_turn(revision, &generation, TaskRisk::Routine)
            .unwrap();
        assert_ne!(
            second.permit_id(),
            first,
            "a second turn must not reuse the first turn's authority"
        );
        assert_eq!(
            controller.record().active_permit.as_deref(),
            Some(second.permit_id())
        );
    }

    /// Anything that ends or moves the run withdraws the admitted turn, so an
    /// answer that arrives afterwards has nothing left to apply against.
    #[test]
    fn a_stop_withdraws_the_admitted_turn() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let mut controller = AdaptiveController::new(&mut record);
        controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .unwrap();
        assert!(controller.record().active_permit.is_some());
        controller.stop(RuntimeSignal::CapabilityRevoked);
        assert_eq!(
            controller.record().active_permit,
            None,
            "a stopped run holds no admitted turn"
        );
    }

    #[test]
    fn the_turn_permit_exposes_an_enforceable_timeout() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let mut controller = AdaptiveController::new(&mut record);
        let permit = controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .unwrap();
        assert_eq!(
            permit.turn_timeout(),
            std::time::Duration::from_millis(permit.budget.max_turn_millis)
        );
        assert!(permit.turn_timeout().as_millis() > 0);
    }

    #[test]
    fn a_stale_revision_is_refused_and_spends_nothing() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        {
            let mut controller = AdaptiveController::new(&mut record);
            controller
                .begin_turn(0, &generation, TaskRisk::Routine)
                .unwrap();
            controller.record_attempt();
            controller.record_success(16, false);
        }
        let attempts = record.cost.provider_attempts;
        let mut controller = AdaptiveController::new(&mut record);
        let error = controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .unwrap_err();
        assert!(matches!(error, ControllerError::RevisionConflict { .. }));
        assert_eq!(record.cost.provider_attempts, attempts);
    }

    #[test]
    fn one_turn_at_a_time() {
        let mut record = record(true, true, TaskRisk::Routine);
        let generation = generation(true);
        let mut controller = AdaptiveController::new(&mut record);
        controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .unwrap();
        assert_eq!(
            controller
                .begin_turn(0, &generation, TaskRisk::Routine)
                .unwrap_err(),
            ControllerError::TurnInFlight
        );
        controller.abort_turn();
        assert!(controller
            .begin_turn(0, &generation, TaskRisk::Routine)
            .is_ok());
    }

    #[test]
    fn an_unchanged_frame_reports_stationarity_at_the_floor() {
        let mut record = record(true, true, TaskRisk::Routine);
        let mut controller = AdaptiveController::new(&mut record);
        let floor = controller.safety_floor().max_stationary_repeats;
        let same = ObservationFingerprint::of(&observation("same"));
        assert_eq!(controller.observe_frame(&same), None);
        for repeat in 1..floor {
            assert_eq!(controller.observe_frame(&same), None, "repeat {repeat}");
        }
        assert_eq!(
            controller.observe_frame(&same),
            Some(RuntimeSignal::RepeatedStationarity)
        );
    }

    #[test]
    fn a_new_observation_id_alone_is_still_the_same_frame() {
        let mut first = observation("same");
        let mut second = observation("same");
        first.observation_id = "obs-1".into();
        first.sequence = 1;
        second.observation_id = "obs-2".into();
        second.sequence = 2;
        second.captured_at = first.captured_at + chrono::Duration::seconds(5);
        assert_eq!(
            ObservationFingerprint::of(&first),
            ObservationFingerprint::of(&second)
        );
    }

    #[test]
    fn escalation_is_recorded_durably_and_resets_the_window() {
        let mut record = record(true, true, TaskRisk::Routine);
        let mut controller = AdaptiveController::new(&mut record);
        let same = ObservationFingerprint::of(&observation("same"));
        controller.observe_frame(&same);
        controller.observe_frame(&same);
        let transition = controller.apply_signal(RuntimeSignal::RepeatedStationarity);
        assert_eq!(
            transition,
            ProfileTransition::Escalate {
                from: AdaptiveProfile::Economy,
                to: AdaptiveProfile::Balanced,
                reason: ProfileReason::RepeatedStationarity,
            }
        );
        assert_eq!(record.profile, AdaptiveProfile::Balanced);
        assert_eq!(record.stationary_repeats, 0);
        assert!(record.last_frame_digest.is_none());
        assert_eq!(record.escalations.len(), 1);
        assert_eq!(record.escalations[0].revision, record.revision);
    }

    #[test]
    fn a_capped_run_stops_instead_of_escalating() {
        let mut record = record(false, false, TaskRisk::Routine);
        assert_eq!(record.decision.ceiling, AdaptiveProfile::Economy);
        let mut controller = AdaptiveController::new(&mut record);
        let transition = controller.apply_signal(RuntimeSignal::AmbiguousObservation);
        assert!(matches!(transition, ProfileTransition::Stop(_)));
        let terminal = record.terminal.as_ref().expect("stopped");
        assert_eq!(terminal.reason, ProfileReason::AmbiguousObservation);
        assert_eq!(terminal.required_profile, Some(AdaptiveProfile::Balanced));
    }

    #[test]
    fn a_second_verification_failure_is_terminal() {
        let mut record = record(true, true, TaskRisk::Routine);
        let mut controller = AdaptiveController::new(&mut record);
        assert_eq!(
            controller.record_verification_failure(),
            RuntimeSignal::VerificationFailed
        );
        assert_eq!(
            controller.record_verification_failure(),
            RuntimeSignal::VerificationExhausted
        );
        assert!(matches!(
            controller.apply_signal(RuntimeSignal::VerificationExhausted),
            ProfileTransition::Stop(_)
        ));
    }

    #[test]
    fn a_terminated_run_never_escalates_again() {
        let mut record = record(true, true, TaskRisk::Routine);
        let mut controller = AdaptiveController::new(&mut record);
        controller.apply_signal(RuntimeSignal::CapabilityRevoked);
        let profile = record.profile;
        let mut controller = AdaptiveController::new(&mut record);
        assert!(matches!(
            controller.apply_signal(RuntimeSignal::AmbiguousObservation),
            ProfileTransition::Stop(_)
        ));
        assert_eq!(record.profile, profile);
        assert!(record.escalations.is_empty());
    }
}
