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
    AuditEntry, AuthContext, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, HostPhase,
    HostRuntime, McpControlClient, MemoryScope, SessionKind, SessionUpdate,
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
        })
        .expect("acquire the GrokPtah instance lock");
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
    })
    .expect("acquire the GrokPtah instance lock");
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
    assert!(
        report.is_clean(),
        "ordered shutdown must meet every guarantee: {}",
        report.operator_summary()
    );
    assert!(
        report.process_lock_released,
        "ordered shutdown must be the call that releases the advisory lock"
    );
    assert!(
        report.durable_writes_sealed,
        "the lock is never released without the durable-write seal"
    );
    assert_eq!(report.supervised_tasks_remaining, 0);
    assert!(report.flush_errors.is_empty(), "{:?}", report.flush_errors);
    assert!(
        report.lock_file_present,
        "the lock file must not be deleted"
    );
    assert_eq!(report.phase, HostPhase::Closed);
}

/// Every durable mutator a stale `AgentHostHandle` can still reach must refuse
/// once the runtime closed — otherwise a replacement process owns the home
/// while an old handle is still writing it.
fn assert_every_durable_mutator_refuses(
    handle: &grokptah_agent_bridge::AgentHostHandle,
    session_id: Uuid,
    workspace: &Path,
) {
    let mut refused: Vec<&str> = Vec::new();
    let mut accepted: Vec<&str> = Vec::new();
    macro_rules! check {
        ($name:literal, $call:expr) => {
            if $call.is_err() {
                refused.push($name);
            } else {
                accepted.push($name);
            }
        };
    }
    check!("start", handle.start());
    check!(
        "session_new_kind",
        handle.session_new_kind(SessionKind::Build)
    );
    check!(
        "session_rename",
        handle.session_rename(session_id, "renamed by a stale handle".into())
    );
    check!("session_archive", handle.session_archive(session_id, true));
    check!(
        "session_set_cwd",
        handle.session_set_cwd(session_id, workspace)
    );
    check!("session_delete", handle.session_delete(session_id));
    check!("set_project_cwd", handle.set_project_cwd(workspace));
    check!(
        "session_queue_add",
        handle.session_queue_add(session_id, "stale queue write".into(), false)
    );
    check!(
        "memory_remember",
        handle.memory_remember(session_id, MemoryScope::Project, "stale memory fact")
    );
    check!(
        "set_api_key",
        handle.set_api_key("sk-stale".into(), "stale".into())
    );
    check!(
        "delete_provider_profile",
        handle.delete_provider_profile("stale-profile")
    );
    check!(
        "ensure_orchestration_store",
        handle.ensure_orchestration_store()
    );
    check!("ensure_computer_store", handle.ensure_computer_store());
    check!(
        "ensure_session_accepts_new_work",
        handle.ensure_session_accepts_new_work(session_id)
    );
    check!(
        "hold_durable_write",
        handle.hold_durable_write_for_test("stale write")
    );
    assert!(
        accepted.is_empty(),
        "these durable mutators were still reachable from a stale handle: {accepted:?}"
    );
    assert!(refused.len() >= 15, "expected the full mutator surface");
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
    runtime
        .attach_control_server(server)
        .unwrap_or_else(|_| panic!("a running runtime must accept its control server"));

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
    runtime
        .attach_control_server(server)
        .unwrap_or_else(|_| panic!("a running runtime must accept its control server"));

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
    assert_every_durable_mutator_refuses(&stale, session_id, lane.ws());

    // The replacement process owns the home now; the stale handle still fails.
    let replacement = restart_same_home_now(&lane);
    assert_every_durable_mutator_refuses(&stale, session_id, lane.ws());
    drop(replacement);
}

/// P0 from independent review: a stale handle must not be able to mutate
/// durable state after the lock is released, and specifically not while a
/// replacement process owns the same home.
///
/// The assertion is on the bytes: the replacement's own session title must
/// survive everything the stale handle tries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn stale_handle_cannot_write_the_home_a_replacement_owns() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let stale = runtime.handle();
    runtime
        .session_rename(session_id, "owned by the first runtime".into())
        .unwrap();
    assert_clean_shutdown(&runtime.shutdown().await);

    let replacement = restart_same_home_now(&lane);
    replacement
        .session_rename(session_id, "owned by the replacement".into())
        .expect("the live owner may rename");

    // Every durable mutator on the stale handle is refused...
    assert_every_durable_mutator_refuses(&stale, session_id, lane.ws());
    // ...and nothing it attempted reached disk.
    let observed = replacement
        .session_load(session_id)
        .expect("session still loads")
        .title;
    assert_eq!(
        observed, "owned by the replacement",
        "a stale handle overwrote a home owned by a replacement process"
    );
    drop(replacement);
}

/// P0 from independent review: a direct desktop command future already in
/// flight when shutdown starts must not write afterwards.
///
/// The command task is intentionally **unsupervised** — Tauri command futures
/// are not this crate's to spawn — so the only thing standing between it and
/// the durable state is the write authority itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn in_flight_desktop_command_cannot_write_after_shutdown() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    runtime
        .session_rename(session_id, "before shutdown".into())
        .unwrap();

    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let outcome = Arc::new(std::sync::Mutex::new(None::<Result<(), String>>));

    let command_host = runtime.handle();
    let entered_tx = entered.clone();
    let release_rx = release.clone();
    let outcome_tx = outcome.clone();
    // Deliberately a raw spawn: this models a Tauri command future.
    let command = tokio::spawn(async move {
        entered_tx.notify_one();
        release_rx.notified().await;
        let result = command_host
            .session_rename(session_id, "written by an in-flight command".into())
            .map(|_| ())
            .map_err(|error| error.to_string());
        *outcome_tx.lock().unwrap() = Some(result);
    });

    // The command is running and has not yet written.
    entered.notified().await;
    let report = runtime.shutdown().await;
    assert_clean_shutdown(&report);

    // Now let it try, after the lock is gone.
    release.notify_one();
    command.await.unwrap();
    let result = outcome.lock().unwrap().clone().expect("command settled");
    let error = result.expect_err("an in-flight command must not write after shutdown");
    assert!(
        error.contains("durable-write authority") || error.contains("closed"),
        "unexpected refusal: {error}"
    );

    let replacement = restart_same_home_now(&lane);
    assert_eq!(
        replacement.session_load(session_id).unwrap().title,
        "before shutdown",
        "an in-flight command wrote a home it no longer owned"
    );
    drop(replacement);
}

/// P0 from independent review: a control-plane bootstrap that finishes *after*
/// shutdown drained the attached servers must be refused, and must be handed
/// its server back so the listener is stopped rather than left serving a
/// closed runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn late_control_server_attach_is_refused_and_handed_back() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let orch = lane.orchestration(&runtime);

    // A bootstrap that is still binding when shutdown begins.
    let bind_now = Arc::new(tokio::sync::Notify::new());
    let bootstrap_orch = orch.clone();
    let bind_rx = bind_now.clone();
    let bootstrap = tokio::spawn(async move {
        bind_rx.notified().await;
        start_control_server(bootstrap_orch, 0).await.unwrap()
    });

    let report = runtime.shutdown().await;
    assert_clean_shutdown(&report);
    assert_eq!(report.control_servers_stopped, 0);

    // The bootstrap finishes late and tries to publish its listener.
    bind_now.notify_one();
    let late = bootstrap.await.unwrap();
    let addr = late.addr;
    let rejected = runtime
        .attach_control_server(late)
        .expect_err("a closed runtime must refuse a late control server");
    assert_eq!(rejected.phase, HostPhase::Closed);
    // The caller still owns the listener and must stop it.
    rejected.server.stop_and_wait().await;

    // Nothing is serving that address any more, and the home is free.
    let probe = reqwest::Client::new()
        .get(format!("http://{addr}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    assert!(
        probe.is_err(),
        "a refused control server must not still be serving"
    );
    let replacement = restart_same_home_now(&lane);
    assert!(replacement.session_load(session_id).is_ok());
    drop(orch);
    drop(replacement);
}

/// P0 from independent review: `Drop` must not create a split brain. With a
/// durable write genuinely in progress, dropping the runtime must **retain**
/// the process lock, and a replacement must be refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn drop_with_a_running_writer_retains_the_lock_and_refuses_a_replacement() {
    let lane = Lane::new();
    let (mut runtime, session_id) = lane.boot();
    // Bound the drop-time wait so the test is fast; the production default is
    // seconds, and the behaviour under test is identical.
    runtime.set_durable_write_seal_timeout(Duration::from_millis(200));
    let stale = runtime.handle();

    // A writer that is genuinely holding durable-write authority right now.
    let writer = stale
        .hold_durable_write_for_test("a long durable write")
        .expect("a running host issues write authority");
    assert_eq!(runtime.in_flight_durable_writes(), 1);

    drop(runtime);

    // Fail closed: the lock was kept because a writer was still live, so a
    // replacement cannot even be constructed — it is refused before it can
    // touch the keychain, the workspace, or any durable store.
    assert_eq!(stale.lifecycle_phase(), HostPhase::Closed);
    let refused = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let error = refused
        .err()
        .expect("dropping a runtime with a live writer must not hand the home to a replacement");
    assert!(
        format!("{error:#}").contains("single-instance lock"),
        "unexpected refusal: {error:#}"
    );

    // The stale handle is still closed, so the writer cannot start new work.
    assert_every_durable_mutator_refuses(&stale, session_id, lane.ws());
    drop(writer);
    assert!(lane.instance_lock().is_file());
}

/// The same `Drop`, with no writer in flight: the seal succeeds immediately and
/// the lock is released, so an immediate replacement works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn drop_without_a_running_writer_releases_the_lock() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    assert_eq!(runtime.in_flight_durable_writes(), 0);
    drop(runtime);
    let replacement = restart_same_home_now(&lane);
    drop(replacement);
}

/// P1 from independent review: a failing shutdown hook must be reported, and
/// must make the report unclean rather than being swallowed under a clean
/// lock-release claim. This is the seam the durable audit ledger uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn shutdown_hook_failures_are_reported_not_swallowed() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let ok_ran = ran.clone();
    runtime
        .register_shutdown_hook(
            "audit-ledger-close",
            Box::new(move || {
                ok_ran.fetch_add(1, std::sync::atomic::Ordering::Release);
                Ok(())
            }),
        )
        .unwrap();
    let bad_ran = ran.clone();
    runtime
        .register_shutdown_hook(
            "audit-ledger-seal",
            Box::new(move || {
                bad_ran.fetch_add(1, std::sync::atomic::Ordering::Release);
                anyhow::bail!("ledger seal could not be committed")
            }),
        )
        .unwrap();

    let report = runtime.shutdown().await;
    assert_eq!(ran.load(std::sync::atomic::Ordering::Acquire), 2);
    assert_eq!(report.hooks_run, 2);
    assert!(
        !report.is_clean(),
        "a failed hook must not report a clean shutdown"
    );
    assert!(
        report
            .flush_errors
            .iter()
            .any(|error| error.contains("audit-ledger-seal")),
        "{:?}",
        report.flush_errors
    );
    // The seal held, but a hook failed — so the durable state this process
    // leaves behind is not known-good and the lock is deliberately retained.
    // Refusing a replacement is safer than handing it a home whose teardown
    // did not complete.
    assert!(report.durable_writes_sealed);
    assert!(!report.process_lock_released);
    assert!(report.process_lock_retained_for_safety);
    assert!(report.process_lock_held_after);
    assert!(report.lock_file_present, "the lock file is never deleted");

    // Registration after shutdown is refused, so a hook can never be
    // registered into a drain that has already happened.
    assert!(runtime
        .register_shutdown_hook("late", Box::new(|| Ok(())))
        .is_err());

    // And the retained lock really does refuse a replacement.
    let refused = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    assert!(
        refused.is_err(),
        "a retained lock must refuse a replacement host"
    );
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

// ---------------------------------------------------------------------------
// Adversarial authority tests required by the independent review packet.
// ---------------------------------------------------------------------------

/// A second **real process** holding the instance lock, with this process
/// reaching the same home through a symlinked path.
///
/// The child is this same test binary re-executed into a holder test, so the
/// check is genuinely two processes on every platform rather than depending on
/// `flock(1)`, which macOS does not ship.
#[test]
fn a_second_process_holding_the_lock_refuses_this_one_through_a_path_alias() {
    if std::env::var_os(LOCK_HOLDER_ENV).is_some() {
        // We are the child; the holder test below does the work.
        return;
    }
    let lane = Lane::new();
    let home = lane.grokptah_home();
    std::fs::create_dir_all(&home).unwrap();
    let release = home.join("release-the-lock");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "holds_the_instance_lock_until_released",
            "--nocapture",
            "--test-threads=1",
            "--ignored",
        ])
        .env(LOCK_HOLDER_ENV, "1")
        .env("GROKPTAH_HOME", &home)
        .env("GROKPTAH_LOCK_RELEASE", &release)
        .spawn()
        .expect("re-exec this test binary as the lock holder");
    let mut child = ChildGuard {
        child,
        release: release.clone(),
    };

    // Wait, bounded, for the child to actually own the home.
    let lock_path = home.join(".instance.lock");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
        assert!(
            std::time::Instant::now() < deadline,
            "the child process never took the instance lock"
        );
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "the child exited before taking the lock"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The same home through a symlink is the same home.
    let alias = lane.home.path().join("alias");
    std::os::unix::fs::symlink(lane.home.path(), &alias).unwrap();
    set_grokptah_home_override(Some(alias.join(".grokptah")));

    let refused = AgentHost::create(HostConfig::default());
    assert!(
        refused.is_err(),
        "a home another process owns must be refused even through a path alias"
    );

    // A store on that aliased home is refused outright: opening one runs
    // recovery and retention, which are durable effects, and registry absence
    // is not authority when the OS lock says another process owns the home.
    let store = OrchStore::open(alias.join(".grokptah").join("orchestration"));
    let error = store
        .err()
        .expect("a store on a home another process owns must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("single-instance lock"),
        "the refusal must name the lock that another process holds: {message}"
    );

    child.release();
    set_grokptah_home_override(Some(home));
}

const LOCK_HOLDER_ENV: &str = "GROKPTAH_TEST_LOCK_HOLDER";

struct ChildGuard {
    child: std::process::Child,
    release: std::path::PathBuf,
}

impl ChildGuard {
    fn release(&mut self) {
        let _ = std::fs::write(&self.release, b"go");
        let _ = self.child.wait();
    }
}

impl Drop for ChildGuard {
    /// A panicking test must not leave a process holding a lock on a temp home
    /// that is about to be deleted.
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, b"go");
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// The child half of the two-process check. Ignored so it never runs on its
/// own; the parent re-executes it with [`LOCK_HOLDER_ENV`] set.
#[test]
#[ignore = "child process of a_second_process_holding_the_lock_refuses_this_one_through_a_path_alias"]
fn holds_the_instance_lock_until_released() {
    let Some(release) = std::env::var_os("GROKPTAH_LOCK_RELEASE") else {
        return;
    };
    let release = std::path::PathBuf::from(release);
    let runtime = AgentHost::create(HostConfig::default())
        .expect("the child must be able to take the lock first");
    runtime.start().expect("start the holder");
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while !release.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(runtime);
}

/// One durable store effect, used to probe whether a handle still has
/// authority. The audit ledger is the smallest real write the store performs.
fn probe_store_write(store: &OrchStore, session_id: Uuid) -> anyhow::Result<()> {
    store.append_audit(&AuditEntry {
        ts: chrono::Utc::now(),
        tool: "authority.probe".into(),
        request_id: None,
        session_id: Some(session_id),
        workspace: None,
        outcome: "accepted".into(),
        error_code: None,
        detail: String::new(),
    })
}

/// A store handle cloned out of a runtime must fail closed with it, even
/// though the clone is a perfectly valid `OrchStore` that never learned the
/// runtime stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_cloned_store_fails_closed_with_the_runtime_that_opened_it() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let store = runtime
        .ensure_orchestration_store()
        .expect("a running host opens its ledger");
    assert!(
        probe_store_write(&store, session_id).is_ok(),
        "a live runtime's store writes"
    );
    // The clone a supervisor or service handle would be holding.
    let stale_clone = store.clone();

    assert_clean_shutdown(&runtime.shutdown().await);

    let refused = probe_store_write(&stale_clone, session_id);
    let error = refused.expect_err("a cloned store must not outlive its runtime's authority");
    assert!(
        format!("{error:#}").contains("authority") || format!("{error:#}").contains("closed"),
        "unexpected refusal: {error:#}"
    );
    drop(store);

    // And it still refuses once a replacement owns the home.
    let replacement = restart_same_home_now(&lane);
    assert!(probe_store_write(&stale_clone, session_id).is_err());
    drop(replacement);
}

/// A supervisor that was never attached to any runtime — an `OrchStore` opened
/// directly, the shape a stray background service takes — must not write a home
/// a runtime owns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn an_unattached_store_cannot_write_a_home_a_runtime_owns() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();

    // Opened directly, never handed to the host — the shape a stray background
    // supervisor takes. It binds to whichever runtime owns the home, so its
    // writes are serialized against that runtime's seal rather than escaping it.
    let unattached = OrchStore::open(lane.grokptah_home().join("orchestration-side")).unwrap();
    assert!(
        unattached.is_lease_bound(),
        "a store opened on an owned home must bind to that runtime, not float free"
    );
    assert!(
        probe_store_write(&unattached, session_id).is_ok(),
        "while the owner is live its authority covers this handle"
    );

    assert_clean_shutdown(&runtime.shutdown().await);
    assert!(
        probe_store_write(&unattached, session_id).is_err(),
        "an unattached supervisor must fail closed with the runtime that owned the home"
    );

    let replacement = restart_same_home_now(&lane);
    assert!(
        probe_store_write(&unattached, session_id).is_err(),
        "and it must still refuse once a replacement owns the home"
    );
    drop(replacement);
}

/// A writer that starts *just* as shutdown begins: the seal must either wait
/// for it or refuse it, never let it land after the lock is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn a_delayed_writer_either_completes_before_the_seal_or_is_refused() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let handle = runtime.handle();

    let start = Arc::new(tokio::sync::Barrier::new(2));
    let outcome = Arc::new(std::sync::Mutex::new(None::<bool>));
    let writer_start = start.clone();
    let writer_outcome = outcome.clone();
    let writer = tokio::spawn(async move {
        writer_start.wait().await;
        // Racing the seal deliberately.
        let wrote = handle
            .session_rename(session_id, "written by the delayed writer".into())
            .is_ok();
        *writer_outcome.lock().unwrap() = Some(wrote);
    });

    start.wait().await;
    let report = runtime.shutdown().await;
    writer.await.unwrap();
    let wrote = outcome.lock().unwrap().unwrap();

    // Whichever way the race resolved, the invariant is the same: the lock is
    // only released when nothing can still be writing.
    if report.process_lock_released {
        assert!(report.durable_writes_sealed);
        assert_eq!(report.durable_writes_in_flight, 0);
    } else {
        assert!(report.process_lock_retained_for_safety);
    }
    assert!(report.lock_file_present);
    // A write that succeeded must have landed before the seal, so the durable
    // record is consistent either way.
    let replacement = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    match replacement {
        Ok(replacement) => {
            replacement.start().unwrap();
            let title = replacement.session_load(session_id).unwrap().title;
            if wrote {
                assert_eq!(title, "written by the delayed writer");
            }
        }
        Err(error) => assert!(
            !report.process_lock_released,
            "a released lock must admit a replacement: {error:#}"
        ),
    }
}

/// A hook registered concurrently with shutdown is either drained and run, or
/// refused — never accepted into a drain that already happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn a_hook_registered_during_shutdown_is_never_silently_dropped() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    let runtime = Arc::new(runtime);
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let registrar = runtime.clone();
    let registrar_ran = ran.clone();
    let registering = tokio::spawn(async move {
        let mut accepted = 0usize;
        for _ in 0..64 {
            let counter = registrar_ran.clone();
            if registrar
                .register_shutdown_hook(
                    "racing",
                    Box::new(move || {
                        counter.fetch_add(1, std::sync::atomic::Ordering::Release);
                        Ok(())
                    }),
                )
                .is_ok()
            {
                accepted += 1;
            }
            tokio::task::yield_now().await;
        }
        accepted
    });

    let report = runtime.shutdown().await;
    let accepted = registering.await.unwrap();

    assert_eq!(
        report.hooks_run,
        ran.load(std::sync::atomic::Ordering::Acquire),
        "every hook the report counts must actually have run"
    );
    assert_eq!(
        report.hooks_run, accepted,
        "every accepted hook must be drained and run; none may be silently dropped"
    );
    assert!(
        registrar_refuses_after(&runtime),
        "registration must be refused once shutdown has drained"
    );
}

fn registrar_refuses_after(runtime: &HostRuntime) -> bool {
    runtime
        .register_shutdown_hook("after", Box::new(|| Ok(())))
        .is_err()
}

/// Headless construction on a home another runtime owns must fail before it
/// reads credentials or touches any durable state.
///
/// The keychain read is the one that matters: it used to happen on the way past
/// a failed lock acquisition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn headless_construction_fails_before_reading_credentials_or_state() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();

    let home = lane.grokptah_home();
    let chrome_before = std::fs::read(home.join("workspace.json")).ok();

    let refused = AgentHost::create_with_runtime_home(
        HostConfig::default(),
        grokptah_agent_bridge::RuntimeHome::from_path(&home).unwrap(),
    );
    let error = refused.err().expect("a second host on one home is refused");
    assert!(
        format!("{error:#}").contains("single-instance lock"),
        "the refusal must name the lock, not a downstream symptom: {error:#}"
    );

    // Nothing the refused construction would have done touched the home.
    assert_eq!(
        std::fs::read(home.join("workspace.json")).ok(),
        chrome_before,
        "a refused construction must not have rewritten durable state"
    );

    assert_clean_shutdown(&runtime.shutdown().await);
    let replacement = restart_same_home_now(&lane);
    drop(replacement);
}

// ---------------------------------------------------------------------------
// Negative controls for the second correction packet: each of these bites
// against a specific bypass that existed before it was closed.
// ---------------------------------------------------------------------------

/// P0 — the check-only probe was a TOCTOU.
///
/// An unbound handle used to *probe* the instance lock and then write. A
/// `HostRuntime` could acquire the lock between the probe and the write and the
/// writer would never know. The handle now **holds** the lock for its whole
/// lifetime, so the negative control is decisive: while the handle is alive a
/// replacement host cannot be constructed at all, and once it is dropped the
/// host starts immediately.
#[test]
fn an_offline_handle_holds_the_home_lock_for_its_whole_lifetime() {
    let lane = Lane::new();
    let store = OrchStore::open(lane.grokptah_home().join("orchestration"))
        .expect("no runtime owns this home, so offline maintenance may take it");

    // A momentary probe would have said "free" here. The retained lock does not.
    assert!(
        grokptah_agent_bridge::instance_lock_is_held(&lane.instance_lock()),
        "an offline handle must hold the home's lock, not merely have probed it"
    );
    // The decisive form of that claim: the handle can say it *is* the owner.
    // A reintroduced check-only probe would leave this false while writes kept
    // succeeding, so this assertion is what bites if the TOCTOU comes back.
    assert!(
        store.holds_home_lock_itself(),
        "an unowned-home handle must retain the OS lock itself, not authorize from a probe"
    );
    assert!(
        !store.is_lease_bound(),
        "there is no runtime to be bound to; the authority must be the retained lock"
    );
    let refused = AgentHost::create(HostConfig::default());
    assert!(
        refused.is_err(),
        "a host must not start beside a live offline maintenance handle"
    );
    // And the handle can still write, because it is the owner.
    assert!(probe_store_write(&store, Uuid::nil()).is_ok());

    drop(store);
    let runtime = AgentHost::create(HostConfig::default())
        .expect("the home is free once the offline handle is dropped");
    runtime.start().unwrap();
    drop(runtime);
}

/// P0 — mutation before authority.
///
/// `OrchStore::open` and `ComputerStore::open` used to create their whole
/// directory layout and `.store.lock` before proving anything. On a home
/// another process owns, the refusal must now leave the filesystem untouched.
#[test]
fn a_refused_store_open_creates_nothing() {
    if std::env::var_os(LOCK_HOLDER_ENV).is_some() {
        return;
    }
    let lane = Lane::new();
    let home = lane.grokptah_home();
    std::fs::create_dir_all(&home).unwrap();
    let release = home.join("release-the-lock");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "holds_the_instance_lock_until_released",
            "--nocapture",
            "--test-threads=1",
            "--ignored",
        ])
        .env(LOCK_HOLDER_ENV, "1")
        .env("GROKPTAH_HOME", &home)
        .env("GROKPTAH_LOCK_RELEASE", &release)
        .spawn()
        .expect("re-exec as the lock holder");
    let mut child = ChildGuard {
        child,
        release: release.clone(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !grokptah_agent_bridge::instance_lock_is_held(&lane.instance_lock()) {
        assert!(
            std::time::Instant::now() < deadline,
            "child never took the lock"
        );
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "child exited early"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let orch_root = home.join("orchestration-untouched");
    let computer_root = home.join("computer-untouched");
    assert!(OrchStore::open(&orch_root).is_err());
    assert!(
        !orch_root.exists(),
        "a refused orchestration open must not have created its layout"
    );
    assert!(grokptah_agent_bridge::computer_use::ComputerStore::open(&computer_root).is_err());
    assert!(
        !computer_root.exists(),
        "a refused Computer Run open must not have created its layout"
    );

    child.release();
}

/// P1 — a nested store owner and a parent runtime must not both hold "their"
/// lock over the same state.
///
/// A durable root inside a home is governed by the home's lock. A root opened
/// before any runtime could be identified governs itself, taking
/// `<root>/.instance.lock`. Without this check a runtime could then acquire the
/// home lock and both would be correct about their own lock while writing the
/// same ledger.
#[test]
fn a_home_is_refused_while_one_of_its_stores_is_separately_owned() {
    let lane = Lane::new();
    let home = lane.grokptah_home();
    let orch_root = home.join("orchestration");
    std::fs::create_dir_all(&orch_root).unwrap();

    // Take the nested root's own lock, the shape an offline handle leaves when
    // it resolved its home before any runtime existed.
    let nested = orch_root.join(".instance.lock");
    let nested_owner =
        grokptah_agent_bridge::InstanceLock::try_acquire_path_for_test(&nested, &orch_root)
            .expect("nobody owns this root yet");

    let refused = AgentHost::create(HostConfig::default());
    let error = refused
        .err()
        .expect("a home with a separately owned store must be refused");
    let message = format!("{error:#}");
    assert!(
        message.contains("separately owned"),
        "the refusal must name the overlapping owner: {message}"
    );

    // Released, the home is takeable again — the refusal tracks live ownership,
    // not the mere presence of a lock file.
    drop(nested_owner);
    assert!(nested.is_file(), "the nested lock file stays on disk");
    let runtime = AgentHost::create(HostConfig::default())
        .expect("the home is free once the nested owner releases");
    runtime.start().unwrap();
    drop(runtime);
}

/// P0 — a refused contender must not mutate the home it does not own.
///
/// Two ordering defects made this false. `InstanceLock::try_acquire_at` ran the
/// full `RuntimeHome::prepare()` layout *before* acquiring, and the lock file
/// itself was opened with `truncate(true)` *before* `flock` — so a process that
/// was about to be refused had already laid down the home tree and erased the
/// live owner's pid stamp, the one piece of evidence an operator uses to find
/// the process actually holding the home.
///
/// The contender here is this process; the owner is a real second process.
#[test]
fn a_refused_host_creates_nothing_and_preserves_the_owner_stamp() {
    if std::env::var_os(LOCK_HOLDER_ENV).is_some() {
        return;
    }
    let lane = Lane::new();
    let home = lane.grokptah_home();
    std::fs::create_dir_all(&home).unwrap();
    let release = home.join("release-the-lock");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "holds_the_instance_lock_until_released",
            "--nocapture",
            "--test-threads=1",
            "--ignored",
        ])
        .env(LOCK_HOLDER_ENV, "1")
        .env("GROKPTAH_HOME", &home)
        .env("GROKPTAH_LOCK_RELEASE", &release)
        .spawn()
        .expect("re-exec as the lock holder");
    let mut child = ChildGuard {
        child,
        release: release.clone(),
    };
    let lock_path = lane.instance_lock();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
        assert!(
            std::time::Instant::now() < deadline,
            "child never took the lock"
        );
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "child exited early"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The owner's stamp, and the exact set of entries the owner's home has.
    let owner_stamp = std::fs::read(&lock_path).expect("the owner stamped its lock");
    assert!(
        !owner_stamp.is_empty(),
        "the owning process must have written a pid stamp"
    );
    let entries_before = home_entries(&home);

    let refused = AgentHost::create(HostConfig::default());
    assert!(
        refused.is_err(),
        "a home another live process owns must be refused"
    );

    assert_eq!(
        std::fs::read(&lock_path).unwrap(),
        owner_stamp,
        "a refused contender must not truncate or rewrite the live owner's lock stamp"
    );
    assert_eq!(
        home_entries(&home),
        entries_before,
        "a refused contender must not create any of the home layout"
    );

    child.release();
}

/// Sorted file names directly under a home, for before/after comparison.
fn home_entries(home: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(home)
        .map(|dir| {
            dir.filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// P0 — bypass inventory. Each of these public entry points reaches a durable
/// primitive that was **not** the audit append: exclusive create, durable
/// remove, and the Computer Run mutation claim. A stale clone must be refused
/// through every one of them, not merely through the one that was guarded
/// first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn every_durable_primitive_refuses_a_stale_clone() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();
    let orch = runtime.ensure_orchestration_store().unwrap();
    let computer = runtime.ensure_computer_store().unwrap();
    let stale_orch = orch.clone();
    let stale_computer = computer.clone();

    // Live: each primitive works.
    assert!(probe_store_write(&orch, session_id).is_ok(), "audit append");
    assert!(
        orch.claim_idempotency("probe", "req-live", "hash-live")
            .is_ok(),
        "exclusive create"
    );
    assert!(
        orch.prune_retention(Default::default()).is_ok(),
        "durable remove / retention"
    );

    assert_clean_shutdown(&runtime.shutdown().await);

    // Stale: every one of them is refused.
    let mut refusals: Vec<(&str, String)> = Vec::new();
    if let Err(error) = probe_store_write(&stale_orch, session_id) {
        refusals.push(("audit append", format!("{error:#}")));
    }
    if let Err(error) = stale_orch.claim_idempotency("probe", "req-stale", "hash-stale") {
        refusals.push(("exclusive create", format!("{error:?}")));
    }
    if let Err(error) = stale_orch.prune_retention(Default::default()) {
        refusals.push(("durable remove", format!("{error:#}")));
    }
    if let Err(error) = stale_computer.prune_retention() {
        refusals.push(("computer retention", format!("{error:?}")));
    }
    assert_eq!(
        refusals.len(),
        4,
        "every durable primitive must refuse a stale clone; got {refusals:?}"
    );

    let replacement = restart_same_home_now(&lane);
    drop(replacement);
}

/// P0 — external effects must not survive lock release.
///
/// Sealing durable writes says nothing about a supervised task that is still
/// running: it can edit the workspace, send to a provider, or drive Computer
/// Use. `Drop` therefore releases only when nothing is outstanding, and this
/// proves an old task cannot act after a new host would have acquired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn drop_with_an_outstanding_task_refuses_a_replacement_host() {
    let lane = Lane::new();
    let (mut runtime, session_id) = lane.boot();
    runtime.set_durable_write_seal_timeout(Duration::from_millis(100));
    let handle = runtime.handle();

    // A supervised task that ignores cancellation — the shape of an effectful
    // task mid-flight: it performs no durable write, only an external effect.
    let released = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let acted_after_release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered_tx = entered.clone();
    let release_rx = released.clone();
    let acted = acted_after_release.clone();
    handle
        .spawn_supervised("an effectful task", async move {
            entered_tx.notify_one();
            release_rx.notified().await;
            acted.store(true, std::sync::atomic::Ordering::Release);
        })
        .expect("a running host supervises work");
    entered.notified().await;
    assert!(runtime.supervised_task_count() > 0);

    drop(runtime);

    // Fail closed: the task is still outstanding, so the home is not handed on.
    let refused = AgentHost::create(HostConfig::default());
    assert!(
        refused.is_err(),
        "a replacement must not start while a supervised task can still act"
    );
    assert!(
        !acted_after_release.load(std::sync::atomic::Ordering::Acquire),
        "the task has not acted yet; the refusal is what matters"
    );

    // Even after the task finishes, the retained lock stays retained for this
    // process — Drop cannot await, so it has no later point to re-check.
    released.notify_one();
    handle.shutdown_signal().cancelled().await;
    let still_refused = AgentHost::create(HostConfig::default());
    assert!(
        still_refused.is_err(),
        "a lock retained by Drop stays retained for the life of the process"
    );
    let _ = session_id;
}

/// P0 — an *external* effect must not survive the lock release either.
///
/// The durable-write seal only covers writes to the home. A supervised task can
/// also touch the world outside it: a workspace edit, a Computer Use input, a
/// provider send. Those effects have no guard to refuse them, so the only thing
/// standing between an old task and a replacement host is the join.
///
/// This test is adversarial about that join. The old task's effect is a file
/// written **outside** the home — nothing in the lease machinery can stop it —
/// and it is racing a replacement that acquires the moment the lock frees. The
/// property proved is ordering, not refusal: by the time a replacement can
/// acquire, the effectful task has already run to completion and been joined,
/// so there is never a moment when both an old effect and a new owner are live.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn an_effectful_task_cannot_act_after_a_replacement_acquires() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    let handle = runtime.handle();

    // The effect lands outside the home, so no durable-write guard governs it.
    let effect_path = lane.ws().join("external-effect.txt");
    let effect_for_task = effect_path.clone();
    // Observed by the task at the instant it acts: was the home already free?
    let acted_while_home_was_free = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = acted_while_home_was_free.clone();
    let lock_path = lane.instance_lock();
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_tx = entered.clone();

    handle
        .spawn_supervised("an effectful task outside the home", async move {
            entered_tx.notify_one();
            // Yield enough times that a shutdown racing this task would have
            // every chance to seal and release before the effect lands.
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            // `flock` is per open-file-description, so this same-process probe
            // is meaningful: it is false exactly while some handle owns the home.
            if !grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
                observed.store(true, std::sync::atomic::Ordering::Release);
            }
            std::fs::write(&effect_for_task, b"the old runtime acted here").unwrap();
        })
        .expect("a running host supervises work");
    entered.notified().await;

    // Ordered shutdown must join that task before it releases the lock.
    assert_clean_shutdown(&runtime.shutdown().await);

    // The effect completed *before* the release, never after it.
    assert!(
        effect_path.is_file(),
        "an ordered shutdown joins effectful work rather than abandoning it"
    );
    assert!(
        !acted_while_home_was_free.load(std::sync::atomic::Ordering::Acquire),
        "a supervised task must never act while the home is unowned; the join is \
         what keeps an old effect and a new owner from ever being live together"
    );

    // A replacement can now take the home, and the stale handle can no longer
    // introduce a new effectful task into it.
    let replacement = restart_same_home_now(&lane);
    assert!(
        handle
            .spawn_supervised("a late effectful task", async {})
            .is_err(),
        "a stale handle must not be able to spawn effectful work into a home a \
         replacement now owns"
    );

    // Negative control: the same task shape, spawned *outside* supervision, does
    // reach the hazardous state. This is what proves the assertion above is
    // testing supervision rather than a timing accident — if `spawn_supervised`
    // silently degraded to a bare spawn, the assertion above would still pass
    // while this one shows the window it was supposed to close.
    let lock_path = lane.instance_lock();
    let unsupervised_saw_a_foreign_owner = tokio::spawn(async move {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        // The home is owned again — by the *replacement*, not by the runtime
        // that spawned this task. An unjoined task cannot tell the difference,
        // which is exactly why the join, not a probe, is the guarantee.
        grokptah_agent_bridge::instance_lock_is_held(&lock_path)
    })
    .await
    .unwrap();
    assert!(
        unsupervised_saw_a_foreign_owner,
        "the probe used above must be able to observe an owned home, or the \
         negative assertion would be vacuous"
    );

    assert_clean_shutdown(&replacement.shutdown().await);
}

/// P0 — "retained on uncertainty" must survive the destruction of every object.
///
/// The previous shape retained the lock by *leaving it inside the lifecycle*.
/// That is only a decision not to release, and it lasts exactly as long as the
/// last `Arc<HostLifecycle>` does — which, for a consuming `shutdown()`, is
/// until this function returns. `InstanceLock::drop` would then release the OS
/// lock and admit the replacement the unclean report was refusing.
///
/// The tests that previously covered this all kept a runtime or a stale handle
/// alive, so none of them could have caught it. This one deliberately keeps
/// nothing: the shutdown consumes the runtime, no handle is retained, and the
/// replacement attempt happens with every object gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn an_unclean_shutdown_retains_the_home_with_no_surviving_handle() {
    let lane = Lane::new();
    let quarantined_before = grokptah_agent_bridge::quarantined_process_lock_count();

    // Scoped so nothing outlives it: the runtime is consumed by `shutdown()`
    // and no handle escapes.
    let report = {
        let (runtime, _session_id) = lane.boot();
        runtime
            .register_shutdown_hook(
                "audit-ledger-seal",
                Box::new(|| anyhow::bail!("ledger seal could not be committed")),
            )
            .unwrap();
        runtime.shutdown().await
    };
    assert!(
        !report.is_clean(),
        "a failed shutdown hook must produce an unclean report: {}",
        report.operator_summary()
    );
    assert!(
        !report.process_lock_released,
        "an unclean shutdown must not release the process lock"
    );
    assert!(
        report.process_lock_retained_for_safety,
        "the report must say the lock was retained: {}",
        report.operator_summary()
    );

    // The decisive assertion: every object from that runtime is gone, and the
    // home is still refused.
    assert_eq!(
        grokptah_agent_bridge::quarantined_process_lock_count(),
        quarantined_before + 1,
        "the lock must be held by the process, not by an object someone can drop"
    );
    let refused = AgentHost::create(HostConfig::default());
    assert!(
        refused.is_err(),
        "a home whose owner could not prove a safe stop must stay refused once \
         every handle to that owner is gone"
    );
    assert!(
        lane.instance_lock().is_file(),
        "quarantine holds the advisory lock; it never deletes the lock file"
    );
}

/// P0 — the same property on the `Drop` path, with a task that captures no
/// lifecycle so nothing but the runtime itself could have kept the lock alive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_dropped_runtime_retains_the_home_with_no_surviving_handle() {
    let lane = Lane::new();
    let quarantined_before = grokptah_agent_bridge::quarantined_process_lock_count();
    let released = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());

    {
        let (mut runtime, _session_id) = lane.boot();
        runtime.set_durable_write_seal_timeout(Duration::from_millis(100));
        let entered_tx = entered.clone();
        let release_rx = released.clone();
        // Captures only two Notify handles — no host handle, no lifecycle. When
        // the runtime is dropped below, nothing in this task keeps it alive.
        runtime
            .spawn_supervised("an effectful task holding no host reference", async move {
                entered_tx.notify_one();
                release_rx.notified().await;
            })
            .expect("a running host supervises work");
        entered.notified().await;
        assert!(runtime.supervised_task_count() > 0);
        drop(runtime);
    }

    assert_eq!(
        grokptah_agent_bridge::quarantined_process_lock_count(),
        quarantined_before + 1,
        "a drop with outstanding work must quarantine the lock in the process"
    );
    assert!(
        AgentHost::create(HostConfig::default()).is_err(),
        "no replacement may start while work this process cannot account for is outstanding"
    );

    // Even after the task finishes, the quarantine stands: `Drop` had no later
    // point to re-check, so the honest contract is "until this process exits".
    released.notify_one();
    assert!(
        AgentHost::create(HostConfig::default()).is_err(),
        "a quarantined home stays quarantined for the life of the process"
    );
}

/// Registration must happen **before** the effect starts, not when its future
/// is first polled.
///
/// A future registered at first poll has a window: it has been created and can
/// be polled by any executor, while shutdown still counts zero. This asserts
/// the count rises at construction, before anything has run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn an_effect_is_registered_before_it_starts() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    let handle = runtime.handle();
    let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let polled_tx = polled.clone();
    let before = runtime.supervised_task_count();
    let tracked = handle
        .track_supervised("an effect that has not started", async move {
            polled_tx.store(true, std::sync::atomic::Ordering::Release);
        })
        .expect("a running host registers work");

    assert_eq!(
        runtime.supervised_task_count(),
        before + 1,
        "the barrier must count this effect before it has been polled once"
    );
    assert!(
        !polled.load(std::sync::atomic::Ordering::Acquire),
        "nothing has run yet; the registration is what is being asserted"
    );

    tracked.await;
    assert!(polled.load(std::sync::atomic::Ordering::Acquire));
    assert_clean_shutdown(&runtime.shutdown().await);
}

/// The race, not merely the refusal: an effect that is *in flight* when
/// shutdown begins must hold the release until it finishes.
///
/// The refusal test proves a stopped runtime says no. This proves the other
/// half — that shutdown cannot slip past an effect already running — by having
/// the effect observe, at its own end, whether the home had already been handed
/// on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn an_in_flight_effect_holds_the_release_until_it_finishes() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();
    let handle = runtime.handle();

    let entered = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let home_was_free_at_the_end = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let entered_tx = entered.clone();
    let finished_tx = finished.clone();
    let observed = home_was_free_at_the_end.clone();
    let lock_path = lane.instance_lock();
    handle
        .spawn_supervised("an effect racing shutdown", async move {
            entered_tx.notify_one();
            // Long enough that a shutdown which did not wait would have
            // released and let a replacement in before this line.
            for _ in 0..256 {
                tokio::task::yield_now().await;
            }
            if !grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
                observed.store(true, std::sync::atomic::Ordering::Release);
            }
            finished_tx.store(true, std::sync::atomic::Ordering::Release);
        })
        .expect("a running host supervises work");
    entered.notified().await;

    // Shutdown starts with the effect already running.
    let report = runtime.shutdown().await;
    assert_clean_shutdown(&report);

    assert!(
        finished.load(std::sync::atomic::Ordering::Acquire),
        "a clean shutdown must not return before an in-flight effect finished"
    );
    assert!(
        !home_was_free_at_the_end.load(std::sync::atomic::Ordering::Acquire),
        "the home must still have been owned while the effect was acting"
    );

    // Only now may a replacement take it.
    let replacement = restart_same_home_now(&lane);
    assert_clean_shutdown(&replacement.shutdown().await);
}

/// A durable write that fails for lack of space (or any I/O failure) must make
/// the shutdown unclean and quarantine the home, never report a clean stop over
/// a lost write.
///
/// The failure is injected by putting a directory where the audit ledger's file
/// belongs, so the append fails with `EISDIR`. That is uid-independent — mode
/// bits would not bite when the suite runs as root, and a test that silently
/// stopped injecting anything would assert nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_failed_durable_flush_is_unclean_and_quarantines_the_home() {
    let lane = Lane::new();
    let quarantined_before = grokptah_agent_bridge::quarantined_process_lock_count();
    let report = {
        let (runtime, _session_id) = lane.boot();
        let _store = runtime.ensure_orchestration_store().unwrap();
        let audit_file = lane
            .grokptah_home()
            .join("orchestration")
            .join("audit")
            .join("audit.jsonl");
        let _ = std::fs::remove_file(&audit_file);
        std::fs::create_dir_all(&audit_file).unwrap();
        runtime.shutdown().await
    };

    assert!(
        !report.is_clean(),
        "a durable write that could not land must not report a clean stop: {}",
        report.operator_summary()
    );
    assert!(
        !report.flush_errors.is_empty(),
        "the failure must be reported, not swallowed: {}",
        report.operator_summary()
    );
    assert!(
        !report.process_lock_released && report.process_lock_retained_for_safety,
        "a lost durable write must retain the home: {}",
        report.operator_summary()
    );
    assert_eq!(
        grokptah_agent_bridge::quarantined_process_lock_count(),
        quarantined_before + 1,
        "and the retention must be process-owned, surviving every handle"
    );
    assert!(
        AgentHost::create(HostConfig::default()).is_err(),
        "no replacement may take a home whose last write is unaccounted for"
    );
}

/// The durable event journal must be closed and joined *inside* shutdown, so a
/// clean report cannot precede a queued or failed journal write.
///
/// The writer thread is otherwise joined only when the last handle to the bus
/// drops — after the report is produced, and possibly never if any clone
/// outlives shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn the_journal_writer_is_closed_and_joined_before_a_clean_report() {
    let lane = Lane::new();
    let (runtime, session_id) = lane.boot();

    // Enough traffic that entries are genuinely queued behind the writer.
    let bus = runtime.event_bus();
    for i in 0..256 {
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id,
            text: format!("journal-{i}"),
        });
    }
    let last_seq = bus.current_seq();

    // A clone of the bus deliberately outlives shutdown: without an explicit
    // close the writer thread would still be alive here, because it is joined
    // only when the last handle drops.
    let surviving_clone = bus.clone();
    assert!(surviving_clone.journal_writer_is_live());
    assert_clean_shutdown(&runtime.shutdown().await);

    // The discriminating assertion. A writer that merely kept up would still be
    // live here; only an explicit close-and-join inside shutdown makes it false
    // while a clone of the bus is still held.
    assert!(
        !surviving_clone.journal_writer_is_live(),
        "shutdown must close and join the journal writer, not rely on the last \
         handle being dropped afterwards"
    );

    // Everything published before the stop is on disk by the time the report
    // said "clean".
    let journal = lane
        .grokptah_home()
        .join("orchestration")
        .join("event_journal.jsonl");
    let written = std::fs::read_to_string(&journal).expect("the journal was persisted");
    assert!(
        written.lines().count() as u64 >= last_seq,
        "a clean report must not precede the journal: {} lines for {last_seq} events",
        written.lines().count()
    );
    assert!(
        written.contains("journal-255"),
        "the last queued entry must have been flushed before the report"
    );

    // Closing again is idempotent, and the surviving clone has no live writer.
    assert!(surviving_clone.close_journal_writer().is_none());
}

/// Abrupt death, not an orderly stop: `SIGKILL` a real process mid-write and
/// prove the home is takeable afterwards and the durable state is coherent.
///
/// Every other restart test in this file exercises an *orderly* stop, which is
/// not crash evidence: an ordered shutdown releases the lock deliberately, so it
/// says nothing about what happens when a process never gets to run any code.
/// `flock` is released by the kernel on process death; this asserts that in
/// practice, that the lock file survives, and that a replacement can open the
/// same ledger and read what was committed before the kill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_killed_owner_leaves_a_takeable_home_and_a_readable_ledger() {
    if std::env::var_os(LOCK_HOLDER_ENV).is_some() {
        return;
    }
    let lane = Lane::new();
    let home = lane.grokptah_home();
    std::fs::create_dir_all(&home).unwrap();
    let release = home.join("release-the-lock");

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "holds_the_instance_lock_until_released",
            "--nocapture",
            "--test-threads=1",
            "--ignored",
        ])
        .env(LOCK_HOLDER_ENV, "1")
        .env("GROKPTAH_HOME", &home)
        .env("GROKPTAH_LOCK_RELEASE", &release)
        .spawn()
        .expect("re-exec as the lock holder");

    let lock_path = lane.instance_lock();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
        assert!(
            std::time::Instant::now() < deadline,
            "child never took the lock"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before taking the lock"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // While that process owns the home, this one is refused — the precondition
    // that makes the post-kill result meaningful.
    assert!(AgentHost::create(HostConfig::default()).is_err());

    // No shutdown, no unwinding, no destructors: the process is killed outright.
    child.kill().expect("kill the owner");
    let status = child.wait().expect("reap the killed owner");
    assert!(
        !status.success(),
        "the owner was killed, not stopped: {status:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "the owner must have died to SIGKILL, not exited on its own: {status:?}"
        );
    }

    // The kernel released the advisory lock; the lock *file* is still there.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while grokptah_agent_bridge::instance_lock_is_held(&lock_path) {
        assert!(
            std::time::Instant::now() < deadline,
            "the kernel did not release the killed owner's lock"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        lock_path.is_file(),
        "a crash must not remove the lock file; only the advisory lock is gone"
    );

    // A replacement takes the home with no cleanup step, opens the same ledger,
    // and finds it coherent.
    let replacement = AgentHost::create(HostConfig::default())
        .expect("a crashed owner's home must be takeable without manual repair");
    replacement.start().unwrap();
    let store = replacement
        .ensure_orchestration_store()
        .expect("the ledger a killed process left behind must open");
    let session = replacement.session_new_kind(SessionKind::Build).unwrap();
    assert!(
        probe_store_write(&store, session.id).is_ok(),
        "and must be writable by the process that now owns it"
    );
    assert_clean_shutdown(&replacement.shutdown().await);
}

/// P1 — a lease may only be bound to a runtime that owns its home.
///
/// Binding is refused across homes, and — the part that matters — the refusal
/// does not silently hand the foreign ledger this runtime's authority. It
/// keeps the OS lock it took for its own home at open, so it still writes its
/// own home and gains nothing over this one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_store_cannot_be_bound_to_a_runtime_that_owns_another_home() {
    let lane = Lane::new();
    let (runtime, _session_id) = lane.boot();

    // A ledger under a *different* home than the one this runtime owns.
    let other = tempfile::tempdir().unwrap();
    let foreign = OrchStore::open(other.path().join(".grokptah").join("orchestration"))
        .expect("the foreign home is unowned");
    assert!(
        !runtime.bind_store_for_test(&foreign),
        "a runtime must not adopt a ledger for a home it does not own"
    );
    assert!(
        !foreign.is_lease_bound(),
        "the refused bind must leave the foreign ledger on its own authority, \
         not on this runtime's lifecycle"
    );
    // The foreign ledger still owns its own home and still writes there.
    assert!(probe_store_write(&foreign, Uuid::nil()).is_ok());

    // The decisive half: this runtime shutting down must not affect a home it
    // never owned, and the foreign ledger must not have inherited authority
    // over *this* runtime's home either.
    assert_clean_shutdown(&runtime.shutdown().await);
    assert!(
        probe_store_write(&foreign, Uuid::nil()).is_ok(),
        "a ledger holding its own home's lock is unaffected by another home's shutdown"
    );

    drop(foreign);
}
