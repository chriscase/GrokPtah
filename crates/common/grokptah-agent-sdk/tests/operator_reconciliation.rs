//! Deterministic operator-reconciliation scenarios.
//!
//! Every case here drives the contract through the public API only, and every
//! clock value is an explicit constant. There is no wall-clock read, no
//! randomness, no filesystem, and no network, so a failure is always a real
//! behavioural change rather than a timing artifact.

use grokptah_agent_sdk::reconciliation::{
    AttemptObservation, AttemptOutcome, AttentionPolicy, AttentionReason, AuthorityBinding,
    EvidenceKind, EvidenceRecord, LeaseObservation, MAX_LEDGER_ENTRIES, OpaqueRef,
    OperatorIdentity, ProviderState, ReconcileAction, ReconcileErrorCode, ReconcileRequest,
    ReconciliationEntry, ReconciliationLedger, Redactor, RunConfidence, RunObservation,
    StreamObservation,
};
use grokptah_agent_sdk::run::{DurableRunState, RunScope};

const T0: u64 = 1_700_000_000_000;

fn opaque(value: &str) -> OpaqueRef {
    OpaqueRef::new(value).expect("fixture ref is opaque")
}

fn scope() -> RunScope {
    RunScope {
        session_id: "session-7f1c".into(),
        workspace: "approved-alias".into(),
        run_id: "run-4b21".into(),
    }
}

fn authority() -> OpaqueRef {
    opaque("authority-a1")
}

fn binding() -> AuthorityBinding {
    AuthorityBinding {
        authority_ref: authority(),
        session_id: scope().session_id,
        workspace: scope().workspace,
    }
}

fn operator(reference: &str) -> OperatorIdentity {
    OperatorIdentity {
        operator_ref: opaque(reference),
        authority_ref: authority(),
    }
}

fn ledger() -> ReconciliationLedger {
    ReconciliationLedger::new(scope(), authority()).expect("ledger opens")
}

/// A run whose worker crashed mid-attempt: the attempt outcome was never
/// durably recorded, the host restarted, and the lease has lapsed.
fn crash_cut_observation() -> RunObservation {
    RunObservation {
        run_ref: opaque("op-run-e4"),
        state: DurableRunState::Running,
        revision: 12,
        observed_seq: 88,
        observed_at_ms: T0,
        last_evidence_at_ms: Some(T0 - 600_000),
        deadline_at_ms: None,
        cancel_requested: false,
        lease: Some(LeaseObservation {
            holder_ref: opaque("worker-b7"),
            epoch: 4,
            expires_at_ms: T0 - 300_000,
            host_restarted: true,
        }),
        provider: Some(grokptah_agent_sdk::reconciliation::ProviderObservation {
            provider_run_ref: opaque("provider-c9"),
            state: ProviderState::Unknown,
        }),
        attempt: Some(AttemptObservation {
            attempt_ref: opaque("attempt-d2"),
            outcome: AttemptOutcome::Unknown,
        }),
        stream: StreamObservation {
            retained_from_seq: 1,
            retained_through_seq: 88,
            operator_cursor: Some(88),
        },
    }
}

fn evidence(summary: &str) -> Vec<EvidenceRecord> {
    vec![EvidenceRecord {
        kind: EvidenceKind::ProviderProjection,
        digest: "sha256:5f0c9a".into(),
        summary: summary.into(),
    }]
}

fn request(
    request_id: &str,
    action: ReconcileAction,
    expected_revision: u64,
    operator_ref: &str,
) -> ReconcileRequest {
    ReconcileRequest {
        request_id: request_id.into(),
        scope: scope(),
        expected_revision,
        action,
        evidence: evidence("provider console shows the attempt never started"),
        note: "closing out after the worker crash".into(),
        operator: operator(operator_ref),
    }
}

#[test]
fn a_synthetic_crash_cut_is_reported_as_uncertain_across_all_three_domains() {
    let observation = crash_cut_observation();
    let attention = grokptah_agent_sdk::reconciliation::project_attention(
        &observation,
        &AttentionPolicy::default(),
    )
    .expect("projects");

    assert!(attention.needs_attention);
    assert_eq!(attention.confidence, RunConfidence::Uncertain);
    assert_eq!(
        attention.reasons,
        vec![
            AttentionReason::UncertainOutcome,
            AttentionReason::CrashRecovered,
            AttentionReason::LeaseExpired,
            AttentionReason::ProviderAmbiguity,
            AttentionReason::StaleObservation,
        ]
    );
    // The operator can tell "the provider is ambiguous" from "our worker died".
    assert!(
        attention
            .has_domain(grokptah_agent_sdk::reconciliation::UncertaintyDomain::ModelOrProvider)
    );
    assert!(
        attention.has_domain(grokptah_agent_sdk::reconciliation::UncertaintyDomain::WorkerOrLease)
    );
    assert_eq!(attention.revision, 12);
}

#[test]
fn resolution_is_idempotent_and_replays_the_original_entry() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();
    let intent = request("req-1", ReconcileAction::ResolveFailed, 12, "operator-1");

    let first = ledger
        .apply(&intent, &observation, &policy, &redactor)
        .expect("first apply succeeds");
    assert!(first.is_new());
    assert_eq!(first.entry().seq, 1);
    assert_eq!(first.entry().resolved_state, Some(DurableRunState::Failed));
    // The verdict is what the operator now sees, at full confidence.
    assert_eq!(first.attention().state, DurableRunState::Failed);
    assert!(!first.attention().needs_attention);
    assert_eq!(first.attention().confidence, RunConfidence::Confirmed);

    let replay = ledger
        .apply(&intent, &observation, &policy, &redactor)
        .expect("replay succeeds");
    assert!(!replay.is_new());
    assert_eq!(replay.entry(), first.entry());
    // Exactly one durable entry exists after the retry.
    assert_eq!(ledger.audit().len(), 1);
    assert_eq!(ledger.next_seq(), 2);
}

#[test]
fn a_replayed_request_id_with_a_different_payload_is_a_conflict() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();

    ledger
        .apply(
            &request("req-1", ReconcileAction::RecordEvidence, 12, "operator-1"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("first apply succeeds");

    let mut mutated = request("req-1", ReconcileAction::ResolveFailed, 12, "operator-1");
    mutated.note = "a different intent under the same key".into();
    let error = ledger
        .apply(&mutated, &observation, &policy, &redactor)
        .expect_err("reused key with a new payload is rejected");
    assert_eq!(error.code, ReconcileErrorCode::Conflict);
    assert_eq!(ledger.audit().len(), 1);
}

#[test]
fn a_stale_revision_is_fenced_out() {
    let mut ledger = ledger();
    let mut observation = crash_cut_observation();
    observation.revision = 13;
    let error = ledger
        .apply(
            &request("req-1", ReconcileAction::ResolveFailed, 12, "operator-1"),
            &observation,
            &AttentionPolicy::default(),
            &Redactor::new(Vec::new()),
        )
        .expect_err("stale revision is rejected");
    assert_eq!(error.code, ReconcileErrorCode::StaleRevision);
    assert!(ledger.audit().is_empty());
}

#[test]
fn a_dropped_response_replays_instead_of_failing_the_revision_fence() {
    // The client's first call succeeded and bumped the revision, but the
    // response was lost. Its retry must not be punished for its own success.
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();
    let intent = request("req-1", ReconcileAction::ResolveFailed, 12, "operator-1");
    ledger
        .apply(&intent, &observation, &policy, &redactor)
        .expect("first apply succeeds");

    let mut moved_on = observation.clone();
    moved_on.revision = 13;
    let replay = ledger
        .apply(&intent, &moved_on, &policy, &redactor)
        .expect("retry after a revision bump still replays");
    assert!(!replay.is_new());
    assert_eq!(replay.entry().seq, 1);
}

#[test]
fn a_second_operator_reaching_a_verdict_is_told_rather_than_merged() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();

    ledger
        .apply(
            &request("req-a", ReconcileAction::ResolveFailed, 12, "operator-1"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("first operator resolves");

    let error = ledger
        .apply(
            &request("req-b", ReconcileAction::ResolveCompleted, 12, "operator-2"),
            &observation,
            &policy,
            &redactor,
        )
        .expect_err("second verdict is rejected");
    assert_eq!(error.code, ReconcileErrorCode::AlreadyResolved);

    // The losing operator can still attach what they saw; only the verdict is
    // closed, not the record.
    let recorded = ledger
        .apply(
            &request("req-c", ReconcileAction::RecordEvidence, 12, "operator-2"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("evidence from the second operator is still accepted");
    assert!(recorded.is_new());
    assert_eq!(ledger.audit().len(), 2);
    assert_eq!(ledger.resolution().map(|entry| entry.seq), Some(1));
}

#[test]
fn no_applied_entry_ever_carries_a_provider_mutation() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();

    for (index, action) in ReconcileAction::ALL.into_iter().enumerate() {
        // Only the first resolving action can win; the rest are recorded or
        // rejected. Either way none of them may touch a provider attempt.
        let intent = request(
            &format!("req-{index}"),
            action,
            observation.revision,
            "operator-1",
        );
        let outcome = ledger.apply(&intent, &observation, &policy, &redactor);
        assert!(!action.mutates_provider_attempt());
        if let Ok(outcome) = outcome {
            assert_eq!(outcome.entry().action, action);
            // The attempt projection is an input; nothing writes it back.
            assert_eq!(
                observation.attempt.as_ref().map(|attempt| attempt.outcome),
                Some(AttemptOutcome::Unknown)
            );
        }
    }
}

#[test]
fn restart_recovery_rebuilds_cursor_resolution_and_idempotency() {
    let mut original = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();
    original
        .apply(
            &request("req-1", ReconcileAction::RecordEvidence, 12, "operator-1"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("evidence entry");
    original
        .apply(
            &request("req-2", ReconcileAction::ResolveFailed, 12, "operator-1"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("resolution entry");

    // Simulate a process restart: only the durable entries survive.
    let durable = original.audit().to_vec();
    let mut recovered =
        ReconciliationLedger::recover(scope(), authority(), durable).expect("recovery succeeds");

    assert_eq!(recovered.next_seq(), 3);
    assert_eq!(recovered.resolution().map(|entry| entry.seq), Some(2));
    assert_eq!(
        recovered
            .project_with_resolution(&observation, &policy)
            .expect("projects")
            .state,
        DurableRunState::Failed
    );

    // The idempotency index survives too, so an in-flight retry still replays.
    let replay = recovered
        .apply(
            &request("req-2", ReconcileAction::ResolveFailed, 12, "operator-1"),
            &observation,
            &policy,
            &redactor,
        )
        .expect("retry after restart replays");
    assert!(!replay.is_new());
    assert_eq!(recovered.audit().len(), 2);
}

#[test]
fn recovery_fails_closed_on_a_torn_or_duplicated_journal() {
    let mut original = ledger();
    let observation = crash_cut_observation();
    original
        .apply(
            &request("req-1", ReconcileAction::RecordEvidence, 12, "operator-1"),
            &observation,
            &AttentionPolicy::default(),
            &Redactor::new(Vec::new()),
        )
        .expect("entry");
    let entry = original.audit()[0].clone();

    let reordered = vec![
        ReconciliationEntry {
            seq: 2,
            request_id: "req-2".into(),
            ..entry.clone()
        },
        ReconciliationEntry {
            seq: 1,
            ..entry.clone()
        },
    ];
    assert_eq!(
        ReconciliationLedger::recover(scope(), authority(), reordered)
            .expect_err("reordered journal is rejected")
            .code,
        ReconcileErrorCode::InvalidRequest
    );

    let duplicated = vec![
        entry.clone(),
        ReconciliationEntry {
            seq: 2,
            ..entry.clone()
        },
    ];
    assert_eq!(
        ReconciliationLedger::recover(scope(), authority(), duplicated)
            .expect_err("duplicated request id is rejected")
            .code,
        ReconcileErrorCode::Conflict
    );
}

#[test]
fn history_reports_a_pruned_span_instead_of_pretending_it_is_empty() {
    let entry = {
        let mut seed = ledger();
        seed.apply(
            &request(
                "req-seed",
                ReconcileAction::RecordEvidence,
                12,
                "operator-1",
            ),
            &crash_cut_observation(),
            &AttentionPolicy::default(),
            &Redactor::new(Vec::new()),
        )
        .expect("seed entry");
        seed.audit()[0].clone()
    };

    // A journal whose oldest retained entry is 40 has lost 1..=39.
    let retained = (40..=45)
        .map(|seq| ReconciliationEntry {
            seq,
            request_id: format!("req-{seq}"),
            ..entry.clone()
        })
        .collect::<Vec<_>>();
    let recovered =
        ReconciliationLedger::recover(scope(), authority(), retained).expect("recovery succeeds");

    let page = recovered
        .history(&binding(), Some(10), 64)
        .expect("history is readable");
    assert!(page.cursor_expired);
    assert_eq!(page.retained_from_seq, 40);
    assert_eq!(page.retained_through_seq, 45);
    assert_eq!(page.entries.first().map(|entry| entry.seq), Some(40));

    let fresh = recovered
        .history(&binding(), Some(41), 64)
        .expect("history is readable");
    assert!(!fresh.cursor_expired);
    assert_eq!(fresh.entries.first().map(|entry| entry.seq), Some(42));
}

#[test]
fn history_pages_are_bounded_and_chain_by_cursor() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();
    for index in 0..5 {
        ledger
            .apply(
                &request(
                    &format!("req-{index}"),
                    ReconcileAction::RecordEvidence,
                    12,
                    "operator-1",
                ),
                &observation,
                &policy,
                &redactor,
            )
            .expect("entry");
    }

    let first = ledger.history(&binding(), None, 2).expect("first page");
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.next_cursor, Some(2));

    let second = ledger
        .history(&binding(), first.next_cursor, 2)
        .expect("second page");
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );

    let last = ledger
        .history(&binding(), second.next_cursor, 2)
        .expect("last page");
    assert_eq!(last.entries.len(), 1);
    assert_eq!(last.next_cursor, None);

    // An out-of-range page size is clamped rather than honoured.
    let clamped = ledger
        .history(&binding(), None, usize::MAX)
        .expect("clamped");
    assert_eq!(clamped.entries.len(), 5);
}

#[test]
fn an_unbound_authority_cannot_distinguish_a_missing_run_from_a_forbidden_one() {
    let mut ledger = ledger();
    ledger
        .apply(
            &request("req-1", ReconcileAction::RecordEvidence, 12, "operator-1"),
            &crash_cut_observation(),
            &AttentionPolicy::default(),
            &Redactor::new(Vec::new()),
        )
        .expect("entry");

    let outsider = AuthorityBinding {
        authority_ref: opaque("authority-zz"),
        session_id: "session-other".into(),
        workspace: "other-alias".into(),
    };

    let forbidden = ledger
        .history(&outsider, None, 8)
        .expect_err("cross-authority history is refused");
    let missing = ledger
        .inspect(&binding(), 999)
        .expect_err("unknown sequence is refused");
    let forbidden_entry = ledger
        .inspect(&outsider, 1)
        .expect_err("cross-authority inspect is refused");

    assert_eq!(forbidden.code, ReconcileErrorCode::NotAvailable);
    // Byte-identical: an outsider learns nothing about existence either way.
    assert_eq!(missing, forbidden_entry);
    assert_eq!(
        serde_json::to_string(&missing).expect("serializes"),
        serde_json::to_string(&forbidden_entry).expect("serializes")
    );
}

#[test]
fn listing_drops_unbound_ledgers_without_disclosing_them() {
    let mine = ledger();
    let theirs = ReconciliationLedger::new(
        RunScope {
            session_id: "session-other".into(),
            workspace: "other-alias".into(),
            run_id: "run-9999".into(),
        },
        opaque("authority-zz"),
    )
    .expect("ledger opens");
    let observation = crash_cut_observation();

    let visible = grokptah_agent_sdk::reconciliation::list_attention(
        vec![(&mine, &observation), (&theirs, &observation)],
        &binding(),
        &AttentionPolicy::default(),
    )
    .expect("listing succeeds");
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].run_ref, observation.run_ref);
}

#[test]
fn a_note_is_redacted_before_it_reaches_the_durable_entry() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(vec!["hunter2token".into()]);
    let policy = AttentionPolicy::default();

    let mut leaky = request("req-1", ReconcileAction::RecordEvidence, 12, "operator-1");
    leaky.note = "operator pasted hunter2token from the console".into();
    let rejected = ledger
        .apply(&leaky, &observation, &policy, &redactor)
        .expect_err("an unredacted note is refused outright");
    assert_eq!(rejected.code, ReconcileErrorCode::InvalidRequest);

    let mut leaky_evidence = request("req-2", ReconcileAction::RecordEvidence, 12, "operator-1");
    leaky_evidence.evidence = vec![EvidenceRecord {
        kind: EvidenceKind::HostJournal,
        digest: "sha256:aa".into(),
        summary: "journal line with hunter2token".into(),
    }];
    assert_eq!(
        ledger
            .apply(&leaky_evidence, &observation, &policy, &redactor)
            .expect_err("unredacted evidence is refused")
            .code,
        ReconcileErrorCode::InvalidRequest
    );

    // A clean note survives, with control characters flattened.
    let mut clean = request("req-3", ReconcileAction::RecordEvidence, 12, "operator-1");
    clean.note = "checked the console".into();
    let applied = ledger
        .apply(&clean, &observation, &policy, &redactor)
        .expect("clean note is accepted");
    assert_eq!(applied.entry().note, "checked the console");
    assert!(
        ledger
            .audit()
            .iter()
            .all(|entry| !entry.note.contains("hunter2token"))
    );
}

#[test]
fn resolving_an_outcome_without_evidence_is_refused() {
    let mut ledger = ledger();
    let mut unsupported = request("req-1", ReconcileAction::ResolveCompleted, 12, "operator-1");
    unsupported.evidence.clear();
    assert_eq!(
        ledger
            .apply(
                &unsupported,
                &crash_cut_observation(),
                &AttentionPolicy::default(),
                &Redactor::new(Vec::new()),
            )
            .expect_err("a verdict needs evidence")
            .code,
        ReconcileErrorCode::InvalidRequest
    );

    // Acknowledging does not, because it asserts nothing.
    let mut acknowledged = request("req-2", ReconcileAction::Acknowledge, 12, "operator-1");
    acknowledged.evidence.clear();
    assert!(
        ledger
            .apply(
                &acknowledged,
                &crash_cut_observation(),
                &AttentionPolicy::default(),
                &Redactor::new(Vec::new()),
            )
            .is_ok()
    );
}

#[test]
fn evidence_and_ledger_growth_are_both_bounded() {
    let mut ledger = ledger();
    let observation = crash_cut_observation();
    let redactor = Redactor::new(Vec::new());
    let policy = AttentionPolicy::default();

    let mut oversized = request("req-big", ReconcileAction::RecordEvidence, 12, "operator-1");
    oversized.evidence = (0..64)
        .map(|index| EvidenceRecord {
            kind: EvidenceKind::OperatorStatement,
            digest: format!("sha256:{index:04x}"),
            summary: "bounded".into(),
        })
        .collect();
    assert_eq!(
        ledger
            .apply(&oversized, &observation, &policy, &redactor)
            .expect_err("evidence count is bounded")
            .code,
        ReconcileErrorCode::LimitReached
    );

    // Resolve first, then overflow: the retained window must slide without
    // ever evicting the verdict that keeps the run closed.
    ledger
        .apply(
            &request(
                "req-resolve",
                ReconcileAction::ResolveFailed,
                12,
                "operator-1",
            ),
            &observation,
            &policy,
            &redactor,
        )
        .expect("resolution entry");

    for index in 0..(MAX_LEDGER_ENTRIES + 8) {
        ledger
            .apply(
                &request(
                    &format!("req-fill-{index}"),
                    ReconcileAction::RecordEvidence,
                    12,
                    "operator-1",
                ),
                &observation,
                &policy,
                &redactor,
            )
            .expect("fill entry");
    }

    assert!(ledger.audit().len() <= MAX_LEDGER_ENTRIES);
    assert!(ledger.evicted_count() > 0);
    assert_eq!(ledger.resolution().map(|entry| entry.seq), Some(1));
    let page = ledger.history(&binding(), Some(1), 8).expect("history");
    assert!(page.cursor_expired);
}

#[test]
fn an_inverted_or_future_observation_is_rejected_before_projection() {
    let mut inverted = crash_cut_observation();
    inverted.stream = StreamObservation {
        retained_from_seq: 50,
        retained_through_seq: 10,
        operator_cursor: None,
    };
    assert!(
        grokptah_agent_sdk::reconciliation::project_attention(
            &inverted,
            &AttentionPolicy::default()
        )
        .is_err()
    );

    let mut future = crash_cut_observation();
    future.last_evidence_at_ms = Some(T0 + 1);
    assert!(
        grokptah_agent_sdk::reconciliation::project_attention(&future, &AttentionPolicy::default())
            .is_err()
    );
}

#[test]
fn the_projection_is_byte_reproducible_for_one_record() {
    let observation = crash_cut_observation();
    let policy = AttentionPolicy::default();
    let left = grokptah_agent_sdk::reconciliation::project_attention(&observation, &policy)
        .expect("projects");
    let right = grokptah_agent_sdk::reconciliation::project_attention(&observation, &policy)
        .expect("projects");
    assert_eq!(
        serde_json::to_string(&left).expect("serializes"),
        serde_json::to_string(&right).expect("serializes")
    );

    let encoded = serde_json::to_value(&left).expect("serializes");
    assert_eq!(encoded["contract"], "grokptah.operator-reconciliation.v1");
    assert_eq!(encoded["needsAttention"], true);
    assert_eq!(encoded["confidence"], "uncertain");
    assert_eq!(encoded["reasons"][0], "uncertain_outcome");
    assert_eq!(encoded["domains"][0], "model_or_provider");
    // Nothing in the operator projection carries a raw scoped identity.
    let serialized = encoded.to_string();
    for identity in [
        scope().session_id.as_str(),
        scope().workspace.as_str(),
        scope().run_id.as_str(),
    ] {
        assert!(
            !serialized.contains(identity),
            "projection leaked {identity}"
        );
    }
}

#[test]
fn a_hole_above_the_pinned_verdict_is_still_reported_as_a_gap() {
    // Retention pins the resolving entry, so the retained set can be
    // non-contiguous: seq 1 survives while 2..=39 are pruned. A window-start
    // comparison would call this contiguous; contiguity must be exact.
    let seed = {
        let mut ledger = ledger();
        ledger
            .apply(
                &request("req-seed", ReconcileAction::ResolveFailed, 12, "operator-1"),
                &crash_cut_observation(),
                &AttentionPolicy::default(),
                &Redactor::new(Vec::new()),
            )
            .expect("resolution entry");
        ledger.audit()[0].clone()
    };

    let mut retained = vec![seed.clone()];
    retained.extend((40..=42).map(|seq| ReconciliationEntry {
        seq,
        request_id: format!("req-{seq}"),
        resolved_state: None,
        ..seed.clone()
    }));
    let recovered =
        ReconciliationLedger::recover(scope(), authority(), retained).expect("recovery succeeds");

    let after_verdict = recovered
        .history(&binding(), Some(1), 8)
        .expect("history is readable");
    assert!(after_verdict.cursor_expired);
    assert_eq!(after_verdict.retained_from_seq, 1);
    assert_eq!(
        after_verdict
            .entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![40, 41, 42]
    );

    // Reading from inside the surviving tail is not a gap.
    let inside = recovered
        .history(&binding(), Some(40), 8)
        .expect("history is readable");
    assert!(!inside.cursor_expired);

    // A cursor at the head has lost nothing, even on a pruned ledger.
    let at_head = recovered
        .history(&binding(), Some(42), 8)
        .expect("history is readable");
    assert!(!at_head.cursor_expired);
    assert!(at_head.entries.is_empty());
}

#[test]
fn the_crash_cut_projection_matches_the_cross_language_golden_fixture() {
    // The desktop/CLI mirror in `desktop/src/lib/operatorReconciliation.ts`
    // parses this exact document in its own suite. Both sides asserting the
    // same literal is what keeps the two implementations of one contract from
    // drifting apart silently.
    let projected = serde_json::to_value(
        grokptah_agent_sdk::reconciliation::project_attention(
            &crash_cut_observation(),
            &AttentionPolicy::default(),
        )
        .expect("projects"),
    )
    .expect("serializes");

    assert_eq!(
        projected,
        serde_json::json!({
            "contract": "grokptah.operator-reconciliation.v1",
            "runRef": "op-run-e4",
            "state": "running",
            "confidence": "uncertain",
            "needsAttention": true,
            "reasons": [
                "uncertain_outcome",
                "crash_recovered",
                "lease_expired",
                "provider_ambiguity",
                "stale_observation"
            ],
            "severity": "blocking",
            "domains": ["model_or_provider", "worker_or_lease", "operator_decision"],
            "observedSeq": 88,
            "revision": 12
        })
    );
}
