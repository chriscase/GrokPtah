//! Port of the reviewed PR 584 adversarial battery onto the repaired APIs.
//!
//! Old-to-new mapping:
//! - `concurrent_bindings_must_not_publish_foreign_work_in_the_other_receipt`
//!   used `arm_work_request` / `disarm_work_request` on one shared slot. It now
//!   installs `OrchStore::bind_work_request` on each thread and rendezvous on
//!   `set_work_request_test_gate` after both installs and before A's mutation.
//!   B still performs no mutation. The assertions still require B's receipt not
//!   to become complete, and now also require A's own receipt to carry A's work.
//! - `production_claim_crash_child` is still the no-op helper when
//!   `REVIEW584_CHILD_ROOT` is unset, and still the real
//!   `OrchestrationService::claim_work` caller. `REVIEW584_OP` / `REVIEW584_STAGE`
//!   extend it to renew, link, and the before-intent write fault.
//! - `production_claim_recovery_must_complete_the_original_receipt` still crashes
//!   the production claim at `lifecycle_after_item`. It now replays through a
//!   fresh authenticated service.
//! - `failed_quarantine_marker_must_not_reopen_admission` keeps the marker-write
//!   fault. The old `assert!(!path.exists())` recorded the bug (the journal
//!   moved before the marker). The port requires the original bytes to remain
//!   until the marker is durable, then to survive in quarantine across reopens.
//! - `maximal_valid_attempt_result_must_be_readable_through_detail` keeps the
//!   production create/claim/fail fixture and the full-view size assertion.
//!   The read now walks attempt summaries plus `attempt_evidence` and
//!   `attempt_artifacts` under the attempt content fence. The old single
//!   `project_work_detail(..., Attempts, 0, 1)` call could not succeed while
//!   one attempt view was larger than the transport ceiling.
//! - `full_item_and_edge_ceiling_graph_walk_is_lossless` is unchanged.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use grokptah_agent_bridge::orchestration::{
    attempt_content_fence, canonical_workspace, order_work, project_work_detail, safe_id_filename,
    select_work_graph_page, tools_call_response_len, AttemptState, AuthContext, AuthCredential,
    GraphScope, IdempotencyScope, ManagedExecutionIntent, ManagedFinalizationOutcome,
    ManagedFinalizationRecord, ManagedIntentState, ManagedRetryCause, OrchErrorCode, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState, WorkArtifactRef,
    WorkAttemptView, WorkDependency, WorkDetailPageV1, WorkDetailSection, WorkItem, WorkPageFilter,
    WorkPageKind, WorkPageQuery, WorkPageReader, WorkPolicy, WorkRequestBinding, WorkResult,
    WorkState, WorkspaceAllowlist, ATTEMPT_CONSISTENCY, MANAGED_EXECUTION_SCHEMA_VERSION,
    MANAGED_FINALIZATION_SCHEMA_VERSION, MAX_GRAPH_EDGES, MAX_GRAPH_SCOPE_ITEMS,
    MAX_WORK_ARTIFACTS, MAX_WORK_ARTIFACT_BYTES, WORK_PAGE_TRANSPORT_CEILING_BYTES,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, DurableWriteFault, HostConfig,
    HostRuntime, SessionKind, DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const BEARER: &str = "synthetic-review-token";
const READER_BEARER: &str = "synthetic-reader-token";

struct GateGuard;

impl Drop for GateGuard {
    fn drop(&mut self) {
        OrchStore::set_work_request_test_gate(None);
    }
}

fn arm_gate(parties: usize) -> GateGuard {
    OrchStore::set_work_request_test_gate(Some(Arc::new(Barrier::new(parties))));
    GateGuard
}

struct HomeGuard;

impl Drop for HomeGuard {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        OrchStore::set_work_request_test_gate(None);
    }
}

fn boot_service(
    workspaces: &[PathBuf],
) -> (
    ProcessEnvGuard,
    TempDir,
    HostRuntime,
    Arc<OrchestrationService>,
    OrchStore,
) {
    let mut env = ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let home = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: BEARER.into(),
            allowlist: WorkspaceAllowlist::new(workspaces.iter().cloned()),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    (env, home, host, orch, store)
}

fn save_objective(store: &OrchStore, session: Uuid, workspace: &Path, objective: &str) -> WorkItem {
    let item = WorkItem::new(
        "test",
        objective,
        session,
        workspace.display().to_string(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    item
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(future)
}

#[test]
fn concurrent_bindings_must_not_publish_foreign_work_in_the_other_receipt() {
    let _gate = arm_gate(2);
    let root = tempfile::tempdir().unwrap();
    let ws_a = tempfile::tempdir().unwrap();
    let ws_b = tempfile::tempdir().unwrap();
    let store = OrchStore::open(root.path()).unwrap();
    let a = WorkItem::new(
        "test",
        "PRIVATE_OBJECTIVE_A",
        Uuid::new_v4(),
        ws_a.path().display().to_string(),
        "owner-a",
        WorkPolicy::default(),
    )
    .unwrap();
    let b = WorkItem::new(
        "test",
        "PRIVATE_OBJECTIVE_B",
        Uuid::new_v4(),
        ws_b.path().display().to_string(),
        "owner-b",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&a).unwrap();
    store.save_work_item(&b).unwrap();
    let scope_a = IdempotencyScope::new("owner-a", a.session_id, ws_a.path()).unwrap();
    let scope_b = IdempotencyScope::new("owner-b", b.session_id, ws_b.path()).unwrap();
    store
        .claim_idempotency(&scope_a, "ptah_cancel_work", "request-a", "hash-a")
        .unwrap();
    store
        .claim_idempotency(&scope_b, "ptah_cancel_work", "request-b", "hash-b")
        .unwrap();
    let binding_a = WorkRequestBinding::bound(
        &scope_a,
        a.workspace.clone(),
        "request-a",
        "ptah_cancel_work",
        "hash-a",
        "work_scope",
    );
    let binding_b = WorkRequestBinding::bound(
        &scope_b,
        b.workspace.clone(),
        "request-b",
        "ptah_cancel_work",
        "hash-b",
        "work_scope",
    );
    let store_a = store.clone();
    let a_id = a.work_id.clone();
    let worker_a = std::thread::spawn(move || {
        let _bound = OrchStore::bind_work_request(binding_a).unwrap();
        store_a.cancel_work(&a_id, "cancel A").unwrap();
    });
    let _bound_b = OrchStore::bind_work_request(binding_b).unwrap();
    worker_a.join().unwrap();
    let receipt_b = store
        .load_idempotency(&scope_b, "request-b")
        .unwrap()
        .unwrap();
    let receipt_a = store
        .load_idempotency(&scope_a, "request-a")
        .unwrap()
        .unwrap();
    println!(
        "A receipt status: {}; B status: {}; A workId: {}; B workId: {}; B objective: {}",
        receipt_a.status,
        receipt_b.status,
        receipt_a.response["work"]["workId"],
        receipt_b.response["work"]["workId"],
        receipt_b.response["work"]["objective"]
    );
    assert_eq!(receipt_a.status, "complete");
    assert_eq!(receipt_a.response["work"]["workId"], a.work_id);
    assert_eq!(
        receipt_a.response["work"]["objective"],
        "PRIVATE_OBJECTIVE_A"
    );
    assert!(!receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_B"));
    assert_ne!(
        receipt_b.status, "complete",
        "B has not performed a mutation, yet A committed its work under B's scope"
    );
    assert!(!receipt_b
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_A"));
    assert_eq!(
        store.load_work_item(&b.work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
}

#[test]
fn same_lane_service_requests_keep_their_own_receipts() {
    let _home = HomeGuard;
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace = canonical_workspace(workspace_dir.path()).unwrap();
    let (_env, _home_dir, _host, orch, store) = boot_service(std::slice::from_ref(&workspace));
    let session = _host.session_new_kind(SessionKind::Build).unwrap();
    _host.session_set_cwd(session.id, &workspace).unwrap();
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let a = save_objective(&store, session.id, &workspace, "PRIVATE_OBJECTIVE_A");
    let b = save_objective(&store, session.id, &workspace, "PRIVATE_OBJECTIVE_B");
    let _gate = arm_gate(2);
    let orch_a = orch.clone();
    let auth_a = auth.clone();
    let workspace_a = workspace.clone();
    let a_id = a.work_id.clone();
    let worker = std::thread::spawn(move || {
        block_on(orch_a.cancel_work(
            &auth_a,
            "request-a",
            session.id,
            &workspace_a,
            &a_id,
            "cancel A".into(),
            None,
        ))
        .expect("A cancel")
    });
    let response_b = block_on(orch.cancel_work(
        &auth,
        "request-b",
        session.id,
        &workspace,
        &b.work_id,
        "cancel B".into(),
        None,
    ))
    .expect("B cancel");
    let response_a = worker.join().unwrap();
    let scope = IdempotencyScope::new(&auth.owner_id, session.id, &workspace).unwrap();
    let receipt_a = store
        .load_idempotency(&scope, "request-a")
        .unwrap()
        .unwrap();
    let receipt_b = store
        .load_idempotency(&scope, "request-b")
        .unwrap()
        .unwrap();
    assert_eq!(response_a["work"]["workId"], a.work_id);
    assert_eq!(response_b["work"]["workId"], b.work_id);
    assert_eq!(receipt_a.status, "complete");
    assert_eq!(receipt_b.status, "complete");
    assert_eq!(receipt_a.response["work"]["workId"], a.work_id);
    assert_eq!(receipt_b.response["work"]["workId"], b.work_id);
    assert!(receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_A"));
    assert!(!receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_B"));
    assert!(receipt_b
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_B"));
    assert!(!receipt_b
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_A"));
    assert_eq!(
        store.load_work_item(&a.work_id).unwrap().unwrap().state,
        WorkState::Cancelled
    );
    assert_eq!(
        store.load_work_item(&b.work_id).unwrap().unwrap().state,
        WorkState::Cancelled
    );
}

#[test]
fn different_lane_service_requests_keep_their_own_receipts() {
    let _home = HomeGuard;
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let workspace_a = canonical_workspace(dir_a.path()).unwrap();
    let workspace_b = canonical_workspace(dir_b.path()).unwrap();
    let (_env, _home_dir, host, orch, store) =
        boot_service(&[workspace_a.clone(), workspace_b.clone()]);
    // The host maps every credential to one agent owner. Different owners are
    // covered by the store test. This service race uses two sessions, two
    // workspaces, and two token ids.
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", BEARER).unwrap(),
        AuthCredential::new("reader", READER_BEARER).unwrap(),
    ])
    .unwrap();
    let session_a = host.session_new_kind(SessionKind::Build).unwrap();
    let session_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session_a.id, &workspace_a).unwrap();
    host.session_set_cwd(session_b.id, &workspace_b).unwrap();
    let auth_a = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let auth_b = orch
        .auth_header(Some(&format!("Bearer {READER_BEARER}")))
        .unwrap();
    assert_ne!(auth_a.token_id, auth_b.token_id);
    let a = save_objective(&store, session_a.id, &workspace_a, "PRIVATE_OBJECTIVE_A");
    let b = save_objective(&store, session_b.id, &workspace_b, "PRIVATE_OBJECTIVE_B");
    let _gate = arm_gate(2);
    let orch_a = orch.clone();
    let auth_a_thread = auth_a.clone();
    let workspace_a_thread = workspace_a.clone();
    let a_id = a.work_id.clone();
    let session_a_id = session_a.id;
    let worker = std::thread::spawn(move || {
        block_on(orch_a.cancel_work(
            &auth_a_thread,
            "request-a",
            session_a_id,
            &workspace_a_thread,
            &a_id,
            "cancel A".into(),
            None,
        ))
        .expect("A cancel")
    });
    let response_b = block_on(orch.cancel_work(
        &auth_b,
        "request-b",
        session_b.id,
        &workspace_b,
        &b.work_id,
        "cancel B".into(),
        None,
    ))
    .expect("B cancel");
    let response_a = worker.join().unwrap();
    assert_eq!(response_a["work"]["objective"], "PRIVATE_OBJECTIVE_A");
    assert_eq!(response_b["work"]["objective"], "PRIVATE_OBJECTIVE_B");
    let scope_a = IdempotencyScope::new(&auth_a.owner_id, session_a.id, &workspace_a).unwrap();
    let scope_b = IdempotencyScope::new(&auth_b.owner_id, session_b.id, &workspace_b).unwrap();
    let receipt_a = store
        .load_idempotency(&scope_a, "request-a")
        .unwrap()
        .unwrap();
    let receipt_b = store
        .load_idempotency(&scope_b, "request-b")
        .unwrap()
        .unwrap();
    assert_eq!(receipt_a.status, "complete");
    assert_eq!(receipt_b.status, "complete");
    assert_eq!(receipt_a.owner_id, receipt_b.owner_id);
    assert_ne!(receipt_a.workspace_digest, receipt_b.workspace_digest);
    assert!(!receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_B"));
    assert!(!receipt_b
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_A"));
}

#[test]
fn bound_service_mutation_does_not_capture_an_unbound_lifecycle_writer() {
    let _home = HomeGuard;
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let workspace_a = canonical_workspace(dir_a.path()).unwrap();
    let workspace_b = canonical_workspace(dir_b.path()).unwrap();
    let (_env, _home_dir, host, orch, store) =
        boot_service(&[workspace_a.clone(), workspace_b.clone()]);
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, &workspace_a).unwrap();
    let _session_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(_session_b.id, &workspace_b).unwrap();
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    let a = save_objective(&store, session.id, &workspace_a, "PRIVATE_OBJECTIVE_A");
    let b = save_objective(&store, _session_b.id, &workspace_b, "PRIVATE_OBJECTIVE_B");
    let _gate = arm_gate(2);
    let orch_a = orch.clone();
    let auth_a = auth.clone();
    let workspace = workspace_a.clone();
    let a_id = a.work_id.clone();
    let session_id = session.id;
    let worker = std::thread::spawn(move || {
        block_on(orch_a.cancel_work(
            &auth_a,
            "request-a",
            session_id,
            &workspace,
            &a_id,
            "cancel A".into(),
            None,
        ))
        .expect("bound cancel")
    });
    OrchStore::arrive_at_work_request_test_gate();
    let sealed = store
        .seal_queued_managed_work(&b.work_id, "background seal", chrono::Utc::now())
        .unwrap()
        .unwrap();
    let response_a = worker.join().unwrap();
    let scope = IdempotencyScope::new(&auth.owner_id, session.id, &workspace_a).unwrap();
    let receipt_a = store
        .load_idempotency(&scope, "request-a")
        .unwrap()
        .unwrap();
    assert_eq!(response_a["work"]["workId"], a.work_id);
    assert_eq!(receipt_a.status, "complete");
    assert_eq!(receipt_a.response["work"]["workId"], a.work_id);
    assert!(receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_A"));
    assert!(!receipt_a
        .response
        .to_string()
        .contains("PRIVATE_OBJECTIVE_B"));
    assert_eq!(sealed.work_id, b.work_id);
    assert_eq!(sealed.state, WorkState::Failed);
    assert_eq!(
        store.load_work_item(&a.work_id).unwrap().unwrap().state,
        WorkState::Cancelled
    );
    let foreign = IdempotencyScope::new(&auth.owner_id, _session_b.id, &workspace_b).unwrap();
    assert!(store
        .load_idempotency(&foreign, "request-a")
        .unwrap()
        .is_none());
}

#[test]
fn production_claim_crash_child() {
    let Ok(root) = std::env::var("REVIEW584_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut env = ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let stage = std::env::var("REVIEW584_STAGE").unwrap_or_else(|_| "lifecycle_after_item".into());
    let op = std::env::var("REVIEW584_OP").unwrap_or_else(|_| "claim".into());
    set_grokptah_home_override(Some(root.join("home")));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let workspace = canonical_workspace(&root.join("workspace")).unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .unwrap();
        host.start().unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, &workspace).unwrap();
        let store = OrchStore::open(root.join("ledger")).unwrap();
        let item = save_objective(&store, session.id, &workspace, "crash claim");
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            store.clone(),
            OrchestrationConfig {
                bearer_token: BEARER.into(),
                allowlist: WorkspaceAllowlist::new([workspace.clone()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
        let claim_request = if op == "claim" {
            "claim-crash"
        } else {
            "claim-prelude"
        };
        let target_request = match op.as_str() {
            "claim" => "claim-crash",
            "renew" => "renew-crash",
            "link" => "link-crash",
            other => panic!("unknown op {other}"),
        };
        write_fixture(
            &root,
            &json!({
                "sessionId": session.id,
                "workspace": workspace.display().to_string(),
                "workId": item.work_id,
                "ownerId": auth.owner_id,
                "op": op,
                "stage": stage,
                "targetRequestId": target_request,
                "claimRequestId": claim_request,
            }),
        );
        if op == "claim" && stage == "before_intent" {
            arm_intent_fault();
        } else if op == "claim" {
            env.set("GROKPTAH_CRASH_STAGE", &stage);
        }
        let claim = orch
            .claim_work(
                &auth,
                claim_request,
                session.id,
                &workspace,
                &item.work_id,
                Some(300_000),
                None,
            )
            .await;
        if op == "claim" && stage == "before_intent" {
            clear_intent_fault();
            let error = claim.expect_err("before-intent claim must refuse");
            let mut fixture: Value =
                serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap();
            fixture["error"] = json!(error.message);
            write_fixture(&root, &fixture);
            return;
        }
        let claim = claim.expect("prelude claim");
        let attempt_id = claim["attempt"]["attemptId"].as_str().unwrap().to_string();
        let lease_token = claim["leaseToken"].as_str().unwrap().to_string();
        let run_id = "run-review-584".to_string();
        if op == "link" {
            store
                .save_run(&review_run(
                    &run_id,
                    session.id,
                    &workspace.display().to_string(),
                ))
                .unwrap();
        }
        write_fixture(
            &root,
            &json!({
                "sessionId": session.id,
                "workspace": workspace.display().to_string(),
                "workId": item.work_id,
                "ownerId": auth.owner_id,
                "op": op,
                "stage": stage,
                "targetRequestId": target_request,
                "claimRequestId": claim_request,
                "attemptId": attempt_id,
                "runId": run_id,
            }),
        );
        if stage == "before_intent" {
            arm_intent_fault();
        } else {
            env.set("GROKPTAH_CRASH_STAGE", &stage);
        }
        let outcome = match op.as_str() {
            "claim" => unreachable!("claim crash is inside claim_work"),
            "renew" => {
                orch.renew_work(
                    &auth,
                    target_request,
                    session.id,
                    &workspace,
                    &item.work_id,
                    &attempt_id,
                    &lease_token,
                    Some(600_000),
                )
                .await
            }
            "link" => {
                orch.link_work_run(
                    &auth,
                    target_request,
                    session.id,
                    &workspace,
                    &item.work_id,
                    &attempt_id,
                    &lease_token,
                    &run_id,
                )
                .await
            }
            other => panic!("unknown op {other}"),
        };
        clear_intent_fault();
        if stage == "before_intent" {
            let error = outcome.expect_err("before-intent lease mutation must refuse");
            let mut fixture: Value =
                serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap();
            fixture["error"] = json!(error.message);
            write_fixture(&root, &fixture);
            return;
        }
        panic!("crash point did not execute: {outcome:?}");
    });
}

fn arm_intent_fault() {
    *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
        needle: "work-lifecycle-intents",
        skips_remaining: 0,
    });
}

fn clear_intent_fault() {
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
}

fn write_fixture(root: &Path, value: &Value) {
    std::fs::write(root.join("fixture.json"), value.to_string()).unwrap();
}

fn review_run(run_id: &str, session: Uuid, workspace: &str) -> RunRecord {
    let now = chrono::Utc::now();
    RunRecord {
        run_id: run_id.into(),
        session_id: session,
        workspace: workspace.into(),
        request_id: "run-request-review".into(),
        client_id: None,
        state: RunState::Running,
        purpose: Default::default(),
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "preview".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: now,
        updated_at: now,
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

fn spawn_review_child(op: &str, stage: &str) -> (TempDir, std::process::Output) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("production_claim_crash_child")
        .arg("--nocapture")
        .env("REVIEW584_CHILD_ROOT", root.path())
        .env("REVIEW584_OP", op)
        .env("REVIEW584_STAGE", stage)
        .env_remove("GROKPTAH_CRASH_STAGE")
        .output()
        .unwrap();
    (root, output)
}

fn fixture_of(root: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap()
}

fn child_ok(output: &std::process::Output, expected: i32, stage: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{stage} child stdout/stderr: {} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn open_recovered(root: &Path) -> (Value, OrchStore) {
    let meta = fixture_of(root);
    let store = OrchStore::open(root.join("ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    (meta, store)
}

fn scope_from(meta: &Value) -> IdempotencyScope {
    let session = Uuid::parse_str(meta["sessionId"].as_str().unwrap()).unwrap();
    IdempotencyScope::new(
        meta["ownerId"].as_str().unwrap(),
        session,
        Path::new(meta["workspace"].as_str().unwrap()),
    )
    .unwrap()
}

fn assert_secret_absent(ledger: &Path, secret: &str) {
    let mut stack = vec![ledger.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(secret),
                "lease token leaked in {}",
                path.display()
            );
        }
    }
}

fn fresh_service(
    root: &Path,
    meta: &Value,
) -> (
    HomeGuard,
    ProcessEnvGuard,
    HostRuntime,
    Arc<OrchestrationService>,
    AuthContext,
) {
    let _home = HomeGuard;
    let mut env = ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    set_grokptah_home_override(Some(root.join("home")));
    let workspace = PathBuf::from(meta["workspace"].as_str().unwrap());
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let session = Uuid::parse_str(meta["sessionId"].as_str().unwrap()).unwrap();
    host.session_inspect(session)
        .expect("fresh host reloads the child session");
    host.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.join("ledger")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: BEARER.into(),
            allowlist: WorkspaceAllowlist::new([workspace]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
    (_home, env, host, orch, auth)
}

#[test]
fn production_claim_recovery_must_complete_the_original_receipt() {
    exercise_cut("claim", "lifecycle_after_item", true);
}

#[test]
fn claim_renew_and_link_cuts_replay_without_duplicate_effects() {
    for op in ["claim", "renew", "link"] {
        for stage in [
            "before_intent",
            "lifecycle_after_intent",
            "lifecycle_after_attempts",
            "lifecycle_after_item",
            "lifecycle_after_receipt",
            "lifecycle_after_commit",
        ] {
            if op == "claim" && stage == "lifecycle_after_item" {
                continue;
            }
            let full = op == "claim" && stage == "lifecycle_after_commit";
            exercise_cut(op, stage, full);
        }
    }
}

fn exercise_cut(op: &str, stage: &str, full_refusals: bool) {
    let (root, output) = spawn_review_child(op, stage);
    let refused = stage == "before_intent";
    child_ok(&output, if refused { 0 } else { 86 }, stage);
    let (meta, store) = open_recovered(root.path());
    let work_id = meta["workId"].as_str().unwrap().to_string();
    let work = store.load_work_item(&work_id).unwrap().unwrap();
    let scope = scope_from(&meta);
    let target_request = meta["targetRequestId"].as_str().unwrap();
    let receipt = store
        .load_idempotency(&scope, target_request)
        .unwrap()
        .unwrap();
    let attempts = store.list_work_attempts(Some(&work_id)).unwrap();
    println!(
        "{op} {stage}: state {:?} attempts {} receipt {} error {:?}",
        work.state,
        attempts.len(),
        receipt.status,
        receipt.error.as_ref().map(|error| error.message.clone())
    );
    if refused && op == "claim" {
        assert_eq!(attempts.len(), 0);
        assert_eq!(work.attempt_count, 0);
        assert_eq!(receipt.status, "failed");
        assert!(store.list_work_decisions(&work_id).unwrap().is_empty());
        drop(store);
        reopen_stable(root.path(), &work_id, 0, None);
        return;
    }
    assert_eq!(
        attempts.len(),
        1,
        "{op} {stage} duplicated or lost an attempt"
    );
    assert_eq!(work.attempt_count, 1);
    assert_eq!(receipt.status, if refused { "failed" } else { "complete" });
    if refused {
        let claim_receipt = store
            .load_idempotency(&scope, meta["claimRequestId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(claim_receipt.status, "complete");
        assert_eq!(
            attempts[0].lease_expires_at,
            claim_attempt_expiry(&claim_receipt.response)
        );
        assert!(attempts[0].linked_run_ids.is_empty());
        let revision = work.revision;
        drop(store);
        reopen_stable(root.path(), &work_id, 1, Some(revision));
        return;
    }
    assert!(receipt.response.get("leaseToken").is_none());
    assert!(receipt.error.is_none());
    let revision = work.revision;
    let expiry = attempts[0].lease_expires_at;
    let links = attempts[0].linked_run_ids.clone();
    let decisions = store.list_work_decisions(&work_id).unwrap().len();
    if op == "claim" {
        assert_eq!(work.state, WorkState::Leased);
        assert!(links.is_empty());
        assert_eq!(decisions, 0);
    }
    if op == "renew" {
        let claim_receipt = store
            .load_idempotency(&scope, "claim-prelude")
            .unwrap()
            .unwrap();
        assert_eq!(claim_receipt.status, "complete");
        assert!(expiry > claim_attempt_expiry(&claim_receipt.response));
        assert!(links.is_empty());
        assert_eq!(attempts[0].state, AttemptState::Leased);
    }
    if op == "link" {
        assert_eq!(links, vec![meta["runId"].as_str().unwrap().to_string()]);
        assert_eq!(attempts[0].state, AttemptState::Running);
    }
    drop(store);
    reopen_stable(root.path(), &work_id, 1, Some(revision));

    let meta = fixture_of(root.path());
    let (_home, _env, host, orch, auth) = fresh_service(root.path(), &meta);
    let session = Uuid::parse_str(meta["sessionId"].as_str().unwrap()).unwrap();
    let workspace = PathBuf::from(meta["workspace"].as_str().unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let first_claim = orch
            .claim_work(
                &auth,
                meta["claimRequestId"].as_str().unwrap(),
                session,
                &workspace,
                &work_id,
                Some(300_000),
                None,
            )
            .await
            .expect("claim replay");
        let token = first_claim["leaseToken"].as_str().unwrap().to_string();
        let second_claim = orch
            .claim_work(
                &auth,
                meta["claimRequestId"].as_str().unwrap(),
                session,
                &workspace,
                &work_id,
                Some(300_000),
                None,
            )
            .await
            .unwrap();
        assert_eq!(first_claim, second_claim);
        assert!(first_claim.get("leaseToken").is_some());
        if op == "renew" {
            let first = orch
                .renew_work(
                    &auth,
                    "renew-crash",
                    session,
                    &workspace,
                    &work_id,
                    meta["attemptId"].as_str().unwrap(),
                    &token,
                    Some(600_000),
                )
                .await
                .unwrap();
            let second = orch
                .renew_work(
                    &auth,
                    "renew-crash",
                    session,
                    &workspace,
                    &work_id,
                    meta["attemptId"].as_str().unwrap(),
                    &token,
                    Some(600_000),
                )
                .await
                .unwrap();
            assert_eq!(first, second);
            assert!(first.get("leaseToken").is_none());
        }
        if op == "link" {
            let first = orch
                .link_work_run(
                    &auth,
                    "link-crash",
                    session,
                    &workspace,
                    &work_id,
                    meta["attemptId"].as_str().unwrap(),
                    &token,
                    meta["runId"].as_str().unwrap(),
                )
                .await
                .unwrap();
            let second = orch
                .link_work_run(
                    &auth,
                    "link-crash",
                    session,
                    &workspace,
                    &work_id,
                    meta["attemptId"].as_str().unwrap(),
                    &token,
                    meta["runId"].as_str().unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(first, second);
            assert_eq!(
                first["attempt"]["linkedRunIds"].as_array().unwrap().len(),
                1
            );
        }
        if full_refusals {
            let conflict = orch
                .claim_work(
                    &auth,
                    meta["claimRequestId"].as_str().unwrap(),
                    session,
                    &workspace,
                    &work_id,
                    Some(1_000),
                    None,
                )
                .await
                .expect_err("payload conflict");
            assert!(conflict.message.contains("different payload"));
            orch.set_token("rotated-review-token".into());
            let rotated = orch
                .auth_header(Some("Bearer rotated-review-token"))
                .unwrap();
            let rotated = orch
                .claim_work(
                    &rotated,
                    meta["claimRequestId"].as_str().unwrap(),
                    session,
                    &workspace,
                    &work_id,
                    Some(300_000),
                    None,
                )
                .await
                .expect_err("rotation");
            assert!(rotated.message.contains("credential rotation"));
            let other = host.session_new_kind(SessionKind::Build).unwrap();
            host.session_set_cwd(other.id, &workspace).unwrap();
            let wrong_scope = orch
                .claim_work(
                    &auth,
                    "wrong-scope-claim",
                    other.id,
                    &workspace,
                    &work_id,
                    Some(300_000),
                    None,
                )
                .await
                .expect_err("wrong scope");
            assert!(!wrong_scope.message.contains("leaseToken"));
        }
        assert_secret_absent(&root.path().join("ledger"), &token);
        // The host adopts the external ledger. Dropping the service leaves
        // that clone, and an unordered host drop quarantines the home lock.
        // Stop the supervisors, then shut the runtime down, before a later
        // open of the same ledger.
        let stopped = orch.stop_background_tasks().await;
        assert!(
            stopped.fully_stopped && stopped.errors.is_empty(),
            "{:?}",
            stopped.errors
        );
        drop(orch);
        let shutdown = host.shutdown().await;
        assert!(shutdown.is_clean(), "{}", shutdown.operator_summary());
    });
    drop(host);
    let store = OrchStore::open(root.path().join("ledger")).unwrap();
    let work = store.load_work_item(&work_id).unwrap().unwrap();
    let attempts = store.list_work_attempts(Some(&work_id)).unwrap();
    assert_eq!(work.revision, revision);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].lease_expires_at, expiry);
    assert_eq!(attempts[0].linked_run_ids, links);
    assert_eq!(
        store.list_work_decisions(&work_id).unwrap().len(),
        decisions
    );
}

fn claim_attempt_expiry(response: &Value) -> chrono::DateTime<chrono::Utc> {
    serde_json::from_value(response["attempt"]["leaseExpiresAt"].clone()).unwrap()
}

fn reopen_stable(root: &Path, work_id: &str, attempts: usize, revision: Option<u64>) {
    for _ in 0..2 {
        let store = OrchStore::open(root.join("ledger")).unwrap();
        assert!(store.ledger_admission_closed().is_none());
        let work = store.load_work_item(work_id).unwrap().unwrap();
        assert_eq!(
            store.list_work_attempts(Some(work_id)).unwrap().len(),
            attempts
        );
        if let Some(revision) = revision {
            assert_eq!(work.revision, revision);
        }
        drop(store);
    }
}

#[test]
fn full_item_and_edge_ceiling_graph_walk_is_lossless() {
    use chrono::Utc;
    let session = Uuid::new_v4();
    let at = Utc::now();
    let mut lane = Vec::with_capacity(MAX_GRAPH_SCOPE_ITEMS);
    for i in 0..MAX_GRAPH_SCOPE_ITEMS {
        let mut item = WorkItem::new_at(
            "test",
            "fixture",
            session,
            "/fixture",
            "operator",
            WorkPolicy::default(),
            at,
        )
        .unwrap();
        item.work_id = format!("work-{i:04}");
        if i >= 128 {
            let count = if i < 640 { 5 } else { 4 };
            item.dependencies = (0..count)
                .map(|j| WorkDependency {
                    work_id: format!("work-{j:04}"),
                    required_state: WorkState::Succeeded,
                })
                .collect();
        }
        item.validate().unwrap();
        lane.push(item);
    }
    let edges: usize = lane.iter().map(|item| item.dependencies.len()).sum();
    assert_eq!(edges, MAX_GRAPH_EDGES);
    order_work(&mut lane);
    let reader = WorkPageReader {
        owner_id: "operator".into(),
        token_id: "reader".into(),
        session_id: session,
        workspace_digest: "fixture-digest".into(),
    };
    let key = [23u8; 32];
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut read_edges = 0usize;
    let mut max_bytes = 0usize;
    for _ in 0..100 {
        let page = select_work_graph_page(
            &lane,
            GraphScope {
                session_id: session,
                workspace: "/fixture",
            },
            &WorkPageQuery {
                limit: 200,
                cursor,
                filter: WorkPageFilter::default(),
                kind: WorkPageKind::Graph,
            },
            &reader,
            &key,
            at,
            WORK_PAGE_TRANSPORT_CEILING_BYTES,
        )
        .unwrap();
        max_bytes = max_bytes.max(tools_call_response_len(&json!(page)));
        for node in &page.nodes {
            assert!(seen.insert(node.work_id.clone()), "duplicate graph node");
            assert_eq!(node.unresolved_dependencies, 0);
            read_edges += node.dependencies.len();
        }
        if page.complete {
            assert_eq!(seen.len(), MAX_GRAPH_SCOPE_ITEMS);
            assert_eq!(read_edges, MAX_GRAPH_EDGES);
            println!(
                "Graph capacity proof: {} nodes, {} edges, max envelope {} bytes",
                seen.len(),
                read_edges,
                max_bytes
            );
            return;
        }
        assert!(!page.nodes.is_empty());
        cursor = page.continuation;
    }
    panic!("graph walk failed to finish");
}

const RECONCILIATION_REQUIRED: &str = "ledger reconciliation is required";

fn arm_durable_fault(needle: &'static str, skips_remaining: u32) {
    *DURABLE_WRITE_FAULT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(DurableWriteFault {
        needle,
        skips_remaining,
    });
}

fn clear_durable_fault() {
    *DURABLE_WRITE_FAULT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

struct QuarantineFixture {
    root: TempDir,
    workspace: TempDir,
    work_id: String,
    intent_id: String,
    settled_work_id: String,
    journal: PathBuf,
    journal_bytes: Vec<u8>,
    settled_bytes: Vec<u8>,
}

fn plant_quarantine_fixture() -> QuarantineFixture {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = OrchStore::open(root.path()).unwrap();
    let mut item = WorkItem::new(
        "test",
        "legacy reconciliation",
        Uuid::new_v4(),
        workspace.path().display().to_string(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    item = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(item.state, WorkState::Queued);
    let now = chrono::Utc::now();
    let intent_id = "legacy-intent".to_string();
    store
        .save_managed_intent(&ManagedExecutionIntent {
            schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
            intent_id: intent_id.clone(),
            agent_id: "worker-a".into(),
            agent_spec_revision: 1,
            work_id: item.work_id.clone(),
            work_revision: item.revision,
            attempt_id: None,
            run_id: None,
            session_id: item.session_id,
            workspace: item.workspace.clone(),
            source_routine_id: None,
            source_activation_id: None,
            model_selection_key: "grok".into(),
            bounds: RunBounds::default(),
            execution_mode: Default::default(),
            input_hash: "hash".into(),
            grok: None,
            state: ManagedIntentState::Claiming,
            permission_request_id: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    let mut settled = WorkItem::new(
        "test",
        "settled historical work",
        item.session_id,
        item.workspace.clone(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    settled.state = WorkState::Failed;
    store.save_work_item(&settled).unwrap();
    store
        .save_managed_intent(&ManagedExecutionIntent {
            schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
            intent_id: "settled-intent".into(),
            agent_id: "worker-a".into(),
            agent_spec_revision: 1,
            work_id: settled.work_id.clone(),
            work_revision: settled.revision,
            attempt_id: None,
            run_id: None,
            session_id: settled.session_id,
            workspace: settled.workspace.clone(),
            source_routine_id: None,
            source_activation_id: None,
            model_selection_key: "grok".into(),
            bounds: RunBounds::default(),
            execution_mode: Default::default(),
            input_hash: "hash".into(),
            grok: None,
            state: ManagedIntentState::Finalized,
            permission_request_id: None,
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    drop(store);
    let unbound = ManagedFinalizationRecord {
        schema_version: MANAGED_FINALIZATION_SCHEMA_VERSION,
        intent_id: intent_id.clone(),
        work_id: item.work_id.clone(),
        attempt_id: None,
        outcome: ManagedFinalizationOutcome::Failed,
        attempt_state: AttemptState::Expired,
        work_state: WorkState::Failed,
        reason: "legacy unbound journal".into(),
        result: None,
        created_at: now,
    };
    let settled_record = ManagedFinalizationRecord {
        schema_version: MANAGED_FINALIZATION_SCHEMA_VERSION,
        intent_id: "settled-intent".into(),
        work_id: settled.work_id.clone(),
        attempt_id: None,
        outcome: ManagedFinalizationOutcome::Failed,
        attempt_state: AttemptState::Failed,
        work_state: WorkState::Failed,
        reason: "settled historical record".into(),
        result: None,
        created_at: now,
    };
    let journal_bytes = serde_json::to_vec(&unbound).unwrap();
    let settled_bytes = serde_json::to_vec(&settled_record).unwrap();
    let journal = write_finalization_journal(root.path(), &unbound.intent_id, &journal_bytes);
    write_finalization_journal(root.path(), &settled_record.intent_id, &settled_bytes);
    QuarantineFixture {
        root,
        workspace,
        work_id: item.work_id,
        intent_id,
        settled_work_id: settled.work_id,
        journal,
        journal_bytes,
        settled_bytes,
    }
}

fn write_finalization_journal(root: &Path, intent_id: &str, bytes: &[u8]) -> PathBuf {
    let path = root
        .join("managed-finalization")
        .join(format!("{}.json", safe_id_filename(intent_id).unwrap()));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn quarantine_payloads(root: &Path) -> Vec<Vec<u8>> {
    let dir = root.join("managed-finalization-quarantine");
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut payloads = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            payloads.push(std::fs::read(path).unwrap());
        }
    }
    payloads
}

fn assert_refusal_holds(root: &Path, fixture: &QuarantineFixture) {
    for _ in 0..2 {
        let store = OrchStore::open(root).unwrap();
        assert_eq!(
            store.ledger_admission_closed().as_deref(),
            Some(RECONCILIATION_REQUIRED)
        );
        let payloads = quarantine_payloads(root);
        assert!(
            payloads.iter().any(|bytes| bytes == &fixture.journal_bytes),
            "original journal bytes were not preserved"
        );
        assert!(
            payloads.iter().all(|bytes| bytes != &fixture.settled_bytes),
            "a settled historical journal was quarantined as unresolved"
        );
        assert!(!fixture.journal.exists());
        assert_eq!(
            store
                .load_work_item(&fixture.work_id)
                .unwrap()
                .unwrap()
                .state,
            WorkState::Queued
        );
        assert_eq!(
            store
                .load_work_item(&fixture.settled_work_id)
                .unwrap()
                .unwrap()
                .state,
            WorkState::Failed
        );
        assert_eq!(
            store
                .load_managed_intent(&fixture.intent_id)
                .unwrap()
                .unwrap()
                .state,
            ManagedIntentState::Claiming
        );
        assert_eq!(
            store
                .load_managed_intent("settled-intent")
                .unwrap()
                .unwrap()
                .state,
            ManagedIntentState::Finalized
        );
        let claim = store
            .claim_work(&fixture.work_id, "worker-a", None)
            .unwrap_err();
        assert_eq!(claim.message, RECONCILIATION_REQUIRED);
        let sealed = store
            .seal_queued_managed_work(&fixture.work_id, "must not seal", chrono::Utc::now())
            .unwrap_err();
        assert_eq!(sealed.message, RECONCILIATION_REQUIRED);
        let closed = store
            .close_managed_attempt(
                &fixture.intent_id,
                false,
                ManagedRetryCause::Interrupted,
                "must not close",
                chrono::Utc::now(),
            )
            .unwrap_err();
        assert_eq!(closed.message, RECONCILIATION_REQUIRED);
        let finalized = store
            .finalize_managed_intent(
                &fixture.intent_id,
                ManagedFinalizationOutcome::Failed,
                "must not finalize",
                None,
                chrono::Utc::now(),
            )
            .unwrap_err();
        assert_eq!(finalized.message, RECONCILIATION_REQUIRED);
        assert_eq!(
            store
                .load_managed_intent(&fixture.intent_id)
                .unwrap()
                .unwrap()
                .state,
            ManagedIntentState::Claiming
        );
        drop(store);
    }
    assert!(root.join("ledger-reconciliation.json").is_file());
}

fn assert_honest_reconciliation_readiness(ledger: &Path, workspace: &Path) {
    let _home = HomeGuard;
    let mut env = ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let home = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let store = OrchStore::open(ledger).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: BEARER.into(),
            allowlist: WorkspaceAllowlist::new([workspace.to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let server = start_control_server(orch.clone(), 0).await.unwrap();
        let client = reqwest::Client::new();
        let url = format!("http://{}/ready", server.addr);
        let authed = client.get(&url).bearer_auth(BEARER).send().await.unwrap();
        assert_eq!(authed.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = authed.json().await.unwrap();
        assert_eq!(body["ready"], false);
        assert_eq!(
            body["capacity"]["health"]["ledgerAdmissionClosed"],
            RECONCILIATION_REQUIRED
        );
        let open = client.get(&url).send().await.unwrap();
        assert_eq!(open.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let open_body: Value = open.json().await.unwrap();
        assert_eq!(open_body["ready"], false);
        assert!(open_body.get("capacity").is_none());
        assert!(!open_body.to_string().contains(RECONCILIATION_REQUIRED));
        assert!(server.stop_and_wait().await.is_clean());
        let stopped = orch.stop_background_tasks().await;
        assert!(
            stopped.fully_stopped && stopped.errors.is_empty(),
            "{:?}",
            stopped.errors
        );
        drop(orch);
        let shutdown = host.shutdown().await;
        assert!(shutdown.is_clean(), "{}", shutdown.operator_summary());
    });
    drop(host);
}

#[test]
fn failed_quarantine_marker_must_not_reopen_admission() {
    let fixture = plant_quarantine_fixture();
    arm_durable_fault("ledger-reconciliation.json", 0);
    let first = OrchStore::open(fixture.root.path());
    clear_durable_fault();
    let error = match first {
        Err(error) => error,
        Ok(_) => panic!("the marker write fault must execute"),
    };
    assert!(
        error.to_string().contains("injected durable write failure"),
        "{error}"
    );
    assert!(
        fixture.journal.is_file(),
        "a marker failure must leave the original journal in the recovery scan"
    );
    assert_eq!(
        std::fs::read(&fixture.journal).unwrap(),
        fixture.journal_bytes
    );
    assert!(!fixture
        .root
        .path()
        .join("ledger-reconciliation.json")
        .exists());
    let reopened = OrchStore::open(fixture.root.path()).unwrap();
    assert_eq!(
        reopened.ledger_admission_closed().as_deref(),
        Some(RECONCILIATION_REQUIRED)
    );
    drop(reopened);
    assert_refusal_holds(fixture.root.path(), &fixture);
    println!(
        "Preserved quarantine records: {}; reopened admission state: {:?}",
        quarantine_payloads(fixture.root.path()).len(),
        Some(RECONCILIATION_REQUIRED)
    );
    assert_honest_reconciliation_readiness(fixture.root.path(), fixture.workspace.path());
}

#[test]
fn failed_quarantine_move_must_keep_the_marker_and_the_journal() {
    let fixture = plant_quarantine_fixture();
    arm_durable_fault("managed-finalization-quarantine", 0);
    let first = OrchStore::open(fixture.root.path());
    clear_durable_fault();
    let error = match first {
        Err(error) => error,
        Ok(_) => panic!("the evidence move fault must execute"),
    };
    assert!(
        error.to_string().contains("injected durable write failure"),
        "{error}"
    );
    assert_eq!(
        std::fs::read(&fixture.journal).unwrap(),
        fixture.journal_bytes
    );
    assert!(fixture
        .root
        .path()
        .join("ledger-reconciliation.json")
        .is_file());
    let reopened = OrchStore::open(fixture.root.path()).unwrap();
    assert_eq!(
        reopened.ledger_admission_closed().as_deref(),
        Some(RECONCILIATION_REQUIRED)
    );
    drop(reopened);
    assert_refusal_holds(fixture.root.path(), &fixture);
}

#[test]
fn failed_quarantine_directory_fsync_must_keep_moved_evidence() {
    let fixture = plant_quarantine_fixture();
    arm_durable_fault("managed-finalization-quarantine", 1);
    let first = OrchStore::open(fixture.root.path());
    clear_durable_fault();
    let error = match first {
        Err(error) => error,
        Ok(_) => panic!("the directory durability fault must execute"),
    };
    assert!(
        error.to_string().contains("injected durable write failure"),
        "{error}"
    );
    assert!(
        !fixture.journal.exists(),
        "the rename completed before fsync failed"
    );
    let payloads = quarantine_payloads(fixture.root.path());
    assert!(payloads.iter().any(|bytes| bytes == &fixture.journal_bytes));
    assert!(fixture
        .root
        .path()
        .join("ledger-reconciliation.json")
        .is_file());
    let reopened = OrchStore::open(fixture.root.path()).unwrap();
    assert_eq!(
        reopened.ledger_admission_closed().as_deref(),
        Some(RECONCILIATION_REQUIRED)
    );
    drop(reopened);
    assert_refusal_holds(fixture.root.path(), &fixture);
}

#[test]
fn legacy_quarantine_without_marker_stays_refused() {
    let fixture = plant_quarantine_fixture();
    let dir = fixture.root.path().join("managed-finalization-quarantine");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::rename(
        &fixture.journal,
        dir.join("legacy-intent.json.unbound-legacy"),
    )
    .unwrap();
    let garbage = dir.join("truncated.json");
    std::fs::write(&garbage, b"{not-a-record").unwrap();
    assert!(!fixture
        .root
        .path()
        .join("ledger-reconciliation.json")
        .exists());
    let opened = OrchStore::open(fixture.root.path()).unwrap();
    assert_eq!(
        opened.ledger_admission_closed().as_deref(),
        Some(RECONCILIATION_REQUIRED)
    );
    drop(opened);
    assert_eq!(
        std::fs::read(dir.join("legacy-intent.json.unbound-legacy")).unwrap(),
        fixture.journal_bytes
    );
    assert_eq!(std::fs::read(&garbage).unwrap(), b"{not-a-record");
    assert!(fixture
        .root
        .path()
        .join("ledger-reconciliation.json")
        .is_file());
    assert_refusal_holds(fixture.root.path(), &fixture);
    assert_eq!(std::fs::read(&garbage).unwrap(), b"{not-a-record");
}

#[test]
fn quarantine_crash_child() {
    let Ok(root) = std::env::var("REVIEW584_QUARANTINE_ROOT") else {
        return;
    };
    match OrchStore::open(root) {
        Err(error) => panic!("quarantine crash point did not execute: {error:#}"),
        Ok(_) => panic!("quarantine crash point did not execute"),
    }
}

#[test]
fn quarantine_process_cuts_preserve_evidence_and_refusal() {
    for stage in [
        "quarantine_before_marker",
        "quarantine_after_marker",
        "quarantine_after_move",
    ] {
        let fixture = plant_quarantine_fixture();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("quarantine_crash_child")
            .arg("--nocapture")
            .env("REVIEW584_QUARANTINE_ROOT", fixture.root.path())
            .env("GROKPTAH_CRASH_STAGE", stage)
            .env_remove("REVIEW584_CHILD_ROOT")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(86),
            "{stage} child stdout/stderr: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        match stage {
            "quarantine_before_marker" => {
                assert_eq!(
                    std::fs::read(&fixture.journal).unwrap(),
                    fixture.journal_bytes
                );
                assert!(!fixture
                    .root
                    .path()
                    .join("ledger-reconciliation.json")
                    .exists());
            }
            "quarantine_after_marker" => {
                assert_eq!(
                    std::fs::read(&fixture.journal).unwrap(),
                    fixture.journal_bytes
                );
                assert!(fixture
                    .root
                    .path()
                    .join("ledger-reconciliation.json")
                    .is_file());
            }
            "quarantine_after_move" => {
                assert!(!fixture.journal.exists());
                assert!(quarantine_payloads(fixture.root.path())
                    .iter()
                    .any(|bytes| bytes == &fixture.journal_bytes));
                assert!(fixture
                    .root
                    .path()
                    .join("ledger-reconciliation.json")
                    .is_file());
            }
            _ => unreachable!(),
        }
        let reopened = OrchStore::open(fixture.root.path()).unwrap();
        assert_eq!(
            reopened.ledger_admission_closed().as_deref(),
            Some(RECONCILIATION_REQUIRED)
        );
        drop(reopened);
        assert_refusal_holds(fixture.root.path(), &fixture);
        println!("{stage}: evidence preserved and admission refused");
    }
}

fn attempt_views(store: &OrchStore, work_id: &str) -> Vec<WorkAttemptView> {
    store
        .list_work_attempts(Some(work_id))
        .unwrap()
        .iter()
        .map(WorkAttemptView::from)
        .collect()
}

fn read_detail(
    item: &WorkItem,
    attempts: &[WorkAttemptView],
    section: WorkDetailSection,
    offset: usize,
    limit: usize,
    attempt_id: Option<&str>,
    fence: Option<&str>,
) -> Result<WorkDetailPageV1, grokptah_agent_bridge::orchestration::OrchError> {
    project_work_detail(
        item,
        attempts,
        section,
        offset,
        limit,
        WORK_PAGE_TRANSPORT_CEILING_BYTES,
        attempt_id,
        fence,
    )
}

fn note_bounded(page: &WorkDetailPageV1, largest: &mut usize, lease_token: &str) {
    let bytes = tools_call_response_len(&serde_json::json!(page));
    *largest = (*largest).max(bytes);
    assert!(
        bytes <= WORK_PAGE_TRANSPORT_CEILING_BYTES,
        "detail page is {bytes} bytes"
    );
    let rendered = serde_json::to_string(page).unwrap();
    assert!(
        !rendered.contains(lease_token),
        "lease token leaked into a page"
    );
    assert!(!rendered.contains("leaseTokenHash"));
}

fn collect_evidence(
    item: &WorkItem,
    attempts: &[WorkAttemptView],
    attempt_id: &str,
    largest: &mut usize,
    lease_token: &str,
) -> Vec<String> {
    let mut offset = 0usize;
    let mut fence = None;
    let mut collected = Vec::new();
    for _ in 0..(MAX_WORK_ARTIFACTS + 4) {
        let page = read_detail(
            item,
            attempts,
            WorkDetailSection::AttemptEvidence,
            offset,
            1,
            Some(attempt_id),
            fence.as_deref(),
        )
        .unwrap();
        note_bounded(&page, largest, lease_token);
        let page_fence = page.consistency_fence.clone().unwrap();
        if let Some(expected) = &fence {
            assert_eq!(expected, &page_fence);
        }
        fence = Some(page_fence);
        assert_eq!(page.attempt_id.as_deref(), Some(attempt_id));
        for value in page.body["evidence"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            collected.push(value.as_str().unwrap().to_string());
        }
        if page.complete {
            return collected;
        }
        let next = page.next_offset.unwrap();
        assert!(next > offset);
        let missing_fence = read_detail(
            item,
            attempts,
            WorkDetailSection::AttemptEvidence,
            next,
            1,
            Some(attempt_id),
            None,
        )
        .unwrap_err();
        assert_eq!(missing_fence.code, OrchErrorCode::RestartRequired);
        offset = next;
    }
    panic!("evidence walk did not finish");
}

fn collect_artifacts(
    item: &WorkItem,
    attempts: &[WorkAttemptView],
    attempt_id: &str,
    largest: &mut usize,
    lease_token: &str,
) -> Vec<WorkArtifactRef> {
    let mut offset = 0usize;
    let mut fence = None;
    let mut collected = Vec::new();
    for _ in 0..(MAX_WORK_ARTIFACTS + 4) {
        let page = read_detail(
            item,
            attempts,
            WorkDetailSection::AttemptArtifacts,
            offset,
            64,
            Some(attempt_id),
            fence.as_deref(),
        )
        .unwrap();
        note_bounded(&page, largest, lease_token);
        let page_fence = page.consistency_fence.clone().unwrap();
        if let Some(expected) = &fence {
            assert_eq!(expected, &page_fence);
        }
        fence = Some(page_fence);
        assert_eq!(page.attempt_id.as_deref(), Some(attempt_id));
        for value in page.body["artifacts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            collected.push(serde_json::from_value(value).unwrap());
        }
        if page.complete {
            return collected;
        }
        let next = page.next_offset.unwrap();
        assert!(next > offset);
        let stale = "0".repeat(64);
        let wrong = read_detail(
            item,
            attempts,
            WorkDetailSection::AttemptArtifacts,
            next,
            64,
            Some(attempt_id),
            Some(&stale),
        )
        .unwrap_err();
        assert_eq!(wrong.code, OrchErrorCode::RestartRequired);
        offset = next;
    }
    panic!("artifact walk did not finish");
}

#[test]
fn maximal_valid_attempt_result_must_be_readable_through_detail() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = OrchStore::open(root.path()).unwrap();
    let item = WorkItem::new(
        "test",
        "bounded objective",
        Uuid::new_v4(),
        workspace.path().display().to_string(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work(&item.work_id, "worker", Some(60_000))
        .unwrap();
    let blob = "q".repeat(MAX_WORK_ARTIFACT_BYTES);
    let result = WorkResult {
        summary: "failed with full legal evidence".into(),
        evidence: vec![blob.clone(); MAX_WORK_ARTIFACTS],
        artifacts: (0..MAX_WORK_ARTIFACTS)
            .map(|index| WorkArtifactRef {
                artifact_id: format!("artifact-{index}"),
                kind: blob.clone(),
                label: blob.clone(),
                digest: Some(blob.clone()),
                retained_until: None,
            })
            .collect(),
        failure: Some("fixture failure".into()),
        cancellation_reason: None,
        completed_at: chrono::Utc::now(),
        verification: None,
        candidate_verification: None,
    };
    result.validate().unwrap();
    let (failed, attempt) = store
        .fail_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result,
        )
        .unwrap();
    attempt.validate().unwrap();
    failed.validate().unwrap();
    let attempts = vec![serde_json::to_value(WorkAttemptView::from(&attempt)).unwrap()];
    let wrapped = tools_call_response_len(&json!({ "attempts": attempts }));
    println!(
        "One valid attempt wrapped body bytes: {wrapped} (ceiling {WORK_PAGE_TRANSPORT_CEILING_BYTES})"
    );
    assert!(wrapped > WORK_PAGE_TRANSPORT_CEILING_BYTES);
    let views = attempt_views(&store, &failed.work_id);
    assert_eq!(views.len(), 1);
    let mut largest = 0usize;
    let listed = read_detail(
        &failed,
        &views,
        WorkDetailSection::Attempts,
        0,
        1,
        None,
        None,
    )
    .expect("a legal attempt summary fits under the transport ceiling");
    note_bounded(&listed, &mut largest, &claim.lease_token);
    assert_eq!(listed.schema_version, 1);
    assert!(listed.complete);
    assert_eq!(listed.body["consistency"], ATTEMPT_CONSISTENCY);
    assert!(listed.body["attempts"][0]["result"]
        .get("evidence")
        .is_none());
    assert!(listed.body["attempts"][0]["result"]
        .get("artifacts")
        .is_none());
    assert_eq!(
        listed.body["attempts"][0]["result"]["evidenceCount"],
        MAX_WORK_ARTIFACTS
    );
    assert_eq!(
        listed.body["attempts"][0]["result"]["artifactCount"],
        MAX_WORK_ARTIFACTS
    );
    assert!(!listed.body.to_string().contains(&blob));
    let content_fence = listed.body["attempts"][0]["fence"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(content_fence, attempt_content_fence(&views[0]));
    let detail = read_detail(
        &failed,
        &views,
        WorkDetailSection::AttemptDetail,
        0,
        1,
        Some(&attempt.attempt_id),
        Some(&content_fence),
    )
    .unwrap();
    note_bounded(&detail, &mut largest, &claim.lease_token);
    assert!(detail.complete);
    assert_eq!(
        detail.consistency_fence.as_deref(),
        Some(content_fence.as_str())
    );
    assert_eq!(
        detail.body["attempt"]["result"]["summary"],
        "failed with full legal evidence"
    );
    assert_eq!(
        detail.body["attempt"]["result"]["failure"],
        "fixture failure"
    );
    let evidence = collect_evidence(
        &failed,
        &views,
        &attempt.attempt_id,
        &mut largest,
        &claim.lease_token,
    );
    let artifacts = collect_artifacts(
        &failed,
        &views,
        &attempt.attempt_id,
        &mut largest,
        &claim.lease_token,
    );
    assert_eq!(evidence, attempt.result.as_ref().unwrap().evidence);
    assert_eq!(artifacts, attempt.result.as_ref().unwrap().artifacts);
    println!("Nested attempt page max envelope: {largest} bytes");
    assert!(largest <= WORK_PAGE_TRANSPORT_CEILING_BYTES);
}

#[test]
fn attempt_pages_follow_heartbeat_and_keep_every_nested_element() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = OrchStore::open(root.path()).unwrap();
    let item = WorkItem::new(
        "test",
        "fence objective",
        Uuid::new_v4(),
        workspace.path().display().to_string(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    let empty = read_detail(&item, &[], WorkDetailSection::Attempts, 0, 1, None, None).unwrap();
    assert!(empty.complete);
    assert_eq!(empty.body["attempts"].as_array().unwrap().len(), 0);
    assert!(empty.consistency_fence.is_some());
    let claim = store
        .claim_work(&item.work_id, "worker", Some(60_000))
        .unwrap();
    let revision = claim.work.revision;
    let before = attempt_views(&store, &item.work_id);
    let before_fence = attempt_content_fence(&before[0]);
    let list_fence = read_detail(
        &claim.work,
        &before,
        WorkDetailSection::Attempts,
        0,
        1,
        None,
        None,
    )
    .unwrap()
    .consistency_fence
    .unwrap();
    store
        .renew_work_lease(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            Some(60_000),
        )
        .unwrap();
    let renewed_item = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(renewed_item.revision, revision);
    let renewed = attempt_views(&store, &item.work_id);
    assert_ne!(attempt_content_fence(&renewed[0]), before_fence);
    assert_ne!(renewed[0].last_heartbeat_at, before[0].last_heartbeat_at);
    let stale = read_detail(
        &renewed_item,
        &renewed,
        WorkDetailSection::AttemptDetail,
        0,
        1,
        Some(&claim.attempt.attempt_id),
        Some(&before_fence),
    )
    .unwrap_err();
    assert_eq!(stale.code, OrchErrorCode::RestartRequired);
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "run-1",
        )
        .unwrap();
    let linked_item = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(linked_item.revision, revision);
    let linked = attempt_views(&store, &item.work_id);
    assert_eq!(linked[0].linked_run_ids, vec!["run-1".to_string()]);
    assert_ne!(
        attempt_content_fence(&linked[0]),
        attempt_content_fence(&renewed[0])
    );
    let quotes = "\"".repeat(MAX_WORK_ARTIFACT_BYTES);
    let snow = "𝄞".repeat(MAX_WORK_ARTIFACT_BYTES / "𝄞".len());
    assert_eq!(snow.len(), MAX_WORK_ARTIFACT_BYTES);
    let mut evidence = vec![quotes.clone(); 3];
    evidence.push(snow.clone());
    let failed_result = WorkResult {
        summary: snow.clone(),
        evidence,
        artifacts: vec![
            WorkArtifactRef {
                artifact_id: "snow-1".into(),
                kind: quotes.clone(),
                label: snow.clone(),
                digest: Some(quotes.clone()),
                retained_until: None,
            },
            WorkArtifactRef {
                artifact_id: "snow-2".into(),
                kind: "log".into(),
                label: quotes.clone(),
                digest: None,
                retained_until: None,
            },
        ],
        failure: Some("quoted failure".into()),
        cancellation_reason: None,
        completed_at: chrono::Utc::now(),
        verification: None,
        candidate_verification: None,
    };
    failed_result.validate().unwrap();
    let (failed, attempt) = store
        .fail_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            failed_result,
        )
        .unwrap();
    assert!(failed.revision > revision);
    let views = attempt_views(&store, &item.work_id);
    let mut largest = 0usize;
    let evidence = collect_evidence(
        &failed,
        &views,
        &attempt.attempt_id,
        &mut largest,
        &claim.lease_token,
    );
    let artifacts = collect_artifacts(
        &failed,
        &views,
        &attempt.attempt_id,
        &mut largest,
        &claim.lease_token,
    );
    assert_eq!(evidence, attempt.result.as_ref().unwrap().evidence);
    assert_eq!(artifacts, attempt.result.as_ref().unwrap().artifacts);
    assert_eq!(evidence.last().map(String::as_str), Some(snow.as_str()));
    assert!(largest <= WORK_PAGE_TRANSPORT_CEILING_BYTES);

    let second = store
        .claim_work(&item.work_id, "worker", Some(60_000))
        .unwrap();
    let both = attempt_views(&store, &item.work_id);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].attempt_id, attempt.attempt_id);
    assert_eq!(both[1].attempt_number, 2);
    let mixed = read_detail(
        &second.work,
        &both,
        WorkDetailSection::Attempts,
        0,
        1,
        None,
        Some(&list_fence),
    )
    .unwrap_err();
    assert_eq!(mixed.code, OrchErrorCode::RestartRequired);
    let first_still = read_detail(
        &second.work,
        &both,
        WorkDetailSection::AttemptEvidence,
        0,
        1,
        Some(&attempt.attempt_id),
        Some(&attempt_content_fence(&both[0])),
    )
    .unwrap();
    assert_eq!(first_still.body["evidence"][0], quotes);
    let unknown = read_detail(
        &second.work,
        &both,
        WorkDetailSection::AttemptDetail,
        0,
        1,
        Some("missing-attempt"),
        None,
    )
    .unwrap_err();
    assert_eq!(unknown.code, OrchErrorCode::InvalidRequest);
    assert_eq!(unknown.message, "unknown attempt_id");
    let foreign = read_detail(
        &second.work,
        &both,
        WorkDetailSection::AttemptDetail,
        0,
        1,
        Some(&second.attempt.attempt_id),
        None,
    )
    .unwrap();
    assert_eq!(
        foreign.attempt_id.as_deref(),
        Some(second.attempt.attempt_id.as_str())
    );
    let other = WorkItem::new(
        "test",
        "other objective",
        Uuid::new_v4(),
        workspace.path().display().to_string(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&other).unwrap();
    let other_claim = store
        .claim_work(&other.work_id, "worker", Some(60_000))
        .unwrap();
    let wrong_work = read_detail(
        &other,
        &attempt_views(&store, &other.work_id),
        WorkDetailSection::AttemptDetail,
        0,
        1,
        Some(&attempt.attempt_id),
        None,
    )
    .unwrap_err();
    assert_eq!(wrong_work.code, unknown.code);
    assert_eq!(wrong_work.message, unknown.message);
    assert_ne!(other_claim.attempt.attempt_id, attempt.attempt_id);
    let on_summary = read_detail(
        &other,
        &[],
        WorkDetailSection::Summary,
        0,
        1,
        Some(&other_claim.attempt.attempt_id),
        None,
    )
    .unwrap_err();
    assert_eq!(on_summary.code, OrchErrorCode::InvalidRequest);
}
