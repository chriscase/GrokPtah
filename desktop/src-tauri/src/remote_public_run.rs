//! Additive desktop consumer for `grokptah.public-run.v1`.
//!
//! Parses remote `ptah_list_runs` / `ptah_get_run` bodies with
//! `grokptah-agent-sdk` so this crate does not duplicate the allowlist. Unknown
//! versions, unknown fields, and legacy `RunRecord` fail closed as a redacted
//! decode error (no serde payload, field names, or secret values).
//!
//! `session_id` and `workspace` are stamped from the MCP request or the
//! session loop that issued it. They are never deserialized from the remote
//! document. Raw `list_runs` / `get_run` stay on `RunRecord`.

use anyhow::{anyhow, Result};
use grokptah_agent_sdk::{parse_public_run_list_v1, parse_public_run_v1, PublicRunV1};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Request-stamped public-run row returned by additive Tauri commands.
///
/// Allowlisted body fields come from [`PublicRunV1`]. Scope fields are not
/// part of the remote document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePublicRun {
    pub session_id: Uuid,
    pub workspace: String,
    #[serde(flatten)]
    pub document: PublicRunV1,
}

/// Request-stamped public-run list for one session/workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePublicRunList {
    pub session_id: Uuid,
    pub workspace: String,
    pub schema_version: String,
    pub runs: Vec<RemotePublicRun>,
}

fn redact_public_run_error() -> anyhow::Error {
    anyhow!("remote public-run decode failed")
}

/// Parse one `ptah_get_run` `grokptah.public-run.v1` document and stamp scope
/// from the request, never from `body`.
pub fn parse_remote_public_run(
    body: &Value,
    session_id: Uuid,
    workspace: &str,
) -> Result<RemotePublicRun> {
    let document = parse_public_run_v1(body).map_err(|_| redact_public_run_error())?;
    Ok(RemotePublicRun {
        session_id,
        workspace: workspace.to_string(),
        document,
    })
}

/// Parse one `ptah_list_runs` `grokptah.public-run.v1` envelope and stamp every
/// row from the request/session loop, never from `body`.
pub fn parse_remote_public_run_list(
    body: &Value,
    session_id: Uuid,
    workspace: &str,
) -> Result<RemotePublicRunList> {
    let parsed = parse_public_run_list_v1(body).map_err(|_| redact_public_run_error())?;
    let workspace = workspace.to_string();
    Ok(RemotePublicRunList {
        session_id,
        schema_version: parsed.schema_version,
        runs: parsed
            .runs
            .into_iter()
            .map(|document| RemotePublicRun {
                session_id,
                workspace: workspace.clone(),
                document,
            })
            .collect(),
        workspace,
    })
}

#[cfg(test)]
mod tests {
    use grokptah_agent_bridge::RunRecord;
    use grokptah_agent_sdk::{PublicRunState, PublicRunV1, PUBLIC_RUN_SCHEMA_VERSION};
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{parse_remote_public_run, parse_remote_public_run_list, RemotePublicRun};

    const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
    const SECRET_RESPONSE: &str = "wrote tokens to /tmp/secret-chat/credentials.env";
    const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
    const SECRET_CWD: &str = "/tmp/secret-chat";
    const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
    const SECRET_DETAIL: &str = "editing /tmp/secret-chat/credentials.env";
    const OPAQUE_RUN: &str = "run_public_dto_1";
    const TS: &str = "2026-08-01T00:00:00Z";
    const REQUEST_SESSION: &str = "11111111-1111-4111-8111-111111111111";
    const REQUEST_WORKSPACE: &str = "/tmp/project";
    const BODY_SESSION: &str = "22222222-2222-4222-8222-222222222222";
    const BODY_WORKSPACE: &str = "/tmp/secret-chat";

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

    fn request_session() -> Uuid {
        Uuid::parse_str(REQUEST_SESSION).unwrap()
    }

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

    fn public_list_wire() -> Value {
        json!({
            "schemaVersion": PUBLIC_RUN_SCHEMA_VERSION,
            "runs": [serde_json::to_value(public_run()).unwrap()]
        })
    }

    fn run_record_wire() -> Value {
        json!({
            "runId": OPAQUE_RUN,
            "sessionId": BODY_SESSION,
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
                "executionWorkspace": SECRET_CWD,
                "baseRevision": "base-secret",
                "sourceFingerprint": "source-secret",
                "finalFingerprint": "final-secret",
                "promotionState": "ready",
                "promotedAt": null
            },
            "approval": {
                "approvalId": "appr-secret",
                "runId": OPAQUE_RUN,
                "sessionId": BODY_SESSION,
                "workspace": SECRET_CWD,
                "sourceFingerprint": "source-secret",
                "finalFingerprint": "final-secret",
                "changedFiles": [{ "path": SECRET_PATH, "summary": "leaked" }],
                "issuedAt": TS,
                "expiresAt": TS
            }
        })
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
            "sessionId" => json!(BODY_SESSION),
            "workspace" => json!(BODY_WORKSPACE),
            _ => json!(SECRET_PATH),
        }
    }

    fn error_blob(error: &anyhow::Error) -> String {
        format!("{error:#} {error:?}")
    }

    fn assert_redacted(error: anyhow::Error) {
        let blob = error_blob(&error);
        assert!(
            blob.contains("remote public-run decode failed"),
            "expected redacted public-run error, got {blob}"
        );
        for needle in [
            SECRET_PROMPT,
            SECRET_RESPONSE,
            SECRET_PATH,
            SECRET_CWD,
            SECRET_TOOL,
            SECRET_DETAIL,
            BODY_SESSION,
            BODY_WORKSPACE,
            REQUEST_SESSION,
            REQUEST_WORKSPACE,
            "promptPreview",
            "finalResponse",
            "unknown field",
            "grokptah.public-run.v2",
            "grokptah.public-run.v0",
        ] {
            assert!(
                !blob.contains(needle),
                "public-run error leaked {needle:?}: {blob}"
            );
        }
    }

    fn parse_get(body: &Value) -> Result<RemotePublicRun, anyhow::Error> {
        parse_remote_public_run(body, request_session(), REQUEST_WORKSPACE)
    }

    #[test]
    fn list_and_get_stamp_request_scope_not_body() {
        let got = parse_get(&serde_json::to_value(public_run()).unwrap()).unwrap();
        assert_eq!(got.session_id, request_session());
        assert_eq!(got.workspace, REQUEST_WORKSPACE);
        assert_eq!(got.document, public_run());

        let listed =
            parse_remote_public_run_list(&public_list_wire(), request_session(), REQUEST_WORKSPACE)
                .unwrap();
        assert_eq!(listed.session_id, request_session());
        assert_eq!(listed.workspace, REQUEST_WORKSPACE);
        assert_eq!(listed.schema_version, PUBLIC_RUN_SCHEMA_VERSION);
        assert_eq!(listed.runs.len(), 1);
        assert_eq!(listed.runs[0].session_id, request_session());
        assert_eq!(listed.runs[0].workspace, REQUEST_WORKSPACE);
        assert_eq!(listed.runs[0].document.run_id, OPAQUE_RUN);

        let encoded = serde_json::to_value(&got).unwrap();
        assert_eq!(encoded["sessionId"], json!(REQUEST_SESSION));
        assert_eq!(encoded["workspace"], json!(REQUEST_WORKSPACE));
        assert_eq!(encoded["schemaVersion"], json!(PUBLIC_RUN_SCHEMA_VERSION));
        assert_eq!(encoded["runId"], json!(OPAQUE_RUN));
        for key in ["clientId", "requestId", "promptPreview", "finalResponse"] {
            assert!(encoded.get(key).is_none(), "stamped row leaked {key}");
        }
    }

    #[test]
    fn list_and_get_reject_legacy_run_record_without_secrets() {
        let wire = run_record_wire();
        assert_redacted(parse_get(&wire).unwrap_err());
        assert_redacted(
            parse_remote_public_run_list(
                &json!({ "runs": [wire.clone()] }),
                request_session(),
                REQUEST_WORKSPACE,
            )
            .unwrap_err(),
        );
        assert_redacted(
            parse_remote_public_run_list(
                &json!({
                    "schemaVersion": PUBLIC_RUN_SCHEMA_VERSION,
                    "runs": [wire]
                }),
                request_session(),
                REQUEST_WORKSPACE,
            )
            .unwrap_err(),
        );
    }

    #[test]
    fn list_and_get_reject_unknown_fields_without_secrets() {
        for key in PRIVATE_KEYS {
            let mut row = serde_json::to_value(public_run()).unwrap();
            row[*key] = secret_value_for(key);
            assert_redacted(parse_get(&row).unwrap_err());
        }

        let mut envelope = public_list_wire();
        envelope["sessionId"] = json!(BODY_SESSION);
        envelope["workspace"] = json!(BODY_WORKSPACE);
        envelope["promptPreview"] = json!(SECRET_PROMPT);
        assert_redacted(
            parse_remote_public_run_list(&envelope, request_session(), REQUEST_WORKSPACE)
                .unwrap_err(),
        );
    }

    #[test]
    fn list_and_get_reject_unknown_versions_without_secrets() {
        let mut get = serde_json::to_value(public_run()).unwrap();
        get["schemaVersion"] = json!("grokptah.public-run.v2");
        get["promptPreview"] = json!(SECRET_PROMPT);
        assert_redacted(parse_get(&get).unwrap_err());

        let mut list = public_list_wire();
        list["schemaVersion"] = json!("grokptah.public-run.v2");
        assert_redacted(
            parse_remote_public_run_list(&list, request_session(), REQUEST_WORKSPACE).unwrap_err(),
        );

        let mut nested = public_list_wire();
        nested["runs"][0]["schemaVersion"] = json!("grokptah.public-run.v0");
        nested["runs"][0]["finalResponse"] = json!(SECRET_RESPONSE);
        assert_redacted(
            parse_remote_public_run_list(&nested, request_session(), REQUEST_WORKSPACE)
                .unwrap_err(),
        );
    }

    #[test]
    fn raw_run_record_compatibility_is_unchanged() {
        let wire = run_record_wire();
        let raw: RunRecord = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(raw.run_id, OPAQUE_RUN);
        assert_eq!(raw.session_id.to_string(), BODY_SESSION);
        assert_eq!(raw.workspace, SECRET_CWD);
        assert_eq!(raw.prompt_preview, SECRET_PROMPT);
        assert_eq!(raw.final_response.as_deref(), Some(SECRET_RESPONSE));

        let listed: Vec<RunRecord> = serde_json::from_value(json!([wire])).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].prompt_preview, SECRET_PROMPT);

        let public = serde_json::to_value(public_run()).unwrap();
        assert!(
            serde_json::from_value::<RunRecord>(public.clone()).is_err(),
            "public-run.v1 must not decode as raw RunRecord"
        );
        assert!(parse_get(&public).is_ok());
    }
}
