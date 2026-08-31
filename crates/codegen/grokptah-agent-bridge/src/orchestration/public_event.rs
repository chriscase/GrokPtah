//! Versioned public Build-event DTO seam.
//!
//! Allowlisted projection for public MCP `ptah_get_events` and the live event
//! page/replay it feeds. Private `JournalPage` / `SessionUpdate` stay on the
//! local host journal.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::event_bus::{JournalEntry, JournalPage};
use crate::events::SessionUpdate;

/// Explicit public-event document version. Unknown values fail closed.
pub const PUBLIC_EVENT_SCHEMA_VERSION: &str = "grokptah.public-event.v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicEventDtoError {
    #[error("unknown public-event schema version: {0}")]
    UnknownSchemaVersion(String),
    #[error("public-event dto decode failed: {0}")]
    Decode(String),
}

/// Safe event classification. Unknown wire values fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicEventKindV1 {
    AgentMessage,
    AgentThought,
    TurnStarted,
    ToolCall,
    ToolCallUpdate,
    Plan,
    PermissionRequired,
    TurnComplete,
    Completion,
    Error,
    SubagentSpawned,
    SubagentUpdate,
    BackgroundTask,
    ShellSessionStarted,
    ShellOutput,
    ShellSessionEnded,
    FileEdit,
    AgentProgress,
    RateLimited,
    SteeringInjected,
    PromptQueueChanged,
}

/// Safe seq/ts/kind plus allowlisted status counts. No text, path, command,
/// tool I/O, session, or workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicEventV1 {
    pub schema_version: String,
    pub seq: u64,
    pub ts: String,
    pub kind: PublicEventKindV1,
    #[serde(default)]
    pub tool_kind: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub step_count: Option<u32>,
    #[serde(default)]
    pub cancelled: Option<bool>,
    #[serde(default)]
    pub interrupted: Option<bool>,
    #[serde(default)]
    pub round: Option<u32>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
    #[serde(default)]
    pub revision: Option<u64>,
}

/// Allowlisted `ptah_get_events` page. Cursor expiry remains the 410 error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicEventPageV1 {
    pub schema_version: String,
    pub events: Vec<PublicEventV1>,
    pub next_cursor: Option<u64>,
}

impl PublicEventV1 {
    pub fn from_entry(entry: &JournalEntry) -> Self {
        Self::from_update(entry.seq, entry.ts.clone(), &entry.update)
    }

    pub fn from_update(seq: u64, ts: String, update: &SessionUpdate) -> Self {
        let mut dto = Self {
            schema_version: PUBLIC_EVENT_SCHEMA_VERSION.to_string(),
            seq,
            ts,
            kind: PublicEventKindV1::Error,
            tool_kind: None,
            status: None,
            step_count: None,
            cancelled: None,
            interrupted: None,
            round: None,
            max_rounds: None,
            retry_after_ms: None,
            revision: None,
        };
        match update {
            SessionUpdate::AgentMessageChunk { .. } => {
                dto.kind = PublicEventKindV1::AgentMessage;
            }
            SessionUpdate::AgentThoughtChunk { .. } => {
                dto.kind = PublicEventKindV1::AgentThought;
            }
            SessionUpdate::TurnStarted { .. } => {
                dto.kind = PublicEventKindV1::TurnStarted;
            }
            SessionUpdate::ToolCall { kind, status, .. } => {
                dto.kind = PublicEventKindV1::ToolCall;
                dto.tool_kind = enum_snake(kind);
                dto.status = enum_snake(status);
            }
            SessionUpdate::ToolCallUpdate { status, .. } => {
                dto.kind = PublicEventKindV1::ToolCallUpdate;
                dto.status = enum_snake(status);
            }
            SessionUpdate::Plan { steps, .. } => {
                dto.kind = PublicEventKindV1::Plan;
                dto.step_count = Some(steps.len() as u32);
            }
            SessionUpdate::PermissionRequired { .. } => {
                dto.kind = PublicEventKindV1::PermissionRequired;
            }
            SessionUpdate::TurnComplete { cancelled, .. } => {
                dto.kind = PublicEventKindV1::TurnComplete;
                dto.cancelled = Some(*cancelled);
            }
            SessionUpdate::CompletionEvidence { evidence, .. } => {
                dto.kind = PublicEventKindV1::Completion;
                dto.status = Some(evidence.status.clone());
                dto.interrupted = Some(evidence.interrupted);
            }
            SessionUpdate::Error { .. } => {
                dto.kind = PublicEventKindV1::Error;
            }
            SessionUpdate::SubagentSpawned { kind, .. } => {
                dto.kind = PublicEventKindV1::SubagentSpawned;
                dto.tool_kind = Some(kind.clone());
            }
            SessionUpdate::SubagentUpdate { status, .. } => {
                dto.kind = PublicEventKindV1::SubagentUpdate;
                dto.status = Some(status.clone());
            }
            SessionUpdate::BackgroundTask { status, .. } => {
                dto.kind = PublicEventKindV1::BackgroundTask;
                dto.status = Some(status.clone());
            }
            SessionUpdate::ShellSessionStarted { .. } => {
                dto.kind = PublicEventKindV1::ShellSessionStarted;
            }
            SessionUpdate::ShellOutput { .. } => {
                dto.kind = PublicEventKindV1::ShellOutput;
            }
            SessionUpdate::ShellSessionEnded { cancelled, .. } => {
                dto.kind = PublicEventKindV1::ShellSessionEnded;
                dto.cancelled = Some(*cancelled);
            }
            SessionUpdate::FileEdit { .. } => {
                dto.kind = PublicEventKindV1::FileEdit;
            }
            SessionUpdate::AgentProgress {
                round, max_rounds, ..
            } => {
                dto.kind = PublicEventKindV1::AgentProgress;
                dto.round = Some(*round);
                dto.max_rounds = Some(*max_rounds);
            }
            SessionUpdate::RateLimited { retry_after_ms, .. } => {
                dto.kind = PublicEventKindV1::RateLimited;
                dto.retry_after_ms = *retry_after_ms;
            }
            SessionUpdate::SteeringInjected { .. } => {
                dto.kind = PublicEventKindV1::SteeringInjected;
            }
            SessionUpdate::PromptQueueChanged { revision, .. } => {
                dto.kind = PublicEventKindV1::PromptQueueChanged;
                dto.revision = Some(*revision);
            }
        }
        dto
    }
}

impl PublicEventPageV1 {
    pub fn from_page(page: &JournalPage) -> Self {
        Self {
            schema_version: PUBLIC_EVENT_SCHEMA_VERSION.to_string(),
            events: page.entries.iter().map(PublicEventV1::from_entry).collect(),
            next_cursor: page.next_cursor,
        }
    }
}

pub fn parse_public_event_v1(value: &Value) -> Result<PublicEventV1, PublicEventDtoError> {
    parse_versioned(value, |row: &PublicEventV1| row.schema_version.as_str())
}

pub fn parse_public_event_page_v1(value: &Value) -> Result<PublicEventPageV1, PublicEventDtoError> {
    let parsed = parse_versioned(value, |row: &PublicEventPageV1| row.schema_version.as_str())?;
    for event in &parsed.events {
        require_known_version(&event.schema_version)?;
    }
    Ok(parsed)
}

fn parse_versioned<T, F>(value: &Value, version: F) -> Result<T, PublicEventDtoError>
where
    T: DeserializeOwned,
    F: FnOnce(&T) -> &str,
{
    let parsed: T = serde_json::from_value(value.clone())
        .map_err(|err| PublicEventDtoError::Decode(err.to_string()))?;
    require_known_version(version(&parsed))?;
    Ok(parsed)
}

fn require_known_version(version: &str) -> Result<(), PublicEventDtoError> {
    if version != PUBLIC_EVENT_SCHEMA_VERSION {
        return Err(PublicEventDtoError::UnknownSchemaVersion(
            version.to_string(),
        ));
    }
    Ok(())
}

fn enum_snake<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ToolCallKind, ToolCallStatus};
    use crate::prompt_queue::PromptQueueEntry;
    use serde_json::json;
    use std::collections::BTreeSet;
    use uuid::Uuid;

    const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
    const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
    const SECRET_CWD: &str = "/tmp/secret-chat";
    const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
    const SECRET_DETAIL: &str = "editing /tmp/secret-chat/credentials.env";
    const SECRET_OUTPUT: &str = "AKIASECRETTOKEN";
    const SECRET_DIFF: &str = "--- a/tmp/secret-chat/credentials.env";
    const TS: &str = "2026-08-01T00:00:01Z";

    fn session() -> Uuid {
        Uuid::nil()
    }

    fn secret_entries() -> Vec<JournalEntry> {
        vec![
            JournalEntry {
                seq: 1,
                ts: TS.into(),
                update: SessionUpdate::AgentMessageChunk {
                    session_id: session(),
                    text: SECRET_PROMPT.into(),
                },
            },
            JournalEntry {
                seq: 2,
                ts: TS.into(),
                update: SessionUpdate::ToolCall {
                    session_id: session(),
                    call_id: "c1".into(),
                    title: SECRET_TOOL.into(),
                    kind: ToolCallKind::Execute,
                    status: ToolCallStatus::Running,
                    input: json!({ "command": SECRET_TOOL, "cwd": SECRET_CWD }),
                },
            },
            JournalEntry {
                seq: 3,
                ts: TS.into(),
                update: SessionUpdate::FileEdit {
                    session_id: session(),
                    path: SECRET_PATH.into(),
                    summary: SECRET_DETAIL.into(),
                    unified_diff: SECRET_DIFF.into(),
                },
            },
            JournalEntry {
                seq: 4,
                ts: TS.into(),
                update: SessionUpdate::ShellOutput {
                    session_id: session(),
                    call_id: "c1".into(),
                    data: SECRET_OUTPUT.into(),
                },
            },
            JournalEntry {
                seq: 5,
                ts: TS.into(),
                update: SessionUpdate::AgentProgress {
                    session_id: session(),
                    round: 3,
                    max_rounds: 4,
                    last_tool: Some(SECRET_TOOL.into()),
                    detail: SECRET_DETAIL.into(),
                },
            },
            JournalEntry {
                seq: 6,
                ts: TS.into(),
                update: SessionUpdate::PromptQueueChanged {
                    session_id: session(),
                    revision: 9,
                    entries: vec![PromptQueueEntry::new(SECRET_PROMPT, "mcp", false).unwrap()],
                    action: "enqueue".into(),
                    origin: "mcp".into(),
                    changed_entry: None,
                    disposition: None,
                },
            },
            JournalEntry {
                seq: 7,
                ts: TS.into(),
                update: SessionUpdate::TurnComplete {
                    session_id: session(),
                    cancelled: false,
                },
            },
        ]
    }

    fn secret_page() -> JournalPage {
        let entries = secret_entries();
        let next_cursor = entries.last().map(|entry| entry.seq);
        JournalPage {
            entries,
            next_cursor,
            cursor_expired: false,
        }
    }

    const ALLOWED_KEYS: &[&str] = &[
        "schemaVersion",
        "events",
        "nextCursor",
        "seq",
        "ts",
        "kind",
        "toolKind",
        "status",
        "stepCount",
        "cancelled",
        "interrupted",
        "round",
        "maxRounds",
        "retryAfterMs",
        "revision",
    ];

    fn collect_keys(value: &Value, keys: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    keys.insert(key.clone());
                    collect_keys(child, keys);
                }
            }
            Value::Array(items) => {
                for child in items {
                    collect_keys(child, keys);
                }
            }
            _ => {}
        }
    }

    fn assert_no_secrets(value: &Value) {
        let blob = value.to_string();
        for needle in [
            SECRET_PROMPT,
            SECRET_PATH,
            SECRET_CWD,
            SECRET_TOOL,
            SECRET_DETAIL,
            SECRET_OUTPUT,
            SECRET_DIFF,
        ] {
            assert!(
                !blob.contains(needle),
                "public event dto leaked {needle:?}: {blob}"
            );
        }
        let mut keys = BTreeSet::new();
        collect_keys(value, &mut keys);
        for key in &keys {
            assert!(
                ALLOWED_KEYS.contains(&key.as_str()),
                "public event dto leaked unexpected key {key:?}: {blob}"
            );
        }
        assert!(
            blob.contains(PUBLIC_EVENT_SCHEMA_VERSION),
            "schema version must be present"
        );
    }

    #[test]
    fn from_page_keeps_seq_order_kind_and_terminal_flags() {
        let page = PublicEventPageV1::from_page(&secret_page());
        assert_eq!(page.schema_version, PUBLIC_EVENT_SCHEMA_VERSION);
        assert_eq!(page.next_cursor, Some(7));
        assert_eq!(page.events.len(), 7);
        assert_eq!(page.events[0].seq, 1);
        assert_eq!(page.events[0].kind, PublicEventKindV1::AgentMessage);
        assert_eq!(page.events[1].kind, PublicEventKindV1::ToolCall);
        assert_eq!(page.events[1].tool_kind.as_deref(), Some("execute"));
        assert_eq!(page.events[1].status.as_deref(), Some("running"));
        assert_eq!(page.events[2].kind, PublicEventKindV1::FileEdit);
        assert_eq!(page.events[3].kind, PublicEventKindV1::ShellOutput);
        assert_eq!(page.events[4].kind, PublicEventKindV1::AgentProgress);
        assert_eq!(page.events[4].round, Some(3));
        assert_eq!(page.events[4].max_rounds, Some(4));
        assert_eq!(page.events[5].kind, PublicEventKindV1::PromptQueueChanged);
        assert_eq!(page.events[5].revision, Some(9));
        assert_eq!(page.events[6].kind, PublicEventKindV1::TurnComplete);
        assert_eq!(page.events[6].cancelled, Some(false));
        for event in &page.events {
            assert_eq!(event.schema_version, PUBLIC_EVENT_SCHEMA_VERSION);
        }
    }

    #[test]
    fn projection_is_allowlisted() {
        let value = serde_json::to_value(PublicEventPageV1::from_page(&secret_page())).unwrap();
        assert_no_secrets(&value);
        assert_eq!(value["events"].as_array().map(Vec::len), Some(7));
        assert_eq!(value["nextCursor"], json!(7));
        assert_eq!(value["events"][0]["seq"], json!(1));
        assert_eq!(value["events"][6]["kind"], json!("turn_complete"));
    }

    #[test]
    fn parse_round_trip_accepts_only_v1() {
        let page = PublicEventPageV1::from_page(&secret_page());
        let page_json = serde_json::to_value(&page).unwrap();
        let event_json = serde_json::to_value(&page.events[0]).unwrap();
        assert_eq!(parse_public_event_page_v1(&page_json).unwrap(), page);
        assert_eq!(
            parse_public_event_v1(&event_json).unwrap(),
            page.events[0].clone()
        );
    }

    #[test]
    fn unknown_schema_version_is_denied() {
        let mut value =
            serde_json::to_value(PublicEventV1::from_entry(&secret_entries()[0])).unwrap();
        value["schemaVersion"] = json!("grokptah.public-event.v2");
        match parse_public_event_v1(&value) {
            Err(PublicEventDtoError::UnknownSchemaVersion(version)) => {
                assert_eq!(version, "grokptah.public-event.v2");
            }
            other => panic!("expected unknown version, got {other:?}"),
        }
    }

    #[test]
    fn nested_page_item_unknown_version_is_denied() {
        let mut value = serde_json::to_value(PublicEventPageV1::from_page(&secret_page())).unwrap();
        value["events"][0]["schemaVersion"] = json!("grokptah.public-event.v0");
        match parse_public_event_page_v1(&value) {
            Err(PublicEventDtoError::UnknownSchemaVersion(version)) => {
                assert_eq!(version, "grokptah.public-event.v0");
            }
            other => panic!("expected unknown nested version, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_denied() {
        let mut value =
            serde_json::to_value(PublicEventV1::from_entry(&secret_entries()[0])).unwrap();
        value["text"] = json!(SECRET_PROMPT);
        match parse_public_event_v1(&value) {
            Err(PublicEventDtoError::Decode(message)) => {
                assert!(
                    message.contains("unknown field"),
                    "expected unknown field, got {message}"
                );
            }
            other => panic!("expected decode error, got {other:?}"),
        }
    }

    #[test]
    fn missing_schema_version_is_denied() {
        let mut value =
            serde_json::to_value(PublicEventV1::from_entry(&secret_entries()[0])).unwrap();
        value.as_object_mut().unwrap().remove("schemaVersion");
        assert!(matches!(
            parse_public_event_v1(&value),
            Err(PublicEventDtoError::Decode(_))
        ));
    }

    #[test]
    fn current_journal_page_wire_is_not_a_public_dto() {
        let wire = serde_json::to_value(secret_page()).unwrap();
        assert!(
            matches!(
                parse_public_event_page_v1(&wire),
                Err(PublicEventDtoError::Decode(_))
            ),
            "current JournalPage JSON must not parse as PublicEventPageV1"
        );
        assert!(matches!(
            parse_public_event_v1(&wire["entries"][0]),
            Err(PublicEventDtoError::Decode(_))
        ));
    }
}
