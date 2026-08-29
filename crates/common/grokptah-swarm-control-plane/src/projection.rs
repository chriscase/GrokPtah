//! Redacted public projections.
//!
//! A projection is what a manager, MCP surface, or operator dashboard is
//! allowed to see. Two rules hold for every field here:
//!
//! * **No credential material.** [`crate::spec::WorkerSpec::credential_ref`] is
//!   a reference to a secret held in the OS keychain, and it is not carried
//!   into any projection type at all — there is no field to leak it through.
//! * **Everything free-form is scrubbed.** Worker-authored text reaches these
//!   structs only through the repository's shared secret sanitizer and a byte
//!   bound, so a leaked token in a task summary does not become a dashboard
//!   row.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use xai_grok_secrets::redact_secrets;

use crate::ids::{LeaseId, ModelId, ProviderId, SwarmId, TaskId};
use crate::spec::{IsolationRequirement, TaskKind, WorkerRole};
use crate::state::{ReviewVerdict, SwarmLifecycle, SwarmState, TaskState};

/// Byte bound on a projected objective.
pub const MAX_PROJECTED_OBJECTIVE_BYTES: usize = 2 * 1024;
/// Byte bound on a projected title, summary, or error.
pub const MAX_PROJECTED_LINE_BYTES: usize = 512;
/// Byte bound on a projected evidence detail.
pub const MAX_PROJECTED_EVIDENCE_BYTES: usize = 1024;

/// Scrub secrets, then bound the length on a character boundary.
fn project_text(value: &str, max_bytes: usize) -> String {
    let scrubbed = redact_secrets(value);
    if scrubbed.len() <= max_bytes {
        return scrubbed.into_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !scrubbed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &scrubbed[..end])
}

fn project_optional(value: Option<&String>, max_bytes: usize) -> Option<String> {
    value.map(|text| project_text(text, max_bytes))
}

/// How many tasks sit in each state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStateCounts {
    pub pending: u32,
    pub ready: u32,
    pub dispatching: u32,
    pub running: u32,
    pub cancelling: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub blocked: u32,
    pub cancelled: u32,
    pub dispatch_uncertain: u32,
}

impl TaskStateCounts {
    fn tally(&mut self, state: TaskState) {
        let slot = match state {
            TaskState::Pending => &mut self.pending,
            TaskState::Ready => &mut self.ready,
            TaskState::Dispatching => &mut self.dispatching,
            TaskState::Running => &mut self.running,
            TaskState::Cancelling => &mut self.cancelling,
            TaskState::Succeeded => &mut self.succeeded,
            TaskState::Failed => &mut self.failed,
            TaskState::Blocked => &mut self.blocked,
            TaskState::Cancelled => &mut self.cancelled,
            TaskState::DispatchUncertain => &mut self.dispatch_uncertain,
        };
        *slot = slot.saturating_add(1);
    }
}

/// One task's public row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgressRow {
    pub task_id: TaskId,
    pub kind: TaskKind,
    pub title: String,
    pub state: TaskState,
    pub attempts: u32,
    pub provider: ProviderId,
    pub model: ModelId,
    pub role: WorkerRole,
    pub isolation: IsolationRequirement,
    pub requires_computer_use: bool,
    /// Identity only of the lease that authorized the live dispatch. A lease
    /// identifier is not a secret and carries no authority on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_use_lease: Option<LeaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// The whole campaign's public progress view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmProgressProjection {
    pub swarm_id: SwarmId,
    pub revision: u64,
    pub lifecycle: SwarmLifecycle,
    pub objective: String,
    pub counts: TaskStateCounts,
    pub total_dispatches: u32,
    pub max_total_dispatches: u32,
    pub in_flight: u32,
    pub max_in_flight: u32,
    /// True when at least one dispatch is uncertain. The swarm cannot reach a
    /// terminal state, and no affected task can be retried, until an operator
    /// supplies evidence.
    pub needs_operator_attention: bool,
    pub tasks: Vec<TaskProgressRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// One redacted piece of worker-produced evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceRow {
    pub task_id: TaskId,
    pub label: String,
    pub detail: String,
}

/// Every retained piece of evidence, redacted and bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceProjection {
    pub swarm_id: SwarmId,
    pub revision: u64,
    pub entries: Vec<EvidenceRow>,
}

/// Build the public progress view for a swarm.
pub fn project_progress(state: &SwarmState) -> SwarmProgressProjection {
    let mut counts = TaskStateCounts::default();
    let mut rows = Vec::with_capacity(state.spec.tasks.len());

    for task in &state.spec.tasks {
        let Some(record) = state.task(&task.task_id) else {
            continue;
        };
        counts.tally(record.state);
        let Some(worker) = state.spec.worker(&task.worker_id) else {
            continue;
        };
        let lease = record
            .current_dispatch
            .as_ref()
            .and_then(|id| state.dispatch(id))
            .and_then(|dispatch| dispatch.lease.as_ref())
            .map(|lease| lease.lease_id.clone());

        rows.push(TaskProgressRow {
            task_id: task.task_id.clone(),
            kind: task.kind,
            title: project_text(&task.title, MAX_PROJECTED_LINE_BYTES),
            state: record.state,
            attempts: record.attempts,
            provider: worker.provider.clone(),
            model: worker.model.clone(),
            role: worker.role,
            isolation: worker.isolation,
            requires_computer_use: task.requires_computer_use,
            computer_use_lease: lease,
            verdict: record.verdict,
            summary: project_optional(record.summary.as_ref(), MAX_PROJECTED_LINE_BYTES),
            last_error: project_optional(record.last_error.as_ref(), MAX_PROJECTED_LINE_BYTES),
        });
    }

    let in_flight = state
        .tasks
        .iter()
        .filter(|task| task.state.occupies_slot())
        .count();

    SwarmProgressProjection {
        swarm_id: state.spec.swarm_id.clone(),
        revision: state.revision,
        lifecycle: state.lifecycle,
        objective: project_text(&state.spec.objective, MAX_PROJECTED_OBJECTIVE_BYTES),
        counts,
        total_dispatches: state.total_dispatches,
        max_total_dispatches: state.spec.budget.max_total_dispatches,
        in_flight: u32::try_from(in_flight).unwrap_or(u32::MAX),
        max_in_flight: state.spec.admission.max_in_flight,
        needs_operator_attention: counts.dispatch_uncertain > 0,
        tasks: rows,
        stop_reason: project_optional(state.stop_reason.as_ref(), MAX_PROJECTED_LINE_BYTES),
        updated_at: state.updated_at,
    }
}

/// Build the redacted evidence view for a swarm.
pub fn project_evidence(state: &SwarmState) -> EvidenceProjection {
    let entries = state
        .spec
        .tasks
        .iter()
        .filter_map(|task| state.task(&task.task_id))
        .flat_map(|record| {
            record.evidence.iter().map(move |entry| EvidenceRow {
                task_id: record.task_id.clone(),
                label: project_text(&entry.label, MAX_PROJECTED_LINE_BYTES),
                detail: project_text(&entry.detail, MAX_PROJECTED_EVIDENCE_BYTES),
            })
        })
        .collect();

    EvidenceProjection {
        swarm_id: state.spec.swarm_id.clone(),
        revision: state.revision,
        entries,
    }
}
