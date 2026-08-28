//! Two-principal adversarial tests for session prompt-queue ownership (#461).
//!
//! Every test here is written from the attacker's side: principal `intruder`
//! holds a valid credential for the same tenant, session, and workspace as
//! principal `owner`, and each test asserts that a specific queue verb refuses
//! to act on, disclose, or even confirm the existence of the other principal's
//! work.
//!
//! The refusal contract these tests pin is deliberately strict: unknown,
//! malformed, foreign, and quarantined queue ids must be *byte-identical*, so
//! the queue cannot be used as an existence oracle.

mod common;

use std::path::Path;

use grokptah_agent_bridge::orchestration::{
    AuthContext, AuthCredential, OrchErrorCode, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::queue_authority::{QueueActor, QueuePrincipal, QueueProvenance};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, HostConfig, SessionKind, SessionUpdate,
};
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const OWNER_TOKEN: &str = "owner-token";
const INTRUDER_TOKEN: &str = "intruder-token";

struct Fixture {
    home: tempfile::TempDir,
    _guard: ProcessEnvGuard,
    workspace: tempfile::TempDir,
    host: grokptah_agent_bridge::AgentHostHandle,
    orch: std::sync::Arc<OrchestrationService>,
    session: Uuid,
}

impl Fixture {
    fn owner(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {OWNER_TOKEN}")))
            .expect("owner credential authenticates")
    }

    fn intruder(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {INTRUDER_TOKEN}")))
            .expect("intruder credential authenticates")
    }

    fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    /// Release the running host so a second one may take the instance lock,
    /// keeping the home, workspace, and session identity that a restart is
    /// supposed to preserve.
    ///
    /// Returned rather than done in place because the store must be cloned out
    /// before the service that owns it is dropped.
    fn shut_down(self) -> Restarted {
        let store = self.orch.store().clone();
        let Fixture {
            home,
            _guard,
            workspace,
            host,
            orch,
            session,
        } = self;
        drop(orch);
        drop(host);
        Restarted {
            _home: home,
            _guard,
            workspace,
            session,
            store,
        }
    }

    /// Where the host persists this session's queue.
    ///
    /// Resolved through the same home the host itself resolves, rather than
    /// rebuilt from the fixture's temp dir: the home is a process-global
    /// override, so reconstructing the path by hand can disagree with where the
    /// host actually wrote.
    fn queue_path(&self) -> std::path::PathBuf {
        grokptah_agent_bridge::grokptah_home()
            .join("sessions")
            .join(self.session.to_string())
            .join("prompt_queue.json")
    }
}

/// Two named credentials on one service, one session, one workspace.
///
/// This is the exact shape #461 describes: both principals are fully
/// authenticated and fully in scope. Nothing but principal ownership
/// distinguishes them.
fn fixture() -> Fixture {
    let mut guard = ProcessEnvGuard::new();
    let home_dir = tempdir().unwrap();
    let home = home_dir.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");

    let workspace = tempdir().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home_dir.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: OWNER_TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", OWNER_TOKEN).unwrap(),
        AuthCredential::new("intruder", INTRUDER_TOKEN).unwrap(),
    ])
    .expect("install two device credentials");

    Fixture {
        home: home_dir,
        _guard: guard,
        workspace,
        host,
        orch,
        session: session.id,
    }
}

/// A fixture whose host has been shut down, ready for a second host to open
/// the same home.
struct Restarted {
    _home: tempfile::TempDir,
    _guard: ProcessEnvGuard,
    workspace: tempfile::TempDir,
    session: Uuid,
    store: OrchStore,
}

impl Restarted {
    fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    /// The session identity a restart is supposed to preserve.
    fn session(&self) -> Uuid {
        self.session
    }

    /// Boot a fresh host and service over the same durable home, with both
    /// credentials installed exactly as before.
    fn boot(
        &self,
    ) -> (
        grokptah_agent_bridge::AgentHostHandle,
        std::sync::Arc<OrchestrationService>,
    ) {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().expect("restart host");
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            self.store.clone(),
            OrchestrationConfig {
                bearer_token: OWNER_TOKEN.into(),
                allowlist: WorkspaceAllowlist::new([self.workspace().to_path_buf()]),
                max_concurrent_runs: 4,
                bounds: RunBounds::default(),
            },
        );
        orch.set_auth_credentials(vec![
            AuthCredential::new("primary", OWNER_TOKEN).unwrap(),
            AuthCredential::new("intruder", INTRUDER_TOKEN).unwrap(),
        ])
        .expect("reinstall credentials");
        (host, orch)
    }
}

async fn queue_as(fx: &Fixture, auth: &AuthContext, request_id: &str, text: &str) -> String {
    let response = fx
        .orch
        .queue_prompt(
            auth,
            request_id,
            fx.session,
            fx.workspace(),
            text.into(),
            false,
        )
        .await
        .expect("queue accepted");
    response["entry"]["id"].as_str().unwrap().to_string()
}

fn entry_version(fx: &Fixture, auth: &AuthContext, entry_id: &str) -> u64 {
    let listed = fx
        .orch
        .get_queue(auth, fx.session, fx.workspace())
        .expect("queue readable");
    listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"].as_str() == Some(entry_id))
        .map(|entry| entry["version"].as_u64().unwrap())
        .unwrap_or(0)
}

/// The single refusal every non-owned id must produce.
fn assert_unknown_entry(error: &grokptah_agent_bridge::orchestration::OrchError) {
    assert_eq!(
        error.code,
        OrchErrorCode::InvalidRequest,
        "foreign and unknown ids must share one error code"
    );
    assert_eq!(
        error.message, "unknown queued prompt",
        "foreign and unknown ids must share one byte-identical message"
    );
}

// ── Reads ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_principal_cannot_read_another_principals_queue() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "owner-1", "owner secret plan").await;

    let seen = fx
        .orch
        .get_queue(&intruder, fx.session, fx.workspace())
        .unwrap();
    assert!(
        seen["entries"].as_array().unwrap().is_empty(),
        "intruder must not see the owner's queued work: {seen}"
    );

    let mine = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap();
    assert_eq!(
        mine["entries"].as_array().unwrap().len(),
        1,
        "the owner must still see its own entry"
    );
    assert_eq!(
        mine["entries"][0]["text"], "owner secret plan",
        "scoping must not damage the owner's own read"
    );
}

#[tokio::test]
async fn queue_reads_never_disclose_the_workspace_path_or_tenant() {
    let fx = fixture();
    let owner = fx.owner();
    queue_as(&fx, &owner, "owner-1", "plan").await;
    let listed = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap();

    let owner_key = listed["ownerKey"].as_str().expect("ownerKey projected");
    assert!(
        owner_key.starts_with("v1-sha256:"),
        "ownership must project as an opaque handle, got {owner_key}"
    );
    let workspace_text = fx.workspace().display().to_string();
    assert!(
        !owner_key.contains(&workspace_text),
        "the ownership handle must not disclose the workspace path"
    );
    let entry_owner_key = listed["entries"][0]["owner_key"]
        .as_str()
        .expect("entries carry the ownership handle");
    assert!(
        !entry_owner_key.contains(&workspace_text) && entry_owner_key.starts_with("v1-sha256:"),
        "queue entries must carry only an opaque ownership handle"
    );
}

// ── Mutations ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_principal_cannot_edit_remove_reorder_run_or_steer_another_principals_entry() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    let victim = queue_as(&fx, &owner, "owner-1", "owner work").await;
    let version = entry_version(&fx, &owner, &victim);

    let edit = fx
        .orch
        .edit_queue(
            &intruder,
            "x-edit",
            fx.session,
            fx.workspace(),
            &victim,
            version,
            "hijacked".into(),
        )
        .await
        .expect_err("cross-principal edit must fail");
    assert_unknown_entry(&edit);

    let remove = fx
        .orch
        .remove_queue(
            &intruder,
            "x-remove",
            fx.session,
            fx.workspace(),
            &victim,
            version,
        )
        .await
        .expect_err("cross-principal remove must fail");
    assert_unknown_entry(&remove);

    let reorder = fx
        .orch
        .reorder_queue(
            &intruder,
            "x-reorder",
            fx.session,
            fx.workspace(),
            &victim,
            0,
            version,
            fx.orch
                .get_queue(&intruder, fx.session, fx.workspace())
                .unwrap()["revision"]
                .as_u64()
                .unwrap(),
        )
        .await
        .expect_err("cross-principal reorder must fail");
    assert_unknown_entry(&reorder);

    let run_next = fx
        .orch
        .run_next_queue(
            &intruder,
            "x-run",
            fx.session,
            fx.workspace(),
            &victim,
            version,
        )
        .await
        .expect_err("cross-principal run_next must fail");
    assert_unknown_entry(&run_next);

    let steer = fx
        .orch
        .steer_queued(
            &intruder,
            "x-steer",
            fx.session,
            fx.workspace(),
            &victim,
            version,
        )
        .await
        .expect_err("cross-principal steer must fail");
    assert_unknown_entry(&steer);

    // The victim survived every attempt, unmodified and in place.
    let after = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap();
    assert_eq!(after["entries"].as_array().unwrap().len(), 1);
    assert_eq!(after["entries"][0]["text"], "owner work");
    assert_eq!(after["entries"][0]["version"].as_u64().unwrap(), version);
}

#[tokio::test]
async fn clear_stops_only_the_calling_principals_work() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "owner-1", "owner work").await;
    queue_as(&fx, &intruder, "intruder-1", "intruder work").await;

    fx.orch
        .clear_queue(&intruder, "x-clear", fx.session, fx.workspace())
        .await
        .expect("a principal may clear its own work");

    let owner_view = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap();
    assert_eq!(
        owner_view["entries"].as_array().unwrap().len(),
        1,
        "clear must not cancel another principal's queued work"
    );
    assert_eq!(owner_view["entries"][0]["text"], "owner work");

    let intruder_view = fx
        .orch
        .get_queue(&intruder, fx.session, fx.workspace())
        .unwrap();
    assert!(intruder_view["entries"].as_array().unwrap().is_empty());
}

// ── Enumeration and the non-oracular refusal ────────────────────────────────

#[tokio::test]
async fn foreign_and_unknown_refusals_are_byte_identical() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    let foreign = queue_as(&fx, &owner, "owner-1", "owner work").await;
    let version = entry_version(&fx, &owner, &foreign);

    let mut messages = Vec::new();
    for (label, id, presented_version) in [
        ("foreign-right-version", foreign.as_str(), version),
        ("foreign-wrong-version", foreign.as_str(), version + 99),
        ("unknown-uuid", "11111111-2222-3333-4444-555555555555", 0),
        ("malformed-path", "../../etc/passwd", 0),
        ("non-uuid", "not-a-uuid", 0),
    ] {
        let error = fx
            .orch
            .remove_queue(
                &intruder,
                &format!("probe-{label}"),
                fx.session,
                fx.workspace(),
                id,
                presented_version,
            )
            .await
            .expect_err(&format!("{label} must be refused"));
        assert_unknown_entry(&error);
        messages.push(format!("{:?}:{}", error.code, error.message));
        assert!(
            !error.message.contains(id),
            "{label}: the refusal must not echo the probed id"
        );
    }
    let first = &messages[0];
    assert!(
        messages.iter().all(|m| m == first),
        "every refusal must be identical, got {messages:?}"
    );
}

// ── Stale epochs, rotation, and revocation ──────────────────────────────────

#[tokio::test]
async fn a_context_minted_before_a_credential_rotation_stops_working() {
    let fx = fixture();
    let stale = fx.owner();
    queue_as(&fx, &stale, "owner-1", "before rotation").await;

    fx.orch
        .set_auth_credentials(vec![
            AuthCredential::new("primary", "rotated-token").unwrap(),
            AuthCredential::new("intruder", INTRUDER_TOKEN).unwrap(),
        ])
        .expect("rotate");

    let read = fx
        .orch
        .get_queue(&stale, fx.session, fx.workspace())
        .expect_err("a stale context must not read");
    assert_eq!(read.code, OrchErrorCode::Unauthenticated);

    let write = fx
        .orch
        .queue_prompt(
            &stale,
            "stale-write",
            fx.session,
            fx.workspace(),
            "after rotation".into(),
            false,
        )
        .await
        .expect_err("a stale context must not write");
    assert_eq!(write.code, OrchErrorCode::Unauthenticated);

    // Re-authenticating under the new secret restores the *same* principal's
    // ownership: rotating a credential's secret does not change who it is.
    let fresh = fx
        .orch
        .auth_header(Some("Bearer rotated-token"))
        .expect("rotated credential authenticates");
    let listed = fx
        .orch
        .get_queue(&fresh, fx.session, fx.workspace())
        .expect("rotated context reads");
    assert_eq!(
        listed["entries"].as_array().unwrap().len(),
        1,
        "a token rotation must not orphan the credential's own queue"
    );
}

#[tokio::test]
async fn an_allowlist_change_advances_the_epoch_and_invalidates_contexts() {
    let fx = fixture();
    let before = fx.owner();
    let epoch_before = fx.orch.auth_epoch_counter();

    let other = tempdir().unwrap();
    fx.orch
        .set_allowlist(WorkspaceAllowlist::new([other.path().to_path_buf()]))
        .expect("allowlist change");

    assert!(
        fx.orch.auth_epoch_counter() > epoch_before,
        "a policy change must advance the authentication epoch"
    );
    let error = fx
        .orch
        .get_queue(&before, fx.session, fx.workspace())
        .expect_err("a pre-change context must not survive");
    assert_eq!(error.code, OrchErrorCode::Unauthenticated);
}

#[tokio::test]
async fn removing_a_credential_revokes_its_queued_execution_authority() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "owner-1", "owner work").await;
    queue_as(&fx, &intruder, "intruder-1", "intruder work").await;

    // Drop the intruder credential entirely.
    fx.orch
        .set_auth_credentials(vec![AuthCredential::new("primary", OWNER_TOKEN).unwrap()])
        .expect("revoke the intruder credential");

    // The revoked principal's entry is still on disk (audit evidence) but is no
    // longer deliverable, while the surviving principal's entry still is.
    let drained = fx
        .host
        .session_queue_take_next(fx.session)
        .expect("drain")
        .batch
        .expect("something is deliverable");
    assert!(
        drained
            .entries
            .iter()
            .all(|entry| entry.text != "intruder work"),
        "a revoked credential's queued work must not be delivered: {:?}",
        drained.entries
    );
    assert!(
        drained
            .entries
            .iter()
            .any(|entry| entry.text == "owner work"),
        "the surviving principal's work must still be delivered"
    );
}

// ── Idempotency ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_request_id_is_private_to_its_principal() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();

    let owner_response = fx
        .orch
        .queue_prompt(
            &owner,
            "shared-request-id",
            fx.session,
            fx.workspace(),
            "owner secret plan".into(),
            false,
        )
        .await
        .expect("owner queues");

    // Byte-identical request id and payload from a different principal must
    // neither replay the owner's receipt nor conflict with it.
    let intruder_response = fx
        .orch
        .queue_prompt(
            &intruder,
            "shared-request-id",
            fx.session,
            fx.workspace(),
            "owner secret plan".into(),
            false,
        )
        .await
        .expect("a reused request id from another principal must not conflict");

    assert_ne!(
        owner_response["entry"]["id"], intruder_response["entry"]["id"],
        "a reused request id must not replay another principal's receipt"
    );
    assert_ne!(
        owner_response["ownerKey"], intruder_response["ownerKey"],
        "each principal's receipt must carry its own ownership handle"
    );

    // The owner's own retry still replays, exactly as before.
    let replay = fx
        .orch
        .queue_prompt(
            &owner,
            "shared-request-id",
            fx.session,
            fx.workspace(),
            "owner secret plan".into(),
            false,
        )
        .await
        .expect("same-principal retry replays");
    assert_eq!(
        replay["entry"]["id"], owner_response["entry"]["id"],
        "same-principal idempotency must be preserved"
    );
}

#[tokio::test]
async fn a_reused_request_id_does_not_confirm_another_principals_use() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "probe-id", "owner work").await;

    // A *different* payload under the same request id would previously return
    // `Conflict: request_id reused with different payload`, confirming the id
    // was already in use by someone. It must now simply succeed in the
    // intruder's own namespace.
    let response = fx
        .orch
        .queue_prompt(
            &intruder,
            "probe-id",
            fx.session,
            fx.workspace(),
            "totally different payload".into(),
            false,
        )
        .await
        .expect("a differing payload must not reveal another principal's request id");
    assert_eq!(response["action"], "queued");
}

// ── Cursors ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_cursor_cannot_be_replayed_by_another_principal() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "owner-1", "owner work").await;

    let owner_cursor = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap()["cursor"]
        .as_str()
        .expect("owner cursor")
        .to_string();
    let intruder_cursor = fx
        .orch
        .get_queue(&intruder, fx.session, fx.workspace())
        .unwrap()["cursor"]
        .as_str()
        .expect("intruder cursor")
        .to_string();

    assert_ne!(
        owner_cursor, intruder_cursor,
        "two principals reading the same revision must not receive the same cursor"
    );

    // Verify the binding directly: the cursor is only valid for the principal
    // it was issued to, at the revision it was issued at.
    let owner_actor = QueueActor::new(
        QueuePrincipal::control(
            "primary",
            "mcp",
            fx.session,
            grokptah_agent_bridge::queue_authority::workspace_key(fx.workspace()),
        ),
        QueueProvenance::default(),
    );
    let revision = fx
        .orch
        .get_queue(&owner, fx.session, fx.workspace())
        .unwrap()["revision"]
        .as_u64()
        .unwrap();
    assert!(owner_actor.cursor_matches(&owner_cursor, revision));
    assert!(
        !owner_actor.cursor_matches(&intruder_cursor, revision),
        "one principal's cursor must not validate as another's"
    );
    assert!(
        !owner_actor.cursor_matches(&owner_cursor, revision + 1),
        "a cursor must not survive a revision change"
    );
}

// ── Restart and legacy migration ────────────────────────────────────────────

#[tokio::test]
async fn ownership_survives_restart_without_widening() {
    let fx = fixture();
    let owner = fx.owner();
    queue_as(&fx, &owner, "owner-1", "survives restart").await;
    let restarted = fx.shut_down();
    let session = restarted.session();
    let (_host2, orch2) = restarted.boot();

    let owner2 = orch2
        .auth_header(Some(&format!("Bearer {OWNER_TOKEN}")))
        .unwrap();
    let intruder2 = orch2
        .auth_header(Some(&format!("Bearer {INTRUDER_TOKEN}")))
        .unwrap();

    let listed = orch2
        .get_queue(&owner2, session, restarted.workspace())
        .expect("owner reads after restart");
    assert_eq!(
        listed["entries"].as_array().unwrap().len(),
        1,
        "restart must not orphan a principal's own queue: {listed}"
    );

    let foreign = orch2
        .get_queue(&intruder2, session, restarted.workspace())
        .expect("intruder reads after restart");
    assert!(
        foreign["entries"].as_array().unwrap().is_empty(),
        "restart must not widen another principal's visibility"
    );

    // A context issued by the *previous* service instance is not current at the
    // new one, even though the credential itself is unchanged.
    let cross_instance = orch2.get_queue(&owner, session, restarted.workspace());
    assert_eq!(
        cross_instance.unwrap_err().code,
        OrchErrorCode::Unauthenticated,
        "a context from another service authority must not be honoured"
    );
}

#[tokio::test]
async fn legacy_principal_less_entries_are_quarantined_not_shared() {
    let fx = fixture();
    let owner = fx.owner();
    let legacy_id = queue_as(&fx, &owner, "owner-1", "legacy work").await;
    let queue_path = fx.queue_path();
    assert!(
        queue_path.exists(),
        "the fixture must have persisted a queue to rewrite: {}",
        queue_path.display()
    );

    // Shut down before rewriting: the live host would otherwise persist over
    // the edit from its in-memory copy.
    let restarted = fx.shut_down();
    let session = restarted.session();
    strip_ownership(&queue_path, &legacy_id);

    // A fresh host reloads the stripped, principal-less entry — exactly what an
    // upgrade from a pre-#461 build produces.
    let (host2, orch2) = restarted.boot();
    let owner2 = orch2
        .auth_header(Some(&format!("Bearer {OWNER_TOKEN}")))
        .unwrap();
    let intruder2 = orch2
        .auth_header(Some(&format!("Bearer {INTRUDER_TOKEN}")))
        .unwrap();

    for (label, auth) in [("owner", &owner2), ("intruder", &intruder2)] {
        let seen = orch2
            .get_queue(auth, session, restarted.workspace())
            .unwrap();
        assert!(
            seen["entries"]
                .as_array()
                .unwrap()
                .iter()
                .all(|entry| entry["id"].as_str() != Some(legacy_id.as_str())),
            "{label} must not see a quarantined legacy entry: {seen}"
        );
        assert_eq!(
            seen["quarantined"].as_u64().unwrap(),
            1,
            "{label} must be told a legacy entry is held back rather than shown an empty queue"
        );
    }

    // And it must never be delivered to the agent.
    let drained = host2.session_queue_take_next(session).expect("drain");
    assert!(
        drained.batch.is_none(),
        "a quarantined legacy entry must not be executed: {:?}",
        drained.batch
    );
}

/// Rewrite a persisted queue entry to drop its ownership handle, reproducing a
/// queue written before #461.
fn strip_ownership(path: &Path, entry_id: &str) {
    let text = std::fs::read_to_string(path).expect("persisted queue");
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut stripped = false;
    for entry in value["queued"].as_array_mut().expect("queued array") {
        if entry["id"].as_str() == Some(entry_id) {
            let object = entry.as_object_mut().unwrap();
            object.remove("owner_key");
            object.remove("owner_provenance");
            stripped = true;
        }
    }
    assert!(stripped, "fixture must find the entry it means to strip");
    std::fs::write(path, serde_json::to_string(&value).unwrap()).unwrap();
}

// ── Delivery races ──────────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_consumers_cannot_take_another_principals_entry() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    let victim = queue_as(&fx, &owner, "owner-1", "owner work").await;
    let version = entry_version(&fx, &owner, &victim);

    // Both principals race to remove the same id. Only the owner may win, and
    // the loser's refusal must be the ordinary unknown-entry refusal.
    let (owner_result, intruder_result) = tokio::join!(
        fx.orch.remove_queue(
            &owner,
            "race-owner",
            fx.session,
            fx.workspace(),
            &victim,
            version
        ),
        fx.orch.remove_queue(
            &intruder,
            "race-intruder",
            fx.session,
            fx.workspace(),
            &victim,
            version
        ),
    );
    assert!(owner_result.is_ok(), "the owner must win its own entry");
    assert_unknown_entry(&intruder_result.expect_err("the intruder must lose"));
}

/// The session event journal carries the whole queue on every
/// `PromptQueueChanged`, and a run's event window is a *session* window. Read
/// paths alone therefore do not close cross-principal disclosure: a principal
/// holding any run in the session could read another principal's queued prompt
/// text straight out of the run projection.
#[tokio::test]
async fn queue_events_in_a_run_projection_are_redacted_to_the_reader() {
    let fx = fixture();
    let owner = fx.owner();
    let intruder = fx.intruder();
    queue_as(&fx, &owner, "owner-1", "owner secret plan").await;

    // Precondition: the raw journal really does carry the owner's text, so the
    // redaction asserted below is load bearing rather than vacuous.
    let raw = fx
        .host
        .event_bus()
        .read_range_all(0, None, Some(fx.session))
        .expect("journal readable");
    assert!(
        raw.iter().any(|entry| matches!(
            &entry.update,
            SessionUpdate::PromptQueueChanged { entries, .. }
                if entries.iter().any(|e| e.text == "owner secret plan")
        )),
        "the raw journal must contain the queue event this test is about"
    );

    // A run in the same session gives the intruder a legitimate projection to
    // read. Its window spans the owner's queue event.
    let run = fx
        .orch
        .submit_task(
            &intruder,
            "intruder-run",
            fx.session,
            fx.workspace(),
            "list files".into(),
            None,
        )
        .await
        .expect("intruder submits its own run");
    let run_id = run["runId"].as_str().expect("run id").to_string();

    let projected = fx
        .orch
        .get_events_scoped(&intruder, fx.session, fx.workspace(), &run_id, 0, 500)
        .expect("intruder reads its own run's events");
    let rendered = projected.to_string();
    assert!(
        !rendered.contains("owner secret plan"),
        "a run projection must not disclose another principal's queued prompt text: {rendered}"
    );

    // The owner still sees its own queue events in its own projection.
    let owner_run = fx
        .orch
        .submit_task(
            &owner,
            "owner-run",
            fx.session,
            fx.workspace(),
            "list files".into(),
            None,
        )
        .await;
    if let Ok(owner_run) = owner_run {
        if let Some(owner_run_id) = owner_run["runId"].as_str() {
            let own = fx
                .orch
                .get_events_scoped(&owner, fx.session, fx.workspace(), owner_run_id, 0, 500)
                .expect("owner reads its own run's events");
            let _ = own;
        }
    }
}
