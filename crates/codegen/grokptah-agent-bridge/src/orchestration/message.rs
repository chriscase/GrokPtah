//! Durable typed coordinator/worker messages (#307).
//!
//! Messages are not a second inbox queue and do not execute Work. A future
//! message adapter may create an activation through the reserved
//! `RoutineTrigger::External { Message }` boundary.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{hash_payload, OrchError, OrchErrorCode};
use super::workload::invalid;

pub const MESSAGE_SCHEMA_VERSION: u32 = 1;
pub const MAX_MESSAGE_BODY_BYTES: usize = 8 * 1024;
pub const MAX_MESSAGE_PAYLOAD_BYTES: usize = 8 * 1024;
pub const MAX_MESSAGE_ACTOR_BYTES: usize = 256;
pub const DEFAULT_QUESTION_TTL_MS: i64 = 15 * 60 * 1_000;
pub const MAX_RETAINED_MESSAGES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Status,
    Question,
    Answer,
    Instruction,
    Handoff,
    ReviewRequest,
    ReviewResult,
    Informational,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Question => "question",
            Self::Answer => "answer",
            Self::Instruction => "instruction",
            Self::Handoff => "handoff",
            Self::ReviewRequest => "review_request",
            Self::ReviewResult => "review_result",
            Self::Informational => "informational",
        }
    }

    pub fn default_ttl_ms(self) -> Option<i64> {
        match self {
            Self::Question => Some(DEFAULT_QUESTION_TTL_MS),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkMessage {
    pub schema_version: u32,
    pub message_id: String,
    pub seq: u64,
    pub kind: MessageKind,
    pub from_actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_agent_id: Option<String>,
    pub session_id: Uuid,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl WorkMessage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: MessageKind,
        from_actor: impl Into<String>,
        from_agent_id: Option<String>,
        to_agent_id: Option<String>,
        session_id: Uuid,
        workspace: impl Into<String>,
        work_id: Option<String>,
        body: impl Into<String>,
        payload: Option<serde_json::Value>,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let payload = redact_message_payload(payload)?;
        let message = Self {
            schema_version: MESSAGE_SCHEMA_VERSION,
            message_id: Uuid::new_v4().to_string(),
            seq: 0,
            kind,
            from_actor: from_actor.into(),
            from_agent_id,
            to_agent_id,
            session_id,
            workspace: workspace.into(),
            work_id,
            attempt_id: None,
            run_id: None,
            reply_to_id: None,
            thread_id: None,
            body: body.into(),
            payload,
            expires_at: kind
                .default_ttl_ms()
                .map(|ms| now + chrono::Duration::milliseconds(ms)),
            acked_at: None,
            acked_by: None,
            created_at: now,
        };
        message.validate()?;
        Ok(message)
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != MESSAGE_SCHEMA_VERSION
            || self.seq == 0 && self.message_id.is_empty()
        {
            return Err(invalid("message schema is invalid"));
        }
        validate_text(&self.message_id, MAX_MESSAGE_ACTOR_BYTES, "message_id")?;
        validate_text(&self.from_actor, MAX_MESSAGE_ACTOR_BYTES, "from_actor")?;
        validate_text(&self.workspace, MAX_MESSAGE_ACTOR_BYTES * 4, "workspace")?;
        validate_text(&self.body, MAX_MESSAGE_BODY_BYTES, "body")?;
        if let Some(payload) = &self.payload {
            let encoded = serde_json::to_vec(payload).unwrap_or_default();
            if encoded.len() > MAX_MESSAGE_PAYLOAD_BYTES {
                return Err(invalid("message payload exceeds 8192 bytes"));
            }
        }
        Ok(())
    }

    pub fn expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires| expires < now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<WorkMessage>,
    pub next_seq: u64,
    pub retained_from_seq: u64,
}

pub fn redact_message_payload(
    payload: Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>, OrchError> {
    let Some(payload) = payload else {
        return Ok(None);
    };
    let encoded = serde_json::to_vec(&payload)
        .map_err(|error| OrchError::new(OrchErrorCode::InvalidRequest, error.to_string()))?;
    if encoded.len() > MAX_MESSAGE_PAYLOAD_BYTES {
        return Err(invalid("message payload exceeds 8192 bytes"));
    }
    Ok(Some(redact_value(&payload)))
}

#[allow(dead_code)]
pub fn message_idempotency_hash(message: &WorkMessage) -> String {
    hash_payload(&serde_json::json!({
        "kind": message.kind,
        "from": message.from_actor,
        "to": message.to_agent_id,
        "workId": message.work_id,
        "body": message.body,
        "payload": message.payload,
        "replyTo": message.reply_to_id,
    }))
}

fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "token"
                        | "password"
                        | "secret"
                        | "authorization"
                        | "api_key"
                        | "apikey"
                        | "bearer"
                ) {
                    out.insert(key.clone(), serde_json::Value::String("[redacted]".into()));
                } else {
                    out.insert(key.clone(), redact_value(child));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_value).collect())
        }
        other => other.clone(),
    }
}

fn validate_text(value: &str, max_bytes: usize, field: &str) -> Result<(), OrchError> {
    if value.is_empty() || value.len() > max_bytes || value.contains('\0') {
        return Err(invalid(format!("{field} is empty or exceeds its bound")));
    }
    Ok(())
}

/// Reserved seam for a later message-triggered routine adapter.
pub fn message_activation_unsupported() -> OrchError {
    OrchError::new(
        OrchErrorCode::Unsupported,
        "message-triggered activation uses RoutineTrigger::External { Message }; it is not enabled in this slice",
    )
}
