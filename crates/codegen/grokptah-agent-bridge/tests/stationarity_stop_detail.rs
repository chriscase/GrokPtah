//! Adversarial tests for the durable stationarity stop detail.
//!
//! Synthetic fixtures only: no provider, no VM, no Computer Use. These drive the
//! shipped store and types, not a reimplementation.

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, RunBounds, RunRecord, RunState, RunStopCause, RunStopDetail, RunStopDetailKind,
    WorkItem, WorkPolicy, WorkResult, WorkState,
};
use tempfile::tempdir;
use uuid::Uuid;

fn run_with(run_id: &str, stop: Option<(RunStopCause, RunStopDetail)>) -> RunRecord {
    let (stop_cause, stop_detail) = match stop {
        Some((cause, detail)) => (Some(cause), Some(detail)),
        None => (None, None),
    };
    RunRecord {
        run_id: run_id.into(),
        session_id: Uuid::new_v4(),
        workspace: "/tmp/fixture-workspace".into(),
        request_id: format!("req-{run_id}"),
        client_id: Some("mcp".into()),
        state: RunState::LimitReached,
        stop_cause,
        stop_detail,
        bounds: RunBounds::default(),
        prompt_preview: "SENSITIVE-USER-PROMPT".into(),
        start_seq: Some(1),
        end_seq: Some(2),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: Some("stationarity".into()),
        error_code: Some("stationarity".into()),
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
        final_response: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

/// A durable stop detail survives a process restart byte-identically.
#[test]
fn a_stop_detail_survives_a_store_restart() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("get_task_output");

    {
        let store = OrchStore::open(&root).unwrap();
        store
            .save_run(&run_with(
                "run-inert",
                Some((RunStopCause::Stationarity, detail.clone())),
            ))
            .unwrap();
    }

    let store = OrchStore::open(&root).unwrap();
    let recovered = store.load_run("run-inert").unwrap().expect("run present");
    assert_eq!(recovered.stop_cause, Some(RunStopCause::Stationarity));
    assert_eq!(recovered.stop_detail, Some(detail));
    // Restart recovery must not reopen a run that already stopped.
    assert!(recovered.state.is_terminal());
}

/// Records written before this field existed still load, and stay detail-free.
#[test]
fn a_legacy_record_without_a_stop_detail_still_loads() {
    let legacy = serde_json::json!({
        "runId": "run-legacy",
        "sessionId": Uuid::new_v4(),
        "workspace": "/tmp/project",
        "requestId": "request-1",
        "state": "limit_reached",
        "bounds": {"maxPromptBytes": 1000, "maxRounds": 2, "maxDurationMs": 1000},
        "promptPreview": "inspect",
        "createdAt": Utc::now(),
        "updatedAt": Utc::now(),
        "stopCause": "stationarity",
    });
    let run: RunRecord = serde_json::from_value(legacy).expect("legacy record must still load");
    assert_eq!(run.stop_cause, Some(RunStopCause::Stationarity));
    assert!(
        run.stop_detail.is_none(),
        "absent detail must stay absent, never be invented"
    );
    // And it must not start serializing a null into the record either.
    let encoded = serde_json::to_string(&run).unwrap();
    assert!(!encoded.contains("stopDetail"));
}

/// The three stationarity flavours stay distinguishable through serialization.
#[test]
fn each_stop_detail_kind_round_trips_distinctly() {
    for (kind, wire) in [
        (RunStopDetailKind::IdenticalCalls, "identical_calls"),
        (RunStopDetailKind::TrueNoop, "true_noop"),
        (RunStopDetailKind::InertRepeat, "inert_repeat"),
    ] {
        let detail = RunStopDetail::new(kind, 7).with_tool("read_file");
        let encoded = serde_json::to_string(&detail).unwrap();
        assert!(encoded.contains(wire), "{wire} missing from {encoded}");
        let decoded: RunStopDetail = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, detail);
        assert_eq!(decoded.kind.as_str(), wire);
        assert_eq!(decoded.repeats, 7);
    }
}

/// A budget stop is a different cause and carries no stationarity detail.
#[test]
fn a_budget_exhaustion_stop_is_not_labelled_stationary() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();
    for (cause, code) in [
        (RunStopCause::RoundLimit, "max_rounds_reached"),
        (RunStopCause::DurationLimit, "max_duration_reached"),
        (RunStopCause::TokenCeiling, "max_total_tokens_reached"),
    ] {
        let mut run = run_with("run-budget", None);
        run.stop_cause = Some(cause);
        run.error_code = Some(code.into());
        store.save_run(&run).unwrap();
        let loaded = store.load_run("run-budget").unwrap().unwrap();
        assert_eq!(loaded.stop_cause, Some(cause));
        assert!(
            loaded.stop_detail.is_none(),
            "{code} must not borrow a stationarity label"
        );
    }
}

/// Bounds are enforced so nothing can smuggle content onto the read surface.
#[test]
fn a_stop_detail_rejects_malformed_evidence() {
    let ok = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("run_terminal_cmd");
    assert!(ok.validate().is_ok());

    // A stop that reports zero repeats is claiming something that did not
    // happen, so it is refused rather than displayed.
    let zero = RunStopDetail::new(RunStopDetailKind::InertRepeat, 0).with_tool("run_terminal_cmd");
    assert!(zero.validate().is_err(), "accepted repeats == 0");

    // An over-long tool label is truncated at construction, so it can never
    // become a channel for arguments or paths.
    let long = "x".repeat(4096);
    let detail = RunStopDetail::new(RunStopDetailKind::IdenticalCalls, 1).with_tool(long);
    assert!(detail.validate().is_ok());
    assert!(detail.tool.as_deref().unwrap().len() <= 128);
}

/// The cross-field invariant is restored on write, so a later lifecycle
/// transition can never strand a run, and nothing inconsistent reaches disk.
#[test]
fn a_detail_is_dropped_when_the_cause_moves_off_stationarity() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();

    // A detail attached to a budget stop means nothing: it qualifies
    // stationarity. Writing is allowed, but the detail does not survive.
    let mut run = run_with("run-mismatch", None);
    run.stop_cause = Some(RunStopCause::RoundLimit);
    run.stop_detail = Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("poll"));
    store.save_run(&run).expect("write must not be refused");
    let loaded = store.load_run("run-mismatch").unwrap().unwrap();
    assert_eq!(loaded.stop_cause, Some(RunStopCause::RoundLimit));
    assert!(loaded.stop_detail.is_none(), "detail outlived its cause");

    // Same for a detail with no cause at all.
    let mut orphan = run_with("run-orphan", None);
    orphan.stop_cause = None;
    orphan.stop_detail =
        Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("poll"));
    store.save_run(&orphan).unwrap();
    assert!(store
        .load_run("run-orphan")
        .unwrap()
        .unwrap()
        .stop_detail
        .is_none());
}

/// Normalization restores the invariant; it does not excuse a malformed detail
/// that survives it.
#[test]
fn a_malformed_detail_under_the_right_cause_is_still_refused() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();

    // Unattributed: nothing to show an operator, so not reportable.
    let mut unattributed = run_with("run-unattributed", None);
    unattributed.stop_cause = Some(RunStopCause::Stationarity);
    unattributed.stop_detail = Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 4));
    assert!(store.save_run(&unattributed).is_err());

    // Zero repeats claims something that did not happen.
    let mut zero = run_with("run-zero", None);
    zero.stop_cause = Some(RunStopCause::Stationarity);
    zero.stop_detail =
        Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 0).with_tool("poll"));
    assert!(store.save_run(&zero).is_err());
}

/// A run that stopped for stationarity and is then cancelled must still be
/// writable. An invariant that can deadlock a legitimate transition is the
/// wrong invariant; this is the regression test for that.
#[test]
fn cancelling_a_stationarity_stopped_run_still_writes() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();
    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("task_output");
    store
        .save_run(&run_with(
            "run-cancel",
            Some((RunStopCause::Stationarity, detail)),
        ))
        .unwrap();

    let updated = store
        .update_run("run-cancel", |run| {
            run.state = RunState::Cancelled;
            run.stop_cause = Some(RunStopCause::Cancelled);
            Ok(())
        })
        .expect("cancel must not be blocked by a stale stop detail")
        .expect("run present");
    assert_eq!(updated.stop_cause, Some(RunStopCause::Cancelled));
    assert!(updated.stop_detail.is_none());
}

/// Durable data is not automatically trusted: a record tampered with after the
/// write path fails closed on read instead of being rendered as fact.
#[test]
fn malformed_durable_data_fails_closed_on_read() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let store = OrchStore::open(&root).unwrap();

    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("poll");
    store
        .save_run(&run_with(
            "run-tamper",
            Some((RunStopCause::Stationarity, detail)),
        ))
        .unwrap();
    assert!(store.load_run("run-tamper").unwrap().is_some());

    // Rewrite the record on disk with a cause the detail may not accompany.
    let path = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .expect("run file");
    let text = std::fs::read_to_string(&path).unwrap();
    let tampered = text.replace("\"stationarity\"", "\"round_limit\"");
    assert_ne!(tampered, text, "fixture did not actually tamper anything");
    std::fs::write(&path, tampered).unwrap();

    assert!(
        store.load_run("run-tamper").is_err(),
        "a tampered stop detail must be refused, not returned"
    );
    assert!(
        store.list_runs().unwrap().is_empty(),
        "a tampered record must not be listed either"
    );
}

/// The detail carries no prompt, path, argument, or payload material.
#[test]
fn a_stop_detail_carries_no_content() {
    let detail =
        RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("run_terminal_cmd");
    let encoded = serde_json::to_string(&detail).unwrap();
    for leak in [
        "SENSITIVE-USER-PROMPT",
        "/home/",
        "/tmp/fixture-workspace",
        "hunter2",
        "AKIA",
    ] {
        assert!(!encoded.contains(leak), "stop detail leaked {leak}");
    }
    // Only the bounded tool name, the kind, the count, and the digest.
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    // Exactly these four and nothing else: any new key would be a new channel.
    assert_eq!(
        keys,
        vec!["kind", "repeats", "tool"],
        "stop detail gained an unexpected field"
    );
}

/// Recording a stop detail never resurrects or mutates a terminal run's state.
#[test]
fn recording_a_detail_does_not_reopen_a_stopped_run() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();
    let run = run_with("run-terminal", None);
    let terminal_state = run.state;
    store.save_run(&run).unwrap();

    store
        .update_run("run-terminal", |r| {
            r.stop_cause = Some(RunStopCause::Stationarity);
            r.stop_detail =
                Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("poll"));
            Ok(())
        })
        .unwrap();

    let after = store.load_run("run-terminal").unwrap().unwrap();
    assert_eq!(after.state, terminal_state);
    assert!(after.state.is_terminal(), "a labelled stop stays stopped");
}

/// A manager reopening failed work must still be revision-fenced, and must not
/// disturb a run's durable stop label.
#[test]
fn a_stale_revision_still_fails_closed_and_leaves_the_stop_detail_intact() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();

    // A run that stopped for an inert repeat.
    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("get_task_output");
    store
        .save_run(&run_with(
            "run-linked",
            Some((RunStopCause::Stationarity, detail.clone())),
        ))
        .unwrap();

    let item = WorkItem::new(
        "test",
        "objective",
        Uuid::new_v4(),
        "/tmp/project",
        "test-operator",
        WorkPolicy::default(),
    )
    .unwrap();
    store.save_work_item(&item).unwrap();

    // The default policy retries a failure, so drive attempts until the work
    // item is genuinely terminal rather than assuming one failure ends it.
    let mut failed = None;
    for _ in 0..16 {
        let claim = store.claim_work(&item.work_id, "agent-1", None).unwrap();
        let (state, _) = store
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
                },
            )
            .unwrap();
        if state.state == WorkState::Failed {
            failed = Some(state);
            break;
        }
    }
    let failed = failed.expect("work item must reach a terminal failure");

    // A manager holding a superseded revision is refused.
    let stale = store.retry_work(&item.work_id, "operator retry", Some(failed.revision + 1));
    assert!(stale.is_err(), "a stale revision must not reopen work");

    // With the *correct* revision the call gets past the fence and is judged on
    // its own merits (here: the retry budget is spent). Either way the fence
    // itself is proven live, and the two failures are distinguishable.
    let correct = store.retry_work(&item.work_id, "operator retry", Some(failed.revision));
    match correct {
        Ok(retried) => assert_eq!(retried.state, WorkState::Queued),
        Err(error) => assert!(
            error.message.contains("retry budget"),
            "correct revision must fail only on budget, got: {error}"
        ),
    }

    // Neither outcome touches the run's durable stop evidence.
    let run = store.load_run("run-linked").unwrap().unwrap();
    assert_eq!(run.stop_detail, Some(detail));
    assert_eq!(run.stop_cause, Some(RunStopCause::Stationarity));
    assert!(
        run.state.is_terminal(),
        "manager retry must not reopen the run"
    );
}
