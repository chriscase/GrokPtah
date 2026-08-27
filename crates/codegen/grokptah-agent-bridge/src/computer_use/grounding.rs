//! Semantic observation grounding and recovery for Computer Use (#267).
//!
//! This module turns bounded AX/semantic observations into **stable element
//! identities** and **authorized candidate targets** that a model-facing layer
//! can consume, and it owns the continuity story between observation
//! revisions. It sits strictly *beside* [`super::service::ComputerUseService`]:
//! it never dispatches, never opens the ledger, never invents a second run
//! state machine, and derives authority exclusively from the existing
//! [`ComputerRun`] lifecycle, [`super::types::ActionGrant`] lease, and
//! [`ComputerPolicy`] fences.
//!
//! Invariants:
//!
//! - **Authority is the AX/semantic tree.** Screenshot/OCR/vision input is an
//!   optional enrichment capability used to corroborate or contradict, never
//!   to originate a target. Frame bytes, evidence asset tokens, and content
//!   hashes never appear in any output of this module.
//! - **Continuity is explicit.** A grounding session accepts the next
//!   observation only when the target identity triple, run binding, control
//!   epoch, sequence (`last + 1`), and capture monotonicity all hold. Any
//!   violation makes recovery **sticky**: later, individually plausible
//!   revisions are refused until an explicitly authoritative re-observation
//!   ([`GroundingSession::ingest_authoritative`]) or [`GroundingSession::reset`]
//!   re-bases the session. A gap is never silently absorbed.
//! - **Identity never crosses app/window/session boundaries.** Stable element
//!   identities hash the exact `(app_id, window_id, generation)` triple, and
//!   [`AuthorizedGroundedTarget`] revalidation compares run id, grant id,
//!   target triple, observation binding, and both epochs. A target minted
//!   under one identity can never validate under another.
//! - **Ambiguity is rejected, not guessed.** Duplicate fingerprints resolve
//!   only through an explicit deterministic discriminator (coarse region or
//!   duplicate ordinal) supplied by the caller; otherwise the resolution is
//!   [`GroundingResolution::Ambiguous`] with bounded escalation evidence.
//! - **Raw coordinates require an explicit bounded decision.**
//!   [`CoordinateFallbackDecision`] is the only path that yields a pointer
//!   action, it derives its point from the currently observed element bounds
//!   (never caller-supplied coordinates), and it passes the existing
//!   [`ComputerPolicy::authorize_action`] grant-class and geometry fences.
//! - **Failures are recorded as closed codes.** The bounded failure journal
//!   and [`GroundingSessionProjection`] carry enum codes, counts, sequences,
//!   and ids only — no observed text, no geometry, no hashes — mirroring the
//!   redaction-by-construction bar of [`super::projection`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::policy::ComputerPolicy;
use super::types::{
    validate_id, ActionClass, ComputerAction, ComputerCapabilities, ComputerControlDisposition,
    ComputerError, ComputerErrorCode, ComputerObservation, ComputerResult, ComputerRun,
    PointerButton, SemanticAction, SemanticElement, MAX_LABEL_BYTES,
};

/// Domain-separation prefix for stable identity hashing. Bump the version if
/// the fingerprint facets ever change so stale identities cannot collide with
/// new ones.
const STABLE_ID_DOMAIN: &str = "ptah-grounding-v1";
/// Minimum normalized OCR/vision hint text length that can corroborate or
/// contradict an AX label. Shorter fragments are treated as unmatched noise.
const MIN_HINT_TEXT_CHARS: usize = 2;

// ---------------------------------------------------------------------------
// Policy bounds
// ---------------------------------------------------------------------------

/// Bounds for grounding, mirroring the [`super::types::ComputerUseLimits`]
/// ceiling pattern: every knob has a hard ceiling and zero is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingPolicy {
    /// Maximum candidates returned by enumeration or an ambiguous resolution.
    pub max_candidates: u32,
    /// Maximum bytes for caller-supplied query text (label / role filters).
    pub max_query_bytes: u32,
    /// Maximum candidate label bytes exposed on the compact candidate tier.
    pub max_candidate_label_bytes: u32,
    /// Maximum OCR/vision hints accepted per correlation call.
    pub max_visual_hints: u32,
    /// Minimum region/element overlap (permille of the hint area) before a
    /// hint is considered to land on an element at all.
    pub min_visual_overlap_permille: u32,
    /// Consecutive post-action revisions with identical semantic digest and
    /// screenshot hash before the pipeline is suspected frozen.
    pub max_stationary_repeats: u32,
    /// Bounded failure-journal ring size.
    pub max_failure_records: u32,
}

impl Default for GroundingPolicy {
    fn default() -> Self {
        Self {
            max_candidates: 8,
            max_query_bytes: 256,
            max_candidate_label_bytes: 160,
            max_visual_hints: 32,
            min_visual_overlap_permille: 500,
            max_stationary_repeats: 3,
            max_failure_records: 64,
        }
    }
}

impl GroundingPolicy {
    pub fn ceiling() -> Self {
        Self {
            max_candidates: 32,
            max_query_bytes: MAX_LABEL_BYTES as u32,
            max_candidate_label_bytes: MAX_LABEL_BYTES as u32,
            max_visual_hints: 128,
            min_visual_overlap_permille: 1_000,
            max_stationary_repeats: 32,
            max_failure_records: 256,
        }
    }

    pub fn validate(self) -> ComputerResult<Self> {
        let ceiling = Self::ceiling();
        let valid = self.max_candidates > 0
            && self.max_candidates <= ceiling.max_candidates
            && self.max_query_bytes > 0
            && self.max_query_bytes <= ceiling.max_query_bytes
            && self.max_candidate_label_bytes > 0
            && self.max_candidate_label_bytes <= ceiling.max_candidate_label_bytes
            && self.max_visual_hints > 0
            && self.max_visual_hints <= ceiling.max_visual_hints
            && self.min_visual_overlap_permille > 0
            && self.min_visual_overlap_permille <= ceiling.min_visual_overlap_permille
            && self.max_stationary_repeats > 0
            && self.max_stationary_repeats <= ceiling.max_stationary_repeats
            && self.max_failure_records > 0
            && self.max_failure_records <= ceiling.max_failure_records;
        if !valid {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "grounding policy exceeds a hard ceiling or contains zero",
            ));
        }
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// Stable identity
// ---------------------------------------------------------------------------

/// Content-derived stable identity for one semantic element facet set within
/// one exact `(app_id, window_id, generation)` target identity.
///
/// The identity is a truncated SHA-256 over durable facets only (role,
/// normalized label, advertised action set) plus the target triple, so it is
/// deterministic across revisions, survives tree reordering, and can never
/// collide across incompatible app/window/generation identities. Mutable
/// facets (value, focus, enablement, geometry) are deliberately excluded.
/// Elements sharing every durable facet share the identity; that duplication
/// is surfaced as ambiguity rather than resolved by guessing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StableElementId(String);

impl StableElementId {
    fn derive(run_target: &super::types::ComputerTarget, element: &SemanticElement) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(STABLE_ID_DOMAIN.as_bytes());
        hasher.update([0]);
        hasher.update(run_target.app_id.as_bytes());
        hasher.update([0]);
        hasher.update(run_target.window_id.as_bytes());
        hasher.update([0]);
        hasher.update(run_target.generation.to_be_bytes());
        hasher.update([0]);
        hasher.update(element.role.as_bytes());
        hasher.update([0]);
        if let Some(label) = &element.label {
            hasher.update(normalize_text(label).as_bytes());
        }
        hasher.update([0]);
        for action in &element.actions {
            hasher.update(format!("{action:?}").as_bytes());
            hasher.update([1]);
        }
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(32);
        for byte in digest.iter().take(16) {
            encoded.push_str(&format!("{byte:02x}"));
        }
        Self(encoded)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> ComputerResult<()> {
        if self.0.len() != 32 || !self.0.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid stable element identity",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Failure journal
// ---------------------------------------------------------------------------

/// Closed reason set for grounding refusals and recoveries. These are the only
/// "why" values the module records or projects; free text from the observed
/// surface never enters the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingFailureCode {
    NotGrounded,
    RecoveryRequired,
    RunMismatch,
    TargetIdentityChanged,
    ControlEpochAdvanced,
    ObservationGap,
    ObservationReplayed,
    ObservationRegressed,
    CapturedAtRegressed,
    ObservationFromFuture,
    StaleRevision,
    ObservationTooOld,
    /// A previously minted artifact (target or correlation) was presented
    /// after the grounding epoch it was minted under was superseded. The
    /// artifact is dead; the live session is unaffected.
    TargetEpochSuperseded,
    AuthorityMissing,
    AmbiguousIdentity,
    NoMatch,
    TrackedElementVanished,
    CandidateNotActionable,
    VisualEvidenceMismatch,
    AxVisualContradiction,
    StationaryFrames,
    CoordinateFallbackDenied,
}

impl GroundingFailureCode {
    /// Whether this violation poisons continuity: once observed, ordinary
    /// ingest and resolution are refused until an authoritative
    /// re-observation or an explicit reset.
    fn is_sticky(self) -> bool {
        matches!(
            self,
            Self::TargetIdentityChanged
                | Self::ControlEpochAdvanced
                | Self::ObservationGap
                | Self::ObservationReplayed
                | Self::ObservationRegressed
                | Self::CapturedAtRegressed
                | Self::AxVisualContradiction
                | Self::StationaryFrames
        )
    }
}

/// One bounded failure record. Safe by construction: a closed code, the
/// affected sequence, and the observation id — never observed content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingFailure {
    pub at: DateTime<Utc>,
    pub code: GroundingFailureCode,
    pub sequence: Option<u64>,
    pub observation_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

/// Continuity state of a grounding session. This is deliberately **not** a
/// run lifecycle: [`ComputerRun::state`] and the grant lease stay
/// authoritative for whether anything may happen at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroundingState {
    /// No authoritative baseline yet; only `ingest_authoritative` is accepted.
    AwaitingBaseline,
    /// Continuity holds; ordinary ingest and resolution are available.
    Grounded,
    /// A continuity violation was observed. Sticky until an authoritative
    /// re-observation or explicit reset; ordinary ingest and resolution are
    /// refused, including later revisions that would individually look fine.
    RecoveryRequired,
}

#[derive(Debug, Clone)]
struct GroundedRevision {
    observation_id: String,
    sequence: u64,
    captured_at: DateTime<Utc>,
    semantic_digest: [u8; 32],
    screenshot_sha256: Option<String>,
    /// Stable identity -> element ids in tree order for the revision.
    index: BTreeMap<StableElementId, Vec<String>>,
}

/// Per-run grounding session. Create one per Computer Run and feed it every
/// observation revision the caller intends to act on.
#[derive(Debug)]
pub struct GroundingSession {
    run_id: String,
    target: super::types::ComputerTarget,
    policy: GroundingPolicy,
    state: GroundingState,
    recovery_reason: Option<GroundingFailureCode>,
    /// Increments on every authoritative re-base and reset, fencing every
    /// resolution artifact minted before it.
    grounding_epoch: u64,
    control_epoch: u64,
    current: Option<GroundedRevision>,
    previous_index: Option<BTreeMap<StableElementId, Vec<String>>>,
    stationary_streak: u32,
    last_action_count: u32,
    coordinate_fallback_count: u32,
    failures: VecDeque<GroundingFailure>,
}

impl GroundingSession {
    /// Bind a session to one run. The target identity triple is captured here
    /// and every later interaction must present the same triple.
    pub fn new(run: &ComputerRun, policy: GroundingPolicy) -> ComputerResult<Self> {
        let policy = policy.validate()?;
        run.target.validate()?;
        if run.target.sensitivity.is_hard_denied() {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "grounding cannot bind a hard-denied target",
            ));
        }
        Ok(Self {
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            policy,
            state: GroundingState::AwaitingBaseline,
            recovery_reason: None,
            grounding_epoch: 0,
            control_epoch: run.control_epoch,
            current: None,
            previous_index: None,
            stationary_streak: 0,
            last_action_count: run.action_count,
            coordinate_fallback_count: 0,
            failures: VecDeque::new(),
        })
    }

    pub fn state(&self) -> GroundingState {
        self.state
    }

    pub fn grounding_epoch(&self) -> u64 {
        self.grounding_epoch
    }

    fn record_failure(
        &mut self,
        code: GroundingFailureCode,
        sequence: Option<u64>,
        observation_id: Option<&str>,
        at: DateTime<Utc>,
    ) {
        if self.failures.len() == self.policy.max_failure_records as usize {
            self.failures.pop_front();
        }
        self.failures.push_back(GroundingFailure {
            at,
            code,
            sequence,
            observation_id: observation_id.map(|id| {
                crate::textutil::truncate_at_char_boundary(id, super::types::MAX_ID_BYTES)
                    .to_string()
            }),
        });
        if code.is_sticky() {
            self.state = GroundingState::RecoveryRequired;
            self.recovery_reason = Some(code);
        }
    }

    fn refuse(
        &mut self,
        code: GroundingFailureCode,
        error_code: ComputerErrorCode,
        message: &str,
        sequence: Option<u64>,
        observation_id: Option<&str>,
        at: DateTime<Utc>,
    ) -> ComputerError {
        self.record_failure(code, sequence, observation_id, at);
        ComputerError::new(error_code, message)
    }

    /// Shared binding checks between the session, the durable run record, and
    /// one observation revision. Identity is exact; anything else is a
    /// grounding failure with a closed code.
    fn check_binding(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        if run.run_id != self.run_id {
            return Err(self.refuse(
                GroundingFailureCode::RunMismatch,
                ComputerErrorCode::Unauthorized,
                "grounding session is bound to a different computer run",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if run.target != self.target || observation.target != self.target {
            return Err(self.refuse(
                GroundingFailureCode::TargetIdentityChanged,
                ComputerErrorCode::TargetChanged,
                "observation target identity does not match the grounded target",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        Ok(())
    }

    /// Ordinary continuity ingest: exactly the next revision of the same
    /// identity under the same control epoch. Anything else is refused and,
    /// for continuity violations, poisons the session until an authoritative
    /// re-observation.
    pub fn ingest(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        match self.state {
            GroundingState::AwaitingBaseline => {
                return Err(self.refuse(
                    GroundingFailureCode::NotGrounded,
                    ComputerErrorCode::InvalidState,
                    "grounding has no authoritative baseline yet",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            GroundingState::RecoveryRequired => {
                // The heart of "never silently accept a later event after a
                // gap": even a perfectly plausible next revision is refused
                // while recovery is pending.
                return Err(self.refuse(
                    GroundingFailureCode::RecoveryRequired,
                    ComputerErrorCode::InvalidState,
                    "grounding recovery is pending; an authoritative re-observation is required",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            GroundingState::Grounded => {}
        }
        self.check_binding(run, observation, now)?;
        if run.control_epoch != self.control_epoch {
            return Err(self.refuse(
                GroundingFailureCode::ControlEpochAdvanced,
                ComputerErrorCode::Conflict,
                "run control epoch advanced; grounding must re-base authoritatively",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        observation.validate(&run.limits)?;
        let current = self
            .current
            .as_ref()
            .expect("grounded state always has a current revision");
        let expected = current.sequence.saturating_add(1);
        if observation.sequence != expected {
            let code = if observation.sequence == current.sequence {
                GroundingFailureCode::ObservationReplayed
            } else if observation.sequence < current.sequence {
                GroundingFailureCode::ObservationRegressed
            } else {
                GroundingFailureCode::ObservationGap
            };
            return Err(self.refuse(
                code,
                ComputerErrorCode::StaleObservation,
                "observation sequence broke grounding continuity",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if observation.captured_at < current.captured_at {
            return Err(self.refuse(
                GroundingFailureCode::CapturedAtRegressed,
                ComputerErrorCode::StaleObservation,
                "observation capture time went backwards",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if observation.captured_at > now {
            return Err(self.refuse(
                GroundingFailureCode::ObservationFromFuture,
                ComputerErrorCode::StaleObservation,
                "observation capture time is in the future",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }

        let semantic_digest = semantic_digest(observation);
        let screenshot_sha256 = observation
            .screenshot
            .as_ref()
            .map(|evidence| evidence.content_sha256.clone());
        let actions_advanced = run.action_count > self.last_action_count;
        let unchanged = current.semantic_digest == semantic_digest
            && current.screenshot_sha256 == screenshot_sha256;
        if actions_advanced && unchanged {
            self.stationary_streak = self.stationary_streak.saturating_add(1);
        } else if !unchanged {
            self.stationary_streak = 0;
        }
        if self.stationary_streak >= self.policy.max_stationary_repeats {
            return Err(self.refuse(
                GroundingFailureCode::StationaryFrames,
                ComputerErrorCode::UncertainOutcome,
                "surface did not change across repeated post-action revisions",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }

        self.commit_revision(run, observation, semantic_digest, screenshot_sha256);
        Ok(())
    }

    /// Authoritative re-observation: establishes or re-bases the baseline and
    /// clears sticky recovery. Even here time never runs backwards — an older
    /// or replayed revision is refused rather than adopted.
    pub fn ingest_authoritative(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        self.check_binding(run, observation, now)?;
        observation.validate(&run.limits)?;
        if let Some(current) = &self.current {
            if observation.sequence <= current.sequence {
                let code = if observation.sequence == current.sequence {
                    GroundingFailureCode::ObservationReplayed
                } else {
                    GroundingFailureCode::ObservationRegressed
                };
                return Err(self.refuse(
                    code,
                    ComputerErrorCode::StaleObservation,
                    "authoritative re-observation must advance the sequence",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            if observation.captured_at < current.captured_at {
                return Err(self.refuse(
                    GroundingFailureCode::CapturedAtRegressed,
                    ComputerErrorCode::StaleObservation,
                    "authoritative re-observation capture time went backwards",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
        }
        if observation.captured_at > now {
            return Err(self.refuse(
                GroundingFailureCode::ObservationFromFuture,
                ComputerErrorCode::StaleObservation,
                "authoritative observation capture time is in the future",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        let age = now.signed_duration_since(observation.captured_at);
        if age > Duration::milliseconds(run.limits.max_observation_age_millis as i64) {
            return Err(self.refuse(
                GroundingFailureCode::ObservationTooOld,
                ComputerErrorCode::StaleObservation,
                "authoritative observation is already older than the freshness bound",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }

        let semantic_digest = semantic_digest(observation);
        let screenshot_sha256 = observation
            .screenshot
            .as_ref()
            .map(|evidence| evidence.content_sha256.clone());
        // An authoritative re-base adopts the run's current authority fence.
        self.control_epoch = run.control_epoch;
        self.state = GroundingState::Grounded;
        self.recovery_reason = None;
        self.grounding_epoch = self.grounding_epoch.saturating_add(1);
        self.stationary_streak = 0;
        self.commit_revision(run, observation, semantic_digest, screenshot_sha256);
        Ok(())
    }

    /// Explicit local reset: drop the baseline entirely. The next interaction
    /// must be an authoritative observation.
    pub fn reset(&mut self) {
        self.state = GroundingState::AwaitingBaseline;
        self.recovery_reason = None;
        self.grounding_epoch = self.grounding_epoch.saturating_add(1);
        self.current = None;
        self.previous_index = None;
        self.stationary_streak = 0;
    }

    fn commit_revision(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        semantic_digest: [u8; 32],
        screenshot_sha256: Option<String>,
    ) {
        let mut index: BTreeMap<StableElementId, Vec<String>> = BTreeMap::new();
        for element in &observation.elements {
            index
                .entry(StableElementId::derive(&self.target, element))
                .or_default()
                .push(element.element_id.clone());
        }
        self.previous_index = self.current.take().map(|revision| revision.index);
        self.current = Some(GroundedRevision {
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
            captured_at: observation.captured_at,
            semantic_digest,
            screenshot_sha256,
            index,
        });
        self.last_action_count = run.action_count;
    }

    /// Every gate that must hold before grounding output may be derived:
    /// grounded continuity, exact current revision, freshness, run authority
    /// (state + lease via [`ComputerPolicy`]), agent-owned disposition, and
    /// an unchanged control epoch.
    fn check_resolvable(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        match self.state {
            GroundingState::AwaitingBaseline => {
                return Err(self.refuse(
                    GroundingFailureCode::NotGrounded,
                    ComputerErrorCode::InvalidState,
                    "grounding has no authoritative baseline",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            GroundingState::RecoveryRequired => {
                return Err(self.refuse(
                    GroundingFailureCode::RecoveryRequired,
                    ComputerErrorCode::InvalidState,
                    "grounding recovery is pending; resolution is refused",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            GroundingState::Grounded => {}
        }
        self.check_binding(run, observation, now)?;
        let (current_id, current_sequence, captured_at) = {
            let current = self
                .current
                .as_ref()
                .expect("grounded state always has a current revision");
            (
                current.observation_id.clone(),
                current.sequence,
                current.captured_at,
            )
        };
        if observation.observation_id != current_id || observation.sequence != current_sequence {
            return Err(self.refuse(
                GroundingFailureCode::StaleRevision,
                ComputerErrorCode::StaleObservation,
                "resolution must use the exact ingested current revision",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        // The durable run record must agree that this is still the current
        // observation; a run whose observation was invalidated (or replaced)
        // cannot be resolved against a remembered frame.
        let run_current = run.current_observation.as_ref().filter(|current| {
            current.observation_id == observation.observation_id
                && current.sequence == observation.sequence
        });
        if run_current.is_none() {
            return Err(self.refuse(
                GroundingFailureCode::StaleRevision,
                ComputerErrorCode::StaleObservation,
                "run no longer holds this observation as current",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        let age = now.signed_duration_since(captured_at);
        if age < Duration::zero()
            || age > Duration::milliseconds(run.limits.max_observation_age_millis as i64)
        {
            return Err(self.refuse(
                GroundingFailureCode::ObservationTooOld,
                ComputerErrorCode::StaleObservation,
                "grounded revision is outside the freshness bound",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if run.control_disposition != ComputerControlDisposition::AgentOwned {
            return Err(self.refuse(
                GroundingFailureCode::AuthorityMissing,
                ComputerErrorCode::InvalidState,
                "run is not agent-owned; grounding output is refused",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if run.control_epoch != self.control_epoch {
            return Err(self.refuse(
                GroundingFailureCode::ControlEpochAdvanced,
                ComputerErrorCode::Conflict,
                "run control epoch advanced past the grounded baseline",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        // Reuse the existing lease/lifecycle fence rather than re-deriving it.
        if let Err(error) = ComputerPolicy.authorize_observation(run, now) {
            self.record_failure(
                GroundingFailureCode::AuthorityMissing,
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            );
            return Err(error);
        }
        Ok(())
    }

    fn grant_allows(&self, run: &ComputerRun, action: SemanticAction) -> bool {
        let class = match action {
            SemanticAction::SetValue => ActionClass::TextEntry,
            SemanticAction::Invoke | SemanticAction::Select | SemanticAction::Scroll => {
                ActionClass::Semantic
            }
        };
        run.grant
            .as_ref()
            .is_some_and(|grant| grant.action_classes.contains(&class))
    }

    /// Compact, bounded, deterministic candidate enumeration for one semantic
    /// action: what a small model may see. Values are reduced to presence,
    /// labels are tightly bounded, and geometry is reduced to a coarse
    /// nine-grid region.
    pub fn enumerate_candidates(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        action: SemanticAction,
        visual: Option<&VisualCorrelation>,
        now: DateTime<Utc>,
    ) -> ComputerResult<Vec<GroundedCandidate>> {
        self.check_resolvable(run, observation, now)?;
        if !self.grant_allows(run, action) {
            return Err(self.refuse(
                GroundingFailureCode::AuthorityMissing,
                ComputerErrorCode::ForbiddenAction,
                "grant does not cover the requested semantic action class",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if let Some(visual) = visual {
            self.check_visual_binding(visual, observation, now)?;
        }
        let mut candidates = self.actionable_candidates(observation, action, visual);
        candidates.truncate(self.policy.max_candidates as usize);
        Ok(candidates)
    }

    fn actionable_candidates(
        &self,
        observation: &ComputerObservation,
        action: SemanticAction,
        visual: Option<&VisualCorrelation>,
    ) -> Vec<GroundedCandidate> {
        let current = self
            .current
            .as_ref()
            .expect("checked grounded before candidate derivation");
        let mut duplicate_ordinals: BTreeMap<StableElementId, u32> = BTreeMap::new();
        let mut candidates = Vec::new();
        for element in &observation.elements {
            if !element.enabled
                || element.sensitivity.is_hard_denied()
                || !element.actions.contains(&action)
            {
                continue;
            }
            let stable_id = StableElementId::derive(&self.target, element);
            let duplicates = current
                .index
                .get(&stable_id)
                .map_or(1, |ids| ids.len() as u32);
            let ordinal = duplicate_ordinals
                .entry(stable_id.clone())
                .and_modify(|ordinal| *ordinal += 1)
                .or_insert(1);
            let visual_standing = match visual {
                None => VisualStanding::NotEvaluated,
                Some(correlation) => {
                    if correlation.corroborated.contains_key(&element.element_id) {
                        VisualStanding::Corroborated
                    } else {
                        VisualStanding::Unmatched
                    }
                }
            };
            candidates.push(GroundedCandidate {
                element_id: element.element_id.clone(),
                stable_id,
                role: element.role.clone(),
                label: element.label.as_deref().map(|label| {
                    crate::textutil::truncate_at_char_boundary(
                        label,
                        self.policy.max_candidate_label_bytes as usize,
                    )
                    .to_string()
                }),
                actions: element.actions.clone(),
                focused: element.focused,
                value_present: element.value.is_some(),
                region: element
                    .bounds
                    .map(|bounds| CoarseRegion::locate(bounds, observation.geometry)),
                ambiguity: if duplicates > 1 {
                    AmbiguityClass::DuplicateFingerprint { count: duplicates }
                } else {
                    AmbiguityClass::Unique
                },
                duplicate_ordinal: *ordinal,
                visual: visual_standing,
            });
        }
        // Deterministic order: focused elements first, then tree order.
        candidates.sort_by_key(|candidate| !candidate.focused);
        candidates
    }

    /// Resolve one discriminating query into either a single authorized
    /// target, an explicit ambiguity with escalation evidence, or an explicit
    /// no-match — never a guess.
    pub fn resolve(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        query: &TargetQuery,
        visual: Option<&VisualCorrelation>,
        now: DateTime<Utc>,
    ) -> ComputerResult<GroundingResolution> {
        query.validate(&self.policy)?;
        self.check_resolvable(run, observation, now)?;
        if !self.grant_allows(run, query.action) {
            return Err(self.refuse(
                GroundingFailureCode::AuthorityMissing,
                ComputerErrorCode::ForbiddenAction,
                "grant does not cover the requested semantic action class",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if let Some(visual) = visual {
            self.check_visual_binding(visual, observation, now)?;
        }

        let candidates = self.actionable_candidates(observation, query.action, visual);
        let mut matched: Vec<GroundedCandidate> = candidates
            .into_iter()
            .filter(|candidate| query.matches(candidate))
            .collect();

        if matched.is_empty() {
            let vanished = query.stable_id.as_ref().is_some_and(|stable_id| {
                let in_current = self
                    .current
                    .as_ref()
                    .is_some_and(|revision| revision.index.contains_key(stable_id));
                let in_previous = self
                    .previous_index
                    .as_ref()
                    .is_some_and(|index| index.contains_key(stable_id));
                !in_current && in_previous
            });
            let code = if vanished {
                GroundingFailureCode::TrackedElementVanished
            } else {
                GroundingFailureCode::NoMatch
            };
            self.record_failure(
                code,
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            );
            return Ok(GroundingResolution::NoMatch { code });
        }

        if matched.len() > 1 {
            self.record_failure(
                GroundingFailureCode::AmbiguousIdentity,
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            );
            let candidate_count = matched.len() as u32;
            matched.truncate(self.policy.max_candidates as usize);
            let evidence = EscalationEvidence {
                candidate_count,
                vision_available: observation
                    .screenshot
                    .as_ref()
                    .is_some_and(|evidence| evidence.redacted),
                discriminators_accepted: vec![
                    QueryDiscriminator::Region,
                    QueryDiscriminator::DuplicateOrdinal,
                    QueryDiscriminator::Role,
                ],
            };
            return Ok(GroundingResolution::Ambiguous {
                candidates: matched,
                evidence,
            });
        }

        let candidate = matched.remove(0);
        let grant_id = run
            .grant
            .as_ref()
            .map(|grant| grant.grant_id.clone())
            .expect("authorize_observation verified an active grant");
        let target = AuthorizedGroundedTarget {
            run_id: self.run_id.clone(),
            grant_id,
            app_id: self.target.app_id.clone(),
            window_id: self.target.window_id.clone(),
            generation: self.target.generation,
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
            grounding_epoch: self.grounding_epoch,
            control_epoch: self.control_epoch,
            element_id: candidate.element_id.clone(),
            stable_id: candidate.stable_id.clone(),
            action: query.action,
            issued_at: now,
        };
        Ok(GroundingResolution::Resolved { target, candidate })
    }

    /// Correlate OCR/vision regions produced by an optional local capability
    /// against the grounded revision. Enrichment only: output is categorical
    /// per element; a pixel/AX contradiction poisons the session because the
    /// frame binding itself can no longer be trusted.
    pub fn correlate_visual(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        hints: &[VisualRegionHint],
        now: DateTime<Utc>,
    ) -> ComputerResult<VisualCorrelation> {
        self.check_resolvable(run, observation, now)?;
        if hints.len() > self.policy.max_visual_hints as usize {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "visual hint count exceeds the grounding policy bound",
            ));
        }
        let Some(evidence) = observation.screenshot.as_ref().filter(|shot| shot.redacted) else {
            return Err(self.refuse(
                GroundingFailureCode::VisualEvidenceMismatch,
                ComputerErrorCode::InvalidRequest,
                "revision has no redacted screenshot to correlate against",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        };
        for hint in hints {
            hint.validate()?;
            if !hint
                .evidence_sha256
                .eq_ignore_ascii_case(&evidence.content_sha256)
            {
                // A single hint from a different frame poisons the batch: OCR
                // output that mixes frames cannot be reasoned about safely.
                return Err(self.refuse(
                    GroundingFailureCode::VisualEvidenceMismatch,
                    ComputerErrorCode::Conflict,
                    "visual hint is bound to different frame evidence",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
        }

        let mut corroborated: BTreeMap<String, ElementCorroboration> = BTreeMap::new();
        let mut unmatched_hints = 0_u32;
        for hint in hints {
            let hint_text = normalize_text(&hint.text);
            if hint_text.chars().count() < MIN_HINT_TEXT_CHARS {
                unmatched_hints += 1;
                continue;
            }
            let Some((element, overlap_permille)) = best_overlap(
                observation,
                hint.region,
                self.policy.min_visual_overlap_permille,
            ) else {
                unmatched_hints += 1;
                continue;
            };
            let Some(label) = element.label.as_deref() else {
                unmatched_hints += 1;
                continue;
            };
            let label_text = normalize_text(label);
            let agrees = label_text == hint_text
                || label_text.contains(&hint_text)
                || hint_text.contains(&label_text);
            if !agrees {
                // Pixels claim different text than Accessibility at the same
                // location: the frame/tree pairing is not trustworthy.
                return Err(self.refuse(
                    GroundingFailureCode::AxVisualContradiction,
                    ComputerErrorCode::UncertainOutcome,
                    "visual text contradicts the accessibility label at this region",
                    Some(observation.sequence),
                    Some(&observation.observation_id),
                    now,
                ));
            }
            let stable_id = StableElementId::derive(&self.target, element);
            let entry =
                corroborated
                    .entry(element.element_id.clone())
                    .or_insert(ElementCorroboration {
                        element_id: element.element_id.clone(),
                        stable_id,
                        overlap_permille,
                        hint_count: 0,
                    });
            entry.hint_count = entry.hint_count.saturating_add(1);
            entry.overlap_permille = entry.overlap_permille.max(overlap_permille);
        }

        Ok(VisualCorrelation {
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
            grounding_epoch: self.grounding_epoch,
            evaluated_hints: hints.len() as u32,
            unmatched_hints,
            corroborated,
        })
    }

    fn check_visual_binding(
        &mut self,
        visual: &VisualCorrelation,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        if visual.observation_id != observation.observation_id
            || visual.sequence != observation.sequence
            || visual.grounding_epoch != self.grounding_epoch
        {
            return Err(self.refuse(
                GroundingFailureCode::VisualEvidenceMismatch,
                ComputerErrorCode::StaleObservation,
                "visual correlation is bound to a different revision or epoch",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        Ok(())
    }

    /// Re-validate a previously minted target against the live session, run,
    /// and revision immediately before use. Every fence is exact; a target
    /// can never validate across a different run, grant, identity triple,
    /// revision, or epoch.
    pub fn validate_target_for_dispatch(
        &mut self,
        target: &AuthorizedGroundedTarget,
        run: &ComputerRun,
        observation: &ComputerObservation,
        now: DateTime<Utc>,
    ) -> ComputerResult<()> {
        self.check_resolvable(run, observation, now)?;
        if target.run_id != self.run_id
            || target.app_id != self.target.app_id
            || target.window_id != self.target.window_id
            || target.generation != self.target.generation
        {
            return Err(self.refuse(
                GroundingFailureCode::RunMismatch,
                ComputerErrorCode::Unauthorized,
                "grounded target belongs to a different run or target identity",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if target.grounding_epoch != self.grounding_epoch
            || target.control_epoch != self.control_epoch
        {
            // The artifact is superseded; the session itself is healthy, so
            // this refusal is deliberately not sticky.
            return Err(self.refuse(
                GroundingFailureCode::TargetEpochSuperseded,
                ComputerErrorCode::Conflict,
                "grounded target was minted under a superseded epoch",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if target.observation_id != observation.observation_id
            || target.sequence != observation.sequence
        {
            return Err(self.refuse(
                GroundingFailureCode::StaleRevision,
                ComputerErrorCode::StaleObservation,
                "grounded target is bound to a superseded revision",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        if run
            .grant
            .as_ref()
            .is_none_or(|grant| grant.grant_id != target.grant_id)
        {
            return Err(self.refuse(
                GroundingFailureCode::AuthorityMissing,
                ComputerErrorCode::Unauthorized,
                "grounded target was minted under a different grant",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        let element = observation.element(&target.element_id).filter(|element| {
            element.enabled
                && !element.sensitivity.is_hard_denied()
                && element.actions.contains(&target.action)
                && StableElementId::derive(&self.target, element) == target.stable_id
        });
        if element.is_none() {
            return Err(self.refuse(
                GroundingFailureCode::TrackedElementVanished,
                ComputerErrorCode::TargetChanged,
                "grounded element is no longer present and actionable",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        Ok(())
    }

    /// The only path to a raw pointer action. The decision point is derived
    /// from the currently observed element bounds — callers never supply
    /// coordinates — and the existing grant-class/geometry fences run via
    /// [`ComputerPolicy::authorize_action`]. Backends that do not advertise
    /// pointer fallback refuse here regardless of the grant.
    #[allow(clippy::too_many_arguments)]
    pub fn authorize_coordinate_fallback(
        &mut self,
        run: &ComputerRun,
        observation: &ComputerObservation,
        capabilities: &ComputerCapabilities,
        target: &AuthorizedGroundedTarget,
        reason: CoordinateFallbackReason,
        button: PointerButton,
        now: DateTime<Utc>,
    ) -> ComputerResult<CoordinateFallbackDecision> {
        self.validate_target_for_dispatch(target, run, observation, now)?;
        if !capabilities.pointer_fallback {
            return Err(self.refuse(
                GroundingFailureCode::CoordinateFallbackDenied,
                ComputerErrorCode::ForbiddenAction,
                "backend does not advertise pointer fallback",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        }
        let element = observation
            .element(&target.element_id)
            .expect("validate_target_for_dispatch verified presence");
        let Some(bounds) = element.bounds else {
            return Err(self.refuse(
                GroundingFailureCode::CoordinateFallbackDenied,
                ComputerErrorCode::ForbiddenAction,
                "grounded element has no observed bounds for a pointer decision",
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            ));
        };
        let x = bounds.x + bounds.width / 2.0;
        let y = bounds.y + bounds.height / 2.0;
        let action = ComputerAction::PointerClick { x, y, button };
        // The existing policy fence owns the grant class (PointerFallback),
        // freshness, sensitivity, and geometry checks.
        if let Err(error) = ComputerPolicy.authorize_action(run, observation, &action, now) {
            self.record_failure(
                GroundingFailureCode::CoordinateFallbackDenied,
                Some(observation.sequence),
                Some(&observation.observation_id),
                now,
            );
            return Err(error);
        }
        self.coordinate_fallback_count = self.coordinate_fallback_count.saturating_add(1);
        Ok(CoordinateFallbackDecision {
            action,
            element_id: target.element_id.clone(),
            stable_id: target.stable_id.clone(),
            observation_id: observation.observation_id.clone(),
            sequence: observation.sequence,
            grounding_epoch: self.grounding_epoch,
            reason,
            decided_at: now,
        })
    }

    /// Redaction-safe session view, mirroring the projection bar: state,
    /// epochs, counters, and closed failure codes only.
    pub fn projection(&self) -> GroundingSessionProjection {
        GroundingSessionProjection {
            run_id: self.run_id.clone(),
            state: self.state,
            recovery_reason: self.recovery_reason,
            grounding_epoch: self.grounding_epoch,
            control_epoch: self.control_epoch,
            last_sequence: self.current.as_ref().map(|revision| revision.sequence),
            last_observation_id: self
                .current
                .as_ref()
                .map(|revision| revision.observation_id.clone()),
            tracked_identities: self
                .current
                .as_ref()
                .map_or(0, |revision| revision.index.len() as u32),
            duplicate_identities: self.current.as_ref().map_or(0, |revision| {
                revision.index.values().filter(|ids| ids.len() > 1).count() as u32
            }),
            stationary_streak: self.stationary_streak,
            coordinate_fallback_count: self.coordinate_fallback_count,
            failures: self.failures.iter().cloned().collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Queries and resolutions
// ---------------------------------------------------------------------------

/// How a query label is compared against observed labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelMatch {
    /// Byte-exact equality.
    Exact,
    /// Case-insensitive, whitespace-collapsed equality.
    Normalized,
}

/// Explicit deterministic discriminators a caller may add when a query is
/// ambiguous. Listed in escalation evidence so an orchestrating layer knows
/// what refinement is accepted, without any model-specific protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryDiscriminator {
    Region,
    DuplicateOrdinal,
    Role,
}

/// One bounded, discriminating target query. At least one selector must be
/// present; the semantic action names the authority class being requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetQuery {
    pub action: SemanticAction,
    pub role: Option<String>,
    pub label: Option<String>,
    pub label_match: LabelMatch,
    pub stable_id: Option<StableElementId>,
    /// Coarse nine-grid refinement evaluated against the current revision.
    pub region: Option<CoarseRegion>,
    /// 1-based ordinal among fingerprint duplicates in tree order, evaluated
    /// against the current revision only.
    pub duplicate_ordinal: Option<u32>,
}

impl TargetQuery {
    pub fn validate(&self, policy: &GroundingPolicy) -> ComputerResult<()> {
        if self.role.is_none() && self.label.is_none() && self.stable_id.is_none() {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "target query needs a role, label, or stable identity selector",
            ));
        }
        for text in [self.role.as_deref(), self.label.as_deref()]
            .into_iter()
            .flatten()
        {
            if text.trim().is_empty()
                || text.len() > policy.max_query_bytes as usize
                || text.contains('\0')
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::InvalidRequest,
                    "target query selector is empty, oversized, or contains a null byte",
                ));
            }
        }
        if let Some(stable_id) = &self.stable_id {
            stable_id.validate()?;
        }
        if self.duplicate_ordinal == Some(0) {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "duplicate ordinal is 1-based",
            ));
        }
        Ok(())
    }

    fn matches(&self, candidate: &GroundedCandidate) -> bool {
        if let Some(role) = &self.role {
            if candidate.role != *role {
                return false;
            }
        }
        if let Some(label) = &self.label {
            let Some(candidate_label) = candidate.label.as_deref() else {
                return false;
            };
            let matched = match self.label_match {
                LabelMatch::Exact => candidate_label == label,
                LabelMatch::Normalized => normalize_text(candidate_label) == normalize_text(label),
            };
            if !matched {
                return false;
            }
        }
        if let Some(stable_id) = &self.stable_id {
            if candidate.stable_id != *stable_id {
                return false;
            }
        }
        if let Some(region) = self.region {
            if candidate.region != Some(region) {
                return false;
            }
        }
        if let Some(ordinal) = self.duplicate_ordinal {
            if candidate.duplicate_ordinal != ordinal {
                return false;
            }
        }
        true
    }
}

/// Confidence categories on the visual axis. AX stays the source of
/// authority; corroboration only ever raises confidence, and contradiction is
/// handled before this value exists (it poisons the session instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualStanding {
    NotEvaluated,
    Corroborated,
    Unmatched,
}

/// Identity-collision category for one candidate within one revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AmbiguityClass {
    Unique,
    DuplicateFingerprint { count: u32 },
}

/// Coarse nine-grid location derived from observed bounds. Deliberately
/// low-resolution: enough to disambiguate duplicates without exposing exact
/// geometry on the compact tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoarseRegion {
    NorthWest,
    North,
    NorthEast,
    West,
    Center,
    East,
    SouthWest,
    South,
    SouthEast,
}

impl CoarseRegion {
    fn locate(
        bounds: super::types::ObservationGeometry,
        frame: super::types::ObservationGeometry,
    ) -> Self {
        let center_x = bounds.x + bounds.width / 2.0;
        let center_y = bounds.y + bounds.height / 2.0;
        let column = grid_cell(center_x, frame.width);
        let row = grid_cell(center_y, frame.height);
        match (row, column) {
            (0, 0) => Self::NorthWest,
            (0, 1) => Self::North,
            (0, _) => Self::NorthEast,
            (1, 0) => Self::West,
            (1, 1) => Self::Center,
            (1, _) => Self::East,
            (_, 0) => Self::SouthWest,
            (_, 1) => Self::South,
            (_, _) => Self::SouthEast,
        }
    }
}

fn grid_cell(center: f64, extent: f64) -> u8 {
    if extent <= 0.0 || !center.is_finite() {
        return 1;
    }
    let ratio = (center / extent).clamp(0.0, 1.0);
    if ratio < 1.0 / 3.0 {
        0
    } else if ratio < 2.0 / 3.0 {
        1
    } else {
        2
    }
}

/// Compact candidate: the small-model tier. Bounded label, presence bit for
/// the value, coarse region, ambiguity and visual categories — no raw
/// geometry, no values, no evidence locators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundedCandidate {
    pub element_id: String,
    pub stable_id: StableElementId,
    pub role: String,
    pub label: Option<String>,
    pub actions: BTreeSet<SemanticAction>,
    pub focused: bool,
    pub value_present: bool,
    pub region: Option<CoarseRegion>,
    pub ambiguity: AmbiguityClass,
    pub duplicate_ordinal: u32,
    pub visual: VisualStanding,
}

/// Bounded evidence returned with an ambiguous resolution so a larger model
/// or operator can escalate deterministically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationEvidence {
    pub candidate_count: u32,
    /// Whether a redacted screenshot exists for optional visual enrichment.
    pub vision_available: bool,
    pub discriminators_accepted: Vec<QueryDiscriminator>,
}

/// Outcome of a resolution attempt. Ambiguity and no-match are explicit
/// outcomes with recorded reasons, never silent fallbacks.
// A resolution is a transient return value, never stored in bulk, so the
// size skew between variants is acceptable.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GroundingResolution {
    Resolved {
        target: AuthorizedGroundedTarget,
        candidate: GroundedCandidate,
    },
    Ambiguous {
        candidates: Vec<GroundedCandidate>,
        evidence: EscalationEvidence,
    },
    NoMatch {
        code: GroundingFailureCode,
    },
}

/// A single-revision, single-grant authorization to address one element. The
/// embedded run id, grant id, identity triple, revision binding, and epochs
/// make cross-identity or post-recovery reuse structurally detectable; it
/// must be revalidated via [`GroundingSession::validate_target_for_dispatch`]
/// immediately before any dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizedGroundedTarget {
    pub run_id: String,
    pub grant_id: String,
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub observation_id: String,
    pub sequence: u64,
    pub grounding_epoch: u64,
    pub control_epoch: u64,
    pub element_id: String,
    pub stable_id: StableElementId,
    pub action: SemanticAction,
    pub issued_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Visual correlation inputs and outputs
// ---------------------------------------------------------------------------

/// Provenance of a visual hint. Closed set; no model-specific variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualHintSource {
    Ocr,
    VisionModel,
}

/// Target-relative logical region reported by an OCR/vision capability.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl RegionBox {
    pub fn validate(&self) -> ComputerResult<()> {
        if !self.x.is_finite()
            || !self.y.is_finite()
            || !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid visual hint region",
            ));
        }
        Ok(())
    }
}

/// One OCR/vision observation over the exact current frame evidence. The
/// SHA-256 binding ensures a hint can never be replayed against a different
/// frame; the frame bytes themselves never pass through this module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualRegionHint {
    pub evidence_sha256: String,
    pub region: RegionBox,
    pub text: String,
    pub source: VisualHintSource,
}

impl VisualRegionHint {
    pub fn validate(&self) -> ComputerResult<()> {
        if self.evidence_sha256.len() != 64
            || !self
                .evidence_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "visual hint evidence hash is malformed",
            ));
        }
        self.region.validate()?;
        if self.text.trim().is_empty()
            || self.text.len() > MAX_LABEL_BYTES
            || self.text.contains('\0')
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "visual hint text is empty, oversized, or contains a null byte",
            ));
        }
        Ok(())
    }
}

/// Per-element corroboration category. No hint text is echoed back; the
/// output is ids plus bounded numeric categories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementCorroboration {
    pub element_id: String,
    pub stable_id: StableElementId,
    pub overlap_permille: u32,
    pub hint_count: u32,
}

/// Result of correlating one hint batch with one revision. Bound to the exact
/// observation and grounding epoch so it can never season a later frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualCorrelation {
    pub observation_id: String,
    pub sequence: u64,
    pub grounding_epoch: u64,
    pub evaluated_hints: u32,
    pub unmatched_hints: u32,
    pub corroborated: BTreeMap<String, ElementCorroboration>,
}

// ---------------------------------------------------------------------------
// Coordinate fallback
// ---------------------------------------------------------------------------

/// Closed reasons a caller may cite for requesting the pointer fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateFallbackReason {
    SemanticDispatchUnavailable,
    SemanticDispatchRejected,
    OperatorDirected,
}

/// The explicit bounded decision that authorizes exactly one pointer action
/// at the center of one currently observed element. This record is the audit
/// artifact; the embedded action is the only pointer action grounding will
/// ever emit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinateFallbackDecision {
    pub action: ComputerAction,
    pub element_id: String,
    pub stable_id: StableElementId,
    pub observation_id: String,
    pub sequence: u64,
    pub grounding_epoch: u64,
    pub reason: CoordinateFallbackReason,
    pub decided_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Redaction-safe session view: continuity state, epochs, counters, and the
/// bounded failure journal. Labels, values, geometry, hashes, and evidence
/// tokens are absent from the type, mirroring [`super::projection`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingSessionProjection {
    pub run_id: String,
    pub state: GroundingState,
    pub recovery_reason: Option<GroundingFailureCode>,
    pub grounding_epoch: u64,
    pub control_epoch: u64,
    pub last_sequence: Option<u64>,
    pub last_observation_id: Option<String>,
    pub tracked_identities: u32,
    pub duplicate_identities: u32,
    pub stationary_streak: u32,
    pub coordinate_fallback_count: u32,
    pub failures: Vec<GroundingFailure>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Case-folded, whitespace-collapsed text used for identity facets and
/// label/hint comparison. Deterministic; no locale input.
fn normalize_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.trim().chars() {
        if ch.is_whitespace() {
            pending_space = !normalized.is_empty();
            continue;
        }
        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }
        for lower in ch.to_lowercase() {
            normalized.push(lower);
        }
    }
    normalized
}

/// Digest over the semantic content of one revision, used only for
/// stationary-surface detection. Element ids are excluded because they embed
/// the observation id and always change.
fn semantic_digest(observation: &ComputerObservation) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for element in &observation.elements {
        hasher.update(element.role.as_bytes());
        hasher.update([0]);
        if let Some(label) = &element.label {
            hasher.update(label.as_bytes());
        }
        hasher.update([0]);
        if let Some(value) = &element.value {
            hasher.update(value.as_bytes());
        }
        hasher.update([0]);
        hasher.update([element.enabled as u8, element.focused as u8]);
        if let Some(bounds) = element.bounds {
            for part in [bounds.x, bounds.y, bounds.width, bounds.height] {
                hasher.update(part.to_bits().to_be_bytes());
            }
        }
        for action in &element.actions {
            hasher.update(format!("{action:?}").as_bytes());
        }
        hasher.update([2]);
    }
    hasher.finalize().into()
}

fn best_overlap(
    observation: &ComputerObservation,
    region: RegionBox,
    min_overlap_permille: u32,
) -> Option<(&SemanticElement, u32)> {
    let hint_area = region.width * region.height;
    if hint_area <= 0.0 {
        return None;
    }
    let mut best: Option<(&SemanticElement, u32)> = None;
    for element in &observation.elements {
        let Some(bounds) = element.bounds else {
            continue;
        };
        let left = region.x.max(bounds.x);
        let top = region.y.max(bounds.y);
        let right = (region.x + region.width).min(bounds.x + bounds.width);
        let bottom = (region.y + region.height).min(bounds.y + bounds.height);
        if right <= left || bottom <= top {
            continue;
        }
        let overlap = (right - left) * (bottom - top);
        let permille = ((overlap / hint_area) * 1000.0).round().clamp(0.0, 1000.0) as u32;
        if permille < min_overlap_permille {
            continue;
        }
        if best.is_none_or(|(_, best_permille)| permille > best_permille) {
            best = Some((element, permille));
        }
    }
    best
}

/// Validate that an externally persisted stable id has the expected shape.
/// Provided for callers that round-trip identities through serialized state.
pub fn parse_stable_element_id(value: &str) -> ComputerResult<StableElementId> {
    validate_id("stable_element_id", value)?;
    let id = StableElementId(value.to_string());
    id.validate()?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use uuid::Uuid;

    use super::super::types::{
        ActionGrant, ComputerRunState, ComputerTarget, ComputerUseLimits, EvidenceRef, GrantIssuer,
        ObservationGeometry, Sensitivity,
    };
    use super::*;

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 7,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn element(
        id: &str,
        role: &str,
        label: Option<&str>,
        actions: &[SemanticAction],
        bounds: Option<ObservationGeometry>,
    ) -> SemanticElement {
        SemanticElement {
            element_id: id.into(),
            role: role.into(),
            label: label.map(Into::into),
            value: None,
            bounds,
            enabled: true,
            focused: false,
            sensitivity: Sensitivity::None,
            actions: actions.iter().copied().collect::<BTreeSet<_>>(),
        }
    }

    fn bounds(x: f64, y: f64) -> ObservationGeometry {
        ObservationGeometry {
            x,
            y,
            width: 100.0,
            height: 40.0,
            scale_factor: 1.0,
        }
    }

    fn observation(sequence: u64, at: DateTime<Utc>) -> ComputerObservation {
        ComputerObservation {
            observation_id: format!("obs-{sequence}"),
            sequence,
            target: target(),
            captured_at: at,
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: vec![
                element(
                    &format!("obs-{sequence}-element-0"),
                    "button",
                    Some("Save"),
                    &[SemanticAction::Invoke],
                    Some(bounds(40.0, 40.0)),
                ),
                element(
                    &format!("obs-{sequence}-element-1"),
                    "text_field",
                    Some("Name"),
                    &[SemanticAction::SetValue],
                    Some(bounds(40.0, 120.0)),
                ),
            ],
            elements_truncated: false,
            sensitivity: Sensitivity::None,
        }
    }

    fn ready_run(now: DateTime<Utc>) -> ComputerRun {
        let mut run =
            ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default()).unwrap();
        run.grant = Some(ActionGrant {
            grant_id: "grant-1".into(),
            run_id: run.run_id.clone(),
            target: target(),
            action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            issued_by: GrantIssuer::LocalUser,
            issued_at: now - Duration::seconds(1),
            expires_at: now + Duration::minutes(5),
            uses_remaining: None,
            revoked_at: None,
        });
        run.transition(ComputerRunState::Ready).unwrap();
        run
    }

    fn grounded(now: DateTime<Utc>) -> (GroundingSession, ComputerRun, ComputerObservation) {
        let mut run = ready_run(now);
        let mut session = GroundingSession::new(&run, GroundingPolicy::default()).unwrap();
        let observation = observation(1, now);
        run.current_observation = Some(observation.clone());
        session
            .ingest_authoritative(&run, &observation, now)
            .unwrap();
        (session, run, observation)
    }

    fn invoke_query(label: &str) -> TargetQuery {
        TargetQuery {
            action: SemanticAction::Invoke,
            role: None,
            label: Some(label.into()),
            label_match: LabelMatch::Normalized,
            stable_id: None,
            region: None,
            duplicate_ordinal: None,
        }
    }

    #[test]
    fn baseline_requires_authoritative_ingest() {
        let now = Utc::now();
        let run = ready_run(now);
        let mut session = GroundingSession::new(&run, GroundingPolicy::default()).unwrap();
        let observation = observation(1, now);
        let error = session.ingest(&run, &observation, now).unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
        assert_eq!(session.state(), GroundingState::AwaitingBaseline);
        session
            .ingest_authoritative(&run, &observation, now)
            .unwrap();
        assert_eq!(session.state(), GroundingState::Grounded);
    }

    #[test]
    fn sequence_gap_is_sticky_and_refuses_plausible_successors() {
        let now = Utc::now();
        let (mut session, run, _observation) = grounded(now);
        // Sequence 3 after 1 is a gap.
        let gap = observation(3, now + Duration::milliseconds(10));
        let error = session
            .ingest(&run, &gap, now + Duration::milliseconds(10))
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
        assert_eq!(session.state(), GroundingState::RecoveryRequired);
        // A later, individually plausible revision must still be refused.
        let plausible = observation(4, now + Duration::milliseconds(20));
        let error = session
            .ingest(&run, &plausible, now + Duration::milliseconds(20))
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
        // Authoritative re-observation clears the recovery.
        session
            .ingest_authoritative(&run, &plausible, now + Duration::milliseconds(20))
            .unwrap();
        assert_eq!(session.state(), GroundingState::Grounded);
        let projection = session.projection();
        assert!(projection
            .failures
            .iter()
            .any(|failure| failure.code == GroundingFailureCode::ObservationGap));
    }

    #[test]
    fn replayed_and_regressed_sequences_are_refused_even_authoritatively() {
        let now = Utc::now();
        let (mut session, run, observation_one) = grounded(now);
        let replay = session
            .ingest_authoritative(&run, &observation_one, now)
            .unwrap_err();
        assert_eq!(replay.code, ComputerErrorCode::StaleObservation);
        let mut regressed = observation(0, now);
        regressed.sequence = 0;
        let error = session
            .ingest_authoritative(&run, &regressed, now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn duplicate_labels_resolve_to_explicit_ambiguity_with_escalation() {
        let now = Utc::now();
        let (mut session, mut run, mut observation_one) = grounded(now);
        observation_one.elements.push(element(
            "obs-1-element-2",
            "button",
            Some("Save"),
            &[SemanticAction::Invoke],
            Some(bounds(700.0, 500.0)),
        ));
        run.current_observation = Some(observation_one.clone());
        session
            .ingest_authoritative(
                &run,
                &ComputerObservation {
                    sequence: 2,
                    observation_id: "obs-1b".into(),
                    ..observation_one.clone()
                },
                now,
            )
            .unwrap();
        let observation_two = ComputerObservation {
            sequence: 2,
            observation_id: "obs-1b".into(),
            ..observation_one
        };
        run.current_observation = Some(observation_two.clone());
        let resolution = session
            .resolve(&run, &observation_two, &invoke_query("Save"), None, now)
            .unwrap();
        let GroundingResolution::Ambiguous {
            candidates,
            evidence,
        } = resolution
        else {
            panic!("expected ambiguity");
        };
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|candidate| matches!(
            candidate.ambiguity,
            AmbiguityClass::DuplicateFingerprint { count: 2 }
        )));
        assert_eq!(evidence.candidate_count, 2);
        // Region refinement resolves deterministically.
        let refined = TargetQuery {
            region: Some(CoarseRegion::SouthEast),
            ..invoke_query("Save")
        };
        let resolution = session
            .resolve(&run, &observation_two, &refined, None, now)
            .unwrap();
        assert!(matches!(resolution, GroundingResolution::Resolved { .. }));
    }

    #[test]
    fn stable_identity_survives_reordering_and_scopes_to_target_identity() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        let save_id = {
            let resolution = session
                .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
                .unwrap();
            let GroundingResolution::Resolved { target, .. } = resolution else {
                panic!("expected resolution");
            };
            target.stable_id
        };
        // Reorder the tree in revision 2; the stable id must follow the
        // element, not the index.
        let mut observation_two = observation(2, now + Duration::milliseconds(5));
        observation_two.elements.reverse();
        run.current_observation = Some(observation_two.clone());
        session
            .ingest(&run, &observation_two, now + Duration::milliseconds(5))
            .unwrap();
        let query = TargetQuery {
            stable_id: Some(save_id.clone()),
            label: None,
            ..invoke_query("ignored")
        };
        let resolution = session
            .resolve(
                &run,
                &observation_two,
                &query,
                None,
                now + Duration::milliseconds(5),
            )
            .unwrap();
        let GroundingResolution::Resolved {
            target: resolved, ..
        } = resolution
        else {
            panic!("expected stable-id resolution");
        };
        assert_eq!(resolved.stable_id, save_id);
        assert_eq!(resolved.element_id, "obs-2-element-0");

        // The same facets under a different window generation produce a
        // different identity.
        let mut other_target = target();
        other_target.generation = 8;
        let foreign = StableElementId::derive(&other_target, &observation_one.elements[0]);
        assert_ne!(foreign, save_id);
    }

    #[test]
    fn target_identity_change_is_sticky() {
        let now = Utc::now();
        let (mut session, run, _observation) = grounded(now);
        let mut swapped = observation(2, now);
        swapped.target.generation = 99;
        let error = session.ingest(&run, &swapped, now).unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::TargetChanged);
        assert_eq!(session.state(), GroundingState::RecoveryRequired);
        assert_eq!(
            session.projection().recovery_reason,
            Some(GroundingFailureCode::TargetIdentityChanged)
        );
    }

    #[test]
    fn resolution_requires_exact_current_revision_and_run_agreement() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        // A remembered older frame is refused.
        let mut stale = observation_one.clone();
        stale.observation_id = "obs-forged".into();
        let error = session
            .resolve(&run, &stale, &invoke_query("Save"), None, now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
        // The durable run dropping the observation also refuses resolution.
        run.current_observation = None;
        let error = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn authority_gaps_refuse_resolution() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        run.grant = None;
        let error = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Unauthorized);
        assert!(session
            .projection()
            .failures
            .iter()
            .any(|failure| failure.code == GroundingFailureCode::AuthorityMissing));
    }

    #[test]
    fn takeover_disposition_refuses_grounding_output() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        run.set_control_disposition(ComputerControlDisposition::OperatorTakeover);
        let error = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::InvalidState);
    }

    #[test]
    fn stationary_frames_after_actions_trip_recovery() {
        let now = Utc::now();
        let (mut session, mut run, _first) = grounded(now);
        for step in 0_u32..3 {
            run.action_count += 1;
            let sequence = u64::from(step) + 2;
            let at = now + Duration::milliseconds(i64::from(step) + 1);
            let repeat = observation(sequence, at);
            let result = session.ingest(&run, &repeat, at);
            if step < 2 {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
                assert_eq!(session.state(), GroundingState::RecoveryRequired);
            }
        }
    }

    #[test]
    fn stationary_streak_ignores_actionless_polling() {
        let now = Utc::now();
        let (mut session, run, _first) = grounded(now);
        for step in 0_u32..6 {
            let sequence = u64::from(step) + 2;
            let at = now + Duration::milliseconds(i64::from(step) + 1);
            session
                .ingest(&run, &observation(sequence, at), at)
                .unwrap();
        }
        assert_eq!(session.state(), GroundingState::Grounded);
        assert_eq!(session.projection().stationary_streak, 0);
    }

    #[test]
    fn visual_contradiction_poisons_the_session() {
        let now = Utc::now();
        let (mut session, mut run, mut observation_one) = grounded(now);
        observation_one.screenshot = Some(EvidenceRef {
            content_sha256: "b".repeat(64),
            media_type: "image/png".into(),
            byte_len: 512,
            width: 900,
            height: 600,
            redacted: true,
            asset_id: "asset-1".into(),
        });
        let refreshed = ComputerObservation {
            sequence: 2,
            observation_id: "obs-2v".into(),
            ..observation_one
        };
        run.current_observation = Some(refreshed.clone());
        session.ingest_authoritative(&run, &refreshed, now).unwrap();
        let hostile = VisualRegionHint {
            evidence_sha256: "b".repeat(64),
            region: RegionBox {
                x: 40.0,
                y: 40.0,
                width: 100.0,
                height: 40.0,
            },
            text: "Delete Everything".into(),
            source: VisualHintSource::Ocr,
        };
        let error = session
            .correlate_visual(&run, &refreshed, &[hostile], now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::UncertainOutcome);
        assert_eq!(session.state(), GroundingState::RecoveryRequired);
        assert_eq!(
            session.projection().recovery_reason,
            Some(GroundingFailureCode::AxVisualContradiction)
        );
    }

    #[test]
    fn visual_hints_must_bind_to_the_exact_frame_hash() {
        let now = Utc::now();
        let (mut session, mut run, mut observation_one) = grounded(now);
        observation_one.screenshot = Some(EvidenceRef {
            content_sha256: "c".repeat(64),
            media_type: "image/png".into(),
            byte_len: 512,
            width: 900,
            height: 600,
            redacted: true,
            asset_id: "asset-2".into(),
        });
        let refreshed = ComputerObservation {
            sequence: 2,
            observation_id: "obs-2w".into(),
            ..observation_one
        };
        run.current_observation = Some(refreshed.clone());
        session.ingest_authoritative(&run, &refreshed, now).unwrap();
        let wrong_frame = VisualRegionHint {
            evidence_sha256: "d".repeat(64),
            region: RegionBox {
                x: 40.0,
                y: 40.0,
                width: 100.0,
                height: 40.0,
            },
            text: "Save".into(),
            source: VisualHintSource::Ocr,
        };
        let error = session
            .correlate_visual(&run, &refreshed, &[wrong_frame], now)
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::Conflict);
    }

    #[test]
    fn corroboration_upgrades_candidates_and_binds_to_the_revision() {
        let now = Utc::now();
        let (mut session, mut run, mut observation_one) = grounded(now);
        observation_one.screenshot = Some(EvidenceRef {
            content_sha256: "e".repeat(64),
            media_type: "image/png".into(),
            byte_len: 512,
            width: 900,
            height: 600,
            redacted: true,
            asset_id: "asset-3".into(),
        });
        let refreshed = ComputerObservation {
            sequence: 2,
            observation_id: "obs-2x".into(),
            ..observation_one
        };
        run.current_observation = Some(refreshed.clone());
        session.ingest_authoritative(&run, &refreshed, now).unwrap();
        let hint = VisualRegionHint {
            evidence_sha256: "e".repeat(64),
            region: RegionBox {
                x: 42.0,
                y: 42.0,
                width: 90.0,
                height: 30.0,
            },
            text: "save".into(),
            source: VisualHintSource::Ocr,
        };
        let correlation = session
            .correlate_visual(&run, &refreshed, &[hint], now)
            .unwrap();
        assert_eq!(correlation.corroborated.len(), 1);
        let candidates = session
            .enumerate_candidates(
                &run,
                &refreshed,
                SemanticAction::Invoke,
                Some(&correlation),
                now,
            )
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].visual, VisualStanding::Corroborated);

        // The correlation cannot season a later revision.
        let observation_three = observation(3, now + Duration::milliseconds(5));
        run.current_observation = Some(observation_three.clone());
        session
            .ingest(&run, &observation_three, now + Duration::milliseconds(5))
            .unwrap();
        let error = session
            .enumerate_candidates(
                &run,
                &observation_three,
                SemanticAction::Invoke,
                Some(&correlation),
                now + Duration::milliseconds(5),
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn coordinate_fallback_needs_capability_grant_and_current_bounds() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        let GroundingResolution::Resolved { target, .. } = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap()
        else {
            panic!("expected resolution");
        };
        let semantic_only = ComputerCapabilities {
            backend_id: "fixture".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: false,
            pointer_fallback: false,
        };
        let error = session
            .authorize_coordinate_fallback(
                &run,
                &observation_one,
                &semantic_only,
                &target,
                CoordinateFallbackReason::SemanticDispatchRejected,
                PointerButton::Primary,
                now,
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);

        // Capability present but the grant lacks the PointerFallback class.
        let capable = ComputerCapabilities {
            pointer_fallback: true,
            ..semantic_only
        };
        let error = session
            .authorize_coordinate_fallback(
                &run,
                &observation_one,
                &capable,
                &target,
                CoordinateFallbackReason::SemanticDispatchRejected,
                PointerButton::Primary,
                now,
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::ForbiddenAction);

        // With the class granted, the decision derives the point from the
        // observed bounds center.
        run.grant.as_mut().unwrap().action_classes = BTreeSet::from([
            ActionClass::Semantic,
            ActionClass::TextEntry,
            ActionClass::PointerFallback,
        ]);
        let decision = session
            .authorize_coordinate_fallback(
                &run,
                &observation_one,
                &capable,
                &target,
                CoordinateFallbackReason::SemanticDispatchRejected,
                PointerButton::Primary,
                now,
            )
            .unwrap();
        let ComputerAction::PointerClick { x, y, .. } = decision.action else {
            panic!("expected a pointer action");
        };
        assert_eq!((x, y), (90.0, 60.0));
        assert_eq!(session.projection().coordinate_fallback_count, 1);
    }

    #[test]
    fn coordinate_drift_across_revisions_invalidates_the_fallback_target() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        let GroundingResolution::Resolved { target, .. } = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap()
        else {
            panic!("expected resolution");
        };
        run.grant.as_mut().unwrap().action_classes =
            BTreeSet::from([ActionClass::Semantic, ActionClass::PointerFallback]);
        let capable = ComputerCapabilities {
            backend_id: "fixture".into(),
            observe: true,
            semantic_actions: true,
            text_entry: true,
            key_chords: false,
            pointer_fallback: true,
        };
        // The element drifted in revision 2; the old target is bound to
        // revision 1 and must fail closed rather than click stale bounds.
        let mut observation_two = observation(2, now + Duration::milliseconds(5));
        observation_two.elements[0].bounds = Some(bounds(600.0, 400.0));
        run.current_observation = Some(observation_two.clone());
        session
            .ingest(&run, &observation_two, now + Duration::milliseconds(5))
            .unwrap();
        let error = session
            .authorize_coordinate_fallback(
                &run,
                &observation_two,
                &capable,
                &target,
                CoordinateFallbackReason::SemanticDispatchRejected,
                PointerButton::Primary,
                now + Duration::milliseconds(5),
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn disappearing_target_is_reported_as_vanished() {
        let now = Utc::now();
        let (mut session, mut run, observation_one) = grounded(now);
        let GroundingResolution::Resolved { target, .. } = session
            .resolve(&run, &observation_one, &invoke_query("Save"), None, now)
            .unwrap()
        else {
            panic!("expected resolution");
        };
        let mut observation_two = observation(2, now + Duration::milliseconds(5));
        observation_two.elements.remove(0);
        run.current_observation = Some(observation_two.clone());
        session
            .ingest(&run, &observation_two, now + Duration::milliseconds(5))
            .unwrap();
        let query = TargetQuery {
            stable_id: Some(target.stable_id.clone()),
            label: None,
            ..invoke_query("ignored")
        };
        let resolution = session
            .resolve(
                &run,
                &observation_two,
                &query,
                None,
                now + Duration::milliseconds(5),
            )
            .unwrap();
        assert_eq!(
            resolution,
            GroundingResolution::NoMatch {
                code: GroundingFailureCode::TrackedElementVanished
            }
        );
        // The stale target also refuses dispatch validation.
        let error = session
            .validate_target_for_dispatch(
                &target,
                &run,
                &observation_two,
                now + Duration::milliseconds(5),
            )
            .unwrap_err();
        assert_eq!(error.code, ComputerErrorCode::StaleObservation);
    }

    #[test]
    fn projection_serializes_only_pinned_safe_keys() {
        let now = Utc::now();
        let (mut session, run, observation_one) = grounded(now);
        let _ = session.resolve(&run, &observation_one, &invoke_query("Absent"), None, now);
        let projection = session.projection();
        let encoded = serde_json::to_value(&projection).unwrap();
        let keys: BTreeSet<&str> = encoded
            .as_object()
            .expect("projection is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "runId",
                "state",
                "recoveryReason",
                "groundingEpoch",
                "controlEpoch",
                "lastSequence",
                "lastObservationId",
                "trackedIdentities",
                "duplicateIdentities",
                "stationaryStreak",
                "coordinateFallbackCount",
                "failures",
            ]),
            "adding a projection field must consciously widen this pin"
        );
        let wire = serde_json::to_string(&projection).unwrap();
        assert!(!wire.contains("Save"));
        assert!(!wire.contains("Name"));
    }

    #[test]
    fn candidates_are_compact_and_never_leak_values_or_geometry() {
        let now = Utc::now();
        let (mut session, mut run, mut observation_one) = grounded(now);
        observation_one.elements[1].value = Some("PRIVATE_DOCUMENT_TEXT".into());
        let refreshed = ComputerObservation {
            sequence: 2,
            observation_id: "obs-2c".into(),
            ..observation_one
        };
        run.current_observation = Some(refreshed.clone());
        session.ingest_authoritative(&run, &refreshed, now).unwrap();
        let candidates = session
            .enumerate_candidates(&run, &refreshed, SemanticAction::SetValue, None, now)
            .unwrap();
        let encoded = serde_json::to_string(&candidates).unwrap();
        assert!(!encoded.contains("PRIVATE_DOCUMENT_TEXT"));
        assert!(!encoded.contains("bounds"));
        assert!(encoded.contains("\"valuePresent\":true"));
        let encoded_candidate = serde_json::to_value(&candidates[0]).unwrap();
        let keys: BTreeSet<&str> = encoded_candidate
            .as_object()
            .expect("candidate is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "elementId",
                "stableId",
                "role",
                "label",
                "actions",
                "focused",
                "valuePresent",
                "region",
                "ambiguity",
                "duplicateOrdinal",
                "visual",
            ]),
            "adding a candidate field must consciously widen this pin"
        );
    }

    #[test]
    fn policy_ceilings_reject_escalation() {
        let policy = GroundingPolicy {
            max_candidates: GroundingPolicy::ceiling().max_candidates + 1,
            ..Default::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        let zero = GroundingPolicy {
            max_stationary_repeats: 0,
            ..Default::default()
        };
        assert_eq!(
            zero.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn query_requires_a_selector_and_bounded_text() {
        let policy = GroundingPolicy::default();
        let unselective = TargetQuery {
            action: SemanticAction::Invoke,
            role: None,
            label: None,
            label_match: LabelMatch::Exact,
            stable_id: None,
            region: Some(CoarseRegion::Center),
            duplicate_ordinal: Some(1),
        };
        assert_eq!(
            unselective.validate(&policy).unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        let oversized = TargetQuery {
            label: Some("x".repeat(policy.max_query_bytes as usize + 1)),
            ..invoke_query("Save")
        };
        assert_eq!(
            oversized.validate(&policy).unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }
}
