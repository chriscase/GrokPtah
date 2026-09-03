use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ManagerDecisionRecord, ManagerDecisionState, ManagerPlan, ManagerStepSpec, OrchErrorCode,
    OrchStore, WorkDependency, WorkItem, WorkPolicy, WorkState, MANAGER_SCHEMA_VERSION,
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
    root.is_container = true;
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
    assert!(reopened
        .claim_work(&root.work_id, "operator", None)
        .is_err());
}

#[test]
fn concurrent_same_revision_advances_commit_one_child() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-cas";
    let store = OrchStore::open(home.path()).unwrap();
    let now = Utc::now();
    let root = WorkItem::new(
        "manager-plan",
        "cas objective",
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
        "cas objective",
        root.work_id,
        vec![step("only")],
        1,
        2,
        now,
    )
    .unwrap();
    store.save_manager_plan(&plan).unwrap();
    let mut left = plan.clone();
    let mut right = plan.clone();
    let left_work = left.advance(&[], "left", now).unwrap();
    let right_work = right.advance(&[], "right", now).unwrap();
    store
        .save_manager_plan_with_work_cas(&left, plan.revision, &left_work)
        .unwrap();
    assert!(store
        .save_manager_plan_with_work_cas(&right, plan.revision, &right_work)
        .is_err());
    let manager_children = store
        .list_work_items()
        .unwrap()
        .into_iter()
        .filter(|work| work.source_manager_plan_id.as_deref() == Some(&plan.plan_id))
        .collect::<Vec<_>>();
    assert_eq!(manager_children.len(), 1);
    assert_eq!(manager_children[0].work_id, left_work[0].work_id);
}

/// Manager materialization must run the canonical graph authority over the full
/// proposed batch before the first durable write. A refusal must not leave a
/// partial adoption behind.
#[test]
fn manager_materialization_validates_the_full_batch_before_writing() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-batch-validation";
    let store = OrchStore::open(home.path()).unwrap();
    let now = Utc::now();
    let root = WorkItem::new(
        "manager-plan",
        "batch objective",
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
        "batch objective",
        root.work_id.clone(),
        vec![step("a"), step("b")],
        2,
        2,
        now,
    )
    .unwrap();
    store.save_manager_plan(&plan).unwrap();

    let mut valid = WorkItem::new(
        "coding",
        "first materialized step",
        session_id,
        workspace,
        "manager",
        WorkPolicy::default(),
    )
    .unwrap();
    valid.work_id = "manager-valid".into();
    let mut invalid = WorkItem::new(
        "coding",
        "second materialized step",
        session_id,
        workspace,
        "manager",
        WorkPolicy::default(),
    )
    .unwrap();
    invalid.work_id = "manager-invalid".into();
    invalid.dependencies.push(WorkDependency {
        work_id: "no-such-work".into(),
        required_state: WorkState::Succeeded,
    });
    invalid.source_manager_plan_id = Some(plan.plan_id.clone());
    invalid.source_manager_step_id = Some("b".into());

    let error = store
        .save_manager_plan_with_work_cas(&plan, plan.revision, &[valid, invalid])
        .expect_err("an invalid graph in the batch must refuse the whole CAS");
    assert_eq!(error.code, OrchErrorCode::InvalidRequest);
    assert!(
        store.load_work_item("manager-valid").unwrap().is_none(),
        "a refused manager batch must not leave the first item behind"
    );
}

#[test]
fn decision_occurrence_and_work_replay_once_after_restart() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-decision-restart";
    let now = Utc::now();
    let mut work = WorkItem::new(
        "manager-decision",
        "return proposal",
        session_id,
        workspace,
        "manager-supervisor",
        WorkPolicy::default(),
    )
    .unwrap();
    work.work_id = "manager-decision-work-stable".into();
    work.parent_work_id = Some("root".into());
    work.assigned_agent_id = Some("manager".into());
    work.assignment_status = grokptah_agent_bridge::orchestration::AssignmentStatus::Accepted;
    work.source_manager_plan_id = Some("plan".into());
    work.source_manager_step_id = Some("__manager_decision__".into());
    work.validate().unwrap();
    let decision = ManagerDecisionRecord {
        schema_version: MANAGER_SCHEMA_VERSION,
        decision_id: "manager-decision-stable".into(),
        plan_id: "plan".into(),
        expected_plan_revision: 3,
        manager_agent_id: "manager".into(),
        agent_spec_revision: 1,
        triggering_work_ids: vec!["failed-work".into()],
        triggering_message_ids: vec![],
        input_snapshot_hash: "snapshot-hash".into(),
        decision_work_id: work.work_id.clone(),
        run_id: None,
        state: ManagerDecisionState::AwaitingResult,
        proposed_directive: None,
        outcome: None,
        applied_mutation_ids: vec![],
        created_at: now,
        updated_at: now,
    };
    let store = OrchStore::open(home.path()).unwrap();
    store
        .save_manager_decision_with_work(&decision, &work)
        .unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    reopened
        .save_manager_decision_with_work(&decision, &work)
        .unwrap();
    assert_eq!(
        reopened.list_manager_decisions(Some("plan")).unwrap().len(),
        1
    );
    assert_eq!(
        reopened
            .list_work_items()
            .unwrap()
            .into_iter()
            .filter(|item| item.kind == "manager-decision")
            .count(),
        1
    );
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

#[test]
fn notification_fence_survives_store_restart() {
    let home = tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let workspace = "/tmp/manager-notification-recovery";
    let store = OrchStore::open(home.path()).unwrap();
    store
        .save_agent(&agent("manager", session_id, workspace))
        .unwrap();
    let now = Utc::now();
    let root = WorkItem::new(
        "manager-plan",
        "recover notifications",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    let mut plan = ManagerPlan::new(
        session_id,
        workspace,
        "manager",
        "recover notifications",
        root.work_id.clone(),
        vec![step("observe")],
        1,
        2,
        now,
    )
    .unwrap();
    let mut created = plan.advance(&[], "operator", now).unwrap();
    let mut completed = created[0].clone();
    completed.state = WorkState::Succeeded;
    completed.bump_at(now);
    plan.advance(&[completed.clone()], "operator", now).unwrap();
    let pending = plan.pending_notifications(&[completed.clone()]);
    assert_eq!(pending.len(), 1);
    plan.mark_notifications_sent(
        &[(
            pending[0].step_id.clone(),
            pending[0].work_revision,
            "notification-1".into(),
        )],
        now,
    )
    .unwrap();
    created[0] = completed.clone();
    store.save_work_item(&root).unwrap();
    store.save_manager_plan_with_work(&plan, &created).unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let loaded = reopened.load_manager_plan(&plan.plan_id).unwrap().unwrap();
    assert!(loaded.pending_notifications(&[completed]).is_empty());
}
