//! Stationarity, budget, and escalation policy for the durable always-on
//! agent loop.
//!
//! This module is deliberately pure: it owns no clock, no I/O, and no provider
//! handle. Every input is supplied by the caller and every decision is a total
//! function of the durable [`LoopState`] plus one [`LoopStep`]. That is what
//! makes the same verdict reproducible after a process restart, and what lets
//! the adversarial tests drive it with synthetic fixtures only.
//!
//! Three invariants hold across everything below.
//!
//! 1. **Spending is not progress.** Tokens, turns, wall-clock, and tool calls
//!    are costs. Only externally attributable change — a file edit, a recorded
//!    test observation, or a genuinely novel observation — counts as progress.
//!    A small model that burns budget restating itself is stationary, and the
//!    loop says so rather than reporting activity as advancement.
//! 2. **Waiting needs a witness.** A model asserting that it is waiting proves
//!    nothing. A wait is productive only while an external witness advances:
//!    a changed witness digest, or a strictly increasing attempt counter under
//!    a future deadline. Unwitnessed waiting is stationary.
//! 3. **Uncertain is absorbing.** A dispatch whose outcome is unknown is never
//!    retried, never escalated into a retry, and never resolved by a stronger
//!    model. It requires a human. This mirrors the computer-use mutation ledger
//!    (`Sending` -> `Uncertain`) so the two surfaces cannot drift apart.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{hash_payload, OrchError, OrchErrorCode};

/// Retained signature history. Bounded so a long-lived agent cannot grow its
/// durable record without limit; large enough that a small model cycling
/// between a handful of restatements is still caught.
pub const SIGNATURE_HISTORY: usize = 8;

/// Upper bound on any caller-supplied digest / witness string.
pub const MAX_DIGEST_BYTES: usize = 128;

/// Declared capability tier for the model driving a loop.
///
/// This is an operator-declared input, never inferred from a model name. The
/// loop does not measure or assert model quality; it only applies the envelope
/// it was told to apply. An undeclared tier gets the most conservative
/// envelope, because "unknown" must not buy a larger budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Explicitly undeclared. Treated exactly as [`ModelTier::Small`].
    #[default]
    Unspecified,
    Small,
    Large,
}

impl ModelTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Small => "small",
            Self::Large => "large",
        }
    }

    /// The next tier an escalation may target, if any.
    fn stronger(self) -> Option<Self> {
        match self {
            Self::Unspecified | Self::Small => Some(Self::Large),
            Self::Large => None,
        }
    }
}

/// Deterministic policy envelope for one tier.
///
/// These are conservative engineering defaults chosen so a stuck loop is cut
/// short quickly. They are not derived from any benchmark, cost, or latency
/// measurement, and they make no claim about how any model actually performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyEnvelope {
    pub tier: ModelTier,
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub max_tokens: u64,
    pub max_wall_ms: u64,
    /// Consecutive stationary steps tolerated before NeedsAttention.
    pub max_stationary_streak: u32,
    /// Consecutive witnessed waits tolerated before NeedsAttention.
    pub max_consecutive_waits: u32,
    /// Total time one uninterrupted wait may span.
    pub max_wait_ms: u64,
    /// Consecutive novel-but-inert steps tolerated. Bounds the "churn" case:
    /// a model emitting a fresh action every turn while changing nothing.
    pub max_novel_without_mutation: u32,
}

impl PolicyEnvelope {
    pub const fn small() -> Self {
        Self {
            tier: ModelTier::Small,
            max_turns: 12,
            max_tool_calls: 48,
            max_tokens: 120_000,
            max_wall_ms: 5 * 60 * 1000,
            max_stationary_streak: 2,
            max_consecutive_waits: 6,
            max_wait_ms: 60_000,
            max_novel_without_mutation: 8,
        }
    }

    pub const fn large() -> Self {
        Self {
            tier: ModelTier::Large,
            max_turns: 32,
            max_tool_calls: 160,
            max_tokens: 400_000,
            max_wall_ms: 15 * 60 * 1000,
            max_stationary_streak: 4,
            max_consecutive_waits: 12,
            max_wait_ms: 180_000,
            max_novel_without_mutation: 20,
        }
    }

    /// Envelope for a declared tier. `Unspecified` gets the small envelope.
    pub const fn for_tier(tier: ModelTier) -> Self {
        match tier {
            ModelTier::Large => Self::large(),
            ModelTier::Small => Self::small(),
            // Unknown capability must not widen any bound. Keep the tier as
            // declared so the record stays truthful about what was known.
            ModelTier::Unspecified => {
                let mut envelope = Self::small();
                envelope.tier = ModelTier::Unspecified;
                envelope
            }
        }
    }

    /// Narrow this envelope toward `requested`. A caller may only tighten a
    /// bound; any attempt to widen one, or to zero one out, is rejected.
    /// This mirrors `merge_bounds` so the two ceilings cannot diverge.
    pub fn narrow(&self, requested: &PolicyEnvelope) -> Result<Self, OrchError> {
        fn check_u32(requested: u32, ceiling: u32, field: &str) -> Result<u32, OrchError> {
            if requested == 0 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("{field} must be > 0"),
                ));
            }
            if requested > ceiling {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("{field} exceeds the {field} ceiling for this tier"),
                ));
            }
            Ok(requested)
        }
        fn check_u64(requested: u64, ceiling: u64, field: &str) -> Result<u64, OrchError> {
            if requested == 0 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("{field} must be > 0"),
                ));
            }
            if requested > ceiling {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("{field} exceeds the {field} ceiling for this tier"),
                ));
            }
            Ok(requested)
        }
        if requested.tier != self.tier {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "narrowing cannot change the declared model tier",
            ));
        }
        Ok(Self {
            tier: self.tier,
            max_turns: check_u32(requested.max_turns, self.max_turns, "max_turns")?,
            max_tool_calls: check_u32(
                requested.max_tool_calls,
                self.max_tool_calls,
                "max_tool_calls",
            )?,
            max_tokens: check_u64(requested.max_tokens, self.max_tokens, "max_tokens")?,
            max_wall_ms: check_u64(requested.max_wall_ms, self.max_wall_ms, "max_wall_ms")?,
            max_stationary_streak: check_u32(
                requested.max_stationary_streak,
                self.max_stationary_streak,
                "max_stationary_streak",
            )?,
            max_consecutive_waits: check_u32(
                requested.max_consecutive_waits,
                self.max_consecutive_waits,
                "max_consecutive_waits",
            )?,
            max_wait_ms: check_u64(requested.max_wait_ms, self.max_wait_ms, "max_wait_ms")?,
            max_novel_without_mutation: check_u32(
                requested.max_novel_without_mutation,
                self.max_novel_without_mutation,
                "max_novel_without_mutation",
            )?,
        })
    }
}

/// The budget dimension that ran out. Reported verbatim so an exhausted loop
/// can never be described with a vaguer reason than the one that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    Turns,
    ToolCalls,
    Tokens,
    WallClock,
}

impl BudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turns => "turns",
            Self::ToolCalls => "tool_calls",
            Self::Tokens => "tokens",
            Self::WallClock => "wall_clock",
        }
    }
}

/// Externally attested evidence that a wait is still doing something.
///
/// The witness must come from outside the model: a shell session that is still
/// open, a provider-reported retry-after, a permission request that a human has
/// not answered yet. The model's own claim that it is waiting is not a witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitWitness {
    /// Bounded, non-secret label for the kind of wait (`shell`, `permission`,
    /// `rate_limit`, ...). Never free-form model text.
    pub kind: String,
    /// Digest of the external state being waited on.
    pub witness_digest: String,
    /// Monotonic external attempt / poll counter.
    pub attempt: u32,
    /// Remaining time the external system says it needs, if it said anything.
    pub deadline_ms: Option<u64>,
}

impl WaitWitness {
    fn validate(&self) -> Result<(), OrchError> {
        bounded(&self.kind, 64, "wait.kind")?;
        bounded(&self.witness_digest, MAX_DIGEST_BYTES, "wait.witnessDigest")?;
        Ok(())
    }

    /// A wait advances when the external state changed, or when the external
    /// system reports a still-open deadline and its own attempt counter moved.
    fn advanced_over(&self, previous: &WaitWitness) -> bool {
        if self.kind != previous.kind {
            return true;
        }
        if self.witness_digest != previous.witness_digest {
            return true;
        }
        let deadline_open = self.deadline_ms.is_some_and(|ms| ms > 0);
        deadline_open && self.attempt > previous.attempt
    }
}

/// One observed step of the loop, as reported by the caller.
///
/// Counters are cumulative rather than per-step deltas. That is deliberate:
/// after a restart the caller re-derives them from the durable run record, so
/// a replayed step produces the same classification it produced before the
/// crash instead of double-counting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopStep {
    /// Digest of what the agent saw. Caller-computed via [`digest_of`].
    pub observation_digest: String,
    /// Digest of what the agent decided to do about it.
    pub action_digest: String,
    /// Cumulative distinct files changed by this run.
    pub changed_files: u32,
    /// Cumulative recognized test observations for this run.
    pub tests_observed: u32,
    /// Cumulative tool calls issued by this run.
    pub tool_calls: u32,
    /// Cumulative tokens attributed to this run.
    pub tokens: u64,
    /// Cumulative wall-clock milliseconds this run has been alive.
    pub elapsed_ms: u64,
    /// External witness, when this step is a wait rather than an action.
    #[serde(default)]
    pub wait: Option<WaitWitness>,
}

impl LoopStep {
    fn validate(&self) -> Result<(), OrchError> {
        bounded(
            &self.observation_digest,
            MAX_DIGEST_BYTES,
            "observationDigest",
        )?;
        bounded(&self.action_digest, MAX_DIGEST_BYTES, "actionDigest")?;
        if let Some(wait) = self.wait.as_ref() {
            wait.validate()?;
        }
        Ok(())
    }

    fn signature(&self) -> StepSignature {
        StepSignature {
            observation_digest: self.observation_digest.clone(),
            action_digest: self.action_digest.clone(),
        }
    }
}

/// The (observation, action) pair used for repeat detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepSignature {
    pub observation_digest: String,
    pub action_digest: String,
}

/// How one step was classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepClass {
    /// External state changed: files edited or tests observed.
    Mutation,
    /// A signature never seen in the retained window, with no mutation.
    NovelObservation,
    /// A wait whose external witness advanced.
    ProductiveWait,
    /// A wait whose external witness did not advance.
    StalledWait,
    /// A repeat of a retained signature with no mutation.
    NoOp,
}

impl StepClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mutation => "mutation",
            Self::NovelObservation => "novel_observation",
            Self::ProductiveWait => "productive_wait",
            Self::StalledWait => "stalled_wait",
            Self::NoOp => "no_op",
        }
    }

    /// Stationary steps are the ones that consumed budget without moving the
    /// world. A stalled wait counts: pretending to wait is not waiting.
    pub fn is_stationary(self) -> bool {
        matches!(self, Self::NoOp | Self::StalledWait)
    }
}

/// Why the loop is asking for attention. Each variant names the exact
/// condition that fired; there is no generic "stuck".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    /// Repeated equivalent (observation, action) pairs with no external change.
    StationaryLoop,
    /// A wait that no external witness supports.
    UnwitnessedWait,
    /// Novel actions that never change anything.
    InertChurn,
    /// A witnessed wait that outlived its envelope.
    WaitTimeout,
    /// A dispatch whose outcome is unknown. Absorbing; never auto-retried.
    UncertainDispatch,
    /// A budget dimension ran out.
    BudgetExhausted,
}

impl AttentionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StationaryLoop => "stationary_loop",
            Self::UnwitnessedWait => "unwitnessed_wait",
            Self::InertChurn => "inert_churn",
            Self::WaitTimeout => "wait_timeout",
            Self::UncertainDispatch => "uncertain_dispatch",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }

    /// Whether a stronger model could plausibly be handed this condition.
    ///
    /// An uncertain dispatch never can: the side effect may already have
    /// landed, and handing it to a larger model is still a retry.
    pub fn is_model_escalatable(self) -> bool {
        !matches!(self, Self::UncertainDispatch)
    }
}

/// The truthful state of the loop after a step.
///
/// `Stationary` exists precisely so a no-op is never reported as progress. It
/// is a distinct, visible state that still permits another step while the
/// streak is inside the envelope, and it is what the projection shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum LoopDisposition {
    /// The last step changed the world or observed something genuinely new.
    Progressing { class: StepClass },
    /// The last step repeated itself, or waited without a witness, but the
    /// streak is still inside the envelope. Not progress.
    Stationary { class: StepClass, streak: u32 },
    /// Waiting on an external witness that is still advancing.
    Waiting { wait_kind: String, waits: u32 },
    /// The loop cannot claim progress and will not continue on its own.
    /// Absorbing: only a manager-issued [`AttentionGrant`] reopens it.
    NeedsAttention {
        reason: AttentionReason,
        /// Escalation is the only forward path; a human is required when no
        /// stronger model may take it.
        human_required: bool,
    },
    /// A budget dimension ran out. Absorbing, like `NeedsAttention`.
    Exhausted { dimension: BudgetDimension },
}

impl LoopDisposition {
    /// Whether the loop may take another step by itself.
    ///
    /// Both stopped states are absorbing: re-entering them requires an
    /// explicit grant, so nothing here ever auto-retries or quietly resumes.
    pub fn may_continue(&self) -> bool {
        matches!(
            self,
            Self::Progressing { .. } | Self::Stationary { .. } | Self::Waiting { .. }
        )
    }

    /// The reason this loop stopped, when it stopped.
    pub fn attention_reason(&self) -> Option<AttentionReason> {
        match self {
            Self::NeedsAttention { reason, .. } => Some(*reason),
            Self::Exhausted { .. } => Some(AttentionReason::BudgetExhausted),
            _ => None,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Progressing { .. } => "progressing",
            Self::Stationary { .. } => "stationary",
            Self::Waiting { .. } => "waiting",
            Self::NeedsAttention { .. } => "needs_attention",
            Self::Exhausted { .. } => "exhausted",
        }
    }
}

/// A manager- or human-issued authorization to reopen a stopped loop.
///
/// The grant is bound to one run, one exact revision, and the specific reason
/// the loop stopped. A grant issued for a different stop, a superseded
/// revision, or an expired window is refused, so a copied or replayed grant
/// cannot revive a loop the manager did not actually look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionGrant {
    pub run_id: String,
    /// Revision the loop stopped at. Must match exactly.
    pub revision: u64,
    /// The stop this grant answers. Must match the current disposition.
    pub reason: AttentionReason,
    /// Bounded, non-secret identity of the issuer.
    pub issued_by: String,
    /// Optional promotion to a stronger tier. May only move up the ladder,
    /// and only to the tier the escalation ticket actually named.
    #[serde(default)]
    pub promote_to_tier: Option<ModelTier>,
    /// Set only when a human has reconciled an unknown-outcome dispatch by
    /// hand. Without it, an uncertain loop stays stopped.
    #[serde(default)]
    pub acknowledges_uncertain_outcome: bool,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl AttentionGrant {
    fn validate(&self) -> Result<(), OrchError> {
        bounded(&self.run_id, 256, "grant.runId")?;
        bounded(&self.issued_by, 256, "grant.issuedBy")?;
        if self.expires_at <= self.issued_at {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "grant expiry must be after its issue time",
            ));
        }
        Ok(())
    }
}

/// Outcome of a provider dispatch, mirroring the computer-use mutation ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// Nothing has been sent for this revision.
    #[default]
    Idle,
    /// A send is in flight. A crash here becomes `Uncertain`, never a resend.
    Sending,
    Delivered,
    /// The send provably did not reach the provider. Safe to retry under a new
    /// revision because no side effect can have landed.
    Failed,
    /// The outcome is unknown. Absorbing.
    Uncertain,
}

impl DispatchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn is_uncertain(self) -> bool {
        matches!(self, Self::Uncertain)
    }
}

/// A request to hand this loop to a stronger model or to a human.
///
/// The ticket carries digests and counters only. It is safe to surface on an
/// unauthenticated projection and safe to persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationTicket {
    pub run_id: String,
    /// Revision the escalation was cut at. A resume against a different
    /// revision is stale and must be rejected.
    pub revision: u64,
    pub reason: AttentionReason,
    pub from_tier: ModelTier,
    /// `None` when no stronger model may take this; a human must.
    pub to_tier: Option<ModelTier>,
    pub human_required: bool,
    /// Whether the receiving side may resume automatically. Always false when
    /// a dispatch outcome is unknown.
    pub auto_resume_allowed: bool,
    /// Digest binding the ticket to the exact loop state it was cut from.
    pub evidence_digest: String,
    pub issued_at: DateTime<Utc>,
}

/// Durable per-run loop state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopState {
    pub run_id: String,
    /// Monotonic. Bumped once per accepted step and never reused.
    pub revision: u64,
    pub envelope: PolicyEnvelope,
    /// Bounded window of recent signatures, oldest first.
    #[serde(default)]
    pub signatures: Vec<StepSignature>,
    #[serde(default)]
    pub last_wait: Option<WaitWitness>,
    #[serde(default)]
    pub wait_started_ms: Option<u64>,
    #[serde(default)]
    pub stationary_streak: u32,
    #[serde(default)]
    pub wait_streak: u32,
    #[serde(default)]
    pub novel_without_mutation_streak: u32,
    #[serde(default)]
    pub turns: u32,
    #[serde(default)]
    pub changed_files: u32,
    #[serde(default)]
    pub tests_observed: u32,
    #[serde(default)]
    pub tool_calls: u32,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub dispatch: DispatchState,
    pub disposition: LoopDisposition,
    #[serde(default)]
    pub escalation: Option<EscalationTicket>,
    pub updated_at: DateTime<Utc>,
}

impl LoopState {
    pub fn new(run_id: impl Into<String>, tier: ModelTier, now: DateTime<Utc>) -> Self {
        Self {
            run_id: run_id.into(),
            revision: 0,
            envelope: PolicyEnvelope::for_tier(tier),
            signatures: Vec::new(),
            last_wait: None,
            wait_started_ms: None,
            stationary_streak: 0,
            wait_streak: 0,
            novel_without_mutation_streak: 0,
            turns: 0,
            changed_files: 0,
            tests_observed: 0,
            tool_calls: 0,
            tokens: 0,
            elapsed_ms: 0,
            dispatch: DispatchState::Idle,
            disposition: LoopDisposition::Progressing {
                class: StepClass::NovelObservation,
            },
            escalation: None,
            updated_at: now,
        }
    }

    pub fn with_envelope(mut self, envelope: PolicyEnvelope) -> Self {
        self.envelope = envelope;
        self
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        bounded(&self.run_id, 256, "runId")?;
        if self.signatures.len() > SIGNATURE_HISTORY {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "signature history exceeds its bound",
            ));
        }
        for signature in &self.signatures {
            bounded(
                &signature.observation_digest,
                MAX_DIGEST_BYTES,
                "observationDigest",
            )?;
            bounded(&signature.action_digest, MAX_DIGEST_BYTES, "actionDigest")?;
        }
        if let Some(wait) = self.last_wait.as_ref() {
            wait.validate()?;
        }
        Ok(())
    }

    /// Digest binding an escalation ticket to this exact state. Counters and
    /// digests only, so the ticket leaks nothing the projection would not.
    pub fn evidence_digest(&self) -> String {
        hash_payload(&serde_json::json!({
            "runId": self.run_id,
            "revision": self.revision,
            "turns": self.turns,
            "toolCalls": self.tool_calls,
            "tokens": self.tokens,
            "elapsedMs": self.elapsed_ms,
            "changedFiles": self.changed_files,
            "testsObserved": self.tests_observed,
            "stationaryStreak": self.stationary_streak,
            "waitStreak": self.wait_streak,
            "dispatch": self.dispatch.as_str(),
        }))
    }

    /// Fail closed on restart: an in-flight send becomes an unknown outcome.
    ///
    /// Idle / Delivered / Failed states are all known, so they are untouched.
    /// Only `Sending` is ambiguous, and it is exactly the case that must never
    /// be resent. Returns whether the state changed.
    pub fn recover_after_restart(&mut self, now: DateTime<Utc>) -> bool {
        if self.dispatch != DispatchState::Sending {
            return false;
        }
        self.dispatch = DispatchState::Uncertain;
        self.disposition = LoopDisposition::NeedsAttention {
            reason: AttentionReason::UncertainDispatch,
            human_required: true,
        };
        self.escalation = Some(self.escalation_for(AttentionReason::UncertainDispatch, now));
        self.updated_at = now;
        true
    }

    /// Mark a send in flight against an expected revision.
    ///
    /// The revision check is what prevents a duplicate send after a restart:
    /// a recovered caller replaying its step carries the revision it read, and
    /// a state that has already advanced rejects it as stale.
    pub fn begin_dispatch(
        &mut self,
        expected_revision: u64,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        self.require_revision(expected_revision)?;
        if self.dispatch.is_uncertain() {
            return Err(uncertain_error());
        }
        if self.dispatch == DispatchState::Sending {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "a dispatch is already in flight for this revision",
            ));
        }
        // A stopped loop has no authority to talk to a provider. Without this
        // the escalation would be advisory: the caller could stop and send
        // anyway.
        if !self.disposition.may_continue() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "loop is stopped and cannot dispatch until an attention grant is applied",
            ));
        }
        self.dispatch = DispatchState::Sending;
        self.updated_at = now;
        Ok(())
    }

    /// Record a settled dispatch outcome.
    pub fn settle_dispatch(
        &mut self,
        expected_revision: u64,
        outcome: DispatchState,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        self.require_revision(expected_revision)?;
        if self.dispatch.is_uncertain() {
            return Err(uncertain_error());
        }
        if matches!(outcome, DispatchState::Sending) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "settle_dispatch cannot re-open a send",
            ));
        }
        if self.dispatch != DispatchState::Sending {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "no dispatch is in flight to settle",
            ));
        }
        self.dispatch = outcome;
        if outcome.is_uncertain() {
            self.disposition = LoopDisposition::NeedsAttention {
                reason: AttentionReason::UncertainDispatch,
                human_required: true,
            };
            self.escalation = Some(self.escalation_for(AttentionReason::UncertainDispatch, now));
        }
        self.updated_at = now;
        Ok(())
    }

    /// Reopen a stopped loop under a manager-issued grant.
    ///
    /// This is the only path out of `NeedsAttention` / `Exhausted`, and it is
    /// deliberately strict: the grant must name this run, this exact revision,
    /// and this exact stop reason, and it must still be inside its window.
    /// Applying it always advances the revision, which is what makes any
    /// dispatch still in flight against the old revision unreplayable.
    pub fn apply_grant(
        &mut self,
        grant: &AttentionGrant,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        grant.validate()?;
        let Some(reason) = self.disposition.attention_reason() else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "loop is not stopped and does not need a grant",
            ));
        };
        if grant.run_id != self.run_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "grant was issued for a different run",
            ));
        }
        if grant.revision != self.revision {
            return Err(stale_revision_error(grant.revision, self.revision));
        }
        if grant.reason != reason {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "grant was issued for a different stop reason",
            ));
        }
        if now >= grant.expires_at {
            return Err(OrchError::new(OrchErrorCode::Conflict, "grant has expired"));
        }
        // An unknown outcome is only ever cleared by a human who says they
        // reconciled it. No grant, however privileged, retries it implicitly.
        if self.dispatch.is_uncertain() && !grant.acknowledges_uncertain_outcome {
            return Err(uncertain_error());
        }
        if let Some(tier) = grant.promote_to_tier {
            let allowed = self
                .escalation
                .as_ref()
                .and_then(|ticket| ticket.to_tier)
                .filter(|_| {
                    self.escalation
                        .as_ref()
                        .is_some_and(|t| t.revision == self.revision)
                });
            if allowed != Some(tier) {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "grant promotes to a tier this escalation did not authorize",
                ));
            }
            self.envelope = PolicyEnvelope::for_tier(tier);
        }
        // Clearing the stop resets only what the stop was about. Cumulative
        // spend is untouched: a grant reopens the loop, it does not refund it.
        self.revision = self.revision.saturating_add(1);
        self.stationary_streak = 0;
        self.wait_streak = 0;
        self.novel_without_mutation_streak = 0;
        self.wait_started_ms = None;
        self.last_wait = None;
        self.signatures.clear();
        self.dispatch = DispatchState::Idle;
        self.escalation = None;
        self.disposition = LoopDisposition::Progressing {
            class: StepClass::NovelObservation,
        };
        self.updated_at = now;
        Ok(())
    }

    fn require_revision(&self, expected: u64) -> Result<(), OrchError> {
        if expected != self.revision {
            return Err(stale_revision_error(expected, self.revision));
        }
        Ok(())
    }

    fn escalation_for(&self, reason: AttentionReason, now: DateTime<Utc>) -> EscalationTicket {
        // An unknown outcome is never handed to another model: doing so is a
        // retry wearing a different name. Everything else may climb one tier,
        // and a top-tier loop goes to a human.
        let to_tier = if reason.is_model_escalatable() {
            self.envelope.tier.stronger()
        } else {
            None
        };
        EscalationTicket {
            run_id: self.run_id.clone(),
            revision: self.revision,
            reason,
            from_tier: self.envelope.tier,
            to_tier,
            human_required: to_tier.is_none(),
            auto_resume_allowed: to_tier.is_some() && !self.dispatch.is_uncertain(),
            evidence_digest: self.evidence_digest(),
            issued_at: now,
        }
    }
}

/// Result of admitting one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepVerdict {
    pub class: StepClass,
    pub disposition: LoopDisposition,
    pub revision: u64,
    pub escalation: Option<EscalationTicket>,
}

/// Admit one observed step against the durable state.
///
/// `expected_revision` must equal the state's current revision. On success the
/// revision advances by exactly one, so a replayed step is rejected as stale
/// instead of being counted twice.
///
/// Budget checks run before classification: a loop that is already out of
/// budget reports the exhausted dimension rather than a fresh step class.
pub fn admit_step(
    state: &mut LoopState,
    expected_revision: u64,
    step: &LoopStep,
    now: DateTime<Utc>,
) -> Result<StepVerdict, OrchError> {
    step.validate()?;
    state.require_revision(expected_revision)?;

    // Uncertain is absorbing: no further step may be admitted, because the
    // caller cannot know whether the previous action landed.
    if state.dispatch.is_uncertain() {
        return Err(uncertain_error());
    }

    if state.dispatch == DispatchState::Sending {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "a dispatch is still in flight; settle it before admitting the next step",
        ));
    }

    // A stopped loop stays stopped. Without this, admitting another step
    // would silently overwrite the escalation and report progress the loop
    // was never authorized to make.
    if !state.disposition.may_continue() {
        return Err(OrchError::with_data(
            OrchErrorCode::Conflict,
            "loop is stopped and needs an attention grant before it can step again",
            serde_json::json!({
                "disposition": state.disposition.kind_str(),
                "reason": state.disposition.attention_reason().map(|r| r.as_str()),
            }),
        ));
    }

    // Counters are cumulative and must never go backwards.
    if step.changed_files < state.changed_files
        || step.tests_observed < state.tests_observed
        || step.tool_calls < state.tool_calls
        || step.tokens < state.tokens
        || step.elapsed_ms < state.elapsed_ms
    {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "cumulative loop counters cannot decrease",
        ));
    }

    let mutated =
        step.changed_files > state.changed_files || step.tests_observed > state.tests_observed;
    let signature = step.signature();
    let repeated = state.signatures.contains(&signature);

    let class = match (&step.wait, mutated, repeated) {
        // Mutation always wins: something in the world actually changed.
        (_, true, _) => StepClass::Mutation,
        (Some(wait), false, _) => {
            let advanced = match state.last_wait.as_ref() {
                // The first wait on a witness is productive by construction:
                // there is nothing yet to compare it against.
                None => true,
                Some(previous) => wait.advanced_over(previous),
            };
            if advanced {
                StepClass::ProductiveWait
            } else {
                StepClass::StalledWait
            }
        }
        (None, false, true) => StepClass::NoOp,
        (None, false, false) => StepClass::NovelObservation,
    };

    let turns = state.turns.saturating_add(1);

    // Budget is evaluated on the post-step totals so the bound is a real
    // ceiling rather than an off-by-one over it.
    let exhausted = if turns > state.envelope.max_turns {
        Some(BudgetDimension::Turns)
    } else if step.tool_calls > state.envelope.max_tool_calls {
        Some(BudgetDimension::ToolCalls)
    } else if step.tokens > state.envelope.max_tokens {
        Some(BudgetDimension::Tokens)
    } else if step.elapsed_ms > state.envelope.max_wall_ms {
        Some(BudgetDimension::WallClock)
    } else {
        None
    };

    // Streaks.
    let stationary_streak = if class.is_stationary() {
        state.stationary_streak.saturating_add(1)
    } else {
        0
    };
    let wait_streak = match class {
        StepClass::ProductiveWait => state.wait_streak.saturating_add(1),
        _ => 0,
    };
    let novel_streak = match class {
        StepClass::NovelObservation => state.novel_without_mutation_streak.saturating_add(1),
        StepClass::Mutation => 0,
        // Waits and no-ops neither advance nor clear the churn counter: they
        // are accounted for by their own streaks.
        _ => state.novel_without_mutation_streak,
    };
    let wait_started_ms = match class {
        StepClass::ProductiveWait | StepClass::StalledWait => {
            Some(state.wait_started_ms.unwrap_or(step.elapsed_ms))
        }
        _ => None,
    };
    let wait_span_ms = wait_started_ms
        .map(|start| step.elapsed_ms.saturating_sub(start))
        .unwrap_or(0);

    // Disposition. Budget exhaustion is reported before any streak verdict:
    // it is the harder, more specific fact.
    let disposition = if let Some(dimension) = exhausted {
        LoopDisposition::Exhausted { dimension }
    } else if class == StepClass::StalledWait
        && stationary_streak > state.envelope.max_stationary_streak
    {
        needs_attention(AttentionReason::UnwitnessedWait, state.envelope.tier)
    } else if stationary_streak > state.envelope.max_stationary_streak {
        needs_attention(AttentionReason::StationaryLoop, state.envelope.tier)
    } else if wait_streak > state.envelope.max_consecutive_waits
        || wait_span_ms > state.envelope.max_wait_ms
    {
        needs_attention(AttentionReason::WaitTimeout, state.envelope.tier)
    } else if novel_streak > state.envelope.max_novel_without_mutation {
        needs_attention(AttentionReason::InertChurn, state.envelope.tier)
    } else if class == StepClass::ProductiveWait {
        LoopDisposition::Waiting {
            wait_kind: step
                .wait
                .as_ref()
                .map(|w| w.kind.clone())
                .unwrap_or_default(),
            waits: wait_streak,
        }
    } else if class.is_stationary() {
        // Inside tolerance, but still a no-op. Say so.
        LoopDisposition::Stationary {
            class,
            streak: stationary_streak,
        }
    } else {
        LoopDisposition::Progressing { class }
    };

    // Commit.
    state.revision = state.revision.saturating_add(1);
    state.turns = turns;
    state.changed_files = step.changed_files;
    state.tests_observed = step.tests_observed;
    state.tool_calls = step.tool_calls;
    state.tokens = step.tokens;
    state.elapsed_ms = step.elapsed_ms;
    state.stationary_streak = stationary_streak;
    state.wait_streak = wait_streak;
    state.novel_without_mutation_streak = novel_streak;
    state.last_wait = step.wait.clone();
    state.wait_started_ms = wait_started_ms;
    state.disposition = disposition.clone();
    state.updated_at = now;
    if !state.signatures.contains(&signature) {
        state.signatures.push(signature);
        if state.signatures.len() > SIGNATURE_HISTORY {
            state.signatures.remove(0);
        }
    }
    // A settled dispatch does not carry over to the next revision.
    if matches!(
        state.dispatch,
        DispatchState::Delivered | DispatchState::Failed
    ) {
        state.dispatch = DispatchState::Idle;
    }

    let escalation = match &disposition {
        LoopDisposition::NeedsAttention { reason, .. } => Some(state.escalation_for(*reason, now)),
        LoopDisposition::Exhausted { .. } => {
            Some(state.escalation_for(AttentionReason::BudgetExhausted, now))
        }
        _ => None,
    };
    state.escalation = escalation.clone();

    Ok(StepVerdict {
        class,
        disposition,
        revision: state.revision,
        escalation,
    })
}

/// Build a stop.
///
/// `human_required` must agree with what the escalation ticket will say, so it
/// depends on the tier as well as the reason: a top-tier loop has no stronger
/// model to hand to, and an unknown outcome has no model path at all.
fn needs_attention(reason: AttentionReason, tier: ModelTier) -> LoopDisposition {
    let model_can_take_it = reason.is_model_escalatable() && tier.stronger().is_some();
    LoopDisposition::NeedsAttention {
        reason,
        human_required: !model_can_take_it,
    }
}

fn uncertain_error() -> OrchError {
    OrchError::new(
        OrchErrorCode::Conflict,
        "the previous provider dispatch has an uncertain outcome and will not be retried automatically",
    )
}

fn stale_revision_error(expected: u64, actual: u64) -> OrchError {
    OrchError::with_data(
        OrchErrorCode::StaleVersion,
        "loop revision is stale",
        serde_json::json!({ "expectedRevision": expected, "currentRevision": actual }),
    )
}

fn bounded(value: &str, max_bytes: usize, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(|c| c == '\0') {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is empty or exceeds its bound"),
        ));
    }
    Ok(())
}

/// Stable digest for an observation or action.
///
/// Callers pass already-redacted, structured values. The digest is what gets
/// persisted and projected, so raw content never reaches either.
pub fn digest_of(value: &serde_json::Value) -> String {
    hash_payload(value)
}

/// Redacted public projection of a loop.
///
/// Every field is a counter, an enum, a bounded label, or a digest. There is no
/// prompt, path, command, model output, or workspace identity here, so this is
/// safe to expose on a read surface that a run record itself is not.
///
/// Serialize-only: this is an outward projection, and its `&'static str` fields
/// exist so a reader cannot be handed anything but a known label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopProjection {
    pub revision: u64,
    pub tier: &'static str,
    pub disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exhausted_dimension: Option<&'static str>,
    pub human_required: bool,
    pub dispatch: &'static str,
    pub turns: u32,
    pub max_turns: u32,
    pub tool_calls: u32,
    pub max_tool_calls: u32,
    pub tokens: u64,
    pub max_tokens: u64,
    pub elapsed_ms: u64,
    pub max_wall_ms: u64,
    pub changed_files: u32,
    pub tests_observed: u32,
    /// Classification of the most recent step: `mutation`, `novel_observation`,
    /// `productive_wait`, `stalled_wait`, or `no_op`.
    pub last_step_class: &'static str,
    pub stationary_streak: u32,
    pub wait_streak: u32,
    /// Bounded wait-kind label only; never the witnessed content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_digest: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// Project a loop state for an untrusted reader.
pub fn project_loop(state: &LoopState) -> LoopProjection {
    let (disposition, attention_reason, exhausted_dimension, human_required) =
        match &state.disposition {
            LoopDisposition::Progressing { .. } => ("progressing", None, None, false),
            LoopDisposition::Stationary { .. } => ("stationary", None, None, false),
            LoopDisposition::Waiting { .. } => ("waiting", None, None, false),
            LoopDisposition::NeedsAttention {
                reason,
                human_required,
            } => (
                "needs_attention",
                Some(reason.as_str()),
                None,
                *human_required,
            ),
            LoopDisposition::Exhausted { dimension } => (
                "exhausted",
                Some(AttentionReason::BudgetExhausted.as_str()),
                Some(dimension.as_str()),
                false,
            ),
        };
    LoopProjection {
        revision: state.revision,
        tier: state.envelope.tier.as_str(),
        disposition,
        attention_reason,
        exhausted_dimension,
        human_required: human_required
            || state.escalation.as_ref().is_some_and(|e| e.human_required),
        dispatch: state.dispatch.as_str(),
        turns: state.turns,
        max_turns: state.envelope.max_turns,
        tool_calls: state.tool_calls,
        max_tool_calls: state.envelope.max_tool_calls,
        tokens: state.tokens,
        max_tokens: state.envelope.max_tokens,
        elapsed_ms: state.elapsed_ms,
        max_wall_ms: state.envelope.max_wall_ms,
        changed_files: state.changed_files,
        tests_observed: state.tests_observed,
        last_step_class: match &state.disposition {
            LoopDisposition::Progressing { class } | LoopDisposition::Stationary { class, .. } => {
                class.as_str()
            }
            LoopDisposition::Waiting { .. } => StepClass::ProductiveWait.as_str(),
            LoopDisposition::NeedsAttention { .. } | LoopDisposition::Exhausted { .. } => {
                StepClass::NoOp.as_str()
            }
        },
        stationary_streak: state.stationary_streak,
        wait_streak: state.wait_streak,
        wait_kind: state.last_wait.as_ref().map(|w| w.kind.clone()),
        escalation_digest: state.escalation.as_ref().map(|e| e.evidence_digest.clone()),
        updated_at: state.updated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_700_000_000_000 + ms).expect("valid fixture timestamp")
    }

    fn state(tier: ModelTier) -> LoopState {
        LoopState::new("run-fixture", tier, at(0))
    }

    /// A step that changes nothing: same observation, same action, no mutation.
    fn inert_step(elapsed_ms: u64, tool_calls: u32, tokens: u64) -> LoopStep {
        LoopStep {
            observation_digest: digest_of(&serde_json::json!({"screen": "same"})),
            action_digest: digest_of(&serde_json::json!({"tool": "read", "arg": "same"})),
            changed_files: 0,
            tests_observed: 0,
            tool_calls,
            tokens,
            elapsed_ms,
            wait: None,
        }
    }

    fn novel_step(n: u32, elapsed_ms: u64) -> LoopStep {
        LoopStep {
            observation_digest: digest_of(&serde_json::json!({"screen": n})),
            action_digest: digest_of(&serde_json::json!({"tool": "read", "n": n})),
            changed_files: 0,
            tests_observed: 0,
            tool_calls: n,
            tokens: u64::from(n) * 10,
            elapsed_ms,
            wait: None,
        }
    }

    fn wait_step(attempt: u32, witness: &str, elapsed_ms: u64) -> LoopStep {
        LoopStep {
            observation_digest: digest_of(&serde_json::json!({"screen": "waiting"})),
            action_digest: digest_of(&serde_json::json!({"tool": "poll"})),
            changed_files: 0,
            tests_observed: 0,
            tool_calls: 1,
            tokens: 10,
            elapsed_ms,
            wait: Some(WaitWitness {
                kind: "shell".into(),
                witness_digest: digest_of(&serde_json::json!({"w": witness})),
                attempt,
                deadline_ms: Some(5_000),
            }),
        }
    }

    fn try_step(
        state: &mut LoopState,
        step: &LoopStep,
        now: DateTime<Utc>,
    ) -> Result<StepVerdict, OrchError> {
        let revision = state.revision;
        admit_step(state, revision, step, now)
    }

    fn drive(state: &mut LoopState, step: &LoopStep, tick: i64) -> StepVerdict {
        try_step(state, step, at(tick)).expect("step admitted")
    }

    #[test]
    fn a_repeated_no_op_is_never_reported_as_progress() {
        let mut state = state(ModelTier::Small);
        // First sighting is genuinely new.
        let first = drive(&mut state, &inert_step(10, 1, 10), 10);
        assert_eq!(first.class, StepClass::NovelObservation);
        assert!(matches!(
            first.disposition,
            LoopDisposition::Progressing { .. }
        ));

        // Every repeat after that is a no-op, and is labelled one.
        for tick in 1..=2 {
            let verdict = drive(&mut state, &inert_step(10 + tick as u64, 2, 20), 20 + tick);
            assert_eq!(verdict.class, StepClass::NoOp);
            assert!(
                matches!(verdict.disposition, LoopDisposition::Stationary { .. }),
                "a repeat must report as stationary, got {:?}",
                verdict.disposition
            );
        }

        // Past the envelope it stops rather than looping forever.
        let stop = drive(&mut state, &inert_step(20, 3, 30), 40);
        assert_eq!(
            stop.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::StationaryLoop,
                human_required: false,
            }
        );
        assert!(!stop.disposition.may_continue());
    }

    #[test]
    fn spending_tokens_and_tool_calls_is_not_progress() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &inert_step(1, 1, 1_000), 1);
        // Burn heavily while changing nothing at all.
        let verdict = drive(&mut state, &inert_step(2, 20, 90_000), 2);
        assert_eq!(verdict.class, StepClass::NoOp);
        assert!(matches!(
            verdict.disposition,
            LoopDisposition::Stationary { .. }
        ));
        assert_eq!(state.changed_files, 0);
    }

    #[test]
    fn a_mutation_outranks_a_repeated_signature() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &inert_step(1, 1, 10), 1);
        let mut step = inert_step(2, 2, 20);
        step.changed_files = 1;
        let verdict = drive(&mut state, &step, 2);
        assert_eq!(verdict.class, StepClass::Mutation);
        assert_eq!(state.stationary_streak, 0);
    }

    #[test]
    fn a_witnessed_wait_is_productive_and_an_unwitnessed_one_is_not() {
        let mut state = state(ModelTier::Small);
        let first = drive(&mut state, &wait_step(1, "a", 10), 10);
        assert_eq!(first.class, StepClass::ProductiveWait);
        assert!(matches!(first.disposition, LoopDisposition::Waiting { .. }));

        // Witness moved on: still productive.
        let advanced = drive(&mut state, &wait_step(2, "b", 20), 20);
        assert_eq!(advanced.class, StepClass::ProductiveWait);

        // Identical witness, identical attempt: nothing outside is moving.
        let stalled = drive(&mut state, &wait_step(2, "b", 30), 30);
        assert_eq!(stalled.class, StepClass::StalledWait);
        assert!(stalled.class.is_stationary());
    }

    #[test]
    fn an_unwitnessed_wait_stops_the_loop_with_its_own_reason() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &wait_step(1, "a", 10), 10);
        // Three identical polls in a row exceeds max_stationary_streak = 2.
        for tick in 1..=3 {
            drive(&mut state, &wait_step(1, "a", 10 + tick as u64), 20 + tick);
        }
        assert_eq!(
            state.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::UnwitnessedWait,
                human_required: false,
            }
        );
    }

    #[test]
    fn even_a_productive_wait_is_bounded_by_its_envelope() {
        let mut state = state(ModelTier::Small);
        // Each poll genuinely advances, but the wait outlives max_wait_ms.
        let mut attempt = 0;
        let mut elapsed = 0;
        loop {
            attempt += 1;
            elapsed += 20_000;
            let verdict = drive(
                &mut state,
                &wait_step(attempt, &format!("w{attempt}"), elapsed),
                attempt as i64,
            );
            if !verdict.disposition.may_continue() {
                assert_eq!(
                    verdict.disposition,
                    LoopDisposition::NeedsAttention {
                        reason: AttentionReason::WaitTimeout,
                        human_required: false,
                    }
                );
                break;
            }
            assert!(attempt < 20, "a productive wait must not run unbounded");
        }
    }

    #[test]
    fn novel_but_inert_churn_is_bounded() {
        let mut state = state(ModelTier::Small);
        // A fresh action every turn that never changes anything.
        for n in 1..=PolicyEnvelope::small().max_novel_without_mutation {
            let verdict = drive(&mut state, &novel_step(n, u64::from(n)), i64::from(n));
            assert_eq!(verdict.class, StepClass::NovelObservation);
            assert!(verdict.disposition.may_continue());
        }
        let n = PolicyEnvelope::small().max_novel_without_mutation + 1;
        let stop = drive(&mut state, &novel_step(n, u64::from(n)), i64::from(n));
        assert_eq!(
            stop.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::InertChurn,
                human_required: false,
            }
        );
    }

    #[test]
    fn every_budget_dimension_bounds_the_loop() {
        let envelope = PolicyEnvelope::small();

        // Turns. Each step genuinely edits a file, so neither the churn bound
        // nor the stationarity bound can fire first and steal the verdict.
        let mut turns = state(ModelTier::Small);
        let mut n = 0;
        let dimension = loop {
            n += 1;
            let mut step = novel_step(n, u64::from(n));
            step.changed_files = n;
            let verdict = drive(&mut turns, &step, i64::from(n));
            if let LoopDisposition::Exhausted { dimension } = verdict.disposition {
                break dimension;
            }
            assert!(n < 200, "turns budget never fired");
        };
        assert_eq!(dimension, BudgetDimension::Turns);
        assert_eq!(turns.turns, envelope.max_turns + 1);

        // Tokens.
        let mut tokens = state(ModelTier::Small);
        let mut step = novel_step(1, 1);
        step.tokens = envelope.max_tokens + 1;
        let verdict = drive(&mut tokens, &step, 1);
        assert_eq!(
            verdict.disposition,
            LoopDisposition::Exhausted {
                dimension: BudgetDimension::Tokens
            }
        );

        // Wall clock.
        let mut wall = state(ModelTier::Small);
        let mut step = novel_step(1, 1);
        step.elapsed_ms = envelope.max_wall_ms + 1;
        let verdict = drive(&mut wall, &step, 1);
        assert_eq!(
            verdict.disposition,
            LoopDisposition::Exhausted {
                dimension: BudgetDimension::WallClock
            }
        );

        // Tool calls on their own.
        let mut calls = state(ModelTier::Small);
        let mut step = novel_step(1, 1);
        step.tool_calls = envelope.max_tool_calls + 1;
        let verdict = drive(&mut calls, &step, 1);
        assert_eq!(
            verdict.disposition,
            LoopDisposition::Exhausted {
                dimension: BudgetDimension::ToolCalls
            }
        );
    }

    #[test]
    fn a_stale_revision_is_rejected_and_changes_nothing() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &novel_step(1, 1), 1);
        let snapshot = state.clone();
        let error = admit_step(&mut state, 0, &novel_step(2, 2), at(2)).unwrap_err();
        assert_eq!(error.code, OrchErrorCode::StaleVersion);
        assert_eq!(state, snapshot, "a rejected step must not mutate state");
    }

    #[test]
    fn cumulative_counters_cannot_go_backwards() {
        let mut state = state(ModelTier::Small);
        let mut step = novel_step(1, 10);
        step.tokens = 500;
        drive(&mut state, &step, 1);
        let mut regressed = novel_step(2, 20);
        regressed.tokens = 100;
        let error = try_step(&mut state, &regressed, at(2)).unwrap_err();
        assert_eq!(error.code, OrchErrorCode::InvalidRequest);
    }

    #[test]
    fn a_stopped_loop_is_absorbing_until_a_grant_arrives() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &inert_step(1, 1, 10), 1);
        for tick in 2..=4 {
            let _ = try_step(&mut state, &inert_step(tick as u64, 2, 20), at(tick));
        }
        assert!(!state.disposition.may_continue());

        // No amount of further stepping resumes it, and the escalation stands.
        let error = try_step(&mut state, &novel_step(9, 90), at(9)).unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert!(state.escalation.is_some());
    }

    fn grant_for(state: &LoopState, now: DateTime<Utc>) -> AttentionGrant {
        AttentionGrant {
            run_id: state.run_id.clone(),
            revision: state.revision,
            reason: state.disposition.attention_reason().expect("stopped"),
            issued_by: "manager".into(),
            promote_to_tier: None,
            acknowledges_uncertain_outcome: false,
            issued_at: now,
            expires_at: now + chrono::Duration::minutes(5),
        }
    }

    fn stall(state: &mut LoopState) {
        drive(state, &inert_step(1, 1, 10), 1);
        for tick in 2..=4 {
            let _ = admit_step(
                state,
                state.revision,
                &inert_step(tick as u64, 2, 20),
                at(tick),
            );
        }
        assert!(!state.disposition.may_continue());
    }

    #[test]
    fn a_grant_reopens_the_loop_and_invalidates_the_revision_it_was_cut_at() {
        let mut state = state(ModelTier::Small);
        stall(&mut state);
        let stopped_revision = state.revision;
        let grant = grant_for(&state, at(100));
        state.apply_grant(&grant, at(101)).expect("grant applies");

        assert!(state.disposition.may_continue());
        assert_eq!(state.stationary_streak, 0);
        assert!(state.escalation.is_none());
        // Reopening advances the revision, so anything still holding the old
        // one — including a duplicate in-flight send — is now stale.
        assert!(state.revision > stopped_revision);
        assert_eq!(
            admit_step(&mut state, stopped_revision, &novel_step(1, 1), at(102))
                .unwrap_err()
                .code,
            OrchErrorCode::StaleVersion
        );
        // Spend is not refunded by a grant.
        assert!(state.turns >= 4);
    }

    #[test]
    fn a_grant_must_match_the_run_revision_and_reason_and_window() {
        let mut state = state(ModelTier::Small);
        stall(&mut state);
        let base = grant_for(&state, at(100));

        let wrong_run = AttentionGrant {
            run_id: "some-other-run".into(),
            ..base.clone()
        };
        assert_eq!(
            state
                .clone()
                .apply_grant(&wrong_run, at(101))
                .unwrap_err()
                .code,
            OrchErrorCode::Conflict
        );

        let wrong_revision = AttentionGrant {
            revision: base.revision + 1,
            ..base.clone()
        };
        assert_eq!(
            state
                .clone()
                .apply_grant(&wrong_revision, at(101))
                .unwrap_err()
                .code,
            OrchErrorCode::StaleVersion
        );

        let wrong_reason = AttentionGrant {
            reason: AttentionReason::WaitTimeout,
            ..base.clone()
        };
        assert_eq!(
            state
                .clone()
                .apply_grant(&wrong_reason, at(101))
                .unwrap_err()
                .code,
            OrchErrorCode::Conflict
        );

        // Expired.
        let expired_at = base.expires_at;
        assert_eq!(
            state
                .clone()
                .apply_grant(&base, expired_at)
                .unwrap_err()
                .code,
            OrchErrorCode::Conflict
        );

        // And a loop that is not stopped needs no grant at all.
        let mut running = state.clone();
        running.apply_grant(&base, at(101)).expect("applies once");
        assert_eq!(
            running.apply_grant(&base, at(102)).unwrap_err().code,
            OrchErrorCode::Conflict
        );
    }

    #[test]
    fn a_grant_cannot_promote_to_a_tier_the_escalation_did_not_authorize() {
        let mut state = state(ModelTier::Small);
        stall(&mut state);
        let ticket = state.escalation.clone().expect("escalation issued");
        assert_eq!(ticket.to_tier, Some(ModelTier::Large));

        // Small -> Large is what the ticket named, so it is allowed.
        let promote = AttentionGrant {
            promote_to_tier: Some(ModelTier::Large),
            ..grant_for(&state, at(100))
        };
        let mut promoted = state.clone();
        promoted
            .apply_grant(&promote, at(101))
            .expect("authorized promotion");
        assert_eq!(promoted.envelope.tier, ModelTier::Large);
        assert_eq!(
            promoted.envelope.max_turns,
            PolicyEnvelope::large().max_turns
        );

        // A self-promotion the ticket never offered is refused.
        let sideways = AttentionGrant {
            promote_to_tier: Some(ModelTier::Small),
            ..grant_for(&state, at(100))
        };
        assert_eq!(
            state
                .clone()
                .apply_grant(&sideways, at(101))
                .unwrap_err()
                .code,
            OrchErrorCode::ForbiddenScope
        );
    }

    #[test]
    fn a_top_tier_stop_requires_a_human() {
        let mut state = state(ModelTier::Large);
        // Large tolerates 4 stationary steps; 6 repeats clears it.
        drive(&mut state, &inert_step(1, 1, 10), 1);
        for tick in 2..=7 {
            let _ = try_step(&mut state, &inert_step(tick as u64, 2, 20), at(tick));
        }
        assert_eq!(
            state.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::StationaryLoop,
                human_required: true,
            }
        );
        let ticket = state.escalation.clone().expect("escalation issued");
        assert_eq!(ticket.to_tier, None);
        assert!(ticket.human_required);
        assert!(!ticket.auto_resume_allowed);
    }

    #[test]
    fn an_in_flight_dispatch_becomes_uncertain_after_a_restart() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &novel_step(1, 1), 1);
        state
            .begin_dispatch(state.revision, at(2))
            .expect("dispatch begins");
        assert_eq!(state.dispatch, DispatchState::Sending);

        // Process dies here; the store reopens.
        assert!(state.recover_after_restart(at(3)));
        assert_eq!(state.dispatch, DispatchState::Uncertain);
        assert_eq!(
            state.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::UncertainDispatch,
                human_required: true,
            }
        );
        // Recovery is idempotent: a second open does not re-fire it.
        assert!(!state.recover_after_restart(at(4)));
    }

    #[test]
    fn an_uncertain_outcome_is_never_auto_retried_or_escalated_to_a_model() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &novel_step(1, 1), 1);
        state.begin_dispatch(state.revision, at(2)).expect("begins");
        state.recover_after_restart(at(3));

        // No resend.
        assert_eq!(
            state
                .begin_dispatch(state.revision, at(4))
                .unwrap_err()
                .code,
            OrchErrorCode::Conflict
        );
        // No further steps.
        assert_eq!(
            try_step(&mut state, &novel_step(2, 2), at(5))
                .unwrap_err()
                .code,
            OrchErrorCode::Conflict
        );
        // No stronger model may take it: escalating an unknown outcome is
        // still a retry.
        let ticket = state.escalation.clone().expect("escalation issued");
        assert_eq!(ticket.to_tier, None);
        assert!(ticket.human_required);
        assert!(!ticket.auto_resume_allowed);

        // Even a manager grant will not clear it without an explicit human
        // acknowledgement that the outcome was reconciled by hand.
        let plain = grant_for(&state, at(6));
        assert_eq!(
            state.clone().apply_grant(&plain, at(7)).unwrap_err().code,
            OrchErrorCode::Conflict
        );
        let acknowledged = AttentionGrant {
            acknowledges_uncertain_outcome: true,
            ..plain
        };
        state
            .apply_grant(&acknowledged, at(7))
            .expect("human reconciled");
        assert_eq!(state.dispatch, DispatchState::Idle);
    }

    #[test]
    fn a_settled_dispatch_never_reopens_a_send() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &novel_step(1, 1), 1);
        state.begin_dispatch(state.revision, at(2)).expect("begins");
        assert_eq!(
            state
                .settle_dispatch(state.revision, DispatchState::Sending, at(3))
                .unwrap_err()
                .code,
            OrchErrorCode::InvalidRequest
        );
        state
            .settle_dispatch(state.revision, DispatchState::Delivered, at(3))
            .expect("settles");
        // A concurrent worker holding a stale revision cannot settle it again.
        assert_eq!(
            state
                .settle_dispatch(state.revision + 5, DispatchState::Failed, at(4))
                .unwrap_err()
                .code,
            OrchErrorCode::StaleVersion
        );
    }

    #[test]
    fn an_envelope_may_be_narrowed_but_never_widened() {
        let ceiling = PolicyEnvelope::small();

        let tighter = PolicyEnvelope {
            max_turns: 4,
            max_tokens: 1_000,
            ..ceiling
        };
        let merged = ceiling.narrow(&tighter).expect("narrowing is allowed");
        assert_eq!(merged.max_turns, 4);
        assert_eq!(merged.max_tokens, 1_000);

        let wider = PolicyEnvelope {
            max_turns: ceiling.max_turns + 1,
            ..ceiling
        };
        assert_eq!(
            ceiling.narrow(&wider).unwrap_err().code,
            OrchErrorCode::InvalidRequest
        );

        let zeroed = PolicyEnvelope {
            max_tool_calls: 0,
            ..ceiling
        };
        assert_eq!(
            ceiling.narrow(&zeroed).unwrap_err().code,
            OrchErrorCode::InvalidRequest
        );

        // Narrowing is not a tier change.
        let retiered = PolicyEnvelope {
            tier: ModelTier::Large,
            ..ceiling
        };
        assert_eq!(
            ceiling.narrow(&retiered).unwrap_err().code,
            OrchErrorCode::InvalidRequest
        );
    }

    #[test]
    fn the_small_envelope_is_tighter_than_the_large_one_in_every_dimension() {
        let small = PolicyEnvelope::small();
        let large = PolicyEnvelope::large();
        assert!(small.max_turns <= large.max_turns);
        assert!(small.max_tool_calls <= large.max_tool_calls);
        assert!(small.max_tokens <= large.max_tokens);
        assert!(small.max_wall_ms <= large.max_wall_ms);
        assert!(small.max_stationary_streak <= large.max_stationary_streak);
        assert!(small.max_consecutive_waits <= large.max_consecutive_waits);
        assert!(small.max_wait_ms <= large.max_wait_ms);
        assert!(small.max_novel_without_mutation <= large.max_novel_without_mutation);
    }

    #[test]
    fn an_undeclared_tier_does_not_buy_a_larger_budget() {
        let unspecified = PolicyEnvelope::for_tier(ModelTier::Unspecified);
        let small = PolicyEnvelope::small();
        assert_eq!(unspecified.max_turns, small.max_turns);
        assert_eq!(unspecified.max_tokens, small.max_tokens);
        assert_eq!(unspecified.max_wall_ms, small.max_wall_ms);
        assert_eq!(
            unspecified.max_stationary_streak,
            small.max_stationary_streak
        );
        // The record still says the tier was never declared.
        assert_eq!(unspecified.tier, ModelTier::Unspecified);
    }

    #[test]
    fn the_signature_window_stays_bounded() {
        let mut state = state(ModelTier::Large);
        for n in 1..=(SIGNATURE_HISTORY as u32 + 6) {
            let _ = try_step(&mut state, &novel_step(n, u64::from(n)), at(n.into()));
            assert!(state.signatures.len() <= SIGNATURE_HISTORY);
        }
        assert_eq!(state.signatures.len(), SIGNATURE_HISTORY);
    }

    #[test]
    fn the_public_projection_carries_no_content() {
        let mut state = state(ModelTier::Small);
        let secret_observation = serde_json::json!({
            "prompt": "SECRET-PROMPT-MATERIAL",
            "path": "/home/someone/private/notes.md",
        });
        let step = LoopStep {
            observation_digest: digest_of(&secret_observation),
            action_digest: digest_of(&serde_json::json!({"cmd": "cat /etc/shadow"})),
            changed_files: 1,
            tests_observed: 0,
            tool_calls: 1,
            tokens: 42,
            elapsed_ms: 5,
            wait: None,
        };
        drive(&mut state, &step, 1);

        let encoded = serde_json::to_string(&project_loop(&state)).expect("projection encodes");
        for leak in [
            "SECRET-PROMPT-MATERIAL",
            "/home/someone",
            "notes.md",
            "/etc/shadow",
            "run-fixture",
        ] {
            assert!(
                !encoded.contains(leak),
                "projection leaked {leak}: {encoded}"
            );
        }
        // What it does carry is the truthful shape of the loop.
        assert!(encoded.contains("\"disposition\":\"progressing\""));
        assert!(encoded.contains("\"lastStepClass\":\"mutation\""));
        assert!(encoded.contains("\"revision\":1"));
    }

    #[test]
    fn the_projection_reports_a_stop_without_dressing_it_up() {
        let mut state = state(ModelTier::Small);
        stall(&mut state);
        let projection = project_loop(&state);
        assert_eq!(projection.disposition, "needs_attention");
        assert_eq!(
            projection.attention_reason,
            Some(AttentionReason::StationaryLoop.as_str())
        );
        assert_eq!(projection.changed_files, 0);
        assert!(projection.stationary_streak > PolicyEnvelope::small().max_stationary_streak);
        assert!(projection.escalation_digest.is_some());
    }

    #[test]
    fn loop_state_round_trips_through_json_unchanged() {
        let mut state = state(ModelTier::Small);
        drive(&mut state, &wait_step(1, "a", 10), 1);
        let encoded = serde_json::to_vec(&state).expect("encodes");
        let decoded: LoopState = serde_json::from_slice(&encoded).expect("decodes");
        assert_eq!(decoded, state);
        decoded.validate().expect("decoded state is valid");
    }

    #[test]
    fn oversized_digests_and_witnesses_are_refused() {
        let mut state = state(ModelTier::Small);
        let mut step = novel_step(1, 1);
        step.observation_digest = "x".repeat(MAX_DIGEST_BYTES + 1);
        assert_eq!(
            admit_step(&mut state, 0, &step, at(1)).unwrap_err().code,
            OrchErrorCode::InvalidRequest
        );

        let mut step = novel_step(1, 1);
        step.wait = Some(WaitWitness {
            kind: "k".repeat(65),
            witness_digest: "d".into(),
            attempt: 1,
            deadline_ms: None,
        });
        assert_eq!(
            admit_step(&mut state, 0, &step, at(1)).unwrap_err().code,
            OrchErrorCode::InvalidRequest
        );
        assert_eq!(state.revision, 0, "rejected steps never advance revision");
    }
}
