//! Durable provider-authority ledger: denial boundaries, crash/restart
//! recovery, and replay refusal.
//!
//! Every test here is provider-free. No network, no credentials, no gateway:
//! the ledger is exercised purely through its durable records, and "crashes"
//! are deterministic file-level fakes plus a reopen.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use grokptah_agent_bridge::orchestration::{OrchError, OrchErrorCode};
use grokptah_agent_bridge::{
    confirmation_nonce_digest, new_confirmation_nonce, provider_request_fingerprint,
    safe_id_filename, AgentModelSpec, CancelDisposition, CredentialMethodClass,
    FollowUpDisposition, ProviderAttemptRecord, ProviderAttemptRequest, ProviderAuthorityBinding,
    ProviderAuthorityLedger, ProviderAuthorityScope, ProviderContinuationIntent,
    ProviderRepositoryBinding, ProviderRequestIdentity, ProviderRouteClass, ProviderSendState,
    ProviderSettledOutcome, ProviderUncertaintyReason, ProviderUncertaintyResolution,
    ProviderUnknown, RunStopCause, DEFAULT_BINDING_TTL_MS, DEFAULT_GRANT_TTL_MS,
};
use tempfile::TempDir;
use uuid::Uuid;

const PAYLOAD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn at(offset_seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_760_000_000 + offset_seconds, 0).unwrap()
}

fn scope() -> ProviderAuthorityScope {
    ProviderAuthorityScope {
        owner_principal_id: "account-alpha".into(),
        tenant_id: "tenant-alpha".into(),
        installation_id: "installation-1".into(),
        agent_id: "agent-1".into(),
        run_id: "run-1".into(),
        lane_id: Uuid::from_u128(7),
        agent_spec_revision: 3,
        model: AgentModelSpec::from_selection_key("grok-4-fast").unwrap(),
        route_class: ProviderRouteClass::GrokBuildProxy,
        endpoint_fingerprint: "a".repeat(64),
        credential_method: CredentialMethodClass::GrokBuildOidc,
        credential_binding_digest: "b".repeat(64),
        repository: ProviderRepositoryBinding {
            workspace: "/srv/workspaces/alpha".into(),
            repository_ref: "refs/heads/main".into(),
            policy_digest: "c".repeat(64),
        },
    }
}

fn binding(
    scope: &ProviderAuthorityScope,
    continuation_key: &str,
    ordinal: u32,
    now: DateTime<Utc>,
) -> ProviderAuthorityBinding {
    let fingerprint = provider_request_fingerprint(scope, continuation_key, ordinal, PAYLOAD);
    ProviderAuthorityBinding::bind(
        scope.clone(),
        fingerprint,
        continuation_key,
        now,
        Duration::milliseconds(DEFAULT_BINDING_TTL_MS),
    )
    .unwrap()
}

fn request(
    scope: &ProviderAuthorityScope,
    attempt_id: &str,
    continuation_key: &str,
    ordinal: u32,
    now: DateTime<Utc>,
) -> ProviderAttemptRequest {
    ProviderAttemptRequest {
        attempt_id: attempt_id.into(),
        binding: binding(scope, continuation_key, ordinal, now),
        request: ProviderRequestIdentity::new(format!("client-{attempt_id}")),
        intent: ProviderContinuationIntent {
            follow_up: FollowUpDisposition::ContinueRun,
            ..ProviderContinuationIntent::default()
        },
    }
}

fn denial(error: &OrchError) -> String {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("denial"))
        .and_then(|value| value.as_str())
        .unwrap_or("<none>")
        .to_string()
}

/// Admit an attempt and take it all the way through transport.
fn sent_attempt(
    ledger: &ProviderAuthorityLedger,
    authority: &ProviderAuthorityScope,
    attempt_id: &str,
    now: DateTime<Utc>,
) -> ProviderAttemptRecord {
    ledger
        .begin_attempt(
            authority,
            request(authority, attempt_id, "round-1", 1, now),
            now,
        )
        .unwrap();
    let nonce = new_confirmation_nonce();
    let grant = ledger
        .issue_confirmation_grant(
            authority,
            attempt_id,
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            now,
        )
        .unwrap();
    ledger
        .begin_transport(authority, attempt_id, &grant.grant_id, &nonce, now)
        .unwrap()
}

fn attempt_file(root: &Path, run_id: &str, attempt_id: &str) -> PathBuf {
    root.join("provider-authority")
        .join("attempts")
        .join(safe_id_filename(run_id).unwrap())
        .join(format!("{}.json", safe_id_filename(attempt_id).unwrap()))
}

fn open(root: &TempDir) -> ProviderAuthorityLedger {
    ProviderAuthorityLedger::open(root.path()).unwrap()
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test]
fn attempt_is_durable_before_transport_and_settles_with_a_full_receipt() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();

    let admitted = ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();

    // Intent and request identity are on disk before anything is sent.
    assert_eq!(admitted.send_state, ProviderSendState::KnownNotSent);
    assert_eq!(admitted.intent.follow_up, FollowUpDisposition::ContinueRun);
    assert_eq!(admitted.request.client_request_id, "client-attempt-1");
    assert!(admitted.request.provider_request_id.is_none());
    assert!(admitted.auto_retry_allowed());

    let nonce = new_confirmation_nonce();
    let grant = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(1),
        )
        .unwrap();
    assert_eq!(grant.nonce_digest, confirmation_nonce_digest(&nonce));
    assert!(!grant.is_consumed());

    let sending = ledger
        .begin_transport(&authority, "attempt-1", &grant.grant_id, &nonce, at(2))
        .unwrap();
    assert_eq!(sending.send_state, ProviderSendState::Sending);
    assert!(!sending.auto_retry_allowed());
    assert_eq!(
        sending.authorizing_grant_id.as_deref(),
        Some(grant.grant_id.as_str())
    );

    ledger
        .record_provider_request_id(&authority, "attempt-1", "provider-req-9", at(3))
        .unwrap();
    let settled = ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::Delivered,
            at(4),
        )
        .unwrap();

    assert_eq!(settled.send_state, ProviderSendState::Settled);
    assert!(settled.unknowns().is_empty());

    let receipts = ledger.receipts(&authority).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.attempt_id, "attempt-1");
    assert_eq!(
        receipt.provider_request_id.as_deref(),
        Some("provider-req-9")
    );
    assert_eq!(receipt.send_state, ProviderSendState::Settled);
    assert!(receipt.confirmed);
    assert_eq!(receipt.authority.agent_spec_revision, 3);
    assert_eq!(receipt.authority.repository_ref, "refs/heads/main");
    assert!(receipt.unknowns.is_empty());
}

// ---------------------------------------------------------------------------
// Binding denial boundaries
// ---------------------------------------------------------------------------

#[test]
fn admission_denies_mismatched_stale_and_replayed_bindings() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();

    // Cross-tenant binding.
    let mut other_tenant = authority.clone();
    other_tenant.tenant_id = "tenant-beta".into();
    let error = ledger
        .begin_attempt(
            &authority,
            request(&other_tenant, "attempt-tenant", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "tenant_mismatch");
    assert_eq!(error.code, OrchErrorCode::ForbiddenScope);

    // Cross-repository binding.
    let mut other_repo = authority.clone();
    other_repo.repository.workspace = "/srv/workspaces/beta".into();
    let error = ledger
        .begin_attempt(
            &authority,
            request(&other_repo, "attempt-repo", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "repository_mismatch");
    assert_eq!(error.code, OrchErrorCode::WorkspaceMismatch);

    // Superseded specification revision.
    let mut stale = authority.clone();
    stale.agent_spec_revision = 2;
    let error = ledger
        .begin_attempt(
            &authority,
            request(&stale, "attempt-stale", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "binding_stale");

    // Expired binding.
    let expired = request(&authority, "attempt-expired", "round-1", 1, at(0));
    let error = ledger
        .begin_attempt(&authority, expired, at(DEFAULT_BINDING_TTL_MS / 1000 + 1))
        .unwrap_err();
    assert_eq!(denial(&error), "binding_stale");

    // Nothing above was admitted.
    assert!(ledger.list_attempts(&authority).unwrap().is_empty());
}

#[test]
fn a_replayed_request_fingerprint_is_refused() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();

    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::AbandonedBeforeSend,
            at(1),
        )
        .unwrap();

    // Same continuation key and ordinal → same fingerprint → replay.
    let error = ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-2", "round-1", 1, at(2)),
            at(2),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "binding_replayed");
    assert_eq!(error.code, OrchErrorCode::Conflict);

    // A legitimate re-issue re-fingerprints on the next ordinal.
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-2", "round-1", 2, at(3)),
            at(3),
        )
        .unwrap();
    assert_eq!(ledger.list_attempts(&authority).unwrap().len(), 2);
}

#[test]
fn a_replayed_fingerprint_cannot_cross_into_another_run() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    let admitted = request(&authority, "attempt-1", "round-1", 1, at(0));
    let fingerprint = admitted.binding.request_fingerprint.clone();
    ledger.begin_attempt(&authority, admitted, at(0)).unwrap();

    let mut second_run = scope();
    second_run.run_id = "run-2".into();
    let mut crafted = request(&second_run, "attempt-1", "round-1", 1, at(1));
    crafted.binding = ProviderAuthorityBinding::bind(
        second_run.clone(),
        fingerprint,
        "round-1",
        at(1),
        Duration::milliseconds(DEFAULT_BINDING_TTL_MS),
    )
    .unwrap();

    let error = ledger
        .begin_attempt(&second_run, crafted, at(1))
        .unwrap_err();
    assert_eq!(denial(&error), "binding_replayed");
}

#[test]
fn a_duplicate_attempt_id_is_refused() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let error = ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-2", 1, at(1)),
            at(1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "attempt_already_exists");
}

// ---------------------------------------------------------------------------
// Confirmation grant boundaries
// ---------------------------------------------------------------------------

#[test]
fn a_confirmation_grant_is_single_use() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let nonce = new_confirmation_nonce();
    let grant = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(0),
        )
        .unwrap();
    ledger
        .begin_transport(&authority, "attempt-1", &grant.grant_id, &nonce, at(1))
        .unwrap();

    let error = ledger
        .begin_transport(&authority, "attempt-1", &grant.grant_id, &nonce, at(2))
        .unwrap_err();
    // The lattice refuses a second send before the grant is even inspected.
    assert_eq!(denial(&error), "send_state_transition_invalid");

    // And the grant itself is durably spent for any other attempt as well.
    assert!(ledger
        .load_grant(&authority, &grant.grant_id)
        .unwrap()
        .unwrap()
        .is_consumed());
}

#[test]
fn a_grant_minted_for_another_attempt_is_refused() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    for (attempt, key) in [("attempt-1", "round-1"), ("attempt-2", "round-2")] {
        ledger
            .begin_attempt(
                &authority,
                request(&authority, attempt, key, 1, at(0)),
                at(0),
            )
            .unwrap();
    }
    let nonce = new_confirmation_nonce();
    let grant = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(0),
        )
        .unwrap();
    let error = ledger
        .begin_transport(&authority, "attempt-2", &grant.grant_id, &nonce, at(1))
        .unwrap_err();
    assert_eq!(denial(&error), "grant_subject_mismatch");
    // The refused grant is still unspent for its own subject.
    assert!(!ledger
        .load_grant(&authority, &grant.grant_id)
        .unwrap()
        .unwrap()
        .is_consumed());
}

#[test]
fn a_grant_whose_audience_no_longer_matches_the_binding_is_refused() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let nonce = new_confirmation_nonce();
    let mut grant = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(0),
        )
        .unwrap();

    // Rewrite the durable grant with an audience from a different binding.
    grant.audience = binding(&authority, "round-9", 4, at(0)).binding_digest();
    let grant_path = root
        .path()
        .join("provider-authority")
        .join("grants")
        .join(safe_id_filename("run-1").unwrap())
        .join(format!(
            "{}.json",
            safe_id_filename(&grant.grant_id).unwrap()
        ));
    fs::write(&grant_path, serde_json::to_vec_pretty(&grant).unwrap()).unwrap();

    let error = ledger
        .begin_transport(&authority, "attempt-1", &grant.grant_id, &nonce, at(1))
        .unwrap_err();
    assert_eq!(denial(&error), "grant_audience_mismatch");
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::KnownNotSent
    );
}

#[test]
fn expired_grants_and_wrong_nonces_are_refused() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let nonce = new_confirmation_nonce();
    let grant = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            &nonce,
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(0),
        )
        .unwrap();

    let wrong = new_confirmation_nonce();
    let error = ledger
        .begin_transport(&authority, "attempt-1", &grant.grant_id, &wrong, at(1))
        .unwrap_err();
    assert_eq!(denial(&error), "grant_nonce_mismatch");

    let error = ledger
        .begin_transport(
            &authority,
            "attempt-1",
            &grant.grant_id,
            &nonce,
            at(DEFAULT_GRANT_TTL_MS / 1000 + 1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "grant_expired");

    let error = ledger
        .begin_transport(
            &authority,
            "attempt-1",
            "grant-that-never-existed",
            &nonce,
            at(1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "grant_missing");

    // None of the refusals moved the attempt or spent the grant.
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::KnownNotSent
    );
    assert!(!ledger
        .load_grant(&authority, &grant.grant_id)
        .unwrap()
        .unwrap()
        .is_consumed());
}

#[test]
fn a_low_entropy_nonce_is_refused_at_issue_time() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let error = ledger
        .issue_confirmation_grant(
            &authority,
            "attempt-1",
            "host",
            "short",
            Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
            at(0),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "grant_nonce_mismatch");
}

// ---------------------------------------------------------------------------
// Crash / restart
// ---------------------------------------------------------------------------

#[test]
fn a_restart_during_transport_makes_the_attempt_uncertain() {
    let root = TempDir::new().unwrap();
    let authority = scope();
    {
        let ledger = open(&root);
        let sending = sent_attempt(&ledger, &authority, "attempt-1", at(0));
        assert_eq!(sending.send_state, ProviderSendState::Sending);
    }

    // Restart.
    let ledger = open(&root);
    let recovered = ledger.load_attempt(&authority, "attempt-1").unwrap();
    assert_eq!(recovered.send_state, ProviderSendState::Uncertain);
    assert_eq!(
        recovered.uncertainty_reason,
        Some(ProviderUncertaintyReason::RestartDuringTransport)
    );
    assert!(!recovered.auto_retry_allowed());
    assert!(recovered.unknowns().contains(&ProviderUnknown::Delivery));

    // Recovery is idempotent across further restarts.
    drop(ledger);
    let ledger = open(&root);
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::Uncertain
    );
}

#[test]
fn a_restart_before_transport_leaves_the_attempt_retryable() {
    let root = TempDir::new().unwrap();
    let authority = scope();
    {
        let ledger = open(&root);
        ledger
            .begin_attempt(
                &authority,
                request(&authority, "attempt-1", "round-1", 1, at(0)),
                at(0),
            )
            .unwrap();
    }
    let ledger = open(&root);
    let recovered = ledger.load_attempt(&authority, "attempt-1").unwrap();
    assert_eq!(recovered.send_state, ProviderSendState::KnownNotSent);
    assert!(recovered.auto_retry_allowed());
    assert!(!recovered.unknowns().contains(&ProviderUnknown::Delivery));
}

#[test]
fn a_grant_consumed_before_a_crash_stays_spent() {
    let root = TempDir::new().unwrap();
    let authority = scope();
    let nonce = new_confirmation_nonce();
    let grant_id = {
        let ledger = open(&root);
        ledger
            .begin_attempt(
                &authority,
                request(&authority, "attempt-1", "round-1", 1, at(0)),
                at(0),
            )
            .unwrap();
        let grant = ledger
            .issue_confirmation_grant(
                &authority,
                "attempt-1",
                "host",
                &nonce,
                Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
                at(0),
            )
            .unwrap();
        ledger
            .begin_transport(&authority, "attempt-1", &grant.grant_id, &nonce, at(1))
            .unwrap();

        // Deterministic crash fake: the grant consumption reached disk but the
        // attempt transition did not. Roll the attempt file back to its
        // pre-transport bytes, exactly as a torn write would leave it.
        let path = attempt_file(root.path(), "run-1", "attempt-1");
        let mut torn: ProviderAttemptRecord =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        torn.send_state = ProviderSendState::KnownNotSent;
        torn.authorizing_grant_id = None;
        torn.transitions.clear();
        fs::write(&path, serde_json::to_vec_pretty(&torn).unwrap()).unwrap();
        grant.grant_id
    };

    // Restart. The attempt looks unsent, but the grant is durably spent, so
    // the transport boundary refuses rather than sending twice on one
    // confirmation.
    let ledger = open(&root);
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::KnownNotSent
    );
    let error = ledger
        .begin_transport(&authority, "attempt-1", &grant_id, &nonce, at(2))
        .unwrap_err();
    assert_eq!(denial(&error), "grant_already_consumed");
    assert_eq!(error.code, OrchErrorCode::Conflict);
}

#[test]
fn a_corrupted_attempt_record_fails_closed_instead_of_reading_as_absent() {
    let root = TempDir::new().unwrap();
    let authority = scope();
    let ledger = open(&root);
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();

    let path = attempt_file(root.path(), "run-1", "attempt-1");
    fs::write(&path, b"{ not json").unwrap();

    let error = ledger.load_attempt(&authority, "attempt-1").unwrap_err();
    assert_eq!(error.code, OrchErrorCode::Internal);
    assert!(ledger.list_attempts(&authority).is_err());
}

// ---------------------------------------------------------------------------
// Uncertainty is never auto-retried
// ---------------------------------------------------------------------------

#[test]
fn an_uncertain_attempt_blocks_a_new_attempt_on_the_same_continuation_key() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));
    ledger
        .mark_uncertain(
            &authority,
            "attempt-1",
            ProviderUncertaintyReason::TransportInterrupted,
            at(1),
        )
        .unwrap();

    // A retry re-fingerprints, so the replay guard would let it through. The
    // continuation key is what actually refuses it.
    let error = ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-2", "round-1", 2, at(2)),
            at(2),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "uncertain_attempt_not_retryable");
    assert_eq!(error.code, OrchErrorCode::Conflict);

    // A different logical request is unaffected.
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-3", "round-2", 1, at(3)),
            at(3),
        )
        .unwrap();
}

#[test]
fn a_live_attempt_blocks_a_second_attempt_on_the_same_continuation_key() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let error = ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-2", "round-1", 2, at(1)),
            at(1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "continuation_key_busy");

    // Settling the holder releases the key.
    ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::AbandonedBeforeSend,
            at(2),
        )
        .unwrap();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-2", "round-1", 2, at(3)),
            at(3),
        )
        .unwrap();
}

#[test]
fn an_uncertain_attempt_only_leaves_by_explicit_reconciliation() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));
    ledger
        .mark_uncertain(
            &authority,
            "attempt-1",
            ProviderUncertaintyReason::DeadlineElapsed,
            at(1),
        )
        .unwrap();

    // The ordinary settle path refuses.
    let error = ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::Delivered,
            at(2),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "uncertain_attempt_not_retryable");

    // So does an ordinary settle dressed up as a reconciliation.
    let error = ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::ReconciledDelivered,
            at(2),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");

    // Explicit reconciliation with recorded evidence is the only exit.
    let resolved = ledger
        .resolve_uncertain(
            &authority,
            "attempt-1",
            ProviderUncertaintyResolution {
                outcome: ProviderSettledOutcome::ReconciledDelivered,
                provider_request_id: Some("provider-req-42".into()),
                evidence_code: "provider_usage_ledger_match".into(),
                resolved_by: "operator".into(),
            },
            at(3),
        )
        .unwrap();
    assert_eq!(resolved.send_state, ProviderSendState::Settled);
    assert_eq!(
        resolved.settled_outcome,
        Some(ProviderSettledOutcome::ReconciledDelivered)
    );
    assert_eq!(
        resolved.request.provider_request_id.as_deref(),
        Some("provider-req-42")
    );
    assert!(resolved.unknowns().is_empty());

    // Reconciling twice is refused.
    assert_eq!(
        denial(
            &ledger
                .resolve_uncertain(
                    &authority,
                    "attempt-1",
                    ProviderUncertaintyResolution {
                        outcome: ProviderSettledOutcome::ReconciledNotDelivered,
                        provider_request_id: None,
                        evidence_code: "second_pass".into(),
                        resolved_by: "operator".into(),
                    },
                    at(4),
                )
                .unwrap_err()
        ),
        "send_state_transition_invalid"
    );
}

#[test]
fn an_unsent_attempt_cannot_settle_as_delivered() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let error = ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::Delivered,
            at(1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");

    ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::AbandonedBeforeSend,
            at(1),
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// Cancellation intent
// ---------------------------------------------------------------------------

#[test]
fn a_durable_cancel_intent_stops_transport_across_restart() {
    let root = TempDir::new().unwrap();
    let authority = scope();
    let nonce = new_confirmation_nonce();
    let grant_id = {
        let ledger = open(&root);
        ledger
            .begin_attempt(
                &authority,
                request(&authority, "attempt-1", "round-1", 1, at(0)),
                at(0),
            )
            .unwrap();
        let grant = ledger
            .issue_confirmation_grant(
                &authority,
                "attempt-1",
                "host",
                &nonce,
                Duration::milliseconds(DEFAULT_GRANT_TTL_MS),
                at(0),
            )
            .unwrap();
        let cancelled = ledger
            .record_cancel_intent(&authority, "attempt-1", RunStopCause::Cancelled, at(1))
            .unwrap();
        assert_eq!(cancelled.intent.cancel, CancelDisposition::Requested);
        assert_eq!(cancelled.intent.cancel_requested_at, Some(at(1)));
        grant.grant_id
    };

    let ledger = open(&root);
    let error = ledger
        .begin_transport(&authority, "attempt-1", &grant_id, &nonce, at(2))
        .unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");

    // The grant survives unspent; the attempt is still safely unsent.
    assert!(!ledger
        .load_grant(&authority, &grant_id)
        .unwrap()
        .unwrap()
        .is_consumed());
    let record = ledger.load_attempt(&authority, "attempt-1").unwrap();
    assert_eq!(record.send_state, ProviderSendState::KnownNotSent);
    assert_eq!(
        record.intent.cancel_stop_cause,
        Some(RunStopCause::Cancelled)
    );

    ledger
        .acknowledge_cancel(&authority, "attempt-1", at(3))
        .unwrap();
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .intent
            .cancel,
        CancelDisposition::Acknowledged
    );
}

// ---------------------------------------------------------------------------
// Cross-tenant / cross-repository control
// ---------------------------------------------------------------------------

#[test]
fn another_tenant_cannot_read_or_control_an_attempt() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));

    let mut intruder = scope();
    intruder.tenant_id = "tenant-beta".into();
    assert_eq!(
        denial(&ledger.load_attempt(&intruder, "attempt-1").unwrap_err()),
        "tenant_mismatch"
    );
    assert_eq!(
        denial(
            &ledger
                .settle_attempt(
                    &intruder,
                    "attempt-1",
                    ProviderSettledOutcome::Delivered,
                    at(1)
                )
                .unwrap_err()
        ),
        "tenant_mismatch"
    );
    assert_eq!(
        denial(
            &ledger
                .mark_uncertain(
                    &intruder,
                    "attempt-1",
                    ProviderUncertaintyReason::DeadlineElapsed,
                    at(1)
                )
                .unwrap_err()
        ),
        "tenant_mismatch"
    );
    assert_eq!(
        denial(
            &ledger
                .record_cancel_intent(&intruder, "attempt-1", RunStopCause::Cancelled, at(1))
                .unwrap_err()
        ),
        "tenant_mismatch"
    );
    assert!(ledger.list_attempts(&intruder).unwrap().is_empty());
    assert!(ledger.receipts(&intruder).unwrap().is_empty());

    // The owner is unaffected.
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::Sending
    );
}

#[test]
fn another_repository_cannot_control_an_attempt() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));

    let mut other_repo = scope();
    other_repo.repository.workspace = "/srv/workspaces/beta".into();
    let error = ledger.load_attempt(&other_repo, "attempt-1").unwrap_err();
    assert_eq!(denial(&error), "repository_mismatch");
    assert_eq!(error.code, OrchErrorCode::WorkspaceMismatch);
}

#[test]
fn another_agent_in_the_same_tenant_cannot_control_an_attempt() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));

    let mut sibling = scope();
    sibling.agent_id = "agent-2".into();
    assert_eq!(
        denial(&ledger.load_attempt(&sibling, "attempt-1").unwrap_err()),
        "binding_mismatch"
    );
}

#[test]
fn a_later_specification_revision_can_still_settle_a_live_attempt() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));

    // The Agent specification was revised while the request was in flight.
    let mut revised = scope();
    revised.agent_spec_revision = 4;
    let settled = ledger
        .settle_attempt(
            &revised,
            "attempt-1",
            ProviderSettledOutcome::Delivered,
            at(1),
        )
        .unwrap();
    assert_eq!(settled.send_state, ProviderSendState::Settled);
    // But it cannot admit a new attempt under the superseded binding.
    assert_eq!(
        denial(
            &ledger
                .begin_attempt(
                    &revised,
                    request(&authority, "attempt-2", "round-2", 1, at(2)),
                    at(2)
                )
                .unwrap_err()
        ),
        "binding_stale"
    );
}

#[test]
fn an_unknown_attempt_is_denied_rather_than_invented() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    assert_eq!(
        denial(
            &ledger
                .load_attempt(&authority, "attempt-missing")
                .unwrap_err()
        ),
        "attempt_unknown"
    );
}

#[test]
fn a_sent_attempt_cannot_settle_as_never_sent() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));
    let error = ledger
        .settle_attempt(
            &authority,
            "attempt-1",
            ProviderSettledOutcome::AbandonedBeforeSend,
            at(1),
        )
        .unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");
    assert_eq!(
        ledger
            .load_attempt(&authority, "attempt-1")
            .unwrap()
            .send_state,
        ProviderSendState::Sending
    );
}

#[test]
fn an_attempt_cannot_be_admitted_with_a_provider_request_identity() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    let mut forged = request(&authority, "attempt-1", "round-1", 1, at(0));
    forged.request.provider_request_id = Some("provider-req-1".into());
    let error = ledger.begin_attempt(&authority, forged, at(0)).unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");
    assert!(ledger.list_attempts(&authority).unwrap().is_empty());
}

#[test]
fn an_attempt_cannot_be_admitted_already_cancelled() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    let mut forged = request(&authority, "attempt-1", "round-1", 1, at(0));
    forged.intent.cancel = CancelDisposition::Requested;
    forged.intent.cancel_requested_at = Some(at(0));
    let error = ledger.begin_attempt(&authority, forged, at(0)).unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");
    assert!(ledger.list_attempts(&authority).unwrap().is_empty());
}

#[test]
fn a_provider_request_identity_cannot_exist_before_transport() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    ledger
        .begin_attempt(
            &authority,
            request(&authority, "attempt-1", "round-1", 1, at(0)),
            at(0),
        )
        .unwrap();
    let error = ledger
        .record_provider_request_id(&authority, "attempt-1", "provider-req-1", at(1))
        .unwrap_err();
    assert_eq!(denial(&error), "send_state_transition_invalid");
    assert!(ledger
        .load_attempt(&authority, "attempt-1")
        .unwrap()
        .request
        .provider_request_id
        .is_none());
}

#[test]
fn a_provider_request_identity_cannot_be_rebound() {
    let root = TempDir::new().unwrap();
    let ledger = open(&root);
    let authority = scope();
    sent_attempt(&ledger, &authority, "attempt-1", at(0));
    ledger
        .record_provider_request_id(&authority, "attempt-1", "provider-req-1", at(1))
        .unwrap();
    // Repeating the same identity is idempotent.
    ledger
        .record_provider_request_id(&authority, "attempt-1", "provider-req-1", at(2))
        .unwrap();
    let error = ledger
        .record_provider_request_id(&authority, "attempt-1", "provider-req-2", at(3))
        .unwrap_err();
    assert_eq!(denial(&error), "binding_mismatch");
}
