//! Authority and reconciliation tests for durable provider operations.
//!
//! Every test here is deterministic and offline. The "provider" is a fake that
//! drives the same durable attempt lifecycle the real send engine drives
//! (admit → dispatch → observe), plus the crash it cannot control: a restart
//! that finds an attempt mid-flight. No network, no credentials, no real
//! provider payloads.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use grokptah_agent_bridge::orchestration::{
    attempt_id_for, AttemptSendState, AuthContext, AuthCredential, IdempotencyClaim, OrchErrorCode,
    OrchStore, OrchestrationConfig, OrchestrationService, PrincipalScope, ProviderAttempt,
    ReconcileRequest, RetentionPolicy, RunBounds, RunRecord, RunState, SettlementBinding,
    SettlementEvidence, SettlementOutcome, WorkspaceAllowlist, MAX_RECEIPT_PAGE,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, CONTROL_TOOLS};
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const OWNER: &str = "account-under-test";
const OPERATOR_TOKEN: &str = "operator-laptop";
const OTHER_TOKEN: &str = "other-desktop";

// ── fixtures ───────────────────────────────────────────────────────────

fn principal(token: &str, session_id: Uuid, workspace: &Path) -> PrincipalScope {
    PrincipalScope {
        owner_id: OWNER.into(),
        token_id: token.into(),
        session_id: Some(session_id),
        workspace: Some(workspace.display().to_string()),
    }
}

fn evidence(byte: char) -> SettlementEvidence {
    SettlementEvidence {
        kind: "provider_console_export".into(),
        digest: std::iter::repeat_n(byte, 64).collect(),
        // Fixed so an honest retry is byte-identical; the replay rule keys on
        // the proof's identity rather than when the operator looked at it.
        observed_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn run_record(run_id: &str, session_id: Uuid, workspace: &Path, token: &str) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        session_id,
        workspace: workspace.display().to_string(),
        request_id: format!("req-{run_id}"),
        client_id: Some(token.into()),
        owner_id: Some(OWNER.into()),
        state: RunState::Completed,
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
        bounds: RunBounds::default(),
        prompt_preview: "deterministic fixture".into(),
        start_seq: Some(1),
        end_seq: Some(2),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: Some("completed".into()),
        final_response: Some("done".into()),
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

/// A fake provider that drives the durable attempt lifecycle exactly as the
/// send engine does, and can be told to crash at a chosen point.
struct FakeProvider<'a> {
    store: &'a OrchStore,
    run_id: String,
    request_id: String,
    principal: PrincipalScope,
    ordinal: u32,
}

/// Where the fake provider stops, mirroring what a real crash can interrupt.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    /// Admitted but killed before anything reached the wire.
    BeforeDispatch,
    /// Dispatched, then killed before any outcome was observed.
    AfterDispatch,
    /// Ran to completion with an observed outcome.
    Never,
}

impl<'a> FakeProvider<'a> {
    fn new(store: &'a OrchStore, run: &RunRecord, token: &str) -> Self {
        Self {
            store,
            run_id: run.run_id.clone(),
            request_id: run.request_id.clone(),
            principal: principal(token, run.session_id, Path::new(&run.workspace)),
            ordinal: 0,
        }
    }

    fn attempt(&mut self, interrupt: Interrupt) -> ProviderAttempt {
        self.ordinal += 1;
        let attempt = self
            .store
            .begin_provider_attempt(
                &self.run_id,
                self.ordinal,
                &self.request_id,
                &self.principal,
            )
            .expect("admit provider attempt");
        if interrupt == Interrupt::BeforeDispatch {
            return attempt;
        }
        let attempt = self
            .store
            .mark_provider_attempt_sent(&attempt.attempt_id)
            .expect("dispatch provider attempt");
        if interrupt == Interrupt::AfterDispatch {
            return attempt;
        }
        self.store
            .resolve_provider_attempt(&attempt.attempt_id)
            .expect("observe provider outcome")
    }
}

fn store_at(root: &Path) -> OrchStore {
    OrchStore::open(root).expect("open orchestration store")
}

// ── receipts: cross-principal denial ───────────────────────────────────

#[test]
fn a_second_credential_can_never_replay_another_credentials_receipt() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());

    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let theirs = principal(OTHER_TOKEN, session, ws.path());

    assert!(matches!(
        store
            .claim_idempotency("ptah_submit_task", "req-1", "hash-1", &mine)
            .unwrap(),
        IdempotencyClaim::Perform
    ));
    store
        .complete_idempotency(
            "ptah_submit_task",
            "req-1",
            "hash-1",
            &mine,
            Some("run-1".into()),
            serde_json::json!({"secret": "only mine"}),
        )
        .unwrap();

    // `IdempotencyClaim` deliberately has no `Debug`: the replay arm carries a
    // caller's response payload, and a derived formatter is exactly how such a
    // payload ends up in a log line. Tests match it structurally, and take the
    // error side through `.err()` rather than `unwrap_err()`, which would want
    // that formatter back.

    // The owning credential replays its own outcome.
    match store
        .claim_idempotency("ptah_submit_task", "req-1", "hash-1", &mine)
        .unwrap()
    {
        IdempotencyClaim::Replay(Ok(value)) => assert_eq!(value["secret"], "only mine"),
        _ => panic!("owning credential must replay its own response"),
    }

    // A sibling credential of the same owner, presenting the identical
    // request_id and payload hash, gets a conflict rather than the response.
    let denied = store
        .claim_idempotency("ptah_submit_task", "req-1", "hash-1", &theirs)
        .err()
        .expect("a foreign credential must never replay");
    assert_eq!(denied.code, OrchErrorCode::Conflict);

    // And that answer is byte-identical to reusing your own request_id with a
    // different payload, so probing reveals nothing extra.
    let own_reuse = store
        .claim_idempotency("ptah_submit_task", "req-1", "different-hash", &mine)
        .err()
        .expect("payload reuse is a conflict");
    assert_eq!(denied.code, own_reuse.code);
    assert_eq!(denied.message, own_reuse.message);
}

#[test]
fn receipt_reads_report_another_credentials_receipt_as_absent() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let theirs = principal(OTHER_TOKEN, session, ws.path());

    store
        .claim_idempotency("ptah_submit_task", "req-1", "hash-1", &mine)
        .unwrap();
    store
        .complete_idempotency(
            "ptah_submit_task",
            "req-1",
            "hash-1",
            &mine,
            None,
            serde_json::json!({"ok": true}),
        )
        .unwrap();

    assert!(store.load_receipt_for("req-1", &mine).unwrap().is_some());
    assert!(
        store.load_receipt_for("req-1", &theirs).unwrap().is_none(),
        "a foreign credential must see the same absence as a nonexistent id"
    );
    assert!(store
        .load_receipt_for("never-claimed", &theirs)
        .unwrap()
        .is_none());
    assert!(store
        .list_receipts_for(&theirs, None, 50)
        .unwrap()
        .receipts
        .is_empty());
}

#[test]
fn receipt_reads_require_the_exact_session_and_workspace() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let other_ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());

    store
        .claim_idempotency("ptah_submit_task", "req-1", "hash-1", &mine)
        .unwrap();
    store
        .complete_idempotency(
            "ptah_submit_task",
            "req-1",
            "hash-1",
            &mine,
            None,
            serde_json::json!({"ok": true}),
        )
        .unwrap();

    let wrong_session = principal(OPERATOR_TOKEN, Uuid::new_v4(), ws.path());
    let wrong_workspace = principal(OPERATOR_TOKEN, session, other_ws.path());
    assert!(store
        .load_receipt_for("req-1", &wrong_session)
        .unwrap()
        .is_none());
    assert!(store
        .load_receipt_for("req-1", &wrong_workspace)
        .unwrap()
        .is_none());
    assert!(store.load_receipt_for("req-1", &mine).unwrap().is_some());
}

// ── receipts: bounded, deterministic pagination ────────────────────────

#[test]
fn receipt_listing_paginates_deterministically_and_bounds_the_page() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());

    for index in 0..25 {
        let request_id = format!("req-{index:03}");
        store
            .claim_idempotency("ptah_submit_task", &request_id, "hash", &mine)
            .unwrap();
        store
            .complete_idempotency(
                "ptah_submit_task",
                &request_id,
                "hash",
                &mine,
                None,
                serde_json::json!({"index": index}),
            )
            .unwrap();
    }

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let page = store
            .list_receipts_for(&mine, cursor.as_deref(), 10)
            .unwrap();
        assert!(page.receipts.len() <= 10, "page must respect its limit");
        assert!(
            !page.scan_truncated,
            "25 receipts is well inside the scan bound"
        );
        seen.extend(page.receipts.iter().map(|r| r.request_id.clone()));
        pages += 1;
        assert!(pages < 10, "pagination must terminate");
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    assert_eq!(seen.len(), 25, "every receipt appears exactly once");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        25,
        "pagination must not duplicate or skip rows"
    );

    // The same walk repeated yields the same order.
    let first = store.list_receipts_for(&mine, None, 10).unwrap();
    let again = store.list_receipts_for(&mine, None, 10).unwrap();
    assert_eq!(
        first
            .receipts
            .iter()
            .map(|r| &r.request_id)
            .collect::<Vec<_>>(),
        again
            .receipts
            .iter()
            .map(|r| &r.request_id)
            .collect::<Vec<_>>()
    );

    // An oversized limit is clamped to the hard bound rather than honoured.
    let huge = store.list_receipts_for(&mine, None, 100_000).unwrap();
    assert!(huge.receipts.len() <= MAX_RECEIPT_PAGE);
}

#[test]
fn a_cursor_carries_no_authority() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let theirs = principal(OTHER_TOKEN, session, ws.path());

    for index in 0..5 {
        let request_id = format!("req-{index}");
        store
            .claim_idempotency("ptah_submit_task", &request_id, "hash", &mine)
            .unwrap();
        store
            .complete_idempotency(
                "ptah_submit_task",
                &request_id,
                "hash",
                &mine,
                None,
                serde_json::json!({"index": index}),
            )
            .unwrap();
    }

    let page = store.list_receipts_for(&mine, None, 2).unwrap();
    let stolen = page.next_cursor.expect("more rows remain");
    let replayed = store.list_receipts_for(&theirs, Some(&stolen), 10).unwrap();
    assert!(
        replayed.receipts.is_empty(),
        "a cursor must not carry rows across principals"
    );

    let malformed = store.list_receipts_for(&mine, Some("not-a-cursor"), 10);
    assert_eq!(
        malformed.unwrap_err().code,
        OrchErrorCode::CursorExpired,
        "a malformed cursor is rejected, not silently treated as page one"
    );
}

// ── receipts: redaction ────────────────────────────────────────────────

#[test]
fn the_receipt_projection_withholds_payloads_paths_and_error_text() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());

    let secret_path = "/home/someone/.ssh/id_ed25519";
    let secret_prompt = "the user's private prompt text";
    store
        .claim_idempotency("ptah_submit_task", "req-ok", "hash", &mine)
        .unwrap();
    store
        .complete_idempotency(
            "ptah_submit_task",
            "req-ok",
            "hash",
            &mine,
            None,
            serde_json::json!({
                "promptPreview": secret_prompt,
                "resolvedPath": secret_path,
                "providerPayload": {"messages": [{"role": "user", "content": secret_prompt}]},
            }),
        )
        .unwrap();

    store
        .claim_idempotency("ptah_submit_task", "req-bad", "hash", &mine)
        .unwrap();
    store
        .fail_idempotency(
            "ptah_submit_task",
            "req-bad",
            "hash",
            &mine,
            None,
            grokptah_agent_bridge::orchestration::OrchError::new(
                OrchErrorCode::Conflict,
                format!("could not open {secret_path} for {secret_prompt}"),
            ),
        )
        .unwrap();

    let page = store.list_receipts_for(&mine, None, 50).unwrap();
    let rendered = serde_json::to_string(&page).unwrap();
    for leaked in [secret_path, secret_prompt, "providerPayload", "messages"] {
        assert!(
            !rendered.contains(leaked),
            "receipt projection leaked {leaked}: {rendered}"
        );
    }

    // What it does carry is identity, exact scope, status, and digests.
    let ok = page
        .receipts
        .iter()
        .find(|r| r.request_id == "req-ok")
        .expect("own receipt is listed");
    assert_eq!(ok.status, "complete");
    assert_eq!(ok.owner_id, OWNER);
    assert_eq!(ok.token_id, OPERATOR_TOKEN);
    assert_eq!(ok.session_id, Some(session));
    assert_eq!(ok.response_digest.len(), 64);

    let bad = page
        .receipts
        .iter()
        .find(|r| r.request_id == "req-bad")
        .expect("failed receipt is listed");
    assert_eq!(bad.status, "failed");
    assert_eq!(
        bad.error_code.as_deref(),
        Some("conflict"),
        "the failure class is surfaced; the assembled message is not"
    );
}

// ── provider attempts: restart recovery and takeover ───────────────────

#[test]
fn restart_recovery_separates_known_not_sent_from_uncertain() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let run = run_record("run-recovery", session, ws.path(), OPERATOR_TOKEN);

    let (never_sent, dispatched, observed) = {
        let store = store_at(home.path());
        store.save_run(&run).unwrap();
        let mut provider = FakeProvider::new(&store, &run, OPERATOR_TOKEN);
        let never_sent = provider.attempt(Interrupt::BeforeDispatch);
        let dispatched = provider.attempt(Interrupt::AfterDispatch);
        let observed = provider.attempt(Interrupt::Never);
        assert_eq!(never_sent.send_state, AttemptSendState::Preparing);
        assert_eq!(dispatched.send_state, AttemptSendState::Sent);
        assert_eq!(observed.send_state, AttemptSendState::Resolved);
        (never_sent, dispatched, observed)
    };

    // Reopening the store is the restart: recovery runs during open(). The
    // store takes an exclusive lock, so each restart releases the previous
    // handle first — exactly as a real process restart would.
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let reload = |attempt: &ProviderAttempt| {
        let store = store_at(home.path());
        store
            .load_provider_attempt_for(&attempt.attempt_id, &mine)
            .unwrap()
            .expect("attempt survives restart")
    };

    let never_sent = reload(&never_sent);
    assert_eq!(never_sent.send_state, AttemptSendState::NotSent);
    assert!(
        never_sent.permits_takeover(),
        "an attempt that provably never reached the wire is safe to retry"
    );

    let dispatched = reload(&dispatched);
    assert_eq!(dispatched.send_state, AttemptSendState::Uncertain);
    assert!(
        !dispatched.permits_takeover(),
        "a dispatched attempt with no observed outcome must refuse takeover"
    );
    assert!(dispatched.send_state.is_unsettled());

    let observed = reload(&observed);
    assert_eq!(
        observed.send_state,
        AttemptSendState::Resolved,
        "an observed outcome is never re-opened by a restart"
    );

    // Further restarts must not churn already-recovered records.
    let revision_before = dispatched.revision;
    for _ in 0..2 {
        let twice = reload(&dispatched);
        assert_eq!(twice.revision, revision_before, "recovery is idempotent");
        assert_eq!(twice.send_state, AttemptSendState::Uncertain);
    }
}

#[test]
fn unsettled_attempts_are_visible_with_their_exact_scope() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let run = run_record("run-visible", session, ws.path(), OPERATOR_TOKEN);
    {
        let store = store_at(home.path());
        store.save_run(&run).unwrap();
        let mut provider = FakeProvider::new(&store, &run, OPERATOR_TOKEN);
        provider.attempt(Interrupt::AfterDispatch);
        provider.attempt(Interrupt::Never);
    }

    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let page = store
        .list_provider_attempts_for(&mine, Some("run-visible"), true, None, 50)
        .unwrap();
    assert_eq!(
        page.attempts.len(),
        1,
        "only the uncertain attempt is unsettled"
    );
    let attempt = &page.attempts[0];
    assert_eq!(attempt.send_state, AttemptSendState::Uncertain);
    assert_eq!(attempt.run_id, "run-visible");
    assert_eq!(attempt.request_id, run.request_id);
    assert_eq!(attempt.scope.owner_id, OWNER);
    assert_eq!(attempt.scope.token_id, OPERATOR_TOKEN);
    assert_eq!(attempt.scope.session_id, Some(session));

    // The receipt read surfaces the same unresolved state as a count.
    store
        .claim_idempotency("ptah_submit_task", &run.request_id, "hash", &mine)
        .unwrap();
    store
        .complete_idempotency(
            "ptah_submit_task",
            &run.request_id,
            "hash",
            &mine,
            Some(run.run_id.clone()),
            serde_json::json!({"runId": run.run_id}),
        )
        .unwrap();
    let receipts = store.list_receipts_for(&mine, None, 50).unwrap();
    let row = receipts
        .receipts
        .iter()
        .find(|r| r.request_id == run.request_id)
        .unwrap();
    assert_eq!(
        row.unsettled_provider_attempts, 1,
        "a completed receipt must not read as the whole story while an attempt is unsettled"
    );

    // Another credential sees none of it.
    let theirs = principal(OTHER_TOKEN, session, ws.path());
    assert!(store
        .list_provider_attempts_for(&theirs, Some("run-visible"), true, None, 50)
        .unwrap()
        .attempts
        .is_empty());
    assert!(store
        .load_provider_attempt_for(&attempt.attempt_id, &theirs)
        .unwrap()
        .is_none());
}

// ── settlement ─────────────────────────────────────────────────────────

fn uncertain_fixture(
    home: &Path,
    ws: &Path,
    session: Uuid,
) -> (OrchStore, RunRecord, ProviderAttempt) {
    let run = run_record("run-settle", session, ws, OPERATOR_TOKEN);
    {
        let store = store_at(home);
        store.save_run(&run).unwrap();
        let mut provider = FakeProvider::new(&store, &run, OPERATOR_TOKEN);
        provider.attempt(Interrupt::AfterDispatch);
    }
    // Reopening runs restart recovery, which is what turns the dispatched
    // attempt into an uncertain one.
    let store = store_at(home);
    let mine = principal(OPERATOR_TOKEN, session, ws);
    let attempt = store
        .load_provider_attempt_for(&attempt_id_for("run-settle", 1), &mine)
        .unwrap()
        .expect("uncertain attempt");
    assert_eq!(attempt.send_state, AttemptSendState::Uncertain);
    (store, run, attempt)
}

#[test]
fn settlement_demands_exact_binding_proof_and_the_current_revision() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let (store, run, attempt) = uncertain_fixture(home.path(), ws.path(), session);
    let mine = principal(OPERATOR_TOKEN, session, ws.path());

    let bind = |run_id: &'static str, attempt_id: &'static str, request_id: &'static str| {
        SettlementBinding {
            run_id,
            attempt_id,
            request_id,
            reconcile_request_id: "rec-1",
            operator_token_id: OPERATOR_TOKEN,
        }
    };
    let good_attempt_id: &'static str = Box::leak(attempt.attempt_id.clone().into_boxed_str());
    let good_request_id: &'static str = Box::leak(run.request_id.clone().into_boxed_str());

    // Wrong run, wrong attempt, and wrong originating request each refuse.
    for binding in [
        bind("run-other", good_attempt_id, good_request_id),
        bind("run-settle", "run-settle.attempt-000009", good_request_id),
        bind("run-settle", good_attempt_id, "req-someone-else"),
    ] {
        let error = store
            .settle_provider_attempt(
                &binding,
                &mine,
                attempt.revision,
                SettlementOutcome::Delivered,
                evidence('a'),
                None,
            )
            .unwrap_err();
        assert!(
            matches!(error.code, OrchErrorCode::InvalidRequest),
            "a mismatched binding must not settle: {error:?}"
        );
    }

    // Malformed evidence is not proof.
    for bad in [
        SettlementEvidence {
            kind: String::new(),
            digest: "a".repeat(64),
            observed_at: Utc::now(),
        },
        SettlementEvidence {
            kind: "console".into(),
            digest: "A".repeat(64),
            observed_at: Utc::now(),
        },
        SettlementEvidence {
            kind: "console".into(),
            digest: "short".into(),
            observed_at: Utc::now(),
        },
    ] {
        assert!(store
            .settle_provider_attempt(
                &bind("run-settle", good_attempt_id, good_request_id),
                &mine,
                attempt.revision,
                SettlementOutcome::Delivered,
                bad,
                None,
            )
            .is_err());
    }

    // A stale revision is rejected rather than applied to newer state.
    let stale = store
        .settle_provider_attempt(
            &bind("run-settle", good_attempt_id, good_request_id),
            &mine,
            attempt.revision - 1,
            SettlementOutcome::Delivered,
            evidence('a'),
            None,
        )
        .unwrap_err();
    assert_eq!(stale.code, OrchErrorCode::StaleVersion);

    // A foreign credential is answered with absence, not a conflict.
    let theirs = principal(OTHER_TOKEN, session, ws.path());
    let foreign = store
        .settle_provider_attempt(
            &bind("run-settle", good_attempt_id, good_request_id),
            &theirs,
            attempt.revision,
            SettlementOutcome::Delivered,
            evidence('a'),
            None,
        )
        .unwrap_err();
    assert_eq!(foreign.code, OrchErrorCode::InvalidRequest);
    assert_eq!(foreign.message, "no such record in the requested scope");

    // With the exact binding, proof, and current revision, it settles.
    let settled = store
        .settle_provider_attempt(
            &bind("run-settle", good_attempt_id, good_request_id),
            &mine,
            attempt.revision,
            SettlementOutcome::Delivered,
            evidence('a'),
            Some("checked the provider console".into()),
        )
        .unwrap();
    assert_eq!(settled.send_state, AttemptSendState::Settled);
    assert!(settled.send_state.is_terminal());
    assert!(!settled.send_state.is_unsettled());
    assert!(
        !settled.permits_takeover(),
        "an attempt proven delivered must never be retried"
    );
    let record = settled.settlement.as_ref().unwrap();
    assert_eq!(record.operator_token_id, OPERATOR_TOKEN);
    assert_eq!(record.request_id, "rec-1");
    assert_eq!(record.outcome, SettlementOutcome::Delivered);
}

#[test]
fn settlement_is_idempotent_and_refuses_a_conflicting_second_verdict() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let (store, run, attempt) = uncertain_fixture(home.path(), ws.path(), session);
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let binding = SettlementBinding {
        run_id: &run.run_id,
        attempt_id: &attempt.attempt_id,
        request_id: &run.request_id,
        reconcile_request_id: "rec-1",
        operator_token_id: OPERATOR_TOKEN,
    };

    let first = store
        .settle_provider_attempt(
            &binding,
            &mine,
            attempt.revision,
            SettlementOutcome::NotDelivered,
            evidence('a'),
            None,
        )
        .unwrap();

    // The identical reconciliation replays without a second settlement.
    let replay = store
        .settle_provider_attempt(
            &binding,
            &mine,
            attempt.revision,
            SettlementOutcome::NotDelivered,
            evidence('a'),
            None,
        )
        .unwrap();
    assert_eq!(
        replay.revision, first.revision,
        "replay must not advance state"
    );
    assert_eq!(replay.settlement, first.settlement);

    // Reusing the same reconciliation key with different proof is a conflict.
    let key_reuse = store
        .settle_provider_attempt(
            &binding,
            &mine,
            first.revision,
            SettlementOutcome::Delivered,
            evidence('b'),
            None,
        )
        .unwrap_err();
    assert_eq!(key_reuse.code, OrchErrorCode::Conflict);

    // And a fresh reconciliation of an already-settled attempt is refused,
    // because the attempt is no longer uncertain.
    let second_verdict = store
        .settle_provider_attempt(
            &SettlementBinding {
                run_id: &run.run_id,
                attempt_id: &attempt.attempt_id,
                request_id: &run.request_id,
                reconcile_request_id: "rec-2",
                operator_token_id: OPERATOR_TOKEN,
            },
            &mine,
            first.revision,
            SettlementOutcome::Delivered,
            evidence('b'),
            None,
        )
        .unwrap_err();
    assert_eq!(second_verdict.code, OrchErrorCode::Conflict);

    // An attempt proven undelivered is safe for the existing retry machine.
    assert!(first.permits_takeover());
}

#[test]
fn a_tampered_attempt_record_is_never_treated_as_evidence() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();
    let run = run_record("run-forged", session, ws.path(), OPERATOR_TOKEN);
    {
        let store = store_at(home.path());
        store.save_run(&run).unwrap();
        let mut provider = FakeProvider::new(&store, &run, OPERATOR_TOKEN);
        provider.attempt(Interrupt::AfterDispatch);
    }

    // Rewrite the durable record's identity digest on disk.
    let dir = home.path().join("provider-attempts");
    let mut rewritten = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["requestDigest"] = serde_json::Value::String("f".repeat(64));
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        rewritten += 1;
    }
    assert_eq!(rewritten, 1);

    let store = store_at(home.path());
    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    let page = store
        .list_provider_attempts_for(&mine, None, false, None, 50)
        .unwrap();
    assert!(
        page.attempts.is_empty(),
        "a record whose identity digest does not recompute is not evidence"
    );

    let error = store
        .settle_provider_attempt(
            &SettlementBinding {
                run_id: &run.run_id,
                attempt_id: &attempt_id_for("run-forged", 1),
                request_id: &run.request_id,
                reconcile_request_id: "rec-1",
                operator_token_id: OPERATOR_TOKEN,
            },
            &mine,
            2,
            SettlementOutcome::Delivered,
            evidence('a'),
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::Conflict);
}

// ── retention ──────────────────────────────────────────────────────────

#[test]
fn retention_never_collects_unsettled_evidence() {
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let session = Uuid::new_v4();

    let unsettled_run = run_record("run-unsettled", session, ws.path(), OPERATOR_TOKEN);
    let clean_run = run_record("run-clean", session, ws.path(), OPERATOR_TOKEN);
    {
        let store = store_at(home.path());
        store.save_run(&unsettled_run).unwrap();
        store.save_run(&clean_run).unwrap();
        let mine = principal(OPERATOR_TOKEN, session, ws.path());
        FakeProvider::new(&store, &unsettled_run, OPERATOR_TOKEN).attempt(Interrupt::AfterDispatch);
        FakeProvider::new(&store, &clean_run, OPERATOR_TOKEN).attempt(Interrupt::Never);

        for run in [&unsettled_run, &clean_run] {
            store
                .claim_idempotency("ptah_submit_task", &run.request_id, "hash", &mine)
                .unwrap();
            store
                .complete_idempotency(
                    "ptah_submit_task",
                    &run.request_id,
                    "hash",
                    &mine,
                    Some(run.run_id.clone()),
                    serde_json::json!({"runId": run.run_id}),
                )
                .unwrap();
        }
    }

    let store = store_at(home.path());
    assert!(store.run_has_unsettled_attempts("run-unsettled").unwrap());
    assert!(!store.run_has_unsettled_attempts("run-clean").unwrap());

    // The most aggressive retention this policy allows: keep one of each, and
    // treat everything older than a nanosecond as expired.
    let report = store
        .prune_retention(RetentionPolicy {
            max_terminal_runs: 1,
            max_idempotency_receipts: 1,
            terminal_run_age: ChronoDuration::nanoseconds(1),
            idempotency_receipt_age: ChronoDuration::nanoseconds(1),
        })
        .unwrap();

    assert!(
        report.protected_unsettled_runs >= 1,
        "the run holding unsettled evidence must be reported as protected"
    );
    assert!(
        report.protected_unsettled_receipts >= 1,
        "its receipt must be reported as protected too"
    );
    assert!(
        store.load_run("run-unsettled").unwrap().is_some(),
        "a run with an unsettled provider attempt must survive retention"
    );

    let mine = principal(OPERATOR_TOKEN, session, ws.path());
    assert!(
        store
            .load_receipt_for(&unsettled_run.request_id, &mine)
            .unwrap()
            .is_some(),
        "unsettled evidence cannot be silently deleted"
    );
    assert!(
        store
            .load_provider_attempt_for(&attempt_id_for("run-unsettled", 1), &mine)
            .unwrap()
            .is_some(),
        "the attempt record itself must survive"
    );

    // Once settled, the same evidence is collectable on the ordinary schedule.
    let attempt = store
        .load_provider_attempt_for(&attempt_id_for("run-unsettled", 1), &mine)
        .unwrap()
        .unwrap();
    store
        .settle_provider_attempt(
            &SettlementBinding {
                run_id: &unsettled_run.run_id,
                attempt_id: &attempt.attempt_id,
                request_id: &unsettled_run.request_id,
                reconcile_request_id: "rec-1",
                operator_token_id: OPERATOR_TOKEN,
            },
            &mine,
            attempt.revision,
            SettlementOutcome::NotDelivered,
            evidence('a'),
            None,
        )
        .unwrap();
    assert!(!store.run_has_unsettled_attempts("run-unsettled").unwrap());
    let after = store
        .prune_retention(RetentionPolicy {
            max_terminal_runs: 1,
            max_idempotency_receipts: 1,
            terminal_run_age: ChronoDuration::nanoseconds(1),
            idempotency_receipt_age: ChronoDuration::nanoseconds(1),
        })
        .unwrap();
    assert_eq!(
        after.protected_unsettled_runs, 0,
        "settling releases the retention hold"
    );
    assert!(
        store
            .load_provider_attempt_for(&attempt_id_for("run-unsettled", 1), &mine)
            .unwrap()
            .is_some(),
        "a settled attempt is the durable proof of an operator decision and outlives the receipt age"
    );
}

// ── service seam ───────────────────────────────────────────────────────

struct Harness {
    _home: tempfile::TempDir,
    _env: ProcessEnvGuard,
    ws: tempfile::TempDir,
    store: OrchStore,
    orch: Arc<OrchestrationService>,
    session: Uuid,
    owner: String,
}

/// A live service over a store the test also holds, so a test can plant exact
/// durable state and then read it back through the authenticated seam.
///
/// `set_auth_credentials` requires the compatibility `primary` credential, so
/// the harness registers it alongside the two named device credentials whose
/// isolation is what these tests are about.
fn harness(operators: Vec<String>) -> Harness {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    let grokptah_home = home.path().join(".grokptah");
    std::fs::create_dir_all(&grokptah_home).unwrap();
    set_grokptah_home_override(Some(grokptah_home));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let ws = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");

    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "primary-secret".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
            reconciliation_operators: operators,
        },
    );
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", "primary-secret").unwrap(),
        AuthCredential::new(OPERATOR_TOKEN, "operator-secret").unwrap(),
        AuthCredential::new(OTHER_TOKEN, "other-secret").unwrap(),
    ])
    .unwrap();

    let operator = orch.auth_header(Some("Bearer operator-secret")).unwrap();
    let owner = operator.owner_id.clone();
    let created = orch
        .create_session(&operator, ws.path(), Some("authority".into()))
        .unwrap();
    let session = created["sessionId"].as_str().unwrap().parse().unwrap();

    Harness {
        _home: home,
        _env: env,
        ws,
        store,
        orch,
        session,
        owner,
    }
}

impl Harness {
    fn operator(&self) -> AuthContext {
        self.orch
            .auth_header(Some("Bearer operator-secret"))
            .unwrap()
    }

    fn other(&self) -> AuthContext {
        self.orch.auth_header(Some("Bearer other-secret")).unwrap()
    }

    /// The canonical workspace form the service stores on the session.
    fn workspace(&self) -> PathBuf {
        dunce::canonicalize(self.ws.path()).unwrap()
    }

    fn principal(&self, token: &str) -> PrincipalScope {
        PrincipalScope {
            owner_id: self.owner.clone(),
            token_id: token.into(),
            session_id: Some(self.session),
            workspace: Some(self.workspace().display().to_string()),
        }
    }

    fn save_run(&self, run_id: &str, owner: Option<&str>, session: Uuid) -> RunRecord {
        let mut run = run_record(run_id, session, &self.workspace(), OPERATOR_TOKEN);
        run.owner_id = owner.map(str::to_string);
        self.store.save_run(&run).unwrap();
        run
    }
}

#[test]
fn the_new_read_and_reconcile_tools_are_advertised_and_never_forbidden() {
    for tool in [
        "ptah_list_receipts",
        "ptah_get_receipt",
        "ptah_list_provider_attempts",
        "ptah_reconcile_provider_attempt",
    ] {
        assert!(
            CONTROL_TOOLS.contains(&tool),
            "{tool} must be reachable through the control plane"
        );
    }
}

#[test]
fn run_reads_answer_a_foreign_owner_exactly_as_they_answer_a_missing_run() {
    let h = harness(Vec::new());
    let operator = h.operator();
    let ws = h.workspace();
    h.save_run("run-mine", Some(&h.owner), h.session);
    h.save_run("run-theirs", Some("a-different-account"), h.session);

    // A run this owner created reads normally.
    let mine = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "run-mine")
        .expect("own run must remain readable");
    assert_eq!(mine["runId"], "run-mine");

    let foreign = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "run-theirs")
        .unwrap_err();
    let missing = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "run-does-not-exist")
        .unwrap_err();
    assert_eq!(foreign.code, missing.code);
    assert_eq!(
        foreign.message, missing.message,
        "a foreign run and a missing run must be indistinguishable"
    );
    assert!(!foreign.message.contains("run-theirs"));

    // Every other run read agrees, including a malformed id.
    type Read = fn(
        &OrchestrationService,
        &AuthContext,
        Uuid,
        &Path,
        &str,
    )
        -> Result<serde_json::Value, grokptah_agent_bridge::orchestration::OrchError>;
    let reads: [(&str, Read); 5] = [
        ("get_run", |o, a, s, w, r| o.get_run_scoped(a, s, w, r)),
        ("get_progress", |o, a, s, w, r| {
            o.get_progress_scoped(a, s, w, r)
        }),
        ("get_changes", |o, a, s, w, r| {
            o.get_changes_scoped(a, s, w, r)
        }),
        ("get_test_results", |o, a, s, w, r| {
            o.get_test_results_scoped(a, s, w, r)
        }),
        ("get_handoff", |o, a, s, w, r| {
            o.get_handoff_scoped(a, s, w, r)
        }),
    ];
    for (name, read) in reads {
        let foreign = read(&h.orch, &operator, h.session, &ws, "run-theirs").unwrap_err();
        let missing = read(&h.orch, &operator, h.session, &ws, "absent").unwrap_err();
        let malformed = read(&h.orch, &operator, h.session, &ws, "../escape").unwrap_err();
        assert_eq!(foreign.code, missing.code, "{name}");
        assert_eq!(foreign.message, missing.message, "{name}");
        assert_eq!(foreign.message, malformed.message, "{name}");
    }
}

#[test]
fn run_reads_answer_a_wrong_session_exactly_as_they_answer_a_missing_run() {
    let h = harness(Vec::new());
    let operator = h.operator();
    let ws = h.workspace();
    h.save_run("run-other-session", Some(&h.owner), Uuid::new_v4());

    let wrong_session = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "run-other-session")
        .unwrap_err();
    let missing = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "absent")
        .unwrap_err();
    assert_eq!(wrong_session.code, missing.code);
    assert_eq!(wrong_session.message, missing.message);

    // A run recorded against a different workspace is equally invisible.
    let elsewhere = tempdir().unwrap();
    let mut stray = run_record(
        "run-other-workspace",
        h.session,
        elsewhere.path(),
        OPERATOR_TOKEN,
    );
    stray.owner_id = Some(h.owner.clone());
    h.store.save_run(&stray).unwrap();
    let wrong_workspace = h
        .orch
        .get_run_scoped(&operator, h.session, &ws, "run-other-workspace")
        .unwrap_err();
    assert_eq!(wrong_workspace.code, missing.code);
    assert_eq!(wrong_workspace.message, missing.message);

    // A workspace outside the allowlist is the caller's own error about its
    // own request, and stays a distinct, actionable answer.
    let outside = tempdir().unwrap();
    let error = h
        .orch
        .get_run_scoped(&operator, h.session, outside.path(), "run-other-session")
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::WorkspaceMismatch);
}

#[test]
fn receipt_reads_through_the_service_are_credential_scoped() {
    let h = harness(Vec::new());
    let operator = h.operator();
    let other = h.other();
    let ws = h.workspace();
    let mine = h.principal(OPERATOR_TOKEN);

    h.store
        .claim_idempotency("ptah_submit_task", "req-1", "hash", &mine)
        .unwrap();
    h.store
        .complete_idempotency(
            "ptah_submit_task",
            "req-1",
            "hash",
            &mine,
            None,
            serde_json::json!({"ok": true}),
        )
        .unwrap();

    let listed = h
        .orch
        .list_receipts_scoped(&operator, h.session, &ws, None, 50)
        .unwrap();
    assert_eq!(listed["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(listed["scanTruncated"], false);
    assert!(h
        .orch
        .get_receipt_scoped(&operator, h.session, &ws, "req-1")
        .is_ok());

    let theirs = h
        .orch
        .list_receipts_scoped(&other, h.session, &ws, None, 50)
        .unwrap();
    assert!(
        theirs["receipts"].as_array().unwrap().is_empty(),
        "a sibling credential must not see another credential's operations"
    );
    let denied = h
        .orch
        .get_receipt_scoped(&other, h.session, &ws, "req-1")
        .unwrap_err();
    let missing = h
        .orch
        .get_receipt_scoped(&other, h.session, &ws, "never-existed")
        .unwrap_err();
    assert_eq!(denied.code, missing.code);
    assert_eq!(denied.message, missing.message);
}

/// Plant an uncertain attempt on a live store, the way a crash would leave one.
fn plant_uncertain_attempt(h: &Harness, run: &RunRecord) -> ProviderAttempt {
    let bound = h.principal(OPERATOR_TOKEN);
    let attempt = h
        .store
        .begin_provider_attempt(&run.run_id, 1, &run.request_id, &bound)
        .unwrap();
    h.store
        .mark_provider_attempt_sent(&attempt.attempt_id)
        .unwrap();
    // Recovery is what a restart runs; calling it directly avoids reopening a
    // store the live service still holds.
    h.store.recover_provider_attempts().unwrap();
    let recovered = h
        .store
        .load_provider_attempt_for(&attempt.attempt_id, &bound)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.send_state, AttemptSendState::Uncertain);
    recovered
}

fn interrupted_run(h: &Harness) -> RunRecord {
    let mut run = run_record("run-settle", h.session, &h.workspace(), OPERATOR_TOKEN);
    run.owner_id = Some(h.owner.clone());
    run.state = RunState::Interrupted;
    run.terminal_result = Some("interrupted".into());
    run.aggregates.usage_complete = false;
    h.store.save_run(&run).unwrap();
    run
}

#[tokio::test]
async fn reconciliation_requires_a_configured_operator() {
    let h = harness(Vec::new());
    let operator = h.operator();
    let run = interrupted_run(&h);
    let attempt = plant_uncertain_attempt(&h, &run);

    let error = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-1",
            h.session,
            &h.workspace(),
            ReconcileRequest {
                run_id: &run.run_id,
                attempt_id: &attempt.attempt_id,
                attempt_request_id: &run.request_id,
                expected_revision: attempt.revision,
                outcome: SettlementOutcome::Delivered,
                evidence: evidence('a'),
                note: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        OrchErrorCode::ForbiddenScope,
        "holding a valid bearer token is not operator authority"
    );

    // The attempt is untouched, so the evidence survives the refusal.
    let after = h
        .store
        .load_provider_attempt_for(&attempt.attempt_id, &h.principal(OPERATOR_TOKEN))
        .unwrap()
        .unwrap();
    assert_eq!(after.send_state, AttemptSendState::Uncertain);
    assert_eq!(after.revision, attempt.revision);
}

#[tokio::test]
async fn a_non_operator_credential_learns_nothing_about_attempts_it_cannot_settle() {
    let h = harness(vec![OPERATOR_TOKEN.into()]);
    let other = h.other();
    let run = interrupted_run(&h);
    let attempt = plant_uncertain_attempt(&h, &run);

    let real = h
        .orch
        .reconcile_provider_attempt(
            &other,
            "rec-1",
            h.session,
            &h.workspace(),
            ReconcileRequest {
                run_id: &run.run_id,
                attempt_id: &attempt.attempt_id,
                attempt_request_id: &run.request_id,
                expected_revision: attempt.revision,
                outcome: SettlementOutcome::Delivered,
                evidence: evidence('a'),
                note: None,
            },
        )
        .await
        .unwrap_err();
    let invented = h
        .orch
        .reconcile_provider_attempt(
            &other,
            "rec-2",
            h.session,
            &h.workspace(),
            ReconcileRequest {
                run_id: "run-imaginary",
                attempt_id: "run-imaginary.attempt-000001",
                attempt_request_id: "req-imaginary",
                expected_revision: 1,
                outcome: SettlementOutcome::Delivered,
                evidence: evidence('a'),
                note: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(real.code, invented.code);
    assert_eq!(
        real.message, invented.message,
        "a non-operator must not distinguish a real attempt from an invented one"
    );
}

#[tokio::test]
async fn reconciliation_settles_on_proof_without_ever_claiming_success() {
    let h = harness(vec![OPERATOR_TOKEN.into()]);
    let operator = h.operator();
    let ws = h.workspace();
    let run = interrupted_run(&h);
    let attempt = plant_uncertain_attempt(&h, &run);

    let request = |revision: u64, digest: char| ReconcileRequest {
        run_id: &run.run_id,
        attempt_id: &attempt.attempt_id,
        attempt_request_id: &run.request_id,
        expected_revision: revision,
        outcome: SettlementOutcome::Delivered,
        evidence: evidence(digest),
        note: Some("provider console shows the request"),
    };

    // A stale revision is rejected before anything is written.
    let stale = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-stale",
            h.session,
            &ws,
            request(attempt.revision - 1, 'a'),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, OrchErrorCode::StaleVersion);

    // A mismatched binding is rejected even with the right revision.
    let mismatched = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-mismatch",
            h.session,
            &ws,
            ReconcileRequest {
                attempt_request_id: "req-someone-else",
                ..request(attempt.revision, 'a')
            },
        )
        .await
        .unwrap_err();
    assert_eq!(mismatched.code, OrchErrorCode::InvalidRequest);

    let response = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-1",
            h.session,
            &ws,
            request(attempt.revision, 'a'),
        )
        .await
        .unwrap();

    assert_eq!(response["attempt"]["sendState"], "settled");
    assert_eq!(response["attempt"]["settlement"]["outcome"], "delivered");
    assert_eq!(
        response["attempt"]["settlement"]["operatorTokenId"],
        OPERATOR_TOKEN
    );
    assert_eq!(
        response["providerResponseObserved"], false,
        "settling delivery is never a claim that a response was seen"
    );
    assert_eq!(
        response["usageComplete"], false,
        "a settled attempt must not make token accounting look whole"
    );
    assert_eq!(response["runState"], "interrupted");

    let after = h.store.load_run(&run.run_id).unwrap().unwrap();
    assert_eq!(after.state, RunState::Interrupted);
    assert_eq!(after.terminal_result.as_deref(), Some("interrupted"));
    assert!(
        !after.aggregates.usage_complete,
        "reconciliation must never manufacture a terminal success"
    );

    // Replaying the same reconciliation returns the same receipt.
    let replay = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-1",
            h.session,
            &ws,
            request(attempt.revision, 'a'),
        )
        .await
        .unwrap();
    assert_eq!(replay, response, "reconciliation is idempotent");

    // A fresh reconciliation of an already-settled attempt is refused.
    let second = h
        .orch
        .reconcile_provider_attempt(
            &operator,
            "rec-2",
            h.session,
            &ws,
            request(attempt.revision, 'b'),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            second.code,
            OrchErrorCode::StaleVersion | OrchErrorCode::Conflict
        ),
        "a second verdict must not overwrite a settled attempt: {second:?}"
    );

    // The settled attempt is now visible through the read seam, and the run's
    // receipt no longer reports unsettled evidence.
    let attempts = h
        .orch
        .list_provider_attempts_scoped(&operator, h.session, &ws, Some(&run.run_id), true, None, 50)
        .unwrap();
    assert!(
        attempts["attempts"].as_array().unwrap().is_empty(),
        "nothing remains unsettled once the operator has settled it"
    );
}
