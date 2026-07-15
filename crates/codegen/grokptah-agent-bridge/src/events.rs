use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::permission::PermissionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Read,
    Edit,
    Search,
    Execute,
    Think,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Denied,
}

/// Streamed session updates delivered to the UI (Tauri events).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionUpdate {
    AgentMessageChunk {
        session_id: Uuid,
        text: String,
    },
    AgentThoughtChunk {
        session_id: Uuid,
        text: String,
    },
    ToolCall {
        session_id: Uuid,
        call_id: String,
        title: String,
        kind: ToolCallKind,
        status: ToolCallStatus,
        input: serde_json::Value,
    },
    ToolCallUpdate {
        session_id: Uuid,
        call_id: String,
        status: ToolCallStatus,
        output: Option<String>,
    },
    Plan {
        session_id: Uuid,
        steps: Vec<String>,
        status: String,
    },
    PermissionRequired {
        session_id: Uuid,
        request: PermissionRequest,
    },
    TurnComplete {
        session_id: Uuid,
        cancelled: bool,
    },
    Error {
        session_id: Uuid,
        message: String,
    },
    SubagentSpawned {
        session_id: Uuid,
        subagent_id: String,
        kind: String,
        title: String,
    },
    SubagentUpdate {
        session_id: Uuid,
        subagent_id: String,
        status: String,
        detail: Option<String>,
    },
    BackgroundTask {
        session_id: Option<Uuid>,
        task_id: String,
        title: String,
        status: String,
    },
}
