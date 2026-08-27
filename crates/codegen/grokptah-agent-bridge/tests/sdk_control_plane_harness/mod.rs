//! Hermetic, offline qualification harness for an SDK-typed service control
//! plane driven over the **real** loopback MCP transport.
//!
//! Nothing here is a scripted double. Every assertion in the qualification
//! suite travels over a bound TCP listener, through the production axum
//! router built by [`start_control_server`], through the same
//! `authenticate_request` middleware, session table, tool allowlist, and
//! `OrchestrationService` policy the desktop host uses. The only thing the
//! harness owns is a disposable `GROKPTAH_HOME`, a synthetic workspace, and
//! the offline agent switch, so no provider, credential, or user data is ever
//! reachable from a test.
//!
//! The adapter under qualification is [`SdkServiceControlPlane`]. It speaks
//! `grokptah-agent-sdk` DTOs to callers and performs the explicit request and
//! response translation the live routes require. That translation is the
//! artifact being qualified: it is deliberately written by hand, so a wire
//! change breaks a test instead of being absorbed by a permissive double.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    canonical_workspace_string, set_grokptah_home_override, start_control_server, AgentHost,
    AgentHostHandle, ComputerClientIdentity, ComputerError, ComputerErrorCode,
    ComputerGrantRequest, ComputerRun, ComputerRunController, ComputerStore, ComputerUseService,
    ControlServerHandle, HostConfig, SessionKind, SimulatorBackend,
};
use grokptah_agent_sdk::{
    ComputerActionClass, ComputerControlRequest, ComputerControlResponse, ComputerEvent,
    ComputerEventPage, DurableRun, DurableRunState, ErrorCode, ErrorEnvelope, ExecutionMode,
    RunEventPage, RunScope as SdkRunScope, SubmitTaskRequest,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::common::ProcessEnvGuard;

/// Bearer token used by every harness server. It never leaves loopback and is
/// not a credential for anything outside the test process.
pub const HARNESS_TOKEN: &str = "sdk-control-plane-qualification";

/// A disposable, offline GrokPtah service instance.
///
/// Owns a temporary home, a synthetic workspace, the shared host, the durable
/// orchestration store, and the bound control server. Dropping it restores the
/// process environment and releases the home override.
pub struct DisposableService {
    _home: tempfile::TempDir,
    _env: ProcessEnvGuard,
    pub workspace: tempfile::TempDir,
    pub host: AgentHostHandle,
    pub orch: std::sync::Arc<OrchestrationService>,
    pub server: Option<ControlServerHandle>,
    computer: Arc<ComputerUseService>,
}

impl DisposableService {
    /// Launch a real control server bound to an ephemeral loopback port.
    ///
    /// The agent runtime is forced offline, so a submitted run exercises the
    /// durable lifecycle without reaching any provider.
    pub async fn launch() -> Self {
        let mut env = ProcessEnvGuard::new();
        let home = tempfile::tempdir().expect("disposable home");
        std::fs::create_dir_all(home.path().join(".grokptah")).expect("home layout");
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        env.set("GROKPTAH_AGENT_OFFLINE", "1");

        let workspace = tempfile::tempdir().expect("synthetic workspace");
        // A synthetic workspace with real content, so workspace-scoped reads
        // resolve against a directory that actually exists on disk.
        std::fs::write(workspace.path().join("README.md"), "# synthetic\n")
            .expect("workspace seed file");

        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().expect("host starts");
        host.set_project_cwd(workspace.path()).expect("project cwd");

        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.path().join("orch")).expect("durable orchestration store"),
            OrchestrationConfig {
                bearer_token: HARNESS_TOKEN.into(),
                allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
                max_concurrent_runs: 4,
                bounds: RunBounds::default(),
            },
        );

        // Install the host-side backend owner the MCP mutation routes require.
        // `ComputerRunController` has no implementation inside this crate: the
        // desktop registers `DesktopComputerUse`. The harness therefore
        // installs a delegate that mirrors that production path exactly (see
        // `HarnessComputerController`), so lease, revision, audit, and
        // redaction logic all remain the real service's.
        let computer = Arc::new(ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            host.ensure_computer_store().expect("computer store opens"),
        ));
        host.set_computer_run_controller(Arc::new(HarnessComputerController {
            service: computer.clone(),
        }));

        let server = start_control_server(orch.clone(), 0)
            .await
            .expect("control server binds loopback");

        Self {
            _home: home,
            _env: env,
            workspace,
            host,
            orch,
            server: Some(server),
            computer,
        }
    }

    /// Loopback base URL of the running transport.
    pub fn base_url(&self) -> String {
        format!(
            "http://{}",
            self.server.as_ref().expect("server running").addr
        )
    }

    /// The canonical workspace string the authority compares scopes against.
    pub fn canonical_workspace(&self) -> String {
        canonical_workspace_string(self.workspace.path()).expect("canonical workspace")
    }

    /// Create a host-owned Build session already bound to the workspace.
    ///
    /// Session creation is deliberately *not* reachable from the transport:
    /// `ptah_create_session` is in `FORBIDDEN_TOOLS`. The host is the
    /// authority that mints sessions; the control plane only observes and
    /// operates within one.
    pub fn new_build_session(&self) -> Uuid {
        let session = self
            .host
            .session_new_kind(SessionKind::Build)
            .expect("build session");
        self.host
            .session_set_cwd(session.id, self.workspace.path())
            .expect("session workspace binding");
        session.id
    }

    /// Create a session bound to a *different* workspace, for cross-scope
    /// denial checks.
    pub fn new_session_in(&self, other: &std::path::Path) -> Uuid {
        let session = self
            .host
            .session_new_kind(SessionKind::Build)
            .expect("build session");
        self.host
            .session_set_cwd(session.id, other)
            .expect("session workspace binding");
        session.id
    }

    /// The shared durable Computer Use ledger this host owns.
    pub fn computer_store(&self) -> ComputerStore {
        self.host
            .ensure_computer_store()
            .expect("computer store opens")
    }

    /// The *same* Computer Use service the registered controller delegates to
    /// and the transport reads through. Runs are created here because the
    /// control plane exposes no creation route: creation is host authority.
    pub fn computer_service(&self) -> Arc<ComputerUseService> {
        self.computer.clone()
    }

    /// Read a durable Computer Use record straight from the shared ledger,
    /// so a test can compare the public projection against the truth the
    /// authority stored rather than against another projection.
    pub fn stored_computer_run(&self, run_id: &str) -> grokptah_agent_bridge::ComputerRun {
        self.computer_service()
            .get_run(run_id)
            .expect("computer ledger readable")
            .expect("computer run present")
    }

    /// Connect an SDK-typed adapter to this instance.
    pub fn control_plane(&self) -> SdkServiceControlPlane {
        SdkServiceControlPlane::new(self.base_url(), HARNESS_TOKEN)
    }

    /// Stop the transport and wait for it to release the durable store.
    pub async fn shutdown(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop_and_wait().await;
        }
    }
}

impl Drop for DisposableService {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        set_grokptah_home_override(None);
    }
}

/// A denial or failure as the *public* boundary reports it.
///
/// `envelope` is parsed from the JSON-RPC `error.data` object, which the
/// service builds from `grokptah_agent_sdk::ErrorEnvelope`. `raw` is retained
/// so a test can assert that nothing beyond the public projection was sent.
#[derive(Debug, Clone)]
pub struct TransportFailure {
    pub http_status: u16,
    pub envelope: ErrorEnvelope,
    pub raw: Value,
}

impl TransportFailure {
    pub fn code(&self) -> ErrorCode {
        self.envelope.code
    }

    pub fn reason(&self) -> Option<&str> {
        self.envelope.reason_code.as_deref()
    }
}

/// A tool call that returned `isError: false` but whose structured content
/// could not be read as the SDK DTO the contract advertises.
#[derive(Debug)]
pub enum AdapterError {
    /// The transport refused the call.
    Denied(TransportFailure),
    /// The call succeeded but the payload did not match the SDK contract.
    Contract {
        route: &'static str,
        dto: &'static str,
        detail: String,
        payload: Value,
    },
}

impl AdapterError {
    pub fn denied(self) -> TransportFailure {
        match self {
            Self::Denied(failure) => failure,
            Self::Contract {
                route, dto, detail, ..
            } => panic!(
                "expected a transport denial from {route}, got a {dto} contract miss: {detail}"
            ),
        }
    }

    pub fn contract_detail(&self) -> Option<&str> {
        match self {
            Self::Contract { detail, .. } => Some(detail),
            Self::Denied(_) => None,
        }
    }
}

/// The receipt `ptah_submit_task` actually returns.
///
/// This is intentionally *not* `grokptah_agent_sdk::DurableRun`: the accept
/// receipt carries admission data (`queuedPosition`, `executionMode`) and
/// omits the durable projection fields. Typing it separately — with SDK enums
/// for the shared vocabulary — pins both facts.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitReceipt {
    pub run_id: String,
    pub session_id: String,
    pub state: DurableRunState,
    pub request_id: String,
    pub execution_mode: ExecutionMode,
    #[serde(default)]
    pub queued_position: Option<usize>,
}

/// Negotiated transport identity returned by a real `initialize`.
#[derive(Debug, Clone)]
pub struct Negotiated {
    pub protocol_version: String,
    pub session_id: String,
    pub server_name: String,
    pub capability_contract: Value,
    pub raw: Value,
}

/// An SDK-typed control-plane adapter over the live MCP transport.
///
/// Every method here performs the explicit translation the real routes
/// require. It holds no credential beyond the loopback bearer token it was
/// constructed with and never reads process state.
pub struct SdkServiceControlPlane {
    http: reqwest::Client,
    base_url: String,
    token: String,
    session_id: Option<String>,
    next_id: AtomicU64,
}

impl SdkServiceControlPlane {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            token: token.into(),
            session_id: None,
            next_id: AtomicU64::new(1),
        }
    }

    /// Drop the negotiated session without telling the server, to prove which
    /// operations require an initialized client session.
    pub fn forget_session(&mut self) {
        self.session_id = None;
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn next_rpc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// One JSON-RPC round trip over the real `/mcp` route.
    pub async fn rpc(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(Value, reqwest::header::HeaderMap), TransportFailure> {
        let id = self.next_rpc_id();
        let mut request = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json");
        if let Some(session) = &self.session_id {
            request = request.header("mcp-session-id", session.clone());
        }
        let response = request
            .json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .send()
            .await
            .expect("loopback transport reachable");

        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body: Value = response.json().await.expect("service returns JSON");

        if let Some(error) = body.get("error") {
            let data = error.get("data").cloned().unwrap_or(Value::Null);
            let envelope: ErrorEnvelope = serde_json::from_value(data).unwrap_or_else(|failure| {
                panic!(
                    "service error data must deserialize into the SDK ErrorEnvelope \
                     ({failure}); got {error}"
                )
            });
            return Err(TransportFailure {
                http_status: status,
                envelope,
                raw: body,
            });
        }
        let result = body.get("result").cloned().unwrap_or(Value::Null);
        Ok((result, headers))
    }

    /// Real MCP handshake. Captures the server-issued `mcp-session-id`, which
    /// is the only thing that makes this client a grant-capable actor.
    pub async fn initialize(
        &mut self,
        client_name: &str,
        client_version: &str,
    ) -> Result<Negotiated, TransportFailure> {
        let (result, headers) = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": client_name, "version": client_version },
                }),
            )
            .await?;
        let session_id = headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .expect("initialize must issue an mcp-session-id");
        self.session_id = Some(session_id.clone());

        // The spec handshake is only complete after the initialized
        // notification; the service marks the session grant-capable there.
        let _ = self.rpc("notifications/initialized", json!({})).await?;

        Ok(Negotiated {
            protocol_version: result["protocolVersion"]
                .as_str()
                .expect("negotiated protocol version")
                .to_string(),
            session_id,
            server_name: result["serverInfo"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            capability_contract: result["serverInfo"]["capabilityContract"].clone(),
            raw: result,
        })
    }

    /// Tool names the live route advertises, in wire order.
    pub async fn list_tools(&self) -> Result<Vec<String>, TransportFailure> {
        let (result, _) = self.rpc("tools/list", json!({})).await?;
        Ok(result["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_string))
            .collect())
    }

    /// Call a tool with already-translated arguments and return its
    /// `structuredContent`.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, TransportFailure> {
        let (result, _) = self
            .rpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        Ok(result["structuredContent"].clone())
    }

    /// Deliberately naive path: send an SDK DTO to a route with **no**
    /// translation. Used to pin exactly which contracts are wire-compatible
    /// as-is and which require an adapter.
    pub async fn call_tool_with_untranslated_dto<T: serde::Serialize>(
        &self,
        name: &str,
        dto: &T,
    ) -> Result<Value, TransportFailure> {
        self.call_tool(name, serde_json::to_value(dto).expect("dto serializes"))
            .await
    }

    // ── Durable run routes ────────────────────────────────────────────────

    /// Submit a task from an SDK `SubmitTaskRequest`.
    ///
    /// The route's argument struct is snake_case with `deny_unknown_fields`,
    /// while the SDK DTO is camelCase, so the adapter must rename every field
    /// explicitly. `bounds` is forwarded as the route's nested object.
    pub async fn submit_task(
        &self,
        request: &SubmitTaskRequest,
    ) -> Result<SubmitReceipt, AdapterError> {
        request
            .validate()
            .expect("caller validates the SDK DTO before transport");

        let mut arguments = Map::new();
        arguments.insert("request_id".into(), json!(request.request_id));
        arguments.insert("session_id".into(), json!(request.session_id));
        arguments.insert("workspace".into(), json!(request.workspace));
        arguments.insert("prompt".into(), json!(request.prompt));
        if let Some(bounds) = &request.bounds {
            // Absent ceilings must be omitted, not sent as null: the route
            // rejects a present bounds key whose value is not a positive
            // number, so `Option::None` cannot be forwarded verbatim.
            let mut merged = Map::new();
            if let Some(value) = bounds.max_prompt_bytes {
                merged.insert("max_prompt_bytes".into(), json!(value));
            }
            if let Some(value) = bounds.max_rounds {
                merged.insert("max_rounds".into(), json!(value));
            }
            if let Some(value) = bounds.max_duration_ms {
                merged.insert("max_duration_ms".into(), json!(value));
            }
            if !merged.is_empty() {
                arguments.insert("bounds".into(), Value::Object(merged));
            }
        }
        if let Some(mode) = request.execution_mode {
            arguments.insert(
                "execution_mode".into(),
                serde_json::to_value(mode).expect("execution mode serializes"),
            );
        }
        if let Some(allow_queue) = request.allow_queue {
            arguments.insert("allow_queue".into(), json!(allow_queue));
        }

        let payload = self
            .call_tool("ptah_submit_task", Value::Object(arguments))
            .await
            .map_err(AdapterError::Denied)?;

        serde_json::from_value(payload.clone()).map_err(|failure| AdapterError::Contract {
            route: "ptah_submit_task",
            dto: "SubmitReceipt",
            detail: failure.to_string(),
            payload,
        })
    }

    /// Read a durable run and project it into the SDK's `DurableRun`.
    pub async fn get_run(&self, scope: &SdkRunScope) -> Result<DurableRun, AdapterError> {
        scope.validate().expect("caller validates the scope fence");
        let payload = self
            .call_tool("ptah_get_run", scope_args(scope))
            .await
            .map_err(AdapterError::Denied)?;
        let run: DurableRun =
            serde_json::from_value(payload.clone()).map_err(|failure| AdapterError::Contract {
                route: "ptah_get_run",
                dto: "DurableRun",
                detail: failure.to_string(),
                payload: payload.clone(),
            })?;
        run.validate().map_err(|detail| AdapterError::Contract {
            route: "ptah_get_run",
            dto: "DurableRun",
            detail: detail.to_string(),
            payload,
        })?;
        Ok(run)
    }

    /// Read a cursor-paged durable event window as the SDK's `RunEventPage`.
    pub async fn get_events(
        &self,
        scope: &SdkRunScope,
        after_seq: u64,
        limit: usize,
    ) -> Result<RunEventPage, AdapterError> {
        let mut arguments = scope_args(scope);
        arguments["after_seq"] = json!(after_seq);
        arguments["limit"] = json!(limit);
        let payload = self
            .call_tool("ptah_get_events", arguments)
            .await
            .map_err(AdapterError::Denied)?;
        let page: RunEventPage =
            serde_json::from_value(payload.clone()).map_err(|failure| AdapterError::Contract {
                route: "ptah_get_events",
                dto: "RunEventPage",
                detail: failure.to_string(),
                payload: payload.clone(),
            })?;
        for entry in &page.entries {
            entry.validate().map_err(|detail| AdapterError::Contract {
                route: "ptah_get_events",
                dto: "RunEvent",
                detail: detail.to_string(),
                payload: payload.clone(),
            })?;
        }
        Ok(page)
    }

    /// Cancel a durable run under an exact scope fence.
    pub async fn cancel_run(
        &self,
        request_id: &str,
        scope: &SdkRunScope,
    ) -> Result<Value, TransportFailure> {
        let mut arguments = scope_args(scope);
        arguments["request_id"] = json!(request_id);
        self.call_tool("ptah_cancel", arguments).await
    }

    // ── Computer Use routes ───────────────────────────────────────────────

    /// Issue a lease from an SDK `ComputerControlRequest`.
    ///
    /// The route flattens the scope, renames every field, and takes
    /// `action_classes` as a set. The response is a `ComputerRunProjection`,
    /// not an SDK `ComputerControlResponse`, so the adapter also builds the
    /// public response from the projection plus the caller's own scope.
    pub async fn authorize_computer_run(
        &self,
        request: &ComputerControlRequest,
        uses_remaining: Option<u32>,
    ) -> Result<(ComputerControlResponse, Value), AdapterError> {
        request
            .validate()
            .expect("caller validates the SDK lease request");

        let mut arguments = scope_args(&request.scope);
        arguments["request_id"] = json!(request.request_id);
        arguments["expected_version"] = json!(request.expected_version);
        arguments["ttl_ms"] = json!(request.ttl_ms);
        arguments["action_classes"] = Value::Array(
            request
                .action_classes
                .iter()
                .map(|class| match class {
                    ComputerActionClass::Semantic => json!("semantic"),
                    ComputerActionClass::TextEntry => json!("text_entry"),
                })
                .collect(),
        );
        if let Some(uses) = uses_remaining {
            arguments["uses_remaining"] = json!(uses);
        }

        let payload = self
            .call_tool("ptah_authorize_computer_run", arguments)
            .await
            .map_err(AdapterError::Denied)?;

        let response = computer_control_response(&request.scope, &payload).ok_or_else(|| {
            AdapterError::Contract {
                route: "ptah_authorize_computer_run",
                dto: "ComputerControlResponse",
                detail: "projection lacks version or controlDisposition".into(),
                payload: payload.clone(),
            }
        })?;
        response
            .validate()
            .map_err(|detail| AdapterError::Contract {
                route: "ptah_authorize_computer_run",
                dto: "ComputerControlResponse",
                detail: detail.to_string(),
                payload: payload.clone(),
            })?;
        Ok((response, payload))
    }

    /// Read the redacted Computer Use projection for a run.
    pub async fn get_computer_run(&self, scope: &SdkRunScope) -> Result<Value, TransportFailure> {
        self.call_tool("ptah_get_computer_run", scope_args(scope))
            .await
    }

    /// Read the redacted Computer Use audit journal as the SDK's
    /// `ComputerEventPage`.
    ///
    /// The route's entries are `ComputerAuditEntry` records
    /// (`sequence`/`at`/`operation`/…), so the adapter maps each one onto the
    /// SDK's `seq`/`ts`/`kind`/`detail` shape rather than deserializing.
    pub async fn get_computer_run_events(
        &self,
        scope: &SdkRunScope,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<(ComputerEventPage, Value), AdapterError> {
        let mut arguments = scope_args(scope);
        if let Some(after) = after_seq {
            arguments["after_seq"] = json!(after);
        }
        arguments["limit"] = json!(limit);
        let payload = self
            .call_tool("ptah_get_computer_run_events", arguments)
            .await
            .map_err(AdapterError::Denied)?;

        let page = computer_event_page(&payload).ok_or_else(|| AdapterError::Contract {
            route: "ptah_get_computer_run_events",
            dto: "ComputerEventPage",
            detail: "audit page lacks entries/cursor fields".into(),
            payload: payload.clone(),
        })?;
        for entry in &page.entries {
            entry.validate().map_err(|detail| AdapterError::Contract {
                route: "ptah_get_computer_run_events",
                dto: "ComputerEvent",
                detail: detail.to_string(),
                payload: payload.clone(),
            })?;
        }
        Ok((page, payload))
    }
}

/// Flatten an SDK scope fence into the route's snake_case arguments.
fn scope_args(scope: &SdkRunScope) -> Value {
    json!({
        "session_id": scope.session_id,
        "workspace": scope.workspace,
        "run_id": scope.run_id,
    })
}

/// Build the SDK's public control response from the live run projection.
fn computer_control_response(
    scope: &SdkRunScope,
    projection: &Value,
) -> Option<ComputerControlResponse> {
    Some(ComputerControlResponse {
        scope: scope.clone(),
        version: projection.get("version")?.as_u64()?,
        disposition: projection.get("controlDisposition")?.as_str()?.to_string(),
    })
}

/// Map the live audit page onto the SDK's redacted event page.
fn computer_event_page(payload: &Value) -> Option<ComputerEventPage> {
    let entries = payload.get("entries")?.as_array()?;
    let mapped = entries
        .iter()
        .map(|entry| {
            let mut detail = Map::new();
            for key in ["disposition", "actionClass", "observationId", "errorCode"] {
                if let Some(value) = entry.get(key) {
                    detail.insert(key.to_string(), value.clone());
                }
            }
            ComputerEvent {
                seq: entry.get("sequence").and_then(Value::as_u64).unwrap_or(0),
                ts: entry
                    .get("at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                kind: entry
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                detail: Value::Object(detail),
            }
        })
        .collect();
    Some(ComputerEventPage {
        entries: mapped,
        next_cursor: payload.get("nextCursor").and_then(Value::as_u64),
        cursor_expired: payload
            .get("cursorExpired")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Build an SDK scope fence from harness identities.
pub fn scope(session: Uuid, workspace: &str, run_id: &str) -> SdkRunScope {
    SdkRunScope {
        session_id: session.to_string(),
        workspace: workspace.to_string(),
        run_id: run_id.to_string(),
    }
}

/// Recursively assert that no host path, home directory, or bearer token
/// appears anywhere in a public projection.
pub fn assert_no_privileged_leak(payload: &Value, forbidden: &[&str]) {
    let rendered = serde_json::to_string(payload).expect("payload renders");
    for needle in forbidden {
        assert!(
            !rendered.contains(needle),
            "public projection leaked {needle}: {rendered}"
        );
    }
}

/// Absolute path helper for building an out-of-allowlist workspace claim.
pub fn foreign_workspace() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("foreign workspace");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// Host-side backend owner installed by the harness.
///
/// This mirrors the desktop's `DesktopComputerUse` MCP mutation path
/// (`desktop/src-tauri/src/computer_use.rs`) one-for-one: the same
/// owner-session plus workspace scope filter, the same `grant_request`
/// validation against the run's own limits, the same delegation into the
/// shared [`ComputerUseService`], and the same server-derived
/// `client.actor_id()` as the grant actor. It adds no policy of its own, so
/// every lease, revision fence, audit entry, and redaction under test is the
/// real service's behaviour rather than a fixture's.
struct HarnessComputerController {
    service: Arc<ComputerUseService>,
}

impl HarnessComputerController {
    /// Exactly the desktop's `controller_run` gate: a run is reachable only
    /// through the session that owns it *and* the workspace it was bound to.
    fn scoped_run(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
    ) -> Result<ComputerRun, ComputerError> {
        self.service
            .get_run(run_id)?
            .filter(|run| {
                run.owner_session_id == owner_session_id
                    && run.workspace.as_deref() == Some(workspace)
            })
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "Computer Run is not available to this client scope",
                )
            })
    }
}

#[async_trait]
impl ComputerRunController for HarnessComputerController {
    async fn authorize(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        grant_request: ComputerGrantRequest,
    ) -> Result<ComputerRun, ComputerError> {
        let run = self.scoped_run(owner_session_id, workspace, run_id)?;
        grant_request.validate(run.limits)?;
        self.service.authorize_mcp_client(
            request_id,
            run_id,
            expected_version,
            client.actor_id(),
            grant_request,
        )
    }

    async fn pause(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service
            .pause(request_id, run_id, expected_version)
            .await
    }

    async fn take_over(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service
            .take_over(request_id, run_id, expected_version)
            .await
    }

    async fn cancel(
        &self,
        _client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        _expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service.cancel(request_id, run_id).await
    }
}
