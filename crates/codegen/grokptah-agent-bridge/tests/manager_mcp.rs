//! Hosted MCP conformance for durable manager plans.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkResult, WorkState,
    WorkspaceAllowlist,
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
async fn hosted_manager_plan_replays_and_unlocks_dependency_graph() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
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
    })
    .expect("acquire the GrokPtah instance lock");
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
