//! Loopback-only authenticated MCP control transport (#196).
//!
//! Minimal JSON-RPC 2.0 over HTTP (MCP tools/list + tools/call + initialize).
//! Quarantined: uses only hyper/http primitives already available via axum.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
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
    // Strict JSON-RPC 2.0 version.
    if !req.jsonrpc.is_empty() && req.jsonrpc != "2.0" {
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
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "missing tool name"))?;
    if FORBIDDEN_TOOLS.contains(&name) || !CONTROL_TOOLS.contains(&name) {
        return Err(OrchError::new(
            OrchErrorCode::ForbiddenScope,
            format!("tool {name} is not available"),
        ));
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let body = dispatch_tool(orch, auth, name, &args).await?;
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
        "ptah_list_sessions" => orch.list_sessions(auth),
        "ptah_get_capacity" => orch.get_capacity(auth),
        "ptah_get_run" => {
            let run_id = str_arg(args, "run_id")?;
            orch.get_run(auth, run_id)
        }
        "ptah_get_progress" => {
            let run_id = str_arg(args, "run_id")?;
            orch.get_progress(auth, run_id)
        }
        "ptah_get_events" => {
            let after = args.get("after_seq").and_then(|v| v.as_u64()).unwrap_or(0);
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let run_id = args.get("run_id").and_then(|v| v.as_str());
            orch.get_events(auth, run_id, after, limit)
        }
        "ptah_get_changes" => {
            let run_id = str_arg(args, "run_id")?;
            orch.get_changes(auth, run_id)
        }
        "ptah_get_test_results" => {
            let run_id = str_arg(args, "run_id")?;
            orch.get_test_results(auth, run_id)
        }
        "ptah_get_handoff" => {
            let run_id = str_arg(args, "run_id")?;
            orch.get_handoff(auth, run_id)
        }
        "ptah_submit_task" => {
            let request_id = str_arg(args, "request_id")?.to_string();
            let session_id = uuid_arg(args, "session_id")?;
            let workspace = PathBuf::from(str_arg(args, "workspace")?);
            let prompt = str_arg(args, "prompt")?.to_string();
            let bounds = args.get("bounds").cloned();
            orch.submit_task(auth, &request_id, session_id, &workspace, prompt, bounds)
                .await
        }
        "ptah_queue_prompt" => {
            let request_id = str_arg(args, "request_id")?.to_string();
            let session_id = uuid_arg(args, "session_id")?;
            let workspace = PathBuf::from(str_arg(args, "workspace")?);
            let prompt = str_arg(args, "prompt")?.to_string();
            let priority = args
                .get("priority")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            orch.queue_prompt(auth, &request_id, session_id, &workspace, prompt, priority)
        }
        "ptah_steer" => {
            let request_id = str_arg(args, "request_id")?.to_string();
            let session_id = uuid_arg(args, "session_id")?;
            let workspace = PathBuf::from(str_arg(args, "workspace")?);
            let text = str_arg(args, "text")?.to_string();
            orch.steer(auth, &request_id, session_id, &workspace, text)
        }
        "ptah_cancel" => {
            let request_id = str_arg(args, "request_id")?.to_string();
            let session_id = uuid_arg(args, "session_id")?;
            let workspace = PathBuf::from(str_arg(args, "workspace")?);
            let run_id = args.get("run_id").and_then(|v| v.as_str());
            orch.cancel(auth, &request_id, session_id, &workspace, run_id)
        }
        other => Err(OrchError::new(
            OrchErrorCode::Unsupported,
            format!("unknown tool {other}"),
        )),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, OrchError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, format!("missing {key}")))
}

fn uuid_arg(args: &Value, key: &str) -> Result<Uuid, OrchError> {
    let s = str_arg(args, key)?;
    Uuid::parse_str(s)
        .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, format!("invalid {key}")))
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
    use crate::set_grokptah_home_override;
    use tempfile::tempdir;

    #[tokio::test]
    async fn e2e_loopback_auth_and_read() {
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
