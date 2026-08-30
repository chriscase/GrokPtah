//! Additive `ReadObservatory` public-run methods over a synthetic `McpTransport`.
//!
//! Public methods consume `grokptah.public-run.v1` only. Legacy `list_runs` /
//! `observe_run` return `unsupported` and must not call `ptah_list_runs` /
//! `ptah_get_run`.

use std::sync::{Arc, Mutex};

use grokptah_agent_sdk::{
    McpTool, McpTransport, PUBLIC_RUN_SCHEMA_VERSION, PublicRunHandoffV1, PublicRunListV1,
    PublicRunProgressV1, PublicRunState, PublicRunV1, ReadObservatory, RunId, RunSelector,
    SdkError, SessionScope, TransportError,
};
use serde_json::{Value, json};

const BUILD_SESSION: &str = "11111111-1111-4111-8111-111111111111";
const BUILD_CWD: &str = "/tmp/project";
const RUN_ID: &str = "run_public_dto_1";
const TS: &str = "2026-08-01T00:00:00Z";
const SECRET_PROMPT: &str = "leak-prompt: rotate /tmp/secret-chat/credentials.env";
const SECRET_RESPONSE: &str = "wrote tokens to /tmp/secret-chat/credentials.env";
const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";
const SECRET_CWD: &str = "/tmp/secret-chat";
const SECRET_TOOL: &str = "cat /tmp/secret-chat/credentials.env";
const SECRET_DETAIL: &str = "editing /tmp/secret-chat/credentials.env";

const PUBLIC_RUN_TOOLS: &[&str] = &[
    "ptah_list_sessions",
    "ptah_list_runs",
    "ptah_get_run",
    "ptah_get_progress",
    "ptah_get_handoff",
];

const LEGACY_READ_TOOLS: &[&str] = &[
    "ptah_list_sessions",
    "ptah_list_runs",
    "ptah_get_run",
    "ptah_get_events",
    "ptah_get_capacity",
];

type ToolHandler = dyn Fn(&str, &Value) -> Result<Value, TransportError> + Send + Sync;
type ToolHandlerFn = fn(&str, &Value) -> Result<Value, TransportError>;

struct ScriptedTransport {
    tools: Vec<String>,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    handler: Arc<ToolHandler>,
}

impl ScriptedTransport {
    fn new(
        tools: &[&str],
        handler: impl Fn(&str, &Value) -> Result<Value, TransportError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            tools: tools.iter().map(|name| (*name).to_string()).collect(),
            calls: Arc::new(Mutex::new(Vec::new())),
            handler: Arc::new(handler),
        }
    }
}

impl McpTransport for ScriptedTransport {
    async fn list_tools(&self) -> Result<Vec<McpTool>, TransportError> {
        Ok(self
            .tools
            .iter()
            .map(|name| McpTool { name: name.clone() })
            .collect())
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, TransportError> {
        self.calls
            .lock()
            .expect("call log")
            .push((name.to_string(), arguments.clone()));
        (self.handler)(name, &arguments)
    }
}

fn sessions_wire() -> Value {
    json!({
        "sessions": [{
            "sessionId": BUILD_SESSION,
            "title": "Build",
            "kind": "build",
            "cwd": BUILD_CWD,
            "workspaceStatus": "ready",
            "updatedAt": TS,
            "busy": false
        }]
    })
}

fn public_run() -> PublicRunV1 {
    PublicRunV1 {
        schema_version: PUBLIC_RUN_SCHEMA_VERSION.to_string(),
        run_id: RUN_ID.into(),
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
        run_id: RUN_ID.into(),
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
        run_id: RUN_ID.into(),
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
        "runId": RUN_ID,
        "sessionId": BUILD_SESSION,
        "workspace": BUILD_CWD,
        "requestId": "request-1",
        "clientId": "mcp",
        "state": "completed",
        "queuePosition": null,
        "bounds": {
            "maxPromptBytes": 1000,
            "maxRounds": 2,
            "maxDurationMs": 1000
        },
        "promptPreview": SECRET_PROMPT,
        "startSeq": 1,
        "endSeq": 4,
        "createdAt": TS,
        "updatedAt": TS,
        "terminalResult": "completed",
        "finalResponse": SECRET_RESPONSE,
        "errorCode": null,
        "stopCause": "completed",
        "aggregates": {
            "changes": [{ "path": SECRET_PATH, "summary": "leaked" }],
            "tests": [{ "callId": "t1", "command": SECRET_TOOL, "status": "passed" }],
            "usage": {
                "promptTokens": 10,
                "completionTokens": 4,
                "totalTokens": 14,
                "requests": 1
            },
            "usageComplete": true
        },
        "progress": {
            "round": 3,
            "maxRounds": 4,
            "lastTool": SECRET_TOOL,
            "detail": SECRET_DETAIL,
            "updatedAt": TS
        }
    })
}

fn public_run_handler(name: &str, _arguments: &Value) -> Result<Value, TransportError> {
    match name {
        "ptah_list_sessions" => Ok(sessions_wire()),
        "ptah_list_runs" => Ok(serde_json::to_value(public_list()).unwrap()),
        "ptah_get_run" => Ok(serde_json::to_value(public_run()).unwrap()),
        "ptah_get_progress" => Ok(serde_json::to_value(public_progress()).unwrap()),
        "ptah_get_handoff" => Ok(serde_json::to_value(public_handoff()).unwrap()),
        _ => Err(TransportError::Host {
            code: "unsupported".into(),
            event_range: None,
        }),
    }
}

fn legacy_run_handler(name: &str, _arguments: &Value) -> Result<Value, TransportError> {
    match name {
        "ptah_list_sessions" => Ok(sessions_wire()),
        "ptah_list_runs" => Ok(json!({ "runs": [run_record_wire()] })),
        "ptah_get_run" => Ok(run_record_wire()),
        "ptah_get_progress" => Ok(run_record_wire()),
        "ptah_get_handoff" => Ok(run_record_wire()),
        _ => Err(TransportError::Host {
            code: "unsupported".into(),
            event_range: None,
        }),
    }
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
        BUILD_CWD,
        BUILD_SESSION,
        "promptPreview",
        "finalResponse",
        "unknown field",
        "grokptah.public-run.v2",
    ] {
        assert!(
            !blob.contains(needle),
            "public-run error leaked {needle:?}: {blob}"
        );
    }
}

fn assert_no_request_scope(value: &Value) {
    for key in [
        "sessionId",
        "workspace",
        "session_id",
        "clientId",
        "requestId",
    ] {
        assert!(
            value.get(key).is_none(),
            "public-run body must omit request-derived {key}: {value}"
        );
    }
}

async fn connect(
    transport: ScriptedTransport,
) -> (
    ReadObservatory<ScriptedTransport>,
    Arc<Mutex<Vec<(String, Value)>>>,
) {
    let calls = transport.calls.clone();
    let sdk = ReadObservatory::connect(transport).await.expect("connect");
    (sdk, calls)
}

fn call_named<'a>(log: &'a [(String, Value)], name: &str) -> &'a Value {
    log.iter()
        .find(|(tool, _)| tool == name)
        .map(|(_, args)| args)
        .unwrap_or_else(|| panic!("missing tool call {name}"))
}

#[tokio::test]
async fn public_run_methods_send_exact_scoped_arguments() {
    let (sdk, calls) = connect(ScriptedTransport::new(PUBLIC_RUN_TOOLS, public_run_handler)).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));

    sdk.list_public_runs(&scope).await.unwrap();
    sdk.observe_public_run(&selector).await.unwrap();
    sdk.observe_public_progress(&selector).await.unwrap();
    sdk.observe_public_handoff(&selector).await.unwrap();

    let log = calls.lock().unwrap().clone();
    let list_args = call_named(&log, "ptah_list_runs");
    assert_eq!(
        list_args,
        &json!({
            "session_id": BUILD_SESSION,
            "workspace": BUILD_CWD,
        })
    );

    let expected_run_args = json!({
        "session_id": BUILD_SESSION,
        "workspace": BUILD_CWD,
        "run_id": RUN_ID,
    });
    assert_eq!(call_named(&log, "ptah_get_run"), &expected_run_args);
    assert_eq!(call_named(&log, "ptah_get_progress"), &expected_run_args);
    assert_eq!(call_named(&log, "ptah_get_handoff"), &expected_run_args);
}

#[tokio::test]
async fn public_run_methods_accept_only_v1() {
    let (sdk, _) = connect(ScriptedTransport::new(PUBLIC_RUN_TOOLS, public_run_handler)).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));

    let listed = sdk.list_public_runs(&scope).await.unwrap();
    assert_eq!(listed, public_list());
    assert_eq!(listed.schema_version, PUBLIC_RUN_SCHEMA_VERSION);
    assert_no_request_scope(&serde_json::to_value(&listed).unwrap());
    assert_no_request_scope(&serde_json::to_value(&listed.runs[0]).unwrap());

    let observed = sdk.observe_public_run(&selector).await.unwrap();
    assert_eq!(observed, public_run());
    assert_no_request_scope(&serde_json::to_value(&observed).unwrap());

    let progress = sdk.observe_public_progress(&selector).await.unwrap();
    assert_eq!(progress, public_progress());
    assert_no_request_scope(&serde_json::to_value(&progress).unwrap());

    let handoff = sdk.observe_public_handoff(&selector).await.unwrap();
    assert_eq!(handoff, public_handoff());
    assert_no_request_scope(&serde_json::to_value(&handoff).unwrap());
}

#[tokio::test]
async fn public_run_methods_reject_unknown_version_fields_and_legacy_record() {
    let cases: Vec<(&str, Value)> = vec![
        (
            "ptah_list_runs",
            json!({
                "schemaVersion": "grokptah.public-run.v2",
                "runs": []
            }),
        ),
        ("ptah_get_run", {
            let mut row = serde_json::to_value(public_run()).unwrap();
            row["schemaVersion"] = json!("grokptah.public-run.v2");
            row
        }),
        ("ptah_get_progress", {
            let mut row = serde_json::to_value(public_progress()).unwrap();
            row["promptPreview"] = json!(SECRET_PROMPT);
            row
        }),
        ("ptah_get_handoff", {
            let mut row = serde_json::to_value(public_handoff()).unwrap();
            row["finalResponse"] = json!(SECRET_RESPONSE);
            row
        }),
        ("ptah_list_runs", json!({ "runs": [run_record_wire()] })),
        ("ptah_get_run", run_record_wire()),
        ("ptah_get_progress", run_record_wire()),
        ("ptah_get_handoff", run_record_wire()),
    ];

    for (tool, body) in cases {
        let transport = ScriptedTransport::new(PUBLIC_RUN_TOOLS, move |name, arguments| {
            if name == tool {
                return Ok(body.clone());
            }
            public_run_handler(name, arguments)
        });
        let (sdk, _) = connect(transport).await;
        let session = sdk.list_sessions().await.unwrap().remove(0);
        let scope = SessionScope::from_session(&session);
        let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
        let err = match tool {
            "ptah_list_runs" => sdk.list_public_runs(&scope).await.unwrap_err(),
            "ptah_get_run" => sdk.observe_public_run(&selector).await.unwrap_err(),
            "ptah_get_progress" => sdk.observe_public_progress(&selector).await.unwrap_err(),
            "ptah_get_handoff" => sdk.observe_public_handoff(&selector).await.unwrap_err(),
            other => panic!("unexpected tool {other}"),
        };
        assert_redacted_internal(err);
    }
}

#[tokio::test]
async fn legacy_list_runs_and_observe_run_are_unsupported_and_uninvoked() {
    let cases: &[(&[&str], ToolHandlerFn)] = &[
        (PUBLIC_RUN_TOOLS, public_run_handler),
        (LEGACY_READ_TOOLS, legacy_run_handler),
    ];
    for (tools, handler) in cases {
        let handler = *handler;
        let (sdk, calls) = connect(ScriptedTransport::new(tools, handler)).await;
        let session = sdk.list_sessions().await.unwrap().remove(0);
        let scope = SessionScope::from_session(&session);
        let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
        calls.lock().unwrap().clear();

        let list_err = sdk.list_runs(&scope).await.unwrap_err();
        let observe_err = sdk.observe_run(&selector).await.unwrap_err();
        assert_eq!(list_err, SdkError::Unsupported);
        assert_eq!(observe_err, SdkError::Unsupported);
        assert_eq!(list_err.code(), "unsupported");
        for err in [&list_err, &observe_err] {
            let blob = error_blob(err);
            for needle in [
                SECRET_PROMPT,
                SECRET_RESPONSE,
                SECRET_PATH,
                SECRET_CWD,
                SECRET_TOOL,
                SECRET_DETAIL,
                BUILD_CWD,
                BUILD_SESSION,
                "promptPreview",
                "finalResponse",
                "ptah_list_runs",
                "ptah_get_run",
            ] {
                assert!(
                    !blob.contains(needle),
                    "legacy unsupported error leaked {needle:?}: {blob}"
                );
            }
        }

        let log = calls.lock().unwrap().clone();
        assert!(
            log.is_empty(),
            "legacy list_runs/observe_run must not call transport: {log:?}"
        );
    }
}

#[tokio::test]
async fn missing_public_run_tools_are_unsupported_and_uninvoked() {
    let transport = ScriptedTransport::new(&["ptah_list_sessions"], |name, arguments| {
        if name == "ptah_list_sessions" {
            return public_run_handler(name, arguments);
        }
        panic!("missing tool {name} must not be called");
    });
    let (sdk, calls) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));

    assert_eq!(
        sdk.list_public_runs(&scope).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_public_run(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_public_progress(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_public_handoff(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );

    let names: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    assert_eq!(names, vec!["ptah_list_sessions"]);
}

#[tokio::test]
async fn public_run_host_and_transport_errors_preserve_existing_mappings() {
    let cases = [
        (TransportError::Unauthenticated, SdkError::Unauthenticated),
        (TransportError::Timeout, SdkError::Timeout),
        (
            TransportError::CapacityExhausted,
            SdkError::CapacityExhausted,
        ),
        (TransportError::Protocol, SdkError::Internal),
        (TransportError::Io, SdkError::Internal),
        (
            TransportError::from_host_data(&json!({"code": "workspace_mismatch"})),
            SdkError::WorkspaceMismatch,
        ),
        (
            TransportError::from_host_data(&json!({"code": "unsupported"})),
            SdkError::Unsupported,
        ),
        (
            TransportError::from_host_data(&json!({"code": "invalid_request"})),
            SdkError::ForbiddenScope,
        ),
        (
            TransportError::from_host_data(&json!({"code": "forbidden_scope"})),
            SdkError::ForbiddenScope,
        ),
        (
            TransportError::from_host_data(&json!({"code": "mystery_code"})),
            SdkError::Internal,
        ),
    ];
    for (transport_error, expected) in cases {
        let err = transport_error.clone();
        let transport = ScriptedTransport::new(PUBLIC_RUN_TOOLS, move |name, arguments| {
            if name == "ptah_get_run" {
                Err(err.clone())
            } else {
                public_run_handler(name, arguments)
            }
        });
        let (sdk, _) = connect(transport).await;
        let session = sdk.list_sessions().await.unwrap().remove(0);
        let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
        assert_eq!(
            sdk.observe_public_run(&selector).await.unwrap_err(),
            expected
        );
    }

    let list_mismatch = ScriptedTransport::new(PUBLIC_RUN_TOOLS, |name, arguments| {
        if name == "ptah_list_runs" {
            Err(TransportError::from_host_data(
                &json!({"code": "workspace_mismatch"}),
            ))
        } else {
            public_run_handler(name, arguments)
        }
    });
    let (sdk, _) = connect(list_mismatch).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    assert_eq!(
        sdk.list_public_runs(&scope).await.unwrap_err(),
        SdkError::WorkspaceMismatch
    );
}

#[tokio::test]
async fn public_run_structured_content_envelope_is_unwrapped() {
    let transport = ScriptedTransport::new(PUBLIC_RUN_TOOLS, |name, arguments| {
        public_run_handler(name, arguments).map(|body| {
            json!({
                "content": [{ "type": "text", "text": "{}" }],
                "structuredContent": body,
                "isError": false
            })
        })
    });
    let (sdk, _) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    assert_eq!(sdk.list_public_runs(&scope).await.unwrap(), public_list());
    assert_eq!(
        sdk.observe_public_run(&selector).await.unwrap(),
        public_run()
    );
    assert_eq!(
        sdk.observe_public_progress(&selector).await.unwrap(),
        public_progress()
    );
    assert_eq!(
        sdk.observe_public_handoff(&selector).await.unwrap(),
        public_handoff()
    );
}
