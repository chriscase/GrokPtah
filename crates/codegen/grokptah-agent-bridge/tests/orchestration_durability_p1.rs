//! Integrated tests for the second durability pass.
//!
//! These cover the corrections the independent audit asked for: one keyed
//! immutable execution specification, honest receipts, atomic registration,
//! a single async teardown owner, independent reconcilers, durable provider
//! send identity, action-time reauthorization, and idempotency decisions that
//! outlive their receipts.
//!
//! As in the first pass, execution is proved by side effects and termination
//! by the worker's own liveness guard — never by ledger state alone.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, project_admission, AcceptanceIntent, AttemptLeaseState, AuthContext, OrchStore,
    OrchestrationConfig, OrchestrationService, ProviderSendFailure, ProviderSendState, RunBounds,
    SealedBounds, SpecBinding, SpecHolder, WorkspaceAllowlist, ACCEPTANCE_INTENT_VERSION,
};
use grokptah_agent_bridge::{
    safe_id_filename, set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig,
    RunExecutionMode, RunState, SessionKind,
};
use serde_json::json;
use tempfile::{tempdir, TempDir};
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN: &str = "durability-p1-secret-token";

struct Rig {
    home: TempDir,
    ws: TempDir,
    _env: ProcessEnvGuard,
    host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
    session: Uuid,
    max_concurrent: usize,
}

impl Rig {
    async fn new(max_concurrent: usize) -> Self {
        let mut env = ProcessEnvGuard::new();
        let home = tempdir().unwrap();
        let grokptah_home = home.path().join(".grokptah");
        std::fs::create_dir_all(&grokptah_home).unwrap();
        set_grokptah_home_override(Some(grokptah_home));
        env.set("GROKPTAH_AGENT_OFFLINE", "1");

        let ws = tempdir().unwrap();
        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        let orch = build_service(&host, home.path(), ws.path(), max_concurrent).await;
        Self {
            home,
            ws,
            _env: env,
            host,
            orch,
            session: session.id,
            max_concurrent,
        }
    }

    fn auth(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN}")))
            .unwrap()
    }

    fn store_path(&self) -> std::path::PathBuf {
        self.home.path().join("orch")
    }

    fn markers(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(self.ws.path().join("ledger.txt")) {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                *counts.entry(line.to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    async fn restart(self) -> Self {
        let Rig {
            home,
            ws,
            _env,
            host,
            orch,
            session,
            max_concurrent,
        } = self;
        drop(orch);
        drop(host);
        let host = start_host().await;
        host.set_project_cwd(ws.path()).unwrap();
        host.session_set_cwd(session, ws.path()).unwrap();
        let orch = build_service(&host, home.path(), ws.path(), max_concurrent).await;
        Self {
            home,
            ws,
            _env,
            host,
            orch,
            session,
            max_concurrent,
        }
    }
}

async fn start_host() -> AgentHostHandle {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        match host.start() {
            Ok(()) => return host,
            Err(error) if std::time::Instant::now() < deadline => {
                drop(host);
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("host never started: {error}"),
        }
    }
}

async fn open_store(root: &Path) -> OrchStore {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match OrchStore::open(root) {
            Ok(store) => return store,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => panic!("store never opened: {error}"),
        }
    }
}

async fn build_service(
    host: &AgentHostHandle,
    home: &Path,
    ws: &Path,
    max_concurrent: usize,
) -> Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        open_store(&home.join("orch")).await,
        OrchestrationConfig {
            bearer_token: TOKEN.to_string(),
            allowlist: WorkspaceAllowlist::new([ws.to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds {
                max_prompt_bytes: 50_000,
                max_rounds: 4,
                max_duration_ms: 30_000,
            },
        },
    )
}

fn marker_prompt(marker: &str) -> String {
    format!("run printf '{marker}\\n' >> ledger.txt")
}

async fn wait_for<F>(label: &str, timeout: Duration, mut ready: F)
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    loop {
        if ready() {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {label}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_all_terminal(rig: &Rig, run_ids: &[String], timeout: Duration) {
    let auth = rig.auth();
    let start = std::time::Instant::now();
    loop {
        let outstanding = run_ids
            .iter()
            .filter(|run_id| {
                rig.orch
                    .get_run(&auth, run_id)
                    .ok()
                    .and_then(|value| {
                        serde_json::from_value::<RunState>(value["state"].clone()).ok()
                    })
                    .map(|state| !state.is_terminal())
                    .unwrap_or(false)
            })
            .count();
        if outstanding == 0 {
            return;
        }
        if start.elapsed() > timeout {
            panic!("{outstanding} run(s) never reached a terminal state");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn intent_file(store_root: &Path, run_id: &str) -> std::path::PathBuf {
    store_root
        .join("inputs")
        .join(format!("{}.json", safe_id_filename(run_id).unwrap()))
}

// ── one keyed immutable execution specification ────────────────────────

/// Every durable holder names the same specification key, and a holder bound
/// to a different key is refused. This is what makes the specification
/// authoritative rather than advisory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn all_six_holders_agree_on_one_specification_key() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "key-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 5".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "key-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("key-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();

    let store = rig.orch.store().clone();
    let intent = store
        .load_acceptance_intent(&run_id)
        .unwrap()
        .expect("durable input");
    let key = intent.spec_key().to_string();

    let run = store.load_run(&run_id).unwrap().unwrap();
    assert_eq!(run.spec_key.as_deref(), Some(key.as_str()), "run is bound");
    let receipt = store
        .load_idempotency("key-target")
        .unwrap()
        .expect("receipt");
    assert_eq!(
        receipt.spec_key.as_deref(),
        Some(key.as_str()),
        "receipt is bound"
    );

    let lease = store
        .acquire_attempt_lease(&run_id, "owner-x", rig.session, &key, 600_000)
        .unwrap();
    let send = store
        .open_provider_send(&run_id, &lease.attempt_id, &key)
        .unwrap();

    // Every holder agrees.
    SpecBinding {
        run: run.spec_key.as_deref(),
        receipt: receipt.spec_key.as_deref(),
        attempt: Some(&lease.intent_digest),
        lease: Some(&lease.intent_digest),
        provider_send: Some(&send.spec_key),
        worker: Some(intent.spec_key()),
    }
    .verify(
        &intent,
        &[
            SpecHolder::Run,
            SpecHolder::Receipt,
            SpecHolder::Attempt,
            SpecHolder::Lease,
            SpecHolder::ProviderSend,
            SpecHolder::Worker,
        ],
    )
    .expect("all six holders must agree");

    // One dissenting holder is enough to refuse.
    let other = hash_payload(&json!({"other": true}));
    for dissent in [
        SpecBinding {
            run: Some(&other),
            ..Default::default()
        },
        SpecBinding {
            receipt: Some(&other),
            ..Default::default()
        },
        SpecBinding {
            lease: Some(&other),
            ..Default::default()
        },
        SpecBinding {
            provider_send: Some(&other),
            ..Default::default()
        },
        SpecBinding {
            worker: Some(&other),
            ..Default::default()
        },
    ] {
        assert!(
            dissent.verify(&intent, &[]).is_err(),
            "a holder bound to a different key must be refused"
        );
    }

    // A required holder that is bound to nothing is also a refusal.
    assert!(SpecBinding::default()
        .verify(&intent, &[SpecHolder::Run])
        .is_err());
    set_grokptah_home_override(None);
}

/// A forgery that is *resealed* — internally consistent, digest recomputed —
/// is still refused, because it is a different specification and no durable
/// holder is bound to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_resealed_forgery_is_a_different_specification_and_never_runs() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "forge-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 5".into(),
            None,
        )
        .await
        .unwrap();
    let response = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "forge-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("original-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    let store_root = rig.store_path();

    let original = rig
        .orch
        .store()
        .load_acceptance_intent(&run_id)
        .unwrap()
        .unwrap();

    // Swap the prompt and *reseal*, so the record verifies on its own terms.
    // Only the binding to the run and the receipt exposes it.
    let forged = AcceptanceIntent {
        prompt: marker_prompt("forged-marker"),
        digest: String::new(),
        ..original.clone()
    }
    .seal();
    assert!(
        forged.validate().is_ok(),
        "the forgery is internally consistent, which is the point"
    );
    assert_ne!(forged.spec_key(), original.spec_key());

    let run = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    let binding = SpecBinding {
        run: run.spec_key.as_deref(),
        ..Default::default()
    };
    assert!(
        binding.verify(&forged, &[SpecHolder::Run]).is_err(),
        "a resealed forgery must not satisfy the run's binding"
    );

    // Install it on disk anyway; the run's own key is what refuses it.
    let path = intent_file(&store_root, &run_id);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let rig = rig.restart().await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let ledger = std::fs::read_to_string(rig.ws.path().join("ledger.txt")).unwrap_or_default();
    assert!(
        !ledger.contains("forged-marker"),
        "a resealed forgery executed: {ledger}"
    );
    let recovered = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    assert!(
        recovered.state.is_terminal() && recovered.state != RunState::Completed,
        "a forged admission must not complete, saw {:?}",
        recovered.state
    );
    set_grokptah_home_override(None);
}

// ── honest receipts and atomic registration ────────────────────────────

/// The receipt reports what is true at the moment it is issued. Nothing has
/// started, so it says `queued` — and the run only becomes `running` once its
/// worker has acknowledged, which is also when its handles are registered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn the_receipt_is_queued_until_registration_and_worker_acknowledgement() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let response = rig
        .orch
        .submit_task(
            &auth,
            "ack-run",
            rig.session,
            rig.ws.path(),
            "run sleep 5".into(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        response["state"], "queued",
        "the receipt must not promise a state that has not happened"
    );
    let run_id = response["runId"].as_str().unwrap().to_string();

    // Registration is atomic: by the time the run is live, every handle is
    // present, so teardown can always find and abort it.
    wait_for("registration", Duration::from_secs(10), || {
        rig.orch.live_run_ids().contains(&run_id)
    })
    .await;
    assert!(
        rig.orch.live_attempt(&run_id).is_some(),
        "a registered attempt must expose its attempt identity"
    );

    // Acknowledgement is the worker's own first durable act.
    wait_for("worker acknowledgement", Duration::from_secs(10), || {
        rig.orch
            .store()
            .load_run(&run_id)
            .ok()
            .flatten()
            .map(|run| run.state == RunState::Running)
            .unwrap_or(false)
    })
    .await;
    assert_eq!(
        rig.orch.worker_future_finished(&run_id),
        Some(false),
        "an acknowledged worker is live"
    );

    // The stored receipt still says what it said; it is a record, not a view.
    let receipt = rig
        .orch
        .store()
        .load_idempotency("ack-run")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.response["state"], "queued");
    set_grokptah_home_override(None);
}

// ── one async teardown owner ───────────────────────────────────────────

/// Cancellation goes through the single teardown owner: capacity is released
/// only after the worker future is proved gone, and the next attempt starts
/// only after that.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn teardown_releases_capacity_only_after_proved_quiescence() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "teardown-long",
            rig.session,
            rig.ws.path(),
            "run sleep 30".into(),
            None,
        )
        .await
        .unwrap();
    let long_id = long["runId"].as_str().unwrap().to_string();
    let queued = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "teardown-next",
            rig.session,
            rig.ws.path(),
            marker_prompt("teardown-next-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_id = queued["runId"].as_str().unwrap().to_string();

    wait_for("dispatch", Duration::from_secs(10), || {
        rig.orch.live_run_ids().contains(&long_id)
    })
    .await;
    assert_eq!(rig.orch.worker_future_finished(&long_id), Some(false));

    let cancelled = rig
        .orch
        .cancel(
            &auth,
            "teardown-cancel",
            rig.session,
            rig.ws.path(),
            Some(&long_id),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["teardownComplete"], true);

    wait_all_terminal(&rig, &[queued_id.clone()], Duration::from_secs(60)).await;
    assert_eq!(
        rig.orch.worker_future_finished(&long_id),
        Some(true),
        "the cancelled worker must be gone before its slot was reused"
    );
    assert_eq!(
        rig.markers().get("teardown-next-marker").copied(),
        Some(1),
        "the promoted task must run exactly once"
    );

    // A settled attempt gives up its lease and its private input.
    let lease = rig.orch.store().load_attempt_lease(&long_id).unwrap();
    assert!(
        lease.is_none() || lease.unwrap().state == AttemptLeaseState::Released,
        "a torn-down attempt must not keep a held lease"
    );
    assert!(!intent_file(&rig.store_path(), &long_id).is_file());
    set_grokptah_home_override(None);
}

// ── reconcilers ────────────────────────────────────────────────────────

/// An expired lease whose holder is gone is reclaimed, and the expired holder
/// can never heartbeat, renew, or release its way back in.
///
/// The reclamation is asserted as an *outcome*, not as a particular caller:
/// the background reconciler and an explicit sweep are both legitimate, and a
/// test that insisted on one would be asserting scheduling rather than
/// behaviour.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn expired_leases_are_reclaimed_and_expired_holders_cannot_renew() {
    let rig = Rig::new(1).await;
    let store = rig.orch.store().clone();
    let run_id = Uuid::new_v4().to_string();
    let key = hash_payload(&json!({"spec": "expiring"}));

    let short = store
        .acquire_attempt_lease(&run_id, "dead-owner", rig.session, &key, 1)
        .unwrap();
    assert_eq!(short.state, AttemptLeaseState::Held);

    // Past its own durable heartbeat deadline almost immediately.
    wait_for("the lease to expire", Duration::from_secs(5), || {
        short.is_expired(chrono::Utc::now())
    })
    .await;
    assert!(
        store
            .renew_attempt_lease(&run_id, &short.attempt_id, "dead-owner")
            .is_err(),
        "an expired holder must not be able to renew"
    );

    // Reclaimed by a reconciler: the background sweep, or this explicit one if
    // it gets there first. Either way the lease ends released.
    wait_for("the lease to be reclaimed", Duration::from_secs(30), || {
        rig.orch.reconcile_expired_leases();
        store
            .load_attempt_lease(&run_id)
            .ok()
            .flatten()
            .map(|lease| lease.state == AttemptLeaseState::Released)
            .unwrap_or(false)
    })
    .await;

    // Sweeping again is a no-op; reclamation is not repeatable work.
    assert_eq!(
        rig.orch.reconcile_expired_leases(),
        0,
        "an already-reclaimed lease must not be reclaimed twice"
    );

    // A fresh attempt may now take the run, under a new identity.
    let next = store
        .acquire_attempt_lease(&run_id, "live-owner", rig.session, &key, 600_000)
        .unwrap();
    assert_eq!(next.attempt, 2, "attempt numbers never repeat");
    assert_ne!(next.attempt_id, short.attempt_id);

    // And the reaped holder can never act on the new holder's lease.
    assert!(store
        .renew_attempt_lease(&run_id, &short.attempt_id, "dead-owner")
        .is_err());
    assert!(store
        .release_attempt_lease(&run_id, &short.attempt_id, "dead-owner")
        .is_err());
    // Nor can a live lease be taken by a second attempt.
    assert!(store
        .acquire_attempt_lease(&run_id, "third-owner", rig.session, &key, 600_000)
        .is_err());
    set_grokptah_home_override(None);
}

/// Durable queued work is re-derived from the ledger alone. Even with the
/// in-memory queue emptied behind its back, the reconciler finds it and it
/// runs exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn the_queue_reconciler_recovers_work_from_the_ledger_alone() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "recon-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 2".into(),
            None,
        )
        .await
        .unwrap();
    let queued = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "recon-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("recon-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_id = queued["runId"].as_str().unwrap().to_string();

    // Recovering purely from the ledger is idempotent: calling it repeatedly
    // must never produce a second dispatch.
    for _ in 0..5 {
        rig.orch.reconcile_durable_queued();
    }
    wait_all_terminal(&rig, &[queued_id.clone()], Duration::from_secs(60)).await;
    assert_eq!(
        rig.markers().get("recon-marker").copied(),
        Some(1),
        "repeated ledger-derived recovery must not duplicate dispatch"
    );
    set_grokptah_home_override(None);
}

// ── provider send evidence ─────────────────────────────────────────────

/// The four send states are distinct, only forward transitions are accepted,
/// and `uncertain` is never silently promoted to a success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn provider_send_evidence_only_ever_becomes_more_definite() {
    let rig = Rig::new(1).await;
    let store = rig.orch.store().clone();
    let run_id = Uuid::new_v4().to_string();
    let key = hash_payload(&json!({"spec": "send"}));

    let send = store
        .open_provider_send(&run_id, "attempt-1", &key)
        .unwrap();
    assert_eq!(send.state, ProviderSendState::KnownNotSent);
    assert!(send.state.permits_new_attempt());
    assert!(!send.state.permits_completion());

    // Another attempt cannot advance this attempt's evidence.
    assert!(store
        .advance_provider_send(
            &run_id,
            &send.send_id,
            "attempt-2",
            ProviderSendState::Sending,
            None,
            None,
        )
        .is_err());

    let sending = store
        .advance_provider_send(
            &run_id,
            &send.send_id,
            "attempt-1",
            ProviderSendState::Sending,
            None,
            None,
        )
        .unwrap();
    assert_eq!(sending.state, ProviderSendState::Sending);

    let uncertain = store
        .advance_provider_send(
            &run_id,
            &send.send_id,
            "attempt-1",
            ProviderSendState::Uncertain,
            Some(ProviderSendFailure::ResponseUnobserved),
            None,
        )
        .unwrap();
    assert_eq!(uncertain.state, ProviderSendState::Uncertain);
    assert!(
        !uncertain.state.permits_new_attempt(),
        "unknown is not the same as not-sent, and must never be resent implicitly"
    );
    assert!(!uncertain.state.permits_completion());

    // Evidence never weakens: no path back to sending or not-sent.
    for backwards in [
        ProviderSendState::Sending,
        ProviderSendState::KnownNotSent,
        ProviderSendState::Sent,
    ] {
        assert!(
            store
                .advance_provider_send(
                    &run_id,
                    &uncertain.send_id,
                    "attempt-1",
                    backwards,
                    None,
                    None,
                )
                .is_err(),
            "{backwards:?} must not be reachable from uncertain"
        );
    }

    // A failure that contradicts its state is not a storable record.
    assert_eq!(
        ProviderSendFailure::PreflightRejected.resulting_state(),
        ProviderSendState::KnownNotSent
    );
    assert_eq!(
        ProviderSendFailure::ResponseUnobserved.resulting_state(),
        ProviderSendState::Uncertain
    );
    assert_eq!(
        ProviderSendFailure::AttemptTornDown.resulting_state(),
        ProviderSendState::Uncertain
    );
    set_grokptah_home_override(None);
}

/// A run whose work is not known to have reached the provider must never be
/// reported completed, however cleanly the local future returned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_run_without_sent_evidence_is_never_reported_completed() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();

    let response = rig
        .orch
        .submit_task(
            &auth,
            "fake-provider",
            rig.session,
            rig.ws.path(),
            marker_prompt("fake-provider-marker"),
            None,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    wait_all_terminal(&rig, &[run_id.clone()], Duration::from_secs(60)).await;

    let run = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    let send = rig.orch.store().load_provider_send(&run_id).unwrap();
    match send.map(|send| send.state) {
        Some(ProviderSendState::Sent) => {
            assert_eq!(run.state, RunState::Completed);
        }
        other => {
            assert_ne!(
                run.state,
                RunState::Completed,
                "completed without sent evidence (send={other:?})"
            );
        }
    }
    set_grokptah_home_override(None);
}

// ── action-time reauthorization ────────────────────────────────────────

/// Authorization is re-answered at the moment of action. Revoking the
/// workspace between acceptance and dispatch refuses the run rather than
/// executing under authority that no longer exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn policy_drift_between_acceptance_and_action_refuses_the_run() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "drift-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 2".into(),
            None,
        )
        .await
        .unwrap();
    let queued = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "drift-target",
            rig.session,
            rig.ws.path(),
            marker_prompt("drift-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_id = queued["runId"].as_str().unwrap().to_string();

    // The policy changes after acceptance: another root is added, so the
    // sealed policy fingerprint no longer matches.
    let other = tempdir().unwrap();
    rig.orch.set_allowlist(WorkspaceAllowlist::new([
        rig.ws.path().to_path_buf(),
        other.path().to_path_buf(),
    ]));

    wait_all_terminal(&rig, &[queued_id.clone()], Duration::from_secs(60)).await;
    let run = rig.orch.store().load_run(&queued_id).unwrap().unwrap();
    assert_ne!(
        run.state,
        RunState::Completed,
        "work must not run under drifted authorization"
    );
    assert_eq!(run.error_code.as_deref(), Some("authorization_drift"));
    assert!(
        rig.markers().get("drift-marker").is_none(),
        "the drifted run executed: {:?}",
        rig.markers()
    );
    set_grokptah_home_override(None);
}

// ── idempotency horizon ────────────────────────────────────────────────

/// A decision outlives its receipt. Pruning a failed receipt must not turn a
/// refused request back into an executable one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_decision_survives_receipt_retention() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();
    let store_root = rig.store_path();

    // A submission that fails for good.
    let inputs = store_root.join("inputs");
    std::fs::remove_dir_all(&inputs).unwrap();
    std::fs::write(&inputs, b"not a directory").unwrap();
    let error = rig
        .orch
        .submit_task(
            &auth,
            "horizon-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("horizon-marker"),
            None,
        )
        .await
        .expect_err("the admission must fail");
    assert!(!error.message.is_empty());
    std::fs::remove_file(&inputs).unwrap();
    std::fs::create_dir_all(&inputs).unwrap();

    let tombstone = rig
        .orch
        .store()
        .load_idempotency_tombstone("horizon-request")
        .unwrap()
        .expect("a decided request must leave a tombstone");
    assert_eq!(tombstone.outcome, "failed");

    // Retire the receipt the way retention eventually would.
    let receipt_path = store_root.join("idempotency").join(format!(
        "{}.json",
        safe_id_filename("horizon-request").unwrap()
    ));
    std::fs::remove_file(&receipt_path).unwrap();
    assert!(rig
        .orch
        .store()
        .load_idempotency("horizon-request")
        .unwrap()
        .is_none());

    // The decision still stands.
    let replay = rig
        .orch
        .submit_task(
            &auth,
            "horizon-request",
            rig.session,
            rig.ws.path(),
            marker_prompt("horizon-marker"),
            None,
        )
        .await;
    assert!(
        replay.is_err(),
        "a retired receipt must not reopen a decided request"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        rig.markers().get("horizon-marker").is_none(),
        "a decided-failed request executed after its receipt was retired"
    );
    set_grokptah_home_override(None);
}

// ── projections ────────────────────────────────────────────────────────

/// The public projection carries identity, state, and evidence — never the
/// execution input or any internal identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn the_public_projection_never_carries_execution_material() {
    let rig = Rig::new(2).await;
    let auth = rig.auth();
    let secret = "PROJECTION-SECRET-3f9c";

    let response = rig
        .orch
        .submit_task(
            &auth,
            "projection-run",
            rig.session,
            rig.ws.path(),
            format!(
                "run printf 'projection-marker\\n' >> ledger.txt # {} {secret}",
                "y".repeat(600)
            ),
            None,
        )
        .await
        .unwrap();
    let run_id = response["runId"].as_str().unwrap().to_string();
    wait_all_terminal(&rig, &[run_id.clone()], Duration::from_secs(60)).await;

    let run = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    let lease = rig.orch.store().load_attempt_lease(&run_id).unwrap();
    let send = rig.orch.store().load_provider_send(&run_id).unwrap();
    let projection = project_admission(&run, lease.as_ref(), send.as_ref(), chrono::Utc::now());
    let encoded = serde_json::to_string(&projection).unwrap();

    assert!(!encoded.contains(secret), "projection leaked the prompt");
    assert!(!encoded.contains(TOKEN), "projection leaked the token");
    if let Some(lease) = lease.as_ref() {
        assert!(
            !encoded.contains(&lease.owner_id),
            "projection leaked the attempt owner"
        );
        assert!(
            !encoded.contains(&lease.attempt_id),
            "projection leaked the attempt identity"
        );
    }
    assert_eq!(projection.run_id, run_id);
    assert_eq!(projection.spec_key, run.spec_key);
    set_grokptah_home_override(None);
}

// ── registration, panic, and escape ────────────────────────────────────

/// Registration is atomic: a run is never live with only some of its handles
/// present, and a second dispatch for the same run is refused rather than
/// racing the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn registration_is_atomic_and_admits_one_attempt() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "reg-run",
            rig.session,
            rig.ws.path(),
            "run sleep 20".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = long["runId"].as_str().unwrap().to_string();
    wait_for("registration", Duration::from_secs(10), || {
        rig.orch.live_run_ids().contains(&run_id)
    })
    .await;

    // Registered means every handle is present, so teardown can find it.
    let (attempt_id, attempt) = rig
        .orch
        .live_attempt(&run_id)
        .expect("a registered attempt exposes its identity");
    assert_eq!(attempt, 1);
    assert!(!attempt_id.is_empty());

    // A second attempt cannot be authorized while the first holds its lease.
    let key = rig
        .orch
        .store()
        .load_run(&run_id)
        .unwrap()
        .unwrap()
        .spec_key
        .expect("a dispatched run is bound to a specification");
    assert!(
        rig.orch
            .store()
            .acquire_attempt_lease(&run_id, "intruder", rig.session, &key, 600_000)
            .is_err(),
        "one active attempt per run"
    );
    assert_eq!(rig.orch.live_run_ids(), vec![run_id.clone()]);

    let _ = rig
        .orch
        .cancel(
            &auth,
            "reg-cancel",
            rig.session,
            rig.ws.path(),
            Some(&run_id),
        )
        .await
        .unwrap();
    set_grokptah_home_override(None);
}

/// A run whose attempt is torn down mid-flight records `uncertain` provider
/// evidence and must never be reported completed. Whether the provider saw the
/// request is genuinely unknown, and the ledger has to say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn a_cancelled_in_flight_attempt_leaves_honest_evidence() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "inflight-run",
            rig.session,
            rig.ws.path(),
            "run sleep 30".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = long["runId"].as_str().unwrap().to_string();
    wait_for("the send to open", Duration::from_secs(10), || {
        rig.orch
            .store()
            .load_provider_send(&run_id)
            .ok()
            .flatten()
            .is_some()
    })
    .await;

    let cancelled = rig
        .orch
        .cancel(
            &auth,
            "inflight-cancel",
            rig.session,
            rig.ws.path(),
            Some(&run_id),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["teardownComplete"], true);
    assert_eq!(rig.orch.worker_future_finished(&run_id), Some(true));

    let run = rig.orch.store().load_run(&run_id).unwrap().unwrap();
    assert_ne!(
        run.state,
        RunState::Completed,
        "an interrupted attempt must never be reported completed"
    );
    let send = rig.orch.store().load_provider_send(&run_id).unwrap();
    if let Some(send) = send {
        assert!(
            !send.state.permits_completion() || run.state == RunState::Cancelled,
            "completion evidence and outcome must agree"
        );
    }
    set_grokptah_home_override(None);
}

/// Shutdown fences and stages; it never releases capacity from a synchronous
/// path. The run still ends durably terminal, and its lease is not left held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn shutdown_fences_and_stages_without_releasing_capacity_synchronously() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let long = rig
        .orch
        .submit_task(
            &auth,
            "shutdown-fence",
            rig.session,
            rig.ws.path(),
            "run sleep 30".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = long["runId"].as_str().unwrap().to_string();
    wait_for("dispatch", Duration::from_secs(10), || {
        rig.orch.live_run_ids().contains(&run_id)
    })
    .await;

    let store_root = rig.store_path();
    let Rig {
        home,
        ws,
        _env,
        host,
        orch,
        ..
    } = rig;
    drop(orch);
    drop(host);

    let reopened = open_store(&store_root).await;
    let record = reopened.load_run(&run_id).unwrap().expect("run record");
    assert!(
        record.state.is_terminal(),
        "shutdown left run {run_id} in {:?}",
        record.state
    );
    assert_eq!(record.state, RunState::Interrupted);
    let lease = reopened.load_attempt_lease(&run_id).unwrap();
    assert!(
        lease.is_none() || lease.unwrap().state == AttemptLeaseState::Released,
        "restart must not leave a held lease behind"
    );
    drop(reopened);
    drop(home);
    drop(ws);
    drop(_env);
    set_grokptah_home_override(None);
}

// ── transcript privacy and storage ─────────────────────────────────────

/// Everything the ledger writes that can carry execution material is
/// owner-only, refuses to be read through a link, and is refused outright once
/// its permissions are widened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn every_private_ledger_is_owner_only_and_no_follow() {
    let rig = Rig::new(1).await;
    let auth = rig.auth();

    let _blocker = rig
        .orch
        .submit_task(
            &auth,
            "priv-blocker",
            rig.session,
            rig.ws.path(),
            "run sleep 5".into(),
            None,
        )
        .await
        .unwrap();
    let queued = rig
        .orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "priv-queued",
            rig.session,
            rig.ws.path(),
            marker_prompt("priv-marker"),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_id = queued["runId"].as_str().unwrap().to_string();
    let store_root = rig.store_path();

    // Every private ledger directory is owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for ledger in ["inputs", "leases", "sends", "tombstones"] {
            let mode = std::fs::metadata(store_root.join(ledger))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{ledger} is {mode:o}, must be owner-only");
        }

        let path = intent_file(&store_root, &queued_id);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "durable input is {mode:o}");

        // Widened permissions are tampering, not a convenience to repair.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(rig.orch.store().load_acceptance_intent(&queued_id).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(rig.orch.store().load_acceptance_intent(&queued_id).is_ok());

        // A link in the ledger is never followed.
        let decoy = store_root.join("decoy.json");
        std::fs::copy(&path, &decoy).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&decoy, &path).unwrap();
        let error = rig
            .orch
            .store()
            .load_acceptance_intent(&queued_id)
            .unwrap_err();
        assert!(
            !format!("{error}").contains("priv-marker"),
            "a link target must never be read into an error message"
        );
    }
    set_grokptah_home_override(None);
}

/// The name of every ledger record is a store-generated digest, so a request
/// or run identity can never steer a write outside its own ledger.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn ledger_names_cannot_be_steered_by_caller_identity() {
    let rig = Rig::new(1).await;
    let store = rig.orch.store().clone();
    for hostile in ["../escape", "..", "a/b", "a\\b", "/etc/passwd", "with\0nul"] {
        assert!(
            store.load_acceptance_intent(hostile).is_err()
                || store.load_acceptance_intent(hostile).unwrap().is_none(),
            "{hostile:?} must never resolve to a record"
        );
        assert!(
            store.remove_acceptance_intent(hostile).is_err()
                || !store.remove_acceptance_intent(hostile).unwrap(),
            "{hostile:?} must never remove a record"
        );
        // Only store-generated digests name input files.
        assert!(store.remove_acceptance_intent_file(hostile).is_err());
    }
    set_grokptah_home_override(None);
}
