//! The operator control protocol.
//!
//! One newline-delimited JSON request in, one reply out, in order, over a
//! stream the host already owns — no listening socket, no second writer, no new
//! identity or authentication model. That keeps the headless host inside the
//! authority boundary ADR-002 draws while still being steerable while it runs.
//!
//! Every request and every command body rejects unknown fields, so a typo or a
//! newer client's extra key is refused rather than silently ignored.

use grokptah_agent_sdk::ErrorEnvelope;
use grokptah_agent_sdk::run::{Bounds, ExecutionMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::attention::AttentionResolution;
use crate::error::HostError;
use crate::lease::ControlClass;

/// Maximum bytes accepted in one request line.
pub const MAX_REQUEST_BYTES: usize = 128 * 1024;

/// One operator request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlRequest {
    /// Optional correlation identity echoed on the reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The requested operation.
    pub command: ControlCommand,
}

/// The operations a headless operator can perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ControlCommand {
    /// Report host health and readiness.
    Health,
    /// Report the capabilities this host can honor.
    Capabilities,
    /// Submit a bounded run.
    Submit {
        /// Fresh idempotency key.
        request_id: String,
        /// Prompt after policy validation.
        prompt: String,
        /// Optional caller bounds; may narrow the host ceiling only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounds: Option<Bounds>,
        /// Shared or isolated execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_mode: Option<ExecutionMode>,
        /// Wait for bounded admission instead of failing fast.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allow_queue: Option<bool>,
    },
    /// Report one run's truthful status.
    Status {
        /// Run identity.
        run_id: String,
    },
    /// Replay a run's retained events after a cursor.
    Events {
        /// Run identity.
        run_id: String,
        /// Last sequence the caller already saw.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_seq: Option<u64>,
        /// Maximum entries to return.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Take a short-lived control lease over a run.
    Lease {
        /// Run identity.
        run_id: String,
        /// Control classes requested.
        classes: Vec<ControlClass>,
        /// Revision the operator observed.
        expected_revision: u64,
        /// Optional lease lifetime; the host ceiling still applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_ms: Option<u64>,
    },
    /// Steer a run that is already admitted.
    Steer {
        /// Run identity.
        run_id: String,
        /// Lease authorizing the change.
        lease_id: String,
        /// Revision the operator observed.
        expected_revision: u64,
        /// Bounded steering directive.
        directive: String,
    },
    /// Halt a run so it can be resumed later.
    Pause {
        /// Run identity.
        run_id: String,
        /// Lease authorizing the change.
        lease_id: String,
        /// Revision the operator observed.
        expected_revision: u64,
    },
    /// Continue a halted run.
    Resume {
        /// Run identity.
        run_id: String,
        /// Lease authorizing the change.
        lease_id: String,
        /// Revision the operator observed.
        expected_revision: u64,
        /// Fresh prompt, required when the host no longer retains the original.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Terminate a run.
    Cancel {
        /// Run identity.
        run_id: String,
        /// Lease authorizing the change.
        lease_id: String,
        /// Revision the operator observed.
        expected_revision: u64,
    },
    /// Read a run's open escalation.
    Attention {
        /// Run identity.
        run_id: String,
    },
    /// Answer a run's open escalation.
    ResolveAttention {
        /// Run identity.
        run_id: String,
        /// Escalation identity the operator is answering.
        attention_id: String,
        /// Allow or deny.
        resolution: AttentionResolution,
    },
    /// Read a completed run's review receipt.
    Receipt {
        /// Run identity.
        run_id: String,
    },
    /// Advance the host by a bounded number of engine steps.
    Tick {
        /// Steps to take; defaults to one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        steps: Option<u32>,
    },
    /// Ask the host to stop.
    Shutdown {
        /// Stop now instead of draining.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        immediate: Option<bool>,
    },
}

impl ControlCommand {
    /// Stable label for logs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Capabilities => "capabilities",
            Self::Submit { .. } => "submit",
            Self::Status { .. } => "status",
            Self::Events { .. } => "events",
            Self::Lease { .. } => "lease",
            Self::Steer { .. } => "steer",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
            Self::Cancel { .. } => "cancel",
            Self::Attention { .. } => "attention",
            Self::ResolveAttention { .. } => "resolveAttention",
            Self::Receipt { .. } => "receipt",
            Self::Tick { .. } => "tick",
            Self::Shutdown { .. } => "shutdown",
        }
    }
}

/// The outcome of one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ControlResult {
    /// The operation succeeded.
    Ok {
        /// Operation-specific payload.
        payload: Value,
    },
    /// The operation was refused.
    Error {
        /// Stable public envelope.
        error: ErrorEnvelope,
    },
}

/// One reply, correlated to its request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlReply {
    /// Correlation identity echoed from the request, when one was supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The outcome.
    pub result: ControlResult,
}

impl ControlReply {
    /// A successful reply.
    pub fn ok(id: Option<String>, payload: Value) -> Self {
        Self {
            id,
            result: ControlResult::Ok { payload },
        }
    }

    /// A refused reply.
    pub fn error(id: Option<String>, error: &HostError) -> Self {
        Self {
            id,
            result: ControlResult::Error {
                error: error.envelope(),
            },
        }
    }

    /// Whether the reply reports success.
    pub fn is_ok(&self) -> bool {
        matches!(self.result, ControlResult::Ok { .. })
    }

    /// Serialize the reply as one NDJSON line.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // A reply that cannot serialize is still an answer, not a hang.
            "{\"result\":{\"status\":\"error\",\"error\":{\"code\":\"internal\",\
             \"message\":\"reply could not be serialized\",\"requestId\":null}}}"
                .to_owned()
        })
    }
}

/// Parse one NDJSON request line.
pub fn parse_request(line: &str) -> Result<ControlRequest, HostError> {
    if line.len() > MAX_REQUEST_BYTES {
        return Err(HostError::invalid(
            "request_too_large",
            "the request exceeds its byte bound",
        ));
    }
    serde_json::from_str(line).map_err(|_| {
        HostError::invalid(
            "request_malformed",
            "the request is not a valid control request",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_through_their_wire_shape() {
        let request = parse_request(
            r#"{"id":"1","command":{"op":"submit","requestId":"req-1","prompt":"build",
                "bounds":{"maxRounds":2},"executionMode":"isolated_worktree","allowQueue":true}}"#,
        )
        .expect("parses");
        assert_eq!(request.id.as_deref(), Some("1"));
        assert_eq!(request.command.label(), "submit");

        let encoded = serde_json::to_value(&request).expect("serializes");
        assert_eq!(encoded["command"]["op"], "submit");
        assert_eq!(encoded["command"]["bounds"]["maxRounds"], 2);
    }

    #[test]
    fn unknown_operations_and_unknown_fields_are_refused() {
        assert_eq!(
            parse_request(r#"{"command":{"op":"selfDestruct"}}"#)
                .expect_err("unknown op")
                .reason_code(),
            "request_malformed"
        );
        assert_eq!(
            parse_request(r#"{"command":{"op":"health"},"extra":1}"#)
                .expect_err("unknown top-level field")
                .reason_code(),
            "request_malformed"
        );
        assert_eq!(
            parse_request(r#"{"command":{"op":"status","runId":"r","sudo":true}}"#)
                .expect_err("unknown command field")
                .reason_code(),
            "request_malformed"
        );
        assert_eq!(
            parse_request("not json")
                .expect_err("garbage")
                .reason_code(),
            "request_malformed"
        );
    }

    #[test]
    fn oversized_requests_are_refused_before_parsing() {
        let line = format!(
            r#"{{"command":{{"op":"submit","requestId":"r","prompt":"{}"}}}}"#,
            "x".repeat(MAX_REQUEST_BYTES)
        );
        assert_eq!(
            parse_request(&line).expect_err("too large").reason_code(),
            "request_too_large"
        );
    }

    #[test]
    fn replies_carry_the_stable_envelope_and_correlation_id() {
        let reply = ControlReply::error(
            Some("7".into()),
            &HostError::forbidden("capability_gated", "needs a grant"),
        );
        let line = reply.to_line();
        let decoded: ControlReply = serde_json::from_str(&line).expect("round-trips");
        assert_eq!(decoded.id.as_deref(), Some("7"));
        assert!(!decoded.is_ok());
        assert!(line.contains("capability_gated"));

        let ok = ControlReply::ok(None, serde_json::json!({"ready": true}));
        assert!(ok.is_ok());
        assert!(!ok.to_line().contains("\"id\""));
    }
}
