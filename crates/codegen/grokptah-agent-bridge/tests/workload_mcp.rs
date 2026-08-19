//! MCP conformance for the shared durable workload surface (#305).

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkPolicy, WorkRetryPolicy,
    WorkspaceAllowlist,
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
async fn workload_protocol_is_idempotent_scoped_and_lane_archive_safe() {
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
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "workload-token-305".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "workload-token-305");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    let create_args = json!({
        "request_id": "work-create-305",
        "session_id": lane.id,
        "workspace": workspace_text,
        "kind": "verification",
        "objective": "Run the hosted workload conformance test"
    });
    let created = client
        .call_tool("ptah_create_work", create_args.clone())
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let replay = client
        .call_tool("ptah_create_work", create_args)
        .await
        .unwrap();
    assert_eq!(replay.structured["work"]["workId"], work_id);
    assert!(client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "work-create-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "A conflicting replay must not mutate the original"
            }),
        )
        .await
        .is_err());

    let claimed = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-claim-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id
            }),
        )
        .await
        .unwrap();
    let attempt_id = claimed.structured["attempt"]["attemptId"]
        .as_str()
        .unwrap()
        .to_string();
    let lease_token = claimed.structured["leaseToken"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(claimed.structured["work"]["state"], "leased");
    assert!(claimed.structured["attempt"]
        .get("leaseTokenHash")
        .is_none());

    let claim_replay = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-claim-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(claim_replay.structured["attempt"]["attemptId"], attempt_id);
    assert_eq!(claim_replay.structured["leaseToken"], lease_token);
    let receipt_name = format!(
        "{}.json",
        grokptah_agent_bridge::safe_id_filename("work-claim-305").unwrap()
    );
    let receipt_paths = [
        home.path()
            .join("orchestration")
            .join("idempotency")
            .join(&receipt_name),
        home.path()
            .join(".grokptah")
            .join("orchestration")
            .join("idempotency")
            .join(&receipt_name),
    ];
    let claim_receipt = receipt_paths
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .expect("durable claim receipt");
    assert!(!claim_receipt.contains("leaseToken"));

    let duplicate_claim = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-claim-305-duplicate",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id
            }),
        )
        .await;
    assert!(duplicate_claim.is_err());

    client
        .call_tool(
            "ptah_report_work_progress",
            json!({
                "request_id": "work-progress-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "conformance worker is running",
                "percent": 50
            }),
        )
        .await
        .unwrap();

    let final_read = client
        .call_tool(
            "ptah_complete_work",
            json!({
                "request_id": "work-complete-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "attempt_id": attempt_id,
                "lease_token": lease_token,
                "summary": "work completed",
                "evidence": ["MCP client observed the durable result"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(final_read.structured["work"]["state"], "succeeded");
    assert!(final_read.structured["attempt"]
        .get("leaseTokenHash")
        .is_none());

    let work_snapshot = client
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
    assert!(work_snapshot.structured["attempts"][0]
        .get("leaseTokenHash")
        .is_none());

    let mut approval_policy = WorkPolicy::default();
    approval_policy.requires_approval = true;
    let approval_work = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "work-approval-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "approval-proof",
                "objective": "Require an explicit human completion decision",
                "policy": approval_policy,
            }),
        )
        .await
        .unwrap();
    let approval_id = approval_work.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let approval_revision = approval_work.structured["work"]["revision"]
        .as_u64()
        .unwrap();
    let assigned = client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "work-assign-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": approval_id,
                "assigned_agent_id": "review-agent",
                "expected_revision": approval_revision,
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        assigned.structured["work"]["assignedAgentId"],
        "review-agent"
    );
    let approval_claim = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-approval-claim-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": approval_id,
            }),
        )
        .await
        .unwrap();
    let approval_attempt = approval_claim.structured["attempt"]["attemptId"]
        .as_str()
        .unwrap()
        .to_string();
    let approval_token = approval_claim.structured["leaseToken"]
        .as_str()
        .unwrap()
        .to_string();
    let awaiting = client
        .call_tool(
            "ptah_complete_work",
            json!({
                "request_id": "work-approval-complete-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": approval_id,
                "attempt_id": approval_attempt,
                "lease_token": approval_token,
                "summary": "ready for human review",
            }),
        )
        .await
        .unwrap();
    assert_eq!(awaiting.structured["work"]["state"], "awaiting_approval");
    let approved = client
        .call_tool(
            "ptah_approve_work",
            json!({
                "request_id": "work-approval-decision-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": approval_id,
                "note": "Evidence reviewed by the operator",
                "expected_revision": awaiting.structured["work"]["revision"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(approved.structured["work"]["state"], "succeeded");
    assert_eq!(
        approved.structured["work"]["approval"]["reviewerId"],
        "primary"
    );

    let retry_policy = WorkPolicy {
        retry: WorkRetryPolicy {
            max_attempts: 2,
            retry_failed: false,
            ..WorkRetryPolicy::default()
        },
        ..WorkPolicy::default()
    };
    let retry_work = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "work-retry-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "retry-proof",
                "objective": "Reopen a failed item with a bounded retry",
                "policy": retry_policy,
            }),
        )
        .await
        .unwrap();
    let retry_id = retry_work.structured["work"]["workId"].as_str().unwrap();
    let retry_claim = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-retry-claim-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": retry_id,
            }),
        )
        .await
        .unwrap();
    let retry_attempt = retry_claim.structured["attempt"]["attemptId"]
        .as_str()
        .unwrap();
    let retry_token = retry_claim.structured["leaseToken"].as_str().unwrap();
    let failed = client
        .call_tool(
            "ptah_fail_work",
            json!({
                "request_id": "work-retry-fail-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": retry_id,
                "attempt_id": retry_attempt,
                "lease_token": retry_token,
                "summary": "fixture failure",
                "failure": "deterministic failure",
            }),
        )
        .await
        .unwrap();
    assert_eq!(failed.structured["work"]["state"], "failed");
    let retried = client
        .call_tool(
            "ptah_retry_work",
            json!({
                "request_id": "work-retry-reopen-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": retry_id,
                "reason": "operator approved a bounded retry",
                "expected_revision": failed.structured["work"]["revision"],
            }),
        )
        .await
        .unwrap();
    assert_eq!(retried.structured["work"]["state"], "queued");

    let archived_work = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "work-archive-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "archive-proof",
                "objective": "Remain durable while the Lane is archived"
            }),
        )
        .await
        .unwrap();
    let archived_id = archived_work.structured["work"]["workId"].as_str().unwrap();
    host.session_archive(lane.id, true).unwrap();
    let listed = client
        .call_tool(
            "ptah_list_work",
            json!({"session_id": lane.id, "workspace": workspace_text}),
        )
        .await
        .unwrap();
    assert!(listed.structured["work"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["workId"] == archived_id));
    assert!(client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "work-archive-claim-305",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": archived_id
            }),
        )
        .await
        .is_err());

    client.close_session().await.unwrap();
    server.stop();
    host.stop().unwrap();
    set_grokptah_home_override(None);
}
