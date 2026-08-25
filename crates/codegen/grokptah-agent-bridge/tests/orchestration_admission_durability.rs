//! Focused durability tests for the long-running-agent control plane.
//!
//! Two release-blocking gaps are covered here.
//!
//! **P0-A — accepted queued work must survive restart.** Admission used to be
//! split: a durable `RunRecord` in state `queued` holding only a redacted
//! preview, and the real execution input in a process-local `VecDeque`. The
//! client held a receipt saying the work was accepted; a restart destroyed the
//! input and the replay kept affirming success. These tests pin the durable
//! admission record as the single admission truth, the fsync-before-receipt
//! ordering, exact restart reconstruction, and every crash cut around it.
//!
//! **P0-B — `Running` must be verified and reaped.** There was no heartbeat,
//! no lease expiry and no reaper, and a failing finalization write pinned an
//! admission slot forever. These tests pin owner/attempt lease identity,
//! heartbeat denial, deterministic reaping, and bounded finalization that
//! preserves a replayable intent instead of claiming a success that never
//! happened.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    safe_id_filename, AdmissionRecord, AdmissionState, LeaseDenied, LeasePolicy, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, RunExecutionMode, RunRecord, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, SessionKind};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

/// A bearer long enough to be a real redaction target when it appears inside
/// a prompt, so "the private record keeps it, the public one does not" is an
/// assertion rather than a hope.
const BEARER: &str = "durability-control-bearer-8f21c0";

fn setup_home() -> (tempfile::TempDir, ProcessEnvGuard) {
    let mut guard = ProcessEnvGuard::new();
    let dir = tempdir().unwrap();
    let home = dir.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
    (dir, guard)
}

fn started_host() -> grokptah_agent_bridge::AgentHostHandle {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");
    host
}

fn service_on(
    host: &grokptah_agent_bridge::AgentHostHandle,
    store: OrchStore,
    ws: &Path,
    max_concurrent: usize,
) -> std::sync::Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: BEARER.into(),
            allowlist: WorkspaceAllowlist::new([ws.to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds::default(),
        },
    )
}

/// Reopen the ledger the way a restarted process would. The store root is
/// held under an exclusive advisory lock, so this also proves the previous
/// "process" released it.
async fn reopen_store(root: &Path) -> OrchStore {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match OrchStore::open(root) {
            Ok(store) => return store,
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "store must reopen after restart: {error}"
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

fn admission_file(root: &Path, run_id: &str) -> PathBuf {
    root.join("admissions")
        .join(format!("{}.json", safe_id_filename(run_id).unwrap()))
}

fn write_admission_file(root: &Path, record: &AdmissionRecord) {
    let path = admission_file(root, &record.run_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
}

fn sealed_admission(run_id: &str, session_id: Uuid, ws: &Path, prompt: &str) -> AdmissionRecord {
    let mut record = AdmissionRecord {
        run_id: run_id.into(),
        session_id,
        workspace: ws.display().to_string(),
        request_id: format!("req-{run_id}"),
        client_id: Some("mcp".into()),
        prompt: prompt.into(),
        bounds: RunBounds::default(),
        execution_mode: RunExecutionMode::Shared,
        parent_run_id: None,
        retry_of: None,
        sequence: 1,
        state: AdmissionState::Queued,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        integrity: String::new(),
    };
    record.seal();
    record
}

fn queued_run(run_id: &str, session_id: Uuid, ws: &Path, request_id: &str) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        session_id,
        workspace: ws.display().to_string(),
        request_id: request_id.into(),
        client_id: Some("mcp".into()),
        state: RunState::Queued,
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        queue_position: Some(1),
        bounds: RunBounds::default(),
        prompt_preview: "preview".into(),
        start_seq: None,
        end_seq: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

fn settle_receipt_complete(store: &OrchStore, request_id: &str, run_id: &str) {
    let payload_hash = "hash";
    store
        .claim_idempotency("ptah_submit_task", request_id, payload_hash)
        .expect("claim");
    store
        .complete_idempotency(
            "ptah_submit_task",
            request_id,
            payload_hash,
            Some(run_id.into()),
            json!({"runId": run_id, "state": "queued"}),
        )
        .expect("settle");
}

// ── P0-A: accepted queued work survives restart ────────────────────────

/// The headline regression: fill capacity, accept the full bounded queue,
/// restart, and require the exact private inputs, the exact arrival order,
/// the still-queued run records, and the receipts to all come back.
///
/// Capacity is held by a direct host reservation rather than a live turn, so
/// nothing is in flight and the assertion is about durability alone.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn queued_admissions_survive_restart_with_exact_private_inputs_and_order() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();

    let mut accepted: Vec<(String, String, Uuid, String)> = Vec::new();
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let mut sessions = Vec::new();
        for _ in 0..4 {
            let session = host.session_new_kind(SessionKind::Build).unwrap();
            host.session_set_cwd(session.id, ws.path()).unwrap();
            sessions.push(session.id);
        }
        // Hold the only capacity slot without starting a turn.
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();

        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

        for index in 0..32usize {
            let session_id = sessions[index % sessions.len()];
            let request_id = format!("durable-admit-{index}");
            // The bearer inside the prompt is the redaction probe: the private
            // record must keep it verbatim, the public preview must not.
            let prompt = format!("durable task {index} with token {BEARER}");
            let response = orch
                .submit_task_with_execution_mode_and_queue(
                    &auth,
                    &request_id,
                    session_id,
                    ws.path(),
                    prompt.clone(),
                    None,
                    RunExecutionMode::Shared,
                    true,
                )
                .await
                .expect("queued submission must be accepted");
            assert_eq!(response["state"], "queued", "task {index}");
            assert_eq!(
                response["queuedPosition"],
                json!(index + 1),
                "arrival order must be dense and monotonic"
            );
            accepted.push((
                response["runId"].as_str().unwrap().to_string(),
                request_id,
                session_id,
                prompt,
            ));
        }

        let capacity = orch.get_capacity(&auth).unwrap();
        assert_eq!(capacity["queuedRuns"], 32);
        assert_eq!(
            capacity["durableQueuedRuns"], 32,
            "every accepted task must already be durable, not merely in memory"
        );
        drop(orch);
        drop(host);
    }

    // ── restart ────────────────────────────────────────────────────────
    let store = reopen_store(&root).await;
    let recovered = store.take_recovered_admissions();
    assert_eq!(
        recovered.len(),
        32,
        "a restart must reconstruct the whole accepted queue"
    );
    for (index, record) in recovered.iter().enumerate() {
        let (run_id, request_id, session_id, prompt) = &accepted[index];
        assert_eq!(
            &record.run_id, run_id,
            "queue order must be exact at {index}"
        );
        assert_eq!(&record.request_id, request_id);
        assert_eq!(record.session_id, *session_id);
        assert_eq!(
            &record.prompt, prompt,
            "the complete private execution input must survive verbatim"
        );
        assert_eq!(record.execution_mode, RunExecutionMode::Shared);
        assert_eq!(record.state, AdmissionState::Queued);
        assert!(record.integrity_ok(), "integrity digest must verify");

        let run = store.load_run(run_id).unwrap().expect("run record");
        assert_eq!(
            run.state,
            RunState::Queued,
            "durably admitted work must not be silently destroyed by restart"
        );
        assert!(
            !run.prompt_preview.contains(BEARER),
            "the public run projection must stay redacted"
        );
        assert!(
            run.prompt_preview.len() <= 512,
            "the public run projection must stay bounded"
        );

        let receipt = store
            .load_idempotency(request_id)
            .unwrap()
            .expect("receipt must survive");
        assert_eq!(receipt.status, "complete");
        assert_eq!(receipt.response["state"], "queued");
        assert_eq!(
            receipt.response["runId"].as_str(),
            Some(run_id.as_str()),
            "the receipt must name the run so a client can reconcile by identity"
        );
    }
    assert!(
        recovered
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence),
        "durable admission order must be strictly increasing"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(admission_file(store.root(), &accepted[0].0))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "admission records are private store material");
    }

    assert!(
        store.take_recovered_admissions().is_empty(),
        "the recovered queue is adopted exactly once per ledger"
    );
}

/// Reopening the ledger repeatedly must be stable: recovery is a
/// reconstruction, never a consumption.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn repeated_recovery_is_stable() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();

    {
        let store = OrchStore::open(&root).unwrap();
        let run = queued_run("stable-run", session_id, ws.path(), "req-stable-run");
        store.save_run(&run).unwrap();
        let record = sealed_admission("stable-run", session_id, ws.path(), "stable input");
        store.save_admission(&record).unwrap();
        settle_receipt_complete(&store, "req-stable-run", "stable-run");
    }

    for round in 0..3 {
        let store = reopen_store(&root).await;
        let recovered = store.take_recovered_admissions();
        assert_eq!(recovered.len(), 1, "round {round}");
        assert_eq!(recovered[0].prompt, "stable input", "round {round}");
        assert_eq!(
            store.load_run("stable-run").unwrap().unwrap().state,
            RunState::Queued,
            "round {round}"
        );
    }
}

// ── P0-A: crash cuts ───────────────────────────────────────────────────

/// Cut before the durable input landed: a queued run with no admission record
/// is unrecoverable, so it fails closed instead of waiting forever.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn crash_cut_before_durable_input_fails_closed() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&queued_run("orphan", session_id, ws.path(), "req-orphan"))
            .unwrap();
        settle_receipt_complete(&store, "req-orphan", "orphan");
    }
    let store = reopen_store(&root).await;
    assert!(store.take_recovered_admissions().is_empty());
    let run = store.load_run("orphan").unwrap().unwrap();
    assert_eq!(run.state, RunState::Interrupted);
    assert_eq!(run.terminal_result.as_deref(), Some("interrupted"));
}

/// Cut between the durable input and the settled receipt. The client's
/// mutation is failed as orphaned, so running this work anyway would execute
/// a request its caller was told did not happen. Reconcile by request
/// identity and fail closed — never a blind resend.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn crash_cut_after_durable_input_before_receipt_reconciles_by_request_identity() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&queued_run("unacked", session_id, ws.path(), "req-unacked"))
            .unwrap();
        store
            .save_admission(&sealed_admission(
                "unacked",
                session_id,
                ws.path(),
                "never acknowledged",
            ))
            .unwrap();
        // Claim taken, never settled: exactly the crash window.
        store
            .claim_idempotency("ptah_submit_task", "req-unacked", "hash")
            .unwrap();
    }
    let store = reopen_store(&root).await;
    assert!(
        store.take_recovered_admissions().is_empty(),
        "unacknowledged admissions must not be re-dispatched"
    );
    assert_eq!(store.uncertain_admissions(), 1);
    assert_eq!(
        store.load_run("unacked").unwrap().unwrap().state,
        RunState::Interrupted
    );
    let receipt = store.load_idempotency("req-unacked").unwrap().unwrap();
    assert_eq!(receipt.status, "failed");
}

/// Cut after the receipt settled: the acceptance was honoured, so the work
/// must come back.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn crash_cut_after_receipt_preserves_queued_work() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&queued_run("acked", session_id, ws.path(), "req-acked"))
            .unwrap();
        store
            .save_admission(&sealed_admission(
                "acked",
                session_id,
                ws.path(),
                "acknowledged input",
            ))
            .unwrap();
        settle_receipt_complete(&store, "req-acked", "acked");
    }
    let store = reopen_store(&root).await;
    let recovered = store.take_recovered_admissions();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].prompt, "acknowledged input");
    assert_eq!(
        store.load_run("acked").unwrap().unwrap().state,
        RunState::Queued
    );
}

/// Cuts on either side of promotion. In both shapes the admission is already
/// consumed, so recovery must retire it and interrupt the run rather than
/// dispatch the same work a second time.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn crash_cut_during_promotion_never_dispatches_twice() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        // Cut A: admission consumed, run still queued.
        let mut consumed = sealed_admission("cut-a", session_id, ws.path(), "cut a");
        consumed.state = AdmissionState::Promoted;
        consumed.seal();
        store
            .save_run(&queued_run("cut-a", session_id, ws.path(), "req-cut-a"))
            .unwrap();
        settle_receipt_complete(&store, "req-cut-a", "cut-a");
        write_admission_file(store.root(), &consumed);

        // Cut B: admission consumed, run already running.
        let mut running = queued_run("cut-b", session_id, ws.path(), "req-cut-b");
        running.state = RunState::Running;
        running.queue_position = None;
        store.save_run(&running).unwrap();
        settle_receipt_complete(&store, "req-cut-b", "cut-b");
        let mut consumed_b = sealed_admission("cut-b", session_id, ws.path(), "cut b");
        consumed_b.state = AdmissionState::Promoted;
        consumed_b.seal();
        write_admission_file(store.root(), &consumed_b);
    }
    let store = reopen_store(&root).await;
    assert!(
        store.take_recovered_admissions().is_empty(),
        "a consumed admission is never re-queued"
    );
    for run_id in ["cut-a", "cut-b"] {
        assert_eq!(
            store.load_run(run_id).unwrap().unwrap().state,
            RunState::Interrupted,
            "{run_id}"
        );
        assert!(
            store.load_admission(run_id).unwrap().is_none(),
            "{run_id} admission must be retired"
        );
    }
}

/// Cut after cancellation: the terminal run record is the fence, and the
/// leftover admission must not resurrect the work.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn crash_cut_after_cancellation_cannot_resurrect_queued_work() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        let mut cancelled = queued_run("cancelled", session_id, ws.path(), "req-cancelled");
        cancelled.state = RunState::Cancelled;
        cancelled.terminal_result = Some("cancelled".into());
        cancelled.queue_position = None;
        store.save_run(&cancelled).unwrap();
        settle_receipt_complete(&store, "req-cancelled", "cancelled");
        store
            .save_admission(&sealed_admission(
                "cancelled",
                session_id,
                ws.path(),
                "cancelled input",
            ))
            .unwrap();
    }
    let store = reopen_store(&root).await;
    assert!(store.take_recovered_admissions().is_empty());
    assert!(store.load_admission("cancelled").unwrap().is_none());
    assert_eq!(
        store.load_run("cancelled").unwrap().unwrap().state,
        RunState::Cancelled,
        "a cancelled run must stay cancelled"
    );
}

/// A record whose bytes cannot be trusted is quarantined rather than executed
/// on the assumption that the missing bytes did not matter.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn tampered_admission_is_quarantined_and_fails_closed() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&queued_run(
                "tampered",
                session_id,
                ws.path(),
                "req-tampered",
            ))
            .unwrap();
        settle_receipt_complete(&store, "req-tampered", "tampered");
        let mut record = sealed_admission("tampered", session_id, ws.path(), "original input");
        // Rewrite the input after sealing: the digest no longer matches.
        record.prompt = "substituted input".into();
        write_admission_file(store.root(), &record);
    }
    let store = reopen_store(&root).await;
    assert!(store.take_recovered_admissions().is_empty());
    assert_eq!(store.admission_integrity_failures(), 1);
    assert_eq!(
        store.load_run("tampered").unwrap().unwrap().state,
        RunState::Interrupted
    );
    let quarantined = std::fs::read_dir(store.root().join("admissions"))
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("json.corrupt-")
        })
        .count();
    assert_eq!(quarantined, 1, "the evidence must be kept, not deleted");
}

// ── P0-A: promotion and cancellation are exactly-once ──────────────────

/// Promotion is a compare-and-set: the second attempt cannot dispatch the
/// same work, whatever raced it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn promotion_consumes_the_durable_record_exactly_once() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let store = OrchStore::open(&root).unwrap();
    store
        .save_run(&queued_run("promote-once", session_id, ws.path(), "req-p"))
        .unwrap();
    store
        .save_admission(&sealed_admission(
            "promote-once",
            session_id,
            ws.path(),
            "exactly once",
        ))
        .unwrap();

    let ttl = chrono::Duration::seconds(30);
    let (run, admission) = store
        .promote_admission("promote-once", "owner-a", 7, ttl)
        .expect("first promotion succeeds");
    assert_eq!(run.state, RunState::Running);
    assert_eq!(run.start_seq, Some(7));
    assert_eq!(run.queue_position, None);
    assert_eq!(admission.prompt, "exactly once");
    let lease = store.load_lease("promote-once").expect("lease installed");
    assert_eq!(lease.attempt, 1);
    assert!(lease.matches("owner-a", 1));

    let second = store.promote_admission("promote-once", "owner-b", 8, ttl);
    assert!(
        second.is_err(),
        "a consumed admission cannot be promoted again"
    );
    assert!(store.load_admission("promote-once").unwrap().is_none());
}

/// Cancelling queued work tombstones the durable record so promotion refuses
/// it, in this process and after any restart.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancellation_tombstones_durable_queued_work() {
    let (home, _guard) = setup_home();
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let blocker = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker.id, ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    host.reserve_orchestration_turn("capacity-blocker", blocker.id)
        .unwrap();

    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let accepted = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "cancel-durable",
            session.id,
            ws.path(),
            "queued work to cancel".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap().to_string();
    assert!(store.load_admission(&run_id).unwrap().is_some());

    let cancelled = orch
        .cancel(
            &auth,
            "cancel-durable-request",
            session.id,
            ws.path(),
            Some(&run_id),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["wasQueued"], true);
    assert!(
        store.load_admission(&run_id).unwrap().is_none(),
        "cancellation must retire the durable record"
    );
    assert_eq!(
        store.load_run(&run_id).unwrap().unwrap().state,
        RunState::Cancelled
    );
    assert_eq!(store.queued_admission_count(), 0);
    assert!(
        store
            .promote_admission(&run_id, "owner", 1, chrono::Duration::seconds(5))
            .is_err(),
        "tombstoned work must never be promotable"
    );
}

// ── P0-B: Running is verified and reaped ───────────────────────────────

/// A heartbeat may only ever extend the exact live attempt.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn heartbeat_denies_stale_wrong_owner_and_terminal_attempts() {
    let (home, _guard) = setup_home();
    let ws = tempdir().unwrap();
    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let session_id = Uuid::new_v4();
    let mut run = queued_run("leased", session_id, ws.path(), "req-leased");
    run.state = RunState::Running;
    store.save_run(&run).unwrap();

    let ttl = chrono::Duration::seconds(30);
    let lease = store
        .install_lease("leased", session_id, "owner-a", ttl)
        .expect("lease");
    assert_eq!(lease.attempt, 1);

    assert!(store.heartbeat_run("leased", "owner-a", 1, ttl).is_ok());
    assert_eq!(
        store
            .heartbeat_run("leased", "owner-b", 1, ttl)
            .unwrap_err(),
        LeaseDenied::WrongOwner
    );
    assert_eq!(
        store
            .heartbeat_run("leased", "owner-a", 2, ttl)
            .unwrap_err(),
        LeaseDenied::WrongOwner,
        "a stale attempt number must not adopt the live lease"
    );
    assert_eq!(
        store
            .heartbeat_run("missing", "owner-a", 1, ttl)
            .unwrap_err(),
        LeaseDenied::UnknownRun
    );

    // A superseding attempt takes ownership; the old one is locked out.
    let next = store
        .install_lease("leased", session_id, "owner-c", ttl)
        .unwrap();
    assert_eq!(next.attempt, 2);
    assert_eq!(
        store
            .heartbeat_run("leased", "owner-a", 1, ttl)
            .unwrap_err(),
        LeaseDenied::WrongOwner
    );
    assert!(
        !store.release_lease("leased", "owner-a", 1),
        "a stale attempt cannot release a newer lease"
    );

    // Terminal wins over everything.
    store
        .update_run("leased", |current| {
            current.state = RunState::Completed;
            current.terminal_result = Some("completed".into());
            Ok(())
        })
        .unwrap();
    assert_eq!(
        store
            .heartbeat_run("leased", "owner-c", 2, ttl)
            .unwrap_err(),
        LeaseDenied::Terminal,
        "a heartbeat must never revive a terminal run"
    );
}

/// Deterministic reaping: an expired attempt becomes `lost_worker`, a live
/// one is untouched, and the capacity it held is released.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn expired_lease_is_reaped_and_releases_capacity() {
    let (home, _guard) = setup_home();
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 2);
    orch.set_lease_policy(LeasePolicy {
        heartbeat: Duration::from_millis(50),
        ttl: Duration::from_millis(60),
        sweep: Duration::from_secs(3_600),
    });
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

    // A run whose worker is gone: Running, holding capacity, lease expired.
    let mut lost = queued_run("lost", session.id, ws.path(), "req-lost");
    lost.state = RunState::Running;
    lost.queue_position = None;
    store.save_run(&lost).unwrap();
    host.reserve_orchestration_turn("lost", session.id).unwrap();
    store
        .install_lease(
            "lost",
            session.id,
            "dead-owner",
            chrono::Duration::milliseconds(1),
        )
        .unwrap();

    // A healthy run must survive the same sweep untouched.
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();
    let mut live = queued_run("live", other.id, ws.path(), "req-live");
    live.state = RunState::Running;
    live.queue_position = None;
    store.save_run(&live).unwrap();
    store
        .install_lease(
            "live",
            other.id,
            "live-owner",
            chrono::Duration::seconds(120),
        )
        .unwrap();

    assert_eq!(orch.get_capacity(&auth).unwrap()["activeRuns"], 1);
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert_eq!(orch.reap_stale_runs(), 1, "exactly the expired attempt");

    let reaped = store.load_run("lost").unwrap().unwrap();
    assert_eq!(reaped.state, RunState::Interrupted);
    assert_eq!(reaped.error_code.as_deref(), Some("lost_worker"));
    assert!(store.load_lease("lost").is_none());

    let survivor = store.load_run("live").unwrap().unwrap();
    assert_eq!(
        survivor.state,
        RunState::Running,
        "a live attempt must never be reaped"
    );

    let capacity = orch.get_capacity(&auth).unwrap();
    assert_eq!(capacity["activeRuns"], 0, "capacity must come back");
    assert_eq!(capacity["health"]["reapedRuns"], 1);
    assert_eq!(orch.reap_stale_runs(), 0, "reaping is idempotent");
}

/// Every attempt lease dies with the process that owned it: a lease found at
/// open can never belong to a live attempt, so restart resolves `Running` to
/// `interrupted` and clears the lease rather than trusting it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn restart_retires_every_attempt_lease() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        let mut run = queued_run("in-flight", session_id, ws.path(), "req-in-flight");
        run.state = RunState::Running;
        store.save_run(&run).unwrap();
        store
            .install_lease(
                "in-flight",
                session_id,
                "previous-process",
                chrono::Duration::hours(1),
            )
            .unwrap();
    }
    let store = reopen_store(&root).await;
    assert!(
        store.load_lease("in-flight").is_none(),
        "a lease from a dead process must not look live"
    );
    assert_eq!(
        store.load_run("in-flight").unwrap().unwrap().state,
        RunState::Interrupted
    );
}

/// Bounded finalization: when the terminal write cannot land, the candidate
/// is preserved as a replayable intent and the next open installs it. Nothing
/// ever claims the finalization succeeded.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn finalization_failure_preserves_replay_intent_without_claiming_success() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let run_id = "stuck-finalization";
    {
        let store = OrchStore::open(&root).unwrap();
        let mut run = queued_run(run_id, session_id, ws.path(), "req-stuck");
        run.state = RunState::Running;
        store.save_run(&run).unwrap();

        let mut terminal = run.clone();
        terminal.state = RunState::Completed;
        terminal.terminal_result = Some("completed".into());
        terminal.final_response = Some("done".into());

        // Break the runs directory the way a full disk or a removed store
        // root would: the terminal write cannot land.
        std::fs::remove_dir_all(root.join("runs")).unwrap();
        std::fs::write(root.join("runs"), b"not a directory").unwrap();
        assert!(
            store.persist_finalization(&terminal).is_err(),
            "the terminal write must genuinely fail"
        );

        std::fs::remove_file(root.join("runs")).unwrap();
        std::fs::create_dir_all(root.join("runs")).unwrap();
        store.save_run(&run).unwrap();
        std::fs::remove_dir_all(root.join("runs")).unwrap();
        std::fs::write(root.join("runs"), b"not a directory").unwrap();

        store.write_finalization_intent(&terminal).unwrap();
        assert_eq!(store.pending_finalization_intents(), 1);
        assert_eq!(store.note_stuck_finalization(), 1);
        assert_eq!(store.stuck_finalizations(), 1);

        std::fs::remove_file(root.join("runs")).unwrap();
        std::fs::create_dir_all(root.join("runs")).unwrap();
        store.save_run(&run).unwrap();
        assert_eq!(
            store.load_run(run_id).unwrap().unwrap().state,
            RunState::Running,
            "the run must not be reported terminal while the write is unresolved"
        );
    }

    let store = reopen_store(&root).await;
    let replayed = store.load_run(run_id).unwrap().unwrap();
    assert_eq!(
        replayed.state,
        RunState::Completed,
        "the preserved intent must be replayed at open"
    );
    assert_eq!(replayed.final_response.as_deref(), Some("done"));
    assert_eq!(store.pending_finalization_intents(), 0);
}

/// End to end: a run whose finalization write keeps failing must give its
/// admission slot back within the bounded retry instead of pinning capacity
/// at 1 Hz forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn finalization_failure_releases_admission_capacity() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    let store = OrchStore::open(&root).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

    let accepted = orch
        .submit_task(
            &auth,
            "stuck-capacity",
            session.id,
            ws.path(),
            "run sleep 1".into(),
            Some(json!({"maxDurationMs": 20000})),
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap().to_string();
    assert_eq!(orch.get_capacity(&auth).unwrap()["available"], 0);

    // Break the terminal write while the turn is still in flight.
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::remove_dir_all(root.join("runs")).unwrap();
    std::fs::write(root.join("runs"), b"not a directory").unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let capacity = orch.get_capacity(&auth).unwrap();
        if capacity["available"] == json!(1) {
            assert_eq!(
                capacity["health"]["stuckFinalizations"],
                json!(1),
                "the stuck finalization must be visible, not silent"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a failing finalization must not pin admission capacity: {capacity}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The intent is preserved for replay rather than reported as success.
    std::fs::remove_file(root.join("runs")).unwrap();
    std::fs::create_dir_all(root.join("runs")).unwrap();
    assert!(
        store.pending_finalization_intents() >= 1,
        "the terminal candidate must survive as a replayable intent"
    );
    let _ = run_id;
    drop(orch);
}

// ── boundaries the repair must not blur ────────────────────────────────

/// The per-session durable prompt queue and the host-global admission queue
/// are different mechanisms with different durability. Restart must exercise
/// both, so a passing prompt-queue test can never stand in for admission
/// behaviour.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn durable_prompt_queue_does_not_mask_the_admission_queue() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id;
    let run_id;
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        session_id = session.id;
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();

        // A steering prompt on the per-session queue.
        host.session_queue_add(session_id, "session queue entry".into(), false)
            .unwrap();

        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        let accepted = orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                "distinct-queues",
                session_id,
                ws.path(),
                "admission queue entry".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        run_id = accepted["runId"].as_str().unwrap().to_string();
        drop(orch);
        drop(host);
    }

    let store = reopen_store(&root).await;
    let recovered = store.take_recovered_admissions();
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].prompt, "admission queue entry",
        "the admission queue must carry its own durable input"
    );
    assert_eq!(recovered[0].run_id, run_id);
    assert_eq!(
        store.load_run(&run_id).unwrap().unwrap().state,
        RunState::Queued
    );

    // The per-session durable prompt queue is restored by the host, from a
    // different store, with different contents.
    let restarted = started_host();
    let entries = restarted.session_queue_list(session_id).unwrap();
    assert_eq!(entries.len(), 1, "the session prompt queue is unaffected");
    assert!(
        entries
            .iter()
            .all(|entry| entry.text != "admission queue entry"),
        "the two queues must stay distinct: neither can stand in for the other"
    );
    assert_eq!(entries[0].text, "session queue entry");
    drop(restarted);
}

/// Nothing derived from the private admission records may leak into a public
/// projection, and the new health metadata stays bounded.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn public_projections_stay_redacted_and_bounded() {
    let (home, _guard) = setup_home();
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let blocker = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker.id, ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    host.reserve_orchestration_turn("capacity-blocker", blocker.id)
        .unwrap();

    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let secret_prompt = format!("{} classified body {}", BEARER, "x".repeat(4_000));
    let accepted = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "redaction-probe",
            session.id,
            ws.path(),
            secret_prompt.clone(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap();

    // Private side keeps the complete input under the store's own authority.
    let record = store.load_admission(run_id).unwrap().unwrap();
    assert_eq!(record.prompt, secret_prompt);

    // Public side stays redacted and bounded.
    let run = orch.get_run(&auth, run_id).unwrap();
    let run_text = serde_json::to_string(&run).unwrap();
    assert!(!run_text.contains(BEARER), "bearer must be redacted");
    assert!(
        !run_text.contains(&"x".repeat(200)),
        "the run projection must not carry the execution input"
    );
    assert!(run["promptPreview"].as_str().unwrap().len() <= 512);

    let capacity = orch.get_capacity(&auth).unwrap();
    let capacity_text = serde_json::to_string(&capacity).unwrap();
    assert!(!capacity_text.contains(BEARER));
    assert!(!capacity_text.contains("classified body"));
    assert_eq!(capacity["durableQueuedRuns"], 1);
    for key in [
        "stuckFinalizations",
        "pendingFinalizationIntents",
        "reapedRuns",
        "recoveredAdmissions",
        "uncertainAdmissions",
        "admissionIntegrityFailures",
    ] {
        assert!(
            capacity["health"][key].is_number(),
            "{key} must be a bounded count"
        );
    }

    let receipt = store.load_idempotency("redaction-probe").unwrap().unwrap();
    let receipt_text = serde_json::to_string(&receipt).unwrap();
    assert!(!receipt_text.contains(BEARER));
    assert!(!receipt_text.contains("classified body"));
}

/// One ledger, two embedded control services: the recovered queue is adopted
/// once, so a restart can never fan a single accepted task out to two
/// supervisors.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn second_supervisor_on_the_same_ledger_adopts_nothing() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let mut run_ids = Vec::new();
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();
        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        for index in 0..3 {
            let accepted = orch
                .submit_task_with_execution_mode_and_queue(
                    &auth,
                    &format!("adopt-{index}"),
                    session.id,
                    ws.path(),
                    format!("adopted task {index}"),
                    None,
                    RunExecutionMode::Shared,
                    true,
                )
                .await
                .unwrap();
            run_ids.push(accepted["runId"].as_str().unwrap().to_string());
        }
        drop(orch);
        drop(host);
    }

    let host = started_host();
    let blocker = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker.id, ws.path()).unwrap();
    host.reserve_orchestration_turn("capacity-blocker", blocker.id)
        .unwrap();
    let store = reopen_store(&root).await;
    let first = service_on(&host, store.clone(), ws.path(), 1);
    let auth = first
        .auth_header(Some(&format!("Bearer {BEARER}")))
        .unwrap();
    assert_eq!(
        first.get_capacity(&auth).unwrap()["queuedRuns"],
        3,
        "the first supervisor adopts the recovered queue"
    );
    for (index, run_id) in run_ids.iter().enumerate() {
        assert_eq!(
            first.get_run(&auth, run_id).unwrap()["queuePosition"],
            json!(index + 1),
            "arrival order must be reproduced exactly"
        );
    }

    let second = service_on(&host, store.clone(), ws.path(), 1);
    let second_auth = second
        .auth_header(Some(&format!("Bearer {BEARER}")))
        .unwrap();
    assert_eq!(
        second.get_capacity(&second_auth).unwrap()["queuedRuns"],
        3,
        "a second supervisor must not double-admit the same work"
    );
    assert_eq!(store.queued_admission_count(), 3);
    drop(second);
    drop(first);
}

// ── durable-write failure must never settle a receipt ──────────────────

/// Replace the admissions directory with a regular file so every create,
/// write and fsync under it returns a real `io::Error` — the same boundary
/// ENOSPC, a short write, or a failed fsync would fail at.
fn break_admissions_dir(root: &Path) {
    let dir = root.join("admissions");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, b"not a directory").unwrap();
}

fn repair_admissions_dir(root: &Path) {
    let dir = root.join("admissions");
    std::fs::remove_file(&dir).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
}

fn admission_files(root: &Path) -> usize {
    std::fs::read_dir(root.join("admissions"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
                .count()
        })
        .unwrap_or(0)
}

/// The core honesty invariant: if the executable input cannot be made
/// durable, the submission fails and its receipt settles `failed`. A later
/// retry replays that failure — it can never turn into a success.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_write_failure_never_settles_the_receipt() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let blocker = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker.id, ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    host.reserve_orchestration_turn("capacity-blocker", blocker.id)
        .unwrap();

    let store = OrchStore::open(&root).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

    break_admissions_dir(&root);
    let refused = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "enospc-1",
            session.id,
            ws.path(),
            "work that cannot be made durable".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await;
    let error = refused.expect_err("a submission that cannot persist must not report accepted");

    let receipt = store
        .load_idempotency("enospc-1")
        .unwrap()
        .expect("the claim must still be settled, not left pending");
    assert_eq!(
        receipt.status, "failed",
        "a receipt must never say complete for work the ledger could not persist"
    );
    assert_ne!(receipt.response["state"], "queued");

    let run_id = receipt
        .run_id
        .clone()
        .expect("the failed receipt must still name the run it created");
    let run = store.load_run(&run_id).unwrap().expect("run record");
    assert!(
        run.state.is_terminal(),
        "a run whose admission could not persist must fail closed, got {:?}",
        run.state
    );
    let _ = &error;
    assert_eq!(orch.get_capacity(&auth).unwrap()["queuedRuns"], 0);

    // Even with storage healthy again, the same request replays its failure.
    repair_admissions_dir(&root);
    let replayed = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "enospc-1",
            session.id,
            ws.path(),
            "work that cannot be made durable".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await;
    assert!(
        replayed.is_err(),
        "a settled failure must replay as a failure, never as a queued success"
    );
    assert_eq!(admission_files(&root), 0, "no partial record may survive");
}

/// A half-written record is removed rather than left for recovery to reason
/// about, so a failed submission leaves nothing promotable behind.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn failed_admission_write_leaves_no_promotable_record() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&queued_run("partial", session_id, ws.path(), "req-partial"))
            .unwrap();
        break_admissions_dir(&root);
        let refused = store.save_admission(&sealed_admission(
            "partial",
            session_id,
            ws.path(),
            "unpersistable",
        ));
        assert!(refused.is_err(), "the durable write must genuinely fail");
        repair_admissions_dir(&root);
        assert_eq!(admission_files(&root), 0);
    }
    let store = reopen_store(&root).await;
    assert!(store.take_recovered_admissions().is_empty());
    let run = store.load_run("partial").unwrap().unwrap();
    assert_eq!(run.state, RunState::Interrupted);
    assert_eq!(
        run.error_code.as_deref(),
        Some("admission_lost"),
        "a queued run with no durable input must be distinguishable from a plain restart"
    );
}

/// A settled `queued` receipt must not keep affirming work whose executable
/// input is gone. The replay fails closed and names the run, so the caller
/// reconciles by identity instead of waiting forever.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn settled_queued_receipt_fails_closed_when_its_work_was_lost() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let session_id;
    let run_id;
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        session_id = session.id;
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();
        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        let accepted = orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                "lost-1",
                session_id,
                ws.path(),
                "work whose input will be lost".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        assert_eq!(accepted["state"], "queued");
        run_id = accepted["runId"].as_str().unwrap().to_string();
        drop(orch);
        drop(host);
        // The durable input disappears while the process is down.
        std::fs::remove_file(admission_file(&root, &run_id)).unwrap();
    }

    let store = reopen_store(&root).await;
    let lost = store.load_run(&run_id).unwrap().unwrap();
    assert_eq!(lost.state, RunState::Interrupted);
    assert_eq!(lost.error_code.as_deref(), Some("admission_lost"));

    let host = started_host();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let replayed = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "lost-1",
            session_id,
            ws.path(),
            "work whose input will be lost".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await;
    let error =
        replayed.expect_err("a queued receipt whose work was lost must not replay as success");
    assert!(
        error.message.contains(&run_id),
        "the refusal must name the run so the caller can reconcile: {}",
        error.message
    );
}

/// One request id, one run, one durable record — however many times it is
/// retried.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duplicate_submit_is_exactly_once() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let blocker = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker.id, ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    host.reserve_orchestration_turn("capacity-blocker", blocker.id)
        .unwrap();

    let store = OrchStore::open(&root).unwrap();
    let orch = service_on(&host, store.clone(), ws.path(), 1);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

    let mut seen = Vec::new();
    for _ in 0..3 {
        let accepted = orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                "exactly-once",
                session.id,
                ws.path(),
                "retried admission".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        assert_eq!(accepted["state"], "queued");
        seen.push(accepted["runId"].as_str().unwrap().to_string());
    }
    assert!(
        seen.windows(2).all(|pair| pair[0] == pair[1]),
        "a repeated request id must resolve to one run: {seen:?}"
    );
    assert_eq!(admission_files(&root), 1, "exactly one durable record");
    assert_eq!(orch.get_capacity(&auth).unwrap()["queuedRuns"], 1);
    assert_eq!(store.list_runs().unwrap().len(), 1);
}

/// Cancelling queued work retires it durably: a restart neither recovers it
/// nor re-admits it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancel_then_restart_never_resurrects() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let run_id;
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();
        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        let accepted = orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                "cancel-restart",
                session.id,
                ws.path(),
                "run sh -c 'echo resurrected >> resurrection.txt'".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        run_id = accepted["runId"].as_str().unwrap().to_string();
        orch.cancel(
            &auth,
            "cancel-restart-request",
            session.id,
            ws.path(),
            Some(&run_id),
        )
        .await
        .unwrap();
        drop(orch);
        drop(host);
    }

    let store = reopen_store(&root).await;
    assert!(
        store.take_recovered_admissions().is_empty(),
        "cancelled work must not come back"
    );
    assert_eq!(
        store.load_run(&run_id).unwrap().unwrap().state,
        RunState::Cancelled
    );

    let host = started_host();
    let orch = service_on(&host, store.clone(), ws.path(), 4);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(orch.get_capacity(&auth).unwrap()["queuedRuns"], 0);
    assert_eq!(
        store.load_run(&run_id).unwrap().unwrap().state,
        RunState::Cancelled,
        "a full capacity window must not restart cancelled work"
    );
    assert!(
        !ws.path().join("resurrection.txt").exists(),
        "cancelled work must never execute"
    );
}

/// Restart the ledger repeatedly, then let the queue drain: the work runs
/// exactly once and nothing is left Running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn repeated_restart_yields_exactly_one_execution() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orch");
    let ws = tempdir().unwrap();
    let log = ws.path().join("exec_log.txt");
    let run_id;
    {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let blocker = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(blocker.id, ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.reserve_orchestration_turn("capacity-blocker", blocker.id)
            .unwrap();
        let orch = service_on(&host, OrchStore::open(&root).unwrap(), ws.path(), 1);
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        let accepted = orch
            .submit_task_with_execution_mode_and_queue(
                &auth,
                "replay-once",
                session.id,
                ws.path(),
                "run sh -c 'echo tick >> exec_log.txt'".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        run_id = accepted["runId"].as_str().unwrap().to_string();
        drop(orch);
        drop(host);
    }

    // Three restarts with nobody draining the queue.
    for round in 0..3 {
        let store = reopen_store(&root).await;
        let recovered = store.take_recovered_admissions();
        assert_eq!(recovered.len(), 1, "round {round}");
        assert_eq!(recovered[0].run_id, run_id, "round {round}");
        assert_eq!(
            store.load_run(&run_id).unwrap().unwrap().state,
            RunState::Queued,
            "round {round}"
        );
        assert!(!log.exists(), "nothing may execute while nothing drains");
        drop(store);
    }

    // Fourth restart: this one drains.
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let store = reopen_store(&root).await;
    let orch = service_on(&host, store.clone(), ws.path(), 2);
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let run = store.load_run(&run_id).unwrap().unwrap();
        if run.state.is_terminal() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "recovered work must reach a terminal state, stuck in {:?}",
            run.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let executions = std::fs::read_to_string(&log).unwrap_or_default();
    let ticks = executions
        .lines()
        .filter(|line| line.trim() == "tick")
        .count();
    assert_eq!(
        ticks, 1,
        "four restarts must still produce exactly one execution, got {executions:?}"
    );
    assert!(
        store.load_admission(&run_id).unwrap().is_none(),
        "the durable record must be consumed exactly once"
    );
    let runs = store.list_runs().unwrap();
    assert_eq!(runs.len(), 1, "no duplicate run records");
    assert!(
        runs.iter().all(|run| run.state != RunState::Running),
        "no run may be left Running"
    );
    assert_eq!(orch.get_capacity(&auth).unwrap()["queuedRuns"], 0);
}
