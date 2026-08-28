//! Loopback-only authenticated MCP control transport (#196 / #200).
//!
//! **Standards path:** MCP Streamable HTTP (2025-03-26 / 2025-06-18 compatible)
//! over axum — initialize, tools/list, tools/call, session headers, JSON responses.
//! **Compat path:** legacy single-shot JSON-RPC POST (in-tree `McpControlClient`).
//!
//! Policy remains in [`OrchestrationService`]; this module is a thin adapter.
//! `rmcp` is intentionally not linked here (reqwest 0.13 quarantine; see #200).

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures::stream;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::host::AgentHostHandle;
use crate::orchestration::{
    AuthContext, ChangeRecord, ManagerStepSpec, MessageKind, MissedRunPolicy, OrchError,
    OrchErrorCode, OrchestrationConfig, OrchestrationService, RoutineConcurrencyPolicy,
    RoutineLifecycle, RoutineRetryPolicy, RoutineTrigger, RunExecutionMode, WorkArtifactRef,
    WorkDependency, WorkPolicy, WorkResult, WorkTemplate, WorkerHostKind, WorkspaceAllowlist,
    CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use crate::{EventReceiver, JournalPage, SessionUpdate};

/// Max concurrent in-flight MCP requests (post-auth).
const MAX_CONCURRENT_REQUESTS: usize = 32;
/// Hard cap on Streamable HTTP sessions (LRU eviction beyond this).
const MAX_SESSIONS: usize = 256;
/// Bound long-lived coordinator event streams independently of request floods.
const MAX_LIVE_STREAMS: usize = 32;
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
    live_streams: Arc<Semaphore>,
    health_requires_auth: bool,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRunQuery {
    #[serde(default)]
    session_id: Option<Uuid>,
    #[serde(default)]
    workspace: Option<PathBuf>,
    #[serde(default)]
    run_id: Option<String>,
}

struct LiveStreamState {
    orch: Arc<OrchestrationService>,
    auth: AuthContext,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    start_seq: u64,
    end_seq: Option<u64>,
    receiver: EventReceiver,
    last_seq: u64,
    replay_cursor: Option<u64>,
    pending: VecDeque<Bytes>,
    heartbeat: tokio::time::Interval,
    done: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl LiveStreamState {
    fn queue_page(&mut self, page: JournalPage) {
        self.replay_cursor = (page.entries.len() >= 500)
            .then_some(page.next_cursor)
            .flatten();
        for entry in page.entries {
            self.queue_entry(entry.seq, entry.ts, entry.update);
        }
        if self.end_seq.is_some_and(|end_seq| self.last_seq >= end_seq) {
            self.done = true;
            self.replay_cursor = None;
        }
    }

    fn queue_entry(&mut self, seq: u64, ts: String, update: SessionUpdate) {
        if seq <= self.last_seq || seq < self.start_seq {
            return;
        }
        if let Some(end_seq) = self.end_seq {
            if seq > end_seq {
                self.done = true;
                return;
            }
        }
        let terminal = matches!(&update, SessionUpdate::TurnComplete { .. });
        self.last_seq = seq;
        self.pending.push_back(sse_message(
            Some(seq),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/ptah_event",
                "params": {
                    "sessionId": self.session_id,
                    "workspace": self.workspace,
                    "runId": self.run_id,
                    "seq": seq,
                    "ts": ts,
                    "update": update,
                }
            }),
        ));
        if terminal || self.end_seq == Some(seq) {
            self.done = true;
            self.replay_cursor = None;
        }
    }

    fn queue_recovery(&mut self, reason: &str) {
        self.pending.push_back(sse_message(
            None,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/ptah_recovery",
                "params": {
                    "sessionId": self.session_id,
                    "workspace": self.workspace,
                    "runId": self.run_id,
                    "afterSeq": self.last_seq,
                    "reason": reason,
                    "pollTool": "ptah_get_events",
                }
            }),
        ));
        self.done = true;
        self.replay_cursor = None;
    }

    async fn next_frame(&mut self) -> Option<Bytes> {
        loop {
            if self.pending.front().is_some() {
                // A frame may have been queued before credential rotation.
                // Revalidate both after the wakeup and immediately before
                // returning it; admission-time auth is not a stream lease.
                if !self.orch.auth_is_current(&self.auth) {
                    self.pending.clear();
                    self.done = true;
                    return None;
                }
                let frame = self.pending.pop_front();
                if !self.orch.auth_is_current(&self.auth) {
                    self.pending.clear();
                    self.done = true;
                    return None;
                }
                return frame;
            }
            if self.done {
                return None;
            }
            if let Some(cursor) = self.replay_cursor.take() {
                match self.orch.live_run_page(
                    &self.auth,
                    self.session_id,
                    &self.workspace,
                    &self.run_id,
                    cursor,
                    500,
                ) {
                    Ok((scope, page)) => {
                        self.end_seq = scope.end_seq;
                        self.queue_page(page);
                        continue;
                    }
                    Err(error) => {
                        self.queue_recovery(&format!(
                            "durable replay unavailable: {}",
                            error.message
                        ));
                        continue;
                    }
                }
            }

            tokio::select! {
                event = self.receiver.recv_with_seq() => {
                    if !self.orch.auth_is_current(&self.auth) {
                        self.pending.clear();
                        self.done = true;
                        continue;
                    }
                    let Some((seq, update)) = event else {
                        self.done = true;
                        continue;
                    };
                    if seq == 0 {
                        self.queue_recovery("live event subscriber lagged; resynchronize from the durable journal");
                        continue;
                    }
                    if crate::event_bus::session_id_of(&update) != Some(self.session_id) {
                        continue;
                    }
                    self.queue_entry(seq, chrono::Utc::now().to_rfc3339(), update);
                }
                _ = self.heartbeat.tick() => {
                    if !self.orch.auth_is_current(&self.auth) {
                        self.done = true;
                        continue;
                    }
                    return Some(Bytes::from_static(b": grokptah-control keep-alive\n\n"));
                }
            }
        }
    }
}

fn sse_message(id: Option<u64>, body: Value) -> Bytes {
    let mut frame = String::from("event: message\n");
    if let Some(id) = id {
        frame.push_str(&format!("id: {id}\n"));
    }
    frame.push_str("data: ");
    frame.push_str(&serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()));
    frame.push_str("\n\n");
    Bytes::from(frame)
}

/// Handle for a running control server.
pub struct ControlServerHandle {
    pub addr: SocketAddr,
    pub token: String,
    orch: Arc<OrchestrationService>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    cancel: tokio_util::sync::CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ControlServerHandle {
    /// Shared policy service for trusted desktop commands that surface the
    /// same MCP-owned runs. The transport remains the only network boundary.
    pub fn orchestration_service(&self) -> Arc<OrchestrationService> {
        self.orch.clone()
    }

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
        self.orch.stop_background_tasks().await;
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
    start_control_server_with_bind(
        orch,
        SocketAddr::from(([127, 0, 0, 1], port)),
        limits,
        false,
    )
    .await
}

/// Start the control server on an explicit address.
///
/// Loopback remains the default and keeps `/health` and `/ready` probeable
/// without credentials. A non-loopback listener is permitted only when the
/// caller explicitly enables authenticated health probes; this prevents a
/// service configuration from accidentally exposing unauthenticated status.
pub async fn start_control_server_with_bind(
    orch: Arc<OrchestrationService>,
    addr: SocketAddr,
    limits: ControlServerLimits,
    health_requires_auth: bool,
) -> anyhow::Result<ControlServerHandle> {
    if !addr.ip().is_loopback() && !health_requires_auth {
        anyhow::bail!("non-loopback control listeners require authenticated health probes");
    }
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
        live_streams: Arc::new(Semaphore::new(MAX_LIVE_STREAMS)),
        health_requires_auth,
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
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

    let listener = TcpListener::bind(addr).await?;
    let addr = listener.local_addr()?;
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
        orch: state.orch.clone(),
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
    // Health/readiness are unauthenticated only for loopback listeners. A
    // service explicitly exposed beyond the host must authenticate probes too.
    if matches!(request.uri().path(), "/health" | "/ready") && !state.health_requires_auth {
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
    let readiness = readiness_snapshot(&state);
    Json(json!({
        "ok": true,
        "ready": readiness.ready,
        "service": "grokptah-control",
        "transport": "mcp-streamable-http",
        "uptimeMs": state.started_at.elapsed().as_millis() as u64,
        "activeRequests": state.active_requests.load(Ordering::Relaxed),
        "totalRequests": state.total_requests.load(Ordering::Relaxed),
        "sessions": state.sessions.lock().len(),
        "activeLiveStreams": MAX_LIVE_STREAMS - state.live_streams.available_permits(),
        "maxLiveStreams": MAX_LIVE_STREAMS,
        "maxConcurrent": state.max_concurrent,
        "protocolVersions": SUPPORTED_PROTOCOL_VERSIONS,
    }))
}

struct ReadinessSnapshot {
    ready: bool,
    payload: Value,
}

fn readiness_snapshot(state: &AppState) -> ReadinessSnapshot {
    let payload = state
        .orch
        .capacity_for_health()
        .unwrap_or_else(|error| json!({"health": {"serviceError": error.message}}));
    let health = payload.get("health").cloned().unwrap_or_else(|| json!({}));
    let ready = [
        "eventJournalPersistenceError",
        "auditPersistenceError",
        "runPersistenceError",
        "workloadSupervisorError",
        "routineSupervisorError",
        "managerSupervisorError",
        "nativeExecutorError",
        "serviceError",
    ]
    .iter()
    .all(|key| health.get(*key).is_none_or(Value::is_null));
    ReadinessSnapshot { ready, payload }
}

async fn ready_handler(State(state): State<AppState>) -> Response {
    let snapshot = readiness_snapshot(&state);
    let status = if snapshot.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "ok": snapshot.ready,
            "ready": snapshot.ready,
            "capacity": snapshot.payload,
        })),
    )
        .into_response()
}

async fn fail_closed_fallback() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "unsupported", "message": "not found"}})),
    )
}

/// Streamable HTTP GET: optional SSE notifications channel (session-scoped).
async fn streamable_get_handler(
    State(state): State<AppState>,
    Query(query): Query<LiveRunQuery>,
    headers: HeaderMap,
) -> Response {
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

    let no_scope =
        query.session_id.is_none() && query.workspace.is_none() && query.run_id.is_none();
    if no_scope {
        // Keep-alive only stream; tool results use POST JSON responses. A
        // scoped query opts into the live run channel below.
        let body = sse_message(
            None,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": {"level": "info", "data": "grokptah-control sse open"}
            }),
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .header(axum::http::header::CACHE_CONTROL, "no-cache")
            .header("mcp-session-id", session_id)
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    if query.session_id.is_none() || query.workspace.is_none() || query.run_id.is_none() {
        return json_err(
            None,
            StatusCode::BAD_REQUEST,
            &OrchError::new(
                OrchErrorCode::InvalidRequest,
                "live event streams require session_id, workspace, and run_id together",
            ),
        );
    }
    let session_scope = query.session_id.expect("checked above");
    let workspace = query.workspace.expect("checked above");
    let run_id = query.run_id.expect("checked above");
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let auth = match state.orch.auth_header(auth_header) {
        Ok(auth) => auth,
        Err(error) => return json_err(None, StatusCode::UNAUTHORIZED, &error),
    };
    let last_seq = match headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().parse::<u64>())
        .transpose()
    {
        Ok(value) => value.unwrap_or(0),
        Err(_) => {
            return json_err(
                None,
                StatusCode::BAD_REQUEST,
                &OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "Last-Event-ID must be a sequence number",
                ),
            )
        }
    };
    let receiver = state.orch.subscribe_events();
    let (scope, page) =
        match state
            .orch
            .live_run_page(&auth, session_scope, &workspace, &run_id, last_seq, 500)
        {
            Ok(value) => value,
            Err(error) => {
                let status = if error.code == OrchErrorCode::CursorExpired {
                    StatusCode::GONE
                } else {
                    StatusCode::CONFLICT
                };
                return json_err(None, status, &error);
            }
        };
    let permit = match state.live_streams.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return json_err(
                None,
                StatusCode::TOO_MANY_REQUESTS,
                &OrchError::new(
                    OrchErrorCode::CapacityExhausted,
                    "too many live MCP event streams",
                ),
            )
        }
    };
    let mut live = LiveStreamState {
        orch: state.orch.clone(),
        auth,
        session_id: scope.session_id,
        workspace,
        run_id: scope.run_id,
        start_seq: scope.start_seq,
        end_seq: scope.end_seq,
        receiver,
        last_seq,
        replay_cursor: None,
        pending: VecDeque::new(),
        heartbeat: tokio::time::interval(Duration::from_secs(10)),
        done: false,
        _permit: permit,
    };
    live.queue_page(page);
    let stream = stream::unfold(live, |mut state| async move {
        state
            .next_frame()
            .await
            .map(|frame| (Ok::<Bytes, Infallible>(frame), state))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .header("mcp-session-id", session_id)
        .header(axum::http::header::CONNECTION, "keep-alive")
        .body(axum::body::Body::from_stream(stream))
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
struct PersistentAgentArgs {
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionWorkspaceArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSessionArgs {
    workspace: PathBuf,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistentAgentResumeArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
    prompt: String,
    #[serde(default)]
    max_rounds: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    source_fingerprint: String,
    final_fingerprint: String,
    changed_files: Vec<ChangeRecord>,
    #[serde(default)]
    ttl_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromoteArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    approval_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscardArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsArgs {
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    #[serde(default)]
    after_seq: u64,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerEventsArgs {
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    /// Omitted = read from the start of the retained journal. A present
    /// cursor gets strict continuity semantics, including expiry.
    #[serde(default)]
    after_seq: Option<u64>,
    #[serde(default = "default_computer_event_limit")]
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
    #[serde(default)]
    execution_mode: RunExecutionMode,
    /// When true, capacity/session contention uses the bounded fair admission queue.
    #[serde(default)]
    allow_queue: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    run_id: String,
    prompt: String,
    #[serde(default)]
    bounds: Option<Value>,
    #[serde(default)]
    execution_mode: Option<RunExecutionMode>,
    #[serde(default)]
    allow_queue: bool,
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
struct QueueScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueEditArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    entry_id: String,
    version: u64,
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueEntryMutationArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    entry_id: String,
    expected_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueReorderArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    entry_id: String,
    to_index: usize,
    expected_version: u64,
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueClearArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    kind: String,
    objective: String,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    deadline: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    parent_work_id: Option<String>,
    #[serde(default)]
    dependencies: Vec<WorkDependency>,
    #[serde(default)]
    policy: WorkPolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateManagerPlanArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    manager_agent_id: String,
    objective: String,
    steps: Vec<ManagerStepSpec>,
    #[serde(default = "default_manager_in_flight")]
    max_in_flight: u32,
    #[serde(default = "default_manager_replans")]
    max_replans: u32,
    #[serde(default)]
    autonomous: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagerPlanScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
    plan_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvanceManagerPlanArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    plan_id: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TickManagerPlanArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    plan_id: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplanManagerPlanArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    plan_id: String,
    reason: String,
    steps: Vec<ManagerStepSpec>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRoutineArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    name: String,
    agent_id: String,
    trigger: RoutineTrigger,
    work_template: WorkTemplate,
    #[serde(default)]
    missed_run_policy: MissedRunPolicy,
    #[serde(default)]
    concurrency: RoutineConcurrencyPolicy,
    #[serde(default)]
    retry: RoutineRetryPolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutineScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutineArgs {
    session_id: Uuid,
    workspace: PathBuf,
    routine_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutineLifecycleArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    routine_id: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FireRoutineArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    routine_id: String,
    #[serde(default)]
    payload: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkArgs {
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    #[serde(default)]
    lease_ms: Option<u64>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkLeaseArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    #[serde(default)]
    lease_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkWorkRunArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkProgressArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    summary: String,
    #[serde(default)]
    percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkReasonArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkResultArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    summary: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    artifacts: Vec<WorkArtifactRef>,
    #[serde(default)]
    failure: Option<String>,
    #[serde(default)]
    cancellation_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    #[serde(default)]
    assigned_agent_id: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerScopeArgs {
    session_id: Uuid,
    workspace: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerArgs {
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeartbeatWorkerArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
    #[serde(default)]
    host_kind: Option<WorkerHostKind>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    agent_id: String,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    manager_agent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    agent_id: String,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReprioritizeWorkArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    priority: i32,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    kind: MessageKind,
    body: String,
    #[serde(default)]
    from_agent_id: Option<String>,
    #[serde(default)]
    to_agent_id: Option<String>,
    #[serde(default)]
    work_id: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    reply_to_id: Option<String>,
    #[serde(default)]
    attempt_id: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckMessageArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    message_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxArgs {
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
    #[serde(default)]
    after_seq: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetManagedExecutionArgs {
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
    policy: crate::orchestration::ManagedExecutionPolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetManagedExecutionArgs {
    session_id: Uuid,
    workspace: PathBuf,
    agent_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizeWorkExecutionArgs {
    request_id: String,
    session_id: Uuid,
    workspace: PathBuf,
    work_id: String,
    reason: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveWorkInputArgs {
    session_id: Uuid,
    workspace: PathBuf,
    permission_id: Uuid,
    allow: bool,
}

fn empty_object() -> Value {
    json!({})
}

fn default_event_limit() -> usize {
    50
}

fn default_computer_event_limit() -> usize {
    crate::computer_use::DEFAULT_EVENT_PAGE
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
        OrchErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
        OrchErrorCode::StaleVersion | OrchErrorCode::Conflict => StatusCode::CONFLICT,
        OrchErrorCode::Unsupported => StatusCode::METHOD_NOT_ALLOWED,
        OrchErrorCode::CursorExpired => StatusCode::GONE,
        OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted => StatusCode::CONFLICT,
        OrchErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn json_err(id: Option<Value>, status: StatusCode, e: &OrchError) -> Response {
    let mut data = json!({ "code": e.code.as_str() });
    if let Some(extra) = e.data.as_ref().and_then(Value::as_object) {
        for (key, value) in extra {
            data[key] = value.clone();
        }
    }
    (
        status,
        Json(JsonRpcResp {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({
                "code": -32000,
                "message": e.message,
                "data": data,
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
        "ptah_list_sessions" | "ptah_list_persistent_agents" | "ptah_get_capacity" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "ptah_create_session" => json!({
            "type": "object",
            "required": ["workspace"],
            "additionalProperties": false,
            "properties": {
                "workspace": workspace,
                "title": {"type": "string", "minLength": 1, "maxLength": 160}
            }
        }),
        "ptah_list_runs" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_get_persistent_agent" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "agent_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256}
            }
        }),
        "ptah_get_run"
        | "ptah_get_progress"
        | "ptah_get_changes"
        | "ptah_get_test_results"
        | "ptah_get_handoff"
        | "ptah_review_run"
        | "ptah_get_computer_run" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "run_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id
            }
        }),
        "ptah_list_computer_runs" | "ptah_get_computer_capacity" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_get_computer_run_events" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "run_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id,
                "after_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Durable cursor. Omit to read from the start of the retained journal; a cursor below the retained window fails with cursor_expired and includes eventRange."
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 500}
            }
        }),
        "ptah_get_events" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "run_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
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
                "bounds": bounds,
                "execution_mode": {
                    "type": "string",
                    "enum": ["shared", "isolated_worktree"],
                    "default": "shared",
                    "description": "Use shared execution by default; isolated_worktree creates a reviewable managed Git worktree for this Build run."
                },
                "allow_queue": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, queue behind bounded capacity/session contention instead of failing fast."
                }
            }
        }),
        "ptah_resume_persistent_agent" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "agent_id", "prompt"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "prompt": {"type": "string", "minLength": 1},
                "max_rounds": {"type": "integer", "minimum": 1, "maximum": 24}
            }
        }),
        "ptah_list_work" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_get_work" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "work_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id
            }
        }),
        "ptah_create_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "kind", "objective"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "kind": {"type": "string", "minLength": 1, "maxLength": 96},
                "objective": {"type": "string", "minLength": 1, "maxLength": 32768},
                "priority": {"type": "integer"},
                "deadline": {"type": "string", "format": "date-time"},
                "parent_work_id": run_id,
                "dependencies": {"type": "array", "maxItems": 128},
                "policy": {"type": "object"}
            }
        }),
        "ptah_create_manager_plan" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "manager_agent_id", "objective", "steps"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "manager_agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "objective": {"type": "string", "minLength": 1, "maxLength": 32768},
                "steps": {"type": "array", "minItems": 1, "maxItems": 64, "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["stepId", "kind", "objective"],
                    "properties": {
                        "stepId": {"type": "string", "minLength": 1, "maxLength": 256},
                        "kind": {"type": "string", "minLength": 1, "maxLength": 96},
                        "objective": {"type": "string", "minLength": 1, "maxLength": 32768},
                        "priority": {"type": "integer"},
                        "dependencies": {"type": "array", "maxItems": 64, "items": {"type": "string", "maxLength": 256}},
                        "assignedAgentId": {"type": ["string", "null"], "maxLength": 256},
                        "policy": {"type": "object"}
                    }
                }},
                "max_in_flight": {"type": "integer", "minimum": 1, "maximum": 16},
                "max_replans": {"type": "integer", "minimum": 0, "maximum": 16}
                ,"autonomous": {"type": "boolean", "default": false}
            }
        }),
        "ptah_list_manager_plans" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {"session_id": session, "workspace": workspace}
        }),
        "ptah_get_manager_plan" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "plan_id"],
            "additionalProperties": false,
            "properties": {"session_id": session, "workspace": workspace, "plan_id": run_id}
        }),
        "ptah_advance_manager_plan" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "plan_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "plan_id": run_id,
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_tick_manager_plan" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "plan_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "plan_id": run_id,
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_replan_manager_plan" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "plan_id", "reason", "steps"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "plan_id": run_id,
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "steps": {"type": "array", "minItems": 1, "maxItems": 64, "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["stepId", "kind", "objective"],
                    "properties": {
                        "stepId": {"type": "string", "minLength": 1, "maxLength": 256},
                        "kind": {"type": "string", "minLength": 1, "maxLength": 96},
                        "objective": {"type": "string", "minLength": 1, "maxLength": 32768},
                        "priority": {"type": "integer"},
                        "dependencies": {"type": "array", "maxItems": 64, "items": {"type": "string", "maxLength": 256}},
                        "assignedAgentId": {"type": ["string", "null"], "maxLength": 256},
                        "policy": {"type": "object"}
                    }
                }},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_assign_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "assigned_agent_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "assigned_agent_id": {"type": ["string", "null"], "maxLength": 256},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_claim_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "lease_ms": {"type": "integer", "minimum": 1, "maximum": 3600000},
                "agent_id": {"type": "string", "maxLength": 256}
            }
        }),
        "ptah_renew_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "attempt_id", "lease_token"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "attempt_id": run_id,
                "lease_token": {"type": "string", "minLength": 1, "maxLength": 256},
                "lease_ms": {"type": "integer", "minimum": 1, "maximum": 3600000}
            }
        }),
        "ptah_link_work_run" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "attempt_id", "lease_token", "run_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "attempt_id": run_id,
                "lease_token": {"type": "string", "minLength": 1, "maxLength": 256},
                "run_id": run_id
            }
        }),
        "ptah_report_work_progress" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "attempt_id", "lease_token", "summary"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "attempt_id": run_id,
                "lease_token": {"type": "string", "minLength": 1, "maxLength": 256},
                "summary": {"type": "string", "minLength": 1, "maxLength": 32768},
                "percent": {"type": "integer", "minimum": 0, "maximum": 100}
            }
        }),
        "ptah_release_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "attempt_id", "lease_token", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "attempt_id": run_id,
                "lease_token": {"type": "string", "minLength": 1, "maxLength": 256},
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768}
            }
        }),
        "ptah_complete_work" | "ptah_fail_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "attempt_id", "lease_token", "summary"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "attempt_id": run_id,
                "lease_token": {"type": "string", "minLength": 1, "maxLength": 256},
                "summary": {"type": "string", "minLength": 1, "maxLength": 32768},
                "evidence": {"type": "array", "maxItems": 256, "items": {"type": "string", "maxLength": 2048}},
                "artifacts": {"type": "array", "maxItems": 256},
                "failure": {"type": "string", "maxLength": 32768},
                "cancellation_reason": {"type": "string", "maxLength": 32768}
            }
        }),
        "ptah_cancel_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_retry_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_create_routine" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "name", "agent_id", "trigger", "work_template"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "name": {"type": "string", "minLength": 1, "maxLength": 128},
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "trigger": {"type": "object"},
                "work_template": {"type": "object"},
                "missed_run_policy": {"type": "string", "enum": ["skip", "coalesce", "catch_up"]},
                "concurrency": {"type": "object"},
                "retry": {"type": "object"}
            }
        }),
        "ptah_list_routines" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_list_workers" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_get_worker" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "agent_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256}
            }
        }),
        "ptah_heartbeat_worker" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "agent_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "host_kind": {"type": "string", "enum": ["desktop", "service", "unknown"]}
            }
        }),
        "ptah_offer_work" | "ptah_reassign_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "agent_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1},
                "manager_agent_id": {"type": "string", "maxLength": 256}
            }
        }),
        "ptah_accept_work" | "ptah_decline_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "agent_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_reprioritize_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "priority", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "priority": {"type": "integer"},
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_block_work" | "ptah_request_review" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_list_work_decisions" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "work_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id
            }
        }),
        "ptah_send_message" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "kind", "body"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "kind": {"type": "string"},
                "body": {"type": "string", "minLength": 1, "maxLength": 8192},
                "from_agent_id": {"type": "string"},
                "to_agent_id": {"type": "string"},
                "work_id": run_id,
                "payload": {"type": "object"},
                "reply_to_id": run_id,
                "attempt_id": run_id,
                "run_id": run_id
            }
        }),
        "ptah_ack_message" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "message_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "message_id": run_id
            }
        }),
        "ptah_set_managed_execution" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "agent_id", "policy"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "policy": {"type": "object"}
            }
        }),
        "ptah_get_managed_execution" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "agent_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256}
            }
        }),
        "ptah_authorize_work_execution" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id", "reason"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "reason": {"type": "string", "minLength": 1, "maxLength": 32768},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_resolve_work_input" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "permission_id", "allow"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "permission_id": session,
                "allow": {"type": "boolean"}
            }
        }),
        "ptah_list_execution_intents" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
            }
        }),
        "ptah_list_inbox" | "ptah_list_outbox" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "agent_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "agent_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "after_seq": {"type": "integer", "minimum": 0}
            }
        }),
        "ptah_get_routine" | "ptah_list_activations" => json!({
            "type": "object",
            "required": ["session_id", "workspace", "routine_id"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace,
                "routine_id": run_id
            }
        }),
        "ptah_pause_routine" | "ptah_enable_routine" | "ptah_disable_routine" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "routine_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "routine_id": run_id,
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_fire_routine" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "routine_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "routine_id": run_id,
                "payload": {"type": "object"}
            }
        }),
        "ptah_approve_work" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "work_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "work_id": run_id,
                "note": {"type": "string", "maxLength": 4096},
                "expected_revision": {"type": "integer", "minimum": 1}
            }
        }),
        "ptah_retry_run" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "run_id", "prompt"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id,
                "prompt": {"type": "string", "minLength": 1},
                "bounds": bounds,
                "execution_mode": {
                    "type": "string",
                    "enum": ["shared", "isolated_worktree"],
                    "description": "Optional explicit mode; it must match the interrupted source run."
                },
                "allow_queue": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, queue behind bounded capacity/session contention instead of failing fast."
                }
            }
        }),
        "ptah_approve_run" => json!({
            "type": "object",
            "required": [
                "request_id", "session_id", "workspace", "run_id",
                "source_fingerprint", "final_fingerprint", "changed_files"
            ],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id,
                "source_fingerprint": {"type": "string", "minLength": 1},
                "final_fingerprint": {"type": "string", "minLength": 1},
                "changed_files": {
                    "type": "array",
                    "maxItems": 2000,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["path", "summary"],
                        "properties": {
                            "path": {"type": "string", "minLength": 1},
                            "summary": {"type": "string"}
                        }
                    }
                },
                "ttl_ms": {"type": "integer", "minimum": 1, "maximum": 900000}
            }
        }),
        "ptah_promote_run" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "run_id", "approval_id"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "run_id": run_id,
                "approval_id": {"type": "string", "minLength": 1, "maxLength": 256}
            }
        }),
        "ptah_discard_run" => json!({
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
        "ptah_get_queue" => json!({
            "type": "object",
            "required": ["session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "session_id": session,
                "workspace": workspace
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
        "ptah_edit_queue" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "entry_id", "version", "text"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "entry_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "version": {"type": "integer", "minimum": 0},
                "text": {"type": "string", "minLength": 1}
            }
        }),
        "ptah_remove_queue" | "ptah_run_next" | "ptah_steer_queued" => json!({
            "type": "object",
            // expected_version is required: an optional CAS on a two-writer
            // control plane is last-write-wins, and ptah_run_next cancels the
            // active turn.
            "required": ["request_id", "session_id", "workspace", "entry_id", "expected_version"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "entry_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "expected_version": {"type": "integer", "minimum": 0}
            }
        }),
        "ptah_reorder_queue" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace", "entry_id", "to_index", "expected_version", "expected_revision"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace,
                "entry_id": {"type": "string", "minLength": 1, "maxLength": 256},
                "to_index": {"type": "integer", "minimum": 0},
                "expected_version": {"type": "integer", "minimum": 0},
                "expected_revision": {"type": "integer", "minimum": 0}
            }
        }),
        "ptah_clear_queue" => json!({
            "type": "object",
            "required": ["request_id", "session_id", "workspace"],
            "additionalProperties": false,
            "properties": {
                "request_id": req_id,
                "session_id": session,
                "workspace": workspace
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
        "ptah_create_session" => {
            let args: CreateSessionArgs = parse_value(args)?;
            orch.create_session(auth, &args.workspace, args.title)
        }
        "ptah_list_persistent_agents" => {
            let _: EmptyArgs = parse_value(args)?;
            orch.list_persistent_agents(auth)
        }
        "ptah_list_runs" => {
            let args: SessionWorkspaceArgs = parse_value(args)?;
            orch.list_runs_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_persistent_agent" => {
            let args: PersistentAgentArgs = parse_value(args)?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.get_persistent_agent_scoped(auth, args.session_id, &args.workspace, &args.agent_id)
        }
        "ptah_get_capacity" => {
            let _: EmptyArgs = parse_value(args)?;
            orch.get_capacity(auth)
        }
        "ptah_get_run" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_run_scoped(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_get_progress" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_progress_scoped(auth, args.session_id, &args.workspace, &args.run_id)
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
            orch.get_events_scoped(
                auth,
                args.session_id,
                &args.workspace,
                &args.run_id,
                args.after_seq,
                args.limit,
            )
        }
        "ptah_get_changes" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_changes_scoped(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_get_test_results" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_test_results_scoped(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_get_handoff" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_handoff_scoped(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_review_run" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.review_run(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_list_computer_runs" => {
            let args: ComputerScopeArgs = parse_value(args)?;
            orch.list_computer_runs_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_computer_run" => {
            let args: RunArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.get_computer_run_scoped(auth, args.session_id, &args.workspace, &args.run_id)
        }
        "ptah_get_computer_run_events" => {
            let args: ComputerEventsArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            if !(1..=500).contains(&args.limit) {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "limit must be between 1 and 500",
                ));
            }
            orch.get_computer_run_events_scoped(
                auth,
                args.session_id,
                &args.workspace,
                &args.run_id,
                args.after_seq,
                args.limit,
            )
        }
        "ptah_get_computer_capacity" => {
            let args: ComputerScopeArgs = parse_value(args)?;
            orch.get_computer_capacity_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_submit_task" => {
            let args: SubmitArgs = parse_value(args)?;
            orch.submit_task_with_execution_mode_and_queue(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.prompt,
                args.bounds,
                args.execution_mode,
                args.allow_queue,
            )
            .await
        }
        "ptah_resume_persistent_agent" => {
            let args: PersistentAgentResumeArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.resume_persistent_agent(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.agent_id,
                args.prompt,
                args.max_rounds,
            )
            .await
        }
        "ptah_list_work" => {
            let args: WorkScopeArgs = parse_value(args)?;
            orch.list_work_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_work" => {
            let args: WorkArgs = parse_value(args)?;
            require_nonempty(&args.work_id, "work_id")?;
            orch.get_work_scoped(auth, args.session_id, &args.workspace, &args.work_id)
        }
        "ptah_create_work" => {
            let args: CreateWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            orch.create_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.kind,
                args.objective,
                args.priority,
                args.deadline,
                args.parent_work_id,
                args.dependencies,
                args.policy,
            )
            .await
        }
        "ptah_create_manager_plan" => {
            let args: CreateManagerPlanArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.manager_agent_id, "manager_agent_id")?;
            orch.create_manager_plan(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.manager_agent_id,
                args.objective,
                args.steps,
                args.max_in_flight,
                args.max_replans,
                args.autonomous,
            )
            .await
        }
        "ptah_list_manager_plans" => {
            let args: SessionWorkspaceArgs = parse_value(args)?;
            orch.list_manager_plans_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_manager_plan" => {
            let args: ManagerPlanScopeArgs = parse_value(args)?;
            require_nonempty(&args.plan_id, "plan_id")?;
            orch.get_manager_plan_scoped(auth, args.session_id, &args.workspace, &args.plan_id)
        }
        "ptah_advance_manager_plan" => {
            let args: AdvanceManagerPlanArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.plan_id, "plan_id")?;
            orch.advance_manager_plan(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.plan_id,
                args.expected_revision,
            )
            .await
        }
        "ptah_tick_manager_plan" => {
            let args: TickManagerPlanArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.plan_id, "plan_id")?;
            orch.tick_manager_plan(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.plan_id,
                args.expected_revision,
            )
            .await
        }
        "ptah_replan_manager_plan" => {
            let args: ReplanManagerPlanArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.plan_id, "plan_id")?;
            orch.replan_manager_plan(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.plan_id,
                args.reason,
                args.steps,
                args.expected_revision,
            )
            .await
        }
        "ptah_assign_work" => {
            let args: AssignWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            if let Some(agent_id) = &args.assigned_agent_id {
                require_nonempty(agent_id, "assigned_agent_id")?;
            }
            orch.assign_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.assigned_agent_id,
                args.expected_revision,
            )
            .await
        }
        "ptah_claim_work" => {
            let args: ClaimWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            orch.claim_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.lease_ms,
                args.agent_id,
            )
            .await
        }
        "ptah_renew_work" => {
            let args: WorkLeaseArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.attempt_id, "attempt_id")?;
            require_nonempty(&args.lease_token, "lease_token")?;
            orch.renew_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.attempt_id,
                &args.lease_token,
                args.lease_ms,
            )
            .await
        }
        "ptah_link_work_run" => {
            let args: LinkWorkRunArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.attempt_id, "attempt_id")?;
            require_nonempty(&args.lease_token, "lease_token")?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.link_work_run(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.attempt_id,
                &args.lease_token,
                &args.run_id,
            )
            .await
        }
        "ptah_report_work_progress" => {
            let args: WorkProgressArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.attempt_id, "attempt_id")?;
            require_nonempty(&args.lease_token, "lease_token")?;
            orch.report_work_progress(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.attempt_id,
                &args.lease_token,
                args.summary,
                args.percent,
            )
            .await
        }
        "ptah_release_work" => {
            let args: WorkReasonArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.attempt_id, "attempt_id")?;
            require_nonempty(&args.lease_token, "lease_token")?;
            orch.release_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.attempt_id,
                &args.lease_token,
                args.reason,
            )
            .await
        }
        "ptah_complete_work" | "ptah_fail_work" => {
            let args: WorkResultArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.attempt_id, "attempt_id")?;
            require_nonempty(&args.lease_token, "lease_token")?;
            let result = WorkResult {
                summary: args.summary,
                evidence: args.evidence,
                artifacts: args.artifacts,
                failure: args.failure,
                cancellation_reason: args.cancellation_reason,
                completed_at: chrono::Utc::now(),
            };
            if name == "ptah_complete_work" {
                orch.complete_work(
                    auth,
                    &args.request_id,
                    args.session_id,
                    &args.workspace,
                    &args.work_id,
                    &args.attempt_id,
                    &args.lease_token,
                    result,
                )
                .await
            } else {
                orch.fail_work(
                    auth,
                    &args.request_id,
                    args.session_id,
                    &args.workspace,
                    &args.work_id,
                    &args.attempt_id,
                    &args.lease_token,
                    result,
                )
                .await
            }
        }
        "ptah_cancel_work" => {
            let args: CancelWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.reason, "reason")?;
            orch.cancel_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_retry_work" => {
            let args: RetryWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            require_nonempty(&args.reason, "reason")?;
            orch.retry_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_approve_work" => {
            let args: ApproveWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.work_id, "work_id")?;
            if let Some(note) = &args.note {
                require_nonempty(note, "note")?;
            }
            orch.approve_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.note,
                args.expected_revision,
            )
            .await
        }
        "ptah_create_routine" => {
            let args: CreateRoutineArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.name, "name")?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.create_routine(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.name,
                args.agent_id,
                args.trigger,
                args.work_template,
                args.missed_run_policy,
                args.concurrency,
                args.retry,
            )
            .await
        }
        "ptah_list_routines" => {
            let args: RoutineScopeArgs = parse_value(args)?;
            orch.list_routines_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_routine" => {
            let args: RoutineArgs = parse_value(args)?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.get_routine_scoped(auth, args.session_id, &args.workspace, &args.routine_id)
        }
        "ptah_list_activations" => {
            let args: RoutineArgs = parse_value(args)?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.list_activations_scoped(auth, args.session_id, &args.workspace, &args.routine_id)
        }
        "ptah_pause_routine" => {
            let args: RoutineLifecycleArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.set_routine_lifecycle(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.routine_id,
                RoutineLifecycle::Paused,
                args.expected_revision,
                "ptah_pause_routine",
            )
            .await
        }
        "ptah_enable_routine" => {
            let args: RoutineLifecycleArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.set_routine_lifecycle(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.routine_id,
                RoutineLifecycle::Enabled,
                args.expected_revision,
                "ptah_enable_routine",
            )
            .await
        }
        "ptah_disable_routine" => {
            let args: RoutineLifecycleArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.set_routine_lifecycle(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.routine_id,
                RoutineLifecycle::Disabled,
                args.expected_revision,
                "ptah_disable_routine",
            )
            .await
        }
        "ptah_list_workers" => {
            let args: WorkerScopeArgs = parse_value(args)?;
            orch.list_workers_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_get_worker" => {
            let args: WorkerArgs = parse_value(args)?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.get_worker_scoped(auth, args.session_id, &args.workspace, &args.agent_id)
        }
        "ptah_heartbeat_worker" => {
            let args: HeartbeatWorkerArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.heartbeat_worker(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.agent_id,
                args.host_kind.unwrap_or(WorkerHostKind::Service),
            )
            .await
        }
        "ptah_offer_work" => {
            let args: OfferWorkArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            orch.offer_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.agent_id,
                args.reason,
                args.expected_revision,
                args.manager_agent_id,
            )
            .await
        }
        "ptah_accept_work" => {
            let args: AcceptWorkArgs = parse_value(args)?;
            orch.accept_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.agent_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_decline_work" => {
            let args: AcceptWorkArgs = parse_value(args)?;
            orch.decline_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.agent_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_reassign_work" => {
            let args: OfferWorkArgs = parse_value(args)?;
            orch.reassign_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                &args.agent_id,
                args.reason,
                args.expected_revision,
                args.manager_agent_id,
            )
            .await
        }
        "ptah_reprioritize_work" => {
            let args: ReprioritizeWorkArgs = parse_value(args)?;
            orch.reprioritize_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.priority,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_block_work" => {
            let args: RetryWorkArgs = parse_value(args)?;
            orch.block_work(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_request_review" => {
            let args: RetryWorkArgs = parse_value(args)?;
            orch.request_work_review(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_list_work_decisions" => {
            let args: WorkArgs = parse_value(args)?;
            orch.list_work_decisions_scoped(auth, args.session_id, &args.workspace, &args.work_id)
        }
        "ptah_send_message" => {
            let args: SendMessageArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.body, "body")?;
            orch.send_message(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                args.kind,
                args.from_agent_id,
                args.to_agent_id,
                args.work_id,
                args.body,
                args.payload,
                args.reply_to_id,
                args.attempt_id,
                args.run_id,
            )
            .await
        }
        "ptah_ack_message" => {
            let args: AckMessageArgs = parse_value(args)?;
            orch.ack_message(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.message_id,
            )
            .await
        }
        "ptah_list_inbox" => {
            let args: InboxArgs = parse_value(args)?;
            orch.list_inbox_scoped(
                auth,
                args.session_id,
                &args.workspace,
                &args.agent_id,
                args.after_seq,
            )
        }
        "ptah_list_outbox" => {
            let args: InboxArgs = parse_value(args)?;
            orch.list_outbox_scoped(
                auth,
                args.session_id,
                &args.workspace,
                &args.agent_id,
                args.after_seq,
            )
        }
        "ptah_set_managed_execution" => {
            let args: SetManagedExecutionArgs = parse_value(args)?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.set_managed_execution(
                auth,
                args.session_id,
                &args.workspace,
                &args.agent_id,
                args.policy,
            )
        }
        "ptah_get_managed_execution" => {
            let args: GetManagedExecutionArgs = parse_value(args)?;
            require_nonempty(&args.agent_id, "agent_id")?;
            orch.get_managed_execution(auth, args.session_id, &args.workspace, &args.agent_id)
        }
        "ptah_authorize_work_execution" => {
            let args: AuthorizeWorkExecutionArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            orch.authorize_work_execution(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.work_id,
                args.reason,
                args.expected_revision,
            )
            .await
        }
        "ptah_resolve_work_input" => {
            let args: ResolveWorkInputArgs = parse_value(args)?;
            orch.resolve_work_input(
                auth,
                args.session_id,
                &args.workspace,
                args.permission_id,
                args.allow,
            )
        }
        "ptah_list_execution_intents" => {
            let args: SessionWorkspaceArgs = parse_value(args)?;
            orch.list_execution_intents_scoped(auth, args.session_id, &args.workspace)
        }
        "ptah_fire_routine" => {
            let args: FireRoutineArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.routine_id, "routine_id")?;
            orch.fire_routine(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.routine_id,
                args.payload,
            )
            .await
        }
        "ptah_retry_run" => {
            let args: RetryArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.retry_run(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.run_id,
                args.prompt,
                args.bounds,
                args.execution_mode,
                args.allow_queue,
            )
            .await
        }
        "ptah_approve_run" => {
            let args: ApproveArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.approve_run(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.run_id,
                args.source_fingerprint,
                args.final_fingerprint,
                args.changed_files,
                args.ttl_ms,
            )
            .await
        }
        "ptah_promote_run" => {
            let args: PromoteArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            require_nonempty(&args.approval_id, "approval_id")?;
            orch.promote_run(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.run_id,
                &args.approval_id,
            )
            .await
        }
        "ptah_discard_run" => {
            let args: DiscardArgs = parse_value(args)?;
            require_nonempty(&args.run_id, "run_id")?;
            orch.discard_run(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.run_id,
            )
            .await
        }
        "ptah_get_queue" => {
            let args: QueueScopeArgs = parse_value(args)?;
            orch.get_queue(auth, args.session_id, &args.workspace)
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
        "ptah_edit_queue" => {
            let args: QueueEditArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.entry_id, "entry_id")?;
            orch.edit_queue(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.entry_id,
                args.version,
                args.text,
            )
            .await
        }
        "ptah_remove_queue" => {
            let args: QueueEntryMutationArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.entry_id, "entry_id")?;
            orch.remove_queue(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.entry_id,
                args.expected_version,
            )
            .await
        }
        "ptah_reorder_queue" => {
            let args: QueueReorderArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.entry_id, "entry_id")?;
            orch.reorder_queue(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.entry_id,
                args.to_index,
                args.expected_version,
                args.expected_revision,
            )
            .await
        }
        "ptah_clear_queue" => {
            let args: QueueClearArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            orch.clear_queue(auth, &args.request_id, args.session_id, &args.workspace)
                .await
        }
        "ptah_run_next" => {
            let args: QueueEntryMutationArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.entry_id, "entry_id")?;
            orch.run_next_queue(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.entry_id,
                args.expected_version,
            )
            .await
        }
        "ptah_steer_queued" => {
            let args: QueueEntryMutationArgs = parse_value(args)?;
            require_nonempty(&args.request_id, "request_id")?;
            require_nonempty(&args.entry_id, "entry_id")?;
            orch.steer_queued(
                auth,
                &args.request_id,
                args.session_id,
                &args.workspace,
                &args.entry_id,
                args.expected_version,
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

fn default_manager_in_flight() -> u32 {
    4
}

fn default_manager_replans() -> u32 {
    3
}

/// Discoverable tool names for schema snapshot tests.
pub fn discovered_tool_names() -> Vec<&'static str> {
    CONTROL_TOOLS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentHost, HostConfig};
    use crate::orchestration::{
        ContinuationCheckpoint, ContinuationReason, OrchErrorCode, OrchStore, OrchestrationConfig,
        RunBounds, WorkspaceAllowlist,
    };
    use crate::{home_override_serial, set_grokptah_home_override};
    use chrono::Utc;
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
        assert!(names.contains(&"ptah_list_persistent_agents"));
        assert!(names.contains(&"ptah_resume_persistent_agent"));
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
        for name in [
            "ptah_get_run",
            "ptah_get_progress",
            "ptah_get_events",
            "ptah_get_changes",
            "ptah_get_test_results",
            "ptah_get_handoff",
            "ptah_review_run",
            "ptah_get_computer_run",
            "ptah_get_computer_run_events",
        ] {
            let schema = tool_input_schema(name);
            let required = schema["required"]
                .as_array()
                .expect("scoped read schema required list");
            for key in ["session_id", "workspace", "run_id"] {
                assert!(
                    required.iter().any(|item| item == key),
                    "{name} missing {key}"
                );
            }
        }
        for name in ["ptah_list_computer_runs", "ptah_get_computer_capacity"] {
            let schema = tool_input_schema(name);
            let required = schema["required"]
                .as_array()
                .expect("computer scope schema required list");
            for key in ["session_id", "workspace"] {
                assert!(
                    required.iter().any(|item| item == key),
                    "{name} missing {key}"
                );
            }
        }
    }

    use crate::computer_use::{
        canonical_workspace_string, ActionClass, ActionGrant, ComputerStore, ComputerUseService,
        GrantIssuer, SimulatorBackend,
    };

    struct ComputerFixture {
        srv: ControlServerHandle,
        url: String,
        client: reqwest::Client,
    }

    fn operator_workspace_snapshot(host: &crate::host::AgentHostHandle) -> Value {
        json!({
            "workspace": host.workspace_ui_state(),
            "mcpServers": host.mcp_list(),
            "skills": host.skills(),
        })
    }

    async fn call_tool(
        fixture: &ComputerFixture,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> (reqwest::StatusCode, Value) {
        let response = fixture
            .client
            .post(&fixture.url)
            .header("Authorization", "Bearer computer-token")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap())
    }

    fn computer_orch(
        host: &crate::host::AgentHostHandle,
        home: &std::path::Path,
        roots: Vec<std::path::PathBuf>,
    ) -> Arc<OrchestrationService> {
        OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.join("orch")).unwrap(),
            OrchestrationConfig {
                bearer_token: "computer-token".into(),
                allowlist: WorkspaceAllowlist::new(roots),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        )
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn computer_read_tools_are_scoped_and_fail_indistinguishably() {
        let _guard = home_override_serial();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let ws_a = tempdir().unwrap();
        let ws_b = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().unwrap();
        host.set_project_cwd(ws_a.path()).unwrap();
        let session_a = host.session_new_kind(crate::SessionKind::Build).unwrap();
        host.session_set_cwd(session_a.id, ws_a.path()).unwrap();
        let session_b = host.session_new_kind(crate::SessionKind::Build).unwrap();
        host.session_set_cwd(session_b.id, ws_b.path()).unwrap();

        let canon_a = canonical_workspace_string(ws_a.path()).unwrap();
        let canon_b = canonical_workspace_string(ws_b.path()).unwrap();
        let computer = ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            host.ensure_computer_store().unwrap(),
        );
        let run_a = computer
            .create_run(
                "create-a",
                session_a.id,
                Some(canon_a.clone()),
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        computer
            .create_run(
                "create-b",
                session_b.id,
                Some(canon_b.clone()),
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        let unbound = computer
            .create_run(
                "create-unbound",
                session_a.id,
                None,
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();

        let orch = computer_orch(
            &host,
            home.path(),
            vec![ws_a.path().to_path_buf(), ws_b.path().to_path_buf()],
        );
        let srv = start_control_server(orch, 0).await.unwrap();
        let fixture = ComputerFixture {
            url: format!("http://{}/mcp", srv.addr),
            srv,
            client: reqwest::Client::new(),
        };

        // Coordinator reads inspect a Lane; they must never open it in the
        // local operator cockpit. Keep session_b focused while exercising all
        // four reads against session_a and pin the complete workspace chrome
        // plus its project-derived MCP/skill discovery after every call.
        let operator_before = operator_workspace_snapshot(&host);
        let nullipotent_reads = [
            (
                "ptah_list_computer_runs",
                json!({"session_id": session_a.id, "workspace": ws_a.path()}),
            ),
            (
                "ptah_get_computer_run",
                json!({
                    "session_id": session_a.id,
                    "workspace": ws_a.path(),
                    "run_id": run_a.run_id,
                }),
            ),
            (
                "ptah_get_computer_run_events",
                json!({
                    "session_id": session_a.id,
                    "workspace": ws_a.path(),
                    "run_id": run_a.run_id,
                }),
            ),
            (
                "ptah_get_computer_capacity",
                json!({"session_id": session_a.id, "workspace": ws_a.path()}),
            ),
        ];
        for (index, (name, arguments)) in nullipotent_reads.into_iter().enumerate() {
            let (status, _) = call_tool(&fixture, 100 + index as u64, name, arguments).await;
            assert_eq!(status, StatusCode::OK, "{name} must remain readable");
            assert_eq!(
                operator_workspace_snapshot(&host),
                operator_before,
                "{name} must not change local operator workspace state"
            );
        }

        // Archiving ends execution but preserves durable evidence. The same
        // scoped reads remain available without restoring or promoting the
        // Lane, and the operator's workspace remains untouched.
        host.session_archive(session_a.id, true).unwrap();
        let archived_before = operator_workspace_snapshot(&host);
        let (status, _) = call_tool(
            &fixture,
            110,
            "ptah_get_computer_run",
            json!({
                "session_id": session_a.id,
                "workspace": ws_a.path(),
                "run_id": run_a.run_id,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(operator_workspace_snapshot(&host), archived_before);
        host.session_archive(session_a.id, false).unwrap();

        // Discovery includes exactly the read tools, never a computer mutation.
        let list = fixture
            .client
            .post(&fixture.url)
            .header("Authorization", "Bearer computer-token")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for name in [
            "ptah_list_computer_runs",
            "ptah_get_computer_run",
            "ptah_get_computer_run_events",
            "ptah_get_computer_capacity",
        ] {
            assert!(names.contains(&name), "{name} must be discoverable");
        }
        assert!(
            !names.iter().any(|name| name.contains("computer")
                && !name.contains("get")
                && !name.contains("list")),
            "no computer mutation may be discoverable"
        );

        // Listing is scoped to the session AND the durable workspace binding:
        // the unbound run is invisible even to its own session.
        let (status, body) = call_tool(
            &fixture,
            2,
            "ptah_list_computer_runs",
            json!({"session_id": session_a.id, "workspace": ws_a.path()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let runs = body["result"]["structuredContent"]["runs"]
            .as_array()
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["runId"], run_a.run_id.as_str());

        // Given the same (record, now), GUI and MCP serialize identically.
        // This unstarted run has no clock-derived fields; see the bound-read
        // clock test for a started record. Live MCP calls use Utc::now()
        // independently and do not promise cross-surface identity for
        // elapsedMillis / stale / expired.
        let (status, body) = call_tool(
            &fixture,
            3,
            "ptah_get_computer_run",
            json!({"session_id": session_a.id, "workspace": ws_a.path(), "run_id": run_a.run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let gui = crate::computer_use::project_run_at(
            &computer.get_run(&run_a.run_id).unwrap().unwrap(),
            chrono::Utc::now(),
        );
        assert_eq!(
            body["result"]["structuredContent"],
            serde_json::to_value(&gui).unwrap()
        );

        // Unknown run, another session's run, and an unbound run must produce
        // byte-identical error responses, or the read is an existence oracle.
        let mut error_bodies = Vec::new();
        for run_id in ["no-such-run", "create-b-run", &unbound.run_id] {
            let run_id = if run_id == "create-b-run" {
                // A real run owned by session_b, probed through session_a's scope.
                let listed = call_tool(
                    &fixture,
                    90,
                    "ptah_list_computer_runs",
                    json!({"session_id": session_b.id, "workspace": ws_b.path()}),
                )
                .await
                .1;
                listed["result"]["structuredContent"]["runs"][0]["runId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            } else {
                run_id.to_string()
            };
            let (status, body) = call_tool(
                &fixture,
                7,
                "ptah_get_computer_run",
                json!({"session_id": session_a.id, "workspace": ws_a.path(), "run_id": run_id}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            error_bodies.push(body);
        }
        assert_eq!(error_bodies[0], error_bodies[1]);
        assert_eq!(error_bodies[0], error_bodies[2]);
        assert_eq!(error_bodies[0]["error"]["data"]["code"], "forbidden_scope");

        // Claiming another allowlisted workspace, or an unknown session, is
        // the same unauthorized error as an unknown run — session existence
        // is not distinguishable from cross-scope.
        let (status, cross_workspace) = call_tool(
            &fixture,
            7,
            "ptah_get_computer_run",
            json!({"session_id": session_a.id, "workspace": ws_b.path(), "run_id": run_a.run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, unknown_session) = call_tool(
            &fixture,
            7,
            "ptah_get_computer_run",
            json!({"session_id": Uuid::new_v4(), "workspace": ws_a.path(), "run_id": run_a.run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(cross_workspace, error_bodies[0]);
        assert_eq!(unknown_session, error_bodies[0]);
        assert_eq!(unknown_session["error"]["data"]["code"], "forbidden_scope");

        // Capacity is scoped to the (session, workspace) binding. Host-wide
        // occupancy is absent so the tool cannot count other sessions' runs.
        let (status, body) = call_tool(
            &fixture,
            10,
            "ptah_get_computer_capacity",
            json!({"session_id": session_a.id, "workspace": ws_a.path()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let capacity = &body["result"]["structuredContent"];
        assert_eq!(capacity["boundRuns"], 1);
        assert_eq!(capacity["boundActiveRuns"], 1);
        assert_eq!(capacity["maxRunRecords"], 256);
        assert!(capacity.get("storedRuns").is_none());
        assert!(capacity.get("activeRuns").is_none());
        assert!(capacity.get("sessionRuns").is_none());

        // Events: bounded page, exact tail, and reads are nullipotent — the
        // same request twice returns the identical body.
        let events_args = json!({
            "session_id": session_a.id,
            "workspace": ws_a.path(),
            "run_id": run_a.run_id
        });
        let (status, first) = call_tool(
            &fixture,
            11,
            "ptah_get_computer_run_events",
            events_args.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let page = &first["result"]["structuredContent"];
        assert_eq!(page["entries"].as_array().unwrap().len(), 1);
        assert_eq!(page["entries"][0]["operation"], "create_run");
        assert_eq!(page["nextCursor"], Value::Null);
        assert_eq!(page["cursorExpired"], false);
        let (_, second) = call_tool(
            &fixture,
            11,
            "ptah_get_computer_run_events",
            events_args.clone(),
        )
        .await;
        assert_eq!(first, second);
        let end_seq = page["range"]["endSeq"].as_u64().unwrap();
        let (status, tail) = call_tool(
            &fixture,
            12,
            "ptah_get_computer_run_events",
            json!({
                "session_id": session_a.id,
                "workspace": ws_a.path(),
                "run_id": run_a.run_id,
                "after_seq": end_seq
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(tail["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(tail["result"]["structuredContent"]["cursorExpired"], false);

        // Limit bounds are validated before any read happens.
        for bad_limit in [0, 501] {
            let (status, _) = call_tool(
                &fixture,
                13,
                "ptah_get_computer_run_events",
                json!({
                    "session_id": session_a.id,
                    "workspace": ws_a.path(),
                    "run_id": run_a.run_id,
                    "limit": bad_limit
                }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        // Auth still runs before the body is interpreted for the new tools.
        let unauthenticated = fixture
            .client
            .post(&fixture.url)
            .header("Content-Type", "application/json")
            .body("{\"jsonrpc\":\"2.0\",\"id\":14,\"method\":\"tools/call\",\"params\":{\"name\":\"ptah_list_computer_runs\"")
            .send()
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        fixture.srv.stop();
        set_grokptah_home_override(None);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn computer_event_cursor_below_retention_returns_410() {
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

        let canon = canonical_workspace_string(ws.path()).unwrap();
        let store = host.ensure_computer_store().unwrap();
        let computer = ComputerUseService::new(Arc::new(SimulatorBackend::new()), store.clone());
        let run = computer
            .create_run(
                "create-ring",
                session.id,
                Some(canon),
                SimulatorBackend::demo_target(),
                Default::default(),
            )
            .unwrap();
        // Push the bounded audit ring past its 1024-entry retention so the
        // earliest sequences are genuinely evicted.
        store
            .update_run(&run.run_id, |run| {
                for _ in 0..1100 {
                    run.record_audit("op", "accepted", None, None, None);
                }
                Ok(())
            })
            .unwrap();

        let orch = computer_orch(&host, home.path(), vec![ws.path().to_path_buf()]);
        let srv = start_control_server(orch, 0).await.unwrap();
        let fixture = ComputerFixture {
            url: format!("http://{}/mcp", srv.addr),
            srv,
            client: reqwest::Client::new(),
        };

        // A cursor below the retained window is a hard 410, mirroring
        // ptah_get_events; the gap is never silently skipped. The retained
        // window rides the error so recovery does not need a second get.
        let (status, body) = call_tool(
            &fixture,
            1,
            "ptah_get_computer_run_events",
            json!({
                "session_id": session.id,
                "workspace": ws.path(),
                "run_id": run.run_id,
                "after_seq": 0
            }),
        )
        .await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["error"]["data"]["code"], "cursor_expired");
        let start_seq = body["error"]["data"]["eventRange"]["startSeq"]
            .as_u64()
            .unwrap();
        assert!(start_seq > 1, "eviction must have advanced the window");
        let (status, body) = call_tool(
            &fixture,
            3,
            "ptah_get_computer_run_events",
            json!({
                "session_id": session.id,
                "workspace": ws.path(),
                "run_id": run.run_id,
                "after_seq": start_seq - 1,
                "limit": 500
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let page = &body["result"]["structuredContent"];
        assert_eq!(page["cursorExpired"], false);
        assert_eq!(page["entries"][0]["sequence"].as_u64().unwrap(), start_seq);
        // Omitting the cursor reads from the retained start without expiry.
        let (status, body) = call_tool(
            &fixture,
            4,
            "ptah_get_computer_run_events",
            json!({"session_id": session.id, "workspace": ws.path(), "run_id": run.run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["structuredContent"]["cursorExpired"], false);

        fixture.srv.stop();
        set_grokptah_home_override(None);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn computer_reads_project_interrupted_after_store_restart() {
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
        let canon = canonical_workspace_string(ws.path()).unwrap();

        // First lifetime: a live authorized run, then every store handle is
        // dropped, modeling a process exit. The host's lazy slot is untouched
        // so its later ensure re-opens the ledger and runs recovery.
        let run_id;
        {
            let store =
                ComputerStore::open(crate::discover::grokptah_home().join("computer-use")).unwrap();
            let computer = ComputerUseService::new(Arc::new(SimulatorBackend::new()), store);
            let run = computer
                .create_run(
                    "create-restart",
                    session.id,
                    Some(canon.clone()),
                    SimulatorBackend::demo_target(),
                    Default::default(),
                )
                .unwrap();
            run_id = run.run_id.clone();
            let now = chrono::Utc::now();
            computer
                .authorize(
                    "grant-restart",
                    &run.run_id,
                    run.version,
                    ActionGrant {
                        grant_id: "grant-restart".into(),
                        run_id: run.run_id.clone(),
                        target: run.target.clone(),
                        action_classes: std::collections::BTreeSet::from([ActionClass::Semantic]),
                        issued_by: GrantIssuer::LocalUser,
                        issued_at: now,
                        expires_at: now + chrono::Duration::minutes(5),
                        uses_remaining: None,
                        revoked_at: None,
                    },
                )
                .unwrap();
        }

        let orch = computer_orch(&host, home.path(), vec![ws.path().to_path_buf()]);
        let srv = start_control_server(orch, 0).await.unwrap();
        let fixture = ComputerFixture {
            url: format!("http://{}/mcp", srv.addr),
            srv,
            client: reqwest::Client::new(),
        };

        let (status, body) = call_tool(
            &fixture,
            1,
            "ptah_get_computer_run",
            json!({"session_id": session.id, "workspace": ws.path(), "run_id": run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let projection = &body["result"]["structuredContent"];
        assert_eq!(projection["state"], "interrupted");
        assert_eq!(projection["controlDisposition"], "interrupted");
        assert_eq!(projection["terminal"], true);
        assert_eq!(projection["agentActive"], false);
        assert_eq!(
            projection["grant"],
            Value::Null,
            "authority must not survive restart"
        );
        assert_eq!(
            projection["lastOutcome"],
            Value::Null,
            "restart must not keep a leaky last_outcome"
        );
        assert_eq!(projection["lastError"]["code"], "interrupted");
        assert!(
            projection["lastError"].get("message").is_none(),
            "lastError must be a code-only summary"
        );

        // The journal itself shows the interruption and stays replayable.
        let (status, body) = call_tool(
            &fixture,
            2,
            "ptah_get_computer_run_events",
            json!({"session_id": session.id, "workspace": ws.path(), "run_id": run_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let operations: Vec<&str> = body["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["operation"].as_str())
            .collect();
        assert!(operations.contains(&"create_run"));
        assert!(operations.contains(&"authorize"));
        assert!(operations.contains(&"recover"));

        fixture.srv.stop();
        set_grokptah_home_override(None);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn computer_reads_fail_closed_when_the_ledger_is_unavailable() {
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

        // Hold the exclusive store lock outside the host so its ensure fails.
        let _outside =
            ComputerStore::open(crate::discover::grokptah_home().join("computer-use")).unwrap();

        let orch = computer_orch(&host, home.path(), vec![ws.path().to_path_buf()]);
        let srv = start_control_server(orch, 0).await.unwrap();
        let fixture = ComputerFixture {
            url: format!("http://{}/mcp", srv.addr),
            srv,
            client: reqwest::Client::new(),
        };
        let (status, body) = call_tool(
            &fixture,
            1,
            "ptah_list_computer_runs",
            json!({"session_id": session.id, "workspace": ws.path()}),
        )
        .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(body["error"]["data"]["code"], "unsupported");

        fixture.srv.stop();
        set_grokptah_home_override(None);
    }

    #[test]
    fn queue_control_schemas_require_scope_and_mutation_identity() {
        let scoped = tool_input_schema("ptah_get_queue");
        assert_eq!(scoped["required"], json!(["session_id", "workspace"]));
        for name in [
            "ptah_edit_queue",
            "ptah_remove_queue",
            "ptah_reorder_queue",
            "ptah_clear_queue",
            "ptah_run_next",
            "ptah_steer_queued",
        ] {
            let schema = tool_input_schema(name);
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item == "request_id"),
                "{name} missing request_id"
            );
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item == "session_id"),
                "{name} missing session_id"
            );
            assert_eq!(schema["additionalProperties"], json!(false));
            if name == "ptah_reorder_queue" {
                assert!(
                    schema["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item == "expected_revision"),
                    "{name} missing expected_revision"
                );
            }
        }
    }

    #[test]
    fn persistent_agent_tools_require_scope_and_expose_checkpoint_resume() {
        let list = tool_input_schema("ptah_list_persistent_agents");
        assert_eq!(list["additionalProperties"], json!(false));

        let get = tool_input_schema("ptah_get_persistent_agent");
        assert_eq!(
            get["required"],
            json!(["session_id", "workspace", "agent_id"])
        );
        let resume = tool_input_schema("ptah_resume_persistent_agent");
        for key in [
            "request_id",
            "session_id",
            "workspace",
            "agent_id",
            "prompt",
        ] {
            assert!(resume["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == key));
        }
        assert_eq!(resume["additionalProperties"], json!(false));
    }

    #[test]
    fn persistent_agent_service_scopes_checkpoint_reads_to_session_workspace() {
        let _guard = home_override_serial();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let workspace = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().unwrap();
        let session = host.session_new_kind(crate::SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, workspace.path()).unwrap();
        let agent = host.ensure_session_agent(session.id).unwrap();
        let store = host.ensure_orchestration_store().unwrap();
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: "checkpoint-agent-scope-1".into(),
            agent_id: agent.agent_id.clone(),
            session_id: session.id,
            run_id: "desktop-run-scope-1".into(),
            agent_spec_revision: Some(agent.current_spec().unwrap().revision),
            parent_checkpoint_id: None,
            ordinal: 1,
            workspace: agent.workspace.clone(),
            context_summary: "A bounded verified checkpoint".into(),
            context_hash: String::new(),
            event_seq: 1,
            reason: ContinuationReason::TurnCompleted,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        store.save_checkpoint(&checkpoint).unwrap();
        store
            .update_agent(&agent.agent_id, |current| {
                current.latest_checkpoint_id = Some(checkpoint.checkpoint_id.clone());
                current.continuation_ordinal = checkpoint.ordinal;
                Ok(())
            })
            .unwrap();

        let workspace_path = std::path::PathBuf::from(&agent.workspace);
        assert!(host
            .get_persistent_agent(&agent.agent_id)
            .unwrap()
            .is_some());
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            store.clone(),
            OrchestrationConfig {
                bearer_token: "agent-scope-token".into(),
                allowlist: WorkspaceAllowlist::new([workspace_path.clone()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let auth = orch.auth_header(Some("Bearer agent-scope-token")).unwrap();
        assert!(host
            .get_persistent_agent(&agent.agent_id)
            .unwrap()
            .is_some());
        let listed = orch.list_persistent_agents(&auth).unwrap();
        assert_eq!(listed["agents"].as_array().unwrap().len(), 1);
        let desktop_agent = host
            .list_persistent_agents()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.agent_id == agent.agent_id)
            .unwrap();
        assert_eq!(
            listed["agents"][0],
            serde_json::to_value(&desktop_agent).unwrap(),
            "service/MCP Agent projection must match the desktop runtime record"
        );
        let plan = orch
            .get_persistent_agent_scoped(&auth, session.id, &workspace_path, &agent.agent_id)
            .unwrap();
        assert_eq!(
            plan["agent"],
            serde_json::to_value(&desktop_agent).unwrap(),
            "scoped service Agent reads must not rewrite transport-neutral state"
        );
        assert_eq!(
            plan["checkpoint"]["checkpointId"],
            "checkpoint-agent-scope-1"
        );

        let error = orch
            .get_persistent_agent_scoped(&auth, session.id, &workspace_path, "agent-not-visible")
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::ForbiddenScope);
        set_grokptah_home_override(None);
    }
}
