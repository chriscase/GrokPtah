//! Deterministic vertical slice for the runtime-owned manager loop.

mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ManagerDecisionState, ManagerPlanState, OrchStore, OrchestrationConfig, OrchestrationService,
    RunBounds, WorkResult, WorkState, WorkspaceAllowlist, MAX_MANAGER_PLANS_PER_PASS,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    McpRemoteError, SessionKind,
};
use serde_json::{json, Value};
use tempfile::tempdir;

use common::ProcessEnvGuard;

fn assert_remote_error(error: anyhow::Error, expected_code: &str) {
    let remote = error
        .downcast_ref::<McpRemoteError>()
        .expect("MCP failure should be a typed remote error");
    assert_eq!(remote.data_code(), Some(expected_code));
}

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

/// Shared offline fixture for the autonomous supervisor invariants below.
struct SupervisorFixture {
    _env: ProcessEnvGuard,
    _home: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    /// The control server must outlive the fixture; dropping the handle
    /// closes the transport the client is still using.
    _server: grokptah_agent_bridge::ControlServerHandle,
    orch: std::sync::Arc<OrchestrationService>,
    fixture_store: OrchStore,
    lane_id: uuid::Uuid,
    manager_agent_id: String,
    workspace_text: String,
}

async fn boot_supervisor(token: &str) -> (SupervisorFixture, McpControlClient) {
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
            bearer_token: token.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let fixture_store = host.ensure_orchestration_store().unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), token);
    client.initialize().await.unwrap();
    // Quiesce the interval- and event-driven watcher so `drive_manager_supervisor_once`
    // is the only driver. Pass boundaries are otherwise unobservable: the
    // autonomous supervisor wakes on plan creation and would converge the
    // fixture before an explicit pass ever ran.
    orch.stop_background_tasks().await;
    let workspace_text = workspace.path().display().to_string();
    (
        SupervisorFixture {
            _env: env,
            _home: home,
            _workspace: workspace,
            _server: server,
            orch,
            fixture_store,
            lane_id: lane.id,
            manager_agent_id: manager.agent_id,
            workspace_text,
        },
        client,
    )
}

impl SupervisorFixture {
    /// Put the lane's manager Agent into the only policy shape that admits
    /// autonomous coordination.
    fn allow_autonomous_coordination(&self) {
        self.fixture_store
            .revise_agent_spec(&self.manager_agent_id, "supervisor-certification", |spec| {
                spec.managed_execution.enabled = true;
                spec.managed_execution.requires_approval_before_execution = false;
                spec.managed_execution.allowed_work_kinds = Vec::new();
                Ok(())
            })
            .unwrap();
    }

    fn create_plan_arguments(&self, request_id: &str, step_id: &str, autonomous: bool) -> Value {
        json!({
            "request_id": request_id,
            "session_id": self.lane_id,
            "workspace": self.workspace_text,
            "manager_agent_id": self.manager_agent_id,
            "objective": "certify the autonomous supervisor",
            "steps": [{"stepId": step_id, "kind": "verification", "objective": "observe the fixture"}],
            "max_in_flight": 1,
            "max_replans": 2,
            "autonomous": autonomous
        })
    }

    /// Plans whose step Work the supervisor has actually materialized. The
    /// plan's root container also carries `sourceManagerPlanId`, so it is
    /// excluded here — counting it would hide starvation.
    fn plans_with_materialized_step_work(&self) -> std::collections::BTreeSet<String> {
        self.fixture_store
            .list_work_items()
            .unwrap()
            .into_iter()
            .filter(|work| work.source_manager_step_id.is_some())
            .filter_map(|work| work.source_manager_plan_id)
            .collect()
    }

    fn materialized_step_work_count(&self) -> usize {
        self.fixture_store
            .list_work_items()
            .unwrap()
            .iter()
            .filter(|work| work.source_manager_step_id.is_some())
            .count()
    }
}

/// A pass is bounded, and its rotating cursor guarantees that the plans it
/// could not reach are reached by the next pass. Without rotation the same
/// lowest-sorting plans would win every pass and the tail would starve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_bounded_pass_rotates_so_no_autonomous_plan_starves() {
    let (fixture, mut client) = boot_supervisor("manager-fairness-token").await;
    fixture.allow_autonomous_coordination();

    let total = MAX_MANAGER_PLANS_PER_PASS + 3;
    let mut plan_ids = std::collections::BTreeSet::new();
    for index in 0..total {
        let created = client
            .call_tool(
                "ptah_create_manager_plan",
                fixture.create_plan_arguments(
                    &format!("manager-fairness-{index}"),
                    &format!("step-{index}"),
                    true,
                ),
            )
            .await
            .unwrap();
        plan_ids.insert(
            created.structured["plan"]["planId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(plan_ids.len(), total);

    fixture.orch.drive_manager_supervisor_once().await;
    let first = fixture.orch.manager_supervisor_status().last_report;
    assert_eq!(first.plans_scanned, total);
    assert_eq!(first.plans_processed, MAX_MANAGER_PLANS_PER_PASS);
    assert!(
        first.bounded,
        "a pass over more plans than its bound must report bounded"
    );
    let after_first = fixture.plans_with_materialized_step_work();
    assert_eq!(after_first.len(), MAX_MANAGER_PLANS_PER_PASS);
    assert!(
        after_first.len() < plan_ids.len(),
        "the fixture must exceed one pass so starvation is observable"
    );

    // The plans the first pass could not reach must be reached next, without
    // re-materializing Work for the plans it already advanced.
    fixture.orch.drive_manager_supervisor_once().await;
    let after_second = fixture.plans_with_materialized_step_work();
    assert_eq!(
        after_second, plan_ids,
        "every autonomous plan must be reached within two bounded passes"
    );
    assert_eq!(
        fixture.materialized_step_work_count(),
        total,
        "rotation must not duplicate Work for plans an earlier pass advanced"
    );
    client.close_session().await.unwrap();
    fixture.orch.stop_background_tasks().await;
}

/// Interval and event wakeups both call the same pass, so repeating a pass at
/// an unchanged durable state must be a no-op rather than a second
/// notification or a second child Work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn repeated_passes_converge_without_duplicate_work_or_notifications() {
    let (fixture, mut client) = boot_supervisor("manager-convergence-token").await;
    fixture.allow_autonomous_coordination();
    let created = client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-convergence-create", "observe", true),
        )
        .await
        .unwrap();
    let plan_id = created.structured["plan"]["planId"]
        .as_str()
        .unwrap()
        .to_string();

    fixture.orch.drive_manager_supervisor_once().await;
    let plan = fixture
        .fixture_store
        .load_manager_plan(&plan_id)
        .unwrap()
        .unwrap();
    let work_id = plan.steps[0].work_id.clone().unwrap();

    // Drive a terminal outcome the supervisor must notify exactly once.
    let mut work = fixture
        .fixture_store
        .load_work_item(&work_id)
        .unwrap()
        .unwrap();
    work.state = WorkState::Succeeded;
    work.result = Some(WorkResult {
        summary: "fixture succeeded".into(),
        evidence: vec![],
        artifacts: vec![],
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
    });
    work.bump_at(Utc::now());
    fixture.fixture_store.save_work_item(&work).unwrap();

    for _ in 0..5 {
        fixture.orch.drive_manager_supervisor_once().await;
    }
    let work_for_plan = fixture
        .fixture_store
        .list_work_items()
        .unwrap()
        .into_iter()
        .filter(|item| item.source_manager_plan_id.as_deref() == Some(plan_id.as_str()))
        .count();
    assert_eq!(
        work_for_plan, 1,
        "repeated passes must not materialize the step twice"
    );
    let status_messages = fixture
        .fixture_store
        .list_recent_messages(fixture.lane_id, &fixture.workspace_text, None, None, 100)
        .unwrap()
        .messages
        .into_iter()
        .filter(|message| message.kind.as_str() == "status")
        .count();
    assert_eq!(
        status_messages, 1,
        "one Work revision must produce one notification across repeated passes"
    );
    assert_eq!(
        fixture
            .fixture_store
            .load_manager_plan(&plan_id)
            .unwrap()
            .unwrap()
            .state,
        ManagerPlanState::Succeeded
    );
    client.close_session().await.unwrap();
    fixture.orch.stop_background_tasks().await;
}

/// Autonomous coordination is a host gate, not a prompt instruction. Every
/// way the manager Agent's captured policy falls short must fail closed, and
/// manual plans must remain available while it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn autonomous_coordination_fails_closed_without_approval_free_manager_decision_policy() {
    let (fixture, mut client) = boot_supervisor("manager-gate-token").await;

    // Default policy: managed execution is off.
    let denied = client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-gate-disabled", "disabled", true),
        )
        .await
        .unwrap_err();
    assert_remote_error(denied, "forbidden_scope");

    // Manual coordination stays available under the same policy, so the gate
    // is specific to autonomous operation.
    client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-gate-manual", "manual", false),
        )
        .await
        .unwrap();

    // Enabled, but execution still waits for an operator authorization.
    fixture
        .fixture_store
        .revise_agent_spec(
            &fixture.manager_agent_id,
            "supervisor-certification",
            |spec| {
                spec.managed_execution.enabled = true;
                spec.managed_execution.requires_approval_before_execution = true;
                Ok(())
            },
        )
        .unwrap();
    let denied = client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-gate-approval", "approval", true),
        )
        .await
        .unwrap_err();
    assert_remote_error(denied, "forbidden_scope");

    // Approval-free, but the policy does not admit manager-decision Work.
    fixture
        .fixture_store
        .revise_agent_spec(
            &fixture.manager_agent_id,
            "supervisor-certification",
            |spec| {
                spec.managed_execution.requires_approval_before_execution = false;
                spec.managed_execution.allowed_work_kinds = vec!["verification".into()];
                Ok(())
            },
        )
        .unwrap();
    let denied = client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-gate-kind", "kind", true),
        )
        .await
        .unwrap_err();
    assert_remote_error(denied, "forbidden_scope");

    // A refused autonomous request must leave no durable plan behind.
    assert_eq!(
        fixture
            .fixture_store
            .list_manager_plans()
            .unwrap()
            .iter()
            .filter(|plan| plan.coordination.autonomous())
            .count(),
        0
    );

    fixture.allow_autonomous_coordination();
    let allowed = client
        .call_tool(
            "ptah_create_manager_plan",
            fixture.create_plan_arguments("manager-gate-allowed", "allowed", true),
        )
        .await
        .unwrap();
    assert_eq!(
        allowed.structured["plan"]["coordination"]["mode"],
        "autonomous"
    );
    client.close_session().await.unwrap();
    fixture.orch.stop_background_tasks().await;
}
