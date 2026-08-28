//! Shared orchestration types for #196.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::authz::AuthContext;
use crate::completion::{CompletionEvidence, CompletionUsage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    LimitReached,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::LimitReached
        )
    }
}

/// Host-authored capability class for a finite Run.
///
/// This is durable because security decisions must survive process restarts
/// and must not depend on an in-memory marker installed after admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunPurpose {
    #[default]
    Execution,
    ManagerProposal,
}

/// Host-decided terminal cause. Unlike `terminal_result`/`final_response`,
/// this is never inferred from model-authored prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStopCause {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBounds {
    pub max_prompt_bytes: usize,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
    /// Optional run-wide model-token stop threshold. `None` preserves the
    /// historical unbounded-token behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<u64>,
}

impl Default for RunBounds {
    fn default() -> Self {
        Self {
            max_prompt_bytes: 100_000,
            max_rounds: 24,
            max_duration_ms: 15 * 60 * 1000,
            max_total_tokens: None,
        }
    }
}

impl RunBounds {
    /// Validate a fully-resolved bounds object (after merge). Rejects zero.
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.max_prompt_bytes == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "max_prompt_bytes must be > 0",
            ));
        }
        if self.max_rounds == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "max_rounds must be > 0",
            ));
        }
        if self.max_duration_ms == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "max_duration_ms must be > 0",
            ));
        }
        if self.max_total_tokens == Some(0) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "max_total_tokens must be > 0",
            ));
        }
        Ok(())
    }
}

/// How a Build turn is allowed to touch the user's workspace.
///
/// Shared execution preserves the historical behavior. Isolated worktrees are
/// opt-in and are the only mode that can later be promoted through the
/// explicit review flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunExecutionMode {
    #[default]
    Shared,
    IsolatedWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    #[default]
    NotApplicable,
    Preparing,
    Ready,
    Promoted,
    Conflicted,
    Discarded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunExecution {
    pub mode: RunExecutionMode,
    pub source_workspace: String,
    pub execution_workspace: String,
    pub base_revision: String,
    pub source_fingerprint: String,
    #[serde(default)]
    pub final_fingerprint: Option<String>,
    #[serde(default)]
    pub promotion_state: PromotionState,
    #[serde(default)]
    pub promoted_at: Option<DateTime<Utc>>,
}

/// Merge caller bounds under server ceilings. Caller may only narrow.
/// Missing fields use the ceiling. Zero / overflow rejected.
pub fn merge_bounds(
    ceiling: &RunBounds,
    caller: Option<&serde_json::Value>,
) -> Result<RunBounds, OrchError> {
    let Some(v) = caller else {
        ceiling.validate()?;
        return Ok(ceiling.clone());
    };
    if !v.is_object() {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "bounds must be an object",
        ));
    }
    let obj = v.as_object().unwrap();
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "maxPromptBytes"
                | "max_prompt_bytes"
                | "maxRounds"
                | "max_rounds"
                | "maxDurationMs"
                | "max_duration_ms"
                | "maxTotalTokens"
                | "max_total_tokens"
        ) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!("unknown bounds field {key}"),
            ));
        }
    }
    let max_prompt_bytes = read_bound_usize(obj, &["maxPromptBytes", "max_prompt_bytes"])?
        .unwrap_or(ceiling.max_prompt_bytes);
    let max_rounds =
        read_bound_u32(obj, &["maxRounds", "max_rounds"])?.unwrap_or(ceiling.max_rounds);
    let max_duration_ms = read_bound_u64(obj, &["maxDurationMs", "max_duration_ms"])?
        .unwrap_or(ceiling.max_duration_ms);
    let requested_max_total_tokens = read_bound_u64(obj, &["maxTotalTokens", "max_total_tokens"])?;
    let max_total_tokens = match (ceiling.max_total_tokens, requested_max_total_tokens) {
        (Some(server_ceiling), Some(requested)) if requested > server_ceiling => {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "max_total_tokens exceeds server ceiling",
            ));
        }
        (Some(server_ceiling), Some(requested)) => Some(requested.min(server_ceiling)),
        (Some(server_ceiling), None) => Some(server_ceiling),
        (None, requested) => requested,
    };

    if max_prompt_bytes > ceiling.max_prompt_bytes {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "max_prompt_bytes exceeds server ceiling",
        ));
    }
    if max_rounds > ceiling.max_rounds {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "max_rounds exceeds server ceiling",
        ));
    }
    if max_duration_ms > ceiling.max_duration_ms {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "max_duration_ms exceeds server ceiling",
        ));
    }
    let merged = RunBounds {
        max_prompt_bytes,
        max_rounds,
        max_duration_ms,
        max_total_tokens,
    };
    merged.validate()?;
    Ok(merged)
}

fn read_bound_usize(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<usize>, OrchError> {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            return Ok(Some(positive_usize(v, k)?));
        }
    }
    Ok(None)
}

fn read_bound_u32(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<u32>, OrchError> {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            return Ok(Some(positive_u32(v, k)?));
        }
    }
    Ok(None)
}

fn read_bound_u64(
    obj: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Result<Option<u64>, OrchError> {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            return Ok(Some(positive_u64(v, k)?));
        }
    }
    Ok(None)
}

fn positive_usize(v: &serde_json::Value, key: &str) -> Result<usize, OrchError> {
    let n = v.as_u64().ok_or_else(|| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be a positive integer"),
        )
    })?;
    if n == 0 || n > usize::MAX as u64 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be > 0 and within range"),
        ));
    }
    Ok(n as usize)
}

fn positive_u32(v: &serde_json::Value, key: &str) -> Result<u32, OrchError> {
    let n = v.as_u64().ok_or_else(|| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be a positive integer"),
        )
    })?;
    if n == 0 || n > u32::MAX as u64 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be > 0 and within range"),
        ));
    }
    Ok(n as u32)
}

fn positive_u64(v: &serde_json::Value, key: &str) -> Result<u64, OrchError> {
    let n = v.as_u64().ok_or_else(|| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be a positive integer"),
        )
    })?;
    if n == 0 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must be > 0"),
        ));
    }
    Ok(n)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAggregates {
    pub changes: Vec<ChangeRecord>,
    pub tests: Vec<TestObservation>,
    #[serde(default)]
    pub permissions_requested: u32,
    #[serde(default)]
    pub permissions_granted: u32,
    #[serde(default)]
    pub permissions_denied: u32,
    #[serde(default)]
    pub usage: CompletionUsage,
    /// True only when every provider request attributable to this run returned
    /// valid usage metadata. Missing on legacy records intentionally means
    /// false so old zero totals are never mistaken for measured usage.
    #[serde(default)]
    pub usage_complete: bool,
    /// Provider requests durably admitted but not yet reconciled with a
    /// response. A non-zero value after restart makes accounting incomplete.
    #[serde(default)]
    pub usage_pending_requests: u32,
    #[serde(default)]
    pub verification: Option<CompletionEvidence>,
}

impl Default for RunAggregates {
    fn default() -> Self {
        Self {
            changes: Vec::new(),
            tests: Vec::new(),
            permissions_requested: 0,
            permissions_granted: 0,
            permissions_denied: 0,
            usage: CompletionUsage::default(),
            usage_complete: true,
            usage_pending_requests: 0,
            verification: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgress {
    pub round: u32,
    pub max_rounds: u32,
    pub last_tool: Option<String>,
    pub detail: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub path: String,
    pub summary: String,
}

/// A persisted, narrowly scoped authorization to promote one reviewed run.
/// This is intentionally attached to the run so restart recovery cannot lose
/// the review boundary or silently fall back to process-local state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunApproval {
    pub approval_id: String,
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub source_fingerprint: String,
    pub final_fingerprint: String,
    pub changed_files: Vec<ChangeRecord>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestObservation {
    pub call_id: String,
    pub command: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub cancelled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub request_id: String,
    pub client_id: Option<String>,
    pub state: RunState,
    /// Immutable host-authored capability class for this Run.
    #[serde(default)]
    pub purpose: RunPurpose,
    /// Durable agent identity owning this run. Optional for legacy runs and
    /// non-agent orchestration clients.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The interrupted run this explicit replacement was created from.
    #[serde(default)]
    pub retry_of: Option<String>,
    /// The verified continuation source for this run. This is intentionally
    /// distinct from `retry_of`: retry means replacing an interrupted run,
    /// while parent_run_id records normal agent continuation lineage.
    #[serde(default)]
    pub parent_run_id: Option<String>,
    /// Agent specification revision frozen for this finite run.
    #[serde(default)]
    pub agent_spec_revision: Option<u64>,
    /// Verified checkpoint from which this finite run was explicitly resumed.
    #[serde(default)]
    pub checkpoint_id: Option<String>,
    /// Content-addressed deterministic continuation used for this run.
    #[serde(default)]
    pub continuation_context_id: Option<String>,
    #[serde(default)]
    pub continuation_context_hash: Option<String>,
    /// `complete` or `degraded`; failed assembly never creates a Run.
    #[serde(default)]
    pub continuation_fidelity: Option<String>,
    /// One-based position in the bounded host-global admission queue.
    /// Cleared when the run starts, is cancelled, or is interrupted on restart.
    #[serde(default)]
    pub queue_position: Option<usize>,
    pub bounds: RunBounds,
    pub prompt_preview: String,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal_result: Option<String>,
    pub final_response: Option<String>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub stop_cause: Option<RunStopCause>,
    /// Durable per-run aggregates for journal rollover (#196 residual).
    #[serde(default)]
    pub aggregates: RunAggregates,
    /// Latest attributable progress, independent of journal retention.
    #[serde(default)]
    pub progress: Option<RunProgress>,
    /// Optional isolated execution and promotion metadata.
    #[serde(default)]
    pub execution: Option<RunExecution>,
    /// Optional persisted approval for an exact isolated-run review.
    #[serde(default)]
    pub approval: Option<RunApproval>,
}

impl RunRecord {
    /// Product Lane identity during the session-to-Lane compatibility period.
    ///
    /// Keeping this as a derived accessor avoids changing durable Run records
    /// or existing MCP payloads while making the Agent → Lane → Run relation
    /// explicit to new callers.
    pub fn lane_id(&self) -> Uuid {
        self.session_id
    }

    /// Close a durable provider-attempt marker that can no longer be
    /// reconciled with a response. Missing usage is sticky, and a bounded run
    /// records an explicit host-decided accounting stop rather than pretending
    /// its measured total is complete.
    pub(crate) fn fail_closed_unresolved_provider_attempts(&mut self) -> bool {
        if self.aggregates.usage_pending_requests == 0 {
            return false;
        }
        self.aggregates.usage_pending_requests = 0;
        self.aggregates.usage_complete = false;
        if self.bounds.max_total_tokens.is_some() {
            let code = "max_total_tokens_usage_unavailable";
            self.terminal_result = Some(code.into());
            self.error_code = Some(code.into());
            self.stop_cause = Some(RunStopCause::TokenAccountingUnavailable);
        }
        true
    }
}

pub const MAX_AGENT_CONTEXT_BYTES: usize = 16 * 1024;
pub const MAX_AGENT_WORKSPACE_BYTES: usize = 4 * 1024;
pub const MAX_AGENT_MODEL_BYTES: usize = 256;
pub const MAX_AGENT_DISPLAY_NAME_BYTES: usize = 128;
pub const MAX_AGENT_ROLE_BYTES: usize = 512;
pub const MAX_AGENT_POLICY_ENTRY_BYTES: usize = 512;
pub const MAX_AGENT_OWNER_ID_BYTES: usize = 128;
pub const AGENT_SPEC_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS: u64 = 250_000;
pub const DEFAULT_AGENT_TOOL_IDS: &[&str] = &[
    "list_dir",
    "read_file",
    "grep",
    "write_file",
    "write_files",
    "run_terminal_cmd",
    "glob_files",
    "apply_patch",
    "spawn_general_purpose",
    "spawn_explore",
    "todo_write",
    "memory_write",
    "memory_read",
    "web_fetch",
    "kill_task",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Active,
    Waiting,
    Interrupted,
    Failed,
    Completed,
}

impl AgentState {
    pub fn can_resume(self) -> bool {
        matches!(self, Self::Waiting | Self::Interrupted | Self::Failed)
    }

    /// Whether this Agent may be named as a live worker or message party.
    /// Failed/Completed identities remain durable history, not active actors.
    pub fn is_active_identity(self) -> bool {
        !matches!(self, Self::Failed | Self::Completed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationReason {
    TurnCompleted,
    Interrupted,
    Cancelled,
    Failed,
    LimitReached,
}

/// Frozen provider/model route for a durable Agent revision. The selection
/// key is retained alongside its parsed parts so every adapter can display the
/// same route without consulting mutable desktop state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelSpec {
    pub selection_key: String,
    pub provider_id: String,
    pub model_id: String,
}

impl AgentModelSpec {
    pub fn from_selection_key(selection_key: &str) -> Result<Self, OrchError> {
        let parsed =
            crate::gateway_config::parse_model_selection(selection_key).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("agent model selection is invalid: {error}"),
                )
            })?;
        Ok(Self {
            selection_key: selection_key.to_string(),
            provider_id: parsed.provider_id,
            model_id: parsed.model_id,
        })
    }

    fn validate(&self) -> Result<(), OrchError> {
        validate_bounded_string(
            &self.selection_key,
            MAX_AGENT_MODEL_BYTES,
            "model.selection_key",
        )?;
        let parsed = Self::from_selection_key(&self.selection_key)?;
        if parsed.provider_id != self.provider_id || parsed.model_id != self.model_id {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "agent model route does not match its selection key",
            ));
        }
        Ok(())
    }
}

/// Maximum authority captured when an Agent specification revision is made.
/// Runtime settings are intersected with this ceiling; they may restrict an
/// Agent further but can never silently expand it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthorityPolicy {
    pub sandbox_profile: String,
    pub bypass_permissions: bool,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    #[serde(default)]
    pub computer_use_allowed: bool,
    #[serde(default)]
    pub auto_allowed_tools: Vec<String>,
    #[serde(default)]
    pub allow_rules: Vec<String>,
    #[serde(default)]
    pub deny_rules: Vec<String>,
}

impl Default for AgentAuthorityPolicy {
    fn default() -> Self {
        Self {
            // Historical Agent records predate policy capture. Preserve their
            // workspace capability but require explicit approval for guarded
            // tools instead of inheriting a later bypass setting.
            sandbox_profile: "workspace-write".into(),
            bypass_permissions: false,
            allowed_tools: DEFAULT_AGENT_TOOL_IDS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            // A legacy Agent had no captured MCP inventory. Preserve access
            // only through the existing ambient server enablement and normal
            // permission prompt; newly created specs capture exact IDs.
            allowed_mcp_servers: vec!["*".into()],
            computer_use_allowed: false,
            auto_allowed_tools: Vec::new(),
            allow_rules: Vec::new(),
            deny_rules: Vec::new(),
        }
    }
}

impl AgentAuthorityPolicy {
    fn validate(&self) -> Result<(), OrchError> {
        validate_bounded_string(
            &self.sandbox_profile,
            MAX_AGENT_POLICY_ENTRY_BYTES,
            "authority.sandbox_profile",
        )?;
        if !matches!(
            self.sandbox_profile.as_str(),
            "read-only" | "workspace-write" | "full"
        ) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "authority.sandbox_profile is unknown",
            ));
        }
        for (field, values) in [
            ("authority.allowed_tools", &self.allowed_tools),
            ("authority.allowed_mcp_servers", &self.allowed_mcp_servers),
            ("authority.auto_allowed_tools", &self.auto_allowed_tools),
            ("authority.allow_rules", &self.allow_rules),
            ("authority.deny_rules", &self.deny_rules),
        ] {
            if values.len() > 256 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("{field} exceeds its entry bound"),
                ));
            }
            for value in values {
                validate_bounded_string(value, MAX_AGENT_POLICY_ENTRY_BYTES, field)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMemoryPolicy {
    pub project_scope: bool,
    pub agent_private_scope: bool,
    #[serde(default)]
    pub team_ids: Vec<String>,
}

impl Default for AgentMemoryPolicy {
    fn default() -> Self {
        Self {
            project_scope: true,
            agent_private_scope: true,
            team_ids: Vec::new(),
        }
    }
}

impl AgentMemoryPolicy {
    fn validate(&self) -> Result<(), OrchError> {
        if self.team_ids.len() > 64 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "memory.team_ids exceeds its entry bound",
            ));
        }
        for team_id in &self.team_ids {
            validate_id(team_id, "memory.team_ids")?;
        }
        Ok(())
    }
}

/// Immutable, attributable Agent configuration. Mutable lifecycle and Lane
/// associations remain in `AgentRecord`, while each replacement spec is
/// persisted as a new append-only revision by `OrchStore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpec {
    pub schema_version: u32,
    pub revision: u64,
    #[serde(default)]
    pub previous_revision: Option<u64>,
    pub display_name: String,
    pub role: String,
    pub source_workspace: String,
    pub model: AgentModelSpec,
    pub default_run_bounds: RunBounds,
    pub authority: AgentAuthorityPolicy,
    pub memory: AgentMemoryPolicy,
    /// Opt-in native execution. Missing on legacy records and treated as off.
    #[serde(default)]
    pub managed_execution: crate::orchestration::managed::ManagedExecutionPolicy,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

/// Attributable relationship between a durable Agent and a frequently
/// archived Lane. Archiving the Lane does not detach or mutate this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLaneAssociation {
    pub lane_id: Uuid,
    pub source_workspace: String,
    pub attached_at: DateTime<Utc>,
    pub attached_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeState {
    pub state: AgentState,
    #[serde(default)]
    pub current_run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_lane_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_checkpoint_id: Option<String>,
    pub continuation_ordinal: u64,
    pub updated_at: DateTime<Utc>,
}

impl AgentLaneAssociation {
    pub fn is_current(&self) -> bool {
        self.detached_at.is_none()
    }

    fn validate(&self) -> Result<(), OrchError> {
        validate_workspace(&self.source_workspace)?;
        validate_bounded_string(
            &self.attached_by,
            MAX_AGENT_POLICY_ENTRY_BYTES,
            "lane_association.attached_by",
        )?;
        if self.detached_at.is_some() != self.detached_by.is_some() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "Lane detachment attribution is incomplete",
            ));
        }
        if self
            .detached_at
            .is_some_and(|detached_at| detached_at < self.attached_at)
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "Lane detachment predates attachment",
            ));
        }
        if let Some(actor) = self.detached_by.as_deref() {
            validate_bounded_string(
                actor,
                MAX_AGENT_POLICY_ENTRY_BYTES,
                "lane_association.detached_by",
            )?;
        }
        Ok(())
    }
}

impl AgentSpec {
    pub fn initial(
        agent_id: &str,
        workspace: &str,
        model: &str,
        authority: AgentAuthorityPolicy,
        created_at: DateTime<Utc>,
        created_by: impl Into<String>,
    ) -> Result<Self, OrchError> {
        let default_run_bounds = RunBounds {
            max_total_tokens: Some(DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS),
            ..RunBounds::default()
        };
        let spec = Self {
            schema_version: AGENT_SPEC_SCHEMA_VERSION,
            revision: 1,
            previous_revision: None,
            display_name: agent_id.to_string(),
            role: "Software development agent".into(),
            source_workspace: workspace.to_string(),
            model: AgentModelSpec::from_selection_key(model)?,
            default_run_bounds,
            authority,
            memory: AgentMemoryPolicy::default(),
            managed_execution: crate::orchestration::managed::ManagedExecutionPolicy::default(),
            created_at,
            created_by: created_by.into(),
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != AGENT_SPEC_SCHEMA_VERSION || self.revision == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "agent specification version or revision is invalid",
            ));
        }
        if self.revision == 1 && self.previous_revision.is_some()
            || self.revision > 1
                && !self
                    .previous_revision
                    .is_some_and(|previous| previous > 0 && previous < self.revision)
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "agent specification revision lineage is invalid",
            ));
        }
        validate_bounded_string(
            &self.display_name,
            MAX_AGENT_DISPLAY_NAME_BYTES,
            "display_name",
        )?;
        validate_bounded_string(&self.role, MAX_AGENT_ROLE_BYTES, "role")?;
        validate_workspace(&self.source_workspace)?;
        self.model.validate()?;
        self.default_run_bounds.validate()?;
        self.authority.validate()?;
        self.memory.validate()?;
        self.managed_execution.validate()?;
        validate_bounded_string(&self.created_by, MAX_AGENT_POLICY_ENTRY_BYTES, "created_by")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecord {
    pub agent_id: String,
    /// Account identity that owns this durable Agent across device/client
    /// credentials. `None` is retained only for legacy records until the
    /// first authenticated service operation claims the identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_principal_id: Option<String>,
    /// Legacy primary session retained for resume and wire compatibility.
    pub session_id: Uuid,
    /// All Lanes owned by this durable identity. Old records deserialize with
    /// an empty list and are normalized to include `session_id` on access.
    #[serde(default)]
    pub lane_ids: Vec<Uuid>,
    #[serde(default)]
    pub lane_associations: Vec<AgentLaneAssociation>,
    pub workspace: String,
    pub model: String,
    /// Current immutable specification revision. Missing only while reading a
    /// pre-specification record; the durable store migrates it to revision 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<AgentSpec>,
    pub state: AgentState,
    #[serde(default)]
    pub current_run_id: Option<String>,
    #[serde(default)]
    pub last_run_id: Option<String>,
    #[serde(default)]
    pub last_lane_id: Option<Uuid>,
    #[serde(default)]
    pub latest_checkpoint_id: Option<String>,
    #[serde(default)]
    pub continuation_ordinal: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentRecord {
    pub fn migrate_legacy_spec(&mut self) -> Result<bool, OrchError> {
        let mut changed = false;
        if self.lane_associations.is_empty() {
            self.lane_associations = self
                .known_lane_ids()
                .into_iter()
                .map(|lane_id| AgentLaneAssociation {
                    lane_id,
                    source_workspace: self.workspace.clone(),
                    attached_at: self.created_at,
                    attached_by: "legacy_migration".into(),
                    detached_at: None,
                    detached_by: None,
                })
                .collect();
            changed = true;
        }
        if self.spec.is_none() {
            self.spec = Some(AgentSpec::initial(
                &self.agent_id,
                &self.workspace,
                &self.model,
                AgentAuthorityPolicy::default(),
                self.created_at,
                "legacy_migration",
            )?);
            changed = true;
        }
        Ok(changed)
    }

    pub fn current_spec(&self) -> Result<&AgentSpec, OrchError> {
        self.spec.as_ref().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "agent specification has not been migrated",
            )
        })
    }

    pub fn runtime_state(&self) -> AgentRuntimeState {
        AgentRuntimeState {
            state: self.state,
            current_run_ids: self.current_run_id.iter().cloned().collect(),
            last_run_id: self.last_run_id.clone(),
            last_lane_id: self.last_lane_id,
            latest_checkpoint_id: self.latest_checkpoint_id.clone(),
            continuation_ordinal: self.continuation_ordinal,
            updated_at: self.updated_at,
        }
    }

    /// Return the compatibility-complete Lane set without mutating storage.
    pub fn known_lane_ids(&self) -> Vec<Uuid> {
        if !self.lane_associations.is_empty() {
            return self
                .lane_associations
                .iter()
                .filter(|association| association.is_current())
                .map(|association| association.lane_id)
                .collect();
        }
        let mut ids = Vec::new();
        for lane_id in &self.lane_ids {
            if !ids.contains(lane_id) {
                ids.push(*lane_id);
            }
        }
        if !ids.contains(&self.session_id) {
            ids.insert(0, self.session_id);
        }
        ids
    }

    /// Every attributable Lane association, including detached historical
    /// Lanes that may still be named by a verified checkpoint.
    pub fn historical_lane_ids(&self) -> Vec<Uuid> {
        if self.lane_associations.is_empty() {
            return self.known_lane_ids();
        }
        let mut ids = Vec::new();
        for association in &self.lane_associations {
            if !ids.contains(&association.lane_id) {
                ids.push(association.lane_id);
            }
        }
        ids
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.agent_id, "agent_id")?;
        if let Some(owner) = self.owner_principal_id.as_deref() {
            validate_bounded_string(owner, MAX_AGENT_OWNER_ID_BYTES, "owner_principal_id")?;
        }
        validate_workspace(&self.workspace)?;
        validate_bounded_string(&self.model, MAX_AGENT_MODEL_BYTES, "model")?;
        if let Some(spec) = self.spec.as_ref() {
            spec.validate()?;
            if spec.source_workspace != self.workspace || spec.model.selection_key != self.model {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "agent compatibility fields do not match the current specification",
                ));
            }
        }
        if self.lane_associations.len() > 256 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "Agent Lane associations exceed their bound",
            ));
        }
        let mut current_lanes = HashSet::new();
        for association in &self.lane_associations {
            association.validate()?;
            if association.source_workspace != self.workspace {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "Lane association is outside the Agent source workspace",
                ));
            }
            if association.is_current() && !current_lanes.insert(association.lane_id) {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "Agent has duplicate current Lane associations",
                ));
            }
        }
        if let Some(run_id) = self.current_run_id.as_deref() {
            validate_id(run_id, "current_run_id")?;
        }
        if let Some(checkpoint_id) = self.latest_checkpoint_id.as_deref() {
            validate_id(checkpoint_id, "latest_checkpoint_id")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationCheckpoint {
    pub checkpoint_id: String,
    pub agent_id: String,
    pub session_id: Uuid,
    pub run_id: String,
    /// Specification revision active when this checkpoint was created.
    /// Missing only on legacy checkpoints, which retain their v1 hash rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_spec_revision: Option<u64>,
    #[serde(default)]
    pub parent_checkpoint_id: Option<String>,
    pub ordinal: u64,
    pub workspace: String,
    /// Redacted, bounded context sufficient to explain the verified resume
    /// point. The full session transcript remains the source of conversation.
    pub context_summary: String,
    pub context_hash: String,
    pub event_seq: u64,
    pub reason: ContinuationReason,
    pub created_at: DateTime<Utc>,
}

impl ContinuationCheckpoint {
    pub fn context_hash_for(&self) -> String {
        match self.agent_spec_revision {
            Some(revision) => hash_payload(&serde_json::json!({
                "schemaVersion": 2,
                "agentId": self.agent_id,
                "agentSpecRevision": revision,
                "sessionId": self.session_id,
                "runId": self.run_id,
                "parentCheckpointId": self.parent_checkpoint_id,
                "ordinal": self.ordinal,
                "workspace": self.workspace,
                "contextSummary": self.context_summary,
                "eventSeq": self.event_seq,
                "reason": self.reason,
                "createdAt": self.created_at,
            })),
            None => hash_payload(&serde_json::json!({
                "agentId": self.agent_id,
                "runId": self.run_id,
                "ordinal": self.ordinal,
                "contextSummary": self.context_summary,
            })),
        }
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.checkpoint_id, "checkpoint_id")?;
        validate_id(&self.agent_id, "agent_id")?;
        validate_id(&self.run_id, "run_id")?;
        if self.agent_spec_revision == Some(0) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "checkpoint Agent specification revision is invalid",
            ));
        }
        if let Some(parent) = self.parent_checkpoint_id.as_deref() {
            validate_id(parent, "parent_checkpoint_id")?;
        }
        validate_workspace(&self.workspace)?;
        validate_bounded_string(
            &self.context_summary,
            MAX_AGENT_CONTEXT_BYTES,
            "context_summary",
        )?;
        if self.context_hash.len() != 64
            || !self.context_hash.chars().all(|c| c.is_ascii_hexdigit())
            || self.context_hash != self.context_hash_for()
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "checkpoint context hash is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResumePlan {
    pub agent: AgentRecord,
    pub checkpoint: ContinuationCheckpoint,
    pub parent_run_id: String,
}

impl AgentResumePlan {
    pub fn validate_for(&self, session_id: Uuid, workspace: &str) -> Result<(), OrchError> {
        self.agent.validate()?;
        self.checkpoint.validate()?;
        if !self.agent.state.can_resume() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "agent is active and cannot be resumed",
            ));
        }
        if !self.agent.known_lane_ids().contains(&session_id)
            || !self
                .agent
                .historical_lane_ids()
                .contains(&self.checkpoint.session_id)
            || !workspace_identity_matches(&self.agent.workspace, workspace)
            || !workspace_identity_matches(&self.checkpoint.workspace, workspace)
            || self.checkpoint.agent_id != self.agent.agent_id
            || self.agent.latest_checkpoint_id.as_deref()
                != Some(self.checkpoint.checkpoint_id.as_str())
            || self.parent_run_id != self.checkpoint.run_id
        {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "resume plan does not match the requested session workspace",
            ));
        }
        Ok(())
    }
}

fn workspace_identity_matches(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (
        dunce::canonicalize(Path::new(left)),
        dunce::canonicalize(Path::new(right)),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn validate_id(value: &str, field: &str) -> Result<(), OrchError> {
    safe_id_filename(value).map(|_| ()).map_err(|error| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is invalid: {}", error.message),
        )
    })
}

fn validate_workspace(value: &str) -> Result<(), OrchError> {
    validate_bounded_string(value, MAX_AGENT_WORKSPACE_BYTES, "workspace")?;
    if value.trim().is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "workspace must not be empty",
        ));
    }
    Ok(())
}

fn validate_bounded_string(value: &str, max_bytes: usize, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(|c| c == '\0') {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{field} is empty or exceeds its bound"),
        ));
    }
    Ok(())
}

/// The namespace an idempotency key lives in.
///
/// `request_id` is chosen by the caller, so it cannot be an identity on its
/// own: two principals may pick the same string, and before this type existed
/// they shared one global namespace. That gave the second caller three things
/// it should never have had — the first caller's stored response on a matching
/// payload hash, a `conflict` that confirmed the key was taken on a differing
/// one, and the ability to squat a key before its owner used it.
///
/// Identity is therefore `(scope, request_id)`. The wire value is untouched:
/// callers still send, and receipts still report, exactly the `request_id`
/// they chose. Only where the receipt is stored, and which receipts a lookup
/// can reach, are scoped.
///
/// The scope is derived from the **same** value the run fence compares —
/// `OrchestrationService::stamped_client_id` — so run ownership and receipt
/// ownership cannot drift apart. The tenant
/// boundary is structural: a store root belongs to one account, and every
/// `AuthContext` this service issues carries that account's `owner_id`, so
/// hashing the owner in as well would separate nothing the directory does not
/// already separate while giving the two fences different keys.
///
/// # Sealing
///
/// A scope is **not constructible from a string**. The only two ways to obtain
/// one are [`of`](Self::of), which requires the caller's own [`AuthContext`],
/// and [`host`](Self::host), the single named host identity. Nothing — inside
/// this crate or outside it — can name another principal's namespace without
/// holding that principal's authenticated context, and the type is not
/// re-exported from the crate root at all.
///
/// That is the difference between a fence and a convention. A `for_client_id`
/// taking `&str` would have let any in-process caller mint any scope, which
/// makes the storage boundary decorative however carefully the HTTP surface
/// is gated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdempotencyScope(String);

impl IdempotencyScope {
    /// Width of the on-disk prefix. Fixed, so a stored name parses by position.
    ///
    /// 32 hex characters — 128 bits of the digest. The first cut took 64, which
    /// is more than enough against accident and less than one wants for a value
    /// that separates principals.
    pub(crate) const WIDTH: usize = 32;

    /// The scope of work this host authored for itself (the native executor,
    /// managed intents, in-process resume). Named, never inferred from absence.
    pub(crate) fn host() -> Self {
        Self::derive(HOST_AUTHORED_CLIENT_ID)
    }

    /// The scope belonging to an authenticated caller.
    ///
    /// Takes the `AuthContext` rather than a client id so a scope cannot be
    /// named without the authority it stands for.
    pub(crate) fn of(auth: &AuthContext) -> Self {
        Self::derive(&stamped_client_id(auth))
    }

    /// Private on purpose: the two entry points above are the whole API.
    fn derive(client_id: &str) -> Self {
        use sha2::{Digest, Sha256};
        let digest = hex_sha256(&Sha256::digest(
            format!("{SCOPE_DERIVATION_VERSION}\u{1f}{client_id}").as_bytes(),
        ));
        Self(digest[..Self::WIDTH].to_string())
    }

    /// Re-wrap a scope value read back from a stored receipt.
    ///
    /// `pub(crate)` on purpose: nothing outside this crate should be able to
    /// name a scope it was not issued, and no caller input reaches here.
    pub(crate) fn from_stored(value: &str) -> Self {
        Self(value.to_string())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which rule produced a scope value.
///
/// Recorded in every receipt so a later change to the derivation — binding to
/// canonical #460 principal identity, an auth generation, a delegation chain —
/// is a **deliberate migration** with a readable before and after, rather than
/// a silent orphaning of every receipt written under the old rule.
///
/// `1` is the provisional rule: the stamped credential id. It is not canonical
/// authority and this crate does not claim it is.
pub(crate) const SCOPE_DERIVATION_VERSION: u32 = 1;

/// The `client_id` a credential stamps on the runs and receipts it creates.
///
/// Lives here, beside the scope that consumes it, because the run fence and
/// the receipt fence must not derive ownership from two different values. The
/// compatibility credential keeps the established wire value `mcp`; every other
/// named device credential is stamped by its stable id.
pub(crate) fn stamped_client_id(auth: &AuthContext) -> String {
    if auth.token_id == "primary" {
        "mcp".into()
    } else {
        auth.token_id.clone()
    }
}

/// The single identity this host authors work under.
///
/// Declared here because both the run fence and the idempotency scope depend
/// on it, and a second definition is how the two would drift.
pub(crate) const HOST_AUTHORED_CLIENT_ID: &str = "native-executor";

/// A durable receipt, as stored.
///
/// `pub(crate)` with the rest of the receipt surface. It carries the full
/// replayed `response` and an `error` with its message, so it is the thing the
/// projection exists to avoid handing out: [`PublicReceipt`] is what leaves
/// this host. Exporting the type while every method that produces one is
/// crate-private would advertise an affordance that does not exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdempotencyReceipt {
    pub request_id: String,
    /// The scope this receipt was claimed in.
    ///
    /// `None` marks a receipt written before scoping existed. Such a receipt
    /// cannot be attributed to any principal, so — exactly as with a run whose
    /// `client_id` is absent — it is served to none and ages out under the
    /// existing retention policy. The one visible consequence is that a retry
    /// spanning the upgrade is treated as a new request rather than replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub payload_hash: String,
    pub run_id: Option<String>,
    pub tool: String,
    pub response: serde_json::Value,
    /// Durable rejected/failed outcome. Exact retries replay this error.
    #[serde(default)]
    pub error: Option<OrchError>,
    /// When the key was **claimed**. Immutable for the receipt's whole life.
    ///
    /// Settlement used to overwrite this with `Utc::now()`, which made the
    /// page ordering mutable: a pending receipt claimed before an issued
    /// cursor would settle, jump ahead of that cursor, and be handed to the
    /// caller a second time on the next page. A key that orders a listing has
    /// to be one the listing cannot change underneath a walk.
    pub created_at: DateTime<Utc>,
    /// When the receipt reached `complete` or `failed`. `None` while pending.
    ///
    /// Separated from `created_at` rather than replacing it, so settlement can
    /// be observed without moving the receipt in the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    /// pending | complete | failed
    #[serde(default = "default_receipt_status")]
    pub status: String,
}

fn default_receipt_status() -> String {
    "complete".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub ts: DateTime<Utc>,
    pub tool: String,
    pub request_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub workspace: Option<String>,
    pub outcome: String,
    pub error_code: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchErrorCode {
    Unauthenticated,
    ForbiddenScope,
    WorkspaceMismatch,
    SessionBusy,
    CapacityExhausted,
    StaleVersion,
    CursorExpired,
    Internal,
    /// Wall-clock / transport request deadline exceeded (maps to HTTP 504).
    Timeout,
    InvalidRequest,
    Unsupported,
    Conflict,
}

impl OrchErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::ForbiddenScope => "forbidden_scope",
            Self::WorkspaceMismatch => "workspace_mismatch",
            Self::SessionBusy => "session_busy",
            Self::CapacityExhausted => "capacity_exhausted",
            Self::StaleVersion => "stale_version",
            Self::CursorExpired => "cursor_expired",
            Self::Internal => "internal",
            Self::Timeout => "timeout",
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchError {
    pub code: OrchErrorCode,
    pub message: String,
    /// Extra JSON-RPC `error.data` fields (merged with `code`). Used so a
    /// 410 `cursor_expired` can carry `eventRange` without a second read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl OrchError {
    pub fn new(code: OrchErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(
        code: OrchErrorCode,
        message: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

impl std::fmt::Display for OrchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for OrchError {}

/// Reject shell bang prompts and administrative slash commands at the control boundary.
pub fn reject_control_prompt(prompt: &str) -> Result<(), OrchError> {
    let t = prompt.trim_start();
    if t.is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "control prompt cannot be empty",
        ));
    }
    if t.starts_with('!') {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "shell-style ! prompts are not allowed via control plane",
        ));
    }
    if t.starts_with('/') {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "slash commands are not allowed via the orchestration control plane",
        ));
    }
    Ok(())
}

/// UTF-8 safe prompt preview (never slice mid-codepoint).
pub fn prompt_preview(prompt: &str) -> String {
    let p = prompt.trim();
    crate::textutil::truncate_at_char_boundary(p, 120).to_string()
}

pub fn hash_payload(v: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let s = serde_json::to_string(v).unwrap_or_default();
    hex_sha256(&Sha256::digest(s.as_bytes()))
}

/// Stable collision-resistant, path-safe filename for request/run ids.
pub fn safe_id_filename(id: &str) -> Result<String, OrchError> {
    if id.is_empty() || id.len() > 256 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "id length out of range",
        ));
    }
    if id.contains("..") || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "id contains path separators",
        ));
    }
    use sha2::{Digest, Sha256};
    Ok(hex_sha256(&Sha256::digest(id.as_bytes())))
}

/// Lowercase hex for arbitrary bytes.
///
/// Same alphabet as [`hex_sha256`], but that one takes a digest and this one
/// takes a payload; keeping them separate stops a digest being hexed twice.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    hex_sha256(bytes)
}

/// Decode lowercase hex. `None` for anything that is not exactly that.
///
/// Strict about case on purpose. An opaque token should have exactly one
/// representation: accepting uppercase would make two different strings decode
/// to the same cursor, and a token that is malleable in any way invites being
/// treated as one that can be edited.
pub(crate) fn hex_decode(text: &str) -> Option<Vec<u8>> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks(2) {
        out.push(nibble(pair[0])? * 16 + nibble(pair[1])?);
    }
    Some(out)
}

/// HMAC-SHA256, hex encoded.
///
/// Written out rather than pulled in: the bridge already depends on `sha2` and
/// nothing else here needs a MAC, so this avoids a dependency and a lockfile
/// change for thirty lines of a fully specified construction (RFC 2104).
pub(crate) fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;

    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block[..digest.len()].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    hex_sha256(&outer.finalize())
}

/// Compare two byte strings without leaking where they first differ.
///
/// A cursor tag is compared against one the host computed; a short-circuiting
/// `==` would let a caller recover the tag a byte at a time.
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Recognized test-runner commands (not mere substring "test").
pub fn is_recognized_test_command(command: &str) -> bool {
    let c = command.trim();
    let lower = c.to_ascii_lowercase();
    // Token-aware: cargo test, npm test, npx vitest, pytest, go test, etc.
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.is_empty() {
        return false;
    }
    // strip env assignments: FOO=bar cargo test
    let mut i = 0;
    while i < tokens.len() && tokens[i].contains('=') && !tokens[i].starts_with('-') {
        i += 1;
    }
    let rest = &tokens[i..];
    if rest.is_empty() {
        return false;
    }
    match rest[0] {
        "cargo" => rest.get(1) == Some(&"test") || rest.get(1) == Some(&"t"),
        "npm" | "pnpm" | "yarn" | "bun" => rest.get(1) == Some(&"test"),
        "npx" => rest
            .get(1)
            .map(|t| *t == "vitest" || *t == "jest" || *t == "mocha")
            .unwrap_or(false),
        "pytest" | "py.test" => true,
        "go" => rest.get(1) == Some(&"test"),
        "python" | "python3" => rest.get(1).map(|t| t.contains("pytest")).unwrap_or(false),
        "make" => rest.get(1) == Some(&"test"),
        other => other == "vitest" || other == "jest",
    }
}

/// Tools exposed by the control plane (schema snapshot source of truth).
/// What an external observer may learn about a durable mutation attempt.
///
/// The stored [`IdempotencyReceipt`] is not safe to publish: `response` is the
/// full replayed body (prompts, absolute workspace paths, queue entries),
/// `error` carries a message that formats absolute paths verbatim, and `tool`
/// is host vocabulary that changes whenever a tool is added. None of them
/// exist on this type, so no projection bug can leak one.
///
/// What remains is enough to answer the only question an observer has a right
/// to ask: *did this request happen, did it settle, and if it failed, what
/// class of failure was it?*
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicReceipt {
    pub request_id: String,
    /// Stable classification, never the raw tool name.
    pub operation: String,
    /// `pending` | `complete` | `failed`. `pending` is the uncertain-send
    /// fence in durable form and must never be read as an outcome.
    pub status: String,
    /// Typed failure code only, present on `failed`. The message is withheld.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Host-issued opaque attempt digest. Equal payloads under this home
    /// agree; the unkeyed payload hash never crosses.
    pub attempt_digest: String,
    pub run_id: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

impl PublicReceipt {
    /// Classify a host tool into the contract's fixed vocabulary.
    ///
    /// Closed on purpose: a tool this build does not know becomes `other`
    /// rather than putting an arbitrary host string in front of a consumer
    /// that believes it is reading a classification.
    pub fn classify(tool: &str) -> &'static str {
        match tool {
            "ptah_create_session" => "create_session",
            "ptah_submit_task" | "ptah_resume_persistent_agent" => "submit_task",
            "ptah_steer" => "follow_up",
            "ptah_cancel_run" => "cancel",
            "ptah_claim_work" => "acquire_lease",
            "ptah_release_work" => "release_lease",
            _ => "other",
        }
    }
}

/// The public capability contract this runtime implements.
///
/// Bump the **major** when an existing field, tool, error code, or capability
/// identifier is removed or reshaped; bump the **minor** for additive changes
/// — a new tool, a new optional field, a new word in an existing vocabulary.
/// Consumers negotiate against this and refuse a major they do not implement.
pub const PUBLIC_CONTRACT_MAJOR: u32 = 1;
pub const PUBLIC_CONTRACT_MINOR: u32 = 2;

pub const CONTROL_TOOLS: &[&str] = &[
    "ptah_list_sessions",
    "ptah_create_session",
    "ptah_list_persistent_agents",
    "ptah_get_persistent_agent",
    "ptah_get_capacity",
    "ptah_list_runs",
    "ptah_get_run",
    "ptah_get_progress",
    "ptah_get_events",
    "ptah_list_receipts",
    "ptah_get_host_info",
    "ptah_get_changes",
    "ptah_get_test_results",
    "ptah_get_handoff",
    "ptah_review_run",
    "ptah_list_computer_runs",
    "ptah_get_computer_run",
    "ptah_get_computer_run_events",
    "ptah_get_computer_capacity",
    "ptah_submit_task",
    "ptah_resume_persistent_agent",
    "ptah_create_work",
    "ptah_create_manager_plan",
    "ptah_list_manager_plans",
    "ptah_get_manager_plan",
    "ptah_advance_manager_plan",
    "ptah_tick_manager_plan",
    "ptah_replan_manager_plan",
    "ptah_assign_work",
    "ptah_list_work",
    "ptah_get_work",
    "ptah_claim_work",
    "ptah_renew_work",
    "ptah_link_work_run",
    "ptah_report_work_progress",
    "ptah_release_work",
    "ptah_complete_work",
    "ptah_fail_work",
    "ptah_cancel_work",
    "ptah_retry_work",
    "ptah_approve_work",
    "ptah_create_routine",
    "ptah_list_routines",
    "ptah_get_routine",
    "ptah_pause_routine",
    "ptah_enable_routine",
    "ptah_disable_routine",
    "ptah_fire_routine",
    "ptah_list_activations",
    "ptah_list_workers",
    "ptah_get_worker",
    "ptah_heartbeat_worker",
    "ptah_offer_work",
    "ptah_accept_work",
    "ptah_decline_work",
    "ptah_reassign_work",
    "ptah_reprioritize_work",
    "ptah_block_work",
    "ptah_request_review",
    "ptah_list_work_decisions",
    "ptah_send_message",
    "ptah_ack_message",
    "ptah_list_inbox",
    "ptah_list_outbox",
    "ptah_set_managed_execution",
    "ptah_get_managed_execution",
    "ptah_authorize_work_execution",
    "ptah_resolve_work_input",
    "ptah_list_execution_intents",
    "ptah_retry_run",
    "ptah_approve_run",
    "ptah_promote_run",
    "ptah_discard_run",
    "ptah_get_queue",
    "ptah_queue_prompt",
    "ptah_edit_queue",
    "ptah_remove_queue",
    "ptah_reorder_queue",
    "ptah_clear_queue",
    "ptah_run_next",
    "ptah_steer_queued",
    "ptah_steer",
    "ptah_cancel",
];

pub const FORBIDDEN_TOOLS: &[&str] = &[
    "run_terminal_cmd",
    "shell",
    "bash",
    "ptah_shell",
    "ptah_set_config",
    "ptah_manage_plugin",
    "ptah_manage_mcp",
    "ptah_approve",
    "ptah_pause",
    "ptah_resume",
    "ptah_delete_session",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bang_and_admin_slash() {
        assert!(reject_control_prompt("!ls").is_err());
        assert!(reject_control_prompt("/mcp list").is_err());
        assert!(reject_control_prompt("/yolo").is_err());
        assert!(reject_control_prompt("/model grok").is_err());
        assert!(reject_control_prompt("/effort max").is_err());
        assert!(reject_control_prompt("   ").is_err());
        assert!(reject_control_prompt("fix the tests").is_ok());
    }

    #[test]
    fn control_tools_exclude_forbidden() {
        for f in FORBIDDEN_TOOLS {
            assert!(!CONTROL_TOOLS.contains(f), "{f} must not be in allowlist");
        }
    }

    #[test]
    fn prompt_preview_utf8_safe() {
        let s = "日".repeat(100);
        let p = prompt_preview(&s);
        assert!(!p.is_empty());
        // must be valid UTF-8 (always true for String) and not panic
        assert!(p.chars().count() <= 120);
    }

    #[test]
    fn bounds_ceiling_reject_escalation() {
        let ceil = RunBounds::default();
        let bad = serde_json::json!({"maxRounds": 100});
        assert!(merge_bounds(&ceil, Some(&bad)).is_err());
        let zero = serde_json::json!({"maxRounds": 0});
        assert!(merge_bounds(&ceil, Some(&zero)).is_err());
        let ok = serde_json::json!({"maxRounds": 2, "maxDurationMs": 1000});
        let m = merge_bounds(&ceil, Some(&ok)).unwrap();
        assert_eq!(m.max_rounds, 2);
        assert_eq!(m.max_duration_ms, 1000);
    }

    #[test]
    fn token_bound_is_optional_and_caller_may_only_narrow() {
        let unbounded = RunBounds::default();
        let omitted = merge_bounds(&unbounded, None).unwrap();
        assert_eq!(omitted.max_total_tokens, None);

        let caller_bound = serde_json::json!({"maxTotalTokens": 9_000});
        assert_eq!(
            merge_bounds(&unbounded, Some(&caller_bound))
                .unwrap()
                .max_total_tokens,
            Some(9_000)
        );

        let ceiling = RunBounds {
            max_total_tokens: Some(8_000),
            ..RunBounds::default()
        };
        assert_eq!(
            merge_bounds(&ceiling, None).unwrap().max_total_tokens,
            Some(8_000)
        );
        assert_eq!(
            merge_bounds(
                &ceiling,
                Some(&serde_json::json!({"max_total_tokens": 4_000}))
            )
            .unwrap()
            .max_total_tokens,
            Some(4_000)
        );
        assert!(merge_bounds(
            &ceiling,
            Some(&serde_json::json!({"maxTotalTokens": 8_001}))
        )
        .is_err());
        assert!(merge_bounds(&ceiling, Some(&serde_json::json!({"maxTotalTokens": 0}))).is_err());
    }

    #[test]
    fn legacy_run_bounds_deserialize_without_a_token_limit() {
        let bounds: RunBounds = serde_json::from_value(serde_json::json!({
            "maxPromptBytes": 1000,
            "maxRounds": 3,
            "maxDurationMs": 5000
        }))
        .unwrap();
        assert_eq!(bounds.max_total_tokens, None);
        assert!(serde_json::to_value(bounds)
            .unwrap()
            .get("maxTotalTokens")
            .is_none());
    }

    #[test]
    fn new_aggregates_start_complete_but_legacy_usage_is_unknown() {
        assert!(RunAggregates::default().usage_complete);
        let legacy: RunAggregates = serde_json::from_value(serde_json::json!({
            "changes": [],
            "tests": [],
            "usage": {"promptTokens": 0, "completionTokens": 0, "totalTokens": 0, "requests": 0}
        }))
        .unwrap();
        assert!(!legacy.usage_complete);
    }

    #[test]
    fn test_command_classification() {
        assert!(is_recognized_test_command("cargo test"));
        assert!(is_recognized_test_command(
            "FOO=1 cargo test -- --nocapture"
        ));
        assert!(is_recognized_test_command("npm test"));
        assert!(is_recognized_test_command("pytest tests/"));
        assert!(!is_recognized_test_command("echo test"));
        assert!(!is_recognized_test_command("cat contest.txt"));
        assert!(!is_recognized_test_command("sleep 1"));
    }

    #[test]
    fn safe_id_rejects_traversal() {
        assert!(safe_id_filename("../etc/passwd").is_err());
        assert!(safe_id_filename("a/b").is_err());
        let a = safe_id_filename("req-1").unwrap();
        let b = safe_id_filename("req_1").unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(!a.contains('/'));
        assert!(!b.contains(".."));
    }

    #[test]
    fn checkpoint_hash_is_verified_and_tamper_evident() {
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: "checkpoint-1".into(),
            agent_id: "agent-1".into(),
            session_id: Uuid::new_v4(),
            run_id: "run-1".into(),
            agent_spec_revision: None,
            parent_checkpoint_id: None,
            ordinal: 1,
            workspace: "/tmp/project".into(),
            context_summary: "last verified turn".into(),
            context_hash: String::new(),
            event_seq: 4,
            reason: ContinuationReason::TurnCompleted,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        assert!(checkpoint.validate().is_ok());
        checkpoint.context_summary.push_str(" changed");
        assert!(checkpoint.validate().is_err());
    }

    #[test]
    fn legacy_agent_records_project_the_primary_session_as_a_lane() {
        let session_id = Uuid::new_v4();
        let legacy = serde_json::json!({
            "agentId": "agent-legacy",
            "sessionId": session_id,
            "workspace": "/tmp/project",
            "model": "grok",
            "state": "waiting",
            "currentRunId": null,
            "latestCheckpointId": null,
            "continuationOrdinal": 0,
            "createdAt": Utc::now(),
            "updatedAt": Utc::now()
        });
        let agent: AgentRecord = serde_json::from_value(legacy).unwrap();
        assert!(agent.lane_ids.is_empty());
        assert_eq!(agent.known_lane_ids(), vec![session_id]);
    }

    #[test]
    fn legacy_run_records_project_the_session_as_the_lane_id() {
        let session_id = Uuid::new_v4();
        let run: RunRecord = serde_json::from_value(serde_json::json!({
            "runId": "run-legacy",
            "sessionId": session_id,
            "workspace": "/tmp/project",
            "requestId": "request-1",
            "state": "queued",
            "bounds": {
                "maxPromptBytes": 1000,
                "maxRounds": 2,
                "maxDurationMs": 1000
            },
            "promptPreview": "inspect",
            "createdAt": Utc::now(),
            "updatedAt": Utc::now()
        }))
        .unwrap();
        assert_eq!(run.lane_id(), session_id);
    }

    #[test]
    fn resume_plan_rejects_workspace_or_active_agent_mismatch() {
        let session_id = Uuid::new_v4();
        let mut agent = AgentRecord {
            agent_id: "agent-1".into(),
            owner_principal_id: None,
            session_id,
            lane_ids: vec![session_id],
            lane_associations: Vec::new(),
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            spec: None,
            state: AgentState::Waiting,
            current_run_id: None,
            last_run_id: None,
            last_lane_id: Some(session_id),
            latest_checkpoint_id: Some("checkpoint-1".into()),
            continuation_ordinal: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: "checkpoint-1".into(),
            agent_id: agent.agent_id.clone(),
            session_id,
            run_id: "run-1".into(),
            agent_spec_revision: None,
            parent_checkpoint_id: None,
            ordinal: 1,
            workspace: agent.workspace.clone(),
            context_summary: "verified".into(),
            context_hash: String::new(),
            event_seq: 2,
            reason: ContinuationReason::TurnCompleted,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        let plan = AgentResumePlan {
            agent: agent.clone(),
            checkpoint,
            parent_run_id: "run-1".into(),
        };
        assert!(plan.validate_for(session_id, "/tmp/project").is_ok());
        let secondary_lane_id = Uuid::new_v4();
        let mut secondary_plan = plan.clone();
        secondary_plan.agent.lane_ids.push(secondary_lane_id);
        assert!(secondary_plan
            .validate_for(secondary_lane_id, "/tmp/project")
            .is_ok());
        let attached_at = Utc::now();
        secondary_plan.agent.lane_associations = vec![
            AgentLaneAssociation {
                lane_id: session_id,
                source_workspace: "/tmp/project".into(),
                attached_at,
                attached_by: "test".into(),
                detached_at: None,
                detached_by: None,
            },
            AgentLaneAssociation {
                lane_id: secondary_lane_id,
                source_workspace: "/tmp/project".into(),
                attached_at,
                attached_by: "test".into(),
                detached_at: Some(attached_at),
                detached_by: Some("test".into()),
            },
        ];
        assert!(!secondary_plan
            .agent
            .known_lane_ids()
            .contains(&secondary_lane_id));
        assert!(secondary_plan
            .validate_for(secondary_lane_id, "/tmp/project")
            .is_err());
        assert!(plan.validate_for(session_id, "/tmp/other").is_err());
        agent.state = AgentState::Active;
        let active_plan = AgentResumePlan { agent, ..plan };
        assert!(active_plan
            .validate_for(session_id, "/tmp/project")
            .is_err());
    }

    #[test]
    fn resume_plan_is_transport_neutral_and_roundtrips_through_json() {
        let session_id = Uuid::new_v4();
        let agent = AgentRecord {
            agent_id: "agent-adapter".into(),
            owner_principal_id: None,
            session_id,
            lane_ids: vec![session_id],
            lane_associations: Vec::new(),
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            spec: None,
            state: AgentState::Interrupted,
            current_run_id: None,
            last_run_id: None,
            last_lane_id: Some(session_id),
            latest_checkpoint_id: Some("checkpoint-adapter".into()),
            continuation_ordinal: 2,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: "checkpoint-adapter".into(),
            agent_id: agent.agent_id.clone(),
            session_id,
            run_id: "run-adapter".into(),
            agent_spec_revision: None,
            parent_checkpoint_id: None,
            ordinal: 2,
            workspace: agent.workspace.clone(),
            context_summary: "adapter contract".into(),
            context_hash: String::new(),
            event_seq: 9,
            reason: ContinuationReason::Interrupted,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        let plan = AgentResumePlan {
            agent,
            checkpoint,
            parent_run_id: "run-adapter".into(),
        };
        let encoded = serde_json::to_vec(&plan).unwrap();
        let decoded: AgentResumePlan = serde_json::from_slice(&encoded).unwrap();
        assert!(decoded.validate_for(session_id, "/tmp/project").is_ok());
    }
}
