//! Restart recovery, shutdown, and exclusive ownership of the home.

mod common;

use common::{Harness, ok, phase, refused, revision, submit};
use grokptah_headless_host::control::ControlCommand;
use grokptah_headless_host::lease::ControlClass;
use grokptah_headless_host::lifecycle::{HostState, ShutdownKind};
use serde_json::json;

#[test]
fn a_graceful_stop_checkpoints_live_runs_so_the_next_start_can_resume() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "forever");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        assert_eq!(phase(&mut host, &run_id), "running");

        let stop = host
            .shutdown(ShutdownKind::Graceful)
            .expect("graceful stop");
        assert_eq!(stop.kind, ShutdownKind::Graceful);
        assert_eq!(stop.paused, vec![run_id.clone()]);
        assert!(stop.left_live.is_empty());
        assert_eq!(host.state(), HostState::Stopped);
    }

    let mut host = harness.open();
    let report = host.startup_report();
    assert!(report.recovery.interrupted.is_empty());
    assert_eq!(report.recovery.resumable, vec![run_id.clone()]);
    assert_eq!(phase(&mut host, &run_id), "paused");
}

#[test]
fn an_immediate_stop_leaves_recovery_to_the_next_start() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "forever");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });

        let stop = host
            .shutdown(ShutdownKind::Immediate)
            .expect("immediate stop");
        assert_eq!(stop.paused, Vec::<String>::new());
        assert_eq!(stop.left_live, vec![run_id.clone()]);
    }

    let mut host = harness.open();
    let report = host.startup_report();
    assert_eq!(report.recovery.interrupted, vec![run_id.clone()]);
    assert!(!report.recovery.is_clean());

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "interrupted");
    assert_eq!(status["durable"]["state"], "interrupted");
    assert_eq!(status["stopReason"], "restart_recovery");

    let health = ok(&mut host, ControlCommand::Health);
    assert!(
        health["awaitingRecovery"]
            .as_array()
            .expect("awaiting recovery")
            .contains(&json!(run_id))
    );
    assert!(
        health["degraded"]
            .as_array()
            .expect("degraded")
            .contains(&json!("runs_awaiting_recovery"))
    );
}

#[test]
fn an_interrupted_run_never_restarts_itself() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "forever");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        host.shutdown(ShutdownKind::Immediate)
            .expect("immediate stop");
    }

    let mut host = harness.open();
    // Ticking repeatedly must not revive it.
    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    assert_eq!(phase(&mut host, &run_id), "interrupted");
}

#[test]
fn resuming_a_recovered_run_requires_an_explicit_prompt() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "forever");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        host.shutdown(ShutdownKind::Immediate)
            .expect("immediate stop");
    }

    let mut host = harness.open();
    let expected_revision = revision(&mut host, &run_id);
    let lease_id = ok(
        &mut host,
        ControlCommand::Lease {
            run_id: run_id.clone(),
            classes: vec![ControlClass::Resume],
            expected_revision,
            ttl_ms: None,
        },
    )["leaseId"]
        .as_str()
        .expect("lease identity")
        .to_owned();

    // The full prompt is never durable, so the host cannot invent one.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Resume {
                run_id: run_id.clone(),
                lease_id: lease_id.clone(),
                expected_revision,
                prompt: None,
            },
        ),
        "prompt_required"
    );

    ok(
        &mut host,
        ControlCommand::Resume {
            run_id: run_id.clone(),
            lease_id,
            expected_revision,
            prompt: Some("forever".to_owned()),
        },
    );
    assert_eq!(phase(&mut host, &run_id), "queued");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "running");
}

#[test]
fn a_second_host_cannot_write_the_same_home() {
    let harness = Harness::new();
    let first = harness.open();
    let error = harness.try_open().expect_err("a second host is refused");
    assert_eq!(error.reason_code(), "home_locked");
    assert_eq!(
        error.envelope().code,
        grokptah_agent_sdk::ErrorCode::AuthorityUnavailable
    );
    drop(first);
    harness.try_open().expect("the home is free again");
}

#[test]
fn durable_state_survives_a_restart_intact() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(4) });
        assert_eq!(phase(&mut host, &run_id), "completed");
    }

    let mut host = harness.open();
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "completed");
    assert_eq!(status["receiptAvailable"], true);

    let receipt = ok(
        &mut host,
        ControlCommand::Receipt {
            run_id: run_id.clone(),
        },
    );
    assert_eq!(receipt["fingerprint"], "fingerprint-build");

    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id,
            after_seq: None,
            limit: Some(64),
        },
    );
    assert!(
        events["page"]["entries"].as_array().expect("entries").len() >= 5,
        "the journal must survive the restart"
    );
}

#[test]
fn a_torn_journal_tail_is_discarded_and_reported() {
    let harness = Harness::new();
    let run_id;
    {
        let mut host = harness.open();
        run_id = submit(&mut host, "req-1", "forever");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        host.shutdown(ShutdownKind::Graceful)
            .expect("graceful stop");
    }

    let journal = harness
        .home
        .path()
        .join("runs")
        .join(&run_id)
        .join("events.jsonl");
    let mut raw = std::fs::read_to_string(&journal).expect("journal readable");
    let intact_lines = raw.lines().count();
    raw.push_str("{\"seq\":99,\"ts\":\"2026-01-01T00:00:00.000Z\",\"upda");
    std::fs::write(&journal, raw).expect("write torn tail");

    let mut host = harness.open();
    let report = host.startup_report();
    assert_eq!(report.recovery.torn_journals, vec![run_id.clone()]);

    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id,
            after_seq: None,
            limit: Some(64),
        },
    );
    assert_eq!(
        events["page"]["entries"].as_array().expect("entries").len(),
        intact_lines
    );
}
