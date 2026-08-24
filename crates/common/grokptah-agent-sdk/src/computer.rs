//! Computer Use observation and control contracts.

use serde::{Deserialize, Serialize};

use crate::run::RunScope;

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
#[serde(rename_all = "camelCase")]
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
        if self.request_id.trim().is_empty() || self.request_id.len() > 256 {
            return Err("request_id must be non-empty and bounded");
        }
        self.scope.validate()?;
        if self.action_classes.is_empty() {
            return Err("at least one action class is required");
        }
        if self.ttl_ms == 0 {
            return Err("ttl_ms must be greater than zero");
        }
        Ok(())
    }
}

/// Safe response envelope for a Computer Use control operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
        if self.disposition.trim().is_empty() || self.disposition.len() > 128 {
            return Err("disposition must be non-empty and bounded");
        }
        Ok(())
    }
}

/// One redacted Computer Use audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
        if self.ts.trim().is_empty() || self.kind.trim().is_empty() || self.kind.len() > 128 {
            return Err("computer event metadata must be non-empty and bounded");
        }
        let bytes = serde_json::to_vec(&self.detail)
            .map_err(|_| "computer event detail is not serializable")?;
        if bytes.len() > 256 * 1024 {
            return Err("computer event detail exceeds its byte bound");
        }
        Ok(())
    }
}

/// Cursor-paged Computer Use audit events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
