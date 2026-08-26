//! Durable run, scope, review, and replay contracts.

use serde::{Deserialize, Serialize};

/// Maximum model rounds accepted by the versioned public contract.
pub const MAX_ROUNDS: u16 = 24;
/// Maximum UTF-8 bytes in a public request identity.
pub const MAX_REQUEST_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes in a durable prompt preview.
pub const MAX_PROMPT_PREVIEW_BYTES: usize = 512;
/// Maximum serialized bytes in one public event update.
pub const MAX_EVENT_UPDATE_BYTES: usize = 256 * 1024;
/// Maximum UTF-8 bytes in a review diff projection.
pub const MAX_REVIEW_DIFF_BYTES: usize = 2 * 1024 * 1024;

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

impl RunScope {
    /// Validate the identity fence before sending it across a product boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.session_id, "session_id")?;
        validate_identity(&self.workspace, "workspace")?;
        validate_identity(&self.run_id, "run_id")
    }
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

impl Bounds {
    /// Validate caller bounds before transport; the authority still applies its
    /// negotiated host ceiling after this check.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_prompt_bytes.is_some_and(|value| value == 0) {
            return Err("max_prompt_bytes must be greater than zero");
        }
        if self
            .max_rounds
            .is_some_and(|value| value == 0 || value > MAX_ROUNDS)
        {
            return Err("max_rounds must be between 1 and 24");
        }
        if self.max_duration_ms.is_some_and(|value| value == 0) {
            return Err("max_duration_ms must be greater than zero");
        }
        Ok(())
    }
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

impl SubmitTaskRequest {
    /// Validate a cross-product submit request without granting authority.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.request_id, "request_id")?;
        self.session_scope().validate()?;
        if self.prompt.trim().is_empty() {
            return Err("prompt must not be empty");
        }
        if let Some(bounds) = &self.bounds {
            bounds.validate()?;
        }
        Ok(())
    }

    fn session_scope(&self) -> RunScope {
        RunScope {
            session_id: self.session_id.clone(),
            workspace: self.workspace.clone(),
            run_id: "submit".into(),
        }
    }
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

impl DurableRun {
    /// Validate the share-safe durable projection before publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        validate_identity(&self.run_id, "run_id")?;
        validate_identity(&self.session_id, "session_id")?;
        validate_identity(&self.workspace, "workspace")?;
        validate_identity(&self.request_id, "request_id")?;
        if self.prompt_preview.len() > MAX_PROMPT_PREVIEW_BYTES {
            return Err("prompt_preview exceeds its byte bound");
        }
        if self.created_at.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err("durable timestamps must not be empty");
        }
        Ok(())
    }
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

impl RunEvent {
    /// Validate the bounded serialized event projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ts.trim().is_empty() {
            return Err("event timestamp must not be empty");
        }
        let bytes =
            serde_json::to_vec(&self.update).map_err(|_| "event update is not serializable")?;
        if bytes.len() > MAX_EVENT_UPDATE_BYTES {
            return Err("event update exceeds its byte bound");
        }
        Ok(())
    }
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
    Event {
        /// Exact run identity associated with the event.
        scope: RunScope,
        /// The bounded event update.
        event: RunEvent,
    },
    /// The client must poll before reconnecting.
    Recovery {
        /// Exact run identity that needs recovery.
        scope: RunScope,
        /// Last safe sequence observed by the authority.
        #[serde(rename = "afterSeq")]
        after_seq: u64,
        /// Share-safe reason.
        reason: String,
        /// Authoritative polling operation.
        #[serde(rename = "pollTool")]
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

impl ReviewReceipt {
    /// Validate the bounded, repository-relative review projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.fingerprint.trim().is_empty() {
            return Err("fingerprint must not be empty");
        }
        if self.diff.len() > MAX_REVIEW_DIFF_BYTES {
            return Err("review diff exceeds its byte bound");
        }
        for file in &self.changed_files {
            if file.path.trim().is_empty()
                || file.path.starts_with('/')
                || file.path.contains("..")
                || file.summary.len() > 512
            {
                return Err("changed files must be bounded repository-relative summaries");
            }
        }
        Ok(())
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() {
        return Err(match field {
            "session_id" => "session_id must not be empty",
            "workspace" => "workspace must not be empty",
            "run_id" => "run_id must not be empty",
            "request_id" => "request_id must not be empty",
            _ => "identity must not be empty",
        });
    }
    if value.len() > MAX_REQUEST_ID_BYTES {
        return Err("identity exceeds its byte bound");
    }
    Ok(())
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

    #[test]
    fn public_contract_validators_reject_unbounded_values() {
        assert!(
            Bounds {
                max_rounds: Some(25),
                ..Bounds::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            RunEvent {
                seq: 1,
                ts: "2026-01-01T00:00:00Z".into(),
                update: serde_json::json!({"text": "x"}),
            }
            .validate()
            .is_ok()
        );
        assert!(
            ReviewReceipt {
                changed_files: vec![ChangedFile {
                    path: "../secret".into(),
                    summary: "x".into()
                }],
                diff: String::new(),
                diff_truncated: false,
                fingerprint: "fp".into(),
            }
            .validate()
            .is_err()
        );
    }
}
