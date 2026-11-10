//! Default-deny authority: capabilities, control leases, and escalation.

mod common;

use common::{Harness, ok, phase, refused, revision, submit};
use grokptah_headless_host::attention::AttentionResolution;
use grokptah_headless_host::authority::CAP_PROMOTE;
use grokptah_headless_host::control::ControlCommand;
use grokptah_headless_host::lease::ControlClass;
use grokptah_headless_host::testing;

fn lease(
    host: &mut grokptah_headless_host::HeadlessHost,
    run_id: &str,
    class: ControlClass,
) -> String {
    let expected_revision = revision(host, run_id);
    ok(
        host,
        ControlCommand::Lease {
            run_id: run_id.to_owned(),
            classes: vec![class],
            expected_revision,
            ttl_ms: None,
        },
    )["leaseId"]
        .as_str()
        .expect("lease identity")
        .to_owned()
}

#[test]
fn a_gated_capability_is_denied_until_it_is_explicitly_granted() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
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

    // Allowing a halted run past its gate is human-gated; denying is not.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::ResolveAttention {
                run_id: run_id.clone(),
                attention_id: attention_id.clone(),
                resolution: AttentionResolution::Allow,
            },
        ),
        "capability_gated"
    );
    assert_eq!(phase(&mut host, &run_id), "needs_attention");

    drop(host);
    let granted = Harness::new().grant(CAP_PROMOTE);
    let mut host = granted.open();
    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
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
}

#[test]
fn denying_an_escalation_stops_the_run_with_an_explicit_reason() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let attention = ok(
        &mut host,
        ControlCommand::Attention {
            run_id: run_id.clone(),
        },
    )["attention"]
        .clone();
    assert_eq!(attention["kind"], "permission_required");
    assert_eq!(attention["reasonCode"], "shell_write_requested");

    ok(
        &mut host,
        ControlCommand::ResolveAttention {
            run_id: run_id.clone(),
            attention_id: attention["attentionId"]
                .as_str()
                .expect("attention identity")
                .to_owned(),
            resolution: AttentionResolution::Deny,
        },
    );
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["stopReason"], "attention_denied");
    assert!(status["attention"].is_null());
}

#[test]
fn an_unanswered_escalation_expires_to_deny_never_to_allow() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    assert_eq!(phase(&mut host, &run_id), "needs_attention");

    // attention_ttl_ms is 5_000 in the fixture limits.
    harness.clock.advance(5_001);
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["stopReason"], "attention_expired");
}

#[test]
fn steering_needs_a_lease_that_matches_scope_class_and_revision() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    let lease_id = lease(&mut host, &run_id, ControlClass::Steer);
    let expected_revision = revision(&mut host, &run_id);

    // A lease that grants steering does not grant cancelling.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Cancel {
                run_id: run_id.clone(),
                lease_id: lease_id.clone(),
                expected_revision,
            },
        ),
        "lease_class_denied"
    );
    // An unknown lease is refused outright.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Steer {
                run_id: run_id.clone(),
                lease_id: "lease-invented".to_owned(),
                expected_revision,
                directive: "focus on tests".to_owned(),
            },
        ),
        "lease_unknown"
    );
    // A stale revision is refused even with the right lease.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Steer {
                run_id: run_id.clone(),
                lease_id: lease_id.clone(),
                expected_revision: expected_revision + 5,
                directive: "focus on tests".to_owned(),
            },
        ),
        "revision_stale"
    );

    ok(
        &mut host,
        ControlCommand::Steer {
            run_id: run_id.clone(),
            lease_id: lease_id.clone(),
            expected_revision,
            directive: "focus on tests".to_owned(),
        },
    );
    // Steering advanced the run, so the same lease is now stale.
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Steer {
                run_id,
                lease_id,
                expected_revision,
                directive: "again".to_owned(),
            },
        ),
        "revision_stale"
    );
}

#[test]
fn a_lease_expires_and_cannot_be_replayed_afterwards() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });

    let lease_id = lease(&mut host, &run_id, ControlClass::Pause);
    let expected_revision = revision(&mut host, &run_id);

    // lease_ttl_ms is 1_000 in the fixture limits.
    harness.clock.advance(1_001);
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Pause {
                run_id,
                lease_id,
                expected_revision,
            },
        ),
        "lease_expired"
    );
}

#[test]
fn a_lease_may_not_outlive_the_host_ceiling() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-1", "forever");
    let expected_revision = revision(&mut host, &run_id);

    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Lease {
                run_id,
                classes: vec![ControlClass::Pause],
                expected_revision,
                ttl_ms: Some(testing::config_fixture().limits.lease_ttl_ms + 1),
            },
        ),
        "bounds_exceed_ceiling"
    );
}

#[test]
fn pause_and_resume_are_explicit_operator_actions() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "running");

    let lease_id = lease(&mut host, &run_id, ControlClass::Pause);
    let expected_revision = revision(&mut host, &run_id);
    ok(
        &mut host,
        ControlCommand::Pause {
            run_id: run_id.clone(),
            lease_id,
            expected_revision,
        },
    );
    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "paused");
    assert_eq!(status["durable"]["state"], "interrupted");
    assert_eq!(status["stopReason"], "operator_pause");

    // A paused run does not move on its own.
    ok(&mut host, ControlCommand::Tick { steps: Some(4) });
    assert_eq!(phase(&mut host, &run_id), "paused");

    let lease_id = lease(&mut host, &run_id, ControlClass::Resume);
    let expected_revision = revision(&mut host, &run_id);
    ok(
        &mut host,
        ControlCommand::Resume {
            run_id: run_id.clone(),
            lease_id,
            expected_revision,
            prompt: None,
        },
    );
    assert_eq!(phase(&mut host, &run_id), "queued");
    ok(&mut host, ControlCommand::Tick { steps: Some(1) });
    assert_eq!(phase(&mut host, &run_id), "running");
}

#[test]
fn a_run_halted_by_an_escalation_cannot_be_resumed_around_it() {
    let harness = Harness::new();
    let mut host = harness.open();
    let run_id = submit(&mut host, "req-esc", "escalate");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let lease_id = lease(&mut host, &run_id, ControlClass::Resume);
    let expected_revision = revision(&mut host, &run_id);
    assert_eq!(
        refused(
            &mut host,
            ControlCommand::Resume {
                run_id,
                lease_id,
                expected_revision,
                prompt: None,
            },
        ),
        "attention_open"
    );
}
