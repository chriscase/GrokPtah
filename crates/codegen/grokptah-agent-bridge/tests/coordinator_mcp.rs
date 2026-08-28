//! Independent MCP worker/coordinator conformance for #307.

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, AgentState, AuthCredential,
    HostConfig, McpControlClient, SessionKind,
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
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", "coord-token-307").unwrap(),
        AuthCredential::new("worker", "worker-token-307").unwrap(),
    ])
    .unwrap();
    assert!(
        orch.auth_header(Some("Bearer worker-token-307")).is_ok(),
        "named worker credential must authenticate after host installation"
    );
    let primary_actor = orch
        .auth_header(Some("Bearer coord-token-307"))
        .unwrap()
        .actor_handle()
        .as_str()
        .to_string();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut coordinator =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    coordinator.initialize().await.unwrap();
    // This legacy workflow models an explicitly shared coordinator/worker
    // service account. Separate principals must use separate scoped lanes;
    // the adversarial authority suite covers that denial.
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
    let offered = coordinator
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
    assert_eq!(offered.structured["decision"]["actorHandle"], primary_actor);
    assert_eq!(
        offered.structured["decision"]["actorAgentId"],
        manager.agent_id
    );
    assert_eq!(
        offered.structured["decision"]["assignedAgentId"],
        worker.agent_id
    );
    assert_eq!(offered.structured["decision"]["policyRevision"], 1);

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
    let accepted = worker_client
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
    assert_eq!(
        accepted.structured["decision"]["actorHandle"],
        primary_actor
    );
    assert_eq!(
        accepted.structured["decision"]["actorAgentId"],
        worker.agent_id
    );
    assert_eq!(accepted.structured["decision"]["policyRevision"], 1);
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
    assert_eq!(question.structured["message"]["actorHandle"], primary_actor);
    assert_eq!(
        question.structured["message"]["fromAgentId"],
        worker.agent_id
    );
    let answer = coordinator
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
    assert_eq!(answer.structured["message"]["actorHandle"], primary_actor);
    assert_eq!(
        answer.structured["message"]["fromAgentId"],
        manager.agent_id
    );
    let ack = coordinator
        .call_tool(
            "ptah_ack_message",
            json!({
                "request_id": "ack-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "message_id": question_id
            }),
        )
        .await
        .unwrap();
    assert!(ack.structured["message"]["actorHandle"]
        .as_str()
        .is_some_and(|handle| handle.starts_with("actor_")));
    assert!(ack.structured["message"]["ackedAt"].is_string());
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
    let replay_offer = coordinator
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
    assert_eq!(
        replay_offer.structured["decision"]["decisionId"],
        offered.structured["decision"]["decisionId"]
    );
    let replay_accept = worker_client
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
    assert_eq!(
        replay_accept.structured["decision"]["decisionId"],
        accepted.structured["decision"]["decisionId"]
    );
    let replay_ack = coordinator
        .call_tool(
            "ptah_ack_message",
            json!({
                "request_id": "ack-307",
                "session_id": lane.id,
                "workspace": workspace_text,
                "message_id": question_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        replay_ack.structured["message"]["ackedAt"],
        ack.structured["message"]["ackedAt"]
    );
    assert_eq!(
        replay_ack.structured["message"]["ackedBy"],
        ack.structured["message"]["ackedBy"]
    );
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

    let decisions = coordinator
        .call_tool(
            "ptah_list_work_decisions",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": child_id
            }),
        )
        .await
        .unwrap();
    let decision_ids: Vec<&str> = decisions.structured["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|decision| decision["decisionId"].as_str().unwrap())
        .collect();
    let mut unique_ids = decision_ids.clone();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    assert_eq!(decision_ids.len(), unique_ids.len());
    let inbox = coordinator
        .call_tool(
            "ptah_list_inbox",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": manager.agent_id,
                "after_seq": 0
            }),
        )
        .await
        .unwrap();
    let question_copies = inbox.structured["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["messageId"] == question_id)
        .count();
    assert_eq!(question_copies, 1);

    orch.stop_background_tasks().await;
    coordinator.close_session().await.unwrap();
    worker_client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn coordinator_identity_and_scope_are_enforced() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace_a = tempdir().unwrap();
    let workspace_b = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane_a = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_a.id, workspace_a.path()).unwrap();
    let lane_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_b.id, workspace_b.path()).unwrap();
    let manager = host.ensure_session_agent(lane_a.id).unwrap();
    let worker = {
        let store = host.ensure_orchestration_store().unwrap();
        let mut worker = manager.clone();
        worker.agent_id = format!("worker-{}", lane_a.id);
        if let Some(spec) = worker.spec.as_mut() {
            spec.display_name = "worker".into();
        }
        store.save_agent(&worker).unwrap();
        worker
    };
    let foreign = host.ensure_session_agent(lane_b.id).unwrap();
    let inactive = {
        let store = host.ensure_orchestration_store().unwrap();
        let mut inactive = manager.clone();
        inactive.agent_id = format!("inactive-{}", lane_a.id);
        inactive.state = AgentState::Completed;
        if let Some(spec) = inactive.spec.as_mut() {
            spec.display_name = "inactive".into();
        }
        store.save_agent(&inactive).unwrap();
        inactive
    };
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "coord-token-307".into(),
            allowlist: WorkspaceAllowlist::new([
                workspace_a.path().to_path_buf(),
                workspace_b.path().to_path_buf(),
            ]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", "coord-token-307").unwrap(),
        AuthCredential::new("worker", "worker-token-307").unwrap(),
    ])
    .unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut coordinator =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    coordinator.initialize().await.unwrap();
    let mut worker_client =
        McpControlClient::new(format!("http://{}", server.addr), "worker-token-307");
    worker_client.initialize().await.unwrap();
    let workspace_a_text = workspace_a.path().display().to_string();
    let workspace_b_text = workspace_b.path().display().to_string();

    let sent = coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "scope-msg",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "status",
                "from_agent_id": manager.agent_id,
                "to_agent_id": worker.agent_id,
                "body": "in scope"
            }),
        )
        .await
        .unwrap();
    let message_id = sent.structured["message"]["messageId"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(sent.structured["message"]["actorHandle"]
        .as_str()
        .is_some_and(|handle| handle.starts_with("actor_")));

    let ack_err = coordinator
        .call_tool(
            "ptah_ack_message",
            json!({
                "request_id": "scope-ack-foreign",
                "session_id": lane_b.id,
                "workspace": workspace_b_text,
                "message_id": message_id
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        ack_err.contains("forbidden_scope") || ack_err.contains("403"),
        "cross-workspace ack must fail: {ack_err}"
    );
    let stored = orch.store().load_message(&message_id).unwrap().unwrap();
    assert!(stored.acked_at.is_none());
    assert!(stored.acked_by.is_none());

    let unknown = coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "unknown-from",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "status",
                "from_agent_id": "missing-agent",
                "to_agent_id": worker.agent_id,
                "body": "unknown"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        unknown.contains("invalid_request") || unknown.contains("400"),
        "unknown from_agent_id must fail: {unknown}"
    );
    let foreign_to = coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "foreign-to",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "status",
                "from_agent_id": manager.agent_id,
                "to_agent_id": foreign.agent_id,
                "body": "foreign"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        foreign_to.contains("forbidden_scope") || foreign_to.contains("403"),
        "foreign to_agent_id must fail: {foreign_to}"
    );
    let inactive_from = coordinator
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "inactive-from",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "status",
                "from_agent_id": inactive.agent_id,
                "to_agent_id": worker.agent_id,
                "body": "inactive"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        inactive_from.contains("conflict") || inactive_from.contains("409"),
        "inactive from_agent_id must fail: {inactive_from}"
    );

    let hb_err = coordinator
        .call_tool(
            "ptah_heartbeat_worker",
            json!({
                "request_id": "hb-foreign",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "agent_id": foreign.agent_id,
                "host_kind": "service"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        hb_err.contains("forbidden_scope") || hb_err.contains("403"),
        "foreign heartbeat must fail: {hb_err}"
    );

    let created = coordinator
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "scope-work",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "child",
                "objective": "identity checks"
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let offer_foreign = coordinator
        .call_tool(
            "ptah_offer_work",
            json!({
                "request_id": "offer-foreign",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "work_id": work_id,
                "agent_id": foreign.agent_id,
                "reason": "should fail",
                "manager_agent_id": manager.agent_id
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        offer_foreign.contains("forbidden_scope") || offer_foreign.contains("403"),
        "foreign offer must fail: {offer_foreign}"
    );

    coordinator
        .call_tool(
            "ptah_offer_work",
            json!({
                "request_id": "offer-local",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "work_id": work_id,
                "agent_id": worker.agent_id,
                "reason": "local worker",
                "manager_agent_id": manager.agent_id
            }),
        )
        .await
        .unwrap();
    let accept_unknown = coordinator
        .call_tool(
            "ptah_accept_work",
            json!({
                "request_id": "accept-unknown",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "work_id": work_id,
                "agent_id": "missing-agent",
                "reason": "impersonate"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        accept_unknown.contains("invalid_request") || accept_unknown.contains("400"),
        "accept as unknown agent must fail: {accept_unknown}"
    );

    let acting = worker_client
        .call_tool(
            "ptah_send_message",
            json!({
                "request_id": "acting-on-behalf",
                "session_id": lane_a.id,
                "workspace": workspace_a_text,
                "kind": "status",
                "from_agent_id": manager.agent_id,
                "to_agent_id": worker.agent_id,
                "body": "worker credential naming the manager agent"
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        acting.contains("unauthenticated") || acting.contains("401"),
        "separate principal must not inherit another principal's session/work scope: {acting}"
    );

    orch.stop_background_tasks().await;
    coordinator.close_session().await.unwrap();
    worker_client.close_session().await.unwrap();
}
