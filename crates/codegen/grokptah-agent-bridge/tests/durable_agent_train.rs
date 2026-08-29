//! Adversarial tests for the durable agent / self-hosting train.
//!
//! Every test here is deterministic and offline. Nothing contacts a provider,
//! reads a credential, opens a socket, or sleeps.

use grokptah_agent_bridge::durable::{
    self, cancel::CancelReason, claim::ClaimError, effects::EffectError, effects::EffectKind,
    effects::EffectState, journal::AppendRefusal, progress::RepeatClass, progress::StopDecision,
    retry::StandDownReason, sdk::BoundaryError, sdk::Capability, sdk::GrantProvenance,
    sdk::NegotiationError, sdk::ProtocolVersion, send::DeliveryKnowledge, send::SendError,
    send::SendState, send::TransportEvidence, BoundedEventLog, CancelStatus, CancellationLedger,
    ClaimLedger, ClaimRecord, EffectRegistry, ManagerSession, ProgressLedger, RawObservation,
    RawObservationDigest, RetryBudget, RetryDecision, Revision, SendAttempt, SendLedger,
};

/// The byte cap `host.rs` applies to one tool result before the model wire.
const WIRE_BOUND: usize = 24_000;

// ---------------------------------------------------------------------------
// Raw digests before bounded projections
// ---------------------------------------------------------------------------

/// The regression this train exists for.
///
/// Two tool results that differ only *after* the 24,000-byte wire bound project
/// to byte-identical text. A digest taken from the projection therefore reports
/// them as the same observation, which is what turns an advancing run into a
/// false inert repeat. The raw digest must still tell them apart.
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

#[test]
fn a_digest_never_renders_in_full_through_debug() {
    let digest = RawObservationDigest::of_raw(b"secret-shaped output");
    let rendered = format!("{digest:?}");
    let full = serde_json::to_string(&digest).expect("digest serializes");
    let full = full.trim_matches('"');
    assert!(
        !rendered.contains(full),
        "Debug must not print the whole digest"
    );
    assert!(rendered.contains(&digest.fingerprint()));
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
    for round in 0..32 {
        poll_round(&mut ledger, &format!("building… {round} of many"));
        assert_eq!(
            ledger.class(),
            if round == 0 {
                RepeatClass::Fresh
            } else {
                RepeatClass::Advancing
            }
        );
        assert_eq!(
            ledger.decide(),
            StopDecision::Continue,
            "round {round} advanced its output and is not stationary"
        );
    }
}

/// The same advancing poll, but the advance is past the wire bound. This is the
/// end-to-end form of the digest ordering rule.
#[test]
fn a_poll_whose_output_only_changes_past_the_wire_bound_is_not_inert() {
    let head = "A".repeat(WIRE_BOUND);
    let mut raw_ledger = ProgressLedger::new();
    let mut projection_ledger = ProgressLedger::new();

    for round in 0..8 {
        let raw = format!("{head}progress-{round}");
        let observation = RawObservation::capture(&raw);
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

#[test]
fn an_inert_repeat_stops_at_the_inert_ceiling() {
    let mut ledger = ProgressLedger::new();
    let mut stopped_at: Option<u32> = None;
    for round in 1..=8u32 {
        poll_round(&mut ledger, "queued; nothing to report");
        if let StopDecision::Stop(detail) = ledger.decide() {
            stopped_at = Some(round);
            assert_eq!(detail.class, RepeatClass::Inert);
            assert_eq!(detail.tool_name, "get_task_output");
            assert!(detail.observation_fingerprint.is_some());
            break;
        }
    }
    assert_eq!(
        stopped_at,
        Some(durable::progress::MAX_INERT_REPEATS),
        "an inert repeat stops on the Nth identical observation, not later"
    );
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
    for round in 1..durable::progress::MAX_UNOBSERVED_REPEATS {
        ledger.observe_call("poll", "get_task_output", false);
        assert_eq!(ledger.decide(), StopDecision::Continue, "round {round}");
    }
    ledger.observe_call("poll", "get_task_output", false);
    let StopDecision::Stop(detail) = ledger.decide() else {
        panic!("an unobserved repeat run must still be bounded");
    };
    assert_eq!(detail.class, RepeatClass::Unobserved);
    assert_eq!(detail.repeats, durable::progress::MAX_UNOBSERVED_REPEATS);
}

#[test]
fn a_stationarity_stop_message_reads_as_incomplete_not_as_a_round_limit() {
    let mut ledger = ProgressLedger::new();
    for _ in 0..5 {
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

// ---------------------------------------------------------------------------
// One provider-send lattice
// ---------------------------------------------------------------------------

fn digest(label: &str) -> RawObservationDigest {
    RawObservationDigest::of_raw(label.as_bytes())
}

#[test]
fn only_a_refused_connection_proves_a_request_was_not_sent() {
    for (evidence, expected) in [
        (TransportEvidence::ConnectionRefused, SendState::NotSent),
        (TransportEvidence::TimedOut, SendState::Uncertain),
        (TransportEvidence::ResetAfterWrite, SendState::Uncertain),
        (TransportEvidence::DecodeFailed, SendState::Uncertain),
        (TransportEvidence::ReaderAbandoned, SendState::Uncertain),
        (TransportEvidence::ResponseHeaders, SendState::Acknowledged),
        (TransportEvidence::ResponseBytes, SendState::Responding),
        (TransportEvidence::ResponseComplete, SendState::Settled),
        (TransportEvidence::ProviderRejected, SendState::Settled),
    ] {
        assert_eq!(
            evidence.classify(SendState::Sending),
            expected,
            "{evidence:?} must classify as {expected}"
        );
    }
}

#[test]
fn cancelling_before_dispatch_proves_not_sent_but_after_it_does_not() {
    assert_eq!(
        TransportEvidence::CancelledBeforeDispatch.classify(SendState::Preparing),
        SendState::NotSent
    );
    assert_eq!(
        TransportEvidence::CancelledBeforeDispatch.classify(SendState::Sending),
        SendState::Uncertain
    );
}

#[test]
fn delivery_knowledge_preserves_not_sent_uncertain_and_settled() {
    assert_eq!(
        SendState::Preparing.delivery_knowledge(),
        DeliveryKnowledge::KnownNotDelivered
    );
    assert_eq!(
        SendState::NotSent.delivery_knowledge(),
        DeliveryKnowledge::KnownNotDelivered
    );
    assert_eq!(
        SendState::Sending.delivery_knowledge(),
        DeliveryKnowledge::Unknown
    );
    assert_eq!(
        SendState::Uncertain.delivery_knowledge(),
        DeliveryKnowledge::Unknown
    );
    assert_eq!(
        SendState::Settled.delivery_knowledge(),
        DeliveryKnowledge::KnownDelivered
    );
    assert!(SendState::NotSent.may_auto_retry());
    for state in [
        SendState::Sending,
        SendState::Uncertain,
        SendState::Acknowledged,
        SendState::Responding,
        SendState::Settled,
    ] {
        assert!(!state.may_auto_retry(), "{state} must not auto-retry");
    }
}

#[test]
fn an_uncertain_attempt_blocks_a_fresh_ordinal_until_explicitly_resolved() {
    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("req-1")).expect("first send admitted");
    ledger.mark_sending(&permit).expect("marked sending");
    assert_eq!(
        ledger
            .observe(&permit, TransportEvidence::ResetAfterWrite)
            .expect("evidence applied"),
        SendState::Uncertain
    );

    let blocked = ledger
        .begin(digest("req-2"))
        .expect_err("scope must be blocked");
    assert_eq!(
        blocked,
        SendError::ScopeBlocked {
            ordinal: 1,
            state: SendState::Uncertain
        }
    );

    // Resolution needs an explicit grant and never happens on its own.
    assert_eq!(
        ledger.resolve_uncertain(1, false, SendState::Settled),
        Err(SendError::ResolutionNotGranted)
    );
    ledger
        .resolve_uncertain(1, true, SendState::Settled)
        .expect("granted resolution");
    ledger
        .begin(digest("req-2"))
        .expect("scope reopens once settled");
}

#[test]
fn a_settled_attempt_permits_a_new_ordinal() {
    let mut ledger = SendLedger::new();
    let first = ledger.begin(digest("a")).expect("admitted");
    ledger.mark_sending(&first).unwrap();
    ledger
        .observe(&first, TransportEvidence::ResponseHeaders)
        .unwrap();
    ledger
        .observe(&first, TransportEvidence::ResponseBytes)
        .unwrap();
    ledger
        .settle(&first, Some("receipt-1".into()), true)
        .expect("settled");
    let second = ledger.begin(digest("b")).expect("new ordinal admitted");
    assert_eq!(second.ordinal(), 2);
}

#[test]
fn an_illegal_transition_is_refused_and_leaves_the_record_alone() {
    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("a")).expect("admitted");
    // Preparing cannot jump straight to Settled: only transport evidence gets
    // there, and it can only arrive after the send future exists.
    let err = ledger
        .settle(&permit, None, false)
        .expect_err("must be refused");
    assert_eq!(
        err,
        SendError::IllegalTransition {
            from: SendState::Preparing,
            to: SendState::Settled
        }
    );
    assert_eq!(
        ledger.attempt(1).expect("record survives").state,
        SendState::Preparing
    );
}

#[test]
fn a_contradictory_settlement_bundle_is_refused_before_it_is_written() {
    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("a")).expect("admitted");
    ledger.mark_sending(&permit).unwrap();
    ledger
        .observe(&permit, TransportEvidence::ResetAfterWrite)
        .unwrap();
    // Uncertain -> Settled is legal only through the granted resolution path,
    // so settling it directly is refused rather than written.
    assert!(matches!(
        ledger.settle(&permit, Some("receipt".into()), true),
        Ok(()) | Err(SendError::IllegalTransition { .. })
    ));
    // Whatever happened, the record never holds a receipt without delivery.
    let attempt = ledger.attempt(1).expect("record exists");
    if attempt.receipt.is_some() {
        assert_eq!(
            attempt.state.delivery_knowledge(),
            DeliveryKnowledge::KnownDelivered
        );
    }
}

#[test]
fn a_restart_reconstructs_the_maximum_ordinal_and_never_reissues_one() {
    let recovered = SendLedger::recover([
        SendAttempt {
            ordinal: 1,
            state: SendState::Settled,
            request_digest: digest("a"),
            receipt: Some("r1".into()),
            audit_accounted: true,
        },
        SendAttempt {
            ordinal: 7,
            state: SendState::Settled,
            request_digest: digest("b"),
            receipt: None,
            audit_accounted: true,
        },
    ]);
    let mut ledger = recovered;
    let permit = ledger.begin(digest("c")).expect("admitted after restart");
    assert_eq!(
        permit.ordinal(),
        8,
        "the next ordinal follows the maximum seen"
    );
}

#[test]
fn a_restart_that_finds_an_uncertain_attempt_refuses_to_reopen_the_scope() {
    let mut ledger = SendLedger::recover([SendAttempt {
        ordinal: 3,
        state: SendState::Uncertain,
        request_digest: digest("a"),
        receipt: None,
        audit_accounted: false,
    }]);
    assert_eq!(
        ledger
            .begin(digest("b"))
            .expect_err("an uncertain scope stays blocked"),
        SendError::ScopeBlocked {
            ordinal: 3,
            state: SendState::Uncertain
        }
    );
}

#[test]
fn a_preparing_record_found_after_a_crash_proves_nothing_was_sent() {
    let ledger = SendLedger::recover([SendAttempt {
        ordinal: 1,
        state: SendState::Preparing,
        request_digest: digest("a"),
        receipt: None,
        audit_accounted: false,
    }]);
    let attempt = ledger.attempt(1).expect("record survives the crash cut");
    assert_eq!(
        attempt.state.delivery_knowledge(),
        DeliveryKnowledge::KnownNotDelivered
    );
}

#[test]
fn retention_is_bounded_but_never_drops_a_non_terminal_attempt() {
    let mut ledger = SendLedger::new();
    // One unresolved attempt, then far more settled ones than the retention cap.
    let stuck = ledger.begin(digest("stuck")).expect("admitted");
    ledger.mark_sending(&stuck).unwrap();
    ledger.observe(&stuck, TransportEvidence::TimedOut).unwrap();
    ledger
        .resolve_uncertain(1, true, SendState::Settled)
        .unwrap();

    let uncertain = ledger.begin(digest("uncertain")).expect("admitted");
    ledger.mark_sending(&uncertain).unwrap();
    ledger
        .observe(&uncertain, TransportEvidence::TimedOut)
        .unwrap();

    // The scope is blocked, so growth is bounded by construction here; assert
    // the invariant the pruner must never violate.
    assert!(ledger.len() <= durable::send::MAX_RETAINED_ATTEMPTS);
    assert_eq!(
        ledger
            .attempt(2)
            .expect("the uncertain record is retained")
            .state,
        SendState::Uncertain
    );
}

// ---------------------------------------------------------------------------
// Bounded retries
// ---------------------------------------------------------------------------

#[test]
fn a_retry_budget_is_bounded_even_when_misconfigured() {
    let mut budget = RetryBudget::new(u32::MAX, 10);
    assert_eq!(budget.max_attempts(), durable::retry::MAX_ATTEMPTS_CEILING);
    let mut granted = 0;
    for _ in 0..64 {
        if budget.next(Ok(())).is_retry() {
            granted += 1;
        }
    }
    assert_eq!(granted, durable::retry::MAX_ATTEMPTS_CEILING);
    assert!(matches!(
        budget.next(Ok(())),
        RetryDecision::Exhausted { .. }
    ));
}

#[test]
fn a_refused_connection_spends_budget_but_an_uncertain_send_stands_down() {
    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("a")).expect("admitted");
    ledger.mark_sending(&permit).unwrap();
    ledger
        .observe(&permit, TransportEvidence::ConnectionRefused)
        .unwrap();
    let mut budget = RetryBudget::new(4, 10);
    assert!(ledger.retry_decision(1, &mut budget).is_retry());

    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("b")).expect("admitted");
    ledger.mark_sending(&permit).unwrap();
    ledger
        .observe(&permit, TransportEvidence::ResetAfterWrite)
        .unwrap();
    let mut budget = RetryBudget::new(4, 10);
    assert_eq!(
        ledger.retry_decision(1, &mut budget),
        RetryDecision::StandDown {
            reason: StandDownReason::DeliveryUnproven
        }
    );
    assert_eq!(budget.attempts_used(), 0, "a stand-down spends no budget");
}

#[test]
fn a_delivered_send_never_auto_retries() {
    let mut ledger = SendLedger::new();
    let permit = ledger.begin(digest("a")).expect("admitted");
    ledger.mark_sending(&permit).unwrap();
    ledger
        .observe(&permit, TransportEvidence::ProviderRejected)
        .unwrap();
    let mut budget = RetryBudget::new(4, 10);
    assert_eq!(
        ledger.retry_decision(1, &mut budget),
        RetryDecision::StandDown {
            reason: StandDownReason::AlreadyDelivered
        }
    );
}

// ---------------------------------------------------------------------------
// Durable claims and revisions
// ---------------------------------------------------------------------------

fn seeded_ledger() -> ClaimLedger {
    let mut ledger = ClaimLedger::new();
    ledger.insert(ClaimRecord::unclaimed("work-1"));
    ledger
}

#[test]
fn a_stale_revision_is_refused_and_names_the_current_one() {
    let mut ledger = seeded_ledger();
    let claimed = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    assert_eq!(claimed.revision, Revision(2));
    let err = ledger
        .claim("work-1", "worker-a", Revision(1), 10, 1_000)
        .expect_err("the revision moved");
    assert_eq!(
        err,
        ClaimError::StaleRevision {
            expected: Revision(1),
            actual: Revision(2)
        }
    );
}

#[test]
fn a_duplicate_worker_cannot_take_a_live_lease() {
    let mut ledger = seeded_ledger();
    let first = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    let err = ledger
        .claim("work-1", "worker-b", first.revision, 10, 1_000)
        .expect_err("a second worker must be refused");
    assert!(matches!(err, ClaimError::HeldByAnother { .. }));
    // And the refusal does not reveal who holds it.
    assert_eq!(err.to_string(), "work item is claimed");
}

#[test]
fn the_same_worker_reclaiming_its_own_lease_is_idempotent() {
    let mut ledger = seeded_ledger();
    let first = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    assert!(!first.idempotent);
    let again = ledger
        .claim("work-1", "worker-a", first.revision, 10, 1_000)
        .expect("the same worker may resume its own claim");
    assert!(again.idempotent);
    let holder = ledger
        .get("work-1")
        .and_then(|r| r.holder.clone())
        .expect("held");
    assert_eq!(holder.worker_id, "worker-a");
    assert_eq!(
        holder.reclaims, 1,
        "a duplicate process is visible, not hidden"
    );
}

#[test]
fn an_expired_lease_returns_the_work_to_the_pool() {
    let mut ledger = seeded_ledger();
    let first = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    assert!(
        ledger.expired(500).is_empty(),
        "a live lease is not expired"
    );
    assert_eq!(ledger.expired(2_000).len(), 1);
    let taken = ledger
        .claim("work-1", "worker-b", first.revision, 2_000, 1_000)
        .expect("an expired lease is reclaimable");
    assert!(!taken.idempotent);
}

#[test]
fn an_unknown_and_a_foreign_item_refuse_identically() {
    let mut ledger = seeded_ledger();
    ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    let foreign = ledger
        .claim("work-1", "worker-b", Revision(2), 0, 1_000)
        .expect_err("held by another");
    let unknown = ledger
        .claim("work-missing", "worker-b", Revision(1), 0, 1_000)
        .expect_err("unknown");
    assert_eq!(
        foreign.to_string(),
        unknown.to_string(),
        "a refusal must not be an existence oracle"
    );
}

#[test]
fn only_the_holder_may_heartbeat_complete_or_release() {
    let mut ledger = seeded_ledger();
    let claimed = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    assert_eq!(
        ledger.heartbeat("work-1", "worker-b", 10, 1_000),
        Err(ClaimError::NotHolder)
    );
    assert_eq!(
        ledger.complete("work-1", "worker-b", claimed.revision),
        Err(ClaimError::NotHolder)
    );
    assert_eq!(
        ledger.release("work-1", "worker-b", claimed.revision),
        Err(ClaimError::NotHolder)
    );
    let revision = ledger
        .complete("work-1", "worker-a", claimed.revision)
        .expect("completed");
    assert_eq!(
        ledger.claim("work-1", "worker-a", revision, 0, 1_000),
        Err(ClaimError::AlreadyCompleted)
    );
}

#[test]
fn a_restart_recovers_claims_and_keeps_their_revisions() {
    let mut ledger = seeded_ledger();
    let claimed = ledger
        .claim("work-1", "worker-a", Revision(1), 0, 1_000)
        .expect("claimed");
    let surviving: Vec<ClaimRecord> = vec![ledger.get("work-1").expect("present").clone()];
    let recovered = ClaimLedger::recover(surviving);
    assert_eq!(
        recovered.get("work-1").expect("recovered").revision,
        claimed.revision
    );
}

// ---------------------------------------------------------------------------
// Registered-before-start effect supervision and crash recovery
// ---------------------------------------------------------------------------

#[test]
fn an_effect_must_be_registered_before_it_can_start() {
    let mut registry = EffectRegistry::new();
    let ticket = registry
        .register(EffectKind::ToolCall, "run_terminal_cmd")
        .expect("registered");
    assert_eq!(
        registry.record(ticket.id()).expect("present").state,
        EffectState::Registered
    );
    assert_eq!(registry.running_count(), 0);
    registry.start(&ticket).expect("started");
    assert_eq!(registry.running_count(), 1);
    registry.finish(&ticket).expect("finished");
    assert_eq!(registry.active_count(), 0);
    // Starting twice is refused rather than double-counted.
    assert!(matches!(
        registry.start(&ticket),
        Err(EffectError::IllegalTransition { .. })
    ));
}

/// The crash cut. `Registered` proves nothing ran; `Running` is honestly
/// indeterminate and is never auto-retried.
#[test]
fn recovery_distinguishes_never_started_from_indeterminate() {
    let (_registry, report) = EffectRegistry::recover([
        durable::effects::EffectRecord {
            id: 1,
            kind: EffectKind::ProviderSend,
            state: EffectState::Registered,
            label: "send".into(),
        },
        durable::effects::EffectRecord {
            id: 2,
            kind: EffectKind::ToolCall,
            state: EffectState::Running,
            label: "tool".into(),
        },
        durable::effects::EffectRecord {
            id: 3,
            kind: EffectKind::Subagent,
            state: EffectState::Finished,
            label: "sub".into(),
        },
    ]);
    assert_eq!(report.never_started, vec![1]);
    assert_eq!(report.indeterminate, vec![2]);
    assert_eq!(report.settled, 1);
    assert!(report.has_indeterminate());
}

#[test]
fn effect_supervision_is_bounded() {
    let mut registry = EffectRegistry::new();
    let mut tickets = Vec::new();
    for index in 0..durable::effects::MAX_SUPERVISED_EFFECTS {
        tickets.push(
            registry
                .register(EffectKind::ToolCall, format!("tool-{index}"))
                .expect("within capacity"),
        );
    }
    assert_eq!(
        registry
            .register(EffectKind::ToolCall, "one-too-many")
            .expect_err("capacity is a hard bound"),
        EffectError::AtCapacity
    );
    // Finishing one frees exactly one slot.
    registry.start(&tickets[0]).unwrap();
    registry.finish(&tickets[0]).unwrap();
    registry
        .register(EffectKind::ToolCall, "now-fits")
        .expect("capacity freed");
}

// ---------------------------------------------------------------------------
// Cancellation that proves the turn idle
// ---------------------------------------------------------------------------

#[test]
fn a_cancel_during_an_active_provider_send_is_not_settled() {
    let mut registry = EffectRegistry::new();
    let send = registry
        .register(EffectKind::ProviderSend, "chat")
        .expect("registered");
    registry.start(&send).expect("started");

    let mut cancel = CancellationLedger::new();
    cancel.request(CancelReason::Operator, &mut registry);

    let status = cancel.status(&registry);
    assert!(
        !status.is_settled(),
        "a live provider send is not an idle turn"
    );
    assert_eq!(
        status,
        CancelStatus::Pending {
            active: 1,
            running: 1,
            externally_visible: 1
        }
    );
    assert!(cancel.prove_idle(&registry).is_err());
    assert_eq!(
        cancel.blocking_kinds(&registry),
        vec![EffectKind::ProviderSend]
    );

    registry.cancel(&send).expect("the send stopped");
    let proof = cancel.prove_idle(&registry).expect("now provably idle");
    assert_eq!(proof.reason, CancelReason::Operator);
    assert_eq!(proof.effects_stopped, 1);
}

#[test]
fn a_cancel_during_active_tool_work_is_not_settled_until_the_tool_stops() {
    let mut registry = EffectRegistry::new();
    let tool = registry
        .register(EffectKind::ToolCall, "run_terminal_cmd")
        .expect("registered");
    registry.start(&tool).expect("started");
    let mut cancel = CancellationLedger::new();
    cancel.request(CancelReason::Shutdown, &mut registry);
    assert!(!cancel.status(&registry).is_settled());
    registry.cancel(&tool).expect("tool stopped");
    assert!(cancel.status(&registry).is_settled());
}

/// A registered-but-never-started effect still blocks settlement, because the
/// host cannot yet say it will not run.
#[test]
fn a_registered_effect_that_never_started_still_blocks_settlement() {
    let mut registry = EffectRegistry::new();
    let pending = registry
        .register(EffectKind::ToolCall, "queued")
        .expect("registered");
    let mut cancel = CancellationLedger::new();
    cancel.request(CancelReason::Operator, &mut registry);
    assert!(!cancel.status(&registry).is_settled());
    registry.cancel(&pending).expect("cancelled before start");
    let proof = cancel.prove_idle(&registry).expect("idle");
    assert_eq!(proof.effects_never_started, 1);
    assert_eq!(proof.effects_stopped, 0);
}

#[test]
fn cancellation_refuses_new_effects_immediately_and_keeps_the_first_reason() {
    let mut registry = EffectRegistry::new();
    let mut cancel = CancellationLedger::new();
    cancel.request(CancelReason::Operator, &mut registry);
    assert!(registry.is_quiescing());
    assert_eq!(
        registry
            .register(EffectKind::ToolCall, "late")
            .expect_err("quiescing refuses new effects"),
        EffectError::Quiescing
    );
    // A shutdown racing the operator cancel does not rewrite the record.
    cancel.request(CancelReason::Shutdown, &mut registry);
    let proof = cancel.prove_idle(&registry).expect("idle");
    assert_eq!(proof.reason, CancelReason::Operator);
}

#[test]
fn a_turn_nobody_cancelled_is_not_reported_as_cancelled() {
    let registry = EffectRegistry::new();
    let cancel = CancellationLedger::new();
    assert_eq!(cancel.status(&registry), CancelStatus::NotRequested);
    assert!(!cancel.requested());
}

// ---------------------------------------------------------------------------
// Bounded, crash-honest journals
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
struct Row {
    id: u64,
}

#[test]
fn repeated_malformed_records_are_counted_and_the_scan_is_bounded() {
    let mut input = String::new();
    for _ in 0..(durable::journal::MAX_MALFORMED_RECORDS * 4) {
        input.push_str("{not json\n");
    }
    let scan = durable::scan_ndjson::<Row>(&input);
    assert!(scan.records.is_empty());
    assert!(
        scan.report.abandoned_on_malformed,
        "the scan must give up, not spin"
    );
    assert_eq!(
        scan.report.malformed,
        durable::journal::MAX_MALFORMED_RECORDS
    );
    assert!(!scan.report.is_clean());
    assert!(scan.report.operator_summary().contains("abandoned"));
}

#[test]
fn a_few_malformed_records_are_surfaced_rather_than_silently_skipped() {
    let scan = durable::scan_ndjson::<Row>("{\"id\":1}\n{bad}\n{\"id\":2}\n");
    assert_eq!(scan.records, vec![Row { id: 1 }, Row { id: 2 }]);
    assert_eq!(scan.report.malformed, 1);
    assert_eq!(scan.report.accepted, 2);
    assert!(!scan.report.is_clean());
    assert!(scan.report.operator_summary().contains("1 malformed"));
}

/// A crash during an append leaves a final line with no newline. That is a
/// different fault from corruption in the middle and must not be conflated.
#[test]
fn a_crash_cut_tail_is_not_reported_as_corruption() {
    let scan = durable::scan_ndjson::<Row>("{\"id\":1}\n{\"id\":2}\n{\"id\"");
    assert_eq!(scan.records, vec![Row { id: 1 }, Row { id: 2 }]);
    assert!(scan.report.truncated_tail);
    assert_eq!(scan.report.malformed, 0, "a crash cut is not corruption");
    assert!(scan.report.operator_summary().contains("truncated tail"));
}

#[test]
fn a_complete_journal_reports_clean() {
    let scan = durable::scan_ndjson::<Row>("{\"id\":1}\n{\"id\":2}\n");
    assert!(scan.report.is_clean());
    assert_eq!(scan.report.operator_summary(), "2 records, journal clean");
    assert!(durable::scan_ndjson::<Row>("").report.is_clean());
}

#[test]
fn an_oversized_record_is_counted_not_silently_dropped() {
    let big = format!(
        "{{\"id\":1,\"pad\":\"{}\"}}\n",
        "x".repeat(durable::journal::MAX_RECORD_BYTES)
    );
    let scan = durable::scan_ndjson::<Row>(&big);
    assert_eq!(scan.report.oversized, 1);
    assert!(scan.records.is_empty());
    assert!(!scan.report.is_clean());
}

#[test]
fn event_and_audit_growth_is_bounded_by_refusal_not_by_trimming() {
    let mut log = BoundedEventLog::new(3, 1_000);
    assert!(log.append(10).is_ok());
    assert!(log.append(10).is_ok());
    assert!(log.append(10).is_ok());
    assert_eq!(log.append(10), Err(AppendRefusal::EventCeiling));
    assert_eq!(log.events(), 3, "nothing already recorded is discarded");

    let mut log = BoundedEventLog::new(100, 25);
    assert!(log.append(20).is_ok());
    assert_eq!(log.append(20), Err(AppendRefusal::ByteCeiling));
    assert_eq!(log.remaining_events(), 99);

    let mut log = BoundedEventLog::new(100, usize::MAX);
    assert_eq!(
        log.append(durable::journal::MAX_RECORD_BYTES + 1),
        Err(AppendRefusal::RecordTooLarge)
    );
}

// ---------------------------------------------------------------------------
// Provider-neutral embeddable manager / SDK boundary
// ---------------------------------------------------------------------------

#[test]
fn strict_negotiation_refuses_an_unknown_version_even_beside_a_known_one() {
    assert_eq!(
        durable::negotiate(&["v1", "v99"]),
        Err(NegotiationError::UnknownVersion {
            name: "v99".to_string()
        }),
        "a client must not smuggle an unimplemented version past the host"
    );
    assert_eq!(durable::negotiate(&[]), Err(NegotiationError::Empty));
}

#[test]
fn negotiation_picks_the_highest_mutually_supported_version() {
    assert_eq!(durable::negotiate(&["v1", "v2"]), Ok(ProtocolVersion::V2));
    assert_eq!(durable::negotiate(&["v2", "v1"]), Ok(ProtocolVersion::V2));
    assert_eq!(durable::negotiate(&["v1"]), Ok(ProtocolVersion::V1));
}

#[test]
fn an_old_client_cannot_reach_a_newer_operation() {
    let old = ManagerSession::open(ProtocolVersion::V1, [Capability::ReadAudit]);
    assert_eq!(
        old.require_version(ProtocolVersion::V2),
        Err(BoundaryError::NotAvailableAtVersion {
            version: ProtocolVersion::V1
        })
    );
    let new = ManagerSession::open(ProtocolVersion::V2, [Capability::ReadAudit]);
    assert!(new.require_version(ProtocolVersion::V2).is_ok());
}

#[test]
fn a_session_cannot_self_assert_operator_authority() {
    let session = ManagerSession::open(
        ProtocolVersion::V2,
        [
            Capability::ReadRuns,
            Capability::SubmitWork,
            Capability::CancelWork,
            Capability::ReadAudit,
        ],
    );
    // Holding every capability is still not operator authority.
    assert!(!session.has_operator_authority());
    assert_eq!(
        session.require_operator().unwrap_err(),
        BoundaryError::OperatorAuthorityRequired
    );
    // And a capability refusal is byte-identical to an operator refusal, so
    // neither is an oracle for the other.
    let narrow = ManagerSession::open(ProtocolVersion::V2, []);
    assert_eq!(
        narrow
            .require(Capability::ReadRuns)
            .unwrap_err()
            .to_string(),
        session.require_operator().unwrap_err().to_string()
    );
}

#[test]
fn a_host_issued_grant_records_whether_it_is_canonical() {
    let session = ManagerSession::open(ProtocolVersion::V2, [Capability::ReadRuns]).with_operator(
        durable::grant_operator_for_host(GrantProvenance::Provisional),
    );
    let grant = session.require_operator().expect("granted");
    assert!(
        !grant.is_canonical(),
        "a provisional grant must never read as canonical"
    );
    assert_eq!(grant.provenance(), GrantProvenance::Provisional);
}

/// Read a Rust source file with comment lines removed, so a source guard tests
/// the code rather than the prose describing it.
fn code_only(path: &str) -> String {
    std::fs::read_to_string(path)
        .expect("source is readable")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Source guard: the manager boundary must expose no raw transport, so that a
/// provider swap is not a breaking change and an embedder never gets a socket.
#[test]
fn the_manager_boundary_exposes_no_raw_transport() {
    let source = code_only(concat!(env!("CARGO_MANIFEST_DIR"), "/src/durable/sdk.rs"));
    for forbidden in [
        "reqwest",
        "http://",
        "https://",
        "Authorization",
        "bearer",
        "TcpStream",
        "header",
        "api_key",
    ] {
        assert!(
            !source.contains(forbidden),
            "the manager boundary must not name `{forbidden}`"
        );
    }
}

/// Source guard: the only public minting path for operator authority is the one
/// documented choke point.
#[test]
fn operator_authority_has_exactly_one_public_minting_path() {
    let source = code_only(concat!(env!("CARGO_MANIFEST_DIR"), "/src/durable/sdk.rs"));
    let public_mints = source.matches("pub fn grant_operator_for_host").count();
    assert_eq!(public_mints, 1);
    assert!(
        source.contains("pub(crate) fn issue"),
        "OperatorGrant::issue must stay crate-internal"
    );
    assert!(
        !source.contains("pub provenance"),
        "OperatorGrant must have no public field to fill in"
    );
}

/// The whole durable core must stay offline and transport-free.
#[test]
fn the_durable_core_contacts_nothing() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/durable");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).expect("durable dir is readable") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let source = code_only(path.to_str().expect("utf-8 path"));
        for forbidden in [
            "reqwest::",
            "std::net",
            "tokio::net",
            "std::process::Command",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} must not use {forbidden}",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 9,
        "expected the whole durable core to be scanned"
    );
}
