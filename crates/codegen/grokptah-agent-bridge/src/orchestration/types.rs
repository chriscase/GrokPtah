//! Shared orchestration types for #196.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub request_id: String,
    pub client_id: Option<String>,
    pub state: RunState,
    pub bounds: RunBounds,
    pub prompt_preview: String,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub terminal_result: Option<String>,
    pub final_response: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdempotencyReceipt {
    pub request_id: String,
    pub payload_hash: String,
    pub run_id: Option<String>,
    pub tool: String,
    pub response: serde_json::Value,
    pub created_at: DateTime<Utc>,
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
    if t.starts_with('!') {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            "shell-style ! prompts are not allowed via control plane",
        ));
    }
    let lower = t.to_ascii_lowercase();
    let admin = [
        "/mcp",
        "/plugin",
        "/settings",
        "/config",
        "/sandbox",
        "/gateway",
        "/hooks",
        "/skills",
        "/login",
        "/logout",
        "/clear",
        "/compact",
    ];
    if t.starts_with('/') {
        for a in admin {
            if lower.starts_with(a) {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    format!("administrative command {a} rejected at control boundary"),
                ));
            }
        }
    }
    Ok(())
}

pub fn prompt_preview(prompt: &str) -> String {
    let p = prompt.trim();
    if p.len() <= 120 {
        p.to_string()
    } else {
        format!("{}…", &p[..120])
    }
}

pub fn hash_payload(v: &serde_json::Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let s = serde_json::to_string(v).unwrap_or_default();
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
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
    "ptah_submit_task",
    "ptah_queue_prompt",
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
        assert!(reject_control_prompt("fix the tests").is_ok());
    }

    #[test]
    fn control_tools_exclude_forbidden() {
        for f in FORBIDDEN_TOOLS {
            assert!(!CONTROL_TOOLS.contains(f), "{f} must not be in allowlist");
        }
    }
}
