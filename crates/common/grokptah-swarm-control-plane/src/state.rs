//! Durable runtime state: lifecycle, task records, and dispatch identity.
//!
//! Every field here is written before the side effect it describes. A dispatch
//! record exists before a child is spawned, so a crash between the two leaves
//! evidence that something *may* have started — which the scheduler treats as
//! uncertain rather than as free to repeat.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{SwarmError, SwarmResult};
use crate::ids::{DispatchId, ExternalRefId, ModelId, ProviderId, TaskId, WorkerId};
use crate::spec::{
    ComputerUseLeaseRef, IsolationRequirement, SwarmSpec, WorkerCapability, WorkerRole,
    validate_text,
};
use xai_tool_types::SubagentCapabilityMode;

/// Namespace for content-derived dispatch identifiers. Fixed forever: changing
/// it would make a replayed attempt mint a second identity.
const DISPATCH_NAMESPACE: Uuid = Uuid::from_u128(0x8f6a_1d3c_5b27_4e91_a0c4_7fb2_e6d5_9310);

/// Maximum evidence entries retained per task.
pub const MAX_EVIDENCE_ENTRIES: usize = 16;
/// Maximum bytes in an evidence label.
pub const MAX_EVIDENCE_LABEL_BYTES: usize = 128;
/// Maximum bytes in an evidence detail.
pub const MAX_EVIDENCE_DETAIL_BYTES: usize = 2 * 1024;
/// Maximum bytes in a task summary.
pub const MAX_SUMMARY_BYTES: usize = 4 * 1024;
/// Maximum bytes in a failure, cancellation, or uncertainty reason.
pub const MAX_REASON_BYTES: usize = 1024;

pub(crate) fn truncate_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Where the whole campaign stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmLifecycle {
    /// Dispatch is permitted subject to policy.
    Active,
    /// A whole-swarm cancel was requested; no new dispatch is permitted and
    /// live children are being wound down.
    Cancelling,
    /// Every task succeeded.
    Succeeded,
    /// The campaign stopped without every task succeeding.
    Failed,
    /// A whole-swarm cancel completed.
    Cancelled,
}

impl SwarmLifecycle {
    /// True once no further transitions are possible.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Where one graph node stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Dependencies are not yet satisfied.
    Pending,
    /// Dependencies are satisfied; eligible for admission.
    Ready,
    /// A dispatch record was written but the worker has not acknowledged it.
    Dispatching,
    /// The worker acknowledged and is running.
    Running,
    /// A cancel was requested for a live child; awaiting confirmation.
    Cancelling,
    /// Completed successfully.
    Succeeded,
    /// Completed unsuccessfully.
    Failed,
    /// An upstream failure, cancellation, or unresolved uncertainty means this
    /// task can never become ready.
    Blocked,
    /// Cancelled before completing.
    Cancelled,
    /// A dispatch was attempted and its fate is unknown. The child may be
    /// running. This task is never re-dispatched without external evidence.
    DispatchUncertain,
}

impl TaskState {
    /// True once the task has a settled outcome that will not change.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    /// True for the states the scheduler recomputes from upstream results.
    ///
    /// `Blocked` is derived, not sticky: if an upstream uncertainty is later
    /// resolved in favor of success, the blocked task becomes ready again.
    pub fn is_derived(self) -> bool {
        matches!(self, Self::Pending | Self::Ready | Self::Blocked)
    }

    /// True while the task may hold live capacity.
    ///
    /// `DispatchUncertain` counts: a child whose fate is unknown may still be
    /// running, so the scheduler refuses to reissue the capacity it holds.
    pub fn occupies_slot(self) -> bool {
        matches!(
            self,
            Self::Dispatching | Self::Running | Self::Cancelling | Self::DispatchUncertain
        )
    }
}

/// Where one dispatch attempt stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// Written before the spawn. Whether the worker started is unknown.
    Requested,
    /// One caller won the durable right to perform the external spawn.
    SpawnClaimed,
    /// The worker acknowledged the dispatch and reported a handle.
    Acknowledged,
    /// The worker reported a terminal outcome.
    Settled,
    /// The attempt's fate is unknown and it will not be retried.
    Uncertain,
}

/// A reviewer's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Reject,
}

/// How a task finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskResult {
    Succeeded,
    Failed,
    Cancelled,
}

/// One bounded piece of evidence a worker produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceEntry {
    pub label: String,
    pub detail: String,
}

impl EvidenceEntry {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }

    pub fn validate(&self) -> SwarmResult<()> {
        validate_text(&self.label, "evidence label", MAX_EVIDENCE_LABEL_BYTES)?;
        validate_text(&self.detail, "evidence detail", MAX_EVIDENCE_DETAIL_BYTES)
    }
}

/// A terminal report from a worker, tied to the dispatch that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskOutcome {
    pub result: TaskResult,
    /// Required for review tasks, rejected for every other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceEntry>,
}

impl TaskOutcome {
    pub fn succeeded() -> Self {
        Self {
            result: TaskResult::Succeeded,
            verdict: None,
            summary: None,
            evidence: Vec::new(),
        }
    }

    pub fn failed(summary: impl Into<String>) -> Self {
        Self {
            result: TaskResult::Failed,
            verdict: None,
            summary: Some(summary.into()),
            evidence: Vec::new(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            result: TaskResult::Cancelled,
            verdict: None,
            summary: None,
            evidence: Vec::new(),
        }
    }

    /// Attach a reviewer verdict.
    pub fn with_verdict(mut self, verdict: ReviewVerdict) -> Self {
        self.verdict = Some(verdict);
        self
    }

    /// Attach bounded evidence.
    pub fn with_evidence(mut self, evidence: Vec<EvidenceEntry>) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn validate(&self) -> SwarmResult<()> {
        if let Some(summary) = &self.summary {
            validate_text(summary, "outcome summary", MAX_SUMMARY_BYTES)?;
        }
        if self.evidence.len() > MAX_EVIDENCE_ENTRIES {
            return Err(SwarmError::bound(format!(
                "an outcome may carry at most {MAX_EVIDENCE_ENTRIES} evidence entries"
            )));
        }
        for entry in &self.evidence {
            entry.validate()?;
        }
        Ok(())
    }
}

/// Durable per-task state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub state: TaskState,
    /// Dispatch attempts made so far. Also the attempt counter that feeds the
    /// content-derived dispatch identity.
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_dispatch: Option<DispatchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceEntry>,
    pub updated_at: DateTime<Utc>,
}

impl TaskRecord {
    pub(crate) fn new(task_id: TaskId, now: DateTime<Utc>) -> Self {
        Self {
            task_id,
            state: TaskState::Pending,
            attempts: 0,
            current_dispatch: None,
            verdict: None,
            summary: None,
            last_error: None,
            evidence: Vec::new(),
            updated_at: now,
        }
    }
}

/// Durable per-dispatch state. This is the duplicate-suppression record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchRecord {
    pub dispatch_id: DispatchId,
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub attempt: u32,
    pub isolation: IsolationRequirement,
    /// The Computer Use lease that authorized this dispatch, when the task
    /// required one. Recorded for audit; never issued or extended here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<ComputerUseLeaseRef>,
    pub state: DispatchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<ExternalRefId>,
    pub requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain_reason: Option<String>,
}

/// A dispatch the scheduler is willing to admit right now.
///
/// This is a proposal, not a record. Nothing has been written and no child has
/// been spawned until the caller hands it back to
/// `SwarmController::record_dispatch_requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchIntent {
    pub dispatch_id: DispatchId,
    pub task_id: TaskId,
    pub worker_id: WorkerId,
    pub attempt: u32,
    pub provider: ProviderId,
    pub model: ModelId,
    pub role: WorkerRole,
    pub capability_mode: SubagentCapabilityMode,
    pub capabilities: BTreeSet<WorkerCapability>,
    pub isolation: IsolationRequirement,
    /// True when the caller must attach a usable Computer Use lease reference.
    pub requires_computer_use: bool,
}

/// What an owner learned when it probed an uncertain dispatch.
///
/// Only positive evidence resolves uncertainty. [`DispatchProbe::Unknown`]
/// leaves the dispatch uncertain rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "probe", deny_unknown_fields)]
pub enum DispatchProbe {
    /// Proven never to have started. Only this verdict makes a resend safe.
    NotStarted,
    /// Proven to be running, with the provider's handle.
    Running { external_ref: ExternalRefId },
    /// Proven to have finished, with the worker's terminal report.
    Settled { outcome: TaskOutcome },
    /// Still unknown.
    Unknown,
}

/// Derive the stable identity of one dispatch attempt.
///
/// The identity is a pure function of swarm, task, and attempt, so replaying a
/// planning pass after a restart proposes the identifier that is already on
/// disk instead of minting a second one.
pub fn derive_dispatch_id(
    swarm: &crate::ids::SwarmId,
    task: &TaskId,
    attempt: u32,
) -> SwarmResult<DispatchId> {
    let name = format!("{swarm}\u{1f}{task}\u{1f}{attempt}");
    let uuid = Uuid::new_v5(&DISPATCH_NAMESPACE, name.as_bytes());
    DispatchId::parse(uuid.to_string())
}

/// The complete durable record for one swarm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SwarmState {
    pub schema_version: u32,
    pub spec: SwarmSpec,
    /// Monotonic revision for compare-and-swap persistence by the owner.
    pub revision: u64,
    pub lifecycle: SwarmLifecycle,
    pub tasks: Vec<TaskRecord>,
    #[serde(default)]
    pub dispatches: Vec<DispatchRecord>,
    #[serde(default)]
    pub total_dispatches: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SwarmState {
    pub(crate) fn new(spec: SwarmSpec, now: DateTime<Utc>) -> Self {
        let tasks = spec
            .tasks
            .iter()
            .map(|task| TaskRecord::new(task.task_id.clone(), now))
            .collect();
        Self {
            schema_version: crate::spec::SWARM_SCHEMA_VERSION,
            spec,
            revision: 1,
            lifecycle: SwarmLifecycle::Active,
            tasks,
            dispatches: Vec::new(),
            total_dispatches: 0,
            stop_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn task(&self, task_id: &TaskId) -> Option<&TaskRecord> {
        self.tasks.iter().find(|record| &record.task_id == task_id)
    }

    pub fn dispatch(&self, dispatch_id: &DispatchId) -> Option<&DispatchRecord> {
        self.dispatches
            .iter()
            .find(|record| &record.dispatch_id == dispatch_id)
    }

    pub(crate) fn task_mut(&mut self, task_id: &TaskId) -> Option<&mut TaskRecord> {
        self.tasks
            .iter_mut()
            .find(|record| &record.task_id == task_id)
    }

    pub(crate) fn dispatch_mut(&mut self, dispatch_id: &DispatchId) -> Option<&mut DispatchRecord> {
        self.dispatches
            .iter_mut()
            .find(|record| &record.dispatch_id == dispatch_id)
    }
}
