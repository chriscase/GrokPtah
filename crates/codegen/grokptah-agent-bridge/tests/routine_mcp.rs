//! MCP/service fixture for durable routine create/inspect/manual fire (#306).

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    SessionKind,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn independent_mcp_client_can_inspect_and_manually_activate_a_routine() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "routine-token-306".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "routine-token-306");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    let created = client
        .call_tool(
            "ptah_create_routine",
            json!({
                "request_id": "routine-create-306",
                "session_id": lane.id,
                "workspace": workspace_text,
                "name": "manual-check",
                "agent_id": agent.agent_id,
                "trigger": { "kind": "manual" },
                "work_template": {
                    "kind": "routine",
                    "objective": "Create eligible Work from a durable routine",
                    "priority": 0,
                    "policy": {
                        "bounds": {
                            "maxPromptBytes": 100000,
                            "maxRounds": 4,
                            "maxDurationMs": 60000
                        },
                        "retry": {
                            "maxAttempts": 3,
                            "retryFailed": true,
                            "retryExpired": true,
                            "backoffMs": 0
                        },
                        "requiresApproval": false,
                        "maxConcurrentAttempts": 1
                    }
                }
            }),
        )
        .await
        .unwrap();
    let routine_id = created.structured["routine"]["routineId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created.structured["routine"]["lifecycle"], "enabled");

    let listed = client
        .call_tool(
            "ptah_list_routines",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text
            }),
        )
        .await
        .unwrap();
    assert!(listed.structured["routines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|routine| routine["routineId"] == routine_id));

    let fired = client
        .call_tool(
            "ptah_fire_routine",
            json!({
                "request_id": "routine-fire-306",
                "session_id": lane.id,
                "workspace": workspace_text,
                "routine_id": routine_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        fired.structured["activation"]["disposition"],
        "created_work"
    );
    let work_id = fired.structured["activation"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let replay = client
        .call_tool(
            "ptah_fire_routine",
            json!({
                "request_id": "routine-fire-306",
                "session_id": lane.id,
                "workspace": workspace_text,
                "routine_id": routine_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay.structured["activation"]["workId"], work_id);

    let work = client
        .call_tool(
            "ptah_get_work",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(work.structured["work"]["state"], "queued");
    assert_eq!(work.structured["work"]["sourceRoutineId"], routine_id);

    let history = client
        .call_tool(
            "ptah_list_activations",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "routine_id": routine_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        history.structured["activations"].as_array().unwrap().len(),
        1
    );

    let paused = client
        .call_tool(
            "ptah_pause_routine",
            json!({
                "request_id": "routine-pause-306",
                "session_id": lane.id,
                "workspace": workspace_text,
                "routine_id": routine_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(paused.structured["routine"]["lifecycle"], "paused");

    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(
        capacity.structured["health"]["routineSupervisor"]["enabled"],
        true
    );

    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}
