//! Adversarial conformance for the durable work-graph authority (#305).
//!
//! Every test here states a property the ledger could not express before the
//! graph authority landed, and fails if the property is lost again.

use std::sync::{Arc, Barrier};
use std::thread;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use grokptah_agent_bridge::orchestration::{
    evaluate_admission, order_work, resolve_dependency_states, validate_scoped_dependency_graph,
    AdmissionBlock, BlockProvenance, GraphScope, OrchErrorCode, OrchStore, WorkDecisionAction,
    WorkDependency, WorkItem, WorkPolicy, WorkState,
};
use tempfile::tempdir;
use uuid::Uuid;

const WORKSPACE: &str = "/tmp/work-graph-lane";

fn item_in(lane: Uuid, workspace: &str, id: &str, objective: &str) -> WorkItem {
    let mut item = WorkItem::new(
        "test",
        objective,
        lane,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .expect("construct work item");
    item.work_id = id.to_string();
    item.validate().expect("fixture item is valid");
    item
}

fn depends_on(item: &mut WorkItem, work_id: &str) {
    item.dependencies.push(WorkDependency {
        work_id: work_id.to_string(),
        required_state: WorkState::Succeeded,
    });
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Two lanes sharing one workspace are still two lanes. A dependency declared
/// across them must not resolve, or the depending item's `Blocked`/`Queued`
/// transitions become a live read of the other lane's progress.
#[test]
fn same_workspace_cross_lane_dependency_is_denied() {
    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    let sibling = item_in(theirs, WORKSPACE, "sibling-work", "another lane's work");
    let mut candidate = item_in(mine, WORKSPACE, "my-work", "my work");
    depends_on(&mut candidate, &sibling.work_id);

    // The sibling is stored, in the very same workspace, and still invisible.
    let ledger = vec![sibling.clone()];
    let error = validate_scoped_dependency_graph(&ledger, &candidate, GraphScope::of(&candidate))
        .expect_err("a cross-lane dependency must not resolve");
    assert_eq!(error.code, OrchErrorCode::InvalidRequest);
    assert_eq!(
        error.message,
        "dependency sibling-work is not resolvable in this scope"
    );

    // Resolution agrees with validation: declared, and unresolvable.
    let states = resolve_dependency_states(&ledger, &candidate, GraphScope::of(&candidate));
    assert_eq!(states.get("sibling-work"), Some(&None));
    assert_eq!(
        evaluate_admission(&candidate, &states, Utc::now()),
        AdmissionBlock::DependencyUnresolved
    );
}

/// Unknown, another lane's, another workspace's, and terminal-in-another-lane
/// work must all answer with the same bytes. Any difference turns a dependency
/// declaration into an existence oracle for work the caller may not observe.
#[test]
fn unknown_inactive_and_detached_work_collapse_to_one_answer() {
    let mine = Uuid::new_v4();
    let mut candidate = item_in(mine, WORKSPACE, "my-work", "my work");
    depends_on(&mut candidate, "probe");
    let scope = GraphScope::of(&candidate);

    // The same id is probed against four different ledgers.
    let mut detached = item_in(mine, "/tmp/some-other-workspace", "probe", "detached");
    detached.state = WorkState::Queued;
    let mut cancelled = item_in(Uuid::new_v4(), WORKSPACE, "probe", "inactive");
    cancelled.state = WorkState::Cancelled;
    let mut succeeded = item_in(Uuid::new_v4(), WORKSPACE, "probe", "finished");
    succeeded.state = WorkState::Succeeded;

    let ledgers: Vec<Vec<WorkItem>> =
        vec![Vec::new(), vec![detached], vec![cancelled], vec![succeeded]];
    let answers = ledgers
        .iter()
        .map(|ledger| {
            let error = validate_scoped_dependency_graph(ledger, &candidate, scope)
                .expect_err("nothing outside the lane may resolve");
            (error.code, error.message)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        answers[0].1,
        "dependency probe is not resolvable in this scope"
    );
    for answer in &answers {
        assert_eq!(
            answer, &answers[0],
            "a caller must not tell absent work from unobservable work"
        );
    }
}

/// A candidate that is not in the lane it claims is refused before any graph
/// work is done, and with the scope error rather than a dependency one.
#[test]
fn candidate_outside_its_own_scope_is_refused() {
    let candidate = item_in(Uuid::new_v4(), WORKSPACE, "my-work", "my work");
    let foreign = GraphScope {
        session_id: Uuid::new_v4(),
        workspace: WORKSPACE,
    };
    let error = validate_scoped_dependency_graph(&[], &candidate, foreign)
        .expect_err("a candidate must belong to the scope it is validated in");
    assert_eq!(error.code, OrchErrorCode::WorkspaceMismatch);
}

// ---------------------------------------------------------------------------
// Graph shape
// ---------------------------------------------------------------------------

/// `WorkItem::validate` sees one item and can only reject a self-edge. A ring
/// is a graph-level fact, and without this check every item on it sits in
/// `Blocked` forever with no operator-visible cause.
#[test]
fn dependency_cycles_and_duplicate_edges_are_rejected() {
    let lane = Uuid::new_v4();

    let mut b = item_in(lane, WORKSPACE, "cycle-b", "second");
    depends_on(&mut b, "cycle-a");
    let mut a = item_in(lane, WORKSPACE, "cycle-a", "first");
    depends_on(&mut a, "cycle-b");
    // An innocent item downstream of the ring. Kahn's algorithm cannot peel
    // it either, so naming the unpeeled set would accuse it of being a member.
    let mut downstream = item_in(lane, WORKSPACE, "z-downstream", "not on the ring");
    depends_on(&mut downstream, "cycle-a");

    let scope = GraphScope::of(&a);
    let error = validate_scoped_dependency_graph(&[b.clone(), downstream.clone()], &a, scope)
        .expect_err("a two-item ring must be rejected");
    assert_eq!(
        error.message, "work dependency cycle: cycle-a -> cycle-b -> cycle-a",
        "the report must be a closed walk of actual ring members"
    );
    assert!(
        !error.message.contains("z-downstream"),
        "an item downstream of a ring is not on it"
    );

    // The same graph presented in a different order reports the same ring.
    let reordered = validate_scoped_dependency_graph(&[downstream, b], &a, scope)
        .expect_err("a two-item ring must be rejected");
    assert_eq!(reordered.message, error.message);

    // A duplicate edge and a self-edge are each named for what they are.
    let mut duplicated = item_in(lane, WORKSPACE, "dup", "duplicate edges");
    let target = item_in(lane, WORKSPACE, "target", "target");
    depends_on(&mut duplicated, "target");
    depends_on(&mut duplicated, "target");
    assert_eq!(
        validate_scoped_dependency_graph(&[target], &duplicated, scope)
            .expect_err("a duplicate edge must be rejected")
            .message,
        "work item declares dependency target more than once"
    );

    let mut narcissist = item_in(lane, WORKSPACE, "self", "self edge");
    depends_on(&mut narcissist, "self");
    assert_eq!(
        validate_scoped_dependency_graph(&[], &narcissist, scope)
            .expect_err("a self edge must be rejected")
            .message,
        "work item depends on itself"
    );
}

/// A longer ring still reports a closed walk, and reports the same one however
/// the ledger happens to hand the nodes over.
#[test]
fn cycle_reporting_is_deterministic_for_a_longer_ring() {
    let lane = Uuid::new_v4();
    let mut a = item_in(lane, WORKSPACE, "ring-a", "a");
    let mut b = item_in(lane, WORKSPACE, "ring-b", "b");
    let mut c = item_in(lane, WORKSPACE, "ring-c", "c");
    depends_on(&mut a, "ring-b");
    depends_on(&mut b, "ring-c");
    depends_on(&mut c, "ring-a");
    let scope = GraphScope::of(&a);

    let forward = validate_scoped_dependency_graph(&[b.clone(), c.clone()], &a, scope)
        .expect_err("a three-item ring must be rejected");
    let reversed = validate_scoped_dependency_graph(&[c, b], &a, scope)
        .expect_err("a three-item ring must be rejected");
    assert_eq!(forward.message, reversed.message);
    assert_eq!(
        forward.message,
        "work dependency cycle: ring-a -> ring-b -> ring-c -> ring-a"
    );
}

/// An acyclic diamond is not a cycle, and a dependency that finished in the
/// wrong terminal state is distinguished from one that is merely pending.
#[test]
fn acyclic_graphs_pass_and_terminal_dependencies_are_unsatisfiable() {
    let lane = Uuid::new_v4();
    let root = item_in(lane, WORKSPACE, "root", "root");
    let mut left = item_in(lane, WORKSPACE, "left", "left");
    let mut right = item_in(lane, WORKSPACE, "right", "right");
    depends_on(&mut left, "root");
    depends_on(&mut right, "root");
    let mut join = item_in(lane, WORKSPACE, "join", "join");
    depends_on(&mut join, "left");
    depends_on(&mut join, "right");
    let scope = GraphScope::of(&join);
    let lane_items = vec![root.clone(), left.clone(), right.clone()];
    validate_scoped_dependency_graph(&lane_items, &join, scope).expect("a diamond is acyclic");

    let now = Utc::now();
    let states = resolve_dependency_states(&lane_items, &join, scope);
    assert_eq!(
        evaluate_admission(&join, &states, now),
        AdmissionBlock::DependenciesPending
    );

    let mut cancelled_left = left;
    cancelled_left.state = WorkState::Cancelled;
    let states = resolve_dependency_states(&[root, cancelled_left, right], &join, scope);
    assert_eq!(
        evaluate_admission(&join, &states, now),
        AdmissionBlock::DependencyUnsatisfiable,
        "a dependency that ended in the wrong terminal state can never become ready"
    );
}

// ---------------------------------------------------------------------------
// Deterministic ordering
// ---------------------------------------------------------------------------

/// Priority and creation instant alone are a partial order: two items created
/// inside one clock tick used to fall back to `read_dir` order, which differs
/// between hosts and between restarts on one host.
#[test]
fn work_order_is_total_and_survives_a_restart() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let minted = Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0).unwrap();

    let mut tied: Vec<WorkItem> = ["w-delta", "w-alpha", "w-charlie", "w-bravo"]
        .iter()
        .map(|id| {
            let mut item = WorkItem::new_at(
                "test",
                "tied",
                lane,
                WORKSPACE,
                "operator",
                WorkPolicy::default(),
                minted,
            )
            .unwrap();
            item.work_id = (*id).to_string();
            item
        })
        .collect();
    // Same priority, same instant: only the id can break the tie.
    let mut urgent = tied[0].clone();
    urgent.work_id = "z-urgent".into();
    urgent.priority = 10;
    tied.push(urgent);

    order_work(&mut tied);
    let ordered: Vec<&str> = tied.iter().map(|item| item.work_id.as_str()).collect();
    assert_eq!(
        ordered,
        vec!["z-urgent", "w-alpha", "w-bravo", "w-charlie", "w-delta"],
        "priority first, then instant, then id"
    );

    let store = OrchStore::open(home.path()).unwrap();
    for item in &tied {
        store.save_work_item(item).unwrap();
    }
    let first: Vec<String> = store
        .list_work_items()
        .unwrap()
        .into_iter()
        .map(|item| item.work_id)
        .collect();
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let after_restart: Vec<String> = reopened
        .list_work_items()
        .unwrap()
        .into_iter()
        .map(|item| item.work_id)
        .collect();
    assert_eq!(first, ordered);
    assert_eq!(
        after_restart, first,
        "the ledger order must survive restart"
    );
}

// ---------------------------------------------------------------------------
// Lifecycle authority
// ---------------------------------------------------------------------------

/// Reconciliation used to lift any `Blocked` item whose dependencies were
/// satisfied, and an operator-blocked item has no dependencies to satisfy — so
/// the next tick silently re-queued, and then executed, work a human stopped.
#[test]
fn a_manual_block_is_not_lifted_by_reconciliation() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let item = item_in(Uuid::new_v4(), WORKSPACE, "held", "stop this");
    store.save_work_item(&item).unwrap();

    let now = Utc::now();
    let (blocked, _) = store
        .block_work(&item.work_id, "operator", "stop for review", None, now)
        .unwrap();
    assert_eq!(blocked.state, WorkState::Blocked);
    assert_eq!(blocked.block_provenance, Some(BlockProvenance::Manual));

    let report = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(report.unblocked_items, 0);
    let after = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(after.state, WorkState::Blocked);
    assert_eq!(after.block_provenance, Some(BlockProvenance::Manual));
    assert_eq!(
        after.blocked_reason.as_deref(),
        Some("stop for review"),
        "reconciliation must not overwrite an operator's stated reason"
    );
    assert!(
        store.claim_work(&item.work_id, "worker", None).is_err(),
        "held work must not be claimable"
    );

    // The hold is released deliberately, under a revision fence, and only then.
    let (released, decision) = store
        .unblock_work(
            &item.work_id,
            "operator",
            "review complete",
            Some(after.revision),
            now,
        )
        .unwrap();
    assert_eq!(released.state, WorkState::Queued);
    assert_eq!(released.block_provenance, None);
    assert_eq!(released.blocked_reason, None);
    assert_eq!(decision.action, WorkDecisionAction::Unblock);
    store
        .claim_work(&item.work_id, "worker", None)
        .expect("released work is claimable");
}

/// A record written before provenance was typed carries none. Reading that as
/// "derived" would let an upgrade re-queue work a human had stopped, so the
/// ambiguous case is read as manual and released only by hand.
#[test]
fn a_block_of_unknown_provenance_fails_closed() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let mut legacy = item_in(Uuid::new_v4(), WORKSPACE, "legacy", "written before #305");
    legacy.state = WorkState::Blocked;
    legacy.blocked_reason = Some("blocked by an older build".into());
    legacy.block_provenance = None;
    store.save_work_item(&legacy).unwrap();

    let report = store.reconcile_workloads_at(Utc::now()).unwrap();
    assert_eq!(report.unblocked_items, 0);
    let after = store.load_work_item(&legacy.work_id).unwrap().unwrap();
    assert_eq!(after.state, WorkState::Blocked);
    assert_eq!(after.block_provenance, None);
    assert!(store.claim_work(&legacy.work_id, "worker", None).is_err());

    store
        .unblock_work(
            &legacy.work_id,
            "operator",
            "reviewed after upgrade",
            None,
            Utc::now(),
        )
        .expect("an ambiguous hold is releasable by hand");
}

/// A derived hold belongs to reconciliation, which re-derives it every tick.
/// Letting an operator clear it by hand would be a lie that lasts one tick.
#[test]
fn a_derived_block_is_not_released_by_hand() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    let dependency = item_in(lane, WORKSPACE, "dep", "run me first");
    let mut dependent = item_in(lane, WORKSPACE, "dependent", "run me second");
    depends_on(&mut dependent, &dependency.work_id);
    store.save_work_item(&dependency).unwrap();
    store.save_work_item(&dependent).unwrap();

    store.reconcile_workloads_at(Utc::now()).unwrap();
    let held = store.load_work_item(&dependent.work_id).unwrap().unwrap();
    assert_eq!(held.block_provenance, Some(BlockProvenance::Derived));
    let error = store
        .unblock_work(
            &dependent.work_id,
            "operator",
            "impatience",
            None,
            Utc::now(),
        )
        .expect_err("a dependency hold is not an operator's to release");
    assert_eq!(error.code, OrchErrorCode::Conflict);
}

/// `Queued` is not admission. Reconciliation may not have run since a
/// dependency was declared, so the claim path consults the one evaluator
/// rather than trusting the persisted state alone.
#[test]
fn a_claim_consults_the_admission_evaluator_not_just_the_state() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    // An edge that names work in another lane. It reaches the ledger only by
    // bypassing `create_work`; the claim path must still refuse it.
    let mut orphan = item_in(lane, WORKSPACE, "orphan", "depends on nothing visible");
    depends_on(&mut orphan, "work-in-another-lane");
    store.save_work_item(&orphan).unwrap();
    assert_eq!(
        store
            .load_work_item(&orphan.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Queued,
        "the fixture starts in the state a claim would otherwise trust"
    );

    let error = store
        .claim_work(&orphan.work_id, "worker", None)
        .expect_err("an unresolvable edge must not be handed to a worker");
    assert_eq!(error.code, OrchErrorCode::Conflict);
    let refreshed = store.load_work_item(&orphan.work_id).unwrap().unwrap();
    assert_eq!(refreshed.state, WorkState::Blocked);
    assert_eq!(
        refreshed.blocked_reason.as_deref(),
        Some("dependency_unresolved"),
        "the operator-visible cause names the actual reason"
    );
}

/// Exactly one claimant wins a contended lease, and the losers are told that
/// they lost without being told who won.
#[test]
fn concurrent_claims_have_one_winner_and_name_no_holder() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let item = item_in(Uuid::new_v4(), WORKSPACE, "contended", "one winner only");
    store.save_work_item(&item).unwrap();

    const CLAIMANTS: usize = 8;
    let barrier = Arc::new(Barrier::new(CLAIMANTS));
    let mut handles = Vec::new();
    for index in 0..CLAIMANTS {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        let work_id = item.work_id.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            store
                .claim_work(&work_id, &format!("worker-{index}"), None)
                .map(|claim| claim.attempt.claimant_id)
                .map_err(|error| error.message)
        }));
    }
    let results: Vec<Result<String, String>> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners: Vec<&String> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(winners.len(), 1, "a lease is single-owner");
    let winner = winners[0].clone();
    for failure in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            !failure.contains(&winner),
            "a claim conflict must not attribute the holder: {failure}"
        );
    }
    assert_eq!(
        store.list_work_attempts(Some(&item.work_id)).unwrap().len(),
        1,
        "a lost race must leave no attempt behind"
    );
}

// ---------------------------------------------------------------------------
// Restart, replay, and atomicity
// ---------------------------------------------------------------------------

/// Reconciliation is a fixpoint: running it again, or running it again after a
/// restart, must move nothing and consume no revisions.
#[test]
fn reconciliation_is_idempotent_across_replay_and_restart() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let now = Utc::now();
    let store = OrchStore::open(home.path()).unwrap();
    let dependency = item_in(lane, WORKSPACE, "dep", "run me first");
    let mut dependent = item_in(lane, WORKSPACE, "dependent", "run me second");
    depends_on(&mut dependent, &dependency.work_id);
    store.save_work_item(&dependency).unwrap();
    store.save_work_item(&dependent).unwrap();

    let first = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(first.blocked_items, 1);
    let settled = store.load_work_item(&dependent.work_id).unwrap().unwrap();
    assert_eq!(settled.state, WorkState::Blocked);

    let replay = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(replay.blocked_items, 0);
    assert_eq!(replay.unblocked_items, 0);
    assert_eq!(
        store
            .load_work_item(&dependent.work_id)
            .unwrap()
            .unwrap()
            .revision,
        settled.revision,
        "a replayed pass must not consume a revision"
    );
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let after_restart = reopened.reconcile_workloads_at(now).unwrap();
    assert_eq!(after_restart.blocked_items, 0);
    assert_eq!(after_restart.unblocked_items, 0);
    let recovered = reopened
        .load_work_item(&dependent.work_id)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.revision, settled.revision);
    assert_eq!(recovered.state, WorkState::Blocked);
    assert_eq!(
        recovered.block_provenance,
        Some(BlockProvenance::Derived),
        "provenance must survive the restart, or the hold changes meaning"
    );

    // Completing the dependency lifts the hold exactly once.
    let claim = reopened
        .claim_work(&dependency.work_id, "worker", None)
        .unwrap();
    reopened
        .complete_work(
            &dependency.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            grokptah_agent_bridge::orchestration::WorkResult {
                summary: "done".into(),
                evidence: vec!["deterministic evidence".into()],
                artifacts: Vec::new(),
                failure: None,
                cancellation_reason: None,
                completed_at: now,
            },
        )
        .unwrap();
    let lifted = reopened
        .reconcile_workloads_at(now + ChronoDuration::seconds(1))
        .unwrap();
    assert_eq!(lifted.unblocked_items, 1);
    let again = reopened
        .reconcile_workloads_at(now + ChronoDuration::seconds(2))
        .unwrap();
    assert_eq!(again.unblocked_items, 0);
}

/// A rejected mutation must leave neither a mutated record nor a decision
/// receipt. A half-applied assignment is worse than a refused one.
#[test]
fn a_refused_transition_writes_nothing() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let item = item_in(Uuid::new_v4(), WORKSPACE, "fenced", "guard my revision");
    store.save_work_item(&item).unwrap();
    let now = Utc::now();
    let (blocked, _) = store
        .block_work(&item.work_id, "operator", "hold", None, now)
        .unwrap();
    let decisions_before = store.list_work_decisions(&item.work_id).unwrap().len();

    let error = store
        .unblock_work(
            &item.work_id,
            "operator",
            "release",
            Some(blocked.revision + 7),
            now,
        )
        .expect_err("a stale revision fence must refuse the write");
    assert_eq!(error.code, OrchErrorCode::StaleVersion);
    let after = store.load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(after.revision, blocked.revision);
    assert_eq!(after.state, WorkState::Blocked);
    assert_eq!(after.block_provenance, Some(BlockProvenance::Manual));
    assert_eq!(
        store.list_work_decisions(&item.work_id).unwrap().len(),
        decisions_before,
        "a refused transition must not leave a decision receipt"
    );
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// The operator view of a lane's graph is rendered wherever a client runs. It
/// must carry the shape of the caller's own lane and nothing about the
/// workspace on disk, the principals involved, what the work says, or who is
/// executing it.
#[test]
fn the_graph_projection_leaks_no_lane_workspace_or_lease_detail() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();

    let secret_objective = "run /Users/someone/secrets/deploy.sh with grok-4 via the Bash tool";
    let mut visible = item_in(mine, WORKSPACE, "mine-visible", secret_objective);
    visible.assigned_agent_id = Some("agent-private-42".into());
    visible.created_by = "token-abcdef".into();
    let mut waiting = item_in(mine, WORKSPACE, "mine-waiting", "waits on the first");
    depends_on(&mut waiting, "mine-visible");
    depends_on(&mut waiting, "sibling-secret");
    let sibling = item_in(theirs, WORKSPACE, "sibling-secret", "another lane's work");
    store.save_work_item(&visible).unwrap();
    store.save_work_item(&waiting).unwrap();
    store.save_work_item(&sibling).unwrap();

    let claim = store
        .claim_work(&visible.work_id, "worker-private", None)
        .unwrap();

    let nodes = store
        .work_graph_scoped(mine, WORKSPACE, Utc::now())
        .unwrap();
    let ids: Vec<&str> = nodes.iter().map(|node| node.work_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["mine-visible", "mine-waiting"],
        "the projection is exactly this lane, in the ledger's order"
    );
    assert_eq!(nodes[0].admission, AdmissionBlock::AttemptActive);
    assert_eq!(nodes[1].admission, AdmissionBlock::DependencyUnresolved);
    assert_eq!(nodes[1].dependencies, vec!["mine-visible".to_string()]);
    assert_eq!(
        nodes[1].unresolved_dependencies, 1,
        "an out-of-lane edge is counted, never named"
    );

    let payload = serde_json::to_string(&nodes).unwrap();
    for secret in [
        WORKSPACE,
        "sibling-secret",
        "agent-private-42",
        "token-abcdef",
        "worker-private",
        "/Users/someone/secrets/deploy.sh",
        "grok-4",
        "Bash",
        claim.attempt.attempt_id.as_str(),
        claim.lease_token.as_str(),
        claim.attempt.lease_token_hash.as_str(),
    ] {
        assert!(
            !payload.contains(secret),
            "the redacted projection leaked {secret}: {payload}"
        );
    }

    // A lane with nothing in it sees nothing, rather than someone else's work.
    let empty = store
        .work_graph_scoped(Uuid::new_v4(), WORKSPACE, Utc::now())
        .unwrap();
    assert!(empty.is_empty());
}

/// The lane-scoped read a writer validates against is the same lane the
/// projection shows: neither may include a sibling's records.
#[test]
fn scoped_reads_never_include_a_sibling_lane() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    store
        .save_work_item(&item_in(mine, WORKSPACE, "mine", "mine"))
        .unwrap();
    store
        .save_work_item(&item_in(theirs, WORKSPACE, "theirs", "theirs"))
        .unwrap();
    store
        .save_work_item(&item_in(
            mine,
            "/tmp/other-workspace",
            "detached",
            "detached",
        ))
        .unwrap();

    let lane = store.scoped_work_items(mine, WORKSPACE).unwrap();
    let ids: Vec<&str> = lane.iter().map(|item| item.work_id.as_str()).collect();
    assert_eq!(ids, vec!["mine"]);
    assert_eq!(store.list_work_items().unwrap().len(), 3);
}
