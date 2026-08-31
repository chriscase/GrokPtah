use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use grokptah_agent_bridge::orchestration::{
    safe_id_filename, AttemptState, ManagedExecutionIntent, ManagedFinalizationOutcome,
    ManagedFinalizationStage, ManagedIntentState, OrchErrorCode, OrchStore, RunRecord, RunState,
    WorkClaim, WorkItem, WorkPolicy, WorkProgress, WorkResult, WorkState, WorkloadSupervisor,
    MANAGED_EXECUTION_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{
    CompletionClaims, CompletionEvidence, CompletionObservations, CompletionUsage,
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
        verification: None,
    }
}

fn verified_evidence(work_id: &str, run_id: &str, attempt_id: &str) -> CompletionEvidence {
    CompletionEvidence {
        status: "verified".into(),
        stop_reason: "completed".into(),
        interrupted: false,
        claims: CompletionClaims {
            present: true,
            mentions_changes: true,
            mentions_tests: true,
            mentions_verification: true,
        },
        observations: CompletionObservations {
            changed_files: 1,
            tests_observed: 1,
            tests_passed: 1,
            ..CompletionObservations::default()
        },
        usage: CompletionUsage::default(),
        work_id: Some(work_id.into()),
        run_id: Some(run_id.into()),
        attempt_id: Some(attempt_id.into()),
    }
}

fn unverified_evidence(work_id: &str, run_id: &str, attempt_id: &str) -> CompletionEvidence {
    let mut evidence = verified_evidence(work_id, run_id, attempt_id);
    evidence.status = "unverified".into();
    evidence.claims.mentions_verification = false;
    evidence.observations.tests_observed = 0;
    evidence.observations.tests_passed = 0;
    evidence
}

fn failed_test_evidence(work_id: &str, run_id: &str, attempt_id: &str) -> CompletionEvidence {
    let mut evidence = verified_evidence(work_id, run_id, attempt_id);
    evidence.status = "failed".into();
    evidence.observations.tests_passed = 0;
    evidence.observations.tests_failed = 1;
    evidence
}

fn result_with(summary: &str, evidence: CompletionEvidence) -> WorkResult {
    WorkResult {
        summary: summary.into(),
        evidence: vec!["deterministic test evidence".into()],
        artifacts: Vec::new(),
        failure: None,
        cancellation_reason: None,
        completed_at: Utc::now(),
        verification: Some(evidence),
    }
}

fn fixture_run(
    run_id: &str,
    session_id: Uuid,
    workspace: &str,
    evidence: Option<CompletionEvidence>,
) -> RunRecord {
    let aggregates = grokptah_agent_bridge::orchestration::RunAggregates {
        verification: evidence,
        ..Default::default()
    };
    RunRecord {
        run_id: run_id.into(),
        session_id,
        workspace: workspace.into(),
        request_id: format!("req-{run_id}"),
        client_id: None,
        state: RunState::Completed,
        purpose: Default::default(),
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: Default::default(),
        prompt_preview: "preview".into(),
        start_seq: Some(1),
        end_seq: Some(2),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: Some("completed".into()),
        final_response: Some("Changed src/lib.rs; cargo test passed; verification green.".into()),
        error_code: None,
        stop_cause: None,
        aggregates,
        progress: None,
        execution: None,
        approval: None,
    }
}

fn complete_verified(
    store: &OrchStore,
    item: &WorkItem,
    claim: &WorkClaim,
    summary: &str,
) -> (WorkItem, grokptah_agent_bridge::orchestration::WorkAttempt) {
    let run_id = format!("run-{}", item.work_id);
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run_id,
        )
        .unwrap();
    let evidence = verified_evidence(&item.work_id, &run_id, &claim.attempt.attempt_id);
    store
        .save_run(&fixture_run(
            &run_id,
            item.session_id,
            &item.workspace,
            Some(evidence.clone()),
        ))
        .unwrap();
    store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result_with(summary, evidence),
        )
        .unwrap()
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
    let (completed, attempt) = complete_verified(&store, &item, &retry, "completed once");
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
    let report = store
        .reconcile_workloads_at(claim.attempt.lease_expires_at + ChronoDuration::days(365))
        .unwrap();
    assert_eq!(report.expired_attempts, 0);
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::AwaitingApproval
    );
    assert_eq!(
        store.list_work_attempts(Some(&item.work_id)).unwrap()[0].state,
        AttemptState::AwaitingApproval
    );

    let (approved, approved_attempt) = store
        .approve_work(
            &item.work_id,
            "reviewer-1",
            Some("reviewed the evidence".into()),
            Some(completed.revision),
        )
        .unwrap();
    assert_eq!(approved.state, WorkState::Succeeded);
    assert_eq!(
        approved.approval.as_ref().unwrap().reviewer_id,
        "reviewer-1"
    );
    assert_eq!(
        approved_attempt.state,
        grokptah_agent_bridge::orchestration::AttemptState::Succeeded
    );
}

#[test]
fn assignment_and_manual_retry_use_revision_fences_and_preserve_history() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let policy = WorkPolicy {
        retry: grokptah_agent_bridge::orchestration::WorkRetryPolicy {
            max_attempts: 2,
            retry_failed: false,
            ..Default::default()
        },
        ..WorkPolicy::default()
    };
    let item = WorkItem::new(
        "test",
        "retry after review",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        policy,
    )
    .unwrap();
    store.save_work_item(&item).unwrap();

    let assigned = store
        .assign_work(&item.work_id, Some("agent-1".into()), Some(item.revision))
        .unwrap();
    assert_eq!(assigned.assigned_agent_id.as_deref(), Some("agent-1"));
    assert!(store
        .assign_work(&item.work_id, None, Some(item.revision))
        .is_err());

    let claim = store.claim_work(&item.work_id, "agent-1", None).unwrap();
    let (failed, _) = store
        .fail_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkResult {
                summary: "worker failed".into(),
                evidence: Vec::new(),
                artifacts: Vec::new(),
                failure: Some("fixture failure".into()),
                cancellation_reason: None,
                completed_at: Utc::now(),
                verification: None,
            },
        )
        .unwrap();
    assert_eq!(failed.state, WorkState::Failed);

    let retried = store
        .retry_work(
            &item.work_id,
            "operator approved retry",
            Some(failed.revision),
        )
        .unwrap();
    assert_eq!(retried.state, WorkState::Queued);
    assert_eq!(retried.attempt_count, 1);
    assert_eq!(
        store.list_work_attempts(Some(&item.work_id)).unwrap().len(),
        1
    );
}

#[test]
fn dependency_blocks_then_unblocks_work() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    // One lane. A dependency is resolved inside the depending item's session
    // and workspace, so both items are created in the same one; an edge that
    // crossed sessions used to resolve, which is the leak
    // `work_graph_authority.rs` now holds closed.
    let lane = Uuid::new_v4();
    let dependency = WorkItem::new(
        "test",
        "dependency",
        lane,
        "/tmp/project",
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&dependency).unwrap();
    let mut dependent = WorkItem::new(
        "test",
        "dependent",
        lane,
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
    complete_verified(
        &store,
        &dependency,
        &dependency_claim,
        "dependency complete",
    );

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

#[test]
fn reconciliation_expires_leases_and_requeues_with_deterministic_time() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "reconcile me");
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work(&item.work_id, "worker-a", Some(1))
        .unwrap();

    let report = store
        .reconcile_workloads_at(claim.attempt.lease_expires_at + ChronoDuration::milliseconds(1))
        .unwrap();
    assert_eq!(report.scanned_items, 1);
    assert_eq!(report.expired_attempts, 1);
    assert_eq!(report.retried_items, 1);
    assert_eq!(report.failed_items, 0);
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Queued
    );
    assert_eq!(
        store.list_work_attempts(Some(&item.work_id)).unwrap()[0].state,
        grokptah_agent_bridge::orchestration::AttemptState::Expired
    );
}

#[test]
fn reconciliation_fails_deadlines_and_reports_dependency_transitions() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    let dependency = WorkItem::new(
        "test",
        "dependency",
        lane,
        "/tmp/project",
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&dependency).unwrap();

    let mut dependent = WorkItem::new(
        "test",
        "dependent",
        lane,
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
    store.save_work_item(&dependent).unwrap();

    let first = store.reconcile_workloads_at(Utc::now()).unwrap();
    assert_eq!(first.blocked_items, 1);
    assert_eq!(
        store
            .load_work_item(&dependent.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Blocked
    );
    let held = store.load_work_item(&dependent.work_id).unwrap().unwrap();
    assert_eq!(held.blocked_reason.as_deref(), Some("dependencies_pending"));
    assert_eq!(
        held.block_provenance,
        Some(grokptah_agent_bridge::orchestration::BlockProvenance::Derived)
    );

    let dependency_claim = store
        .claim_work(&dependency.work_id, "worker", None)
        .unwrap();
    complete_verified(
        &store,
        &dependency,
        &dependency_claim,
        "dependency complete",
    );
    let second = store.reconcile_workloads_at(Utc::now()).unwrap();
    assert_eq!(second.unblocked_items, 1);

    let mut deadline = WorkItem::new(
        "test",
        "deadline",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    deadline.deadline = Some(Utc::now() - ChronoDuration::seconds(1));
    store.save_work_item(&deadline).unwrap();
    let deadline_report = store.reconcile_workloads().unwrap();
    assert_eq!(deadline_report.deadline_failed_items, 1);
    assert_eq!(
        store
            .load_work_item(&deadline.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Failed
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workload_supervisor_runs_and_reports_success() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let supervisor = WorkloadSupervisor::start(store, Duration::from_millis(5))
        .expect("workload supervisor thread should start");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let status = supervisor.status();
    assert!(status.enabled);
    assert!(status.last_run_at.is_some());
    assert!(status.last_success_at.is_some());
    assert!(status.last_error.is_none());
}

#[test]
fn allowed_files_policy_survives_store_reload_and_binds_to_run() {
    let home = tempdir().unwrap();
    let policy = WorkPolicy {
        allowed_files: vec!["src/only.rs".into()],
        ..WorkPolicy::default()
    };
    let store = OrchStore::open(home.path()).unwrap();
    let item = WorkItem::new(
        "coding",
        "scoped writes",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        policy.clone(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "run-scoped-1",
        )
        .unwrap();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let loaded = reopened.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(loaded.policy.allowed_files, vec!["src/only.rs".to_string()]);
    assert!(loaded.policy.denies_shell());
    let bound = reopened.work_item_for_run("run-scoped-1").unwrap().unwrap();
    assert_eq!(bound.work_id, item.work_id);
    assert_eq!(bound.policy.allowed_files, loaded.policy.allowed_files);
    assert!(reopened
        .work_item_for_run("run-unrelated")
        .unwrap()
        .is_none());
}

#[test]
fn work_file_policy_does_not_leak_across_works_and_fails_closed_if_missing() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let policy_a = WorkPolicy {
        allowed_files: vec!["shared.txt".into()],
        ..WorkPolicy::default()
    };
    let policy_b = WorkPolicy {
        allowed_files: vec!["other.txt".into()],
        ..WorkPolicy::default()
    };
    let work_a = WorkItem::new(
        "coding",
        "a",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        policy_a,
    )
    .unwrap();
    let work_b = WorkItem::new(
        "coding",
        "b",
        Uuid::new_v4(),
        "/tmp/project",
        "operator",
        policy_b,
    )
    .unwrap();
    store.save_work_item(&work_a).unwrap();
    store.save_work_item(&work_b).unwrap();
    let claim_a = store.claim_work(&work_a.work_id, "worker", None).unwrap();
    let claim_b = store.claim_work(&work_b.work_id, "worker", None).unwrap();
    store
        .link_work_run(
            &work_a.work_id,
            &claim_a.attempt.attempt_id,
            &claim_a.lease_token,
            "run-a",
        )
        .unwrap();
    store
        .link_work_run(
            &work_b.work_id,
            &claim_b.attempt.attempt_id,
            &claim_b.lease_token,
            "run-b",
        )
        .unwrap();
    let loaded_b = store.work_item_for_run("run-b").unwrap().unwrap();
    assert_eq!(loaded_b.work_id, work_b.work_id);
    assert_eq!(loaded_b.policy.allowed_files, vec!["other.txt".to_string()]);
    assert!(!loaded_b.policy.allowed_files.contains(&"shared.txt".into()));

    let work_a_filename = safe_id_filename(&work_a.work_id).unwrap();
    std::fs::remove_file(
        home.path()
            .join("work-items")
            .join(format!("{work_a_filename}.json")),
    )
    .unwrap();
    let missing = store.work_item_for_run("run-a").unwrap_err();
    assert!(missing.to_string().contains("missing its Work item"));
}

fn bind_and_save_run(
    store: &OrchStore,
    item: &WorkItem,
    claim: &WorkClaim,
    evidence: &CompletionEvidence,
) -> String {
    let run_id = evidence.run_id.clone().expect("bound run id");
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run_id,
        )
        .unwrap();
    store
        .save_run(&fixture_run(
            &run_id,
            item.session_id,
            &item.workspace,
            Some(evidence.clone()),
        ))
        .unwrap();
    run_id
}

#[test]
fn unverified_evidence_refuses_success_and_lands_in_review() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "needs review");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let run_id = format!("run-{}", item.work_id);
    let evidence = unverified_evidence(&item.work_id, &run_id, &claim.attempt.attempt_id);
    bind_and_save_run(&store, &item, &claim, &evidence);
    let (completed, attempt) = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result_with("unverified completion", evidence),
        )
        .unwrap();
    assert_eq!(completed.state, WorkState::Review);
    assert_eq!(attempt.state, AttemptState::Review);
    assert!(completed.approval.is_none());
    let report = store
        .reconcile_workloads_at(claim.attempt.lease_expires_at + ChronoDuration::days(365))
        .unwrap();
    assert_eq!(report.expired_attempts, 0);
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Review
    );
    assert_eq!(
        store.list_work_attempts(Some(&item.work_id)).unwrap()[0].state,
        AttemptState::Review
    );
}

#[test]
fn mismatched_run_attempt_or_work_evidence_is_refused() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "bind me");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let original_state = store.load_work_item(&item.work_id).unwrap().unwrap().state;
    let run_id = format!("run-{}", item.work_id);
    let host = verified_evidence(&item.work_id, &run_id, &claim.attempt.attempt_id);
    bind_and_save_run(&store, &item, &claim, &host);

    let mut wrong_work = host.clone();
    wrong_work.work_id = Some("other-work".into());
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                &claim.lease_token,
                result_with("wrong work", wrong_work),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );

    let mut wrong_attempt = host.clone();
    wrong_attempt.attempt_id = Some("other-attempt".into());
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                &claim.lease_token,
                result_with("wrong attempt", wrong_attempt),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );

    let mut wrong_run = host.clone();
    wrong_run.run_id = Some("other-run".into());
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                &claim.lease_token,
                result_with("wrong run", wrong_run),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );

    let mut tampered_observation = host.clone();
    tampered_observation.observations.changed_files += 1;
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                &claim.lease_token,
                result_with("tampered observation", tampered_observation),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        original_state
    );
}

#[test]
fn failed_test_evidence_never_becomes_succeeded() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "tests failed");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let run_id = format!("run-{}", item.work_id);
    let evidence = failed_test_evidence(&item.work_id, &run_id, &claim.attempt.attempt_id);
    bind_and_save_run(&store, &item, &claim, &evidence);
    let mut forged = evidence.clone();
    forged.status = "verified".into();
    let (completed, _) = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result_with("failed tests", forged),
        )
        .unwrap();
    assert_ne!(completed.state, WorkState::Succeeded);
    assert_eq!(completed.state, WorkState::Review);
}

#[test]
fn verified_evidence_and_durable_approval_are_the_success_paths() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "verified success");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let (completed, attempt) = complete_verified(&store, &item, &claim, "verified");
    assert_eq!(completed.state, WorkState::Succeeded);
    assert_eq!(attempt.state, AttemptState::Succeeded);
    let stored = completed.result.clone().unwrap();
    let replay = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            stored,
        )
        .unwrap();
    assert_eq!(replay.0.state, WorkState::Succeeded);
    assert_eq!(replay.0.revision, completed.revision);
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                &claim.lease_token,
                success_result("different result"),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
    assert_eq!(
        store
            .complete_work(
                &item.work_id,
                &claim.attempt.attempt_id,
                "foreign-lease",
                completed.result.clone().unwrap(),
            )
            .unwrap_err()
            .code,
        OrchErrorCode::ForbiddenScope
    );

    let policy = WorkPolicy {
        requires_approval: true,
        ..WorkPolicy::default()
    };
    let gated = WorkItem::new(
        "test",
        "approval success",
        item.session_id,
        item.workspace.clone(),
        "operator",
        policy,
    )
    .unwrap();
    store.save_work_item(&gated).unwrap();
    let gated_claim = store.claim_work(&gated.work_id, "worker", None).unwrap();
    let (awaiting, _) = store
        .complete_work(
            &gated.work_id,
            &gated_claim.attempt.attempt_id,
            &gated_claim.lease_token,
            success_result("awaiting"),
        )
        .unwrap();
    assert_eq!(awaiting.state, WorkState::AwaitingApproval);
    assert_eq!(
        store
            .approve_work(
                &gated.work_id,
                "other-reviewer",
                None,
                Some(awaiting.revision - 1)
            )
            .unwrap_err()
            .code,
        OrchErrorCode::StaleVersion
    );
    let (approved, approved_attempt) = store
        .approve_work(
            &gated.work_id,
            "reviewer-1",
            Some("looks good".into()),
            Some(awaiting.revision),
        )
        .unwrap();
    assert_eq!(approved.state, WorkState::Succeeded);
    assert_eq!(
        approved.approval.as_ref().unwrap().reviewer_id,
        "reviewer-1"
    );
    assert_eq!(approved_attempt.state, AttemptState::Succeeded);
    let replay_approval = store
        .approve_work(&gated.work_id, "reviewer-1", None, Some(awaiting.revision))
        .unwrap();
    assert_eq!(replay_approval.0.state, WorkState::Succeeded);
    assert_eq!(replay_approval.0.revision, approved.revision);
    assert_eq!(
        store
            .approve_work(
                &gated.work_id,
                "foreign-actor",
                None,
                Some(approved.revision)
            )
            .unwrap_err()
            .code,
        OrchErrorCode::Conflict
    );
}

#[test]
fn verified_success_survives_store_reload() {
    let home = tempdir().unwrap();
    let work_id;
    {
        let (store, item) = new_work(home.path(), "reload me");
        store.save_work_item(&item).unwrap();
        let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
        complete_verified(&store, &item, &claim, "verified");
        work_id = item.work_id;
    }
    let reopened = OrchStore::open(home.path()).unwrap();
    let loaded = reopened.load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(loaded.state, WorkState::Succeeded);
    assert!(loaded.result.as_ref().unwrap().verification.is_some());
}

#[test]
fn managed_finalization_cannot_bypass_completion_authority() {
    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "managed");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let intent = ManagedExecutionIntent {
        schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: "intent-managed-1".into(),
        agent_id: "worker".into(),
        agent_spec_revision: 1,
        work_id: item.work_id.clone(),
        work_revision: claim.work.revision,
        attempt_id: Some(claim.attempt.attempt_id.clone()),
        run_id: Some("run-unverified".into()),
        session_id: item.session_id,
        workspace: item.workspace.clone(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        bounds: Default::default(),
        input_hash: "hash".into(),
        state: ManagedIntentState::Admitted,
        permission_request_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.save_managed_intent(&intent).unwrap();
    store
        .finalize_managed_intent(
            &intent.intent_id,
            ManagedFinalizationOutcome::Completed,
            "provider completed",
            Some(success_result("provider terminal")),
            Utc::now(),
        )
        .unwrap();
    assert_eq!(
        store.load_work_item(&item.work_id).unwrap().unwrap().state,
        WorkState::Review,
        "a terminal provider/run state is not success authority"
    );

    let verified_item = WorkItem::new(
        "test",
        "managed verified",
        item.session_id,
        item.workspace.clone(),
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&verified_item).unwrap();
    let verified_claim = store
        .claim_work(&verified_item.work_id, "worker", None)
        .unwrap();
    let run_id = format!("run-{}", verified_item.work_id);
    let evidence = verified_evidence(
        &verified_item.work_id,
        &run_id,
        &verified_claim.attempt.attempt_id,
    );
    store
        .link_work_run(
            &verified_item.work_id,
            &verified_claim.attempt.attempt_id,
            &verified_claim.lease_token,
            &run_id,
        )
        .unwrap();
    store
        .save_run(&fixture_run(
            &run_id,
            verified_item.session_id,
            &verified_item.workspace,
            Some(evidence.clone()),
        ))
        .unwrap();
    let verified_intent = ManagedExecutionIntent {
        schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: "intent-managed-2".into(),
        agent_id: "worker".into(),
        agent_spec_revision: 1,
        work_id: verified_item.work_id.clone(),
        work_revision: verified_claim.work.revision,
        attempt_id: Some(verified_claim.attempt.attempt_id.clone()),
        run_id: Some(run_id.clone()),
        session_id: verified_item.session_id,
        workspace: verified_item.workspace.clone(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        bounds: Default::default(),
        input_hash: "hash".into(),
        state: ManagedIntentState::Admitted,
        permission_request_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    store.save_managed_intent(&verified_intent).unwrap();
    store
        .finalize_managed_intent_until(
            &verified_intent.intent_id,
            ManagedFinalizationOutcome::Completed,
            "managed verified",
            Some(result_with("managed verified", evidence)),
            Utc::now(),
            ManagedFinalizationStage::Complete,
        )
        .unwrap();
    assert_eq!(
        store
            .load_work_item(&verified_item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Succeeded
    );
}

#[test]
fn historical_records_deserialize_but_unbound_evidence_is_not_success() {
    let legacy: WorkResult = serde_json::from_str(
        r#"{"summary":"legacy complete","evidence":[],"artifacts":[],"completedAt":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    assert!(legacy.verification.is_none());

    let home = tempdir().unwrap();
    let (store, item) = new_work(home.path(), "historical");
    store.save_work_item(&item).unwrap();
    let claim = store.claim_work(&item.work_id, "worker", None).unwrap();
    let run_id = format!("run-{}", item.work_id);
    let mut historical = verified_evidence(&item.work_id, &run_id, &claim.attempt.attempt_id);
    historical.work_id = None;
    historical.run_id = None;
    historical.attempt_id = None;
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run_id,
        )
        .unwrap();
    store
        .save_run(&fixture_run(
            &run_id,
            item.session_id,
            &item.workspace,
            Some(historical.clone()),
        ))
        .unwrap();
    let (completed, _) = store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            result_with("historical unbound", historical),
        )
        .unwrap();
    assert_eq!(completed.state, WorkState::Review);
}

#[test]
fn production_succeeded_assignment_is_confined_to_the_store_helper() {
    let files = [
        include_str!("../src/orchestration/store.rs"),
        include_str!("../src/orchestration/service.rs"),
        include_str!("../src/orchestration/managed.rs"),
        include_str!("../src/orchestration/workload.rs"),
        include_str!("../src/orchestration/manager.rs"),
        include_str!("../src/orchestration/graph.rs"),
    ];
    let mut assignments = Vec::new();
    for src in files {
        let production = src.split("#[cfg(test)]").next().unwrap_or(src);
        for (idx, line) in production.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            if trimmed.contains("state = WorkState::Succeeded;")
                && !trimmed.contains("==")
                && !trimmed.contains("!=")
            {
                assignments.push((idx + 1, trimmed.to_string()));
            }
        }
    }
    assert_eq!(
        assignments.len(),
        1,
        "only the store helper may assign Succeeded: {assignments:?}"
    );
    assert!(
        assignments[0]
            .1
            .contains("item.state = WorkState::Succeeded"),
        "{assignments:?}"
    );
}
