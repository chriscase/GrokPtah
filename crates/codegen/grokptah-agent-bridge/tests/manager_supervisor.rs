//! Deterministic vertical slice for the runtime-owned manager loop.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ManagerDecisionState, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkResult, WorkState, WorkspaceAllowlist,
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
async fn autonomous_manager_replans_once_and_reaches_success() {
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
            bearer_token: "manager-supervisor-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let fixture_store = host.ensure_orchestration_store().unwrap();
    fixture_store
        .revise_agent_spec(&manager.agent_id, "manager-supervisor-test", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.requires_approval_before_execution = false;
            Ok(())
        })
        .unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(
        format!("http://{}", server.addr),
        "manager-supervisor-token",
    );
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    let created = client
        .call_tool(
            "ptah_create_manager_plan",
            json!({
                "request_id": "manager-supervisor-create",
                "session_id": lane.id,
                "workspace": workspace_text,
                "manager_agent_id": manager.agent_id,
                "objective": "recover from a deterministic failure",
                "steps": [{"stepId": "original", "kind": "verification", "objective": "run original"}],
                "max_in_flight": 1,
                "max_replans": 2,
                "autonomous": true
            }),
        )
        .await
        .unwrap();
    let plan_id = created.structured["plan"]["planId"]
        .as_str()
        .unwrap()
        .to_string();
    let root_id = created.structured["rootWork"]["workId"].as_str().unwrap();
    assert!(fixture_store.claim_work(root_id, "operator", None).is_err());

    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;
    let plan = fixture_store.load_manager_plan(&plan_id).unwrap().unwrap();
    let original_id = plan.steps[0].work_id.clone().unwrap();
    assert_eq!(
        fixture_store
            .list_work_items()
            .unwrap()
            .iter()
            .filter(|work| work.source_manager_plan_id.as_deref() == Some(&plan_id))
            .count(),
        1
    );

    let mut original = fixture_store.load_work_item(&original_id).unwrap().unwrap();
    original.state = WorkState::AwaitingInput;
    original.blocked_reason = Some("fixture question".into());
    original.bump_at(Utc::now());
    fixture_store.save_work_item(&original).unwrap();
    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;
    original.state = WorkState::Review;
    original.bump_at(Utc::now());
    fixture_store.save_work_item(&original).unwrap();
    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;

    original.state = WorkState::Failed;
    original.result = Some(WorkResult {
        summary: "fixture failed".into(),
        evidence: vec![],
        artifacts: vec![],
        failure: Some("fixture failure".into()),
        cancellation_reason: None,
        completed_at: Utc::now(),
    });
    original.bump_at(Utc::now());
    fixture_store.save_work_item(&original).unwrap();
    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;
    let decisions = fixture_store
        .list_manager_decisions(Some(&plan_id))
        .unwrap();
    assert_eq!(
        decisions.len(),
        1,
        "supervisor status: {:?}; plan: {:?}",
        orch.manager_supervisor_status(),
        fixture_store.load_manager_plan(&plan_id).unwrap()
    );
    let decision = &decisions[0];
    let mut decision_work = fixture_store
        .load_work_item(&decision.decision_work_id)
        .unwrap()
        .unwrap();
    let directive = json!({
        "schemaVersion": 1,
        "occurrenceId": decision.decision_id,
        "planId": plan_id,
        "expectedPlanRevision": decision.expected_plan_revision,
        "managerAgentId": decision.manager_agent_id,
        "expectedAgentSpecRevision": decision.agent_spec_revision,
        "inputSnapshotHash": decision.input_snapshot_hash,
        "directive": {
            "type": "append_replacement_steps",
            "reason": "replace deterministic failure",
            "replacesStepIds": ["original"],
            "steps": [{"stepId": "replacement", "kind": "verification", "objective": "run replacement"}]
        }
    });
    decision_work.state = WorkState::Succeeded;
    decision_work.result = Some(WorkResult {
        summary: directive.to_string(),
        evidence: vec![],
        artifacts: vec![],
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
    });
    decision_work.bump_at(Utc::now());
    fixture_store.save_work_item(&decision_work).unwrap();
    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;
    let decision = fixture_store
        .list_manager_decisions(Some(&plan_id))
        .unwrap()
        .remove(0);
    assert_eq!(decision.state, ManagerDecisionState::Applied);

    let plan = fixture_store.load_manager_plan(&plan_id).unwrap().unwrap();
    let replacement_id = plan
        .steps
        .iter()
        .find(|step| step.step_id == "replacement")
        .and_then(|step| step.work_id.clone())
        .unwrap();
    let mut replacement = fixture_store
        .load_work_item(&replacement_id)
        .unwrap()
        .unwrap();
    replacement.state = WorkState::Succeeded;
    replacement.result = Some(WorkResult {
        summary: "replacement succeeded".into(),
        evidence: vec![],
        artifacts: vec![],
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
    });
    replacement.bump_at(Utc::now());
    fixture_store.save_work_item(&replacement).unwrap();
    orch.drive_manager_supervisor_once().await;
    orch.drive_manager_supervisor_once().await;
    let plan = fixture_store.load_manager_plan(&plan_id).unwrap().unwrap();
    assert_eq!(
        plan.state,
        grokptah_agent_bridge::orchestration::ManagerPlanState::Succeeded
    );
    assert_eq!(
        fixture_store
            .list_manager_decisions(Some(&plan_id))
            .unwrap()
            .len(),
        1
    );
    let messages = fixture_store
        .list_recent_messages(lane.id, &workspace_text, None, None, 100)
        .unwrap()
        .messages;
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.kind.as_str() == "question")
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.kind.as_str() == "review_request")
            .count(),
        1
    );
}
