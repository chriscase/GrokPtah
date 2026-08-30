//! Adaptive planner/executor review: one advisory tightening on the existing
//! admission path.
//!
//! A Computer Run can be driven by a small, cheap, local model or by a strong
//! one. They differ in how sure they are, how ambiguous they let a target be,
//! and how willing they are to act on a frame that has drifted. None of that
//! is an authority question, and this module deliberately has no authority.
//!
//! # Where this sits, and why it cannot widen anything
//!
//! [`review`] is called from exactly one place: inside the `act` mutation
//! closure in [`super::service`], **after** [`super::policy::ComputerPolicy::authorize_action`]
//! has already returned `Ok`. That placement is the whole safety argument:
//!
//! * It runs only on the already-authorized path, so it can turn an admit into
//!   a refusal and has no reachable code path that turns a refusal into an
//!   admit. A cheap model cannot buy its way past a kernel gate here because
//!   the kernel gate has already run and already said yes.
//! * It returns [`AdaptiveOutcome::Admit`] or [`AdaptiveOutcome::Refuse`].
//!   There is no third variant. "Allow anyway", "retry", and "downgrade" are
//!   not expressible.
//! * It owns no state machine. It never transitions the run, never touches the
//!   grant, never re-observes, and never dispatches. The single action state
//!   machine in [`super::types::ComputerRun::transition`] stays the only one.
//! * Every refusal maps onto an existing [`ComputerErrorCode`]. No gate is
//!   renamed, widened, or given a new escape code.
//!
//! # Retries
//!
//! There are none. When planner and executor resolve to anything other than
//! "commit" -- more evidence needed, a stronger model needed, a human needed --
//! the action is **refused** and the caller must come back with a fresh
//! observation, a fresh plan, or an approval. Silently re-running uncertain
//! work is the failure mode this module exists to prevent, so the refusal is
//! the only outcome available.
//!
//! # Opt-in
//!
//! [`super::service::ComputerUseService::act`] passes `None` and is therefore
//! byte-identical to its behaviour before this module existed. Only
//! [`super::service::ComputerUseService::act_with_plan`] supplies a claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{
    ActionClass, ComputerAction, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerObservation, ComputerRun,
};

/// Basis points. Ratios are integers so thresholds compare exactly and a
/// decision record is reproducible on every platform.
pub type Bps = u32;

/// Full scale in basis points.
pub const BPS_FULL: Bps = 10_000;

/// Which efficiency profile the caller selected for this action.
///
/// Selection is explicit per action: there is no implicit default, and the
/// chosen profile is recorded in the decision so profile-shopping is visible
/// to an operator rather than silent. Profiles differ only in how much
/// verification they buy; none of them can reach the authority checks that
/// already ran, so choosing the cheapest one cannot lower the floor below the
/// kernel's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveProfile {
    /// Cheapest. Loosest confidence floor, widest candidate tolerance.
    Economy,
    /// Default posture.
    Balanced,
    /// Tightest floors, and no unattended low-confidence commit: a human may
    /// not be asked to underwrite a guess, the action is refused instead.
    HighAssurance,
}

impl AdaptiveProfile {
    pub const ALL: &'static [AdaptiveProfile] =
        &[Self::Economy, Self::Balanced, Self::HighAssurance];

    /// The knobs this profile sets. Every one of them is a *spending* or
    /// *verification* knob; none is an authority knob.
    #[must_use]
    pub fn thresholds(self) -> AdaptiveThresholds {
        match self {
            Self::Economy => AdaptiveThresholds {
                max_observation_age_millis: 10_000,
                commit_floor_bps: 6_000,
                min_margin_bps: 500,
                max_candidates: 3,
                human_may_underwrite: true,
            },
            Self::Balanced => AdaptiveThresholds {
                max_observation_age_millis: 5_000,
                commit_floor_bps: 7_000,
                min_margin_bps: 1_000,
                max_candidates: 2,
                human_may_underwrite: true,
            },
            Self::HighAssurance => AdaptiveThresholds {
                max_observation_age_millis: 2_000,
                commit_floor_bps: 8_000,
                min_margin_bps: 1_500,
                max_candidates: 1,
                human_may_underwrite: false,
            },
        }
    }
}

/// The thresholds one profile applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdaptiveThresholds {
    /// Oldest observation this profile will act on. Clamped down to the run's
    /// own kernel bound by [`AdaptiveThresholds::effective_age_bound`]; a
    /// profile can only ever be stricter than the run.
    pub max_observation_age_millis: u64,
    pub commit_floor_bps: Bps,
    pub min_margin_bps: Bps,
    pub max_candidates: u32,
    /// Whether a human may authorize a below-floor commit, or whether such a
    /// commit is refused outright.
    pub human_may_underwrite: bool,
}

impl AdaptiveThresholds {
    /// The age bound actually applied: the tighter of the profile's and the
    /// run's.
    ///
    /// Taking the minimum is what makes a profile unable to buy staleness. A
    /// caller that hands in a profile looser than the run's limits gets the
    /// run's limits, not its own.
    #[must_use]
    pub fn effective_age_bound(&self, run_bound_millis: u64) -> u64 {
        self.max_observation_age_millis.min(run_bound_millis)
    }
}

/// What the proposer believes about the candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AmbiguityAssessment {
    pub candidate_count: u32,
    pub top_confidence_bps: Bps,
    pub runner_up_confidence_bps: Bps,
}

impl AmbiguityAssessment {
    #[must_use]
    pub fn unambiguous(top_confidence_bps: Bps) -> Self {
        Self {
            candidate_count: 1,
            top_confidence_bps,
            runner_up_confidence_bps: 0,
        }
    }

    #[must_use]
    pub fn margin_bps(&self) -> Bps {
        self.top_confidence_bps
            .saturating_sub(self.runner_up_confidence_bps)
    }

    /// An assessment that claims a runner-up above the top, more confidence
    /// than exists, or a runner-up with no second candidate is malformed
    /// rather than merely unconfident.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.candidate_count >= 1
            && self.top_confidence_bps <= BPS_FULL
            && self.runner_up_confidence_bps <= self.top_confidence_bps
            && (self.candidate_count > 1 || self.runner_up_confidence_bps == 0)
    }
}

/// One rung of the disposition ladder, ordered from least to most
/// conservative.
///
/// The ordering is chosen so that raising confidence never produces a stricter
/// rung: `RequestApproval` sits below `Escalate` because a run is likelier to
/// survive a human answer than a hand-off. A ladder that jumped around would
/// make "more confident" mean "more blocked" somewhere in the middle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveDisposition {
    Commit,
    Disambiguate,
    RequestApproval,
    Escalate,
    Refuse,
}

impl AdaptiveDisposition {
    /// Position on the ladder. Higher is more conservative.
    #[must_use]
    pub fn strictness(self) -> u8 {
        match self {
            Self::Commit => 0,
            Self::Disambiguate => 1,
            Self::RequestApproval => 2,
            Self::Escalate => 3,
            Self::Refuse => 4,
        }
    }

    /// Combine two independent conclusions conservatively: the stricter wins.
    ///
    /// This is the planner/executor disagreement rule, and it is symmetric on
    /// purpose. A confident planner cannot talk the executor into acting, and
    /// a confident executor cannot talk a cautious planner out of stopping.
    #[must_use]
    pub fn resolve(self, other: Self) -> Self {
        if self.strictness() >= other.strictness() {
            self
        } else {
            other
        }
    }
}

/// Why a review reached the rung it did. Closed, and payload-free: a reason
/// crosses every boundary in the system, so there is nowhere for observed
/// text to ride out on one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveReason {
    /// Nothing to report; the review admitted the action.
    Admitted,
    /// The planner decided against a frame that is no longer current, or one
    /// older than the profile allows.
    StaleFrame,
    /// A pause, takeover, cancellation, or recovery moved the control epoch
    /// between the plan and the action.
    ControlEpochMoved,
    /// An operator holds the run.
    OperatorControls,
    /// No usable grant is attached to the run.
    GrantNotHeld,
    /// The action's class is outside the grant.
    ClassOutsideGrant,
    /// Confidence is below the profile's floor for a commit.
    ConfidenceBelowFloor,
    /// Candidates could not be separated within the profile's bound.
    AmbiguityUnresolved,
    /// The step needs a stronger model than the one that proposed it.
    NeedsStrongerModel,
    /// A human has to answer before this may proceed.
    ApprovalRequired,
    /// A human answered with a refusal.
    ApprovalDenied,
    /// Planner and executor disagreed and the conflict resolved against
    /// acting.
    PlannerExecutorDisagreement,
    /// The claim itself is malformed.
    SchemaViolation,
}

impl AdaptiveReason {
    pub const ALL: &'static [AdaptiveReason] = &[
        Self::Admitted,
        Self::StaleFrame,
        Self::ControlEpochMoved,
        Self::OperatorControls,
        Self::GrantNotHeld,
        Self::ClassOutsideGrant,
        Self::ConfidenceBelowFloor,
        Self::AmbiguityUnresolved,
        Self::NeedsStrongerModel,
        Self::ApprovalRequired,
        Self::ApprovalDenied,
        Self::PlannerExecutorDisagreement,
        Self::SchemaViolation,
    ];

    /// The existing kernel error code this refusal is reported as.
    ///
    /// Every variant maps onto a code the safety kernel already has. Nothing
    /// here invents a code, and no existing gate is renamed or relaxed to make
    /// room for one.
    #[must_use]
    pub fn error_code(self) -> ComputerErrorCode {
        match self {
            // Never surfaces as an error; present so the mapping is total.
            Self::Admitted => ComputerErrorCode::Internal,
            Self::StaleFrame => ComputerErrorCode::StaleObservation,
            Self::ControlEpochMoved => ComputerErrorCode::InvalidState,
            Self::OperatorControls | Self::GrantNotHeld => ComputerErrorCode::Unauthorized,
            Self::ClassOutsideGrant => ComputerErrorCode::ForbiddenAction,
            Self::ConfidenceBelowFloor
            | Self::AmbiguityUnresolved
            | Self::NeedsStrongerModel
            | Self::PlannerExecutorDisagreement => ComputerErrorCode::UncertainOutcome,
            Self::ApprovalRequired => ComputerErrorCode::PermissionRequired,
            Self::ApprovalDenied => ComputerErrorCode::PermissionDenied,
            Self::SchemaViolation => ComputerErrorCode::InvalidRequest,
        }
    }

    /// A bounded, content-free message. Built from the variant alone, so no
    /// observed text can reach it.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Admitted => "adaptive review admitted the action",
            Self::StaleFrame => {
                "adaptive review: the plan is bound to a superseded or expired observation"
            }
            Self::ControlEpochMoved => "adaptive review: run control moved between plan and action",
            Self::OperatorControls => "adaptive review: an operator holds this run",
            Self::GrantNotHeld => "adaptive review: the run holds no usable grant",
            Self::ClassOutsideGrant => "adaptive review: the action class is outside the grant",
            Self::ConfidenceBelowFloor => {
                "adaptive review: confidence is below the selected profile's commit floor"
            }
            Self::AmbiguityUnresolved => {
                "adaptive review: candidate targets are not separated within the profile's bound"
            }
            Self::NeedsStrongerModel => {
                "adaptive review: this step needs a stronger model than the one that proposed it"
            }
            Self::ApprovalRequired => {
                "adaptive review: a local approval is required before this action"
            }
            Self::ApprovalDenied => "adaptive review: the local approval was refused",
            Self::PlannerExecutorDisagreement => {
                "adaptive review: planner and executor disagreed and resolved against acting"
            }
            Self::SchemaViolation => "adaptive review: the planner claim is malformed",
        }
    }
}

/// An opaque local approval answer, bound to exactly one run, control epoch,
/// and observation.
///
/// This type is intentionally not serializable and its fields are private.
/// Planner or model JSON cannot turn `approved: true` into a human
/// underwrite. A production host that already holds the operator's yes/no
/// mints one through
/// [`super::service::ComputerUseService::mint_host_adaptive_approval`], which
/// binds that boolean to the live stored run. This crate does not collect
/// the decision, prompt an operator, or accept a token from the wire.
///
/// The binding is what stops an answer being banked and spent later: a token
/// minted for one observation cannot authorize the next one, and one minted
/// before a takeover cannot authorize anything after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptiveApproval {
    run_id: String,
    control_epoch: u64,
    observation_id: String,
    approved: bool,
}

impl AdaptiveApproval {
    /// Mint an approval from a trusted host decision. This is crate-private
    /// by design; public callers must never be able to construct one from
    /// wire data. Production minting goes through
    /// [`super::service::ComputerUseService::mint_host_adaptive_approval`] so
    /// the binding is the live stored run, not a caller-supplied snapshot.
    pub(crate) fn host_mint(
        run: &ComputerRun,
        observation: &ComputerObservation,
        approved: bool,
    ) -> Self {
        Self {
            run_id: run.run_id.clone(),
            control_epoch: run.control_epoch,
            observation_id: observation.observation_id.clone(),
            approved,
        }
    }

    /// Non-secret fingerprint of the hidden run/epoch/observation binding.
    ///
    /// Uses the same payload hash as mutation receipts so the marker is
    /// deterministic. The digest is not a capability: it cannot reconstitute
    /// this token, is never accepted as input, and does not include the
    /// yes/no decision. Raw identities never appear on the mutation payload.
    pub(crate) fn binding_fingerprint(&self) -> String {
        crate::orchestration::hash_payload(&serde_json::json!({
            "runId": self.run_id,
            "controlEpoch": self.control_epoch,
            "observationId": self.observation_id,
        }))
    }

    /// Mutation-replay marker: host decision plus the binding fingerprint.
    /// Distinct hidden bindings therefore cannot collide merely because both
    /// tokens were approved or both were denied.
    pub(crate) fn replay_marker(&self) -> serde_json::Value {
        serde_json::json!({
            "approved": self.approved,
            "binding": self.binding_fingerprint(),
        })
    }

    fn matches(&self, run: &ComputerRun, observation: &ComputerObservation) -> bool {
        self.run_id == run.run_id
            && self.control_epoch == run.control_epoch
            && self.observation_id == observation.observation_id
    }
}

/// What the planner is asserting about this action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdaptiveClaim {
    /// Explicit profile selection. There is no default.
    pub profile: AdaptiveProfile,
    /// The planner's own conclusion, on the frame it saw.
    pub planner: AdaptiveDisposition,
    /// The evidence behind that conclusion.
    pub assessment: AmbiguityAssessment,
    /// The control epoch the planner observed. A mismatch means the run moved
    /// underneath the plan.
    pub observed_control_epoch: u64,
    /// The observation sequence the planner decided against.
    pub observed_sequence: u64,
    /// A local approval answer, if one has been collected. It is intentionally
    /// skipped on the public wire: untrusted claims can never deserialize a
    /// successful approval. Trusted host code may set it after
    /// [`super::service::ComputerUseService::mint_host_adaptive_approval`].
    #[serde(skip)]
    pub approval: Option<AdaptiveApproval>,
}

impl AdaptiveClaim {
    /// Internal replay marker for the opaque approval. The decision and a
    /// non-secret binding fingerprint are part of the mutation identity
    /// without serializing the approval token or its raw ids.
    pub(crate) fn approval_marker(&self) -> Option<serde_json::Value> {
        self.approval.as_ref().map(AdaptiveApproval::replay_marker)
    }
}

/// The durable, redacted record of one review.
///
/// Carries dispositions, a closed reason, and integer confidence only. No
/// element identity, label, value, geometry, or backend-authored text can
/// reach it, which is what lets it be projected to a coordinator unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdaptiveDecisionRecord {
    pub profile: AdaptiveProfile,
    pub planner: AdaptiveDisposition,
    pub executor: AdaptiveDisposition,
    pub resolved: AdaptiveDisposition,
    pub reason: AdaptiveReason,
    /// True when planner and executor did not reach the same rung.
    pub disagreed: bool,
    pub admitted: bool,
    pub action_class: ActionClass,
    pub top_confidence_bps: Bps,
    pub margin_bps: Bps,
    pub candidate_count: u32,
    /// The age bound actually applied, after clamping the profile's to the
    /// run's.
    pub applied_age_bound_millis: u64,
    pub observation_age_millis: i64,
    pub control_epoch: u64,
    pub decided_at: DateTime<Utc>,
}

/// The only two things a review can conclude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdaptiveOutcome {
    /// Leave the already-authorized action exactly as it is.
    Admit(AdaptiveDecisionRecord),
    /// Refuse it. The record explains why, in closed vocabulary.
    Refuse(AdaptiveDecisionRecord, ComputerError),
}

impl AdaptiveOutcome {
    #[must_use]
    pub fn record(&self) -> &AdaptiveDecisionRecord {
        match self {
            Self::Admit(record) | Self::Refuse(record, _) => record,
        }
    }

    #[must_use]
    pub fn refusal(&self) -> Option<&ComputerError> {
        match self {
            Self::Admit(_) => None,
            Self::Refuse(_, error) => Some(error),
        }
    }
}

/// Review one already-authorized action.
///
/// Callers must have run [`super::policy::ComputerPolicy::authorize_action`]
/// first; this function assumes it and only ever tightens the result. It reads
/// `run` and `observation` and mutates nothing.
#[must_use]
pub fn review(
    run: &ComputerRun,
    observation: &ComputerObservation,
    action: &ComputerAction,
    claim: &AdaptiveClaim,
    now: DateTime<Utc>,
) -> AdaptiveOutcome {
    let thresholds = claim.profile.thresholds();
    let applied_age_bound_millis =
        thresholds.effective_age_bound(run.limits.max_observation_age_millis);
    let observation_age_millis = now
        .signed_duration_since(observation.captured_at)
        .num_milliseconds();

    let (executor, reason) = derive(
        run,
        observation,
        action,
        claim,
        &thresholds,
        applied_age_bound_millis,
        observation_age_millis,
    );

    let resolved = claim.planner.resolve(executor);
    // A resolution that is not a commit is a refusal. There is deliberately no
    // branch that retries, re-observes, or downgrades the action instead.
    let reason = if resolved == AdaptiveDisposition::Commit {
        AdaptiveReason::Admitted
    } else if resolved != executor {
        // The planner was the stricter side, and it gave no machine-readable
        // reason of its own, so the disagreement is the reason.
        AdaptiveReason::PlannerExecutorDisagreement
    } else {
        reason
    };

    let record = AdaptiveDecisionRecord {
        profile: claim.profile,
        planner: claim.planner,
        executor,
        resolved,
        reason,
        disagreed: claim.planner != executor,
        admitted: resolved == AdaptiveDisposition::Commit,
        action_class: action.class(),
        top_confidence_bps: claim.assessment.top_confidence_bps.min(BPS_FULL),
        margin_bps: claim.assessment.margin_bps(),
        candidate_count: claim.assessment.candidate_count,
        applied_age_bound_millis,
        observation_age_millis,
        control_epoch: run.control_epoch,
        decided_at: now,
    };

    if record.admitted {
        AdaptiveOutcome::Admit(record)
    } else {
        let error = ComputerError::new(reason.error_code(), reason.message());
        AdaptiveOutcome::Refuse(record, error)
    }
}

/// The executor's own conclusion, derived from the live run alone.
///
/// The order is fixed: schema, then who is driving, then which frame, then the
/// grant, then confidence. The first refusal is the one a reviewer reads, and
/// "an operator took over" matters more than "and it was also unconfident".
fn derive(
    run: &ComputerRun,
    observation: &ComputerObservation,
    action: &ComputerAction,
    claim: &AdaptiveClaim,
    thresholds: &AdaptiveThresholds,
    applied_age_bound_millis: u64,
    observation_age_millis: i64,
) -> (AdaptiveDisposition, AdaptiveReason) {
    if !claim.assessment.is_well_formed() {
        return (AdaptiveDisposition::Refuse, AdaptiveReason::SchemaViolation);
    }

    // --- who is driving -------------------------------------------------
    if run.control_disposition != ComputerControlDisposition::AgentOwned {
        return (
            AdaptiveDisposition::Refuse,
            AdaptiveReason::OperatorControls,
        );
    }
    if claim.observed_control_epoch != run.control_epoch {
        return (
            AdaptiveDisposition::Refuse,
            AdaptiveReason::ControlEpochMoved,
        );
    }

    // --- which frame ----------------------------------------------------
    if claim.observed_sequence != observation.sequence {
        return (AdaptiveDisposition::Refuse, AdaptiveReason::StaleFrame);
    }
    // A frame from the future is not fresh, it is wrong; refusing rather than
    // clamping keeps a skewed clock from buying unlimited freshness.
    if observation_age_millis < 0 || observation_age_millis as u64 > applied_age_bound_millis {
        return (AdaptiveDisposition::Refuse, AdaptiveReason::StaleFrame);
    }

    // --- the lease ------------------------------------------------------
    let Some(grant) = run.grant.as_ref() else {
        return (AdaptiveDisposition::Refuse, AdaptiveReason::GrantNotHeld);
    };
    if grant.revoked_at.is_some() || grant.uses_remaining == Some(0) {
        return (AdaptiveDisposition::Refuse, AdaptiveReason::GrantNotHeld);
    }
    if !grant.action_classes.contains(&action.class()) {
        return (
            AdaptiveDisposition::Refuse,
            AdaptiveReason::ClassOutsideGrant,
        );
    }

    // --- is anyone sure enough -----------------------------------------
    let assessment = &claim.assessment;
    if assessment.candidate_count > thresholds.max_candidates
        || assessment.margin_bps() < thresholds.min_margin_bps
    {
        return (
            AdaptiveDisposition::Disambiguate,
            AdaptiveReason::AmbiguityUnresolved,
        );
    }
    if assessment.top_confidence_bps < thresholds.commit_floor_bps {
        if !thresholds.human_may_underwrite {
            // The dearest profile declines rather than asking a person to
            // underwrite a guess.
            return (
                AdaptiveDisposition::Refuse,
                AdaptiveReason::ConfidenceBelowFloor,
            );
        }
        return match claim.approval.as_ref() {
            Some(approval) if approval.matches(run, observation) && approval.approved => {
                (AdaptiveDisposition::Commit, AdaptiveReason::Admitted)
            }
            Some(approval) if approval.matches(run, observation) => {
                (AdaptiveDisposition::Refuse, AdaptiveReason::ApprovalDenied)
            }
            // A missing answer, or one bound to a different run, epoch, or
            // observation, is an outstanding requirement -- never consent.
            _ => (
                AdaptiveDisposition::RequestApproval,
                AdaptiveReason::ApprovalRequired,
            ),
        };
    }

    (AdaptiveDisposition::Commit, AdaptiveReason::Admitted)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use uuid::Uuid;

    use super::*;
    use crate::computer_use::types::{
        ActionGrant, ComputerTarget, ComputerUseLimits, GrantIssuer, ObservationGeometry,
        SemanticAction, SemanticElement, Sensitivity,
    };

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn observation(now: DateTime<Utc>) -> ComputerObservation {
        ComputerObservation {
            observation_id: "obs-1".into(),
            sequence: 7,
            target: target(),
            captured_at: now,
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
                value: None,
                bounds: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                actions: BTreeSet::from([SemanticAction::Invoke]),
            }],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    fn run(now: DateTime<Utc>) -> ComputerRun {
        let mut run =
            ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default()).unwrap();
        run.grant = Some(ActionGrant {
            grant_id: "grant-1".into(),
            run_id: run.run_id.clone(),
            target: target(),
            action_classes: BTreeSet::from([ActionClass::Semantic]),
            issued_by: GrantIssuer::LocalUser,
            issued_at: now - Duration::seconds(1),
            expires_at: now + Duration::minutes(5),
            uses_remaining: None,
            revoked_at: None,
        });
        run.current_observation = Some(observation(now));
        run
    }

    fn claim(profile: AdaptiveProfile, run: &ComputerRun) -> AdaptiveClaim {
        AdaptiveClaim {
            profile,
            planner: AdaptiveDisposition::Commit,
            assessment: AmbiguityAssessment::unambiguous(9_500),
            observed_control_epoch: run.control_epoch,
            observed_sequence: 7,
            approval: None,
        }
    }

    fn action() -> ComputerAction {
        ComputerAction::Invoke {
            element_id: "field".into(),
        }
    }

    #[test]
    fn adaptive_wire_types_reject_unknown_fields() {
        let now = Utc::now();
        let run = run(now);
        let mut encoded = serde_json::to_value(claim(AdaptiveProfile::Economy, &run)).unwrap();
        encoded["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AdaptiveClaim>(encoded).is_err());

        let mut assessment = serde_json::to_value(AmbiguityAssessment::unambiguous(9_000)).unwrap();
        assessment["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<AmbiguityAssessment>(assessment).is_err());
    }

    #[test]
    fn a_clean_action_is_admitted_unchanged_under_every_profile() {
        let now = Utc::now();
        let run = run(now);
        let observation = observation(now);
        for profile in AdaptiveProfile::ALL {
            let outcome = review(&run, &observation, &action(), &claim(*profile, &run), now);
            assert!(
                outcome.refusal().is_none(),
                "{profile:?} refused a clean action"
            );
            let record = outcome.record();
            assert!(record.admitted);
            assert!(!record.disagreed);
            assert_eq!(record.reason, AdaptiveReason::Admitted);
            assert_eq!(record.profile, *profile);
        }
    }

    #[test]
    fn the_review_has_no_outcome_that_admits_more_than_it_was_given() {
        // The type has exactly two variants and neither carries permission.
        let now = Utc::now();
        let run = run(now);
        let outcome = review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::Economy, &run),
            now,
        );
        match outcome {
            AdaptiveOutcome::Admit(record) => assert!(record.admitted),
            AdaptiveOutcome::Refuse(record, error) => {
                assert!(!record.admitted);
                assert_eq!(error.code, record.reason.error_code());
            }
        }
    }

    #[test]
    fn a_confident_planner_cannot_override_a_live_refusal() {
        let now = Utc::now();
        let mut run = run(now);
        run.set_control_disposition(ComputerControlDisposition::OperatorTakeover);
        let mut claim = claim(AdaptiveProfile::Economy, &run);
        claim.planner = AdaptiveDisposition::Commit;
        claim.observed_control_epoch = run.control_epoch;
        let outcome = review(&run, &observation(now), &action(), &claim, now);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::Unauthorized)
        );
        assert_eq!(outcome.record().reason, AdaptiveReason::OperatorControls);
    }

    #[test]
    fn a_cautious_planner_is_never_overridden_by_a_permissive_executor() {
        let now = Utc::now();
        let run = run(now);
        for planner in [
            AdaptiveDisposition::Disambiguate,
            AdaptiveDisposition::RequestApproval,
            AdaptiveDisposition::Escalate,
            AdaptiveDisposition::Refuse,
        ] {
            let mut claim = claim(AdaptiveProfile::Economy, &run);
            claim.planner = planner;
            let outcome = review(&run, &observation(now), &action(), &claim, now);
            assert!(
                outcome.refusal().is_some(),
                "a planner that said {planner:?} was overridden"
            );
            let record = outcome.record();
            assert_eq!(record.executor, AdaptiveDisposition::Commit);
            assert!(record.disagreed);
            assert_eq!(record.reason, AdaptiveReason::PlannerExecutorDisagreement);
        }
    }

    #[test]
    fn resolution_is_symmetric_and_never_relaxes() {
        let ladder = [
            AdaptiveDisposition::Commit,
            AdaptiveDisposition::Disambiguate,
            AdaptiveDisposition::RequestApproval,
            AdaptiveDisposition::Escalate,
            AdaptiveDisposition::Refuse,
        ];
        for a in ladder {
            assert_eq!(a.resolve(a), a);
            for b in ladder {
                let resolved = a.resolve(b);
                assert_eq!(resolved, b.resolve(a));
                assert!(resolved.strictness() >= a.strictness());
                assert!(resolved.strictness() >= b.strictness());
            }
        }
    }

    #[test]
    fn a_profile_can_only_tighten_the_runs_own_staleness_bound() {
        let run_bound = ComputerUseLimits::default().max_observation_age_millis;
        for profile in AdaptiveProfile::ALL {
            let applied = profile.thresholds().effective_age_bound(run_bound);
            assert!(
                applied <= run_bound,
                "{profile:?} would accept a frame the run calls stale"
            );
        }
        // Even a profile handed a looser bound than the run gets the run's.
        let loose = AdaptiveThresholds {
            max_observation_age_millis: u64::MAX,
            ..AdaptiveProfile::Economy.thresholds()
        };
        assert_eq!(loose.effective_age_bound(1_234), 1_234);
    }

    #[test]
    fn a_superseded_or_expired_frame_is_refused() {
        let now = Utc::now();
        let run = run(now);
        let mut stale_sequence = claim(AdaptiveProfile::Economy, &run);
        stale_sequence.observed_sequence = 6;
        assert_eq!(
            review(&run, &observation(now), &action(), &stale_sequence, now)
                .record()
                .reason,
            AdaptiveReason::StaleFrame
        );

        let aged = now + Duration::milliseconds(2_500);
        let outcome = review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::HighAssurance, &run),
            aged,
        );
        assert_eq!(outcome.record().reason, AdaptiveReason::StaleFrame);
        // The same age passes under a looser profile: this is a verification
        // knob, and the kernel's own bound still applies underneath.
        assert!(review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::Balanced, &run),
            aged
        )
        .refusal()
        .is_none());
    }

    #[test]
    fn a_frame_from_the_future_is_refused_rather_than_treated_as_fresh() {
        let now = Utc::now();
        let run = run(now);
        let outcome = review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::Economy, &run),
            now - Duration::milliseconds(1),
        );
        assert_eq!(outcome.record().reason, AdaptiveReason::StaleFrame);
    }

    #[test]
    fn a_moved_control_epoch_is_refused() {
        let now = Utc::now();
        let run = run(now);
        let mut claim = claim(AdaptiveProfile::Economy, &run);
        claim.observed_control_epoch = run.control_epoch + 1;
        let outcome = review(&run, &observation(now), &action(), &claim, now);
        assert_eq!(outcome.record().reason, AdaptiveReason::ControlEpochMoved);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::InvalidState)
        );
    }

    #[test]
    fn a_revoked_or_spent_grant_is_refused() {
        let now = Utc::now();
        for mutate in [
            (|grant: &mut ActionGrant| grant.revoked_at = Some(Utc::now())) as fn(&mut ActionGrant),
            |grant: &mut ActionGrant| grant.uses_remaining = Some(0),
        ] {
            let mut run = run(now);
            mutate(run.grant.as_mut().unwrap());
            let outcome = review(
                &run,
                &observation(now),
                &action(),
                &claim(AdaptiveProfile::Economy, &run),
                now,
            );
            assert_eq!(outcome.record().reason, AdaptiveReason::GrantNotHeld);
        }
    }

    #[test]
    fn an_action_class_outside_the_grant_is_refused() {
        let now = Utc::now();
        let mut run = run(now);
        run.grant.as_mut().unwrap().action_classes = BTreeSet::from([ActionClass::TextEntry]);
        let outcome = review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::Economy, &run),
            now,
        );
        assert_eq!(outcome.record().reason, AdaptiveReason::ClassOutsideGrant);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::ForbiddenAction)
        );
    }

    #[test]
    fn uncertain_work_is_refused_and_never_retried_or_downgraded() {
        let now = Utc::now();
        let run = run(now);
        let mut coin_toss = claim(AdaptiveProfile::Balanced, &run);
        coin_toss.assessment = AmbiguityAssessment {
            candidate_count: 2,
            top_confidence_bps: 9_500,
            runner_up_confidence_bps: 9_400,
        };
        let outcome = review(&run, &observation(now), &action(), &coin_toss, now);
        assert_eq!(outcome.record().executor, AdaptiveDisposition::Disambiguate);
        assert_eq!(outcome.record().reason, AdaptiveReason::AmbiguityUnresolved);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::UncertainOutcome)
        );
        assert!(!outcome.record().admitted);
    }

    #[test]
    fn the_dearest_profile_refuses_rather_than_asking_a_human_to_underwrite_a_guess() {
        let now = Utc::now();
        let run = run(now);
        let mut low = claim(AdaptiveProfile::HighAssurance, &run);
        low.assessment = AmbiguityAssessment::unambiguous(7_500);
        let outcome = review(&run, &observation(now), &action(), &low, now);
        assert_eq!(
            outcome.record().reason,
            AdaptiveReason::ConfidenceBelowFloor
        );
        assert_eq!(outcome.record().executor, AdaptiveDisposition::Refuse);
    }

    #[test]
    fn an_approval_authorizes_one_observation_at_one_epoch() {
        let now = Utc::now();
        let run = run(now);
        let current_observation = observation(now);
        let mut low = claim(AdaptiveProfile::Balanced, &run);
        low.assessment = AmbiguityAssessment::unambiguous(6_500);
        // Unanswered: an outstanding requirement, not consent.
        let outcome = review(&run, &current_observation, &action(), &low, now);
        assert_eq!(outcome.record().reason, AdaptiveReason::ApprovalRequired);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::PermissionRequired)
        );

        let answered = AdaptiveApproval::host_mint(&run, &current_observation, true);
        low.approval = Some(answered.clone());
        assert!(review(&run, &current_observation, &action(), &low, now)
            .refusal()
            .is_none());

        // Bound elsewhere: still an outstanding requirement.
        let mut other_observation = current_observation.clone();
        other_observation.observation_id = "obs-2".into();
        let mut other_epoch = run.clone();
        other_epoch.control_epoch += 1;
        let mut other_run = run.clone();
        other_run.run_id = "another-run".into();
        for wrong in [
            AdaptiveApproval::host_mint(&run, &other_observation, true),
            AdaptiveApproval::host_mint(&other_epoch, &current_observation, true),
            AdaptiveApproval::host_mint(&other_run, &current_observation, true),
        ] {
            low.approval = Some(wrong);
            assert_eq!(
                review(&run, &current_observation, &action(), &low, now)
                    .record()
                    .reason,
                AdaptiveReason::ApprovalRequired
            );
        }

        // A refusal is distinguishable from a missing answer.
        low.approval = Some(AdaptiveApproval::host_mint(
            &run,
            &current_observation,
            false,
        ));
        let refused = review(&run, &current_observation, &action(), &low, now);
        assert_eq!(refused.record().reason, AdaptiveReason::ApprovalDenied);
        assert_eq!(
            refused.refusal().map(|error| error.code),
            Some(ComputerErrorCode::PermissionDenied)
        );
    }

    #[test]
    fn an_approval_cannot_clear_a_hard_refusal() {
        let now = Utc::now();
        let mut run = run(now);
        run.grant.as_mut().unwrap().revoked_at = Some(now);
        let mut low = claim(AdaptiveProfile::Balanced, &run);
        low.assessment = AmbiguityAssessment::unambiguous(6_500);
        low.approval = Some(AdaptiveApproval::host_mint(&run, &observation(now), true));
        let outcome = review(&run, &observation(now), &action(), &low, now);
        assert_eq!(outcome.record().reason, AdaptiveReason::GrantNotHeld);
    }

    #[test]
    fn a_malformed_claim_is_refused_before_anything_else_is_asked() {
        let now = Utc::now();
        let mut run = run(now);
        run.grant.as_mut().unwrap().revoked_at = Some(now);
        let mut broken = claim(AdaptiveProfile::Economy, &run);
        broken.assessment = AmbiguityAssessment {
            candidate_count: 1,
            top_confidence_bps: 100,
            runner_up_confidence_bps: 9_000,
        };
        let outcome = review(&run, &observation(now), &action(), &broken, now);
        assert_eq!(outcome.record().reason, AdaptiveReason::SchemaViolation);
        assert_eq!(
            outcome.refusal().map(|error| error.code),
            Some(ComputerErrorCode::InvalidRequest)
        );
    }

    #[test]
    fn every_reason_maps_onto_an_existing_kernel_error_code() {
        // No new code, and no existing gate renamed to make room for one.
        for reason in AdaptiveReason::ALL {
            let code = reason.error_code();
            assert!(!reason.message().is_empty());
            if *reason == AdaptiveReason::Admitted {
                continue;
            }
            assert_ne!(
                code,
                ComputerErrorCode::Internal,
                "{reason:?} has no kernel code of its own"
            );
        }
    }

    #[test]
    fn approval_replay_markers_distinguish_hidden_bindings() {
        let now = Utc::now();
        let run = run(now);
        let observation = observation(now);
        let approved = AdaptiveApproval::host_mint(&run, &observation, true);
        let denied = AdaptiveApproval::host_mint(&run, &observation, false);
        assert_eq!(
            approved.binding_fingerprint(),
            denied.binding_fingerprint(),
            "the fingerprint is the binding, not the yes/no"
        );
        assert_ne!(
            approved.replay_marker(),
            denied.replay_marker(),
            "approve and deny of one binding must not collide"
        );

        let mut other_observation = observation.clone();
        other_observation.observation_id = "obs-2".into();
        let mut other_epoch = run.clone();
        other_epoch.control_epoch += 1;
        let mut other_run = run.clone();
        other_run.run_id = "another-run".into();
        let other_bindings = [
            AdaptiveApproval::host_mint(&run, &other_observation, true),
            AdaptiveApproval::host_mint(&other_epoch, &observation, true),
            AdaptiveApproval::host_mint(&other_run, &observation, true),
        ];
        for other in other_bindings {
            assert_ne!(approved.binding_fingerprint(), other.binding_fingerprint());
            assert_ne!(approved.replay_marker(), other.replay_marker());
            assert_eq!(other.replay_marker()["approved"], serde_json::json!(true));
        }
    }

    #[test]
    fn approval_binding_fingerprint_does_not_carry_raw_ids() {
        let now = Utc::now();
        let run = run(now);
        let observation = observation(now);
        let approval = AdaptiveApproval::host_mint(&run, &observation, true);
        let fingerprint = approval.binding_fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(
            !fingerprint.contains(&run.run_id),
            "fingerprint leaked the run id"
        );
        assert!(
            !fingerprint.contains(&observation.observation_id),
            "fingerprint leaked the observation id"
        );
        let marker = serde_json::to_string(&approval.replay_marker()).unwrap();
        assert!(!marker.contains(&run.run_id));
        assert!(!marker.contains(&observation.observation_id));
        assert!(!marker.contains("obs-1"));
    }

    #[test]
    fn a_decision_record_carries_no_observed_content() {
        let now = Utc::now();
        let run = run(now);
        let outcome = review(
            &run,
            &observation(now),
            &action(),
            &claim(AdaptiveProfile::Balanced, &run),
            now,
        );
        let serialized = serde_json::to_string(outcome.record()).unwrap();
        for forbidden in ["Name", "text_field", "field", "obs-1", "Demo", "main"] {
            assert!(
                !serialized.contains(forbidden),
                "the decision record leaked {forbidden:?}"
            );
        }
    }
}
