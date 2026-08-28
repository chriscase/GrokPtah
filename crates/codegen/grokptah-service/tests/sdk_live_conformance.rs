//! The public SDK contract, run against a **real service process**.
//!
//! Everything below PR #431 proved the seam against a scripted transport: the
//! wire *shape* was right, but no line of the runtime had ever answered one of
//! these calls. This file closes that gap. It boots the real
//! `grokptah-service` over a real loopback socket against a disposable
//! `GROKPTAH_HOME`, points the published `ServiceControlPlane` at it through a
//! transport that does nothing but JSON-RPC framing, and runs the same
//! versioned conformance battery the fake runs.
//!
//! Two properties make the result meaningful rather than decorative:
//!
//! * **The adapter is unmodified.** No test-only constructor, no bypass. The
//!   plane under test is the one a consumer would build.
//! * **No provider is reachable.** `GROKPTAH_AGENT_OFFLINE=1` is set for the
//!   whole environment, so a model turn cannot be attempted. Where a check
//!   needs a run in a state only a provider could produce, the durable record
//!   is seeded through the store — the same technique the service's own
//!   conformance suite uses — and that is called out, never disguised as a
//!   live turn.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use grokptah_agent_bridge::orchestration::RunState;
use grokptah_agent_bridge::{
    start_control_from_env, AgentHost, AgentHostHandle, HostConfig, McpControlClient,
};
use grokptah_agent_sdk::conformance::{self, CheckOutcome, Harness};
use grokptah_agent_sdk::prelude::*;
use grokptah_agent_sdk::service::{McpTransport, ServiceControlPlane, TransportFault};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use common::{create_build_session, mcp_client, start_isolated, ServiceEnv, TOKEN};

// ── The transport ─────────────────────────────────────────────────────────

/// JSON-RPC framing over the service's real HTTP control plane, and nothing
/// else.
///
/// Deliberately built on `rpc_raw` rather than `McpControlClient::call_tool`.
/// `call_tool` collapses a JSON-RPC error into `anyhow`, keeping only
/// `error.data.code`; the contract's error taxonomy also needs the message and
/// the sibling diagnostic fields the host merges into `data` (a
/// `cursor_expired` carries its `eventRange` there). Going through the raw
/// envelope means the adapter sees exactly what the host sent, so a mapping
/// bug shows up as a failed check instead of being smoothed over here.
struct LiveTransport {
    client: Mutex<McpControlClient>,
}

impl LiveTransport {
    async fn connect(addr: std::net::SocketAddr) -> Self {
        let mut client = McpControlClient::new(format!("http://{addr}"), TOKEN);
        client.initialize().await.expect("initialize MCP client");
        Self {
            client: Mutex::new(client),
        }
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, TransportFault> {
        let mut client = self.client.lock().await;
        let (_status, body) =
            client
                .rpc_raw("2.0", method, params, true)
                .await
                .map_err(|error| TransportFault::Unreachable {
                    detail: error.to_string(),
                })?;
        if let Some(error) = body.get("error") {
            // The status code is not consulted: the host's typed code is the
            // authority, and a 410 carrying `cursor_expired` must map the same
            // way whether or not the transport noticed the status.
            return Err(TransportFault::from_jsonrpc_error(error));
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| TransportFault::Malformed {
                detail: "response carried neither `result` nor `error`".into(),
            })
    }
}

#[async_trait]
impl McpTransport for LiveTransport {
    async fn list_tools(&self) -> Result<Vec<String>, TransportFault> {
        let result = self.rpc("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| TransportFault::Malformed {
                detail: "tools/list result has no `tools` array".into(),
            })?;
        Ok(tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect())
    }

    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, TransportFault> {
        let result = self
            .rpc(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await?;
        // A tool that reports failure in-band rather than as a JSON-RPC error
        // must still reach the adapter as a fault, or a refusal would read as
        // a successful empty answer.
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(TransportFault::from_jsonrpc_error(
                result.get("structuredContent").unwrap_or(&Value::Null),
            ));
        }
        Ok(result
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

// ── The harness ───────────────────────────────────────────────────────────

struct LiveHarness {
    plane: ServiceControlPlane<LiveTransport>,
    host: AgentHostHandle,
    session: SessionView,
    workspace: PathBuf,
    next: AtomicU64,
}

#[async_trait]
impl Harness for LiveHarness {
    fn plane(&self) -> &dyn AgentControlPlane {
        &self.plane
    }

    async fn owned_session(&self) -> SessionView {
        self.session.clone()
    }

    async fn foreign_workspace(&self) -> Option<WorkspaceRef> {
        // Skipped, and the skip is the finding. A `WorkspaceRef` exists only
        // once the host has reported that workspace, so a consumer cannot name
        // one the allowlist would reject — there is no such ref to hand back.
        // Against the fake this check runs because the fake lets the harness
        // mint refs; against a real host the property it tests is enforced one
        // layer earlier, by construction.
        None
    }

    async fn drive_to_completion(&self, run_id: &RunId) -> bool {
        // A real terminal transition needs a provider turn, and this
        // environment has none by design. The durable record is moved to its
        // terminal state through the store instead — the same seam the
        // service's own conformance suite uses. Everything the battery then
        // reads is a genuine round trip through the control plane over HTTP;
        // only the *cause* of the transition is synthetic.
        let Ok(store) = self.host.ensure_orchestration_store() else {
            return false;
        };
        let Ok(Some(mut run)) = store.load_run(run_id.as_str()) else {
            return false;
        };
        run.state = RunState::Completed;
        run.end_seq = Some(self.host.event_bus().next_seq());
        run.updated_at = Utc::now();
        run.terminal_result = Some("completed".into());
        store.save_run(&run).is_ok()
    }

    fn next_request_id(&self) -> RequestId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        RequestId::new(format!("live-battery-{n:06}")).expect("minted id is valid")
    }
}

// ── The battery ───────────────────────────────────────────────────────────

async fn live_harness(
    env: &ServiceEnv,
    addr: std::net::SocketAddr,
    host: AgentHostHandle,
) -> LiveHarness {
    // One session is created through the raw client so the host has something
    // to report. The adapter then *learns* the workspace from that report —
    // it is never told a path.
    let mut seed = mcp_client(addr).await;
    create_build_session(&mut seed, &env.workspace_path(), "live-battery").await;

    let plane = ServiceControlPlane::read_only(LiveTransport::connect(addr).await)
        .with_operator_authority();

    let sessions = plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list sessions over the live control plane");
    let session = sessions
        .items
        .into_iter()
        .next()
        .expect("the seeded session is visible to the adapter");

    LiveHarness {
        plane,
        host,
        session,
        workspace: env.workspace_path(),
        next: AtomicU64::new(1),
    }
}

#[tokio::test]
async fn the_versioned_battery_runs_against_a_real_service_process() {
    let env = ServiceEnv::new();
    let service = start_isolated(&env, vec![env.workspace_path()], 4).await;
    let harness = live_harness(&env, service.addr, service.host()).await;

    let report = conformance::run_battery(&harness).await;

    // The matrix is the deliverable. Print it whole so a reviewer sees every
    // skip and its stated reason rather than a bare pass count.
    println!(
        "\n=== live service adapter matrix ===\n{}",
        report.summary()
    );
    for check in &report.checks {
        let outcome = match &check.outcome {
            CheckOutcome::Passed => "PASS".to_string(),
            CheckOutcome::Skipped(why) => format!("SKIP  ({why})"),
            CheckOutcome::Failed(why) => format!("FAIL  ({why})"),
        };
        println!("  {outcome:<8} {}", check.name);
    }

    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| matches!(c.outcome, CheckOutcome::Failed(_)))
        .map(|c| c.name)
        .collect();
    assert!(failed.is_empty(), "live battery failures: {failed:?}");

    // A battery that skipped everything would "pass" while proving nothing.
    assert!(
        report.passed_count() >= 10,
        "too few live checks actually ran: {}",
        report.summary()
    );

    let _ = &harness.workspace;
    service.stop_and_wait().await;
}

#[tokio::test]
async fn the_live_host_serves_redacted_receipts() {
    let env = ServiceEnv::new();
    let service = start_isolated(&env, vec![env.workspace_path()], 4).await;
    let harness = live_harness(&env, service.addr, service.host()).await;

    let advertised = harness
        .plane
        .transport()
        .list_tools()
        .await
        .expect("tools/list");
    println!(
        "host advertises ptah_list_receipts: {}",
        advertised.iter().any(|t| t == "ptah_list_receipts")
    );

    let accepted = harness
        .plane
        .submit_task(TaskSubmission {
            request_id: harness.next_request_id(),
            session_id: harness.session.session_id.clone(),
            workspace: harness.session.workspace.clone(),
            prompt: "LIVE-SECRET-PROMPT-do-not-echo".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit over the live control plane");

    let selector = RunSelector {
        session_id: harness.session.session_id.clone(),
        workspace: harness.session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };

    match harness
        .plane
        .list_receipts(selector, PageRequest::new())
        .await
    {
        Err(error) => panic!("live receipt listing failed: {error:?}"),
        Ok(page) => {
            println!(
                "live receipts: {} item(s), retention {:?}",
                page.items.len(),
                page.retention
            );
            let encoded = serde_json::to_string(&page).expect("serialize");
            assert!(
                !encoded.contains("LIVE-SECRET-PROMPT-do-not-echo"),
                "a live receipt echoed the prompt: {encoded}"
            );
            for absent in ["response", "workspace", "/tmp", "message"] {
                assert!(!encoded.contains(absent), "`{absent}` leaked: {encoded}");
            }
            assert!(
                page.items
                    .iter()
                    .all(|r| r.payload_digest.as_str().len() == AttemptDigest::BYTES * 2),
                "host digest is not the advertised width"
            );
        }
    }
    service.stop_and_wait().await;
}

// ── The Desktop embedded control server ───────────────────────────────────

/// The same contract, over the server the **Desktop** embeds.
///
/// `desktop/src-tauri/src/lib.rs` starts its control plane with exactly one
/// call — `start_control_from_env(host)` — so that function *is* the Desktop
/// adapter's host. This test drives it directly, with the same environment
/// contract the Desktop uses (`GROKPTAH_CONTROL_TOKEN`, `_PORT`,
/// `_WORKSPACES`), and points the identical published `ServiceControlPlane`
/// at the result.
///
/// The point is what is **absent**: there is no desktop-specific adapter, no
/// second DTO set, and no bespoke JSON-RPC client. A consumer embedding the
/// Desktop and a consumer talking to the headless service compile against one
/// contract and get one set of answers. What this does *not* cover is the
/// Tauri application shell around that server — only its control-plane entry
/// point.
#[tokio::test]
async fn the_versioned_battery_runs_against_the_desktop_embedded_control_server() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();

    // Exactly the Desktop's bootstrap: a default host against the ambient
    // home, then the env-driven control server.
    let host = AgentHost::create(HostConfig::default());
    host.start().expect("start agent host");

    std::env::set_var("GROKPTAH_CONTROL_TOKEN", TOKEN);
    std::env::set_var("GROKPTAH_CONTROL_PORT", "0");
    std::env::set_var("GROKPTAH_CONTROL_WORKSPACES", &workspace);

    let server = start_control_from_env(host.clone())
        .await
        .expect("the Desktop bootstrap starts a control server when a token is set");

    let harness = live_harness(&env, server.addr, host).await;
    let report = conformance::run_battery(&harness).await;

    println!(
        "\n=== desktop embedded adapter matrix ===\n{}",
        report.summary()
    );
    for check in &report.checks {
        let outcome = match &check.outcome {
            CheckOutcome::Passed => "PASS".to_string(),
            CheckOutcome::Skipped(why) => format!("SKIP  ({why})"),
            CheckOutcome::Failed(why) => format!("FAIL  ({why})"),
        };
        println!("  {outcome:<8} {}", check.name);
    }

    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| matches!(c.outcome, CheckOutcome::Failed(_)))
        .map(|c| c.name)
        .collect();
    assert!(failed.is_empty(), "desktop battery failures: {failed:?}");
    assert!(
        report.passed_count() >= 10,
        "too few desktop checks actually ran: {}",
        report.summary()
    );

    std::env::remove_var("GROKPTAH_CONTROL_TOKEN");
    std::env::remove_var("GROKPTAH_CONTROL_PORT");
    std::env::remove_var("GROKPTAH_CONTROL_WORKSPACES");
    server.stop();
}

#[tokio::test]
async fn the_live_host_states_its_own_contract_version() {
    let env = ServiceEnv::new();
    let service = start_isolated(&env, vec![env.workspace_path()], 4).await;
    let harness = live_harness(&env, service.addr, service.host()).await;

    // The embedder's guess is deliberately wrong, so a document that still
    // carries it would fail this rather than quietly asserting a fiction.
    let connected = harness.plane.connect().await.expect("connect");

    assert_eq!(connected.document.host.product.as_str(), "GrokPtah");
    assert_ne!(
        connected.document.host.host_version.as_str(),
        "unknown",
        "host version must come from the host, not the embedder default"
    );
    assert_eq!(
        connected.document.contract_version, CONTRACT_VERSION,
        "this host and this build implement the same contract"
    );
    assert!(!connected.negotiated.degraded);
    println!(
        "live host: {} {} contract {}",
        connected.document.host.product,
        connected.document.host.host_version,
        connected.document.contract_version
    );
    service.stop_and_wait().await;
}
