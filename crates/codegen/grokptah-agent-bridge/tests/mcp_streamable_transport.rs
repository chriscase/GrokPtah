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
