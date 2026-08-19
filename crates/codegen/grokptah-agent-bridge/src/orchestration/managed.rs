//! Native persistent-Agent execution policy, intent, and input assembly.
//!
//! Managed execution is opt-in and defaults off. The runtime-home owner is the
//! only dispatcher: this module never reads focused desktop state, ambient
//! transcripts, or a prompt inbox.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::message::WorkMessage;
use super::types::{
    hash_payload, AgentRecord, AgentSpec, OrchError, OrchErrorCode, RunBounds,
    DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
};
use super::workload::{
    invalid, AssignmentStatus, WorkDecision, WorkDecisionAction, WorkItem, WorkState,
    MAX_WORK_OBJECTIVE_BYTES,
};
use super::workspaces_match;

pub const MANAGED_EXECUTION_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANAGED_KIND_BYTES: usize = 96;
pub const MAX_MANAGED_KINDS: usize = 64;
pub const MAX_MANAGED_ROUTINE_SOURCES: usize = 64;
pub const MAX_MANAGED_CONTEXT_BYTES: usize = 8 * 1024;
pub const MAX_MANAGED_MESSAGES: usize = 16;
pub const DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedExecutionPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allowed_work_kinds: Vec<String>,
    #[serde(default)]
    pub allowed_source_routine_ids: Vec<String>,
    #[serde(default = "default_managed_max_concurrent_runs")]
    pub max_concurrent_runs: u32,
    #[serde(default = "default_managed_bounds")]
    pub bounds: RunBounds,
    #[serde(default)]
    pub retry_eligible: bool,
    #[serde(default)]
    pub requires_approval_before_execution: bool,
}

fn default_managed_max_concurrent_runs() -> u32 {
    1
}

fn default_managed_bounds() -> RunBounds {
    RunBounds {
        max_prompt_bytes: 16 * 1024,
        max_rounds: 8,
        max_duration_ms: 5 * 60 * 1_000,
        max_total_tokens: Some(DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS),
    }
}

impl Default for ManagedExecutionPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_work_kinds: Vec::new(),
            allowed_source_routine_ids: Vec::new(),
            max_concurrent_runs: default_managed_max_concurrent_runs(),
            bounds: default_managed_bounds(),
            retry_eligible: false,
            requires_approval_before_execution: false,
        }
    }
}

impl ManagedExecutionPolicy {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.max_concurrent_runs == 0 || self.max_concurrent_runs > 4 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managedExecution.maxConcurrentRuns must be between 1 and 4",
            ));
        }
        if self.allowed_work_kinds.len() > MAX_MANAGED_KINDS {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managedExecution.allowedWorkKinds exceeds its bound",
            ));
        }
        for kind in &self.allowed_work_kinds {
            if kind.is_empty() || kind.len() > MAX_MANAGED_KIND_BYTES || kind.contains('\0') {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "managedExecution.allowedWorkKinds entry is invalid",
                ));
            }
        }
        if self.allowed_source_routine_ids.len() > MAX_MANAGED_ROUTINE_SOURCES {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managedExecution.allowedSourceRoutineIds exceeds its bound",
            ));
        }
        self.bounds.validate()?;
        Ok(())
    }

    pub fn allows_kind(&self, kind: &str) -> bool {
        self.allowed_work_kinds.is_empty()
            || self
                .allowed_work_kinds
                .iter()
                .any(|allowed| allowed == kind)
    }

    pub fn allows_routine_source(&self, source_routine_id: Option<&str>) -> bool {
        if self.allowed_source_routine_ids.is_empty() {
            return true;
        }
        source_routine_id.is_some_and(|routine_id| {
            self.allowed_source_routine_ids
                .iter()
                .any(|allowed| allowed == routine_id)
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedWorkMode {
    #[default]
    Inherit,
    Forbid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedIntentState {
    Claiming,
    Admitted,
    Parked,
    Finalized,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedExecutionIntent {
    pub schema_version: u32,
    pub intent_id: String,
    pub agent_id: String,
    pub agent_spec_revision: u64,
    pub work_id: String,
    pub work_revision: u64,
    pub attempt_id: Option<String>,
    pub run_id: Option<String>,
    pub session_id: Uuid,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_routine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_activation_id: Option<String>,
    pub model_selection_key: String,
    pub bounds: RunBounds,
    pub input_hash: String,
    pub state: ManagedIntentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_request_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ManagedExecutionIntent {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != MANAGED_EXECUTION_SCHEMA_VERSION {
            return Err(invalid("managed execution intent schema is invalid"));
        }
        for (value, field) in [
            (self.intent_id.as_str(), "intent_id"),
            (self.agent_id.as_str(), "agent_id"),
            (self.work_id.as_str(), "work_id"),
            (self.workspace.as_str(), "workspace"),
            (self.model_selection_key.as_str(), "model_selection_key"),
            (self.input_hash.as_str(), "input_hash"),
        ] {
            if value.is_empty() || value.len() > 512 || value.contains('\0') {
                return Err(invalid(format!("{field} is empty or exceeds its bound")));
            }
        }
        self.bounds.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeExecutorStatus {
    pub enabled: bool,
    pub interval_ms: u64,
    pub started_at: Option<DateTime<Utc>>,
    pub last_tick_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub admitted: u64,
    pub finalized: u64,
    pub skipped_manual: u64,
    pub skipped_ineligible: u64,
}

impl NativeExecutorStatus {
    pub fn disabled(interval_ms: u64) -> Self {
        Self {
            enabled: false,
            interval_ms,
            started_at: None,
            last_tick_at: None,
            last_success_at: None,
            last_error: None,
            admitted: 0,
            finalized: 0,
            skipped_manual: 0,
            skipped_ineligible: 0,
        }
    }
}

pub fn intersect_run_bounds(parts: &[&RunBounds]) -> RunBounds {
    let mut out = RunBounds {
        max_prompt_bytes: usize::MAX,
        max_rounds: u32::MAX,
        max_duration_ms: u64::MAX,
        max_total_tokens: None,
    };
    let mut saw = false;
    for bounds in parts {
        saw = true;
        out.max_prompt_bytes = out.max_prompt_bytes.min(bounds.max_prompt_bytes);
        out.max_rounds = out.max_rounds.min(bounds.max_rounds);
        out.max_duration_ms = out.max_duration_ms.min(bounds.max_duration_ms);
        out.max_total_tokens = match (out.max_total_tokens, bounds.max_total_tokens) {
            (None, value) => value,
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(left), None) => Some(left),
        };
    }
    if !saw {
        return default_managed_bounds();
    }
    if out.max_prompt_bytes == usize::MAX {
        default_managed_bounds()
    } else {
        out
    }
}

pub fn managed_execution_eligible(
    work: &WorkItem,
    agent: &AgentRecord,
    spec: &AgentSpec,
    decisions: &[WorkDecision],
    live_intents_for_agent: usize,
    server_ceiling: &RunBounds,
) -> Result<RunBounds, OrchError> {
    let policy = &spec.managed_execution;
    if !policy.enabled || work.policy.managed_execution == ManagedWorkMode::Forbid {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "managed execution is not enabled for this Agent or Work item",
        ));
    }
    if !agent.state.is_active_identity() {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "agent identity is inactive",
        ));
    }
    if !workspaces_match(&agent.workspace, &work.workspace)
        || !agent.known_lane_ids().contains(&work.session_id)
    {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "agent is outside the work session workspace",
        ));
    }
    if work.assignment_status != AssignmentStatus::Accepted
        || work.assigned_agent_id.as_deref() != Some(agent.agent_id.as_str())
    {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "work is not accepted by this Agent",
        ));
    }
    if work.state != WorkState::Queued {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "work item is not claimable",
        ));
    }
    if !policy.allows_kind(&work.kind)
        || !policy.allows_routine_source(work.source_routine_id.as_deref())
    {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "work kind or routine source is not allowed for managed execution",
        ));
    }
    if spec.authority.computer_use_allowed || spec.authority.bypass_permissions {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "managed execution cannot grant Computer Use or bypass permissions",
        ));
    }
    if policy.requires_approval_before_execution
        && !decisions
            .iter()
            .any(|decision| decision.action == WorkDecisionAction::AuthorizeExecution)
    {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "managed execution requires an explicit authorization decision",
        ));
    }
    if live_intents_for_agent >= policy.max_concurrent_runs as usize {
        return Err(OrchError::new(
            OrchErrorCode::CapacityExhausted,
            "managed execution concurrent run ceiling is exhausted",
        ));
    }
    let bounds = intersect_run_bounds(&[
        server_ceiling,
        &spec.default_run_bounds,
        &policy.bounds,
        &work.policy.bounds,
    ]);
    bounds.validate()?;
    Ok(bounds)
}

pub fn assemble_managed_run_input(
    work: &WorkItem,
    spec: &AgentSpec,
    attempt_number: u32,
    parent: Option<&WorkItem>,
    messages: &[WorkMessage],
    continuation_context: Option<&str>,
) -> Result<(String, String), OrchError> {
    let mut body = String::new();
    body.push_str("Managed Work execution. This is a new finite Run; do not resume an interrupted model invocation.\n");
    body.push_str(&format!("Work ID: {}\n", work.work_id));
    body.push_str(&format!("Kind: {}\n", work.kind));
    body.push_str(&format!("Attempt: {attempt_number}\n"));
    body.push_str(&format!("Agent spec revision: {}\n", spec.revision));
    if let Some(routine_id) = &work.source_routine_id {
        body.push_str(&format!("Source routine: {routine_id}\n"));
    }
    if let Some(activation_id) = &work.source_activation_id {
        body.push_str(&format!("Source activation: {activation_id}\n"));
    }
    if let Some(parent) = parent {
        body.push_str(&format!(
            "Parent work {}: {}\n",
            parent.work_id, parent.objective
        ));
    }
    body.push_str("Objective:\n");
    body.push_str(&work.objective);
    body.push('\n');
    if let Some(context) = continuation_context {
        body.push_str("Verified continuation context:\n");
        body.push_str(context);
        body.push('\n');
    }
    if !messages.is_empty() {
        body.push_str("Relevant messages:\n");
        for message in messages.iter().take(MAX_MANAGED_MESSAGES) {
            body.push_str(&format!("- [{}] {}\n", message.kind.as_str(), message.body));
        }
    }
    if body.len() > MAX_WORK_OBJECTIVE_BYTES.min(spec.default_run_bounds.max_prompt_bytes) {
        body.truncate(MAX_MANAGED_CONTEXT_BYTES);
        body.push_str("\n...[truncated]\n");
    }
    let input_hash = hash_payload(&serde_json::json!({
        "workId": work.work_id,
        "workRevision": work.revision,
        "kind": work.kind,
        "objective": work.objective,
        "attempt": attempt_number,
        "agentSpecRevision": spec.revision,
        "parentWorkId": work.parent_work_id,
        "sourceRoutineId": work.source_routine_id,
        "sourceActivationId": work.source_activation_id,
        "messages": messages.iter().map(|message| &message.message_id).collect::<Vec<_>>(),
        "continuation": continuation_context,
    }));
    Ok((body, input_hash))
}
