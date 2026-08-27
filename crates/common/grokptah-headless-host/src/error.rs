//! Host-side failures and their stable public envelopes.
//!
//! Every operator-visible failure carries a stable [`ErrorCode`] category plus
//! a machine-readable `reason_code`. Privileged diagnostics never reach the
//! envelope: the message is built from fixed vocabulary and redacted values.

use grokptah_agent_sdk::{ErrorCode, ErrorEnvelope, ErrorEventRange};

/// Result alias for host operations.
pub type HostResult<T> = Result<T, HostError>;

/// A host failure that can be projected into the public error contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    code: ErrorCode,
    reason_code: &'static str,
    message: String,
    request_id: Option<String>,
    event_range: Option<ErrorEventRange>,
}

impl HostError {
    fn new(code: ErrorCode, reason_code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            reason_code,
            message: message.into(),
            request_id: None,
            event_range: None,
        }
    }

    /// Request shape, identity, or bounds are invalid.
    pub fn invalid(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, reason_code, message)
    }

    /// The caller is not authenticated for this host home.
    pub fn unauthenticated(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthenticated, reason_code, message)
    }

    /// Capability, scope, or lease authority is missing. This is the
    /// default-deny outcome; it is never downgraded to a softer category.
    pub fn forbidden(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ForbiddenScope, reason_code, message)
    }

    /// The opaque identity is unknown to this caller.
    pub fn not_found(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, reason_code, message)
    }

    /// A cursor, revision, or lease is stale and the caller must recover.
    pub fn stale(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StaleOrRecovery, reason_code, message)
    }

    /// Bounded admission is full.
    pub fn capacity(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Capacity, reason_code, message)
    }

    /// The host authority is not available (not started, locked, draining).
    pub fn unavailable(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AuthorityUnavailable, reason_code, message)
    }

    /// An unexpected host failure with no privileged detail.
    pub fn internal(reason_code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, reason_code, message)
    }

    /// Attach the caller idempotency key for audit correlation.
    #[must_use]
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// Attach the retained event window for cursor recovery.
    #[must_use]
    pub fn with_event_range(mut self, start_seq: u64, end_seq: u64) -> Self {
        self.event_range = Some(ErrorEventRange { start_seq, end_seq });
        self
    }

    /// Stable public category.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// Stable machine-readable reason.
    pub fn reason_code(&self) -> &'static str {
        self.reason_code
    }

    /// Project the failure into the share-safe public envelope.
    pub fn envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            code: self.code,
            message: self.message.clone(),
            request_id: self.request_id.clone(),
            reason_code: Some(self.reason_code.to_owned()),
            event_range: self.event_range,
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.reason_code, self.message)
    }
}

impl std::error::Error for HostError {}

/// Map an I/O failure without leaking a host path or OS detail.
pub(crate) fn io_error(reason_code: &'static str, error: &std::io::Error) -> HostError {
    HostError::internal(
        reason_code,
        format!("host storage failed ({})", error.kind()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_keeps_the_stable_category_and_reason() {
        let error = HostError::forbidden("capability_gated", "capability requires a grant")
            .with_request_id("req-1");
        let envelope = error.envelope();
        assert_eq!(envelope.code, ErrorCode::ForbiddenScope);
        assert_eq!(envelope.reason_code.as_deref(), Some("capability_gated"));
        assert_eq!(envelope.request_id.as_deref(), Some("req-1"));
        assert!(envelope.event_range.is_none());
    }

    #[test]
    fn cursor_recovery_carries_the_retained_window() {
        let error = HostError::stale("cursor_expired", "resume from the retained window")
            .with_event_range(4, 9);
        let envelope = error.envelope();
        assert_eq!(envelope.code, ErrorCode::StaleOrRecovery);
        assert_eq!(
            envelope.event_range,
            Some(ErrorEventRange {
                start_seq: 4,
                end_seq: 9
            })
        );
    }

    #[test]
    fn io_failures_do_not_leak_a_host_path() {
        let raw = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "/private/home/secret");
        let error = io_error("journal_write_failed", &raw);
        assert_eq!(error.code(), ErrorCode::Internal);
        assert!(!error.envelope().message.contains("secret"));
    }
}
