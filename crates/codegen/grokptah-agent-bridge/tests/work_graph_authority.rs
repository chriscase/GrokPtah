//! Adversarial conformance for the durable work-graph authority (#305).
//!
//! Every test here states a property the ledger could not express before the
//! graph authority landed, and fails if the property is lost again.

use std::sync::{Arc, Barrier};
use std::thread;

use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use grokptah_agent_bridge::orchestration::{
    evaluate_admission, order_work, resolve_dependency_states, safe_id_filename,
    validate_scoped_dependency_graph, AdmissionBlock, BlockProvenance, GraphScope, OrchErrorCode,
    OrchStore, RunRecord, RunState, WorkClaim, WorkDecisionAction, WorkDependency, WorkItem,
    WorkPolicy, WorkResult, WorkState, MAX_GRAPH_SCOPE_ITEMS,
};
use grokptah_agent_bridge::{
    CompletionClaims, CompletionEvidence, CompletionObservations, CompletionUsage,
};
use tempfile::tempdir;
use uuid::Uuid;

const WORKSPACE: &str = "/tmp/work-graph-lane";

fn write_raw_work_fixture(root: &std::path::Path, item: &WorkItem) {
    let dir = root.join("work-items");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}.json", safe_id_filename(&item.work_id).unwrap())),
        serde_json::to_vec_pretty(item).unwrap(),
    )
    .unwrap();
}

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

fn complete_with_verified_evidence(
    store: &OrchStore,
    item: &WorkItem,
    claim: &WorkClaim,
    completed_at: chrono::DateTime<Utc>,
) {
    let run_id = format!("run-{}", item.work_id);
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run_id,
        )
        .unwrap();
    let evidence = CompletionEvidence {
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
        work_id: Some(item.work_id.clone()),
        run_id: Some(run_id.clone()),
        attempt_id: Some(claim.attempt.attempt_id.clone()),
    };
    let aggregates = grokptah_agent_bridge::orchestration::RunAggregates {
        verification: Some(evidence.clone()),
        ..Default::default()
    };
    store
        .save_run(&RunRecord {
            run_id: run_id.clone(),
            session_id: item.session_id,
            workspace: item.workspace.clone(),
            request_id: format!("req-{}", item.work_id),
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
            created_at: completed_at,
            updated_at: completed_at,
            terminal_result: Some("completed".into()),
            final_response: Some(
                "Changed src/lib.rs; cargo test passed; verification green.".into(),
            ),
            error_code: None,
            stop_cause: None,
            aggregates,
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();
    store
        .complete_work(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            WorkResult {
                summary: "done".into(),
                evidence: vec!["deterministic evidence".into()],
                artifacts: Vec::new(),
                failure: None,
                cancellation_reason: None,
                completed_at,
                verification: Some(evidence),
            },
        )
        .unwrap();
}

/// The proven pre-upgrade derived wait: `Blocked`, no provenance, no reason,
/// non-empty dependencies, not a container.
fn plant_legacy_derived_wait(store: &OrchStore, lane: Uuid, id: &str, dep_id: &str) -> WorkItem {
    let mut item = item_in(lane, WORKSPACE, id, "written before provenance");
    depends_on(&mut item, dep_id);
    item.state = WorkState::Blocked;
    item.blocked_reason = None;
    item.block_provenance = None;
    store.save_work_item_unchecked(&item).unwrap();
    item
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

/// The durable store is the last writer shared by service, manager, and host
/// recovery paths. A cycle introduced by an update must be rejected there as
/// well, rather than relying on the create endpoint to have seen it.
#[test]
fn checked_store_write_rejects_a_new_cycle() {
    let store = OrchStore::open(tempdir().unwrap().path()).unwrap();
    let lane = Uuid::new_v4();
    let mut first = item_in(lane, WORKSPACE, "store-a", "first");
    store.save_work_item(&first).unwrap();

    let mut second = item_in(lane, WORKSPACE, "store-b", "second");
    depends_on(&mut second, &first.work_id);
    store.save_work_item(&second).unwrap();

    depends_on(&mut first, &second.work_id);
    let error = store
        .save_work_item(&first)
        .expect_err("the store must refuse a cycle introduced by an update");
    assert!(error.to_string().contains("work dependency cycle"));
    assert_eq!(
        store
            .load_work_item("store-a")
            .unwrap()
            .unwrap()
            .dependencies,
        Vec::<WorkDependency>::new()
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

/// A container is never executable, but a manual hold on one must still remain
/// a hold. If `Container` outranks `ManuallyBlocked`, reconciliation treats the
/// item as "not waiting" and re-queues work a human stopped.
#[test]
fn a_manual_block_on_a_container_is_not_lifted_by_reconciliation() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let mut container = item_in(Uuid::new_v4(), WORKSPACE, "held-container", "coordination");
    container.is_container = true;
    store.save_work_item(&container).unwrap();

    let now = Utc::now();
    let (blocked, _) = store
        .block_work(&container.work_id, "operator", "stop the plan", None, now)
        .unwrap();
    assert_eq!(blocked.state, WorkState::Blocked);
    assert_eq!(blocked.block_provenance, Some(BlockProvenance::Manual));
    assert!(blocked.is_container);

    let states = resolve_dependency_states(&[], &blocked, GraphScope::of(&blocked));
    assert_eq!(
        evaluate_admission(&blocked, &states, now),
        AdmissionBlock::ManuallyBlocked,
        "a manual hold outranks Container"
    );

    let report = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(report.unblocked_items, 0);
    let after = store.load_work_item(&container.work_id).unwrap().unwrap();
    assert_eq!(after.state, WorkState::Blocked);
    assert_eq!(after.block_provenance, Some(BlockProvenance::Manual));
    assert_eq!(after.blocked_reason.as_deref(), Some("stop the plan"));
    assert_eq!(after.revision, blocked.revision);
    let after_states = resolve_dependency_states(&[], &after, GraphScope::of(&after));
    assert_eq!(
        evaluate_admission(&after, &after_states, now),
        AdmissionBlock::ManuallyBlocked
    );
    assert!(
        store
            .claim_work(&container.work_id, "worker", None)
            .is_err(),
        "a held container must not become executable"
    );
}

/// A record written before provenance was typed carries none. A free-text
/// reason is not the proven pre-upgrade derived shape, so the ambiguous case
/// is read as manual and released only by hand.
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

/// Pre-upgrade reconciliation persisted an unmet dependency as `Blocked` with
/// neither provenance nor reason. That exact shape must refresh as derived so
/// an upgrade does not strand ordinary waits, and must not be operator-unblocked.
#[test]
fn a_legacy_derived_wait_is_refreshed_while_dependencies_remain_pending() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let now = Utc::now();
    let store = OrchStore::open(home.path()).unwrap();
    let dependency = item_in(lane, WORKSPACE, "dep", "run me first");
    store.save_work_item(&dependency).unwrap();
    let planted = plant_legacy_derived_wait(&store, lane, "dependent", "dep");

    let states = resolve_dependency_states(
        &[dependency.clone(), planted.clone()],
        &planted,
        GraphScope::of(&planted),
    );
    assert_eq!(
        evaluate_admission(&planted, &states, now),
        AdmissionBlock::DependenciesPending,
        "the proven legacy wait is a dependency hold, not a manual one"
    );

    let first = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(first.unblocked_items, 0);
    let held = store.load_work_item("dependent").unwrap().unwrap();
    assert_eq!(held.state, WorkState::Blocked);
    assert_eq!(held.block_provenance, Some(BlockProvenance::Derived));
    assert_eq!(held.blocked_reason.as_deref(), Some("dependencies_pending"));

    let replay = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(replay.blocked_items, 0);
    assert_eq!(replay.unblocked_items, 0);
    assert_eq!(
        store.load_work_item("dependent").unwrap().unwrap().revision,
        held.revision,
        "stamping provenance is durable and must not keep consuming revisions"
    );

    let claim = store
        .claim_work("dependent", "worker", None)
        .expect_err("a still-pending legacy wait must not be handed to a worker");
    assert_eq!(claim.code, OrchErrorCode::Conflict);
    let unblock = store
        .unblock_work("dependent", "operator", "impatience", None, now)
        .expect_err("a refreshed legacy wait is reconciliation's, not an operator's");
    assert_eq!(unblock.code, OrchErrorCode::Conflict);

    drop(store);
    let reopened = OrchStore::open(home.path()).unwrap();
    let recovered = reopened.load_work_item("dependent").unwrap().unwrap();
    assert_eq!(recovered.state, WorkState::Blocked);
    assert_eq!(recovered.block_provenance, Some(BlockProvenance::Derived));
    assert_eq!(
        recovered.blocked_reason.as_deref(),
        Some("dependencies_pending")
    );
    assert_eq!(recovered.revision, held.revision);
    assert!(
        reopened.claim_work("dependent", "worker", None).is_err(),
        "restart recovery must not claim a still-pending derived wait"
    );
}

/// Opening the ledger reconciles. A pre-upgrade wait whose dependency already
/// succeeded must become claimable without a human unblock.
#[test]
fn a_legacy_derived_wait_is_lifted_when_dependencies_are_already_satisfied() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let now = Utc::now();
    let store = OrchStore::open(home.path()).unwrap();
    let mut dependency = item_in(lane, WORKSPACE, "dep", "already done");
    dependency.state = WorkState::Succeeded;
    store.save_work_item_unchecked(&dependency).unwrap();
    let planted = plant_legacy_derived_wait(&store, lane, "dependent", "dep");

    let states = resolve_dependency_states(
        &[dependency.clone(), planted.clone()],
        &planted,
        GraphScope::of(&planted),
    );
    assert_eq!(
        evaluate_admission(&planted, &states, now),
        AdmissionBlock::Admissible
    );

    let report = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(report.unblocked_items, 1);
    let ready = store.load_work_item("dependent").unwrap().unwrap();
    assert_eq!(ready.state, WorkState::Queued);
    assert_eq!(ready.block_provenance, None);
    assert_eq!(ready.blocked_reason, None);
    store
        .claim_work("dependent", "worker", None)
        .expect("a satisfied legacy wait is claimable after refresh");
}

/// Claim refreshes before it admits. An upgrade must not require a separate
/// reconcile tick for a satisfied pre-upgrade wait, and must still refuse a
/// pending one.
#[test]
fn a_claim_refreshes_a_legacy_derived_wait_without_a_prior_reconcile_tick() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let store = OrchStore::open(home.path()).unwrap();

    let mut done = item_in(lane, WORKSPACE, "done", "already done");
    done.state = WorkState::Succeeded;
    store.save_work_item_unchecked(&done).unwrap();
    plant_legacy_derived_wait(&store, lane, "ready", "done");
    store
        .claim_work("ready", "worker", None)
        .expect("claim must lift a satisfied legacy wait during refresh");
    assert_eq!(
        store.load_work_item("ready").unwrap().unwrap().state,
        WorkState::Leased
    );

    let pending_dep = item_in(lane, WORKSPACE, "pending-dep", "not done");
    store.save_work_item(&pending_dep).unwrap();
    plant_legacy_derived_wait(&store, lane, "waiting", "pending-dep");
    let denied = store
        .claim_work("waiting", "worker", None)
        .expect_err("claim must not admit a still-pending legacy wait");
    assert_eq!(denied.code, OrchErrorCode::Conflict);
    let waiting = store.load_work_item("waiting").unwrap().unwrap();
    assert_eq!(waiting.state, WorkState::Blocked);
    assert_eq!(waiting.block_provenance, Some(BlockProvenance::Derived));
    let unblock = store
        .unblock_work("waiting", "operator", "impatience", None, Utc::now())
        .expect_err("claim-stamped derived waits stay unreleasable by hand");
    assert_eq!(unblock.code, OrchErrorCode::Conflict);
}

/// Restart recovery is the upgrade path: `OrchStore::open` reconciles once.
#[test]
fn a_legacy_derived_wait_is_lifted_across_restart_when_dependencies_succeeded() {
    let home = tempdir().unwrap();
    let lane = Uuid::new_v4();
    let store = OrchStore::open(home.path()).unwrap();
    let mut dependency = item_in(lane, WORKSPACE, "dep", "already done");
    dependency.state = WorkState::Succeeded;
    store.save_work_item_unchecked(&dependency).unwrap();
    plant_legacy_derived_wait(&store, lane, "dependent", "dep");
    drop(store);

    let reopened = OrchStore::open(home.path()).unwrap();
    let recovered = reopened.load_work_item("dependent").unwrap().unwrap();
    assert_eq!(recovered.state, WorkState::Queued);
    assert_eq!(recovered.block_provenance, None);
    reopened
        .claim_work("dependent", "worker", None)
        .expect("open-time recovery must lift a satisfied legacy wait");
}

/// The recognizer is deliberately narrow. A free-text reason, a missing
/// dependency list, a container, and a typed manual hold must stay fail-closed.
#[test]
fn the_legacy_derived_rule_does_not_convert_ambiguous_or_manual_holds() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    let now = Utc::now();
    let dependency = item_in(lane, WORKSPACE, "dep", "run me first");
    store.save_work_item(&dependency).unwrap();

    let mut looks_derived = item_in(lane, WORKSPACE, "looks-derived", "ambiguous reason");
    depends_on(&mut looks_derived, "dep");
    looks_derived.state = WorkState::Blocked;
    looks_derived.blocked_reason = Some("dependencies_pending".into());
    looks_derived.block_provenance = None;
    store.save_work_item_unchecked(&looks_derived).unwrap();

    let mut no_deps = item_in(lane, WORKSPACE, "no-deps", "blocked with no edges");
    no_deps.state = WorkState::Blocked;
    no_deps.blocked_reason = None;
    no_deps.block_provenance = None;
    store.save_work_item_unchecked(&no_deps).unwrap();

    let mut container = item_in(lane, WORKSPACE, "held-container", "coordination");
    depends_on(&mut container, "dep");
    container.is_container = true;
    container.state = WorkState::Blocked;
    container.blocked_reason = None;
    container.block_provenance = None;
    store.save_work_item_unchecked(&container).unwrap();

    let mut with_deps = item_in(lane, WORKSPACE, "manual-with-deps", "stop this too");
    depends_on(&mut with_deps, "dep");
    store.save_work_item(&with_deps).unwrap();
    let (manual, _) = store
        .block_work(&with_deps.work_id, "operator", "stop for review", None, now)
        .unwrap();
    assert_eq!(manual.block_provenance, Some(BlockProvenance::Manual));

    let report = store.reconcile_workloads_at(now).unwrap();
    assert_eq!(report.unblocked_items, 0);

    for id in [
        "looks-derived",
        "no-deps",
        "held-container",
        "manual-with-deps",
    ] {
        let after = store.load_work_item(id).unwrap().unwrap();
        assert_eq!(after.state, WorkState::Blocked, "{id} must stay blocked");
        assert!(
            store.claim_work(id, "worker", None).is_err(),
            "{id} must not become claimable"
        );
        assert_ne!(
            after.block_provenance,
            Some(BlockProvenance::Derived),
            "{id} must not be rewritten as derived"
        );
    }
    assert_eq!(
        store
            .load_work_item("manual-with-deps")
            .unwrap()
            .unwrap()
            .block_provenance,
        Some(BlockProvenance::Manual)
    );
    assert_eq!(
        store
            .load_work_item("looks-derived")
            .unwrap()
            .unwrap()
            .block_provenance,
        None
    );
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
    store.save_work_item_unchecked(&orphan).unwrap();
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
    complete_with_verified_evidence(&reopened, &dependency, &claim, now);
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
    store.save_work_item_unchecked(&waiting).unwrap();
    store.save_work_item_unchecked(&sibling).unwrap();

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

#[test]
fn scoped_reads_refuse_to_materialize_an_oversized_lane() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    for index in 0..=MAX_GRAPH_SCOPE_ITEMS {
        let item = item_in(
            lane,
            WORKSPACE,
            &format!("ceiling-{index}"),
            "bounded read fixture",
        );
        write_raw_work_fixture(home.path(), &item);
    }
    let error = store
        .scoped_work_items(lane, WORKSPACE)
        .expect_err("lane reads must refuse more than the graph ceiling");
    assert!(error.to_string().contains("bounded read refused"));
    let error = store
        .work_graph_scoped(lane, WORKSPACE, Utc::now())
        .expect_err("graph projections must use the same ceiling");
    assert!(error.to_string().contains("bounded read refused"));
}

/// Dependency-free creates used to skip the graph ceiling. The 4096th
/// independent item is admitted; the 4097th is refused; an update of an item
/// already at the ceiling still lands; reads stay ordered.
#[test]
fn a_dependency_free_create_is_bound_by_the_graph_ceiling() {
    let home = tempdir().unwrap();
    let store = OrchStore::open(home.path()).unwrap();
    let lane = Uuid::new_v4();
    let minted = Utc.with_ymd_and_hms(2026, 8, 30, 12, 0, 0).unwrap();
    for index in 0..(MAX_GRAPH_SCOPE_ITEMS - 1) {
        let mut item = WorkItem::new_at(
            "test",
            "independent",
            lane,
            WORKSPACE,
            "operator",
            WorkPolicy::default(),
            minted,
        )
        .unwrap();
        item.work_id = format!("ceiling-{index:04}");
        write_raw_work_fixture(home.path(), &item);
    }

    let mut at_ceiling = WorkItem::new_at(
        "test",
        "independent",
        lane,
        WORKSPACE,
        "operator",
        WorkPolicy::default(),
        minted,
    )
    .unwrap();
    at_ceiling.work_id = format!("ceiling-{:04}", MAX_GRAPH_SCOPE_ITEMS - 1);
    store
        .save_work_item(&at_ceiling)
        .expect("the 4096th independent item is admitted");

    let mut overflow = WorkItem::new_at(
        "test",
        "independent",
        lane,
        WORKSPACE,
        "operator",
        WorkPolicy::default(),
        minted,
    )
    .unwrap();
    overflow.work_id = format!("ceiling-{:04}", MAX_GRAPH_SCOPE_ITEMS);
    let error = store
        .save_work_item(&overflow)
        .expect_err("the 4097th independent item is refused");
    assert!(
        error.to_string().contains("scope holds more than"),
        "overflow must name the graph ceiling: {error}"
    );

    let mut existing = store.load_work_item("ceiling-0000").unwrap().unwrap();
    existing.objective = "updated at the ceiling".into();
    existing.bump();
    store
        .save_work_item(&existing)
        .expect("an update of an existing item at the ceiling still lands");

    let lane_items = store.scoped_work_items(lane, WORKSPACE).unwrap();
    assert_eq!(lane_items.len(), MAX_GRAPH_SCOPE_ITEMS);
    let ids: Vec<&str> = lane_items
        .iter()
        .map(|item| item.work_id.as_str())
        .collect();
    let last_id = format!("ceiling-{:04}", MAX_GRAPH_SCOPE_ITEMS - 1);
    assert_eq!(ids.first().copied(), Some("ceiling-0000"));
    assert_eq!(ids.last().copied(), Some(last_id.as_str()));
    assert_eq!(
        store
            .load_work_item("ceiling-0000")
            .unwrap()
            .unwrap()
            .objective,
        "updated at the ceiling"
    );
    let nodes = store.work_graph_scoped(lane, WORKSPACE, minted).unwrap();
    assert_eq!(nodes.len(), MAX_GRAPH_SCOPE_ITEMS);
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.work_id.as_str())
            .collect::<Vec<_>>(),
        ids
    );
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
