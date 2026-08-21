use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ManagerPlan, ManagerStepSpec, OrchStore, WorkItem, WorkPolicy,
};
use grokptah_agent_bridge::{AgentRecord, AgentState};
use tempfile::tempdir;
use uuid::Uuid;

fn agent(agent_id: &str, session_id: Uuid, workspace: &str) -> AgentRecord {
    let now = Utc::now();
    AgentRecord {
        agent_id: agent_id.into(),
        owner_principal_id: None,
        session_id,
        lane_ids: vec![session_id],
        lane_associations: Vec::new(),
        workspace: workspace.into(),
        model: "grok".into(),
        spec: None,
        state: AgentState::Waiting,
        current_run_id: None,
        last_run_id: None,
        last_lane_id: Some(session_id),
        latest_checkpoint_id: None,
        continuation_ordinal: 0,
        created_at: now,
        updated_at: now,
    }
}

fn step(step_id: &str) -> ManagerStepSpec {
    ManagerStepSpec {
        step_id: step_id.into(),
        kind: "coding".into(),
        objective: format!("complete {step_id}"),
        priority: 0,
        dependencies: Vec::new(),
        assigned_agent_id: None,
        policy: WorkPolicy::default(),
    }
}

#[test]
fn manager_plan_and_materialized_work_survive_restart() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-project";
    let store = OrchStore::open(home.path()).unwrap();
    store
        .save_agent(&agent("manager", session_id, workspace))
        .unwrap();
    let now = Utc::now();
    let mut root = WorkItem::new(
        "manager-plan",
        "coordinate the objective",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    root.state = grokptah_agent_bridge::orchestration::WorkState::Blocked;
    root.blocked_reason = Some("container".into());
    root.bump_at(now);
    let plan = ManagerPlan::new(
        session_id,
        workspace,
        "manager",
        "coordinate the objective",
        root.work_id.clone(),
        vec![step("first")],
        1,
        2,
        now,
    )
    .unwrap();
    let mut plan = plan;
    let created = plan.advance(&[], "operator", now).unwrap();
    store.save_manager_plan_with_work(&plan, &created).unwrap();
    store.save_work_item(&root).unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let loaded = reopened
        .load_manager_plan(&plan.plan_id)
        .unwrap()
        .expect("manager plan should persist");
    assert_eq!(loaded.steps[0].work_id, plan.steps[0].work_id);
    assert_eq!(reopened.list_work_items().unwrap().len(), 2);
    assert_eq!(reopened.list_manager_plans().unwrap().len(), 1);
}

#[test]
fn advance_adopts_a_child_written_before_plan_revision() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-recovery";
    let store = OrchStore::open(home.path()).unwrap();
    store
        .save_agent(&agent("manager", session_id, workspace))
        .unwrap();
    let now = Utc::now();
    let root = WorkItem::new(
        "manager-plan",
        "recover the objective",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    let plan = ManagerPlan::new(
        session_id,
        workspace,
        "manager",
        "recover the objective",
        root.work_id.clone(),
        vec![step("recoverable")],
        1,
        2,
        now,
    )
    .unwrap();
    let mut orphan = WorkItem::new(
        "coding",
        "complete recoverable",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    orphan.parent_work_id = Some(root.work_id.clone());
    orphan.source_manager_plan_id = Some(plan.plan_id.clone());
    orphan.source_manager_step_id = Some("recoverable".into());
    orphan.validate().unwrap();
    store.save_work_item(&root).unwrap();
    store.save_work_item(&orphan).unwrap();
    store.save_manager_plan(&plan).unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let mut loaded = reopened.load_manager_plan(&plan.plan_id).unwrap().unwrap();
    let created = loaded
        .advance(&reopened.list_work_items().unwrap(), "operator", now)
        .unwrap();
    assert!(created.is_empty());
    assert_eq!(
        loaded.steps[0].work_id.as_deref(),
        Some(orphan.work_id.as_str())
    );
}
