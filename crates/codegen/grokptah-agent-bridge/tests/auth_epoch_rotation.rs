//! Stale-authentication epoch tests.
//!
//! Every test drives the shipped `OrchestrationService` against a real host,
//! bus and store. The point of each is *ordering*: a context that stopped being
//! current must be refused before the entry point touches the host or the
//! store, not after a partial side effect has landed.

mod common;

use std::path::Path;
use std::sync::Arc;

use grokptah_agent_bridge::orchestration::{
    require_bearer, AuthCredential, OrchErrorCode, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig, WorkPolicy,
};
use tempfile::TempDir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN_A: &str = "epoch-token-alpha";
const TOKEN_B: &str = "epoch-token-bravo";

struct Harness {
    /// Owns the home-override directory for the lifetime of the test.
    _home: TempDir,
    /// `home_override_serial` is a process-global lock, so exactly one guard
    /// may be live per test.
    _guard: ProcessEnvGuard,
    workspace: TempDir,
    spare_workspace: TempDir,
    host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
}

/// Build a service instance over an existing host/home/workspace. Two calls
/// with identical arguments still yield two distinct authorities.
fn service(
    host: &AgentHostHandle,
    home: &TempDir,
    workspace: &TempDir,
    bearer: &str,
) -> Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: bearer.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    )
}

impl Harness {
    /// A second service over the same host and the same durable store, so the
    /// only thing separating it from `self.orch` is its own authority.
    fn sibling_service(&self, bearer: &str) -> Arc<OrchestrationService> {
        OrchestrationService::new(
            self.host.clone(),
            self.host.event_bus(),
            self.host
                .ensure_orchestration_store()
                .expect("host already owns the ledger opened by the first service"),
            OrchestrationConfig {
                bearer_token: bearer.into(),
                allowlist: WorkspaceAllowlist::new([self.workspace.path().to_path_buf()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        )
    }
}

fn harness() -> Harness {
    let mut guard = ProcessEnvGuard::new();
    let home = tempfile::tempdir().unwrap();
    let home_dir = home.path().join(".grokptah");
    std::fs::create_dir_all(&home_dir).unwrap();
    set_grokptah_home_override(Some(home_dir));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");

    let workspace = tempfile::tempdir().unwrap();
    let spare_workspace = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("host starts");
    let orch = service(&host, &home, &workspace, TOKEN_A);
    Harness {
        _home: home,
        _guard: guard,
        workspace,
        spare_workspace,
        host,
        orch,
    }
}

fn session_count(host: &AgentHostHandle) -> usize {
    host.list_sessions().len()
}

fn work_count(orch: &OrchestrationService) -> usize {
    orch.store().list_work_items().unwrap().len()
}

async fn create_work(
    orch: &OrchestrationService,
    auth: &grokptah_agent_bridge::AuthContext,
    request_id: &str,
    session_id: Uuid,
    workspace: &Path,
) -> Result<serde_json::Value, grokptah_agent_bridge::orchestration::OrchError> {
    orch.create_work(
        auth,
        request_id,
        session_id,
        workspace,
        "review".into(),
        "confirm epoch ordering".into(),
        0,
        None,
        None,
        Vec::new(),
        WorkPolicy::default(),
    )
    .await
}

fn session_id_of(value: &serde_json::Value) -> Uuid {
    value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .expect("create_session returns a session id")
}

#[tokio::test]
async fn stale_context_after_credential_rotation_is_refused_before_host_or_store_mutation() {
    let h = harness();
    let before = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();

    // Baseline: the context works and its side effects are observable.
    let session = h
        .orch
        .create_session(&before, h.workspace.path(), None)
        .unwrap();
    let session_id = session_id_of(&session);
    create_work(&h.orch, &before, "work-1", session_id, h.workspace.path())
        .await
        .unwrap();
    assert_eq!(work_count(&h.orch), 1);
    let sessions_at_rotation = session_count(&h.host);

    // Rotate the credential the context was issued under.
    h.orch
        .set_auth_credentials(vec![AuthCredential::new("primary", TOKEN_B).unwrap()])
        .unwrap();

    // The old bearer no longer authenticates at all...
    assert_eq!(
        h.orch
            .auth_header(Some(&format!("Bearer {TOKEN_A}")))
            .unwrap_err()
            .code,
        OrchErrorCode::Unauthenticated
    );

    // ...and the context it previously issued is refused *before* the host is
    // asked to create anything.
    let err = h
        .orch
        .create_session(&before, h.workspace.path(), None)
        .unwrap_err();
    assert_eq!(err.code, OrchErrorCode::Unauthenticated);
    assert_eq!(
        session_count(&h.host),
        sessions_at_rotation,
        "a stale context must not reach the host session provider"
    );

    // Same for a store mutation.
    let err = create_work(&h.orch, &before, "work-2", session_id, h.workspace.path())
        .await
        .unwrap_err();
    assert_eq!(err.code, OrchErrorCode::Unauthenticated);
    assert_eq!(
        work_count(&h.orch),
        1,
        "a stale context must not reach the work store"
    );

    // A context issued under the new credential continues to work, and keeps
    // its named-credential attribution.
    let after = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_B}")))
        .unwrap();
    assert_eq!(after.token_id, "primary");
    assert_eq!(after.owner_id, before.owner_id);
    h.orch
        .create_session(&after, h.workspace.path(), None)
        .unwrap();
    assert_eq!(session_count(&h.host), sessions_at_rotation + 1);
    create_work(&h.orch, &after, "work-3", session_id, h.workspace.path())
        .await
        .unwrap();
    assert_eq!(work_count(&h.orch), 2);
}

#[tokio::test]
async fn stale_context_after_allowlist_rotation_is_refused_before_the_workspace_check() {
    let h = harness();
    let before = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();
    h.orch
        .create_session(&before, h.workspace.path(), None)
        .unwrap();
    let sessions_at_rotation = session_count(&h.host);

    h.orch
        .set_allowlist(WorkspaceAllowlist::new([h
            .spare_workspace
            .path()
            .to_path_buf()]))
        .unwrap();

    // Newly allowlisted workspace, but a context minted under the old policy:
    // the failure must be the stale-auth one, not a workspace verdict reached
    // after the guard.
    let err = h
        .orch
        .create_session(&before, h.spare_workspace.path(), None)
        .unwrap_err();
    assert_eq!(
        err.code,
        OrchErrorCode::Unauthenticated,
        "the epoch guard must run before the allowlist check"
    );
    assert_eq!(session_count(&h.host), sessions_at_rotation);

    // Re-authentication picks up the rotated policy without broadening it: the
    // new root is usable and the dropped root is not.
    let after = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();
    h.orch
        .create_session(&after, h.spare_workspace.path(), None)
        .unwrap();
    assert_eq!(session_count(&h.host), sessions_at_rotation + 1);
    let err = h
        .orch
        .create_session(&after, h.workspace.path(), None)
        .unwrap_err();
    assert_eq!(err.code, OrchErrorCode::ForbiddenScope);
    assert_eq!(session_count(&h.host), sessions_at_rotation + 1);
}

#[tokio::test]
async fn contexts_from_another_service_or_from_the_policy_helper_are_refused() {
    let h = harness();
    // A second service instance over the same host, store, allowlist and bearer
    // token: only the issuing authority differs.
    let other = h.sibling_service(TOKEN_A);

    let from_h = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();
    let from_other = other
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();
    assert_eq!(from_h.token_id, from_other.token_id);
    assert_eq!(from_h.owner_id, from_other.owner_id);
    assert_ne!(
        from_h.epoch(),
        from_other.epoch(),
        "identical credentials must not produce interchangeable contexts"
    );

    let sessions_before = session_count(&h.host);
    for (service, foreign) in [(&h.orch, &from_other), (&other, &from_h)] {
        let err = service
            .create_session(foreign, h.workspace.path(), None)
            .unwrap_err();
        assert_eq!(
            err.code,
            OrchErrorCode::Unauthenticated,
            "a context issued by another service instance must never be current here"
        );
    }
    assert_eq!(session_count(&h.host), sessions_before);

    // The pure-policy helper checks the token but mints no service authority.
    let helper = require_bearer(Some(&format!("Bearer {TOKEN_A}")), TOKEN_A).unwrap();
    assert_eq!(helper.token_id, "primary");
    for service in [&h.orch, &other] {
        let err = service
            .create_session(&helper, h.workspace.path(), None)
            .unwrap_err();
        assert_eq!(err.code, OrchErrorCode::Unauthenticated);
    }
    assert_eq!(session_count(&h.host), sessions_before);

    // Each instance still honours its own context.
    h.orch
        .create_session(&from_h, h.workspace.path(), None)
        .unwrap();
    other
        .create_session(&from_other, h.workspace.path(), None)
        .unwrap();
    assert_eq!(session_count(&h.host), sessions_before + 2);
}

#[tokio::test]
async fn every_rotation_advances_the_epoch_exactly_once() {
    let h = harness();
    let start = h.orch.auth_epoch_counter();

    h.orch.set_token(TOKEN_B.into()).unwrap();
    assert_eq!(h.orch.auth_epoch_counter(), start + 1);

    h.orch
        .set_auth_credentials(vec![AuthCredential::new("primary", TOKEN_A).unwrap()])
        .unwrap();
    assert_eq!(h.orch.auth_epoch_counter(), start + 2);

    h.orch.set_agent_owner_id("account-77".into()).unwrap();
    assert_eq!(h.orch.auth_epoch_counter(), start + 3);

    h.orch
        .set_allowlist(WorkspaceAllowlist::new([h.workspace.path().to_path_buf()]))
        .unwrap();
    assert_eq!(h.orch.auth_epoch_counter(), start + 4);

    // Rejected input must not consume an epoch.
    assert!(h.orch.set_auth_credentials(Vec::new()).is_err());
    assert!(h.orch.set_agent_owner_id("   ".into()).is_err());
    assert_eq!(h.orch.auth_epoch_counter(), start + 4);

    let auth = h
        .orch
        .auth_header(Some(&format!("Bearer {TOKEN_A}")))
        .unwrap();
    assert_eq!(auth.owner_id, "account-77");
    h.orch
        .create_session(&auth, h.workspace.path(), None)
        .unwrap();
}
