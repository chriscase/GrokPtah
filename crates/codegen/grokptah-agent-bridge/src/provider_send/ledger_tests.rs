//! Ledger behaviour: ordinal monotonicity, admission, CAS, recovery, takeover.

use super::*;
use crate::provider_send::dialect::WireDialect;
use crate::provider_send::identity::{
    CallSiteFamily, RequestDigest, RouteIncarnation, SendOrigin, SendScope,
};
use crate::provider_send::record::{
    AccountingRecord, AuditOutcome, CancellationRecord, ReceiptRecord, SettlementOutcome,
};
use crate::provider_send::seams::{
    AuditGeneration, CapabilityGeneration, LifecycleGeneration, PrincipalGeneration,
    QueueOwnershipGeneration,
};
use crate::provider_send::state::{HostFailureClass, UncertaintyClass};

fn scope(session: &str) -> SendScope {
    SendScope::new(
        "/workspace",
        session,
        None,
        SendOrigin::Desktop,
        CallSiteFamily::DesktopChatTurn,
    )
    .expect("scope")
}

fn spec_for(session: &str, body: &str) -> AttemptBindingSpec {
    AttemptBindingSpec {
        scope: scope(session),
        principal: PrincipalGeneration::provisional(&["principal"]),
        capability: CapabilityGeneration::provisional(&["capability"]),
        lifecycle: LifecycleGeneration::provisional(&["lifecycle"]),
        queue: QueueOwnershipGeneration::provisional(&["queue"]),
        audit: AuditGeneration::provisional(&["audit"]),
        route: RouteIncarnation::new(
            "https://gateway.invalid/v1",
            "model-a",
            WireDialect::OpenAiChatCompletions,
            "gateway_api_key",
            None,
        ),
        request_digest: RequestDigest::of_body(body.as_bytes()),
    }
}

fn completed() -> Settlement {
    Settlement {
        outcome: SettlementOutcome::Completed,
        cancellation: CancellationRecord::NotRequested,
        receipt: ReceiptRecord {
            provider_receipt: None,
            status: Some(200),
        },
        accounting: AccountingRecord {
            request_bytes: 12,
            response_bytes: 34,
            ..AccountingRecord::default()
        },
        audit: AuditOutcome::Accounted,
        settled_at: Utc::now(),
        uncertainty: None,
    }
}

fn uncertain() -> Settlement {
    Settlement {
        outcome: SettlementOutcome::Uncertain,
        cancellation: CancellationRecord::NotRequested,
        receipt: ReceiptRecord::default(),
        accounting: AccountingRecord {
            request_bytes: 12,
            ..AccountingRecord::default()
        },
        audit: AuditOutcome::Unresolved,
        settled_at: Utc::now(),
        uncertainty: Some(UncertaintyClass::Timeout),
    }
}

fn ledger(dir: &tempfile::TempDir, name: &str) -> AttemptLedger {
    AttemptLedger::open_as(dir.path(), HostIncarnationId::from_raw(name)).expect("open")
}

#[test]
fn ordinals_are_monotonic_within_a_scope() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    for expected in 1..=3u64 {
        let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
        assert_eq!(handle.ordinal(), expected);
        store.mark_sending(&mut handle).expect("sending");
        store.settle(&mut handle, completed()).expect("settle");
    }
    assert_eq!(store.max_ordinal(&scope("s")).expect("max"), Some(3));
}

#[test]
fn different_scopes_have_independent_ordinal_sequences() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut first = store.begin_attempt(spec_for("s1", "b")).expect("begin");
    let mut second = store.begin_attempt(spec_for("s2", "b")).expect("begin");
    assert_eq!(first.ordinal(), 1);
    assert_eq!(second.ordinal(), 1);
    store.mark_sending(&mut first).expect("sending");
    store.settle(&mut first, completed()).expect("settle");
    store.mark_sending(&mut second).expect("sending");
    store.settle(&mut second, completed()).expect("settle");
}

#[test]
fn an_unsettled_attempt_blocks_a_new_ordinal() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    let refused = store.begin_attempt(spec_for("s", "b"));
    assert!(matches!(
        refused,
        Err(LedgerError::ScopeNotSettled {
            ordinal: 1,
            state: ProviderAttemptState::Sending
        })
    ));
}

#[test]
fn an_uncertain_attempt_never_silently_reopens_with_a_new_ordinal() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    store
        .apply_transport(
            &mut handle,
            TransportEvidence::PossibleWriteUnresolved {
                class: UncertaintyClass::Timeout,
            },
        )
        .expect("uncertain");
    assert_eq!(handle.state(), ProviderAttemptState::Uncertain);
    assert!(!handle.may_auto_retry());

    let refused = store.begin_attempt(spec_for("s", "b"));
    assert!(matches!(
        refused,
        Err(LedgerError::ScopeNotSettled {
            state: ProviderAttemptState::Uncertain,
            ..
        })
    ));
}

#[test]
fn a_not_sent_attempt_frees_the_scope_for_a_retry() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    store
        .apply_transport(&mut handle, TransportEvidence::ConnectionNeverEstablished)
        .expect("not sent");
    assert_eq!(handle.state(), ProviderAttemptState::NotSent);
    assert!(handle.may_auto_retry());

    let next = store.begin_attempt(spec_for("s", "b")).expect("retry");
    assert_eq!(next.ordinal(), 2);
}

#[test]
fn host_evidence_is_the_only_way_out_of_preparing_without_the_wire() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store
        .mark_not_sent(
            &mut handle,
            HostEvidence::OwnerObservedBeforeDispatch {
                detail: HostFailureClass::RequestSerialization,
            },
        )
        .expect("not sent");
    assert_eq!(handle.state(), ProviderAttemptState::NotSent);
}

#[test]
fn sending_cannot_be_talked_back_into_not_sent() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    let refused = store.mark_not_sent(
        &mut handle,
        HostEvidence::OwnerObservedBeforeDispatch {
            detail: HostFailureClass::CancelledBeforeDispatch,
        },
    );
    assert!(matches!(
        refused,
        Err(LedgerError::IllegalTransition {
            from: ProviderAttemptState::Sending,
            to: ProviderAttemptState::NotSent
        })
    ));
}

#[test]
fn restart_reconstructs_max_ordinal_and_exact_identity() {
    let dir = tempfile::tempdir().expect("tmp");
    let first = ledger(&dir, "host-1");
    let mut handle = first.begin_attempt(spec_for("s", "b")).expect("begin");
    first.mark_sending(&mut handle).expect("sending");
    first.settle(&mut handle, completed()).expect("settle");
    let expected_key = handle.binding().host_idempotency().key().clone();
    let expected_ordinal = handle.ordinal();
    drop(first);

    let restarted = ledger(&dir, "host-2");
    assert_eq!(
        restarted.max_ordinal(&scope("s")).expect("max"),
        Some(expected_ordinal)
    );
    let reloaded = restarted
        .load(&scope("s"), expected_ordinal)
        .expect("load")
        .expect("present");
    assert_eq!(
        reloaded.binding.host_idempotency().key(),
        &expected_key,
        "restart must recognise the exact prior identity"
    );
    assert_eq!(
        AttemptLedger::rederive_host_key(&spec_for("s", "b"), expected_ordinal),
        expected_key
    );
}

#[test]
fn recovery_resolves_preparing_to_not_sent_and_leaves_sending_uncertain() {
    let dir = tempfile::tempdir().expect("tmp");
    let dead = ledger(&dir, "dead-host");

    // Ordinal 1: crashed at Preparing.
    let preparing = dead.begin_attempt(spec_for("s", "b")).expect("begin");
    assert_eq!(preparing.state(), ProviderAttemptState::Preparing);
    drop(preparing);

    let live = ledger(&dir, "live-host");
    let report = live.recover_scope(&scope("s")).expect("recover");
    assert_eq!(report.max_ordinal, Some(1));
    assert_eq!(report.resolved_not_sent, vec![1]);
    assert!(report.left_uncertain.is_empty());

    // The freed scope may now admit a new ordinal.
    let mut next = live.begin_attempt(spec_for("s", "b")).expect("begin");
    assert_eq!(next.ordinal(), 2);
    live.mark_sending(&mut next).expect("sending");
    drop(next);

    // A second restart finds a Sending record and must leave it uncertain.
    let later = ledger(&dir, "later-host");
    let report = later.recover_scope(&scope("s")).expect("recover");
    assert_eq!(report.left_uncertain, vec![2]);
    assert_eq!(report.already_terminal, vec![1]);
    let recovered = later.load(&scope("s"), 2).expect("load").expect("present");
    assert_eq!(recovered.state, ProviderAttemptState::Uncertain);
    assert!(!recovered.may_auto_retry());

    // And the scope stays blocked until the uncertainty is resolved.
    assert!(matches!(
        later.begin_attempt(spec_for("s", "b")),
        Err(LedgerError::ScopeNotSettled {
            state: ProviderAttemptState::Uncertain,
            ..
        })
    ));
}

#[test]
fn recovery_does_not_touch_the_running_incarnations_own_attempts() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");

    let report = store.recover_scope(&scope("s")).expect("recover");
    assert!(report.left_uncertain.is_empty());
    assert!(report.resolved_not_sent.is_empty());
    let reloaded = store.load(&scope("s"), 1).expect("load").expect("present");
    assert_eq!(reloaded.state, ProviderAttemptState::Sending);
}

#[test]
fn takeover_is_revision_cas_and_idempotent() {
    let dir = tempfile::tempdir().expect("tmp");
    let dead = ledger(&dir, "dead-host");
    let handle = dead.begin_attempt(spec_for("s", "b")).expect("begin");
    let before = handle.revision();
    drop(handle);

    let live = ledger(&dir, "live-host");
    assert_eq!(
        live.takeover(&scope("s"), 1).expect("takeover"),
        TakeoverOutcome::Claimed {
            state: ProviderAttemptState::Preparing
        }
    );
    let claimed = live.load(&scope("s"), 1).expect("load").expect("present");
    assert_eq!(claimed.owner, *live.incarnation());
    assert_eq!(claimed.revision, before + 1);

    // Idempotent: the second takeover writes nothing.
    assert_eq!(
        live.takeover(&scope("s"), 1).expect("takeover"),
        TakeoverOutcome::AlreadyOwned {
            state: ProviderAttemptState::Preparing
        }
    );
    let again = live.load(&scope("s"), 1).expect("load").expect("present");
    assert_eq!(again.revision, before + 1);
}

#[test]
fn a_stale_handle_loses_the_compare_and_swap() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");

    // Another incarnation advances the same record underneath us.
    let other = ledger(&dir, "host-2");
    other.takeover(&scope("s"), 1).expect("takeover");

    let conflict = store.mark_sending(&mut handle);
    assert!(matches!(
        conflict,
        Err(LedgerError::RevisionConflict { .. })
    ));
}

#[test]
fn settlement_lands_whole_or_not_at_all() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    store
        .apply_transport(
            &mut handle,
            TransportEvidence::ResponseComplete {
                status: 200,
                bytes: 34,
            },
        )
        .expect("settled");
    store.settle(&mut handle, completed()).expect("settle");

    let stored = store.load(&scope("s"), 1).expect("load").expect("present");
    let settlement = stored.settlement.expect("settlement present");
    assert_eq!(settlement.outcome, SettlementOutcome::Completed);
    assert_eq!(settlement.audit, AuditOutcome::Accounted);
    assert_eq!(settlement.receipt.status, Some(200));
    assert_eq!(settlement.accounting.response_bytes, 34);
    settlement.validate().expect("consistent");
}

#[test]
fn a_contradictory_settlement_is_refused_before_it_is_written() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    store
        .apply_transport(&mut handle, TransportEvidence::ConnectionNeverEstablished)
        .expect("not sent");

    let mut bad = completed();
    bad.outcome = SettlementOutcome::NotSent;
    bad.receipt.status = Some(200);
    assert!(matches!(
        store.settle(&mut handle, bad),
        Err(LedgerError::Contradiction(_))
    ));
    let stored = store.load(&scope("s"), 1).expect("load").expect("present");
    assert!(stored.settlement.is_none(), "nothing partial may land");
}

#[test]
fn resolving_uncertainty_requires_a_matching_grant() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let mut handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    store.mark_sending(&mut handle).expect("sending");
    store
        .apply_transport(
            &mut handle,
            TransportEvidence::PossibleWriteUnresolved {
                class: UncertaintyClass::UnexpectedEof,
            },
        )
        .expect("uncertain");
    store.settle(&mut handle, uncertain()).expect("settle");

    let wrong = ReconciliationGrant::provisional(
        &["operator"],
        ReconciliationResolution::ObservedNotDelivered,
    );
    assert!(matches!(
        store.resolve_uncertain(&scope("s"), 1, &wrong, completed()),
        Err(LedgerError::ResolutionRequiresGrant)
    ));

    let right = ReconciliationGrant::provisional(
        &["operator"],
        ReconciliationResolution::ObservedDelivered,
    );
    store
        .resolve_uncertain(&scope("s"), 1, &right, completed())
        .expect("resolve");
    let stored = store.load(&scope("s"), 1).expect("load").expect("present");
    assert_eq!(stored.state, ProviderAttemptState::Settled);
    assert!(matches!(
        stored.transitions.last().map(|t| &t.evidence),
        Some(TransitionEvidence::ReconciliationGrant { .. })
    ));

    // Now that the attempt is settled the scope admits the next ordinal.
    let next = store.begin_attempt(spec_for("s", "b")).expect("begin");
    assert_eq!(next.ordinal(), 2);
}

#[test]
fn two_processes_never_share_an_ordinal() {
    let dir = tempfile::tempdir().expect("tmp");
    let first = ledger(&dir, "host-1");
    let second = ledger(&dir, "host-2");

    let a = first.begin_attempt(spec_for("s", "b")).expect("begin");
    // The second process sees the first's unsettled attempt and stands down
    // rather than minting a duplicate ordinal.
    assert!(matches!(
        second.begin_attempt(spec_for("s", "b")),
        Err(LedgerError::ScopeNotSettled { ordinal: 1, .. })
    ));
    assert_eq!(a.ordinal(), 1);
}

#[test]
fn a_tampered_record_is_refused_rather_than_trusted() {
    let dir = tempfile::tempdir().expect("tmp");
    let store = ledger(&dir, "host-1");
    let handle = store.begin_attempt(spec_for("s", "b")).expect("begin");
    let path = handle.path.clone();
    drop(handle);

    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    raw["binding"]["ordinal"] = serde_json::json!(99);
    std::fs::write(&path, serde_json::to_vec(&raw).expect("encode")).expect("write");

    assert!(matches!(
        store.load(&scope("s"), 1),
        Err(LedgerError::BindingNotRederivable { .. })
    ));
}
