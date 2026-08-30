//! Versioned public Build-run DTO seam.
//!
//! Allowlisted projection for a later coordinated switch of `ptah_list_runs`,
//! `ptah_get_run`, `ptah_get_progress`, and `ptah_get_handoff`. Current MCP
//! dispatch still serializes full `RunRecord` / ad-hoc JSON. Do not adopt this
//! type on any one of those four tools until the others and their consumers
//! switch in the same change. Consumer inventory and staged order:
//! `docs/PUBLIC_RUN_WIRE_MIGRATION.md`.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::types::{RunRecord, RunState};

/// Explicit public-run document version. Unknown values fail closed.
pub const PUBLIC_RUN_SCHEMA_VERSION: &str = "grokptah.public-run.v1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicRunDtoError {
    #[error("unknown public-run schema version: {0}")]
    UnknownSchemaVersion(String),
    #[error("public-run dto decode failed: {0}")]
    Decode(String),
}

/// Safe status, timestamps, counts, and opaque run id. No prompt, path, body,
/// or workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: RunState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub queue_position: Option<u64>,
    pub event_start_seq: Option<u64>,
    pub event_end_seq: Option<u64>,
    pub change_count: u64,
    pub test_count: u64,
    pub permission_requested_count: u64,
    pub permission_granted_count: u64,
    pub permission_denied_count: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
    pub usage_request_count: u64,
    pub usage_complete: bool,
    pub usage_pending_request_count: u64,
    pub progress_round: Option<u32>,
    pub progress_max_rounds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunListV1 {
    pub schema_version: String,
    pub runs: Vec<PublicRunV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunProgressV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: RunState,
    pub busy: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub queue_position: Option<u64>,
    pub event_start_seq: Option<u64>,
    pub event_end_seq: Option<u64>,
    pub progress_round: Option<u32>,
    pub progress_max_rounds: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicRunHandoffV1 {
    pub schema_version: String,
    pub run_id: String,
    pub state: RunState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub event_start_seq: Option<u64>,
    pub event_end_seq: Option<u64>,
    pub change_count: u64,
    pub test_count: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
    pub usage_request_count: u64,
    pub usage_complete: bool,
    pub usage_pending_request_count: u64,
}

impl PublicRunV1 {
    pub fn from_run(run: &RunRecord) -> Self {
        Self {
            schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
            run_id: run.run_id.clone(),
            state: run.state,
            created_at: run.created_at,
            updated_at: run.updated_at,
            queue_position: run.queue_position.map(|n| n as u64),
            event_start_seq: run.start_seq,
            event_end_seq: run.end_seq,
            change_count: run.aggregates.changes.len() as u64,
            test_count: run.aggregates.tests.len() as u64,
            permission_requested_count: u64::from(run.aggregates.permissions_requested),
            permission_granted_count: u64::from(run.aggregates.permissions_granted),
            permission_denied_count: u64::from(run.aggregates.permissions_denied),
            usage_prompt_tokens: run.aggregates.usage.prompt_tokens,
            usage_completion_tokens: run.aggregates.usage.completion_tokens,
            usage_total_tokens: run.aggregates.usage.total_tokens,
            usage_request_count: run.aggregates.usage.requests,
            usage_complete: run.aggregates.usage_complete,
            usage_pending_request_count: u64::from(run.aggregates.usage_pending_requests),
            progress_round: run.progress.as_ref().map(|progress| progress.round),
            progress_max_rounds: run.progress.as_ref().map(|progress| progress.max_rounds),
        }
    }
}

impl PublicRunListV1 {
    pub fn from_runs(runs: &[RunRecord]) -> Self {
        Self {
            schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
            runs: runs.iter().map(PublicRunV1::from_run).collect(),
        }
    }
}

impl PublicRunProgressV1 {
    pub fn from_run(run: &RunRecord, busy: bool) -> Self {
        Self {
            schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
            run_id: run.run_id.clone(),
            state: run.state,
            busy,
            created_at: run.created_at,
            updated_at: run.updated_at,
            queue_position: run.queue_position.map(|n| n as u64),
            event_start_seq: run.start_seq,
            event_end_seq: run.end_seq,
            progress_round: run.progress.as_ref().map(|progress| progress.round),
            progress_max_rounds: run.progress.as_ref().map(|progress| progress.max_rounds),
        }
    }
}

impl PublicRunHandoffV1 {
    pub fn from_run(run: &RunRecord) -> Self {
        Self {
            schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
            run_id: run.run_id.clone(),
            state: run.state,
            created_at: run.created_at,
            updated_at: run.updated_at,
            event_start_seq: run.start_seq,
            event_end_seq: run.end_seq,
            change_count: run.aggregates.changes.len() as u64,
            test_count: run.aggregates.tests.len() as u64,
            usage_prompt_tokens: run.aggregates.usage.prompt_tokens,
            usage_completion_tokens: run.aggregates.usage.completion_tokens,
            usage_total_tokens: run.aggregates.usage.total_tokens,
            usage_request_count: run.aggregates.usage.requests,
            usage_complete: run.aggregates.usage_complete,
            usage_pending_request_count: u64::from(run.aggregates.usage_pending_requests),
        }
    }
}

pub fn parse_public_run_v1(value: &Value) -> Result<PublicRunV1, PublicRunDtoError> {
    parse_versioned(value, |row: &PublicRunV1| row.schema_version.as_str())
}

pub fn parse_public_run_list_v1(value: &Value) -> Result<PublicRunListV1, PublicRunDtoError> {
    let parsed = parse_versioned(value, |row: &PublicRunListV1| row.schema_version.as_str())?;
    for run in &parsed.runs {
        require_known_version(&run.schema_version)?;
    }
    Ok(parsed)
}

pub fn parse_public_run_progress_v1(
    value: &Value,
) -> Result<PublicRunProgressV1, PublicRunDtoError> {
    parse_versioned(value, |row: &PublicRunProgressV1| {
        row.schema_version.as_str()
    })
}

pub fn parse_public_run_handoff_v1(value: &Value) -> Result<PublicRunHandoffV1, PublicRunDtoError> {
    parse_versioned(value, |row: &PublicRunHandoffV1| {
        row.schema_version.as_str()
    })
}

fn parse_versioned<T, F>(value: &Value, version: F) -> Result<T, PublicRunDtoError>
where
    T: DeserializeOwned,
    F: FnOnce(&T) -> &str,
{
    let parsed: T = serde_json::from_value(value.clone())
        .map_err(|err| PublicRunDtoError::Decode(err.to_string()))?;
    require_known_version(version(&parsed))?;
    Ok(parsed)
}

fn require_known_version(version: &str) -> Result<(), PublicRunDtoError> {
    if version != PUBLIC_RUN_SCHEMA_VERSION {
        return Err(PublicRunDtoError::UnknownSchemaVersion(version.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::types::{
        ChangeRecord, PromotionState, RunAggregates, RunApproval, RunBounds, RunExecution,
        RunExecutionMode, RunProgress, RunPurpose, TestObservation,
    };
    use super::*;
    use crate::completion::CompletionUsage;
    use serde_json::json;
    use uuid::Uuid;

    const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
    const SECRET_RESPONSE: &str = "wrote tokens to /tmp/secret-chat/credentials.env";
    const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
    const SECRET_CWD: &str = "/tmp/secret-chat";
    const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
    const SECRET_DETAIL: &str = "editing /tmp/secret-chat/credentials.env";
    const SECRET_HASH: &str = "sha256:secret-continuation";
    const OPAQUE_RUN: &str = "run_public_dto_1";

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn secret_run() -> RunRecord {
        let session = Uuid::nil();
        RunRecord {
            run_id: OPAQUE_RUN.into(),
            session_id: session,
            workspace: SECRET_CWD.into(),
            request_id: "request-secret".into(),
            client_id: Some("mcp".into()),
            state: RunState::Completed,
            purpose: RunPurpose::Execution,
            agent_id: Some("agent-secret".into()),
            retry_of: Some("run-secret-parent".into()),
            parent_run_id: Some("run-secret-lineage".into()),
            agent_spec_revision: Some(9),
            checkpoint_id: Some("checkpoint-secret".into()),
            continuation_context_id: Some("ctx-secret".into()),
            continuation_context_hash: Some(SECRET_HASH.into()),
            continuation_fidelity: Some("degraded".into()),
            queue_position: Some(2),
            bounds: RunBounds {
                max_prompt_bytes: 1000,
                max_rounds: 4,
                max_duration_ms: 1000,
                max_total_tokens: Some(250),
            },
            prompt_preview: SECRET_PROMPT.into(),
            start_seq: Some(3),
            end_seq: Some(8),
            created_at: ts(),
            updated_at: ts(),
            terminal_result: Some("completed".into()),
            final_response: Some(SECRET_RESPONSE.into()),
            error_code: Some("secret_error".into()),
            stop_cause: None,
            aggregates: RunAggregates {
                changes: vec![ChangeRecord {
                    path: SECRET_PATH.into(),
                    summary: "leaked".into(),
                }],
                tests: vec![TestObservation {
                    call_id: "t1".into(),
                    command: Some(SECRET_TOOL.into()),
                    status: "passed".into(),
                    exit_code: Some(0),
                    cancelled: Some(false),
                }],
                permissions_requested: 2,
                permissions_granted: 1,
                permissions_denied: 1,
                usage: CompletionUsage {
                    prompt_tokens: 10,
                    completion_tokens: 4,
                    total_tokens: 14,
                    requests: 1,
                },
                usage_complete: true,
                usage_pending_requests: 0,
                verification: None,
            },
            progress: Some(RunProgress {
                round: 3,
                max_rounds: 4,
                last_tool: Some(SECRET_TOOL.into()),
                detail: SECRET_DETAIL.into(),
                updated_at: ts(),
            }),
            execution: Some(RunExecution {
                mode: RunExecutionMode::IsolatedWorktree,
                source_workspace: SECRET_CWD.into(),
                execution_workspace: SECRET_CWD.into(),
                base_revision: "abc".into(),
                source_fingerprint: "def".into(),
                final_fingerprint: Some("ghi".into()),
                promotion_state: PromotionState::Ready,
                promoted_at: None,
            }),
            approval: Some(RunApproval {
                approval_id: "appr-secret".into(),
                run_id: OPAQUE_RUN.into(),
                session_id: session,
                workspace: SECRET_CWD.into(),
                source_fingerprint: "def".into(),
                final_fingerprint: "ghi".into(),
                changed_files: vec![ChangeRecord {
                    path: SECRET_PATH.into(),
                    summary: "leaked".into(),
                }],
                issued_at: ts(),
                expires_at: ts(),
            }),
        }
    }

    fn assert_no_secrets(value: &Value) {
        let blob = value.to_string();
        for needle in [
            SECRET_PROMPT,
            SECRET_RESPONSE,
            SECRET_PATH,
            SECRET_CWD,
            SECRET_TOOL,
            SECRET_DETAIL,
            SECRET_HASH,
            "promptPreview",
            "finalResponse",
            "terminalResult",
            "errorCode",
            "stopCause",
            "requestId",
            "clientId",
            "workspace",
            "agentId",
            "retryOf",
            "parentRunId",
            "checkpointId",
            "continuationContextId",
            "continuationContextHash",
            "continuationFidelity",
            "sourceWorkspace",
            "executionWorkspace",
            "changedFiles",
            "lastTool",
            "bounds",
            "aggregates",
            "execution",
            "approval",
            "sessionId",
        ] {
            assert!(
                !blob.contains(needle),
                "public run dto leaked {needle:?}: {blob}"
            );
        }
        assert!(blob.contains(OPAQUE_RUN), "opaque run id must be retained");
        assert!(
            blob.contains(PUBLIC_RUN_SCHEMA_VERSION),
            "schema version must be present"
        );
    }

    #[test]
    fn from_run_keeps_status_timestamps_counts_and_opaque_id() {
        let run = secret_run();
        let dto = PublicRunV1::from_run(&run);
        assert_eq!(dto.schema_version, PUBLIC_RUN_SCHEMA_VERSION);
        assert_eq!(dto.run_id, OPAQUE_RUN);
        assert_eq!(dto.state, RunState::Completed);
        assert_eq!(dto.created_at, ts());
        assert_eq!(dto.updated_at, ts());
        assert_eq!(dto.queue_position, Some(2));
        assert_eq!(dto.event_start_seq, Some(3));
        assert_eq!(dto.event_end_seq, Some(8));
        assert_eq!(dto.change_count, 1);
        assert_eq!(dto.test_count, 1);
        assert_eq!(dto.permission_requested_count, 2);
        assert_eq!(dto.permission_granted_count, 1);
        assert_eq!(dto.permission_denied_count, 1);
        assert_eq!(dto.usage_prompt_tokens, 10);
        assert_eq!(dto.usage_completion_tokens, 4);
        assert_eq!(dto.usage_total_tokens, 14);
        assert_eq!(dto.usage_request_count, 1);
        assert!(dto.usage_complete);
        assert_eq!(dto.usage_pending_request_count, 0);
        assert_eq!(dto.progress_round, Some(3));
        assert_eq!(dto.progress_max_rounds, Some(4));
    }

    #[test]
    fn list_progress_and_handoff_projections_are_allowlisted() {
        let run = secret_run();
        let list =
            serde_json::to_value(PublicRunListV1::from_runs(std::slice::from_ref(&run))).unwrap();
        let progress = serde_json::to_value(PublicRunProgressV1::from_run(&run, true)).unwrap();
        let handoff = serde_json::to_value(PublicRunHandoffV1::from_run(&run)).unwrap();
        assert_no_secrets(&list);
        assert_no_secrets(&progress);
        assert_no_secrets(&handoff);
        assert_eq!(progress["busy"], json!(true));
        assert_eq!(list["runs"].as_array().map(Vec::len), Some(1));
        assert_eq!(handoff["changeCount"], json!(1));
        assert_eq!(handoff["testCount"], json!(1));
    }

    #[test]
    fn parse_round_trip_accepts_only_v1() {
        let run = secret_run();
        let run_json = serde_json::to_value(PublicRunV1::from_run(&run)).unwrap();
        let list_json =
            serde_json::to_value(PublicRunListV1::from_runs(std::slice::from_ref(&run))).unwrap();
        let progress_json =
            serde_json::to_value(PublicRunProgressV1::from_run(&run, false)).unwrap();
        let handoff_json = serde_json::to_value(PublicRunHandoffV1::from_run(&run)).unwrap();
        assert_eq!(
            parse_public_run_v1(&run_json).unwrap(),
            PublicRunV1::from_run(&run)
        );
        assert_eq!(
            parse_public_run_list_v1(&list_json).unwrap(),
            PublicRunListV1::from_runs(std::slice::from_ref(&run))
        );
        assert_eq!(
            parse_public_run_progress_v1(&progress_json).unwrap(),
            PublicRunProgressV1::from_run(&run, false)
        );
        assert_eq!(
            parse_public_run_handoff_v1(&handoff_json).unwrap(),
            PublicRunHandoffV1::from_run(&run)
        );
    }

    #[test]
    fn unknown_schema_version_is_denied() {
        let mut value = serde_json::to_value(PublicRunV1::from_run(&secret_run())).unwrap();
        value["schemaVersion"] = json!("grokptah.public-run.v2");
        match parse_public_run_v1(&value) {
            Err(PublicRunDtoError::UnknownSchemaVersion(version)) => {
                assert_eq!(version, "grokptah.public-run.v2");
            }
            other => panic!("expected unknown version, got {other:?}"),
        }
    }

    #[test]
    fn nested_list_item_unknown_version_is_denied() {
        let mut value = serde_json::to_value(PublicRunListV1::from_runs(&[secret_run()])).unwrap();
        value["runs"][0]["schemaVersion"] = json!("grokptah.public-run.v0");
        match parse_public_run_list_v1(&value) {
            Err(PublicRunDtoError::UnknownSchemaVersion(version)) => {
                assert_eq!(version, "grokptah.public-run.v0");
            }
            other => panic!("expected unknown nested version, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_denied() {
        let mut value = serde_json::to_value(PublicRunV1::from_run(&secret_run())).unwrap();
        value["promptPreview"] = json!(SECRET_PROMPT);
        match parse_public_run_v1(&value) {
            Err(PublicRunDtoError::Decode(message)) => {
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
        let mut value = serde_json::to_value(PublicRunV1::from_run(&secret_run())).unwrap();
        value.as_object_mut().unwrap().remove("schemaVersion");
        assert!(matches!(
            parse_public_run_v1(&value),
            Err(PublicRunDtoError::Decode(_))
        ));
    }

    #[test]
    fn current_run_record_wire_is_not_a_public_dto() {
        let wire = serde_json::to_value(secret_run()).unwrap();
        assert!(
            matches!(
                parse_public_run_v1(&wire),
                Err(PublicRunDtoError::Decode(_))
            ),
            "current RunRecord JSON must not parse as PublicRunV1"
        );
        assert!(matches!(
            parse_public_run_list_v1(&json!({ "runs": [wire.clone()] })),
            Err(PublicRunDtoError::Decode(_))
        ));
        assert!(matches!(
            parse_public_run_progress_v1(&wire),
            Err(PublicRunDtoError::Decode(_))
        ));
        assert!(matches!(
            parse_public_run_handoff_v1(&wire),
            Err(PublicRunDtoError::Decode(_))
        ));
    }
}
