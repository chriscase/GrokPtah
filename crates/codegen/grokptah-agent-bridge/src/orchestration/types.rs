//! Shared orchestration types for #196.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBounds {
    pub max_prompt_bytes: usize,
    pub max_rounds: u32,
    pub max_duration_ms: u64,
}

impl Default for RunBounds {
    fn default() -> Self {
        Self {
            max_prompt_bytes: 100_000,
            max_rounds: 24,
            max_duration_ms: 15 * 60 * 1000,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    #[serde(default)]
    pub verification: Option<CompletionEvidence>,
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

pub const MAX_AGENT_CONTEXT_BYTES: usize = 16 * 1024;
pub const MAX_AGENT_WORKSPACE_BYTES: usize = 4 * 1024;
pub const MAX_AGENT_MODEL_BYTES: usize = 256;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecord {
    pub agent_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub model: String,
    pub state: AgentState,
    #[serde(default)]
    pub current_run_id: Option<String>,
    #[serde(default)]
    pub latest_checkpoint_id: Option<String>,
    #[serde(default)]
    pub continuation_ordinal: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AgentRecord {
    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.agent_id, "agent_id")?;
        validate_workspace(&self.workspace)?;
        validate_bounded_string(&self.model, MAX_AGENT_MODEL_BYTES, "model")?;
        if let Some(run_id) = self.current_run_id.as_deref() {
            validate_id(run_id, "current_run_id")?;
        }
        if let Some(checkpoint_id) = self.latest_checkpoint_id.as_deref() {
            validate_id(checkpoint_id, "latest_checkpoint_id")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationCheckpoint {
    pub checkpoint_id: String,
    pub agent_id: String,
    pub session_id: Uuid,
    pub run_id: String,
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
        hash_payload(&serde_json::json!({
            "agentId": self.agent_id,
            "runId": self.run_id,
            "ordinal": self.ordinal,
            "contextSummary": self.context_summary,
        }))
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        validate_id(&self.checkpoint_id, "checkpoint_id")?;
        validate_id(&self.agent_id, "agent_id")?;
        validate_id(&self.run_id, "run_id")?;
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
        if self.agent.session_id != session_id
            || self.checkpoint.session_id != session_id
            || self.agent.workspace != workspace
            || self.checkpoint.workspace != workspace
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdempotencyReceipt {
    pub request_id: String,
    pub payload_hash: String,
    pub run_id: Option<String>,
    pub tool: String,
    pub response: serde_json::Value,
    /// Durable rejected/failed outcome. Exact retries replay this error.
    #[serde(default)]
    pub error: Option<OrchError>,
    pub created_at: DateTime<Utc>,
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
}

impl OrchError {
    pub fn new(code: OrchErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
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

fn hex_sha256(bytes: &[u8]) -> String {
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
pub const CONTROL_TOOLS: &[&str] = &[
    "ptah_list_sessions",
    "ptah_get_capacity",
    "ptah_get_run",
    "ptah_get_progress",
    "ptah_get_events",
    "ptah_get_changes",
    "ptah_get_test_results",
    "ptah_get_handoff",
    "ptah_review_run",
    "ptah_submit_task",
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
    "ptah_create_session",
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
    fn resume_plan_rejects_workspace_or_active_agent_mismatch() {
        let session_id = Uuid::new_v4();
        let mut agent = AgentRecord {
            agent_id: "agent-1".into(),
            session_id,
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            state: AgentState::Waiting,
            current_run_id: None,
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
            session_id,
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            state: AgentState::Interrupted,
            current_run_id: None,
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
