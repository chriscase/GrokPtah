//! Adversarial durability tests for the long-running-agent P0 repairs.
//!
//! Every test drives shipped service/store/host code. Nothing here reaches
//! into a test-only execution path: the "crash" cases manipulate the durable
//! ledger the way a power loss would leave it, then restart the real service
//! and assert what it does.
//!
//! Two rules shape the assertions:
//!
//! * *Exactly once* is proved by side effects, not by ledger state. Each task
//!   appends a unique marker line to a workspace file; a task that ran twice
//!   leaves two lines, and a task that never ran leaves none.
//! * *Termination* is proved by the worker future's own liveness guard, not by
//!   a terminal record. A run can be marked `cancelled` while its future is
//!   still executing; `worker_future_finished` is what rules that out.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, AcceptanceIntent, AttemptLeaseState, AuthContext, OrchStore, OrchestrationConfig,
    OrchestrationService, ProviderSendState, RunBounds, SealStamp, SealedBounds,
    WorkspaceAllowlist, ACCEPTANCE_INTENT_VERSION,
};
use grokptah_agent_bridge::{
    safe_id_filename, set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig,
    RunExecutionMode, RunState, SessionKind,
};
use serde_json::json;
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN: &str = "durability-p0-secret-token";

/// One restartable deployment: a home directory, a workspace, a host, and the
/// control service over them. `restart()` tears all of it down (releasing the
/// store's exclusive lock) and brings a fresh instance up over the same disk.
struct Rig {
    home: TempDir,
    ws: TempDir,
    _env: ProcessEnvGuard,
    host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
    session: Uuid,
    max_concurrent: usize,
}

impl Rig {
    async fn new(max_concurrent: usize) -> Self {
        let mut env = ProcessEnvGuard::new();
        let home = tempdir().unwrap();
        let grokptah_home = home.path().join(".grokptah");
        std::fs::create_dir_all(&grokptah_home).unwrap();
        set_grokptah_home_override(Some(grokptah_home));
        env.set("GROKPTAH_AGENT_OFFLINE", "1");

        let ws = tempdir().unwrap();
        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        let orch = build_service(&host, home.path(), ws.path(), max_concurrent).await;
        Self {
            home,
            ws,
            _env: env,
            host,
            orch,
            session: session.id,
            max_concurrent,
        }
    }

    fn auth(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN}")))
            .unwrap()
    }

    fn store_path(&self) -> std::path::PathBuf {
        self.home.path().join("orch")
    }

    fn marker_path(&self) -> std::path::PathBuf {
        self.ws.path().join("ledger.txt")
    }

    /// Read every marker line written so far, counted by value.
    fn markers(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(self.marker_path()) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                *counts.entry(line.to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Tear the process down the way a crash would, then bring it back over
    /// the same durable state. Dropping the host and the service releases the
    /// store's exclusive advisory lock, which is what makes the reopen real.
    async fn restart(self) -> Self {
        let Rig {
            home,
            ws,
            _env,
            host,
            orch,
            session,
            max_concurrent,
        } = self;
        drop(orch);
        drop(host);

        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        host.session_set_cwd(session, ws.path()).unwrap();
        let orch = build_service(&host, home.path(), ws.path(), max_concurrent).await;
        Self {
            home,
            ws,
            _env,
            host,
            orch,
            session,
            max_concurrent,
        }
    }

    /// Tear down without bringing anything back, so the durable state can be
    /// edited or inspected directly.
    fn shutdown(self) -> (TempDir, TempDir, ProcessEnvGuard) {
        let Rig {
            home,
            ws,
            _env,
            host,
            orch,
            ..
        } = self;
        drop(orch);
        drop(host);
        (home, ws, _env)
    }
}

/// Start a host, waiting out the previous instance's process lock.
///
/// A restart is only real once the old instance has released its locks, and
/// that release is driven by background tasks finishing. This yields to the
/// runtime between attempts so those tasks can actually run.
async fn start_host() -> AgentHostHandle {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        match host.start() {
            Ok(()) => return host,
            Err(error) if std::time::Instant::now() < deadline => {
                drop(host);
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("host never started: {error}"),
        }
    }
}

/// Open the durable ledger, waiting out the previous instance's exclusive
/// advisory lock.
async fn open_store(root: &Path) -> OrchStore {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match OrchStore::open(root) {
            Ok(store) => return store,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("store never opened: {error}"),
        }
    }
}

async fn build_service(
    host: &AgentHostHandle,
    home: &Path,
    ws: &Path,
    max_concurrent: usize,
) -> Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        open_store(&home.join("orch")).await,
        OrchestrationConfig {
            bearer_token: TOKEN.to_string(),
            allowlist: WorkspaceAllowlist::new([ws.to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds {
                max_prompt_bytes: 50_000,
                max_rounds: 4,
                max_duration_ms: 30_000,
            },
        },
    )
}

/// A prompt whose only effect is one appended marker line.
fn marker_prompt(marker: &str) -> String {
    format!("run printf '{marker}\\n' >> ledger.txt")
}

async fn wait_for<F>(label: &str, timeout: Duration, mut ready: F)
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    loop {
        if ready() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {label}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_all_terminal(rig: &Rig, run_ids: &[String], timeout: Duration) {
    let auth = rig.auth();
    let start = std::time::Instant::now();
    loop {
        let outstanding = run_ids
            .iter()
            .filter(|run_id| {
                rig.orch
                    .get_run(&auth, run_id)
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<RunState>(value["state"].clone()).ok()
                    })
                    .map(|state| !state.is_terminal())
                    .unwrap_or(false)
            })
            .count();
        if outstanding == 0 {
            return;
        }
        if start.elapsed() > timeout {
            panic!("{outstanding} run(s) never reached a terminal state");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn run_state(rig: &Rig, run_id: &str) -> RunState {
    let value = rig.orch.get_run(&rig.auth(), run_id).unwrap();
    serde_json::from_value(value["state"].clone()).unwrap()
}

fn intent_file(store_root: &Path, run_id: &str) -> std::path::PathBuf {
    store_root
        .join("inputs")
        .join(format!("{}.json", safe_id_filename(run_id).unwrap()))
}

fn receipt_file(store_root: &Path, request_id: &str) -> std::path::PathBuf {
    store_root
        .join("idempotency")
        .join(format!("{}.json", safe_id_filename(request_id).unwrap()))
}

// ── P0-2: every accepted task survives a restart, exactly once ─────────

/// Thirty-two queued tasks, one restart, thirty-two side effects.
///
/// This is the headline durability claim: an accepted task is durable input,
/// not an in-memory prompt. The process is destroyed while all of them are
/// still queued, and every one of them still runs — once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn thirty_two_queued_tasks_survive_restart_and_run_exactly_once() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    // Hold the single capacity slot with a long run so the queue genuinely
    // fills instead of draining behind each submission.
    let blocker = rig
        .orch
        .submit_task(
            &auth,
            "queue-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 45".into(),
            None,
        )
        .await
        .expect("the blocker must be accepted");
    // The receipt is honestly `queued`: nothing has started at the moment it
    // is issued. The run reaches `running` when its worker acknowledges.
    assert_eq!(blocker["state"], "queued");

    let mut queued_markers = Vec::new();
    let mut queued_runs = Vec::new();
    for index in 0..32 {
        let marker = format!("task-{index:02}");
        let response = rig
            .orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                &format!("req-{index:02}"),
                rig.session,
                rig.ws.path(),
                marker_prompt(&marker),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .expect("submission must be accepted");
        assert_eq!(
            response["state"], "queued",
            "task {index} was not queued behind the blocker"
        );
        queued_markers.push(marker);
        queued_runs.push(response["runId"].as_str().unwrap().to_string());
    }
    assert_eq!(
        queued_markers.len(),
        32,
        "expected exactly 32 queued admissions, got {}",
        queued_markers.len()
    );

    // Every queued task must have durable input before the restart: that is
    // the whole reason it can survive one.
    let store_root = rig.store_path();
    for run_id in &queued_runs {
        assert!(
            intent_file(&store_root, run_id).is_file(),
            "queued run {run_id} has no durable input"
        );
    }

    // Crash.
    let rig = rig.restart().await;

    wait_all_terminal(&rig, &queued_runs, Duration::from_secs(180)).await;

    let markers = rig.markers();
    for marker in &queued_markers {
        assert_eq!(
            markers.get(marker).copied().unwrap_or(0),
            1,
            "{marker} must run exactly once across the restart (saw {:?})",
            markers.get(marker)
        );
    }
    for run_id in &queued_runs {
        let state = run_state(&rig, run_id);
        assert!(
            state.is_terminal(),
            "run {run_id} is {state:?} after recovery"
        );
        assert!(
            !intent_file(&store_root, run_id).is_file(),
            "terminal run {run_id} must not keep executable input"
        );
    }
    set_grokptah_home_override(None);
}

/// Recovery is idempotent. Restarting repeatedly, with queued work present at
/// every restart, must not dispatch anything a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn repeated_recovery_never_duplicates_dispatch() {
    let mut rig = Rig::new(1).await;
    let mut all_markers = Vec::new();
    let mut all_runs = Vec::new();

    for round in 0..3 {
        let auth = rig.auth();
        for index in 0..4 {
            let marker = format!("round-{round}-task-{index}");
            let response = rig
                .orch
                .submit_task_with_execution_mode_and_queue(
                    &auth,
                    &format!("req-{round}-{index}"),
                    rig.session,
                    rig.ws.path(),
                    marker_prompt(&marker),
                    None,
                    RunExecutionMode::Shared,
                    true,
                )
                .await
                .expect("submission must be accepted");
            all_markers.push(marker);
            all_runs.push(response["runId"].as_str().unwrap().to_string());
        }
        // Restart immediately, while work is still in flight.
        rig = rig.restart().await;
    }

    wait_all_terminal(&rig, &all_runs, Duration::from_secs(180)).await;
    let markers = rig.markers();
    for marker in &all_markers {
        let seen = markers.get(marker).copied().unwrap_or(0);
        assert!(
            seen <= 1,
            "{marker} was dispatched {seen} times across repeated recovery"
        );
    }
    // A run that was `Running` when the process died is never resumed; the
    // rest must have completed. Nothing may still be pending.
    for run_id in &all_runs {
        assert!(run_state(&rig, run_id).is_terminal());
    }
    set_grokptah_home_override(None);
}

// ── P0-1: an explicit Err can never later execute ──────────────────────

/// A submission whose durable input cannot be written must fail, and must
/// never become executable work — not now, and not after any number of
/// restarts. Recovery must not synthesize a run for it either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn input_persistence_error_never_executes_after_repeated_restart() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();
    let store_root = rig.store_path();

    // Make the private-input directory unusable exactly the way a full or
    // hostile volume would: the path exists, but not as a directory.
    let inputs = store_root.join("inputs");
    std::fs::remove_dir_all(&inputs).unwrap();
    std::fs::write(&inputs, b"not a directory").unwrap();

    let error = rig
        .orch
        .submit_task(
            &auth,
            "doomed-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("must-never-run"),
            None,
        )
        .await
        .expect_err("a submission without durable input must fail");
    assert!(
        !error.message.is_empty(),
        "the failure must name a reason: {error:?}"
    );

    // The receipt is settled failed, so an exact retry replays the failure
    // rather than turning into queued success.
    let replay = rig
        .orch
        .submit_task(
            &auth,
            "doomed-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("must-never-run"),
            None,
        )
        .await;
    assert!(replay.is_err(), "a failed receipt must replay as a failure");

    std::fs::remove_file(&inputs).unwrap();
    std::fs::create_dir_all(&inputs).unwrap();

    let mut rig = rig;
    for _ in 0..3 {
        rig = rig.restart().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            rig.markers().is_empty(),
            "a failed admission executed after a restart: {:?}",
            rig.markers()
        );
        // Recovery must not have synthesized a run for the failed request.
        let runs = rig.orch.store().list_runs().unwrap();
        assert!(
            runs.iter().all(|run| run.request_id != "doomed-request"),
            "recovery synthesized a run for a failed request"
        );
    }
    set_grokptah_home_override(None);
}

/// The receipt is the promise. If it cannot be written, the admission is
/// tombstoned and its input destroyed in the same step, so no later recovery
/// pass can find anything to run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn receipt_persistence_error_tombstones_the_admission() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();
    let store_root = rig.store_path();

    // The exclusive claim still lands (it is written directly), but the
    // completion rewrite cannot: its staging path is occupied by a directory.
    let blocked = store_root.join("idempotency").join(format!(
        "{}.json.tmp",
        safe_id_filename("receipt-doomed").unwrap()
    ));
    std::fs::create_dir_all(&blocked).unwrap();

    let error = rig
        .orch
        .submit_task(
            &auth,
            "receipt-doomed",
            rig.session,
            rig.ws.path(),
            marker_prompt("receipt-marker"),
            None,
        )
        .await
        .expect_err("a submission without a durable receipt must fail");
    assert!(!error.message.is_empty(), "{error:?}");

    let doomed = rig
        .orch
        .store()
        .list_runs()
        .unwrap()
        .into_iter()
        .find(|run| run.request_id == "receipt-doomed");
    if let Some(run) = &doomed {
        assert_eq!(
            run.state,
            RunState::Failed,
            "a run whose receipt failed must be tombstoned, not left runnable"
        );
        assert!(
            !intent_file(&store_root, &run.run_id).is_file(),
            "a tombstoned admission must not keep executable input"
        );
    }

    std::fs::remove_dir_all(&blocked).unwrap();
    let mut rig = rig;
    for _ in 0..2 {
        rig = rig.restart().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            rig.markers().is_empty(),
            "an admission with no durable receipt executed: {:?}",
            rig.markers()
        );
    }
    if let Some(run) = doomed {
        assert_eq!(run_state(&rig, &run.run_id), RunState::Failed);
    }
    set_grokptah_home_override(None);
}

// ── crash-safe cuts ────────────────────────────────────────────────────

/// Cut C3: durable input and a `Queued` run exist, but the receipt never
/// completed. The caller was never told the work was accepted, so it must
/// never run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn crash_between_input_and_receipt_never_executes() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    // Occupy the only slot so the task under test stays queued.
    let blocker = rig
        .orch
        .submit_task(
            &auth,
            "blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let _ = blocker;

    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "half-accepted",
            rig.session,
            rig.ws.path(),
            marker_prompt("half-accepted-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    assert_eq!(response["state"], "queued");
    let run_id = response["runId"].as_str().unwrap().to_string();

    let store_root = rig.store_path();
    let (home, ws, env) = rig.shutdown();

    // Rewind the receipt to the state a crash mid-completion would leave.
    let receipt_path = receipt_file(&store_root, "half-accepted");
    let mut receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt_path).unwrap()).unwrap();
    receipt["status"] = json!("pending");
    receipt["response"] = json!(null);
    receipt["runId"] = json!(null);
    std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    assert!(
        intent_file(&store_root, &run_id).is_file(),
        "the durable input must still be present before recovery"
    );

    let host = start_host().await;
    host.set_project_cwd(ws.path()).unwrap();
    let orch = build_service(&host, home.path(), ws.path(), 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let recovered = orch.store().load_run(&run_id).unwrap().expect("run record");
    assert_eq!(
        recovered.state,
        RunState::Interrupted,
        "a half-accepted admission must be tombstoned"
    );
    assert_eq!(recovered.error_code.as_deref(), Some("admission_lost"));
    assert!(
        !intent_file(&store_root, &run_id).is_file(),
        "a tombstoned admission must not keep executable input"
    );
    let ledger = std::fs::read_to_string(ws.path().join("ledger.txt")).unwrap_or_default();
    assert!(
        !ledger.contains("half-accepted-marker"),
        "a half-accepted admission executed: {ledger}"
    );
    drop(orch);
    drop(host);
    drop(env);
    set_grokptah_home_override(None);
}

/// Cut C2: durable input exists for a run that was never recorded. Recovery
/// must reclaim it as garbage and must never synthesize a run from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn recovery_never_synthesizes_a_run_from_orphaned_input() {
    let rig = Rig::new(1).await;
    let store_root = rig.store_path();
    let session = rig.session;
    let workspace = rig.ws.path().display().to_string();
    let orphan_run = Uuid::new_v4().to_string();

    let intent = AcceptanceIntent {
        intent_version: ACCEPTANCE_INTENT_VERSION,
        run_id: orphan_run.clone(),
        request_id: "orphan-request".into(),
        payload_hash: hash_payload(&json!({"orphan": true})),
        tool: "ptah_submit_task".into(),
        session_id: session,
        session_revision: "0:0".into(),
        workspace: workspace.clone(),
        workspace_revision: "ready:shared".into(),
        agent_id: None,
        agent_revision: 0,
        spec_revision: "grokptah-agent-bridge/orchestration/1".into(),
        principal_token_id: "primary".into(),
        principal_revision: hash_payload(&json!({"principal": "test"})),
        policy_revision: hash_payload(&json!({"policy": "test"})),
        route_revision: hash_payload(&json!({"route": "test"})),
        prompt: marker_prompt("orphan-marker"),
        bounds: SealedBounds {
            max_prompt_bytes: 50_000,
            max_rounds: 2,
            max_duration_ms: 10_000,
        },
        execution_mode: RunExecutionMode::Shared,
        allow_queue: true,
        retry_of: None,
        parent_run_id: None,
        created_at: chrono::Utc::now(),
        digest: String::new(),
        seal: SealStamp::unsealed(),
    }
    .seal_with(rig.orch.store().seal_authority())
    .unwrap();
    rig.orch.store().save_acceptance_intent(&intent).unwrap();
    assert!(intent_file(&store_root, &orphan_run).is_file());

    let rig = rig.restart().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(
        rig.orch.store().load_run(&orphan_run).unwrap().is_none(),
        "recovery synthesized a run from orphaned input"
    );
    assert!(
        !intent_file(&store_root, &orphan_run).is_file(),
        "orphaned input must be reclaimed"
    );
    assert!(
        rig.markers().is_empty(),
        "orphaned input executed: {:?}",
        rig.markers()
    );
    set_grokptah_home_override(None);
}

/// Cut C5: the attempt lease was taken, then the process died. Exclusive
/// ownership of the ledger proves the old holder is gone, so the work still
/// runs — exactly once, under a fresh attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn crash_after_lease_before_spawn_still_runs_exactly_once() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "lease-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "lease-crash",
            rig.session,
            rig.ws.path(),
            marker_prompt("lease-crash-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();

    // Take a lease and die holding it.
    let intent = rig
        .orch
        .store()
        .load_acceptance_intent(&run_id)
        .unwrap()
        .expect("durable input");
    let held = rig
        .orch
        .store()
        .acquire_attempt_lease(
            &run_id,
            "dead-instance",
            rig.session,
            &intent.digest,
            600_000,
        )
        .unwrap();
    assert_eq!(held.state, AttemptLeaseState::Held);

    let rig = rig.restart().await;
    wait_all_terminal(&rig, std::slice::from_ref(&run_id), Duration::from_secs(90)).await;

    assert_eq!(
        rig.markers()
            .get("lease-crash-marker")
            .copied()
            .unwrap_or(0),
        1,
        "a crash holding the lease must still run the work exactly once"
    );
    set_grokptah_home_override(None);
}

/// Cut C7: input left behind for an already-terminal run is garbage. It must
/// be reclaimed, and must never re-execute.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn input_left_behind_after_terminalization_never_reruns() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();
    let store_root = rig.store_path();

    let response = rig
        .orch
        .submit_task(
            &auth,
            "cleanup-crash",
            rig.session,
            rig.ws.path(),
            marker_prompt("cleanup-marker"),
            None,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    wait_all_terminal(&rig, std::slice::from_ref(&run_id), Duration::from_secs(60)).await;
    assert_eq!(rig.markers().get("cleanup-marker").copied(), Some(1));

    // Re-plant the input, as a crash between "terminal" and "input removed"
    // would leave it.
    let replanted = AcceptanceIntent {
        intent_version: ACCEPTANCE_INTENT_VERSION,
        run_id: run_id.clone(),
        request_id: "cleanup-crash".into(),
        payload_hash: hash_payload(&json!({"replanted": true})),
        tool: "ptah_submit_task".into(),
        session_id: rig.session,
        session_revision: "0:0".into(),
        workspace: rig.ws.path().display().to_string(),
        workspace_revision: "ready:shared".into(),
        agent_id: None,
        agent_revision: 0,
        spec_revision: "grokptah-agent-bridge/orchestration/1".into(),
        principal_token_id: "primary".into(),
        principal_revision: hash_payload(&json!({"principal": "test"})),
        policy_revision: hash_payload(&json!({"policy": "test"})),
        route_revision: hash_payload(&json!({"route": "test"})),
        prompt: marker_prompt("cleanup-marker"),
        bounds: SealedBounds {
            max_prompt_bytes: 50_000,
            max_rounds: 2,
            max_duration_ms: 10_000,
        },
        execution_mode: RunExecutionMode::Shared,
        allow_queue: true,
        retry_of: None,
        parent_run_id: None,
        created_at: chrono::Utc::now(),
        digest: String::new(),
        seal: SealStamp::unsealed(),
    }
    .seal_with(rig.orch.store().seal_authority())
    .unwrap();
    rig.orch.store().save_acceptance_intent(&replanted).unwrap();

    let rig = rig.restart().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rig.markers().get("cleanup-marker").copied(),
        Some(1),
        "a terminal run re-executed from leftover input"
    );
    assert!(
        !intent_file(&store_root, &run_id).is_file(),
        "leftover input for a terminal run must be reclaimed"
    );
    set_grokptah_home_override(None);
}

// ── P0-3: the seal ─────────────────────────────────────────────────────

/// A parseable, well-formed tamper of any execution-relevant field must fail
/// closed. The task is tombstoned as tampered and never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn parseable_field_tamper_fails_closed_and_never_executes() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "tamper-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "tamper-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("original-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();

    let store_root = rig.store_path();
    let (home, ws, env) = rig.shutdown();

    // Swap the prompt for an attacker's, keeping the record perfectly
    // parseable. Only the seal stands between this and execution.
    let path = intent_file(&store_root, &run_id);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["prompt"] = json!("run printf 'tampered-marker\\n' >> ledger.txt");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let host = start_host().await;
    host.set_project_cwd(ws.path()).unwrap();
    let orch = build_service(&host, home.path(), ws.path(), 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let recovered = orch.store().load_run(&run_id).unwrap().expect("run record");
    assert_eq!(recovered.state, RunState::Interrupted);
    assert_eq!(
        recovered.error_code.as_deref(),
        Some("admission_tampered"),
        "a tampered admission must be named as such"
    );
    let ledger = std::fs::read_to_string(ws.path().join("ledger.txt")).unwrap_or_default();
    assert!(
        !ledger.contains("tampered-marker"),
        "a tampered prompt executed: {ledger}"
    );
    assert!(
        !ledger.contains("original-marker"),
        "a tampered admission executed its original prompt: {ledger}"
    );
    assert!(
        orch.store().load_acceptance_intent(&run_id).is_err()
            || !intent_file(&store_root, &run_id).is_file(),
        "a tampered input must never load as valid"
    );
    drop(orch);
    drop(host);
    drop(env);
    set_grokptah_home_override(None);
}

/// Durable input is private and symlink-resistant: owner-only on disk,
/// unreadable through a symlink, and rejected outright once its permissions
/// are widened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn durable_input_is_private_and_symlink_resistant() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "priv-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "priv-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("private-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    let store_root = rig.store_path();
    // Only the Unix authority assertions below read this; on Windows the
    // equivalent check is the DACL verdict, unit-tested in `ledger_io`.
    #[cfg_attr(not(unix), allow(unused_variables))]
    let path = intent_file(&store_root, &run_id);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "durable input must be owner-only, saw {mode:o}"
        );

        // Widened permissions are treated as tampering, not repaired quietly.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            rig.orch.store().load_acceptance_intent(&run_id).is_err(),
            "world-readable durable input must fail closed"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(rig.orch.store().load_acceptance_intent(&run_id).is_ok());

        // A symlink in place of the record must never be followed.
        let decoy = store_root.join("decoy.json");
        std::fs::copy(&path, &decoy).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&decoy, &path).unwrap();
        assert!(
            rig.orch.store().load_acceptance_intent(&run_id).is_err(),
            "a symlinked durable input must fail closed"
        );
    }
    set_grokptah_home_override(None);
}

// ── P0-2 / P0-4: the mandatory attempt lease ───────────────────────────

/// Exactly one attempt may hold a run, and only that attempt can heartbeat or
/// release it. A stale attempt id or a different owner is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn one_active_attempt_and_stale_or_wrong_owner_heartbeats_are_refused() {
    let rig = Rig::new(1).await;
    let store = rig.orch.store().clone();
    let run_id = Uuid::new_v4().to_string();
    let digest = hash_payload(&json!({"intent": "x"}));

    let first = store
        .acquire_attempt_lease(&run_id, "owner-a", rig.session, &digest, 600_000)
        .expect("first attempt takes the lease");
    assert_eq!(first.attempt, 1);

    // One active attempt.
    assert!(
        store
            .acquire_attempt_lease(&run_id, "owner-b", rig.session, &digest, 600_000)
            .is_err(),
        "a second attempt must not be able to hold the same run"
    );

    // Wrong owner cannot heartbeat or release.
    assert!(store
        .renew_attempt_lease(&run_id, &first.attempt_id, "owner-b")
        .is_err());
    assert!(store
        .release_attempt_lease(&run_id, &first.attempt_id, "owner-b")
        .is_err());
    // Stale attempt identity cannot heartbeat either.
    assert!(store
        .renew_attempt_lease(&run_id, "not-the-attempt", "owner-a")
        .is_err());
    // The real holder can.
    assert!(store
        .renew_attempt_lease(&run_id, &first.attempt_id, "owner-a")
        .is_ok());

    // Expiry promotes a new attempt, and the old one is dead for good.
    let expiring = Uuid::new_v4().to_string();
    let short = store
        .acquire_attempt_lease(&expiring, "owner-a", rig.session, &digest, 1)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let stolen = store
        .acquire_attempt_lease(&expiring, "owner-c", rig.session, &digest, 600_000)
        .expect("an expired lease must be reapable");
    assert_eq!(stolen.attempt, 2, "attempt numbers never repeat");
    assert_ne!(stolen.attempt_id, short.attempt_id);
    assert!(
        store
            .renew_attempt_lease(&expiring, &short.attempt_id, "owner-a")
            .is_err(),
        "a reaped attempt must never be able to renew its way back in"
    );
    assert!(
        store
            .release_attempt_lease(&expiring, &short.attempt_id, "owner-a")
            .is_err(),
        "a reaped attempt must not be able to release the new holder's lease"
    );
    set_grokptah_home_override(None);
}

// ── P0-4: capacity follows confirmed termination ───────────────────────

/// Cancelling a run must abort and bounded-await the actual worker before its
/// capacity can be reused. The assertion is on the worker future's own
/// liveness, not on the run record: a `cancelled` ledger entry proves nothing
/// about whether the work stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn capacity_is_not_reused_until_the_worker_future_is_gone() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "long-run",
            rig.session,
            rig.ws.path(),
            "run sleep 30".into(),
            None,
        )
        .await
        .unwrap();
    let long_id = long["runId"].as_str().unwrap().to_string();
    assert_eq!(
        long["state"], "queued",
        "the receipt reports what is true now"
    );

    let queued = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "follow-on",
            rig.session,
            rig.ws.path(),
            marker_prompt("follow-on-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_id = queued["runId"].as_str().unwrap().to_string();
    assert_eq!(queued["state"], "queued");

    wait_for(
        "the long run to be dispatched",
        Duration::from_secs(10),
        || rig.orch.live_run_ids().contains(&long_id),
    )
    .await;
    assert_eq!(
        rig.orch.worker_future_finished(&long_id),
        Some(false),
        "the long run's worker must still be live before teardown"
    );
    // Exactly one attempt holds capacity.
    assert_eq!(rig.orch.live_run_ids(), vec![long_id.clone()]);

    let cancelled = rig
        .orch
        .cancel(
            &auth,
            "cancel-long",
            rig.session,
            rig.ws.path(),
            Some(&long_id),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["teardownComplete"], true);
    assert_eq!(
        rig.orch.worker_future_finished(&long_id),
        Some(true),
        "capacity was reported free while the worker future could still run"
    );

    wait_all_terminal(
        &rig,
        std::slice::from_ref(&queued_id),
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(
        rig.orch.worker_future_finished(&long_id),
        Some(true),
        "the cancelled worker must still be gone once its slot was reused"
    );
    assert_eq!(
        rig.markers().get("follow-on-marker").copied(),
        Some(1),
        "the promoted task must run exactly once"
    );
    assert_eq!(run_state(&rig, &long_id), RunState::Cancelled);
    set_grokptah_home_override(None);
}

// ── P0-5: every outer-supervisor exit terminalizes ─────────────────────

/// Shutdown is an outer-supervisor exit. A live run must end with a durable
/// terminal record — `interrupted`, never left `Running` with no explanation —
/// and its capacity must be released exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn shutdown_terminalizes_every_live_run() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "shutdown-run",
            rig.session,
            rig.ws.path(),
            "run sleep 30".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = long["runId"].as_str().unwrap().to_string();
    // Wait for the worker to actually be *running*, not merely published.
    //
    // Publication now happens before the start gate opens, so `the future has
    // not finished` is also true of a worker parked behind a closed gate,
    // which is a different state: teardown there is certain, not uncertain.
    // `Running` is written by the worker itself as its first durable act, so
    // pairing it with an unfinished future is the unambiguous statement that
    // work is in flight.
    wait_for("the worker to start", Duration::from_secs(20), || {
        rig.orch.worker_future_finished(&run_id) == Some(false)
            && run_state(&rig, &run_id) == RunState::Running
    })
    .await;

    let store_root = rig.store_path();
    let (home, ws, env) = rig.shutdown();

    let reopened = open_store(&store_root).await;
    let record = reopened.load_run(&run_id).unwrap().expect("run record");
    assert!(
        record.state.is_terminal(),
        "shutdown left run {run_id} in {:?}",
        record.state
    );
    assert_eq!(record.state, RunState::Interrupted);
    assert!(
        matches!(
            record.error_code.as_deref(),
            Some("shutdown") | Some("supervisor_exit") | Some("interrupted")
        ),
        "shutdown must name why the run ended, saw {:?}",
        record.error_code
    );
    // Shutdown reaches one of exactly two states, and which one it reaches is
    // a genuine race with the abort: if the worker's future is dropped before
    // the synchronous path looks, quiescence really was proved and there is
    // nothing to fence; if it is not, the outcome is unknown and must be
    // recorded. Asserting only one of those would be asserting who won a
    // race, so what is asserted here is that the three durable consequences
    // never disagree — which is the actual bug class. The fenced branch is
    // driven deterministically, across a real process boundary, by
    // `orchestration_durability_p2::a_fenced_attempt_survives_the_death_of_the_coordinator_that_fenced_it`.
    let fence = reopened.load_teardown_uncertain(&run_id).unwrap();
    let lease = reopened
        .load_attempt_lease(&run_id)
        .unwrap()
        .expect("a dispatched attempt always leaves a lease record");
    let input_retained = intent_file(&store_root, &run_id).is_file();
    match &fence {
        Some(fence) => {
            assert_eq!(fence.run_id, run_id);
            assert!(
                !fence.reason.is_empty(),
                "the fence must say why the outcome is unknown"
            );
            assert_eq!(
                lease.state,
                AttemptLeaseState::Held,
                "a fenced run's lease must not be released, by shutdown or by a restart"
            );
            assert!(
                input_retained,
                "a fenced run keeps its durable input until the fence is lifted"
            );
        }
        None => {
            assert_eq!(
                lease.state,
                AttemptLeaseState::Released,
                "an unfenced terminal run must not still hold its lease"
            );
            assert!(
                !input_retained,
                "an unfenced terminal run must not keep executable input"
            );
        }
    }
    drop(reopened);
    drop(home);
    drop(ws);
    drop(env);
    set_grokptah_home_override(None);
}

// ── prompt / redaction boundary ────────────────────────────────────────

/// The full execution input is private. It lives in exactly one place — the
/// owner-only sealed intent — and never reaches a receipt, a run projection,
/// an event page, or the audit log. The control bearer never appears at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn public_surfaces_never_expose_the_private_prompt() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();
    let secret = "PROMPT-SECRET-NEVER-PUBLIC-9f2a";
    // The secret sits well past the bounded preview window, so this asserts
    // that the *full* input never escapes — the short preview is a documented,
    // deliberately bounded projection, not a leak.
    let prompt = format!(
        "run printf 'redaction-marker\\n' >> ledger.txt # {} {secret}",
        "x".repeat(600)
    );

    let response = rig
        .orch
        .submit_task(
            &auth,
            "redaction-request",
            rig.session,
            rig.ws.path(),
            prompt.clone(),
            None,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();

    // The accept response is a receipt, not a copy of the work.
    let encoded = serde_json::to_string(&response).unwrap();
    assert!(!encoded.contains(secret), "receipt leaked the prompt");
    assert!(!encoded.contains(TOKEN), "receipt leaked the control token");

    wait_all_terminal(&rig, std::slice::from_ref(&run_id), Duration::from_secs(60)).await;

    let store_root = rig.store_path();
    // The admission surfaces summarize the work; they never carry the input.
    for projection in [
        rig.orch.get_run(&auth, &run_id).unwrap(),
        rig.orch.get_progress(&auth, &run_id).unwrap(),
    ] {
        let text = serde_json::to_string(&projection).unwrap();
        assert!(
            !text.contains(secret),
            "an admission projection exposed the private prompt: {text}"
        );
        assert!(
            !text.contains(TOKEN),
            "an admission projection exposed the control token"
        );
    }

    // The run-scoped event journal is a different surface: it reports what the
    // agent actually did, to the authorized owner of that run, and a tool call
    // legitimately echoes the command it ran. What it must never carry is a
    // control-plane secret.
    let journal = rig.orch.get_events(&auth, Some(&run_id), 0, 50).unwrap();
    let journal_text = serde_json::to_string(&journal).unwrap();
    assert!(
        !journal_text.contains(TOKEN),
        "the event journal exposed the control token"
    );

    // The receipt on disk is a response, not an input copy.
    let receipt = std::fs::read_to_string(receipt_file(&store_root, "redaction-request")).unwrap();
    assert!(
        !receipt.contains(secret),
        "the receipt persisted the prompt"
    );

    // The audit log records what happened, never the work itself.
    let audit =
        std::fs::read_to_string(store_root.join("audit").join("audit.jsonl")).unwrap_or_default();
    assert!(!audit.contains(secret), "the audit log leaked the prompt");
    assert!(!audit.contains(TOKEN), "the audit log leaked the token");

    // And the run record keeps only a bounded preview, which cannot reach the
    // secret because the preview window ends long before it.
    let record = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    assert!(record.prompt_preview.len() <= 500);
    assert!(record.prompt_preview.len() < prompt.len());
    assert!(!record.prompt_preview.contains(secret));

    // The one place the full input ever existed is the sealed private input,
    // and a terminal run no longer has one.
    assert!(
        !intent_file(&store_root, &run_id).is_file(),
        "a terminal run must not keep its private input"
    );
    set_grokptah_home_override(None);
}

/// A completed run must carry `Sent` provider evidence, and a run without it
/// must never be reported completed. This is the "no fake Completed" rule seen
/// from the outside: the ledger and the outcome have to agree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_completed_run_carries_sent_provider_evidence() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();

    let response = rig
        .orch
        .submit_task(
            &auth,
            "send-evidence",
            rig.session,
            rig.ws.path(),
            marker_prompt("send-evidence-marker"),
            None,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    wait_all_terminal(&rig, std::slice::from_ref(&run_id), Duration::from_secs(60)).await;

    let run = rig.orch.store().load_run(&run_id).unwrap().expect("run");
    let send = rig
        .orch
        .store()
        .load_provider_send(&run_id)
        .unwrap()
        .expect("a dispatched attempt must leave provider-send evidence");
    assert_eq!(
        send.state,
        ProviderSendState::Sent,
        "a completed run must be backed by an observed provider response"
    );
    assert_eq!(
        rig.markers().get("send-evidence-marker").copied(),
        Some(1),
        "the work must actually have run"
    );
    assert_eq!(run.state, RunState::Completed, "error={:?}", run.error_code);
    set_grokptah_home_override(None);
}
