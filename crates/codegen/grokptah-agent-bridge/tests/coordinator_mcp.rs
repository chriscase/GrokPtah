//! Independent MCP worker/coordinator conformance for #307.

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
async fn independent_worker_recovers_assignment_and_messages() {
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
    let manager = host.ensure_session_agent(lane.id).unwrap();
    let worker = {
        let store = host.ensure_orchestration_store().unwrap();
        let mut worker = manager.clone();
        worker.agent_id = format!("worker-{}", lane.id);
        if let Some(spec) = worker.spec.as_mut() {
            spec.display_name = "worker".into();
        }
        store.save_agent(&worker).unwrap();
        worker
    };
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "coord-token-307".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut coordinator =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    coordinator.initialize().await.unwrap();
    let mut worker_client =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    worker_client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    coordinator
        .call_tool(
            "ptah_heartbeat_worker",
            json!({
                "request_id": "hb-manager",
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": manager.agent_id,
                "host_kind": "service"
            }),
        )
        .await
        .unwrap();
    worker_client
        .call_tool(
            "ptah_heartbeat_worker",
            json!({
                "request_id": "hb-worker",
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": worker.agent_id,
                "host_kind": "service"
            }),
        )
        .await
        .unwrap();
    let workers = coordinator
        .call_tool(
            "ptah_list_workers",
            json!({"session_id": lane.id, "workspace": workspace_text}),
        )
        .await
        .unwrap();
    assert!(workers.structured["workers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["agentId"] == worker.agent_id));

    let parent = coordinator
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "parent-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "parent",
                "objective": "Parent objective"
            }),
        )
        .await
        .unwrap();
    let parent_id = parent.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let child = coordinator
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "child-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "child",
                "objective": "Child objective",
                "parent_work_id": parent_id
            }),
        )
        .await
        .unwrap();
    let child_id = child.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    coordinator
        .call_tool(
            "ptah_offer_work",
            json!({
                "request_id": "offer-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "agent_id": worker.agent_id,
                "reason": "eligible worker",
                "manager_agent_id": manager.agent_id
            }),
        )
        .await
        .unwrap();

    worker_client.close_session().await.unwrap();
    let mut worker_client =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    worker_client.initialize().await.unwrap();
    let inbox = worker_client
        .call_tool(
            "ptah_list_inbox",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": worker.agent_id,
                "after_seq": 0
            }),
        )
        .await
        .unwrap();
    assert!(inbox.structured["messages"].as_array().unwrap().is_empty());
    worker_client
        .call_tool(
            "ptah_accept_work",
            json!({
                "request_id": "accept-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "agent_id": worker.agent_id,
                "reason": "accepted after reconnect"
            }),
        )
        .await
        .unwrap();
    let claimed = worker_client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "claim-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "agent_id": worker.agent_id
            }),
        )
        .await
        .unwrap();
    let attempt_id = claimed.structured["attempt"]["attemptId"]
        .as_str()
        .unwrap()
        .to_string();
    let lease = claimed.structured["leaseToken"]
        .as_str()
        .unwrap()
        .to_string();
    worker_client
        .call_tool(
            "ptah_report_work_progress",
            json!({
                "request_id": "progress-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "attempt_id": attempt_id,
                "lease_token": lease,
                "summary": "halfway",
                "percent": 50
            }),
        )
        .await
        .unwrap();
    let question = worker_client
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "q-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "question",
                "from_agent_id": worker.agent_id,
                "to_agent_id": manager.agent_id,
                "work_id": child_id,
                "attempt_id": attempt_id,
                "body": "Which fixture should I use?"
            }),
        )
        .await
        .unwrap();
    let question_id = question.structured["message"]["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "a-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "answer",
                "from_agent_id": manager.agent_id,
                "to_agent_id": worker.agent_id,
                "work_id": child_id,
                "reply_to_id": question_id,
                "body": "Use the existing fixture."
            }),
        )
        .await
        .unwrap();
    worker_client
        .call_tool(
            "ptah_complete_work",
            json!({
                "request_id": "complete-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "attempt_id": attempt_id,
                "lease_token": lease,
                "summary": "child finished",
                "evidence": ["test log"]
            }),
        )
        .await
        .unwrap();
    coordinator
        .call_tool(
            "ptah_request_review",
            json!({
                "request_id": "review-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id,
                "reason": "please review evidence"
            }),
        )
        .await
        .unwrap();
    coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "review-result-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "review_result",
                "from_agent_id": manager.agent_id,
                "to_agent_id": worker.agent_id,
                "work_id": child_id,
                "body": "accepted"
            }),
        )
        .await
        .unwrap();
    let replay = coordinator
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "child-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "child",
                "objective": "Child objective",
                "parent_work_id": parent_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay.structured["work"]["workId"], child_id);
    let replay_q = worker_client
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "q-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "question",
                "from_agent_id": worker.agent_id,
                "to_agent_id": manager.agent_id,
                "work_id": child_id,
                "attempt_id": attempt_id,
                "body": "Which fixture should I use?"
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay_q.structured["message"]["messageId"], question_id);

    orch.stop_background_tasks().await;
    coordinator.close_session().await.unwrap();
    worker_client.close_session().await.unwrap();
}
