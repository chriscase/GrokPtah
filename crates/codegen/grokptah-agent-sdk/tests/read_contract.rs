//! Contract tests against current-main `mcp_control` / OrchestrationService JSON.
//!
//! Fixtures copy wire keys from `orchestration/service.rs` (`list_sessions`,
//! `get_capacity`) and `grokptah.public-event.v1`, plus the `cursor_expired` +
//! `eventRange` envelope used by current MCP 410 responses. Public run list/get
//! coverage lives in `public_run_observatory.rs`; legacy `list_runs` /
//! `observe_run` / `stream_events` are `unsupported` and must not call
//! `ptah_list_runs` / `ptah_get_run` / `ptah_get_events`.

use std::sync::{Arc, Mutex};

use grokptah_agent_sdk::{
    CONTRACT_VERSION, CapabilityState, EVENT_PAGE_LIMIT_DEFAULT, EVENT_PAGE_LIMIT_MAX,
    EVENT_PAGE_LIMIT_MIN, EventQuery, McpTool, McpTransport, PUBLIC_EVENT_SCHEMA_VERSION,
    PublicEventKindV1, ReadObservatory, RetainedRange, RunId, RunSelector, SdkError, SessionId,
    SessionScope, TransportError, contract_version,
};
use serde_json::{Value, json};

const BUILD_SESSION: &str = "11111111-1111-4111-8111-111111111111";
const CHAT_SESSION: &str = "22222222-2222-4222-8222-222222222222";
const RUN_ID: &str = "run-legacy";
const BUILD_CWD: &str = "/tmp/project";
const SECRET_CWD: &str = "/tmp/secret-chat";
const SECRET_PROMPT: &str = "inspect the secret token Bearer sk-live-example";
const SECRET_PATH: &str = "/tmp/secret-chat/credentials.env";

const ALL_READ_TOOLS: &[&str] = &[
    "ptah_list_sessions",
    "ptah_list_runs",
    "ptah_get_run",
    "ptah_get_events",
    "ptah_get_capacity",
];

type ToolHandler = dyn Fn(&str, &Value) -> Result<Value, TransportError> + Send + Sync;

struct ScriptedTransport {
    tools: Vec<String>,
    list_error: Option<TransportError>,
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
            list_error: None,
            calls: Arc::new(Mutex::new(Vec::new())),
            handler: Arc::new(handler),
        }
    }
}

impl McpTransport for ScriptedTransport {
    async fn list_tools(&self) -> Result<Vec<McpTool>, TransportError> {
        if let Some(error) = &self.list_error {
            return Err(error.clone());
        }
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

/// `OrchestrationService::list_sessions` row shape, plus a leaked chat session
/// that the SDK must drop.
fn sessions_wire() -> Value {
    json!({
        "sessions": [
            {
                "sessionId": BUILD_SESSION,
                "title": "Build",
                "kind": "build",
                "cwd": BUILD_CWD,
                "workspaceStatus": "ready",
                "updatedAt": "2026-08-01T00:00:00Z",
                "busy": false
            },
            {
                "sessionId": CHAT_SESSION,
                "title": "Chat",
                "kind": "chat",
                "cwd": SECRET_CWD,
                "workspaceStatus": "ready",
                "updatedAt": "2026-08-01T00:00:00Z",
                "busy": false
            }
        ]
    })
}

/// Current `RunRecord` camelCase, including fields the SDK must strip.
/// `promptPreview` / bounds match `orchestration/types.rs` legacy-run fixture.
fn run_wire() -> Value {
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
        "createdAt": "2026-08-01T00:00:00Z",
        "updatedAt": "2026-08-01T00:01:00Z",
        "terminalResult": "completed",
        "finalResponse": "wrote tokens to /tmp/secret-chat/credentials.env",
        "errorCode": null,
        "stopCause": "completed",
        "aggregates": {
            "changes": [{ "path": SECRET_PATH, "summary": "leaked" }],
            "tests": [{ "callId": "t1", "command": "cat /tmp/secret-chat/credentials.env", "status": "passed" }],
            "usage": {
                "promptTokens": 10,
                "completionTokens": 4,
                "totalTokens": 14,
                "requests": 1
            },
            "usageComplete": true
        },
        "execution": {
            "mode": "shared",
            "sourceWorkspace": BUILD_CWD,
            "executionWorkspace": SECRET_CWD,
            "baseRevision": "abc",
            "sourceFingerprint": "def"
        }
    })
}

/// `grokptah.public-event.v1` page emitted by public `ptah_get_events`.
fn events_wire() -> Value {
    json!({
        "schemaVersion": PUBLIC_EVENT_SCHEMA_VERSION,
        "events": [
            {
                "schemaVersion": PUBLIC_EVENT_SCHEMA_VERSION,
                "seq": 1,
                "ts": "2026-08-01T00:00:01Z",
                "kind": "agent_message"
            },
            {
                "schemaVersion": PUBLIC_EVENT_SCHEMA_VERSION,
                "seq": 2,
                "ts": "2026-08-01T00:00:02Z",
                "kind": "tool_call",
                "toolKind": "read",
                "status": "running"
            },
            {
                "schemaVersion": PUBLIC_EVENT_SCHEMA_VERSION,
                "seq": 3,
                "ts": "2026-08-01T00:00:03Z",
                "kind": "file_edit"
            },
            {
                "schemaVersion": PUBLIC_EVENT_SCHEMA_VERSION,
                "seq": 4,
                "ts": "2026-08-01T00:00:04Z",
                "kind": "prompt_queue_changed",
                "revision": 3
            }
        ],
        "nextCursor": 4
    })
}

/// `OrchestrationService::get_capacity` occupancy/health object.
fn capacity_wire() -> Value {
    json!({
        "maxConcurrentRuns": 4,
        "activeRuns": 1,
        "available": 3,
        "queuedRuns": 0,
        "queueLimit": 32,
        "health": {
            "laggedLiveEvents": 2,
            "eventJournalPersistenceError": null,
            "auditPersistenceError": null,
            "runPersistenceError": "disk full at /tmp/secret-chat",
            "workloadSupervisorError": null,
            "workloadSupervisor": {
                "enabled": false,
                "intervalMs": 1000,
                "lastReport": { "internalPath": SECRET_PATH }
            },
            "routineSupervisorError": null,
            "routineSupervisor": { "enabled": false, "intervalMs": 1000 },
            "managerSupervisorError": null,
            "managerSupervisor": { "enabled": false, "intervalMs": 1000 },
            "nativeExecutorError": null,
            "nativeExecutor": {
                "enabled": false,
                "intervalMs": 250,
                "admitted": 9,
                "finalized": 8
            }
        }
    })
}

fn default_handler(name: &str, _arguments: &Value) -> Result<Value, TransportError> {
    match name {
        "ptah_list_sessions" => Ok(sessions_wire()),
        "ptah_list_runs" => Ok(json!({ "runs": [run_wire()] })),
        "ptah_get_run" => Ok(run_wire()),
        "ptah_get_events" => Ok(events_wire()),
        "ptah_get_capacity" => Ok(capacity_wire()),
        _ => Err(TransportError::Host {
            code: "unsupported".into(),
            event_range: None,
        }),
    }
}

fn assert_no_sensitive(value: &Value) {
    let blob = value.to_string();
    for needle in [
        SECRET_PROMPT,
        SECRET_PATH,
        SECRET_CWD,
        BUILD_CWD,
        "promptPreview",
        "finalResponse",
        "unified_diff",
        "sourceWorkspace",
        "executionWorkspace",
        "nativeExecutor",
        "workloadSupervisor",
        "Bearer",
        "sk-live-example",
    ] {
        assert!(
            !blob.contains(needle),
            "public projection leaked {needle:?}: {blob}"
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

#[tokio::test]
async fn contract_version_is_1_0() {
    assert_eq!(contract_version(), "1.0");
    assert_eq!(CONTRACT_VERSION, "1.0");
    let (sdk, calls) = connect(ScriptedTransport::new(ALL_READ_TOOLS, default_handler)).await;
    let sessions = sdk.list_sessions().await.unwrap();
    assert_eq!(sessions[0].contract_version, "1.0");
    let scope = SessionScope::from_session(&sessions[0]);
    let selector = RunSelector::from_parts(&sessions[0], RunId::new(RUN_ID));
    calls.lock().unwrap().clear();
    assert_eq!(
        sdk.list_runs(&scope).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_run(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "legacy list_runs/observe_run must not call transport"
    );
    assert_eq!(
        sdk.stream_events(&selector, EventQuery::default())
            .await
            .unwrap_err(),
        SdkError::Unsupported
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "legacy stream_events must not call transport"
    );
    let page = sdk
        .stream_public_events(&selector, EventQuery::default())
        .await
        .unwrap();
    assert_eq!(page.schema_version, PUBLIC_EVENT_SCHEMA_VERSION);
    assert!(
        page.events
            .iter()
            .all(|event| event.schema_version == PUBLIC_EVENT_SCHEMA_VERSION)
    );
    let capacity = sdk.host_capacity().await.unwrap();
    assert_eq!(capacity.contract_version, "1.0");
}

#[tokio::test]
async fn capabilities_are_a_client_projection_of_tools_list() {
    let extra = ScriptedTransport::new(
        &[
            "ptah_list_sessions",
            "ptah_get_run",
            "ptah_get_events",
            "ptah_get_capacity",
            "ptah_list_computer_runs",
            "ptah_submit_task",
        ],
        default_handler,
    );
    let (sdk, _) = connect(extra).await;
    let caps = sdk.capabilities();
    assert_eq!(caps.session_list, CapabilityState::Available);
    assert_eq!(caps.run_observe, CapabilityState::Available);
    assert_eq!(caps.run_events_page, CapabilityState::Available);
    assert_eq!(caps.host_capacity, CapabilityState::Available);
    assert_eq!(caps.computer_control, CapabilityState::Forbidden);
    assert_eq!(caps.provider_credentials, CapabilityState::Forbidden);
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let list_err = sdk
        .list_runs(&SessionScope::from_session(&session))
        .await
        .unwrap_err();
    assert_eq!(list_err, SdkError::Unsupported);
}

#[tokio::test]
async fn missing_tool_is_unsupported_never_empty_data() {
    let transport = ScriptedTransport::new(&["ptah_list_sessions"], |name, arguments| {
        if name == "ptah_list_sessions" {
            return default_handler(name, arguments);
        }
        panic!("missing tool {name} must not be called");
    });
    let (sdk, calls) = connect(transport).await;
    assert_eq!(sdk.capabilities().run_observe, CapabilityState::Unavailable);
    assert_eq!(
        sdk.capabilities().run_events_page,
        CapabilityState::Unavailable
    );
    assert_eq!(
        sdk.capabilities().host_capacity,
        CapabilityState::Unavailable
    );
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    assert_eq!(
        sdk.list_runs(&scope).await.unwrap_err(),
        SdkError::Unsupported
    );
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    assert_eq!(
        sdk.observe_run(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.stream_events(&selector, EventQuery::default())
            .await
            .unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.host_capacity().await.unwrap_err(),
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
async fn unknown_host_tool_maps_to_unsupported() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, |name, arguments| {
        if name == "ptah_get_capacity" {
            Err(TransportError::Host {
                code: "unsupported".into(),
                event_range: None,
            })
        } else {
            default_handler(name, arguments)
        }
    });
    let (sdk, _) = connect(transport).await;
    assert_eq!(
        sdk.host_capacity().await.unwrap_err(),
        SdkError::Unsupported
    );
}

#[tokio::test]
async fn build_only_session_filtering_hides_chat_rows() {
    let (sdk, _) = connect(ScriptedTransport::new(ALL_READ_TOOLS, default_handler)).await;
    let sessions = sdk.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id.as_str(), BUILD_SESSION);
    assert_eq!(sessions[0].kind, "build");
    assert_eq!(format!("{:?}", sessions[0].workspace), "WorkspaceRef");
    let encoded = serde_json::to_value(&sessions[0]).unwrap();
    assert_no_sensitive(&encoded);
    assert_eq!(encoded["workspace"], json!(null));
}

#[tokio::test]
async fn run_and_event_projections_drop_sensitive_fields() {
    let (sdk, calls) = connect(ScriptedTransport::new(ALL_READ_TOOLS, default_handler)).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    calls.lock().unwrap().clear();
    assert_eq!(
        sdk.list_runs(&scope).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_run(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "legacy list_runs/observe_run must not call transport"
    );

    let page = sdk
        .stream_public_events(&selector, EventQuery::default())
        .await
        .unwrap();
    assert_eq!(page.schema_version, PUBLIC_EVENT_SCHEMA_VERSION);
    assert_eq!(page.events.len(), 4);
    assert_eq!(page.next_cursor, Some(4));
    assert_eq!(page.events[0].kind, PublicEventKindV1::AgentMessage);
    assert_eq!(page.events[1].kind, PublicEventKindV1::ToolCall);
    assert_eq!(page.events[1].tool_kind.as_deref(), Some("read"));
    assert_eq!(page.events[1].status.as_deref(), Some("running"));
    assert_eq!(page.events[2].kind, PublicEventKindV1::FileEdit);
    assert_eq!(page.events[3].kind, PublicEventKindV1::PromptQueueChanged);
    assert_eq!(page.events[3].revision, Some(3));
    assert_no_sensitive(&serde_json::to_value(&page).unwrap());
}

#[tokio::test]
async fn host_capacity_is_occupancy_and_health_only() {
    let (sdk, _) = connect(ScriptedTransport::new(ALL_READ_TOOLS, default_handler)).await;
    let capacity = sdk.host_capacity().await.unwrap();
    assert_eq!(capacity.max_concurrent_runs, 4);
    assert_eq!(capacity.active_runs, 1);
    assert_eq!(capacity.available, 3);
    assert_eq!(capacity.queued_runs, 0);
    assert_eq!(capacity.queue_limit, 32);
    assert_eq!(capacity.health.lagged_live_events, 2);
    assert!(capacity.health.event_journal_ok);
    assert!(!capacity.health.run_persistence_ok);
    assert!(capacity.health.native_executor_ok);
    let encoded = serde_json::to_value(&capacity).unwrap();
    assert_no_sensitive(&encoded);
    assert!(
        encoded
            .get("health")
            .unwrap()
            .get("nativeExecutor")
            .is_none()
    );
}

#[tokio::test]
async fn host_wire_still_receives_workspace_token_from_opaque_ref() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, default_handler);
    let (sdk, calls) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let scope = SessionScope::from_session(&session);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    calls.lock().unwrap().clear();
    assert_eq!(
        sdk.list_runs(&scope).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert_eq!(
        sdk.observe_run(&selector).await.unwrap_err(),
        SdkError::Unsupported
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "legacy list_runs/observe_run must not call transport"
    );
    let _ = sdk
        .stream_public_events(&selector, EventQuery::default())
        .await
        .unwrap();
    let log = calls.lock().unwrap().clone();
    assert!(
        log.iter()
            .all(|(name, _)| name != "ptah_list_runs" && name != "ptah_get_run"),
        "legacy methods must not reach public run tools: {log:?}"
    );
    let get_events = log
        .iter()
        .find(|(name, _)| name == "ptah_get_events")
        .map(|(_, args)| args);
    assert_eq!(get_events.unwrap()["workspace"], BUILD_CWD);
    assert_eq!(get_events.unwrap()["session_id"], BUILD_SESSION);
    assert_eq!(get_events.unwrap()["run_id"], RUN_ID);
}

#[tokio::test]
async fn scope_denial_unknown_and_cross_session_are_equal() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, |name, arguments| {
        if name == "ptah_get_run" || name == "ptah_get_events" {
            let run_id = arguments
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let session_id = arguments
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if run_id == "no-such-run" {
                return Err(TransportError::from_host_data(
                    &json!({"code": "invalid_request"}),
                ));
            }
            if session_id != BUILD_SESSION {
                return Err(TransportError::from_host_data(
                    &json!({"code": "forbidden_scope"}),
                ));
            }
        }
        default_handler(name, arguments)
    });
    let (sdk, _) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let unknown = sdk
        .observe_public_run(&RunSelector::from_parts(
            &session,
            RunId::new("no-such-run"),
        ))
        .await
        .unwrap_err();
    let mut foreign = session.clone();
    foreign.session_id = SessionId::new(CHAT_SESSION);
    let cross = sdk
        .observe_public_run(&RunSelector::from_parts(&foreign, RunId::new(RUN_ID)))
        .await
        .unwrap_err();
    assert_eq!(unknown, SdkError::ForbiddenScope);
    assert_eq!(cross, SdkError::ForbiddenScope);
    assert_eq!(unknown, cross);
    assert_eq!(unknown.code(), "forbidden_scope");
}

#[tokio::test]
async fn cursor_expired_carries_host_event_range() {
    let range = json!({ "startSeq": 12, "endSeq": 40 });
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, move |name, arguments| {
        if name == "ptah_get_events" {
            return Err(TransportError::from_host_data(&json!({
                "code": "cursor_expired",
                "eventRange": range
            })));
        }
        default_handler(name, arguments)
    });
    let (sdk, _) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let err = sdk
        .stream_public_events(
            &RunSelector::from_parts(&session, RunId::new(RUN_ID)),
            EventQuery {
                after_seq: Some(0),
                limit: Some(20),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        SdkError::CursorExpired {
            event_range: Some(RetainedRange {
                start_seq: 12,
                end_seq: Some(40),
            }),
        }
    );
}

#[tokio::test]
async fn event_page_bounds_are_the_host_1_to_500_range() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, default_handler);
    let (sdk, calls) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    assert_eq!(
        sdk.stream_public_events(
            &selector,
            EventQuery {
                after_seq: None,
                limit: Some(0),
            },
        )
        .await
        .unwrap_err(),
        SdkError::InvalidRequest
    );
    assert_eq!(
        sdk.stream_public_events(
            &selector,
            EventQuery {
                after_seq: None,
                limit: Some(EVENT_PAGE_LIMIT_MAX + 1),
            },
        )
        .await
        .unwrap_err(),
        SdkError::InvalidRequest
    );
    assert_eq!(EVENT_PAGE_LIMIT_MIN, 1);
    assert_eq!(EVENT_PAGE_LIMIT_MAX, 500);
    assert_eq!(EVENT_PAGE_LIMIT_DEFAULT, 50);
    let names: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    assert!(!names.iter().any(|name| name == "ptah_get_events"));
}

#[tokio::test]
async fn event_query_sends_host_after_seq_and_limit() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, default_handler);
    let (sdk, calls) = connect(transport).await;
    let session = sdk.list_sessions().await.unwrap().remove(0);
    let selector = RunSelector::from_parts(&session, RunId::new(RUN_ID));
    sdk.stream_public_events(
        &selector,
        EventQuery {
            after_seq: Some(7),
            limit: Some(1),
        },
    )
    .await
    .unwrap();
    sdk.stream_public_events(&selector, EventQuery::default())
        .await
        .unwrap();
    let log = calls.lock().unwrap().clone();
    let pages: Vec<&Value> = log
        .iter()
        .filter(|(name, _)| name == "ptah_get_events")
        .map(|(_, args)| args)
        .collect();
    assert_eq!(pages[0]["after_seq"], 7);
    assert_eq!(pages[0]["limit"], 1);
    assert!(pages[1].get("after_seq").is_none());
    assert_eq!(pages[1]["limit"], EVENT_PAGE_LIMIT_DEFAULT);
}

#[tokio::test]
async fn transport_and_auth_errors_map_one_to_one() {
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
            TransportError::from_host_data(&json!({"code": "conflict"})),
            SdkError::Conflict,
        ),
        (
            TransportError::from_host_data(&json!({"code": "session_busy"})),
            SdkError::Conflict,
        ),
        (
            TransportError::from_host_data(&json!({"code": "stale_version"})),
            SdkError::Conflict,
        ),
        (
            TransportError::from_host_data(&json!({"code": "mystery_code"})),
            SdkError::Internal,
        ),
    ];
    for (transport_error, expected) in cases {
        let err = transport_error.clone();
        let transport = ScriptedTransport::new(ALL_READ_TOOLS, move |name, arguments| {
            if name == "ptah_get_capacity" {
                Err(err.clone())
            } else {
                default_handler(name, arguments)
            }
        });
        let (sdk, _) = connect(transport).await;
        assert_eq!(sdk.host_capacity().await.unwrap_err(), expected);
    }

    let mut list_fail = ScriptedTransport::new(ALL_READ_TOOLS, default_handler);
    list_fail.list_error = Some(TransportError::Unauthenticated);
    let connect_err = ReadObservatory::connect(list_fail).await.unwrap_err();
    assert_eq!(connect_err, SdkError::Unauthenticated);
}

#[tokio::test]
async fn mcp_structured_content_envelope_is_unwrapped() {
    let transport = ScriptedTransport::new(ALL_READ_TOOLS, |name, arguments| {
        default_handler(name, arguments).map(|body| {
            json!({
                "content": [{ "type": "text", "text": "{}" }],
                "structuredContent": body,
                "isError": false
            })
        })
    });
    let (sdk, _) = connect(transport).await;
    let sessions = sdk.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
}

#[tokio::test]
async fn host_data_codes_match_sdk_error_codes() {
    for (code, expected) in [
        ("unauthenticated", SdkError::Unauthenticated),
        ("forbidden_scope", SdkError::ForbiddenScope),
        ("workspace_mismatch", SdkError::WorkspaceMismatch),
        ("invalid_request", SdkError::InvalidRequest),
        ("unsupported", SdkError::Unsupported),
        ("conflict", SdkError::Conflict),
        ("timeout", SdkError::Timeout),
        ("capacity_exhausted", SdkError::CapacityExhausted),
        ("internal", SdkError::Internal),
    ] {
        assert_eq!(
            SdkError::from(TransportError::from_host_data(&json!({ "code": code }))),
            expected
        );
        assert_eq!(expected.code(), code);
    }
    assert_eq!(
        SdkError::from(TransportError::from_host_data(&json!({
            "code": "stale_version"
        }))),
        SdkError::Conflict
    );
    let expired = SdkError::from(TransportError::from_host_data(&json!({
        "code": "cursor_expired",
        "eventRange": { "startSeq": 2, "endSeq": 9 }
    })));
    assert_eq!(expired.code(), "cursor_expired");
}
