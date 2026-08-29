//! Stable error taxonomy for the capability boundary.
//!
//! Every code below is either a **mirror** of a code the GrokPtah runtime
//! already emits, or a **seam-local** code that can only arise in the client /
//! adapter layer and never on the runtime wire. [`SdkErrorCode::origin`] states
//! which, so a consumer can tell "the host said no" apart from "the SDK could
//! not reach or trust the host". Nothing here invents a runtime behavior.
//!
//! Runtime mirrors come from two existing taxonomies:
//!
//! * `orchestration::OrchErrorCode` — the authenticated control plane.
//! * `computer_use::ComputerErrorCode` — the Computer Use ledger, whose
//!   read-only projections are reachable through this seam.
//!
//! Unknown wire codes decode to [`SdkErrorCode::Unknown`] instead of failing.
//! A newer host must be able to add a code without breaking an older consumer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a code can legitimately come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorOrigin {
    /// Mirrors a code the GrokPtah runtime already defines, with the same wire
    /// token. Usually it arrived from the host; an adapter or helper may also
    /// raise it for the same condition observed locally — `stale_observation`
    /// from [`RevisionWatermark`] is the example.
    ///
    /// [`RevisionWatermark`]: crate::dto::RevisionWatermark
    Runtime,
    /// Defined by this crate. Has no counterpart in any runtime taxonomy and
    /// never appears on the runtime wire.
    Seam,
    /// A code this build does not know. Treat as non-retryable.
    Unrecognized,
}

/// What a caller is allowed to do about a failure.
///
/// This is deliberately three-valued. Collapsing `Never` and `Unsafe` into one
/// "do not retry" would lose the case that matters most: a mutation whose
/// effect is *unknown*, where an automatic retry can double-apply real work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    /// Safe to retry with the **same** idempotency key.
    Safe,
    /// Retrying cannot help; the request must change first.
    Never,
    /// The mutation may or may not have been applied. An automatic retry is
    /// forbidden. A human or an explicit reconciling read must decide.
    Unsafe,
}

/// Stable, wire-level error identity.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SdkErrorCode {
    // ── Mirrors of `OrchErrorCode` ────────────────────────────────────────
    Unauthenticated,
    ForbiddenScope,
    WorkspaceMismatch,
    SessionBusy,
    CapacityExhausted,
    StaleVersion,
    CursorExpired,
    Timeout,
    InvalidRequest,
    Unsupported,
    Conflict,
    Internal,

    // ── Mirrors of `ComputerErrorCode` reachable through read projections ─
    /// The observation the caller reasoned from is no longer current.
    StaleObservation,
    /// A mutation was claimed but the host stopped before it could record an
    /// outcome. It will not be retried automatically.
    UncertainOutcome,

    // ── Seam-local ────────────────────────────────────────────────────────
    /// The adapter could not reach the host, or an established stream dropped.
    TransportUnavailable,
    /// Contract major mismatch; see [`crate::version::negotiate`].
    ContractVersionUnsupported,
    /// A required capability is absent or denied on this host.
    CapabilityUnavailable,
    /// A fetched artifact failed its declared size or digest check.
    IntegrityMismatch,

    /// Forward compatibility: a code this build does not recognize.
    Unknown(String),
}

impl SdkErrorCode {
    /// Canonical snake_case wire token.
    ///
    /// Mirror codes use byte-identical tokens to the runtime taxonomies, so an
    /// adapter maps them by string without a translation table.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::ForbiddenScope => "forbidden_scope",
            Self::WorkspaceMismatch => "workspace_mismatch",
            Self::SessionBusy => "session_busy",
            Self::CapacityExhausted => "capacity_exhausted",
            Self::StaleVersion => "stale_version",
            Self::CursorExpired => "cursor_expired",
            Self::Timeout => "timeout",
            Self::InvalidRequest => "invalid_request",
            Self::Unsupported => "unsupported",
            Self::Conflict => "conflict",
            Self::Internal => "internal",
            Self::StaleObservation => "stale_observation",
            Self::UncertainOutcome => "uncertain_outcome",
            Self::TransportUnavailable => "transport_unavailable",
            Self::ContractVersionUnsupported => "contract_version_unsupported",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::IntegrityMismatch => "integrity_mismatch",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    /// Decode a wire token. Unrecognized tokens are preserved, not dropped.
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "unauthenticated" => Self::Unauthenticated,
            "forbidden_scope" => Self::ForbiddenScope,
            "workspace_mismatch" => Self::WorkspaceMismatch,
            "session_busy" => Self::SessionBusy,
            "capacity_exhausted" => Self::CapacityExhausted,
            "stale_version" => Self::StaleVersion,
            "cursor_expired" => Self::CursorExpired,
            "timeout" => Self::Timeout,
            "invalid_request" => Self::InvalidRequest,
            "unsupported" => Self::Unsupported,
            "conflict" => Self::Conflict,
            "internal" => Self::Internal,
            "stale_observation" => Self::StaleObservation,
            "uncertain_outcome" => Self::UncertainOutcome,
            "transport_unavailable" => Self::TransportUnavailable,
            "contract_version_unsupported" => Self::ContractVersionUnsupported,
            "capability_unavailable" => Self::CapabilityUnavailable,
            "integrity_mismatch" => Self::IntegrityMismatch,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn origin(&self) -> ErrorOrigin {
        match self {
            Self::Unauthenticated
            | Self::ForbiddenScope
            | Self::WorkspaceMismatch
            | Self::SessionBusy
            | Self::CapacityExhausted
            | Self::StaleVersion
            | Self::CursorExpired
            | Self::Timeout
            | Self::InvalidRequest
            | Self::Unsupported
            | Self::Conflict
            | Self::Internal
            | Self::StaleObservation
            | Self::UncertainOutcome => ErrorOrigin::Runtime,
            Self::TransportUnavailable
            | Self::ContractVersionUnsupported
            | Self::CapabilityUnavailable
            | Self::IntegrityMismatch => ErrorOrigin::Seam,
            Self::Unknown(_) => ErrorOrigin::Unrecognized,
        }
    }

    /// Retry policy for this code.
    ///
    /// `Timeout` and `TransportUnavailable` are `Safe` only because every
    /// mutation on this boundary carries a [`RequestId`]: replaying the same
    /// key returns the original receipt instead of doing the work twice.
    ///
    /// [`RequestId`]: crate::ids::RequestId
    pub fn retry_disposition(&self) -> RetryDisposition {
        match self {
            Self::SessionBusy
            | Self::CapacityExhausted
            | Self::Timeout
            | Self::TransportUnavailable
            | Self::Internal => RetryDisposition::Safe,
            Self::UncertainOutcome => RetryDisposition::Unsafe,
            _ => RetryDisposition::Never,
        }
    }

    /// Convenience predicate for `RetryDisposition::Safe`.
    pub fn is_safely_retryable(&self) -> bool {
        self.retry_disposition() == RetryDisposition::Safe
    }

    /// Every code this build knows, in a stable order. Used by the conformance
    /// battery to pin the taxonomy against accidental removal.
    pub fn known() -> &'static [SdkErrorCode] {
        // `Unknown` is deliberately absent: it has no fixed wire token.
        static ALL: &[SdkErrorCode] = &[
            SdkErrorCode::Unauthenticated,
            SdkErrorCode::ForbiddenScope,
            SdkErrorCode::WorkspaceMismatch,
            SdkErrorCode::SessionBusy,
            SdkErrorCode::CapacityExhausted,
            SdkErrorCode::StaleVersion,
            SdkErrorCode::CursorExpired,
            SdkErrorCode::Timeout,
            SdkErrorCode::InvalidRequest,
            SdkErrorCode::Unsupported,
            SdkErrorCode::Conflict,
            SdkErrorCode::Internal,
            SdkErrorCode::StaleObservation,
            SdkErrorCode::UncertainOutcome,
            SdkErrorCode::TransportUnavailable,
            SdkErrorCode::ContractVersionUnsupported,
            SdkErrorCode::CapabilityUnavailable,
            SdkErrorCode::IntegrityMismatch,
        ];
        ALL
    }
}

impl Serialize for SdkErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for SdkErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::from_wire(&raw))
    }
}

impl std::fmt::Display for SdkErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// One failure crossing the capability boundary.
///
/// `details` is a bounded string map, not free JSON. A host cannot use it to
/// smuggle a transcript, a credential, or a filesystem path into a consumer
/// that believes it is reading a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SdkError {
    pub code: SdkErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

/// Longest error message the seam will carry. Matches the runtime's own
/// `ComputerError` message ceiling.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 512;
/// Longest single `details` value.
pub const MAX_ERROR_DETAIL_BYTES: usize = 256;
/// Most `details` entries on one error.
pub const MAX_ERROR_DETAILS: usize = 16;

impl SdkError {
    pub fn new(code: SdkErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: truncate_on_char_boundary(&message.into(), MAX_ERROR_MESSAGE_BYTES),
            details: BTreeMap::new(),
        }
    }

    /// Attach one bounded diagnostic. Silently ignored past
    /// [`MAX_ERROR_DETAILS`] so a faulty host cannot grow an error without
    /// bound through repeated calls.
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if self.details.len() >= MAX_ERROR_DETAILS {
            return self;
        }
        let key = key.into();
        if key.is_empty() || key.len() > MAX_ERROR_DETAIL_BYTES {
            return self;
        }
        let value = truncate_on_char_boundary(&value.into(), MAX_ERROR_DETAIL_BYTES);
        self.details.insert(key, value);
        self
    }

    pub fn detail(&self, key: &str) -> Option<&str> {
        self.details.get(key).map(String::as_str)
    }

    pub fn retry_disposition(&self) -> RetryDisposition {
        self.code.retry_disposition()
    }
}

impl std::fmt::Display for SdkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SdkError {}

/// UTF-8-safe truncation. Never splits a character.
pub(crate) fn truncate_on_char_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

pub type SdkResult<T> = Result<T, SdkError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_code_round_trips_through_the_wire() {
        for code in SdkErrorCode::known() {
            assert_eq!(&SdkErrorCode::from_wire(code.as_wire()), code);
        }
    }

    #[test]
    fn unknown_codes_are_preserved_not_dropped() {
        let decoded = SdkErrorCode::from_wire("some_future_code");
        assert_eq!(decoded.as_wire(), "some_future_code");
        assert_eq!(decoded.origin(), ErrorOrigin::Unrecognized);
        assert_eq!(decoded.retry_disposition(), RetryDisposition::Never);
    }

    #[test]
    fn uncertain_outcome_is_never_automatically_retryable() {
        assert_eq!(
            SdkErrorCode::UncertainOutcome.retry_disposition(),
            RetryDisposition::Unsafe
        );
        assert!(!SdkErrorCode::UncertainOutcome.is_safely_retryable());
    }

    #[test]
    fn messages_and_details_are_bounded() {
        let err = SdkError::new(SdkErrorCode::Internal, "x".repeat(4096))
            .with_detail("k", "y".repeat(4096));
        assert_eq!(err.message.len(), MAX_ERROR_MESSAGE_BYTES);
        assert_eq!(err.detail("k").unwrap().len(), MAX_ERROR_DETAIL_BYTES);
    }

    #[test]
    fn detail_map_cannot_grow_without_bound() {
        let mut err = SdkError::new(SdkErrorCode::Internal, "bounded");
        for i in 0..(MAX_ERROR_DETAILS * 4) {
            err = err.with_detail(format!("k{i}"), "v");
        }
        assert_eq!(err.details.len(), MAX_ERROR_DETAILS);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let s = "日".repeat(400);
        let out = truncate_on_char_boundary(&s, MAX_ERROR_MESSAGE_BYTES);
        assert!(out.len() <= MAX_ERROR_MESSAGE_BYTES);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }
}
