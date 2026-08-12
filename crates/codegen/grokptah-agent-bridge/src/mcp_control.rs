//! Loopback-only authenticated MCP control transport (#196 / #200).
//!
//! **Standards path:** MCP Streamable HTTP (2025-03-26 / 2025-06-18 compatible)
//! over axum — initialize, tools/list, tools/call, session headers, JSON responses.
//! **Compat path:** legacy single-shot JSON-RPC POST (in-tree `McpControlClient`).
//!
//! Policy remains in [`OrchestrationService`]; this module is a thin adapter.
//! `rmcp` is intentionally not linked here (reqwest 0.13 quarantine; see #200).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::host::AgentHostHandle;
use crate::orchestration::{
    OrchError, OrchErrorCode, OrchestrationConfig, OrchestrationService, WorkspaceAllowlist,
    CONTROL_TOOLS, FORBIDDEN_TOOLS,
};

/// Max concurrent in-flight MCP requests (post-auth).
const MAX_CONCURRENT_REQUESTS: usize = 32;
/// Hard cap on Streamable HTTP sessions (LRU eviction beyond this).
const MAX_SESSIONS: usize = 256;
/// Hard wall-clock bound per request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Supported MCP protocol versions (initialize + header validation).
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// Tunable transport limits for the loopback control server.
///
/// Production callers use [`ControlServerLimits::default`]. Tests may lower
/// concurrency / timeout and optionally inject a work delay to exercise the
/// capacity (429) and request-timeout paths without waiting on wall-clock 120s.
#[derive(Debug, Clone)]
pub struct ControlServerLimits {
    /// Max concurrent in-flight MCP requests after auth (default 32).
    pub max_concurrent: usize,
    /// Hard wall-clock bound per request (default 120s).
    pub request_timeout: Duration,
    /// When set, sleep this long at the start of the timed work future
    /// (after the concurrency permit is held). Test-only; production leaves `None`.
    pub inject_work_delay: Option<Duration>,
}

impl Default for ControlServerLimits {
    fn default() -> Self {
        Self {
            max_concurrent: MAX_CONCURRENT_REQUESTS,
            request_timeout: REQUEST_TIMEOUT,
            inject_work_delay: None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    orch: Arc<OrchestrationService>,
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    inflight: Arc<Semaphore>,
    request_timeout: Duration,
    inject_work_delay: Option<Duration>,
    started_at: Instant,
    active_requests: Arc<AtomicU64>,
    total_requests: Arc<AtomicU64>,
    max_concurrent: usize,
}

#[derive(Debug, Clone)]
struct SessionState {
    #[allow(dead_code)]
    protocol_version: String,
    initialized: bool,
    #[allow(dead_code)]
    created_at: Instant,
    last_seen: Instant,
}

/// Handle for a running control server.
pub struct ControlServerHandle {
    pub addr: SocketAddr,
    pub token: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    cancel: tokio_util::sync::CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ControlServerHandle {
    pub fn stop(mut self) {
        self.cancel.cancel();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }

    /// Stop the server and wait until its serving task has released the
    /// orchestration store and other owned resources.
    pub async fn stop_and_wait(mut self) {
        self.cancel.cancel();
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// Actionable transport health snapshot for coordinators.
    pub fn health(&self, state_active: u64, state_total: u64) -> Value {
        json!({
            "ok": true,
            "addr": self.addr.to_string(),
            "activeRequests": state_active,
            "totalRequests": state_total,
        })
    }
}

/// Desktop / live-smoke bootstrap: start control from the same env contract the
/// Tauri app uses (`GROKPTAH_CONTROL_TOKEN`, `GROKPTAH_CONTROL_PORT`,
/// `GROKPTAH_CONTROL_WORKSPACES`). Returns `None` when token is unset, no
/// workspaces can be allowlisted, or bind fails.
///
/// This is the **production entry path** shared by desktop and the live
/// coordinator smoke harness — not a second policy surface.
///
/// Optional transport knobs (unset in production → defaults of 32 concurrent /
/// 120s timeout / no inject delay). Soak and diagnostics may set:
/// - `GROKPTAH_CONTROL_MAX_CONCURRENT`
/// - `GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS`
/// - `GROKPTAH_CONTROL_INJECT_WORK_DELAY_MS` (holds permit for timeout/429 tests)
pub async fn start_control_from_env(host: AgentHostHandle) -> Option<ControlServerHandle> {
    let token = std::env::var("GROKPTAH_CONTROL_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;
    let port: u16 = std::env::var("GROKPTAH_CONTROL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(list) = std::env::var("GROKPTAH_CONTROL_WORKSPACES") {
        // Platform-correct path list (':' on Unix, ';' on Windows).
        for part in std::env::split_paths(&list) {
            if !part.as_os_str().is_empty() {
                roots.push(part);
            }
        }
    }
    // Prefer current project cwd from host status (desktop sets this from UI).
    if let Some(cwd) = host.status().project_cwd {
        roots.push(PathBuf::from(cwd));
    }
    if roots.is_empty() {
        eprintln!(
            "[grokptah] MCP control: no workspaces allowlisted; set GROKPTAH_CONTROL_WORKSPACES"
        );
        return None;
    }
    // The desktop host owns the single durable ledger. Reusing that handle is
    // important: opening a second store would split desktop and MCP history or
    // contend on the process-wide lock.
    let store = host.ensure_orchestration_store().ok()?;
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: token.clone(),
            allowlist: WorkspaceAllowlist::new(roots),
            max_concurrent_runs: 4,
            bounds: Default::default(),
        },
    );
    let mut limits = ControlServerLimits::default();
    if let Ok(n) = std::env::var("GROKPTAH_CONTROL_MAX_CONCURRENT") {
        if let Ok(v) = n.parse::<usize>() {
            limits.max_concurrent = v.max(1);
        }
    }
    if let Ok(ms) = std::env::var("GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS") {
        if let Ok(v) = ms.parse::<u64>() {
            limits.request_timeout = Duration::from_millis(v.max(1));
        }
    }
    if let Ok(ms) = std::env::var("GROKPTAH_CONTROL_INJECT_WORK_DELAY_MS") {
        if let Ok(v) = ms.parse::<u64>() {
            if v > 0 {
                limits.inject_work_delay = Some(Duration::from_millis(v));
            }
        }
    }
    match start_control_server_with(orch, port, limits).await {
        Ok(mut h) => {
            h.token = token;
            Some(h)
        }
        Err(e) => {
            eprintln!("[grokptah] MCP control failed to bind: {e:#}");
            None
        }
    }
}

/// Start loopback MCP control server. Binds `127.0.0.1:port` (0 = ephemeral).
///
/// Serves MCP Streamable HTTP at `/mcp` (POST/GET/DELETE) and keeps `/` as a
/// legacy JSON-RPC alias for in-tree clients.
pub async fn start_control_server(
    orch: Arc<OrchestrationService>,
    port: u16,
) -> anyhow::Result<ControlServerHandle> {
    start_control_server_with(orch, port, ControlServerLimits::default()).await
}

/// Start the control server with explicit transport limits (tests / diagnostics).
pub async fn start_control_server_with(
    orch: Arc<OrchestrationService>,
    port: u16,
    limits: ControlServerLimits,
) -> anyhow::Result<ControlServerHandle> {
    let cancel = tokio_util::sync::CancellationToken::new();
    let max_concurrent = limits.max_concurrent.max(1);
    let state = AppState {
        orch,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        inflight: Arc::new(Semaphore::new(max_concurrent)),
        request_timeout: limits.request_timeout,
        inject_work_delay: limits.inject_work_delay,
        started_at: Instant::now(),
        active_requests: Arc::new(AtomicU64::new(0)),
        total_requests: Arc::new(AtomicU64::new(0)),
        max_concurrent,
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/", post(streamable_post_handler))
        .route(
            "/mcp",
            post(streamable_post_handler)
                .get(streamable_get_handler)
                .delete(streamable_delete_handler),
        )
        .fallback(fail_closed_fallback)
        // Body limit enforced by axum *after* auth middleware order... we put
        // auth first so unauthenticated clients never get a full 1MiB parse.
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_request,
        ))
        .with_state(state.clone());

    // Fail closed: IPv4 loopback only — never 0.0.0.0 / public interfaces.
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() {
        anyhow::bail!("control server refused non-loopback bind address {addr}");
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let token = String::new();
    let cancel_serve = cancel.clone();

    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = rx => {}
                    _ = cancel_serve.cancelled() => {}
                }
            })
            .await
            .ok();
    });

    Ok(ControlServerHandle {
        addr,
        token,
        shutdown: Some(tx),
        cancel,
        task: Some(task),
    })
}

/// Auth runs before the handler body is fully consumed by business logic.
/// Combined with DefaultBodyLimit, oversized unauthenticated bodies still pay
/// only the limited read — never orchestration mutations.
async fn authenticate_request(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    // Health is unauthenticated (loopback only) for coordinator probes.
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match state.orch.auth_header(auth_header) {
        Ok(_) => next.run(request).await,
        Err(error) => json_err(None, StatusCode::UNAUTHORIZED, &error),
    }
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "service": "grokptah-control",
        "transport": "mcp-streamable-http",
        "uptimeMs": state.started_at.elapsed().as_millis() as u64,
        "activeRequests": state.active_requests.load(Ordering::Relaxed),
        "totalRequests": state.total_requests.load(Ordering::Relaxed),
        "sessions": state.sessions.lock().len(),
        "maxConcurrent": state.max_concurrent,
        "protocolVersions": SUPPORTED_PROTOCOL_VERSIONS,
    }))
}

async fn fail_closed_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "unsupported", "message": "not found"}})),
    )
}

/// Streamable HTTP GET: optional SSE notifications channel (session-scoped).
async fn streamable_get_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if session_id.is_empty() || !state.sessions.lock().contains_key(session_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": "missing or unknown mcp-session-id"}
            })),
        )
            .into_response();
    }
    // Keep-alive only stream; tool results use POST JSON responses.
    let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{\"level\":\"info\",\"data\":\"grokptah-control sse open\"}}\n\n";
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header("mcp-session-id", session_id)
        .body(axum::body::Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn streamable_delete_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if session_id.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    state.sessions.lock().remove(session_id);
    StatusCode::NO_CONTENT.into_response()
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

async fn streamable_post_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    state.total_requests.fetch_add(1, Ordering::Relaxed);
    state.active_requests.fetch_add(1, Ordering::Relaxed);
    let _active_guard = scopeguard_active(state.active_requests.clone());

    // Concurrency bound after auth (auth middleware already ran).
    let Ok(permit) = state.inflight.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32000,
                    "message": "too many concurrent MCP requests",
                    "data": {"code": "capacity_exhausted"}
                }
            })),
        )
            .into_response();
    };
    let _permit = permit;

    if body.len() > 256 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": "request body too large"}
            })),
        )
            .into_response();
    }

    let req: JsonRpcReq = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return json_err(
                None,
                StatusCode::BAD_REQUEST,
                &OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("malformed JSON-RPC body: {e}"),
                ),
            );
        }
    };

    // Strict JSON-RPC 2.0 — missing/empty version is rejected (no silent default).
    if req.jsonrpc != "2.0" {
        return json_err(
            req.id.clone(),
            StatusCode::BAD_REQUEST,
            &OrchError::new(OrchErrorCode::InvalidRequest, "jsonrpc must be \"2.0\""),
        );
    }

    // Optional protocol version header check (Streamable HTTP).
    if let Some(ver) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&ver) {
            return json_err(
                req.id.clone(),
                StatusCode::BAD_REQUEST,
                &OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("unsupported MCP-Protocol-Version: {ver}"),
                ),
            );
        }
    }

    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let auth = match state.orch.auth_header(auth_header) {
        Ok(a) => a,
        Err(e) => return json_err(req.id.clone(), StatusCode::UNAUTHORIZED, &e),
    };

    let method = req.method.as_deref().unwrap_or("");
    // Notifications may omit id.
    let is_notification = req.id.is_none() && method.starts_with("notifications/");

    let work_delay = state.inject_work_delay;
    let work = async {
        // Test hook: hold the concurrency permit / trip request timeout without
        // waiting on production 120s or inventing a fake tool.
        if let Some(delay) = work_delay {
            tokio::time::sleep(delay).await;
        }
        match method {
            "initialize" => handle_initialize(&state, &headers, &req),
            "notifications/initialized" => {
                touch_session(&state, &headers);
                Ok((json!({}), None::<String>))
            }
            "ping" => Ok((json!({}), session_id_from_headers(&headers))),
            "tools/list" => {
                require_session_if_present(&state, &headers)?;
                Ok((tools_list_result(), session_id_from_headers(&headers)))
            }
            "tools/call" => {
                require_session_if_present(&state, &headers)?;
                let v = tools_call(&state.orch, &auth, &req.params).await?;
                Ok((v, session_id_from_headers(&headers)))
            }
            "" => Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "missing method",
            )),
            other => Err(OrchError::new(
                OrchErrorCode::Unsupported,
                format!("unsupported method {other}"),
            )),
        }
    };

    let result = match tokio::time::timeout(state.request_timeout, work).await {
        Ok(r) => r,
        Err(_) => Err(OrchError::new(
            OrchErrorCode::Timeout,
            "MCP request timed out",
        )),
    };

    match result {
        Ok((v, session_hdr)) => {
            if is_notification {
                // Spec: notifications get 202 Accepted with empty body (or no content).
                return StatusCode::ACCEPTED.into_response();
            }
            let mut response = (
                StatusCode::OK,
                Json(JsonRpcResp {
                    jsonrpc: "2.0",
                    id: req.id,
                    result: Some(v),
                    error: None,
                }),
            )
                .into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            if let Some(sid) = session_hdr {
                if let Ok(hv) = HeaderValue::from_str(&sid) {
                    response.headers_mut().insert("mcp-session-id", hv);
                }
            }
            response
        }
        Err(e) => json_err(req.id, status_for(&e), &e),
    }
}

fn scopeguard_active(counter: Arc<AtomicU64>) -> impl Drop {
    struct G(Arc<AtomicU64>);
    impl Drop for G {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    G(counter)
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn touch_session(state: &AppState, headers: &HeaderMap) {
    if let Some(sid) = session_id_from_headers(headers) {
        if let Some(s) = state.sessions.lock().get_mut(&sid) {
            s.last_seen = Instant::now();
            s.initialized = true;
        }
    }
}

fn require_session_if_present(state: &AppState, headers: &HeaderMap) -> Result<(), OrchError> {
    // Stateless clients (legacy McpControlClient) may omit session id.
    let Some(sid) = session_id_from_headers(headers) else {
        return Ok(());
    };
    let mut g = state.sessions.lock();
    let Some(s) = g.get_mut(&sid) else {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "unknown mcp-session-id",
        ));
    };
    s.last_seen = Instant::now();
    Ok(())
}

/// Evict least-recently-seen sessions until `map.len() <= max`.
fn evict_sessions_lru(map: &mut HashMap<String, SessionState>, max: usize) {
    while map.len() > max {
        let victim = map
            .iter()
            .min_by_key(|(_, s)| s.last_seen)
            .map(|(id, _)| id.clone());
        match victim {
            Some(id) => {
                map.remove(&id);
            }
            None => break,
        }
    }
}

#[cfg(test)]
mod session_cap_tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_when_over_cap() {
        let mut map = HashMap::new();
        let t0 = Instant::now() - Duration::from_secs(100);
        for i in 0..5 {
            map.insert(
                format!("s{i}"),
                SessionState {
                    protocol_version: "2025-11-25".into(),
                    initialized: true,
                    created_at: t0,
                    last_seen: t0 + Duration::from_secs(i),
                },
            );
        }
        evict_sessions_lru(&mut map, 3);
        assert_eq!(map.len(), 3);
        assert!(!map.contains_key("s0"));
        assert!(!map.contains_key("s1"));
        assert!(map.contains_key("s2"));
        assert!(map.contains_key("s3"));
        assert!(map.contains_key("s4"));
    }
}

fn handle_initialize(
    state: &AppState,
    headers: &HeaderMap,
    req: &JsonRpcReq,
) -> Result<(Value, Option<String>), OrchError> {
    let client_version = req
        .params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    let negotiated = if SUPPORTED_PROTOCOL_VERSIONS.contains(&client_version) {
        client_version
    } else {
        DEFAULT_PROTOCOL_VERSION
    };
    // Header must match body when both present (Streamable HTTP).
    if let Some(hdr) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
    {
        if hdr != client_version {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "MCP-Protocol-Version header does not match initialize protocolVersion",
            ));
        }
    }
    let session_id = Uuid::new_v4().to_string();
    {
        let mut g = state.sessions.lock();
        g.insert(
            session_id.clone(),
            SessionState {
                protocol_version: negotiated.to_string(),
                initialized: false,
                created_at: Instant::now(),
                last_seen: Instant::now(),
            },
        );
        // Hard-cap: always keep ≤ MAX_SESSIONS by evicting least-recently-seen.
        // Age-only pruning is insufficient when attackers keep last_seen fresh.
        evict_sessions_lru(&mut g, MAX_SESSIONS);
    }
    Ok((
        json!({
            "protocolVersion": negotiated,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "grokptah-control",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Authenticated loopback orchestration control. Build sessions only."
        }),
        Some(session_id),
    ))
}

fn status_for(e: &OrchError) -> StatusCode {
    match e.code {
        OrchErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        OrchErrorCode::ForbiddenScope | OrchErrorCode::WorkspaceMismatch => StatusCode::FORBIDDEN,
        OrchErrorCode::InvalidRequest | OrchErrorCode::Conflict => StatusCode::BAD_REQUEST,
        OrchErrorCode::Unsupported => StatusCode::METHOD_NOT_ALLOWED,
        OrchErrorCode::CursorExpired => StatusCode::GONE,
        OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted => StatusCode::CONFLICT,
        OrchErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
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
