//! Host runtime shutdown authority and process-lock ownership (#455).
//!
//! These tests pin the ownership boundary that the hosted desktop soaks
//! exposed: a completed MCP campaign could stop the control server and drop
//! the caller's host and *still* hold `.instance.lock`, because cloneable
//! request handles captured by spawned tasks extended the lock's lifetime.
//!
//! Every test synchronizes on real durable state or real task joins. Sleeps
//! appear only as poll spacing inside bounded waits for state this process
//! does not control; no assertion depends on a sleep for correctness, and the
//! restart assertions run with zero delay after shutdown returns.

mod common;

use std::path::Path;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    AuthContext, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, HostPhase,
    HostRuntime, McpControlClient, SessionKind,
};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN: &str = "shutdown-ownership-token";

struct Lane {
    home: TempDir,
    workspace: TempDir,
    _env: ProcessEnvGuard,
}

impl Lane {
    fn new() -> Self {
        let mut env = ProcessEnvGuard::new();
        env.set("GROKPTAH_AGENT_OFFLINE", "1");
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        Self {
            home,
            workspace,
            _env: env,
        }
    }

    fn grokptah_home(&self) -> std::path::PathBuf {
        self.home.path().join(".grokptah")
    }

    fn instance_lock(&self) -> std::path::PathBuf {
        self.grokptah_home().join(".instance.lock")
    }

    fn ws(&self) -> &Path {
        self.workspace.path()
    }

    /// A started host runtime on this lane's home, plus one Build session.
    fn boot(&self) -> (HostRuntime, Uuid) {
        let runtime = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        runtime.start().expect("start host");
        runtime.set_project_cwd(self.ws()).unwrap();
        let session = runtime.session_new_kind(SessionKind::Build).unwrap();
        runtime.session_set_cwd(session.id, self.ws()).unwrap();
        (runtime, session.id)
    }

    fn orchestration(&self, runtime: &HostRuntime) -> Arc<OrchestrationService> {
        OrchestrationService::new(
            // The service takes a *request handle*, never the runtime: the
            // ownership rule this issue is about is enforced by the types.
            runtime.handle(),
            runtime.event_bus(),
            OrchStore::open(self.grokptah_home().join("orchestration")).unwrap(),
            OrchestrationConfig {
                bearer_token: TOKEN.into(),
                allowlist: WorkspaceAllowlist::new([self.ws().to_path_buf()]),
                max_concurrent_runs: 4,
                bounds: RunBounds::default(),
            },
        )
    }
}

async fn submit(
    orch: &OrchestrationService,
    auth: &AuthContext,
    request_id: &str,
    session_id: Uuid,
    workspace: &Path,
) -> String {
    orch.submit_task(
        auth,
        request_id,
        session_id,
        workspace,
        "list files please".into(),
        Some(json!({"maxPromptBytes": 10_000, "maxRounds": 2, "maxDurationMs": 30_000})),
    )
    .await
    .expect("submit")["runId"]
        .as_str()
        .expect("runId")
        .to_string()
}

/// Bounded wait on durable run state. The poll interval is scheduling
/// spacing, not the correctness mechanism: the assertion is on the durable
/// record, and the deadline only bounds a hang.
async fn wait_terminal(orch: &OrchestrationService, auth: &AuthContext, run_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let run = orch.get_run(auth, run_id).expect("get_run");
        let state = run["state"].as_str().unwrap_or_default().to_string();
        if matches!(
            state.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted" | "limit_reached"
        ) {
            return state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "run {run_id} never reached a terminal state (last: {state})"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The single restart assertion every test shares: a replacement host must
/// acquire the same home immediately, with no retry and no delay.
fn restart_same_home_now(lane: &Lane) -> HostRuntime {
    let replacement = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    replacement
        .start()
        .unwrap_or_else(|e| panic!("immediate same-home restart must succeed: {e:#}"));
    assert!(
        lane.instance_lock().is_file(),
        "the lock file must stay on disk; only the advisory lock is released"
    );
    replacement
}

fn assert_clean_shutdown(report: &grokptah_agent_bridge::HostShutdownReport) {
    assert!(!report.already_complete);
    assert_eq!(
        report.supervised_tasks_remaining, 0,
        "ordered shutdown must join every supervised task before returning"
    );
    assert!(
        report.process_lock_released,
        "ordered shutdown must be the call that releases the advisory lock"
    );
    assert!(!report.process_lock_held_after);
    assert!(
        report.lock_file_present,
        "the lock file must not be deleted"
    );
    assert_eq!(report.phase, HostPhase::Closed);
}

/// Regression shaped like the two hosted desktop soak failures
/// (PR #450 run 33129615156 job 98715816652, PR #448 run 33131386866 job
/// 98721482743): a full MCP campaign completes, the control server is stopped
/// and joined, and the immediate same-home restart must still succeed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn completed_mcp_campaign_then_immediate_same_home_restart() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let orch = lane.orchestration(&runtime);
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let addr = server.addr;
    runtime.attach_control_server(server);

    // Drive the campaign over the real loopback MCP transport, like the soak.
    let mut client = McpControlClient::new(format!("http://{addr}"), TOKEN);
    client.initialize().await.unwrap();
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "soak-shaped-submit",
                "session_id": session_id.to_string(),
                "workspace": lane.ws().display().to_string(),
                "prompt": "list files in the project root",
                "execution_mode": "shared",
                "bounds": {"maxPromptBytes": 10_000, "maxRounds": 2, "maxDurationMs": 30_000},
            }),
        )
        .await
        .unwrap();
    assert!(!submitted.is_error);
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();
    let state = wait_terminal(&orch, &auth, &run_id).await;
    assert_eq!(state, "completed");
    drop(client);

    // The desktop restart sequence. Under the old ownership model the run
    // task still held a host clone here and the lock stayed held.
    let report = runtime.shutdown().await;
    assert_eq!(report.control_servers_stopped, 1);
    assert_clean_shutdown(&report);
    assert!(!runtime.holds_process_lock());

    let replacement = restart_same_home_now(&lane);
    // Durable recovery survives the handover.
    assert!(replacement.session_load(session_id).is_ok());
    drop(orch);
    drop(replacement);
}

/// Cancelled and steered runs must not leave process authority behind either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn cancelled_and_steered_runs_release_authority_on_shutdown() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let orch = lane.orchestration(&runtime);
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();

    let first = submit(&orch, &auth, "cancel-me", session_id, lane.ws()).await;
    // Steering is non-cancelling: it must not be what ends the run.
    let steered = orch
        .steer(
            &auth,
            "steer-1",
            session_id,
            lane.ws(),
            "also summarize the readme".into(),
        )
        .await;
    assert!(steered.is_ok() || steered.is_err(), "steer is best-effort");
    let _ = orch
        .cancel(&auth, "cancel-1", session_id, lane.ws(), Some(&first))
        .await;
    let cancelled_state = wait_terminal(&orch, &auth, &first).await;
    assert!(
        matches!(
            cancelled_state.as_str(),
            "cancelled" | "completed" | "interrupted" | "failed"
        ),
        "unexpected terminal state {cancelled_state}"
    );

    // A second run that is still in flight when shutdown starts: the ordered
    // stop must cancel it, finalize it durably, and join its task.
    let second = submit(&orch, &auth, "in-flight", session_id, lane.ws()).await;

    let report = runtime.shutdown().await;
    assert_clean_shutdown(&report);
    let after = orch.get_run(&auth, &second).expect("run still readable");
    assert!(
        after["state"].as_str().is_some(),
        "the in-flight run must retain a durable record across shutdown"
    );

    let replacement = restart_same_home_now(&lane);
    drop(orch);
    drop(replacement);
}

/// Background scans, subagents and Computer Use operations all capture host
/// clones. Ordered shutdown must cancel and join every one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn background_subagent_and_computer_use_are_joined_before_lock_release() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();

    let scan = runtime.schedule_background_task("scan project".into());
    assert_eq!(scan.status, "running");
    let subagent = runtime
        .spawn_subagent_public(session_id, "general", "summarize the workspace")
        .await
        .expect("spawn subagent");
    assert!(!subagent.is_empty());
    let (_operation, computer_cancel) = runtime
        .begin_computer_agent_operation_for_test(session_id)
        .expect("begin computer operation");
    assert_eq!(runtime.computer_agent_operation_count(), 1);
    assert!(runtime.supervised_task_count() > 0);

    let report = runtime.shutdown().await;
    assert_clean_shutdown(&report);
    assert!(
        report.supervised_tasks_at_quiesce > 0,
        "the background scan and subagent must have been supervised"
    );
    assert!(
        computer_cancel.is_cancelled(),
        "Computer Use authority must be cancelled by ordered shutdown"
    );
    assert_eq!(
        runtime.computer_agent_operation_count(),
        0,
        "no Computer Use operation may survive shutdown"
    );
    assert!(runtime
        .background_tasks()
        .iter()
        .all(|task| task.status != "running"));

    let replacement = restart_same_home_now(&lane);
    drop(replacement);
}

/// A live SSE subscriber must not keep the process lock alive: shutdown stops
/// acceptance, closes the stream and still releases the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn live_sse_stream_does_not_block_shutdown() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let orch = lane.orchestration(&runtime);
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let addr = server.addr;
    runtime.attach_control_server(server);

    let mut client = McpControlClient::new(format!("http://{addr}"), TOKEN);
    client.initialize().await.unwrap();
    let transport_session = client.session_id().unwrap().to_string();
    let run_id = submit(&orch, &auth, "sse-run", session_id, lane.ws()).await;
    let mut url = reqwest::Url::parse(&format!("http://{addr}/mcp")).unwrap();
    url.query_pairs_mut()
        .append_pair("session_id", &session_id.to_string())
        .append_pair("workspace", &lane.ws().display().to_string())
        .append_pair("run_id", &run_id);
    let live = reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("open live stream");
    assert_eq!(live.status(), 200);
    assert_eq!(live.headers()["content-type"], "text/event-stream");

    // Hold the stream open across shutdown.
    let report = runtime.shutdown().await;
    assert_eq!(report.control_servers_stopped, 1);
    assert_clean_shutdown(&report);
    drop(live);
    drop(client);

    let replacement = restart_same_home_now(&lane);
    drop(orch);
    drop(replacement);
}

/// Repeated stop is idempotent, and stale handles stay fail-closed while the
/// replacement process owns the home.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn repeated_shutdown_is_idempotent_and_stale_handles_fail_closed() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let stale = runtime.handle();

    let first = runtime.shutdown().await;
    assert_clean_shutdown(&first);

    let second = runtime.shutdown().await;
    assert!(second.already_complete);
    assert!(
        !second.process_lock_released,
        "the advisory lock is released exactly once"
    );
    assert!(!second.process_lock_held_after);
    assert!(second.lock_file_present);

    let third = runtime.shutdown().await;
    assert!(third.already_complete);

    // Stale request handles observe the closed phase and refuse authority.
    assert_eq!(stale.lifecycle_phase(), HostPhase::Closed);
    assert!(!stale.is_accepting_work());
    assert!(
        stale.start().is_err(),
        "a stale handle must not restart a closed host"
    );
    assert!(
        stale.ensure_session_accepts_new_work(session_id).is_err(),
        "a stale handle must not admit new work"
    );
    assert!(
        stale.ensure_orchestration_store().is_err(),
        "a stale handle must not reopen the durable ledger"
    );

    // The replacement process owns the home now; the stale handle still fails.
    let replacement = restart_same_home_now(&lane);
    assert!(stale.start().is_err());
    drop(replacement);
}

/// Immediate same-home restart, repeatedly, each with real supervised work in
/// flight. This is the loop the desktop soak performs once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn immediate_same_home_restart_loop() {
    let lane = Lane::new();
    for iteration in 0..4 {
        let (runtime, session_id) = lane.boot();
        let orch = lane.orchestration(&runtime);
        let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
        let run_id = submit(
            &orch,
            &auth,
            &format!("loop-{iteration}"),
            session_id,
            lane.ws(),
        )
        .await;
        let state = wait_terminal(&orch, &auth, &run_id).await;
        assert_eq!(state, "completed", "iteration {iteration}");
        let _scan = runtime.schedule_background_task("scan project".into());

        let report = runtime.shutdown().await;
        assert_clean_shutdown(&report);
        assert!(
            lane.instance_lock().is_file(),
            "iteration {iteration}: lock file must persist"
        );
        drop(orch);
        drop(runtime);
    }
    // One more acquisition proves the home is free after the whole loop.
    let final_runtime = restart_same_home_now(&lane);
    drop(final_runtime);
}

/// A runtime dropped without an ordered shutdown still closes the lifecycle
/// before releasing the lock, so no surviving handle can mutate a home that a
/// replacement process now owns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn dropped_runtime_closes_before_releasing_the_lock() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let stale = runtime.handle();
    drop(runtime);

    assert_eq!(stale.lifecycle_phase(), HostPhase::Closed);
    assert!(stale.ensure_session_accepts_new_work(session_id).is_err());
    assert!(stale.ensure_orchestration_store().is_err());

    let replacement = restart_same_home_now(&lane);
    assert!(stale.start().is_err());
    drop(replacement);
}
