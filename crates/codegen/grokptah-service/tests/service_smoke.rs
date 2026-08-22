use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ContinuationCheckpoint, ContinuationReason, RunAggregates, RunBounds, RunRecord, RunState,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHostHandle, LiveNotification,
    McpControlClient, RunScope, SessionUpdate,
};
use grokptah_service::{start_service, ServiceConfig};
use serde_json::json;
use tempfile::tempdir;
use tokio::time::timeout;
use uuid::Uuid;

/// Isolated smoke tests must not capture a live provider route. Conformance
/// (`ServiceEnv`) already sets `GROKPTAH_AGENT_OFFLINE=1`; this older harness
/// did not, so `ptah_submit_task` failed closed on missing credentials instead
/// of proving capacity, scope, and restart.
struct IsolatedOffline {
    previous: Option<std::ffi::OsString>,
}

impl IsolatedOffline {
    fn enable() -> Self {
        let previous = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
        std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
        Self { previous }
    }
}

impl Drop for IsolatedOffline {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
            None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
        }
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn standalone_service_exposes_authenticated_mcp_and_readiness() {
    let _serial = home_override_serial();
    let _offline = IsolatedOffline::enable();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().to_path_buf()));

    let config = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        "standalone-service-token",
        vec![workspace.path().to_path_buf()],
        false,
        2,
        Duration::from_secs(5),
    )
    .unwrap()
    .with_runtime_home(home.path())
    .unwrap();
    let handle = start_service(config).await.unwrap();
    assert_eq!(
        handle.host().runtime_home().path(),
        dunce::canonicalize(home.path()).unwrap()
    );
    let base = format!("http://{}", handle.addr);

    let unauthenticated = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let readiness = reqwest::get(format!("{base}/ready")).await.unwrap();
    assert_eq!(readiness.status(), reqwest::StatusCode::OK);
    let readiness_json: serde_json::Value = readiness.json().await.unwrap();
    assert_eq!(readiness_json["ready"], true);
    assert_eq!(
        readiness_json["capacity"]["health"]["workloadSupervisor"]["enabled"],
        true
    );
    assert!(
        readiness_json["capacity"]["health"]["workloadSupervisor"]["lastSuccessAt"].is_string()
    );
    assert_eq!(
        readiness_json["capacity"]["health"]["routineSupervisor"]["enabled"],
        true
    );

    let mut client = McpControlClient::new(base, "standalone-service-token");
    client.initialize().await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.name == "ptah_list_persistent_agents"));
    assert!(tools.iter().any(|tool| tool.name == "ptah_create_session"));
    assert!(tools.iter().any(|tool| tool.name == "ptah_submit_task"));
    assert!(tools.iter().any(|tool| tool.name == "ptah_list_runs"));
    assert!(tools.iter().any(|tool| tool.name == "ptah_create_routine"));
    assert!(tools.iter().any(|tool| tool.name == "ptah_fire_routine"));
    assert!(tools
        .iter()
        .any(|tool| tool.name == "ptah_get_native_coding_readiness"));

    let created = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": dunce::canonicalize(workspace.path()).unwrap(),
                "title": "Remote smoke session",
            }),
        )
        .await
        .unwrap();
    assert!(!created.is_error, "create session: {:?}", created.raw);
    assert_eq!(created.structured["title"], "Remote smoke session");
    assert_eq!(
        created.structured["workspace"],
        dunce::canonicalize(workspace.path())
            .unwrap()
            .display()
            .to_string()
    );
    let created_session_id = created.structured["sessionId"].as_str().unwrap();
    let sessions = client
        .call_tool("ptah_list_sessions", json!({}))
        .await
        .unwrap();
    assert!(!sessions.is_error, "list sessions: {:?}", sessions.raw);
    assert!(sessions.structured["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["sessionId"] == created_session_id));
    client.close_session().await.unwrap();

    handle.stop_and_wait().await;

    // Reopen the same service home to prove the standalone entry point can
    // restart against the durable orchestration ledger without splitting it.
    let config_after_restart = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        "standalone-service-token",
        vec![workspace.path().to_path_buf()],
        false,
        2,
        Duration::from_secs(5),
    )
    .unwrap()
    .with_runtime_home(home.path())
    .unwrap();
    let restarted = start_service(config_after_restart).await.unwrap();
    let mut restarted_client = McpControlClient::new(
        format!("http://{}", restarted.addr),
        "standalone-service-token",
    );
    restarted_client.initialize().await.unwrap();
    assert!(!restarted_client.list_tools().await.unwrap().is_empty());
    restarted_client.close_session().await.unwrap();
    restarted.stop_and_wait().await;

    set_grokptah_home_override(None);
    assert!(PathBuf::from(home.path()).exists());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn service_mcp_contract_covers_scoped_live_reconnect_controls_and_restart() {
    let _serial = home_override_serial();
    let _offline = IsolatedOffline::enable();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace_path = dunce::canonicalize(workspace.path()).unwrap();
    set_grokptah_home_override(Some(home.path().to_path_buf()));

    let token = "service-e2e-token-with-enough-entropy";
    let config = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        token,
        vec![workspace_path.clone()],
        false,
        2,
        Duration::from_secs(5),
    )
    .unwrap();
    let handle = start_service(config).await.unwrap();
    let host = handle.host();
    let (session_id, run_id, agent_id, first_seq) = seed_active_run(&host, &workspace_path);
    let scope = RunScope {
        session_id,
        workspace: workspace_path.display().to_string(),
        run_id: run_id.clone(),
    };

    host.event_bus().publish(SessionUpdate::AgentMessageChunk {
        session_id,
        text: "first remote event".into(),
    });

    let mut client = McpControlClient::new(format!("http://{}", handle.addr), token);
    client.initialize().await.unwrap();

    let agents = client
        .call_tool("ptah_list_persistent_agents", json!({}))
        .await
        .unwrap();
    assert!(!agents.is_error, "list agents: {:?}", agents.raw);
    assert!(agents.structured["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|agent| agent["agentId"] == agent_id));

    let run = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
                "run_id": run_id,
            }),
        )
        .await
        .unwrap();
    assert!(!run.is_error, "get run: {:?}", run.raw);
    assert_eq!(run.structured["startSeq"], first_seq);

    let events = client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50,
            }),
        )
        .await
        .unwrap();
    assert!(!events.is_error, "get events: {:?}", events.raw);
    assert_eq!(events.structured["entries"].as_array().unwrap().len(), 1);
    assert_eq!(events.structured["entries"][0]["seq"], first_seq);

    let listed_runs = client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
            }),
        )
        .await
        .unwrap();
    assert!(!listed_runs.is_error, "list runs: {:?}", listed_runs.raw);
    assert!(listed_runs.structured["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|run| run["runId"] == run_id));

    let created = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": workspace_path,
                "title": "Submission smoke session",
            }),
        )
        .await
        .unwrap();
    assert!(
        !created.is_error,
        "create submit session: {:?}",
        created.raw
    );
    let submit_session_id =
        Uuid::parse_str(created.structured["sessionId"].as_str().unwrap()).unwrap();
    // Isolated offline turns complete immediately. Fill both admission slots so
    // the smoke submit stays queued; cancel then proves submit+cancel without a
    // live provider, matching the capacity-matrix harness.
    let hold_a = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": workspace_path,
                "title": "Smoke capacity hold A",
            }),
        )
        .await
        .unwrap();
    let hold_b = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": workspace_path,
                "title": "Smoke capacity hold B",
            }),
        )
        .await
        .unwrap();
    let hold_a_id = Uuid::parse_str(hold_a.structured["sessionId"].as_str().unwrap()).unwrap();
    let hold_b_id = Uuid::parse_str(hold_b.structured["sessionId"].as_str().unwrap()).unwrap();
    host.reserve_orchestration_turn("smoke-hold-a", hold_a_id)
        .unwrap();
    host.reserve_orchestration_turn("smoke-hold-b", hold_b_id)
        .unwrap();
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "service-e2e-submit",
                "session_id": submit_session_id,
                "workspace": workspace_path,
                "prompt": "return a short acknowledgement and stop",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert!(!submitted.is_error, "submit task: {:?}", submitted.raw);
    assert_eq!(submitted.structured["state"], "queued");
    let submitted_run_id = submitted.structured["runId"].as_str().unwrap();
    assert_eq!(
        submitted.structured["sessionId"],
        submit_session_id.to_string()
    );
    let submitted_cancel = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "service-e2e-submit-cancel",
                "session_id": submit_session_id,
                "workspace": workspace_path,
                "run_id": submitted_run_id,
            }),
        )
        .await
        .unwrap();
    assert!(
        !submitted_cancel.is_error,
        "cancel submitted task: {:?}",
        submitted_cancel.raw
    );
    host.release_orchestration_turn("smoke-hold-a");
    host.release_orchestration_turn("smoke-hold-b");

    let mut stream = client
        .open_event_stream(scope.clone(), Some(first_seq.saturating_sub(1)))
        .await
        .unwrap();
    let frame = timeout(Duration::from_secs(2), stream.next_notification())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match frame.notification {
        LiveNotification::Event(event) => {
            assert_eq!(event.seq, first_seq);
            assert_eq!(event.run_id, run_id);
        }
        other => panic!("expected replayed event, got {other:?}"),
    }
    drop(stream);

    client.close_session().await.unwrap();
    host.event_bus().publish(SessionUpdate::AgentMessageChunk {
        session_id,
        text: "reconnected remote event".into(),
    });
    client.initialize().await.unwrap();
    let mut reconnected = client
        .open_event_stream(scope.clone(), Some(first_seq))
        .await
        .unwrap();
    let frame = timeout(Duration::from_secs(2), reconnected.next_notification())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second_seq = match frame.notification {
        LiveNotification::Event(event) => {
            assert!(event.seq > first_seq);
            event.seq
        }
        other => panic!("expected event after reconnect, got {other:?}"),
    };
    assert_eq!(reconnected.last_event_id(), Some(second_seq));
    drop(reconnected);

    let wrong_scope = client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": Uuid::new_v4(),
                "workspace": workspace_path,
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50,
            }),
        )
        .await;
    assert!(wrong_scope.is_err(), "scope leak: {wrong_scope:?}");

    let steer = client
        .call_tool(
            "ptah_steer",
            json!({
                "request_id": "service-e2e-steer",
                "session_id": session_id,
                "workspace": workspace_path,
                "text": "continue the remote run",
            }),
        )
        .await
        .unwrap();
    assert!(!steer.is_error, "steer: {:?}", steer.raw);
    assert_eq!(steer.structured["action"], "steer_now");

    let cancelled = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "service-e2e-cancel",
                "session_id": session_id,
                "workspace": workspace_path,
                "run_id": run_id,
            }),
        )
        .await
        .unwrap();
    assert!(!cancelled.is_error, "cancel: {:?}", cancelled.raw);
    assert_eq!(cancelled.structured["cancelled"], true);
    client.close_session().await.unwrap();
    drop(host);
    handle.stop_and_wait().await;

    let restarted = start_service(
        ServiceConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            token,
            vec![workspace_path.clone()],
            false,
            2,
            Duration::from_secs(5),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let mut restarted_client = McpControlClient::new(format!("http://{}", restarted.addr), token);
    restarted_client.initialize().await.unwrap();
    let recovered_run = restarted_client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
                "run_id": run_id,
            }),
        )
        .await
        .unwrap();
    assert!(
        !recovered_run.is_error,
        "restart get run: {:?}",
        recovered_run.raw
    );
    assert_eq!(recovered_run.structured["state"], "cancelled");
    let recovered_events = restarted_client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50,
            }),
        )
        .await
        .unwrap();
    assert!(
        !recovered_events.is_error,
        "restart events: {:?}",
        recovered_events.raw
    );
    assert!(recovered_events.structured["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["seq"] == first_seq));
    let recovered_list = restarted_client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": session_id,
                "workspace": workspace_path,
            }),
        )
        .await
        .unwrap();
    assert!(
        !recovered_list.is_error,
        "restart list runs: {:?}",
        recovered_list.raw
    );
    assert!(recovered_list.structured["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|run| run["runId"] == run_id && run["state"] == "cancelled"));
    restarted_client.close_session().await.unwrap();
    restarted.stop_and_wait().await;

    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn service_capacity_and_scope_matrix_is_fail_closed() {
    let _serial = home_override_serial();
    let _offline = IsolatedOffline::enable();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let workspace_path = dunce::canonicalize(workspace.path()).unwrap();
    let outside_workspace = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().to_path_buf()));

    let token = "capacity-matrix-token-with-enough-entropy";
    let handle = start_service(
        ServiceConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            token,
            vec![workspace_path.clone()],
            false,
            1,
            Duration::from_secs(5),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let host = handle.host();
    let mut client = McpControlClient::new(format!("http://{}", handle.addr), token);
    client.initialize().await.unwrap();

    let first = client
        .call_tool(
            "ptah_create_session",
            json!({"workspace": workspace_path, "title": "Capacity holder"}),
        )
        .await
        .unwrap();
    let first_session = Uuid::parse_str(first.structured["sessionId"].as_str().unwrap()).unwrap();
    let second = client
        .call_tool(
            "ptah_create_session",
            json!({"workspace": workspace_path, "title": "Capacity queue"}),
        )
        .await
        .unwrap();
    let second_session = Uuid::parse_str(second.structured["sessionId"].as_str().unwrap()).unwrap();

    host.reserve_orchestration_turn("capacity-matrix-hold", first_session)
        .unwrap();
    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["maxConcurrentRuns"], 1);
    assert_eq!(capacity.structured["activeRuns"], 1);
    assert_eq!(capacity.structured["available"], 0);

    let fail_fast = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "capacity-matrix-fail-fast",
                "session_id": second_session,
                "workspace": workspace_path,
                "prompt": "must be rejected while capacity is held",
                "execution_mode": "shared",
                "allow_queue": false,
            }),
        )
        .await;
    assert!(
        fail_fast
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("capacity")),
        "expected capacity rejection, got {fail_fast:?}"
    );

    let queued = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "capacity-matrix-queued",
                "session_id": second_session,
                "workspace": workspace_path,
                "prompt": "wait for the held capacity",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(queued.structured["state"], "queued");
    assert_eq!(queued.structured["queuedPosition"], 1);
    let queued_run_id = queued.structured["runId"].as_str().unwrap();
    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["queuedRuns"], 1);

    let cancelled = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "capacity-matrix-cancel",
                "session_id": second_session,
                "workspace": workspace_path,
                "run_id": queued_run_id,
            }),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.structured["cancelled"], true);

    let unauthorized_create = client
        .call_tool(
            "ptah_create_session",
            json!({"workspace": outside_workspace.path(), "title": "must not exist"}),
        )
        .await;
    assert!(unauthorized_create.is_err());

    host.release_orchestration_turn("capacity-matrix-hold");
    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["activeRuns"], 0);
    assert_eq!(capacity.structured["queuedRuns"], 0);

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
    set_grokptah_home_override(None);
}

fn seed_active_run(
    host: &AgentHostHandle,
    workspace: &std::path::Path,
) -> (Uuid, String, String, u64) {
    let session = host.session_new().unwrap();
    host.session_set_cwd(session.id, workspace).unwrap();
    let session_id = session.id;
    let run_id = "service-e2e-run".to_string();
    let mut agent = host.ensure_session_agent(session_id).unwrap();
    let agent_id = agent.agent_id.clone();
    let now = Utc::now();
    let start_seq = host.event_bus().next_seq();
    let store = host.ensure_orchestration_store().unwrap();
    let mut checkpoint = ContinuationCheckpoint {
        checkpoint_id: "service-e2e-checkpoint".into(),
        agent_id: agent_id.clone(),
        session_id,
        run_id: run_id.clone(),
        agent_spec_revision: None,
        parent_checkpoint_id: None,
        ordinal: 1,
        workspace: workspace.display().to_string(),
        context_summary: "remote e2e checkpoint".into(),
        context_hash: String::new(),
        event_seq: start_seq,
        reason: ContinuationReason::TurnCompleted,
        created_at: now,
    };
    checkpoint.context_hash = checkpoint.context_hash_for();
    store.save_checkpoint(&checkpoint).unwrap();
    agent.state = grokptah_agent_bridge::orchestration::AgentState::Active;
    agent.current_run_id = Some(run_id.clone());
    agent.latest_checkpoint_id = Some(checkpoint.checkpoint_id.clone());
    agent.updated_at = now;
    store.save_agent(&agent).unwrap();
    store
        .save_run(&RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: workspace.display().to_string(),
            request_id: "service-e2e-request".into(),
            client_id: Some("mcp".into()),
            state: RunState::Running,
            purpose: Default::default(),
            provider_route: None,
            agent_id: Some(agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "remote e2e run".into(),
            start_seq: Some(start_seq),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();
    (session_id, run_id, agent_id, start_seq)
}
