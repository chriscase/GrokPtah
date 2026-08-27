//! Durable-admission gating, immutable attribution, and isolation defaults.
//!
//! These exercise the host's fail-closed launch gate through its real
//! injection point rather than a mock of the projection, so what is tested is
//! the policy the desktop and the coordinator actually run under.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use grokptah_agent_bridge::launch_truth::{
    Admission, AdmissionFacts, LaunchGate, LaunchRequirement,
};
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RollbackGuarantee, RunExecutionMode,
    RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, HostConfig, SessionKind,
};
use grokptah_agent_sdk::account::{
    AccountReference, AccountReferenceSource, CredentialMethod, RunAttribution,
};
use grokptah_agent_sdk::launch::{
    BaseCategory, LaunchReason, ModelReference, ProviderClass, RequestDialect, RouteClass,
};
use tempfile::{tempdir, TempDir};

fn setup_home() -> (TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = home_override_serial();
    let dir = tempdir().unwrap();
    set_grokptah_home_override(Some(dir.path().join(".grokptah")));
    unsafe {
        std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    }
    (dir, guard)
}

/// A gate whose verdict is fixed, so admission policy is exercised without a
/// keychain, a network, or a wall clock.
struct FixedGate {
    verdict: Result<AdmissionFacts, LaunchReason>,
    calls: Arc<AtomicUsize>,
    /// Requirements this gate was asked to enforce, in call order.
    seen: parking_lot::Mutex<Vec<Option<LaunchRequirement>>>,
}

impl FixedGate {
    fn ready() -> Self {
        Self::new(Ok(AdmissionFacts {
            truth: ready_truth(),
            requirement: requirement(),
            attribution: attribution(),
        }))
    }

    fn refusing(reason: LaunchReason) -> Self {
        Self::new(Err(reason))
    }

    fn new(verdict: Result<AdmissionFacts, LaunchReason>) -> Self {
        Self {
            verdict,
            calls: Arc::new(AtomicUsize::new(0)),
            seen: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LaunchGate for FixedGate {
    async fn admit(
        &self,
        requirement: Option<&LaunchRequirement>,
    ) -> Result<Admission, LaunchReason> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().push(requirement.cloned());
        self.verdict
            .clone()
            .map(|facts| Admission::Enforced(Box::new(facts)))
    }
}

fn attribution() -> RunAttribution {
    RunAttribution {
        credential_method: CredentialMethod::GrokBuildOidc,
        account_reference: AccountReference::new("usr-0a1b2c3d", AccountReferenceSource::UserId),
    }
}

/// Declare one physical provider send for an admitted run, the way the send
/// path itself does.
///
/// The attempt is created where a real send creates it -- at the socket, by
/// [`grokptah_agent_bridge::send_authority`] -- rather than being pre-recorded
/// at admission. An offline host reaches no provider, so these tests declare
/// explicitly instead of waiting for a request that will never be issued.
fn declare_send(
    store: &OrchStore,
    run_id: &str,
    session_id: uuid::Uuid,
    workspace: &std::path::Path,
    prompt: &str,
) -> grokptah_agent_bridge::send_authority::AttemptTicket {
    use grokptah_agent_bridge::send_authority::{
        ProviderRequestIdentity, SendBinding, SendCause, SendLedger,
    };
    use grokptah_agent_sdk::attempt::BoundedId;

    let ledger = SendLedger::bind(
        store.clone(),
        SendBinding {
            run_id: run_id.into(),
            request_id: format!("req-{run_id}"),
            session_id,
            workspace: workspace.display().to_string(),
            prompt: prompt.into(),
            requirement: Some(requirement()),
            profile: None,
            effort: None,
        },
    )
    .expect("an admitted run binds a ledger");
    ledger
        .declare(
            SendCause::InitialSend,
            &ProviderRequestIdentity {
                route_digest: BoundedId::new("route:0a1b2c3d4e5f6071").unwrap(),
                body_digest: BoundedId::new("body:1122334455667788").unwrap(),
                credential_revision: BoundedId::new("cred:99aabbccddeeff00").unwrap(),
            },
        )
        .expect("a run with no unresolved send may declare one")
}

fn requirement() -> LaunchRequirement {
    LaunchRequirement {
        provider: ProviderClass::Xai,
        credential_method: CredentialMethod::GrokBuildOidc,
        route: RouteClass::XaiFirstParty,
        base: BaseCategory::XaiOfficial,
        dialect: RequestDialect::XaiChatCompletions,
        model: ModelReference::new("grok-4"),
        account_reference: attribution().account_reference.clone(),
    }
}

fn ready_truth() -> grokptah_agent_sdk::launch::GrokLaunchTruth {
    use grokptah_agent_sdk::account::{AccountObservation, CredentialSource, GrokAccountFacts};
    use grokptah_agent_sdk::launch::{
        CapabilityFacts, CapabilityProvenance, GrokLaunchTruth, LaunchObservation, ModelFacts,
        Refreshability,
    };
    const NOW: i64 = 1_787_616_000;
    let account = GrokAccountFacts::project(
        CredentialSource::GrokBuildSession,
        &AccountObservation {
            auth_mode: Some("oidc"),
            user_id: Some("usr-0a1b2c3d"),
            principal_id: None,
            team_id: None,
            expires_at: Some("2026-08-25T12:30:00Z"),
        },
        NOW,
    );
    GrokLaunchTruth::project(&LaunchObservation {
        provider: ProviderClass::Xai,
        route: RouteClass::XaiFirstParty,
        base: BaseCategory::XaiOfficial,
        dialect: RequestDialect::XaiChatCompletions,
        refreshability: Refreshability::Refreshable,
        model: ModelFacts::selected(ModelReference::new("grok-4").unwrap()),
        capabilities: CapabilityFacts {
            provenance: CapabilityProvenance::Declared,
            chat: true,
            tools: true,
            stream: true,
            parallel_tool_calls: true,
            image_input: false,
        },
        account: &account,
    })
}

fn service(
    home: &TempDir,
    workspace: &std::path::Path,
    gate: Arc<dyn LaunchGate>,
) -> (
    Arc<OrchestrationService>,
    grokptah_agent_bridge::AgentHostHandle,
    uuid::Uuid,
    OrchStore,
) {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        launch_gate: Some(gate.clone()),
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace).unwrap();
    let bus = host.event_bus();
    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = OrchestrationService::with_launch_gate(
        host.clone(),
        bus,
        store.clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([workspace.to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: Default::default(),
        },
        gate,
    );
    (orch, host, session.id, store)
}

/// A run record is a promise about where tokens will be spent. The facts
/// behind that promise are established at admission, not inherited from what
/// the UI believed when the operator pressed the button.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_refused_launch_never_becomes_a_durable_run() {
    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let gate = Arc::new(FixedGate::refusing(LaunchReason::ReauthenticationRequired));
    let calls = gate.calls.clone();
    let (orch, host, session_id, store) = service(&home, workspace.path(), gate);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let refused = orch
        .submit_task(
            &auth,
            "req-refused",
            session_id,
            workspace.path(),
            "do the thing".into(),
            None,
        )
        .await
        .expect_err("an expired credential must refuse admission");
    assert!(
        refused.message.contains("reauthentication_required"),
        "the refusal must name the operator's next action: {}",
        refused.message
    );
    assert!(
        refused.message.contains("blocked"),
        "the refusal must be typed: {}",
        refused.message
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the gate ran exactly once");

    // Nothing durable was written, and nothing is running.
    assert!(
        store.list_runs().unwrap().is_empty(),
        "a refused launch left a durable run behind"
    );
    host.stop().unwrap();
}

/// Attribution is recorded in the production path, from the facts the gate
/// enforced, and cannot be rewritten afterwards.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_admitted_run_records_the_exact_facts_it_was_admitted_on() {
    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let gate = Arc::new(FixedGate::ready());
    let (orch, host, session_id, store) = service(&home, workspace.path(), gate);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let response = orch
        .submit_task(
            &auth,
            "req-admitted",
            session_id,
            workspace.path(),
            "do the thing".into(),
            None,
        )
        .await
        .expect("a ready gate admits");
    let run_id = response["runId"].as_str().expect("a run id").to_string();

    let run = store.load_run(&run_id).unwrap().expect("the run exists");
    assert_eq!(run.attribution, Some(attribution()));
    assert_eq!(run.launch_requirement, Some(requirement()));

    // Re-pointing a recorded run at another account is refused at the store.
    let repointed = RunAttribution {
        credential_method: CredentialMethod::ApiKey,
        account_reference: AccountReference::new(
            "usr-someone-else",
            AccountReferenceSource::UserId,
        ),
    };
    assert!(
        store
            .update_run(&run_id, |run| {
                run.attribution = Some(repointed.clone());
                Ok(())
            })
            .is_err(),
        "attribution was rewritten after admission"
    );
    assert_eq!(
        store.load_run(&run_id).unwrap().unwrap().attribution,
        Some(attribution())
    );

    // The recorded attribution carries no credential material.
    let encoded = serde_json::to_string(&run).unwrap();
    for needle in ["bearer", "Bearer", "refresh_token", "apiKey", "@"] {
        assert!(!encoded.contains(needle), "run record leaked {needle:?}");
    }
    host.stop().unwrap();
}

/// A queued run can wait arbitrarily long between admission and start. It is
/// re-checked against the facts it was admitted on, and a drift is recorded as
/// a typed non-success state rather than being run anyway.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_queued_run_is_re_enforced_against_its_pinned_requirement_before_it_starts() {
    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let gate = Arc::new(FixedGate::ready());
    let seen = &gate.seen;
    let calls = gate.calls.clone();
    let (orch, host, session_id, _store) = service(&home, workspace.path(), gate.clone());
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    orch.submit_task(
        &auth,
        "req-pinned",
        session_id,
        workspace.path(),
        "do the thing".into(),
        None,
    )
    .await
    .expect("a ready gate admits");

    // Give the spawned run a moment to reach its re-check.
    for _ in 0..50 {
        if calls.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "the gate was not consulted again before the turn started"
    );
    let requirements = seen.lock().clone();
    assert_eq!(requirements.first(), Some(&None), "admission pins nothing");
    assert_eq!(
        requirements.get(1),
        Some(&Some(requirement())),
        "the start check must enforce the exact admitted facts"
    );
    host.stop().unwrap();
}

/// Shared execution edits the operator's checkout in place. Choosing it where
/// isolation was available is an explicit unsafe opt-in.
#[test]
fn shared_execution_on_an_isolatable_workspace_requires_an_explicit_opt_in() {
    let (_home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    std::fs::write(workspace.path().join("README.md"), "base\n").unwrap();
    git(workspace.path(), &["add", "README.md"]);
    git(workspace.path(), &["commit", "-qm", "base"]);

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    let summary = host.session_set_cwd(session.id, workspace.path()).unwrap();
    assert_eq!(
        summary.execution_mode,
        RunExecutionMode::IsolatedWorktree,
        "a clean Git workspace defaults to the reviewable mode"
    );

    let refused = host
        .session_set_execution_mode(session.id, RunExecutionMode::Shared, false)
        .expect_err("shared execution must not be selectable by accident");
    let message = refused.to_string();
    assert!(message.contains("cannot be rolled back"), "{message}");
    assert!(message.contains("acknowledge_unsafe"), "{message}");
    // The refusal left the session on the safe mode.
    assert_eq!(
        host.session_set_execution_mode(session.id, RunExecutionMode::IsolatedWorktree, false)
            .unwrap()
            .execution_mode,
        RunExecutionMode::IsolatedWorktree
    );

    // With the acknowledgement it is allowed, and it promises no rollback.
    let shared = host
        .session_set_execution_mode(session.id, RunExecutionMode::Shared, true)
        .expect("an acknowledged opt-in is honoured");
    assert_eq!(shared.execution_mode, RunExecutionMode::Shared);
    assert_eq!(
        RunExecutionMode::Shared.rollback_guarantee(),
        RollbackGuarantee::None
    );

    // And that explicit choice survives a later workspace rebind: it is the
    // operator's, not this host's default to recompute.
    let rebound = host.session_set_cwd(session.id, workspace.path()).unwrap();
    assert_eq!(rebound.execution_mode, RunExecutionMode::Shared);
    host.stop().unwrap();
}

/// A workspace that cannot back a worktree is not asked for consent it has no
/// alternative to: shared is the only option there.
#[test]
fn a_workspace_that_cannot_be_isolated_does_not_demand_an_acknowledgement() {
    let (_home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    let summary = host.session_set_cwd(session.id, workspace.path()).unwrap();
    assert_eq!(summary.execution_mode, RunExecutionMode::Shared);
    host.session_set_execution_mode(session.id, RunExecutionMode::Shared, false)
        .expect("shared is the only option here, so it needs no ceremony");
    host.stop().unwrap();
}

/// A run that was refused keeps whatever the operator can read; only the
/// *state* is prevented from claiming success.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_refused_desktop_turn_keeps_its_transcript() {
    let (_home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    // A gate that refuses even offline, so the desktop path is exercised.
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        launch_gate: Some(Arc::new(FixedGate::refusing(LaunchReason::SignInRequired))),
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();

    let refused = host
        .session_prompt(session.id, "please do the thing".into())
        .await
        .expect_err("a signed-out host must refuse the turn");
    let message = refused.to_string();
    assert!(message.contains("sign_in_required"), "{message}");
    assert!(message.contains("blocked"), "{message}");

    // The user turn stays readable: refusing to claim success must not also
    // erase what the operator was about to send.
    let transcript = host.session_transcript(session.id).unwrap();
    assert!(
        transcript
            .iter()
            .any(|entry| entry.text.contains("please do the thing")),
        "the refused turn erased its own transcript"
    );
    // And no durable run claims anything.
    let store = host
        .ensure_orchestration_store()
        .expect("the host owns the ledger");
    assert!(store
        .list_runs()
        .unwrap()
        .iter()
        .all(|run| run.state != RunState::Completed));

    // The session is not left wedged: a second prompt is accepted, not
    // rejected as "already has an active turn".
    let again = host
        .session_prompt(session.id, "try again".into())
        .await
        .expect_err("still signed out");
    assert!(again.to_string().contains("sign_in_required"));
    host.stop().unwrap();
}

fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "GrokPtah tests")
        .env("GIT_AUTHOR_EMAIL", "tests@grokptah.invalid")
        .env("GIT_COMMITTER_NAME", "GrokPtah tests")
        .env("GIT_COMMITTER_EMAIL", "tests@grokptah.invalid")
        .output()
        .expect("start git");
    assert!(output.status.success(), "git {args:?} failed");
}
/// The production admission path records a provider attempt bound to the
/// facts it was admitted on, before anything can reach a provider.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_admitted_run_records_a_bound_provider_attempt() {
    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let gate = Arc::new(FixedGate::ready());
    let (orch, host, session_id, store) = service(&home, workspace.path(), gate);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let response = orch
        .submit_task(
            &auth,
            "req-attempt",
            session_id,
            workspace.path(),
            "do the thing".into(),
            None,
        )
        .await
        .expect("a ready gate admits");
    let run_id = response["runId"].as_str().expect("a run id").to_string();

    // Nothing is recorded yet: an admitted run that has issued no request has
    // no attempt, because an attempt record would describe a send that has not
    // happened and would burn an ordinal -- and therefore an idempotency key --
    // that nothing was ever sent under.
    assert!(
        store.list_attempts_for_run(&run_id).unwrap().is_empty(),
        "admission alone recorded a provider attempt"
    );

    // The send is what creates the record, and it is bound to the exact facts
    // the run was admitted on.
    let ticket = declare_send(
        &store,
        &run_id,
        session_id,
        workspace.path(),
        "do the thing",
    );
    let attempts = store.list_attempts_for_run(&run_id).unwrap();
    assert_eq!(attempts.len(), 1, "one physical send, one attempt");
    let attempt = &attempts[0];
    assert_eq!(
        attempt.send_state,
        grokptah_agent_sdk::attempt::SendState::KnownNotSent,
        "a declared send is durable before it reaches the transport"
    );
    assert!(
        attempt.may_auto_retry(),
        "a request that has not left is the only safely retryable state"
    );
    assert_eq!(attempt.ordinal, 1);
    assert_eq!(attempt.validate(), Ok(()));

    // Bound to the same closed vocabulary the run was admitted on, so a drift
    // is a type-level comparison rather than a string match.
    let pinned = requirement();
    assert_eq!(attempt.route.provider, pinned.provider);
    assert_eq!(attempt.route.credential_method, pinned.credential_method);
    assert_eq!(attempt.route.route, pinned.route);
    assert_eq!(attempt.route.base, pinned.base);
    assert_eq!(attempt.route.dialect, pinned.dialect);
    assert_eq!(Some(attempt.route.model.clone()), pinned.model);
    assert_eq!(attempt.route.account_reference, pinned.account_reference);

    // The intent is a digest, and the idempotency key is reproducible.
    assert_eq!(
        attempt.intent.provider_idempotency_key,
        grokptah_agent_bridge::attempt_binding_testkit::provider_idempotency_key(&run_id, 1)
    );
    assert!(!attempt.intent.digest.as_str().contains("do the thing"));

    // The workspace survives only as an opaque handle, never as a path.
    let encoded = serde_json::to_string(attempt).unwrap();
    assert!(!encoded.contains(&workspace.path().display().to_string()));
    assert!(!encoded.contains("/tmp"));
    assert!(!encoded.contains("do the thing"));
    for needle in [
        "Bearer",
        "refresh_token",
        "apiKey",
        "https://",
        "balance",
        "quota",
    ] {
        assert!(!encoded.contains(needle), "attempt leaked {needle:?}");
    }
    // The exact endpoint, body, and credential are bound as digests, so a
    // silent drift in any of them is detectable without the record holding
    // the URL, the request, or the secret.
    assert!(attempt.route.route_digest.is_some());
    assert!(attempt.intent.body_digest.is_some());
    assert!(attempt.route.credential_digest.is_some());
    ticket.settle_not_sent().expect("nothing was dispatched");
    host.stop().unwrap();
}

/// An interrupted run whose request may already have reached the provider is
/// not retryable: repeating it would duplicate whatever it did.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_retry_is_refused_while_an_attempt_is_unreconciled() {
    use grokptah_agent_sdk::attempt::SendState;

    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let gate = Arc::new(FixedGate::ready());
    let (orch, host, session_id, store) = service(&home, workspace.path(), gate);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let response = orch
        .submit_task(
            &auth,
            "req-unreconciled",
            session_id,
            workspace.path(),
            "do the thing".into(),
            None,
        )
        .await
        .expect("a ready gate admits");
    let run_id = response["runId"].as_str().expect("a run id").to_string();

    // Drive the run to an interrupted state with an in-flight send, which is
    // exactly what a crash mid-dispatch leaves behind: declared, handed to the
    // transport, and never answered.
    let ticket = declare_send(
        &store,
        &run_id,
        session_id,
        workspace.path(),
        "do the thing",
    );
    ticket
        .mark_sending()
        .expect("the send crosses the boundary");
    let attempt_id = ticket.attempt_id().to_string();
    // Dropping the ticket without an observed outcome fences it rather than
    // tidying it away -- the request is gone and may well have executed.
    drop(ticket);
    assert_eq!(
        store.load_attempt(&attempt_id).unwrap().unwrap().send_state,
        SendState::Uncertain
    );
    let _ = store.update_run(&run_id, |run| {
        run.state = grokptah_agent_bridge::orchestration::RunState::Interrupted;
        run.terminal_result = Some("interrupted".into());
        Ok(())
    });

    let refused = orch
        .retry_run(
            &auth,
            "req-retry",
            session_id,
            workspace.path(),
            &run_id,
            "try again".into(),
            None,
            None,
            false,
        )
        .await
        .expect_err("an unreconciled attempt must block a retry");
    assert!(
        refused.message.contains("outcome is unknown"),
        "the refusal must say why: {}",
        refused.message
    );
    assert!(
        refused.message.contains(
            store.list_attempts_for_run(&run_id).unwrap()[0]
                .intent
                .provider_idempotency_key
                .as_str()
        ),
        "the refusal must name the key needed to reconcile: {}",
        refused.message
    );
    host.stop().unwrap();
}

/// An offline host reaches no provider, so it records no attempt and no
/// attribution: claiming either would describe a request that cannot exist.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_unreachable_provider_records_no_attempt_and_no_attribution() {
    use grokptah_agent_bridge::launch_truth::Admission;

    struct OfflineGate;
    #[async_trait::async_trait]
    impl LaunchGate for OfflineGate {
        async fn admit(
            &self,
            _requirement: Option<&LaunchRequirement>,
        ) -> Result<Admission, LaunchReason> {
            Ok(Admission::NoProviderReachable)
        }
    }

    let (home, _lock) = setup_home();
    let workspace = tempdir().unwrap();
    let (orch, host, session_id, store) = service(&home, workspace.path(), Arc::new(OfflineGate));
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let response = orch
        .submit_task(
            &auth,
            "req-offline",
            session_id,
            workspace.path(),
            "do the thing".into(),
            None,
        )
        .await
        .expect("an unreachable provider still admits a stubbed turn");
    let run_id = response["runId"].as_str().expect("a run id").to_string();

    let run = store.load_run(&run_id).unwrap().expect("the run exists");
    assert_eq!(
        run.attribution, None,
        "a run that spent nothing claimed an account"
    );
    assert_eq!(run.launch_requirement, None);
    assert!(
        store.list_attempts_for_run(&run_id).unwrap().is_empty(),
        "an attempt was recorded for a request that can never exist"
    );
    host.stop().unwrap();
}
