//! Public projections served by the headless port.
//!
//! These are the only shapes an embedder ever sees. They are redaction-safe
//! **by construction**: prompts, final model text, filesystem paths, tool
//! input and output, credentials, and provider payloads have no field to
//! occupy anywhere in this module, so a future adapter cannot forward them
//! without changing a published type.
//!
//! Projections take an explicit instant, so the same `(facts, now)` serialize
//! identically for every surface and every call. Clock-derived fields are
//! called out on the types that carry them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{
    PortDelivery, PortEvidenceGap, PortEvidenceSummary, PortLimits, PortPromotionState,
    PortReviewFacts, PortRunFacts, PortRunState, PortStopCause, PortVerification,
};

/// Outcome an embedder is allowed to act on.
///
/// A terminal run is presented as *verified* only when typed durable evidence
/// supports it. A completed run without that evidence is
/// [`PortRunOutcome::CompletedUnverified`] and carries the gaps that kept it
/// there — the port never upgrades a claim into a verified completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortRunOutcome {
    Queued,
    Running,
    CompletedVerified,
    CompletedUnverified,
    Failed,
    Cancelled,
    Interrupted,
    LimitReached,
}

impl PortRunOutcome {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Inclusive durable sequence range of a run's journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEventRange {
    pub start_seq: u64,
    /// Absent while the run is still producing events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seq: Option<u64>,
}

/// Classified event kind.
///
/// Every variant is a unit variant. That is the redaction guarantee for the
/// event stream: there is nowhere to put a prompt, a path, a command line, a
/// tool body, a provider response, or model prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortEventKind {
    /// A model turn began.
    TurnStarted,
    /// Agent loop advanced a round.
    Progress,
    /// Model produced assistant text (content withheld).
    ModelOutput,
    /// Model produced reasoning text (content withheld).
    ModelThinking,
    /// A plan was published (steps withheld).
    Plan,
    /// A tool call was requested.
    ToolCallStarted,
    /// A tool call finished successfully.
    ToolCallCompleted,
    /// A tool call failed.
    ToolCallFailed,
    /// A tool call was denied by permission policy.
    ToolCallDenied,
    /// A file was written or edited (path and diff withheld).
    FileEdited,
    /// A shell session started (command withheld).
    ShellStarted,
    /// A shell session produced output (bytes withheld).
    ShellOutput,
    /// A shell session ended.
    ShellEnded,
    /// A permission decision was requested.
    PermissionRequested,
    /// A subagent was spawned or updated.
    Subagent,
    /// A background task changed state.
    BackgroundTask,
    /// The provider signalled rate limiting or retry.
    RateLimited,
    /// A non-cancelling steering note reached the model context.
    SteeringInjected,
    /// The durable prompt queue changed.
    QueueChanged,
    /// Typed completion evidence was published.
    CompletionEvidence,
    /// The turn ended.
    TurnComplete,
    /// A typed error was published (message withheld).
    Error,
    /// A kind this protocol revision does not classify. Never rendered as an
    /// ordinary event: an embedder must treat it as unrecognized.
    Unclassified,
}

/// One classified event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEvent {
    pub seq: u64,
    pub kind: PortEventKind,
}

/// One bounded page of classified events.
///
/// Cursors are monotonic: `next_cursor` is always greater than the requested
/// `after_seq` and is present only while more entries remain. A cursor below
/// the retained window sets `cursor_expired` on an **empty** page — a gap is
/// reported, never presented as a complete stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEventPage {
    pub run_id: String,
    pub entries: Vec<PortEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    pub cursor_expired: bool,
    /// Page size actually applied after clamping to the negotiated limit.
    pub applied_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<PortEventRange>,
}

/// Authoritative public view of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRunProjection {
    pub run_id: String,
    pub session_id: Uuid,
    pub request_id: String,
    pub state: PortRunState,
    pub outcome: PortRunOutcome,
    /// Delivery state of the request that produced this projection. A run read
    /// directly through `events` or `review` is `delivered` by definition: the
    /// durable record exists, so its mutation took effect. Uncertainty is a
    /// property of an *unacknowledged request*, not of a run you can read.
    pub delivery: PortDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_position: Option<u32>,
    pub round: u32,
    pub max_rounds: u32,
    pub admitted_limits: PortLimits,
    pub evidence: PortEvidenceSummary,
    /// Empty when a terminal run is fully evidenced.
    pub evidence_gaps: Vec<PortEvidenceGap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_cause: Option<PortStopCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PortPromotionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<PortEventRange>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Clock-derived: milliseconds between `created_at` and the projection
    /// instant. Two calls that do not share an instant are not promised to
    /// agree on this field; every other field is durable.
    pub age_millis: i64,
}

/// Public review view. Fingerprints, counts, and promotion state — enough to
/// decide, never enough to read the work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortReviewProjection {
    pub run_id: String,
    pub outcome: PortRunOutcome,
    pub promotion: PortPromotionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_fingerprint: Option<String>,
    pub changed_file_count: u32,
    pub diff_available: bool,
    pub diff_truncated: bool,
    pub evidence: PortEvidenceSummary,
    pub evidence_gaps: Vec<PortEvidenceGap>,
}

/// Gaps that keep a terminal run from being presented as verified.
///
/// Evaluated for terminal runs only: an in-flight run has not yet had the
/// chance to produce evidence, and reporting gaps for it would be noise.
pub fn evidence_gaps(facts: &PortRunFacts) -> Vec<PortEvidenceGap> {
    let mut gaps = Vec::new();
    if !facts.state.is_terminal() {
        return gaps;
    }
    match facts.evidence.verification {
        None => gaps.push(PortEvidenceGap::MissingVerification),
        Some(PortVerification::Verified) => {}
        Some(_) => gaps.push(PortEvidenceGap::UnverifiedVerification),
    }
    if !facts.evidence.usage_complete {
        gaps.push(PortEvidenceGap::IncompleteUsage);
    }
    if facts.evidence.usage_pending_requests > 0 {
        gaps.push(PortEvidenceGap::PendingProviderAttempts);
    }
    if facts.start_seq.is_none() {
        gaps.push(PortEvidenceGap::MissingEventRange);
    }
    gaps.sort_unstable();
    gaps.dedup();
    gaps
}

/// Classify a run outcome. `Completed` becomes `CompletedVerified` only when
/// [`evidence_gaps`] is empty.
pub fn run_outcome(facts: &PortRunFacts, gaps: &[PortEvidenceGap]) -> PortRunOutcome {
    match facts.state {
        PortRunState::Queued => PortRunOutcome::Queued,
        PortRunState::Running => PortRunOutcome::Running,
        PortRunState::Failed => PortRunOutcome::Failed,
        PortRunState::Cancelled => PortRunOutcome::Cancelled,
        PortRunState::Interrupted => PortRunOutcome::Interrupted,
        PortRunState::LimitReached => PortRunOutcome::LimitReached,
        PortRunState::Completed => {
            if gaps.is_empty() {
                PortRunOutcome::CompletedVerified
            } else {
                PortRunOutcome::CompletedUnverified
            }
        }
    }
}

/// Derive the public run projection at an explicit instant.
pub fn project_run_at(
    facts: &PortRunFacts,
    delivery: PortDelivery,
    now: DateTime<Utc>,
) -> PortRunProjection {
    let gaps = evidence_gaps(facts);
    let outcome = run_outcome(facts, &gaps);
    PortRunProjection {
        run_id: facts.run_id.clone(),
        session_id: facts.session_id,
        request_id: facts.request_id.clone(),
        state: facts.state,
        outcome,
        delivery,
        queued_position: facts.queued_position,
        round: facts.round,
        max_rounds: facts.max_rounds,
        admitted_limits: facts.admitted_limits,
        evidence: facts.evidence,
        evidence_gaps: gaps,
        stop_cause: facts.stop_cause,
        promotion: facts.promotion,
        range: event_range(facts),
        created_at: facts.created_at,
        updated_at: facts.updated_at,
        age_millis: (now - facts.created_at).num_milliseconds(),
    }
}

/// Derive the public review projection from run and review facts.
pub fn project_review(facts: &PortRunFacts, review: &PortReviewFacts) -> PortReviewProjection {
    let gaps = evidence_gaps(facts);
    PortReviewProjection {
        run_id: review.run_id.clone(),
        outcome: run_outcome(facts, &gaps),
        promotion: review.promotion,
        source_fingerprint: review.source_fingerprint.clone(),
        final_fingerprint: review.final_fingerprint.clone(),
        changed_file_count: review.changed_file_count,
        diff_available: review.diff_available,
        diff_truncated: review.diff_truncated,
        evidence: facts.evidence,
        evidence_gaps: gaps,
    }
}

pub(crate) fn event_range(facts: &PortRunFacts) -> Option<PortEventRange> {
    facts.start_seq.map(|start_seq| PortEventRange {
        start_seq,
        end_seq: facts.end_seq,
    })
}

/// Classify a runtime session update without carrying any of its content.
///
/// Exhaustive over the runtime's update enum: a newly added variant fails to
/// compile here rather than silently defaulting into a leaky passthrough.
pub fn classify_update(update: &crate::events::SessionUpdate) -> PortEventKind {
    use crate::events::{SessionUpdate, ToolCallStatus};
    match update {
        SessionUpdate::TurnStarted { .. } => PortEventKind::TurnStarted,
        SessionUpdate::AgentMessageChunk { .. } => PortEventKind::ModelOutput,
        SessionUpdate::AgentThoughtChunk { .. } => PortEventKind::ModelThinking,
        SessionUpdate::Plan { .. } => PortEventKind::Plan,
        SessionUpdate::ToolCall { status, .. } => match status {
            ToolCallStatus::Denied => PortEventKind::ToolCallDenied,
            ToolCallStatus::Failed => PortEventKind::ToolCallFailed,
            ToolCallStatus::Completed => PortEventKind::ToolCallCompleted,
            ToolCallStatus::Pending | ToolCallStatus::Running => PortEventKind::ToolCallStarted,
        },
        SessionUpdate::ToolCallUpdate { status, .. } => match status {
            ToolCallStatus::Denied => PortEventKind::ToolCallDenied,
            ToolCallStatus::Failed => PortEventKind::ToolCallFailed,
            ToolCallStatus::Completed => PortEventKind::ToolCallCompleted,
            ToolCallStatus::Pending | ToolCallStatus::Running => PortEventKind::ToolCallStarted,
        },
        SessionUpdate::PermissionRequired { .. } => PortEventKind::PermissionRequested,
        SessionUpdate::FileEdit { .. } => PortEventKind::FileEdited,
        SessionUpdate::ShellSessionStarted { .. } => PortEventKind::ShellStarted,
        SessionUpdate::ShellOutput { .. } => PortEventKind::ShellOutput,
        SessionUpdate::ShellSessionEnded { .. } => PortEventKind::ShellEnded,
        SessionUpdate::AgentProgress { .. } => PortEventKind::Progress,
        SessionUpdate::SubagentSpawned { .. } | SessionUpdate::SubagentUpdate { .. } => {
            PortEventKind::Subagent
        }
        SessionUpdate::BackgroundTask { .. } => PortEventKind::BackgroundTask,
        SessionUpdate::RateLimited { .. } => PortEventKind::RateLimited,
        SessionUpdate::SteeringInjected { .. } => PortEventKind::SteeringInjected,
        SessionUpdate::PromptQueueChanged { .. } => PortEventKind::QueueChanged,
        SessionUpdate::CompletionEvidence { .. } => PortEventKind::CompletionEvidence,
        SessionUpdate::TurnComplete { .. } => PortEventKind::TurnComplete,
        SessionUpdate::Error { .. } => PortEventKind::Error,
    }
}

/// Result of a typed submit: the durable delivery receipt plus the
/// authoritative run projection when the request produced or named a run.
///
/// `run` is absent exactly when no run is attributable to the request id —
/// an `unknown` delivery that was refused before any effect, or an
/// `uncertain` delivery whose run, if one exists, is not yet observable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortSubmitView {
    pub receipt: super::types::PortSubmitReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<PortRunProjection>,
}

/// Result of a cancel: the durable delivery receipt plus the run projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortCancelView {
    pub receipt: super::types::PortCancelReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<PortRunProjection>,
}

/// Result of a bounded event read: one page plus the authoritative run
/// projection derived at the same instant, so a poller never has to reconcile
/// a page against a separately-fetched state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortEventsView {
    pub run: PortRunProjection,
    pub page: PortEventPage,
}
