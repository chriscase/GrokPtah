//! Operator reconciliation contracts for durable always-on runs.
//!
//! An always-on host eventually reaches a state it cannot prove: a worker
//! crashed mid-attempt, a lease expired, a provider answered `unknown`, or the
//! event stream advanced past an operator's cursor. Something has to close
//! those runs out, and the only honest closer is a human operator (or a
//! self-hosted coding agent acting as one).
//!
//! This module is the contract for that. It deliberately provides exactly two
//! capabilities:
//!
//! 1. A **truthful projection** ([`project_attention`]) that reports what the
//!    authority can actually prove about a run, and why it needs attention.
//! 2. An **evidence-only reconciliation action** ([`ReconciliationLedger::apply`])
//!    that records operator evidence and resolves operator-visible state.
//!
//! # What this contract can never do
//!
//! Reconciliation never resends, retries, resumes, or otherwise mutates a
//! provider attempt. That is enforced three ways:
//!
//! * [`ReconcileAction`] is a closed enum with no resend/retry/resume variant,
//!   and [`ReconcileAction::mutates_provider_attempt`] is an exhaustive `match`
//!   returning `false`, so a future variant cannot be added without a decision.
//! * Resolution writes an operator-visible verdict
//!   ([`ReconciliationEntry::resolved_state`]); it never edits the attempt
//!   record itself. [`AttemptObservation`] is an input, never an output.
//! * The owning crate depends only on `serde` and `serde_json`. It has no
//!   network, filesystem, process, credential, or clock dependency, so no code
//!   reachable from here can contact a provider even by mistake. Wall-clock
//!   instants are parameters, not ambient reads, which is also what makes every
//!   projection in this module deterministic under test.
//!
//! # What the host still owns
//!
//! Durability, transport, and authentication. The host persists
//! [`ReconciliationEntry`] values in journal order and rebuilds a ledger with
//! [`ReconciliationLedger::recover`] after a restart or crash-cut.

use serde::{Deserialize, Serialize};

use crate::run::{DurableRunState, IdempotencyKey, RunScope};

/// Contract identifier for operator reconciliation DTOs.
pub const RECONCILIATION_CONTRACT_VERSION: &str = "grokptah.operator-reconciliation.v1";

/// Maximum UTF-8 bytes in an opaque operator-facing reference.
pub const MAX_OPAQUE_REF_BYTES: usize = 128;
/// Maximum UTF-8 bytes in a redacted operator note.
pub const MAX_NOTE_BYTES: usize = 2_048;
/// Maximum UTF-8 bytes in one redacted evidence summary.
pub const MAX_EVIDENCE_SUMMARY_BYTES: usize = 512;
/// Maximum UTF-8 bytes in one evidence digest label.
pub const MAX_EVIDENCE_DIGEST_BYTES: usize = 160;
/// Maximum evidence records accepted in one reconciliation request.
pub const MAX_EVIDENCE_PER_REQUEST: usize = 16;
/// Maximum entries retained in one run's reconciliation ledger.
pub const MAX_LEDGER_ENTRIES: usize = 256;
/// Maximum entries returned by one history page.
pub const MAX_HISTORY_PAGE: usize = 64;
/// Marker substituted for redacted spans in operator-visible text.
pub const REDACTION_MARKER: &str = "[redacted]";
/// Marker appended when redacted text is truncated at its byte bound.
pub const TRUNCATION_MARKER: &str = "[truncated]";

// ---------------------------------------------------------------------------
// Opaque references
// ---------------------------------------------------------------------------

/// A bounded operator-facing identifier that carries no host detail.
///
/// The authority mints these. This type does not derive an opaque value from a
/// real identity — deriving one here would be a non-cryptographic fingerprint
/// dressed up as a privacy boundary. It instead *enforces* opacity: the value
/// must be bounded, free of control characters, free of path and URL syntax,
/// and must not embed the raw session, workspace, or run identity it stands in
/// for (see [`OpaqueRef::validate_for_scope`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueRef(String);

impl OpaqueRef {
    /// Accept a candidate operator-facing reference.
    pub fn new(value: impl Into<String>) -> Result<Self, ReconcileError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_OPAQUE_REF_BYTES {
            return Err(ReconcileError::invalid(
                "opaque ref is empty or exceeds its byte bound",
            ));
        }
        if value.trim() != value {
            return Err(ReconcileError::invalid(
                "opaque ref must not have surrounding whitespace",
            ));
        }
        if value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '/' | '\\' | ':' | '@' | '?' | '#' | '%')
        }) {
            return Err(ReconcileError::invalid(
                "opaque ref must not contain path or URL syntax",
            ));
        }
        if value.split('.').any(|segment| segment == "..") || value.contains("..") {
            return Err(ReconcileError::invalid(
                "opaque ref must not contain a traversal segment",
            ));
        }
        Ok(Self(value))
    }

    /// Borrow the reference value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reject a reference that embeds any part of the identity it stands for.
    ///
    /// This is the check that keeps a "opaque" run reference from quietly
    /// shipping a workspace path or session UUID to an operator surface.
    pub fn validate_for_scope(&self, scope: &RunScope) -> Result<(), ReconcileError> {
        let lowered = self.0.to_ascii_lowercase();
        for identity in [&scope.session_id, &scope.workspace, &scope.run_id] {
            let identity = identity.trim();
            if identity.len() < 4 {
                continue;
            }
            if lowered.contains(&identity.to_ascii_lowercase()) {
                return Err(ReconcileError::invalid(
                    "opaque ref leaks a scoped identity",
                ));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Bounded redaction applied to every operator-visible free-text field.
///
/// The secret list mirrors the host's control-secret registry. Matching is
/// case-insensitive because operators paste values back in odd casings, and a
/// near-miss that leaks the secret is worse than an over-redacted note.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Build a redactor over the host's known control secrets.
    ///
    /// Values shorter than four bytes are dropped: they would match almost any
    /// text and turn every note into redaction markers.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut secrets = secrets
            .into_iter()
            .filter(|secret| secret.trim().len() >= 4)
            .map(|secret| secret.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        secrets.sort();
        secrets.dedup();
        // Longest first, so an overlapping shorter secret cannot split a longer
        // one into two partially-visible halves.
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        Self { secrets }
    }

    /// Number of registered secrets, for host diagnostics.
    pub fn secret_count(&self) -> usize {
        self.secrets.len()
    }

    /// Redact, strip control characters, and truncate to `max_bytes`.
    pub fn redact(&self, text: &str, max_bytes: usize) -> String {
        let mut out = String::with_capacity(text.len().min(max_bytes));
        for character in text.chars() {
            // Tabs and newlines are control characters, so this also flattens
            // any multi-line paste into one bounded operator-visible line.
            if character.is_control() {
                out.push(' ');
            } else {
                out.push(character);
            }
        }
        for secret in &self.secrets {
            out = replace_case_insensitive(&out, secret, REDACTION_MARKER);
        }
        truncate_on_char_boundary(out, max_bytes)
    }

    /// Whether `text` is already free of registered secrets and control bytes.
    pub fn is_clean(&self, text: &str) -> bool {
        if text.chars().any(char::is_control) {
            return false;
        }
        let lowered = text.to_ascii_lowercase();
        !self.secrets.iter().any(|secret| lowered.contains(secret))
    }
}

fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let lowered = haystack.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0usize;
    while let Some(found) = lowered[cursor..].find(needle) {
        let start = cursor + found;
        out.push_str(&haystack[cursor..start]);
        out.push_str(replacement);
        cursor = start + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

fn truncate_on_char_boundary(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let budget = max_bytes.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATION_MARKER);
    text
}

// ---------------------------------------------------------------------------
// Observation inputs
// ---------------------------------------------------------------------------

/// Where an uncertainty actually originates.
///
/// Keeping these apart is the point of the whole projection: a provider that
/// answered `unknown` needs a different operator response than a lease that
/// expired, and neither is the same as a run parked awaiting a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyDomain {
    /// The model or provider's own outcome is ambiguous or unreported.
    ModelOrProvider,
    /// Our worker, lease, or host failed; the provider may be fine.
    WorkerOrLease,
    /// Nothing is broken; the run is waiting on an operator decision.
    OperatorDecision,
}

impl UncertaintyDomain {
    /// Every domain, in declaration order.
    pub const ALL: [Self; 3] = [
        Self::ModelOrProvider,
        Self::WorkerOrLease,
        Self::OperatorDecision,
    ];

    /// Stable wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelOrProvider => "model_or_provider",
            Self::WorkerOrLease => "worker_or_lease",
            Self::OperatorDecision => "operator_decision",
        }
    }
}

/// How firmly the authority can stand behind the projected run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunConfidence {
    /// Durable state is corroborated by evidence no older than the policy bound.
    Confirmed,
    /// Durable state is the last thing written, but nothing corroborates it now.
    Unconfirmed,
    /// The authority cannot prove the run's state at all.
    Uncertain,
}

/// Terminal disposition a provider reported for an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderState {
    /// The provider reports the attempt still running.
    Active,
    /// The provider reports verified terminal success.
    Completed,
    /// The provider reports terminal failure.
    Failed,
    /// The provider reports the attempt cancelled.
    Cancelled,
    /// The provider returned a state the adapter does not recognize.
    Unknown,
}

impl ProviderState {
    /// Whether the provider claims the attempt is over.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// What the local authority believes about a single provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The attempt was claimed and is still in flight.
    InFlight,
    /// The attempt is durably recorded as succeeded.
    Succeeded,
    /// The attempt is durably recorded as failed.
    Failed,
    /// The attempt was claimed but its outcome was never durably recorded.
    ///
    /// This is the receipt state a crash between "send" and "record" leaves
    /// behind, and it is exactly what an operator must close out by hand.
    Unknown,
}

/// The local attempt/receipt projection for the run's most recent attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptObservation {
    /// Opaque attempt reference.
    pub attempt_ref: OpaqueRef,
    /// Local durable outcome for the attempt.
    pub outcome: AttemptOutcome,
}

/// The worker lease covering the run at observation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseObservation {
    /// Opaque reference to the lease holder.
    pub holder_ref: OpaqueRef,
    /// Monotonic fence that increments on every takeover or recovery.
    pub epoch: u64,
    /// Lease expiry, as milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
    /// Whether the authority restarted while this run was still live.
    pub host_restarted: bool,
}

/// The provider projection for the run at observation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderObservation {
    /// Opaque reference to the provider-side run.
    pub provider_run_ref: OpaqueRef,
    /// Provider-reported state.
    pub state: ProviderState,
}

/// The retained event window and the operator's position in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamObservation {
    /// Lowest sequence still retained by the journal.
    pub retained_from_seq: u64,
    /// Highest sequence written to the journal.
    pub retained_through_seq: u64,
    /// The last sequence this operator surface actually consumed.
    pub operator_cursor: Option<u64>,
}

impl StreamObservation {
    /// Whether entries between the operator cursor and the retained window are
    /// permanently gone.
    ///
    /// A cursor of `n` means "I have seen through `n`", so the next entry the
    /// operator needs is `n + 1`. The window starts at `retained_from_seq`;
    /// anything below that is unrecoverable.
    pub fn has_gap(&self) -> bool {
        match self.operator_cursor {
            Some(cursor) => cursor.saturating_add(1) < self.retained_from_seq,
            // No cursor means a fresh reader, which starts at the window head.
            None => false,
        }
    }
}

/// A bounded, pure snapshot of everything the projection is allowed to read.
///
/// Every clock value is supplied by the caller. Nothing in this module reads an
/// ambient clock, which is what makes [`project_attention`] byte-reproducible
/// across the desktop cockpit, a CLI, and a test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunObservation {
    /// Opaque operator-facing run reference.
    pub run_ref: OpaqueRef,
    /// Last durably written lifecycle state.
    pub state: DurableRunState,
    /// Monotonic record revision, used as the reconciliation fence.
    pub revision: u64,
    /// Journal sequence at the moment of observation.
    pub observed_seq: u64,
    /// Observation instant, as milliseconds since the Unix epoch.
    pub observed_at_ms: u64,
    /// When the run last produced corroborating evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_evidence_at_ms: Option<u64>,
    /// The run's wall-clock deadline, if bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    /// Whether a cancel has been requested but not yet confirmed terminal.
    #[serde(default)]
    pub cancel_requested: bool,
    /// The covering lease, if the run is worker-backed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<LeaseObservation>,
    /// The provider projection, if the run is provider-backed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderObservation>,
    /// The most recent attempt, if one has been claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<AttemptObservation>,
    /// Retained journal window and operator cursor.
    #[serde(default)]
    pub stream: StreamObservation,
}

impl RunObservation {
    /// Validate the bounded snapshot before projecting from it.
    pub fn validate(&self) -> Result<(), ReconcileError> {
        if self.stream.retained_through_seq < self.stream.retained_from_seq {
            return Err(ReconcileError::invalid("retained event window is inverted"));
        }
        if self.observed_seq < self.stream.retained_from_seq.saturating_sub(1) {
            return Err(ReconcileError::invalid(
                "observed sequence precedes the retained window",
            ));
        }
        if self
            .last_evidence_at_ms
            .is_some_and(|evidence| evidence > self.observed_at_ms)
        {
            return Err(ReconcileError::invalid(
                "evidence timestamp is in the observation future",
            ));
        }
        Ok(())
    }
}

/// Thresholds that turn a snapshot into an attention verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionPolicy {
    /// Age past which corroborating evidence is treated as stale.
    pub max_evidence_age_ms: u64,
    /// Grace period allowed past a deadline before it is reported.
    pub deadline_grace_ms: u64,
    /// Grace period allowed past a lease expiry before it is reported.
    pub lease_grace_ms: u64,
}

impl Default for AttentionPolicy {
    fn default() -> Self {
        Self {
            max_evidence_age_ms: 120_000,
            deadline_grace_ms: 5_000,
            lease_grace_ms: 5_000,
        }
    }
}

impl AttentionPolicy {
    /// Reject a policy that would disable staleness detection outright.
    pub fn validate(&self) -> Result<(), ReconcileError> {
        if self.max_evidence_age_ms == 0 {
            return Err(ReconcileError::invalid(
                "max_evidence_age_ms must be greater than zero",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Attention projection
// ---------------------------------------------------------------------------

/// Why a run needs an operator.
///
/// Ordered most- to least-severe. The order is load-bearing: [`project_attention`]
/// emits reasons in this order so two surfaces rendering the same run agree on
/// which line goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    /// An attempt was claimed and its outcome was never durably recorded.
    UncertainOutcome,
    /// The authority restarted while this run was live.
    CrashRecovered,
    /// The worker lease expired while the run was still live.
    LeaseExpired,
    /// The provider and local authority disagree about the attempt's fate.
    ProviderAmbiguity,
    /// A cancel was requested but never confirmed terminal.
    CancelUnconfirmed,
    /// The run passed its wall-clock deadline without becoming terminal.
    DeadlineExceeded,
    /// Journal entries the operator has not read have already been evicted.
    StreamGap,
    /// No corroborating evidence within the policy's freshness bound.
    StaleObservation,
}

impl AttentionReason {
    /// Every reason, in severity order.
    pub const ALL: [Self; 8] = [
        Self::UncertainOutcome,
        Self::CrashRecovered,
        Self::LeaseExpired,
        Self::ProviderAmbiguity,
        Self::CancelUnconfirmed,
        Self::DeadlineExceeded,
        Self::StreamGap,
        Self::StaleObservation,
    ];

    /// Which kind of uncertainty this reason represents.
    pub fn domain(self) -> UncertaintyDomain {
        match self {
            Self::UncertainOutcome | Self::ProviderAmbiguity => UncertaintyDomain::ModelOrProvider,
            Self::CrashRecovered | Self::LeaseExpired | Self::StreamGap => {
                UncertaintyDomain::WorkerOrLease
            }
            Self::CancelUnconfirmed | Self::DeadlineExceeded | Self::StaleObservation => {
                UncertaintyDomain::OperatorDecision
            }
        }
    }

    /// How hard this reason blocks an operator.
    pub fn severity(self) -> AttentionSeverity {
        match self {
            Self::UncertainOutcome | Self::ProviderAmbiguity => AttentionSeverity::Blocking,
            Self::CrashRecovered | Self::LeaseExpired | Self::CancelUnconfirmed => {
                AttentionSeverity::Degraded
            }
            Self::DeadlineExceeded | Self::StreamGap | Self::StaleObservation => {
                AttentionSeverity::Advisory
            }
        }
    }

    /// Whether this reason means the run's state is unprovable, as opposed to
    /// provable-but-unwelcome.
    pub fn forces_uncertainty(self) -> bool {
        matches!(
            self,
            Self::UncertainOutcome | Self::ProviderAmbiguity | Self::CrashRecovered
        )
    }

    /// Stable wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UncertainOutcome => "uncertain_outcome",
            Self::CrashRecovered => "crash_recovered",
            Self::LeaseExpired => "lease_expired",
            Self::ProviderAmbiguity => "provider_ambiguity",
            Self::CancelUnconfirmed => "cancel_unconfirmed",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::StreamGap => "stream_gap",
            Self::StaleObservation => "stale_observation",
        }
    }
}

/// How hard an attention reason blocks an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSeverity {
    /// The run cannot be trusted until an operator resolves it.
    Blocking,
    /// The run is degraded and will not self-heal.
    Degraded,
    /// Worth surfacing, but the run may still finish on its own.
    Advisory,
}

/// The authoritative operator-facing view of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttention {
    /// Contract identifier, so a consumer can refuse an unknown revision.
    pub contract: String,
    /// Opaque operator-facing run reference.
    pub run_ref: OpaqueRef,
    /// Last durably written lifecycle state.
    pub state: DurableRunState,
    /// How firmly the authority stands behind `state`.
    pub confidence: RunConfidence,
    /// Whether an operator has something to do here.
    pub needs_attention: bool,
    /// Reasons in severity order; empty when `needs_attention` is false.
    pub reasons: Vec<AttentionReason>,
    /// Severity of the most severe reason, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<AttentionSeverity>,
    /// Distinct domains present in `reasons`, in declaration order.
    pub domains: Vec<UncertaintyDomain>,
    /// Journal sequence the projection was taken at.
    pub observed_seq: u64,
    /// Record revision an operator must fence against.
    pub revision: u64,
}

impl RunAttention {
    /// Whether any reason belongs to the given domain.
    pub fn has_domain(&self, domain: UncertaintyDomain) -> bool {
        self.domains.contains(&domain)
    }
}

/// Derive the truthful operator view of a run at an explicit instant.
///
/// `observation.observed_at_ms` is a parameter rather than an ambient clock
/// read so a cockpit, a CLI, and a test provably produce identical output for
/// one record.
pub fn project_attention(
    observation: &RunObservation,
    policy: &AttentionPolicy,
) -> Result<RunAttention, ReconcileError> {
    observation.validate()?;
    policy.validate()?;

    let terminal = is_terminal(observation.state);
    let mut reasons = Vec::new();

    // Attempt-level uncertainty survives terminality: a run marked Completed
    // whose last attempt outcome was never recorded is still unproven.
    if observation
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.outcome == AttemptOutcome::Unknown)
    {
        reasons.push(AttentionReason::UncertainOutcome);
    }

    if let Some(lease) = &observation.lease {
        if lease.host_restarted && !terminal {
            reasons.push(AttentionReason::CrashRecovered);
        }
        let expiry_deadline = lease.expires_at_ms.saturating_add(policy.lease_grace_ms);
        if !terminal && observation.observed_at_ms > expiry_deadline {
            reasons.push(AttentionReason::LeaseExpired);
        }
    }

    if let Some(provider) = &observation.provider
        && provider_disagrees(provider.state, observation.state)
    {
        reasons.push(AttentionReason::ProviderAmbiguity);
    }

    if observation.cancel_requested && !terminal {
        reasons.push(AttentionReason::CancelUnconfirmed);
    }

    if let Some(deadline) = observation.deadline_at_ms {
        let deadline = deadline.saturating_add(policy.deadline_grace_ms);
        if !terminal && observation.observed_at_ms > deadline {
            reasons.push(AttentionReason::DeadlineExceeded);
        }
    }

    if observation.stream.has_gap() {
        reasons.push(AttentionReason::StreamGap);
    }

    if !terminal && is_stale(observation, policy) {
        reasons.push(AttentionReason::StaleObservation);
    }

    reasons.sort();
    reasons.dedup();

    let confidence = if reasons
        .iter()
        .copied()
        .any(AttentionReason::forces_uncertainty)
    {
        RunConfidence::Uncertain
    } else if reasons.is_empty() {
        RunConfidence::Confirmed
    } else {
        RunConfidence::Unconfirmed
    };

    let severity = reasons.iter().map(|reason| reason.severity()).min();
    let mut domains = Vec::new();
    for domain in UncertaintyDomain::ALL {
        if reasons.iter().any(|reason| reason.domain() == domain) {
            domains.push(domain);
        }
    }

    Ok(RunAttention {
        contract: RECONCILIATION_CONTRACT_VERSION.into(),
        run_ref: observation.run_ref.clone(),
        state: observation.state,
        confidence,
        needs_attention: !reasons.is_empty(),
        reasons,
        severity,
        domains,
        observed_seq: observation.observed_seq,
        revision: observation.revision,
    })
}

fn is_terminal(state: DurableRunState) -> bool {
    matches!(
        state,
        DurableRunState::Completed
            | DurableRunState::Failed
            | DurableRunState::Cancelled
            | DurableRunState::Interrupted
            | DurableRunState::LimitReached
    )
}

fn is_stale(observation: &RunObservation, policy: &AttentionPolicy) -> bool {
    match observation.last_evidence_at_ms {
        Some(evidence) => {
            observation.observed_at_ms.saturating_sub(evidence) > policy.max_evidence_age_ms
        }
        // A live run that never produced evidence is stale by definition.
        None => true,
    }
}

/// Whether a provider projection contradicts the local durable state.
///
/// `Unknown` always contradicts: an adapter that cannot classify the provider's
/// answer has given us no basis to agree with ourselves, and reporting "I could
/// not parse this" as agreement is how a stuck run gets shown green.
///
/// A provider ahead of a non-terminal local state is *not* a contradiction.
/// `Queued` and `Running` mean the local record has not caught up yet, which is
/// ordinary propagation lag and self-heals; flagging it would bury the real
/// ambiguities in noise. A provider that disagrees with a *terminal* local
/// state is always reported, because that one never self-heals.
fn provider_disagrees(provider: ProviderState, local: DurableRunState) -> bool {
    match provider {
        ProviderState::Unknown => true,
        ProviderState::Active => is_terminal(local),
        ProviderState::Completed => !matches!(
            local,
            DurableRunState::Completed | DurableRunState::Running | DurableRunState::Queued
        ),
        ProviderState::Failed => !matches!(
            local,
            DurableRunState::Failed
                | DurableRunState::Running
                | DurableRunState::Queued
                | DurableRunState::LimitReached
        ),
        ProviderState::Cancelled => !matches!(
            local,
            DurableRunState::Cancelled | DurableRunState::Running | DurableRunState::Queued
        ),
    }
}

// ---------------------------------------------------------------------------
// Operator actions
// ---------------------------------------------------------------------------

/// The complete set of operator reconciliation actions.
///
/// There is deliberately no resend, retry, resume, or re-dispatch variant.
/// Reconciliation records what an operator learned and what verdict they
/// reached; making the provider do something again is a different capability
/// with a different approval, on a different surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAction {
    /// Attach evidence without asserting any outcome.
    RecordEvidence,
    /// Acknowledge the attention; state stays exactly as unprovable as it was.
    Acknowledge,
    /// Declare, on the attached evidence, that the attempt completed.
    ResolveCompleted,
    /// Declare, on the attached evidence, that the attempt failed.
    ResolveFailed,
    /// Declare, on the attached evidence, that the cancel took effect.
    ResolveCancelled,
}

impl ReconcileAction {
    /// Every action, in declaration order.
    pub const ALL: [Self; 5] = [
        Self::RecordEvidence,
        Self::Acknowledge,
        Self::ResolveCompleted,
        Self::ResolveFailed,
        Self::ResolveCancelled,
    ];

    /// Whether this action writes an operator verdict over the run's state.
    pub fn resolves_state(self) -> bool {
        self.resolved_state().is_some()
    }

    /// The operator-visible terminal state this action asserts, if any.
    pub fn resolved_state(self) -> Option<DurableRunState> {
        match self {
            Self::RecordEvidence | Self::Acknowledge => None,
            Self::ResolveCompleted => Some(DurableRunState::Completed),
            Self::ResolveFailed => Some(DurableRunState::Failed),
            Self::ResolveCancelled => Some(DurableRunState::Cancelled),
        }
    }

    /// Whether this action could resend, retry, or otherwise mutate a provider
    /// attempt.
    ///
    /// Always `false`. This is written as an exhaustive `match` rather than a
    /// bare `false` so that adding a variant is a compile error until someone
    /// states, in this function, what it does to a provider attempt.
    pub fn mutates_provider_attempt(self) -> bool {
        match self {
            Self::RecordEvidence => false,
            Self::Acknowledge => false,
            Self::ResolveCompleted => false,
            Self::ResolveFailed => false,
            Self::ResolveCancelled => false,
        }
    }

    /// Whether this action requires at least one evidence record.
    ///
    /// Asserting an outcome without evidence is how a ledger becomes fiction.
    pub fn requires_evidence(self) -> bool {
        self.resolves_state()
    }

    /// Stable wire label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordEvidence => "record_evidence",
            Self::Acknowledge => "acknowledge",
            Self::ResolveCompleted => "resolve_completed",
            Self::ResolveFailed => "resolve_failed",
            Self::ResolveCancelled => "resolve_cancelled",
        }
    }
}

/// What kind of thing an operator looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A provider console, dashboard, or API projection.
    ProviderProjection,
    /// A host log, journal excerpt, or crash report.
    HostJournal,
    /// A workspace or repository state check.
    WorkspaceInspection,
    /// A human statement of fact recorded by the operator.
    OperatorStatement,
}

/// One bounded, digest-addressed piece of evidence.
///
/// Evidence carries a digest and a redacted summary, never a payload. Raw
/// provider output routinely contains credentials and customer data, and an
/// operator ledger is one of the longest-lived records in the system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRecord {
    /// What the operator looked at.
    pub kind: EvidenceKind,
    /// Content digest of the underlying material, e.g. `sha256:…`.
    pub digest: String,
    /// Redacted, bounded human summary.
    pub summary: String,
}

impl EvidenceRecord {
    /// Validate a single evidence record against `redactor`.
    pub fn validate(&self, redactor: &Redactor) -> Result<(), ReconcileError> {
        if self.digest.trim().is_empty() || self.digest.len() > MAX_EVIDENCE_DIGEST_BYTES {
            return Err(ReconcileError::invalid(
                "evidence digest is empty or exceeds its bound",
            ));
        }
        if self
            .digest
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(ReconcileError::invalid(
                "evidence digest must not contain whitespace",
            ));
        }
        if self.summary.trim().is_empty() || self.summary.len() > MAX_EVIDENCE_SUMMARY_BYTES {
            return Err(ReconcileError::invalid(
                "evidence summary is empty or exceeds its bound",
            ));
        }
        if !redactor.is_clean(&self.summary) {
            return Err(ReconcileError::invalid(
                "evidence summary is not redaction-clean",
            ));
        }
        Ok(())
    }
}

/// The operator, and the authority they act under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorIdentity {
    /// Opaque operator reference.
    pub operator_ref: OpaqueRef,
    /// Opaque reference to the authority boundary the operator acts under.
    pub authority_ref: OpaqueRef,
}

/// One operator reconciliation intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileRequest {
    /// Fresh idempotency key for this intent.
    pub request_id: IdempotencyKey,
    /// Exact run identity fence.
    pub scope: RunScope,
    /// Revision the operator actually looked at.
    pub expected_revision: u64,
    /// The action, which can never touch a provider attempt.
    pub action: ReconcileAction,
    /// Bounded evidence set.
    pub evidence: Vec<EvidenceRecord>,
    /// Redacted, bounded operator note.
    pub note: String,
    /// Who is acting, and under what authority.
    pub operator: OperatorIdentity,
}

impl ReconcileRequest {
    /// Validate the intent without granting any authority.
    pub fn validate(&self, redactor: &Redactor) -> Result<(), ReconcileError> {
        if self.request_id.trim().is_empty() || self.request_id.len() > 256 {
            return Err(ReconcileError::invalid(
                "request_id is empty or exceeds its bound",
            ));
        }
        self.scope.validate().map_err(ReconcileError::invalid)?;
        if self.evidence.len() > MAX_EVIDENCE_PER_REQUEST {
            return Err(ReconcileError::limit(
                "evidence record count exceeds its bound",
            ));
        }
        if self.action.requires_evidence() && self.evidence.is_empty() {
            return Err(ReconcileError::invalid(
                "resolving an outcome requires at least one evidence record",
            ));
        }
        for evidence in &self.evidence {
            evidence.validate(redactor)?;
        }
        if self.note.len() > MAX_NOTE_BYTES {
            return Err(ReconcileError::invalid(
                "operator note exceeds its byte bound",
            ));
        }
        if !redactor.is_clean(&self.note) {
            return Err(ReconcileError::invalid(
                "operator note is not redaction-clean",
            ));
        }
        Ok(())
    }

    /// The exact fenced payload used for idempotent replay comparison.
    ///
    /// This is the request minus its identity key: two calls with the same
    /// `request_id` are the same intent only if every one of these fields
    /// matches. Comparison is on canonical JSON rather than a digest so that
    /// replay detection makes no collision-resistance assumption.
    fn payload(&self) -> Result<String, ReconcileError> {
        serde_json::to_string(&serde_json::json!({
            "scope": self.scope,
            "expectedRevision": self.expected_revision,
            "action": self.action,
            "evidence": self.evidence,
            "note": self.note,
            "operator": self.operator,
        }))
        .map_err(|_| ReconcileError::invalid("reconciliation payload is not serializable"))
    }
}

// ---------------------------------------------------------------------------
// Ledger entries
// ---------------------------------------------------------------------------

/// One durable, append-only reconciliation record.
///
/// This is both the audit trail and the recovery log: [`ReconciliationLedger::recover`]
/// rebuilds all in-memory state from a sequence of these and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationEntry {
    /// Strictly increasing per-run ledger sequence, starting at 1.
    pub seq: u64,
    /// Idempotency key of the intent that produced this entry.
    pub request_id: IdempotencyKey,
    /// Exact fenced payload, used to detect request-id reuse.
    pub payload: String,
    /// The action taken.
    pub action: ReconcileAction,
    /// Revision the operator fenced against.
    pub expected_revision: u64,
    /// Run state as observed at apply time.
    pub observed_state: DurableRunState,
    /// Attention reasons present at apply time, in severity order.
    pub observed_reasons: Vec<AttentionReason>,
    /// Operator verdict written by this entry, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_state: Option<DurableRunState>,
    /// Bounded evidence attached to the entry.
    pub evidence: Vec<EvidenceRecord>,
    /// Redacted operator note.
    pub note: String,
    /// Who acted, and under what authority.
    pub operator: OperatorIdentity,
    /// Apply instant, as milliseconds since the Unix epoch.
    pub recorded_at_ms: u64,
}

/// Result of a reconciliation apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "camelCase")]
pub enum ReconcileOutcome {
    /// A new entry was appended.
    Applied {
        /// The appended entry.
        entry: ReconciliationEntry,
        /// The projection after the entry was applied.
        attention: RunAttention,
    },
    /// An identical intent had already been applied; nothing was appended.
    Replayed {
        /// The originally appended entry.
        entry: ReconciliationEntry,
        /// The projection after the original entry.
        attention: RunAttention,
    },
}

impl ReconcileOutcome {
    /// The entry, whether freshly appended or replayed.
    pub fn entry(&self) -> &ReconciliationEntry {
        match self {
            Self::Applied { entry, .. } | Self::Replayed { entry, .. } => entry,
        }
    }

    /// The projection carried with the outcome.
    pub fn attention(&self) -> &RunAttention {
        match self {
            Self::Applied { attention, .. } | Self::Replayed { attention, .. } => attention,
        }
    }

    /// Whether this call actually appended a new entry.
    pub fn is_new(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }
}

/// One bounded, cursor-addressed page of reconciliation history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationPage {
    /// Entries in sequence order.
    pub entries: Vec<ReconciliationEntry>,
    /// Sequence to pass as the next cursor, or `None` at the end of history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    /// True when entries between the requested cursor and the retained window
    /// have been evicted. A gap is never presented as complete history.
    pub cursor_expired: bool,
    /// Lowest sequence still retained.
    pub retained_from_seq: u64,
    /// Highest sequence written.
    pub retained_through_seq: u64,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Stable reconciliation error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileErrorCode {
    /// The request failed a bounds or shape check.
    InvalidRequest,
    /// The operator fenced against a revision that has since moved.
    StaleRevision,
    /// The request id was reused with a different payload.
    Conflict,
    /// The run has already been resolved by another operator.
    AlreadyResolved,
    /// A bounded resource is exhausted.
    LimitReached,
    /// The run is not available to this authority.
    ///
    /// Unknown runs and cross-authority runs return this identical error, so
    /// the surface cannot be used to probe whether a run exists.
    NotAvailable,
}

/// A bounded, share-safe reconciliation error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileError {
    /// Stable category.
    pub code: ReconcileErrorCode,
    /// Share-safe message; never contains scope identity.
    pub message: String,
}

impl ReconcileError {
    /// Build an error with an explicit code.
    pub fn new(code: ReconcileErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(ReconcileErrorCode::InvalidRequest, message)
    }

    fn limit(message: impl Into<String>) -> Self {
        Self::new(ReconcileErrorCode::LimitReached, message)
    }

    /// The single non-disclosing error used for both "unknown" and "not yours".
    ///
    /// Callers must not vary this by cause; the whole point is that an
    /// unauthorized caller learns nothing about existence.
    pub fn not_available() -> Self {
        Self::new(
            ReconcileErrorCode::NotAvailable,
            "reconciliation record is not available to this authority",
        )
    }
}

impl std::fmt::Display for ReconcileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ReconcileError {}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

/// The durable operator reconciliation ledger for one run.
///
/// The ledger is pure state over an append-only entry list. The host owns
/// persistence; after a restart it replays stored entries through
/// [`ReconciliationLedger::recover`], which rebuilds the cursor, the resolution
/// verdict, and the idempotency index from the entries alone.
#[derive(Debug, Clone)]
pub struct ReconciliationLedger {
    scope: RunScope,
    authority_ref: OpaqueRef,
    entries: Vec<ReconciliationEntry>,
    next_seq: u64,
    evicted: u64,
}

impl ReconciliationLedger {
    /// Open an empty ledger bound to one run and one authority.
    pub fn new(scope: RunScope, authority_ref: OpaqueRef) -> Result<Self, ReconcileError> {
        scope.validate().map_err(ReconcileError::invalid)?;
        Ok(Self {
            scope,
            authority_ref,
            entries: Vec::new(),
            next_seq: 1,
            evicted: 0,
        })
    }

    /// Rebuild a ledger from durably stored entries after a restart.
    ///
    /// Fails closed on a torn or reordered journal: sequences must be strictly
    /// increasing, and a duplicated `request_id` must carry an identical
    /// payload. A crash between "append" and "acknowledge" therefore recovers
    /// cleanly, while a corrupted journal is rejected rather than half-trusted.
    pub fn recover(
        scope: RunScope,
        authority_ref: OpaqueRef,
        entries: Vec<ReconciliationEntry>,
    ) -> Result<Self, ReconcileError> {
        let mut ledger = Self::new(scope, authority_ref)?;
        let mut previous: Option<u64> = None;
        for entry in &entries {
            if entry.seq == 0 {
                return Err(ReconcileError::invalid("ledger sequence must start at 1"));
            }
            if previous.is_some_and(|last| entry.seq <= last) {
                return Err(ReconcileError::invalid(
                    "ledger sequences must strictly increase",
                ));
            }
            if let Some(existing) = entries.iter().find(|candidate| {
                candidate.request_id == entry.request_id && candidate.seq != entry.seq
            }) {
                if existing.payload != entry.payload {
                    return Err(ReconcileError::new(
                        ReconcileErrorCode::Conflict,
                        "ledger contains a reused request id with a different payload",
                    ));
                }
                return Err(ReconcileError::new(
                    ReconcileErrorCode::Conflict,
                    "ledger contains a duplicated request id",
                ));
            }
            previous = Some(entry.seq);
        }
        if let Some(first) = entries.first() {
            // Entries below the retained window were pruned before the crash;
            // history pages must keep reporting that gap.
            ledger.evicted = first.seq.saturating_sub(1);
        }
        ledger.next_seq = entries
            .last()
            .map_or(1, |entry| entry.seq.saturating_add(1));
        ledger.entries = entries;
        Ok(ledger)
    }

    /// The run this ledger is bound to.
    pub fn scope(&self) -> &RunScope {
        &self.scope
    }

    /// Next sequence this ledger will assign.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Number of entries evicted by the retention bound.
    pub fn evicted_count(&self) -> u64 {
        self.evicted
    }

    /// The operator verdict, if any operator has resolved this run.
    ///
    /// The first resolving entry wins; later ones are rejected as
    /// [`ReconcileErrorCode::AlreadyResolved`].
    pub fn resolution(&self) -> Option<&ReconciliationEntry> {
        self.entries
            .iter()
            .find(|entry| entry.resolved_state.is_some())
    }

    /// The full retained audit trail, oldest first.
    pub fn audit(&self) -> &[ReconciliationEntry] {
        &self.entries
    }

    /// Lowest sequence still retained, or the next sequence when empty.
    pub fn retained_from_seq(&self) -> u64 {
        self.entries
            .first()
            .map_or(self.next_seq, |entry| entry.seq)
    }

    /// Whether any sequence after `cursor` has been pruned.
    ///
    /// Sequences are assigned contiguously, so the entry an operator wants
    /// next is always `cursor + 1`. A retained window is not enough to answer
    /// this: [`Self::enforce_retention`] pins the resolving entry, so the
    /// retained set can have a hole above its own first element. Comparing
    /// against the actual next retained sequence is exact in either shape.
    fn has_gap_after(&self, cursor: u64) -> bool {
        let expected_next = cursor.saturating_add(1);
        if expected_next > self.next_seq.saturating_sub(1) {
            // The caller is at the head; there is nothing yet to have lost.
            return false;
        }
        match self.entries.iter().find(|entry| entry.seq > cursor) {
            Some(first) => first.seq > expected_next,
            // Sequences were written past the cursor but none survive.
            None => true,
        }
    }

    /// Apply one operator intent.
    ///
    /// Ordering matters and is deliberate:
    /// idempotent replay is checked *before* the revision fence, so a client
    /// retrying after a dropped response gets its original answer rather than a
    /// `StaleRevision` caused by its own earlier success.
    ///
    /// # Host obligation
    ///
    /// `observation` must be the authority's snapshot of *this* ledger's run.
    /// The ledger cannot verify that: [`RunObservation`] carries an opaque
    /// reference by design, and resolving it back to a scope would defeat the
    /// privacy boundary the reference exists for. The ledger does check that
    /// the reference does not leak the scope it stands for, and it fences the
    /// request scope and authority, but pairing the right snapshot with the
    /// right ledger stays with the caller that loaded both.
    pub fn apply(
        &mut self,
        request: &ReconcileRequest,
        observation: &RunObservation,
        policy: &AttentionPolicy,
        redactor: &Redactor,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        request.validate(redactor)?;
        if request.scope != self.scope {
            return Err(ReconcileError::not_available());
        }
        if request.operator.authority_ref != self.authority_ref {
            return Err(ReconcileError::not_available());
        }
        observation.validate()?;
        if observation.run_ref.validate_for_scope(&self.scope).is_err() {
            return Err(ReconcileError::invalid(
                "opaque ref leaks a scoped identity",
            ));
        }

        let payload = request.payload()?;

        if let Some(existing) = self
            .entries
            .iter()
            .find(|entry| entry.request_id == request.request_id)
        {
            if existing.payload != payload {
                return Err(ReconcileError::new(
                    ReconcileErrorCode::Conflict,
                    "request id was reused with a different reconciliation payload",
                ));
            }
            let attention = self.project_with_resolution(observation, policy)?;
            return Ok(ReconcileOutcome::Replayed {
                entry: existing.clone(),
                attention,
            });
        }

        if request.expected_revision != observation.revision {
            return Err(ReconcileError::new(
                ReconcileErrorCode::StaleRevision,
                "run revision moved after the operator observed it",
            ));
        }

        if request.action.resolves_state()
            && let Some(existing) = self.resolution()
        {
            // A second operator reaching a verdict is a real event, not a
            // retry: it must be told, not silently merged.
            return Err(ReconcileError::new(
                ReconcileErrorCode::AlreadyResolved,
                format!(
                    "run was already resolved at ledger sequence {} and cannot be re-resolved",
                    existing.seq
                ),
            ));
        }

        let attention_before = project_attention(observation, policy)?;
        let entry = ReconciliationEntry {
            seq: self.next_seq,
            request_id: request.request_id.clone(),
            payload,
            action: request.action,
            expected_revision: request.expected_revision,
            observed_state: observation.state,
            observed_reasons: attention_before.reasons.clone(),
            resolved_state: request.action.resolved_state(),
            evidence: request.evidence.clone(),
            note: redactor.redact(&request.note, MAX_NOTE_BYTES),
            operator: request.operator.clone(),
            recorded_at_ms: observation.observed_at_ms,
        };

        self.next_seq = self.next_seq.saturating_add(1);
        self.entries.push(entry.clone());
        self.enforce_retention();

        let attention = self.project_with_resolution(observation, policy)?;
        Ok(ReconcileOutcome::Applied { entry, attention })
    }

    /// Project the run, folding in any operator verdict already recorded.
    ///
    /// A resolved run reports the operator's state at `Confirmed` confidence
    /// with no outstanding reasons: the operator *is* the evidence of record.
    pub fn project_with_resolution(
        &self,
        observation: &RunObservation,
        policy: &AttentionPolicy,
    ) -> Result<RunAttention, ReconcileError> {
        let mut attention = project_attention(observation, policy)?;
        if let Some(resolution) = self.resolution()
            && let Some(state) = resolution.resolved_state
        {
            attention.state = state;
            attention.confidence = RunConfidence::Confirmed;
            attention.needs_attention = false;
            attention.reasons.clear();
            attention.severity = None;
            attention.domains.clear();
        }
        Ok(attention)
    }

    /// Read a bounded page of history for an authorized operator.
    ///
    /// `after` is exclusive. `cursor_expired` is true when the requested cursor
    /// sits below the retained window, so an operator can never mistake a
    /// pruned span for an empty one.
    pub fn history(
        &self,
        binding: &AuthorityBinding,
        after: Option<u64>,
        limit: usize,
    ) -> Result<ReconciliationPage, ReconcileError> {
        self.authorize(binding)?;
        let limit = limit.clamp(1, MAX_HISTORY_PAGE);
        let start = after.unwrap_or(0);
        let entries = self
            .entries
            .iter()
            .filter(|entry| entry.seq > start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let last_seq = entries.last().map(|entry| entry.seq);
        let next_cursor = match last_seq {
            Some(seq) if self.entries.last().is_some_and(|entry| entry.seq > seq) => Some(seq),
            _ => None,
        };
        Ok(ReconciliationPage {
            entries,
            next_cursor,
            cursor_expired: self.has_gap_after(start),
            retained_from_seq: self.retained_from_seq(),
            retained_through_seq: self.next_seq.saturating_sub(1),
        })
    }

    /// Read one entry by sequence for an authorized operator.
    pub fn inspect(
        &self,
        binding: &AuthorityBinding,
        seq: u64,
    ) -> Result<ReconciliationEntry, ReconcileError> {
        self.authorize(binding)?;
        self.entries
            .iter()
            .find(|entry| entry.seq == seq)
            .cloned()
            // An evicted or never-written sequence is reported exactly like an
            // unauthorized one.
            .ok_or_else(ReconcileError::not_available)
    }

    /// Fail closed unless the binding matches this ledger's authority and run.
    fn authorize(&self, binding: &AuthorityBinding) -> Result<(), ReconcileError> {
        if binding.authority_ref != self.authority_ref
            || binding.session_id != self.scope.session_id
            || binding.workspace != self.scope.workspace
        {
            return Err(ReconcileError::not_available());
        }
        Ok(())
    }

    /// Drop the oldest entries once the ledger exceeds its bound.
    ///
    /// The resolving entry is never evicted: losing it would let an already
    /// resolved run be re-resolved by a second operator.
    fn enforce_retention(&mut self) {
        while self.entries.len() > MAX_LEDGER_ENTRIES {
            let evictable = self
                .entries
                .iter()
                .position(|entry| entry.resolved_state.is_none());
            let Some(index) = evictable else { break };
            self.entries.remove(index);
            self.evicted = self.evicted.saturating_add(1);
        }
    }
}

/// The identity fence a read-only operator surface must present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityBinding {
    /// Opaque reference to the authority boundary.
    pub authority_ref: OpaqueRef,
    /// Authenticated session identity.
    pub session_id: String,
    /// Approved workspace identity.
    pub workspace: String,
}

/// List the runs an operator may see, without disclosing the ones they may not.
///
/// Unbound ledgers are dropped silently. The result carries no count, marker,
/// or ordering artifact that would reveal how many were filtered out.
pub fn list_attention<'a, I>(
    ledgers: I,
    binding: &AuthorityBinding,
    policy: &AttentionPolicy,
) -> Result<Vec<RunAttention>, ReconcileError>
where
    I: IntoIterator<Item = (&'a ReconciliationLedger, &'a RunObservation)>,
{
    let mut visible = Vec::new();
    for (ledger, observation) in ledgers {
        if ledger.authorize(binding).is_err() {
            continue;
        }
        visible.push(ledger.project_with_resolution(observation, policy)?);
    }
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(value: &str) -> OpaqueRef {
        OpaqueRef::new(value).expect("fixture ref is opaque")
    }

    fn observation() -> RunObservation {
        RunObservation {
            run_ref: opaque("run-ref-01"),
            state: DurableRunState::Running,
            revision: 7,
            observed_seq: 40,
            observed_at_ms: 1_000_000,
            last_evidence_at_ms: Some(999_000),
            deadline_at_ms: None,
            cancel_requested: false,
            lease: None,
            provider: None,
            attempt: None,
            stream: StreamObservation {
                retained_from_seq: 1,
                retained_through_seq: 40,
                operator_cursor: Some(40),
            },
        }
    }

    #[test]
    fn no_reconcile_action_can_mutate_a_provider_attempt() {
        for action in ReconcileAction::ALL {
            assert!(
                !action.mutates_provider_attempt(),
                "{} must never touch a provider attempt",
                action.as_str()
            );
        }
        // A resolving action writes an operator verdict and nothing else.
        assert_eq!(
            ReconcileAction::ResolveCompleted.resolved_state(),
            Some(DurableRunState::Completed)
        );
        assert_eq!(ReconcileAction::Acknowledge.resolved_state(), None);
    }

    #[test]
    fn every_attention_reason_maps_to_exactly_one_domain() {
        for reason in AttentionReason::ALL {
            let domain = reason.domain();
            assert!(UncertaintyDomain::ALL.contains(&domain));
            // The mapping is total and stable, so a wire label round-trips.
            let encoded = serde_json::to_value(reason).expect("reason serializes");
            assert_eq!(encoded, serde_json::Value::String(reason.as_str().into()));
        }
        assert_eq!(
            AttentionReason::UncertainOutcome.domain(),
            UncertaintyDomain::ModelOrProvider
        );
        assert_eq!(
            AttentionReason::LeaseExpired.domain(),
            UncertaintyDomain::WorkerOrLease
        );
        assert_eq!(
            AttentionReason::CancelUnconfirmed.domain(),
            UncertaintyDomain::OperatorDecision
        );
    }

    #[test]
    fn a_corroborated_running_run_needs_no_attention() {
        let attention =
            project_attention(&observation(), &AttentionPolicy::default()).expect("projects");
        assert!(!attention.needs_attention);
        assert_eq!(attention.confidence, RunConfidence::Confirmed);
        assert!(attention.reasons.is_empty());
        assert_eq!(attention.severity, None);
        assert_eq!(attention.contract, RECONCILIATION_CONTRACT_VERSION);
    }

    #[test]
    fn an_unrecorded_attempt_outcome_forces_uncertainty_even_when_terminal() {
        let mut input = observation();
        input.state = DurableRunState::Completed;
        input.attempt = Some(AttemptObservation {
            attempt_ref: opaque("attempt-ref-01"),
            outcome: AttemptOutcome::Unknown,
        });
        let attention = project_attention(&input, &AttentionPolicy::default()).expect("projects");
        assert!(attention.needs_attention);
        assert_eq!(attention.confidence, RunConfidence::Uncertain);
        assert_eq!(attention.reasons, vec![AttentionReason::UncertainOutcome]);
        assert_eq!(attention.severity, Some(AttentionSeverity::Blocking));
        assert!(attention.has_domain(UncertaintyDomain::ModelOrProvider));
    }

    #[test]
    fn reasons_are_emitted_in_severity_order_without_duplicates() {
        let mut input = observation();
        input.last_evidence_at_ms = Some(1);
        input.deadline_at_ms = Some(100);
        input.cancel_requested = true;
        input.lease = Some(LeaseObservation {
            holder_ref: opaque("worker-ref-01"),
            epoch: 3,
            expires_at_ms: 100,
            host_restarted: true,
        });
        input.provider = Some(ProviderObservation {
            provider_run_ref: opaque("provider-ref-01"),
            state: ProviderState::Unknown,
        });
        input.stream = StreamObservation {
            retained_from_seq: 30,
            retained_through_seq: 40,
            operator_cursor: Some(10),
        };
        let attention = project_attention(&input, &AttentionPolicy::default()).expect("projects");
        assert_eq!(
            attention.reasons,
            vec![
                AttentionReason::CrashRecovered,
                AttentionReason::LeaseExpired,
                AttentionReason::ProviderAmbiguity,
                AttentionReason::CancelUnconfirmed,
                AttentionReason::DeadlineExceeded,
                AttentionReason::StreamGap,
                AttentionReason::StaleObservation,
            ]
        );
        // The most severe reason wins, and all three domains are distinguished.
        assert_eq!(attention.severity, Some(AttentionSeverity::Blocking));
        assert_eq!(attention.confidence, RunConfidence::Uncertain);
        assert_eq!(attention.domains, UncertaintyDomain::ALL.to_vec());
    }

    #[test]
    fn a_fresh_reader_has_no_gap_but_a_lapsed_cursor_does() {
        let window = StreamObservation {
            retained_from_seq: 30,
            retained_through_seq: 40,
            operator_cursor: None,
        };
        assert!(!window.has_gap());
        // Cursor 29 means "seen through 29"; 30 is still retained, so no gap.
        assert!(
            !StreamObservation {
                operator_cursor: Some(29),
                ..window
            }
            .has_gap()
        );
        assert!(
            StreamObservation {
                operator_cursor: Some(28),
                ..window
            }
            .has_gap()
        );
    }

    #[test]
    fn an_unclassifiable_provider_answer_is_always_ambiguous() {
        for state in [
            DurableRunState::Queued,
            DurableRunState::Running,
            DurableRunState::Completed,
            DurableRunState::Failed,
        ] {
            assert!(provider_disagrees(ProviderState::Unknown, state));
        }
        assert!(!provider_disagrees(
            ProviderState::Completed,
            DurableRunState::Completed
        ));
        assert!(provider_disagrees(
            ProviderState::Completed,
            DurableRunState::Failed
        ));
        assert!(provider_disagrees(
            ProviderState::Active,
            DurableRunState::Completed
        ));
    }

    #[test]
    fn redaction_removes_secrets_case_insensitively_and_bounds_output() {
        let redactor = Redactor::new(["SuperSecretToken".into(), "ab".into()]);
        // Values under four bytes are dropped, or every note would be markers.
        assert_eq!(redactor.secret_count(), 1);
        let redacted = redactor.redact("saw supersecrettoken\nin the log", 512);
        assert!(!redacted.to_ascii_lowercase().contains("supersecrettoken"));
        assert!(redacted.contains(REDACTION_MARKER));
        assert!(!redacted.contains('\n'));
        assert!(!redactor.is_clean("SUPERSECRETTOKEN"));
        assert!(redactor.is_clean("nothing to see"));
    }

    #[test]
    fn truncation_lands_on_a_char_boundary() {
        let redactor = Redactor::new(Vec::new());
        let text = "é".repeat(64);
        let redacted = redactor.redact(&text, 24);
        assert!(redacted.len() <= 24);
        assert!(redacted.ends_with(TRUNCATION_MARKER));
        // Round-tripping proves no multi-byte character was split.
        assert_eq!(
            redacted,
            String::from_utf8(redacted.clone().into_bytes()).expect("utf8")
        );
    }

    #[test]
    fn an_opaque_ref_rejects_path_url_and_leaked_identities() {
        assert!(OpaqueRef::new("/Users/secret/run").is_err());
        assert!(OpaqueRef::new("https://provider/run").is_err());
        assert!(OpaqueRef::new("run ref").is_err());
        assert!(OpaqueRef::new("run..ref").is_err());
        assert!(OpaqueRef::new("").is_err());
        let scope = RunScope {
            session_id: "session-abcdef".into(),
            workspace: "approved-workspace".into(),
            run_id: "run-123456".into(),
        };
        assert!(
            opaque("run-123456-shadow")
                .validate_for_scope(&scope)
                .is_err()
        );
        assert!(opaque("op-9f2c").validate_for_scope(&scope).is_ok());
    }
}
