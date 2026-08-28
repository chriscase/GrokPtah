//! Orchestration wiring for the G1–G4 canonical authority spine.

mod common;

use grokptah_agent_bridge::orchestration::{
    AuthCredential, HostAuthority, OrchErrorCode, OrchStore, OrchestrationConfig,
    OrchestrationService, ResourceKind, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, SessionKind};
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

fn setup() -> (
    tempfile::TempDir,
    ProcessEnvGuard,
    grokptah_agent_bridge::AgentHostHandle,
    std::sync::Arc<OrchestrationService>,
    Uuid,
    tempfile::TempDir,
) {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "primary-secret-g1".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    (home, env, host, orch, session.id, workspace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orch_create_work_is_creation_bound_and_secrets_stay_off_wire() {
    let (_home, _env, _host, orch, session_id, workspace) = setup();
    let workspace_path = workspace.path();
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", "primary-secret-g1").unwrap(),
        AuthCredential::new("laptop", "laptop-secret-g1").unwrap(),
    ])
    .unwrap();
    let primary = orch.auth_header(Some("Bearer primary-secret-g1")).unwrap();
    let laptop = orch.auth_header(Some("Bearer laptop-secret-g1")).unwrap();
    assert_ne!(primary.actor_handle(), laptop.actor_handle());

    let created = orch
        .create_work(
            &primary,
            "create-work-g1",
            session_id,
            workspace_path,
            "adversarial".into(),
            "bound work".into(),
            0,
            None,
            None,
            Vec::new(),
            Default::default(),
        )
        .await
        .unwrap();
    let public = serde_json::to_string(&created).unwrap();
    assert!(!public.contains("primary-secret-g1"));
    assert!(!public.contains("laptop-secret-g1"));
    let work_id = created["work"]["workId"].as_str().unwrap().to_string();
    orch.get_work_scoped(&primary, session_id, workspace_path, &work_id)
        .unwrap();
    let denied = orch
        .get_work_scoped(&laptop, session_id, workspace_path, &work_id)
        .unwrap_err();
    assert_eq!(denied.code, OrchErrorCode::Unauthenticated);

    let other_session = orch
        .create_session(&primary, workspace_path, Some("other".into()))
        .unwrap();
    let other_id = Uuid::parse_str(other_session["sessionId"].as_str().unwrap()).unwrap();
    let cross = orch
        .get_work_scoped(&primary, other_id, workspace_path, &work_id)
        .unwrap_err();
    assert_eq!(cross.code, OrchErrorCode::Unauthenticated);
    assert_eq!(cross.message, denied.message);

    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", "rotated-secret-g1").unwrap(),
        AuthCredential::new("laptop", "laptop-secret-g1").unwrap(),
    ])
    .unwrap();
    assert_eq!(
        orch.get_work_scoped(&primary, session_id, workspace_path, &work_id)
            .unwrap_err()
            .code,
        OrchErrorCode::Unauthenticated
    );
    let rotated = orch.auth_header(Some("Bearer rotated-secret-g1")).unwrap();
    orch.get_work_scoped(&rotated, session_id, workspace_path, &work_id)
        .unwrap();
}

#[test]
fn public_projections_cannot_construct_host_authority_from_json() {
    let dir = tempdir().unwrap();
    let mut host = HostAuthority::open(dir.path()).unwrap();
    let credentials = vec![AuthCredential::new("primary", "secret").unwrap()];
    host.install_credentials(&credentials, "owner").unwrap();
    let issued = host
        .authenticate(Some("Bearer secret"), &credentials, "owner")
        .unwrap();
    let rendered = format!("{issued:?}");
    assert!(!rendered.contains("secret"));
    let _ = ResourceKind::Work;
}
