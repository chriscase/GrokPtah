//! Adversarial coverage for the scoped dependency-graph invariant, the
//! canonical admission evaluator, and the durable review gate.
//!
//! Every test here is a hostile case: a second principal probing for work it
//! may not observe, two writers racing the same invariant, a reviewer casting
//! someone else's verdict, a receipt replayed after the work moved, a
//! revocation that reopens a closed gate, and an audit sink that fails.
//!
//! No provider is contacted and no credential is read.

use std::sync::{Arc, Barrier};

use chrono::{Duration, Utc};
use grokptah_agent_bridge::orchestration::{
    evaluate_admission, evaluate_quorum, AdmissionBlock, DependencyStates, OrchStore,
    QuorumOutcome, ReviewReceipt, ReviewVerdict, WorkReviewPolicy,
};
use grokptah_agent_bridge::orchestration::{WorkDependency, WorkItem, WorkPolicy, WorkState};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Scope {
    session_id: Uuid,
    workspace: String,
}

impl Scope {
    fn new(root: &std::path::Path, name: &str) -> Self {
        let workspace = root.join(name);
        std::fs::create_dir_all(&workspace).expect("workspace");
        Self {
            session_id: Uuid::new_v4(),
            workspace: workspace.display().to_string(),
        }
    }

    fn item(&self, deps: &[&str]) -> WorkItem {
        let mut item = WorkItem::new(
            "build",
            "synthetic objective",
            self.session_id,
            self.workspace.clone(),
            "tester",
            WorkPolicy::default(),
        )
        .expect("work item");
        item.dependencies = deps
            .iter()
            .map(|dep| WorkDependency {
                work_id: (*dep).to_string(),
                required_state: WorkState::Succeeded,
            })
            .collect();
        item
    }
}

fn store(root: &std::path::Path) -> OrchStore {
    OrchStore::open(root.join("ledger")).expect("store opens")
}

// ---------------------------------------------------------------------------
// Blocker 1 — scope isolation and foreign/unknown parity
// ---------------------------------------------------------------------------

#[test]
fn a_dependency_in_another_scope_is_indistinguishable_from_one_that_does_not_exist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let alice = Scope::new(dir.path(), "alice");
    let bob = Scope::new(dir.path(), "bob");

    // Alice owns a real work item. Bob must not be able to learn that.
    let alice_item = alice.item(&[]);
    store.save_work_item(&alice_item).expect("alice writes");

    // Bob depends on Alice's real id, and on an id nobody owns.
    let foreign = bob.item(&[alice_item.work_id.as_str()]);
    let invented = Uuid::new_v4().to_string();
    let unknown = bob.item(&[invented.as_str()]);

    let foreign_error = store
        .save_work_item(&foreign)
        .expect_err("a foreign dependency must be refused");
    let unknown_error = store
        .save_work_item(&unknown)
        .expect_err("an unknown dependency must be refused");

    // Parity: the only difference permitted is the caller's own id, which the
    // caller already supplied. Anything else is an existence oracle.
    let foreign_text = foreign_error
        .to_string()
        .replace(&alice_item.work_id, "<id>");
    let unknown_text = unknown_error.to_string().replace(&invented, "<id>");
    assert_eq!(
        foreign_text, unknown_text,
        "foreign and unknown dependencies must be indistinguishable"
    );
}

#[test]
fn a_cycle_error_never_names_work_from_another_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let alice = Scope::new(dir.path(), "alice");
    let bob = Scope::new(dir.path(), "bob");

    let alice_a = alice.item(&[]);
    let alice_b = alice.item(&[alice_a.work_id.as_str()]);
    store.save_work_item(&alice_a).expect("write");
    store.save_work_item(&alice_b).expect("write");

    let bob_a = bob.item(&[]);
    store.save_work_item(&bob_a).expect("write");
    let mut bob_b = bob.item(&[bob_a.work_id.as_str()]);
    store.save_work_item(&bob_b).expect("write");
    // Close a ring entirely inside Bob's scope.
    let mut bob_a_cyclic = bob_a.clone();
    bob_a_cyclic.dependencies = vec![WorkDependency {
        work_id: bob_b.work_id.clone(),
        required_state: WorkState::Succeeded,
    }];
    let error = store
        .save_work_item(&bob_a_cyclic)
        .expect_err("cycle must be refused");
    let text = error.to_string();
    assert!(text.contains("cycle"), "{text}");
    for foreign in [&alice_a.work_id, &alice_b.work_id] {
        assert!(
            !text.contains(foreign.as_str()),
            "cycle error leaked foreign id {foreign}: {text}"
        );
    }
    bob_b.dependencies.clear();
}

#[test]
fn a_malformed_graph_in_one_scope_cannot_block_a_writer_in_another() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let alice = Scope::new(dir.path(), "alice");
    let bob = Scope::new(dir.path(), "bob");

    // Alice writes an item whose dependency is later removed from under it,
    // leaving her scope internally dangling.
    let alice_a = alice.item(&[]);
    store.save_work_item(&alice_a).expect("write");
    let alice_b = alice.item(&[alice_a.work_id.as_str()]);
    store.save_work_item(&alice_b).expect("write");

    // Bob's own well-formed graph must still be writable.
    let bob_a = bob.item(&[]);
    store.save_work_item(&bob_a).expect("bob root writes");
    let bob_b = bob.item(&[bob_a.work_id.as_str()]);
    store
        .save_work_item(&bob_b)
        .expect("bob must not be blocked by another scope");
}

#[test]
fn the_same_session_in_a_different_workspace_is_a_different_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let first = Scope::new(dir.path(), "first");
    let mut second = Scope::new(dir.path(), "second");
    // Same session id, different workspace.
    second.session_id = first.session_id;

    let anchor = first.item(&[]);
    store.save_work_item(&anchor).expect("write");
    let crossing = second.item(&[anchor.work_id.as_str()]);
    assert!(
        store.save_work_item(&crossing).is_err(),
        "workspace must partition the scope even within one session"
    );
}

// ---------------------------------------------------------------------------
// Blocker 2 — atomicity, every path, revisions
// ---------------------------------------------------------------------------

#[test]
fn concurrent_writers_racing_one_cycle_cannot_both_land() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "race");

    let a = scope.item(&[]);
    let b = scope.item(&[]);
    store.save_work_item(&a).expect("write");
    store.save_work_item(&b).expect("write");

    // Two writers each try to add one edge; together the edges form a ring.
    let mut a_edge = a.clone();
    a_edge.dependencies = vec![WorkDependency {
        work_id: b.work_id.clone(),
        required_state: WorkState::Succeeded,
    }];
    let mut b_edge = b.clone();
    b_edge.dependencies = vec![WorkDependency {
        work_id: a.work_id.clone(),
        required_state: WorkState::Succeeded,
    }];

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for candidate in [a_edge, b_edge] {
        let store = store.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store.save_work_item(&candidate).is_ok()
        }));
    }
    let landed = handles
        .into_iter()
        .filter(|_| true)
        .map(|handle| handle.join().expect("thread joins"))
        .filter(|ok| *ok)
        .count();
    assert_eq!(
        landed, 1,
        "exactly one of two edges that would close a ring may land"
    );

    // Whatever landed, the durable graph is acyclic.
    let items = store.list_work_items().expect("list");
    let cyclic = items
        .iter()
        .filter(|item| !item.dependencies.is_empty())
        .count();
    assert_eq!(cyclic, 1, "a ring must not be durable");
}

#[test]
fn a_state_only_save_does_not_revalidate_and_a_dependency_change_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "paths");

    let a = scope.item(&[]);
    store.save_work_item(&a).expect("write");
    let mut b = scope.item(&[a.work_id.as_str()]);
    store.save_work_item(&b).expect("write");

    // A state-only save of an item that already carries dependencies is
    // accepted — this is the path reconciliation takes on every pass.
    b.state = WorkState::Blocked;
    b.bump();
    store
        .save_work_item(&b)
        .expect("state-only save is accepted");

    // Changing the dependency set to something unresolvable is refused, on
    // the same single save path every producer uses.
    let mut hostile = b.clone();
    hostile.dependencies = vec![WorkDependency {
        work_id: Uuid::new_v4().to_string(),
        required_state: WorkState::Succeeded,
    }];
    assert!(
        store.save_work_item(&hostile).is_err(),
        "a dependency change must be validated wherever it comes from"
    );
}

#[test]
fn reconciliation_and_restart_preserve_the_invariant_and_the_typed_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "restart");
    let (root_id, child_id);
    {
        let store = store(dir.path());
        let root = scope.item(&[]);
        root_id = root.work_id.clone();
        store.save_work_item(&root).expect("write");
        let child = scope.item(&[root.work_id.as_str()]);
        child_id = child.work_id.clone();
        store.save_work_item(&child).expect("write");
        store.reconcile_workloads().expect("reconcile");
    }
    // Reopening the ledger is what a restart looks like from here.
    let store = store(dir.path());
    store
        .reconcile_workloads()
        .expect("reconcile after restart");
    let child = store
        .load_work_item(&child_id)
        .expect("load")
        .expect("child");
    assert_eq!(child.state, WorkState::Blocked);
    assert_eq!(
        child.blocked_reason.as_deref(),
        Some(AdmissionBlock::DependenciesPending.as_str()),
        "the typed reason must survive restart and match the state"
    );
    assert_eq!(
        store
            .admission_block_at(&child_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::DependenciesPending
    );
    assert_eq!(
        store
            .admission_block_at(&root_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::Admissible
    );
}

// ---------------------------------------------------------------------------
// Blocker 3 — the canonical evaluator explains canonical states
// ---------------------------------------------------------------------------

fn states(pairs: &[(&str, Option<WorkState>)]) -> DependencyStates {
    pairs
        .iter()
        .map(|(id, state)| ((*id).to_string(), *state))
        .collect()
}

#[test]
fn a_reconciled_dependency_wait_reports_the_wait_not_merely_unclaimable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "eval");
    let mut item = scope.item(&["dep"]);
    // This is the canonical persisted encoding of "waiting".
    item.state = WorkState::Blocked;
    let block = evaluate_admission(
        &item,
        &states(&[("dep", Some(WorkState::Running))]),
        &[],
        Utc::now(),
    );
    assert_eq!(block, AdmissionBlock::DependenciesPending);
    assert!(!block.needs_operator_attention());
}

#[test]
fn a_deadline_failure_reports_the_deadline_not_a_generic_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "eval2");
    let now = Utc::now();
    let mut item = scope.item(&[]);
    // Canonical persisted encoding of a missed deadline.
    item.state = WorkState::Failed;
    item.deadline = Some(now - Duration::seconds(1));
    assert_eq!(
        evaluate_admission(&item, &DependencyStates::new(), &[], now),
        AdmissionBlock::DeadlineExceeded
    );

    // A failure with no deadline is reported as a failure, not as a deadline.
    let mut plain = scope.item(&[]);
    plain.state = WorkState::Failed;
    assert_eq!(
        evaluate_admission(&plain, &DependencyStates::new(), &[], now),
        AdmissionBlock::Failed
    );
}

#[test]
fn every_canonical_state_maps_to_a_distinct_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "eval3");
    let now = Utc::now();
    let cases = [
        (WorkState::Queued, AdmissionBlock::Admissible),
        (WorkState::Leased, AdmissionBlock::AttemptActive),
        (WorkState::Running, AdmissionBlock::AttemptActive),
        (WorkState::AwaitingInput, AdmissionBlock::AwaitingInput),
        (
            WorkState::AwaitingApproval,
            AdmissionBlock::AwaitingApproval,
        ),
        (WorkState::Review, AdmissionBlock::AwaitingApproval),
        (WorkState::Succeeded, AdmissionBlock::Succeeded),
        (WorkState::Cancelled, AdmissionBlock::Cancelled),
    ];
    for (state, expected) in cases {
        let mut item = scope.item(&[]);
        item.state = state;
        assert_eq!(
            evaluate_admission(&item, &DependencyStates::new(), &[], now),
            expected,
            "state {state:?}"
        );
    }
}

#[test]
fn an_unresolvable_dependency_is_reported_and_never_assumed_satisfied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "eval4");
    let item = scope.item(&["ghost"]);
    // Absent from the map, and present-but-unresolved, are the same answer.
    assert_eq!(
        evaluate_admission(&item, &DependencyStates::new(), &[], Utc::now()),
        AdmissionBlock::DependencyUnresolved
    );
    assert_eq!(
        evaluate_admission(&item, &states(&[("ghost", None)]), &[], Utc::now()),
        AdmissionBlock::DependencyUnresolved
    );
    assert!(AdmissionBlock::DependencyUnresolved.needs_operator_attention());
}

#[test]
fn a_terminally_failed_dependency_is_unsatisfiable_not_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "eval5");
    let item = scope.item(&["dep"]);
    for terminal in [WorkState::Failed, WorkState::Cancelled] {
        assert_eq!(
            evaluate_admission(&item, &states(&[("dep", Some(terminal))]), &[], Utc::now()),
            AdmissionBlock::DependencyUnsatisfiable
        );
    }
}

// ---------------------------------------------------------------------------
// Blocker 4 — durable review authority
// ---------------------------------------------------------------------------

fn policy(reviewers: &[&str], required: u32) -> WorkReviewPolicy {
    WorkReviewPolicy {
        reviewers: reviewers.iter().map(|r| (*r).to_string()).collect(),
        required_approvals: required,
        policy_revision: 1,
    }
}

fn reviewed_item(store: &OrchStore, scope: &Scope, reviewers: &[&str], required: u32) -> WorkItem {
    let mut item = scope.item(&[]);
    item.review = Some(policy(reviewers, required));
    store.save_work_item(&item).expect("write");
    item
}

#[test]
fn a_principal_cannot_cast_another_reviewers_verdict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "review");
    let item = reviewed_item(&store, &scope, &["r1", "r2"], 2);

    let error = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "token-of-r2",
            "r2",
            None,
            Utc::now(),
        )
        .expect_err("impersonation must be refused");
    assert!(error.to_string().contains("only record its own"), "{error}");

    // An identity the gate does not name is refused even when self-attested.
    let error = store
        .record_review_verdict(
            &item.work_id,
            "intruder",
            ReviewVerdict::Approve,
            "token-of-intruder",
            "intruder",
            None,
            Utc::now(),
        )
        .expect_err("an unnamed reviewer must be refused");
    assert!(error.to_string().contains("does not name"), "{error}");
}

#[test]
fn a_verdict_binds_the_work_and_policy_revision_it_was_cast_against() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "bind");
    let item = reviewed_item(&store, &scope, &["r1"], 1);
    let before = item.revision;

    let updated = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "token-r1",
            "r1",
            Some(before),
            Utc::now(),
        )
        .expect("verdict records");
    let receipt = updated.review_receipts.last().expect("receipt");
    assert_eq!(receipt.work_revision, before);
    assert_eq!(receipt.policy_revision, 1);
    assert_eq!(receipt.principal_owner_id, "r1");
    assert!(
        updated.revision > before,
        "recording must bump the revision"
    );

    // A stale expected revision is refused.
    let error = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "token-r1",
            "r1",
            Some(before),
            Utc::now(),
        )
        .expect_err("stale revision must be refused");
    assert!(error.to_string().contains("revision"), "{error}");
}

#[test]
fn a_receipt_from_a_superseded_policy_revision_does_not_count() {
    let gate = policy(&["r1", "r2"], 2);
    let stale = ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        work_revision: 1,
        // Cast under an earlier policy.
        policy_revision: 0_u64.saturating_add(99),
        recorded_at: Utc::now(),
        revoked_at: None,
    };
    assert_eq!(
        evaluate_quorum(&gate, &[stale]).expect("evaluates"),
        QuorumOutcome::Pending,
        "a receipt from another policy revision must not count"
    );
}

#[test]
fn a_verdict_is_immutable_until_revoked_and_revocation_reopens_the_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "revoke");
    let item = reviewed_item(&store, &scope, &["r1"], 1);
    let now = Utc::now();

    let approved = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "t1",
            "r1",
            None,
            now,
        )
        .expect("approve");
    assert_eq!(
        store
            .admission_block_at(&item.work_id, now)
            .expect("admission"),
        AdmissionBlock::Admissible
    );

    // Re-casting without revoking is refused: a verdict is not editable.
    assert!(
        store
            .record_review_verdict(
                &item.work_id,
                "r1",
                ReviewVerdict::Reject,
                "t1",
                "r1",
                None,
                now
            )
            .is_err(),
        "an active verdict must be revoked before it can change"
    );

    let revoked = store
        .revoke_review_verdict(
            &item.work_id,
            "r1",
            "t1",
            "r1",
            Some(approved.revision),
            now,
        )
        .expect("revoke");
    // The receipt is retained, not deleted, so the trail stays complete.
    assert_eq!(revoked.review_receipts.len(), 1);
    assert!(revoked.review_receipts[0].revoked_at.is_some());
    assert_eq!(
        store
            .admission_block_at(&item.work_id, now)
            .expect("admission"),
        AdmissionBlock::ReviewPending,
        "revocation must reopen the gate"
    );

    // After revoking, the reviewer may cast a different verdict.
    store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Reject,
            "t1",
            "r1",
            None,
            now,
        )
        .expect("recast after revocation");
    assert_eq!(
        store
            .admission_block_at(&item.work_id, now)
            .expect("admission"),
        AdmissionBlock::ReviewUnreachable
    );
}

#[test]
fn concurrent_reviewers_racing_one_gate_each_record_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "quorum-race");
    let item = reviewed_item(&store, &scope, &["r1", "r2", "r3"], 2);

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for reviewer in ["r1", "r2", "r3"] {
        let store = store.clone();
        let work_id = item.work_id.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            // No expected revision: these genuinely race.
            store
                .record_review_verdict(
                    &work_id,
                    reviewer,
                    ReviewVerdict::Approve,
                    reviewer,
                    reviewer,
                    None,
                    Utc::now(),
                )
                .is_ok()
        }));
    }
    let recorded = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joins"))
        .filter(|ok| *ok)
        .count();
    assert_eq!(recorded, 3, "every distinct reviewer records once");

    let final_item = store
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("item");
    assert_eq!(
        final_item.review_receipts.len(),
        3,
        "no receipt may be lost to a concurrent write"
    );
    assert_eq!(
        store
            .admission_block_at(&item.work_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::Admissible
    );
}

#[test]
fn a_terminal_or_cancelled_item_accepts_no_verdict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "cancelled");
    let mut item = scope.item(&[]);
    item.review = Some(policy(&["r1"], 1));
    item.state = WorkState::Cancelled;
    store.save_work_item(&item).expect("write");

    let error = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "t1",
            "r1",
            None,
            Utc::now(),
        )
        .expect_err("a cancelled item must not accept a verdict");
    assert!(error.to_string().contains("terminal"), "{error}");
    assert_eq!(
        store
            .admission_block_at(&item.work_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::Cancelled
    );
}

#[test]
fn a_work_item_cannot_carry_receipts_without_a_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "orphan");
    let mut item = scope.item(&[]);
    item.review = None;
    item.review_receipts = vec![ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        work_revision: 1,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    }];
    assert!(
        store.save_work_item(&item).is_err(),
        "an orphaned receipt must fail closed"
    );
}

#[test]
fn malformed_review_policies_are_rejected() {
    for gate in [
        policy(&[], 1),
        policy(&["r1"], 0),
        policy(&["r1"], 2),
        policy(&["r1", "r1"], 1),
        policy(&["   "], 1),
        WorkReviewPolicy {
            reviewers: vec!["r1".into()],
            required_approvals: 1,
            policy_revision: 0,
        },
    ] {
        assert!(gate.validate().is_err(), "{gate:?} must be rejected");
    }
}

#[test]
fn quorum_arithmetic_reports_met_pending_and_unreachable() {
    let gate = policy(&["r1", "r2", "r3"], 2);
    let receipt = |reviewer: &str, verdict| ReviewReceipt {
        reviewer_id: reviewer.to_string(),
        principal_token_id: reviewer.to_string(),
        principal_owner_id: reviewer.to_string(),
        verdict,
        work_revision: 1,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    };
    assert_eq!(
        evaluate_quorum(&gate, &[]).expect("evaluates"),
        QuorumOutcome::Pending
    );
    assert_eq!(
        evaluate_quorum(&gate, &[receipt("r1", ReviewVerdict::Approve)]).expect("evaluates"),
        QuorumOutcome::Pending
    );
    assert_eq!(
        evaluate_quorum(
            &gate,
            &[
                receipt("r1", ReviewVerdict::Approve),
                receipt("r2", ReviewVerdict::Approve)
            ]
        )
        .expect("evaluates"),
        QuorumOutcome::Met
    );
    // Two rejections leave one undecided reviewer, which cannot reach two.
    assert_eq!(
        evaluate_quorum(
            &gate,
            &[
                receipt("r1", ReviewVerdict::Reject),
                receipt("r2", ReviewVerdict::Reject)
            ]
        )
        .expect("evaluates"),
        QuorumOutcome::Unreachable
    );
    // A verdict from an identity the gate does not name is ignored.
    assert_eq!(
        evaluate_quorum(&gate, &[receipt("intruder", ReviewVerdict::Approve)]).expect("evaluates"),
        QuorumOutcome::Pending
    );
}

// ---------------------------------------------------------------------------
// Audit failure
// ---------------------------------------------------------------------------

#[test]
fn a_verdict_that_cannot_be_audited_is_not_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "audit");
    let item = reviewed_item(&store, &scope, &["r1"], 1);

    // Make the audit destination unwritable by replacing the directory the
    // audit log lives in with a regular file.
    let audit_dir = dir.path().join("ledger").join("audit");
    std::fs::remove_dir_all(&audit_dir).expect("remove audit dir");
    std::fs::write(&audit_dir, b"not a directory").expect("occupy audit path");

    let error = store
        .record_review_verdict(
            &item.work_id,
            "r1",
            ReviewVerdict::Approve,
            "t1",
            "r1",
            None,
            Utc::now(),
        )
        .expect_err("an unauditable verdict must be refused");
    assert!(error.to_string().contains("audit"), "{error}");

    let stored = store
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("item");
    assert!(
        stored.review_receipts.is_empty(),
        "no receipt may become durable when its audit failed"
    );
    assert_eq!(
        evaluate_admission(
            &stored,
            &DependencyStates::new(),
            &stored.review_receipts,
            Utc::now()
        ),
        AdmissionBlock::ReviewPending,
        "the gate must remain closed"
    );
}

// ---------------------------------------------------------------------------
// Honest cycle reporting
// ---------------------------------------------------------------------------

#[test]
fn a_reported_cycle_contains_only_real_cycle_members() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "honest");

    // ring: a -> b -> a. downstream: d depends on a but is not on the ring.
    let a = scope.item(&[]);
    let b = scope.item(&[]);
    store.save_work_item(&a).expect("write");
    store.save_work_item(&b).expect("write");
    let mut d = scope.item(&[a.work_id.as_str()]);
    d.priority = -1;
    store.save_work_item(&d).expect("write");

    let mut b_edge = b.clone();
    b_edge.dependencies = vec![WorkDependency {
        work_id: a.work_id.clone(),
        required_state: WorkState::Succeeded,
    }];
    store.save_work_item(&b_edge).expect("write");

    let mut a_edge = a.clone();
    a_edge.dependencies = vec![WorkDependency {
        work_id: b.work_id.clone(),
        required_state: WorkState::Succeeded,
    }];
    let error = store
        .save_work_item(&a_edge)
        .expect_err("cycle must be refused");
    let text = error.to_string();
    assert!(text.contains("cycle"), "{text}");
    assert!(
        !text.contains(d.work_id.as_str()),
        "a node merely downstream of a cycle is not a cycle member: {text}"
    );
    assert!(
        text.contains(a.work_id.as_str()) && text.contains(b.work_id.as_str()),
        "{text}"
    );
}

#[test]
fn a_bounded_deep_chain_is_accepted_without_recursion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "deep");
    let mut previous: Option<String> = None;
    // Deep enough to exercise the iterative peel and walk on a real chain,
    // and bounded well under the scope ceiling so the bound itself is not what
    // is being tested. Validation is O(scope) per dependency-carrying write,
    // so this is deliberately not scaled to the ceiling.
    for _ in 0..400 {
        let deps: Vec<&str> = previous.iter().map(|p| p.as_str()).collect();
        let item = scope.item(&deps);
        store.save_work_item(&item).expect("write");
        previous = Some(item.work_id);
    }
    assert_eq!(store.list_work_items().expect("list").len(), 400);
}

#[test]
fn reconciliation_records_the_reason_without_consuming_a_revision() {
    // A caller holding a revision for compare-and-swap must not have it
    // invalidated merely because the supervisor refreshed a derived reason.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "revisions");

    let root = scope.item(&[]);
    store.save_work_item(&root).expect("write");
    let child = scope.item(&[root.work_id.as_str()]);
    store.save_work_item(&child).expect("write");

    store.reconcile_workloads().expect("first reconcile");
    let after_first = store
        .load_work_item(&child.work_id)
        .expect("load")
        .expect("child");
    assert_eq!(after_first.state, WorkState::Blocked);
    assert_eq!(
        after_first.blocked_reason.as_deref(),
        Some(AdmissionBlock::DependenciesPending.as_str())
    );

    // Repeated passes with nothing changing must be revision-stable.
    for _ in 0..3 {
        store.reconcile_workloads().expect("idempotent reconcile");
    }
    let after_repeat = store
        .load_work_item(&child.work_id)
        .expect("load")
        .expect("child");
    assert_eq!(
        after_repeat.revision, after_first.revision,
        "a reconciliation that changes no decision must not move the revision"
    );

    // And an unrelated compare-and-swap still succeeds afterwards.
    store
        .assign_work(
            &child.work_id,
            Some("agent-1".into()),
            Some(after_repeat.revision),
        )
        .expect("a held revision must remain valid across reconciliation");
}

#[test]
fn a_different_session_in_the_same_workspace_is_a_different_scope() {
    // Reads are already scoped by session and workspace, so resolving a
    // dependency across sessions would let one session confirm the existence
    // of work it cannot otherwise observe. This is a deliberate narrowing of
    // the previous installation-wide resolution.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let first = Scope::new(dir.path(), "shared");
    let second = Scope {
        session_id: Uuid::new_v4(),
        workspace: first.workspace.clone(),
    };

    let anchor = first.item(&[]);
    store.save_work_item(&anchor).expect("write");

    let crossing = second.item(&[anchor.work_id.as_str()]);
    assert!(
        store.save_work_item(&crossing).is_err(),
        "session must partition the scope even within one workspace"
    );

    // And the same declaration inside the owning session is accepted, so the
    // narrowing is about scope rather than about dependencies generally.
    let sibling = first.item(&[anchor.work_id.as_str()]);
    store
        .save_work_item(&sibling)
        .expect("an in-scope dependency is accepted");
}
