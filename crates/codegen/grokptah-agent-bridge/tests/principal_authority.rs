//! Adversarial coverage for the one host-issued principal and
//! authentication-generation fence (#477).
//!
//! Every test here is written from *outside* the crate, which is the position
//! an attacker and an SDK consumer are both in: the only way to obtain an
//! identity is to ask the service for one. Synthetic data only — no provider
//! calls, no network beyond the in-process loopback control server.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use grokptah_agent_bridge::orchestration::{
    AuthContext, AuthCredential, AuthorityOrigin, DelegationLimit, DurableAuthority, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState, WorkspaceAllowlist,
    COMPAT_PRIMARY_PRINCIPAL,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost,
    AgentHostHandle, HostConfig, McpControlClient, SessionKind,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

type HomeGuard = std::sync::MutexGuard<'static, ()>;

struct Fixture {
    _home: TempDir,
    _guard: HomeGuard,
    host: AgentHostHandle,
    workspace: TempDir,
    orch: Arc<OrchestrationService>,
    store_root: std::path::PathBuf,
}

const TOKEN: &str = "principal-fence-token";

fn fixture() -> Fixture {
    fixture_with(|_| {})
}

/// Build a service, running `prepare` against the store root *before* the store
/// is opened so a test can plant durable state (an authority record, a legacy
/// run) the way a previous process would have left it.
fn fixture_with(prepare: impl FnOnce(&Path)) -> Fixture {
    let guard = home_override_serial();
    let home = TempDir::new().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    // SAFETY: `home_override_serial` serializes process-global test mutations.
    unsafe { std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1") };
    let workspace = TempDir::new().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let store_root = home.path().join("orch");
    std::fs::create_dir_all(&store_root).unwrap();
    prepare(&store_root);
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(&store_root).unwrap(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    Fixture {
        _home: home,
        _guard: guard,
        host,
        workspace,
        orch,
        store_root,
    }
}

impl Fixture {
    fn session(&self) -> Uuid {
        let session = self.host.session_new_kind(SessionKind::Build).unwrap();
        self.host
            .session_set_cwd(session.id, self.workspace.path())
            .unwrap();
        session.id
    }

    fn auth(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN}")))
            .unwrap()
    }
}

fn planted_run(
    run_id: &str,
    session: Uuid,
    workspace: &Path,
    principal: Option<&str>,
) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        session_id: session,
        workspace: workspace.display().to_string(),
        request_id: format!("req-{run_id}"),
        client_id: principal.map(str::to_owned),
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
        prompt_preview: "synthetic".into(),
        start_seq: Some(1),
        end_seq: Some(2),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

/// Recursive directory copy: the durable bytes a restarted process would find.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn teardown() {
    set_grokptah_home_override(None);
}

// ── two principals, two credentials ─────────────────────────────────────────

/// Two named device credentials are two principals. Each owns the runs it
/// stamped, and neither can read the other's — and the refusal is the same one
/// an id that does not exist gets, so the read path is not an existence oracle.
#[test]
fn two_principals_cannot_read_each_others_runs() {
    let fx = fixture();
    fx.orch
        .set_auth_credentials(vec![
            AuthCredential::new("primary", TOKEN).unwrap(),
            AuthCredential::new("laptop", "laptop-token").unwrap(),
        ])
        .unwrap();
    let session = fx.session();

    // Identities must be issued after the rotation that installed them.
    let primary = fx.auth();
    let laptop = fx
        .orch
        .auth_header(Some("Bearer laptop-token"))
        .expect("second credential authenticates");
    assert_eq!(primary.principal(), COMPAT_PRIMARY_PRINCIPAL);
    assert_eq!(laptop.principal(), "laptop");
    assert_ne!(primary.principal(), laptop.principal());

    let store = fx.orch.store_unscoped();
    store
        .save_run(&planted_run(
            "run-primary",
            session,
            fx.workspace.path(),
            Some(COMPAT_PRIMARY_PRINCIPAL),
        ))
        .unwrap();
    store
        .save_run(&planted_run(
            "run-laptop",
            session,
            fx.workspace.path(),
            Some("laptop"),
        ))
        .unwrap();

    assert!(fx.orch.get_run(&primary, "run-primary").is_ok());
    assert!(fx.orch.get_run(&laptop, "run-laptop").is_ok());

    let foreign = fx.orch.get_run(&primary, "run-laptop").unwrap_err();
    let absent = fx.orch.get_run(&primary, "run-does-not-exist").unwrap_err();
    assert_eq!(
        (foreign.code.as_str(), foreign.message.as_str()),
        (absent.code.as_str(), absent.message.as_str()),
        "a foreign run and an unknown run must be byte-identical refusals"
    );

    // Listing must not surface what a direct fetch would refuse, or the list
    // becomes the oracle the per-run denial is careful not to be.
    let listed = fx
        .orch
        .list_runs_scoped(&primary, session, fx.workspace.path())
        .unwrap();
    let ids: Vec<&str> = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["runId"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["run-primary"], "listing leaked another principal");
    teardown();
}

/// The refusal for a foreign id, an unknown id and a malformed id must not be
/// separable by the work done either — a malformed id that skipped the store
/// lookup would be a cheap id-shape oracle.
#[test]
fn foreign_unknown_and_malformed_denials_are_equivalent() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    fx.orch
        .store_unscoped()
        .save_run(&planted_run(
            "run-other",
            session,
            fx.workspace.path(),
            Some("someone-else"),
        ))
        .unwrap();

    let cases = ["run-other", "run-absent", "../../etc/passwd"];
    let mut errors = Vec::new();
    let mut timings = Vec::new();
    for case in cases {
        // Warm, then measure a batch so a single scheduling hiccup does not
        // decide the comparison.
        let _ = fx.orch.get_run(&auth, case);
        let start = Instant::now();
        for _ in 0..200 {
            let error = fx.orch.get_run(&auth, case).unwrap_err();
            std::hint::black_box(&error);
        }
        timings.push(start.elapsed().as_secs_f64());
        errors.push(fx.orch.get_run(&auth, case).unwrap_err());
    }
    for error in &errors[1..] {
        assert_eq!(
            (errors[0].code.as_str(), errors[0].message.as_str()),
            (error.code.as_str(), error.message.as_str()),
            "denials must be byte- and status-identical across {cases:?}"
        );
    }
    let slowest = timings.iter().cloned().fold(f64::MIN, f64::max);
    let fastest = timings.iter().cloned().fold(f64::MAX, f64::min);
    // Bounded tolerance, not equality: this runs on shared CI hardware. A path
    // that skipped the store lookup entirely lands orders of magnitude apart,
    // which is what this is sized to catch.
    assert!(
        slowest <= fastest * 12.0,
        "denial timings diverge beyond tolerance: {timings:?} for {cases:?}"
    );
    teardown();
}

// ── rotation ────────────────────────────────────────────────────────────────

/// Every authority mutation invalidates identities issued before it, and a
/// policy change moves the policy revision while a credential change does not.
#[test]
fn rotation_invalidates_issued_identities() {
    let fx = fixture();
    let session = fx.session();

    let before = fx.auth();
    assert!(fx
        .orch
        .list_runs_scoped(&before, session, fx.workspace.path())
        .is_ok());
    let epoch_before = fx.orch.auth_epoch();
    let policy_before = fx.orch.policy_revision();

    // Credential rotation: epoch advances, policy revision does not.
    fx.orch.set_token(TOKEN.into()).unwrap();
    assert_eq!(fx.orch.auth_epoch(), epoch_before + 1);
    assert_eq!(fx.orch.policy_revision(), policy_before);
    let stale = fx
        .orch
        .list_runs_scoped(&before, session, fx.workspace.path())
        .unwrap_err();
    assert_eq!(
        stale.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::Unauthenticated
    );
    assert!(
        fx.auth().generation() != before.generation(),
        "a re-authenticated identity must carry the new generation"
    );

    // Policy rotation: both advance.
    let mid = fx.auth();
    fx.orch
        .set_allowlist(WorkspaceAllowlist::new([fx.workspace.path().to_path_buf()]))
        .unwrap();
    assert_eq!(fx.orch.policy_revision(), policy_before + 1);
    assert!(fx
        .orch
        .list_runs_scoped(&mid, session, fx.workspace.path())
        .is_err());

    // Owner change is a policy rotation too: it re-binds what every future
    // identity asserts.
    let after_policy = fx.auth();
    fx.orch.set_agent_owner_id("tenant-2".into()).unwrap();
    assert_eq!(fx.orch.policy_revision(), policy_before + 2);
    assert!(fx
        .orch
        .list_runs_scoped(&after_policy, session, fx.workspace.path())
        .is_err());
    assert_eq!(fx.auth().owner_id(), "tenant-2");
    teardown();
}

/// Removing a credential id and re-adding it must not revive the removed
/// credential's identities or let its work be claimed by the new registration.
#[test]
fn credential_removal_and_re_add_does_not_revive_old_work() {
    let fx = fixture();
    let session = fx.session();
    fx.orch
        .set_auth_credentials(vec![
            AuthCredential::new("primary", TOKEN).unwrap(),
            AuthCredential::new("laptop", "laptop-token").unwrap(),
        ])
        .unwrap();
    let first_laptop = fx.orch.auth_header(Some("Bearer laptop-token")).unwrap();
    let first_incarnation = fx
        .orch
        .scoped_reads(&first_laptop, session, fx.workspace.path())
        .unwrap()
        .identity();

    // Remove the credential entirely.
    fx.orch
        .set_auth_credentials(vec![AuthCredential::new("primary", TOKEN).unwrap()])
        .unwrap();
    assert!(
        fx.orch.auth_header(Some("Bearer laptop-token")).is_err(),
        "a removed credential must stop authenticating"
    );
    assert!(
        fx.orch
            .list_runs_scoped(&first_laptop, session, fx.workspace.path())
            .is_err(),
        "identities issued to the removed credential must be dead"
    );

    // Re-add the same id with the same secret.
    fx.orch
        .set_auth_credentials(vec![
            AuthCredential::new("primary", TOKEN).unwrap(),
            AuthCredential::new("laptop", "laptop-token").unwrap(),
        ])
        .unwrap();
    let second_laptop = fx.orch.auth_header(Some("Bearer laptop-token")).unwrap();
    let second_incarnation = fx
        .orch
        .scoped_reads(&second_laptop, session, fx.workspace.path())
        .unwrap()
        .identity();

    assert_eq!(
        first_incarnation["principal"], second_incarnation["principal"],
        "the wire principal is deliberately stable across re-registration"
    );
    assert_ne!(
        first_incarnation["scope"], second_incarnation["scope"],
        "a reused credential id must not address the previous incarnation's namespace"
    );
    assert!(
        fx.orch
            .list_runs_scoped(&first_laptop, session, fx.workspace.path())
            .is_err(),
        "re-adding the id must not resurrect the earlier identity"
    );
    teardown();
}

// ── exhaustion ──────────────────────────────────────────────────────────────

/// A generation at the ceiling refuses to rotate, and refusing changes nothing:
/// the previously installed credentials keep working exactly as they were.
#[test]
fn generation_exhaustion_changes_nothing() {
    // Adoption advances the epoch once, so persisting MAX-1 puts the live
    // generation exactly at the ceiling without 2^64 rotations.
    let fx = fixture_with(|root| {
        let record = DurableAuthority {
            authority: Uuid::new_v4(),
            epoch: u64::MAX - 1,
            policy_revision: 7,
        };
        std::fs::write(
            root.join("authority.json"),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    });
    let session = fx.session();
    assert_eq!(fx.orch.auth_epoch(), u64::MAX);
    assert_eq!(fx.orch.authority_origin(), AuthorityOrigin::Resumed);

    let auth = fx.auth();
    let before = fx
        .orch
        .list_runs_scoped(&auth, session, fx.workspace.path())
        .unwrap();

    let credential_rotation = fx.orch.set_token("replacement-token".into()).unwrap_err();
    assert!(
        credential_rotation.message.contains("generation exhausted"),
        "exhaustion must fail closed: {}",
        credential_rotation.message
    );
    let policy_rotation = fx
        .orch
        .set_allowlist(WorkspaceAllowlist::default())
        .unwrap_err();
    assert!(policy_rotation.message.contains("generation exhausted"));
    assert!(fx.orch.set_agent_owner_id("tenant-9".into()).is_err());

    // Nothing moved: the same identity still works, the replacement token does
    // not, and the allowlist still authorizes the original workspace.
    assert_eq!(fx.orch.auth_epoch(), u64::MAX);
    assert_eq!(fx.orch.policy_revision(), 7);
    assert!(fx
        .orch
        .auth_header(Some("Bearer replacement-token"))
        .is_err());
    assert_eq!(
        fx.orch
            .list_runs_scoped(&auth, session, fx.workspace.path())
            .unwrap(),
        before
    );
    teardown();
}

// ── restart ─────────────────────────────────────────────────────────────────

/// A restart resumes the lineage and advances past it, so no identity the
/// previous process issued is current — and a run id reused across the restart
/// does not carry the old identity's reach with it.
#[test]
fn restart_resumes_the_lineage_and_kills_pre_restart_identities() {
    let fx = fixture();
    let session = fx.session();
    let before_restart = fx.auth();
    let epoch_before = fx.orch.auth_epoch();
    assert_eq!(fx.orch.authority_origin(), AuthorityOrigin::Fresh);
    fx.orch
        .store_unscoped()
        .save_run(&planted_run(
            "run-survivor",
            session,
            fx.workspace.path(),
            Some(COMPAT_PRIMARY_PRINCIPAL),
        ))
        .unwrap();
    assert!(fx.orch.get_run(&before_restart, "run-survivor").is_ok());

    // Restart. The store takes an exclusive advisory lock for as long as any
    // handle lives, and supervisor threads hold their own clones, so a faithful
    // in-process restart copies the durable root and opens a second host over
    // it. That is exactly what a restarted process sees: the same bytes on
    // disk, none of the previous process's memory.
    let Fixture {
        _home,
        _guard,
        host,
        workspace,
        orch,
        store_root,
    } = fx;
    let restarted_root = store_root.with_file_name("orch-restarted");
    copy_tree(&store_root, &restarted_root);
    drop(orch);
    let restarted = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(&restarted_root).unwrap(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );

    assert_eq!(restarted.authority_origin(), AuthorityOrigin::Resumed);
    assert!(
        restarted.auth_epoch() > epoch_before,
        "a restart must advance past every epoch the previous process issued under"
    );
    let stale = restarted
        .get_run(&before_restart, "run-survivor")
        .unwrap_err();
    assert_eq!(
        stale.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::Unauthenticated,
        "a pre-restart identity must not become current merely because a process restarted"
    );
    // The record itself survives and is readable by a freshly issued identity.
    let after = restarted
        .auth_header(Some(&format!("Bearer {TOKEN}")))
        .unwrap();
    assert!(restarted.get_run(&after, "run-survivor").is_ok());
    teardown();
}

/// A durable authority record that cannot be trusted is not treated as a first
/// run: the host re-establishes a brand-new lineage and reports it, which
/// invalidates everything rather than silently re-attributing it.
#[test]
fn unreadable_authority_record_re_establishes_fail_closed() {
    let fx = fixture_with(|root| {
        std::fs::write(root.join("authority.json"), b"{ this is not json").unwrap();
    });
    assert_eq!(
        fx.orch.authority_origin(),
        AuthorityOrigin::ReestablishedFailClosed
    );
    // The host still serves: fail-closed here means "new lineage", not "no
    // service", and freshly issued identities work.
    let session = fx.session();
    let auth = fx.auth();
    assert!(fx
        .orch
        .list_runs_scoped(&auth, session, fx.workspace.path())
        .is_ok());
    teardown();
}

// ── legacy / unbound records ────────────────────────────────────────────────

/// Records written before client attribution existed belong to nobody. They are
/// quarantined — never handed to the current caller, never attributed to a
/// hard-coded desktop or MCP principal — and reported in aggregate so an
/// operator can migrate them deliberately.
#[test]
fn legacy_unbound_records_are_quarantined_not_attributed() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    let store = fx.orch.store_unscoped();
    store
        .save_run(&planted_run(
            "run-legacy",
            session,
            fx.workspace.path(),
            None,
        ))
        .unwrap();
    store
        .save_run(&planted_run(
            "run-blank",
            session,
            fx.workspace.path(),
            Some("   "),
        ))
        .unwrap();
    store
        .save_run(&planted_run(
            "run-mine",
            session,
            fx.workspace.path(),
            Some(COMPAT_PRIMARY_PRINCIPAL),
        ))
        .unwrap();

    let absent = fx.orch.get_run(&auth, "run-absent").unwrap_err();
    for quarantined in ["run-legacy", "run-blank"] {
        let error = fx.orch.get_run(&auth, quarantined).unwrap_err();
        assert_eq!(
            (absent.code.as_str(), absent.message.as_str()),
            (error.code.as_str(), error.message.as_str()),
            "{quarantined} must be refused exactly like an unknown id"
        );
        assert!(fx.orch.get_events(&auth, Some(quarantined), 0, 10).is_err());
        assert!(fx.orch.get_changes(&auth, quarantined).is_err());
    }
    assert!(fx.orch.get_run(&auth, "run-mine").is_ok());

    let report = fx.orch.quarantine_report(&auth).unwrap();
    assert_eq!(report["quarantine"]["unboundLegacyRecords"], 1);
    assert_eq!(report["quarantine"]["blankPrincipalRecords"], 1);
    assert_eq!(report["quarantine"]["total"], 2);
    assert_eq!(report["quarantine"]["authorityOrigin"], "fresh");
    let text = report.to_string();
    assert!(
        !text.contains("run-legacy") && !text.contains("run-blank"),
        "the operator report must carry counts, never record contents: {text}"
    );
    teardown();
}

// ── delegation ──────────────────────────────────────────────────────────────

/// Delegation is explicit, expiring, revision-bound and narrowing: a delegate
/// reads within the delegator's scope and can cross no effect boundary, and any
/// rotation revokes the grant.
#[test]
fn delegation_narrows_and_is_revoked_by_rotation() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();

    assert!(
        fx.orch
            .delegate(&auth, "helper", DelegationLimit::ReadOnlyWithinScope, 0)
            .is_err(),
        "a delegation with no lifetime is not a delegation"
    );
    let delegate = fx
        .orch
        .delegate(&auth, "helper", DelegationLimit::ReadOnlyWithinScope, 60)
        .unwrap();
    assert_eq!(delegate.principal(), "helper");
    assert_eq!(
        delegate.owner_id(),
        auth.owner_id(),
        "a delegation must not cross tenants"
    );

    // Reads inside the delegator's scope work.
    assert!(fx
        .orch
        .list_runs_scoped(&delegate, session, fx.workspace.path())
        .is_ok());
    // Effects do not.
    let effect = fx
        .orch
        .create_session(&delegate, fx.workspace.path(), None)
        .unwrap_err();
    assert_eq!(
        effect.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::ForbiddenScope,
        "a read-only delegate is authenticated but not permitted; telling it to \
         re-authenticate would loop forever"
    );
    // Nor can a delegate re-delegate and reset the clock.
    assert!(fx
        .orch
        .delegate(&delegate, "third", DelegationLimit::ReadOnlyWithinScope, 60)
        .is_err());

    // Rotation revokes the grant along with the identity it was minted from.
    fx.orch.set_token(TOKEN.into()).unwrap();
    assert!(fx
        .orch
        .list_runs_scoped(&delegate, session, fx.workspace.path())
        .is_err());
    teardown();
}

// ── the public surface ──────────────────────────────────────────────────────

/// The sanctioned public read surface hands out DTOs bound to a verified
/// principal: an opaque scope, a public workspace alias, and no native path or
/// secret anywhere in it.
#[test]
fn scoped_reads_expose_dtos_without_native_paths_or_raw_mutation() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    let reads = fx
        .orch
        .scoped_reads(&auth, session, fx.workspace.path())
        .unwrap();

    let identity = reads.identity();
    let text = identity.to_string();
    assert!(identity["workspaceAlias"]
        .as_str()
        .unwrap()
        .starts_with("ws1-"));
    assert!(identity["scope"].as_str().unwrap().starts_with("ps1-"));
    assert_eq!(identity["principal"], COMPAT_PRIMARY_PRINCIPAL);
    assert!(
        !text.contains(&fx.workspace.path().display().to_string()),
        "a read DTO must not carry a native workspace path: {text}"
    );
    assert!(!text.contains(TOKEN), "a read DTO must not carry a secret");
    assert_eq!(
        identity["sessionId"].as_str().unwrap(),
        session.to_string(),
        "the DTO reports the exact session the principal is bound to"
    );

    assert!(reads.runs().is_ok());
    assert!(reads.work().is_ok());
    assert!(reads.routines().is_ok());

    // The binding is fixed at construction, so a stale identity cannot mint one.
    fx.orch.set_token(TOKEN.into()).unwrap();
    assert!(fx
        .orch
        .scoped_reads(&auth, session, fx.workspace.path())
        .is_err());
    teardown();
}

// ── live delivery ───────────────────────────────────────────────────────────

/// Rotation revokes the live channel end to end.
///
/// The frame-boundary check itself — that frames already buffered under the
/// previous generation are dropped rather than delivered — is exercised
/// deterministically in `mcp_control::live_frame_authority`, because a terminal
/// run's durable page is queued in one batch and the transport can flush it
/// before a test's rotation lands. What this proves over real HTTP is the part
/// the transport cannot blur: an identity the host no longer honours cannot
/// open a live stream at all, and one it does honour can.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_rotation_revokes_the_live_channel() {
    let fx = fixture();
    let session = fx.session();
    let orch = fx.orch.clone();
    let workspace = fx.workspace.path().to_path_buf();
    let server = start_control_server(fx.orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
    client.initialize().await.unwrap();
    let transport_session = client.session_id().unwrap().to_string();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "live-rotation-submit",
                "session_id": session,
                "workspace": workspace.display().to_string(),
                "prompt": "write live-rotation.txt: observed",
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();

    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session,
                    "workspace": workspace.display().to_string(),
                    "run_id": run_id,
                }),
            )
            .await
            .unwrap();
        if run.structured["startSeq"].as_u64().is_some()
            && matches!(
                run.structured["state"].as_str(),
                Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
            )
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let live_url = |bearer: &str| {
        let mut url = reqwest::Url::parse(&format!("http://{}/mcp", server.addr)).unwrap();
        url.query_pairs_mut()
            .append_pair("session_id", &session.to_string())
            .append_pair("workspace", &workspace.display().to_string())
            .append_pair("run_id", &run_id);
        let bearer = bearer.to_string();
        (url, bearer)
    };

    // Before the rotation the channel opens and replays the run's own events.
    let (url, bearer) = live_url(TOKEN);
    let stream = reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let first = tokio::time::timeout(Duration::from_secs(10), stream.bytes_stream().next())
        .await
        .expect("live stream produced no frame")
        .expect("live stream closed before its first frame")
        .unwrap();
    let first = String::from_utf8_lossy(&first).to_string();
    assert!(
        first.contains("notifications/ptah_event") && first.contains(&run_id),
        "expected the run's own events, got: {first}"
    );

    // Rotate to a different secret. The old bearer is no longer honoured, so
    // the live channel refuses it before any stream is created.
    orch.set_token("rotated-live-token".into()).unwrap();
    let (url, bearer) = live_url(TOKEN);
    let refused = reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        401,
        "a revoked credential must not open a live stream"
    );

    // The rotated credential opens it again: revocation is targeted, not a
    // blanket outage.
    let (url, bearer) = live_url("rotated-live-token");
    let reopened = reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(reopened.status(), 200);

    server.stop();
    teardown();
}

// ── one fence ───────────────────────────────────────────────────────────────

/// Exactly one module may define principal/authority types.
///
/// The whole point of #477 is that there is one identity fence. Drafts #460 and
/// #474 each grew their own `AuthEpoch`, and #470/#471 their own
/// `VerifiedPrincipal`/scope concepts; merging those wholesale would leave the
/// tree with several. This walks the crate's own sources so a second fence
/// cannot reappear quietly.
#[test]
fn only_one_module_declares_principal_authority_types() {
    const FENCE: &str = "src/orchestration/authz.rs";
    const RESERVED: &[&str] = &[
        "struct AuthContext",
        "struct VerifiedPrincipal",
        "struct PrincipalScope",
        "struct AuthGeneration",
        "struct AuthEpoch",
        "struct AuthCredential",
        "struct ReadinessAuthority",
        "enum PrincipalKind",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let relative = path
                .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == FENCE {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for reserved in RESERVED {
                if text.contains(reserved) {
                    offenders.push(format!("{relative} declares `{reserved}`"));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a second principal fence has appeared; all authority types belong in {FENCE}: \
         {offenders:?}"
    );

    // And the fence really does declare them, so the scan above is not vacuous.
    let fence = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(FENCE)).unwrap();
    for reserved in [
        "struct AuthContext",
        "struct VerifiedPrincipal",
        "struct PrincipalScope",
    ] {
        assert!(
            fence.contains(reserved),
            "{FENCE} must declare `{reserved}`"
        );
    }
}
