//! Shared fixtures for the adaptive-contract integration tests.
//!
//! One place to build a well-formed step, plan, and live world, so each test
//! file perturbs exactly the one thing it is about. Everything here is
//! deterministic and content-free: no literals travel that a leak test would
//! not want to find.

#![allow(dead_code)]

use std::collections::BTreeSet;

use grokptah_cu_adaptive::cancel::CleanupLedger;
use grokptah_cu_adaptive::confidence::{AmbiguityAssessment, Disposition, Reversibility};
use grokptah_cu_adaptive::digest::{digest_str, domain};
use grokptah_cu_adaptive::escalation::EscalationContext;
use grokptah_cu_adaptive::executor::{AdmissionRequest, StepVerdict, evaluate};
use grokptah_cu_adaptive::gates::ApprovalDecision;
use grokptah_cu_adaptive::grounding::{GroundingClaim, LiveElement};
use grokptah_cu_adaptive::horizon::Horizon;
use grokptah_cu_adaptive::lease::{FrameToken, RunLease};
use grokptah_cu_adaptive::profile::{ExecutionProfile, ProfileId};
use grokptah_cu_adaptive::redaction::Sensitivity;
use grokptah_cu_adaptive::schema::{
    ADAPTIVE_SCHEMA_VERSION, ElementRef, IntentFamily, PlanEnvelope, PlannedStep, Postcondition,
    StepIntent,
};
use grokptah_cu_adaptive::tier::ModelTier;

pub const ELEMENT_ID: &str = "save-button";
pub const ELEMENT_ROLE: &str = "button";
pub const REGION_SEED: &str = "region-bytes";
pub const OBJECTIVE: &str = "objective-text-that-must-never-travel";
pub const CAPTURED_AT: u64 = 1_000;

#[must_use]
pub fn element() -> ElementRef {
    ElementRef::new(ELEMENT_ID, 1).expect("well-formed element reference")
}

#[must_use]
pub fn role_digest() -> String {
    digest_str(domain::ELEMENT_ROLE, ELEMENT_ROLE)
}

#[must_use]
pub fn region_digest() -> String {
    digest_str(domain::REGION, REGION_SEED)
}

#[must_use]
pub fn frame() -> FrameToken {
    FrameToken {
        frame_id: "frame-1".into(),
        sequence: 4,
        epoch: 0,
        captured_at_millis: CAPTURED_AT,
        digest: digest_str(domain::FRAME, "frame-1"),
    }
}

#[must_use]
pub fn live_element() -> LiveElement {
    LiveElement {
        element: element(),
        role_digest: role_digest(),
        region_digest: region_digest(),
        enabled: true,
        sensitivity: Sensitivity::None,
        advertises: true,
    }
}

/// Grounding at the highest level any profile demands.
///
/// Supplying the maximum is what makes a cross-profile comparison meaningful:
/// the profiles differ in what they *ask for*, and the question under test is
/// what they *refuse*. A claim that satisfied the cheap profile and not the
/// expensive one would have the two disagreeing about grounding rather than
/// about authority.
#[must_use]
pub fn full_grounding() -> GroundingClaim {
    GroundingClaim::SemanticPlusRegion {
        element: element(),
        role_digest: role_digest(),
        region_digest: region_digest(),
    }
}

#[must_use]
pub fn step(intent: StepIntent, reversibility: Reversibility) -> PlannedStep {
    let expected = if intent.family().mutates() {
        Postcondition::FrameChanged
    } else {
        Postcondition::None
    };
    PlannedStep {
        index: 0,
        intent,
        grounding: full_grounding(),
        ambiguity: AmbiguityAssessment::unambiguous(9_900),
        reversibility,
        expected,
    }
}

#[must_use]
pub fn invoke_step() -> PlannedStep {
    step(
        StepIntent::Invoke { element: element() },
        Reversibility::Reversible,
    )
}

#[must_use]
pub fn plan_for(profile: ProfileId, tier: ModelTier, step: PlannedStep) -> PlanEnvelope {
    PlanEnvelope {
        schema_version: ADAPTIVE_SCHEMA_VERSION,
        plan_id: "plan-1".into(),
        objective_digest: digest_str(domain::OBJECTIVE, OBJECTIVE),
        frame: frame(),
        profile,
        tier,
        horizon: Horizon::Short,
        steps: vec![step],
    }
}

#[must_use]
pub fn full_grant() -> BTreeSet<IntentFamily> {
    IntentFamily::ALL.iter().copied().collect()
}

/// A complete, well-formed world that a test then perturbs in exactly one way.
pub struct Fixture {
    pub profile: ExecutionProfile,
    pub tier: ModelTier,
    pub plan: PlanEnvelope,
    pub plan_digest: String,
    pub live_frame: FrameToken,
    pub live_element: Option<LiveElement>,
    pub lease: RunLease,
    pub cleanup: CleanupLedger,
    pub context: EscalationContext,
    pub approval: Option<ApprovalDecision>,
    pub now_millis: u64,
    pub planner: Disposition,
}

impl Fixture {
    #[must_use]
    pub fn new(profile: ProfileId, tier: ModelTier) -> Self {
        Self::with_step(profile, tier, invoke_step())
    }

    #[must_use]
    pub fn with_step(profile: ProfileId, tier: ModelTier, step: PlannedStep) -> Self {
        let plan = plan_for(profile, tier, step);
        let plan_digest = plan.digest().expect("plan digests");
        Self {
            profile: profile.spec(),
            tier,
            plan,
            plan_digest,
            live_frame: frame(),
            live_element: Some(live_element()),
            lease: RunLease::new("run-1", 1_000_000),
            cleanup: CleanupLedger::new(),
            context: EscalationContext::new(full_grant(), 0),
            approval: None,
            now_millis: CAPTURED_AT,
            planner: Disposition::Commit,
        }
    }

    #[must_use]
    pub fn evaluate(&self) -> StepVerdict {
        evaluate(&AdmissionRequest {
            profile: &self.profile,
            tier: self.tier,
            plan: &self.plan,
            plan_digest: &self.plan_digest,
            step: &self.plan.steps[0],
            planner_disposition: self.planner,
            live_frame: &self.live_frame,
            live_element: self.live_element.as_ref(),
            lease: &self.lease,
            cleanup: &self.cleanup,
            context: &self.context,
            approval: self.approval.as_ref(),
            now_millis: self.now_millis,
            already_disambiguated: false,
        })
    }
}
