//! Typed contract for the host-neutral headless agent port.
//!
//! Every type here is deliberately closed. An embedder receives classified
//! enums, counts, fingerprints, and durable IDs — never prompts, filesystem
//! paths, credentials, provider payloads, or raw model output. Redaction is a
//! property of the *shape*: the leaking fields are absent from the types
//! rather than filtered at a transport boundary, so no serializer, log line,
//! or future adapter can reintroduce them.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Wire/behaviour version of the whole port surface. An embedder pins this and
/// a host that negotiates a different value is refused rather than adapted.
pub const HEADLESS_PORT_PROTOCOL_VERSION: u32 = 1;

/// Stable schema identifier recorded in embedding documentation.
pub const HEADLESS_PORT_SCHEMA: &str = "grokptah.headless-port.v1";

/// Page size used when a caller does not choose one.
pub const DEFAULT_PORT_EVENT_PAGE: usize = 100;

/// Hard ceiling on one event page, independent of negotiated limits.
pub const MAX_PORT_EVENT_PAGE: usize = 500;

/// Ceiling for every caller-supplied identifier (request id, run id).
pub const MAX_PORT_ID_BYTES: usize = 256;

/// Closed failure classification. Codes are the contract; messages are fixed
/// host-authored text (see [`PortError::new`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortErrorCode {
    /// The principal is not a recognized identity on this host.
    Unauthenticated,
    /// Unknown, cross-session, cross-workspace, or malformed resource. Every
    /// such failure is byte-identical so a scoped read is not an existence
    /// oracle.
    ForbiddenScope,
    /// Host identity, capability revision, or protocol version moved after the
    /// binding was minted. The caller must renegotiate and rebind.
    StaleBinding,
    /// The host does not declare this operation at the negotiated revision.
    Unsupported,
    /// The request exceeds a freshly negotiated limit.
    LimitExceeded,
    /// Structurally invalid request.
    InvalidRequest,
    /// The requested cursor is below the retained journal window.
    CursorExpired,
    /// A durable claim under this request id may or may not have taken
    /// effect. Never auto-replayed; a new request id is required.
    Uncertain,
    /// A durable claim under this request id is still in flight.
    Conflict,
    /// The host is reachable but cannot serve the operation right now.
    Unavailable,
    /// Host-side defect.
    Internal,
}

impl PortErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::ForbiddenScope => "forbidden_scope",
            Self::StaleBinding => "stale_binding",
            Self::Unsupported => "unsupported",
            Self::LimitExceeded => "limit_exceeded",
            Self::InvalidRequest => "invalid_request",
            Self::CursorExpired => "cursor_expired",
            Self::Uncertain => "uncertain",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

/// Typed port failure.
///
/// `message` is `String` only so the value round-trips through serde. The one
/// constructor accepts `&'static str`, so a message can never carry model
/// prose, a provider error body, a filesystem path, or a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortError {
    pub code: PortErrorCode,
    pub message: String,
}

impl PortError {
    pub fn new(code: PortErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }
}

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for PortError {}

pub type PortResult<T> = Result<T, PortError>;

/// The single scope failure. Unknown run, another session's run, another
/// workspace's run, and a traversal-shaped id all produce this exact value.
pub fn scope_denied() -> PortError {
    PortError::new(
        PortErrorCode::ForbiddenScope,
        "resource is not available to this binding",
    )
}

/// Operations the port exposes. These double as the host's stable capability
/// identifiers: a host declares the subset it supports at each capability
/// revision, and an undeclared operation fails closed as `unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortOperation {
    Submit,
    Events,
    Review,
    Cancel,
}

impl PortOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::Events => "events",
            Self::Review => "review",
            Self::Cancel => "cancel",
        }
    }

    /// Operations that can cause a durable effect. These take the write path:
    /// idempotent claim, effect-boundary authorization recheck, and a durable
    /// delivery state.
    pub fn is_mutation(self) -> bool {
        matches!(self, Self::Submit | Self::Cancel)
    }
}

/// Authority tiers from ADR-002 §5. Delegation may only narrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortTier {
    LocalOperator,
    Coordinator,
    Worker,
    Observer,
}

impl PortTier {
    /// Lower rank is broader authority.
    fn rank(self) -> u8 {
        match self {
            Self::LocalOperator => 0,
            Self::Coordinator => 1,
            Self::Worker => 2,
            Self::Observer => 3,
        }
    }

    /// Tier-permitted operations before host capabilities are consulted. The
    /// port takes the intersection of this set and the declared capabilities.
    pub fn permits(self, operation: PortOperation) -> bool {
        match self {
            Self::LocalOperator | Self::Coordinator => true,
            Self::Worker => !matches!(operation, PortOperation::Submit),
            Self::Observer => !operation.is_mutation(),
        }
    }

    /// True when `other` is this tier or strictly narrower.
    pub fn narrows_to(self, other: Self) -> bool {
        other.rank() >= self.rank()
    }
}

/// Kind of host serving the port. Presentation-neutral: an embedder uses it
/// for capability expectations, never for authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortHostKind {
    Desktop,
    Service,
    Embedded,
}

/// Authenticated identity. A caller-supplied session, run, or workspace is a
/// *requested resource*; only this type is identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortPrincipal {
    pub principal_id: String,
    pub credential_id: String,
    pub tier: PortTier,
    /// Principal that delegated this authority, when narrowed from another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<String>,
}

impl PortPrincipal {
    pub fn new(
        principal_id: impl Into<String>,
        credential_id: impl Into<String>,
        tier: PortTier,
    ) -> PortResult<Self> {
        let principal_id = validate_identifier(principal_id.into())?;
        let credential_id = validate_identifier(credential_id.into())?;
        Ok(Self {
            principal_id,
            credential_id,
            tier,
            delegated_from: None,
        })
    }

    /// Narrow this principal for a delegate. Widening is refused; the
    /// delegation source stays attributable.
    pub fn delegate(
        &self,
        principal_id: impl Into<String>,
        credential_id: impl Into<String>,
        tier: PortTier,
    ) -> PortResult<Self> {
        if !self.tier.narrows_to(tier) {
            return Err(PortError::new(
                PortErrorCode::ForbiddenScope,
                "delegation may only narrow the delegator's tier",
            ));
        }
        let mut delegate = Self::new(principal_id, credential_id, tier)?;
        delegate.delegated_from = Some(self.principal_id.clone());
        Ok(delegate)
    }
}

/// Negotiated description of the host, refreshed before every operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostNegotiation {
    pub protocol_version: u32,
    pub host_id: String,
    pub host_kind: PortHostKind,
    /// Monotonic revision of the declared capability document. Any change to
    /// capabilities or limits must bump it.
    pub capability_revision: u64,
    pub capabilities: BTreeSet<PortOperation>,
    pub limits: PortLimits,
    /// Instant this host generation began serving. A durable claim stamped
    /// before it belongs to a previous generation, so its effect is uncertain.
    pub generation_started_at: DateTime<Utc>,
}

impl HostNegotiation {
    pub fn declares(&self, operation: PortOperation) -> bool {
        self.capabilities.contains(&operation)
    }
}

/// Bounds a host will enforce for the negotiated capability revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortLimits {
    pub max_prompt_bytes: usize,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
    pub max_event_page: usize,
}

impl PortLimits {
    pub fn validate(&self) -> PortResult<()> {
        if self.max_prompt_bytes == 0 || self.max_rounds == 0 || self.max_duration_ms == 0 {
            return Err(PortError::new(
                PortErrorCode::Unavailable,
                "host negotiated a zero run limit",
            ));
        }
        if self.max_total_tokens == Some(0) {
            return Err(PortError::new(
                PortErrorCode::Unavailable,
                "host negotiated a zero token limit",
            ));
        }
        if self.max_event_page == 0 {
            return Err(PortError::new(
                PortErrorCode::Unavailable,
                "host negotiated a zero event page limit",
            ));
        }
        Ok(())
    }

    /// Clamp a requested page to the negotiated limit and the absolute
    /// ceiling. Zero means "caller did not choose".
    pub fn clamp_page(&self, requested: usize) -> usize {
        let requested = if requested == 0 {
            DEFAULT_PORT_EVENT_PAGE
        } else {
            requested
        };
        // `validate` already rejects a zero negotiated page, but this helper is
        // callable on its own, so the floor is guarded explicitly rather than
        // with a clamp that would panic on an inverted range.
        let ceiling = if self.max_event_page == 0 {
            1
        } else {
            self.max_event_page.min(MAX_PORT_EVENT_PAGE)
        };
        requested.min(ceiling)
    }
}

/// Caller-requested run bounds. A caller may only narrow the negotiated
/// limits; every field is optional and defaults to the host ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PortRunBounds {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_prompt_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
}

impl PortRunBounds {
    /// Intersect a caller request with freshly negotiated limits. Widening is
    /// refused rather than silently clamped, so an embedder never believes it
    /// obtained a larger budget than the host granted.
    pub fn resolve(&self, limits: &PortLimits) -> PortResult<PortLimits> {
        let too_large = PortError::new(
            PortErrorCode::LimitExceeded,
            "requested bounds exceed the negotiated host limits",
        );
        let zero = PortError::new(PortErrorCode::InvalidRequest, "requested bound must be > 0");
        let max_prompt_bytes = match self.max_prompt_bytes {
            Some(0) => return Err(zero),
            Some(value) if value > limits.max_prompt_bytes => return Err(too_large),
            Some(value) => value,
            None => limits.max_prompt_bytes,
        };
        let max_rounds = match self.max_rounds {
            Some(0) => return Err(zero),
            Some(value) if value > limits.max_rounds => return Err(too_large),
            Some(value) => value,
            None => limits.max_rounds,
        };
        let max_duration_ms = match self.max_duration_ms {
            Some(0) => return Err(zero),
            Some(value) if value > limits.max_duration_ms => return Err(too_large),
            Some(value) => value,
            None => limits.max_duration_ms,
        };
        let max_total_tokens = match (self.max_total_tokens, limits.max_total_tokens) {
            (Some(0), _) => return Err(zero),
            (Some(requested), Some(ceiling)) if requested > ceiling => return Err(too_large),
            (Some(requested), _) => Some(requested),
            (None, ceiling) => ceiling,
        };
        Ok(PortLimits {
            max_prompt_bytes,
            max_rounds,
            max_duration_ms,
            max_total_tokens,
            max_event_page: limits.max_event_page,
        })
    }
}

/// How the host should execute the run.
///
/// `IsolatedWorktree` is what makes a run reviewable: only an isolated run has
/// a diff for [`PortOperation::Review`] to describe. A shared run is refused by
/// review with `conflict`, because there is nothing to review rather than
/// because the caller lacks scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortExecutionMode {
    #[default]
    Shared,
    IsolatedWorktree,
}

/// Typed submit request. `prompt` is input only: it is never retained in a
/// projection, receipt, error, or event page, and this type deliberately does
/// not implement `Serialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSubmitRequest {
    pub request_id: String,
    pub prompt: String,
    pub bounds: PortRunBounds,
    pub execution_mode: PortExecutionMode,
    /// Queue behind bounded capacity instead of failing fast.
    pub allow_queue: bool,
}

impl PortSubmitRequest {
    pub fn new(request_id: impl Into<String>, prompt: impl Into<String>) -> PortResult<Self> {
        Ok(Self {
            request_id: validate_identifier(request_id.into())?,
            prompt: non_empty_prompt(prompt.into())?,
            bounds: PortRunBounds::default(),
            execution_mode: PortExecutionMode::Shared,
            allow_queue: false,
        })
    }

    pub fn with_bounds(mut self, bounds: PortRunBounds) -> Self {
        self.bounds = bounds;
        self
    }

    pub fn with_execution_mode(mut self, execution_mode: PortExecutionMode) -> Self {
        self.execution_mode = execution_mode;
        self
    }

    pub fn with_allow_queue(mut self, allow_queue: bool) -> Self {
        self.allow_queue = allow_queue;
        self
    }
}

/// Durable delivery state of one mutation request id. This is the visible
/// half of the port's write-ahead / act / log / acknowledge discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDelivery {
    /// No durable claim exists. Nothing has been attempted.
    Unknown,
    /// A claim exists in this host generation and has not settled.
    Sending,
    /// A claim from a previous generation, or a settled-failed claim whose
    /// effect is nonetheless observable. The effect may or may not have
    /// happened. Never auto-replayed.
    Uncertain,
    /// The claim settled successfully and its receipt is authoritative.
    Delivered,
    /// The claim settled as a typed refusal. No effect took place.
    Rejected,
}

impl PortDelivery {
    /// Whether the same request id may be presented again. `Uncertain` is
    /// never retryable: the embedder must mint a new request id.
    pub fn retry_with_same_request_id(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Receipt for a typed submit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortSubmitReceipt {
    pub request_id: String,
    pub delivery: PortDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_position: Option<u32>,
    pub retry_with_same_request_id: bool,
    /// Typed refusal recorded durably for a `Rejected` delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<PortError>,
    /// Bounds the host actually admitted, after intersecting the request with
    /// the freshly negotiated limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_limits: Option<PortLimits>,
}

/// Receipt for a cancel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCancelReceipt {
    pub request_id: String,
    pub run_id: String,
    pub delivery: PortDelivery,
    pub retry_with_same_request_id: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<PortError>,
}

/// Durable lifecycle state of a run, host-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortRunState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    LimitReached,
}

impl PortRunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Closed stop classification. Mirrors the runtime's durable stop causes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortStopCause {
    Completed,
    RoundLimit,
    DurationLimit,
    TokenCeiling,
    TokenAccountingUnavailable,
    TokenAccountingOverflow,
    Stationarity,
    RecoveryExhausted,
    Cancelled,
    Interrupted,
    Failed,
}

/// Promotion state of a reviewable isolated run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortPromotionState {
    NotApplicable,
    Preparing,
    Ready,
    Promoted,
    Conflicted,
    Discarded,
}

/// Typed verification classification carried by durable completion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortVerification {
    Verified,
    Unverified,
    Failed,
    Incomplete,
}

/// Why a terminal run could not be presented as verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortEvidenceGap {
    /// The run carries no durable completion evidence at all.
    MissingVerification,
    /// Evidence exists but the runtime classified it unverified or failed.
    UnverifiedVerification,
    /// Token accounting is not complete for every attributable request.
    IncompleteUsage,
    /// Provider attempts were admitted but never reconciled with a response.
    PendingProviderAttempts,
    /// The run has no durable event range, so its timeline cannot be replayed.
    MissingEventRange,
}

/// Counted, typed evidence. Counts and classifications only — no paths, no
/// commands, no tool output, no provider payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEvidenceSummary {
    pub changed_files: u32,
    pub tests_observed: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_incomplete: u32,
    pub permissions_requested: u32,
    pub permissions_granted: u32,
    pub permissions_denied: u32,
    pub total_tokens: u64,
    /// Reconciled provider attempts attributable to this run.
    pub provider_requests: u64,
    /// Every attributable provider attempt returned valid usage metadata.
    pub usage_complete: bool,
    /// Provider attempts durably admitted but not reconciled with a response.
    pub usage_pending_requests: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<PortVerification>,
}

/// Durable, already redaction-safe facts about one run.
///
/// A host adapter builds this from its own records. The leaking fields — the
/// prompt, the final model text, the workspace path, changed file paths, tool
/// input and output — have no home in this type, so the projection layer
/// cannot accidentally forward them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRunFacts {
    pub run_id: String,
    pub session_id: Uuid,
    pub request_id: String,
    pub state: PortRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seq: Option<u64>,
    pub round: u32,
    pub max_rounds: u32,
    pub admitted_limits: PortLimits,
    pub evidence: PortEvidenceSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_cause: Option<PortStopCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PortPromotionState>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Durable facts about a reviewable isolated run. Fingerprints and counts
/// only: the diff text and the changed paths stay on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortReviewFacts {
    pub run_id: String,
    pub promotion: PortPromotionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_fingerprint: Option<String>,
    pub changed_file_count: u32,
    /// A diff exists on the host and can be fetched through an operator
    /// surface. Its bytes never cross this port.
    pub diff_available: bool,
    pub diff_truncated: bool,
}

/// Durable evidence about one mutation request id, as recorded by the host's
/// existing idempotency ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortClaimState {
    /// Written ahead of the effect and not yet settled.
    Claimed,
    /// Settled with a durable success receipt.
    Completed,
    /// Settled as failed because the process was interrupted before the
    /// receipt completed. The effect may still have happened.
    FailedInterrupted,
    /// Settled as a typed refusal decided before any effect.
    FailedRejected,
}

/// One durable claim as read back from the host ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortClaimEvidence {
    pub state: PortClaimState,
    /// Operation that wrote the claim, when the host records it. A request id
    /// reused for a different operation is a conflict, not a replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<PortOperation>,
    pub claimed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_position: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection: Option<PortError>,
}

/// Everything the port needs to classify a request id without replaying it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortDeliveryEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<PortClaimEvidence>,
    /// A run durably attributed to this request id, if one exists. Its
    /// presence is what makes a failed claim *uncertain* rather than rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<PortRunFacts>,
}

pub(crate) fn validate_identifier(value: String) -> PortResult<String> {
    let trimmed = value.trim();
    let invalid = PortError::new(
        PortErrorCode::InvalidRequest,
        "identifier must be 1..=256 bytes of ASCII letters, numbers, '-', '_', or '.'",
    );
    if trimmed.is_empty() || trimmed.len() > MAX_PORT_ID_BYTES {
        return Err(invalid);
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid);
    }
    // '.' is legal inside an id but a bare dot run is traversal-shaped.
    if trimmed.bytes().all(|b| b == b'.') {
        return Err(invalid);
    }
    Ok(trimmed.to_string())
}

fn non_empty_prompt(prompt: String) -> PortResult<String> {
    if prompt.trim().is_empty() {
        return Err(PortError::new(
            PortErrorCode::InvalidRequest,
            "prompt must not be empty",
        ));
    }
    Ok(prompt)
}
