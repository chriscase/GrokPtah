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

/// Maximum UTF-8 bytes accepted for a Computer Use event kind.
const MAX_COMPUTER_EVENT_KIND_BYTES: usize = 128;
/// Maximum UTF-8 bytes accepted for a Computer Use event timestamp.
const MAX_COMPUTER_EVENT_TS_BYTES: usize = 128;
/// Maximum UTF-8 bytes accepted for a Computer Use disposition.
const MAX_COMPUTER_EVENT_DISPOSITION_BYTES: usize = 128;
/// Maximum UTF-8 bytes accepted for a Computer Use observation identity.
const MAX_COMPUTER_EVENT_ID_BYTES: usize = 256;
/// Maximum UTF-8 bytes accepted for a Computer Use error code.
const MAX_COMPUTER_EVENT_ERROR_BYTES: usize = 128;

/// Bounded, redacted Computer Use audit payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerEventDetail {
    /// Share-safe disposition such as `observed`, `acted`, or `denied`.
    pub disposition: String,
    /// Optional opaque observation identity; never a host path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<String>,
    /// Optional share-safe error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl ComputerEventDetail {
    /// Validate the share-safe, bounded Computer Use detail projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        crate::redact::reject_bounded_text(
            &self.disposition,
            MAX_COMPUTER_EVENT_DISPOSITION_BYTES,
            "computer event disposition must be non-empty and bounded",
            "computer event disposition contains privileged data",
        )?;
        if let Some(observation_id) = &self.observation_id {
            crate::redact::reject_bounded_text(
                observation_id,
                MAX_COMPUTER_EVENT_ID_BYTES,
                "computer event observation_id must be non-empty and bounded",
                "computer event observation_id contains privileged data",
            )?;
        }
        if let Some(error_code) = &self.error_code {
            crate::redact::reject_bounded_text(
                error_code,
                MAX_COMPUTER_EVENT_ERROR_BYTES,
                "computer event error_code must be non-empty and bounded",
                "computer event error_code contains privileged data",
            )?;
        }
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
    pub detail: ComputerEventDetail,
}

impl ComputerEvent {
    /// Validate the share-safe, bounded event projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        crate::redact::reject_bounded_text(
            &self.ts,
            MAX_COMPUTER_EVENT_TS_BYTES,
            "computer event metadata must be non-empty and bounded",
            "computer event metadata contains privileged data",
        )?;
        crate::redact::reject_bounded_text(
            &self.kind,
            MAX_COMPUTER_EVENT_KIND_BYTES,
            "computer event metadata must be non-empty and bounded",
            "computer event metadata contains privileged data",
        )?;
        self.detail.validate()
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

    #[test]
    fn computer_event_detail_is_bounded_redacted_and_fail_closed() {
        let event = ComputerEvent {
            seq: 4,
            ts: "2026-08-25T00:00:00Z".into(),
            kind: "observe".into(),
            detail: ComputerEventDetail {
                disposition: "observed".into(),
                observation_id: Some("obs-1".into()),
                error_code: None,
            },
        };
        event
            .validate()
            .expect("share-safe computer event validates");
        let value = serde_json::to_value(&event).expect("computer event serializes");
        assert_eq!(value["kind"], "observe");
        assert_eq!(value["detail"]["disposition"], "observed");
        assert_eq!(value["detail"]["observationId"], "obs-1");
        assert!(value["detail"].get("authorization").is_none());
        let round: ComputerEvent =
            serde_json::from_value(value).expect("computer event deserializes");
        round
            .validate()
            .expect("round-tripped computer event validates");
        assert_eq!(round.detail.observation_id.as_deref(), Some("obs-1"));

        assert!(
            serde_json::from_value::<ComputerEvent>(serde_json::json!({
                "seq": 4,
                "ts": "2026-08-25T00:00:00Z",
                "kind": "observe",
                "detail": { "disposition": "observed", "authorization": "Bearer secret" }
            }))
            .is_err(),
            "computer event detail must deny unknown fields"
        );
        assert!(
            serde_json::from_value::<ComputerEvent>(serde_json::json!({
                "seq": 4,
                "ts": "2026-08-25T00:00:00Z",
                "kind": "observe",
                "detail": "raw screenshot bytes"
            }))
            .is_err(),
            "computer event detail must not accept an unparsed JSON value"
        );
        assert!(
            ComputerEvent {
                seq: 4,
                ts: "2026-08-25T00:00:00Z".into(),
                kind: "observe".into(),
                detail: ComputerEventDetail {
                    disposition: "/private/secret".into(),
                    observation_id: None,
                    error_code: None,
                },
            }
            .validate()
            .is_err(),
            "computer event detail must reject privileged text"
        );
    }
}
