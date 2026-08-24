//! Durable run, scope, review, and replay contracts.

use serde::{Deserialize, Serialize};

/// An idempotency key for one caller intent.
pub type IdempotencyKey = String;

/// The exact identity fence required for a run operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunScope {
    /// Authenticated GrokPtah session identity.
    pub session_id: String,
    /// Approved workspace alias/path selected by the authority.
    pub workspace: String,
    /// Durable run identity.
    pub run_id: String,
}

/// Prompt and execution bounds selected by the caller and policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    /// Maximum UTF-8 prompt bytes.
    pub max_prompt_bytes: Option<u32>,
    /// Maximum model rounds.
    pub max_rounds: Option<u16>,
    /// Maximum wall-clock duration.
    pub max_duration_ms: Option<u64>,
}

/// Whether a run shares the workspace or receives an isolated worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Run in the approved workspace.
    Shared,
    /// Run in a reviewable managed worktree.
    IsolatedWorktree,
}

/// Cross-product submit request. The authority adds the exact session fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitTaskRequest {
    /// Fresh idempotency key for this intent.
    pub request_id: IdempotencyKey,
    /// Exact session identity.
    pub session_id: String,
    /// Approved workspace identity.
    pub workspace: String,
    /// User/model prompt after policy validation.
    pub prompt: String,
    /// Optional bounded execution limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Shared or isolated execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<ExecutionMode>,
    /// Queue behind bounded admission instead of failing fast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_queue: Option<bool>,
}

/// Durable lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableRunState {
    /// Waiting for bounded admission.
    Queued,
    /// Model/tool execution is active.
    Running,
    /// Verified terminal success.
    Completed,
    /// Terminal failure.
    Failed,
    /// Explicitly cancelled.
    Cancelled,
    /// Interrupted and requiring explicit recovery.
    Interrupted,
    /// Reached a configured limit.
    LimitReached,
}

/// Bounded durable run projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableRun {
    /// Durable run identity.
    pub run_id: String,
    /// Owning session identity.
    pub session_id: String,
    /// Approved workspace identity.
    pub workspace: String,
    /// Caller idempotency key.
    pub request_id: IdempotencyKey,
    /// Current lifecycle state.
    pub state: DurableRunState,
    /// Redacted prompt preview.
    pub prompt_preview: String,
    /// Creation timestamp as an RFC3339 string.
    pub created_at: String,
    /// Last update timestamp as an RFC3339 string.
    pub updated_at: String,
}

/// One durable event journal entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    /// Strictly increasing journal sequence.
    pub seq: u64,
    /// RFC3339 event timestamp.
    pub ts: String,
    /// Redacted state update.
    pub update: serde_json::Value,
}

/// Cursor-paged durable events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEventPage {
    /// Retained entries in sequence order.
    pub entries: Vec<RunEvent>,
    /// Cursor for the next page, if any.
    pub next_cursor: Option<u64>,
    /// Whether the requested cursor fell outside the retained window.
    pub cursor_expired: bool,
}

/// Stream notification for event consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunNotification {
    /// A scoped event journal update.
    Event { scope: RunScope, event: RunEvent },
    /// The client must poll before reconnecting.
    Recovery {
        /// Exact run identity that needs recovery.
        scope: RunScope,
        /// Last safe sequence observed by the authority.
        after_seq: u64,
        /// Share-safe reason.
        reason: String,
        /// Authoritative polling operation.
        poll_tool: String,
    },
}

/// Exact changed-file summary used for human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Bounded human-readable summary.
    pub summary: String,
}

/// Review projection for an isolated run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewReceipt {
    /// Changed files included in the exact review.
    pub changed_files: Vec<ChangedFile>,
    /// Bounded diff, possibly truncated.
    pub diff: String,
    /// Whether the diff was truncated.
    pub diff_truncated: bool,
    /// Final workspace fingerprint.
    pub fingerprint: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_request_keeps_identity_and_bounds_explicit() {
        let request = SubmitTaskRequest {
            request_id: "req-1".into(),
            session_id: "session-1".into(),
            workspace: "/approved".into(),
            prompt: "review".into(),
            bounds: Some(Bounds {
                max_prompt_bytes: Some(4096),
                max_rounds: Some(8),
                max_duration_ms: Some(120_000),
            }),
            execution_mode: Some(ExecutionMode::IsolatedWorktree),
            allow_queue: Some(true),
        };
        let value = serde_json::to_value(request).expect("submit request serializes");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["bounds"]["maxRounds"], 8);
        assert_eq!(value["executionMode"], "isolated_worktree");
    }

    #[test]
    fn scope_and_recovery_notification_match_the_v1_json_schema() {
        let value = serde_json::to_value(RunNotification::Recovery {
            scope: RunScope {
                session_id: "session-1".into(),
                workspace: "/approved".into(),
                run_id: "run-1".into(),
            },
            after_seq: 7,
            reason: "cursor_expired".into(),
            poll_tool: "ptah_get_events".into(),
        })
        .expect("notification serializes");
        assert_eq!(value["scope"]["sessionId"], "session-1");
        assert_eq!(value["scope"]["runId"], "run-1");
        assert_eq!(value["afterSeq"], 7);
        assert_eq!(value["pollTool"], "ptah_get_events");
        assert!(value.get("after_seq").is_none());
    }
}
