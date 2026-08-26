//! Durable run, scope, review, and replay contracts.

use serde::{Deserialize, Serialize};

use crate::projection::{
    ensure_json_share_safe, ensure_no_credential_material, ensure_share_safe_metadata,
};

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
/// Maximum UTF-8 bytes in an RFC3339 timestamp projection.
pub const MAX_TIMESTAMP_BYTES: usize = 64;
/// Maximum UTF-8 bytes in a workspace fingerprint.
pub const MAX_FINGERPRINT_BYTES: usize = 256;
/// Maximum UTF-8 bytes in a repository-relative changed-file path.
pub const MAX_CHANGED_PATH_BYTES: usize = 1024;
/// Maximum UTF-8 bytes in a changed-file summary.
pub const MAX_CHANGED_SUMMARY_BYTES: usize = 512;
/// Maximum UTF-8 bytes in a share-safe recovery reason or poll operation.
pub const MAX_REASON_BYTES: usize = 128;

/// An idempotency key for one caller intent.
pub type IdempotencyKey = String;

/// The exact identity fence required for a run operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

    /// Project authority-side bounds widths into the public contract.
    ///
    /// The trusted host resolves bounds at `usize`/`u32`/`u64`; the public
    /// contract is narrower (`u32`/`u16`/`u64`). Narrowing is the direction
    /// that can silently truncate, so every narrowing step is checked and
    /// fails closed rather than wrapping. Width failures are reported before
    /// contract-ceiling failures so a caller can tell a representation problem
    /// from a policy problem.
    pub fn from_authority_widths(
        max_prompt_bytes: usize,
        max_rounds: u32,
        max_duration_ms: u64,
    ) -> Result<Self, BoundsConversionError> {
        if max_prompt_bytes == 0 || max_rounds == 0 || max_duration_ms == 0 {
            return Err(BoundsConversionError::ZeroValue);
        }
        if u64::try_from(max_prompt_bytes).unwrap_or(u64::MAX) > u64::from(u32::MAX) {
            return Err(BoundsConversionError::PromptBytesOverflow);
        }
        if max_rounds > u32::from(u16::MAX) {
            return Err(BoundsConversionError::RoundsOverflow);
        }
        if max_rounds > u32::from(MAX_ROUNDS) {
            return Err(BoundsConversionError::RoundsAboveContract);
        }
        Ok(Self {
            max_prompt_bytes: Some(max_prompt_bytes as u32),
            max_rounds: Some(max_rounds as u16),
            max_duration_ms: Some(max_duration_ms),
        })
    }

    /// Resolve public bounds back into authority-side widths under a ceiling.
    ///
    /// A caller may only narrow: an absent field inherits the ceiling and any
    /// field above the ceiling is rejected. Widening from the public contract
    /// to the authority widths is lossless by construction, so the only
    /// failures here are zero, above-contract, and above-ceiling.
    pub fn resolve_authority_widths(
        &self,
        ceiling: AuthorityBounds,
    ) -> Result<AuthorityBounds, BoundsConversionError> {
        ceiling.validate()?;
        if self.max_prompt_bytes.is_some_and(|value| value == 0)
            || self.max_rounds.is_some_and(|value| value == 0)
            || self.max_duration_ms.is_some_and(|value| value == 0)
        {
            return Err(BoundsConversionError::ZeroValue);
        }
        if self.max_rounds.is_some_and(|value| value > MAX_ROUNDS) {
            return Err(BoundsConversionError::RoundsAboveContract);
        }

        let max_prompt_bytes = match self.max_prompt_bytes {
            Some(value) => {
                usize::try_from(value).map_err(|_| BoundsConversionError::PromptBytesOverflow)?
            }
            None => ceiling.max_prompt_bytes,
        };
        let max_rounds = self.max_rounds.map_or(ceiling.max_rounds, u32::from);
        let max_duration_ms = self.max_duration_ms.unwrap_or(ceiling.max_duration_ms);

        if max_prompt_bytes > ceiling.max_prompt_bytes
            || max_rounds > ceiling.max_rounds
            || max_duration_ms > ceiling.max_duration_ms
        {
            return Err(BoundsConversionError::AboveCeiling);
        }
        Ok(AuthorityBounds {
            max_prompt_bytes,
            max_rounds,
            max_duration_ms,
        })
    }
}

/// Why a bounds conversion was refused.
///
/// Every variant is a fail-closed outcome: the seam never truncates, wraps, or
/// silently substitutes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundsConversionError {
    /// A bound was zero, which no side of the contract accepts.
    ZeroValue,
    /// The authority prompt-byte bound does not fit the public `u32`.
    PromptBytesOverflow,
    /// The authority round bound does not fit the public `u16`.
    RoundsOverflow,
    /// The round bound is above the versioned contract ceiling.
    RoundsAboveContract,
    /// The caller tried to widen past the authority ceiling.
    AboveCeiling,
}

impl BoundsConversionError {
    /// Stable, share-safe reason code for an [`crate::ErrorEnvelope`].
    pub fn reason_code(self) -> &'static str {
        match self {
            Self::ZeroValue => "bounds_zero",
            Self::PromptBytesOverflow => "bounds_prompt_bytes_overflow",
            Self::RoundsOverflow => "bounds_rounds_overflow",
            Self::RoundsAboveContract => "bounds_rounds_above_contract",
            Self::AboveCeiling => "bounds_above_ceiling",
        }
    }
}

impl std::fmt::Display for BoundsConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason_code())
    }
}

/// The authority-side integer width profile for resolved run bounds.
///
/// This is a conversion *result*, not a wire type: it is deliberately not
/// `Serialize`/`Deserialize` so it cannot become a second public DTO. It
/// mirrors the widths the trusted host resolves bounds at (`usize`, `u32`,
/// `u64`) so [`Bounds`] can be converted without a lossy cast at the call
/// site. The host's own resolved-bounds type remains the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityBounds {
    /// Maximum UTF-8 prompt bytes, at the authority's width.
    pub max_prompt_bytes: usize,
    /// Maximum model rounds, at the authority's width.
    pub max_rounds: u32,
    /// Maximum wall-clock duration in milliseconds.
    pub max_duration_ms: u64,
}

impl AuthorityBounds {
    /// Reject a ceiling that is zero in any dimension.
    pub fn validate(&self) -> Result<(), BoundsConversionError> {
        if self.max_prompt_bytes == 0 || self.max_rounds == 0 || self.max_duration_ms == 0 {
            return Err(BoundsConversionError::ZeroValue);
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
        ensure_no_credential_material(
            "prompt_preview",
            &self.prompt_preview,
            MAX_PROMPT_PREVIEW_BYTES,
        )
        .map_err(|finding| finding.kind.reason_code())?;
        ensure_share_safe_metadata("created_at", &self.created_at, MAX_TIMESTAMP_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        ensure_share_safe_metadata("updated_at", &self.updated_at, MAX_TIMESTAMP_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        Ok(())
    }
}

/// One durable event journal entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        ensure_share_safe_metadata("ts", &self.ts, MAX_TIMESTAMP_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        ensure_json_share_safe("update", &self.update, MAX_EVENT_UPDATE_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        Ok(())
    }
}

/// Cursor-paged durable events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RunNotification {
    /// A scoped event journal update.
    Event {
        /// Exact run identity the event belongs to.
        scope: RunScope,
        /// The bounded, redacted journal entry.
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

impl RunEventPage {
    /// Validate every retained entry and the page's cursor monotonicity.
    ///
    /// A page that claims an expired cursor must not also carry a next cursor:
    /// the consumer would otherwise resume from a window the authority has
    /// already dropped.
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut previous: Option<u64> = None;
        for entry in &self.entries {
            entry.validate()?;
            if previous.is_some_and(|seq| entry.seq <= seq) {
                return Err("event sequences must strictly increase");
            }
            previous = Some(entry.seq);
        }
        if self.cursor_expired && self.next_cursor.is_some() {
            return Err("an expired cursor must not advertise a next cursor");
        }
        if let (Some(next), Some(last)) = (self.next_cursor, previous)
            && next < last
        {
            return Err("next cursor must not rewind behind the page");
        }
        Ok(())
    }
}

impl RunNotification {
    /// The exact scope this notification is bound to.
    pub fn scope(&self) -> &RunScope {
        match self {
            Self::Event { scope, .. } | Self::Recovery { scope, .. } => scope,
        }
    }

    /// Validate the share-safe notification before it reaches a consumer.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.scope().validate()?;
        match self {
            Self::Event { event, .. } => event.validate(),
            Self::Recovery {
                reason, poll_tool, ..
            } => {
                ensure_share_safe_metadata("reason", reason, MAX_REASON_BYTES)
                    .map_err(|finding| finding.kind.reason_code())?;
                ensure_share_safe_metadata("poll_tool", poll_tool, MAX_REASON_BYTES)
                    .map_err(|finding| finding.kind.reason_code())?;
                Ok(())
            }
        }
    }
}

/// Exact changed-file summary used for human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Bounded human-readable summary.
    pub summary: String,
}

/// Review projection for an isolated run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
        ensure_share_safe_metadata("fingerprint", &self.fingerprint, MAX_FINGERPRINT_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        ensure_no_credential_material("diff", &self.diff, MAX_REVIEW_DIFF_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        for file in &self.changed_files {
            // Paths are authority metadata: repository-relative, no absolute
            // host path, no traversal, no provider URL, no credential.
            ensure_share_safe_metadata("changed_files.path", &file.path, MAX_CHANGED_PATH_BYTES)
                .map_err(|finding| finding.kind.reason_code())?;
            ensure_no_credential_material(
                "changed_files.summary",
                &file.summary,
                MAX_CHANGED_SUMMARY_BYTES,
            )
            .map_err(|finding| finding.kind.reason_code())?;
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
