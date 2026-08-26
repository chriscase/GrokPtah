//! Provider-send journal fences: an interrupted run whose physical provider
//! sends are unresolved must not be replaced, and its admission capacity must
//! not be handed to another run, until a reconciliation proves the outcome.
//!
//! These tests never reach a live provider: every attempt record is written
//! through the durable journal that the real send path writes.

mod common;

use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, OrchStore, OrchestrationConfig, OrchestrationService, ProviderAttemptState,
    ProviderReconciliationAction, ProviderRequestIdentity, ProviderSendCause, ProviderSendJournal,
    RunBounds, RunExecutionMode, RunRecord, RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, SessionKind};
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

fn setup_home() -> (tempfile::TempDir, ProcessEnvGuard) {
    let mut guard = ProcessEnvGuard::new();
    let d = tempdir().unwrap();
    let home = d.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
    (d, guard)
}

fn started_host() -> grokptah_agent_bridge::AgentHostHandle {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");
    host
}

fn orch_for(
    host: &grokptah_agent_bridge::AgentHostHandle,
    home: &tempfile::TempDir,
    ws: &tempfile::TempDir,
    max_concurrent: usize,
) -> std::sync::Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds::default(),
        },
    )
}

fn identity(body: &str) -> ProviderRequestIdentity {
    ProviderRequestIdentity {
        route_identity: "compatible:deadbeef/chat/completions".into(),
        provider_profile: "test-profile".into(),
        dialect: "openai_chat_completions".into(),
        wire_model: "test-model".into(),
        credential_revision: hash_payload(&serde_json::json!({ "revision": "rev-1" })),
        body_digest: hash_payload(&serde_json::json!({ "body": body })),
    }
}

/// Record one physical send that crossed the boundary and then died with the
/// process, exactly as the real send path would leave it.
fn unresolved_attempt(journal: &ProviderSendJournal, run_id: &str, session_id: Uuid) -> u64 {
    let record = journal
        .declare(
            run_id,
            session_id,
            1,
            ProviderSendCause::InitialSend,
            &identity("prompt"),
        )
        .unwrap();
    journal.mark_sending(run_id, record.ordinal).unwrap();
    record.ordinal
}

fn interrupted_run(session_id: Uuid, ws: &tempfile::TempDir, run_id: &str) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        session_id,
        workspace: dunce::canonicalize(ws.path())
            .unwrap()
            .display()
            .to_string(),
        request_id: "source-request".into(),
        client_id: Some("mcp".into()),
        state: RunState::Interrupted,
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        queue_position: None,
        bounds: RunBounds {
            max_prompt_bytes: 10_000,
            max_rounds: 2,
            max_duration_ms: 30_000,
        },
        prompt_preview: "previous attempt".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: Some("interrupted".into()),
        final_response: None,
        error_code: Some("interrupted".into()),
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

/// An interrupted run with an unresolved physical send cannot be retried.
/// Reopening the store twice preserves that uncertainty, a reconciliation
/// that does not match the recorded request or credential revision is
/// refused, a second reconciliation of the same attempt is refused, and only
/// a matching proof releases the fence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn unresolved_provider_send_denies_retry_until_reconciled() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let source_id = "interrupted-with-open-send";
    orch.store()
        .save_run(&interrupted_run(session.id, &ws, source_id))
        .unwrap();

    let journal = orch.store().provider_journal();
    let ordinal = unresolved_attempt(&journal, source_id, session.id);

    // The crash cut, applied twice, must not lose the uncertainty.
    for expected in [1usize, 0] {
        let report = orch.store().reopen_provider_journal().unwrap();
        assert_eq!(report.marked_uncertain, expected);
    }
    let attempt = journal.load(source_id, ordinal).unwrap();
    assert_eq!(attempt.state, ProviderAttemptState::Uncertain);

    // Explicit retry is refused while the provider may still be executing.
    let denied = orch
        .retry_run(
            &auth,
            "retry-fenced",
            session.id,
            ws.path(),
            source_id,
            "replace the interrupted run".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code.as_str(), "conflict");
    assert!(
        denied.message.contains("unresolved provider sends"),
        "{}",
        denied.message
    );
    let data = denied.data.expect("denial names the outstanding attempts");
    assert_eq!(data["unresolvedProviderAttempts"][0]["ordinal"], ordinal);
    assert_eq!(data["unresolvedProviderAttempts"][0]["state"], "uncertain");

    // The read projection agrees, under the same run authority.
    let projection = orch
        .list_provider_attempts(&auth, session.id, ws.path(), source_id)
        .unwrap();
    assert_eq!(projection["retryFenced"], true);
    assert_eq!(projection["attempts"][0]["state"], "uncertain");
    assert_eq!(projection["attempts"][0]["wireModel"], "test-model");

    // A proof for a different request, or issued under a rotated credential,
    // is refused and leaves the fence in place.
    let wrong_digest = orch
        .reconcile_provider_attempt(
            &auth,
            session.id,
            ws.path(),
            source_id,
            ordinal,
            ProviderReconciliationAction::ProvenSettled,
            &hash_payload(&serde_json::json!({ "some": "other request" })),
            &attempt.credential_revision,
            "mismatched evidence",
        )
        .unwrap_err();
    assert_eq!(wrong_digest.code.as_str(), "invalid_request");

    let stale = orch
        .reconcile_provider_attempt(
            &auth,
            session.id,
            ws.path(),
            source_id,
            ordinal,
            ProviderReconciliationAction::ProvenSettled,
            &attempt.request_digest,
            &hash_payload(&serde_json::json!({ "revision": "rotated" })),
            "evidence from a rotated credential",
        )
        .unwrap_err();
    assert_eq!(stale.code.as_str(), "stale_version");

    // Cross-session access is still refused by the existing run authority.
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();
    let cross = orch
        .reconcile_provider_attempt(
            &auth,
            other.id,
            ws.path(),
            source_id,
            ordinal,
            ProviderReconciliationAction::ProvenNotSent,
            &attempt.request_digest,
            &attempt.credential_revision,
            "not this session's run",
        )
        .unwrap_err();
    assert_eq!(cross.code.as_str(), "forbidden_scope");
    assert_eq!(journal.unresolved_for_run(source_id).unwrap().len(), 1);

    // A matching proof settles the attempt and clears the fence.
    let resolved = orch
        .reconcile_provider_attempt(
            &auth,
            session.id,
            ws.path(),
            source_id,
            ordinal,
            ProviderReconciliationAction::ProvenNotSent,
            &attempt.request_digest,
            &attempt.credential_revision,
            "provider has no record of the request",
        )
        .unwrap();
    assert_eq!(resolved["retryFenced"], false);
    assert_eq!(resolved["attempt"]["outcome"], "not_sent");

    // Reconciling the same attempt a second time is refused.
    let duplicate = orch
        .reconcile_provider_attempt(
            &auth,
            session.id,
            ws.path(),
            source_id,
            ordinal,
            ProviderReconciliationAction::ProvenSettled,
            &attempt.request_digest,
            &attempt.credential_revision,
            "second opinion",
        )
        .unwrap_err();
    assert_eq!(duplicate.code.as_str(), "conflict");

    // Retry is now allowed, and the replacement is linked as before.
    let allowed = orch
        .retry_run(
            &auth,
            "retry-after-reconcile",
            session.id,
            ws.path(),
            source_id,
            "replace the interrupted run".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(allowed["retryOf"], source_id);

    set_grokptah_home_override(None);
}

/// A journal entry this process cannot account for is unresolved work: the
/// retry fence must hold on a malformed record exactly as it does on an
/// uncertain one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn malformed_journal_entry_denies_retry() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let source_id = "interrupted-with-broken-journal";
    orch.store()
        .save_run(&interrupted_run(session.id, &ws, source_id))
        .unwrap();

    let attempts_dir = orch
        .store()
        .root()
        .join("provider_attempts")
        .join(grokptah_agent_bridge::orchestration::safe_id_filename(source_id).unwrap());
    std::fs::create_dir_all(&attempts_dir).unwrap();
    std::fs::write(attempts_dir.join("000001.json"), "{ truncated").unwrap();

    let denied = orch
        .retry_run(
            &auth,
            "retry-malformed",
            session.id,
            ws.path(),
            source_id,
            "replace the interrupted run".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code.as_str(), "conflict");
    let data = denied.data.expect("denial names the unreadable entry");
    assert!(data["unresolvedProviderAttempts"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("unreadable"));

    // Reopening does not quietly repair or drop it.
    let report = orch.store().reopen_provider_journal().unwrap();
    assert_eq!(report.unreadable, 1);
    assert_eq!(report.settled_not_sent, 0);

    set_grokptah_home_override(None);
}

/// A run that ends — including by cancellation — while a physical provider
/// send is unresolved must keep its admission slot. Releasing capacity there
/// would let a replacement run overlap work the provider may still be doing.
/// Reconciliation is what returns the slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn unresolved_provider_send_holds_admission_capacity() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let blocker_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(blocker_session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 1);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    // Hold the single admission slot so both submissions are queued and
    // neither can start before its journal entry exists.
    host.reserve_orchestration_turn("blocker", blocker_session.id)
        .unwrap();
    let fenced = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "queued-fenced",
            session.id,
            ws.path(),
            "list files".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let fenced_id = fenced["runId"].as_str().unwrap().to_string();
    let pumper = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "queued-pumper",
            session.id,
            ws.path(),
            "list files".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let pumper_id = pumper["runId"].as_str().unwrap().to_string();
    assert_eq!(
        serde_json::from_value::<RunState>(
            orch.get_run(&auth, &fenced_id).unwrap()["state"].clone()
        )
        .unwrap(),
        RunState::Queued
    );

    // Record a physical send that crossed the boundary and never resolved.
    let journal = orch.store().provider_journal();
    let ordinal = unresolved_attempt(&journal, &fenced_id, session.id);
    journal
        .mark_uncertain(&fenced_id, ordinal, "connection lost after the send")
        .unwrap();

    // Free the slot, then cancel the second queued run to wake the scheduler.
    host.release_orchestration_turn("blocker");
    orch.cancel(
        &auth,
        "cancel-pumper",
        session.id,
        ws.path(),
        Some(&pumper_id),
    )
    .await
    .unwrap();

    // The fenced run reaches a terminal state...
    let start = std::time::Instant::now();
    loop {
        let state: RunState =
            serde_json::from_value(orch.get_run(&auth, &fenced_id).unwrap()["state"].clone())
                .unwrap();
        if !matches!(state, RunState::Running | RunState::Queued) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "fenced run never finished"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // ...but its admission slot is deliberately still held. Settle well past
    // the point where an unfenced run would have released it: if the hold had
    // not been registered, the count below would already be zero.
    assert_eq!(journal.unresolved_for_run(&fenced_id).unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(host.orchestration_active_count(), 1);
    assert_eq!(orch.get_capacity(&auth).unwrap()["activeRuns"], 1);
    // The held slot is real capacity, not bookkeeping: a new run cannot take it.
    let exhausted = orch
        .submit_task(
            &auth,
            "post-fence",
            session.id,
            ws.path(),
            "list files".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(exhausted.code.as_str(), "capacity_exhausted");

    let attempt = journal.load(&fenced_id, ordinal).unwrap();
    orch.reconcile_provider_attempt(
        &auth,
        session.id,
        ws.path(),
        &fenced_id,
        ordinal,
        ProviderReconciliationAction::ProvenSettled,
        &attempt.request_digest,
        &attempt.credential_revision,
        "provider reports the request completed",
    )
    .unwrap();
    assert_eq!(host.orchestration_active_count(), 0);
    assert_eq!(orch.get_capacity(&auth).unwrap()["activeRuns"], 0);

    set_grokptah_home_override(None);
}
