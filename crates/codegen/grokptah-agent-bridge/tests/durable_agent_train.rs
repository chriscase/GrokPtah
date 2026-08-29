//! Adversarial tests for the durable-agent stationarity slice.
//!
//! Every test here is deterministic and offline. Nothing contacts a provider,
//! reads a credential, opens a socket, or sleeps.

use grokptah_agent_bridge::durable::{
    self, progress::RepeatClass, progress::StopDecision, ActiveTaskWaitWitness, ActiveWaitState,
    ProgressLedger, RawObservation, RawObservationDigest,
};
use uuid::Uuid;

/// The byte cap `host.rs` applies to one tool result before the model wire.
const WIRE_BOUND: usize = 24_000;

// ---------------------------------------------------------------------------
// Raw digests before bounded projections
// ---------------------------------------------------------------------------

/// The regression this slice exists for.
///
/// Two tool results that differ only *after* the 24,000-byte wire bound project
/// to byte-identical text. A digest taken from the projection reports them as
/// the same observation, which is what turns an advancing run into a false
/// inert repeat. The raw digest must still tell them apart.
#[test]
fn a_suffix_change_beyond_the_wire_bound_still_changes_the_raw_digest() {
    let head = "A".repeat(WIRE_BOUND);
    let first = RawObservation::capture(format!("{head}tail-one"));
    let second = RawObservation::capture(format!("{head}tail-two"));

    let projected_first = first.project(WIRE_BOUND);
    let projected_second = second.project(WIRE_BOUND);

    // The projections really are indistinguishable — same head, same truncation
    // marker, same raw length in the marker.
    assert_eq!(projected_first.text(), projected_second.text());
    assert!(projected_first.truncated() && projected_second.truncated());
    assert_eq!(first.raw_len(), second.raw_len());

    // A digest of the projection cannot tell them apart. This is the defect.
    assert_eq!(
        RawObservationDigest::of_raw(projected_first.text().as_bytes()),
        RawObservationDigest::of_raw(projected_second.text().as_bytes()),
        "a projection digest is blind past the wire bound; that is why it is not used"
    );

    // The raw digest can, and the projection carries it.
    assert_ne!(first.digest(), second.digest());
    assert_eq!(projected_first.raw_digest(), first.digest());
    assert_eq!(projected_second.raw_digest(), second.digest());
}

#[test]
fn a_projection_below_the_bound_is_untouched_and_still_carries_its_digest() {
    let observation = RawObservation::capture("short output");
    let projected = observation.project(WIRE_BOUND);
    assert_eq!(projected.text(), "short output");
    assert!(!projected.truncated());
    assert_eq!(projected.raw_len(), "short output".len());
    assert_eq!(projected.raw_digest(), observation.digest());
}

#[test]
fn digests_are_domain_separated_and_length_prefixed() {
    // Concatenation must not forge equality between different sequences.
    let joined = RawObservationDigest::of_digests(&[
        RawObservationDigest::of_raw(b"ab"),
        RawObservationDigest::of_raw(b"c"),
    ]);
    let other = RawObservationDigest::of_digests(&[
        RawObservationDigest::of_raw(b"a"),
        RawObservationDigest::of_raw(b"bc"),
    ]);
    assert_ne!(joined, other);
    assert!(RawObservationDigest::of_digests(&[]).is_none());
}

/// The digest is taken over real tool-result content, so it must never reach a
/// log line, a durable record, or a read projection.
#[test]
fn a_digest_never_renders_its_value_and_cannot_be_serialized() {
    let digest = RawObservationDigest::of_raw(b"secret-shaped output");
    assert_eq!(format!("{digest:?}"), "RawObservationDigest(<redacted>)");

    // Source guard: the opacity is a property of the type, not of its callers.
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/durable/observation.rs"
    ))
    .expect("source is readable");
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "Serialize",
        "Deserialize",
        "impl fmt::Display",
        "pub fn as_bytes",
    ] {
        assert!(
            !code.contains(forbidden),
            "the observation digest must not gain `{forbidden}`"
        );
    }
}

// ---------------------------------------------------------------------------
// No false no-op / stationarity
// ---------------------------------------------------------------------------

fn poll_round(ledger: &mut ProgressLedger, output: &str) {
    ledger.observe_call("poll", "get_task_output", false);
    ledger.observe_outcome(RawObservation::capture(output).digest());
}

/// A model polling a build log issues a byte-identical call every round. On
/// `main` that alone stops the turn. It must not, while the output advances.
#[test]
fn an_advancing_poll_is_never_stopped_as_stationary() {
    let mut ledger = ProgressLedger::new();
    for round in 0..40 {
        poll_round(&mut ledger, &format!("building… {round} of many"));
        assert_eq!(
            ledger.decide(),
            StopDecision::Continue,
            "round {round} advanced its output and is not stationary"
        );
    }
    assert_eq!(ledger.class(), RepeatClass::Advancing);
}

/// The advance is past the wire bound. End-to-end form of the ordering rule.
#[test]
fn a_poll_whose_output_only_changes_past_the_wire_bound_is_not_inert() {
    let head = "A".repeat(WIRE_BOUND);
    let mut raw_ledger = ProgressLedger::new();
    let mut projection_ledger = ProgressLedger::new();

    for round in 0..durable::progress::MAX_INERT_REPEATS {
        let observation = RawObservation::capture(format!("{head}progress-{round}"));
        let projected = observation.project(WIRE_BOUND);

        raw_ledger.observe_call("poll", "get_task_output", false);
        raw_ledger.observe_outcome(observation.digest());

        // What a digest taken after the bound would have seen.
        projection_ledger.observe_call("poll", "get_task_output", false);
        projection_ledger
            .observe_outcome(RawObservationDigest::of_raw(projected.text().as_bytes()));
    }

    assert_eq!(raw_ledger.class(), RepeatClass::Advancing);
    assert_eq!(raw_ledger.decide(), StopDecision::Continue);

    // The post-bound digest sees an unchanging observation and stops the run.
    assert_eq!(projection_ledger.class(), RepeatClass::Inert);
    assert!(matches!(projection_ledger.decide(), StopDecision::Stop(_)));
}

/// Regression: a run that moved once and then froze must still reach the inert
/// ceiling. An earlier revision kept a sticky "saw advance" flag for the whole
/// signature run, so `A,B,B,B,…` classified as advancing forever and never
/// stopped — a stuck loop that changed its output exactly once was immortal.
#[test]
fn a_run_that_advances_once_and_then_freezes_still_reaches_the_inert_stop() {
    let mut ledger = ProgressLedger::new();
    poll_round(&mut ledger, "phase: build");
    poll_round(&mut ledger, "phase: test");
    assert_eq!(ledger.class(), RepeatClass::Advancing);

    let mut stopped_after = None;
    for frozen in 1..=durable::progress::MAX_INERT_REPEATS + 4 {
        poll_round(&mut ledger, "phase: test");
        if let StopDecision::Stop(detail) = ledger.decide() {
            stopped_after = Some(frozen);
            assert_eq!(detail.class, RepeatClass::Inert);
            break;
        }
    }
    assert_eq!(
        stopped_after,
        Some(durable::progress::MAX_INERT_REPEATS - 1),
        "the unchanged suffix restarts at the change, then reaches the ceiling"
    );
}

#[test]
fn an_inert_repeat_stops_at_the_inert_ceiling() {
    let mut ledger = ProgressLedger::new();
    let mut stopped_at: Option<u32> = None;
    for round in 1..=20u32 {
        poll_round(&mut ledger, "queued; nothing to report");
        if let StopDecision::Stop(detail) = ledger.decide() {
            stopped_at = Some(round);
            assert_eq!(detail.class, RepeatClass::Inert);
            assert_eq!(detail.tool_name, "get_task_output");
            break;
        }
    }
    assert_eq!(stopped_at, Some(durable::progress::MAX_INERT_REPEATS));
}

/// A small model that keeps emitting a no-op shell call must stop quickly, and
/// the no-op run must chain even when the arguments differ.
#[test]
fn a_small_model_no_op_loop_stops_at_four() {
    let mut ledger = ProgressLedger::new();
    for round in 1..=4 {
        assert_eq!(
            ledger.observe_call(&format!("sig{round}"), "run_terminal_cmd", true),
            round
        );
        ledger.observe_outcome(RawObservation::capture("").digest());
    }
    let StopDecision::Stop(detail) = ledger.decide() else {
        panic!("a four-round no-op loop must stop");
    };
    assert_eq!(detail.class, RepeatClass::TrueNoop);
    assert_eq!(detail.repeats, 4);
    assert!(durable::progress::stop_message(&detail).contains("no-op tool calls"));
}

#[test]
fn stationarity_resets_on_a_different_signature() {
    let mut ledger = ProgressLedger::new();
    assert_eq!(ledger.observe_call("a", "read_file", false), 1);
    assert_eq!(ledger.observe_call("a", "read_file", false), 2);
    assert_eq!(ledger.observe_call("b", "read_file", false), 1);
    assert_eq!(ledger.decide(), StopDecision::Continue);
    assert_eq!(ledger.class(), RepeatClass::Fresh);
}

#[test]
fn an_identical_run_nudges_exactly_once_at_eight() {
    let mut ledger = ProgressLedger::new();
    for round in 1..8 {
        poll_round(&mut ledger, &format!("tick {round}"));
        assert!(!ledger.take_nudge());
    }
    poll_round(&mut ledger, "tick 8");
    assert!(ledger.take_nudge(), "the nudge fires at eight repeats");
    assert!(!ledger.take_nudge(), "and only once");
    poll_round(&mut ledger, "tick 9");
    assert!(!ledger.take_nudge());
}

/// With no observation recorded, the host has no evidence of progress, so the
/// historical identical-call ceiling still applies as a safety net.
#[test]
fn an_unobserved_repeat_still_stops_at_the_identical_call_ceiling() {
    let mut ledger = ProgressLedger::new();
    for round in 1..durable::progress::MAX_IDENTICAL_CALLS {
        ledger.observe_call("poll", "get_task_output", false);
        assert_eq!(ledger.decide(), StopDecision::Continue, "round {round}");
    }
    ledger.observe_call("poll", "get_task_output", false);
    let StopDecision::Stop(detail) = ledger.decide() else {
        panic!("an unobserved repeat run must still be bounded");
    };
    assert_eq!(detail.class, RepeatClass::Unobserved);
    assert_eq!(detail.repeats, durable::progress::MAX_IDENTICAL_CALLS);
}

#[test]
fn a_stationarity_stop_message_reads_as_incomplete_not_as_a_round_limit() {
    let mut ledger = ProgressLedger::new();
    for _ in 0..durable::progress::MAX_INERT_REPEATS {
        poll_round(&mut ledger, "unchanged");
    }
    let StopDecision::Stop(detail) = ledger.decide() else {
        panic!("expected a stop");
    };
    let message = durable::progress::stop_message(&detail);
    assert!(message.starts_with("Stopped after "));
    assert!(message.contains("without making progress"));
    assert!(!message.contains("tool rounds"));
}

/// The stop detail is durable, so it must carry no part of the observation.
#[test]
fn a_stop_detail_carries_no_observation_material() {
    let mut ledger = ProgressLedger::new();
    for _ in 0..durable::progress::MAX_INERT_REPEATS {
        poll_round(&mut ledger, "sensitive-looking build output");
    }
    let StopDecision::Stop(detail) = ledger.decide() else {
        panic!("expected a stop");
    };
    let encoded = serde_json::to_string(&detail).expect("stop detail serializes");
    assert!(!encoded.contains("sensitive"));
    // Only the three host-authored fields.
    assert_eq!(
        encoded,
        r#"{"class":"inert","repeats":10,"toolName":"get_task_output"}"#
    );
}

// ---------------------------------------------------------------------------
// Host-issued wait witnesses
// ---------------------------------------------------------------------------

fn witness(session: Uuid, deadline_ms: u64) -> ActiveTaskWaitWitness {
    ActiveTaskWaitWitness {
        task_id: "task-1".into(),
        state: ActiveWaitState::Running,
        owner_session: session,
        generation: 1,
        deadline_ms,
    }
}

/// A wait the host can see is still outstanding is not stuck, even when it
/// returns the same bytes for far longer than the inert ceiling.
#[test]
fn a_long_unchanged_witnessed_wait_survives_past_the_inert_ceiling() {
    let session = Uuid::new_v4();
    let mut ledger = ProgressLedger::new();
    for _ in 0..durable::progress::MAX_INERT_REPEATS + 4 {
        ledger.observe_call("poll", "task_output", false);
        assert!(durable::round_is_witnessed_wait(
            &["task_output"],
            &[witness(session, 600_000)],
            session,
            1_000,
        ));
        ledger.observe_witnessed_wait();
        assert_eq!(ledger.decide(), StopDecision::Continue);
    }
}

/// The exemption is from the inert ceiling only. The identical-call ceiling,
/// which `main` already applied, still bounds the wait.
#[test]
fn a_witnessed_wait_is_still_bounded_by_the_identical_call_ceiling() {
    let mut ledger = ProgressLedger::new();
    let mut stopped_at = None;
    for round in 1..=durable::progress::MAX_IDENTICAL_CALLS + 2 {
        ledger.observe_call("poll", "task_output", false);
        ledger.observe_witnessed_wait();
        if let StopDecision::Stop(detail) = ledger.decide() {
            stopped_at = Some(round);
            assert_eq!(detail.class, RepeatClass::Unobserved);
            break;
        }
    }
    assert_eq!(stopped_at, Some(durable::progress::MAX_IDENTICAL_CALLS));
}

#[test]
fn an_unwitnessed_wait_is_ordinary_work() {
    let session = Uuid::new_v4();
    assert!(
        !durable::round_is_witnessed_wait(&["task_output"], &[], session, 0),
        "a wait with no host witness earns no exemption"
    );
}

#[test]
fn a_witness_from_another_session_earns_nothing() {
    let mine = Uuid::new_v4();
    let theirs = Uuid::new_v4();
    assert!(!durable::round_is_witnessed_wait(
        &["task_output"],
        &[witness(theirs, 600_000)],
        mine,
        0,
    ));
}

#[test]
fn a_witness_past_its_deadline_stops_exempting() {
    let session = Uuid::new_v4();
    assert!(durable::round_is_witnessed_wait(
        &["task_output"],
        &[witness(session, 600_000)],
        session,
        599_999,
    ));
    assert!(
        !durable::round_is_witnessed_wait(
            &["task_output"],
            &[witness(session, 600_000)],
            session,
            600_000,
        ),
        "an abandoned task must not confer an unlimited exemption"
    );
}

#[test]
fn a_round_mixing_a_wait_with_real_work_is_not_exempt() {
    let session = Uuid::new_v4();
    assert!(!durable::round_is_witnessed_wait(
        &["task_output", "run_terminal_cmd"],
        &[witness(session, 600_000)],
        session,
        0,
    ));
    // Witness count must match call count, so one witness cannot cover two polls.
    assert!(!durable::round_is_witnessed_wait(
        &["task_output", "task_output"],
        &[witness(session, 600_000)],
        session,
        0,
    ));
    assert!(!durable::round_is_witnessed_wait(&[], &[], session, 0,));
}

#[test]
fn only_outstanding_task_states_are_witnessable() {
    for status in ["running", "accepted", "proposed", "queued"] {
        assert!(
            ActiveWaitState::from_status(status).is_some(),
            "`{status}` is outstanding work"
        );
    }
    for status in [
        "completed",
        "failed",
        "cancelled",
        "rejected",
        "done",
        "Running",
        "run",
    ] {
        assert!(
            ActiveWaitState::from_status(status).is_none(),
            "`{status}` must not be witnessable"
        );
    }
}

#[test]
fn only_wait_shaped_tools_can_earn_the_exemption() {
    assert!(durable::is_wait_shaped_tool("task_output"));
    assert!(durable::is_wait_shaped_tool("get_task_output"));
    for name in [
        "run_terminal_cmd",
        "read_file",
        "apply_patch",
        "task_outputs",
    ] {
        assert!(
            !durable::is_wait_shaped_tool(name),
            "`{name}` is not a wait"
        );
    }
}

/// A campaign that alternates a witnessed wait with real progress is never
/// stationary, and the wait never masks a genuinely stuck stretch afterwards.
#[test]
fn alternating_witnessed_waits_and_progress_are_never_stopped() {
    let mut ledger = ProgressLedger::new();
    for round in 0..12 {
        if round % 2 == 0 {
            ledger.observe_call("poll", "task_output", false);
            ledger.observe_witnessed_wait();
        } else {
            ledger.observe_call(&format!("work-{round}"), "run_terminal_cmd", false);
            ledger.observe_outcome(RawObservation::capture(format!("built {round}")).digest());
        }
        assert_eq!(ledger.decide(), StopDecision::Continue);
    }
}

/// The durable core must stay offline and hold no authority of its own.
#[test]
fn the_durable_core_contacts_nothing_and_declares_no_authority() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/durable");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("durable dir is readable") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("source is readable");
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "reqwest::",
            "std::net",
            "tokio::net",
            "std::process::Command",
            // Authority belongs to the G1-G4 spine (#497); a second copy here
            // is exactly what #478 and #492 exist to prevent.
            "OperatorGrant",
            "SendLedger",
            "PhysicalSendPermit",
            "Capability",
        ] {
            assert!(
                !code.contains(forbidden),
                "{} must not contain `{forbidden}`",
                path.display()
            );
        }
        checked += 1;
    }
    assert_eq!(
        checked, 5,
        "expected exactly mod, observation, progress, effects and cancel"
    );
}
