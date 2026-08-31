//! Host data-code mapping. Messages are not forwarded (they can name paths).

use crate::page::RetainedRange;

/// Fail-closed SDK error. Variants match current MCP `error.data.code` values
/// listed in the read-seam contract. Unknown host codes collapse to [`Self::Internal`].
/// `grokptah.public-run.v1` / `grokptah.public-event.v1` decode and
/// unknown-version failures also collapse to [`Self::Internal`] without
/// forwarding serde payloads or field values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SdkError {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("forbidden_scope")]
    ForbiddenScope,
    #[error("workspace_mismatch")]
    WorkspaceMismatch,
    #[error("cursor_expired")]
    CursorExpired { event_range: Option<RetainedRange> },
    #[error("invalid_request")]
    InvalidRequest,
    #[error("unsupported")]
    Unsupported,
    #[error("conflict")]
    Conflict,
    #[error("timeout")]
    Timeout,
    #[error("capacity_exhausted")]
    CapacityExhausted,
    #[error("internal")]
    Internal,
}

impl SdkError {
    /// Wire `error.data.code` for this error.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::ForbiddenScope => "forbidden_scope",
            Self::WorkspaceMismatch => "workspace_mismatch",
            Self::CursorExpired { .. } => "cursor_expired",
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::Conflict => "conflict",
            Self::Timeout => "timeout",
            Self::CapacityExhausted => "capacity_exhausted",
            Self::Internal => "internal",
        }
    }

    pub(crate) fn from_host_code(code: &str, event_range: Option<RetainedRange>) -> Self {
        match code {
            "unauthenticated" => Self::Unauthenticated,
            "forbidden_scope" => Self::ForbiddenScope,
            "workspace_mismatch" => Self::WorkspaceMismatch,
            "cursor_expired" => Self::CursorExpired { event_range },
            "invalid_request" => Self::InvalidRequest,
            "unsupported" => Self::Unsupported,
            "conflict" | "session_busy" | "stale_version" => Self::Conflict,
            "timeout" => Self::Timeout,
            "capacity_exhausted" => Self::CapacityExhausted,
            "internal" => Self::Internal,
            _ => Self::Internal,
        }
    }

    /// Unknown and cross-scope run denials are indistinguishable to callers.
    pub(crate) fn collapse_run_scope(self) -> Self {
        match self {
            Self::InvalidRequest | Self::ForbiddenScope => Self::ForbiddenScope,
            other => other,
        }
    }
}
