//! Service-adapter tests: SDK ↔ bridge wire fidelity, and the public consumer
//! parity check.
//!
//! Every response body and argument schema in this file is transcribed from
//! `grokptah-agent-bridge` at commit `67e29bd` (the base of this branch), with
//! the source location named at each fixture. The adapter is not linked
//! against the bridge on purpose: doing so would compile a ~99k-line runtime,
//! its keychain, and its HTTP stack into this contract-only crate's test
//! build, and would re-create the dependency the seam exists to remove. What
//! that trade costs is stated plainly in `docs/AGENT_SDK_SEAM.md`: these
//! fixtures pin the shapes, and only a live two-host run of the battery closes
//! the loop.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use grokptah_agent_sdk::conformance::{self, CheckOutcome, Harness};
use grokptah_agent_sdk::prelude::*;
use serde_json::{json, Value};

// ── The bridge's advertised argument schema ───────────────────────────────
//
// Source: `crates/codegen/grokptah-agent-bridge/src/mcp_control.rs`,
// `fn tool_input_schema`. `required` and `properties` are copied verbatim;
// every schema there is `additionalProperties: false`.

struct ToolSchema {
    required: &'static [&'static str],
    allowed: &'static [&'static str],
}

fn advertised_schema(tool: &str) -> ToolSchema {
    match tool {
        "ptah_list_sessions" => ToolSchema {
            required: &[],
            allowed: &[],
        },
        "ptah_create_session" => ToolSchema {
            required: &["workspace"],
            allowed: &["workspace", "title", "request_id"],
        },
        "ptah_get_run" | "ptah_get_test_results" => ToolSchema {
            required: &["session_id", "workspace", "run_id"],
            allowed: &["session_id", "workspace", "run_id"],
        },
        "ptah_list_receipts" => ToolSchema {
            required: &["session_id", "workspace", "run_id"],
            allowed: &["session_id", "workspace", "run_id", "after", "limit"],
        },
        "ptah_get_events" => ToolSchema {
            required: &["session_id", "workspace", "run_id"],
            allowed: &["session_id", "workspace", "run_id", "after_seq", "limit"],
        },
        "ptah_submit_task" => ToolSchema {
            required: &["request_id", "session_id", "workspace", "prompt"],
            allowed: &[
                "request_id",
                "session_id",
                "workspace",
                "prompt",
                "bounds",
                "execution_mode",
                "allow_queue",
            ],
        },
        "ptah_steer" => ToolSchema {
            required: &["request_id", "session_id", "workspace", "text"],
            allowed: &["request_id", "session_id", "workspace", "text"],
        },
        "ptah_cancel" => ToolSchema {
            required: &["request_id", "session_id", "workspace", "run_id"],
            allowed: &["request_id", "session_id", "workspace", "run_id"],
        },
        "ptah_claim_work" => ToolSchema {
            required: &["request_id", "session_id", "workspace", "work_id"],
            allowed: &[
                "request_id",
                "session_id",
                "workspace",
                "work_id",
                "lease_ms",
                "agent_id",
            ],
        },
        "ptah_release_work" => ToolSchema {
            required: &[
                "request_id",
                "session_id",
                "workspace",
                "work_id",
                "attempt_id",
                "lease_token",
                "reason",
            ],
            allowed: &[
                "request_id",
                "session_id",
                "workspace",
                "work_id",
                "attempt_id",
                "lease_token",
                "reason",
            ],
        },
        other => panic!("adapter called a tool outside the mapped set: {other}"),
    }
}

/// The advertised `bounds` sub-schema, mirroring the host.
///
/// `maxTotalTokens` is present now. It was not before this branch: the runtime
/// accepted it in `merge_bounds` and the coordinator docs documented it, but
/// `tool_input_schema` omitted it while declaring `additionalProperties:
/// false` — so a schema-validating client was refused the one ceiling it most
/// needed. The host schema and this mirror were fixed together.
const ADVERTISED_BOUNDS_KEYS: &[&str] = &[
    "maxPromptBytes",
    "maxRounds",
    "maxDurationMs",
    "maxTotalTokens",
];

fn assert_schema_conformant(tool: &str, args: &Value) {
    let schema = advertised_schema(tool);
    let object = args.as_object().expect("arguments must be an object");
    for key in schema.required {
        assert!(
            object.contains_key(*key),
            "{tool} is missing required argument {key}: {args}"
        );
    }
    for key in object.keys() {
        assert!(
            schema.allowed.contains(&key.as_str()),
            "{tool} sent {key}, which the advertised schema forbids: {args}"
        );
    }
}

// ── Wire-level bridge double ──────────────────────────────────────────────

const WORKSPACE: &str = "/srv/grokptah/allowlisted-project";
const OTHER_WORKSPACE: &str = "/srv/grokptah/second-project";
const SESSION: &str = "11111111-2222-4333-8444-555555555555";
const SECRET_PROMPT: &str = "SECRET-PROMPT-do-not-echo";
const SECRET_RESPONSE: &str = "SECRET-FINAL-RESPONSE-do-not-echo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    Unreachable,
    Unauthenticated,
    CursorExpired,
}

#[derive(Default)]
struct DoubleState {
    calls: Vec<(String, Value)>,
    receipts: BTreeMap<String, Value>,
    runs: BTreeMap<String, Value>,
    leases: BTreeMap<String, (String, String)>,
    next_id: u64,
    fault: Option<(String, Fault)>,
}

/// Emulates the `ptah_*` wire contract for the mapped tools.
///
/// It enforces the same gates the runtime does — allowlist before scope,
/// identical denials for unknown/cross-scope, idempotency by `request_id` —
/// so an adapter that skips one fails here rather than in production.
struct BridgeDouble {
    tools: Vec<String>,
    state: Mutex<DoubleState>,
}

impl BridgeDouble {
    fn new() -> Self {
        Self {
            tools: [
                "ptah_list_sessions",
                "ptah_create_session",
                "ptah_submit_task",
                "ptah_get_run",
                "ptah_get_events",
                "ptah_get_test_results",
                "ptah_steer",
                "ptah_cancel",
                "ptah_claim_work",
                "ptah_release_work",
                // Present on the host, deliberately unmapped by the adapter.
                "ptah_authorize_work_execution",
                "ptah_create_manager_plan",
                "ptah_approve_run",
                "ptah_promote_run",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            state: Mutex::new(DoubleState::default()),
        }
    }

    fn without_tools(mut self, drop: &[&str]) -> Self {
        self.tools.retain(|tool| !drop.contains(&tool.as_str()));
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DoubleState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn inject(&self, tool: &str, fault: Fault) {
        self.lock().fault = Some((tool.to_string(), fault));
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.lock().calls.clone()
    }

    fn calls_to(&self, tool: &str) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, args)| args)
            .collect()
    }

    fn call_count(&self) -> usize {
        self.lock().calls.len()
    }
}

/// `json_err` in `src/mcp_control.rs`: `error.data.code` is the typed code and
/// any extra `OrchError::data` fields are merged into the same object.
fn rpc(code: &str, message: &str) -> TransportFault {
    TransportFault::from_jsonrpc_error(&json!({
        "code": -32000,
        "message": message,
        "data": { "code": code },
    }))
}

/// The single denial the runtime uses for unknown, cross-session, and
/// cross-workspace resources alike.
fn scope_denied() -> TransportFault {
    rpc("forbidden_scope", "run is not available to this session")
}

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

#[async_trait]
impl McpTransport for BridgeDouble {
    async fn list_tools(&self) -> Result<Vec<String>, TransportFault> {
        Ok(self.tools.clone())
    }

    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, TransportFault> {
        // Fidelity is enforced on every call the battery and the focused tests
        // make, not only where a test looks.
        assert_schema_conformant(tool, &arguments);
        if let Some(bounds) = arguments.get("bounds").and_then(Value::as_object) {
            for key in bounds.keys() {
                assert!(
                    ADVERTISED_BOUNDS_KEYS.contains(&key.as_str()),
                    "unexpected bounds key {key}"
                );
            }
        }

        let mut state = self.lock();
        state.calls.push((tool.to_string(), arguments.clone()));
        if let Some((target, fault)) = state.fault.clone() {
            if target == tool {
                state.fault = None;
                return Err(match fault {
                    Fault::Unreachable => TransportFault::Unreachable {
                        detail: "connection to the agent host was lost".into(),
                    },
                    Fault::Unauthenticated => rpc("unauthenticated", "invalid bearer token"),
                    Fault::CursorExpired => rpc(
                        "cursor_expired",
                        "event cursor expired; restart from seq 0 or latest",
                    ),
                });
            }
        }

        // Allowlist first, session-independent — as `authorize_computer_scope`
        // and `require_workspace_match` do in the runtime.
        if let Some(workspace) = arg(&arguments, "workspace") {
            if workspace != WORKSPACE && workspace != OTHER_WORKSPACE {
                return Err(rpc("workspace_mismatch", "workspace not in allowlist"));
            }
        }
        if arg(&arguments, "session_id").is_some_and(|session| session != SESSION) {
            return Err(scope_denied());
        }

        // Durable idempotency receipts: same key replays byte-for-byte.
        if let Some(request_id) = arg(&arguments, "request_id").map(str::to_string) {
            if let Some(prior) = state.receipts.get(&request_id) {
                let prior = prior.clone();
                if prior["payload"] != arguments {
                    return Err(rpc(
                        "conflict",
                        "requestId was already used with a different payload",
                    ));
                }
                return Ok(prior["response"].clone());
            }
        }

        let response = match tool {
            // `OrchestrationService::list_sessions`
            "ptah_list_sessions" => json!({
                "sessions": [{
                    "sessionId": SESSION,
                    "title": "seeded",
                    "kind": "build",
                    "cwd": WORKSPACE,
                    "workspaceStatus": "ready",
                    "updatedAt": "2026-01-01T00:00:00Z",
                    "busy": false,
                }]
            }),
            // `OrchestrationService::create_session`
            "ptah_create_session" => json!({
                "sessionId": SESSION,
                "title": arguments.get("title").cloned().unwrap_or(Value::Null),
                "workspace": arg(&arguments, "workspace").unwrap_or(WORKSPACE),
                "updatedAt": "2026-01-01T00:00:05Z",
                "busy": false,
            }),
            // `submit_task_with_execution_mode_and_queue_parent`
            "ptah_submit_task" => {
                state.next_id += 1;
                let run_id = format!("run-{:04}", state.next_id);
                let queued = arguments
                    .get("allow_queue")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                state
                    .runs
                    .insert(run_id.clone(), run_record(&run_id, "queued"));
                json!({
                    "runId": run_id,
                    "sessionId": SESSION,
                    "state": "queued",
                    "requestId": arg(&arguments, "request_id").unwrap_or_default(),
                    "executionMode": arguments.get("execution_mode").cloned()
                        .unwrap_or(Value::String("shared".into())),
                    "queuedPosition": if queued { json!(1) } else { Value::Null },
                })
            }
            // `run_value` returns the complete durable `RunRecord`.
            "ptah_get_run" => {
                let run_id = arg(&arguments, "run_id").unwrap_or_default();
                state.runs.get(run_id).cloned().ok_or_else(scope_denied)?
            }
            // `events_page_for_run` -> `JournalPage`
            "ptah_get_events" => {
                let run_id = arg(&arguments, "run_id").unwrap_or_default();
                if !state.runs.contains_key(run_id) {
                    return Err(scope_denied());
                }
                let after = arguments
                    .get("after_seq")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100) as usize;
                let all = journal();
                let entries: Vec<Value> = all
                    .into_iter()
                    .filter(|entry| entry["seq"].as_u64().unwrap_or(0) > after)
                    .take(limit)
                    .collect();
                let next = entries.last().and_then(|entry| entry["seq"].as_u64());
                json!({
                    "entries": entries,
                    "nextCursor": next,
                    "cursorExpired": false,
                })
            }
            // `test_results_for_run`
            "ptah_get_test_results" => {
                let run_id = arg(&arguments, "run_id").unwrap_or_default();
                if !state.runs.contains_key(run_id) {
                    return Err(scope_denied());
                }
                json!({
                    "runId": run_id,
                    "tests": [{
                        "callId": "call-1",
                        // An absolute path inside a command string: the adapter
                        // must not carry it into the artifact body.
                        "command": "cargo test --manifest-path /srv/grokptah/allowlisted-project/Cargo.toml",
                        "status": "ended",
                        "exitCode": 0,
                        "cancelled": false,
                    }]
                })
            }
            // `OrchestrationService::steer`
            "ptah_steer" => json!({
                "requestId": arg(&arguments, "request_id").unwrap_or_default(),
                "actionId": arg(&arguments, "request_id").unwrap_or_default(),
                "sessionId": SESSION,
                "workspace": WORKSPACE,
                "origin": "mcp",
                "action": "steer_now",
                "disposition": "queued",
                "actionVersion": 1,
                "revision": 2,
                "entries": [],
            }),
            // `OrchestrationService::cancel` — note the constant `state`.
            "ptah_cancel" => {
                let run_id = arg(&arguments, "run_id").unwrap_or_default().to_string();
                let was_queued = state
                    .runs
                    .get(&run_id)
                    .and_then(|run| run["state"].as_str())
                    .map(|state| state == "queued")
                    .ok_or_else(scope_denied)?;
                state
                    .runs
                    .insert(run_id.clone(), run_record(&run_id, "cancelled"));
                json!({
                    "requestId": arg(&arguments, "request_id").unwrap_or_default(),
                    "sessionId": SESSION,
                    "runId": run_id,
                    "cancelled": true,
                    "wasQueued": was_queued,
                    "teardownComplete": true,
                    "state": "cancelled",
                })
            }
            // `OrchestrationService::claim_work`
            "ptah_claim_work" => {
                let work_id = arg(&arguments, "work_id").unwrap_or_default().to_string();
                if state.leases.contains_key(&work_id) {
                    return Err(rpc("conflict", "work item already has an active lease"));
                }
                state.next_id += 1;
                let attempt_id = format!("attempt-{:04}", state.next_id);
                let token = format!("lease-secret-{attempt_id}");
                state
                    .leases
                    .insert(work_id.clone(), (attempt_id.clone(), token.clone()));
                json!({
                    "work": { "workId": work_id, "state": "leased" },
                    "attempt": {
                        "schemaVersion": 1,
                        "attemptId": attempt_id,
                        "workId": work_id,
                        "attemptNumber": 1,
                        "claimantId": arg(&arguments, "agent_id").unwrap_or_default(),
                        "acquiredAt": "2026-01-01T00:00:10Z",
                        "leaseExpiresAt": "2026-01-01T00:00:40Z",
                        "lastHeartbeatAt": "2026-01-01T00:00:10Z",
                        "state": "leased",
                        "linkedRunIds": [],
                        "createdAt": "2026-01-01T00:00:10Z",
                        "updatedAt": "2026-01-01T00:00:10Z",
                    },
                    "leaseToken": token,
                })
            }
            // `OrchestrationService::release_work`
            "ptah_release_work" => {
                let work_id = arg(&arguments, "work_id").unwrap_or_default().to_string();
                let token = arg(&arguments, "lease_token")
                    .unwrap_or_default()
                    .to_string();
                let held = state
                    .leases
                    .get(&work_id)
                    .cloned()
                    .ok_or_else(scope_denied)?;
                if held.1 != token {
                    return Err(scope_denied());
                }
                state.leases.remove(&work_id);
                json!({
                    "work": { "workId": work_id, "state": "queued" },
                    "attempt": {
                        "attemptId": held.0,
                        "state": "released",
                        "updatedAt": "2026-01-01T00:00:20Z",
                    }
                })
            }
            other => panic!("bridge double received an unmapped tool: {other}"),
        };

        if let Some(request_id) = arg(&arguments, "request_id").map(str::to_string) {
            state.receipts.insert(
                request_id,
                json!({ "payload": arguments, "response": response }),
            );
        }
        Ok(response)
    }
}

/// A durable `RunRecord` as `run_value` serializes it — including the fields
/// the public projection must drop.
fn usage_block() -> Value {
    json!({
        "promptTokens": 100,
        "completionTokens": 40,
        "totalTokens": 140,
        "requests": 2,
    })
}

fn observations_block() -> Value {
    json!({
        "changedFiles": 2,
        "testsObserved": 1,
        "testsPassed": 1,
        "testsFailed": 0,
        "testsIncomplete": 0,
        "permissionsRequested": 0,
        "permissionsGranted": 0,
        "permissionsDenied": 0,
        "permissionsUnresolved": 0,
    })
}

fn verification_block() -> Value {
    json!({
        "status": "verified",
        "stopReason": "completed",
        "interrupted": false,
        "claims": { "present": true },
        "observations": observations_block(),
        "usage": usage_block(),
    })
}

fn aggregates_block(terminal: bool) -> Value {
    json!({
        "changes": [
            { "path": "src/lib.rs", "summary": "edited" },
            // An absolute path recorded upstream: dropped, not surfaced.
            { "path": "/srv/grokptah/allowlisted-project/src/other.rs", "summary": "edited" },
        ],
        "tests": [],
        "permissionsRequested": 0,
        "permissionsGranted": 0,
        "permissionsDenied": 0,
        "usage": usage_block(),
        "usageComplete": true,
        "usagePendingRequests": 0,
        "verification": if terminal { verification_block() } else { Value::Null },
    })
}

/// A durable `RunRecord` as `run_value` serializes it — including the fields
/// the public projection must drop.
fn run_record(run_id: &str, state: &str) -> Value {
    let terminal = state != "queued" && state != "running";
    let stop_cause = match state {
        "cancelled" => json!("cancelled"),
        _ if terminal => json!("completed"),
        _ => Value::Null,
    };
    json!({
        "runId": run_id,
        "sessionId": SESSION,
        "workspace": WORKSPACE,
        "requestId": "req-0001",
        "clientId": "desktop",
        "state": state,
        "purpose": "execution",
        "bounds": {
            "maxPromptBytes": 100000,
            "maxRounds": 24,
            "maxDurationMs": 900000,
        },
        "promptPreview": SECRET_PROMPT,
        "startSeq": 1,
        "endSeq": if terminal { json!(6) } else { Value::Null },
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": if terminal { "2026-01-01T00:00:30Z" } else { "2026-01-01T00:00:00Z" },
        "terminalResult": if terminal { json!("completed") } else { Value::Null },
        "finalResponse": if terminal { json!(SECRET_RESPONSE) } else { Value::Null },
        "errorCode": Value::Null,
        "stopCause": stop_cause,
        "aggregates": aggregates_block(terminal),
        "progress": {
            "round": 1,
            "maxRounds": 24,
            "detail": "working",
            "updatedAt": "2026-01-01T00:00:02Z",
        },
    })
}

/// A journal page. `SessionUpdate` sets `rename_all` for variants only
/// (`src/events.rs`), so update fields are plain snake_case on the wire.
fn journal() -> Vec<Value> {
    vec![
        json!({ "seq": 1, "ts": "2026-01-01T00:00:01Z",
                "update": { "type": "turn_started", "session_id": SESSION, "turn_id": SESSION } }),
        json!({ "seq": 2, "ts": "2026-01-01T00:00:01Z",
                "update": { "type": "agent_message_chunk", "session_id": SESSION, "text": SECRET_RESPONSE } }),
        json!({ "seq": 3, "ts": "2026-01-01T00:00:02Z",
                "update": { "type": "agent_progress", "session_id": SESSION, "round": 1,
                            "max_rounds": 24, "last_tool": "edit", "detail": "working" } }),
        json!({ "seq": 4, "ts": "2026-01-01T00:00:03Z",
                "update": { "type": "tool_call", "session_id": SESSION, "call_id": "call-1",
                            "title": "edit", "kind": "edit", "status": "completed",
                            "input": { "path": "/srv/grokptah/allowlisted-project/src/lib.rs" } } }),
        json!({ "seq": 5, "ts": "2026-01-01T00:00:03Z",
                "update": { "type": "file_edit", "session_id": SESSION, "path": "src/lib.rs",
                            "summary": "edited", "unified_diff": "SECRET-DIFF-BODY" } }),
        json!({ "seq": 6, "ts": "2026-01-01T00:00:04Z",
                "update": { "type": "shell_output", "session_id": SESSION, "call_id": "call-1",
                            "chunk": "SECRET-SHELL-OUTPUT" } }),
    ]
}

// ── Fixtures ──────────────────────────────────────────────────────────────

/// An operator-authority plane over a double the caller keeps a handle to.
///
/// The plane no longer hands its transport back — that accessor let any holder
/// call the host directly and skip every gate in the crate — so a test that
/// wants to inspect what was sent keeps its own `Arc` instead.
fn operator(double: BridgeDouble) -> (ServiceControlPlane<Arc<BridgeDouble>>, Arc<BridgeDouble>) {
    let double = Arc::new(double);
    (
        ServiceControlPlane::read_only(Arc::clone(&double)).with_operator_authority(),
        double,
    )
}

fn request_id(n: u64) -> RequestId {
    RequestId::new(format!("req-{n:04}")).expect("minted id is valid")
}

async fn seeded(plane: &ServiceControlPlane<Arc<BridgeDouble>>) -> SessionView {
    plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list sessions")
        .items
        .into_iter()
        .next()
        .expect("the double seeds one session")
}

fn submission(session: &SessionView, n: u64) -> TaskSubmission {
    TaskSubmission {
        request_id: request_id(n),
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        prompt: SECRET_PROMPT.into(),
        bounds: None,
        execution_mode: ExecutionMode::Shared,
        allow_queue: false,
    }
}

// ── Host-owned registry ───────────────────────────────────────────────────

#[tokio::test]
async fn a_workspace_ref_can_only_come_from_the_host() {
    let double = Arc::new(BridgeDouble::new());
    let plane = ServiceControlPlane::read_only(Arc::clone(&double));
    assert_eq!(plane.known_workspaces(), 0);

    // A well-formed ref the host never reported resolves to a workspace
    // mismatch, and the transport is never touched.
    let forged = WorkspaceRef::new("ws-0123456789abcdef").expect("valid ref");
    let error = plane
        .observe_run(RunSelector {
            session_id: SessionId::new(SESSION).unwrap(),
            workspace: forged,
            run_id: RunId::new("run-0001").unwrap(),
        })
        .await
        .expect_err("an unlearned ref must fail closed");
    assert_eq!(error.code, SdkErrorCode::WorkspaceMismatch);

    let session = seeded(&plane).await;
    assert_eq!(plane.known_workspaces(), 1);
    assert!(session.workspace.as_str().starts_with("ws-"));
}

#[tokio::test]
async fn no_absolute_path_survives_the_session_projection() {
    let double = Arc::new(BridgeDouble::new());
    let plane = ServiceControlPlane::read_only(Arc::clone(&double));
    let session = seeded(&plane).await;
    let encoded = serde_json::to_string(&session).expect("serialize session");
    assert!(!encoded.contains(WORKSPACE), "{encoded}");
    assert!(!encoded.contains("/srv/"), "{encoded}");
}

// ── Argument fidelity against the advertised schema ───────────────────────

#[tokio::test]
async fn every_call_binds_the_exact_scope_the_bridge_requires() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };
    plane.observe_run(selector.clone()).await.expect("observe");
    plane
        .stream_events(selector.clone(), PageRequest::new().limit(10))
        .await
        .expect("events");

    // `assert_schema_conformant` already ran inside the transport for every
    // call; this pins the scope triple explicitly.
    for tool in ["ptah_get_run", "ptah_get_events"] {
        let args = double.calls_to(tool);
        assert_eq!(args.len(), 1, "{tool}");
        assert_eq!(args[0]["session_id"], json!(SESSION));
        assert_eq!(args[0]["workspace"], json!(WORKSPACE));
        assert_eq!(args[0]["run_id"], json!(accepted.run_id.as_str()));
    }
}

#[tokio::test]
async fn a_token_ceiling_travels_outside_the_advertised_bounds_schema() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    plane
        .submit_task(TaskSubmission {
            bounds: Some(RunBoundsRequest {
                max_rounds: Some(8),
                max_duration_ms: None,
                max_total_tokens: Some(50_000),
            }),
            ..submission(&session, 1)
        })
        .await
        .expect("submit");

    let args = double.calls_to("ptah_submit_task");
    let bounds = args[0]["bounds"].as_object().expect("bounds sent");
    assert_eq!(bounds["maxRounds"], json!(8));
    assert_eq!(bounds["maxTotalTokens"], json!(50_000));
    assert!(
        !bounds.contains_key("maxDurationMs"),
        "unset bounds stay unset"
    );
    // Previously a documented divergence: the runtime accepted this key while
    // the advertised schema omitted it. Both now agree.
    assert!(ADVERTISED_BOUNDS_KEYS.contains(&"maxTotalTokens"));
}

// ── Redaction ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_durable_record_is_projected_not_forwarded() {
    let (plane, _double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };
    // Move the run terminal so the record carries a final response.
    plane
        .cancel_run(CancelRequest {
            request_id: request_id(2),
            selector: selector.clone(),
        })
        .await
        .expect("cancel");

    let view = plane.observe_run(selector).await.expect("observe");
    let encoded = serde_json::to_string(&view).expect("serialize view");

    for leak in [
        SECRET_PROMPT,
        SECRET_RESPONSE,
        WORKSPACE,
        "/srv/",
        "clientId",
        "requestId",
    ] {
        assert!(!encoded.contains(leak), "{leak} leaked into {encoded}");
    }
    // The evidence the contract does carry survived.
    assert_eq!(view.usage.total_tokens, 140);
    assert!(view.usage.complete);
    // The absolute changed-file path was dropped; the relative one kept.
    assert_eq!(view.changed_files.len(), 1);
    assert_eq!(view.changed_files[0].path.as_str(), "src/lib.rs");
}

#[tokio::test]
async fn event_pages_carry_no_transcript_and_still_advance() {
    let (plane, _double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };

    let page = plane
        .stream_events(selector, PageRequest::new().limit(6))
        .await
        .expect("events");
    let encoded = serde_json::to_string(&page).expect("serialize page");
    for leak in [
        SECRET_RESPONSE,
        "SECRET-DIFF-BODY",
        "SECRET-SHELL-OUTPUT",
        "/srv/",
    ] {
        assert!(!encoded.contains(leak), "{leak} leaked into {encoded}");
    }

    // Six raw entries, three of them transcript. The surviving events keep
    // their durable sequence, so a consumer's cursor still walks the journal.
    assert_eq!(page.items.len(), 4);
    let cursors: Vec<&str> = page.items.iter().map(|e| e.cursor.as_str()).collect();
    assert_eq!(cursors, vec!["1", "3", "4", "5"]);
}

#[tokio::test]
async fn a_page_of_pure_transcript_does_not_stall_the_cursor() {
    // The bridge journal is filtered by the *host* to a run range, so a page
    // can legitimately contain nothing this contract carries. The cursor must
    // still come from the raw page or paging deadlocks.
    let (plane, _double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };

    // seq 2 alone is an agent_message_chunk.
    let page = plane
        .stream_events(
            selector,
            PageRequest::new().after(Cursor::from_opaque("1")).limit(1),
        )
        .await
        .expect("events");
    assert!(
        page.items.is_empty(),
        "transcript-only page carries nothing"
    );
    assert_eq!(
        page.next_cursor.as_ref().map(Cursor::as_str),
        Some("2"),
        "the cursor must advance past dropped entries"
    );
}

#[tokio::test]
async fn the_test_report_artifact_drops_the_command_string() {
    let (plane, _double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");

    let payload = plane
        .fetch_artifact(ArtifactRequest {
            selector: RunSelector {
                session_id: session.session_id.clone(),
                workspace: session.workspace.clone(),
                run_id: accepted.run_id.clone(),
            },
            artifact_id: ArtifactId::new(TEST_REPORT_ARTIFACT_ID).unwrap(),
            max_bytes: None,
        })
        .await
        .expect("fetch");

    payload
        .verify(1024)
        .expect("adapter returns verified bytes");
    assert!(!payload.content.contains("/srv/"), "{}", payload.content);
    assert!(
        !payload.content.contains("manifest-path"),
        "{}",
        payload.content
    );
    assert!(payload.content.contains("call-1"));
    assert_eq!(payload.descriptor.media, ArtifactMedia::Json);
}

// ── Read-only by default ──────────────────────────────────────────────────

#[tokio::test]
async fn a_read_only_adapter_refuses_mutations_without_a_round_trip() {
    let double = Arc::new(BridgeDouble::new());
    let plane = ServiceControlPlane::read_only(Arc::clone(&double));
    let session = seeded(&plane).await;
    let before = double.call_count();

    let submit = plane
        .submit_task(submission(&session, 1))
        .await
        .expect_err("read-only must refuse submission");
    assert_eq!(submit.code, SdkErrorCode::ForbiddenScope);
    assert_eq!(submit.detail("mutationAuthority"), Some("observer"));

    let cancel = plane
        .cancel_run(CancelRequest {
            request_id: request_id(2),
            selector: RunSelector {
                session_id: session.session_id.clone(),
                workspace: session.workspace.clone(),
                run_id: RunId::new("run-0001").unwrap(),
            },
        })
        .await
        .expect_err("read-only must refuse cancellation");
    assert_eq!(cancel.code, SdkErrorCode::ForbiddenScope);

    assert_eq!(
        double.call_count(),
        before,
        "a refused mutation must not reach the host"
    );
    // Reads still work.
    assert_eq!(plane.known_workspaces(), 1);
}

#[tokio::test]
async fn read_only_mode_is_discoverable_before_the_call() {
    let double = Arc::new(BridgeDouble::new());
    let observer = ServiceControlPlane::read_only(Arc::clone(&double));
    let connected = observer.connect().await.expect("connect");
    assert_eq!(
        connected
            .require(&CapabilityId::TaskSubmit)
            .expect_err("mutating capability")
            .code,
        SdkErrorCode::ForbiddenScope
    );
    assert!(connected.require(&CapabilityId::RunObserve).is_ok());

    let (operator, _double) = operator(BridgeDouble::new());
    let connected = operator.connect().await.expect("connect");
    assert!(connected.require(&CapabilityId::TaskSubmit).is_ok());
}

// ── Host-owned tool registry ──────────────────────────────────────────────

#[tokio::test]
async fn capabilities_follow_the_hosts_tool_registry() {
    let (plane, _double) =
        operator(BridgeDouble::new().without_tools(&["ptah_steer", "ptah_claim_work"]));
    let connected = plane.connect().await.expect("connect");

    assert_eq!(
        connected
            .require(&CapabilityId::RunFollowUp)
            .expect_err("host dropped ptah_steer")
            .code,
        SdkErrorCode::Unsupported
    );
    assert_eq!(
        connected
            .require(&CapabilityId::ControlLease)
            .expect_err("host dropped ptah_claim_work")
            .code,
        SdkErrorCode::Unsupported
    );
    assert!(connected.require(&CapabilityId::TaskSubmit).is_ok());
}

#[tokio::test]
async fn tools_the_host_offers_but_the_contract_declines_stay_unmapped() {
    // The double advertises manager, managed-execution, and promotion tools.
    // None of them may become an available capability, and the adapter must
    // never call one — the double panics if it does.
    let (plane, double) = operator(BridgeDouble::new());
    let connected = plane.connect().await.expect("connect");

    for id in [
        CapabilityId::ComputerControl,
        CapabilityId::ProviderCredentials,
    ] {
        assert_eq!(
            connected
                .require(&id)
                .expect_err("permanently forbidden")
                .code,
            SdkErrorCode::ForbiddenScope
        );
    }
    assert_eq!(
        connected
            .require(&CapabilityId::ComputerRead)
            .expect_err("declared but unmapped")
            .code,
        SdkErrorCode::Unsupported
    );

    let session = seeded(&plane).await;
    plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let called: Vec<String> = double.calls().into_iter().map(|(t, _)| t).collect();
    for forbidden in [
        "ptah_authorize_work_execution",
        "ptah_create_manager_plan",
        "ptah_approve_run",
        "ptah_promote_run",
    ] {
        assert!(!called.contains(&forbidden.to_string()), "{forbidden}");
    }
}

// ── Durable attempt receipts ──────────────────────────────────────────────

#[tokio::test]
async fn a_reused_key_replays_instead_of_doing_the_work_twice() {
    let (plane, _double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;

    let first = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("first");
    let replay = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("replay");
    assert_eq!(
        first.run_id, replay.run_id,
        "the key must not create a second run"
    );
    // The control plane replays a stored receipt byte-for-byte, so the adapter
    // reports "cannot tell" rather than asserting freshness either way.
    assert_eq!(replay.replayed, None);

    let conflict = plane
        .submit_task(TaskSubmission {
            prompt: "a different instruction".into(),
            ..submission(&session, 1)
        })
        .await
        .expect_err("same key, new payload");
    assert_eq!(conflict.code, SdkErrorCode::Conflict);
}

#[tokio::test]
async fn a_lost_connection_is_retryable_under_the_same_key() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    double.inject("ptah_submit_task", Fault::Unreachable);

    let key = request_id(1);
    let error = plane
        .submit_task(submission(&session, 1))
        .await
        .expect_err("armed drop");
    assert_eq!(error.code, SdkErrorCode::TransportUnavailable);
    assert_eq!(
        recover_mutation(&key, &error),
        MutationRecovery::RetrySameKey(key.clone())
    );
    plane
        .submit_task(submission(&session, 1))
        .await
        .expect("same key succeeds after reconnect");
}

// ── Error taxonomy ────────────────────────────────────────────────────────

#[tokio::test]
async fn typed_host_codes_cross_the_seam_unchanged() {
    let double = Arc::new(BridgeDouble::new());
    let plane = ServiceControlPlane::read_only(Arc::clone(&double));
    let session = seeded(&plane).await;
    double.inject("ptah_list_sessions", Fault::Unauthenticated);
    let error = plane
        .list_sessions(PageRequest::new())
        .await
        .expect_err("armed auth failure");
    assert_eq!(error.code, SdkErrorCode::Unauthenticated);
    assert_eq!(error.code.origin(), ErrorOrigin::Runtime);

    double.inject("ptah_get_events", Fault::CursorExpired);
    let error = plane
        .stream_events(
            RunSelector {
                session_id: session.session_id.clone(),
                workspace: session.workspace.clone(),
                run_id: RunId::new("run-0001").unwrap(),
            },
            PageRequest::new(),
        )
        .await
        .expect_err("armed cursor expiry");
    assert_eq!(error.code, SdkErrorCode::CursorExpired);
}

#[tokio::test]
async fn unknown_and_cross_workspace_reads_are_indistinguishable() {
    let double = Arc::new(BridgeDouble::new());
    let plane = ServiceControlPlane::read_only(Arc::clone(&double));
    let session = seeded(&plane).await;

    let unknown = plane
        .observe_run(RunSelector {
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            run_id: RunId::new("run-9999").unwrap(),
        })
        .await
        .expect_err("unknown run");
    assert_eq!(unknown.code, SdkErrorCode::ForbiddenScope);

    let cross_session = plane
        .observe_run(RunSelector {
            session_id: SessionId::new("99999999-2222-4333-8444-555555555555").unwrap(),
            workspace: session.workspace.clone(),
            run_id: RunId::new("run-0001").unwrap(),
        })
        .await
        .expect_err("cross-session run");
    assert_eq!(cross_session.code, SdkErrorCode::ForbiddenScope);
    assert_eq!(unknown.message, cross_session.message);
}

// ── Fencing gap surfaced, not hidden ──────────────────────────────────────

#[tokio::test]
async fn a_fence_this_host_cannot_honor_is_refused_not_dropped() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let before = double.call_count();

    let error = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "also check the README".into(),
            expected_revision: Some(Revision::new(1)),
        })
        .await
        .expect_err("ptah_steer has no compare-and-set");
    assert_eq!(error.code, SdkErrorCode::Unsupported);
    assert_eq!(
        double.call_count(),
        before,
        "a fence the host cannot honor must not become an unfenced mutation"
    );

    // Without a fence the follow-up goes through and reports the host's
    // committed revision.
    let receipt = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "also check the README".into(),
            expected_revision: None,
        })
        .await
        .expect("unfenced follow-up");
    assert_eq!(receipt.disposition, FollowUpDisposition::Queued);
    assert_eq!(receipt.revision, Revision::new(2));
}

// ── Lease ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_lease_credential_reaches_the_host_but_not_the_wire_projection() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let work_id = WorkId::new("work-0001").unwrap();

    let lease = plane
        .acquire_control(ControlLeaseRequest {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id: work_id.clone(),
            claimant: AgentId::new("agent-0001").unwrap(),
            requested_ttl_ms: Some(30_000),
        })
        .await
        .expect("claim");

    let secret = lease.credential.reveal().to_string();
    assert!(!secret.is_empty());
    assert!(!serde_json::to_string(&lease).unwrap().contains(&secret));
    assert!(!format!("{lease:?}").contains(&secret));

    // Releasing without the credential fails before the transport is used.
    let calls = double.call_count();
    let error = plane
        .release_control(ReleaseLeaseRequest {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id: work_id.clone(),
            attempt_id: lease.attempt_id.clone(),
            reason: BoundedText::new("done"),
            credential: LeaseCredential::default(),
        })
        .await
        .expect_err("holder-less release");
    assert_eq!(error.code, SdkErrorCode::InvalidRequest);
    assert_eq!(double.call_count(), calls);

    plane
        .release_control(ReleaseLeaseRequest {
            request_id: request_id(3),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id,
            attempt_id: lease.attempt_id.clone(),
            reason: BoundedText::new("done"),
            credential: lease.credential.clone(),
        })
        .await
        .expect("release");

    // The token did reach the host, under the argument name the tool requires.
    let args = double.calls_to("ptah_release_work");
    assert_eq!(args[0]["lease_token"], json!(secret));
    assert_eq!(args[0]["reason"], json!("done"));
}

// ── Receipts are not reachable over this boundary ─────────────────────────

#[tokio::test]
async fn an_older_host_reports_receipts_absent_rather_than_empty() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;
    let accepted = plane
        .submit_task(submission(&session, 1))
        .await
        .expect("submit");
    let before = double.call_count();

    let error = plane
        .list_receipts(
            RunSelector {
                session_id: session.session_id.clone(),
                workspace: session.workspace.clone(),
                run_id: accepted.run_id,
            },
            PageRequest::new(),
        )
        .await
        .expect_err("this double models a host predating the receipt read");

    // Unsupported, not an empty page: a consumer would read emptiness as
    // "no mutations happened", which is the one thing this must never say.
    assert_eq!(error.code, SdkErrorCode::Unsupported);
    assert_eq!(error.detail("capability"), Some("receipt.read"));
    assert_eq!(
        double.call_count(),
        before,
        "refusing a capability the host lacks must not cost a round trip"
    );

    // And it is discoverable before the call.
    let connected = plane.connect().await.expect("connect");
    assert_eq!(
        connected
            .require(&CapabilityId::ReceiptRead)
            .expect_err("unsupported on this host")
            .code,
        SdkErrorCode::Unsupported
    );
}

// ── Public consumer parity ────────────────────────────────────────────────

struct ServiceHarness {
    plane: ServiceControlPlane<Arc<BridgeDouble>>,
    /// The harness keeps its own handle, because the plane no longer hands its
    /// transport back — that accessor was a way past every gate in the crate.
    double: Arc<BridgeDouble>,
    session: SessionView,
    next: AtomicU64,
}

impl ServiceHarness {
    async fn new() -> Self {
        let (plane, double) = operator(BridgeDouble::new());
        let session = seeded(&plane).await;
        Self {
            plane,
            double,
            session,
            next: AtomicU64::new(1),
        }
    }
}

#[async_trait]
impl Harness for ServiceHarness {
    fn plane(&self) -> &dyn AgentControlPlane {
        &self.plane
    }

    async fn owned_session(&self) -> SessionView {
        self.session.clone()
    }

    async fn foreign_workspace(&self) -> Option<WorkspaceRef> {
        // Well-formed, never reported by the host.
        WorkspaceRef::new("ws-ffffffffffffffff").ok()
    }

    async fn arm_lost_connection(&self) -> bool {
        self.double.inject("ptah_get_run", Fault::Unreachable);
        true
    }

    async fn drive_to_completion(&self, _run_id: &RunId) -> bool {
        // The double serves a terminal record for any run it knows once the
        // run has been cancelled or completed; events and the test report are
        // available immediately.
        true
    }

    async fn claimable_work(&self) -> Option<(WorkId, AgentId)> {
        Some((
            WorkId::new("work-battery").ok()?,
            AgentId::new("agent-battery").ok()?,
        ))
    }

    fn next_request_id(&self) -> RequestId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        RequestId::new(format!("battery-{n:04}")).expect("minted id is valid")
    }
}

#[tokio::test]
async fn the_conformance_battery_passes_against_the_service_adapter() {
    let harness = ServiceHarness::new().await;
    let report = conformance::run_battery(&harness).await;
    assert!(report.is_clean(), "{}", report.summary());

    // The service boundary genuinely cannot do some of what the fake can. Each
    // gap must appear as a skip with a reason, never as a silent pass.
    let skipped: Vec<&str> = report.skipped().map(|check| check.name).collect();
    for expected in [
        "faults.uncertain_send_is_never_auto_retried",
        "authz.cross_tenant_read_is_indistinguishable",
        "events.expired_cursor_reports_retained_range",
        "artifacts.digest_mismatch_is_integrity_error",
        "followup.stale_fence_is_rejected_without_effect",
    ] {
        assert!(
            skipped.contains(&expected),
            "expected a skip for {expected}; got {}",
            report.summary()
        );
    }
    assert!(report.passed_count() >= 14, "{}", report.summary());
}

#[tokio::test]
async fn both_adapters_agree_on_the_checks_they_can_both_run() {
    // The point of one matrix over two adapters: where both can run a check,
    // they must agree. Where they cannot, the difference is visible.
    let service = ServiceHarness::new().await;
    let service_report = conformance::run_battery(&service).await;

    let fake_outcomes: BTreeMap<&str, CheckOutcome> = {
        // Rebuild the fake harness inline to keep the two batteries independent.
        struct FakeHarness {
            plane: FakeControlPlane,
            next: AtomicU64,
        }
        #[async_trait]
        impl Harness for FakeHarness {
            fn plane(&self) -> &dyn AgentControlPlane {
                &self.plane
            }
            async fn owned_session(&self) -> SessionView {
                self.plane.seeded_session().expect("seeded")
            }
            async fn drive_to_completion(&self, run_id: &RunId) -> bool {
                self.plane.start_run(run_id).is_ok()
                    && self
                        .plane
                        .finish_run(run_id, ScriptedOutcome::Completed)
                        .is_ok()
            }
            async fn claimable_work(&self) -> Option<(WorkId, AgentId)> {
                Some((
                    WorkId::new("work-battery").ok()?,
                    AgentId::new("agent-battery").ok()?,
                ))
            }
            fn next_request_id(&self) -> RequestId {
                let n = self.next.fetch_add(1, Ordering::SeqCst);
                RequestId::new(format!("battery-{n:04}")).expect("valid")
            }
        }
        let harness = FakeHarness {
            plane: FakeControlPlane::builder().build(),
            next: AtomicU64::new(1),
        };
        conformance::run_battery(&harness)
            .await
            .checks
            .into_iter()
            .map(|check| (check.name, check.outcome))
            .collect()
    };

    let mut compared = 0usize;
    for check in &service_report.checks {
        let Some(fake) = fake_outcomes.get(check.name) else {
            continue;
        };
        if matches!(check.outcome, CheckOutcome::Skipped(_))
            || matches!(fake, CheckOutcome::Skipped(_))
        {
            continue;
        }
        compared += 1;
        assert_eq!(
            &check.outcome, fake,
            "adapters disagree on {}: service={:?} fake={:?}",
            check.name, check.outcome, fake
        );
    }
    assert!(
        compared >= 10,
        "only {compared} checks were comparable across both adapters"
    );
}

/// A host with no idempotency key on session creation makes a dropped create
/// **uncertain**, never safely retryable.
///
/// The SDK used to drop the caller's `request_id` on the floor while
/// `transport_unavailable` stayed classified `Safe`. A consumer following that
/// advice after a disconnect would create a second session. Where the key
/// cannot go on the wire, the honest answer is the one the three-valued retry
/// disposition exists for: this may or may not have applied, so reconcile
/// before retrying.
///
/// `BridgeDouble` advertises no `ptah_get_host_info`, so it stands in for a
/// host that predates the key.
#[tokio::test]
async fn a_dropped_create_is_uncertain_when_the_host_takes_no_key() {
    let (plane, double) = operator(BridgeDouble::new());
    let session = seeded(&plane).await;

    double.inject("ptah_create_session", Fault::Unreachable);

    let error = plane
        .create_session(CreateSessionRequest {
            request_id: RequestId::new("req-create-dropped").unwrap(),
            workspace: session.workspace.clone(),
            title: None,
        })
        .await
        .expect_err("the create was dropped");

    assert_eq!(error.code, SdkErrorCode::UncertainOutcome);
    assert_eq!(
        error.code.retry_disposition(),
        RetryDisposition::Unsafe,
        "a dropped non-idempotent create must never be advertised as retryable"
    );
    // The original cause is preserved for diagnosis without changing the advice.
    assert_eq!(error.detail("originalCode"), Some("transport_unavailable"));
}
