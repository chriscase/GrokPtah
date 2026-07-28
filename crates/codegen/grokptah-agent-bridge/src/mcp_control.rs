//! Loopback-only authenticated MCP control transport (#196).
//!
//! Minimal JSON-RPC 2.0 over HTTP (MCP tools/list + tools/call + initialize).
//! Quarantined: uses only hyper/http primitives already available via axum.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::orchestration::{
    OrchError, OrchErrorCode, OrchestrationService, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};

#[derive(Clone)]
struct AppState {
    orch: Arc<OrchestrationService>,
}

/// Handle for a running control server.
pub struct ControlServerHandle {
    pub addr: SocketAddr,
    pub token: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ControlServerHandle {
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Start loopback MCP control server. Binds `127.0.0.1:port` (0 = ephemeral).
pub async fn start_control_server(
    orch: Arc<OrchestrationService>,
    port: u16,
) -> anyhow::Result<ControlServerHandle> {
    let state = AppState { orch };
    let app = Router::new()
        .route("/", post(rpc_handler))
        .route("/mcp", post(rpc_handler))
        .fallback(fail_closed_fallback)
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_request,
        ))
        .with_state(state);

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let token = String::new(); // caller sets via orch config

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .ok();
    });

    Ok(ControlServerHandle {
        addr,
        token,
        shutdown: Some(tx),
    })
}

async fn authenticate_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match state.orch.auth_header(auth_header) {
        Ok(_) => next.run(request).await,
        Err(error) => json_err(None, StatusCode::UNAUTHORIZED, &error),
    }
}

async fn fail_closed_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "unsupported", "message": "not found"}})),
    )
}

#[derive(Debug, Deserialize)]
struct JsonRpcReq {
    #[serde(default)]
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsCallParams {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsArgs {
    run_id: String,
    #[serde(default)]
    after_seq: u64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    prompt: String,
    #[serde(default)]
    bounds: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    prompt: String,
    #[serde(default)]
    priority: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SteerArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
}

fn empty_object() -> Value {
    json!({})
}

fn default_event_limit() -> usize {
    50
}

#[derive(Debug, Serialize)]
struct JsonRpcResp {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

async fn rpc_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcReq>,
) -> Response {
    // Strict JSON-RPC 2.0 — missing/empty version is rejected (no silent default).
    if req.jsonrpc != "2.0" {
        return json_err(
            req.id,
            StatusCode::BAD_REQUEST,
            &OrchError::new(OrchErrorCode::InvalidRequest, "jsonrpc must be \"2.0\""),
        );
    }
    // Loopback-only is enforced by bind; reject credentials in query (never used).
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let auth = match state.orch.auth_header(auth_header) {
        Ok(a) => a,
        Err(e) => {
            return json_err(req.id, StatusCode::UNAUTHORIZED, &e);
        }
    };

    let method = req.method.as_deref().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "grokptah-control", "version": env!("CARGO_PKG_VERSION") },
        })),
        "notifications/initialized" | "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list_result()),
        "tools/call" => tools_call(&state.orch, &auth, &req.params).await,
        "" => Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "missing method",
        )),
        other => Err(OrchError::new(
            OrchErrorCode::Unsupported,
            format!("unsupported method {other}"),
        )),
    };

    match result {
        Ok(v) => (
            StatusCode::OK,
            Json(JsonRpcResp {
                jsonrpc: "2.0",
                id: req.id,
                result: Some(v),
                error: None,
            }),
        )
            .into_response(),
        Err(e) => json_err(req.id, status_for(&e), &e),
    }
}

fn status_for(e: &OrchError) -> StatusCode {
    match e.code {
        OrchErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        OrchErrorCode::ForbiddenScope | OrchErrorCode::WorkspaceMismatch => StatusCode::FORBIDDEN,
        OrchErrorCode::InvalidRequest | OrchErrorCode::Conflict => StatusCode::BAD_REQUEST,
        OrchErrorCode::Unsupported => StatusCode::METHOD_NOT_ALLOWED,
        OrchErrorCode::CursorExpired => StatusCode::GONE,
        OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn json_err(id: Option<Value>, status: StatusCode, e: &OrchError) -> Response {
    (
        status,
        Json(JsonRpcResp {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({
                "code": -32000,
                "message": e.message,
                "data": { "code": e.code.as_str() },
            })),
        }),
    )
        .into_response()
}

fn tools_list_result() -> Value {
    let tools: Vec<Value> = CONTROL_TOOLS
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "description": format!("GrokPtah control plane tool {name}"),
                "inputSchema": tool_input_schema(name),
            })
        })
        .collect();
    for f in FORBIDDEN_TOOLS {
        assert!(
            !CONTROL_TOOLS.contains(f),
            "forbidden tool leaked into CONTROL_TOOLS"
        );
    }
    json!({ "tools": tools })
}

fn tool_input_schema(name: &str) -> Value {
    let req_id = json!({"type": "string", "minLength": 1, "maxLength": 256});
    let session = json!({"type": "string", "format": "uuid"});
    let workspace = json!({"type": "string", "minLength": 1});
    let run_id = json!({"type": "string", "minLength": 1, "maxLength": 256});
    let bounds = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "maxPromptBytes": {"type": "integer", "minimum": 1},
            "maxRounds": {"type": "integer", "minimum": 1, "maximum": 24},
            "maxDurationMs": {"type": "integer", "minimum": 1}
        }
    });
    match name {
        "ptah_list_sessions" | "ptah_get_capacity" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "ptah_get_run"
        | "ptah_get_progress"
        | "ptah_get_changes"
        | "ptah_get_test_results"
        | "ptah_get_handoff" => json!({
            "type": "object",
            "required": ["run_id"],
            "additionalProperties": false,
            "properties": { "run_id": run_id }
        }),
        "ptah_get_events" => json!({
            "type": "object",
            "required": ["run_id"],
            "additionalProperties": false,
            "properties": {
                "run_id": run_id,
                "after_seq": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}
            }
        }),
        "ptah_submit_task" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "prompt"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "prompt": {"type": "string", "minLength": 1},
                "bounds": bounds
            }
        }),
        "ptah_queue_prompt" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "prompt"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "prompt": {"type": "string", "minLength": 1},
                "priority": {"type": "boolean"}
            }
        }),
        "ptah_steer" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "text"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "text": {"type": "string", "minLength": 1}
            }
        }),
        "ptah_cancel" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "run_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id
            }
        }),
        _ => json!({"type": "object", "additionalProperties": false}),
    }
}

async fn tools_call(
    orch: &Arc<OrchestrationService>,
    auth: &crate::orchestration::AuthContext,
    params: &Value,
) -> Result<Value, OrchError> {
    let call: ToolsCallParams = match parse_value(params) {
        Ok(call) => call,
        Err(error) => {
            orch.audit_transport_result("tools/call", Some(&error));
            return Err(error);
        }
    };
    let name = call.name.as_str();
    if FORBIDDEN_TOOLS.contains(&name) || !CONTROL_TOOLS.contains(&name) {
        let error = OrchError::new(
            OrchErrorCode::ForbiddenScope,
            format!("tool {name} is not available"),
        );
        orch.audit_transport_result(name, Some(&error));
        return Err(error);
    }
    let result = dispatch_tool(orch, auth, name, &call.arguments).await;
    orch.audit_transport_result(name, result.as_ref().err());
    let body = result?;
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&body).unwrap_or_default() }],
        "structuredContent": body,
        "isError": false,
    }))
}

async fn dispatch_tool(
    orch: &Arc<OrchestrationService>,
    auth: &crate::orchestration::AuthContext,
    name: &str,
    args: &Value,
) -> Result<Value, OrchError> {
    match name {
        "ptah_list_sessions" => {
            let _: EmptyArgs = parse_value(args)?;
            orch.list_sessions(auth)
        }
        "ptah_get_capacity" => {
            let _: EmptyArgs = parse_value(args)?;
            orch.get_capacity(auth)
        }
        "ptah_get_run" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_run(auth, &args.run_id)
        }
        "ptah_get_progress" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_progress(auth, &args.run_id)
        }
        "ptah_get_events" => {
            let args: EventsArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            if !(1..=500).contains(&args.limit) {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "limit must be between 1 and 500",
                ));
            }
            orch.get_events(auth, Some(&args.run_id), args.after_seq, args.limit)
        }
        "ptah_get_changes" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_changes(auth, &args.run_id)
        }
        "ptah_get_test_results" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_test_results(auth, &args.run_id)
        }
        "ptah_get_handoff" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_handoff(auth, &args.run_id)
        }
        "ptah_submit_task" => {
            let args: SubmitArgs = parse_value(args)?;
            orch.submit_task(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.prompt,
                args.bounds,
            )
            .await
        }
        "ptah_queue_prompt" => {
            let args: QueueArgs = parse_value(args)?;
            orch.queue_prompt(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.prompt,
                args.priority,
            )
            .await
        }
        "ptah_steer" => {
            let args: SteerArgs = parse_value(args)?;
            orch.steer(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.text,
            )
            .await
        }
        "ptah_cancel" => {
            let args: CancelArgs = parse_value(args)?;
            orch.cancel(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                Some(&args.run_id),
            )
            .await
        }
        other => Err(OrchError::new(
            OrchErrorCode::Unsupported,
            format!("unknown tool {other}"),
        )),
    }
}

fn parse_value<T: for<'de> Deserialize<'de>>(value: &Value) -> Result<T, OrchError> {
    serde_json::from_value(value.clone()).map_err(|e| {
        OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("invalid tool arguments: {e}"),
        )
    })
}

fn require_nonempty(value: &str, key: &str) -> Result<(), OrchError> {
    if value.trim().is_empty() {
        Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            format!("{key} must not be empty"),
        ))
    } else {
        Ok(())
    }
}

/// Discoverable tool names for schema snapshot tests.
pub fn discovered_tool_names() -> Vec<&'static str> {
    CONTROL_TOOLS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentHost, HostConfig};
    use crate::orchestration::{OrchStore, OrchestrationConfig, RunBounds, WorkspaceAllowlist};
    use crate::{home_override_serial, set_grokptah_home_override};
    use tempfile::tempdir;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn e2e_loopback_auth_and_read() {
        let _guard = home_override_serial();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let ws = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        let bus = host.event_bus();
        let _gui = bus.subscribe();
        let store = OrchStore::open(home.path().join("orch")).unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            bus.clone(),
            store,
            OrchestrationConfig {
                bearer_token: "test-token-196".into(),
                allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let srv = start_control_server(orch, 0).await.unwrap();
        let base = format!("http://{}/mcp", srv.addr);

        // invalid token
        let client = reqwest::Client::new();
        let bad = client
            .post(&base)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 401);
        let malformed_unauthenticated = client
            .post(&base)
            .header("Content-Type", "application/json")
            .body("not-json")
            .send()
            .await
            .unwrap();
        assert_eq!(
            malformed_unauthenticated.status(),
            StatusCode::UNAUTHORIZED,
            "authentication must run before body extraction"
        );

        // valid token
        let good = client
            .post(&base)
            .header("Authorization", "Bearer test-token-196")
            .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap();
        assert_eq!(good.status(), 200);
        let body: Value = good.json().await.unwrap();
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"ptah_list_sessions"));
        assert!(!names.contains(&"run_terminal_cmd"));

        srv.stop();
        set_grokptah_home_override(None);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn malformed_tool_arguments_fail_closed_without_mutation() {
        let _guard = home_override_serial();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let ws = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().unwrap();
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(crate::SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.path().join("orch")).unwrap(),
            OrchestrationConfig {
                bearer_token: "strict-token".into(),
                allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let srv = start_control_server(orch, 0).await.unwrap();
        let url = format!("http://{}/mcp", srv.addr);
        let client = reqwest::Client::new();
        let cases = [
            json!({
                "name": "ptah_queue_prompt",
                "arguments": {
                    "request_id": "bad-priority",
                    "session_id": session.id,
                    "workspace": ws.path(),
                    "prompt": "do not queue",
                    "priority": "yes"
                }
            }),
            json!({
                "name": "ptah_list_sessions",
                "arguments": {"unexpected": true}
            }),
            json!({
                "name": "ptah_get_events",
                "arguments": {"run_id": "x", "limit": "many"}
            }),
        ];
        for (id, params) in cases.into_iter().enumerate() {
            let response = client
                .post(&url)
                .header("Authorization", "Bearer strict-token")
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": params
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        assert!(host.session_queue_list(session.id).unwrap().is_empty());
        srv.stop();
        set_grokptah_home_override(None);
    }

    #[test]
    fn schema_snapshot_allowlist() {
        let names = discovered_tool_names();
        for t in CONTROL_TOOLS {
            assert!(names.contains(t));
        }
        for f in FORBIDDEN_TOOLS {
            assert!(!names.contains(f));
        }
    }
}
