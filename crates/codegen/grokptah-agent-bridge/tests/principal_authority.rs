//! Adversarial coverage for the one host-issued principal and
//! authentication-generation fence (#477).
//!
//! Every test here is written from *outside* the crate, which is the position
//! an attacker and an SDK consumer are both in: the only way to obtain an
//! identity is to ask the host for one. Synthetic data only — no provider
//! calls, no network beyond the in-process loopback control server.
//!
//! Where an earlier revision proved a property by reading this crate's own
//! source text, it now proves it by *behaviour* (drive the entry point, observe
//! the refusal and the absence of mutation) or by Rust privacy (the hostile
//! call does not compile, and `tests/compile_fail_principal.rs` records the
//! exact calls that must stay uncompilable).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use grokptah_agent_bridge::orchestration::{
    AuthContext, AuthCredential, AuthorityOrigin, DelegationLimit, DurableAuthority, HostAdmin,
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState,
    WorkspaceAllowlist, COMPAT_PRIMARY_PRINCIPAL,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost,
    AgentHostHandle, HostConfig, McpControlClient, SessionKind,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

type HomeGuard = std::sync::MutexGuard<'static, ()>;

const TOKEN: &str = "principal-fence-token";

struct Fixture {
    _home: TempDir,
    _guard: HomeGuard,
    host: AgentHostHandle,
    workspace: TempDir,
    /// The path callers claim. Deliberately reached through a symlink so the
    /// claimed path and its canonical form differ — the hosted condition that
    /// broke an earlier revision on macOS, where `/var` resolves to
    /// `/private/var`.
    claimed_workspace: PathBuf,
    orch: Arc<OrchestrationService>,
    admin: HostAdmin,
    store_root: PathBuf,
}

fn fixture() -> Fixture {
    fixture_with(|_| {})
}

/// Build a host, running `prepare` against the store root *before* the store is
/// opened so a test can plant durable state the way a previous process would
/// have left it.
fn fixture_with(prepare: impl FnOnce(&Path)) -> Fixture {
    let guard = home_override_serial();
    let home = TempDir::new().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    // SAFETY: `home_override_serial` serializes process-global test mutations.
    unsafe { std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1") };
    let workspace = TempDir::new().unwrap();

    // Reach the workspace through a symlink so the claimed path is *not* its
    // own canonical form on every platform, not just macOS.
    let claimed_workspace = claim_path_via_symlink(home.path(), workspace.path());

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
    let admin = orch
        .take_host_admin()
        .expect("the constructing host holds the one-shot admin capability");
    Fixture {
        _home: home,
        _guard: guard,
        host,
        workspace,
        claimed_workspace,
        orch,
        admin,
        store_root,
    }
}

#[cfg(unix)]
fn claim_path_via_symlink(home: &Path, target: &Path) -> PathBuf {
    let link = home.join("workspace-link");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(target, &link).unwrap();
    link
}

#[cfg(not(unix))]
fn claim_path_via_symlink(_home: &Path, target: &Path) -> PathBuf {
    target.to_path_buf()
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

    fn store(&self) -> &OrchStore {
        self.orch.store_for_admin(&self.admin).unwrap()
    }

    /// The canonical workspace string the host itself writes onto records.
    fn canonical_workspace(&self) -> String {
        dunce::canonicalize(self.workspace.path())
            .unwrap()
            .display()
            .to_string()
    }

    /// Every durable record, as a comparable snapshot. Used to prove that a
    /// refused call mutated nothing at all.
    fn durable_snapshot(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.store_root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.to_string_lossy().to_string();
                // The audit journal and the lock legitimately move on a refusal
                // — a refusal is supposed to be audited. Everything else is
                // record state and must be untouched.
                if name.contains("/audit") || name.contains(".store.lock") {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.insert(name, format!("{:x}", md5ish(&bytes)));
                }
            }
        }
        out
    }
}

/// Small content digest; only needs to change when bytes change.
fn md5ish(bytes: &[u8]) -> u128 {
    let mut acc: u128 = 0xcbf29ce484222325;
    for b in bytes {
        acc = acc.wrapping_mul(0x100000001b3).wrapping_add(*b as u128);
    }
    acc
}

/// A run stamped the way the host itself stamps one.
fn planted_run(
    run_id: &str,
    session: Uuid,
    canonical_workspace: &str,
    principal: Option<&str>,
    lineage: Option<&str>,
) -> RunRecord {
    RunRecord {
        run_id: run_id.into(),
        session_id: session,
        // The host always writes the *canonical* path. A fixture that wrote the
        // claimed path instead would not be reproducing production, and would
        // hide its own runs from every scope-filtered read.
        workspace: canonical_workspace.to_string(),
        request_id: format!("req-{run_id}"),
        client_id: principal.map(str::to_owned),
        client_lineage: lineage.map(str::to_owned),
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

// ── the hosted regression: exact owner visibility ───────────────────────────

/// A caller must see its **own** runs through a claimed path that is not its
/// own canonical form.
///
/// This is the hosted `#489` failure reproduced deterministically. On the macOS
/// runner `TMPDIR` sits under `/var`, which resolves to `/private/var`, so the
/// claimed path and the stored canonical path differed by canonicalization
/// only. `list_runs_scoped` compared them as exact strings, so the owner's list
/// came back empty — a loss of the owner's own run, not a leak. The fixture
/// here reaches the workspace through a symlink so every platform exercises
/// that condition.
#[test]
fn exact_owner_visibility_survives_a_non_canonical_claimed_path() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    let canonical = fx.canonical_workspace();

    // Precondition: the claimed path really is not its own canonical form,
    // otherwise this test would silently stop reproducing the hosted case.
    #[cfg(unix)]
    assert_ne!(
        fx.claimed_workspace.display().to_string(),
        canonical,
        "the fixture must claim a path that differs from its canonical form"
    );

    let lineage = fx
        .orch
        .scoped_reads(&auth, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();

    fx.store()
        .save_run(&planted_run(
            "run-mine",
            session,
            &canonical,
            Some(COMPAT_PRIMARY_PRINCIPAL),
            Some(&lineage),
        ))
        .unwrap();
    fx.store()
        .save_run(&planted_run(
            "run-theirs",
            session,
            &canonical,
            Some("someone-else"),
            Some("some-other-lineage"),
        ))
        .unwrap();

    let listed = fx
        .orch
        .list_runs_scoped(&auth, session, &fx.claimed_workspace)
        .unwrap();
    let ids: Vec<&str> = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["runId"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["run-mine"],
        "the owner must see exactly its own run through a non-canonical claimed path"
    );
    assert!(fx.orch.get_run(&auth, "run-mine").is_ok());
    assert!(fx.orch.get_run(&auth, "run-theirs").is_err());
    teardown();
}

// ── P0-1: credential lineage ────────────────────────────────────────────────

/// Removing an alias and re-registering it with a new secret must not reach the
/// removed registration's durable work — not for reads, and not for effects.
///
/// Records carry the credential *incarnation*, not just the stable alias, so a
/// re-registration is a different principal even though the wire alias is
/// identical. The only way back is the explicit operator migration.
#[test]
fn credential_re_add_with_a_new_secret_cannot_reach_old_work() {
    let fx = fixture();
    let session = fx.session();
    let canonical = fx.canonical_workspace();

    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![
                AuthCredential::declare(&fx.admin, "primary", TOKEN).unwrap(),
                AuthCredential::declare(&fx.admin, "laptop", "laptop-secret-one").unwrap(),
            ],
        )
        .unwrap();
    let first = fx
        .orch
        .auth_header(Some("Bearer laptop-secret-one"))
        .unwrap();
    let first_lineage = fx
        .orch
        .scoped_reads(&first, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();

    // Plant the first registration's work, stamped exactly as the host would.
    fx.store()
        .save_run(&planted_run(
            "run-old",
            session,
            &canonical,
            Some("laptop"),
            Some(&first_lineage),
        ))
        .unwrap();
    assert!(
        fx.orch.get_run(&first, "run-old").is_ok(),
        "the registration that created the work can read it"
    );

    // Remove the alias, then re-add it with a *different* secret.
    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![AuthCredential::declare(&fx.admin, "primary", TOKEN).unwrap()],
        )
        .unwrap();
    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![
                AuthCredential::declare(&fx.admin, "primary", TOKEN).unwrap(),
                AuthCredential::declare(&fx.admin, "laptop", "laptop-secret-two").unwrap(),
            ],
        )
        .unwrap();
    let second = fx
        .orch
        .auth_header(Some("Bearer laptop-secret-two"))
        .unwrap();
    assert_eq!(
        second.principal(),
        "laptop",
        "the wire alias is deliberately stable across re-registration"
    );

    // Reads refuse, with the same denial an unknown id gets.
    let absent = fx.orch.get_run(&second, "run-absent").unwrap_err();
    let refused = fx.orch.get_run(&second, "run-old").unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        (absent.code.as_str(), absent.message.as_str()),
        "a re-registered alias must not reach the previous registration's work"
    );
    let listed = fx
        .orch
        .list_runs_scoped(&second, session, &fx.claimed_workspace)
        .unwrap();
    assert!(
        listed["runs"].as_array().unwrap().is_empty(),
        "listing must not surface the previous registration's work either"
    );

    // Effects refuse too, and mutate nothing.
    let before = fx.durable_snapshot();
    assert!(fx.orch.get_events(&second, Some("run-old"), 0, 10).is_err());
    assert!(fx.orch.get_changes(&second, "run-old").is_err());
    assert_eq!(
        before,
        fx.durable_snapshot(),
        "a refused read must not mutate durable state"
    );

    // The record is quarantined, not attributed.
    let report = fx.orch.quarantine_report(&second).unwrap();
    assert_eq!(report["quarantine"]["staleLineageRecords"], 1);

    // The explicit operator migration is the only way back.
    let migrated = fx
        .orch
        .migrate_quarantined_lineage(&fx.admin, "laptop", "laptop")
        .unwrap();
    assert_eq!(migrated["migrated"], 1);
    assert!(
        fx.orch.get_run(&second, "run-old").is_ok(),
        "an explicit migration authorizes the new registration to adopt the work"
    );
    teardown();
}

/// Re-declaring an unchanged credential keeps its incarnation, so the records
/// that registration already wrote stay reachable.
///
/// A host that installs its credentials after construction — which is what
/// `grokptah-service` does on every boot — re-declares them from configuration,
/// and `AuthCredential::declare` mints a fresh incarnation each time. Without
/// reconciliation the host would orphan its own durable records on every
/// restart while reporting a clean resume. The alias *and* the secret digest
/// must both match to carry an incarnation forward, so this must not weaken
/// `credential_re_add_with_a_new_secret_cannot_reach_old_work`, which is
/// asserted here too.
#[test]
fn re_declaring_unchanged_credentials_keeps_their_incarnation() {
    let fx = fixture();
    let session = fx.session();
    let canonical = fx.canonical_workspace();

    let auth = fx.auth();
    let lineage = fx
        .orch
        .scoped_reads(&auth, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();
    fx.store()
        .save_run(&planted_run(
            "run-owned",
            session,
            &canonical,
            Some(COMPAT_PRIMARY_PRINCIPAL),
            Some(&lineage),
        ))
        .unwrap();

    // Re-declare the identical secret, as a restarting host does.
    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![AuthCredential::declare(&fx.admin, "primary", TOKEN).unwrap()],
        )
        .unwrap();
    let after = fx
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN}")))
        .expect("the unchanged secret still authenticates");
    let after_lineage = fx
        .orch
        .scoped_reads(&after, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        after_lineage, lineage,
        "an unchanged secret keeps the incarnation its records are bound to"
    );
    assert!(
        fx.orch.get_run(&after, "run-owned").is_ok(),
        "re-declaring the same credential must not orphan its own durable work"
    );

    // The generation still advanced, so identities issued before it are dead.
    assert!(
        fx.orch.get_run(&auth, "run-owned").is_err(),
        "re-registration is still a generation advance for issued identities"
    );

    // A changed secret is still a different principal.
    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![AuthCredential::declare(&fx.admin, "primary", "a-different-secret").unwrap()],
        )
        .unwrap();
    let rotated = fx
        .orch
        .auth_header(Some("Bearer a-different-secret"))
        .unwrap();
    let absent = fx.orch.get_run(&rotated, "run-absent").unwrap_err();
    let refused = fx.orch.get_run(&rotated, "run-owned").unwrap_err();
    assert_eq!(
        (refused.code.as_str(), refused.message.as_str()),
        (absent.code.as_str(), absent.message.as_str()),
        "a changed secret must not inherit the previous incarnation's work"
    );
    teardown();
}

/// A restart with unchanged credentials keeps each registration's incarnation,
/// so a caller's own durable work stays reachable across a restart while
/// pre-restart *identities* still die.
#[test]
fn credential_incarnations_persist_across_restart() {
    let fx = fixture();
    let session = fx.session();
    let canonical = fx.canonical_workspace();
    let before = fx.auth();
    let lineage = fx
        .orch
        .scoped_reads(&before, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();
    fx.store()
        .save_run(&planted_run(
            "run-survivor",
            session,
            &canonical,
            Some(COMPAT_PRIMARY_PRINCIPAL),
            Some(&lineage),
        ))
        .unwrap();

    let Fixture {
        _home,
        _guard,
        host,
        workspace,
        claimed_workspace,
        orch,
        admin,
        store_root,
    } = fx;
    // The admin capability is instance-bound, so the restarted host issues its
    // own; this one is simply not carried forward.
    let _ = admin;
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
        restarted.get_run(&before, "run-survivor").is_err(),
        "a pre-restart identity must not become current merely because a process restarted"
    );
    let after = restarted
        .auth_header(Some(&format!("Bearer {TOKEN}")))
        .unwrap();
    let after_lineage = restarted
        .scoped_reads(&after, session, &claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        after_lineage, lineage,
        "an unchanged credential keeps its incarnation across restart"
    );
    assert!(
        restarted.get_run(&after, "run-survivor").is_ok(),
        "a caller's own durable work survives a restart"
    );
    teardown();
}

// ── P0-2: lineage failures fail closed ──────────────────────────────────────

/// A persisted lineage that cannot be parsed must never be treated as a clean
/// first run: doing so would silently hand every prior record to a brand-new
/// authority. The lineage is re-established *and the prior work quarantined*.
#[test]
fn corrupt_lineage_quarantines_prior_records_and_never_reattributes() {
    let workspace_holder = TempDir::new().unwrap();
    let planted = workspace_holder.path().to_path_buf();
    let fx = fixture_with(move |root| {
        std::fs::create_dir_all(root.join("runs")).unwrap();
        // A record from the previous lineage, attributed to the alias this host
        // will also use.
        let run = planted_run(
            "run-prior",
            Uuid::new_v4(),
            &planted.display().to_string(),
            Some(COMPAT_PRIMARY_PRINCIPAL),
            Some("lineage-from-the-unreadable-authority"),
        );
        std::fs::write(
            root.join("runs").join("run-prior.json"),
            serde_json::to_vec_pretty(&run).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("authority.json"), b"{ this is not json").unwrap();
    });

    assert_eq!(
        fx.orch.authority_origin(),
        AuthorityOrigin::ReestablishedFailClosed
    );
    let auth = fx.auth();
    let absent = fx.orch.get_run(&auth, "run-absent").unwrap_err();
    let prior = fx.orch.get_run(&auth, "run-prior").unwrap_err();
    assert_eq!(
        (prior.code.as_str(), prior.message.as_str()),
        (absent.code.as_str(), absent.message.as_str()),
        "a record from an unreadable lineage must never be re-attributed to the current caller"
    );
    let report = fx.orch.quarantine_report(&auth).unwrap();
    assert_eq!(report["quarantine"]["staleLineageRecords"], 1);
    assert!(
        report["quarantine"]["quarantinedLineages"]
            .as_u64()
            .unwrap()
            >= 1,
        "the unreadable lineage is recorded as quarantined: {report}"
    );
    teardown();
}

/// A host that cannot make its lineage durable must serve nothing at all.
///
/// Continuing undurably would let identities be issued under an epoch a later
/// restart cannot know about, and would leave records attributable to a lineage
/// no one can verify.
#[test]
fn a_host_that_cannot_persist_its_lineage_is_sealed() {
    let fx = fixture_with(|root| {
        // `authority.json` as a directory: unreadable *and* unwritable.
        std::fs::create_dir_all(root.join("authority.json")).unwrap();
    });
    assert_eq!(
        fx.orch.authority_origin(),
        AuthorityOrigin::ReestablishedFailClosed
    );

    let issued = fx.orch.auth_header(Some(&format!("Bearer {TOKEN}")));
    match issued {
        Err(error) => assert!(
            error.message.contains("not durable"),
            "a sealed host must say why it refuses: {}",
            error.message
        ),
        Ok(auth) => {
            // Authentication may still shape-check; every guarded boundary must
            // refuse regardless.
            let refused = fx
                .orch
                .list_runs_scoped(&auth, fx.session(), &fx.claimed_workspace)
                .unwrap_err();
            assert!(
                refused.message.contains("not durable"),
                "a sealed host must refuse every boundary: {}",
                refused.message
            );
        }
    }

    // And no rotation can take effect while sealed.
    assert!(fx.orch.set_token(&fx.admin, "replacement".into()).is_err());
    teardown();
}

// ── P0-3: authority mutation is administrative ──────────────────────────────

/// Changing who a host honours requires the one-shot admin capability, and that
/// capability is issued exactly once — to whoever constructed the host.
#[test]
fn authority_mutation_requires_the_one_shot_host_admin() {
    let fx = fixture();
    assert!(
        fx.orch.take_host_admin().is_none(),
        "the admin capability is one-shot; a second caller must not get one"
    );

    // A capability minted by a *second service over the same host* must not
    // authorize the first.
    //
    // This is the hole an instance-bound capability closes. Two services on one
    // host share a durable store and therefore share an authority lineage, so a
    // capability bound to the lineage could be minted by simply standing up a
    // second service — which anything able to construct one can do — and then
    // used to rotate the first host's credentials.
    //
    // Built inline rather than through `fixture()`: the home-override guard is
    // a non-reentrant process mutex, so a second fixture in the same test would
    // deadlock rather than fail.
    let other_root = fx.store_root.with_file_name("orch-other-host");
    std::fs::create_dir_all(&other_root).unwrap();
    let other = OrchestrationService::new(
        fx.host.clone(),
        fx.host.event_bus(),
        OrchStore::open(&other_root).unwrap(),
        OrchestrationConfig {
            bearer_token: "other-host-token".into(),
            allowlist: WorkspaceAllowlist::new([fx.workspace.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    let foreign = other
        .take_host_admin()
        .expect("the second service issues its own one-shot capability");
    let foreign = &foreign;
    assert!(
        fx.orch.set_token(foreign, "not-allowed".into()).is_err(),
        "an admin capability from another service instance must not change this host"
    );
    assert!(fx
        .orch
        .set_allowlist(foreign, WorkspaceAllowlist::default())
        .is_err());
    assert!(fx
        .orch
        .set_agent_owner_id(foreign, "tenant-x".into())
        .is_err());
    assert!(fx.orch.store_for_admin(foreign).is_err());

    // The host's own capability does authorize it.
    assert!(fx
        .orch
        .set_agent_owner_id(&fx.admin, "tenant-2".into())
        .is_ok());
    assert_eq!(fx.auth().owner_id(), "tenant-2");
    teardown();
}

// ── P0-4/P0-5: whole-tree behavioural coverage ──────────────────────────────

/// Every public boundary, driven for real, must refuse a stale identity **and
/// mutate nothing**.
///
/// This replaces the source-text scanner an earlier revision used. A scanner
/// only proved that a guard call appeared as the first statement; it could not
/// prove the guard actually refused, and it could not prove the refusal was
/// free of side effects. Here each entry point is invoked against a live host
/// with an identity that was current a moment ago and no longer is, and the
/// whole durable tree is hashed before and after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn every_boundary_refuses_a_stale_identity_without_mutating() {
    let fx = fixture();
    let session = fx.session();
    let ws = fx.claimed_workspace.clone();

    // Issue an identity, then rotate so it is stale but well-formed.
    let stale = fx.auth();
    fx.orch.set_token(&fx.admin, TOKEN.into()).unwrap();

    let before = fx.durable_snapshot();
    let mut refused: Vec<&str> = Vec::new();
    let mut accepted: Vec<&str> = Vec::new();

    macro_rules! boundary {
        ($name:literal, $call:expr) => {{
            match $call {
                Err(_) => refused.push($name),
                Ok(_) => accepted.push($name),
            }
        }};
    }

    // ── reads, every resource family ──
    boundary!("list_sessions", fx.orch.list_sessions(&stale));
    boundary!(
        "list_persistent_agents",
        fx.orch.list_persistent_agents(&stale)
    );
    boundary!("get_capacity", fx.orch.get_capacity(&stale));
    boundary!("quarantine_report", fx.orch.quarantine_report(&stale));
    boundary!("scoped_reads", fx.orch.scoped_reads(&stale, session, &ws));
    boundary!(
        "list_runs_scoped",
        fx.orch.list_runs_scoped(&stale, session, &ws)
    );
    boundary!("get_run", fx.orch.get_run(&stale, "run-x"));
    boundary!("get_progress", fx.orch.get_progress(&stale, "run-x"));
    boundary!(
        "get_events",
        fx.orch.get_events(&stale, Some("run-x"), 0, 10)
    );
    boundary!("get_changes", fx.orch.get_changes(&stale, "run-x"));
    boundary!(
        "get_test_results",
        fx.orch.get_test_results(&stale, "run-x")
    );
    boundary!("get_handoff", fx.orch.get_handoff(&stale, "run-x"));
    boundary!(
        "get_run_scoped",
        fx.orch.get_run_scoped(&stale, session, &ws, "run-x")
    );
    boundary!(
        "get_events_scoped",
        fx.orch
            .get_events_scoped(&stale, session, &ws, "run-x", 0, 10)
    );
    boundary!(
        "list_work_scoped",
        fx.orch.list_work_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_work_scoped",
        fx.orch.get_work_scoped(&stale, session, &ws, "work-x")
    );
    boundary!(
        "list_work_decisions_scoped",
        fx.orch
            .list_work_decisions_scoped(&stale, session, &ws, "work-x")
    );
    boundary!(
        "list_workers_scoped",
        fx.orch.list_workers_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_worker_scoped",
        fx.orch.get_worker_scoped(&stale, session, &ws, "agent-x")
    );
    boundary!(
        "list_routines_scoped",
        fx.orch.list_routines_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_routine_scoped",
        fx.orch
            .get_routine_scoped(&stale, session, &ws, "routine-x")
    );
    boundary!(
        "list_activations_scoped",
        fx.orch
            .list_activations_scoped(&stale, session, &ws, "routine-x")
    );
    boundary!(
        "list_manager_plans_scoped",
        fx.orch.list_manager_plans_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_manager_plan_scoped",
        fx.orch
            .get_manager_plan_scoped(&stale, session, &ws, "plan-x")
    );
    boundary!(
        "list_inbox_scoped",
        fx.orch
            .list_inbox_scoped(&stale, session, &ws, "agent-x", 0)
    );
    boundary!(
        "list_outbox_scoped",
        fx.orch
            .list_outbox_scoped(&stale, session, &ws, "agent-x", 0)
    );
    boundary!(
        "list_execution_intents_scoped",
        fx.orch.list_execution_intents_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_managed_execution",
        fx.orch
            .get_managed_execution(&stale, session, &ws, "agent-x")
    );
    boundary!("get_queue", fx.orch.get_queue(&stale, session, &ws));
    boundary!(
        "list_computer_runs_scoped",
        fx.orch.list_computer_runs_scoped(&stale, session, &ws)
    );
    boundary!(
        "get_computer_run_scoped",
        fx.orch
            .get_computer_run_scoped(&stale, session, &ws, "cr-x")
    );
    boundary!(
        "get_computer_capacity_scoped",
        fx.orch.get_computer_capacity_scoped(&stale, session, &ws)
    );

    // ── effects ──
    boundary!("create_session", fx.orch.create_session(&stale, &ws, None));
    boundary!(
        "delegate",
        fx.orch.delegate(
            &stale,
            "helper",
            DelegationLimit::ReadOnlyWithinScope,
            60,
            session,
            &ws
        )
    );
    boundary!(
        "submit_task",
        fx.orch
            .submit_task(&stale, "req-stale", session, &ws, "hello".into(), None)
            .await
    );
    boundary!(
        "queue_prompt",
        fx.orch
            .queue_prompt(&stale, "req-stale-q", session, &ws, "hello".into(), false)
            .await
    );
    boundary!(
        "clear_queue",
        fx.orch
            .clear_queue(&stale, "req-stale-c", session, &ws)
            .await
    );
    boundary!(
        "cancel",
        fx.orch
            .cancel(&stale, "req-stale-x", session, &ws, Some("run-x"))
            .await
    );

    assert!(
        accepted.is_empty(),
        "these boundaries accepted a stale identity: {accepted:?}"
    );
    assert!(
        refused.len() >= 38,
        "the matrix must actually cover the tree; only {} boundaries were driven",
        refused.len()
    );
    assert_eq!(
        before,
        fx.durable_snapshot(),
        "a refused call must leave every durable record byte-identical"
    );
    teardown();
}

/// Submit refuses a caller that does not own the session's Agent **before** it
/// writes anything.
///
/// The ownership check used to run after `begin_idempotency` (which writes a
/// claim record) and after `ensure_session_agent` (which creates an Agent), so
/// a refused caller had already mutated the store on its way to the refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn submit_refused_for_foreign_ownership_writes_nothing() {
    let fx = fixture();
    let session = fx.session();
    let ws = fx.claimed_workspace.clone();

    // Establish the session's Agent under one owner.
    let owner_one = fx.auth();
    fx.orch
        .submit_task(&owner_one, "req-own", session, &ws, "first".into(), None)
        .await
        .unwrap();

    // A second tenant now presents itself for the same session.
    fx.orch
        .set_agent_owner_id(&fx.admin, "tenant-two".into())
        .unwrap();
    let owner_two = fx.auth();
    assert_eq!(owner_two.owner_id(), "tenant-two");

    let before = fx.durable_snapshot();
    let refused = fx
        .orch
        .submit_task(
            &owner_two,
            "req-foreign",
            session,
            &ws,
            "second".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::ForbiddenScope
    );
    assert_eq!(
        before,
        fx.durable_snapshot(),
        "a submit refused for foreign ownership must create no idempotency claim, \
         no Agent, and no Run"
    );
    teardown();
}

// ── delegation is resource-bound ────────────────────────────────────────────

/// A grant reaches exactly the session and workspace it was minted for.
///
/// Before this, a delegation was principal-scoped only: a grant made for one
/// session let the delegate read every session the delegator could.
#[test]
fn delegation_is_bound_to_the_resource_it_was_granted_for() {
    let fx = fixture();
    let granted = fx.session();
    let other = fx.session();
    let auth = fx.auth();
    let ws = fx.claimed_workspace.clone();

    let delegate = fx
        .orch
        .delegate(
            &auth,
            "helper",
            DelegationLimit::ReadOnlyWithinScope,
            60,
            granted,
            &ws,
        )
        .unwrap();
    assert_eq!(delegate.principal(), "helper");
    assert_eq!(delegate.owner_id(), auth.owner_id());

    assert!(
        fx.orch.scoped_reads(&delegate, granted, &ws).is_ok(),
        "the grant reaches the resource it names"
    );
    // `ScopedReads` borrows the host and is intentionally not `Debug`, so the
    // refusal below is matched rather than unwrapped.
    let outside = match fx.orch.scoped_reads(&delegate, other, &ws) {
        Err(error) => error,
        Ok(_) => panic!("the grant must not reach a session it was not minted for"),
    };
    assert_eq!(
        outside.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::ForbiddenScope,
        "the grant must not reach a session it was not minted for"
    );
    // The delegator itself is unaffected.
    assert!(fx.orch.scoped_reads(&auth, other, &ws).is_ok());

    // Effects stay refused, and re-delegation cannot reset the clock.
    assert!(fx.orch.create_session(&delegate, &ws, None).is_err());
    assert!(fx
        .orch
        .delegate(
            &delegate,
            "third",
            DelegationLimit::ReadOnlyWithinScope,
            60,
            granted,
            &ws
        )
        .is_err());

    // Rotation revokes the grant along with the identity it came from.
    fx.orch.set_token(&fx.admin, TOKEN.into()).unwrap();
    assert!(fx.orch.scoped_reads(&delegate, granted, &ws).is_err());
    teardown();
}

// ── retained adversarial coverage ───────────────────────────────────────────

/// Two named device credentials are two principals. Each owns the runs it
/// stamped, neither can read the other's, and the refusal is the same one an id
/// that does not exist gets.
#[test]
fn two_principals_cannot_read_each_others_runs() {
    let fx = fixture();
    fx.orch
        .set_auth_credentials(
            &fx.admin,
            vec![
                AuthCredential::declare(&fx.admin, "primary", TOKEN).unwrap(),
                AuthCredential::declare(&fx.admin, "laptop", "laptop-token").unwrap(),
            ],
        )
        .unwrap();
    let session = fx.session();
    let canonical = fx.canonical_workspace();

    let primary = fx.auth();
    let laptop = fx.orch.auth_header(Some("Bearer laptop-token")).unwrap();
    assert_eq!(primary.principal(), COMPAT_PRIMARY_PRINCIPAL);
    assert_eq!(laptop.principal(), "laptop");

    for (run_id, auth) in [("run-primary", &primary), ("run-laptop", &laptop)] {
        let lineage = fx
            .orch
            .scoped_reads(auth, session, &fx.claimed_workspace)
            .unwrap()
            .identity()["lineage"]
            .as_str()
            .unwrap()
            .to_string();
        fx.store()
            .save_run(&planted_run(
                run_id,
                session,
                &canonical,
                Some(auth.principal()),
                Some(&lineage),
            ))
            .unwrap();
    }

    assert!(fx.orch.get_run(&primary, "run-primary").is_ok());
    assert!(fx.orch.get_run(&laptop, "run-laptop").is_ok());

    let foreign = fx.orch.get_run(&primary, "run-laptop").unwrap_err();
    let absent = fx.orch.get_run(&primary, "run-does-not-exist").unwrap_err();
    assert_eq!(
        (foreign.code.as_str(), foreign.message.as_str()),
        (absent.code.as_str(), absent.message.as_str()),
        "a foreign run and an unknown run must be byte-identical refusals"
    );

    for (auth, expected) in [(&primary, "run-primary"), (&laptop, "run-laptop")] {
        let listed = fx
            .orch
            .list_runs_scoped(auth, session, &fx.claimed_workspace)
            .unwrap();
        let ids: Vec<&str> = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["runId"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![expected],
            "each principal sees exactly its own run"
        );
    }
    teardown();
}

/// Foreign, unknown and malformed run ids must not be separable by the bytes of
/// the refusal or by the work done producing it.
#[test]
fn foreign_unknown_and_malformed_denials_are_equivalent() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    fx.store()
        .save_run(&planted_run(
            "run-other",
            session,
            &fx.canonical_workspace(),
            Some("someone-else"),
            Some("their-lineage"),
        ))
        .unwrap();

    let cases = ["run-other", "run-absent", "../../etc/passwd"];
    let mut errors = Vec::new();
    let mut timings = Vec::new();
    for case in cases {
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
    assert!(
        slowest <= fastest * 12.0,
        "denial timings diverge beyond tolerance: {timings:?} for {cases:?}"
    );
    teardown();
}

/// Every authority mutation invalidates identities issued before it; a policy
/// change moves the policy revision while a credential change does not.
#[test]
fn rotation_invalidates_issued_identities() {
    let fx = fixture();
    let session = fx.session();
    let ws = fx.claimed_workspace.clone();

    let before = fx.auth();
    assert!(fx.orch.list_runs_scoped(&before, session, &ws).is_ok());
    let epoch_before = fx.orch.auth_epoch();
    let policy_before = fx.orch.policy_revision();

    fx.orch.set_token(&fx.admin, TOKEN.into()).unwrap();
    assert_eq!(fx.orch.auth_epoch(), epoch_before + 1);
    assert_eq!(fx.orch.policy_revision(), policy_before);
    let stale = fx.orch.list_runs_scoped(&before, session, &ws).unwrap_err();
    assert_eq!(
        stale.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::Unauthenticated
    );

    let mid = fx.auth();
    fx.orch
        .set_allowlist(
            &fx.admin,
            WorkspaceAllowlist::new([fx.workspace.path().to_path_buf()]),
        )
        .unwrap();
    assert_eq!(fx.orch.policy_revision(), policy_before + 1);
    assert!(fx.orch.list_runs_scoped(&mid, session, &ws).is_err());

    let after_policy = fx.auth();
    fx.orch
        .set_agent_owner_id(&fx.admin, "tenant-2".into())
        .unwrap();
    assert_eq!(fx.orch.policy_revision(), policy_before + 2);
    assert!(fx
        .orch
        .list_runs_scoped(&after_policy, session, &ws)
        .is_err());
    assert_eq!(fx.auth().owner_id(), "tenant-2");
    teardown();
}

/// A generation at the ceiling refuses to rotate, and refusing changes nothing.
#[test]
fn generation_exhaustion_changes_nothing() {
    let fx = fixture_with(|root| {
        let record = DurableAuthority {
            authority: Uuid::new_v4(),
            epoch: u64::MAX - 1,
            policy_revision: 7,
            credentials: Vec::new(),
            quarantined_lineages: Vec::new(),
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
        .list_runs_scoped(&auth, session, &fx.claimed_workspace)
        .unwrap();
    let durable_before = fx.durable_snapshot();

    let credential_rotation = fx
        .orch
        .set_token(&fx.admin, "replacement-token".into())
        .unwrap_err();
    assert!(
        credential_rotation.message.contains("generation exhausted"),
        "exhaustion must fail closed: {}",
        credential_rotation.message
    );
    assert!(fx
        .orch
        .set_allowlist(&fx.admin, WorkspaceAllowlist::default())
        .is_err());
    assert!(fx
        .orch
        .set_agent_owner_id(&fx.admin, "tenant-9".into())
        .is_err());

    assert_eq!(fx.orch.auth_epoch(), u64::MAX);
    assert_eq!(fx.orch.policy_revision(), 7);
    assert!(fx
        .orch
        .auth_header(Some("Bearer replacement-token"))
        .is_err());
    assert_eq!(
        fx.orch
            .list_runs_scoped(&auth, session, &fx.claimed_workspace)
            .unwrap(),
        before
    );
    assert_eq!(
        durable_before,
        fx.durable_snapshot(),
        "a refused rotation must not touch durable state"
    );
    teardown();
}

/// Records written before attribution existed belong to nobody: never handed to
/// the caller, never attributed to a hard-coded desktop or MCP principal.
#[test]
fn legacy_unbound_records_are_quarantined_not_attributed() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    let canonical = fx.canonical_workspace();
    let lineage = fx
        .orch
        .scoped_reads(&auth, session, &fx.claimed_workspace)
        .unwrap()
        .identity()["lineage"]
        .as_str()
        .unwrap()
        .to_string();

    fx.store()
        .save_run(&planted_run("run-legacy", session, &canonical, None, None))
        .unwrap();
    fx.store()
        .save_run(&planted_run(
            "run-blank",
            session,
            &canonical,
            Some("   "),
            None,
        ))
        .unwrap();
    fx.store()
        .save_run(&planted_run(
            "run-no-lineage",
            session,
            &canonical,
            Some(COMPAT_PRIMARY_PRINCIPAL),
            None,
        ))
        .unwrap();
    fx.store()
        .save_run(&planted_run(
            "run-mine",
            session,
            &canonical,
            Some(COMPAT_PRIMARY_PRINCIPAL),
            Some(&lineage),
        ))
        .unwrap();

    let absent = fx.orch.get_run(&auth, "run-absent").unwrap_err();
    for quarantined in ["run-legacy", "run-blank", "run-no-lineage"] {
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
    assert_eq!(report["quarantine"]["unboundLineageRecords"], 1);
    assert_eq!(report["quarantine"]["total"], 3);
    let text = report.to_string();
    assert!(
        !text.contains("run-legacy") && !text.contains("run-blank"),
        "the operator report must carry counts, never record contents: {text}"
    );
    teardown();
}

/// The sanctioned public read surface hands out DTOs bound to a verified
/// principal: an opaque scope, a public workspace alias, no native path, no
/// secret.
#[test]
fn scoped_reads_expose_dtos_without_native_paths_or_secrets() {
    let fx = fixture();
    let session = fx.session();
    let auth = fx.auth();
    let reads = fx
        .orch
        .scoped_reads(&auth, session, &fx.claimed_workspace)
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
        !text.contains(&fx.canonical_workspace()),
        "a read DTO must not carry a native workspace path: {text}"
    );
    assert!(!text.contains(TOKEN), "a read DTO must not carry a secret");
    assert_eq!(identity["sessionId"].as_str().unwrap(), session.to_string());

    assert!(reads.runs().is_ok());
    assert!(reads.work().is_ok());
    assert!(reads.routines().is_ok());
    teardown();
}

/// Rotation revokes the live channel end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn a_rotation_revokes_the_live_channel() {
    let fx = fixture();
    let session = fx.session();
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

    let live_url = || {
        let mut url = reqwest::Url::parse(&format!("http://{}/mcp", server.addr)).unwrap();
        url.query_pairs_mut()
            .append_pair("session_id", &session.to_string())
            .append_pair("workspace", &workspace.display().to_string())
            .append_pair("run_id", &run_id);
        url
    };

    let stream = reqwest::Client::new()
        .get(live_url())
        .header("Authorization", format!("Bearer {TOKEN}"))
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

    fx.orch
        .set_token(&fx.admin, "rotated-live-token".into())
        .unwrap();
    let refused = reqwest::Client::new()
        .get(live_url())
        .header("Authorization", format!("Bearer {TOKEN}"))
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

    // The rotated credential is a *new registration*, so the Run admitted under
    // the previous one is quarantined from it — this is the lineage rule, not
    // an outage. The stream is refused rather than served another
    // registration's events.
    let rotated = reqwest::Client::new()
        .get(live_url())
        .header("Authorization", "Bearer rotated-live-token")
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert!(
        rotated.status().is_client_error(),
        "a rotated credential must not inherit the previous registration's Run: {}",
        rotated.status()
    );

    // An explicit operator migration is what makes it reachable again.
    fx.orch
        .migrate_quarantined_lineage(&fx.admin, COMPAT_PRIMARY_PRINCIPAL, "primary")
        .unwrap();
    let migrated = reqwest::Client::new()
        .get(live_url())
        .header("Authorization", "Bearer rotated-live-token")
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(
        migrated.status(),
        200,
        "after an explicit migration the live channel opens again"
    );

    server.stop();
    teardown();
}
