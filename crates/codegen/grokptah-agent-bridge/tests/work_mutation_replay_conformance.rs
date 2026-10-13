//! Production callers must replay their sealed request outcome, not newer state.
mod common;

use std::path::PathBuf;
use std::sync::Arc;

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, hash_payload, AuthContext, IdempotencyScope, OrchErrorCode, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, WorkAttempt, WorkAttemptView, WorkPolicy,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, DurableWriteFault, HostConfig,
    HostRuntime, McpControlClient, SessionKind, DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use uuid::Uuid;

const TOKEN: &str = "synthetic-v2-caller-conformance";

struct Fixture {
    _env: common::ProcessEnvGuard,
    _root: tempfile::TempDir,
    host: HostRuntime,
    store: OrchStore,
    orch: Arc<OrchestrationService>,
    auth: AuthContext,
    session: Uuid,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let mut env = common::ProcessEnvGuard::new();
        env.set("GROKPTAH_AGENT_OFFLINE", "1");
        let root = tempfile::tempdir().unwrap();
        set_grokptah_home_override(Some(root.path().join("home")));
        std::fs::create_dir_all(root.path().join("workspace")).unwrap();
        let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .unwrap();
        host.start().unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap().id;
        host.session_set_cwd(session, &workspace).unwrap();
        let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            store.clone(),
            OrchestrationConfig {
                bearer_token: TOKEN.into(),
                allowlist: WorkspaceAllowlist::new([workspace.clone()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
        Self {
            _env: env,
            _root: root,
            host,
            store,
            orch,
            auth,
            session,
            workspace,
        }
    }

    async fn created(&self) -> Value {
        self.orch
            .create_work(
                &self.auth,
                "create",
                self.session,
                &self.workspace,
                "test".into(),
                "real caller replay".into(),
                0,
                None,
                None,
                Vec::new(),
                WorkPolicy::default(),
            )
            .await
            .unwrap()
    }

    async fn claimed(&self) -> Value {
        let created = self.created().await;
        self.orch
            .claim_work(
                &self.auth,
                "claim",
                self.session,
                &self.workspace,
                created["work"]["workId"].as_str().unwrap(),
                Some(300_000),
                None,
            )
            .await
            .unwrap()
    }

    fn scope(&self) -> IdempotencyScope {
        IdempotencyScope::new(&self.auth.owner_id, self.session, &self.workspace).unwrap()
    }

    fn receipt_path(&self, request_id: &str) -> PathBuf {
        walkdir::WalkDir::new(self.store.root().join("idempotency"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .find(|entry| {
                serde_json::from_slice::<Value>(&std::fs::read(entry.path()).unwrap())
                    .is_ok_and(|value| value["requestId"] == request_id)
            })
            .unwrap()
            .into_path()
    }

    async fn close(self) {
        let stopped = self.orch.stop_background_tasks().await;
        assert!(stopped.fully_stopped && stopped.errors.is_empty());
        assert!(self.host.shutdown().await.is_clean());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn progress_replays_the_original_result_after_a_newer_progress_report() {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let work_id = claim["work"]["workId"].as_str().unwrap();
    let attempt = claim["attempt"]["attemptId"].as_str().unwrap();
    let lease = claim["leaseToken"].as_str().unwrap();
    let report = |request, summary: &str, percent| {
        fixture.orch.report_work_progress(
            &fixture.auth,
            request,
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            summary.into(),
            Some(percent),
        )
    };
    let original = report("progress-original", "first progress", 25)
        .await
        .unwrap();
    let replay = report("progress-original", "first progress", 25)
        .await
        .expect("the same caller inputs must replay despite a new server timestamp");
    assert_eq!(replay, original);
    let newer = report("progress-newer", "newer progress", 75)
        .await
        .unwrap();
    assert_ne!(newer["work"]["revision"], original["work"]["revision"]);
    assert_eq!(
        report("progress-original", "first progress", 25)
            .await
            .unwrap(),
        original
    );
    let receipt = fixture
        .store
        .load_idempotency(&fixture.scope(), "progress-original")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, "complete");
    assert_eq!(receipt.response, original);
    assert_eq!(
        fixture
            .store
            .load_work_item(work_id)
            .unwrap()
            .unwrap()
            .progress
            .unwrap()
            .summary,
        "newer progress"
    );
    assert_eq!(
        fixture
            .store
            .list_work_attempts(Some(work_id))
            .unwrap()
            .len(),
        1
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn legacy_progress_replay_uses_the_sealed_receipt_timestamp_and_refuses_changed_inputs() {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let work_id = claim["work"]["workId"].as_str().unwrap();
    let attempt = claim["attempt"]["attemptId"].as_str().unwrap();
    let lease = claim["leaseToken"].as_str().unwrap();
    let original = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "legacy-progress",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            "legacy summary".into(),
            Some(20),
        )
        .await
        .unwrap();
    // Exact pre-v2 payload projection, using the timestamp sealed by the real
    // first report. This is a legacy receipt fixture, not a new store claim.
    let legacy_payload = json!({"sessionId":fixture.session,"workspace":fixture.workspace.display().to_string(),
        "workId":work_id,"details":{"attemptId":attempt,"leaseToken":lease,
        "progress":original["work"]["progress"]}});
    let path = fixture.receipt_path("legacy-progress");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    receipt["payloadHash"] = json!(hash_payload(&legacy_payload));
    std::fs::write(&path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    let sealed = std::fs::read(&path).unwrap();
    fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "newer",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            "newer summary".into(),
            Some(80),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .orch
            .report_work_progress(
                &fixture.auth,
                "legacy-progress",
                fixture.session,
                &fixture.workspace,
                work_id,
                attempt,
                lease,
                "legacy summary".into(),
                Some(20)
            )
            .await
            .unwrap(),
        original
    );
    for (target, attempt_id, token, summary, percent) in [
        (work_id, attempt, lease, "changed summary", 20),
        (work_id, attempt, lease, "legacy summary", 21),
        ("different-work", attempt, lease, "legacy summary", 20),
        (work_id, "different-attempt", lease, "legacy summary", 20),
        (work_id, attempt, "wrong-token", "legacy summary", 20),
    ] {
        let error = fixture
            .orch
            .report_work_progress(
                &fixture.auth,
                "legacy-progress",
                fixture.session,
                &fixture.workspace,
                target,
                attempt_id,
                token,
                summary.into(),
                Some(percent),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert_eq!(std::fs::read(&path).unwrap(), sealed);
    }
    assert!(!String::from_utf8(sealed).unwrap().contains(lease));
    assert_eq!(
        fixture
            .store
            .load_work_item(work_id)
            .unwrap()
            .unwrap()
            .progress
            .unwrap()
            .summary,
        "newer summary"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn rejected_progress_replays_its_original_error_without_a_new_timestamp_conflict() {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let work_id = claim["work"]["workId"].as_str().unwrap();
    let attempt = claim["attempt"]["attemptId"].as_str().unwrap();
    let original = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "bad-progress",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            "invalid-lease",
            "no effect".into(),
            Some(50),
        )
        .await
        .unwrap_err();
    let replay = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "bad-progress",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            "invalid-lease",
            "no effect".into(),
            Some(50),
        )
        .await
        .unwrap_err();
    assert_eq!(json!(replay), json!(original));
    assert_eq!(
        fixture
            .store
            .load_work_item(work_id)
            .unwrap()
            .unwrap()
            .revision,
        claim["work"]["revision"].as_u64().unwrap()
    );
    assert_eq!(
        fixture
            .store
            .load_idempotency(&fixture.scope(), "bad-progress")
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn legacy_progress_refusal_preserves_owner_receipt_until_exact_inputs_are_proved() {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let work_id = claim["work"]["workId"].as_str().unwrap();
    let attempt = claim["attempt"]["attemptId"].as_str().unwrap();
    let lease = claim["leaseToken"].as_str().unwrap();
    let original = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "legacy-owner",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            "original summary".into(),
            Some(10),
        )
        .await
        .unwrap();
    let canonical = fixture.receipt_path("legacy-owner");
    let legacy = canonical
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(canonical.file_name().unwrap());
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&canonical).unwrap()).unwrap();
    receipt["payloadHash"] = json!(hash_payload(&json!({"sessionId":fixture.session,
        "workspace":fixture.workspace.display().to_string(), "workId":work_id,
        "details":{"attemptId":attempt,"leaseToken":lease,"progress":original["work"]["progress"]}})));
    std::fs::write(&canonical, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    std::fs::rename(&canonical, &legacy).unwrap();
    let sealed = std::fs::read(&legacy).unwrap();
    let error = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "legacy-owner",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            "changed summary".into(),
            Some(10),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::Conflict);
    assert!(
        legacy.is_file(),
        "a refused payload must not adopt/delete its legacy source"
    );
    assert!(!canonical.exists());
    assert_eq!(std::fs::read(&legacy).unwrap(), sealed);
    let replay = fixture
        .orch
        .report_work_progress(
            &fixture.auth,
            "legacy-owner",
            fixture.session,
            &fixture.workspace,
            work_id,
            attempt,
            lease,
            "original summary".into(),
            Some(10),
        )
        .await
        .unwrap();
    assert_eq!(replay, original);
    assert!(!legacy.exists());
    assert_eq!(std::fs::read(&canonical).unwrap(), sealed);
    fixture.close().await;
}

async fn call_fault_operation(
    orch: &OrchestrationService,
    auth: &AuthContext,
    operation: &str,
    lane: (Uuid, &std::path::Path),
    work_id: &str,
    revision: u64,
) -> Result<Value, grokptah_agent_bridge::orchestration::OrchError> {
    match operation {
        "cancel" => {
            orch.cancel_work(
                auth,
                "operation-fault",
                lane.0,
                lane.1,
                work_id,
                "original reason".into(),
                Some(revision),
            )
            .await
        }
        "reprioritize" => {
            orch.reprioritize_work(
                auth,
                "operation-fault",
                lane.0,
                lane.1,
                work_id,
                42,
                "original reason".into(),
                Some(revision),
            )
            .await
        }
        _ => unreachable!(),
    }
}

async fn work_write_fault(
    operation: &str,
    needle: &'static str,
    skips_remaining: u32,
    expected_status: &str,
) {
    let fixture = Fixture::new();
    let claim = if operation == "cancel" {
        fixture.claimed().await
    } else {
        fixture.created().await
    };
    let work_id = claim["work"]["workId"].as_str().unwrap();
    let revision = claim["work"]["revision"].as_u64().unwrap();
    *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
        needle,
        skips_remaining,
    });
    let result = call_fault_operation(
        &fixture.orch,
        &fixture.auth,
        operation,
        (fixture.session, &fixture.workspace),
        work_id,
        revision,
    )
    .await;
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
    result.expect_err("the injected commit/cleanup write must fail");
    let receipt = fixture
        .store
        .load_idempotency(&fixture.scope(), "operation-fault")
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.status, expected_status,
        "an admitted outcome must survive fail_claim and Drop"
    );
    assert!(
        fixture.store.ledger_admission_closed().is_some(),
        "cleanup uncertainty must close live admission too"
    );
    let directory = if operation == "cancel" {
        "work-lifecycle-intents"
    } else {
        "work-intents"
    };
    let intent_path = std::fs::read_dir(fixture.store.root().join(directory))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let intent: Value = serde_json::from_slice(&std::fs::read(intent_path).unwrap()).unwrap();
    let expected = if operation == "cancel" {
        let attempts: Vec<WorkAttempt> =
            serde_json::from_value(intent["attempts"].clone()).unwrap();
        json!({"work":intent["item"],"attempts":attempts.iter().map(WorkAttemptView::from).collect::<Vec<_>>()})
    } else {
        json!({"work":intent["item"],"sessionId":fixture.session,
            "workspace":fixture.workspace.display().to_string()})
    };
    let Fixture {
        _env,
        _root,
        host,
        orch,
        store,
        session,
        workspace,
        ..
    } = fixture;
    let stopped = orch.stop_background_tasks().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    drop(orch);
    drop(store);
    assert!(host.shutdown().await.is_clean());
    drop(host);
    for _ in 0..3 {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        })
        .unwrap();
        host.start().unwrap();
        host.session_inspect(session).unwrap();
        let store = OrchStore::open(_root.path().join("home/ledger")).unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            store.clone(),
            OrchestrationConfig {
                bearer_token: TOKEN.into(),
                allowlist: WorkspaceAllowlist::new([workspace.clone()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
        let replay = call_fault_operation(
            &orch,
            &auth,
            operation,
            (session, &workspace),
            work_id,
            revision,
        )
        .await
        .unwrap();
        assert_eq!(replay, expected);
        assert_eq!(
            store.list_work_attempts(Some(work_id)).unwrap().len(),
            usize::from(operation == "cancel")
        );
        assert_eq!(
            store.list_work_decisions(work_id).unwrap().len(),
            usize::from(operation == "reprioritize")
        );
        assert!(store.ledger_admission_closed().is_none());
        let stopped = orch.stop_background_tasks().await;
        assert!(stopped.fully_stopped && stopped.errors.is_empty());
        drop(orch);
        drop(store);
        assert!(host.shutdown().await.is_clean());
    }
    drop(_env);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn lifecycle_receipt_write_failure_preserves_the_admitted_original_outcome() {
    work_write_fault("cancel", "idempotency/v2", 0, "pending").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn lifecycle_work_write_failure_preserves_the_admitted_original_outcome() {
    work_write_fault("cancel", "work-items", 0, "pending").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn lifecycle_cleanup_failure_closes_admission_until_the_sealed_outcome_recovers() {
    work_write_fault("cancel", "work-lifecycle-intents", 1, "complete").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mutation_work_write_failure_preserves_the_admitted_original_outcome() {
    work_write_fault("reprioritize", "work-items", 0, "pending").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mutation_receipt_write_failure_preserves_the_admitted_original_outcome() {
    work_write_fault("reprioritize", "idempotency/v2", 0, "pending").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mutation_cleanup_failure_closes_admission_until_the_original_outcome_recovers() {
    work_write_fault("reprioritize", "work-intents", 1, "complete").await;
}

async fn mcp_result_replay(tool: &str) {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let server = start_control_server(fixture.orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
    client.initialize().await.unwrap();
    let mut args = json!({"request_id":"mcp-result", "session_id":fixture.session,
        "workspace":fixture.workspace, "work_id":claim["work"]["workId"],
        "attempt_id":claim["attempt"]["attemptId"], "lease_token":claim["leaseToken"],
        "summary":"original result", "evidence":["synthetic evidence"]});
    if tool == "ptah_fail_work" {
        args["failure"] = json!("original failure");
    }
    let original = client
        .call_tool(tool, args.clone())
        .await
        .unwrap()
        .structured;
    let replay = client
        .call_tool(tool, args.clone())
        .await
        .expect("MCP-generated completed_at must not change the request's identity")
        .structured;
    assert_eq!(replay, original);
    // Exact legacy result hash; its timestamp must come from the original
    // sealed receipt rather than today's adapter-generated value.
    let legacy_payload = json!({"sessionId":fixture.session,
        "workspace":fixture.workspace.display().to_string(), "workId":claim["work"]["workId"],
        "details":{"attemptId":claim["attempt"]["attemptId"],"leaseToken":claim["leaseToken"],
        "result":original["work"]["result"]}});
    let path = fixture.receipt_path("mcp-result");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    receipt["payloadHash"] = json!(hash_payload(&legacy_payload));
    std::fs::write(&path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    let sealed = std::fs::read(&path).unwrap();
    assert_eq!(
        client
            .call_tool(tool, args.clone())
            .await
            .unwrap()
            .structured,
        original
    );
    assert_eq!(std::fs::read(&path).unwrap(), sealed);
    args["summary"] = json!("changed result");
    assert!(client
        .call_tool(tool, args)
        .await
        .unwrap_err()
        .to_string()
        .contains("conflict"));
    let work_id = claim["work"]["workId"].as_str().unwrap();
    assert_eq!(
        fixture
            .store
            .load_work_item(work_id)
            .unwrap()
            .unwrap()
            .revision,
        original["work"]["revision"].as_u64().unwrap()
    );
    assert_eq!(
        fixture
            .store
            .list_work_attempts(Some(work_id))
            .unwrap()
            .len(),
        1
    );
    let stopped = server.stop_and_wait().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_completion_replays_original_result_without_a_timestamp_conflict() {
    mcp_result_replay("ptah_complete_work").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_failure_replays_original_result_without_a_timestamp_conflict() {
    mcp_result_replay("ptah_fail_work").await;
}

async fn mcp_result_refusal(tool: &str) {
    let fixture = Fixture::new();
    let claim = fixture.claimed().await;
    let server = start_control_server(fixture.orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
    client.initialize().await.unwrap();
    let args = json!({"request_id":"mcp-refusal", "session_id":fixture.session,
        "workspace":fixture.workspace, "work_id":claim["work"]["workId"],
        "attempt_id":claim["attempt"]["attemptId"], "lease_token":"wrong-lease",
        "summary":"refuse the original input"});
    let original = client
        .call_tool(tool, args.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(original.contains("forbidden_scope"));
    assert_eq!(
        client.call_tool(tool, args).await.unwrap_err().to_string(),
        original
    );
    let receipt = fixture
        .store
        .load_idempotency(&fixture.scope(), "mcp-refusal")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, "failed");
    let stopped = server.stop_and_wait().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    fixture.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_completion_replays_its_original_refusal() {
    mcp_result_refusal("ptah_complete_work").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_failure_replays_its_original_refusal() {
    mcp_result_refusal("ptah_fail_work").await;
}
