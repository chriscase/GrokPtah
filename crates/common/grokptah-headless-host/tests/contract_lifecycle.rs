//! End-to-end lifecycle: start, observe, complete, and prove the receipt.

mod common;

use common::{Harness, ok, phase, refused, status, submit};
use grokptah_headless_host::control::ControlCommand;
use grokptah_headless_host::lifecycle::HostState;
use serde_json::json;

#[test]
fn a_run_goes_from_admission_to_a_reviewable_receipt() {
    let harness = Harness::new();
    let mut host = harness.open();

    assert_eq!(host.state(), HostState::Ready);
    let run_id = submit(&mut host, "req-1", "build");
    assert_eq!(phase(&mut host, &run_id), "queued");

    // One tick promotes and takes the first scripted step.
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "running");

    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "completed");

    let status = status(&mut host, &run_id);
    assert_eq!(status["durable"]["state"], "completed");
    assert_eq!(status["stopReason"], "completed");
    assert_eq!(status["receiptAvailable"], true);

    let receipt = ok(
        &mut host,
        ControlCommand::Receipt {
            run_id: run_id.clone(),
        },
    );
    assert_eq!(receipt["changedFiles"][0]["path"], "src/lib.rs");
    assert_eq!(receipt["fingerprint"], "fingerprint-build");
    assert_eq!(receipt["diffTruncated"], false);

    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id,
            after_seq: None,
            limit: Some(64),
        },
    );
    let kinds: Vec<&str> = events["page"]["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["update"]["event"].as_str().expect("event kind"))
        .collect();
    // Every dispatch is bracketed: the journal records that one started before
    // the engine ran, so a crash in between is visible as an in-flight dispatch
    // rather than as a step that never happened.
    assert_eq!(
        kinds,
        vec![
            "run.admitted",
            "run.started",
            "run.dispatch_started",
            "run.dispatch_settled",
            "run.progress",
            "run.dispatch_started",
            "run.dispatch_settled",
            "run.completed",
            "run.finished"
        ]
    );
    assert_eq!(events["page"]["cursorExpired"], false);
}

#[test]
fn a_run_that_changes_nothing_has_no_receipt_to_claim() {
    let harness = Harness::new();
    let mut host = harness.open();

    let run_id = submit(&mut host, "req-noop", "noop");
    ok(&mut host, ControlCommand::Tick { steps: Some(4) });
    assert_eq!(phase(&mut host, &run_id), "completed");
    assert_eq!(status(&mut host, &run_id)["receiptAvailable"], false);
    assert_eq!(
        refused(&mut host, ControlCommand::Receipt { run_id }),
        "receipt_absent"
    );
}

#[test]
fn a_repeated_request_replays_and_a_changed_one_conflicts() {
    let harness = Harness::new();
    let mut host = harness.open();

    let first = submit(&mut host, "req-1", "build");
    let replay = ok(
        &mut host,
        ControlCommand::Submit {
            request_id: "req-1".to_owned(),
            prompt: "build".to_owned(),
            bounds: None,
            execution_mode: None,
            allow_queue: Some(true),
        },
    );
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["run"]["durable"]["runId"], first.as_str());

    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Submit {
                request_id: "req-1".to_owned(),
                prompt: "something else".to_owned(),
                bounds: None,
                execution_mode: None,
                allow_queue: Some(true),
            },
        ),
        "idempotency_conflict"
    );
}

#[test]
fn admission_is_bounded_and_says_which_bound_it_hit() {
    let harness = Harness::new();
    let mut host = harness.open();

    // max_active_runs = 1, max_queued_runs = 2 in the fixture limits.
    submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    submit(&mut host, "req-2", "forever");
    submit(&mut host, "req-3", "forever");

    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Submit {
                request_id: "req-4".to_owned(),
                prompt: "forever".to_owned(),
                bounds: None,
                execution_mode: None,
                allow_queue: Some(true),
            },
        ),
        "queue_full"
    );
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Submit {
                request_id: "req-5".to_owned(),
                prompt: "forever".to_owned(),
                bounds: None,
                execution_mode: None,
                allow_queue: Some(false),
            },
        ),
        "admission_full"
    );
}

#[test]
fn a_run_stops_at_its_round_ceiling_rather_than_running_on() {
    let harness = Harness::new();
    let mut host = harness.open();

    let run_id = ok(
        &mut host,
        ControlCommand::Submit {
            request_id: "req-bounded".to_owned(),
            prompt: "forever".to_owned(),
            bounds: Some(serde_json::from_value(json!({ "maxRounds": 2 })).expect("bounds")),
            execution_mode: None,
            allow_queue: Some(true),
        },
    )["run"]["durable"]["runId"]
        .as_str()
        .expect("run id")
        .to_owned();

    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    let status = status(&mut host, &run_id);
    assert_eq!(status["phase"], "limit_reached");
    assert_eq!(status["durable"]["state"], "limit_reached");
    assert_eq!(status["stopReason"], "max_rounds");
    assert_eq!(status["roundsUsed"], 2);
}

#[test]
fn a_host_without_an_engine_refuses_work_instead_of_pretending() {
    let harness = Harness::new().without_engine();
    let mut host = harness.open();

    let health = ok(&mut host, ControlCommand::Health);
    assert_eq!(health["engine"], "none");
    assert!(
        health["degraded"]
            .as_array()
            .expect("degraded")
            .contains(&json!("engine_disabled"))
    );

    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Submit {
                request_id: "req-1".to_owned(),
                prompt: "build".to_owned(),
                bounds: None,
                execution_mode: None,
                allow_queue: Some(true),
            },
        ),
        "engine_disabled"
    );
}

#[test]
fn health_reports_readiness_and_names_what_is_degraded() {
    let harness = Harness::new();
    let mut host = harness.open();

    let health = ok(&mut host, ControlCommand::Health);
    assert_eq!(health["state"], "ready");
    assert_eq!(health["contract"], grokptah_headless_host::CONTRACT_VERSION);
    assert_eq!(health["home"], "<home>");
    assert!(health["degraded"].as_array().expect("degraded").is_empty());
    assert_eq!(health["runs"]["running"], 0);

    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let health = ok(&mut host, ControlCommand::Health);
    assert_eq!(health["runs"]["needsAttention"], 1);
    assert!(
        health["needsAttention"]
            .as_array()
            .expect("needs attention")
            .contains(&json!(run_id))
    );
    assert!(
        health["degraded"]
            .as_array()
            .expect("degraded")
            .contains(&json!("runs_need_attention"))
    );
}
