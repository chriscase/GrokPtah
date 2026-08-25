//! Stable, share-safe error envelopes.

use serde::{Deserialize, Serialize};

/// Cross-product error category. Privileged diagnostics stay server-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Request shape or bounds are invalid.
    InvalidRequest,
    /// Authentication is missing or expired.
    Unauthenticated,
    /// Session/workspace/capability scope is not allowed.
    ForbiddenScope,
    /// Opaque identity was not found for this caller.
    NotFound,
    /// The method, or a host capability it requires, is not available here.
    ///
    /// This is the fail-closed category for a host that never offers the
    /// capability (an unknown method, or a Computer Use ledger that is not
    /// installed on this host). It is deliberately distinct from
    /// [`Self::InvalidRequest`], which reports a malformed request, and from
    /// [`Self::AuthorityUnavailable`], which reports an installed authority
    /// that is temporarily asleep or locked and may recover on retry.
    Unsupported,
    /// Cursor, revision, or approval is stale.
    StaleOrRecovery,
    /// Bounded admission is full.
    Capacity,
    /// The desktop authority is asleep, locked, or unavailable.
    AuthorityUnavailable,
    /// Unexpected failure with no privileged detail.
    Internal,
}

/// Bounded retained-event range returned with cursor recovery errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEventRange {
    /// First retained sequence, inclusive.
    pub start_seq: u64,
    /// Last retained sequence, inclusive.
    pub end_seq: u64,
}

/// Public error envelope suitable for a broker or non-Rust client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    /// Stable category.
    pub code: ErrorCode,
    /// Share-safe message.
    pub message: String,
    /// Request identity for audit/support correlation.
    pub request_id: Option<String>,
    /// Optional bounded transport reason; `code` remains the stable category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Optional retained-event range for cursor recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_range: Option<ErrorEventRange>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_never_requires_privileged_diagnostics() {
        let error = ErrorEnvelope {
            code: ErrorCode::ForbiddenScope,
            message: "workspace is not bound".into(),
            request_id: Some("req-1".into()),
            reason_code: Some("workspace_mismatch".into()),
            event_range: None,
        };
        let value = serde_json::to_value(error).expect("error serializes");
        assert!(value.get("privilegedPath").is_none());
        assert_eq!(value["reasonCode"], "workspace_mismatch");
        let recovered = ErrorEnvelope {
            code: ErrorCode::StaleOrRecovery,
            message: "resume from the retained window".into(),
            request_id: None,
            reason_code: Some("cursor_expired".into()),
            event_range: Some(ErrorEventRange {
                start_seq: 12,
                end_seq: 18,
            }),
        };
        let recovered_value = serde_json::to_value(&recovered).expect("recovery serializes");
        assert_eq!(recovered_value["eventRange"]["startSeq"], 12);
        assert_eq!(recovered_value["eventRange"]["endSeq"], 18);
        let decoded: ErrorEnvelope =
            serde_json::from_value(recovered_value).expect("recovery round-trips");
        assert_eq!(decoded.event_range, recovered.event_range);
    }

    /// Every wire code a transport may emit must round-trip through the
    /// published envelope. A transport that patches a code onto the
    /// serialized payload without a matching variant produces a body no
    /// Rust consumer can decode, so the taxonomy is asserted here rather
    /// than at each transport.
    #[test]
    fn every_public_error_code_round_trips_on_the_wire() {
        let expected = [
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::Unauthenticated, "unauthenticated"),
            (ErrorCode::ForbiddenScope, "forbidden_scope"),
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::Unsupported, "unsupported"),
            (ErrorCode::StaleOrRecovery, "stale_or_recovery"),
            (ErrorCode::Capacity, "capacity"),
            (ErrorCode::AuthorityUnavailable, "authority_unavailable"),
            (ErrorCode::Internal, "internal"),
        ];
        for (code, wire) in expected {
            let envelope = ErrorEnvelope {
                code,
                message: "bounded".into(),
                request_id: None,
                reason_code: None,
                event_range: None,
            };
            let value = serde_json::to_value(&envelope).expect("envelope serializes");
            assert_eq!(value["code"], wire, "{code:?} must serialize to {wire}");
            let decoded: ErrorEnvelope =
                serde_json::from_value(value).expect("envelope round-trips");
            assert_eq!(decoded, envelope);
        }
    }

    /// The fail-closed 405 category is its own code. Collapsing it into the
    /// HTTP 400 request-shape category would tell a client to edit a request
    /// that no host here will ever accept.
    #[test]
    fn unsupported_is_not_the_invalid_request_category() {
        assert_ne!(ErrorCode::Unsupported, ErrorCode::InvalidRequest);
        let value = serde_json::to_value(ErrorCode::Unsupported).expect("code serializes");
        assert_eq!(value, "unsupported");
    }
}
