//! Independent MCP worker/coordinator conformance for #307.

mod common;

use grokptah_agent_bridge::orchestration::{
    AssignmentStatus, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkItem,
    WorkPolicy, WorkerObservatoryProjection, WorkspaceAllowlist,
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
    })
    .expect("acquire the GrokPtah instance lock");
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
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut coordinator =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-307");
    coordinator.initialize().await.unwrap();
    let mut worker_client =
        McpControlClient::new(format!("http://{}", server.addr), "worker-token-307");
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
    assert_eq!(offered.structured["decision"]["actorId"], "primary");
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
        McpControlClient::new(format!("http://{}", server.addr), "worker-token-307");
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
    assert_eq!(accepted.structured["decision"]["actorId"], "worker");
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
    assert_eq!(question.structured["message"]["fromActor"], "worker");
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
    assert_eq!(answer.structured["message"]["fromActor"], "primary");
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
    assert_eq!(ack.structured["message"]["ackedBy"], "primary");
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
    })
    .expect("acquire the GrokPtah instance lock");
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
    assert_eq!(sent.structured["message"]["fromActor"], "primary");

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
    let accept_unknown = worker_client
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
        .unwrap();
    assert_eq!(acting.structured["message"]["fromActor"], "worker");
    assert_eq!(
        acting.structured["message"]["fromAgentId"],
        manager.agent_id
    );

    orch.stop_background_tasks().await;
    coordinator.close_session().await.unwrap();
    worker_client.close_session().await.unwrap();
}

fn assert_observatory_worker_json(worker: &serde_json::Value, workspace: &str) {
    let object = worker
        .as_object()
        .unwrap_or_else(|| panic!("worker object: {worker}"));
    let mut keys: Vec<_> = object.keys().cloned().collect();
    keys.sort();
    let mut allowed: Vec<_> = WorkerObservatoryProjection::allowlisted_json_keys()
        .iter()
        .map(|key| (*key).to_string())
        .collect();
    allowed.sort();
    assert_eq!(keys, allowed, "public worker JSON changed shape: {worker}");
    for key in [
        "workspace",
        "model",
        "declaredTools",
        "measured",
        "policyLimits",
        "activeLeases",
    ] {
        assert!(
            object.get(key).is_none(),
            "public worker JSON leaked top-level {key}: {worker}"
        );
    }
    let load = object
        .get("load")
        .and_then(|value| value.as_object())
        .unwrap_or_else(|| panic!("load object: {worker}"));
    assert!(
        load.get("activeLeases").is_some(),
        "load.activeLeases count must remain on the public worker: {worker}"
    );
    assert!(load.get("attemptId").is_none());
    assert!(load.get("workId").is_none());
    let encoded = worker.to_string();
    assert!(
        !encoded.contains(workspace),
        "public worker JSON leaked workspace path: {encoded}"
    );
    for token in [
        "declaredTools",
        "run_terminal_cmd",
        "web_fetch",
        "attemptId",
        "leaseExpiresAt",
        "providerId",
        "modelId",
        "selectionKey",
        "providerRoute",
        "qualifiedTools",
        "policyLimits",
    ] {
        assert!(
            !encoded.contains(token),
            "public worker JSON leaked {token}: {encoded}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn observatory_reads_redact_and_hide_same_workspace_cross_lane_workers() {
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
    let lane_a = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_a.id, workspace.path()).unwrap();
    let lane_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_b.id, workspace.path()).unwrap();
    let local_template = host.ensure_session_agent(lane_a.id).unwrap();
    let cross_template = host.ensure_session_agent(lane_b.id).unwrap();
    let store = host.ensure_orchestration_store().unwrap();
    let local = {
        let mut local = local_template.clone();
        local.agent_id = format!("obs-local-{}", lane_a.id);
        if let Some(spec) = local.spec.as_mut() {
            spec.display_name = "observatory-local".into();
            spec.authority.allowed_tools = vec!["run_terminal_cmd".into(), "web_fetch".into()];
        }
        store.save_agent(&local).unwrap();
        local
    };
    let cross = {
        let mut cross = cross_template.clone();
        cross.agent_id = format!("obs-cross-{}", lane_b.id);
        if let Some(spec) = cross.spec.as_mut() {
            spec.display_name = "observatory-cross".into();
            spec.authority.allowed_tools = vec!["run_terminal_cmd".into()];
        }
        store.save_agent(&cross).unwrap();
        cross
    };
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "coord-token-obs".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch
        .auth_header(Some("Bearer coord-token-obs"))
        .expect("observatory bearer");
    let workspace_text = workspace.path().display().to_string();
    let model = local
        .spec
        .as_ref()
        .map(|spec| spec.model.clone())
        .expect("local worker model");

    let listed = orch
        .list_workers_scoped(&auth, lane_a.id, workspace.path())
        .expect("list workers in lane A");
    let workers = listed["workers"]
        .as_array()
        .cloned()
        .expect("workers array");
    let ids: Vec<&str> = workers
        .iter()
        .map(|worker| worker["agentId"].as_str().expect("agentId"))
        .collect();
    assert!(
        ids.contains(&local.agent_id.as_str()),
        "lane A list must include the local worker: {ids:?}"
    );
    assert!(
        !ids.contains(&cross.agent_id.as_str()),
        "same-workspace cross-lane worker must be omitted: {ids:?}"
    );
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "observatory list must be sorted by agentId");
    for worker in &workers {
        assert_observatory_worker_json(worker, &workspace_text);
        let encoded = worker.to_string();
        assert!(!encoded.contains(model.selection_key.as_str()));
        assert!(!encoded.contains(model.provider_id.as_str()));
        assert!(!encoded.contains(model.model_id.as_str()));
    }

    let detail = orch
        .get_worker_scoped(&auth, lane_a.id, workspace.path(), &local.agent_id)
        .expect("get local worker");
    assert_observatory_worker_json(&detail["worker"], &workspace_text);

    let unknown = orch
        .get_worker_scoped(
            &auth,
            lane_a.id,
            workspace.path(),
            "missing-observatory-worker",
        )
        .expect_err("unknown worker");
    let foreign = orch
        .get_worker_scoped(&auth, lane_a.id, workspace.path(), &cross.agent_id)
        .expect_err("cross-lane worker");
    assert_eq!(unknown.code, foreign.code);
    assert_eq!(unknown.message, foreign.message);
    assert_eq!(unknown.data, foreign.data);

    orch.stop_background_tasks().await;
}

fn seed_assigned_work(
    store: &OrchStore,
    session_id: uuid::Uuid,
    workspace: &str,
    agent_id: &str,
    objective: &str,
    claim: bool,
) {
    let mut item = WorkItem::new(
        "implementation",
        objective,
        session_id,
        workspace,
        "coordinator",
        WorkPolicy::default(),
    )
    .expect("create assigned work");
    item.assigned_agent_id = Some(agent_id.to_string());
    item.assignment_status = AssignmentStatus::Accepted;
    store.save_work_item(&item).expect("persist assigned work");
    if claim {
        store
            .claim_work(&item.work_id, agent_id, None)
            .expect("claim assigned work");
    }
}

fn observatory_load(worker: &serde_json::Value) -> (u64, u64, u64) {
    let load = worker
        .get("load")
        .unwrap_or_else(|| panic!("load object: {worker}"));
    (
        load["assignedItems"].as_u64().expect("assignedItems"),
        load["queuedItems"].as_u64().expect("queuedItems"),
        load["activeLeases"].as_u64().expect("activeLeases"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn observatory_load_counts_stay_inside_the_requested_lane() {
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
    let lane_a = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_a.id, workspace.path()).unwrap();
    let lane_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_b.id, workspace.path()).unwrap();
    let shared = host.ensure_session_agent(lane_a.id).unwrap();
    host.attach_session_to_agent(lane_b.id, &shared.agent_id)
        .expect("attach shared worker to lane B");
    let store = host.ensure_orchestration_store().unwrap();
    let workspace_text = workspace.path().display().to_string();
    seed_assigned_work(
        &store,
        lane_a.id,
        &workspace_text,
        &shared.agent_id,
        "lane-a queued",
        false,
    );
    seed_assigned_work(
        &store,
        lane_a.id,
        &workspace_text,
        &shared.agent_id,
        "lane-a claimed",
        true,
    );
    seed_assigned_work(
        &store,
        lane_b.id,
        &workspace_text,
        &shared.agent_id,
        "lane-b queued-1",
        false,
    );
    seed_assigned_work(
        &store,
        lane_b.id,
        &workspace_text,
        &shared.agent_id,
        "lane-b queued-2",
        false,
    );
    seed_assigned_work(
        &store,
        lane_b.id,
        &workspace_text,
        &shared.agent_id,
        "lane-b claimed",
        true,
    );
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "coord-token-obs-load".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch
        .auth_header(Some("Bearer coord-token-obs-load"))
        .expect("observatory bearer");

    let listed_a = orch
        .list_workers_scoped(&auth, lane_a.id, workspace.path())
        .expect("list workers in lane A");
    let workers_a = listed_a["workers"]
        .as_array()
        .cloned()
        .expect("lane A workers");
    let shared_a = workers_a
        .iter()
        .find(|worker| worker["agentId"] == shared.agent_id)
        .expect("shared worker in lane A");
    assert_eq!(observatory_load(shared_a), (2, 1, 1));
    let detail_a = orch
        .get_worker_scoped(&auth, lane_a.id, workspace.path(), &shared.agent_id)
        .expect("get shared worker in lane A");
    assert_eq!(observatory_load(&detail_a["worker"]), (2, 1, 1));

    let listed_b = orch
        .list_workers_scoped(&auth, lane_b.id, workspace.path())
        .expect("list workers in lane B");
    let workers_b = listed_b["workers"]
        .as_array()
        .cloned()
        .expect("lane B workers");
    let shared_b = workers_b
        .iter()
        .find(|worker| worker["agentId"] == shared.agent_id)
        .expect("shared worker in lane B");
    assert_eq!(observatory_load(shared_b), (3, 2, 1));
    let detail_b = orch
        .get_worker_scoped(&auth, lane_b.id, workspace.path(), &shared.agent_id)
        .expect("get shared worker in lane B");
    assert_eq!(observatory_load(&detail_b["worker"]), (3, 2, 1));

    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_worker_reads_redact_and_collapse_unknown_inactive_and_cross_lane() {
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
    let lane_a = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_a.id, workspace.path()).unwrap();
    let lane_b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane_b.id, workspace.path()).unwrap();
    let local_template = host.ensure_session_agent(lane_a.id).unwrap();
    let cross_template = host.ensure_session_agent(lane_b.id).unwrap();
    let store = host.ensure_orchestration_store().unwrap();
    let local = {
        let mut local = local_template.clone();
        local.agent_id = format!("mcp-local-{}", lane_a.id);
        if let Some(spec) = local.spec.as_mut() {
            spec.display_name = "mcp-local".into();
            spec.authority.allowed_tools = vec!["run_terminal_cmd".into(), "web_fetch".into()];
        }
        store.save_agent(&local).unwrap();
        local
    };
    let cross = {
        let mut cross = cross_template.clone();
        cross.agent_id = format!("mcp-cross-{}", lane_b.id);
        if let Some(spec) = cross.spec.as_mut() {
            spec.display_name = "mcp-cross".into();
            spec.authority.allowed_tools = vec!["run_terminal_cmd".into()];
        }
        store.save_agent(&cross).unwrap();
        cross
    };
    let inactive = {
        let mut inactive = local_template.clone();
        inactive.agent_id = format!("mcp-inactive-{}", lane_a.id);
        inactive.state = AgentState::Completed;
        if let Some(spec) = inactive.spec.as_mut() {
            spec.display_name = "mcp-inactive".into();
        }
        store.save_agent(&inactive).unwrap();
        inactive
    };
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "coord-token-obs-mcp".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch
        .auth_header(Some("Bearer coord-token-obs-mcp"))
        .expect("observatory bearer");
    let workspace_text = workspace.path().display().to_string();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut coordinator =
        McpControlClient::new(format!("http://{}", server.addr), "coord-token-obs-mcp");
    coordinator.initialize().await.unwrap();

    let listed_mcp = coordinator
        .call_tool(
            "ptah_list_workers",
            json!({"session_id": lane_a.id, "workspace": workspace_text}),
        )
        .await
        .expect("mcp list workers");
    let listed_svc = orch
        .list_workers_scoped(&auth, lane_a.id, workspace.path())
        .expect("service list workers");
    assert_eq!(listed_mcp.structured, listed_svc);
    let workers = listed_mcp.structured["workers"]
        .as_array()
        .cloned()
        .expect("workers array");
    let ids: Vec<&str> = workers
        .iter()
        .map(|worker| worker["agentId"].as_str().expect("agentId"))
        .collect();
    assert!(
        ids.contains(&local.agent_id.as_str()),
        "lane A MCP list must include the local worker: {ids:?}"
    );
    assert!(
        !ids.contains(&cross.agent_id.as_str()),
        "same-workspace cross-lane worker must be omitted from MCP list: {ids:?}"
    );
    assert!(
        !ids.contains(&inactive.agent_id.as_str()),
        "inactive worker must be omitted from MCP list: {ids:?}"
    );
    for worker in &workers {
        assert_observatory_worker_json(worker, &workspace_text);
    }

    let detail_mcp = coordinator
        .call_tool(
            "ptah_get_worker",
            json!({
                "session_id": lane_a.id,
                "workspace": workspace_text,
                "agent_id": local.agent_id
            }),
        )
        .await
        .expect("mcp get local worker");
    let detail_svc = orch
        .get_worker_scoped(&auth, lane_a.id, workspace.path(), &local.agent_id)
        .expect("service get local worker");
    assert_eq!(detail_mcp.structured, detail_svc);
    assert_observatory_worker_json(&detail_mcp.structured["worker"], &workspace_text);

    let unknown = coordinator
        .call_tool(
            "ptah_get_worker",
            json!({
                "session_id": lane_a.id,
                "workspace": workspace_text,
                "agent_id": "missing-mcp-worker"
            }),
        )
        .await
        .expect_err("unknown worker");
    let foreign = coordinator
        .call_tool(
            "ptah_get_worker",
            json!({
                "session_id": lane_a.id,
                "workspace": workspace_text,
                "agent_id": cross.agent_id
            }),
        )
        .await
        .expect_err("cross-lane worker");
    let completed = coordinator
        .call_tool(
            "ptah_get_worker",
            json!({
                "session_id": lane_a.id,
                "workspace": workspace_text,
                "agent_id": inactive.agent_id
            }),
        )
        .await
        .expect_err("inactive worker");
    assert_eq!(unknown.to_string(), foreign.to_string());
    assert_eq!(unknown.to_string(), completed.to_string());
    assert!(
        unknown.to_string().contains("invalid_request"),
        "collapsed MCP worker errors must stay invalid_request: {unknown}"
    );

    orch.stop_background_tasks().await;
    coordinator.close_session().await.unwrap();
}
