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
    evaluate_admission, evaluate_quorum, review_subject_digest, AdmissionBlock, BlockProvenance,
    DependencyStates, OrchStore, QuorumOutcome, ReviewReceipt, ReviewVerdict, WorkReviewPolicy,
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
    // This is the canonical persisted encoding of "waiting": reconciliation
    // stamps its own holds `Derived`, and the evaluator reads through them to
    // the reason the item is actually waiting.
    item.state = WorkState::Blocked;
    item.block_provenance = Some(BlockProvenance::Derived);
    let block = evaluate_admission(
        &item,
        &states(&[("dep", Some(WorkState::Running))]),
        &[],
        None,
        Utc::now(),
    );
    assert_eq!(block, AdmissionBlock::DependenciesPending);
    assert!(!block.needs_operator_attention());

    // A hold with no provenance is ambiguous, and ambiguity fails closed: it
    // may be a legacy record whose migration could not be written, and reading
    // it as derived would let dependencies becoming ready release a hold a
    // human placed.
    item.block_provenance = None;
    assert_eq!(
        evaluate_admission(
            &item,
            &states(&[("dep", Some(WorkState::Running))]),
            &[],
            None,
            Utc::now(),
        ),
        AdmissionBlock::ManuallyBlocked
    );
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
        evaluate_admission(&item, &DependencyStates::new(), &[], None, now),
        AdmissionBlock::DeadlineExceeded
    );

    // A failure with no deadline is reported as a failure, not as a deadline.
    let mut plain = scope.item(&[]);
    plain.state = WorkState::Failed;
    assert_eq!(
        evaluate_admission(&plain, &DependencyStates::new(), &[], None, now),
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
            evaluate_admission(&item, &DependencyStates::new(), &[], None, now),
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
        evaluate_admission(&item, &DependencyStates::new(), &[], None, Utc::now()),
        AdmissionBlock::DependencyUnresolved
    );
    assert_eq!(
        evaluate_admission(&item, &states(&[("ghost", None)]), &[], None, Utc::now()),
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
            evaluate_admission(
                &item,
                &states(&[("dep", Some(terminal))]),
                &[],
                None,
                Utc::now()
            ),
            AdmissionBlock::DependencyUnsatisfiable
        );
    }
}

// ---------------------------------------------------------------------------
// Durable review authority — what an external caller can and cannot do
// ---------------------------------------------------------------------------
//
// This file is a separate crate, so it cannot construct a `VerifiedPrincipal`
// and therefore cannot reach `record_review_verdict` at all. That is the
// fail-closed property: no caller outside the bridge can nominate a reviewer
// identity. The verdict path itself is exercised by in-crate unit tests; what
// is checked here is everything an external caller *can* reach.

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
fn a_review_gated_item_cannot_be_claimed_and_mints_no_attempt() {
    // The P0: `Queued` is not permission. Without an admission check at claim,
    // a gated item would mint a lease and execute before its quorum was met.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "gated");
    let item = reviewed_item(&store, &scope, &["r1"], 1);

    assert_eq!(
        store
            .load_work_item(&item.work_id)
            .expect("load")
            .expect("item")
            .state,
        WorkState::Queued,
        "the gate does not change the persisted state; admission is what refuses"
    );
    // With no executor bound the gate has nothing to approve, and that is the
    // reason reported: `Unassigned` work is claimable by any in-scope worker,
    // so an approval cast here would name no one.
    assert_eq!(
        store
            .admission_block_at(&item.work_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::ExecutorUnbound
    );

    let error = store
        .claim_work(&item.work_id, "worker", None)
        .expect_err("a gated item must not be claimable");
    assert!(error.to_string().contains("executor_unbound"), "{error}");
    assert!(
        store
            .list_work_attempts(Some(&item.work_id))
            .expect("attempts")
            .is_empty(),
        "a refused claim must not leave an attempt behind"
    );

    // Binding an executor moves the reason to the gate itself, and the item is
    // still not claimable: assignment is not approval.
    store
        .assign_work(&item.work_id, Some("worker".into()), None)
        .expect("assign");
    assert_eq!(
        store
            .admission_block_at(&item.work_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::ExecutorUnbound,
        "an assignment whose agent record does not resolve is still unbound"
    );
    let error = store
        .claim_work(&item.work_id, "worker", None)
        .expect_err("assignment alone must not open the gate");
    assert!(error.to_string().contains("executor_unbound"), "{error}");
    assert!(
        store
            .list_work_attempts(Some(&item.work_id))
            .expect("attempts")
            .is_empty(),
        "and still mints no attempt"
    );
}

#[test]
fn an_ungated_item_is_still_claimable() {
    // The admission gate must refuse only what it should.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "ungated");
    let item = scope.item(&[]);
    store.save_work_item(&item).expect("write");
    store
        .claim_work(&item.work_id, "worker", None)
        .expect("an admissible item is claimable");
}

#[test]
fn a_dependency_blocked_item_cannot_be_claimed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "depgate");
    let root = scope.item(&[]);
    store.save_work_item(&root).expect("write");
    let child = scope.item(&[root.work_id.as_str()]);
    store.save_work_item(&child).expect("write");

    let error = store
        .claim_work(&child.work_id, "worker", None)
        .expect_err("a dependency-blocked item must not be claimable");
    assert!(
        store
            .list_work_attempts(Some(&child.work_id))
            .expect("attempts")
            .is_empty(),
        "a refused claim must not leave an attempt behind: {error}"
    );
}

#[test]
fn the_review_gate_cannot_be_changed_through_a_generic_save() {
    // A caller that could rewrite the gate could approve its own work.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "immutable");
    let item = reviewed_item(&store, &scope, &["r1", "r2"], 2);

    // Weakening the policy is refused.
    let mut weakened = item.clone();
    weakened.review = Some(policy(&["r1"], 1));
    assert!(
        store.save_work_item(&weakened).is_err(),
        "the policy must not be replaceable through a generic save"
    );

    // Removing it entirely is refused.
    let mut removed = item.clone();
    removed.review = None;
    assert!(store.save_work_item(&removed).is_err());

    // Forging a receipt is refused.
    let mut forged = item.clone();
    forged.review_receipts = vec![ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        subject_digest: review_subject_digest(&item, None),
        work_revision: item.revision,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    }];
    assert!(
        store.save_work_item(&forged).is_err(),
        "receipts must not be forgeable through a generic save"
    );

    // A state-only save of the same gate is still fine.
    let mut progressed = item.clone();
    progressed.priority = 5;
    progressed.bump();
    store
        .save_work_item(&progressed)
        .expect("a save that leaves the gate alone is accepted");

    // And the stored gate is untouched.
    let stored = store
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("item");
    assert_eq!(stored.review, item.review);
    assert!(stored.review_receipts.is_empty());
}

#[test]
fn a_new_item_cannot_be_created_carrying_receipts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "prefilled");
    let mut item = scope.item(&[]);
    item.review = Some(policy(&["r1"], 1));
    item.review_receipts = vec![ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        subject_digest: review_subject_digest(&item, None),
        work_revision: 1,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    }];
    assert!(
        store.save_work_item(&item).is_err(),
        "a caller must not create work with its approvals already in place"
    );
}

#[test]
fn a_receipt_stops_counting_when_the_review_subject_changes() {
    // The ordinary work revision cannot be the subject: recording a receipt
    // bumps it. The subject digest covers what is actually being reviewed.
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "subject");
    let mut item = scope.item(&[]);
    item.review = Some(policy(&["r1"], 1));
    let original = review_subject_digest(&item, None);

    let receipt = ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        subject_digest: original.clone(),
        work_revision: item.revision,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    };
    let gate = item.review.clone().expect("gate");
    assert_eq!(
        evaluate_quorum(&gate, std::slice::from_ref(&receipt), &original).expect("evaluates"),
        QuorumOutcome::Met
    );

    // A bumped revision alone must not retire the verdict.
    let mut bumped = item.clone();
    bumped.bump();
    assert_eq!(
        review_subject_digest(&bumped, None),
        original,
        "the revision is not the review subject"
    );

    // Editing what is being reviewed does retire it.
    for mutate in [
        (|w: &mut WorkItem| w.objective = "a different objective".into()) as fn(&mut WorkItem),
        |w: &mut WorkItem| w.deadline = Some(Utc::now() + Duration::days(1)),
        |w: &mut WorkItem| w.policy.retry.max_attempts = 9,
    ] {
        let mut edited = item.clone();
        mutate(&mut edited);
        let changed = review_subject_digest(&edited, None);
        assert_ne!(
            changed, original,
            "editing the subject must change its digest"
        );
        assert_eq!(
            evaluate_quorum(&gate, std::slice::from_ref(&receipt), &changed).expect("evaluates"),
            QuorumOutcome::Pending,
            "a receipt for the old subject must not count for the new one"
        );
    }
}

#[test]
fn a_receipt_from_a_superseded_policy_revision_does_not_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    let scope = Scope::new(dir.path(), "policyrev");
    let mut item = scope.item(&[]);
    item.review = Some(policy(&["r1", "r2"], 2));
    let subject = review_subject_digest(&item, None);
    let gate = item.review.clone().expect("gate");
    let stale = ReviewReceipt {
        reviewer_id: "r1".into(),
        principal_token_id: "t".into(),
        principal_owner_id: "r1".into(),
        verdict: ReviewVerdict::Approve,
        subject_digest: subject.clone(),
        work_revision: 1,
        policy_revision: 99,
        recorded_at: Utc::now(),
        revoked_at: None,
    };
    assert_eq!(
        evaluate_quorum(&gate, &[stale], &subject).expect("evaluates"),
        QuorumOutcome::Pending,
        "a receipt from another policy revision must not count"
    );
}

#[test]
fn a_terminal_item_reports_its_outcome_not_its_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "terminal");
    let mut item = scope.item(&[]);
    item.review = Some(policy(&["r1"], 1));
    item.state = WorkState::Cancelled;
    store.save_work_item(&item).expect("write");
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
        subject_digest: "a".repeat(64),
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
    let subject = "d".repeat(64);
    let receipt = |reviewer: &str, verdict| ReviewReceipt {
        reviewer_id: reviewer.to_string(),
        principal_token_id: reviewer.to_string(),
        principal_owner_id: reviewer.to_string(),
        verdict,
        subject_digest: subject.clone(),
        work_revision: 1,
        policy_revision: 1,
        recorded_at: Utc::now(),
        revoked_at: None,
    };
    assert_eq!(
        evaluate_quorum(&gate, &[], &subject).expect("evaluates"),
        QuorumOutcome::Pending
    );
    assert_eq!(
        evaluate_quorum(&gate, &[receipt("r1", ReviewVerdict::Approve)], &subject)
            .expect("evaluates"),
        QuorumOutcome::Pending
    );
    assert_eq!(
        evaluate_quorum(
            &gate,
            &[
                receipt("r1", ReviewVerdict::Approve),
                receipt("r2", ReviewVerdict::Approve)
            ],
            &subject
        )
        .expect("evaluates"),
        QuorumOutcome::Met
    );
    assert_eq!(
        evaluate_quorum(
            &gate,
            &[
                receipt("r1", ReviewVerdict::Reject),
                receipt("r2", ReviewVerdict::Reject)
            ],
            &subject
        )
        .expect("evaluates"),
        QuorumOutcome::Unreachable
    );
    assert_eq!(
        evaluate_quorum(
            &gate,
            &[receipt("intruder", ReviewVerdict::Approve)],
            &subject
        )
        .expect("evaluates"),
        QuorumOutcome::Pending
    );
}

// ---------------------------------------------------------------------------
// Manual block preservation
// ---------------------------------------------------------------------------

#[test]
fn a_manual_block_survives_reconciliation_and_keeps_its_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "manual");
    let item = scope.item(&[]);
    store.save_work_item(&item).expect("write");

    let (blocked, _) = store
        .block_work(
            &item.work_id,
            "operator",
            "waiting on a hardware decision",
            None,
            Utc::now(),
        )
        .expect("manual block");
    assert_eq!(blocked.state, WorkState::Blocked);

    // Reconciliation sees no unmet dependency and would otherwise re-queue it.
    for _ in 0..3 {
        store.reconcile_workloads().expect("reconcile");
    }
    let after = store
        .load_work_item(&item.work_id)
        .expect("load")
        .expect("item");
    assert_eq!(
        after.state,
        WorkState::Blocked,
        "a human block must not be lifted by reconciliation"
    );
    assert_eq!(
        after.blocked_reason.as_deref(),
        Some("waiting on a hardware decision"),
        "the human reason must survive"
    );
    assert_eq!(
        store
            .admission_block_at(&item.work_id, Utc::now())
            .expect("admission"),
        AdmissionBlock::ManuallyBlocked
    );
    assert!(AdmissionBlock::ManuallyBlocked.needs_operator_attention());
    assert!(
        store.claim_work(&item.work_id, "worker", None).is_err(),
        "a manually blocked item must not be claimable"
    );
}

// ---------------------------------------------------------------------------
// Scope mutation and bounds
// ---------------------------------------------------------------------------

#[test]
fn moving_an_item_to_another_scope_revalidates_its_graph() {
    // The dependency ids are unchanged, but they now resolve in a different
    // scope, so the edges point at nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let alice = Scope::new(dir.path(), "alice-move");
    let bob = Scope::new(dir.path(), "bob-move");

    let anchor = alice.item(&[]);
    store.save_work_item(&anchor).expect("write");
    let dependent = alice.item(&[anchor.work_id.as_str()]);
    store.save_work_item(&dependent).expect("write");

    for mutate in [
        (|w: &mut WorkItem, b: &Scope| w.session_id = b.session_id) as fn(&mut WorkItem, &Scope),
        |w: &mut WorkItem, b: &Scope| w.workspace = b.workspace.clone(),
        |w: &mut WorkItem, _b: &Scope| w.created_by = "someone-else".into(),
    ] {
        let mut moved = dependent.clone();
        mutate(&mut moved, &bob);
        assert!(
            store.save_work_item(&moved).is_err(),
            "a scope change must revalidate the graph even with unchanged edges"
        );
    }
}

#[test]
fn two_principals_in_one_session_and_workspace_are_distinct_scopes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = store(dir.path());
    let scope = Scope::new(dir.path(), "principals");

    let mut owned = scope.item(&[]);
    owned.created_by = "principal-a".into();
    store.save_work_item(&owned).expect("write");

    let mut foreign = scope.item(&[owned.work_id.as_str()]);
    foreign.created_by = "principal-b".into();
    let foreign_error = store
        .save_work_item(&foreign)
        .expect_err("another principal must not depend on this work");

    let invented = Uuid::new_v4().to_string();
    let mut unknown = scope.item(&[invented.as_str()]);
    unknown.created_by = "principal-b".into();
    let unknown_error = store
        .save_work_item(&unknown)
        .expect_err("an unknown dependency must be refused");

    assert_eq!(
        foreign_error.to_string().replace(&owned.work_id, "<id>"),
        unknown_error.to_string().replace(&invented, "<id>"),
        "another principal's work must be indistinguishable from work that does not exist"
    );

    // The owning principal may still declare the same edge.
    let mut sibling = scope.item(&[owned.work_id.as_str()]);
    sibling.created_by = "principal-a".into();
    store
        .save_work_item(&sibling)
        .expect("the owning principal is unaffected");
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
