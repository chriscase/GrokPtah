//! End-to-end scope enforcement for dependency declaration through the real
//! MCP control surface (#305).
//!
//! The store-level suite proves the invariant; this proves a real
//! authenticated client cannot reach around it, and that a dependency
//! declaration is not an existence oracle across scopes.

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    SessionKind,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn dependency_declaration_is_scope_bound_and_reveals_nothing_foreign() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();

    // Two Build lanes in one workspace: distinct sessions, so distinct scopes.
    let first = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(first.id, workspace.path()).unwrap();
    let second = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(second.id, workspace.path()).unwrap();

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "work-graph-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "work-graph-token");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    // An anchor owned by the first session.
    let anchor = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "anchor",
                "session_id": first.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "anchor work item"
            }),
        )
        .await
        .unwrap();
    let anchor_id = anchor.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    // The public projection reports the same admission decision the claim path
    // enforces, so a client is never shown a queued item it would be refused.
    assert_eq!(
        anchor.structured["admission"], "admissible",
        "the projection must carry the canonical admission decision"
    );

    // The owning session may depend on it.
    let in_scope = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "in-scope",
                "session_id": first.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on the anchor",
                "dependencies": [{ "workId": anchor_id, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect("an in-scope dependency is accepted");
    let dependent_id = in_scope.structured["work"]["workId"]
        .as_str()
        .expect("work id")
        .to_string();

    // The second session may not, and must not be able to tell the anchor
    // apart from an id that was never issued.
    let foreign = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "foreign",
                "session_id": second.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on another session's work",
                "dependencies": [{ "workId": anchor_id, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect_err("a cross-session dependency must be refused");

    let invented = uuid::Uuid::new_v4().to_string();
    let unknown = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "unknown",
                "session_id": second.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on an id that was never issued",
                "dependencies": [{ "workId": invented, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect_err("an unknown dependency must be refused");

    // Compare what a client actually sees. `Debug` on the transport error
    // carries a captured backtrace whose line numbers differ per call site,
    // which is an artifact of this test rather than anything the client is
    // told.
    let foreign_text = foreign.to_string().replace(&anchor_id, "<id>");
    let unknown_text = unknown.to_string().replace(&invented, "<id>");
    assert_eq!(
        foreign_text, unknown_text,
        "a real foreign id and an invented one must be indistinguishable to a client"
    );
    assert!(
        !foreign_text.contains(&anchor_id),
        "the refusal must not echo another scope's work id: {foreign_text}"
    );

    // A dependent that is waiting reports the wait rather than looking ready.
    let dependent = client
        .call_tool(
            "ptah_get_work",
            json!({
                "session_id": first.id,
                "workspace": workspace_text,
                "work_id": dependent_id,
            }),
        )
        .await
        .expect("read the dependent");
    assert_eq!(dependent.structured["admission"], "dependencies_pending");

    server.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn two_principals_cannot_cast_each_others_review_verdicts() {
    use grokptah_agent_bridge::orchestration::{
        AdmissionBlock, AuthContext, ReviewVerdict, WorkItem, WorkPolicy, WorkReviewPolicy,
    };

    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();

    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "two-principal-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );

    let mut work = WorkItem::new(
        "verification",
        "gated work",
        lane.id,
        workspace.path().display().to_string(),
        "creator",
        WorkPolicy::default(),
    )
    .unwrap();
    work.review = Some(WorkReviewPolicy {
        reviewers: vec!["alice".into(), "bob".into()],
        required_approvals: 2,
        policy_revision: 1,
    });
    store.save_work_item(&work).unwrap();

    let alice = AuthContext {
        token_id: "token-alice".into(),
        owner_id: "alice".into(),
    };
    let bob = AuthContext {
        token_id: "token-bob".into(),
        owner_id: "bob".into(),
    };

    // Impersonation through the service surface is refused: the reviewer
    // identity comes from the authenticated context, not the argument.
    let error = orch
        .record_work_review_verdict_scoped(
            &alice,
            lane.id,
            workspace.path(),
            &work.work_id,
            "bob",
            ReviewVerdict::Approve,
            None,
        )
        .expect_err("alice must not cast bob's verdict");
    assert!(error.to_string().contains("only record its own"), "{error}");

    // Each principal casting its own verdict is accepted, and the gate opens
    // only once both have.
    let first = orch
        .record_work_review_verdict_scoped(
            &alice,
            lane.id,
            workspace.path(),
            &work.work_id,
            "alice",
            ReviewVerdict::Approve,
            None,
        )
        .expect("alice records her own verdict");
    assert_eq!(first["admission"], AdmissionBlock::ReviewPending.as_str());

    let second = orch
        .record_work_review_verdict_scoped(
            &bob,
            lane.id,
            workspace.path(),
            &work.work_id,
            "bob",
            ReviewVerdict::Approve,
            None,
        )
        .expect("bob records his own verdict");
    assert_eq!(second["admission"], AdmissionBlock::Admissible.as_str());

    // A principal the gate does not name is refused even when self-attested.
    let intruder = AuthContext {
        token_id: "token-mallory".into(),
        owner_id: "mallory".into(),
    };
    assert!(
        orch.record_work_review_verdict_scoped(
            &intruder,
            lane.id,
            workspace.path(),
            &work.work_id,
            "mallory",
            ReviewVerdict::Approve,
            None,
        )
        .is_err(),
        "an unnamed principal must be refused"
    );

    // And only the owning principal may revoke its own verdict.
    assert!(
        orch.revoke_work_review_verdict_scoped(
            &alice,
            lane.id,
            workspace.path(),
            &work.work_id,
            "bob",
            None,
        )
        .is_err(),
        "alice must not revoke bob's verdict"
    );

    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn managed_execution_of_a_gated_item_leaves_no_intent_lease_or_run() {
    use grokptah_agent_bridge::orchestration::{
        AdmissionBlock, WorkItem, WorkPolicy, WorkReviewPolicy,
    };

    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();

    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "managed-gate-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );

    let mut work = WorkItem::new(
        "verification",
        "gated work the executor must not touch",
        lane.id,
        workspace.path().display().to_string(),
        "creator",
        WorkPolicy::default(),
    )
    .unwrap();
    work.review = Some(WorkReviewPolicy {
        reviewers: vec!["alice".into()],
        required_approvals: 1,
        policy_revision: 1,
    });
    store.save_work_item(&work).unwrap();
    assert_eq!(
        store
            .admission_block_at(&work.work_id, chrono::Utc::now())
            .unwrap(),
        AdmissionBlock::ReviewPending
    );

    // Drive the native executor repeatedly. A gated item must leave no trace:
    // no managed intent (not even a Claiming one that is later abandoned), no
    // attempt, no lease, and no run.
    for _ in 0..3 {
        orch.drive_native_executor_once().await;
    }

    assert!(
        store
            .list_managed_intents()
            .unwrap()
            .iter()
            .all(|intent| intent.work_id != work.work_id),
        "a denied item must not leave a managed intent behind"
    );
    assert!(
        store
            .list_work_attempts(Some(&work.work_id))
            .unwrap()
            .is_empty(),
        "a denied item must not leave an attempt or lease behind"
    );
    let stored = store.load_work_item(&work.work_id).unwrap().unwrap();
    assert_eq!(stored.state, WorkState::Queued);
    assert_eq!(
        store
            .admission_block_at(&work.work_id, chrono::Utc::now())
            .unwrap(),
        AdmissionBlock::ReviewPending
    );

    set_grokptah_home_override(None);
}
