use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    intersect_run_bounds, managed_execution_eligible, AssignmentStatus, ManagedExecutionPolicy,
    ManagedIntentState, ManagedWorkMode, OrchErrorCode, OrchStore, RunBounds, WorkItem, WorkPolicy,
    WorkProgress, WorkResult, WorkState,
};
use grokptah_agent_bridge::{AgentRecord, AgentState};
use tempfile::tempdir;
use uuid::Uuid;

fn agent(id: &str, workspace: &str, session_id: Uuid) -> AgentRecord {
    let now = Utc::now();
    AgentRecord {
        agent_id: id.into(),
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

fn accepted_work(session_id: Uuid, workspace: &str, agent_id: &str) -> WorkItem {
    let mut item = WorkItem::new(
        "native",
        "Execute natively",
        session_id,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    item.assigned_agent_id = Some(agent_id.into());
    item.assignment_status = AssignmentStatus::Accepted;
    item
}

#[test]
fn legacy_agent_spec_deserializes_as_manual_only() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let loaded = store.load_agent("worker-a").unwrap().unwrap();
    let spec = loaded.current_spec().unwrap();
    assert!(!spec.managed_execution.enabled);
}

#[test]
fn enabling_managed_execution_requires_a_spec_revision() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let first = store.load_agent("worker-a").unwrap().unwrap();
    let before = first.current_spec().unwrap().revision;
    let updated = store
        .revise_agent_spec("worker-a", "operator", |spec| {
            spec.managed_execution.enabled = true;
            spec.managed_execution.bounds.max_total_tokens = Some(8_000);
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(updated.current_spec().unwrap().managed_execution.enabled);
    assert_eq!(updated.current_spec().unwrap().revision, before + 1);
}

#[test]
fn manual_only_and_foreign_agents_are_not_eligible() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let other = Uuid::new_v4();
    store
        .save_agent(&agent("local", "/tmp/ws", session))
        .unwrap();
    store
        .save_agent(&agent("foreign", "/tmp/other", other))
        .unwrap();
    let local = store.load_agent("local").unwrap().unwrap();
    let spec = local.current_spec().unwrap().clone();
    let work = accepted_work(session, "/tmp/ws", "local");
    let ceiling = RunBounds::default();
    assert!(managed_execution_eligible(&work, &local, &spec, &[], 0, &ceiling).is_err());
    let mut enabled = spec.clone();
    enabled.managed_execution.enabled = true;
    enabled.managed_execution.bounds.max_total_tokens = Some(4_000);
    assert!(managed_execution_eligible(&work, &local, &enabled, &[], 0, &ceiling).is_ok());
    let foreign = store.load_agent("foreign").unwrap().unwrap();
    assert_eq!(
        managed_execution_eligible(&work, &foreign, &enabled, &[], 0, &ceiling)
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );
    let mut forbidden = work.clone();
    forbidden.policy.managed_execution = ManagedWorkMode::Forbid;
    assert!(managed_execution_eligible(&forbidden, &local, &enabled, &[], 0, &ceiling).is_err());
}

#[test]
fn bounds_intersection_never_widens() {
    let server = RunBounds {
        max_prompt_bytes: 10_000,
        max_rounds: 10,
        max_duration_ms: 60_000,
        max_total_tokens: Some(20_000),
    };
    let agent = RunBounds {
        max_prompt_bytes: 8_000,
        max_rounds: 20,
        max_duration_ms: 30_000,
        max_total_tokens: Some(50_000),
    };
    let work = RunBounds {
        max_prompt_bytes: 12_000,
        max_rounds: 4,
        max_duration_ms: 90_000,
        max_total_tokens: None,
    };
    let out = intersect_run_bounds(&[&server, &agent, &work]);
    assert_eq!(out.max_prompt_bytes, 8_000);
    assert_eq!(out.max_rounds, 4);
    assert_eq!(out.max_duration_ms, 30_000);
    assert_eq!(out.max_total_tokens, Some(20_000));
}

#[test]
fn abandoned_claiming_intent_returns_work_to_queued() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    let intent = grokptah_agent_bridge::orchestration::ManagedExecutionIntent {
        schema_version: grokptah_agent_bridge::orchestration::MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: "intent-1".into(),
        agent_id: "worker-a".into(),
        agent_spec_revision: 1,
        work_id: item.work_id.clone(),
        work_revision: claim.work.revision,
        attempt_id: Some(claim.attempt.attempt_id.clone()),
        run_id: None,
        session_id: session,
        workspace: "/tmp/ws".into(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        bounds: RunBounds::default(),
        input_hash: "abc".into(),
        state: ManagedIntentState::Claiming,
        permission_request_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.save_managed_intent(&intent).unwrap();
    store
        .abandon_managed_intent(&intent.intent_id, Utc::now())
        .unwrap();
    let restored = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(restored.state, WorkState::Queued);
}

#[test]
fn park_and_unknown_claim_do_not_create_a_second_live_run_link() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, "worker-a", None, "secret")
        .unwrap();
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "run-1",
        )
        .unwrap();
    store
        .report_work_progress(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkProgress {
                summary: "working".into(),
                percent: Some(10),
                updated_at: Utc::now(),
            },
        )
        .unwrap();
    let (parked, _) = store
        .park_work_input(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "permission required: shell",
        )
        .unwrap();
    assert_eq!(parked.state, WorkState::AwaitingInput);
    assert!(store.claim_work(&item.work_id, "other", None).is_err());
}

#[test]
fn two_store_opens_cannot_both_dispatch() {
    let home = tempdir().unwrap();
    let _first = OrchStore::open(home.path()).unwrap();
    assert!(OrchStore::open(home.path()).is_err());
}

#[test]
fn expired_attempt_rejects_late_managed_completion() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let item = accepted_work(session, "/tmp/ws", "worker-a");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let result = WorkResult {
        summary: "late".into(),
        evidence: Vec::new(),
        artifacts: Vec::new(),
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
    };
    assert!(store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result,
        )
        .is_err());
}

#[test]
fn default_policy_is_disabled() {
    assert!(!ManagedExecutionPolicy::default().enabled);
}
