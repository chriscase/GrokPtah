//! Authenticated Streamable HTTP live event channel tests (#259).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost, HostConfig,
    McpControlClient, SessionKind,
};
use serde_json::{json, Value};
use tempfile::tempdir;

fn setup() -> (
    tempfile::TempDir,
    std::sync::MutexGuard<'static, ()>,
    grokptah_agent_bridge::AgentHostHandle,
    tempfile::TempDir,
    Arc<OrchestrationService>,
) {
    let guard = home_override_serial();
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
    for _ in 0..80 {
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
        if matches!(
            run.structured["state"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
        ) {
            return run.structured;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("run did not reach a terminal state: {run_id}");
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
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup();
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_get_delivers_events_while_a_run_is_still_active() {
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, workspace, orch) = setup();
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
    let first = tokio::time::timeout(Duration::from_secs(3), chunks.next())
        .await
        .expect("active stream produced no first event")
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(3), chunks.next())
        .await
        .expect("active stream did not deliver a subsequent event")
        .unwrap()
        .unwrap();
    let first = String::from_utf8(first.to_vec()).unwrap();
    let second = String::from_utf8(second.to_vec()).unwrap();
    assert!(first.contains("notifications/ptah_event"));
    assert!(second.contains("notifications/ptah_event"));
    assert_ne!(first, second, "live stream must deliver distinct updates");

    let _ = wait_terminal(&mut client, owner.id, workspace.path(), &run_id).await;
    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}
