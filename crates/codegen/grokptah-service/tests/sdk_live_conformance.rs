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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use grokptah_agent_bridge::orchestration::RunState;
use grokptah_agent_bridge::{
    start_control_from_env, AgentHost, AgentHostHandle, HostConfig, McpControlClient,
};
use grokptah_agent_sdk::conformance::{self, CheckOutcome, Harness};
use grokptah_agent_sdk::prelude::*;
use grokptah_agent_sdk::service::{McpTransport, ServiceControlPlane, TransportFault};
use grokptah_service::{start_service, ServiceConfig};
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

/// A transport that lets a request through and then loses the response.
///
/// This is the fault that makes durable receipts worth having: the mutation
/// reached the host and **took effect**, and the caller cannot see that it did.
/// A fake cannot produce it honestly — dropping the call before it lands is a
/// different failure with a different correct answer — so it is injected here
/// around a real one.
struct PostEffectDisconnect {
    inner: LiveTransport,
    /// Shared with the test, so the plane can own the transport outright while
    /// the test still decides when the next response goes missing.
    armed: Arc<AtomicBool>,
}

impl PostEffectDisconnect {
    async fn connect(addr: std::net::SocketAddr) -> (Self, Arc<AtomicBool>) {
        let armed = Arc::new(AtomicBool::new(false));
        (
            Self {
                inner: LiveTransport::connect(addr).await,
                armed: Arc::clone(&armed),
            },
            armed,
        )
    }
}

#[async_trait]
impl McpTransport for PostEffectDisconnect {
    async fn list_tools(&self) -> Result<Vec<String>, TransportFault> {
        self.inner.list_tools().await
    }

    async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, TransportFault> {
        // The call is made first, and only then discarded: the effect is real.
        let result = self.inner.call_tool(tool, arguments).await;
        if self.armed.swap(false, Ordering::SeqCst) {
            return Err(TransportFault::Unreachable {
                detail: "connection lost after the request was sent".into(),
            });
        }
        result
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

// ── Two principals ────────────────────────────────────────────────────────

/// A second credential on the same host, same session, same workspace.
///
/// This is the check the single-owner harness had to skip, and it is the one
/// that matters most: the host stamps `client_id` from the authenticated
/// credential when a run is created, and until this branch it discarded that
/// on every read. Any credential that could reach a session could read every
/// run in it, including runs another credential created.
///
/// Two properties are asserted together, and the second is as important as the
/// first: the foreign principal is refused, **and** its refusal is
/// byte-identical to the refusal for a run that does not exist. A principal
/// check that answered "exists but not yours" differently from "no such run"
/// would close the read and open an oracle for probing other principals' run
/// ids.
#[tokio::test]
async fn a_second_principal_cannot_read_the_first_principals_run() {
    const OTHER_TOKEN: &str = "second-principal-token-with-enough-entropy";

    let env = ServiceEnv::new();
    let workspace = env.workspace_path();

    let config = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN,
        vec![workspace.clone()],
        false,
        4,
        std::time::Duration::from_secs(8),
    )
    .expect("valid service config")
    .with_runtime_home(env._home.path())
    .expect("valid runtime home");
    let config = ServiceConfig {
        client_credentials: vec![
            grokptah_agent_bridge::orchestration::AuthCredential::new("primary", TOKEN)
                .expect("primary credential"),
            grokptah_agent_bridge::orchestration::AuthCredential::new("second", OTHER_TOKEN)
                .expect("second credential"),
        ],
        ..config
    };
    let service = start_service(config).await.expect("start service");

    // Principal A creates the session and submits the run.
    let harness = live_harness(&env, service.addr, service.host()).await;
    let accepted = harness
        .plane
        .submit_task(TaskSubmission {
            request_id: harness.next_request_id(),
            session_id: harness.session.session_id.clone(),
            workspace: harness.session.workspace.clone(),
            prompt: "owned by the first principal".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("principal A submits");

    // Principal A can read its own run. Without this the test would pass even
    // if the binding refused everyone.
    let selector = RunSelector {
        session_id: harness.session.session_id.clone(),
        workspace: harness.session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };
    harness
        .plane
        .observe_run(selector.clone())
        .await
        .expect("the owning principal reads its own run");

    // Principal B: a different credential on the same host, reaching the same
    // session and the same allowlisted workspace.
    let mut other_client = McpControlClient::new(format!("http://{}", service.addr), OTHER_TOKEN);
    other_client.initialize().await.expect("initialize as B");
    let other_plane = ServiceControlPlane::read_only(LiveTransport {
        client: Mutex::new(other_client),
    })
    .with_operator_authority();

    // B learns the workspace legitimately — the host reports it to B too, so
    // this is not a forged reference.
    let b_sessions = other_plane
        .list_sessions(PageRequest::new())
        .await
        .expect("B lists sessions");
    let b_session = b_sessions
        .items
        .into_iter()
        .next()
        .expect("B sees the session; the workspace is allowlisted for B as well");

    let foreign_read = other_plane
        .observe_run(RunSelector {
            session_id: b_session.session_id.clone(),
            workspace: b_session.workspace.clone(),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect_err("a second principal must not read the first principal's run");

    let unknown_read = other_plane
        .observe_run(RunSelector {
            session_id: b_session.session_id.clone(),
            workspace: b_session.workspace.clone(),
            run_id: RunId::new("run-that-never-existed").unwrap(),
        })
        .await
        .expect_err("an unknown run is refused");

    assert_eq!(
        foreign_read.code, unknown_read.code,
        "a foreign run must be refused exactly like one that does not exist"
    );
    assert_eq!(foreign_read.code, SdkErrorCode::ForbiddenScope);
    assert_eq!(
        foreign_read.message, unknown_read.message,
        "the refusal message must not distinguish them either"
    );

    // Receipts obey the same fence.
    let foreign_receipts = other_plane
        .list_receipts(
            RunSelector {
                session_id: b_session.session_id.clone(),
                workspace: b_session.workspace.clone(),
                run_id: accepted.run_id.clone(),
            },
            PageRequest::new(),
        )
        .await
        .expect_err("a second principal must not read the first principal's receipts");
    assert_eq!(foreign_receipts.code, SdkErrorCode::ForbiddenScope);

    service.stop_and_wait().await;
}

/// Session creation is idempotent end to end, against the real host.
///
/// The SDK used to build `ptah_create_session` arguments from the workspace
/// and title only, silently dropping the `request_id` the caller supplied — so
/// a timeout could create a second session even after the host learned how to
/// deduplicate. This drives the whole path: the key goes on the wire, the host
/// records a receipt, and the exact retry replays the original session rather
/// than minting another.
#[tokio::test]
async fn creating_a_session_twice_under_one_key_yields_one_session() {
    let env = ServiceEnv::new();
    let service = start_isolated(&env, vec![env.workspace_path()], 4).await;
    let harness = live_harness(&env, service.addr, service.host()).await;

    let before = harness
        .plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list before")
        .items
        .len();

    let key = harness.next_request_id();
    let request = CreateSessionRequest {
        request_id: key.clone(),
        workspace: harness.session.workspace.clone(),
        title: None,
    };

    let first = harness
        .plane
        .create_session(request.clone())
        .await
        .expect("first create");
    let second = harness
        .plane
        .create_session(request)
        .await
        .expect("exact retry replays rather than creating a second session");

    assert_eq!(
        first.session_id, second.session_id,
        "the same key produced two different sessions"
    );

    let after = harness
        .plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list after")
        .items
        .len();
    assert_eq!(
        after,
        before + 1,
        "exactly one session should have been created across two calls"
    );

    service.stop_and_wait().await;
}

/// A `grokptah-service` running as a genuine child OS process.
///
/// `start_service` runs the host inside the test process, which is good
/// persistence evidence and *not* a restart: the allocator, the tokio runtime,
/// every `static`, and the advisory instance lock all survive it. Only a real
/// process boundary proves that what came back was read from disk, and only a
/// real kill proves it survived a process that never got to shut down.
struct ChildService {
    child: std::process::Child,
    addr: std::net::SocketAddr,
    reaped: bool,
}

impl ChildService {
    /// Spawn the service binary against `home`, and wait for it to listen.
    async fn spawn(home: &std::path::Path, workspace: &std::path::Path) -> Self {
        // Ask the OS for a free port and immediately give it back. A race with
        // another listener is possible in principle; the alternative is having
        // the child report its port, which the binary has no flag for.
        let addr = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
            probe.local_addr().expect("probe addr")
        };

        let child = std::process::Command::new(env!("CARGO_BIN_EXE_grokptah-service"))
            .arg("--listen")
            .arg(addr.to_string())
            .arg("--token")
            .arg(TOKEN)
            .arg("--workspace")
            .arg(workspace)
            .env("GROKPTAH_HOME", home)
            .env("GROKPTAH_AGENT_OFFLINE", "1")
            .spawn()
            .expect("spawn the service binary");

        let mut service = Self {
            child,
            addr,
            reaped: false,
        };
        service.wait_until_listening().await;
        println!(
            "spawned {} pid {} on {}",
            env!("CARGO_BIN_EXE_grokptah-service"),
            service.child.id(),
            service.addr
        );
        service
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Poll the socket until it accepts.
    ///
    /// A readiness poll on the real condition, not a sleep standing in for one:
    /// it yields rather than timing out a guess, it fails the moment the child
    /// exits, and it has a hard deadline so a hang is a failure rather than a
    /// hung suite.
    async fn wait_until_listening(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll child") {
                panic!("the service exited before listening: {status}");
            }
            if tokio::net::TcpStream::connect(self.addr).await.is_ok() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the service never started listening on {}",
                self.addr
            );
            tokio::task::yield_now().await;
        }
    }

    /// SIGKILL. No unwinding, no `Drop`, no flush — whatever is on disk is
    /// whatever was already committed.
    fn kill(mut self) {
        self.child.kill().expect("kill the service");
        let status = self.child.wait().expect("reap the service");
        self.reaped = true;
        assert!(
            !status.success(),
            "the process was supposed to be killed, not to exit cleanly"
        );
    }
}

impl Drop for ChildService {
    /// A panicking assertion skips the explicit `kill`, and a leaked service
    /// holds a port, a home, and the advisory instance lock — so the *next*
    /// test fails for a reason that has nothing to do with what it asserts.
    /// A failing test must fail alone.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A mutation that lands and loses its response survives an abrupt kill and a
/// **real second OS process**.
///
/// This is the strong form of the previous test. There the service was stopped
/// and reopened inside one test process, which proves durability across a host
/// instance but not across a process: statics, the allocator, the runtime and
/// the advisory lock all persisted. Here a child process takes the mutation,
/// the response is discarded before the caller sees it, the process is
/// **SIGKILLed** rather than shut down, and a second process is started against
/// the same home. Retrying the key must return the session that already exists.
///
/// Both failure directions fail the test: a lost effect (the retry mints a new
/// session, or the count does not rise) and a duplicated one (the count rises
/// twice).
#[tokio::test]
async fn a_killed_service_process_replays_its_receipt_to_a_second_process() {
    let env = ServiceEnv::new();
    let home = env._home.path().to_path_buf();
    let workspace = env.workspace_path();

    let first = ChildService::spawn(&home, &workspace).await;

    // Seed one session through the raw client so the adapter has a workspace
    // to *learn*. It is never told a path.
    let mut seed = mcp_client(first.addr).await;
    create_build_session(&mut seed, &workspace, "child-process-restart").await;

    let (faulty, armed) = PostEffectDisconnect::connect(first.addr).await;
    let plane = ServiceControlPlane::read_only(faulty).with_operator_authority();
    let workspace_ref = plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list sessions from the child process")
        .items
        .into_iter()
        .next()
        .expect("the seeded session is visible")
        .workspace;
    let before = 1;

    let key = RequestId::new("child-restart-key-000001").expect("valid key");
    armed.store(true, Ordering::SeqCst);
    let lost = plane
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: workspace_ref,
            title: Some(Label::new("created before the kill").unwrap()),
        })
        .await
        .expect_err("the response was dropped, so the caller must see a fault");
    armed.store(false, Ordering::SeqCst);
    assert!(
        matches!(
            lost.code,
            SdkErrorCode::TransportUnavailable | SdkErrorCode::UncertainOutcome
        ),
        "a lost response must not be reported as a clean failure, got {:?}",
        lost.code
    );

    // Killed, not stopped. Nothing gets to run on the way out.
    drop(plane);
    let first_pid = first.pid();
    first.kill();

    let second = ChildService::spawn(&home, &workspace).await;
    assert_ne!(
        first_pid,
        second.pid(),
        "the second service must be a different OS process"
    );
    let reconnected = ServiceControlPlane::read_only(LiveTransport::connect(second.addr).await)
        .with_operator_authority();

    let survivors = reconnected
        .list_sessions(PageRequest::new())
        .await
        .expect("the second process reports what survived")
        .items;
    assert_eq!(
        survivors.len(),
        before + 1,
        "the mutation must have survived a process that was killed mid-flight"
    );
    let known: Vec<_> = survivors
        .iter()
        .map(|session| session.session_id.clone())
        .collect();

    let recovered = reconnected
        .create_session(CreateSessionRequest {
            request_id: key,
            workspace: survivors[0].workspace.clone(),
            title: Some(Label::new("created before the kill").unwrap()),
        })
        .await
        .expect("retrying the key in a new process must reconcile, not fail");
    assert!(
        known.contains(&recovered.session_id),
        "the retry minted a new session instead of replaying the durable one"
    );
    assert_eq!(
        reconnected
            .list_sessions(PageRequest::new())
            .await
            .expect("list after reconciliation")
            .items
            .len(),
        before + 1,
        "reconciliation created a second session"
    );

    second.kill();
}

/// A mutation that lands and then loses its response is recoverable across a
/// **host restart inside one process**.
///
/// This is the case the three-valued retry disposition and the durable receipt
/// exist for, and it cannot be produced honestly by a fake: dropping a call
/// *before* it lands is a different failure with a different correct answer.
/// So the fault is injected around a real transport, after the host has
/// already acted.
///
/// **What this does and does not prove.** The service is stopped and a new one
/// opened against the same durable home, in the same test process: the
/// allocator, the tokio runtime, every `static` and the advisory instance lock
/// all survive. That is real evidence about the host lifecycle and the store,
/// and it is *not* a process restart — an earlier version of this comment said
/// it was. `a_killed_service_process_replays_its_receipt_to_a_second_process`
/// is the process-boundary proof; this one is kept because it isolates the
/// host-lifecycle half from process teardown.
#[tokio::test]
async fn a_lost_response_is_reconciled_after_a_host_restart_in_process() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();

    let config = || {
        ServiceConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            TOKEN,
            vec![workspace.clone()],
            false,
            4,
            std::time::Duration::from_secs(8),
        )
        .expect("valid service config")
        .with_runtime_home(env._home.path())
        .expect("valid runtime home")
    };

    let service = start_service(config()).await.expect("start service");

    // Learn the workspace the way a consumer does, then rebuild the plane on a
    // transport that can lose a response after the fact.
    let harness = live_harness(&env, service.addr, service.host()).await;
    let workspace_ref = harness.session.workspace.clone();
    let before = harness
        .plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list before")
        .items
        .len();

    let (faulty, armed) = PostEffectDisconnect::connect(service.addr).await;
    let plane = ServiceControlPlane::read_only(faulty).with_operator_authority();
    // The plane must learn the workspace through the host's own report; a ref
    // is never carried across adapters.
    let workspace_ref = plane
        .list_sessions(PageRequest::new())
        .await
        .expect("the faulty plane lists sessions")
        .items
        .into_iter()
        .find(|session| session.workspace == workspace_ref)
        .map(|session| session.workspace)
        .expect("the same workspace is reported to this plane");

    let key = harness.next_request_id();
    // The harness holds its own host handle, and the runtime home takes an
    // advisory instance lock. Release it before the restart or the second
    // process refuses to start — correctly.
    drop(harness);
    armed.store(true, Ordering::SeqCst);
    let lost = plane
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: workspace_ref.clone(),
            title: Some(Label::new("created behind a lost response").unwrap()),
        })
        .await
        .expect_err("the response was dropped, so the caller must see a fault");
    armed.store(false, Ordering::SeqCst);

    // The host took the key, so the create is replayable and the disposition
    // says so. What the caller must *not* be told is that nothing happened.
    assert!(
        matches!(
            lost.code,
            SdkErrorCode::TransportUnavailable | SdkErrorCode::UncertainOutcome
        ),
        "a lost response must not be reported as a clean failure, got {:?}",
        lost.code
    );

    // The effect is nevertheless durable. Prove it before restarting, so a
    // later failure cannot be blamed on the write never having happened.
    let after_effect = plane
        .list_sessions(PageRequest::new())
        .await
        .expect("list after the lost response")
        .items;
    assert_eq!(
        after_effect.len(),
        before + 1,
        "the mutation must have landed even though its response did not return"
    );
    let created: Vec<_> = after_effect
        .iter()
        .map(|session| session.session_id.clone())
        .collect();

    // A second host instance against the same durable home — same process.
    drop(plane);
    service.stop_and_wait().await;
    let service = start_service(config()).await.expect("restart service");

    let reconnected = ServiceControlPlane::read_only(LiveTransport::connect(service.addr).await)
        .with_operator_authority();
    let reconnected_workspace = reconnected
        .list_sessions(PageRequest::new())
        .await
        .expect("the reopened host reports its sessions")
        .items
        .into_iter()
        .next()
        .expect("sessions survived the restart")
        .workspace;

    let recovered = reconnected
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: reconnected_workspace,
            title: Some(Label::new("created behind a lost response").unwrap()),
        })
        .await
        .expect("retrying the key after the host reopens must reconcile, not fail");

    assert!(
        created.contains(&recovered.session_id),
        "the retry minted a new session instead of replaying the durable one"
    );
    let after_restart = reconnected
        .list_sessions(PageRequest::new())
        .await
        .expect("list after restart")
        .items
        .len();
    assert_eq!(
        after_restart,
        before + 1,
        "reconciliation created a second session"
    );

    service.stop_and_wait().await;
}

/// A page cursor must be one the host issued, checked rather than assumed.
///
/// The contract said "a cursor this host did not issue is `invalid_request`"
/// while the host checked only that the string looked like `millis:request_id`
/// — so a caller could seek to a position never handed out, and could read a
/// value it was promised was opaque. The host now authenticates cursors and
/// binds them to the scope and run they were issued for. This drives the real
/// host over HTTP with the exact shapes the old parser accepted.
#[tokio::test]
async fn a_forged_receipt_cursor_is_refused_by_the_live_host() {
    let env = ServiceEnv::new();
    let service = start_isolated(&env, vec![env.workspace_path()], 4).await;
    let harness = live_harness(&env, service.addr, service.host()).await;

    let accepted = harness
        .plane
        .submit_task(TaskSubmission {
            request_id: harness.next_request_id(),
            session_id: harness.session.session_id.clone(),
            workspace: harness.session.workspace.clone(),
            prompt: "a run to hang receipts on".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");

    let selector = RunSelector {
        session_id: harness.session.session_id.clone(),
        workspace: harness.session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };

    // The unpaged read works, so a refusal below is about the cursor and not
    // about the fence.
    harness
        .plane
        .list_receipts(selector.clone(), PageRequest::new())
        .await
        .expect("the owner lists its own receipts");

    // Every one of these was accepted by the old syntax-only parser.
    for forged in [
        "0:anything",
        "1700000000000:req-7",
        "9223372036854775807:z",
        "not-a-cursor",
        "",
    ] {
        let page = PageRequest::new().after(Cursor::from_opaque(forged.to_string()));
        match harness.plane.list_receipts(selector.clone(), page).await {
            // An empty cursor is "no cursor" by the contract, not a forgery.
            Ok(_) if forged.is_empty() => {}
            Ok(_) => panic!("the host accepted a cursor it never issued: {forged:?}"),
            Err(error) => assert_eq!(
                error.code,
                SdkErrorCode::InvalidRequest,
                "a forged cursor must be invalid_request, got {:?} for {forged:?}",
                error.code
            ),
        }
    }

    service.stop_and_wait().await;
}

/// One caller's idempotency key must not reach another caller's request.
///
/// `request_id` is chosen by the caller, so it names a request only within the
/// principal that chose it. Before the receipt namespace was scoped, the two
/// principals here shared one: the second caller's create either replayed the
/// first caller's session (a straight cross-principal read) or came back
/// `conflict`, which confirmed the key was taken and made the namespace an
/// oracle. Both are asserted against here, against the real host, and the
/// first caller's own retry must still replay.
#[tokio::test]
async fn one_principals_idempotency_key_does_not_reach_anothers() {
    const OTHER_TOKEN: &str = "idempotency-second-principal-token-entropy";

    let env = ServiceEnv::new();
    let workspace = env.workspace_path();

    let config = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN,
        vec![workspace.clone()],
        false,
        4,
        std::time::Duration::from_secs(8),
    )
    .expect("valid service config")
    .with_runtime_home(env._home.path())
    .expect("valid runtime home");
    let config = ServiceConfig {
        client_credentials: vec![
            grokptah_agent_bridge::orchestration::AuthCredential::new("primary", TOKEN)
                .expect("primary credential"),
            grokptah_agent_bridge::orchestration::AuthCredential::new("second", OTHER_TOKEN)
                .expect("second credential"),
        ],
        ..config
    };
    let service = start_service(config).await.expect("start service");

    let harness = live_harness(&env, service.addr, service.host()).await;

    let mut other_client = McpControlClient::new(format!("http://{}", service.addr), OTHER_TOKEN);
    other_client.initialize().await.expect("initialize as B");
    let other_plane = ServiceControlPlane::read_only(LiveTransport {
        client: Mutex::new(other_client),
    })
    .with_operator_authority();

    // Both principals pick the same key. Nothing stops them: it is their
    // string, not the host's.
    let key = harness.next_request_id();

    let mine = harness
        .plane
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: harness.session.workspace.clone(),
            title: Some(Label::new("first principal").unwrap()),
        })
        .await
        .expect("A creates under the shared key");

    // B learns its own reference to the same allowlisted workspace. The ref is
    // host-issued per principal, so B cannot reuse A's.
    let b_workspace = other_plane
        .list_sessions(PageRequest::new())
        .await
        .expect("B lists sessions")
        .items
        .into_iter()
        .next()
        .expect("B sees a session; the workspace is allowlisted for B as well")
        .workspace;

    let theirs = other_plane
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: b_workspace,
            title: Some(Label::new("second principal").unwrap()),
        })
        .await
        .expect("B's create must succeed, not conflict on a key it never used");

    assert_ne!(
        mine.session_id, theirs.session_id,
        "B was handed A's session; the idempotency namespace is shared"
    );

    // A's exact retry still replays A's own outcome. Scoping must not cost the
    // owner the guarantee the key exists for.
    let replay = harness
        .plane
        .create_session(CreateSessionRequest {
            request_id: key.clone(),
            workspace: harness.session.workspace.clone(),
            title: Some(Label::new("first principal").unwrap()),
        })
        .await
        .expect("A retries its own key");
    assert_eq!(
        replay.session_id, mine.session_id,
        "the owner's retry must replay the owner's session"
    );

    service.stop_and_wait().await;
}

/// Rotating a credential must not turn its history into shared reading.
///
/// This is the failure mode an earlier version of the fence had: host
/// authority was inferred from a `client_id` being absent from the *current*
/// credential set, so removing or rotating credential A reclassified every run
/// A had ever created as host-authored — readable by A's replacement.
///
/// The sequence is the real one: A creates a run, A is removed from the
/// service's configured credentials, B authenticates, and B must still be
/// refused exactly as it would be for a run that never existed.
#[tokio::test]
async fn rotating_a_credential_does_not_share_its_history() {
    const TOKEN_A: &str = "device-a-token-with-enough-entropy-here";
    const TOKEN_B: &str = "device-b-token-with-enough-entropy-here";

    let env = ServiceEnv::new();
    let workspace = env.workspace_path();

    let base = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN,
        vec![workspace.clone()],
        false,
        4,
        std::time::Duration::from_secs(8),
    )
    .expect("valid config")
    .with_runtime_home(env._home.path())
    .expect("valid runtime home");

    let credential = |id: &str, token: &str| {
        grokptah_agent_bridge::orchestration::AuthCredential::new(id, token)
            .expect("valid credential")
    };

    let service = start_service(ServiceConfig {
        client_credentials: vec![
            credential("primary", TOKEN),
            credential("device-a", TOKEN_A),
            credential("device-b", TOKEN_B),
        ],
        ..base
    })
    .await
    .expect("start service");

    // Device A creates a session and a run under its own credential.
    let mut seed = McpControlClient::new(format!("http://{}", service.addr), TOKEN_A);
    seed.initialize().await.expect("initialize as A");
    create_build_session(&mut seed, &workspace, "owned-by-a").await;

    let plane_a = ServiceControlPlane::read_only(LiveTransport {
        client: Mutex::new({
            let mut c = McpControlClient::new(format!("http://{}", service.addr), TOKEN_A);
            c.initialize().await.expect("initialize A plane");
            c
        }),
    })
    .with_operator_authority();

    let session_a = plane_a
        .list_sessions(PageRequest::new())
        .await
        .expect("A lists")
        .items
        .into_iter()
        .next()
        .expect("A sees its session");

    let accepted = plane_a
        .submit_task(TaskSubmission {
            request_id: RequestId::new("rotation-run-1").unwrap(),
            session_id: session_a.session_id.clone(),
            workspace: session_a.workspace.clone(),
            prompt: "created under device-a".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("A submits");

    // Rotate for real: stop the service and bring it back with device A
    // removed from the configured credentials, against the *same* durable
    // home so A's run is still there. This is a genuine two-process restart,
    // not an in-memory reconfiguration.
    drop(plane_a);
    drop(seed);
    service.stop_and_wait().await;

    let rotated = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN,
        vec![workspace.clone()],
        false,
        4,
        std::time::Duration::from_secs(8),
    )
    .expect("valid config")
    .with_runtime_home(env._home.path())
    .expect("valid runtime home");
    let service = start_service(ServiceConfig {
        client_credentials: vec![
            credential("primary", TOKEN),
            credential("device-b", TOKEN_B),
        ],
        ..rotated
    })
    .await
    .expect("restart without device-a");

    // Device B authenticates against the restarted service and tries to read
    // A's now-orphaned run.
    let plane_b = ServiceControlPlane::read_only(LiveTransport {
        client: Mutex::new({
            let mut c = McpControlClient::new(format!("http://{}", service.addr), TOKEN_B);
            c.initialize().await.expect("initialize B plane");
            c
        }),
    })
    .with_operator_authority();

    let session_b = plane_b
        .list_sessions(PageRequest::new())
        .await
        .expect("B lists")
        .items
        .into_iter()
        .next()
        .expect("B sees the session");

    let orphaned = plane_b
        .observe_run(RunSelector {
            session_id: session_b.session_id.clone(),
            workspace: session_b.workspace.clone(),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect_err("a rotated-away credential's history must not become shared");

    let unknown = plane_b
        .observe_run(RunSelector {
            session_id: session_b.session_id.clone(),
            workspace: session_b.workspace.clone(),
            run_id: RunId::new("run-that-never-existed").unwrap(),
        })
        .await
        .expect_err("an unknown run is refused");

    assert_eq!(
        orphaned.code, unknown.code,
        "an orphaned run must be refused exactly like one that never existed"
    );
    assert_eq!(
        orphaned.message, unknown.message,
        "the refusal message must not distinguish them either"
    );
    assert_eq!(orphaned.code, SdkErrorCode::ForbiddenScope);

    service.stop_and_wait().await;
}
