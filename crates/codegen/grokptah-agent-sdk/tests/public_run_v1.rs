//! Fail-closed parser tests for staged `grokptah.public-run.v1` documents.
//!
//! Fixtures are synthetic JSON. Live MCP still emits `RunRecord`; these parsers
//! are not wired to `ReadObservatory`.

use grokptah_agent_sdk::{
    PUBLIC_RUN_SCHEMA_VERSION, PublicRunHandoffV1, PublicRunListV1, PublicRunProgressV1,
    PublicRunState, PublicRunV1, SdkError, parse_public_run_handoff_v1, parse_public_run_list_v1,
    parse_public_run_progress_v1, parse_public_run_v1,
};
use serde_json::{Value, json};

const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
const SECRET_RESPONSE: &str = "wrote tokens to /tmp/secret-chat/credentials.env";
const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
const SECRET_CWD: &str = "/tmp/secret-chat";
const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
const SECRET_DETAIL: &str = "editing /tmp/secret-chat/credentials.env";
const OPAQUE_RUN: &str = "run_public_dto_1";
const TS: &str = "2026-08-01T00:00:00Z";

const PRIVATE_KEYS: &[&str] = &[
    "promptPreview",
    "finalResponse",
    "workspace",
    "clientId",
    "requestId",
    "sessionId",
    "providerId",
    "leaseId",
    "attemptId",
    "workAttemptId",
    "execution",
    "approval",
    "aggregates",
    "progress",
    "path",
    "sourceWorkspace",
    "executionWorkspace",
    "changedFiles",
    "lastTool",
    "bounds",
    "agentId",
];

fn public_run() -> PublicRunV1 {
    PublicRunV1 {
        schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
        run_id: OPAQUE_RUN.into(),
        state: PublicRunState::Completed,
        created_at: TS.into(),
        updated_at: TS.into(),
        queue_position: Some(2),
        event_start_seq: Some(3),
        event_end_seq: Some(8),
        change_count: 1,
        test_count: 1,
        permission_requested_count: 2,
        permission_granted_count: 1,
        permission_denied_count: 1,
        usage_prompt_tokens: 10,
        usage_completion_tokens: 4,
        usage_total_tokens: 14,
        usage_request_count: 1,
        usage_complete: true,
        usage_pending_request_count: 0,
        progress_round: Some(3),
        progress_max_rounds: Some(4),
    }
}

fn public_list() -> PublicRunListV1 {
    PublicRunListV1 {
        schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
        runs: vec![public_run()],
    }
}

fn public_progress() -> PublicRunProgressV1 {
    PublicRunProgressV1 {
        schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
        run_id: OPAQUE_RUN.into(),
        state: PublicRunState::Completed,
        busy: true,
        created_at: TS.into(),
        updated_at: TS.into(),
        queue_position: Some(2),
        event_start_seq: Some(3),
        event_end_seq: Some(8),
        progress_round: Some(3),
        progress_max_rounds: Some(4),
    }
}

fn public_handoff() -> PublicRunHandoffV1 {
    PublicRunHandoffV1 {
        schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
        run_id: OPAQUE_RUN.into(),
        state: PublicRunState::Completed,
        created_at: TS.into(),
        updated_at: TS.into(),
        event_start_seq: Some(3),
        event_end_seq: Some(8),
        change_count: 1,
        test_count: 1,
        usage_prompt_tokens: 10,
        usage_completion_tokens: 4,
        usage_total_tokens: 14,
        usage_request_count: 1,
        usage_complete: true,
        usage_pending_request_count: 0,
    }
}

fn run_record_wire() -> Value {
    json!({
        "runId": OPAQUE_RUN,
        "sessionId": "11111111-1111-4111-8111-111111111111",
        "workspace": SECRET_CWD,
        "requestId": "request-secret",
        "clientId": "mcp",
        "state": "completed",
        "promptPreview": SECRET_PROMPT,
        "startSeq": 3,
        "endSeq": 8,
        "createdAt": TS,
        "updatedAt": TS,
        "finalResponse": SECRET_RESPONSE,
        "bounds": { "maxPromptBytes": 1000, "maxRounds": 4, "maxDurationMs": 1000 },
        "aggregates": {
            "changes": [{ "path": SECRET_PATH, "summary": "leaked" }],
            "tests": [{ "callId": "t1", "command": SECRET_TOOL, "status": "passed" }],
            "permissionsRequested": 2,
            "permissionsGranted": 1,
            "permissionsDenied": 1,
            "usage": { "promptTokens": 10, "completionTokens": 4, "totalTokens": 14, "requests": 1 }
        },
        "progress": {
            "round": 3,
            "maxRounds": 4,
            "lastTool": SECRET_TOOL,
            "detail": SECRET_DETAIL,
            "updatedAt": TS
        },
        "execution": {
            "mode": "isolated_worktree",
            "sourceWorkspace": SECRET_CWD,
            "executionWorkspace": SECRET_CWD
        },
        "approval": {
            "approvalId": "appr-secret",
            "changedFiles": [{ "path": SECRET_PATH }]
        }
    })
}

fn error_blob(error: &SdkError) -> String {
    format!("{error} {error:?} {}", error.code())
}

fn assert_redacted_internal(error: SdkError) {
    assert_eq!(error, SdkError::Internal);
    assert_eq!(error.code(), "internal");
    let blob = error_blob(&error);
    for needle in [
        SECRET_PROMPT,
        SECRET_RESPONSE,
        SECRET_PATH,
        SECRET_CWD,
        SECRET_TOOL,
        SECRET_DETAIL,
        "promptPreview",
        "finalResponse",
        "unknown field",
    ] {
        assert!(
            !blob.contains(needle),
            "public-run error leaked {needle:?}: {blob}"
        );
    }
}

fn secret_value_for(key: &str) -> Value {
    match key {
        "promptPreview" | "finalResponse" => json!(SECRET_PROMPT),
        "aggregates" => json!({
            "changes": [{ "path": SECRET_PATH, "summary": "leaked" }],
            "tests": [{ "command": SECRET_TOOL }]
        }),
        "progress" => json!({
            "round": 3,
            "lastTool": SECRET_TOOL,
            "detail": SECRET_DETAIL
        }),
        "execution" => json!({
            "sourceWorkspace": SECRET_CWD,
            "executionWorkspace": SECRET_PATH
        }),
        "approval" => json!({
            "changedFiles": [{ "path": SECRET_PATH }]
        }),
        "bounds" => json!({ "maxPromptBytes": 1000, "maxRounds": 4, "maxDurationMs": 1000 }),
        _ => json!(SECRET_PATH),
    }
}

#[test]
fn schema_version_constant_is_v1() {
    assert_eq!(PUBLIC_RUN_SCHEMA_VERSION, "grokptah.public-run.v1");
}

#[test]
fn get_document_round_trips() {
    let expected = public_run();
    let value = serde_json::to_value(&expected).unwrap();
    assert_eq!(parse_public_run_v1(&value).unwrap(), expected);
    let wire = json!({
        "schemaVersion": PUBLIC_RUN_SCHEMA_VERSION,
        "runId": OPAQUE_RUN,
        "state": "completed",
        "createdAt": TS,
        "updatedAt": TS,
        "queuePosition": 2,
        "eventStartSeq": 3,
        "eventEndSeq": 8,
        "changeCount": 1,
        "testCount": 1,
        "permissionRequestedCount": 2,
        "permissionGrantedCount": 1,
        "permissionDeniedCount": 1,
        "usagePromptTokens": 10,
        "usageCompletionTokens": 4,
        "usageTotalTokens": 14,
        "usageRequestCount": 1,
        "usageComplete": true,
        "usagePendingRequestCount": 0,
        "progressRound": 3,
        "progressMaxRounds": 4
    });
    assert_eq!(parse_public_run_v1(&wire).unwrap(), expected);
}

#[test]
fn list_document_round_trips() {
    let expected = public_list();
    let value = serde_json::to_value(&expected).unwrap();
    assert_eq!(parse_public_run_list_v1(&value).unwrap(), expected);
}

#[test]
fn progress_document_round_trips() {
    let expected = public_progress();
    let value = serde_json::to_value(&expected).unwrap();
    assert_eq!(parse_public_run_progress_v1(&value).unwrap(), expected);
}

#[test]
fn handoff_document_round_trips() {
    let expected = public_handoff();
    let value = serde_json::to_value(&expected).unwrap();
    assert_eq!(parse_public_run_handoff_v1(&value).unwrap(), expected);
}

#[test]
fn missing_optional_counters_default_to_none() {
    let mut row = serde_json::to_value(public_run()).unwrap();
    for key in [
        "queuePosition",
        "eventStartSeq",
        "eventEndSeq",
        "progressRound",
        "progressMaxRounds",
    ] {
        row.as_object_mut().unwrap().remove(key);
    }
    let parsed = parse_public_run_v1(&row).unwrap();
    assert_eq!(parsed.queue_position, None);
    assert_eq!(parsed.event_start_seq, None);
    assert_eq!(parsed.event_end_seq, None);
    assert_eq!(parsed.progress_round, None);
    assert_eq!(parsed.progress_max_rounds, None);
}

#[test]
fn unknown_schema_version_fails_closed() {
    let mut get = serde_json::to_value(public_run()).unwrap();
    get["schemaVersion"] = json!("grokptah.public-run.v2");
    assert_redacted_internal(parse_public_run_v1(&get).unwrap_err());

    let mut list = serde_json::to_value(public_list()).unwrap();
    list["schemaVersion"] = json!("grokptah.public-run.v2");
    assert_redacted_internal(parse_public_run_list_v1(&list).unwrap_err());

    let mut progress = serde_json::to_value(public_progress()).unwrap();
    progress["schemaVersion"] = json!("grokptah.public-run.v2");
    assert_redacted_internal(parse_public_run_progress_v1(&progress).unwrap_err());

    let mut handoff = serde_json::to_value(public_handoff()).unwrap();
    handoff["schemaVersion"] = json!("grokptah.public-run.v2");
    assert_redacted_internal(parse_public_run_handoff_v1(&handoff).unwrap_err());
}

#[test]
fn nested_list_item_unknown_version_fails_closed() {
    let mut value = serde_json::to_value(public_list()).unwrap();
    value["runs"][0]["schemaVersion"] = json!("grokptah.public-run.v0");
    assert_redacted_internal(parse_public_run_list_v1(&value).unwrap_err());
}

#[test]
fn missing_schema_version_fails_closed() {
    let mut value = serde_json::to_value(public_run()).unwrap();
    value.as_object_mut().unwrap().remove("schemaVersion");
    value["promptPreview"] = json!(SECRET_PROMPT);
    assert_redacted_internal(parse_public_run_v1(&value).unwrap_err());

    let mut handoff = serde_json::to_value(public_handoff()).unwrap();
    handoff.as_object_mut().unwrap().remove("schemaVersion");
    handoff["finalResponse"] = json!(SECRET_RESPONSE);
    assert_redacted_internal(parse_public_run_handoff_v1(&handoff).unwrap_err());
}

#[test]
fn unknown_and_private_keys_are_rejected() {
    for key in PRIVATE_KEYS {
        let mut row = serde_json::to_value(public_run()).unwrap();
        row[*key] = secret_value_for(key);
        assert_redacted_internal(parse_public_run_v1(&row).unwrap_err());
    }
}

#[test]
fn list_envelope_and_nested_runs_reject_private_keys() {
    let mut envelope = serde_json::to_value(public_list()).unwrap();
    envelope["workspace"] = json!(SECRET_CWD);
    envelope["sessionId"] = json!("11111111-1111-4111-8111-111111111111");
    assert_redacted_internal(parse_public_run_list_v1(&envelope).unwrap_err());

    let mut nested = serde_json::to_value(public_list()).unwrap();
    nested["runs"][0]["promptPreview"] = json!(SECRET_PROMPT);
    assert_redacted_internal(parse_public_run_list_v1(&nested).unwrap_err());
}

#[test]
fn progress_rejects_nested_progress_and_private_keys() {
    let mut row = serde_json::to_value(public_progress()).unwrap();
    row["progress"] = json!({
        "round": 3,
        "lastTool": SECRET_TOOL,
        "detail": SECRET_PATH
    });
    row["promptPreview"] = json!(SECRET_PROMPT);
    assert_redacted_internal(parse_public_run_progress_v1(&row).unwrap_err());
}

#[test]
fn get_document_is_not_progress() {
    let value = serde_json::to_value(public_run()).unwrap();
    assert_redacted_internal(parse_public_run_progress_v1(&value).unwrap_err());
}

#[test]
fn handoff_rejects_final_response_paths_and_nested_aggregates() {
    let mut row = serde_json::to_value(public_handoff()).unwrap();
    row["finalResponse"] = json!(SECRET_RESPONSE);
    row["changes"] = json!([{ "path": SECRET_PATH, "summary": "leaked" }]);
    row["aggregates"] = json!({ "tests": [{ "command": SECRET_TOOL }] });
    assert_redacted_internal(parse_public_run_handoff_v1(&row).unwrap_err());
}

#[test]
fn raw_run_record_is_rejected_by_all_four_parsers() {
    let wire = run_record_wire();
    assert_redacted_internal(parse_public_run_v1(&wire).unwrap_err());
    assert_redacted_internal(
        parse_public_run_list_v1(&json!({ "runs": [wire.clone()] })).unwrap_err(),
    );
    assert_redacted_internal(parse_public_run_progress_v1(&wire).unwrap_err());
    assert_redacted_internal(parse_public_run_handoff_v1(&wire).unwrap_err());
}

#[test]
fn public_run_dto_does_not_carry_request_scope() {
    let encoded = serde_json::to_value(public_run()).unwrap();
    for key in ["sessionId", "workspace", "clientId", "requestId"] {
        assert!(
            encoded.get(key).is_none(),
            "public-run dto must omit request-derived {key}"
        );
    }
}
