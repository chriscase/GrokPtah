//! Contract tests for the durable stationarity stop detail.
//!
//! These live inside the crate on purpose. Issuing a stop detail is a host
//! decision, so its constructors and the terminal-observation transition are
//! crate-private; a test that exercises them has to be crate-private too. An
//! out-of-crate test would prove only what an out-of-crate forger can reach,
//! which is exactly what this contract is meant to prevent.
//!
//! Synthetic fixtures only: no provider, no VM, no Computer Use.

#![cfg(test)]

use chrono::Utc;
use tempfile::tempdir;
use uuid::Uuid;

use super::store::OrchStore;
use super::types::{
    safe_id_filename, RunBounds, RunRecord, RunState, RunStopCause, RunStopDetail,
    RunStopDetailKind, RunStopTool, MIN_REPEATS_IDENTICAL_CALLS, MIN_REPEATS_INERT_REPEAT,
    MIN_REPEATS_TRUE_NOOP, PROGRESS_PROJECTION_SCHEMA_VERSION,
};
use super::workload::{WorkItem, WorkPolicy, WorkResult, WorkState};

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
        let detail = RunStopDetail::new(kind, RunStopDetail::min_repeats_for(kind)).with_tool(
            if kind == RunStopDetailKind::TrueNoop {
                "run_terminal_cmd"
            } else {
                "read_file"
            },
        );
        let encoded = serde_json::to_string(&detail).unwrap();
        assert!(encoded.contains(wire), "{wire} missing from {encoded}");
        let decoded: RunStopDetail = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, detail);
        assert_eq!(decoded.kind().as_str(), wire);
        assert_eq!(decoded.repeats(), RunStopDetail::min_repeats_for(kind));
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
}

/// The projected tool identity is host-resolved, so no model-controlled text
/// can reach a durable record, a public projection, or the desktop inspector.
#[test]
fn a_hostile_tool_name_collapses_to_a_closed_category() {
    let hostile = [
        "../../etc/passwd",
        "read_file\n\nAUTHORIZATION: Bearer sk-live-abc123",
        "AKIAIOSFODNN7EXAMPLE",
        "mcp__github__create_pull_request",
        "Read File",
        "read_file ",
        " read_file",
        "READ_FILE",
        "read_file\u{0}",
        &"x".repeat(4096),
        "",
    ];
    for name in hostile {
        let detail = RunStopDetail::new(
            RunStopDetailKind::IdenticalCalls,
            MIN_REPEATS_IDENTICAL_CALLS,
        )
        .with_tool(name);
        assert_eq!(
            detail.tool(),
            Some(RunStopTool::Unresolved),
            "{name:?} must not resolve to a host tool"
        );
        // The wire form is a fixed token; the original text is gone.
        let encoded = serde_json::to_string(&detail).unwrap();
        assert!(encoded.contains("\"tool\":\"unresolved\""), "{encoded}");
        for fragment in ["passwd", "Bearer", "AKIA", "mcp__", "xxxx"] {
            assert!(!encoded.contains(fragment), "{name:?} leaked {fragment}");
        }
    }
}

/// Exact host names resolve, and both dispatch aliases collapse to one
/// identity so alternating them cannot produce two different labels.
#[test]
fn host_tool_names_resolve_and_aliases_converge() {
    assert_eq!(RunStopTool::resolve("read_file"), RunStopTool::ReadFile);
    assert_eq!(
        RunStopTool::resolve("run_terminal_cmd"),
        RunStopTool::RunTerminalCmd
    );
    assert_eq!(RunStopTool::resolve("task_output"), RunStopTool::TaskOutput);
    assert_eq!(
        RunStopTool::resolve("get_task_output"),
        RunStopTool::TaskOutput,
        "the dispatch alias must not be a second identity"
    );
    // Every variant round-trips through its wire token.
    for tool in [
        RunStopTool::ApplyPatch,
        RunStopTool::TaskOutput,
        RunStopTool::Unresolved,
    ] {
        let encoded = serde_json::to_string(&tool).unwrap();
        let decoded: RunStopTool = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, tool);
        assert_eq!(encoded, format!("\"{}\"", tool.as_str()));
    }
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

/// The exact defect the re-review named: finalization could replace
/// `Stationarity` with `TokenAccountingUnavailable` while keeping the
/// stationarity detail. The record installed, then `load_run` refused it and
/// `list_runs` silently hid it — a Run that existed but could not be read.
#[test]
fn a_finalization_that_overrides_the_cause_stays_readable() {
    let dir = tempdir().unwrap();
    let store = OrchStore::open(dir.path().join("orch")).unwrap();

    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("task_output");
    let mut run = run_with("run-accounting", Some((RunStopCause::Stationarity, detail)));
    // A bounded run with an unresolved provider attempt: finalization will
    // fail closed on accounting and replace the cause.
    run.bounds.max_total_tokens = Some(1_000);
    run.aggregates.usage_pending_requests = 1;
    run.aggregates.usage_complete = true;
    store.save_run(&run).unwrap();

    let mut candidate = store.load_run("run-accounting").unwrap().unwrap();
    assert!(candidate.stop_detail.is_some(), "fixture precondition");
    // The accounting fail-closed decision replaces the cause. Cause, code and
    // detail move as one observation, so the stationarity detail cannot stay
    // attached to a cause that no longer holds.
    candidate.set_stop_observation(
        RunStopCause::TokenAccountingUnavailable,
        Some("max_total_tokens_usage_unavailable"),
        None,
    );
    assert!(
        candidate.stop_detail.is_none(),
        "the override must take the detail with it"
    );

    let installed = store.persist_finalization(&candidate).unwrap();
    assert_eq!(
        installed.stop_cause,
        Some(RunStopCause::TokenAccountingUnavailable)
    );

    // Persisted and readable are the same set.
    let read_back = store
        .load_run("run-accounting")
        .expect("a finalized record must be readable")
        .expect("present");
    assert!(read_back.stop_detail.is_none());
    assert_eq!(
        store.list_runs().unwrap().len(),
        1,
        "a finalized record must not vanish from the listing"
    );
}

/// A crash between writing a finalization intent and installing it: reopening
/// the store replays the intent, and what it installs is readable.
#[test]
fn a_finalization_intent_survives_a_crash_cut_and_installs_a_readable_record() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let run_id = "run-crashcut";

    {
        let store = OrchStore::open(&root).unwrap();
        let detail = RunStopDetail::new(RunStopDetailKind::TrueNoop, MIN_REPEATS_TRUE_NOOP)
            .with_tool("run_terminal_cmd");
        store
            .save_run(&run_with(
                run_id,
                Some((RunStopCause::Stationarity, detail)),
            ))
            .unwrap();
    }

    // Plant an intent as a crash would have left one, under the store's own
    // sha256(id) filename scheme.
    let safe = safe_id_filename(run_id).expect("store filename");
    let intent_detail =
        RunStopDetail::new(RunStopDetailKind::InertRepeat, 6).with_tool("task_output");
    let intent = run_with(run_id, Some((RunStopCause::Stationarity, intent_detail)));
    std::fs::write(
        root.join("finalization").join(format!("{safe}.json")),
        serde_json::to_string(&intent).unwrap(),
    )
    .unwrap();

    // Reopen: recovery validates and replays the intent.
    let store = OrchStore::open(&root).expect("recovery must accept a valid intent");
    let recovered = store.load_run(run_id).unwrap().expect("present");
    assert_eq!(recovered.stop_cause, Some(RunStopCause::Stationarity));
    assert_eq!(
        recovered.stop_detail().map(|d| d.repeats()),
        Some(6),
        "the intent must be the record that won"
    );
    assert!(!root
        .join("finalization")
        .join(format!("{safe}.json"))
        .exists());
}

/// Recovery does not repair a malformed intent into existence: durable data of
/// unknown age is validated exactly, so corruption surfaces instead of being
/// normalized away.
#[test]
fn recovery_refuses_a_malformed_finalization_intent() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let run_id = "run-badintent";
    {
        OrchStore::open(&root).unwrap();
    }

    let safe = safe_id_filename(run_id).expect("store filename");

    // A detail under a cause it may not accompany: exactly what normalization
    // would have prevented on the write path, so its presence here means the
    // file was not written by this code.
    let mut intent = run_with(run_id, None);
    intent.stop_cause = Some(RunStopCause::RoundLimit);
    intent.stop_detail =
        Some(RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("task_output"));
    std::fs::write(
        root.join("finalization").join(format!("{safe}.json")),
        serde_json::to_string(&intent).unwrap(),
    )
    .unwrap();

    assert!(
        OrchStore::open(&root).is_err(),
        "a malformed recovery intent must fail closed, not be silently repaired"
    );
}

/// Path to the shared wire fixtures the desktop tests read.
fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../desktop/src/lib/__fixtures__")
        .join(name)
}

/// Rust is the authority for the wire shape, so the fixtures the TypeScript
/// tests consume are generated from Rust serialization and compared here.
///
/// Set `UPDATE_WIRE_FIXTURES=1` to rewrite them after a deliberate change; a
/// drift that is not deliberate fails this test instead of silently changing
/// what desktop believes the wire looks like.
#[test]
fn wire_fixtures_match_rust_serialization() {
    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, 4).with_tool("task_output");
    let unresolved =
        RunStopDetail::new(RunStopDetailKind::IdenticalCalls, 16).with_tool("mcp__x__do_thing");

    let v2 = serde_json::json!({
        "schemaVersion": PROGRESS_PROJECTION_SCHEMA_VERSION,
        "runId": "run-fixture",
        "sessionId": "00000000-0000-4000-8000-000000000000",
        "state": "limit_reached",
        "queuePosition": serde_json::Value::Null,
        "busy": false,
        "startSeq": 1,
        "endSeq": 9,
        "progress": serde_json::Value::Null,
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:05:00Z",
        "terminalResult": "stationarity",
        "stopCause": "stationarity",
        "stopDetail": detail,
        "unresolvedToolExample": unresolved,
        "bounds": {
            "maxPromptBytes": 100000,
            "maxRounds": 24,
            "maxDurationMs": 900000,
            "maxTotalTokens": serde_json::Value::Null,
        },
        "errorCode": "stationarity",
    });

    // v1 is the historical shape: it echoed the prompt and had no stop detail.
    let v1 = serde_json::json!({
        "schemaVersion": 1,
        "runId": "run-fixture",
        "sessionId": "00000000-0000-4000-8000-000000000000",
        "state": "limit_reached",
        "promptPreview": "the user's own prompt text",
        "stopCause": "stationarity",
        "bounds": {
            "maxPromptBytes": 100000,
            "maxRounds": 24,
            "maxDurationMs": 900000,
            "maxTotalTokens": serde_json::Value::Null,
        },
    });

    for (name, value) in [
        ("progress-projection.v1.json", &v1),
        ("progress-projection.v2.json", &v2),
    ] {
        let path = fixture_path(name);
        let rendered = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
        if std::env::var("UPDATE_WIRE_FIXTURES").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &rendered).unwrap();
            continue;
        }
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("missing wire fixture {name}: {error}"));
        assert_eq!(
            committed, rendered,
            "{name} drifted from Rust serialization; rerun with UPDATE_WIRE_FIXTURES=1 if intended"
        );
    }

    // The v2 fixture must carry no prompt text at all.
    let encoded = serde_json::to_string(&v2).unwrap();
    assert!(!encoded.contains("promptPreview"));
    // And the unresolved tool must be the fixed category, not the MCP name.
    assert!(encoded.contains("\"tool\":\"unresolved\""));
    assert!(!encoded.contains("mcp__"));
}

/// The redacted projection, exercised without a host so the wire shape itself
/// is the subject rather than the service plumbing around it.
mod projection {
    use super::*;
    use crate::orchestration::service::project_progress;

    fn stationary_run(detail: RunStopDetail) -> RunRecord {
        let mut run = run_with("run-projection", Some((RunStopCause::Stationarity, detail)));
        run.prompt_preview = "SENSITIVE-PROMPT /home/someone/.ssh/id_ed25519 hunter2".into();
        run
    }

    #[test]
    fn the_projection_is_versioned_redacted_and_reports_the_detail() {
        let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, MIN_REPEATS_INERT_REPEAT)
            .with_tool("get_task_output");
        let projected = project_progress(&stationary_run(detail), None, false).unwrap();
        let encoded = serde_json::to_string(&projected).unwrap();

        assert_eq!(
            projected["schemaVersion"],
            PROGRESS_PROJECTION_SCHEMA_VERSION
        );
        assert!(projected.get("promptPreview").is_none());
        for leak in ["SENSITIVE-PROMPT", "/home/someone", "id_ed25519", "hunter2"] {
            assert!(!encoded.contains(leak), "projection leaked {leak}");
        }
        assert_eq!(projected["stopCause"], "stationarity");
        assert_eq!(projected["stopDetail"]["kind"], "inert_repeat");
        assert_eq!(projected["stopDetail"]["repeats"], MIN_REPEATS_INERT_REPEAT);
        // The dispatch alias was resolved to one host identity before it could
        // reach the wire.
        assert_eq!(projected["stopDetail"]["tool"], "task_output");
    }

    #[test]
    fn a_hostile_tool_name_never_reaches_the_projection() {
        let detail = RunStopDetail::new(
            RunStopDetailKind::IdenticalCalls,
            MIN_REPEATS_IDENTICAL_CALLS,
        )
        .with_tool("mcp__evil__leak\nAUTHORIZATION: Bearer sk-live-abc");
        let projected = project_progress(&stationary_run(detail), None, false).unwrap();
        let encoded = serde_json::to_string(&projected).unwrap();
        assert_eq!(projected["stopDetail"]["tool"], "unresolved");
        for leak in ["mcp__", "Bearer", "sk-live", "AUTHORIZATION"] {
            assert!(!encoded.contains(leak), "projection leaked {leak}");
        }
    }

    #[test]
    fn the_projection_refuses_a_semantically_impossible_detail() {
        // A detail whose repeat count its detector could not have produced, or
        // whose kind and tool contradict each other, is refused at the read
        // surface rather than rendered as a host decision.
        let understated = RunStopDetail::new(
            RunStopDetailKind::IdenticalCalls,
            MIN_REPEATS_IDENTICAL_CALLS - 1,
        )
        .with_tool("read_file");
        assert!(project_progress(&stationary_run(understated), None, false).is_err());

        let impossible = RunStopDetail::new(RunStopDetailKind::TrueNoop, MIN_REPEATS_TRUE_NOOP)
            .with_tool("read_file");
        assert!(project_progress(&stationary_run(impossible), None, false).is_err());

        let inert_of_one = RunStopDetail::new(RunStopDetailKind::InertRepeat, 1).with_tool("grep");
        assert!(project_progress(&stationary_run(inert_of_one), None, false).is_err());
    }
}

/// A record omitted from a listing must leave evidence. A Run disappearing with
/// nothing reporting it is how a durable defect stays invisible.
#[test]
fn an_omitted_malformed_record_raises_store_health_evidence() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("orch");
    let store = OrchStore::open(&root).unwrap();

    let detail = RunStopDetail::new(RunStopDetailKind::InertRepeat, MIN_REPEATS_INERT_REPEAT)
        .with_tool("task_output");
    store
        .save_run(&run_with(
            "run-health",
            Some((RunStopCause::Stationarity, detail)),
        ))
        .unwrap();
    assert_eq!(store.list_runs().unwrap().len(), 1);
    assert_eq!(store.malformed_run_records(), 0);

    // Tamper the cause so the detail no longer belongs to it.
    let path = root
        .join("runs")
        .join(format!("{}.json", safe_id_filename("run-health").unwrap()));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("\"stationarity\"", "\"round_limit\"")).unwrap();

    assert!(store.list_runs().unwrap().is_empty(), "must not be listed");
    assert!(
        store.malformed_run_records() >= 1,
        "omission must be counted, not silent"
    );
    assert!(store
        .last_run_error()
        .is_some_and(|error| error.contains("run-health")));
}
