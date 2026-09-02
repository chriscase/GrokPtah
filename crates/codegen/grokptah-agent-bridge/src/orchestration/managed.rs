//! Native persistent-Agent execution policy, intent, and input assembly.
//!
//! Managed execution is opt-in and defaults off. The runtime-home owner is the
//! only dispatcher: this module never reads focused desktop state, ambient
//! transcripts, or a prompt inbox.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::message::{MessageKind, WorkMessage};
use super::types::{
    hash_payload, AgentRecord, AgentSpec, OrchError, OrchErrorCode, RunBounds, RunExecutionMode,
    DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS,
};
use super::workload::{
    invalid, AssignmentStatus, WorkDecision, WorkDecisionAction, WorkItem, WorkState,
};
use super::workspaces_match;

pub const MANAGED_EXECUTION_SCHEMA_VERSION: u32 = 1;
pub const MAX_MANAGED_KIND_BYTES: usize = 96;
pub const MAX_MANAGED_KINDS: usize = 64;
pub const MAX_MANAGED_ROUTINE_SOURCES: usize = 64;
pub const MAX_MANAGED_CONTEXT_BYTES: usize = 8 * 1024;
pub const MAX_MANAGED_MESSAGES: usize = 16;
pub const DEFAULT_NATIVE_EXECUTOR_INTERVAL_MS: u64 = 1_000;
pub const MANAGED_TRUNCATION_MARKER: &str = "\n...[truncated]\n";
pub const MANAGED_GROK_INVOCATION_SCHEMA_VERSION: u32 = 2;
pub const MAX_MANAGED_GROK_EVIDENCE_REFS: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedExecutorKind {
    #[default]
    NativeRun,
    GrokBuildIsolatedReview,
}

impl ManagedExecutorKind {
    fn is_native(value: &Self) -> bool {
        *value == Self::NativeRun
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedExecutionBudgetProfile {
    Economy,
    Balanced,
    HighAssurance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedGrokBudgetLimits {
    pub max_prompt_bytes: usize,
    pub max_turns: u32,
    pub max_duration_ms: u64,
    pub max_output_bytes: usize,
}

impl ManagedExecutionBudgetProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }

    pub fn limits(self) -> ManagedGrokBudgetLimits {
        match self {
            Self::Economy => ManagedGrokBudgetLimits {
                max_prompt_bytes: 16 * 1024,
                max_turns: 8,
                max_duration_ms: 5 * 60 * 1_000,
                max_output_bytes: 512 * 1024,
            },
            Self::Balanced => ManagedGrokBudgetLimits {
                max_prompt_bytes: 32 * 1024,
                max_turns: 16,
                max_duration_ms: 15 * 60 * 1_000,
                max_output_bytes: 2 * 1024 * 1024,
            },
            Self::HighAssurance => ManagedGrokBudgetLimits {
                max_prompt_bytes: 64 * 1024,
                max_turns: 32,
                max_duration_ms: 30 * 60 * 1_000,
                max_output_bytes: 4 * 1024 * 1024,
            },
        }
    }
}

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
    /// When `false`, native managed execution must not automatically admit
    /// attempt 2 or later. When `true`, retries may be admitted only within
    /// the Work retry policy and `maxAttempts`.
    #[serde(default)]
    pub retry_eligible: bool,
    #[serde(default)]
    pub requires_approval_before_execution: bool,
    /// Selects an executor inside the existing durable managed-work state
    /// machine. Legacy policies omit this field and remain native-only.
    #[serde(default, skip_serializing_if = "ManagedExecutorKind::is_native")]
    pub executor: ManagedExecutorKind,
    /// Selects the existing checkout boundary used by native managed Runs.
    /// Legacy policies omit this field and retain shared-checkout behavior.
    /// Isolated execution is deliberately unavailable to the child CLI
    /// adapter, which owns a separate isolation contract.
    #[serde(default, skip_serializing_if = "is_shared_execution_mode")]
    pub native_execution_mode: RunExecutionMode,
    /// Required for Grok Build. Profiles vary resource consumption only; they
    /// never widen authority, mutation scope, retry, or evidence policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_profile: Option<ManagedExecutionBudgetProfile>,
}

fn is_shared_execution_mode(value: &RunExecutionMode) -> bool {
    *value == RunExecutionMode::Shared
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
            executor: ManagedExecutorKind::NativeRun,
            native_execution_mode: RunExecutionMode::Shared,
            budget_profile: None,
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
        if self.executor == ManagedExecutorKind::NativeRun && self.budget_profile.is_some() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managedExecution.budgetProfile requires the Grok Build executor",
            ));
        }
        if self.executor != ManagedExecutorKind::NativeRun
            && self.native_execution_mode != RunExecutionMode::Shared
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managedExecution.nativeExecutionMode is available only to the native executor",
            ));
        }
        if self.native_execution_mode == RunExecutionMode::IsolatedWorktree {
            if !self.requires_approval_before_execution {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "isolated native managed execution requires approval before execution",
                ));
            }
            if self.retry_eligible {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "isolated native managed execution forbids automatic retry",
                ));
            }
        }
        Ok(())
    }

    pub fn allows_kind(&self, kind: &str) -> bool {
        self.allowed_work_kinds.is_empty()
            || self
                .allowed_work_kinds
                .iter()
                .any(|allowed| allowed == kind)
    }

    /// Native auto-admission of attempt 2+ is allowed only when this flag is
    /// true **and** the Work retry policy still has budget for `cause`.
    pub fn allows_auto_retry(
        &self,
        work: &WorkItem,
        next_attempt_number: u32,
        cause: ManagedRetryCause,
    ) -> bool {
        if !self.retry_eligible {
            return false;
        }
        if next_attempt_number == 0 || next_attempt_number > work.policy.retry.max_attempts {
            return false;
        }
        match cause {
            ManagedRetryCause::Failed => work.policy.retry.retry_failed,
            ManagedRetryCause::Interrupted | ManagedRetryCause::Expired => {
                work.policy.retry.retry_expired
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRetryCause {
    Failed,
    Interrupted,
    Expired,
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
    /// Exact launch authority is durably recorded, but process admission has
    /// not yet been proven. Recovery never redispatches this state.
    Dispatching,
    Admitted,
    Parked,
    /// Durable commit of a permission resolution is in flight. Counts as live
    /// capacity until the host oneshot and Work/attempt writes converge.
    Resolving,
    Finalized,
    Abandoned,
}

impl ManagedIntentState {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Self::Claiming | Self::Dispatching | Self::Admitted | Self::Parked | Self::Resolving
        )
    }
}

/// Secret-free durable record for one Grok Build dispatch. The orchestration
/// owner persists this before physical spawn. Prompt text, raw output,
/// transcripts, filesystem locations, provider identity, and credentials are
/// deliberately not representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedGrokCliPermissionMode {
    /// Grok's headless write tool requires this CLI spelling. GrokPtah sets it
    /// only after revision-bound Work authorization is revalidated and still
    /// applies its own workspace/file authority before and after spawn.
    HostMappedBypassPermissions,
}

impl ManagedGrokCliPermissionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HostMappedBypassPermissions => "host_mapped_bypass_permissions",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedGrokInvocation {
    pub schema_version: u32,
    pub profile: ManagedExecutionBudgetProfile,
    pub identity: grokptah_agent_sdk::GrokBuildGitIdentity,
    pub request_id: String,
    pub dispatch_nonce: String,
    pub credential_alias_hash: String,
    pub prompt_hash: String,
    pub cli_permission_mode: ManagedGrokCliPermissionMode,
    pub host_execution_approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_state: Option<grokptah_agent_sdk::GrokBuildRunState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<grokptah_agent_sdk::GrokBuildVerdict>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_digest: Option<String>,
}

impl ManagedGrokInvocation {
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != MANAGED_GROK_INVOCATION_SCHEMA_VERSION {
            return Err(invalid("managed Grok invocation schema is invalid"));
        }
        if !self.host_execution_approved {
            return Err(invalid(
                "managed Grok invocation is missing host execution approval",
            ));
        }
        self.identity
            .validate()
            .map_err(|_| invalid("managed Grok invocation identity is invalid"))?;
        for (value, field) in [
            (self.request_id.as_str(), "request_id"),
            (self.dispatch_nonce.as_str(), "dispatch_nonce"),
            (self.credential_alias_hash.as_str(), "credential_alias_hash"),
            (self.prompt_hash.as_str(), "prompt_hash"),
        ] {
            if value.is_empty() || value.len() > 512 || value.contains('\0') {
                return Err(invalid(format!(
                    "managed Grok invocation {field} is empty or exceeds its bound"
                )));
            }
        }
        if self.evidence_refs.len() > MAX_MANAGED_GROK_EVIDENCE_REFS {
            return Err(invalid("managed Grok evidence refs exceed their bound"));
        }
        for evidence_ref in &self.evidence_refs {
            if evidence_ref.is_empty() || evidence_ref.len() > 512 || evidence_ref.contains('\0') {
                return Err(invalid("managed Grok evidence ref is invalid"));
            }
        }
        let normalized = super::workload::normalize_allowed_files(&self.changed_paths)?;
        if normalized != self.changed_paths {
            return Err(invalid("managed Grok changed paths are not normalized"));
        }
        Ok(())
    }
}

pub const MANAGED_FINALIZATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedFinalizationOutcome {
    Completed,
    AwaitingApproval,
    /// Execution ended without trustworthy completion evidence. Review is
    /// terminal for this attempt and is never eligible for automatic retry.
    Review,
    Failed,
    RetryQueued,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedFinalizationRecord {
    pub schema_version: u32,
    pub intent_id: String,
    pub work_id: String,
    pub attempt_id: Option<String>,
    pub outcome: ManagedFinalizationOutcome,
    pub attempt_state: super::workload::AttemptState,
    pub work_state: WorkState,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<super::workload::WorkResult>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedFinalizationStage {
    BeforeJournal,
    AfterJournal,
    AfterAttempt,
    AfterWork,
    Complete,
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
    /// Exact checkout boundary resolved from the captured AgentSpec revision
    /// before admission. Legacy intents default to shared.
    #[serde(default, skip_serializing_if = "is_shared_execution_mode")]
    pub execution_mode: RunExecutionMode,
    pub input_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok: Option<ManagedGrokInvocation>,
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
        if let Some(grok) = &self.grok {
            grok.validate()?;
            if self.execution_mode != RunExecutionMode::Shared {
                return Err(invalid(
                    "managed Grok execution intent cannot use the native checkout mode",
                ));
            }
        }
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
    if work.attempt_count >= 1 && !policy.retry_eligible {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "managed execution retryEligible forbids another native attempt",
        ));
    }
    if work.attempt_count >= work.policy.retry.max_attempts {
        return Err(OrchError::new(
            OrchErrorCode::Conflict,
            "work item retry budget is exhausted",
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
    if policy.requires_approval_before_execution {
        let current_authorization = work.last_decision_id.as_deref().and_then(|decision_id| {
            decisions
                .iter()
                .find(|decision| decision.decision_id == decision_id)
        });
        let authorized = current_authorization.is_some_and(|decision| {
            decision.action == WorkDecisionAction::AuthorizeExecution
                && decision.work_id == work.work_id
                && decision.assigned_agent_id.as_deref() == work.assigned_agent_id.as_deref()
                && decision.policy_revision == Some(spec.revision)
                && decision
                    .work_revision
                    .and_then(|revision| revision.checked_add(1))
                    == Some(work.revision)
        });
        if !authorized {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed execution requires the current authorization decision",
            ));
        }
    }
    if policy.executor == ManagedExecutorKind::GrokBuildIsolatedReview {
        if !policy.requires_approval_before_execution {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "Grok Build managed execution requires approval before execution",
            ));
        }
        if policy.budget_profile.is_none() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "Grok Build managed execution requires an explicit budget profile",
            ));
        }
        if policy.retry_eligible
            || work.policy.retry.max_attempts != 1
            || work.policy.retry.retry_failed
            || work.policy.retry.retry_expired
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "Grok Build managed execution forbids automatic retry",
            ));
        }
        if work.source_manager_plan_id.is_none() || work.source_manager_step_id.is_none() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "Grok Build managed execution requires a linked manager plan and step",
            ));
        }
        if !work.policy.restricts_local_mutations() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "Grok Build managed execution requires a non-empty allowedFiles scope",
            ));
        }
    }
    if policy.executor == ManagedExecutorKind::NativeRun
        && policy.native_execution_mode == RunExecutionMode::IsolatedWorktree
    {
        if !policy.requires_approval_before_execution || policy.retry_eligible {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated native managed execution requires current approval and forbids automatic retry",
            ));
        }
        if !work.policy.restricts_local_mutations() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "isolated native managed execution requires a non-empty allowedFiles scope",
            ));
        }
        if work.policy.retry.max_attempts != 1
            || work.policy.retry.retry_failed
            || work.policy.retry.retry_expired
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated native managed execution forbids automatic retry",
            ));
        }
    }
    if live_intents_for_agent >= policy.max_concurrent_runs as usize {
        return Err(OrchError::new(
            OrchErrorCode::CapacityExhausted,
            "managed execution concurrent run ceiling is exhausted",
        ));
    }
    let mut bounds = intersect_run_bounds(&[
        server_ceiling,
        &spec.default_run_bounds,
        &policy.bounds,
        &work.policy.bounds,
    ]);
    if let Some(profile) = policy.budget_profile {
        let limits = profile.limits();
        bounds.max_prompt_bytes = bounds.max_prompt_bytes.min(limits.max_prompt_bytes);
        bounds.max_rounds = bounds.max_rounds.min(limits.max_turns);
        bounds.max_duration_ms = bounds.max_duration_ms.min(limits.max_duration_ms);
    }
    bounds.validate()?;
    Ok(bounds)
}

pub fn truncate_utf8_to_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    if max_bytes == 0 {
        return String::new();
    }
    if MANAGED_TRUNCATION_MARKER.len() >= max_bytes {
        // No room for the marker plus any input. Truncate the source at a
        // char boundary so tiny limits stay valid UTF-8 without panicking.
        let mut end = max_bytes.min(input.len());
        while end > 0 && !input.is_char_boundary(end) {
            end -= 1;
        }
        return input[..end].to_string();
    }
    let budget = max_bytes - MANAGED_TRUNCATION_MARKER.len();
    let mut end = budget.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + MANAGED_TRUNCATION_MARKER.len());
    out.push_str(&input[..end]);
    out.push_str(MANAGED_TRUNCATION_MARKER);
    debug_assert!(out.len() <= max_bytes);
    debug_assert!(out.is_char_boundary(out.len()));
    out
}

/// Bind a managed Grok invocation to the exact host authority that will be
/// enforced after the child exits. The footer is host-authored, always kept in
/// full, and included in the durable prompt hash. Only the untrusted managed
/// context may be truncated to fit the selected profile.
pub fn seal_managed_grok_prompt(
    managed_context: &str,
    request_id: &str,
    identity: &grokptah_agent_sdk::GrokBuildGitIdentity,
    profile: ManagedExecutionBudgetProfile,
    allowed_files: &[String],
    max_bytes: usize,
) -> Result<(String, String), OrchError> {
    identity
        .validate()
        .map_err(|_| invalid("managed Grok prompt identity is invalid"))?;
    if request_id.is_empty() || request_id.len() > 512 || request_id.contains('\0') {
        return Err(invalid("managed Grok prompt request id is invalid"));
    }
    let allowed_files = super::workload::normalize_allowed_files(allowed_files)?;
    if allowed_files.is_empty() {
        return Err(invalid("managed Grok prompt requires an allowlist"));
    }

    let mut footer = String::from("\n--- GrokPtah sealed execution contract ---\n");
    footer.push_str(&format!("Request ID: {request_id}\n"));
    footer.push_str(&format!("Repository ID: {}\n", identity.repository_id));
    footer.push_str(&format!("Base SHA: {}\n", identity.base_sha));
    footer.push_str(&format!("Head SHA: {}\n", identity.head_sha));
    footer.push_str(&format!("Git ref: {}\n", identity.git_ref));
    footer.push_str(&format!("Budget profile: {}\n", profile.as_str()));
    footer.push_str("Exact mutable-file allowlist:\n");
    for path in &allowed_files {
        footer.push_str("- ");
        footer.push_str(path);
        footer.push('\n');
    }
    footer.push_str(
        "Authority: edit only the allowlisted files. Do not commit, push, merge, fetch, add or change a remote, use browser authentication, resume another session, or launch a second invocation. Do not claim tests or verification you did not run. Stop rather than widening scope.\nFinal response: provide a concise truthful summary, then end with exactly one of these lines:\nGROK_BUILD_VERDICT=clean\nGROK_BUILD_VERDICT=findings\nGROK_BUILD_VERDICT=not_complete\n",
    );
    if footer.len() >= max_bytes {
        return Err(invalid(
            "managed Grok sealed contract exceeds the selected prompt bound",
        ));
    }
    let context_budget = max_bytes - footer.len();
    let context = truncate_utf8_to_bytes(managed_context, context_budget);
    let mut sealed = String::with_capacity(context.len() + footer.len());
    sealed.push_str(&context);
    sealed.push_str(&footer);
    if sealed.len() > max_bytes {
        return Err(invalid("managed Grok sealed prompt exceeds its bound"));
    }
    let prompt_hash = hash_payload(&serde_json::json!({
        "managedGrokPrompt": sealed,
    }));
    Ok((sealed, prompt_hash))
}

pub fn select_relevant_managed_messages(
    messages: &[WorkMessage],
    work: &WorkItem,
    agent_id: &str,
    now: DateTime<Utc>,
    limit: usize,
) -> Vec<WorkMessage> {
    let mut relevant = messages
        .iter()
        .filter(|message| {
            if message.kind == MessageKind::Question && message.expired_at(now) {
                return false;
            }
            match message.work_id.as_deref() {
                Some(work_id) => work_id == work.work_id,
                None => {
                    message.to_agent_id.as_deref() == Some(agent_id)
                        || message.from_agent_id.as_deref() == Some(agent_id)
                }
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    relevant.sort_by_key(|message| message.seq);
    let mut kept: Vec<WorkMessage> = Vec::new();
    for message in relevant {
        if let Some(existing) = kept.iter_mut().find(|prior| {
            prior.thread_id.is_some()
                && prior.thread_id == message.thread_id
                && prior.kind == message.kind
                && prior.body == message.body
        }) {
            *existing = message;
        } else {
            kept.push(message);
        }
    }
    kept.sort_by_key(|message| message.seq);
    if limit == 0 {
        return Vec::new();
    }
    let skip = kept.len().saturating_sub(limit);
    kept.into_iter().skip(skip).collect()
}

pub fn assemble_managed_run_input(
    work: &WorkItem,
    spec: &AgentSpec,
    bounds: &RunBounds,
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
        for message in messages {
            body.push_str(&format!("- [{}] {}\n", message.kind.as_str(), message.body));
        }
    }
    let max_bytes = bounds.max_prompt_bytes.clamp(1, MAX_MANAGED_CONTEXT_BYTES);
    let body = truncate_utf8_to_bytes(&body, max_bytes);
    if body.len() > max_bytes {
        return Err(invalid("managed run input exceeded its prompt bound"));
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
        "maxPromptBytes": bounds.max_prompt_bytes,
    }));
    Ok((body, input_hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::AgentSpec;
    use crate::orchestration::workload::WorkPolicy;
    use chrono::{Duration, TimeZone, Utc};
    use uuid::Uuid;

    fn spec_with_bounds(max_prompt_bytes: usize) -> AgentSpec {
        AgentSpec::initial(
            "agent-1",
            "/tmp/ws",
            "grok",
            crate::orchestration::AgentAuthorityPolicy::default(),
            Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            "test",
        )
        .unwrap()
        .pipe(|mut spec| {
            spec.default_run_bounds.max_prompt_bytes = max_prompt_bytes;
            spec
        })
    }

    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl<T> Pipe for T {}

    #[test]
    fn truncate_utf8_respects_char_boundaries_and_small_limits() {
        assert_eq!(truncate_utf8_to_bytes("hello", 16), "hello");
        assert_eq!(truncate_utf8_to_bytes("hello", 0), "");
        let emoji = "😀😀😀";
        let inside_emoji = truncate_utf8_to_bytes(emoji, 6);
        assert!(inside_emoji.is_char_boundary(inside_emoji.len()));
        assert!(inside_emoji.len() <= 6);
        assert!(std::str::from_utf8(inside_emoji.as_bytes()).is_ok());
        let before_emoji = truncate_utf8_to_bytes("hi😀", 2);
        assert_eq!(before_emoji, "hi");
        let cut_inside_emoji = truncate_utf8_to_bytes("hi😀", 4);
        assert!(cut_inside_emoji.is_char_boundary(cut_inside_emoji.len()));
        assert!(cut_inside_emoji.len() <= 4);
        assert!(!cut_inside_emoji.contains('😀') || cut_inside_emoji == "hi");
        let combining = "e\u{0301}e\u{0301}";
        let out = truncate_utf8_to_bytes(combining, 2);
        assert!(out.is_char_boundary(out.len()));
        assert!(out.len() <= 2);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        let cjk = "漢字漢字";
        let out = truncate_utf8_to_bytes(cjk, 5);
        assert!(out.is_char_boundary(out.len()));
        assert!(out.len() <= 5);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        let marker_only = truncate_utf8_to_bytes("abcdef", MANAGED_TRUNCATION_MARKER.len());
        assert!(marker_only.len() <= MANAGED_TRUNCATION_MARKER.len());
        assert!(std::str::from_utf8(marker_only.as_bytes()).is_ok());
        let tiny = truncate_utf8_to_bytes("abcdef", 1);
        assert_eq!(tiny, "a");
        let marker_budget = MANAGED_TRUNCATION_MARKER.len() + 2;
        let marked = truncate_utf8_to_bytes("abcdefghijklmnopqrstuvwxyz", marker_budget);
        assert!(marked.ends_with(MANAGED_TRUNCATION_MARKER));
        assert!(marked.len() <= marker_budget);
    }

    #[test]
    fn assemble_honors_intersected_prompt_bound() {
        let work = WorkItem::new(
            "native",
            "objective that is definitely longer than the tiny bound",
            Uuid::from_u128(1),
            "/tmp/ws",
            "op",
            WorkPolicy::default(),
        )
        .unwrap();
        let spec = spec_with_bounds(100_000);
        let bounds = RunBounds {
            max_prompt_bytes: 64,
            max_rounds: 2,
            max_duration_ms: 1_000,
            max_total_tokens: Some(100),
        };
        let (body, _) =
            assemble_managed_run_input(&work, &spec, &bounds, 1, None, &[], None).unwrap();
        assert!(body.len() <= 64);
        assert!(body.is_char_boundary(body.len()));
        let tiny = RunBounds {
            max_prompt_bytes: 8,
            ..bounds
        };
        let (body, _) =
            assemble_managed_run_input(&work, &spec, &tiny, 1, None, &[], None).unwrap();
        assert!(body.len() <= 8);
    }

    #[test]
    fn sealed_grok_prompt_preserves_authority_footer_and_hashes_exact_bytes() {
        let identity = grokptah_agent_sdk::GrokBuildGitIdentity {
            repository_id: "repo-grokptah".into(),
            base_sha: "a".repeat(40),
            head_sha: "b".repeat(40),
            git_ref: "refs/heads/codex/self-host".into(),
        };
        let (sealed, hash) = seal_managed_grok_prompt(
            &"context ".repeat(1_000),
            "11111111-1111-4111-8111-111111111111",
            &identity,
            ManagedExecutionBudgetProfile::Economy,
            &["src/lib.rs".into(), "tests/self_host.rs".into()],
            1_024,
        )
        .unwrap();
        assert!(sealed.len() <= 1_024);
        assert!(sealed.contains(MANAGED_TRUNCATION_MARKER));
        assert!(sealed.contains("Budget profile: economy"));
        assert!(sealed.contains("- src/lib.rs"));
        assert!(sealed.contains("- tests/self_host.rs"));
        assert!(sealed.contains("Do not commit, push, merge, fetch"));
        assert!(sealed.ends_with("GROK_BUILD_VERDICT=not_complete\n"));
        assert_eq!(
            hash,
            hash_payload(&serde_json::json!({ "managedGrokPrompt": sealed }))
        );
    }

    #[test]
    fn sealed_grok_prompt_refuses_to_truncate_its_authority_contract() {
        let identity = grokptah_agent_sdk::GrokBuildGitIdentity {
            repository_id: "repo-grokptah".into(),
            base_sha: "a".repeat(40),
            head_sha: "b".repeat(40),
            git_ref: "refs/heads/codex/self-host".into(),
        };
        let error = seal_managed_grok_prompt(
            "context",
            "request",
            &identity,
            ManagedExecutionBudgetProfile::Economy,
            &["src/lib.rs".into()],
            64,
        )
        .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::InvalidRequest);
    }

    #[test]
    fn relevant_messages_exclude_unrelated_and_expired() {
        let now = Utc::now();
        let session = Uuid::from_u128(7);
        let work = WorkItem::new(
            "native",
            "obj",
            session,
            "/tmp/ws",
            "op",
            WorkPolicy::default(),
        )
        .unwrap();
        let mk = |seq: u64,
                  from: Option<&str>,
                  to: Option<&str>,
                  work_id: Option<&str>,
                  kind: MessageKind,
                  body: &str,
                  expired: bool| {
            let mut message = WorkMessage::new(
                kind,
                "actor",
                from.map(str::to_string),
                to.map(str::to_string),
                session,
                "/tmp/ws",
                work_id.map(str::to_string),
                body,
                None,
                now,
            )
            .unwrap();
            message.seq = seq;
            if expired {
                message.expires_at = Some(now - Duration::minutes(1));
            }
            message
        };
        let mut messages = Vec::new();
        for i in 1..=20 {
            messages.push(mk(
                i,
                Some("other"),
                Some("other"),
                Some("other-work"),
                MessageKind::Status,
                "noise",
                false,
            ));
        }
        messages.push(mk(
            21,
            Some("agent-1"),
            Some("agent-1"),
            Some(&work.work_id),
            MessageKind::Instruction,
            "do this",
            false,
        ));
        messages.push(mk(
            22,
            Some("agent-1"),
            Some("manager"),
            Some(&work.work_id),
            MessageKind::Question,
            "old q",
            true,
        ));
        messages.push(mk(
            23,
            Some("agent-1"),
            None,
            None,
            MessageKind::Status,
            "agent chatter",
            false,
        ));
        let selected = select_relevant_managed_messages(
            &messages,
            &work,
            "agent-1",
            now,
            MAX_MANAGED_MESSAGES,
        );
        assert!(selected.iter().all(|message| {
            message.work_id.as_deref() == Some(work.work_id.as_str())
                || message.from_agent_id.as_deref() == Some("agent-1")
        }));
        assert!(!selected.iter().any(|message| message.body == "noise"));
        assert!(!selected.iter().any(|message| message.body == "old q"));
        assert!(selected.iter().any(|message| message.body == "do this"));
    }
}
