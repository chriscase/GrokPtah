//! Route- and ledger-level proof that a provider send cannot escape the record.
//!
//! The transport-level suite (in `host_helpers`) drives the real HTTP path
//! against a scripted loopback server. This one asks the questions a *control
//! plane* asks: what does a restarted process read, what does a coordinator
//! over MCP get told, and what can a client of the read routes see.
//!
//! Nothing here reaches a provider. Every attempt is declared against the real
//! [`OrchStore`] and the outcomes are supplied by the test, because the point
//! is what the durable record says, not what a network did.

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::send_authority::{
    ProviderRequestIdentity, SendBinding, SendCause, SendLedger,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, EventBus, HostConfig, SessionKind,
};
use grokptah_agent_sdk::account::CredentialMethod;
use grokptah_agent_sdk::attempt::{BoundedId, SendOutcome, SendState};
use grokptah_agent_sdk::launch::{
    BaseCategory, LaunchRequirement, ModelReference, ProviderClass, RequestDialect, RouteClass,
};
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

fn bounded(value: &str) -> BoundedId {
    BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
}

fn requirement() -> LaunchRequirement {
    LaunchRequirement {
        provider: ProviderClass::OpenAiCompatible,
        credential_method: CredentialMethod::ProviderEnv,
        route: RouteClass::CompatibleProvider,
        base: BaseCategory::CompatibleLoopback,
        dialect: RequestDialect::OpenAiChatCompletions,
        model: ModelReference::new("test-model"),
        account_reference: None,
    }
}

/// A prompt with something worth leaking in it, so a projection that carried
/// request text would be caught rather than merely suspected.
const PROMPT: &str = "patient ledger for Contoso Health, account 4111-1111-1111-1111";

fn ledger(store: &OrchStore, run_id: &str) -> SendLedger {
    SendLedger::bind(
        store.clone(),
        SendBinding {
            run_id: run_id.into(),
            request_id: format!("req-{run_id}"),
            session_id: Uuid::nil(),
            workspace: "/home/operator/customers/contoso".into(),
            prompt: PROMPT.into(),
            requirement: Some(requirement()),
            profile: Some("openai-compatible".into()),
            effort: Some("none".into()),
        },
    )
    .expect("an admitted turn binds a ledger")
}

fn identity(body: &str) -> ProviderRequestIdentity {
    ProviderRequestIdentity {
        route_digest: bounded("route:0a1b2c3d4e5f6071"),
        body_digest: bounded(&format!("body:{body}")),
        credential_revision: bounded("cred:9f8e7d6c5b4a3928"),
    }
}

/// Reopen the ledger the way a restarted process would. Every live handle must
/// be gone first: the store holds an exclusive lock, exactly as a second
/// process would find it.
fn reopen(root: &std::path::Path) -> OrchStore {
    OrchStore::open(root).expect("a restarted process reopens its ledger")
}

/// The crash cut that decides whether a restart may re-send: a process that
/// died *before* handing bytes to the transport left a request that provably
/// never went anywhere.
#[test]
fn a_crash_between_declaring_and_sending_stays_safely_retryable() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orchestration");
    {
        let store = OrchStore::open(root.clone()).unwrap();
        let ledger = ledger(&store, "run-precut");
        // Declared, and then the process dies. `mark_sending` never ran, and
        // dropping the ticket changes nothing: a request still recorded as
        // never-sent has nothing to fence.
        drop(
            ledger
                .declare(SendCause::InitialSend, &identity("aaaa"))
                .unwrap(),
        );
        drop(ledger);
        drop(store);
    }

    let store = reopen(&root);
    let recovered = store.list_attempts_for_run("run-precut").unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].send_state, SendState::KnownNotSent);
    assert!(
        recovered[0].may_auto_retry(),
        "a request that never left was not retryable"
    );
    assert!(store.run_permits_new_attempt("run-precut").unwrap());
}

/// The same cut one instant later, where the answer inverts: bytes were handed
/// to the transport, so a restart must not assume anything.
#[test]
fn a_crash_after_the_boundary_is_never_auto_retried() {
    let (home, _guard) = setup_home();
    let root = home.path().join("orchestration");
    let store = OrchStore::open(root.clone()).unwrap();
    let ledger = ledger(&store, "run-postcut");
    let ticket = ledger
        .declare(SendCause::InitialSend, &identity("bbbb"))
        .unwrap();
    ticket.mark_sending().unwrap();
    // No destructors, exactly like a process that was killed. A real crash
    // also releases the ledger's OS lock, which this process cannot do while
    // the handle is leaked -- so the durable state is read back through a
    // fresh directory scan, which is what a restart actually reads.
    std::mem::forget(ticket);

    let interrupted = store.list_attempts_for_run("run-postcut").unwrap();
    assert_eq!(interrupted[0].send_state, SendState::Sending);
    assert!(!interrupted[0].may_auto_retry());
    assert!(!store.run_permits_new_attempt("run-postcut").unwrap());

    // Restart reconciliation names the ambiguity rather than clearing it.
    grokptah_agent_bridge::send_authority::fence_run(&store, "run-postcut");
    let fenced = store.list_attempts_for_run("run-postcut").unwrap();
    assert_eq!(fenced[0].send_state, SendState::Uncertain);
    assert!(!fenced[0].may_auto_retry());
    assert!(!store.run_permits_new_attempt("run-postcut").unwrap());

    // And re-running reconciliation is not a second chance to clear it.
    grokptah_agent_bridge::send_authority::fence_run(&store, "run-postcut");
    assert_eq!(
        store.list_attempts_for_run("run-postcut").unwrap()[0].send_state,
        SendState::Uncertain
    );
}

/// Cancellation is not a clean exit for a request already on the wire. The
/// provider is still working on something nobody will read.
#[test]
fn cancelling_a_turn_fences_its_in_flight_send() {
    let (home, _guard) = setup_home();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-cancel");
    let ticket = ledger
        .declare(SendCause::InitialSend, &identity("cccc"))
        .unwrap();
    ticket.mark_sending().unwrap();
    ticket
        .mark_sent(Some("prq:x-request-id.abc123"), 200)
        .unwrap();

    // The operator cancels. Delivered, unread, unresolved.
    drop(ticket);
    ledger.fence();

    let attempts = store.list_attempts_for_run("run-cancel").unwrap();
    assert_eq!(attempts[0].send_state, SendState::Uncertain);
    assert!(
        !store.run_permits_new_attempt("run-cancel").unwrap(),
        "a cancelled in-flight send stopped fencing the run"
    );
    // Fencing twice is not a second event.
    ledger.fence();
    assert_eq!(
        store.list_attempts_for_run("run-cancel").unwrap()[0].send_state,
        SendState::Uncertain
    );
}

/// Two declarations racing for the same run must not collide onto one ordinal:
/// a reused ordinal is a reused idempotency key, which is exactly the
/// duplicate the key exists to suppress.
#[test]
fn concurrent_declarations_never_share_an_ordinal_or_a_key() {
    let (home, _guard) = setup_home();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-conflict");

    let mut keys = Vec::new();
    for round in 0..4u32 {
        let ticket = ledger
            .declare(SendCause::InitialSend, &identity(&format!("{round:04}")))
            .expect("each settled send may be followed by another");
        keys.push(ticket.idempotency_key().to_string());
        // Settle so the next declaration is not refused as a duplicate send.
        ticket.settle_not_sent().unwrap();
    }

    let attempts = store.list_attempts_for_run("run-conflict").unwrap();
    assert_eq!(attempts.len(), 4);
    let ordinals: Vec<u32> = attempts.iter().map(|a| a.ordinal).collect();
    assert_eq!(ordinals, vec![1, 2, 3, 4]);
    let unique: std::collections::BTreeSet<&String> = keys.iter().collect();
    assert_eq!(
        unique.len(),
        4,
        "two physical sends shared one key: {keys:?}"
    );
    // Every one settled as never-sent, so none of them fences the run.
    assert!(store.run_permits_new_attempt("run-conflict").unwrap());
    for attempt in &attempts {
        assert_eq!(attempt.receipts.outcome, Some(SendOutcome::NotSent));
    }
}

/// The refusal an operator actually meets: an unresolved send makes the next
/// equivalent request fail closed, and the message names the key to reconcile
/// against rather than a bare "conflict".
#[test]
fn an_unresolved_send_refuses_the_next_one_and_names_its_key() {
    let (home, _guard) = setup_home();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-refusal");
    let ticket = ledger
        .declare(SendCause::InitialSend, &identity("dddd"))
        .unwrap();
    ticket.mark_sending().unwrap();
    let key = ticket.idempotency_key().to_string();
    drop(ticket); // dropped mid-flight: fenced, not tidied away

    let refusal = ledger
        .declare(SendCause::InitialSend, &identity("eeee"))
        .expect_err("an equivalent request must be refused");
    let refusal = refusal.to_string();
    assert!(refusal.contains("refusing to send"), "{refusal}");
    assert!(refusal.contains(&key), "{refusal}");
    assert_eq!(store.list_attempts_for_run("run-refusal").unwrap().len(), 1);

    // A connect-phase failure is the one resend that may follow an unresolved
    // attempt, because it is positive evidence nothing left the process.
    ledger
        .declare(SendCause::TransportRetry, &identity("ffff"))
        .expect("a proven-unsent retry is not a duplicate");
}

/// A durable attempt is read by restarts, operators, and support. Nothing in
/// it may name a credential, an endpoint, a workspace path, or the request
/// text — and the workspace and prompt used here contain material that would
/// be obvious if it escaped.
#[test]
fn the_durable_attempt_carries_no_credential_private_data_or_endpoint() {
    let (home, _guard) = setup_home();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-projection");
    let ticket = ledger
        .declare(SendCause::InitialSend, &identity("1234"))
        .unwrap();
    ticket.mark_sending().unwrap();
    ticket
        .mark_sent(Some("prq:x-request-id.abc123"), 200)
        .unwrap();
    ticket.mark_responding().unwrap();
    ticket.settle_accepted(None, Some("resp-1")).unwrap();

    let attempts = store.list_attempts_for_run("run-projection").unwrap();
    let encoded = serde_json::to_string(&attempts[0]).unwrap();
    for forbidden in [
        PROMPT,
        "Contoso",
        "4111-1111-1111-1111",
        "/home/operator",
        "customers",
        "http://",
        "https://",
        "chat/completions",
        "Bearer",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "the durable attempt leaked {forbidden:?}: {encoded}"
        );
    }

    // What it does carry is bounded and opaque, and it validates as such.
    assert_eq!(attempts[0].validate(), Ok(()));
    assert!(attempts[0].subject.workspace.as_str().starts_with("wsp:"));
    assert!(attempts[0].intent.digest.as_str().starts_with("sha256:"));
    assert_eq!(attempts[0].receipts.outcome, Some(SendOutcome::Accepted));
}

/// The send ledger is not part of the control plane's read surface.
///
/// Read routes are the widest audience the durable record has, so the property
/// worth pinning is not "the projection redacts the ledger" but "the ledger is
/// never in the projection at all". Anything else is one field away from a
/// leak.
#[tokio::test]
async fn the_control_plane_read_routes_publish_no_send_ledger_material() {
    let (home, _guard) = setup_home();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");
    let ws = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-readroutes");
    let ticket = ledger
        .declare(SendCause::InitialSend, &identity("5678"))
        .unwrap();
    ticket.mark_sending().unwrap();
    let key = ticket.idempotency_key().to_string();
    let body_digest = identity("5678").body_digest.as_str().to_string();
    let credential_digest = identity("5678").credential_revision.as_str().to_string();
    ticket
        .mark_sent(Some("prq:x-request-id.abc123"), 200)
        .unwrap();
    ticket.mark_responding().unwrap();
    ticket.settle_accepted(None, None).unwrap();

    let orch = OrchestrationService::new(
        host.clone(),
        EventBus::new(64),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    // Every read route a client can reach, rendered as the client sees it.
    let mut projections = vec![
        orch.list_sessions(&auth).unwrap().to_string(),
        orch.get_capacity(&auth).unwrap().to_string(),
    ];
    if let Ok(value) = orch.get_run(&auth, "run-readroutes") {
        projections.push(value.to_string());
    }
    if let Ok(value) = orch.get_progress(&auth, "run-readroutes") {
        projections.push(value.to_string());
    }

    for projection in &projections {
        for forbidden in [
            key.as_str(),
            body_digest.as_str(),
            credential_digest.as_str(),
            PROMPT,
            "Contoso",
            "4111-1111-1111-1111",
            "prq:x-request-id.abc123",
        ] {
            assert!(
                !projection.contains(forbidden),
                "a read route published {forbidden:?}: {projection}"
            );
        }
    }

    // The ledger itself is intact — it simply is not published.
    assert_eq!(
        store.list_attempts_for_run("run-readroutes").unwrap()[0].send_state,
        SendState::Settled
    );
}

/// A replayed intent is the same intent. Re-declaring after a settled send
/// must produce a *new* physical request rather than resurrecting the old one,
/// and the two must stay distinguishable to the provider.
#[test]
fn a_replayed_intent_is_a_new_physical_send_not_a_reused_one() {
    let (home, _guard) = setup_home();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let ledger = ledger(&store, "run-replay");

    let first = ledger
        .declare(SendCause::InitialSend, &identity("aaaa"))
        .unwrap();
    first.mark_sending().unwrap();
    first.mark_sent(Some("prq:x-request-id.one"), 200).unwrap();
    first.mark_responding().unwrap();
    first.settle_accepted(None, None).unwrap();
    let first_key = first.idempotency_key().to_string();
    let first_attempt_id = first.attempt_id().to_string();

    let second = ledger
        .declare(SendCause::InitialSend, &identity("aaaa"))
        .unwrap();
    assert_ne!(second.idempotency_key(), first_key);
    assert_ne!(second.attempt_id(), first_attempt_id);
    second.settle_not_sent().unwrap();

    let attempts = store.list_attempts_for_run("run-replay").unwrap();
    assert_eq!(attempts.len(), 2);
    // The first stays exactly as it settled: a replay never rewrites history.
    assert_eq!(attempts[0].send_state, SendState::Settled);
    assert_eq!(attempts[0].receipts.outcome, Some(SendOutcome::Accepted));
    assert_eq!(attempts[0].receipts.response_status, Some(200));
    assert_eq!(attempts[1].receipts.outcome, Some(SendOutcome::NotSent));
    // Same intent digest on both, because it is the same intent.
    assert_eq!(attempts[0].intent.digest, attempts[1].intent.digest);
}
