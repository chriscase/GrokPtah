//! Provider-neutral data transfer objects.
//!
//! # What is deliberately absent
//!
//! Following the runtime's own Computer Use projection, anything a consumer
//! may not observe is **absent from the type**, not filtered at the transport
//! boundary. A field that does not exist cannot be leaked by a buggy adapter.
//!
//! | Not on this boundary | Why |
//! |---|---|
//! | Prompt text, prompt previews, model prose, final responses | Transcript. A seam that carries one run's prose carries every run's. See `docs/AGENT_SDK_SEAM.md` for the authorship-scoped design that would allow it. |
//! | Agent/thought chunks, shell output, unified diffs | Transcript, and unbounded. |
//! | Absolute workspace paths, `GROKPTAH_HOME`, store paths | Internal storage. [`WorkspaceRef`] is an opaque handle. |
//! | Provider names, routes, keys, auth material, gateway config | Provider authority. Never delegated; see [`CapabilityId::ProviderCredentials`]. |
//! | Computer Use evidence bytes, screenshots, element labels | Denied at the runtime and denied here; see [`CapabilityId::ComputerControl`]. |
//! | Lease secrets | Held in [`LeaseCredential`], which is not `Serialize`. |
//!
//! [`CapabilityId::ProviderCredentials`]: crate::capability::CapabilityId::ProviderCredentials
//! [`CapabilityId::ComputerControl`]: crate::capability::CapabilityId::ComputerControl

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{truncate_on_char_boundary, SdkError, SdkErrorCode, SdkResult};
use crate::ids::{
    AgentId, ArtifactId, AttemptId, Label, RelativePath, RequestId, RunId, SessionId, WorkId,
    WorkspaceRef,
};
use crate::page::{Cursor, RetainedRange};
use crate::vocab::open_vocabulary;

/// Longest bounded free-text summary the seam carries.
pub const MAX_SUMMARY_BYTES: usize = 512;
/// Hard ceiling on an inline artifact body, whatever a host advertises.
pub const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

/// Monotonic per-resource revision.
///
/// A consumer keeps the newest revision it has applied and ignores anything
/// not strictly greater. The runtime publishes events after releasing its
/// mutation lock, so publish order (`seq`) and commit order (`revision`) can
/// differ; without this watermark a late-delivered older snapshot silently
/// regresses a consumer's view.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Revision(pub u64);

impl Revision {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn is_newer_than(self, other: Self) -> bool {
        self.0 > other.0
    }
}

impl std::fmt::Display for Revision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Applies the "strictly newer wins" rule for one resource.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RevisionWatermark {
    applied: Revision,
}

impl RevisionWatermark {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn applied(&self) -> Revision {
        self.applied
    }

    /// Accept `incoming` only if it is strictly newer.
    ///
    /// A non-advancing snapshot is [`SdkErrorCode::StaleObservation`], not a
    /// silent no-op: a consumer that keeps receiving stale snapshots has a
    /// delivery problem worth surfacing.
    pub fn admit(&mut self, incoming: Revision) -> SdkResult<()> {
        if !incoming.is_newer_than(self.applied) {
            return Err(SdkError::new(
                SdkErrorCode::StaleObservation,
                "snapshot revision does not advance the applied watermark",
            )
            .with_detail("appliedRevision", self.applied.to_string())
            .with_detail("incomingRevision", incoming.to_string()));
        }
        self.applied = incoming;
        Ok(())
    }
}

// ── Sessions ──────────────────────────────────────────────────────────────

open_vocabulary! {
    /// Mirrors the runtime `SessionKind`.
    SessionKind {
    /// Coding-agent session. The only kind that accepts task submission,
    /// follow-up, cancel, or queue control.
        Build => "build",
    /// Conversational session. Read-only across this boundary.
        Chat => "chat",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub request_id: RequestId,
    /// Must already be advertised by the host. A consumer cannot name a new
    /// workspace, only select one the host allowlisted.
    pub workspace: WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Label>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Label>,
    pub revision: Revision,
    pub created_at: DateTime<Utc>,
}

/// Names one run without ambient state.
///
/// All three parts are required on every read. The runtime refuses a run
/// lookup by ID alone so a read cannot become an existence oracle for another
/// caller's scope, and this seam does not soften that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSelector {
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub run_id: RunId,
}

// ── Run lifecycle (mirror only — no second state machine) ─────────────────

open_vocabulary! {
    /// The run lifecycle, mirroring the runtime `RunState` exactly.
    ///
    /// This enum adds no state, removes no state, and renames nothing. If the
    /// runtime gains a state, this is a contract **major** change.
    RunLifecycle {
        Queued => "queued",
        Running => "running",
        Completed => "completed",
        Failed => "failed",
        Cancelled => "cancelled",
        Interrupted => "interrupted",
        LimitReached => "limit_reached",
    }
}

impl RunLifecycle {
    /// `true` only for a state this build knows to be final.
    ///
    /// An [`Unknown`](Self::Unknown) lifecycle is **not** terminal. A consumer
    /// that treated it as terminal would stop observing a run that may still
    /// be executing, and could report a result the host never produced;
    /// treating it as live only costs another poll. Pair this with
    /// [`is_known`](Self::is_known) to surface "this host is ahead of this
    /// build" rather than polling an unrecognized state forever.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::LimitReached
        )
    }

    /// The seven wire tokens, in the runtime's declaration order.
    pub fn wire_tokens() -> &'static [&'static str] {
        &[
            "queued",
            "running",
            "completed",
            "failed",
            "cancelled",
            "interrupted",
            "limit_reached",
        ]
    }
}

open_vocabulary! {
    /// Host-decided terminal cause, mirroring the runtime `RunStopCause`.
    ///
    /// This is never inferred from model prose; that is the whole point of the
    /// runtime carrying it separately from a final response.
    StopCause {
        Completed => "completed",
        RoundLimit => "round_limit",
        DurationLimit => "duration_limit",
        TokenCeiling => "token_ceiling",
        TokenAccountingUnavailable => "token_accounting_unavailable",
        TokenAccountingOverflow => "token_accounting_overflow",
        Stationarity => "stationarity",
        RecoveryExhausted => "recovery_exhausted",
        Cancelled => "cancelled",
        Interrupted => "interrupted",
        Failed => "failed",
    }
}

open_vocabulary! {
    /// Mirrors the runtime `RunExecutionMode`.
    ExecutionMode {
        Shared => "shared",
        IsolatedWorktree => "isolated_worktree",
    }
}

/// Caller-requested bounds. Every field may only **narrow** the host ceiling;
/// an attempt to widen is rejected by the host, not silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBoundsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
}

/// The bounds the host actually applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedBounds {
    pub max_prompt_bytes: u64,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
}

/// Cumulative token accounting.
///
/// `complete` is the trust signal. The runtime persists usage per provider
/// response and fails a bounded run closed when accounting is unavailable, so
/// `complete: false` must never be rendered as a confirmed total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageView {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub complete: bool,
    pub pending_requests: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgressView {
    pub round: u32,
    pub max_rounds: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<Label>,
    pub updated_at: DateTime<Utc>,
}

open_vocabulary! {
    /// Evidence-backed verification, mirroring `ptah_get_handoff`.
    VerificationStatus {
        Verified => "verified",
        Unverified => "unverified",
        Failed => "failed",
        Incomplete => "incomplete",
    }
}

/// Host observations. These are counts the runtime derived from typed events,
/// not claims the model made about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationCounts {
    pub changed_files: u32,
    pub tests_observed: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub tests_incomplete: u32,
    pub permissions_requested: u32,
    pub permissions_granted: u32,
    pub permissions_denied: u32,
    pub permissions_unresolved: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationView {
    pub status: VerificationStatus,
    pub stop_cause: StopCause,
    pub interrupted: bool,
    pub observations: ObservationCounts,
    pub usage: UsageView,
}

/// One changed file, workspace-relative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: RelativePath,
    /// Bounded description. Never a diff body.
    pub summary: BoundedText,
}

/// Free text that is bounded and control-character-free at construction.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BoundedText(String);

impl<'de> Deserialize<'de> for BoundedText {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(d)?))
    }
}

impl BoundedText {
    pub fn new(raw: impl AsRef<str>) -> Self {
        let cleaned: String = raw
            .as_ref()
            .chars()
            .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        Self(truncate_on_char_boundary(cleaned.trim(), MAX_SUMMARY_BYTES))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BoundedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The public projection of one finite run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub lifecycle: RunLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_cause: Option<StopCause>,
    /// Advances on every durable change to this run.
    pub revision: Revision,
    pub execution_mode: ExecutionMode,
    /// One-based position in the host-global admission queue while queued.
    /// Live visibility, not a reservation: `lifecycle` is authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
    pub bounds: AppliedBounds,
    pub usage: UsageView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<RunProgressView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<ChangedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationView>,
    /// Host-authored terminal marker such as `max_total_tokens_usage_unavailable`.
    /// Distinct from [`crate::error::SdkErrorCode`], which describes a failed
    /// *request*, not a completed run's outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_marker: Option<Label>,
    /// Readable journal window, once the run has started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_range: Option<RetainedRange>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for ExecutionMode {
    /// The runtime's default: run in the session's own workspace.
    fn default() -> Self {
        Self::Shared
    }
}

// ── Task submission ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSubmission {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    /// The instruction. Carried into the host but never read back out of it:
    /// no projection on this boundary echoes prompt text.
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<RunBoundsRequest>,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    /// Opt in to the bounded admission queue instead of failing fast when the
    /// host is at capacity.
    #[serde(default)]
    pub allow_queue: bool,
}

/// Receipt for an accepted submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAccepted {
    pub run_id: RunId,
    pub lifecycle: RunLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<u32>,
    pub revision: Revision,
    /// Whether this response replayed a durable idempotency receipt rather
    /// than starting new work.
    ///
    /// `None` means the host does not report it. The MCP control plane replays
    /// a stored receipt byte-for-byte, so a replay is indistinguishable from
    /// fresh work on that boundary; an adapter there reports `None` rather
    /// than asserting `false`. The invariant a caller can always rely on is
    /// the weaker and more important one: the same key never does the work
    /// twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

// ── Follow-up ─────────────────────────────────────────────────────────────

open_vocabulary! {
    /// Mirrors the runtime `SteeringDisposition`.
    FollowUpDisposition {
    /// Accepted for the active turn; it lands at the next safe model boundary.
        Pending => "pending",
    /// The session was idle, so it became a durable queued follow-up.
        Queued => "queued",
    }
}

/// A non-cancelling follow-up.
///
/// This never interrupts a turn and never starts a second concurrent turn.
/// Cancellation is a separate, explicit operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub text: String,
    /// Compare-and-set fence. When present, the host rejects the mutation with
    /// [`SdkErrorCode::StaleVersion`] unless the session queue is still at this
    /// revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<Revision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpReceipt {
    pub disposition: FollowUpDisposition,
    /// The revision this mutation produced. Chain from here instead of
    /// re-reading, so a competing writer cannot slip between your read and
    /// your next fenced mutation.
    pub revision: Revision,
    /// See [`RunAccepted::replayed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

// ── Cancellation ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelRequest {
    pub request_id: RequestId,
    pub selector: RunSelector,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelReceipt {
    pub run_id: RunId,
    pub lifecycle: RunLifecycle,
    /// `true` when the run had not started, so no model turn was launched.
    #[serde(default)]
    pub was_queued: bool,
    pub revision: Revision,
    /// See [`RunAccepted::replayed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

// ── Control leases ────────────────────────────────────────────────────────

/// A lease secret.
///
/// Not `Serialize`, not `Deserialize`, and `Debug`-redacted, mirroring the
/// runtime's `AuthCredential`. It cannot reach a log, a JSON body, or another
/// process by accident — only by an adapter deliberately calling [`Self::reveal`].
#[derive(Clone, Default, PartialEq, Eq)]
pub struct LeaseCredential(String);

impl LeaseCredential {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Adapter-only. Never log this, never serialize it, never forward it to
    /// another process or another consumer.
    pub fn reveal(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for LeaseCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeaseCredential([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLeaseRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub work_id: WorkId,
    /// The agent this lease is claimed *for*. A caller-supplied agent ID is a
    /// requested resource; it never substitutes for authenticated identity.
    pub claimant: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ttl_ms: Option<u64>,
}

/// An acquired lease. At most one is active per work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLease {
    pub work_id: WorkId,
    pub attempt_id: AttemptId,
    pub attempt_number: u32,
    pub claimant: AgentId,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revision: Revision,
    /// Never serialized. See [`LeaseCredential`].
    #[serde(skip)]
    pub credential: LeaseCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseLeaseRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub workspace: WorkspaceRef,
    pub work_id: WorkId,
    pub attempt_id: AttemptId,
    /// Why the lease is being given up. Durable and attributable.
    pub reason: BoundedText,
    /// The credential [`ControlLease`] handed back on acquisition.
    ///
    /// Never serialized, so releasing is possible only for the process that
    /// actually holds the lease — the runtime authorizes a release against the
    /// claimant's token, not against the attempt id alone.
    #[serde(skip)]
    pub credential: LeaseCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseLeaseReceipt {
    pub work_id: WorkId,
    pub attempt_id: AttemptId,
    pub revision: Revision,
    /// See [`RunAccepted::replayed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

// ── Redacted receipts ─────────────────────────────────────────────────────

open_vocabulary! {
    /// What a mutation was, without naming the host tool that performed it.
    ///
    /// The raw tool name never crosses: it is host vocabulary that changes when
    /// the host adds a tool, and forwarding it would let a newer host put an
    /// arbitrary string in front of a consumer that believes it is reading a
    /// classification.
    ///
    /// Two different unknowns, deliberately kept apart. [`Other`](Self::Other)
    /// is a mutation the *adapter* looked at and classified as outside this
    /// contract's vocabulary — a decision. `Unknown` is a token that arrived on
    /// the wire from a host this build does not understand — an absence of one.
    /// A consumer auditing coverage needs to tell those apart.
    OperationClass {
        CreateSession => "create_session",
        SubmitTask => "submit_task",
        FollowUp => "follow_up",
        Cancel => "cancel",
        AcquireLease => "acquire_lease",
        ReleaseLease => "release_lease",
    /// A mutation outside this contract's vocabulary.
        Other => "other",
    }
}

open_vocabulary! {
    /// Where a durable idempotency receipt stands.
    ///
    /// Mirrors the runtime's three values. `Pending` is the one that matters: the
    /// host claimed the key and then stopped before recording an outcome, so the
    /// mutation may or may not have applied. See [`ReceiptView::is_uncertain`].
    ReceiptStatus {
        Pending => "pending",
        Complete => "complete",
        Failed => "failed",
    }
}

/// Tells one attempt from another without letting anyone confirm what was sent.
///
/// # Why this is not the host's payload hash
///
/// The runtime stores `payload_hash = SHA-256(serialized request)`, unkeyed.
/// For `submit_task` that request contains the **prompt**. Passing the host's
/// hash through this seam verbatim would hand every reader a prompt-confirmation
/// oracle: guess the text, hash it, compare. That is the same weakness the
/// crate already documents for [`WorkspaceRef`] — except a prompt is the one
/// thing this boundary exists to withhold, and unlike a path it is worth
/// guessing.
///
/// It is worse than it first looks, because reads here are **not**
/// principal-scoped (see `docs/AGENT_SDK_SEAM.md`): every credential that can
/// reach a session sees every receipt in it, including receipts written by a
/// different credential. The oracle would therefore work *across* consumers
/// sharing a session, not just on one's own traffic.
///
/// # What crosses instead
///
/// `SHA-256(scope_salt ‖ 0x00 ‖ host_payload_hash)`, truncated to 16 bytes.
/// Within one adapter and one salt this is a bijection on the host's hash, so
/// the only property a consumer actually needs — *these two receipts are the
/// same attempt / different attempts* — survives exactly. Everything else does
/// not: an attacker without the salt cannot test a guess offline, and two
/// scopes never produce a matching digest for the same payload.
///
/// # What you may not do with it
///
/// Do not compare digests across adapters, across salts, or across hosts, and
/// do not persist one as a durable identity. It is a within-scope equality
/// token, not a content address. [`RequestId`] is the durable identity.
///
/// [`WorkspaceRef`]: crate::ids::WorkspaceRef
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptDigest(String);

impl AttemptDigest {
    /// Bytes of truncated digest carried across the seam.
    pub const BYTES: usize = 16;

    /// Derive from an adapter salt and the host's stored payload hash.
    pub fn derive(scope_salt: &[u8], host_payload_hash: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(scope_salt);
        hasher.update([0u8]);
        hasher.update(host_payload_hash.as_bytes());
        let full = hasher.finalize();
        Self(hex_lower(&full[..Self::BYTES]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Accept a digest the **host** already salted.
    ///
    /// Preferred over [`derive`](Self::derive) when the host issues opaque
    /// digests itself: the unkeyed payload hash then never leaves the host at
    /// all, rather than travelling the wire to be salted here. Validated on
    /// the way in so a host cannot smuggle a longer string, mixed case, or
    /// non-hex through a field consumers treat as an opaque token.
    pub fn from_host(raw: impl AsRef<str>) -> SdkResult<Self> {
        let raw = raw.as_ref();
        if raw.len() != Self::BYTES * 2 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SdkError::new(
                SdkErrorCode::Internal,
                "host attempt digest was not the advertised width of lowercase hex",
            ));
        }
        if raw.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(SdkError::new(
                SdkErrorCode::Internal,
                "host attempt digest must be lowercase",
            ));
        }
        Ok(Self(raw.to_string()))
    }
}

impl std::fmt::Display for AttemptDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

open_vocabulary! {
    /// Which population a retention budget counts against.
    RetentionBudgetScope {
        /// One budget shared by every receipt the host holds. Pressure from
        /// other runs can expire this run's receipts.
        Host => "host",
        /// A budget applied per run.
        Run => "run",
    }
}

/// Receipts a retention window keeps regardless of age or count.
///
/// Without these a consumer would compute "this receipt is older than
/// `max_age_days`, so it must be gone" and be wrong — the runtime keeps two
/// classes indefinitely, and both are exactly the classes an observer most
/// wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionExemptions {
    /// A receipt that never settled is never expired. Its whole purpose is to
    /// record that an outcome is unknown; expiring it would silently convert
    /// "may have applied" into "never happened".
    pub unsettled_retained: bool,
    /// A receipt whose run has not reached a terminal state is never expired,
    /// however old, so a long-running run cannot lose the evidence of the
    /// request that started it.
    pub active_run_retained: bool,
}

/// Redacted evidence that a mutation happened.
///
/// An observer may learn *that* a request was made, what class it belonged to,
/// whether it settled, and — when it failed — the typed reason. It may not
/// learn what was sent or what came back.
///
/// # What is absent, and why
///
/// | Absent | Why |
/// |---|---|
/// | The stored response body | The runtime replays a mutation's full response from its receipt. That body carries whatever the mutation returned — run prompts, workspace paths, queue entries. |
/// | The failure *message* | Runtime messages embed absolute paths verbatim; `canonical_workspace` formats one straight into a `workspace_mismatch`. The typed [`SdkErrorCode`] carries the meaning without the text. |
/// | The raw tool name | Host vocabulary; see [`OperationClass`]. |
/// | The request payload | Only a *salted* digest crosses. The host's own unkeyed `payload_hash` would be a confirmation oracle on the prompt it covers; see [`AttemptDigest`]. |
///
/// [`SdkErrorCode`]: crate::error::SdkErrorCode
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptView {
    pub request_id: RequestId,
    pub operation: OperationClass,
    pub status: ReceiptStatus,
    /// Typed reason, present only on [`ReceiptStatus::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<crate::error::SdkErrorCode>,
    /// Salted digest of the request payload. Distinguishes attempts within
    /// this scope and reveals nothing about their contents — see
    /// [`AttemptDigest`] for why the host's own hash must not be used here.
    pub payload_digest: AttemptDigest,
    /// The run this mutation produced or acted on, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// When the host **claimed** the request id — not when the mutation
    /// settled.
    ///
    /// This is the ordering key a receipt page and its cursor both use, so it
    /// has to be immutable: a host that moved it on settlement would move a
    /// receipt past a cursor a caller is already holding and hand it out
    /// twice. Do not read it as a completion time or subtract it from one to
    /// get a duration; `status` says whether the mutation settled.
    pub recorded_at: DateTime<Utc>,
}

/// The bounded window a receipt listing was drawn from.
///
/// Travels *with* the page rather than beside it, so a consumer cannot hold
/// receipts without also holding the caveat: a receipt that aged out is
/// indistinguishable from one that never existed. Absence is never proof that
/// a mutation did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRetention {
    /// Most settled receipts the budget keeps. Read it together with
    /// [`budget_scope`](Self::budget_scope): under
    /// [`RetentionBudgetScope::Host`] this is *not* a per-run allowance, and a
    /// busy neighbouring run can expire this run's receipts well before the
    /// count suggests.
    pub max_receipts: u32,
    /// Age at which an expirable receipt is dropped.
    pub max_age_days: u32,
    /// The population `max_receipts` counts against.
    pub budget_scope: RetentionBudgetScope,
    /// Receipts the window keeps regardless of age or count.
    pub exemptions: RetentionExemptions,
}

impl ReceiptRetention {
    /// The runtime's shipped policy, as `OrchStore::apply_retention` applies
    /// it: a **host-wide** budget of the newest 1,000 expirable receipts and a
    /// 7-day ceiling, with unsettled receipts and receipts belonging to
    /// non-terminal runs held back from both.
    pub const RUNTIME_DEFAULT: Self = Self {
        max_receipts: 1_000,
        max_age_days: 7,
        budget_scope: RetentionBudgetScope::Host,
        exemptions: RetentionExemptions {
            unsettled_retained: true,
            active_run_retained: true,
        },
    };

    /// `true` when a receipt in this state is held back from expiry.
    ///
    /// The conservative reading: an unknown status counts as unsettled, so a
    /// consumer never concludes a receipt was safe to drop on the strength of
    /// a word it could not interpret.
    pub fn is_exempt(&self, status: &ReceiptStatus, run_is_terminal: Option<bool>) -> bool {
        if self.exemptions.unsettled_retained
            && !matches!(status, ReceiptStatus::Complete | ReceiptStatus::Failed)
        {
            return true;
        }
        self.exemptions.active_run_retained && run_is_terminal == Some(false)
    }
}

/// One page of receipts, inseparable from the window it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptPage {
    pub items: Vec<ReceiptView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
    pub retention: ReceiptRetention,
}

impl ReceiptPage {
    pub fn new(
        items: Vec<ReceiptView>,
        next_cursor: Option<Cursor>,
        retention: ReceiptRetention,
    ) -> Self {
        Self {
            items,
            next_cursor,
            retention,
        }
    }

    /// `true` when the caller has read everything the window still holds.
    pub fn is_caught_up(&self) -> bool {
        self.next_cursor.is_none()
    }
}

/// Deterministic ordering key for a receipt listing.
///
/// Receipts order by `(recorded_at, request_id)`: chronological, with the
/// request id breaking ties so two receipts written in the same millisecond
/// cannot swap places between pages. The cursor is that pair, encoded.
///
/// **This is the in-process fake's own encoding, not a security boundary.** A
/// real host authenticates the cursors it issues and refuses one it did not,
/// which is why a consumer must treat a `Cursor` as bytes to hand back rather
/// than a value to construct: the encoding here is deliberately not the
/// encoding a live host uses, and a consumer that learned to build one from
/// this would break against every real host.
pub fn receipt_cursor(receipt: &ReceiptView) -> Cursor {
    Cursor::from_opaque(format!(
        "{}:{}",
        receipt.recorded_at.timestamp_millis().max(0),
        receipt.request_id
    ))
}

/// Decode a cursor minted by [`receipt_cursor`].
pub fn parse_receipt_cursor(cursor: &Cursor) -> SdkResult<(i64, String)> {
    let raw = cursor.as_str();
    let (millis, request_id) = raw.split_once(':').ok_or_else(|| {
        SdkError::new(
            SdkErrorCode::InvalidRequest,
            "cursor was not issued by this adapter",
        )
    })?;
    let millis = millis.parse::<i64>().map_err(|_| {
        SdkError::new(
            SdkErrorCode::InvalidRequest,
            "cursor was not issued by this adapter",
        )
    })?;
    Ok((millis, request_id.to_string()))
}

impl ReceiptView {
    /// `true` when the mutation's effect is unknown.
    ///
    /// A pending receipt is the uncertain-send fence in durable form: the key
    /// was claimed and no outcome was recorded. Retrying under the same key is
    /// safe *only* because the host will replay rather than repeat — but until
    /// the host settles it, an observer must not report the mutation as either
    /// applied or refused.
    /// A status this build cannot read counts as uncertain. The dangerous
    /// reading is the other one: "settled" licenses a consumer to retry a
    /// mutation that may already have applied.
    pub fn is_uncertain(&self) -> bool {
        !matches!(self.status, ReceiptStatus::Complete | ReceiptStatus::Failed)
    }

    /// `true` once the host has settled this key either way.
    pub fn is_settled(&self) -> bool {
        !self.is_uncertain()
    }
}

// ── Artifacts ─────────────────────────────────────────────────────────────

open_vocabulary! {
    DigestAlgorithm {
        Sha256 => "sha256",
    }
}

/// A content digest over the exact bytes an artifact carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDigest {
    pub algorithm: DigestAlgorithm,
    /// Lowercase hex.
    pub hex: String,
}

impl ContentDigest {
    pub fn sha256_of(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Self {
            algorithm: DigestAlgorithm::Sha256,
            hex: hex_lower(&hasher.finalize()),
        }
    }

    pub fn validate(&self) -> SdkResult<()> {
        let expected = match &self.algorithm {
            DigestAlgorithm::Sha256 => 64,
            // Decoding an unknown algorithm keeps the surrounding record
            // readable; *verifying* with one is impossible, so integrity
            // checking refuses rather than passing something it did not check.
            DigestAlgorithm::Unknown(name) => {
                return Err(SdkError::new(
                    SdkErrorCode::Unsupported,
                    format!("this build cannot verify a `{name}` digest"),
                ))
            }
        };
        if self.hex.len() != expected
            || !self.hex.bytes().all(|b| b.is_ascii_hexdigit())
            || self.hex.bytes().any(|b| b.is_ascii_uppercase())
        {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                "digest must be lowercase hex of the algorithm's exact length",
            ));
        }
        Ok(())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble is hex"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble is hex"));
    }
    out
}

open_vocabulary! {
    /// The media an artifact may carry.
    ///
    /// There is no binary variant, by design. Screenshots, Computer Use evidence
    /// assets, and other opaque blobs have no representation here, so the seam
    /// cannot become a byte-exfiltration path for the evidence store the runtime
    /// deliberately keeps host-local.
    ArtifactMedia {
        PlainText => "plain_text",
        Markdown => "markdown",
        Json => "json",
        UnifiedDiff => "unified_diff",
    }
}

impl ArtifactMedia {
    pub fn media_type(&self) -> &'static str {
        match self {
            Self::PlainText => "text/plain; charset=utf-8",
            Self::Markdown => "text/markdown; charset=utf-8",
            Self::Json => "application/json",
            Self::UnifiedDiff => "text/x-diff; charset=utf-8",
            // A medium this build does not know is opaque bytes. Guessing a
            // renderable type from an unrecognized token is how a consumer
            // ends up rendering something the host never said was safe to
            // render.
            Self::Unknown(_) => "application/octet-stream",
        }
    }
}

open_vocabulary! {
    /// What an artifact is, without saying where it lives.
    ArtifactKind {
    /// Reviewed diff for an isolated run.
        ReviewDiff => "review_diff",
    /// Structured test observations.
        TestReport => "test_report",
    /// Host-authored run summary.
        RunSummary => "run_summary",
    /// Structured, non-transcript run metadata.
        Metadata => "metadata",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDescriptor {
    pub artifact_id: ArtifactId,
    pub kind: ArtifactKind,
    pub media: ArtifactMedia,
    pub label: Label,
    pub byte_len: u64,
    pub digest: ContentDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRequest {
    pub selector: RunSelector,
    pub artifact_id: ArtifactId,
    /// Refuse anything larger. Defaults to the boundary ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

/// A fetched artifact, verified before a consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactPayload {
    pub descriptor: ArtifactDescriptor,
    /// UTF-8 body. The media allowlist guarantees text, so this is a `String`
    /// rather than bytes a consumer would have to sniff.
    pub content: String,
}

impl ArtifactPayload {
    /// Verify size and digest before use.
    ///
    /// Callers should not need to remember this: [`crate::client`] adapters are
    /// required by the conformance battery to verify before returning. It is
    /// public so a consumer that persists and reloads an artifact can re-check.
    pub fn verify(&self, max_bytes: u64) -> SdkResult<()> {
        self.descriptor.digest.validate()?;
        let ceiling = max_bytes.min(MAX_ARTIFACT_BYTES as u64);
        let actual = self.content.len() as u64;
        if actual > ceiling {
            return Err(SdkError::new(
                SdkErrorCode::InvalidRequest,
                format!("artifact body is {actual} bytes, above the {ceiling}-byte ceiling"),
            )
            .with_detail("maxBytes", ceiling.to_string())
            .with_detail("actualBytes", actual.to_string()));
        }
        if actual != self.descriptor.byte_len {
            return Err(SdkError::new(
                SdkErrorCode::IntegrityMismatch,
                "artifact body length does not match its declared byteLen",
            )
            .with_detail("declaredBytes", self.descriptor.byte_len.to_string())
            .with_detail("actualBytes", actual.to_string()));
        }
        let computed = ContentDigest::sha256_of(self.content.as_bytes());
        if computed.algorithm != self.descriptor.digest.algorithm
            || computed.hex != self.descriptor.digest.hex
        {
            return Err(SdkError::new(
                SdkErrorCode::IntegrityMismatch,
                "artifact digest does not match its content",
            )
            .with_detail("declaredDigest", self.descriptor.digest.hex.clone())
            .with_detail("computedDigest", computed.hex));
        }
        Ok(())
    }
}

// ── Events ────────────────────────────────────────────────────────────────

open_vocabulary! {
    /// Mirrors the runtime `ToolCallKind`.
    ToolKind {
        Read => "read",
        Edit => "edit",
        Search => "search",
        Execute => "execute",
        Think => "think",
        Other => "other",
    }
}

open_vocabulary! {
    /// Mirrors the runtime `ToolCallStatus`.
    ToolStatus {
        Pending => "pending",
        Running => "running",
        Completed => "completed",
        Failed => "failed",
        Denied => "denied",
    }
}

open_vocabulary! {
    TestOutcome {
        Passed => "passed",
        Failed => "failed",
        Incomplete => "incomplete",
    }
}

open_vocabulary! {
    PermissionOutcome {
        Requested => "requested",
        Granted => "granted",
        Denied => "denied",
        Unresolved => "unresolved",
    }
}

/// A bounded, non-transcript event.
///
/// The runtime journal also carries `AgentMessageChunk`, `AgentThoughtChunk`,
/// `ShellOutput`, and `FileEdit.unified_diff`. None of them appear here: they
/// are transcript and unbounded. A newer host emitting an event this build does
/// not know decodes to [`PublicEventKind::Unrecognized`] rather than failing
/// the whole page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PublicEventKind {
    TurnStarted,
    Progress {
        round: u32,
        max_rounds: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_tool: Option<Label>,
    },
    ToolCall {
        call_id: Label,
        tool: ToolKind,
        status: ToolStatus,
    },
    FileChanged {
        path: RelativePath,
        summary: BoundedText,
    },
    TestObserved {
        outcome: TestOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    Permission {
        outcome: PermissionOutcome,
        tool: ToolKind,
    },
    RateLimited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    FollowUpAccepted {
        disposition: FollowUpDisposition,
    },
    QueueChanged {
        revision: Revision,
        entry_count: u32,
    },
    RunTerminal {
        lifecycle: RunLifecycle,
        stop_cause: StopCause,
    },
    /// A kind this build does not know. The label is the host's wire token.
    Unrecognized {
        wire_kind: Label,
    },
}

/// Every `kind` token this build understands.
///
/// Checked *before* delegating to the derived decoder so that an unknown kind
/// becomes [`PublicEventKind::Unrecognized`] while a malformed *known* kind
/// still fails loudly. Collapsing both into "unrecognized" would hide real
/// corruption behind the forward-compatibility path.
pub const KNOWN_EVENT_KINDS: &[&str] = &[
    "turn_started",
    "progress",
    "tool_call",
    "file_changed",
    "test_observed",
    "permission",
    "rate_limited",
    "follow_up_accepted",
    "queue_changed",
    "run_terminal",
    "unrecognized",
];

/// One event with its durable position.
///
/// # Decoding a page from a newer host
///
/// A host that adds an event kind must not break a consumer pinned to an older
/// build, and must not break it *page-wide* — one unrecognized event in a page
/// of five hundred would otherwise discard the other four hundred and
/// ninety-nine. Decoding therefore checks the `kind` token against
/// [`KNOWN_EVENT_KINDS`] first: a token this build does not have becomes
/// [`PublicEventKind::Unrecognized`], carrying the host's word so a consumer
/// can log or count it. A *known* kind whose fields are malformed still fails,
/// because that is corruption rather than a newer vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicEvent {
    /// Opaque durable position. Send it back as a page cursor or a stream
    /// resume token; do not do arithmetic on it.
    pub cursor: Cursor,
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub kind: PublicEventKind,
}

impl<'de> Deserialize<'de> for PublicEvent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let value = serde_json::Value::deserialize(d)?;
        let cursor = value
            .get("cursor")
            .cloned()
            .ok_or_else(|| D::Error::missing_field("cursor"))?;
        let cursor: Cursor = serde_json::from_value(cursor).map_err(D::Error::custom)?;
        let at = value
            .get("at")
            .cloned()
            .ok_or_else(|| D::Error::missing_field("at"))?;
        let at: DateTime<Utc> = serde_json::from_value(at).map_err(D::Error::custom)?;

        let wire_kind = value
            .get("kind")
            .and_then(|k| k.as_str())
            .ok_or_else(|| D::Error::missing_field("kind"))?;
        let kind = if KNOWN_EVENT_KINDS.contains(&wire_kind) {
            serde_json::from_value(value.clone()).map_err(D::Error::custom)?
        } else {
            PublicEventKind::Unrecognized {
                wire_kind: crate::vocab::unknown_label(wire_kind),
            }
        };

        Ok(Self { cursor, at, kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attempt_digest_is_never_the_host_payload_hash() {
        let host_hash = {
            let mut h = Sha256::new();
            h.update(b"{\"prompt\":\"ship the release\"}");
            hex_lower(&h.finalize())
        };
        let salt = b"tenant-a";
        let digest = AttemptDigest::derive(salt, &host_hash);

        // The whole point: possession of the payload — and therefore of its
        // unkeyed hash — must not let anyone confirm it against what crosses.
        assert_ne!(digest.as_str(), host_hash);
        assert!(!host_hash.starts_with(digest.as_str()));
        assert_eq!(digest.as_str().len(), AttemptDigest::BYTES * 2);
        assert!(digest.as_str().bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn attempt_digests_compare_within_a_scope_and_never_across_one() {
        let a = "aa".repeat(32);
        let b = "bb".repeat(32);

        // Within one salt the only property a consumer needs survives exactly.
        assert_eq!(
            AttemptDigest::derive(b"scope-1", &a),
            AttemptDigest::derive(b"scope-1", &a)
        );
        assert_ne!(
            AttemptDigest::derive(b"scope-1", &a),
            AttemptDigest::derive(b"scope-1", &b)
        );
        // Across salts, the same payload is not correlatable.
        assert_ne!(
            AttemptDigest::derive(b"scope-1", &a),
            AttemptDigest::derive(b"scope-2", &a)
        );
    }

    #[test]
    fn an_unverifiable_digest_algorithm_refuses_rather_than_passes() {
        let digest = ContentDigest {
            algorithm: DigestAlgorithm::from_wire("blake3"),
            hex: "00".repeat(32),
        };
        let error = digest
            .validate()
            .expect_err("cannot verify what it cannot compute");
        assert_eq!(error.code, SdkErrorCode::Unsupported);
        // Decoding still worked — the record around it stays readable.
        assert_eq!(digest.algorithm.as_wire(), "blake3");
        assert!(!digest.algorithm.is_known());
    }

    #[test]
    fn retention_exempts_what_the_runtime_exempts() {
        let policy = ReceiptRetention::RUNTIME_DEFAULT;
        assert_eq!(policy.budget_scope, RetentionBudgetScope::Host);

        // Unsettled is held back, settled is not.
        assert!(policy.is_exempt(&ReceiptStatus::Pending, Some(true)));
        assert!(!policy.is_exempt(&ReceiptStatus::Complete, Some(true)));
        // A live run holds its receipts back however old they are.
        assert!(policy.is_exempt(&ReceiptStatus::Complete, Some(false)));
        // An unreadable status counts as unsettled, never as safe to drop.
        assert!(policy.is_exempt(&ReceiptStatus::from_wire("superseded"), Some(true)));
    }

    #[test]
    fn lifecycle_mirrors_the_runtime_state_machine_exactly() {
        let states = [
            RunLifecycle::Queued,
            RunLifecycle::Running,
            RunLifecycle::Completed,
            RunLifecycle::Failed,
            RunLifecycle::Cancelled,
            RunLifecycle::Interrupted,
            RunLifecycle::LimitReached,
        ];
        assert_eq!(states.len(), RunLifecycle::wire_tokens().len());
        for (state, token) in states.iter().zip(RunLifecycle::wire_tokens()) {
            let encoded = serde_json::to_value(state).unwrap();
            assert_eq!(encoded, serde_json::Value::String((*token).to_string()));
        }
        assert!(!RunLifecycle::Queued.is_terminal());
        assert!(!RunLifecycle::Running.is_terminal());
        for terminal in &states[2..] {
            assert!(terminal.is_terminal(), "{terminal:?} must be terminal");
        }
    }

    #[test]
    fn watermark_rejects_non_advancing_snapshots() {
        let mut mark = RevisionWatermark::new();
        assert!(mark.admit(Revision::new(5)).is_ok());
        assert!(mark.admit(Revision::new(9)).is_ok());
        let err = mark.admit(Revision::new(7)).unwrap_err();
        assert_eq!(err.code, SdkErrorCode::StaleObservation);
        assert_eq!(err.detail("appliedRevision"), Some("9"));
        assert_eq!(err.detail("incomingRevision"), Some("7"));
        // Equal is also not newer.
        assert!(mark.admit(Revision::new(9)).is_err());
        assert_eq!(mark.applied(), Revision::new(9));
    }

    #[test]
    fn lease_credential_never_reaches_debug_or_json() {
        let lease = ControlLease {
            work_id: WorkId::new("work-1").unwrap(),
            attempt_id: AttemptId::new("attempt-1").unwrap(),
            attempt_number: 1,
            claimant: AgentId::new("agent-1").unwrap(),
            acquired_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            expires_at: DateTime::from_timestamp(1_700_000_030, 0).unwrap(),
            revision: Revision::new(1),
            credential: LeaseCredential::new("super-secret-lease-token"),
        };
        let debug = format!("{lease:?}");
        assert!(!debug.contains("super-secret-lease-token"), "{debug}");
        let json = serde_json::to_string(&lease).unwrap();
        assert!(!json.contains("super-secret-lease-token"), "{json}");
        assert!(!json.contains("credential"), "{json}");
    }

    #[test]
    fn artifact_verification_catches_tampering_and_oversize() {
        let content = "diff --git a/x b/x\n".to_string();
        let descriptor = ArtifactDescriptor {
            artifact_id: ArtifactId::new("artifact-1").unwrap(),
            kind: ArtifactKind::ReviewDiff,
            media: ArtifactMedia::UnifiedDiff,
            label: Label::new("review diff").unwrap(),
            byte_len: content.len() as u64,
            digest: ContentDigest::sha256_of(content.as_bytes()),
            retained_until: None,
        };
        let good = ArtifactPayload {
            descriptor: descriptor.clone(),
            content: content.clone(),
        };
        assert!(good.verify(1024).is_ok());

        let tampered = ArtifactPayload {
            descriptor: ArtifactDescriptor {
                byte_len: content.len() as u64 + 1,
                ..descriptor.clone()
            },
            content: content.clone(),
        };
        assert_eq!(
            tampered.verify(1024).unwrap_err().code,
            SdkErrorCode::IntegrityMismatch
        );

        let swapped = ArtifactPayload {
            descriptor: descriptor.clone(),
            content: "totally different".to_string(),
        };
        assert_eq!(
            swapped.verify(1024).unwrap_err().code,
            SdkErrorCode::IntegrityMismatch
        );

        assert_eq!(
            good.verify(4).unwrap_err().code,
            SdkErrorCode::InvalidRequest
        );
    }

    #[test]
    fn digests_must_be_lowercase_hex_of_exact_length() {
        assert!(ContentDigest::sha256_of(b"x").validate().is_ok());
        for bad in ["", "abc", &"A".repeat(64), &"z".repeat(64)] {
            let digest = ContentDigest {
                algorithm: DigestAlgorithm::Sha256,
                hex: bad.to_string(),
            };
            assert!(digest.validate().is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn bounded_text_strips_control_characters_and_truncates() {
        let text = BoundedText::new("line one\nline\u{1b}[31m two\t");
        assert!(!text.as_str().contains('\n'));
        assert!(!text.as_str().contains('\u{1b}'));
        assert!(BoundedText::new("x".repeat(9999)).as_str().len() <= MAX_SUMMARY_BYTES);
    }

    #[test]
    fn unknown_event_kinds_decode_instead_of_failing_the_page() {
        let raw = serde_json::json!({
            "cursor": "42",
            "at": "2026-01-01T00:00:00Z",
            "kind": "unrecognized",
            "wireKind": "future_event"
        });
        let event: PublicEvent = serde_json::from_value(raw).unwrap();
        match event.kind {
            PublicEventKind::Unrecognized { wire_kind } => {
                assert_eq!(wire_kind.as_str(), "future_event");
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }
}
