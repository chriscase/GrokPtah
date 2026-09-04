use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::IdempotencyReceipt;

pub const OPERATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const MAX_OPERATION_RECEIPT_PAGE_SIZE: usize = 100;

/// Secret-free operator projection of one durable mutation receipt.
///
/// The payload hash, replay body, detailed error, owner identity, credential
/// identity, and host workspace are intentionally absent. Scope is proven by
/// the service before this projection is constructed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationReceiptV1 {
    pub schema_version: u32,
    pub request_id: String,
    pub session_id: Uuid,
    pub tool: String,
    pub run_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub error_code: Option<String>,
}

impl From<&IdempotencyReceipt> for OperationReceiptV1 {
    fn from(receipt: &IdempotencyReceipt) -> Self {
        Self {
            schema_version: OPERATION_RECEIPT_SCHEMA_VERSION,
            request_id: receipt.request_id.clone(),
            session_id: receipt.session_id,
            tool: receipt.tool.clone(),
            run_id: receipt.run_id.clone(),
            status: receipt.status.clone(),
            created_at: receipt.created_at,
            error_code: receipt
                .error
                .as_ref()
                .map(|error| error.code.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationReceiptPageV1 {
    pub schema_version: u32,
    pub receipts: Vec<OperationReceiptV1>,
    pub next_after_request_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::types::IdempotencyReceipt;
    use super::*;
    use crate::orchestration::{OrchError, OrchErrorCode};

    #[test]
    fn projection_is_allowlisted_and_strict() {
        let receipt = IdempotencyReceipt {
            schema_version: 2,
            owner_id: "owner-secret".into(),
            session_id: Uuid::nil(),
            workspace_digest: "a".repeat(64),
            request_id: "request-1".into(),
            payload_hash: "payload-secret".into(),
            run_id: Some("run-1".into()),
            tool: "ptah_submit_task".into(),
            response: serde_json::json!({"secret": "response-secret"}),
            error: Some(OrchError::new(OrchErrorCode::Conflict, "error-secret")),
            created_at: Utc::now(),
            status: "failed".into(),
        };

        let value = serde_json::to_value(OperationReceiptV1::from(&receipt)).unwrap();
        let rendered = value.to_string();
        for secret in [
            "owner-secret",
            "payload-secret",
            "response-secret",
            "error-secret",
            "workspaceDigest",
        ] {
            assert!(!rendered.contains(secret), "projection leaked {secret}");
        }
        assert_eq!(value["errorCode"], "conflict");

        let mut extra = value;
        extra["workspace"] = serde_json::json!("/private/secret");
        assert!(serde_json::from_value::<OperationReceiptV1>(extra).is_err());
    }
}
