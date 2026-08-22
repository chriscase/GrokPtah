//! Hosted MCP conformance for durable manager plans.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkResult, WorkState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    McpRemoteError, SessionKind,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn hosted_manager_plan_replays_and_unlocks_dependency_graph() {
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
    let manager = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "manager-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "manager-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    let created = client
        .call_tool(
            "ptah_create_manager_plan",
            json!({
                "request_id": "manager-create-308",
                "session_id": lane.id,
                "workspace": workspace_text,
                "manager_agent_id": manager.agent_id,
                "objective": "verify the hosted manager path",
                "steps": [
                    {"stepId": "inspect", "kind": "verification", "objective": "inspect the project"},
                    {"stepId": "report", "kind": "verification", "objective": "report the result", "dependencies": ["inspect"]}
                ],
                "max_in_flight": 1,
                "max_replans": 2
            }),
        )
        .await
        .unwrap();
    let plan_id = created.structured["plan"]["planId"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = client
        .call_tool(
            "ptah_list_manager_plans",
            json!({"session_id": lane.id, "workspace": workspace_text}),
        )
        .await
        .unwrap();
    assert_eq!(listed.structured["plans"].as_array().unwrap().len(), 1);

    let advance_args = json!({
        "request_id": "manager-advance-308",
        "session_id": lane.id,
        "workspace": workspace_text,
        "plan_id": plan_id,
        "expected_revision": 1
    });
    let advanced = client
        .call_tool("ptah_advance_manager_plan", advance_args.clone())
        .await
        .unwrap();
    let first_work = advanced.structured["createdWork"][0]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let replay = client
        .call_tool("ptah_advance_manager_plan", advance_args)
        .await
        .unwrap();
    assert_eq!(replay.structured["createdWork"][0]["workId"], first_work);

    let claimed = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "manager-claim-308",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": first_work
            }),
        )
        .await
        .unwrap();
    client
        .call_tool(
            "ptah_complete_work",
            json!({
                "request_id": "manager-complete-308",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": first_work,
                "attempt_id": claimed.structured["attempt"]["attemptId"],
                "lease_token": claimed.structured["leaseToken"],
                "summary": "inspection complete"
            }),
        )
        .await
        .unwrap();

    let next = client
        .call_tool(
            "ptah_advance_manager_plan",
            json!({
                "request_id": "manager-advance-308-next",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": advanced.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(next.structured["createdWork"].as_array().unwrap().len(), 1);
    assert_eq!(next.structured["plan"]["state"], "active");

    let second_work = next.structured["createdWork"][0]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let failed_claim = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "manager-claim-308-second",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": second_work
            }),
        )
        .await
        .unwrap();
    let failed = client
        .call_tool(
            "ptah_fail_work",
            json!({
                "request_id": "manager-fail-308-second",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": second_work,
                "attempt_id": failed_claim.structured["attempt"]["attemptId"],
                "lease_token": failed_claim.structured["leaseToken"],
                "summary": "verification failed",
                "failure": "fixture failure"
            }),
        )
        .await
        .unwrap();
    client
        .call_tool(
            "ptah_cancel_work",
            json!({
                "request_id": "manager-cancel-308-second",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": second_work,
                "reason": "preserve the failed outcome for manager re-planning",
                "expected_revision": failed.structured["work"]["revision"]
            }),
        )
        .await
        .unwrap();
    let needs_replan = client
        .call_tool(
            "ptah_advance_manager_plan",
            json!({
                "request_id": "manager-advance-308-failure",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": next.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(needs_replan.structured["plan"]["state"], "needs_replan");
    let replanned = client
        .call_tool(
            "ptah_replan_manager_plan",
            json!({
                "request_id": "manager-replan-308",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "reason": "replace the failed verification step",
                "steps": [{"stepId": "replacement", "kind": "verification", "objective": "retry independently"}],
                "expected_revision": needs_replan.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    let resumed = client
        .call_tool(
            "ptah_advance_manager_plan",
            json!({
                "request_id": "manager-advance-308-replanned",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": replanned.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        resumed.structured["createdWork"].as_array().unwrap().len(),
        1
    );
    assert_eq!(resumed.structured["plan"]["state"], "active");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn hosted_manager_tick_routes_attention_and_terminal_outcomes() {
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
    let manager = host.ensure_session_agent(lane.id).unwrap();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "manager-tick-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let store_for_fixture = host.ensure_orchestration_store().unwrap();
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "manager-tick-token");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    let created = client
        .call_tool(
            "ptah_create_manager_plan",
            json!({
                "request_id": "manager-tick-create",
                "session_id": lane.id,
                "workspace": workspace_text,
                "manager_agent_id": manager.agent_id,
                "objective": "exercise durable manager observations",
                "steps": [{"stepId": "observe", "kind": "verification", "objective": "observe the fixture"}],
                "max_in_flight": 1,
                "max_replans": 2
            }),
        )
        .await
        .unwrap();
    let plan_id = created.structured["plan"]["planId"]
        .as_str()
        .unwrap()
        .to_string();
    let advanced = client
        .call_tool(
            "ptah_advance_manager_plan",
            json!({
                "request_id": "manager-tick-advance",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": 1
            }),
        )
        .await
        .unwrap();
    let work_id = advanced.structured["createdWork"][0]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    let mut work = store_for_fixture.load_work_item(&work_id).unwrap().unwrap();
    work.state = WorkState::AwaitingInput;
    work.blocked_reason = Some("permission required by the fixture".into());
    work.bump_at(Utc::now());
    store_for_fixture.save_work_item(&work).unwrap();
    let tick = client
        .call_tool(
            "ptah_tick_manager_plan",
            json!({
                "request_id": "manager-tick-input",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": advanced.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(tick.structured["messages"][0]["kind"], "question");
    assert_eq!(
        tick.structured["plan"]["steps"][0]["state"],
        "awaiting_input"
    );
    let tick_replay = client
        .call_tool(
            "ptah_tick_manager_plan",
            json!({
                "request_id": "manager-tick-input",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": advanced.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        tick_replay.structured["messages"][0]["messageId"],
        tick.structured["messages"][0]["messageId"]
    );

    work.state = WorkState::Review;
    work.bump_at(Utc::now());
    store_for_fixture.save_work_item(&work).unwrap();
    let review = client
        .call_tool(
            "ptah_tick_manager_plan",
            json!({
                "request_id": "manager-tick-review",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": tick.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(review.structured["messages"][0]["kind"], "review_request");
    assert_eq!(
        review.structured["plan"]["steps"][0]["state"],
        "awaiting_review"
    );

    let now = Utc::now();
    work.state = WorkState::Failed;
    work.result = Some(WorkResult {
        summary: "fixture failed".into(),
        evidence: Vec::new(),
        artifacts: Vec::new(),
        failure: Some("fixture failure".into()),
        cancellation_reason: None,
        completed_at: now,
    });
    work.bump_at(now);
    store_for_fixture.save_work_item(&work).unwrap();
    let terminal = client
        .call_tool(
            "ptah_tick_manager_plan",
            json!({
                "request_id": "manager-tick-terminal",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": review.structured["plan"]["revision"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(terminal.structured["messages"][0]["kind"], "status");
    assert_eq!(terminal.structured["plan"]["state"], "needs_replan");
    assert!(terminal.structured["messages"][0]["body"]
        .as_str()
        .unwrap()
        .contains("fixture failure"));
}

/// Manager tools are the newest mutating surface on the control plane: they
/// materialize Work, fence plan revisions, and drive autonomous decisions.
/// Every refusal below must also leave the plan and its ledger untouched —
/// an error return is not by itself proof that nothing happened.
/// Assert one control-plane call was refused for the stated reason. Matching
/// the typed code keeps a scope test from passing on an argument error.
fn assert_refused<T>(outcome: anyhow::Result<T>, expected_code: &str, context: &str) {
    let error = match outcome {
        Ok(_) => panic!("{context} must fail closed"),
        Err(error) => error,
    };
    let code = error
        .downcast_ref::<McpRemoteError>()
        .and_then(McpRemoteError::data_code);
    assert_eq!(
        code,
        Some(expected_code),
        "unexpected refusal for {context}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn manager_plan_mutations_fail_closed_outside_their_scope() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let other_workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let manager = host.ensure_session_agent(lane.id).unwrap();
    let peer_lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(peer_lane.id, other_workspace.path())
        .unwrap();
    host.ensure_session_agent(peer_lane.id).unwrap();
    let store = host.ensure_orchestration_store().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "manager-scope-token".into(),
            allowlist: WorkspaceAllowlist::new([
                workspace.path().to_path_buf(),
                other_workspace.path().to_path_buf(),
            ]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client =
        McpControlClient::new(format!("http://{}", server.addr), "manager-scope-token");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    let other_workspace_text = other_workspace.path().display().to_string();

    let created = client
        .call_tool(
            "ptah_create_manager_plan",
            json!({
                "request_id": "manager-scope-create",
                "session_id": lane.id,
                "workspace": workspace_text,
                "manager_agent_id": manager.agent_id,
                "objective": "certify manager scope enforcement",
                "steps": [{"stepId": "only", "kind": "verification", "objective": "observe"}],
                "max_in_flight": 1,
                "max_replans": 2
            }),
        )
        .await
        .unwrap();
    let plan_id = created.structured["plan"]["planId"]
        .as_str()
        .unwrap()
        .to_string();
    let sealed_revision = created.structured["plan"]["revision"].as_u64().unwrap();
    let sealed_work = store.list_work_items().unwrap().len();
    let sealed_messages = store
        .list_recent_messages(lane.id, &workspace_text, None, None, 200)
        .unwrap()
        .messages
        .len();

    // A plan created in one Lane and workspace must not be readable,
    // advanceable, observable, or re-plannable from anywhere else.
    let unknown_session = Uuid::new_v4();
    // The expected code matters: a refusal that came from argument shape
    // rather than scope resolution would prove nothing about enforcement.
    // An unknown session is refused before scope is resolved at all.
    let scopes: Vec<(&str, serde_json::Value, &str, &str)> = vec![
        (
            "peer lane in another workspace",
            json!({"session_id": peer_lane.id, "workspace": other_workspace_text}),
            "peer-lane",
            "forbidden_scope",
        ),
        (
            "peer lane claiming the owning workspace",
            json!({"session_id": peer_lane.id, "workspace": workspace_text}),
            "peer-lane-owning-workspace",
            "workspace_mismatch",
        ),
        (
            "owning lane claiming another workspace",
            json!({"session_id": lane.id, "workspace": other_workspace_text}),
            "owning-lane-other-workspace",
            "workspace_mismatch",
        ),
        (
            "unknown session",
            json!({"session_id": unknown_session, "workspace": workspace_text}),
            "unknown-session",
            "invalid_request",
        ),
    ];

    for (label, scope, slug, expected_code) in &scopes {
        let session_id = scope["session_id"].clone();
        let scope_workspace = scope["workspace"].clone();

        let read = client
            .call_tool(
                "ptah_get_manager_plan",
                json!({
                    "session_id": session_id,
                    "workspace": scope_workspace,
                    "plan_id": plan_id
                }),
            )
            .await;
        assert_refused(read, expected_code, &format!("read from {label}"));

        for (tool, extra) in [
            ("ptah_advance_manager_plan", json!({})),
            ("ptah_tick_manager_plan", json!({})),
            (
                "ptah_replan_manager_plan",
                json!({
                    "reason": "cross-scope replan",
                    "steps": [{"stepId": "injected", "kind": "verification", "objective": "injected"}]
                }),
            ),
        ] {
            let mut arguments = json!({
                "request_id": format!("manager-scope-{slug}-{tool}"),
                "session_id": session_id,
                "workspace": scope_workspace,
                "plan_id": plan_id,
                "expected_revision": sealed_revision
            });
            for (key, value) in extra.as_object().unwrap() {
                arguments[key] = value.clone();
            }
            let refused = client.call_tool(tool, arguments).await;
            assert_refused(refused, expected_code, &format!("{tool} from {label}"));
        }
    }

    // An unknown plan id inside the owning scope is refused too, so a caller
    // cannot probe for plans by guessing identifiers.
    let unknown_plan = client
        .call_tool(
            "ptah_get_manager_plan",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": Uuid::new_v4().to_string()
            }),
        )
        .await;
    assert_refused(
        unknown_plan,
        "invalid_request",
        "unknown plan id in the owning scope",
    );

    // Nothing above may have mutated durable state.
    let plan = store.load_manager_plan(&plan_id).unwrap().unwrap();
    assert_eq!(
        plan.revision, sealed_revision,
        "a refused mutation must not bump the plan revision"
    );
    assert_eq!(plan.steps[0].work_id, None, "no step Work may be created");
    assert_eq!(
        plan.steps.len(),
        1,
        "a refused replan must not append a step"
    );
    assert_eq!(
        store.list_work_items().unwrap().len(),
        sealed_work,
        "a refused mutation must not create Work"
    );
    assert_eq!(
        store
            .list_recent_messages(lane.id, &workspace_text, None, None, 200)
            .unwrap()
            .messages
            .len(),
        sealed_messages,
        "a refused tick must not deliver a notification"
    );

    // The owning scope still works, so the refusals above are scope
    // enforcement rather than a broken plan.
    let advanced = client
        .call_tool(
            "ptah_advance_manager_plan",
            json!({
                "request_id": "manager-scope-owner-advance",
                "session_id": lane.id,
                "workspace": workspace_text,
                "plan_id": plan_id,
                "expected_revision": sealed_revision
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        advanced.structured["createdWork"].as_array().unwrap().len(),
        1
    );
}
