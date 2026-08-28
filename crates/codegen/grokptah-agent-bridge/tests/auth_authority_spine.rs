//! Adversarial coverage for the #477 host-issued authority spine.

mod common;

use grokptah_agent_bridge::orchestration::{
    AuthCredential, OrchErrorCode, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
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
            bearer_token: "primary-secret-477".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    (home, env, host, orch, session.id, workspace)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rotation_and_reincarnation_fence_session_work_and_queue_resources() {
    let (_home, _env, host, orch, session_id, workspace) = setup();
    let workspace_path = workspace.path();
    let primary = orch.auth_header(Some("Bearer primary-secret-477")).unwrap();
    let created = orch
        .create_work(
            &primary,
            "create-work-477",
            session_id,
            workspace_path,
            "adversarial".into(),
            "old resource".into(),
            0,
            None,
            None,
            Vec::new(),
            Default::default(),
        )
        .await
        .unwrap();
    let public_work = serde_json::to_string(&created).unwrap();
    assert!(!public_work.contains("primary-secret-477"));
    assert!(!public_work.contains(&workspace_path_string(workspace_path)));
    assert!(public_work.contains("actor_"));
    let work_id = created["work"]["workId"].as_str().unwrap().to_string();
    let queue = orch
        .queue_prompt(
            &primary,
            "queue-477",
            session_id,
            workspace_path,
            "queued resource".into(),
            false,
        )
        .await
        .unwrap();
    let entry_id = queue["entry"]["id"].as_str().unwrap().to_string();
    let run = orch
        .submit_task(
            &primary,
            "run-477",
            session_id,
            workspace_path,
            "bounded offline run".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = run["runId"].as_str().unwrap().to_string();

    // Explicit same-incarnation secret rotation is the continuity path.
    orch.set_token("rotated-secret-477".into());
    let rotated = orch.auth_header(Some("Bearer rotated-secret-477")).unwrap();
    assert_eq!(primary.actor_handle(), rotated.actor_handle());
    assert!(orch
        .get_work_scoped(&rotated, session_id, workspace_path, &work_id)
        .is_ok());
    assert!(orch.get_queue(&rotated, session_id, workspace_path).is_ok());
    assert!(orch
        .get_run_scoped(&rotated, session_id, workspace_path, &run_id)
        .is_ok());

    // Removing a credential and adding the same textual id back is a new
    // incarnation, not a secret rotation.
    orch.set_token(String::new());
    orch.set_token("readded-secret-477".into());
    let readded = orch.auth_header(Some("Bearer readded-secret-477")).unwrap();
    assert_ne!(primary.actor_handle(), readded.actor_handle());
    for result in [
        orch.list_sessions(&readded),
        orch.get_work_scoped(&readded, session_id, workspace_path, &work_id),
        orch.get_queue(&readded, session_id, workspace_path),
        orch.get_run_scoped(&readded, session_id, workspace_path, &run_id),
    ] {
        assert_eq!(result.unwrap_err().code, OrchErrorCode::Unauthenticated);
    }

    // A named credential replacement receives a new incarnation, even when
    // its textual id is unchanged. It cannot inherit the old ledger entries.
    orch.set_auth_credentials(vec![AuthCredential::new(
        "primary",
        "replacement-secret-477",
    )
    .unwrap()])
        .unwrap();
    let replacement = orch
        .auth_header(Some("Bearer replacement-secret-477"))
        .unwrap();
    assert_ne!(primary.actor_handle(), replacement.actor_handle());
    for result in [
        orch.list_sessions(&replacement),
        orch.get_work_scoped(&replacement, session_id, workspace_path, &work_id),
        orch.get_queue(&replacement, session_id, workspace_path),
        orch.get_run_scoped(&replacement, session_id, workspace_path, &run_id),
    ] {
        assert_eq!(
            result.unwrap_err().code,
            OrchErrorCode::Unauthenticated,
            "replacement credential inherited an old resource"
        );
    }

    // Reassigning the owner is another host lifecycle transition and cannot
    // turn the old resource ledger into the new owner's authority.
    orch.set_agent_owner_id("owner-changed-477".into()).unwrap();
    let new_owner = orch
        .auth_header(Some("Bearer replacement-secret-477"))
        .unwrap();
    assert_ne!(replacement.actor_handle(), new_owner.actor_handle());
    assert!(orch
        .get_work_scoped(&new_owner, session_id, workspace_path, &work_id)
        .is_err());
    assert!(orch
        .get_queue(&new_owner, session_id, workspace_path)
        .is_err());

    let public = serde_json::to_string(
        &orch
            .get_work_scoped(&primary, session_id, workspace_path, &work_id)
            .unwrap_err(),
    )
    .unwrap();
    assert!(!public.contains("primary-secret-477"));
    assert!(!public.contains("replacement-secret-477"));
    assert!(!public.contains("owner-changed-477"));
    assert!(!public.contains(&workspace_path.display().to_string()));
    assert!(!entry_id.is_empty());

    orch.stop_background_tasks().await;
    host.stop().unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_foreign_run_denials_are_byte_identical() {
    let (_home, _env, host, orch, session_id, workspace) = setup();
    let primary = orch.auth_header(Some("Bearer primary-secret-477")).unwrap();
    let foreign_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(foreign_session.id, workspace.path())
        .unwrap();
    let foreign = orch
        .submit_task(
            &primary,
            "foreign-run-477",
            foreign_session.id,
            workspace.path(),
            "foreign run".into(),
            None,
        )
        .await
        .unwrap();
    let foreign_run_id = foreign["runId"].as_str().unwrap();
    let unknown = orch
        .get_run_scoped(&primary, session_id, workspace.path(), "unknown-run-477")
        .unwrap_err();
    let foreign = orch
        .get_run_scoped(&primary, session_id, workspace.path(), foreign_run_id)
        .unwrap_err();
    assert_eq!(
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&foreign).unwrap(),
        "unknown and foreign run reads must not form an existence oracle"
    );
    orch.stop_background_tasks().await;
    host.stop().unwrap();
    set_grokptah_home_override(None);
}

fn workspace_path_string(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[test]
fn effect_lease_is_revalidated_at_the_physical_boundary() {
    let (_home, _env, _host, orch, _session_id, _workspace) = setup();
    let auth = orch.auth_header(Some("Bearer primary-secret-477")).unwrap();
    let mut lease = orch
        .mint_effect_lease(&auth, "provider:attempt-477")
        .unwrap();
    orch.rotate_authentication_generation("primary").unwrap();
    let error = orch
        .consume_effect_lease(&auth, &mut lease, "provider:attempt-477")
        .unwrap_err();
    assert_eq!(error.code, OrchErrorCode::Unauthenticated);
}
