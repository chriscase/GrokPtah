//! Additive desktop consumer for `grokptah.public-event.v1`.
//!
//! Parses remote `ptah_get_events` bodies with `grokptah-agent-sdk` so this
//! crate does not duplicate the allowlist. Unknown versions, unknown fields,
//! and legacy `JournalPage` fail closed as a redacted decode error (no serde
//! payload, field names, or secret values).
//!
//! `session_id` and `workspace` are stamped from the MCP request. They are
//! never deserialized from the remote document. Raw `get_events` stays on
//! `JournalPage` and is unsupported for the public wire.

use anyhow::{anyhow, Result};
use grokptah_agent_sdk::{parse_public_event_page_v1, PublicEventV1};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Request-stamped public-event page returned by additive Tauri commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePublicEventPage {
    pub session_id: Uuid,
    pub workspace: String,
    pub schema_version: String,
    pub events: Vec<PublicEventV1>,
    pub next_cursor: Option<u64>,
}

fn redact_public_event_error() -> anyhow::Error {
    anyhow!("remote public-event decode failed")
}

/// Parse one `ptah_get_events` `grokptah.public-event.v1` page and stamp scope
/// from the request, never from `body`.
pub fn parse_remote_public_event_page(
    body: &Value,
    session_id: Uuid,
    workspace: &str,
) -> Result<RemotePublicEventPage> {
    let parsed = parse_public_event_page_v1(body).map_err(|_| redact_public_event_error())?;
    Ok(RemotePublicEventPage {
        session_id,
        workspace: workspace.to_string(),
        schema_version: parsed.schema_version,
        events: parsed.events,
        next_cursor: parsed.next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use grokptah_agent_bridge::JournalPage;
    use grokptah_agent_sdk::{PublicEventKindV1, PublicEventPageV1, PublicEventV1, PUBLIC_EVENT_SCHEMA_VERSION};
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{parse_remote_public_event_page, RemotePublicEventPage};

    const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
    const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
    const SECRET_CWD: &str = "/tmp/secret-chat";
    const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
    const TS: &str = "2026-08-01T00:00:01Z";
    const REQUEST_SESSION: &str = "11111111-1111-4111-8111-111111111111";
    const REQUEST_WORKSPACE: &str = "/tmp/project";

    const PRIVATE_KEYS: &[&str] = &[
        "update",
        "entries",
        "text",
        "path",
        "command",
        "input",
        "output",
        "unifiedDiff",
        "sessionId",
        "workspace",
        "runId",
        "cursorExpired",
    ];

    fn request_session() -> Uuid {
        Uuid::parse_str(REQUEST_SESSION).unwrap()
    }

    fn public_event() -> PublicEventV1 {
        PublicEventV1 {
            schema_version: PUBLIC_EVENT_SCHEMA_VERSION.to_string(),
            seq: 4,
            ts: TS.into(),
            kind: PublicEventKindV1::TurnComplete,
            tool_kind: None,
            status: None,
            step_count: None,
            cancelled: Some(false),
            interrupted: None,
            round: None,
            max_rounds: None,
            retry_after_ms: None,
            revision: None,
        }
    }

    fn public_page() -> PublicEventPageV1 {
        PublicEventPageV1 {
            schema_version: PUBLIC_EVENT_SCHEMA_VERSION.to_string(),
            events: vec![public_event()],
            next_cursor: Some(4),
        }
    }

    fn journal_page_wire() -> Value {
        json!({
            "entries": [{
                "seq": 4,
                "ts": TS,
                "update": {
                    "type": "agent_message_chunk",
                    "session_id": REQUEST_SESSION,
                    "text": SECRET_PROMPT
                }
            }],
            "nextCursor": 4,
            "cursorExpired": false
        })
    }

    fn error_blob(error: &anyhow::Error) -> String {
        format!("{error:#} {error:?}")
    }

    fn assert_redacted(error: anyhow::Error) {
        let blob = error_blob(&error);
        assert!(
            blob.contains("remote public-event decode failed"),
            "expected redacted public-event error, got {blob}"
        );
        for needle in [
            SECRET_PROMPT,
            SECRET_PATH,
            SECRET_CWD,
            SECRET_TOOL,
            REQUEST_SESSION,
            REQUEST_WORKSPACE,
            "unknown field",
            "grokptah.public-event.v2",
            "grokptah.public-event.v0",
        ] {
            assert!(
                !blob.contains(needle),
                "public-event error leaked {needle:?}: {blob}"
            );
        }
    }

    fn parse_page(body: &Value) -> Result<RemotePublicEventPage, anyhow::Error> {
        parse_remote_public_event_page(body, request_session(), REQUEST_WORKSPACE)
    }

    #[test]
    fn page_stamps_request_scope_not_body() {
        let got = parse_page(&serde_json::to_value(public_page()).unwrap()).unwrap();
        assert_eq!(got.session_id, request_session());
        assert_eq!(got.workspace, REQUEST_WORKSPACE);
        assert_eq!(got.schema_version, PUBLIC_EVENT_SCHEMA_VERSION);
        assert_eq!(got.events, vec![public_event()]);
        assert_eq!(got.next_cursor, Some(4));

        let encoded = serde_json::to_value(&got).unwrap();
        assert_eq!(encoded["sessionId"], json!(REQUEST_SESSION));
        assert_eq!(encoded["workspace"], json!(REQUEST_WORKSPACE));
        assert_eq!(encoded["schemaVersion"], json!(PUBLIC_EVENT_SCHEMA_VERSION));
        assert_eq!(encoded["events"][0]["kind"], json!("turn_complete"));
        for key in ["update", "entries", "cursorExpired", "text", "path"] {
            assert!(encoded.get(key).is_none(), "stamped page leaked {key}");
        }
    }

    #[test]
    fn page_rejects_legacy_journal_page_without_secrets() {
        let wire = journal_page_wire();
        assert_redacted(parse_page(&wire).unwrap_err());
        let raw: JournalPage = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(raw.entries.len(), 1);
        let public = serde_json::to_value(public_page()).unwrap();
        assert!(
            serde_json::from_value::<JournalPage>(public.clone()).is_err(),
            "public-event.v1 must not decode as JournalPage"
        );
        assert!(parse_page(&public).is_ok());
    }

    #[test]
    fn page_rejects_unknown_fields_without_secrets() {
        for key in PRIVATE_KEYS {
            let mut row = serde_json::to_value(public_page()).unwrap();
            row[*key] = json!(SECRET_PROMPT);
            assert_redacted(parse_page(&row).unwrap_err());
        }
    }

    #[test]
    fn page_rejects_unknown_versions_without_secrets() {
        let mut page = serde_json::to_value(public_page()).unwrap();
        page["schemaVersion"] = json!("grokptah.public-event.v2");
        page["text"] = json!(SECRET_PROMPT);
        assert_redacted(parse_page(&page).unwrap_err());

        let mut nested = serde_json::to_value(public_page()).unwrap();
        nested["events"][0]["schemaVersion"] = json!("grokptah.public-event.v0");
        nested["events"][0]["path"] = json!(SECRET_PATH);
        assert_redacted(parse_page(&nested).unwrap_err());
    }
}
