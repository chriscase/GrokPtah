use chrono::{Duration, Utc};
use grokptah_agent_bridge::orchestration::{
    AssignmentStatus, MessageKind, OrchStore, WorkDecisionAction, WorkItem, WorkMessage, WorkPolicy,
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

fn work(session_id: Uuid, workspace: &str) -> WorkItem {
    WorkItem::new(
        "child",
        "Do the child task",
        session_id,
        workspace,
        "coordinator",
        WorkPolicy::default(),
    )
    .unwrap()
}

#[test]
fn offer_accept_and_duplicate_assignment_are_idempotent_at_store() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let item = work(session, "/tmp/ws");
    store.save_work_item(&item).unwrap();
    let now = Utc::now();
    let (offered, decision) = store
        .offer_work(
            &item.work_id,
            "worker-a",
            "coord",
            "please take this",
            Some(1),
            now,
        )
        .unwrap();
    assert_eq!(offered.assignment_status, AssignmentStatus::Offered);
    assert_eq!(decision.action, WorkDecisionAction::Offer);
    assert!(store.claim_work(&item.work_id, "worker-a", None).is_err());
    let (accepted, _) = store
        .accept_work(
            &item.work_id,
            "worker-a",
            "worker-a",
            "accepted",
            Some(offered.revision),
            now,
        )
        .unwrap();
    assert_eq!(accepted.assignment_status, AssignmentStatus::Accepted);
    assert!(store.claim_work(&item.work_id, "worker-a", None).is_ok());
}

#[test]
fn cross_workspace_projection_does_not_list_foreign_workers() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("local", "/tmp/ws-a", session))
        .unwrap();
    store
        .save_agent(&agent("foreign", "/tmp/ws-b", session))
        .unwrap();
    let workers = store.list_workers("/tmp/ws-a", Utc::now()).unwrap();
    assert_eq!(workers.len(), 1);
    assert_eq!(workers[0].agent_id, "local");
    assert_eq!(
        workers[0].liveness,
        grokptah_agent_bridge::orchestration::WorkerLivenessState::Unknown
    );
}

#[test]
fn stale_liveness_is_explicit_and_not_a_lease() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    let past = Utc::now() - Duration::minutes(5);
    store
        .heartbeat_worker(
            "worker-a",
            "token",
            grokptah_agent_bridge::orchestration::WorkerHostKind::Service,
            past,
        )
        .unwrap();
    let worker = store.get_worker("worker-a", Utc::now()).unwrap().unwrap();
    assert_eq!(
        worker.liveness,
        grokptah_agent_bridge::orchestration::WorkerLivenessState::Stale
    );
    assert!(worker.active_leases.is_empty());
}

#[test]
fn messages_are_ordered_redacted_and_cursor_bounded() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let now = Utc::now();
    let mut first = WorkMessage::new(
        MessageKind::Question,
        "worker-a",
        Some("worker-a".into()),
        Some("manager".into()),
        session,
        "/tmp/ws",
        Some("work-1".into()),
        "Need a decision",
        Some(serde_json::json!({"token": "secret"})),
        now,
    )
    .unwrap();
    first = store.send_message(first).unwrap();
    assert_eq!(first.payload.unwrap()["token"], "[redacted]");
    let second = WorkMessage::new(
        MessageKind::Answer,
        "manager",
        Some("manager".into()),
        Some("worker-a".into()),
        session,
        "/tmp/ws",
        Some("work-1".into()),
        "Use the existing plan",
        None,
        now,
    )
    .unwrap();
    store.send_message(second).unwrap();
    let page = store
        .list_messages(session, "/tmp/ws", 0, Some("manager"), None, 10)
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].kind, MessageKind::Question);
    let expired = store
        .list_messages(session, "/tmp/ws", 0, Some("missing"), None, 10)
        .unwrap();
    assert!(expired.messages.is_empty());
}

#[test]
fn expired_question_cannot_be_answered() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let now = Utc::now() - Duration::hours(1);
    let mut question = WorkMessage::new(
        MessageKind::Question,
        "worker-a",
        Some("worker-a".into()),
        Some("manager".into()),
        session,
        "/tmp/ws",
        None,
        "old question",
        None,
        now,
    )
    .unwrap();
    question.expires_at = Some(now + Duration::minutes(1));
    let question = store.send_message(question).unwrap();
    assert!(question.expired_at(Utc::now()));
}

#[test]
fn concurrent_reassign_uses_revision_fence() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    store
        .save_agent(&agent("worker-a", "/tmp/ws", session))
        .unwrap();
    store
        .save_agent(&agent("worker-b", "/tmp/ws", session))
        .unwrap();
    let item = work(session, "/tmp/ws");
    store.save_work_item(&item).unwrap();
    let now = Utc::now();
    store
        .reassign_work(&item.work_id, "worker-a", "coord", "first", Some(1), now)
        .unwrap();
    assert!(store
        .reassign_work(&item.work_id, "worker-b", "coord", "stale", Some(1), now)
        .is_err());
}

#[test]
fn expired_lease_rejects_late_completion() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let item = work(session, "/tmp/ws");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let result = grokptah_agent_bridge::orchestration::WorkResult {
        summary: "too late".into(),
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
fn cancel_wins_over_late_claim() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let session = Uuid::new_v4();
    let item = work(session, "/tmp/ws");
    store.save_work_item(&item).unwrap();
    store
        .cancel_work(&item.work_id, "coordinator cancelled")
        .unwrap();
    assert!(store.claim_work(&item.work_id, "worker-a", None).is_err());
}
