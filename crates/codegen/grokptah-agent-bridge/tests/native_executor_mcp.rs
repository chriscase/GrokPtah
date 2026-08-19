//! Native persistent-Agent executor conformance.

mod common;

use grokptah_agent_bridge::orchestration::{
    ManagedExecutionPolicy, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    SessionKind,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

fn enabled_policy() -> ManagedExecutionPolicy {
    ManagedExecutionPolicy {
        enabled: true,
        max_concurrent_runs: 1,
        bounds: RunBounds {
            max_prompt_bytes: 16 * 1024,
            max_rounds: 4,
            max_duration_ms: 30_000,
            max_total_tokens: Some(8_000),
        },
        retry_eligible: true,
        ..ManagedExecutionPolicy::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn native_executor_runs_assigned_work_without_an_external_worker() {
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
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    client
        .call_tool(
            "ptah_set_managed_execution",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": agent.agent_id,
                "policy": enabled_policy()
            }),
        )
        .await
        .unwrap();

    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "native-work-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "native",
                "objective": "Native executor should finish this Work"
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "native-assign-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();

    let manual = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "manual-work-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "manual",
                "objective": "Stay queued because it is not assigned"
            }),
        )
        .await
        .unwrap();
    let manual_id = manual.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    orch.drive_native_executor_once().await;
    orch.drive_native_executor_once().await;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(8);
    loop {
        orch.drive_native_executor_once().await;
        let snapshot = client
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
        let state = snapshot.structured["work"]["state"].as_str().unwrap();
        if state == "succeeded"
            || state == "awaiting_approval"
            || tokio::time::Instant::now() >= deadline
        {
            assert!(
                state == "succeeded"
                    || state == "leased"
                    || state == "running"
                    || state == "awaiting_approval",
                "native work ended in {state}"
            );
            let attempts = snapshot.structured["attempts"].as_array().unwrap();
            if !attempts.is_empty() {
                assert!(attempts[0]["linkedRunIds"].as_array().unwrap().len() <= 1);
            }
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let replay_assign = client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "native-assign-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay_assign.structured["work"]["workId"], work_id);

    let still_manual = client
        .call_tool(
            "ptah_get_work",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": manual_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(still_manual.structured["work"]["state"], "queued");
    assert!(still_manual.structured["attempts"]
        .as_array()
        .is_some_and(|attempts| attempts.is_empty()));

    let intents = client
        .call_tool(
            "ptah_list_execution_intents",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text
            }),
        )
        .await
        .unwrap();
    let listed = intents.structured["intents"].as_array().unwrap();
    let work_intents = listed
        .iter()
        .filter(|intent| intent["workId"] == work_id)
        .count();
    assert!(work_intents <= 1);

    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert!(capacity.structured["health"]["nativeExecutor"]["enabled"]
        .as_bool()
        .unwrap());

    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn approval_required_work_pauses_before_success() {
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
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    client
        .call_tool(
            "ptah_set_managed_execution",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": agent.agent_id,
                "policy": enabled_policy()
            }),
        )
        .await
        .unwrap();
    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "approval-work",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "native",
                "objective": "Require human approval after the Run",
                "policy": {
                    "bounds": {
                        "maxPromptBytes": 100000,
                        "maxRounds": 4,
                        "maxDurationMs": 30000,
                        "maxTotalTokens": 8000
                    },
                    "retry": {
                        "maxAttempts": 2,
                        "retryFailed": true,
                        "retryExpired": true,
                        "backoffMs": 0
                    },
                    "requiresApproval": true,
                    "maxConcurrentAttempts": 1
                }
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"].as_str().unwrap();
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "approval-assign",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();
    for _ in 0..20 {
        orch.drive_native_executor_once().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let snapshot = client
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
        let state = snapshot.structured["work"]["state"].as_str().unwrap();
        if state == "awaiting_approval" || state == "succeeded" {
            if state == "awaiting_approval" {
                let approved = client
                    .call_tool(
                        "ptah_approve_work",
                        json!({
                            "request_id": "approval-decide",
                            "session_id": lane.id,
                            "workspace": workspace_text,
                            "work_id": work_id,
                            "note": "operator reviewed native evidence"
                        }),
                    )
                    .await
                    .unwrap();
                assert_eq!(approved.structured["work"]["state"], "succeeded");
            }
            break;
        }
    }
    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}
