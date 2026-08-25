//! Scheduling, restart safety, cancellation, and quorum gates.

mod support;

use grokptah_swarm_control_plane::{
    DispatchProbe, DispatchState, EvidenceEntry, FailurePolicy, QuorumRule, ReviewVerdict,
    SwarmController, SwarmErrorCode, SwarmLifecycle, SwarmState, TaskOutcome, TaskState,
    derive_dispatch_id,
};
use support::*;

fn state_of(swarm: &SwarmController, id: &str) -> TaskState {
    swarm
        .state()
        .task(&task_id(id))
        .unwrap_or_else(|| panic!("task {id} exists"))
        .state
}

fn planned_ids(swarm: &SwarmController, at_time: chrono::DateTime<chrono::Utc>) -> Vec<String> {
    swarm
        .plan_dispatches(at_time)
        .into_iter()
        .map(|intent| intent.task_id.as_str().to_string())
        .collect()
}

// ── dependency ordering and parallel readiness ───────────────────────────

#[test]
fn only_the_root_is_ready_before_anything_runs() {
    let swarm = SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    assert_eq!(state_of(&swarm, "t-root"), TaskState::Ready);
    for downstream in ["t-a", "t-b", "t-review-a", "t-review-b", "t-synth"] {
        assert_eq!(
            state_of(&swarm, downstream),
            TaskState::Pending,
            "{downstream} must wait on its dependencies"
        );
    }
    assert_eq!(planned_ids(&swarm, at(0)), vec!["t-root"]);
}

#[test]
fn parallel_branches_become_ready_together() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));

    assert_eq!(state_of(&swarm, "t-a"), TaskState::Ready);
    assert_eq!(state_of(&swarm, "t-b"), TaskState::Ready);
    assert_eq!(planned_ids(&swarm, at(2)), vec!["t-a", "t-b"]);
}

#[test]
fn dispatch_order_is_deterministic_by_priority_then_identity() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_in_flight = 1;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));

    // t-a carries the higher priority, so it is admitted into the only slot.
    assert_eq!(planned_ids(&swarm, at(2)), vec!["t-a"]);

    // With equal priority the tie breaks on task ID, so the order still never
    // depends on hashing or iteration order.
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.tasks[1].priority = 5;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));
    assert_eq!(planned_ids(&swarm, at(2)), vec!["t-a", "t-b"]);
}

// ── admission ────────────────────────────────────────────────────────────

#[test]
fn the_concurrency_bound_caps_simultaneous_dispatches() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_in_flight = 1;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));

    let intents = swarm.plan_dispatches(at(2));
    assert_eq!(intents.len(), 1, "only one slot is free");
    swarm
        .record_dispatch_requested(&intents[0], None, at(2))
        .expect("first dispatch fits");

    assert!(
        swarm.plan_dispatches(at(3)).is_empty(),
        "no slot remains while a child is live"
    );
}

#[test]
fn recording_past_the_concurrency_bound_is_refused() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.admission.max_in_flight = 1;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));

    let a = intent_for(&swarm.plan_dispatches(at(2)), "t-a");
    swarm
        .record_dispatch_requested(&a, None, at(2))
        .expect("first dispatch fits");

    // Derive the intent the scheduler would have proposed for the sibling and
    // try to force it through anyway.
    let forced =
        derive_dispatch_id(&swarm.spec().swarm_id.clone(), &task_id("t-b"), 1).expect("derivable");
    let intent = grokptah_swarm_control_plane::DispatchIntent {
        dispatch_id: forced,
        task_id: task_id("t-b"),
        worker_id: worker_id("impl-grok"),
        attempt: 1,
        isolation: grokptah_swarm_control_plane::IsolationRequirement::Worktree,
        requires_computer_use: false,
    };
    let error = swarm
        .record_dispatch_requested(&intent, None, at(3))
        .expect_err("the bound must hold even against a hand-built intent");
    assert_eq!(error.code, SwarmErrorCode::BoundExceeded);
}

// ── dispatch identity, duplicate suppression, replay ─────────────────────

#[test]
fn dispatch_identity_is_content_derived_and_stable_across_replans() {
    let swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let first = swarm.plan_dispatches(at(1));
    let second = swarm.plan_dispatches(at(600));
    assert_eq!(
        first, second,
        "within the budget window, planning is a pure projection of state"
    );

    let expected = derive_dispatch_id(&swarm.spec().swarm_id.clone(), &task_id("t-only"), 1)
        .expect("derivable");
    assert_eq!(first[0].dispatch_id, expected);
}

#[test]
fn replaying_a_recorded_dispatch_never_writes_twice() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);

    let first = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("first write");
    assert_eq!(first.state, DispatchState::Requested);
    assert_eq!(swarm.state().total_dispatches, 1);

    // The owner crashed after the durable write and is replaying the same
    // intent. The stored record comes back untouched.
    let replay = swarm
        .record_dispatch_requested(&intent, None, at(2))
        .expect("replay is idempotent");
    assert_eq!(replay, first);
    assert_eq!(swarm.state().total_dispatches, 1, "no second charge");
    assert_eq!(swarm.state().dispatches.len(), 1, "no second record");
    assert_eq!(
        swarm
            .state()
            .task(&task_id("t-only"))
            .expect("task")
            .attempts,
        1,
        "no second attempt"
    );
}

#[test]
fn a_live_task_is_never_replanned() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");

    assert!(
        swarm.plan_dispatches(at(2)).is_empty(),
        "a dispatching task must not be proposed again"
    );
}

#[test]
fn a_forged_dispatch_identity_is_refused() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let mut intent = swarm.plan_dispatches(at(1)).remove(0);
    intent.dispatch_id =
        grokptah_swarm_control_plane::DispatchId::parse("not-the-derived-identity")
            .expect("shape is valid");

    let error = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect_err("identity must be content-derived");
    assert_eq!(error.code, SwarmErrorCode::Conflict);
}

// ── restart safety ───────────────────────────────────────────────────────

fn reload(swarm: SwarmController) -> SwarmController {
    let json = serde_json::to_string(swarm.state()).expect("state serializes");
    let restored: SwarmState = serde_json::from_str(&json).expect("state deserializes");
    SwarmController::load(restored).expect("stored state reloads")
}

#[test]
fn a_restart_turns_an_unacknowledged_dispatch_uncertain() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write precedes the spawn");
    // The process dies here: the record exists, the child may or may not.

    let mut swarm = reload(swarm);
    let report = swarm.recover(at(2));
    assert_eq!(report.uncertain, vec![intent.dispatch_id.clone()]);
    assert!(!report.is_clean());
    assert_eq!(state_of(&swarm, "t-only"), TaskState::DispatchUncertain);
    assert_eq!(
        swarm
            .state()
            .dispatch(&intent.dispatch_id)
            .expect("record")
            .state,
        DispatchState::Uncertain
    );
}

#[test]
fn a_restart_leaves_an_acknowledged_dispatch_alone() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_acknowledged(&intent.dispatch_id, external("ext-1"), at(2))
        .expect("worker acknowledged");

    let mut swarm = reload(swarm);
    let report = swarm.recover(at(3));
    assert!(
        report.is_clean(),
        "an acknowledged dispatch carries a handle and can be probed"
    );
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Running);
}

#[test]
fn an_uncertain_dispatch_is_never_resent_or_settled_by_assumption() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    let mut swarm = reload(swarm);
    swarm.recover(at(2));

    assert!(
        swarm.plan_dispatches(at(3)).is_empty(),
        "an uncertain task must never be proposed again"
    );

    let error = swarm
        .record_task_outcome(&intent.dispatch_id, TaskOutcome::succeeded(), at(3))
        .expect_err("an uncertain dispatch cannot be settled directly");
    assert_eq!(error.code, SwarmErrorCode::UncertainDispatch);

    let error = swarm
        .cancel_task(&task_id("t-only"), at(3))
        .expect_err("an uncertain child cannot be cancelled blind");
    assert_eq!(error.code, SwarmErrorCode::UncertainDispatch);
}

#[test]
fn an_unknown_probe_resolves_nothing() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    let mut swarm = reload(swarm);
    swarm.recover(at(2));

    let resolved = swarm
        .reconcile_uncertain(&intent.dispatch_id, DispatchProbe::Unknown, at(3))
        .expect("probing is legal");
    assert!(!resolved, "absence of evidence resolves nothing");
    assert_eq!(state_of(&swarm, "t-only"), TaskState::DispatchUncertain);
    assert!(swarm.plan_dispatches(at(4)).is_empty());
}

#[test]
fn proof_that_a_child_never_started_permits_a_fresh_attempt() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let first = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&first, None, at(1))
        .expect("write");
    let mut swarm = reload(swarm);
    swarm.recover(at(2));

    let resolved = swarm
        .reconcile_uncertain(&first.dispatch_id, DispatchProbe::NotStarted, at(3))
        .expect("probing is legal");
    assert!(resolved);
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Ready);

    let second = swarm.plan_dispatches(at(4)).remove(0);
    assert_eq!(second.attempt, 2);
    assert_ne!(
        second.dispatch_id, first.dispatch_id,
        "a retry must never reuse the identity of an attempt that may have run"
    );
}

#[test]
fn proof_that_a_child_is_running_resumes_the_task() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    let mut swarm = reload(swarm);
    swarm.recover(at(2));

    let resolved = swarm
        .reconcile_uncertain(
            &intent.dispatch_id,
            DispatchProbe::Running {
                external_ref: external("ext-found"),
            },
            at(3),
        )
        .expect("probing is legal");
    assert!(resolved);
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Running);
    assert_eq!(
        swarm
            .state()
            .dispatch(&intent.dispatch_id)
            .expect("record")
            .external_ref
            .as_ref()
            .map(|id| id.as_str().to_string()),
        Some("ext-found".to_string())
    );
}

#[test]
fn proof_that_a_child_finished_settles_the_task() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    let mut swarm = reload(swarm);
    swarm.recover(at(2));

    let resolved = swarm
        .reconcile_uncertain(
            &intent.dispatch_id,
            DispatchProbe::Settled {
                outcome: TaskOutcome::succeeded(),
            },
            at(3),
        )
        .expect("probing is legal");
    assert!(resolved);
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Succeeded);
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Succeeded);
}

#[test]
fn uncertainty_blocks_dependents_and_holds_its_capacity() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    let intent = intent_for(&swarm.plan_dispatches(at(1)), "t-root");
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_uncertain(&intent.dispatch_id, "the spawn reply was lost", at(2))
        .expect("uncertainty is recordable");

    assert_eq!(state_of(&swarm, "t-root"), TaskState::DispatchUncertain);
    for downstream in ["t-a", "t-b"] {
        assert_eq!(
            state_of(&swarm, downstream),
            TaskState::Blocked,
            "{downstream} must not build on an unproven result"
        );
    }
    // The swarm cannot declare an outcome it cannot prove.
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Active);
}

// ── cancellation ─────────────────────────────────────────────────────────

#[test]
fn cancelling_a_task_that_never_started_is_immediate() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    swarm
        .cancel_task(&task_id("t-root"), at(1))
        .expect("cancel is legal");

    assert_eq!(state_of(&swarm, "t-root"), TaskState::Cancelled);
    assert_eq!(state_of(&swarm, "t-a"), TaskState::Blocked);
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Failed);
}

#[test]
fn cancelling_a_live_task_waits_for_confirmation() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_acknowledged(&intent.dispatch_id, external("ext-1"), at(2))
        .expect("ack");

    swarm
        .cancel_task(&task_id("t-only"), at(3))
        .expect("cancel is legal");
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Cancelling);
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Active);

    swarm
        .record_task_outcome(&intent.dispatch_id, TaskOutcome::cancelled(), at(4))
        .expect("the child confirmed it stopped");
    assert_eq!(state_of(&swarm, "t-only"), TaskState::Cancelled);
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Failed);
}

#[test]
fn cancelling_the_swarm_stops_dispatch_and_settles_idle_tasks() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    swarm
        .cancel_swarm("the operator withdrew the objective", at(1))
        .expect("cancel is legal");

    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Cancelled);
    for id in [
        "t-root",
        "t-a",
        "t-b",
        "t-review-a",
        "t-review-b",
        "t-synth",
    ] {
        assert_eq!(state_of(&swarm, id), TaskState::Cancelled, "{id}");
    }
    assert!(swarm.plan_dispatches(at(2)).is_empty());
}

#[test]
fn cancelling_the_swarm_waits_on_a_live_child() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    let intent = intent_for(&swarm.plan_dispatches(at(1)), "t-root");
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_acknowledged(&intent.dispatch_id, external("ext-1"), at(2))
        .expect("ack");

    swarm.cancel_swarm("stop everything", at(3)).expect("legal");
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Cancelling);
    assert_eq!(state_of(&swarm, "t-root"), TaskState::Cancelling);
    assert_eq!(state_of(&swarm, "t-a"), TaskState::Cancelled);

    swarm
        .record_task_outcome(&intent.dispatch_id, TaskOutcome::cancelled(), at(4))
        .expect("the child confirmed it stopped");
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Cancelled);
}

#[test]
fn cancelling_the_swarm_stays_open_while_a_dispatch_is_uncertain() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    let intent = intent_for(&swarm.plan_dispatches(at(1)), "t-root");
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    swarm
        .record_dispatch_uncertain(&intent.dispatch_id, "the spawn reply was lost", at(2))
        .expect("uncertainty is recordable");

    swarm.cancel_swarm("stop everything", at(3)).expect("legal");
    assert_eq!(
        swarm.state().lifecycle,
        SwarmLifecycle::Cancelling,
        "a swarm cannot claim to be fully cancelled while a child may still run"
    );

    swarm
        .reconcile_uncertain(&intent.dispatch_id, DispatchProbe::NotStarted, at(4))
        .expect("evidence arrives");
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Cancelled);
}

// ── failure propagation ──────────────────────────────────────────────────

#[test]
fn a_failure_blocks_its_dependents_and_spares_its_siblings() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));
    run_task(
        &mut swarm,
        "t-a",
        TaskOutcome::failed("the build broke"),
        at(2),
    );

    assert_eq!(state_of(&swarm, "t-a"), TaskState::Failed);
    assert_eq!(state_of(&swarm, "t-review-a"), TaskState::Blocked);
    assert_eq!(
        state_of(&swarm, "t-synth"),
        TaskState::Blocked,
        "the blockage reaches the whole downstream cone"
    );
    assert_eq!(
        state_of(&swarm, "t-b"),
        TaskState::Ready,
        "an independent branch keeps running"
    );
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Active);
}

#[test]
fn the_fail_fast_policy_cancels_the_whole_swarm() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.failure = FailurePolicy::CancelSwarm;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));
    run_task(
        &mut swarm,
        "t-a",
        TaskOutcome::failed("the build broke"),
        at(2),
    );

    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Cancelled);
    assert_eq!(state_of(&swarm, "t-b"), TaskState::Cancelled);
    assert!(swarm.plan_dispatches(at(3)).is_empty());
}

// ── quorum and synthesis gates ───────────────────────────────────────────

fn run_to_reviews(swarm: &mut SwarmController) {
    run_task(swarm, "t-root", TaskOutcome::succeeded(), at(1));
    run_task(swarm, "t-a", TaskOutcome::succeeded(), at(2));
    run_task(swarm, "t-b", TaskOutcome::succeeded(), at(3));
}

#[test]
fn a_unanimous_quorum_opens_the_synthesis_gate() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_to_reviews(&mut swarm);

    run_task(
        &mut swarm,
        "t-review-a",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(4),
    );
    assert_eq!(
        state_of(&swarm, "t-synth"),
        TaskState::Pending,
        "one approval is not yet the quorum"
    );

    run_task(
        &mut swarm,
        "t-review-b",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(5),
    );
    assert_eq!(state_of(&swarm, "t-synth"), TaskState::Ready);
}

#[test]
fn a_rejection_holds_a_unanimous_gate_closed() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_to_reviews(&mut swarm);
    run_task(
        &mut swarm,
        "t-review-a",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(4),
    );
    run_task(
        &mut swarm,
        "t-review-b",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Reject),
        at(5),
    );

    assert_eq!(
        state_of(&swarm, "t-review-b"),
        TaskState::Succeeded,
        "a reviewer that rejects has still done its job"
    );
    assert_eq!(
        state_of(&swarm, "t-synth"),
        TaskState::Blocked,
        "the gate stays shut and no replacement work is invented"
    );
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Failed);
}

#[test]
fn a_majority_quorum_tolerates_one_rejection() {
    let mut spec = diamond_spec(QuorumRule::AtLeast { approvals: 1 });
    spec.tasks[5].review_gate = Some(grokptah_swarm_control_plane::ReviewGate {
        reviewers: vec![task_id("t-review-a"), task_id("t-review-b")],
        quorum: QuorumRule::AtLeast { approvals: 1 },
    });
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_to_reviews(&mut swarm);
    run_task(
        &mut swarm,
        "t-review-a",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(4),
    );
    run_task(
        &mut swarm,
        "t-review-b",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Reject),
        at(5),
    );

    assert_eq!(state_of(&swarm, "t-synth"), TaskState::Ready);
}

#[test]
fn a_review_task_must_report_a_verdict() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_to_reviews(&mut swarm);

    let intent = intent_for(&swarm.plan_dispatches(at(4)), "t-review-a");
    let record = swarm
        .record_dispatch_requested(&intent, None, at(4))
        .expect("write");
    let error = swarm
        .record_task_outcome(&record.dispatch_id, TaskOutcome::succeeded(), at(5))
        .expect_err("a verdictless review must be refused");
    assert_eq!(error.code, SwarmErrorCode::InvalidSpec);
}

#[test]
fn only_a_review_task_may_report_a_verdict() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    let record = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");
    let error = swarm
        .record_task_outcome(
            &record.dispatch_id,
            TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
            at(2),
        )
        .expect_err("a work task must not vote");
    assert_eq!(error.code, SwarmErrorCode::InvalidSpec);
}

#[test]
fn the_whole_graph_runs_to_success() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_to_reviews(&mut swarm);
    run_task(
        &mut swarm,
        "t-review-a",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(4),
    );
    run_task(
        &mut swarm,
        "t-review-b",
        TaskOutcome::succeeded().with_verdict(ReviewVerdict::Approve),
        at(5),
    );
    run_task(
        &mut swarm,
        "t-synth",
        TaskOutcome::succeeded().with_evidence(vec![EvidenceEntry::new(
            "diff",
            "6 files changed across both branches",
        )]),
        at(6),
    );

    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Succeeded);
    assert_eq!(swarm.state().total_dispatches, 6);
}

// ── budget ───────────────────────────────────────────────────────────────

#[test]
fn exhausting_the_dispatch_budget_stops_the_swarm() {
    let mut spec = diamond_spec(QuorumRule::Unanimous);
    spec.budget.max_total_dispatches = 1;
    let mut swarm = SwarmController::new(spec, at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));

    assert!(swarm.plan_dispatches(at(2)).is_empty());
    assert_eq!(swarm.state().lifecycle, SwarmLifecycle::Failed);
    assert!(
        swarm
            .state()
            .stop_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no further progress"))
    );
}

#[test]
fn passing_the_wall_clock_budget_stops_dispatch() {
    let mut spec = single_task_spec();
    spec.budget.max_wall_clock_secs = 30;
    let swarm = SwarmController::new(spec, at(0)).expect("valid");
    assert_eq!(swarm.plan_dispatches(at(29)).len(), 1);
    assert!(swarm.plan_dispatches(at(31)).is_empty());
}

// ── Computer Use leases ──────────────────────────────────────────────────

fn computer_use_swarm() -> SwarmController {
    let mut spec = single_task_spec();
    spec.workers = vec![computer_use_worker()];
    spec.tasks[0].worker_id = worker_id("cu-cursor");
    spec.tasks[0].requires_computer_use = true;
    SwarmController::new(spec, at(0)).expect("valid")
}

#[test]
fn a_computer_use_dispatch_without_a_lease_is_refused() {
    let mut swarm = computer_use_swarm();
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    assert!(intent.requires_computer_use);

    let error = swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect_err("Computer Use is never implicit");
    assert_eq!(error.code, SwarmErrorCode::CapabilityNotGranted);
    assert!(swarm.state().dispatches.is_empty(), "nothing was written");
}

#[test]
fn an_expired_or_revoked_lease_is_refused() {
    let mut swarm = computer_use_swarm();
    let intent = swarm.plan_dispatches(at(1)).remove(0);

    let expired = lease("lease-1", 0, 5);
    let error = swarm
        .record_dispatch_requested(&intent, Some(expired), at(10))
        .expect_err("an expired lease grants nothing");
    assert_eq!(error.code, SwarmErrorCode::CapabilityNotGranted);

    let mut revoked = lease("lease-2", 0, 100);
    revoked.revoked_at = Some(at(3));
    let error = swarm
        .record_dispatch_requested(&intent, Some(revoked), at(10))
        .expect_err("a revoked lease grants nothing");
    assert_eq!(error.code, SwarmErrorCode::CapabilityNotGranted);
}

#[test]
fn a_usable_lease_is_recorded_with_the_dispatch() {
    let mut swarm = computer_use_swarm();
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    let record = swarm
        .record_dispatch_requested(&intent, Some(lease("lease-ok", 0, 600)), at(1))
        .expect("a live operator lease admits the dispatch");

    assert_eq!(
        record
            .lease
            .as_ref()
            .map(|l| l.lease_id.as_str().to_string()),
        Some("lease-ok".to_string())
    );
    assert_eq!(record.isolation, intent.isolation);
}

#[test]
fn a_lease_cannot_be_attached_to_a_task_that_does_not_need_one() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    let error = swarm
        .record_dispatch_requested(&intent, Some(lease("lease-stray", 0, 600)), at(1))
        .expect_err("a stray lease must not ride along");
    assert_eq!(error.code, SwarmErrorCode::CapabilityNotGranted);
}

// ── durable reload ───────────────────────────────────────────────────────

#[test]
fn durable_state_round_trips_without_drift() {
    let mut swarm =
        SwarmController::new(diamond_spec(QuorumRule::Unanimous), at(0)).expect("valid");
    run_task(&mut swarm, "t-root", TaskOutcome::succeeded(), at(1));
    let before = swarm.state().clone();

    let restored = reload(swarm);
    assert_eq!(restored.state(), &before);
}

#[test]
fn a_tampered_dispatch_identity_is_refused_on_reload() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");

    let mut state = swarm.into_state();
    state.dispatches[0].dispatch_id =
        grokptah_swarm_control_plane::DispatchId::parse("hand-edited-identity")
            .expect("shape is valid");
    let error = SwarmController::load(state).expect_err("a rewritten identity must not resume");
    assert_eq!(error.code, SwarmErrorCode::CorruptState);
}

#[test]
fn an_unsupported_schema_version_is_refused_on_reload() {
    let swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let mut state = swarm.into_state();
    state.schema_version = 999;
    let error = SwarmController::load(state).expect_err("an unknown shape must not resume");
    assert_eq!(error.code, SwarmErrorCode::CorruptState);
}

#[test]
fn a_stored_specification_that_no_longer_validates_is_refused() {
    let swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let mut state = swarm.into_state();
    state.spec.catalog = grokptah_swarm_control_plane::ProviderCatalog::default();
    let error =
        SwarmController::load(state).expect_err("capability must be re-proved on every reload");
    assert_eq!(error.code, SwarmErrorCode::CorruptState);
}

#[test]
fn a_rewritten_dispatch_counter_is_refused_on_reload() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");

    let mut state = swarm.into_state();
    // Rewinding the counter would buy dispatches the budget never granted.
    state.total_dispatches = 0;
    let error = SwarmController::load(state).expect_err("the budget counter must be trustworthy");
    assert_eq!(error.code, SwarmErrorCode::CorruptState);
}

#[test]
fn a_dangling_current_dispatch_pointer_is_refused_on_reload() {
    let mut swarm = SwarmController::new(single_task_spec(), at(0)).expect("valid");
    let intent = swarm.plan_dispatches(at(1)).remove(0);
    swarm
        .record_dispatch_requested(&intent, None, at(1))
        .expect("write");

    let mut state = swarm.into_state();
    state.dispatches.clear();
    state.total_dispatches = 0;
    let error = SwarmController::load(state).expect_err("a task must not point at nothing");
    assert_eq!(error.code, SwarmErrorCode::CorruptState);
}
