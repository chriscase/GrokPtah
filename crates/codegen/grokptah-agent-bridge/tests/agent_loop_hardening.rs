//! Adversarial durability tests for the always-on agent loop.
//!
//! These drive the shipped store and policy code against synthetic fixtures
//! only: no provider is contacted, no credential is read, and no timing or
//! cost claim is made. Every clock value is a fixture constant so the same
//! run produces the same verdict.
//!
//! The scenarios here are the ones a small model actually fails at: repeating
//! itself, waiting on nothing, burning budget, and being restarted mid-send.

use chrono::{DateTime, Duration, Utc};
use grokptah_agent_bridge::orchestration::{
    admit_step, digest_of, project_loop, AttentionGrant, AttentionReason, DispatchState,
    LoopDisposition, LoopState, LoopStep, ModelTier, OrchError, OrchStore, PolicyEnvelope,
    RetentionPolicy, RunBounds, RunRecord, RunState, StepClass, StepVerdict, WaitWitness,
};
use tempfile::tempdir;
use uuid::Uuid;

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(1_700_000_000_000 + ms).expect("valid fixture timestamp")
}

fn inert_step(elapsed_ms: u64) -> LoopStep {
    LoopStep {
        observation_digest: digest_of(&serde_json::json!({"screen": "unchanged"})),
        action_digest: digest_of(&serde_json::json!({"tool": "read", "arg": "same"})),
        changed_files: 0,
        tests_observed: 0,
        tool_calls: 1,
        tokens: 100,
        elapsed_ms,
        wait: None,
    }
}

fn novel_step(n: u32) -> LoopStep {
    LoopStep {
        observation_digest: digest_of(&serde_json::json!({"screen": n})),
        action_digest: digest_of(&serde_json::json!({"tool": "read", "n": n})),
        changed_files: 0,
        tests_observed: 0,
        tool_calls: n,
        tokens: u64::from(n) * 10,
        elapsed_ms: u64::from(n),
        wait: None,
    }
}

/// `admit_step` is a free function, so the revision has to be read out before
/// the mutable borrow is taken.
fn try_step(
    state: &mut LoopState,
    step: &LoopStep,
    now: DateTime<Utc>,
) -> Result<StepVerdict, OrchError> {
    let revision = state.revision;
    admit_step(state, revision, step, now)
}

/// Persist a loop, then reopen the store as a fresh process would.
fn reopen(root: &std::path::Path) -> OrchStore {
    OrchStore::open(root).expect("store reopens")
}

fn seed_run(store: &OrchStore, run_id: &str, state: RunState) -> RunRecord {
    let run = RunRecord {
        run_id: run_id.into(),
        session_id: Uuid::new_v4(),
        workspace: "/tmp/fixture-workspace".into(),
        request_id: format!("req-{run_id}"),
        client_id: Some("mcp".into()),
        state,
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "fixture".into(),
        start_seq: Some(1),
        end_seq: Some(2),
        created_at: at(0),
        updated_at: at(0),
        terminal_result: Some("completed".into()),
        final_response: None,
        error_code: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    };
    store.save_run(&run).expect("run saved");
    run
}

#[test]
fn an_in_flight_dispatch_becomes_uncertain_after_restart_and_is_never_resent() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");

    // A worker begins a send, then the process dies.
    {
        let store = OrchStore::open(&root).expect("store opens");
        let mut state = LoopState::new("run-inflight", ModelTier::Small, at(0));
        let revision = state.revision;
        admit_step(&mut state, revision, &novel_step(1), at(1)).expect("first step");
        state
            .begin_dispatch(state.revision, at(2))
            .expect("dispatch begins");
        store.commit_loop_state(&state).expect("state committed");
        assert_eq!(state.dispatch, DispatchState::Sending);
    }

    // The store reopens. An unknown outcome must never be replayed.
    let store = reopen(&root);
    let recovered = store
        .load_loop_state("run-inflight")
        .expect("loads")
        .expect("state present");
    assert_eq!(recovered.dispatch, DispatchState::Uncertain);
    assert_eq!(
        recovered.disposition,
        LoopDisposition::NeedsAttention {
            reason: AttentionReason::UncertainDispatch,
            human_required: true,
        }
    );

    // No resend, and no further step.
    let mut resumed = recovered.clone();
    assert!(resumed.begin_dispatch(resumed.revision, at(10)).is_err());
    assert!(try_step(&mut resumed, &novel_step(2), at(11)).is_err());

    // No stronger model may take an unknown outcome either.
    let ticket = recovered.escalation.clone().expect("escalation issued");
    assert_eq!(ticket.to_tier, None);
    assert!(ticket.human_required);
    assert!(!ticket.auto_resume_allowed);
}

#[test]
fn a_settled_dispatch_is_left_alone_by_restart_recovery() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    {
        let store = OrchStore::open(&root).expect("opens");
        let mut state = LoopState::new("run-settled", ModelTier::Small, at(0));
        admit_step(&mut state, 0, &novel_step(1), at(1)).expect("step");
        state.begin_dispatch(state.revision, at(2)).expect("begins");
        state
            .settle_dispatch(state.revision, DispatchState::Delivered, at(3))
            .expect("settles");
        store.commit_loop_state(&state).expect("committed");
    }
    let store = reopen(&root);
    let recovered = store
        .load_loop_state("run-settled")
        .expect("loads")
        .expect("present");
    // A known outcome is not ambiguous, so recovery leaves it exactly as-is.
    assert_eq!(recovered.dispatch, DispatchState::Delivered);
    assert!(recovered.disposition.may_continue());
}

#[test]
fn a_stale_revision_write_cannot_clobber_the_durable_ledger() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");

    let mut state = LoopState::new("run-cas", ModelTier::Small, at(0));
    admit_step(&mut state, 0, &novel_step(1), at(1)).expect("step");
    store.commit_loop_state(&state).expect("rev 1 committed");

    // A second worker holding the pre-step copy tries to write back.
    let stale = LoopState::new("run-cas", ModelTier::Small, at(0));
    let error = store.commit_loop_state(&stale).unwrap_err();
    assert_eq!(error.code.as_str(), "stale_version");

    // The durable record still reflects the newer revision.
    let durable = store
        .load_loop_state("run-cas")
        .expect("loads")
        .expect("present");
    assert_eq!(durable.revision, state.revision);
}

#[test]
fn a_stale_worker_cannot_downgrade_an_uncertain_dispatch_in_place() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");

    let mut state = LoopState::new("run-downgrade", ModelTier::Small, at(0));
    admit_step(&mut state, 0, &novel_step(1), at(1)).expect("step");
    state.begin_dispatch(state.revision, at(2)).expect("begins");
    store.commit_loop_state(&state).expect("sending committed");

    let mut uncertain = state.clone();
    uncertain.recover_after_restart(at(3));
    store
        .commit_loop_state(&uncertain)
        .expect("uncertain committed");

    // The pre-crash worker comes back holding `Sending` at the same revision.
    // Accepting that write would erase the unknown outcome and buy a retry.
    let error = store.commit_loop_state(&state).unwrap_err();
    assert_eq!(error.code.as_str(), "stale_version");
    assert_eq!(
        store
            .load_loop_state("run-downgrade")
            .expect("loads")
            .expect("present")
            .dispatch,
        DispatchState::Uncertain
    );
}

#[test]
fn update_loop_state_refuses_a_closure_that_rewinds_the_revision() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");
    let mut state = LoopState::new("run-rewind", ModelTier::Small, at(0));
    admit_step(&mut state, 0, &novel_step(1), at(1)).expect("step");
    try_step(&mut state, &novel_step(2), at(2)).expect("step");
    store.commit_loop_state(&state).expect("committed");

    let error = store
        .update_loop_state("run-rewind", |current| {
            current.revision = 0;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.code.as_str(), "stale_version");
    assert_eq!(
        store
            .load_loop_state("run-rewind")
            .expect("loads")
            .expect("present")
            .revision,
        state.revision
    );
}

#[test]
fn a_no_op_loop_stops_itself_and_survives_restart_as_needing_attention() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let stopped_revision;
    {
        let store = OrchStore::open(&root).expect("opens");
        let mut state = LoopState::new("run-noop", ModelTier::Small, at(0));
        admit_step(&mut state, 0, &inert_step(1), at(1)).expect("first sighting");
        for tick in 2..=4 {
            let _ = try_step(&mut state, &inert_step(tick as u64), at(tick));
        }
        assert_eq!(
            state.disposition,
            LoopDisposition::NeedsAttention {
                reason: AttentionReason::StationaryLoop,
                human_required: false,
            }
        );
        assert_eq!(state.changed_files, 0, "a no-op loop changed nothing");
        stopped_revision = state.revision;
        store.commit_loop_state(&state).expect("committed");
    }

    let store = reopen(&root);
    let recovered = store
        .load_loop_state("run-noop")
        .expect("loads")
        .expect("present");
    // The stop is durable; a restart is not a fresh start.
    assert_eq!(recovered.revision, stopped_revision);
    assert!(!recovered.disposition.may_continue());
    assert!(recovered.escalation.is_some());
}

#[test]
fn a_productive_wait_is_not_mistaken_for_a_stuck_loop() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");
    let mut state = LoopState::new("run-wait", ModelTier::Small, at(0));

    // A shell session that keeps reporting fresh output is genuinely working.
    for attempt in 1..=5u32 {
        let step = LoopStep {
            observation_digest: digest_of(&serde_json::json!({"screen": "waiting"})),
            action_digest: digest_of(&serde_json::json!({"tool": "poll"})),
            changed_files: 0,
            tests_observed: 0,
            tool_calls: 1,
            tokens: u64::from(attempt) * 5,
            elapsed_ms: u64::from(attempt) * 1_000,
            wait: Some(WaitWitness {
                kind: "shell".into(),
                witness_digest: digest_of(&serde_json::json!({"bytes": attempt * 512})),
                attempt,
                deadline_ms: Some(30_000),
            }),
        };
        let verdict = try_step(&mut state, &step, at(attempt.into())).expect("admitted");
        assert_eq!(verdict.class, StepClass::ProductiveWait);
        assert!(matches!(
            verdict.disposition,
            LoopDisposition::Waiting { .. }
        ));
    }
    assert_eq!(state.wait_streak, 5);
    assert_eq!(state.stationary_streak, 0);
    store.commit_loop_state(&state).expect("committed");

    // The moment the external witness stops moving, the same shape of step is
    // reclassified. Waiting is only productive while something outside moves.
    let frozen = LoopStep {
        observation_digest: digest_of(&serde_json::json!({"screen": "waiting"})),
        action_digest: digest_of(&serde_json::json!({"tool": "poll"})),
        changed_files: 0,
        tests_observed: 0,
        tool_calls: 1,
        tokens: 100,
        elapsed_ms: 6_000,
        wait: Some(WaitWitness {
            kind: "shell".into(),
            witness_digest: digest_of(&serde_json::json!({"bytes": 5 * 512})),
            attempt: 5,
            deadline_ms: Some(30_000),
        }),
    };
    let verdict = try_step(&mut state, &frozen, at(6)).expect("admitted");
    assert_eq!(verdict.class, StepClass::StalledWait);
    assert_eq!(state.wait_streak, 0);
}

#[test]
fn a_grant_reopens_a_stopped_loop_and_makes_the_old_revision_unusable() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let store = OrchStore::open(&root).expect("opens");

    let mut state = LoopState::new("run-grant", ModelTier::Small, at(0));
    admit_step(&mut state, 0, &inert_step(1), at(1)).expect("first sighting");
    for tick in 2..=4 {
        let _ = try_step(&mut state, &inert_step(tick as u64), at(tick));
    }
    store.commit_loop_state(&state).expect("committed");
    let stopped_revision = state.revision;

    let updated = store
        .update_loop_state("run-grant", |current| {
            let grant = AttentionGrant {
                run_id: current.run_id.clone(),
                revision: current.revision,
                reason: AttentionReason::StationaryLoop,
                issued_by: "manager".into(),
                promote_to_tier: Some(ModelTier::Large),
                acknowledges_uncertain_outcome: false,
                issued_at: at(100),
                expires_at: at(100) + Duration::minutes(5),
            };
            current.apply_grant(&grant, at(101))
        })
        .expect("grant applies")
        .expect("state present");

    assert!(updated.disposition.may_continue());
    assert_eq!(updated.envelope.tier, ModelTier::Large);
    assert_eq!(
        updated.envelope.max_turns,
        PolicyEnvelope::large().max_turns
    );
    assert!(updated.revision > stopped_revision);
    // A grant reopens the loop; it does not refund what the loop already spent.
    assert!(updated.turns >= 4);

    // Anything still holding the stopped revision is now rejected, so a
    // duplicate worker cannot step twice against one grant.
    let mut duplicate = updated.clone();
    assert_eq!(
        admit_step(&mut duplicate, stopped_revision, &novel_step(7), at(102))
            .unwrap_err()
            .code
            .as_str(),
        "stale_version"
    );
}

#[test]
fn the_same_grant_cannot_be_replayed_to_step_the_loop_twice() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");
    let mut state = LoopState::new("run-replay", ModelTier::Small, at(0));
    admit_step(&mut state, 0, &inert_step(1), at(1)).expect("first sighting");
    for tick in 2..=4 {
        let _ = try_step(&mut state, &inert_step(tick as u64), at(tick));
    }
    store.commit_loop_state(&state).expect("committed");

    let grant = AttentionGrant {
        run_id: state.run_id.clone(),
        revision: state.revision,
        reason: AttentionReason::StationaryLoop,
        issued_by: "manager".into(),
        promote_to_tier: None,
        acknowledges_uncertain_outcome: false,
        issued_at: at(100),
        expires_at: at(100) + Duration::minutes(5),
    };
    store
        .update_loop_state("run-replay", |current| current.apply_grant(&grant, at(101)))
        .expect("first application");

    // Replaying the identical grant is refused: its revision is spent.
    let error = store
        .update_loop_state("run-replay", |current| current.apply_grant(&grant, at(102)))
        .unwrap_err();
    assert!(matches!(error.code.as_str(), "stale_version" | "conflict"));
}

#[test]
fn the_public_projection_of_a_stopped_loop_is_redacted_and_truthful() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).expect("opens");

    let secret = serde_json::json!({
        "prompt": "TOP-SECRET-USER-PROMPT",
        "path": "/home/someone/.ssh/id_ed25519",
    });
    let mut state = LoopState::new("run-projection", ModelTier::Small, at(0));
    let mut step = inert_step(1);
    step.observation_digest = digest_of(&secret);
    admit_step(&mut state, 0, &step, at(1)).expect("first sighting");
    for tick in 2..=4 {
        let mut repeat = inert_step(tick as u64);
        repeat.observation_digest = digest_of(&secret);
        let _ = try_step(&mut state, &repeat, at(tick));
    }
    store.commit_loop_state(&state).expect("committed");

    let encoded = serde_json::to_string(&project_loop(&state)).expect("encodes");
    for leak in [
        "TOP-SECRET-USER-PROMPT",
        "/home/someone",
        "id_ed25519",
        "run-projection",
    ] {
        assert!(!encoded.contains(leak), "projection leaked {leak}");
    }
    // It reports the stop honestly rather than as activity.
    assert!(encoded.contains("\"disposition\":\"needs_attention\""));
    assert!(encoded.contains("\"attentionReason\":\"stationary_loop\""));
    assert!(encoded.contains("\"changedFiles\":0"));
}

#[test]
fn the_loop_ledger_expires_with_the_run_it_belongs_to() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let store = OrchStore::open(&root).expect("opens");

    let run = seed_run(&store, "run-retained", RunState::Completed);
    let mut state = LoopState::new(run.run_id.as_str(), ModelTier::Small, at(0));
    admit_step(&mut state, 0, &novel_step(1), at(1)).expect("step");
    store.commit_loop_state(&state).expect("committed");
    assert!(store
        .load_loop_state("run-retained")
        .expect("loads")
        .is_some());

    // Retention that keeps one terminal run but ages everything out must not
    // leave the loop ledger behind as an orphan.
    store
        .prune_retention(RetentionPolicy {
            max_terminal_runs: 1,
            terminal_run_age: Duration::milliseconds(1),
            ..RetentionPolicy::default()
        })
        .expect("prunes");

    assert!(
        store
            .load_loop_state("run-retained")
            .expect("loads")
            .is_none(),
        "loop state outlived its run record"
    );
}

#[test]
fn a_small_model_budget_is_strictly_tighter_than_a_large_one() {
    // Deterministic policy, not a measurement: the same synthetic step
    // sequence must stop a small-tier loop no later than a large-tier one.
    fn steps_until_stop(tier: ModelTier) -> u32 {
        let mut state = LoopState::new("run-envelope", tier, at(0));
        let mut n = 0;
        loop {
            n += 1;
            let mut step = novel_step(n);
            step.changed_files = n;
            match try_step(&mut state, &step, at(n.into())) {
                Ok(verdict) if verdict.disposition.may_continue() => {}
                _ => return n,
            }
            assert!(n < 500, "loop never stopped for {tier:?}");
        }
    }
    let small = steps_until_stop(ModelTier::Small);
    let large = steps_until_stop(ModelTier::Large);
    let unspecified = steps_until_stop(ModelTier::Unspecified);
    assert!(small <= large, "small tier must stop no later than large");
    assert_eq!(
        unspecified, small,
        "an undeclared tier must not buy a larger budget"
    );
}

#[test]
fn loop_states_persist_across_restart_without_double_counting() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let (revision, turns, tokens) = {
        let store = OrchStore::open(&root).expect("opens");
        let mut state = LoopState::new("run-counters", ModelTier::Small, at(0));
        for n in 1..=3u32 {
            let mut step = novel_step(n);
            step.changed_files = n;
            try_step(&mut state, &step, at(n.into())).expect("step");
        }
        store.commit_loop_state(&state).expect("committed");
        (state.revision, state.turns, state.tokens)
    };

    let store = reopen(&root);
    let recovered = store
        .load_loop_state("run-counters")
        .expect("loads")
        .expect("present");
    assert_eq!(recovered.revision, revision);
    assert_eq!(recovered.turns, turns);
    assert_eq!(recovered.tokens, tokens);

    // Replaying the step the pre-restart worker had already recorded is
    // rejected as stale rather than counted a second time.
    let mut resumed = recovered.clone();
    let mut replayed = novel_step(3);
    replayed.changed_files = 3;
    assert_eq!(
        admit_step(&mut resumed, revision - 1, &replayed, at(10))
            .unwrap_err()
            .code
            .as_str(),
        "stale_version"
    );
    assert_eq!(resumed.turns, turns);
}
