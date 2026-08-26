//! Computer Use observation and control contracts.

use serde::{Deserialize, Serialize};

use crate::projection::{ensure_json_share_safe, ensure_share_safe_metadata};
use crate::run::{MAX_REASON_BYTES, MAX_REQUEST_ID_BYTES, MAX_TIMESTAMP_BYTES, RunScope};

/// Maximum serialized bytes in one redacted Computer Use event detail.
pub const MAX_COMPUTER_EVENT_DETAIL_BYTES: usize = 256 * 1024;
/// Maximum UTF-8 bytes in a share-safe Computer Use disposition.
pub const MAX_DISPOSITION_BYTES: usize = 128;
/// Maximum action classes one lease may grant.
pub const MAX_ACTION_CLASSES: usize = 8;
/// Maximum lease duration accepted by the versioned public contract.
pub const MAX_LEASE_TTL_MS: u64 = 5 * 60 * 1000;

/// A semantic action class allowed by an explicit lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerActionClass {
    /// Activate or invoke a freshly observed semantic target.
    Semantic,
    /// Enter text into a freshly observed safe field.
    TextEntry,
}

/// Scope for a Computer Use run; kept distinct to make call sites explicit.
pub type ComputerRunScope = RunScope;

/// Lease/revision-fenced Computer Use control request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerControlRequest {
    /// Fresh idempotency key for this control intent.
    pub request_id: String,
    /// Exact run/session/workspace scope.
    pub scope: ComputerRunScope,
    /// Revision observed by the human/operator.
    pub expected_version: u64,
    /// Action classes granted by the human/operator.
    pub action_classes: Vec<ComputerActionClass>,
    /// Short lease duration.
    pub ttl_ms: u64,
}

impl ComputerControlRequest {
    /// Validate a lease request before it crosses a product boundary.
    pub fn validate(&self) -> Result<(), &'static str> {
        ensure_share_safe_metadata("request_id", &self.request_id, MAX_REQUEST_ID_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        self.scope.validate()?;
        if self.action_classes.is_empty() {
            return Err("at least one action class is required");
        }
        if self.action_classes.len() > MAX_ACTION_CLASSES {
            return Err("action_classes exceeds its bound");
        }
        // A lease must not silently grant the same class twice; duplicates
        // make an audit of what was granted ambiguous.
        for (index, class) in self.action_classes.iter().enumerate() {
            if self.action_classes[..index].contains(class) {
                return Err("action_classes must not repeat a class");
            }
        }
        if self.ttl_ms == 0 {
            return Err("ttl_ms must be greater than zero");
        }
        if self.ttl_ms > MAX_LEASE_TTL_MS {
            return Err("ttl_ms exceeds the contract lease ceiling");
        }
        Ok(())
    }

    /// Reject a lease request that is not fenced to the revision the operator
    /// actually observed.
    ///
    /// A control request carries the revision the human approved. If the
    /// authority has advanced past it, the approval no longer describes what
    /// is on screen and the request must fail closed rather than replay
    /// against newer state.
    pub fn ensure_fresh_against(&self, authority_version: u64) -> Result<(), &'static str> {
        if self.expected_version != authority_version {
            return Err("expected_version is stale for this run revision");
        }
        Ok(())
    }
}

/// Safe response envelope for a Computer Use control operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerControlResponse {
    /// Exact run scope returned by the authority.
    pub scope: ComputerRunScope,
    /// New revision after the accepted operation.
    pub version: u64,
    /// Share-safe disposition.
    pub disposition: String,
}

impl ComputerControlResponse {
    /// Validate the bounded result returned by an authority.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.scope.validate()?;
        ensure_share_safe_metadata("disposition", &self.disposition, MAX_DISPOSITION_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        Ok(())
    }
}

/// One redacted Computer Use audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerEvent {
    /// Strictly increasing event sequence.
    pub seq: u64,
    /// RFC3339 timestamp.
    pub ts: String,
    /// Share-safe event kind.
    pub kind: String,
    /// Redacted event payload.
    pub detail: serde_json::Value,
}

impl ComputerEvent {
    /// Validate the share-safe, bounded event projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        ensure_share_safe_metadata("ts", &self.ts, MAX_TIMESTAMP_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        ensure_share_safe_metadata("kind", &self.kind, MAX_REASON_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        ensure_json_share_safe("detail", &self.detail, MAX_COMPUTER_EVENT_DETAIL_BYTES)
            .map_err(|finding| finding.kind.reason_code())?;
        Ok(())
    }
}

/// Cursor-paged Computer Use audit events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerEventPage {
    /// Retained events in sequence order.
    pub entries: Vec<ComputerEvent>,
    /// Cursor for the next page, if any.
    pub next_cursor: Option<u64>,
    /// Whether the requested cursor is outside the retained window.
    pub cursor_expired: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_serializes_scope_and_lease() {
        let request = ComputerControlRequest {
            request_id: "req-1".into(),
            scope: ComputerRunScope {
                session_id: "s".into(),
                workspace: "/approved".into(),
                run_id: "r".into(),
            },
            expected_version: 4,
            action_classes: vec![ComputerActionClass::Semantic],
            ttl_ms: 30_000,
        };
        let value = serde_json::to_value(request).expect("control request serializes");
        assert_eq!(value["expectedVersion"], 4);
        assert_eq!(value["actionClasses"][0], "semantic");
        assert_eq!(value["scope"]["runId"], "r");
    }

    #[test]
    fn control_request_validation_requires_a_bounded_lease() {
        let request = ComputerControlRequest {
            request_id: "req-1".into(),
            scope: ComputerRunScope {
                session_id: "s".into(),
                workspace: "/approved".into(),
                run_id: "r".into(),
            },
            expected_version: 0,
            action_classes: Vec::new(),
            ttl_ms: 0,
        };
        assert!(request.validate().is_err());
    }
}
