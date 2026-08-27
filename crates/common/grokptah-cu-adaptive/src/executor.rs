//! The executor: admission, re-derivation, and disagreement.
//!
//! The planner proposes against the frame it saw. The executor decides against
//! the frame that is there now. Those are different frames often enough that
//! trusting the first is the whole class of bug this module exists to prevent
//! -- the window was rebound, the element became disabled, an operator took
//! over, thirty seconds passed while an approval prompt sat unanswered.
//!
//! So the executor does not check the planner's reasoning. It **re-derives**
//! its own disposition from the live frame, the live lease, and the same
//! thresholds, and then resolves the two conservatively via
//! [`Disposition::resolve`]. The planner's disposition is an input, not an
//! authority: a planner that says `Commit` cannot make the executor commit,
//! and a planner that says `Refuse` cannot be talked out of it.
//!
//! ## Order of checks
//!
//! The order is fixed and is part of the contract, because the first refusal
//! is the one a reviewer reads:
//!
//! 1. schema -- a malformed step is not a low-confidence step
//! 2. cancellation and lease -- who is driving
//! 3. frame -- epoch, identity, freshness
//! 4. grant -- is this family authorized at all
//! 5. grounding -- is the target what it claims to be, including hard denial
//! 6. gates -- does a human have to say yes first
//! 7. thresholds -- is anyone sure enough
//!
//! Steps 1 through 5 short-circuit: the first refusal is returned. Sensitivity
//! outranks confidence, freshness outranks grounding, and the grant outranks
//! all of them, so a run refused for `SensitiveSurface` is never reported as
//! merely unconfident.
//!
//! Steps 6 and 7 are *resolved* rather than short-circuited, because a gate
//! gates a commit and does not override a refusal. Returning the gate first
//! would put a step nobody has any confidence in in front of a person, and
//! would make the ladder non-monotone: an irreversible step at zero confidence
//! would come back as "ask someone" while the same step at slightly higher
//! confidence came back as "refuse".
//!
//! ## Disagreement
//!
//! Disagreement is recorded, not resolved by vote. [`Disagreement`] says which
//! side was stricter and how, the resolved disposition is always the stricter
//! of the two, and the benchmark's disagreement scenarios exist to check that
//! the conservative side wins even when the planner is the one being careful.

use serde::{Deserialize, Serialize};

use crate::cancel::CleanupLedger;
use crate::confidence::Disposition;
use crate::escalation::EscalationContext;
use crate::gates::{ApprovalDecision, GateSet, check_gates, gates_for};
use crate::grounding::{GroundingLevel, LiveElement, required_level, verify};
use crate::lease::{FrameToken, RunLease};
use crate::profile::ExecutionProfile;
use crate::schema::{PlanEnvelope, PlannedStep};
use crate::tier::ModelTier;
use crate::vocabulary::{ApprovalReason, DenyReason};

/// Everything the executor needs to decide one step.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionRequest<'a> {
    pub profile: &'a ExecutionProfile,
    pub tier: ModelTier,
    pub plan: &'a PlanEnvelope,
    pub plan_digest: &'a str,
    pub step: &'a PlannedStep,
    /// What the planner concluded, on the frame it saw.
    pub planner_disposition: Disposition,
    pub live_frame: &'a FrameToken,
    /// What the live frame says about the element the step names, if any.
    pub live_element: Option<&'a LiveElement>,
    pub lease: &'a RunLease,
    pub cleanup: &'a CleanupLedger,
    pub context: &'a EscalationContext,
    pub approval: Option<&'a ApprovalDecision>,
    pub now_millis: u64,
    /// Whether this step has already spent its one free look.
    pub already_disambiguated: bool,
}

/// How the two sides differed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisagreementKind {
    /// The planner wanted to act; the executor refused.
    ExecutorRefusedCommit,
    /// The planner wanted to act; the executor wants a human first.
    ExecutorGatedCommit,
    /// The planner wanted to act; the executor wants a stronger model.
    ExecutorEscalatedCommit,
    /// The planner wanted to act; the executor wants another look.
    ExecutorDisambiguatedCommit,
    /// The executor would have acted; the planner would not. The planner
    /// still wins, because conservative wins.
    PlannerMoreConservative,
    /// Same rung, different reason.
    SameRungDifferentReason,
}

/// One recorded disagreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Disagreement {
    pub kind: DisagreementKind,
    pub planner: Disposition,
    pub executor: Disposition,
}

/// The executor's answer for one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepVerdict {
    pub step_index: u32,
    pub plan_digest: String,
    pub planner: Disposition,
    pub executor: Disposition,
    /// Always the stricter of the two.
    pub resolved: Disposition,
    pub disagreement: Option<Disagreement>,
    /// Gates this step opened, whether or not they were answered.
    pub gates: Vec<ApprovalReason>,
    /// The grounding the profile demanded of this step.
    pub required_grounding: GroundingLevel,
}

impl StepVerdict {
    /// True when the step may be dispatched.
    #[must_use]
    pub fn commits(&self) -> bool {
        self.resolved.commits()
    }

    /// The refusal, if the resolved disposition is one.
    #[must_use]
    pub fn refusal(&self) -> Option<DenyReason> {
        match self.resolved {
            Disposition::Refuse { reason } => Some(reason),
            _ => None,
        }
    }
}

/// Decide one step against the live world.
///
/// Never returns an error: a refusal is a disposition, so every request
/// produces a verdict that can be recorded. That matters for the ledger --
/// a step that vanished because a function returned `Err` is a step the
/// receipt cannot account for.
#[must_use]
pub fn evaluate(request: &AdmissionRequest<'_>) -> StepVerdict {
    let executor = derive(request);
    let planner = request.planner_disposition;
    let resolved = planner.resolve(executor);
    let disagreement = classify(planner, executor);
    let gates = pending_gates(request);
    StepVerdict {
        step_index: request.step.index,
        plan_digest: request.plan_digest.to_string(),
        planner,
        executor,
        resolved,
        disagreement,
        gates: gates.into_iter().collect(),
        required_grounding: required_level(request.profile, request.step.intent.family()),
    }
}

/// The gates this step opens, from the live frame's sensitivity.
fn pending_gates(request: &AdmissionRequest<'_>) -> GateSet {
    gates_for(
        request.step,
        request.live_element.map(|element| element.sensitivity),
    )
}

fn refuse(reason: DenyReason) -> Disposition {
    Disposition::Refuse { reason }
}

/// The executor's own disposition, derived from the live world alone.
fn derive(request: &AdmissionRequest<'_>) -> Disposition {
    // 1. Schema. A step that does not parse is not a proposal.
    if let Err(reason) = request.plan.validate() {
        return refuse(reason);
    }
    if let Err(reason) = request.step.validate() {
        return refuse(reason);
    }
    if request.plan_digest.is_empty() {
        return refuse(DenyReason::SchemaViolation);
    }

    // 2. Who is driving.
    if let Err(reason) = request.cleanup.check_admits() {
        return refuse(reason);
    }
    if let Err(reason) = request.lease.check_agent_may_act(request.now_millis) {
        return refuse(reason);
    }
    if request.context.epoch != request.lease.epoch {
        return refuse(DenyReason::FrameEpochChanged);
    }

    // 3. Which frame.
    if let Err(reason) = request.plan.frame.admit(
        request.live_frame,
        request.lease,
        request.now_millis,
        request.profile.max_frame_age_millis,
    ) {
        return refuse(reason);
    }

    let family = request.step.intent.family();

    // 4. What the grant authorizes.
    if let Err(reason) = request.context.check_family(family) {
        return refuse(reason);
    }

    // 5. Whether the target is what it claims to be.
    if let Err(reason) = verify(
        request.profile,
        &request.tier.declared(),
        family,
        &request.step.grounding,
        request.live_element,
    ) {
        return refuse(reason);
    }

    // 6. Whether a human has to say yes.
    //
    // A gate gates a *commit*; it does not override a refusal. So the gate's
    // disposition is resolved against the threshold ladder's rather than
    // returned ahead of it: a step nobody has any confidence in is refused,
    // not put in front of a person. Returning early here would also make the
    // ladder non-monotone -- an irreversible step at zero confidence would
    // come back as "ask someone" while the same step at slightly higher
    // confidence came back as "refuse".
    let gates = pending_gates(request);
    let gate_disposition = match check_gates(
        &gates,
        request.plan_digest,
        request.step.index,
        request.lease.epoch,
        request.approval,
    ) {
        Ok(()) => Disposition::Commit,
        Err(DenyReason::ApprovalRequired) => Disposition::RequestApproval {
            // The lowest-ordinal open gate names the request, so two
            // executors looking at the same step ask for the same thing.
            reason: gates
                .iter()
                .copied()
                .next()
                .unwrap_or(ApprovalReason::LowConfidenceCommit),
        },
        Err(other) => refuse(other),
    };

    // 7. Whether anyone is sure enough.
    let decided = request.profile.thresholds.decide(
        &request.step.ambiguity,
        request.step.reversibility,
        request.already_disambiguated,
    );

    // An approval answers whatever request names it. The threshold ladder can
    // ask for a human on its own account (a below-floor commit the profile
    // allows a person to underwrite), and without this the request would be
    // re-raised forever: the answer would satisfy the step's gates and then
    // the same threshold would ask again. The answer still has to be bound to
    // this plan, this step, and this epoch, so nothing here weakens the gate
    // rules -- it reuses them.
    let decided = if let Disposition::RequestApproval { reason } = decided {
        let asked: GateSet = [reason].into_iter().collect();
        match check_gates(
            &asked,
            request.plan_digest,
            request.step.index,
            request.lease.epoch,
            request.approval,
        ) {
            Ok(()) => Disposition::Commit,
            Err(DenyReason::ApprovalDenied) => refuse(DenyReason::ApprovalDenied),
            Err(_) => decided,
        }
    } else {
        decided
    };

    decided.resolve(gate_disposition)
}

fn classify(planner: Disposition, executor: Disposition) -> Option<Disagreement> {
    if planner == executor {
        return None;
    }
    let kind = match (planner, executor) {
        (Disposition::Commit, Disposition::Refuse { .. }) => {
            DisagreementKind::ExecutorRefusedCommit
        }
        (Disposition::Commit, Disposition::RequestApproval { .. }) => {
            DisagreementKind::ExecutorGatedCommit
        }
        (Disposition::Commit, Disposition::Escalate { .. }) => {
            DisagreementKind::ExecutorEscalatedCommit
        }
        (Disposition::Commit, Disposition::Disambiguate) => {
            DisagreementKind::ExecutorDisambiguatedCommit
        }
        _ if planner.strictness() > executor.strictness() => {
            DisagreementKind::PlannerMoreConservative
        }
        _ if planner.strictness() == executor.strictness() => {
            DisagreementKind::SameRungDifferentReason
        }
        // Executor stricter than a planner that was not committing: the
        // planner already wanted to stop, and the executor wants to stop
        // harder. Reported as the executor refusing the planner's intent to
        // proceed at all, which is the honest reading.
        _ => DisagreementKind::ExecutorRefusedCommit,
    };
    Some(Disagreement {
        kind,
        planner,
        executor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::{AmbiguityAssessment, Reversibility};
    use crate::digest::{digest_str, domain};
    use crate::escalation::EscalationContext;
    use crate::grounding::GroundingClaim;
    use crate::horizon::Horizon;
    use crate::profile::ProfileId;
    use crate::redaction::Sensitivity;
    use crate::schema::{
        ADAPTIVE_SCHEMA_VERSION, ElementRef, IntentFamily, PlanEnvelope, Postcondition, StepIntent,
    };
    use std::collections::BTreeSet;

    struct World {
        profile: ExecutionProfile,
        plan: PlanEnvelope,
        plan_digest: String,
        live_frame: FrameToken,
        live_element: LiveElement,
        lease: RunLease,
        cleanup: CleanupLedger,
        context: EscalationContext,
    }

    fn element() -> ElementRef {
        ElementRef::new("save-button", 1).unwrap()
    }

    fn frame() -> FrameToken {
        FrameToken {
            frame_id: "frame-1".into(),
            sequence: 1,
            epoch: 0,
            captured_at_millis: 1_000,
            digest: digest_str(domain::FRAME, "frame-1"),
        }
    }

    fn step(confidence: u32) -> PlannedStep {
        PlannedStep {
            index: 0,
            intent: StepIntent::Invoke { element: element() },
            grounding: GroundingClaim::Semantic {
                element: element(),
                role_digest: digest_str(domain::ELEMENT_ROLE, "button"),
            },
            ambiguity: AmbiguityAssessment::unambiguous(confidence),
            reversibility: Reversibility::Reversible,
            expected: Postcondition::FrameChanged,
        }
    }

    fn world(confidence: u32) -> World {
        let profile = ProfileId::Balanced.spec();
        let plan = PlanEnvelope {
            schema_version: ADAPTIVE_SCHEMA_VERSION,
            plan_id: "plan-1".into(),
            objective_digest: digest_str(domain::OBJECTIVE, "save the document"),
            frame: frame(),
            profile: profile.id,
            tier: ModelTier::StrongHosted,
            horizon: Horizon::Short,
            steps: vec![step(confidence)],
        };
        let plan_digest = plan.digest().unwrap();
        let families: BTreeSet<IntentFamily> = [IntentFamily::Ambient, IntentFamily::Semantic]
            .into_iter()
            .collect();
        World {
            profile,
            plan,
            plan_digest,
            live_frame: frame(),
            live_element: LiveElement {
                element: element(),
                role_digest: digest_str(domain::ELEMENT_ROLE, "button"),
                region_digest: digest_str(domain::REGION, "region"),
                enabled: true,
                sensitivity: Sensitivity::None,
                advertises: true,
            },
            lease: RunLease::new("run-1", 100_000),
            cleanup: CleanupLedger::new(),
            context: EscalationContext::new(families, 0),
        }
    }

    fn request<'a>(world: &'a World, planner: Disposition) -> AdmissionRequest<'a> {
        AdmissionRequest {
            profile: &world.profile,
            tier: ModelTier::StrongHosted,
            plan: &world.plan,
            plan_digest: &world.plan_digest,
            step: &world.plan.steps[0],
            planner_disposition: planner,
            live_frame: &world.live_frame,
            live_element: Some(&world.live_element),
            lease: &world.lease,
            cleanup: &world.cleanup,
            context: &world.context,
            approval: None,
            now_millis: 2_000,
            already_disambiguated: false,
        }
    }

    #[test]
    fn a_clean_step_commits_and_records_no_disagreement() {
        let world = world(9_800);
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert!(verdict.commits());
        assert!(verdict.disagreement.is_none());
        assert!(verdict.gates.is_empty());
        assert_eq!(verdict.required_grounding, GroundingLevel::Semantic);
    }

    #[test]
    fn a_planner_commit_cannot_override_a_live_refusal() {
        let mut world = world(9_800);
        world.live_element.enabled = false;
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert!(!verdict.commits());
        assert_eq!(verdict.refusal(), Some(DenyReason::ElementDisabled));
        assert_eq!(
            verdict.disagreement.unwrap().kind,
            DisagreementKind::ExecutorRefusedCommit
        );
    }

    #[test]
    fn a_conservative_planner_is_never_talked_out_of_it() {
        let world = world(9_800);
        let verdict = evaluate(&request(
            &world,
            Disposition::Refuse {
                reason: DenyReason::BackendUnavailable,
            },
        ));
        assert!(!verdict.commits());
        assert_eq!(verdict.executor, Disposition::Commit);
        assert_eq!(verdict.refusal(), Some(DenyReason::BackendUnavailable));
        assert_eq!(
            verdict.disagreement.unwrap().kind,
            DisagreementKind::PlannerMoreConservative
        );
    }

    #[test]
    fn sensitivity_outranks_confidence() {
        let mut world = world(500);
        world.live_element.sensitivity = Sensitivity::Secure;
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert_eq!(verdict.refusal(), Some(DenyReason::SensitiveSurface));
    }

    #[test]
    fn the_grant_outranks_grounding() {
        let mut world = world(9_800);
        world.context = EscalationContext::new(BTreeSet::new(), 0);
        // Also break the grounding, to prove which refusal is reported.
        world.live_element.advertises = false;
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert_eq!(verdict.refusal(), Some(DenyReason::ClassOutsideGrant));
    }

    #[test]
    fn a_moved_epoch_refuses_before_the_frame_is_even_compared() {
        let mut world = world(9_800);
        world.lease.bump_epoch(crate::lease::EpochBump::Paused);
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert_eq!(verdict.refusal(), Some(DenyReason::FrameEpochChanged));
    }

    #[test]
    fn a_stale_frame_refuses_even_when_everything_else_is_perfect() {
        let world = world(9_800);
        let mut request = request(&world, Disposition::Commit);
        request.now_millis = 1_000 + world.profile.max_frame_age_millis + 1;
        let verdict = evaluate(&request);
        assert_eq!(verdict.refusal(), Some(DenyReason::StaleFrame));
    }

    #[test]
    fn an_open_gate_produces_a_request_not_a_refusal() {
        let mut world = world(9_800);
        world.live_element.sensitivity = Sensitivity::Potential;
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert!(!verdict.commits());
        assert_eq!(
            verdict.executor,
            Disposition::RequestApproval {
                reason: ApprovalReason::SensitiveAdjacentTextEntry
            }
        );
        assert_eq!(
            verdict.gates,
            vec![ApprovalReason::SensitiveAdjacentTextEntry]
        );
        assert_eq!(
            verdict.disagreement.unwrap().kind,
            DisagreementKind::ExecutorGatedCommit
        );
    }

    #[test]
    fn an_answered_gate_lets_the_step_through() {
        let mut world = world(9_800);
        world.live_element.sensitivity = Sensitivity::Potential;
        let decision = ApprovalDecision {
            plan_digest: world.plan_digest.clone(),
            step_index: 0,
            granted: vec![ApprovalReason::SensitiveAdjacentTextEntry],
            approved: true,
            epoch: 0,
        };
        let mut request = request(&world, Disposition::Commit);
        request.approval = Some(&decision);
        let verdict = evaluate(&request);
        assert!(verdict.commits());
        // The gate is still reported, so the receipt shows a human was asked.
        assert_eq!(
            verdict.gates,
            vec![ApprovalReason::SensitiveAdjacentTextEntry]
        );
    }

    #[test]
    fn cancellation_refuses_between_decision_and_dispatch() {
        let mut world = world(9_800);
        let mut lease = world.lease.clone();
        let mut cleanup = CleanupLedger::new();
        cleanup.acquire(crate::cancel::Resource::Lease);
        cleanup.cancel(
            &mut lease,
            crate::cancel::CancelCause::OperatorRequest,
            1_500,
        );
        world.lease = lease;
        world.cleanup = cleanup;
        let verdict = evaluate(&request(&world, Disposition::Commit));
        assert_eq!(verdict.refusal(), Some(DenyReason::Cancelled));
    }

    #[test]
    fn an_answered_low_confidence_request_is_not_re_raised() {
        // Balanced allows a human to underwrite a below-floor commit, so the
        // threshold ladder asks on its own account rather than through a gate.
        let world = world(6_500);
        let mut request = request(&world, Disposition::Commit);
        request.step = &world.plan.steps[0];
        let unanswered = evaluate(&request);
        assert_eq!(
            unanswered.executor,
            Disposition::RequestApproval {
                reason: ApprovalReason::LowConfidenceCommit
            }
        );
        // The step itself opens no gate, so the answer is the only thing that
        // can clear the request.
        assert!(unanswered.gates.is_empty());

        let decision = ApprovalDecision {
            plan_digest: world.plan_digest.clone(),
            step_index: 0,
            granted: vec![ApprovalReason::LowConfidenceCommit],
            approved: true,
            epoch: 0,
        };
        request.approval = Some(&decision);
        assert!(evaluate(&request).commits());

        let refused = ApprovalDecision {
            approved: false,
            ..decision.clone()
        };
        request.approval = Some(&refused);
        assert_eq!(
            evaluate(&request).refusal(),
            Some(DenyReason::ApprovalDenied)
        );
    }

    #[test]
    fn an_answer_for_a_different_step_does_not_clear_the_request() {
        let world = world(6_500);
        let mut request = request(&world, Disposition::Commit);
        let elsewhere = ApprovalDecision {
            plan_digest: world.plan_digest.clone(),
            step_index: 9,
            granted: vec![ApprovalReason::LowConfidenceCommit],
            approved: true,
            epoch: 0,
        };
        request.approval = Some(&elsewhere);
        assert_eq!(
            evaluate(&request).executor,
            Disposition::RequestApproval {
                reason: ApprovalReason::LowConfidenceCommit
            }
        );
    }

    #[test]
    fn a_gate_never_softens_a_refusal() {
        // An irreversible step at zero confidence opens a gate and fails the
        // threshold. The refusal has to win: putting a step nobody believes in
        // front of a person is worse than declining it.
        let world = world(0);
        let mut request = request(&world, Disposition::Commit);
        let mut step = world.plan.steps[0].clone();
        step.reversibility = crate::confidence::Reversibility::Irreversible;
        request.step = &step;
        let verdict = evaluate(&request);
        assert_eq!(
            verdict.refusal(),
            Some(DenyReason::ConfidenceBelowThreshold)
        );
        // The gate is still reported, so a reviewer sees it was there.
        assert_eq!(verdict.gates, vec![ApprovalReason::IrreversibleStep]);
    }

    #[test]
    fn the_resolved_disposition_is_never_weaker_than_either_side() {
        let world = world(9_800);
        for planner in [
            Disposition::Commit,
            Disposition::Disambiguate,
            Disposition::RequestApproval {
                reason: ApprovalReason::PointerFallback,
            },
            Disposition::Escalate {
                reason: crate::vocabulary::EscalationReason::CapabilityGap,
            },
            Disposition::Refuse {
                reason: DenyReason::StaleFrame,
            },
        ] {
            let verdict = evaluate(&request(&world, planner));
            assert!(verdict.resolved.strictness() >= verdict.planner.strictness());
            assert!(verdict.resolved.strictness() >= verdict.executor.strictness());
        }
    }
}
