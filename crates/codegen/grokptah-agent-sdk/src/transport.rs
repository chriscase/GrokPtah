//! Consumer-supplied MCP transport. Auth, TLS, and session setup stay outside.

use std::future::Future;

use serde_json::Value;

use crate::error::SdkError;
use crate::page::RetainedRange;

/// `tools/list` entry used as discovery input. Extra MCP fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpTool {
    pub name: String,
}

/// Failures raised by the consumer transport before SDK projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    Unauthenticated,
    Timeout,
    CapacityExhausted,
    /// Malformed MCP/JSON-RPC envelope.
    Protocol,
    /// I/O, DNS, TLS, or other connection failure.
    Io,
    /// Host JSON-RPC `error.data` (`code` plus optional `eventRange`).
    Host {
        code: String,
        event_range: Option<RetainedRange>,
    },
}

impl TransportError {
    /// Map current MCP control `error.data` (`{"code": "...", "eventRange"?}`).
    pub fn from_host_data(data: &Value) -> Self {
        let code = data
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("internal");
        let event_range = RetainedRange::from_host(data.get("eventRange"));
        match code {
            "unauthenticated" => Self::Unauthenticated,
            "timeout" => Self::Timeout,
            "capacity_exhausted" => Self::CapacityExhausted,
            other => Self::Host {
                code: other.to_string(),
                event_range,
            },
        }
    }
}

impl From<TransportError> for SdkError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Unauthenticated => Self::Unauthenticated,
            TransportError::Timeout => Self::Timeout,
            TransportError::CapacityExhausted => Self::CapacityExhausted,
            TransportError::Protocol | TransportError::Io => Self::Internal,
            TransportError::Host { code, event_range } => {
                SdkError::from_host_code(&code, event_range)
            }
        }
    }
}

/// Minimal MCP surface: `tools/list` plus `tools/call` JSON bodies.
///
/// Implementations supply authentication, TLS, MCP initialize/session headers,
/// and JSON-RPC framing. This trait never opens sockets itself.
pub trait McpTransport: Send + Sync {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<McpTool>, TransportError>> + Send;

    /// Call one MCP tool. Return `structuredContent` or the OrchestrationService body.
    fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> impl Future<Output = Result<Value, TransportError>> + Send;
}
