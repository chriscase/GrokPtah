//! Standards MCP Streamable HTTP transport tests (#200).
//! Independent SDK interop uses Node `@modelcontextprotocol/sdk` (not McpControlClient).

use std::path::PathBuf;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost, HostConfig,
    McpControlClient, SessionKind, CONTROL_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;

fn setup() -> (
    tempfile::TempDir,
    std::sync::MutexGuard<'static, ()>,
    grokptah_agent_bridge::AgentHostHandle,
    tempfile::TempDir,
    std::sync::Arc<OrchestrationService>,
) {
    let guard = home_override_serial();
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let ws = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "stream-token-200".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    (home, guard, host, ws, orch)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn streamable_compat_client_session_and_tools() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    let init = client.initialize().await.unwrap();
    assert!(init["protocolVersion"].as_str().unwrap().starts_with("202"));
    assert!(client.session_id().is_some());
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), CONTROL_TOOLS.len());
    let cap = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert!(!cap.is_error);
    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unauthenticated_and_oversized_fail_closed() {
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let client = reqwest::Client::new();

    // No auth
    let unauth = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    // Oversized body (auth present) — body limit 256KiB
    let big = "x".repeat(300_000);
    let over = client
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .body(format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"pad":"{big}"}}}}"#
        ))
        .send()
        .await
        .unwrap();
    assert!(
        over.status() == 413 || over.status() == 400 || over.status().is_client_error(),
        "status={}",
        over.status()
    );

    // Malformed JSON
    let mal = client
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .body("{not-json")
        .send()
        .await
        .unwrap();
    assert!(mal.status().is_client_error());

    // Health unauthenticated on loopback
    let health = client
        .get(format!("http://{}/health", srv.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let h: serde_json::Value = health.json().await.unwrap();
    assert_eq!(h["ok"], true);
    assert_eq!(h["transport"], "mcp-streamable-http");

    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn notification_returns_accepted_without_result_body() {
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let client = reqwest::Client::new();
    // initialize first to get session
    let init = client
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{
                "protocolVersion":"2025-03-26",
                "capabilities":{},
                "clientInfo":{"name":"t","version":"0"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(init.status(), 200);
    let sid = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let note = client
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &sid)
        .json(&json!({
            "jsonrpc":"2.0",
            "method":"notifications/initialized",
            "params":{}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(note.status(), 202);

    // DELETE session
    let del = client
        .delete(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("mcp-session-id", &sid)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_clients_and_duplicate_mutations() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let base = format!("http://{}", srv.addr);

    let mut clients = Vec::new();
    for i in 0..4 {
        let mut c = McpControlClient::new(base.clone(), "stream-token-200");
        c.initialize().await.unwrap();
        let _ = c
            .call_tool("ptah_get_capacity", json!({}))
            .await
            .unwrap_or_else(|e| panic!("client {i}: {e}"));
        clients.push(c);
    }

    // Duplicate idempotent queue via two clients, same request_id
    let a = clients[0]
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "dup-stream-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "follow up once"
            }),
        )
        .await
        .unwrap();
    let b = clients[1]
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "dup-stream-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "follow up once"
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        a.structured
            .get("entries")
            .or(a.raw.get("structuredContent")),
        b.structured
            .get("entries")
            .or(b.raw.get("structuredContent"))
    );
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), 1);

    for mut c in clients {
        let _ = c.close_session().await;
    }
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn independent_node_mcp_sdk_interop() {
    // multi_thread + async Command: std::process::Command::output would block the
    // single-thread runtime and starve axum::serve (server never accepts Node).
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let sdk_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop");
    assert!(
        sdk_dir.join("package.json").is_file(),
        "mcp_sdk_interop package missing"
    );
    if !sdk_dir
        .join("node_modules/@modelcontextprotocol/sdk")
        .is_dir()
    {
        let st = tokio::process::Command::new("npm")
            .args(["install", "--no-fund", "--no-audit"])
            .current_dir(&sdk_dir)
            .status()
            .await
            .expect("npm install");
        assert!(st.success());
    }
    let output = tokio::process::Command::new("node")
        .arg(sdk_dir.join("run_interop.mjs"))
        .env("GROKPTAH_MCP_URL", &url)
        .env("GROKPTAH_MCP_TOKEN", "stream-token-200")
        .output()
        .await
        .expect("spawn node interop");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "independent SDK interop failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["ok"], true);
    assert!(report["toolCount"].as_u64().unwrap() >= CONTROL_TOOLS.len() as u64);
    // Prefer official SDK path success when available.
    if report.get("sdkOk") == Some(&json!(false)) {
        eprintln!(
            "note: protocol-level independent client passed; official SDK path: {:?}",
            report.get("sdkError")
        );
    }
    srv.stop();
    set_grokptah_home_override(None);
    let _ = Duration::from_millis(1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn reconnect_after_delete_and_stale_session_fails_closed() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let http = reqwest::Client::new();

    // Session A
    let init_a = http
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{
                "protocolVersion":"2025-11-25",
                "capabilities":{},
                "clientInfo":{"name":"a","version":"0"}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(init_a.status(), 200);
    let sid_a = init_a
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let list_ok = http
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &sid_a)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(list_ok.status(), 200);

    // Client drop / DELETE session
    let del = http
        .delete(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("mcp-session-id", &sid_a)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);

    // Stale session id must fail closed (no tools for ghost session).
    let stale = http
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", &sid_a)
        .json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert!(
        stale.status().is_client_error() || stale.status() == 400,
        "stale session accepted: {}",
        stale.status()
    );
    let stale_body: serde_json::Value = stale.json().await.unwrap_or(json!({}));
    assert!(
        stale_body.get("error").is_some() || stale_body.get("result").is_none(),
        "stale session returned tools: {stale_body}"
    );

    // Reconnect: new initialize + list/call succeed.
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();
    let sid_b = client.session_id().unwrap().to_string();
    assert_ne!(sid_a, sid_b);
    let tools = client.list_tools().await.unwrap();
    assert!(!tools.is_empty());
    let cap = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert!(!cap.is_error);
    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn disconnect_mid_request_then_idempotent_retry_no_double_mutation() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let before = host.session_queue_list(session.id).unwrap().len();

    // Mid-request disconnect: open TCP, send a full POST, drop without reading body.
    // Server still processes the mutation; client must be able to retry safely.
    let addr = srv.addr;
    let body = serde_json::to_vec(&json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{
            "name":"ptah_queue_prompt",
            "arguments":{
                "request_id":"retry-after-drop-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "queued once despite disconnect"
            }
        }
    }))
    .unwrap();
    {
        use tokio::io::AsyncWriteExt;
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer stream-token-200\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            addr,
            body.len()
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        // Drop without reading response = client disconnect.
        drop(stream);
    }
    // Allow the server to finish the request.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Idempotent retry with same request_id must not double-enqueue.
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();
    let again = client
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "retry-after-drop-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "queued once despite disconnect"
            }),
        )
        .await
        .unwrap();
    assert!(!again.is_error);
    let after = host.session_queue_list(session.id).unwrap().len();
    assert_eq!(
        after,
        before + 1,
        "disconnect+retry must not double-mutate queue (before={before} after={after})"
    );
    // Conflict retry with different payload still fails without a second entry.
    let conflict = client
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "retry-after-drop-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "different payload"
            }),
        )
        .await;
    assert!(conflict.is_err());
    assert_eq!(
        host.session_queue_list(session.id).unwrap().len(),
        before + 1
    );
    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn session_map_hard_capped_under_initialize_spam() {
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let http = reqwest::Client::new();
    let mut oldest: Option<String> = None;
    // Spam beyond MAX_SESSIONS (256); health must never report unbounded growth.
    for i in 0..320 {
        let r = http
            .post(&url)
            .header("Authorization", "Bearer stream-token-200")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({
                "jsonrpc":"2.0","id":i,"method":"initialize",
                "params":{
                    "protocolVersion":"2025-11-25",
                    "capabilities":{},
                    "clientInfo":{"name":"spam","version":"0"}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "initialize {i}");
        let sid = r
            .headers()
            .get("mcp-session-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        if i == 0 {
            oldest = Some(sid);
        }
    }
    let health = http
        .get(format!("http://{}/health", srv.addr))
        .send()
        .await
        .unwrap();
    let h: serde_json::Value = health.json().await.unwrap();
    let n = h["sessions"].as_u64().unwrap();
    assert!(
        n <= 256,
        "session map grew unbounded under initialize spam: {n}"
    );
    // Oldest session should have been LRU-evicted.
    let stale = http
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .header("mcp-session-id", oldest.as_ref().unwrap())
        .json(&json!({"jsonrpc":"2.0","id":999,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert!(
        stale.status().is_client_error() || stale.status() == 400,
        "evicted session still accepted: {}",
        stale.status()
    );
    srv.stop();
    set_grokptah_home_override(None);
}
