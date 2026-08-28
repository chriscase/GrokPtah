//! Authenticated Streamable HTTP live event channel tests (#259).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost, HostConfig,
    LiveNotification, McpControlClient, RunScope, SessionKind, SessionUpdate,
};
use serde_json::{json, Value};
use tempfile::tempdir;

type HomeGuard = std::sync::MutexGuard<'static, ()>;

fn setup_with_guard(
    guard: HomeGuard,
) -> (
    tempfile::TempDir,
    HomeGuard,
    grokptah_agent_bridge::AgentHostHandle,
    tempfile::TempDir,
    Arc<OrchestrationService>,
) {
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "live-event-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    (home, guard, host, workspace, orch)
}

fn live_url(
    addr: std::net::SocketAddr,
    session_id: uuid::Uuid,
    workspace: &std::path::Path,
    run_id: &str,
) -> String {
    let mut url = reqwest::Url::parse(&format!("http://{addr}/mcp")).unwrap();
    url.query_pairs_mut()
        .append_pair("session_id", &session_id.to_string())
        .append_pair("workspace", &workspace.display().to_string())
        .append_pair("run_id", run_id);
    url.to_string()
}

async fn wait_terminal(
    client: &mut McpControlClient,
    session_id: uuid::Uuid,
    workspace: &std::path::Path,
    run_id: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut last_state = None;
    while tokio::time::Instant::now() < deadline {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session_id,
                    "workspace": workspace.display().to_string(),
                    "run_id": run_id,
                }),
            )
            .await
            .unwrap();
        last_state = run.structured["state"].as_str().map(str::to_owned);
        if matches!(
            last_state.as_deref(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
        ) {
            return run.structured;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run did not reach a terminal state: {run_id}; last state: {last_state:?}");
}

async fn first_chunk(response: reqwest::Response) -> String {
    let mut chunks = response.bytes_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(3), chunks.next())
        .await
        .expect("live stream produced no frame")
        .expect("live stream closed before its first frame")
        .unwrap();
    String::from_utf8(chunk.to_vec()).unwrap()
}

fn event_id(frame: &str) -> u64 {
    frame
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .expect("event frame must carry an SSE id")
        .parse()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_get_replays_scoped_events_and_resumes_after_last_event() {
    let guard = home_override_serial();
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup_with_guard(guard);
    let owner = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(owner.id, workspace.path()).unwrap();
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, workspace.path()).unwrap();
    let server = start_control_server(orch, 0).await.unwrap();
    let base = format!("http://{}/mcp", server.addr);
    let http = reqwest::Client::new();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "live-event-token");
    client.initialize().await.unwrap();
    let transport_session = client.session_id().unwrap().to_string();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "live-event-submit",
                "session_id": owner.id,
                "workspace": workspace.path().display().to_string(),
                "prompt": "write live-events.txt: observed",
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();
    let terminal = wait_terminal(&mut client, owner.id, workspace.path(), &run_id).await;
    assert!(terminal["startSeq"].as_u64().is_some());

    // The existing unscoped GET remains a standards-compatible keep-alive.
    let keep_alive = http
        .get(&base)
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .send()
        .await
        .unwrap();
    assert_eq!(keep_alive.status(), 200);
    assert!(first_chunk(keep_alive).await.contains("sse open"));

    let stream_response = http
        .get(live_url(server.addr, owner.id, workspace.path(), &run_id))
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(stream_response.status(), 200);
    assert_eq!(
        stream_response.headers()["content-type"],
        "text/event-stream"
    );
    let first = first_chunk(stream_response).await;
    assert!(first.contains("notifications/ptah_event"));
    assert!(first.contains(&run_id));
    let first_id = event_id(&first);

    // Reconnect with the last delivered event. The durable replay must not
    // redeliver that event and must remain scoped to the same run.
    let resumed = http
        .get(live_url(server.addr, owner.id, workspace.path(), &run_id))
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .header("Last-Event-ID", first_id.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), 200);
    let next = first_chunk(resumed).await;
    assert!(next.contains("notifications/ptah_event"));
    assert!(event_id(&next) > first_id);
    assert!(next.contains(&run_id));

    // Reconnecting after the terminal sequence must close promptly instead
    // of leaving a completed run's stream open forever.
    let terminal_id = terminal["endSeq"].as_u64().expect("terminal end sequence");
    let completed = http
        .get(live_url(server.addr, owner.id, workspace.path(), &run_id))
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .header("Last-Event-ID", terminal_id.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(completed.status(), 200);
    let mut completed_chunks = completed.bytes_stream();
    let eof = tokio::time::timeout(Duration::from_secs(3), completed_chunks.next())
        .await
        .expect("completed stream did not close promptly");
    assert!(
        eof.is_none(),
        "completed stream emitted data after its terminal sequence"
    );

    // Exact ownership applies to the live channel too.
    let wrong_session = http
        .get(live_url(server.addr, other.id, workspace.path(), &run_id))
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .send()
        .await
        .unwrap();
    assert!(wrong_session.status().is_client_error());
    let body: Value = wrong_session.json().await.unwrap();
    assert_eq!(body["error"]["data"]["code"], "forbidden_scope");

    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn reusable_client_reconnects_from_last_live_event() {
    let guard = home_override_serial();
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup_with_guard(guard);
    let owner = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(owner.id, workspace.path()).unwrap();
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "live-event-token");
    client.initialize().await.unwrap();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "reusable-client-submit",
                "session_id": owner.id,
                "workspace": workspace.path().display().to_string(),
                "prompt": "write reusable-client.txt: observed"
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();
    let terminal = wait_terminal(&mut client, owner.id, workspace.path(), &run_id).await;
    let scope = RunScope {
        session_id: owner.id,
        workspace: workspace.path().display().to_string(),
        run_id,
    };

    let mut stream = client.open_event_stream(scope.clone(), None).await.unwrap();
    let first = stream.next_notification().await.unwrap().unwrap();
    let first_seq = first.sse_id.unwrap();
    assert!(matches!(first.notification, LiveNotification::Event(_)));
    assert_eq!(stream.last_event_id(), Some(first_seq));
    drop(stream);

    let mut resumed = client
        .open_event_stream(scope, Some(first_seq))
        .await
        .unwrap();
    let next = resumed.next_notification().await.unwrap().unwrap();
    assert!(next.sse_id.unwrap() > first_seq);
    let mut terminal_seen = matches!(
        next.notification,
        LiveNotification::Event(ref event)
            if matches!(event.update, SessionUpdate::TurnComplete { .. })
    );
    while let Some(frame) = resumed.next_notification().await.unwrap() {
        if matches!(
            frame.notification,
            LiveNotification::Event(ref event)
                if matches!(event.update, SessionUpdate::TurnComplete { .. })
        ) {
            terminal_seen = true;
        }
    }
    assert!(
        terminal_seen,
        "client must surface terminal event before close"
    );
    assert!(terminal["state"].as_str().is_some());

    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn post_rotation_event_is_not_delivered_to_an_existing_sse_stream() {
    let guard = home_override_serial();
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup_with_guard(guard);
    let owner = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(owner.id, workspace.path()).unwrap();
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "live-event-token");
    client.initialize().await.unwrap();
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "post-rotation-sse",
                "session_id": owner.id,
                "workspace": workspace.path().display().to_string(),
                "prompt": "bounded offline event"
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();
    let scope = RunScope {
        session_id: owner.id,
        workspace: workspace.path().display().to_string(),
        run_id,
    };
    let mut stream = client.open_event_stream(scope, None).await.unwrap();

    orch.rotate_authentication_generation("primary").unwrap();
    host.event_bus().publish(SessionUpdate::AgentMessageChunk {
        session_id: owner.id,
        text: "must not cross the rotation fence".into(),
    });
    let frame = tokio::time::timeout(Duration::from_secs(3), stream.next_notification())
        .await
        .unwrap()
        .unwrap();
    assert!(
        frame.is_none(),
        "stale SSE stream delivered a post-rotation frame"
    );

    server.stop_and_wait().await;
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_get_delivers_events_while_a_run_is_still_active() {
    let guard = home_override_serial();
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup_with_guard(guard);
    let owner = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(owner.id, workspace.path()).unwrap();
    let server = start_control_server(orch, 0).await.unwrap();
    let http = reqwest::Client::new();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "live-event-token");
    client.initialize().await.unwrap();
    let transport_session = client.session_id().unwrap().to_string();
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "live-active-submit",
                "session_id": owner.id,
                "workspace": workspace.path().display().to_string(),
                "prompt": "run sleep 1",
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();

    // Wait only for durable admission/start, not terminal completion.
    for _ in 0..40 {
        let progress = client
            .call_tool(
                "ptah_get_progress",
                json!({
                    "session_id": owner.id,
                    "workspace": workspace.path().display().to_string(),
                    "run_id": run_id,
                }),
            )
            .await
            .unwrap();
        if progress.structured["startSeq"].as_u64().is_some()
            && progress.structured["state"].as_str() == Some("running")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let response = http
        .get(live_url(server.addr, owner.id, workspace.path(), &run_id))
        .header("Authorization", "Bearer live-event-token")
        .header("mcp-session-id", &transport_session)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut chunks = response.bytes_stream();
    let notifications = tokio::time::timeout(Duration::from_secs(3), async {
        let mut notifications = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = String::from_utf8(chunk.unwrap().to_vec()).unwrap();
            // Keep-alive comments are valid SSE chunks and may arrive before
            // the first durable event, especially when admission is fast.
            if chunk.contains("notifications/ptah_event") {
                notifications.push(chunk);
                if notifications.len() == 2 {
                    break;
                }
            }
        }
        notifications
    })
    .await
    .expect("active stream did not deliver two events");
    assert_eq!(
        notifications.len(),
        2,
        "active stream closed before two events"
    );
    let first = &notifications[0];
    let second = &notifications[1];
    assert_ne!(first, second, "live stream must deliver distinct updates");

    let _ = wait_terminal(&mut client, owner.id, workspace.path(), &run_id).await;
    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}
