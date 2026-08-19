use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, WorkItem, WorkPolicy, WorkProgress, WorkResult, WorkState,
};
use tempfile::tempdir;
use uuid::Uuid;

fn new_work(store_path: &std::path::Path, objective: &str) -> (OrchStore, WorkItem) {
    let store = OrchStore::open(store_path).expect("open workload store");
    let item = WorkItem::new(
        "test",
        objective,
        Uuid::new_v4(),
        "/tmp/project",
        "test-operator",
        WorkPolicy::default(),
    )
    .expect("construct work item");
    (store, item)
}

fn success_result(summary: &str) -> WorkResult {
    WorkResult {
        summary: summary.into(),
        evidence: vec!["deterministic test evidence".into()],
        artifacts: Vec::new(),
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
    }
}

#[test]
fn claim_release_and_complete_are_durable_and_token_scoped() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "claim me");
    store.save_work_item(&item).unwrap();

    let claim = store.claim_work(&item.work_id, "worker-a", None).unwrap();
    assert_eq!(claim.work.state, WorkState::Leased);
    assert!(store.claim_work(&item.work_id, "worker-b", None).is_err());
    assert!(store
        .renew_work_lease(
            &item.work_id,
            &claim.attempt.attempt_id,
            "wrong-token",
            None,
        )
        .is_err());

    let (_, released) = store
        .release_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "worker paused",
        )
        .unwrap();
    assert_eq!(
        released.state,
        grokptah_agent_bridge::orchestration::AttemptState::Released
    );

    let retry = store.claim_work(&item.work_id, "worker-b", None).unwrap();
    let (completed, attempt) = store
        .complete_work(
            &item.work_id,
            &retry.attempt.attempt_id,
            &retry.lease_token,
            success_result("completed once"),
        )
        .unwrap();
    assert_eq!(completed.state, WorkState::Succeeded);
    assert_eq!(
        attempt.state,
        grokptah_agent_bridge::orchestration::AttemptState::Succeeded
    );

    drop(store);
    let reopened = OrchStore::open(home.path()).unwrap();
    assert_eq!(
        reopened
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
    assert_eq!(
        reopened
            .list_work_attempts(Some(&item.work_id))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn expired_lease_requeues_without_duplicate_live_attempt() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "expire me");
    store.save_work_item(&item).unwrap();

    let first = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();
    thread::sleep(Duration::from_millis(10));
    let second = store.claim_work(&item.work_id, "worker-b", None).unwrap();
    assert_ne!(first.attempt.attempt_id, second.attempt.attempt_id);
    assert_eq!(second.attempt.attempt_number, 2);
    assert_eq!(
        store
            .list_work_attempts(Some(&item.work_id))
            .unwrap()
            .iter()
            .filter(|attempt| attempt.state.is_active())
            .count(),
        1
    );
}

#[test]
fn expired_lease_is_recoverable_after_store_reopen() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "recover after restart");
    store.save_work_item(&item).unwrap();
    let first = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();
    drop(store);
    thread::sleep(Duration::from_millis(10));

    let reopened = OrchStore::open(home.path()).unwrap();
    let second = reopened
        .claim_work(&item.work_id, "worker-b", None)
        .unwrap();
    assert_eq!(
        second.attempt.attempt_number,
        first.attempt.attempt_number + 1
    );
    assert_eq!(second.work.state, WorkState::Leased);
}

#[test]
fn concurrent_claim_race_has_exactly_one_winner() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "one winner");
    store.save_work_item(&item).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let first_store = store.clone();
    let second_store = store.clone();
    let first_barrier = barrier.clone();
    let second_barrier = barrier.clone();
    let work_id = item.work_id.clone();
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_store.claim_work(&work_id, "worker-a", None)
    });
    let work_id = item.work_id.clone();
    let second = thread::spawn(move || {
        second_barrier.wait();
        second_store.claim_work(&work_id, "worker-b", None)
    });
    barrier.wait();

    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
}

#[test]
fn approval_policy_stops_completion_at_awaiting_approval() {
    let home = tempdir().unwrap();
    let policy = WorkPolicy {
        requires_approval: true,
        ..WorkPolicy::default()
    };
    let store = OrchStore::open(home.path()).unwrap();
    let item = WorkItem::new(
        "test",
        "approval gate",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        policy,
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let (completed, attempt) = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            success_result("awaiting human review"),
        )
        .unwrap();
    assert_eq!(completed.state, WorkState::AwaitingApproval);
    assert_eq!(
        attempt.state,
        grokptah_agent_bridge::orchestration::AttemptState::AwaitingApproval
    );
    assert!(!completed.state.is_terminal());
}

#[test]
fn dependency_blocks_then_unblocks_work() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let dependency = WorkItem::new(
        "test",
        "dependency",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&dependency).unwrap();
    let mut dependent = WorkItem::new(
        "test",
        "dependent",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    dependent
        .dependencies
        .push(grokptah_agent_bridge::orchestration::WorkDependency {
            work_id: dependency.work_id.clone(),
            required_state: WorkState::Succeeded,
        });
    dependent.validate().unwrap();
    store.save_work_item(&dependent).unwrap();

    assert!(store
        .claim_work(&dependent.work_id, "worker", None)
        .is_err());
    let dependency_claim = store
        .claim_work(&dependency.work_id, "worker", None)
        .unwrap();
    store
        .complete_work(
            &dependency.work_id,
            &dependency_claim.attempt.attempt_id,
            &dependency_claim.lease_token,
            success_result("dependency complete"),
        )
        .unwrap();

    let dependent_claim = store
        .claim_work(&dependent.work_id, "worker", None)
        .unwrap();
    store
        .report_work_progress(
            &dependent.work_id,
            &dependent_claim.attempt.attempt_id,
            &dependent_claim.lease_token,
            WorkProgress {
                summary: "started dependent work".into(),
                percent: Some(20),
                updated_at: Utc::now(),
            },
        )
        .unwrap();
}
