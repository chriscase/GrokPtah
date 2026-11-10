//! Conformance for the orchestrator adapter: durability, no duplicate
//! dispatch, fail-closed restart and uncertainty, binding, and cancellation.

mod common;

use common::{Harness, ok, phase, refused, revision, submit};
use grokptah_headless_host::attention::AttentionResolution;
use grokptah_headless_host::control::ControlCommand;
use grokptah_headless_host::engine::{DispatchDisposition, EngineOutcome};
use grokptah_headless_host::lease::ControlClass;
use grokptah_headless_host::lifecycle::ShutdownKind;
use grokptah_headless_host::orchestration::{OrchestratedEngine, OrchestratorBinding, TurnRefusal};
use grokptah_headless_host::testing::{DispatchLog, FakeOrchestrator, FakeTurn};
use serde_json::{Value, json};

fn progress(note: &str) -> EngineOutcome {
    EngineOutcome::Progress {
        update: json!({ "note": note }),
    }
}

fn completed() -> EngineOutcome {
    EngineOutcome::Completed {
        changed_files: vec![grokptah_headless_host::engine::EngineChangedFile {
            path: "src/lib.rs".into(),
            summary: "add guard".into(),
        }],
        diff: "--- a\n+++ b\n".into(),
        fingerprint: "fingerprint-orchestrated".into(),
    }
}

/// The durable record for one run, straight off disk.
fn record_on_disk(harness: &Harness, run_id: &str) -> Value {
    let path = harness
        .home
        .path()
        .join("runs")
        .join(run_id)
        .join("record.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("record readable"))
        .expect("record parses")
}

/// Simulate the process dying between the write-ahead record and the answer.
fn strip_dispatch_settlement(harness: &Harness, run_id: &str) {
    let path = harness
        .home
        .path()
        .join("runs")
        .join(run_id)
        .join("record.json");
    let mut record: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("record readable"))
            .expect("record parses");
    record["dispatch"]
        .as_object_mut()
        .expect("a dispatch was recorded")
        .remove("settled")
        .expect("the dispatch had settled before it was stripped");
    std::fs::write(&path, record.to_string()).expect("record rewritten");
}

#[test]
fn an_orchestrated_run_records_its_attempt_and_receipt_references_durably() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![
        FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            Some("receipt-1"),
        ),
        FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-2"),
            Some("receipt-2"),
        ),
    ]);

    assert_eq!(
        ok(&mut host, ControlCommand::Health)["engine"],
        "orchestrated"
    );
    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    assert_eq!(phase(&mut host, &run_id), "completed");

    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["ordinal"], 2);
    assert_eq!(dispatch["settled"]["disposition"], "resolved");
    assert_eq!(dispatch["settled"]["attempt"], "attempt-2");
    assert_eq!(dispatch["settled"]["receipt"], "receipt-2");

    // The references reach the journal too, so a reconciliation can be done
    // from the event stream without reading the record.
    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id: run_id.clone(),
            after_seq: None,
            limit: Some(64),
        },
    );
    let rendered = events.to_string();
    assert!(
        rendered.contains("attempt-1"),
        "the first attempt is journaled"
    );
    assert!(
        rendered.contains("attempt-2"),
        "the second attempt is journaled"
    );

    assert_eq!(
        log.ordinals(),
        vec![1, 2],
        "one dispatch per round, in order"
    );

    // The record survives a restart with its references intact.
    drop(host);
    let (mut host, _) = harness.open_orchestrated(Vec::new());
    let receipt = ok(&mut host, ControlCommand::Receipt { run_id });
    assert_eq!(receipt["fingerprint"], "fingerprint-orchestrated");
}

#[test]
fn a_dispatch_ordinal_is_never_handed_out_twice() {
    let harness = Harness::new();
    let mut config = harness.config();
    config.limits.max_rounds = 6;
    let log = DispatchLog::new();
    let engine = OrchestratedEngine::new(FakeOrchestrator::fixture(
        log.clone(),
        vec![FakeTurn::dispatched(
            progress("still working"),
            DispatchDisposition::Resolved,
            Some("attempt-n"),
            None,
        )],
    ));
    let mut host = {
        let mut tuned = config;
        tuned.engine = grokptah_headless_host::config::EngineSelection::Disabled;
        grokptah_headless_host::HeadlessHost::open(
            tuned,
            Some(Box::new(engine)),
            harness.clock.clone(),
            harness.shutdown.clone(),
        )
        .expect("host opens")
    };

    submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(6) });

    let ordinals = log.ordinals();
    assert_eq!(ordinals, vec![1, 2, 3, 4, 5, 6]);
    let mut unique = ordinals.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ordinals.len(), "ordinals must never repeat");
}

#[test]
fn a_lost_answer_halts_the_run_and_is_never_dispatched_again() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![
        FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        ),
        FakeTurn::lost("attempt-2"),
    ]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "needs_attention");
    assert_eq!(status["stopReason"], "dispatch_indeterminate");
    assert_eq!(status["attention"]["kind"], "dispatch_uncertain");
    assert_eq!(status["attention"]["reasonCode"], "dispatch_indeterminate");

    // The lost attempt is on disk so it can be reconciled with the provider.
    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "indeterminate");
    assert_eq!(dispatch["settled"]["attempt"], "attempt-2");

    // Ticking on does not try again, no matter how many times it is asked.
    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    assert_eq!(
        log.ordinals(),
        vec![1, 2],
        "an unresolved dispatch is never repeated"
    );
    assert_eq!(phase(&mut host, &run_id), "needs_attention");
}

#[test]
fn an_uncertain_run_cannot_be_allowed_or_resumed_around() {
    let harness = Harness::new().grant(grokptah_headless_host::authority::CAP_PROMOTE);
    let (mut host, log) = harness.open_orchestrated(vec![FakeTurn::lost("attempt-1")]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "needs_attention");

    let attention_id = ok(
        &mut host,
        ControlCommand::Attention {
            run_id: run_id.clone(),
        },
    )["attention"]["attentionId"]
        .as_str()
        .expect("attention identity")
        .to_owned();

    // Allowing would re-run a round that may already have taken effect.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::ResolveAttention {
                run_id: run_id.clone(),
                attention_id: attention_id.clone(),
                resolution: AttentionResolution::Allow,
            },
        ),
        "dispatch_indeterminate"
    );

    // So would resuming, even with a fresh prompt and a valid lease.
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
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Resume {
                run_id: run_id.clone(),
                lease_id,
                expected_revision,
                prompt: Some("build".to_owned()),
            },
        ),
        "dispatch_indeterminate"
    );

    // Denying is always available: it stops work rather than repeating it.
    ok(
        &mut host,
        ControlCommand::ResolveAttention {
            run_id: run_id.clone(),
            attention_id,
            resolution: AttentionResolution::Deny,
        },
    );
    assert_eq!(phase(&mut host, &run_id), "failed");
    assert_eq!(log.ordinals(), vec![1], "no further dispatch at any point");
}

#[test]
fn a_host_that_dies_mid_dispatch_recovers_fail_closed() {
    let harness = Harness::new();
    let run_id;
    {
        let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        )]);
        run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        assert_eq!(phase(&mut host, &run_id), "running");
    }
    // The record now looks exactly like a process killed inside the step.
    strip_dispatch_settlement(&harness, &run_id);

    let (mut host, log) = harness.open_orchestrated(vec![FakeTurn::dispatched(
        completed(),
        DispatchDisposition::Resolved,
        Some("attempt-2"),
        None,
    )]);
    let report = host.startup_report();
    assert_eq!(report.recovery.indeterminate_dispatch, vec![run_id.clone()]);
    assert_eq!(report.recovery.interrupted, vec![run_id.clone()]);
    assert!(!report.recovery.is_clean());

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "interrupted");
    assert_eq!(status["attention"]["kind"], "dispatch_uncertain");

    // Nothing restarts it, and nothing dispatches for it.
    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    assert_eq!(phase(&mut host, &run_id), "interrupted");
    assert!(log.entries().is_empty(), "recovery must not dispatch");

    let health = ok(&mut host, ControlCommand::Health);
    assert!(
        health["needsAttention"]
            .as_array()
            .expect("needs attention")
            .contains(&json!(run_id))
    );
}

#[test]
fn an_unreconciled_dispatch_never_rewrites_a_finished_run() {
    let harness = Harness::new();
    let run_id;
    {
        let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        )]);
        run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        assert_eq!(phase(&mut host, &run_id), "completed");
    }
    // The run finished, but its dispatch record looks unreconciled on disk.
    strip_dispatch_settlement(&harness, &run_id);

    let (mut host, log) = harness.open_orchestrated(Vec::new());
    let report = host.startup_report();
    assert_eq!(report.recovery.indeterminate_dispatch, vec![run_id.clone()]);

    // A finished run stays finished: recovery settles the dispatch so it can be
    // reconciled, but does not reopen or escalate a run that already has an
    // answer.
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "completed");
    assert!(status["attention"].is_null());
    assert_eq!(
        record_on_disk(&harness, &run_id)["dispatch"]["settled"]["disposition"],
        "indeterminate"
    );

    // Nor does the escalation deadline passing turn it into a failure.
    harness.clock.advance(60_000);
    ok(&mut host, ControlCommand::Tick { steps: Some(4) });
    assert_eq!(phase(&mut host, &run_id), "completed");
    assert!(
        log.entries().is_empty(),
        "a finished run is never dispatched again"
    );
    ok(&mut host, ControlCommand::Receipt { run_id });
}

#[test]
fn an_orchestrator_bound_elsewhere_never_dispatches() {
    let harness = Harness::new();
    let log = DispatchLog::new();
    let engine = OrchestratedEngine::new(FakeOrchestrator::new(
        OrchestratorBinding::new("session-somewhere-else", "project").expect("binding"),
        log.clone(),
        vec![FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        )],
    ));
    let mut host = harness.open_injected(Box::new(engine));

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["stopReason"], "orchestrator_binding_mismatch");
    assert!(
        log.entries().is_empty(),
        "a mismatched binding must not dispatch"
    );

    // Nothing was dispatched, so the dispatch settles clean rather than unknown.
    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "local");
}

#[test]
fn cancellation_stops_the_next_dispatch_and_leaves_the_run_recoverable() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![
        FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        ),
        FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-2"),
            None,
        ),
    ]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(log.ordinals(), vec![1]);

    // An immediate stop reaches the step channel even while the loop is busy.
    host.cancel_signal().cancel();
    ok(&mut host, ControlCommand::Tick { steps: Some(4) });

    assert_eq!(
        log.ordinals(),
        vec![1],
        "cancellation prevents a new dispatch"
    );
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "needs_attention");
    assert_eq!(
        status["attention"]["reasonCode"],
        "cancelled_before_dispatch"
    );
    assert_eq!(status["attention"]["kind"], "recovery_required");
    // Nothing went out, so there is nothing to reconcile.
    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "local");
}

#[test]
fn an_immediate_shutdown_trips_the_step_cancellation_channel() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![FakeTurn::dispatched(
        progress("planning"),
        DispatchDisposition::Resolved,
        Some("attempt-1"),
        None,
    )]);
    let cancel = host.cancel_signal();
    assert!(!cancel.is_cancelled());

    submit(&mut host, "req-1", "build");
    host.shutdown(ShutdownKind::Immediate)
        .expect("immediate stop");
    assert!(cancel.is_cancelled());
    assert!(log.entries().is_empty(), "nothing ran before the stop");
}

#[test]
fn a_missing_route_fails_the_run_and_a_busy_one_only_halts_it() {
    let harness = Harness::new();

    let (mut host, log) =
        harness.open_orchestrated(vec![FakeTurn::Refusal(TurnRefusal::NotConfigured {
            reason_code: "no_provider_route".to_owned(),
            detail: "no provider is configured for this host".to_owned(),
        })]);
    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    let status = common::status(&mut host, &run_id);
    assert_eq!(
        status["phase"], "failed",
        "waiting cannot fix a missing route"
    );
    assert_eq!(status["stopReason"], "no_provider_route");
    assert_eq!(log.ordinals(), vec![1]);
    drop(host);

    let harness = Harness::new();
    let (mut host, _) =
        harness.open_orchestrated(vec![FakeTurn::Refusal(TurnRefusal::Unavailable {
            reason_code: "breaker_open".to_owned(),
            detail: "the route is cooling down".to_owned(),
        })]);
    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    let status = common::status(&mut host, &run_id);
    assert_eq!(
        status["phase"], "needs_attention",
        "an operator can fix this"
    );
    assert_eq!(status["attention"]["reasonCode"], "breaker_open");
    assert_eq!(status["attention"]["kind"], "engine_failure");

    // A refusal dispatched nothing, so the run stays allowable.
    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "local");
}

#[test]
fn a_recoverable_halt_can_be_allowed_and_the_run_continues() {
    let harness = Harness::new().grant(grokptah_headless_host::authority::CAP_PROMOTE);
    let (mut host, log) = harness.open_orchestrated(vec![
        FakeTurn::Refusal(TurnRefusal::Unavailable {
            reason_code: "breaker_open".to_owned(),
            detail: "cooling down".to_owned(),
        }),
        FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-2"),
            None,
        ),
    ]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    let attention_id = ok(
        &mut host,
        ControlCommand::Attention {
            run_id: run_id.clone(),
        },
    )["attention"]["attentionId"]
        .as_str()
        .expect("attention identity")
        .to_owned();

    ok(
        &mut host,
        ControlCommand::ResolveAttention {
            run_id: run_id.clone(),
            attention_id,
            resolution: AttentionResolution::Allow,
        },
    );
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    assert_eq!(phase(&mut host, &run_id), "completed");
    assert_eq!(
        log.ordinals(),
        vec![1, 2],
        "the retry is a new dispatch, not a repeat of the old ordinal"
    );
}

#[test]
fn every_disposition_has_exactly_one_host_behaviour() {
    // The four dispositions are the whole vocabulary the host acts on, so each
    // one is exercised end to end rather than only where it happens to appear.
    let cases = [
        (FakeTurn::local(progress("local")), "running", "local"),
        (
            FakeTurn::dispatched(
                progress("prepared"),
                DispatchDisposition::NotDispatched,
                None,
                None,
            ),
            "running",
            "not_dispatched",
        ),
        (
            FakeTurn::dispatched(
                progress("sent"),
                DispatchDisposition::Resolved,
                Some("attempt-1"),
                Some("receipt-1"),
            ),
            "running",
            "resolved",
        ),
        (
            FakeTurn::dispatched(
                progress("unknown"),
                DispatchDisposition::Indeterminate,
                Some("attempt-1"),
                None,
            ),
            "needs_attention",
            "indeterminate",
        ),
    ];

    for (turn, expected_phase, expected_disposition) in cases {
        let harness = Harness::new();
        let (mut host, log) = harness.open_orchestrated(vec![turn]);
        let run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });

        assert_eq!(
            phase(&mut host, &run_id),
            expected_phase,
            "phase for {expected_disposition}"
        );
        assert_eq!(
            record_on_disk(&harness, &run_id)["dispatch"]["settled"]["disposition"],
            expected_disposition
        );
        assert_eq!(log.ordinals(), vec![1]);

        // Only the unprovable one stops the run from taking another turn.
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        let advanced = log.ordinals().len() > 1;
        assert_eq!(
            advanced,
            expected_disposition != "indeterminate",
            "{expected_disposition} should {} advance",
            if expected_disposition == "indeterminate" {
                "not"
            } else {
                ""
            }
        );
    }
}

#[test]
fn a_completed_turn_on_an_unproven_dispatch_is_not_a_completion() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![FakeTurn::dispatched(
        completed(),
        DispatchDisposition::Indeterminate,
        Some("attempt-1"),
        Some("receipt-1"),
    )]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    // The orchestrator said "completed". It could not prove the request landed,
    // so the host records no completion and publishes no receipt.
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "needs_attention");
    assert_eq!(status["receiptAvailable"], false);
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Receipt {
                run_id: run_id.clone()
            }
        ),
        "receipt_absent"
    );
    assert!(
        record_on_disk(&harness, &run_id)["completion"].is_null(),
        "an unproven dispatch must not record completion evidence"
    );

    // And no amount of ticking turns it into one.
    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    assert_eq!(phase(&mut host, &run_id), "needs_attention");
    assert_eq!(log.ordinals(), vec![1]);
}

#[test]
fn a_stop_during_a_long_turn_halts_it_without_a_second_runtime() {
    let harness = Harness::new();
    let (mut host, log) = harness.open_orchestrated(vec![
        FakeTurn::cancelled_after_dispatch("attempt-1"),
        FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-2"),
            None,
        ),
    ]);
    let cancel = host.cancel_signal();

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    // The stop arrived inside the turn, on the same channel the OS signal
    // watcher uses. Nothing here spawned a runtime to deliver it.
    assert!(cancel.is_cancelled());
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "needs_attention");
    assert_eq!(status["stopReason"], "dispatch_indeterminate");
    assert_eq!(status["attention"]["kind"], "dispatch_uncertain");

    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "indeterminate");
    assert_eq!(dispatch["settled"]["attempt"], "attempt-1");

    // The second scripted turn is never reached.
    ok(&mut host, ControlCommand::Tick { steps: Some(8) });
    assert_eq!(log.ordinals(), vec![1]);
}

#[test]
fn a_turn_that_aborts_before_sending_leaves_nothing_to_reconcile() {
    let harness = Harness::new().grant(grokptah_headless_host::authority::CAP_PROMOTE);
    let (mut host, log) = harness.open_orchestrated(vec![FakeTurn::CancelledBeforeSend]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "needs_attention");
    assert_eq!(status["attention"]["reasonCode"], "cancelled_before_send");
    assert_eq!(status["attention"]["kind"], "recovery_required");

    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "not_dispatched");
    assert!(
        dispatch["settled"]["attempt"].is_null(),
        "nothing went out, so there is no attempt to reconcile"
    );

    // Nothing was sent, so this run is still allowable — unlike an unproven one.
    let attention_id = ok(
        &mut host,
        ControlCommand::Attention {
            run_id: run_id.clone(),
        },
    )["attention"]["attentionId"]
        .as_str()
        .expect("attention identity")
        .to_owned();
    ok(
        &mut host,
        ControlCommand::ResolveAttention {
            run_id: run_id.clone(),
            attention_id,
            resolution: AttentionResolution::Allow,
        },
    );
    assert_eq!(phase(&mut host, &run_id), "queued");
    assert_eq!(log.ordinals(), vec![1]);
}

#[test]
fn a_clean_stop_between_turns_is_never_reported_as_unproven() {
    // Graceful: the run is checkpointed and stays resumable.
    let harness = Harness::new();
    let run_id;
    {
        let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        )]);
        run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        let stop = host
            .shutdown(ShutdownKind::Graceful)
            .expect("graceful stop");
        assert_eq!(stop.paused, vec![run_id.clone()]);
    }
    let (host, _) = harness.open_orchestrated(Vec::new());
    let report = host.startup_report();
    assert_eq!(report.recovery.resumable, vec![run_id.clone()]);
    assert!(
        report.recovery.indeterminate_dispatch.is_empty(),
        "a dispatch that settled before the stop is not unproven"
    );
    drop(host);

    // Immediate: the run is interrupted, but its settled dispatch is still not
    // in question. Only one caught mid-flight is.
    let harness = Harness::new();
    let run_id;
    {
        let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        )]);
        run_id = submit(&mut host, "req-1", "build");
        ok(&mut host, ControlCommand::Tick { steps: Some(1) });
        host.shutdown(ShutdownKind::Immediate)
            .expect("immediate stop");
    }
    let (mut host, _) = harness.open_orchestrated(Vec::new());
    let report = host.startup_report();
    assert_eq!(report.recovery.interrupted, vec![run_id.clone()]);
    assert!(report.recovery.indeterminate_dispatch.is_empty());

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "interrupted");
    assert_eq!(status["stopReason"], "restart_recovery");
    assert!(
        status["attention"].is_null(),
        "a settled dispatch raises no reconciliation escalation"
    );
}

#[test]
fn a_handle_that_is_not_opaque_is_dropped_and_settles_unproven() {
    let harness = Harness::new();
    let smuggled = "../../etc/passwd";
    let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::smuggled_handle(smuggled)]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    // The orchestrator claimed the turn resolved. Its reference cannot be used
    // to reconcile anything, so the claim is downgraded rather than trusted.
    let dispatch = record_on_disk(&harness, &run_id)["dispatch"].clone();
    assert_eq!(dispatch["settled"]["disposition"], "indeterminate");
    assert!(dispatch["settled"]["attempt"].is_null());
    assert_eq!(phase(&mut host, &run_id), "needs_attention");

    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id: run_id.clone(),
            after_seq: None,
            limit: Some(64),
        },
    );
    for surface in [
        events.to_string(),
        common::status(&mut host, &run_id).to_string(),
        record_on_disk(&harness, &run_id).to_string(),
    ] {
        assert!(
            !surface.contains(smuggled),
            "a rejected handle must not be echoed anywhere"
        );
    }
}

#[test]
fn public_projections_carry_no_host_path_and_no_raw_prompt() {
    let harness = Harness::new();
    let (mut host, _) = harness.open_orchestrated(vec![
        FakeTurn::dispatched(
            progress("planning"),
            DispatchDisposition::Resolved,
            Some("attempt-1"),
            None,
        ),
        FakeTurn::dispatched(
            completed(),
            DispatchDisposition::Resolved,
            Some("attempt-2"),
            None,
        ),
    ]);

    // A prompt longer than the preview bound, carrying a host path, a
    // credential, and a sentinel far past where any preview could reach.
    let workspace = harness.workspace_path().display().to_string();
    let home = harness.home.path().display().to_string();
    let secret = "xai-abcdefghijklmnopqrstuvwxyz012345";
    let sentinel = "SENTINEL-PROMPT-TAIL-9f2c";
    let prompt = format!(
        "build in {workspace} with XAI_API_KEY={secret} {} {sentinel}",
        "filler ".repeat(120)
    );

    let run_id = submit(&mut host, "req-1", &prompt);
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    assert_eq!(phase(&mut host, &run_id), "completed");

    let surfaces = [
        common::status(&mut host, &run_id).to_string(),
        ok(
            &mut host,
            ControlCommand::Events {
                run_id: run_id.clone(),
                after_seq: None,
                limit: Some(64),
            },
        )
        .to_string(),
        ok(
            &mut host,
            ControlCommand::Receipt {
                run_id: run_id.clone(),
            },
        )
        .to_string(),
        ok(&mut host, ControlCommand::Health).to_string(),
        record_on_disk(&harness, &run_id).to_string(),
    ];
    for surface in surfaces {
        assert!(!surface.contains(&workspace), "leaked the workspace path");
        assert!(!surface.contains(&home), "leaked the host home path");
        assert!(!surface.contains(secret), "leaked the credential");
        assert!(!surface.contains(sentinel), "leaked the raw prompt");
    }

    // The identity in the record is the host's own, not anything the
    // orchestrator supplied.
    let durable = common::status(&mut host, &run_id)["durable"].clone();
    assert_eq!(durable["sessionId"], "session-fixture");
    assert_eq!(durable["workspace"], common::WORKSPACE_NAME);
}

#[test]
fn a_completion_naming_a_path_outside_the_workspace_is_refused_through_the_adapter() {
    let harness = Harness::new();
    let (mut host, _) = harness.open_orchestrated(vec![FakeTurn::dispatched(
        EngineOutcome::Completed {
            changed_files: vec![grokptah_headless_host::engine::EngineChangedFile {
                path: "/etc/shadow".into(),
                summary: "x".into(),
            }],
            diff: String::new(),
            fingerprint: "fingerprint-escape".into(),
        },
        DispatchDisposition::Resolved,
        Some("attempt-1"),
        None,
    )]);

    let run_id = submit(&mut host, "req-1", "build");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["stopReason"], "completion_path_rejected");
    assert_eq!(status["receiptAvailable"], false);
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Receipt {
                run_id: run_id.clone()
            }
        ),
        "receipt_absent"
    );
    assert!(
        !record_on_disk(&harness, &run_id)
            .to_string()
            .contains("/etc/shadow"),
        "a refused path is not recorded"
    );
}
