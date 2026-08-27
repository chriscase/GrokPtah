//! Standards MCP Streamable HTTP transport tests (#200).
//! Independent SDK interop uses Node `@modelcontextprotocol/sdk` (not McpControlClient).

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    canonical_workspace_string, set_grokptah_home_override, start_control_from_env,
    start_control_server, start_control_server_with, ActionClass, ActionGrant, AgentHost,
    ComputerRun, ComputerUseService, ControlServerLimits, GrantIssuer, HostConfig,
    McpControlClient, SessionKind, SimulatorBackend, CONTROL_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

fn setup() -> (
    tempfile::TempDir,
    ProcessEnvGuard,
    grokptah_agent_bridge::AgentHostHandle,
    tempfile::TempDir,
    std::sync::Arc<OrchestrationService>,
) {
    let mut guard = ProcessEnvGuard::new();
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
            reconciliation_operators: Vec::new(),
        },
    );
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn run_reads_require_exact_session_and_workspace_scope() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, ws, orch) = setup();
    let owner = host.session_new_kind(SessionKind::Build).unwrap();
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(owner.id, ws.path()).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();
    let outside = tempdir().unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "read-scope-submit",
                "session_id": owner.id,
                "workspace": ws.path().display().to_string(),
                "prompt": "list files"
            }),
        )
        .await
        .unwrap();
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();

    assert!(client
        .call_tool("ptah_get_run", json!({ "run_id": run_id }))
        .await
        .is_err());
    assert!(client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": other.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .is_err());
    assert!(client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": owner.id,
                "workspace": outside.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .is_err());
    let valid = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": owner.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    assert!(!valid.is_error);

    let scoped_tools = [
        "ptah_get_run",
        "ptah_get_progress",
        "ptah_get_events",
        "ptah_get_changes",
        "ptah_get_test_results",
        "ptah_get_handoff",
        "ptah_review_run",
    ];
    for name in scoped_tools {
        assert!(
            client
                .call_tool(name, json!({ "run_id": run_id }))
                .await
                .is_err(),
            "{name} accepted missing caller scope"
        );
        let args = if name == "ptah_get_events" {
            json!({
                "session_id": other.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50
            })
        } else {
            json!({
                "session_id": other.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            })
        };
        assert!(
            client.call_tool(name, args).await.is_err(),
            "{name} accepted a foreign session"
        );
    }
    for name in [
        "ptah_get_run",
        "ptah_get_progress",
        "ptah_get_events",
        "ptah_get_changes",
        "ptah_get_test_results",
        "ptah_get_handoff",
    ] {
        let args = if name == "ptah_get_events" {
            json!({
                "session_id": owner.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50
            })
        } else {
            json!({
                "session_id": owner.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            })
        };
        let result = client.call_tool(name, args).await.unwrap();
        assert!(
            !result.is_error,
            "owner read failed for {name}: {:?}",
            result.raw
        );
    }

    srv.stop_and_wait().await;
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
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
            .args(["ci", "--no-fund", "--no-audit"])
            .current_dir(&sdk_dir)
            .status()
            .await
            .expect("npm ci");
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn binds_loopback_only() {
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    assert!(
        srv.addr.ip().is_loopback(),
        "control server must bind loopback, got {}",
        srv.addr
    );
    assert_eq!(
        srv.addr.ip(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "control server must bind 127.0.0.1 (not ::1-only or public)"
    );
    // Health reports the same address; coordinator probes use it.
    let health = reqwest::Client::new()
        .get(format!("http://{}/health", srv.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let h: serde_json::Value = health.json().await.unwrap();
    assert_eq!(h["ok"], true);
    assert!(
        h["maxConcurrent"].as_u64().unwrap() >= 1,
        "health must advertise concurrency bound"
    );
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn concurrency_cap_returns_429() {
    // Hold max_concurrent permits via inject_work_delay; overflow must 429.
    let (_home, _lock, _host, _ws, orch) = setup();
    let limits = ControlServerLimits {
        max_concurrent: 2,
        request_timeout: Duration::from_secs(5),
        inject_work_delay: Some(Duration::from_millis(800)),
    };
    let srv = start_control_server_with(orch.clone(), 0, limits)
        .await
        .unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let http = reqwest::Client::new();
    let body = json!({
        "jsonrpc":"2.0","id":1,"method":"tools/list","params":{}
    });

    let hold_a = {
        let http = http.clone();
        let url = url.clone();
        let body = body.clone();
        tokio::spawn(async move {
            http.post(&url)
                .header("Authorization", "Bearer stream-token-200")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
        })
    };
    let hold_b = {
        let http = http.clone();
        let url = url.clone();
        let body = body.clone();
        tokio::spawn(async move {
            http.post(&url)
                .header("Authorization", "Bearer stream-token-200")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
        })
    };
    // Let both holders enter work (permit acquired + delay started).
    tokio::time::sleep(Duration::from_millis(120)).await;

    let overflow = http
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        overflow.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "body={}",
        overflow.text().await.unwrap_or_default()
    );

    let a = hold_a.await.unwrap().unwrap();
    let b = hold_b.await.unwrap().unwrap();
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn request_timeout_returns_error() {
    // inject_work_delay > request_timeout trips the real timeout branch.
    let (_home, _lock, _host, _ws, orch) = setup();
    let limits = ControlServerLimits {
        max_concurrent: 4,
        request_timeout: Duration::from_millis(80),
        inject_work_delay: Some(Duration::from_millis(500)),
    };
    let srv = start_control_server_with(orch.clone(), 0, limits)
        .await
        .unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", "Bearer stream-token-200")
        .header("Content-Type", "application/json")
        .json(&json!({
            "jsonrpc":"2.0","id":42,"method":"tools/list","params":{}
        }))
        .send()
        .await
        .unwrap();
    // Timeout maps to HTTP 504 + structured data.code=timeout.
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::GATEWAY_TIMEOUT,
        "expected 504 Gateway Timeout"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        msg.contains("timed out") || msg.contains("timeout"),
        "expected timeout error message, got {body}"
    );
    assert_eq!(body["error"]["data"]["code"], "timeout");
    assert_eq!(body["id"], 42);
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn unknown_and_forbidden_tools_fail_closed_over_http() {
    let (_home, _lock, _host, _ws, orch) = setup();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let http = reqwest::Client::new();
    for (name, expect_fragment) in [
        ("run_terminal_cmd", "not available"),
        ("ptah_shell", "not available"),
        ("ptah_manage_mcp", "not available"),
        ("no_such_tool_xyz", "not available"),
    ] {
        let resp = http
            .post(&url)
            .header("Authorization", "Bearer stream-token-200")
            .header("Content-Type", "application/json")
            .json(&json!({
                "jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name": name, "arguments":{}}
            }))
            .send()
            .await
            .unwrap();
        assert!(
            resp.status().is_client_error(),
            "{name} status {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        let msg = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains(expect_fragment),
            "{name}: expected '{expect_fragment}' in {body}"
        );
        assert_eq!(body["error"]["data"]["code"], "forbidden_scope");
    }
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn http_submit_allow_queue_and_cancel_queued_run() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, ws, orch) = setup();
    host.configure_orchestration_capacity(1);
    let first_session = host.session_new_kind(SessionKind::Build).unwrap();
    let queued_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(first_session.id, ws.path()).unwrap();
    host.session_set_cwd(queued_session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();

    let first = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "http-queue-first",
                "session_id": first_session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "run sleep 3"
            }),
        )
        .await
        .unwrap();
    assert!(!first.is_error);
    assert_eq!(first.structured["state"], "running");

    let queued = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "http-queue-second",
                "session_id": queued_session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "list files in the project root",
                "allow_queue": true
            }),
        )
        .await
        .unwrap();
    assert!(!queued.is_error);
    let queued_run_id = queued.structured["runId"].as_str().unwrap().to_string();
    assert_eq!(queued.structured["state"], "queued");
    assert_eq!(queued.structured["queuedPosition"], 1);
    let visible_queued = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": queued_session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": queued_run_id
            }),
        )
        .await
        .unwrap();
    assert!(!visible_queued.is_error);
    assert_eq!(visible_queued.structured["state"], "queued");
    assert_eq!(visible_queued.structured["queuePosition"], 1);
    let progress_queued = client
        .call_tool(
            "ptah_get_progress",
            json!({
                "session_id": queued_session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": queued_run_id
            }),
        )
        .await
        .unwrap();
    assert!(!progress_queued.is_error);
    assert_eq!(progress_queued.structured["state"], "queued");
    assert_eq!(progress_queued.structured["queuePosition"], 1);

    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["activeRuns"], 1);
    assert_eq!(capacity.structured["queuedRuns"], 1);
    assert_eq!(capacity.structured["queueLimit"], 32);

    let cancelled = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "http-queue-cancel",
                "session_id": queued_session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": queued_run_id
            }),
        )
        .await
        .unwrap();
    assert!(!cancelled.is_error);
    assert_eq!(cancelled.structured["wasQueued"], true);
    assert_eq!(cancelled.structured["teardownComplete"], true);
    assert_eq!(cancelled.structured["state"], "cancelled");
    let visible_cancelled = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": queued_session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": queued_run_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(visible_cancelled.structured["state"], "cancelled");
    assert!(visible_cancelled.structured["queuePosition"].is_null());

    srv.stop_and_wait().await;
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn http_submit_durable_run_events_handoff_and_cancel() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conf-submit-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "list files in the project root"
            }),
        )
        .await
        .unwrap();
    assert!(!submitted.is_error);
    let run_id = submitted
        .structured
        .get("runId")
        .or_else(|| submitted.structured.get("run_id"))
        .and_then(|v| v.as_str())
        .expect("submit must return runId")
        .to_string();

    // Poll durable run visibility until terminal (offline agent completes quickly).
    let mut state = String::new();
    for _ in 0..40 {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session.id,
                    "workspace": ws.path().display().to_string(),
                    "run_id": run_id
                }),
            )
            .await
            .unwrap();
        assert!(!run.is_error);
        assert_eq!(
            run.structured.get("clientId").and_then(|v| v.as_str()),
            Some("mcp"),
            "MCP-submitted runs must retain their origin for desktop visibility"
        );
        state = run
            .structured
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if matches!(
            state.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted" | "limit_reached"
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !state.is_empty(),
        "run must reach a durable visible state via MCP"
    );

    let events = client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "after_seq": 0,
                "limit": 50
            }),
        )
        .await
        .unwrap();
    assert!(!events.is_error);
    // Event page is ordered; if present, sequences are non-decreasing.
    if let Some(arr) = events
        .structured
        .get("events")
        .or_else(|| events.structured.get("entries"))
        .and_then(|v| v.as_array())
    {
        let mut prev = 0u64;
        for e in arr {
            if let Some(seq) = e.get("seq").and_then(|s| s.as_u64()) {
                assert!(seq >= prev, "event seq must be non-decreasing");
                prev = seq;
            }
        }
    }

    let handoff = client
        .call_tool(
            "ptah_get_handoff",
            json!({
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    assert!(
        !handoff.is_error,
        "handoff must be durable for terminal run"
    );
    assert!(
        handoff.structured["verification"].is_object(),
        "handoff must expose evidence-backed verification"
    );
    assert!(
        handoff.structured["verification"]["status"]
            .as_str()
            .is_some(),
        "verification must expose a bounded trust status"
    );
    assert_eq!(
        handoff.structured["stopCause"], "completed",
        "handoff must expose the host-decided typed stop cause"
    );
    assert!(
        handoff.structured["bounds"].is_object(),
        "handoff must expose the applied run bounds"
    );
    assert!(
        handoff.structured["bounds"]["maxRounds"].is_number(),
        "handoff bounds must use the durable run projection"
    );
    assert!(
        handoff.structured["usage"].is_object()
            && handoff.structured["usageComplete"].is_boolean()
            && handoff.structured["usagePendingRequests"].is_number(),
        "handoff must project usage and its completeness together"
    );

    // Cancel of already-terminal run fails closed (no silent success).
    let cancel_term = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conf-cancel-term",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await;
    assert!(
        cancel_term.is_err(),
        "cancel on terminal run must fail closed"
    );

    // Unknown run cancel fails closed.
    let cancel_unknown = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conf-cancel-unknown",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": "no-such-run-id"
            }),
        )
        .await;
    assert!(cancel_unknown.is_err());

    // Cross-session: foreign session cannot cancel this run.
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();
    let cross = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conf-cancel-cross",
                "session_id": other.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await;
    assert!(cross.is_err(), "cross-session cancel must fail closed");

    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn http_retry_interrupted_run_is_explicit_and_idempotent() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let source_id = "http-interrupted-source";
    orch.store()
        .save_run(&RunRecord {
            run_id: source_id.into(),
            session_id: session.id,
            workspace: dunce::canonicalize(ws.path())
                .unwrap()
                .display()
                .to_string(),
            request_id: "http-source-request".into(),
            client_id: Some("mcp".into()),
            owner_id: None,
            state: RunState::Interrupted,
            purpose: Default::default(),
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds {
                max_prompt_bytes: 10_000,
                max_rounds: 6,
                max_duration_ms: 30_000,
                max_total_tokens: None,
            },
            prompt_preview: "previous attempt".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: Some("interrupted".into()),
            final_response: None,
            error_code: Some("interrupted".into()),
            stop_cause: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();

    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();
    let listed = client.list_tools().await.unwrap();
    assert!(listed.iter().any(|tool| tool.name == "ptah_retry_run"));

    let retry_args = json!({
        "request_id": "http-retry-1",
        "session_id": session.id.to_string(),
        "workspace": ws.path().display().to_string(),
        "run_id": source_id,
        "prompt": "resume with a fresh bounded prompt"
    });
    let first = client
        .call_tool("ptah_retry_run", retry_args.clone())
        .await
        .unwrap();
    assert!(!first.is_error, "retry: {:?}", first.raw);
    assert_eq!(first.structured["sourceRunId"], source_id);
    assert_eq!(first.structured["retryOf"], source_id);
    let retry_id = first.structured["runId"].as_str().unwrap().to_string();

    for _ in 0..80 {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session.id,
                    "workspace": ws.path().display().to_string(),
                    "run_id": retry_id
                }),
            )
            .await
            .unwrap();
        if matches!(
            run.structured["state"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
        ) {
            assert_eq!(run.structured["retryOf"], source_id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let replay = client
        .call_tool("ptah_retry_run", retry_args)
        .await
        .unwrap();
    assert_eq!(replay.structured, first.structured);
    assert!(client
        .call_tool(
            "ptah_retry_run",
            json!({
                "request_id": "http-retry-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": source_id,
                "prompt": "different retry must conflict"
            }),
        )
        .await
        .is_err());

    srv.stop_and_wait().await;
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn mcp_isolated_run_review_approval_and_restart_promotion() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock, host, ws, orch) = setup();
    std::fs::write(ws.path().join("README.md"), "baseline\n").unwrap();
    for args in [
        vec!["init"],
        vec!["add", "README.md"],
        vec![
            "-c",
            "user.name=GrokPtah Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "baseline",
        ],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(ws.path())
            .status()
            .unwrap();
        assert!(status.success());
    }
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();

    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "isolated-submit-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "write mcp-approved.txt: hello from isolated run",
                "execution_mode": "isolated_worktree"
            }),
        )
        .await
        .unwrap();
    assert!(!submitted.is_error, "isolated submit: {:?}", submitted.raw);
    assert_eq!(submitted.structured["executionMode"], "isolated_worktree");
    let run_id = submitted.structured["runId"].as_str().unwrap().to_string();

    let mut state = String::new();
    for _ in 0..80 {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session.id,
                    "workspace": ws.path().display().to_string(),
                    "run_id": run_id
                }),
            )
            .await
            .unwrap();
        state = run.structured["state"].as_str().unwrap_or_default().into();
        if state == "completed" || state == "failed" {
            assert_eq!(run.structured["clientId"], "mcp");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(state, "completed");

    let review = client
        .call_tool(
            "ptah_review_run",
            json!({
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    assert!(!review.is_error, "review: {:?}", review.raw);
    assert!(review.structured["diff"]
        .as_str()
        .unwrap()
        .contains("mcp-approved"));
    let approval = client
        .call_tool(
            "ptah_approve_run",
            json!({
                "request_id": "isolated-approve-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "source_fingerprint": review.structured["sourceFingerprint"],
                "final_fingerprint": review.structured["finalFingerprint"],
                "changed_files": review.structured["changedFiles"]
            }),
        )
        .await
        .unwrap();
    assert!(!approval.is_error, "approval: {:?}", approval.raw);
    let approval_id = approval.structured["approvalId"]
        .as_str()
        .unwrap()
        .to_string();
    let run_before_tamper = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    let isolated_workspace = run_before_tamper.structured["execution"]["executionWorkspace"]
        .as_str()
        .unwrap();
    let isolated_file = PathBuf::from(isolated_workspace).join("mcp-approved.txt");
    std::fs::write(&isolated_file, "tampered after approval\n").unwrap();
    let stale = client
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": "isolated-promote-stale",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "approval_id": approval_id
            }),
        )
        .await;
    assert!(stale.is_err(), "tampered worktree must fail closed");
    std::fs::write(&isolated_file, "hello from isolated run").unwrap();
    orch.store()
        .update_run(&run_id, |run| {
            run.approval.as_mut().unwrap().expires_at = Utc::now() - ChronoDuration::seconds(1);
            Ok(())
        })
        .unwrap();
    let expired = client
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": "isolated-promote-expired",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "approval_id": approval_id
            }),
        )
        .await;
    assert!(expired.is_err(), "expired approval must fail closed");
    orch.store()
        .update_run(&run_id, |run| {
            run.approval.as_mut().unwrap().expires_at = Utc::now() + ChronoDuration::minutes(5);
            Ok(())
        })
        .unwrap();
    client.close_session().await.unwrap();
    srv.stop_and_wait().await;
    drop(orch);
    drop(host);

    // Reopen the same durable home. The approval must remain usable without
    // the desktop-only in-memory reviewed_runs marker.
    let host2 = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host2.start().unwrap();
    let orch2 = OrchestrationService::new(
        host2.clone(),
        host2.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "stream-token-200".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
            reconciliation_operators: Vec::new(),
        },
    );
    let srv2 = start_control_server(orch2.clone(), 0).await.unwrap();
    let mut client2 = McpControlClient::new(format!("http://{}", srv2.addr), "stream-token-200");
    client2.initialize().await.unwrap();
    let other = host2.session_new_kind(SessionKind::Build).unwrap();
    host2.session_set_cwd(other.id, ws.path()).unwrap();
    let cross_session = client2
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": "isolated-promote-cross-session",
                "session_id": other.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "approval_id": approval_id
            }),
        )
        .await;
    assert!(
        cross_session.is_err(),
        "approval must be bound to its session"
    );
    let promoted = client2
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": "isolated-promote-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "approval_id": approval_id
            }),
        )
        .await
        .unwrap();
    assert!(
        !promoted.is_error,
        "promote after restart: {:?}",
        promoted.raw
    );
    assert_eq!(
        promoted.structured["execution"]["promotionState"],
        "promoted"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join("mcp-approved.txt")).unwrap(),
        "hello from isolated run"
    );
    assert_eq!(
        orch2.store().list_runs().unwrap().len(),
        1,
        "one durable MCP run, no desktop duplicate"
    );
    let replay = client2
        .call_tool(
            "ptah_promote_run",
            json!({
                "request_id": "isolated-promote-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id,
                "approval_id": approval_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay.structured["runId"], promoted.structured["runId"]);
    client2.close_session().await.unwrap();
    srv2.stop_and_wait().await;
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn http_cancel_busy_run_reaches_cancelled() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();

    // Long shell so cancel races a live run (same pattern as orch adversarial).
    let marker = ws.path().join("busy_cancel_marker.txt");
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conf-busy-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": format!(
                    "run (sleep 5; echo leaked > {}) & wait",
                    marker.display()
                ),
                "bounds": {"maxDurationMs": 60000, "maxRounds": 8, "maxPromptBytes": 50000}
            }),
        )
        .await
        .unwrap();
    let run_id = submitted
        .structured
        .get("runId")
        .or_else(|| submitted.structured.get("run_id"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    tokio::time::sleep(Duration::from_millis(80)).await;

    let cancelled = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conf-busy-cancel",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    assert!(!cancelled.is_error);
    // Replay same request_id is idempotent.
    let replay = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conf-busy-cancel",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "run_id": run_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        cancelled
            .structured
            .get("runId")
            .or(cancelled.structured.get("run_id")),
        replay
            .structured
            .get("runId")
            .or(replay.structured.get("run_id"))
    );

    let mut final_state = String::new();
    for _ in 0..40 {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session.id,
                    "workspace": ws.path().display().to_string(),
                    "run_id": run_id
                }),
            )
            .await
            .unwrap();
        final_state = run
            .structured
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if final_state == "cancelled" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        final_state, "cancelled",
        "busy cancel must leave durable cancelled state"
    );

    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn independent_node_coordinator_conformance() {
    // multi_thread so Node interop does not starve the server runtime.
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let sdk_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop");
    if !sdk_dir
        .join("node_modules/@modelcontextprotocol/sdk")
        .is_dir()
    {
        let st = tokio::process::Command::new("npm")
            .args(["ci", "--no-fund", "--no-audit"])
            .current_dir(&sdk_dir)
            .status()
            .await
            .expect("npm ci");
        assert!(st.success());
    }
    let output = tokio::process::Command::new("node")
        .arg(sdk_dir.join("run_conformance.mjs"))
        .env("GROKPTAH_MCP_URL", &url)
        .env("GROKPTAH_MCP_TOKEN", "stream-token-200")
        .env("GROKPTAH_MCP_SESSION_ID", session.id.to_string())
        .env("GROKPTAH_MCP_WORKSPACE", ws.path().display().to_string())
        .output()
        .await
        .expect("spawn conformance");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "coordinator conformance failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["ok"], true, "report={report}");
    assert!(
        report["checks"]
            .as_object()
            .map(|o| o.values().all(|v| v == &json!(true)))
            .unwrap_or(false),
        "not all checks true: {report}"
    );
    // SDK path reported honestly (may be false); must not be invented as true without success.
    assert!(report.get("sdkOk").is_some());
    // Durable transcript for verifiers (`--nocapture`).
    eprintln!("MCP_CONFORMANCE_NODE_JSON {report}");
    if !stderr.trim().is_empty() {
        eprintln!("MCP_CONFORMANCE_NODE_STDERR {stderr}");
    }
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn http_steer_idle_queues_without_starting_run() {
    // HTTP path: ptah_steer on idle Build session defers to queue (non-cancelling).
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let before = host.session_queue_list(session.id).unwrap().len();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();
    let steer = client
        .call_tool(
            "ptah_steer",
            json!({
                "request_id": "steer-http-idle-1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "text": "prefer unit tests"
            }),
        )
        .await
        .unwrap();
    assert!(!steer.is_error);
    let disposition = steer
        .structured
        .get("disposition")
        .or_else(|| steer.raw.pointer("/structuredContent/disposition"))
        .or_else(|| steer.raw.pointer("/result/structuredContent/disposition"))
        .cloned()
        .unwrap_or(json!(null));
    assert_eq!(
        disposition, "queued",
        "idle steer must queue, not cancel/start a turn; body={:?}",
        steer.raw
    );
    let after = host.session_queue_list(session.id).unwrap().len();
    assert_eq!(after, before + 1, "steer must enqueue exactly one entry");
    // No active run started by steer alone.
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
async fn http_queue_controls_share_versions_replay_and_scope() {
    let (_home, _lock, host, ws, orch) = setup();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "stream-token-200");
    client.initialize().await.unwrap();
    let scope = |session_id| {
        json!({
            "session_id": session_id,
            "workspace": ws.path().display().to_string(),
        })
    };

    let first = client
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "queue-http-first",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "prompt": "first queued prompt"
            }),
        )
        .await
        .unwrap();
    assert_eq!(first.structured["actionId"], "queue-http-first");
    assert_eq!(first.structured["origin"], "mcp");
    assert_eq!(first.structured["disposition"], "queued");
    let first_entry = first.structured["entry"].clone();
    let first_id = first_entry["id"].as_str().unwrap().to_string();
    assert_eq!(first_entry["version"], 0);

    let second = client
        .call_tool(
            "ptah_queue_prompt",
            json!({
                "request_id": "queue-http-second",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "prompt": "second queued prompt"
            }),
        )
        .await
        .unwrap();
    let second_id = second.structured["entry"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let snapshot = client
        .call_tool("ptah_get_queue", scope(session.id))
        .await
        .unwrap();
    assert_eq!(snapshot.structured["entries"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot.structured["revision"], 2);

    let edit_args = json!({
        "request_id": "queue-http-edit",
        "session_id": session.id,
        "workspace": ws.path().display().to_string(),
        "entry_id": first_id,
        "version": 0,
        "text": "edited queued prompt"
    });
    let edited = client
        .call_tool("ptah_edit_queue", edit_args.clone())
        .await
        .unwrap();
    assert_eq!(edited.structured["entry"]["version"], 1);
    assert_eq!(edited.structured["action"], "edited");
    let replay = client
        .call_tool("ptah_edit_queue", edit_args)
        .await
        .unwrap();
    assert_eq!(replay.structured, edited.structured);
    assert!(client
        .call_tool(
            "ptah_edit_queue",
            json!({
                "request_id": "queue-http-stale",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "entry_id": first_id,
                "version": 0,
                "text": "stale edit must fail"
            }),
        )
        .await
        .is_err());

    let before_reorder = client
        .call_tool("ptah_get_queue", scope(session.id))
        .await
        .unwrap();
    let reorder_revision = before_reorder.structured["revision"]
        .as_u64()
        .expect("queue revision");
    assert_eq!(reorder_revision, 3);

    // S3: every mutator below is compare-and-set, and omitting the version is
    // a schema rejection rather than a last-write-wins mutation. Assert that
    // before any of them is allowed to succeed, and that nothing moved.
    for tool in [
        "ptah_remove_queue",
        "ptah_run_next",
        "ptah_steer_queued",
        "ptah_reorder_queue",
    ] {
        let mut args = json!({
            "request_id": format!("queue-http-noversion-{tool}"),
            "session_id": session.id,
            "workspace": ws.path().display().to_string(),
            "entry_id": second_id,
        });
        if tool == "ptah_reorder_queue" {
            args["to_index"] = json!(0);
            args["expected_revision"] = json!(0);
        }
        assert!(
            client.call_tool(tool, args).await.is_err(),
            "{tool} must require expected_version"
        );
    }
    let untouched = client
        .call_tool("ptah_get_queue", scope(session.id))
        .await
        .unwrap();
    assert_eq!(untouched.structured["entries"].as_array().unwrap().len(), 2);
    assert_eq!(untouched.structured["entries"][0]["id"], first_id);

    let reordered = client
        .call_tool(
            "ptah_reorder_queue",
            json!({
                "request_id": "queue-http-reorder",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "entry_id": second_id,
                "to_index": 0,
                "expected_version": 0,
                "expected_revision": reorder_revision
            }),
        )
        .await
        .unwrap();
    assert_eq!(reordered.structured["entries"][0]["id"], second_id);
    assert_eq!(reordered.structured["revision"], reorder_revision + 1);
    // S3: reorder now bumps every version it shifted, so a second coordinator
    // holding the pre-move ordering loses its CAS instead of both succeeding.
    assert_eq!(reordered.structured["entries"][0]["version"], 1);
    assert_eq!(reordered.structured["entries"][1]["version"], 2);
    assert!(
        client
            .call_tool(
                "ptah_reorder_queue",
                json!({
                    "request_id": "queue-http-reorder-stale",
                    "session_id": session.id,
                    "workspace": ws.path().display().to_string(),
                    "entry_id": first_id,
                    "to_index": 0,
                    "expected_version": 1,
                    "expected_revision": reorder_revision + 1
                }),
            )
            .await
            .is_err(),
        "a reorder built on the pre-move ordering must fail closed"
    );
    let after_conflict = client
        .call_tool("ptah_get_queue", scope(session.id))
        .await
        .unwrap();
    assert_eq!(after_conflict.structured["entries"][0]["id"], second_id);

    let steered = client
        .call_tool(
            "ptah_steer_queued",
            json!({
                "request_id": "queue-http-steer-queued",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "entry_id": second_id,
                "expected_version": 1
            }),
        )
        .await
        .unwrap();
    assert_eq!(steered.structured["disposition"], "queued");
    assert_eq!(steered.structured["entry"]["version"], 2);

    let run_next = client
        .call_tool(
            "ptah_run_next",
            json!({
                "request_id": "queue-http-run-next",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "entry_id": first_id,
                "expected_version": 2
            }),
        )
        .await
        .unwrap();
    assert_eq!(run_next.structured["action"], "run_next");
    assert_eq!(run_next.structured["entry"]["id"], first_id);
    assert_eq!(run_next.structured["entry"]["version"], 3);

    let removed = client
        .call_tool(
            "ptah_remove_queue",
            json!({
                "request_id": "queue-http-remove",
                "session_id": session.id,
                "workspace": ws.path().display().to_string(),
                "entry_id": first_id,
                "expected_version": 3
            }),
        )
        .await
        .unwrap();
    assert_eq!(removed.structured["action"], "removed");
    assert_eq!(removed.structured["entry"]["id"], first_id);
    assert_eq!(removed.structured["actionVersion"], 3);

    let cleared = client
        .call_tool(
            "ptah_clear_queue",
            json!({
                "request_id": "queue-http-clear",
                "session_id": session.id,
                "workspace": ws.path().display().to_string()
            }),
        )
        .await
        .unwrap();
    assert!(cleared.structured["entries"].as_array().unwrap().is_empty());
    // S4: an empty `entries` list is not by itself a promise that the session
    // stopped. The receipt must say what clear actually cancelled and whether
    // anything survived it, so a coordinator can branch on `stopped`.
    assert_eq!(cleared.structured["steeringCancelled"], 0);
    assert_eq!(cleared.structured["steeringInFlight"], 0);
    assert_eq!(cleared.structured["stopped"], true);
    assert!(cleared.structured["clearedQueued"].is_u64());
    assert!(client
        .call_tool(
            "ptah_get_queue",
            json!({
                "session_id": session.id,
                "workspace": tempdir().unwrap().path().display().to_string()
            }),
        )
        .await
        .is_err());

    client.close_session().await.unwrap();
    srv.stop();
    set_grokptah_home_override(None);
}

/// Live coordinator smoke against the **desktop env bootstrap** path
/// (`start_control_from_env` — same contract as Tauri `start_embedded_control`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_desktop_bootstrap_node_smoke() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    // Disposable token — never a real credential.
    let token = format!("live-smoke-{}", uuid::Uuid::new_v4());
    env.set("GROKPTAH_CONTROL_TOKEN", &token);
    env.set("GROKPTAH_CONTROL_PORT", "0");
    env.set("GROKPTAH_CONTROL_WORKSPACES", ws.path().as_os_str());

    let srv = start_control_from_env(host.clone())
        .await
        .expect("desktop env bootstrap must start control server");
    assert!(
        srv.addr.ip().is_loopback(),
        "desktop bootstrap must bind loopback, got {}",
        srv.addr
    );
    assert_eq!(srv.token, token);

    // Without token the shared bootstrap fails closed (structural desktop path).
    env.remove("GROKPTAH_CONTROL_TOKEN");
    assert!(
        start_control_from_env(host.clone()).await.is_none(),
        "missing GROKPTAH_CONTROL_TOKEN must not start control"
    );
    env.set("GROKPTAH_CONTROL_TOKEN", &token);

    let url = format!("http://{}/mcp", srv.addr);
    let sdk_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop");
    if !sdk_dir
        .join("node_modules/@modelcontextprotocol/sdk")
        .is_dir()
    {
        let st = tokio::process::Command::new("npm")
            .args(["ci", "--no-fund", "--no-audit"])
            .current_dir(&sdk_dir)
            .status()
            .await
            .expect("npm ci");
        assert!(st.success());
    }
    let output = tokio::process::Command::new("node")
        .arg(sdk_dir.join("run_live_smoke.mjs"))
        .env("GROKPTAH_MCP_URL", &url)
        .env("GROKPTAH_MCP_TOKEN", &token)
        .env("GROKPTAH_MCP_SESSION_ID", session.id.to_string())
        .env("GROKPTAH_MCP_WORKSPACE", ws.path().display().to_string())
        .output()
        .await
        .expect("spawn live smoke");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "live desktop bootstrap smoke failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["ok"], true, "live smoke report={report}");
    if let Some(failed) = report["failed"].as_array() {
        assert!(failed.is_empty(), "failed checks: {failed:?}");
    }
    // Durable transcript for verifiers (`--nocapture`).
    eprintln!("LIVE_DESKTOP_MCP_SMOKE_REPORT {report}");
    if !stderr.trim().is_empty() {
        eprintln!("LIVE_DESKTOP_MCP_SMOKE_STDERR {stderr}");
    }

    srv.stop();
    set_grokptah_home_override(None);
    drop(env);
}

/// Desktop wiring must keep delegating to the shared bootstrap (no fork of policy).
#[test]
fn desktop_control_bootstrap_uses_shared_start_control_from_env() {
    let lib =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/src-tauri/src/lib.rs");
    let src = std::fs::read_to_string(&lib).expect("read desktop lib.rs");
    assert!(
        src.contains("start_control_from_env"),
        "desktop must call shared start_control_from_env"
    );
    assert!(
        src.contains("GROKPTAH_CONTROL_TOKEN") || src.contains("start_embedded_control"),
        "desktop control entry must remain present"
    );
    // Must not re-implement OrchStore::open + start_control_server inline.
    assert!(
        !src.contains("OrchStore::open(grokptah_home()"),
        "desktop must not re-open orch store outside shared bootstrap"
    );
}

/// The Computer Run ledger holds an exclusive lock, so the desktop must reach
/// it through the host's shared handle — a direct open would either split the
/// surfaces or fail on the lock at runtime.
#[test]
fn desktop_computer_use_shares_the_host_store() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../desktop/src-tauri/src/computer_use.rs");
    let src = std::fs::read_to_string(&path).expect("read desktop computer_use.rs");
    assert!(
        src.contains("ensure_computer_store"),
        "desktop must borrow the host's computer store"
    );
    assert!(
        !src.contains("ComputerStore::open(grokptah_home()"),
        "desktop must not open the shared computer ledger outside the host"
    );
}

fn smoke_grant(run: &ComputerRun) -> ActionGrant {
    let now = Utc::now();
    ActionGrant {
        grant_id: format!("smoke-grant-{}", run.run_id),
        run_id: run.run_id.clone(),
        target: run.target.clone(),
        action_classes: std::collections::BTreeSet::from([ActionClass::Semantic]),
        issued_by: GrantIssuer::LocalUser,
        issued_at: now,
        expires_at: now + ChronoDuration::minutes(10),
        uses_remaining: None,
        revoked_at: None,
    }
}

/// Live desktop-equivalent proof for the read-only Computer Run tools: the
/// production `start_control_from_env` bootstrap (the same entry the Tauri
/// desktop uses), seeded with real simulator runs through the host's shared
/// store, exercised end to end by an independent Node client over real
/// loopback HTTP — discovery, scoped reads, redaction pins, cursor paging,
/// duplicate replay, stale-cursor recovery, cross-session/workspace
/// rejection, capacity, auth-before-body, and reconnect replay.
#[tokio::test]
async fn live_computer_reads_node_smoke() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    let ws_other = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let other_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other_session.id, ws_other.path())
        .unwrap();

    let canon = canonical_workspace_string(ws.path()).unwrap();
    let canon_other = canonical_workspace_string(ws_other.path()).unwrap();
    let computer = ComputerUseService::new(
        std::sync::Arc::new(SimulatorBackend::new()),
        host.ensure_computer_store().unwrap(),
    );

    // Run A: bound, authorized, observed — projection metadata plus journal.
    let run_a = computer
        .create_run(
            "smoke-create-a",
            session.id,
            Some(canon.clone()),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .unwrap();
    let run_a = computer
        .authorize(
            "smoke-grant-a",
            &run_a.run_id,
            run_a.version,
            smoke_grant(&run_a),
        )
        .unwrap();
    computer
        .observe("smoke-observe-a", &run_a.run_id, run_a.version)
        .await
        .unwrap();

    // Run B: another session's run in another workspace — the rejection target.
    let run_b = computer
        .create_run(
            "smoke-create-b",
            other_session.id,
            Some(canon_other),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .unwrap();

    // Run C: journal driven past the bounded ring so an early cursor is
    // genuinely evicted on the wire, using only public service operations.
    let run_c = computer
        .create_run(
            "smoke-create-c",
            session.id,
            Some(canon.clone()),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .unwrap();
    let run_c = computer
        .authorize(
            "smoke-grant-c",
            &run_c.run_id,
            run_c.version,
            smoke_grant(&run_c),
        )
        .unwrap();
    let mut version = run_c.version;
    let mut spins = 0u32;
    loop {
        computer
            .observe(&format!("smoke-observe-c-{spins}"), &run_c.run_id, version)
            .await
            .unwrap();
        let current = computer.get_run(&run_c.run_id).unwrap().unwrap();
        version = current.version;
        if current
            .audit
            .first()
            .map(|entry| entry.sequence)
            .unwrap_or(1)
            > 1
        {
            break;
        }
        spins += 1;
        assert!(spins < 1_500, "audit ring eviction did not occur");
    }

    // Disposable token — never a real credential.
    let token = format!("computer-smoke-{}", uuid::Uuid::new_v4());
    env.set("GROKPTAH_CONTROL_TOKEN", &token);
    env.set("GROKPTAH_CONTROL_PORT", "0");
    env.set(
        "GROKPTAH_CONTROL_WORKSPACES",
        std::env::join_paths([ws.path(), ws_other.path()]).unwrap(),
    );

    let srv = start_control_from_env(host.clone())
        .await
        .expect("desktop env bootstrap must start control server");
    assert!(srv.addr.ip().is_loopback());

    let url = format!("http://{}/mcp", srv.addr);
    let sdk_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop");
    let output = tokio::process::Command::new("node")
        .arg(sdk_dir.join("run_computer_reads_smoke.mjs"))
        .env("GROKPTAH_MCP_URL", &url)
        .env("GROKPTAH_MCP_TOKEN", &token)
        .env("GROKPTAH_MCP_SESSION_ID", session.id.to_string())
        .env("GROKPTAH_MCP_WORKSPACE", ws.path().display().to_string())
        .env(
            "GROKPTAH_MCP_OTHER_WORKSPACE",
            ws_other.path().display().to_string(),
        )
        .env("GROKPTAH_MCP_COMPUTER_RUN_A", &run_a.run_id)
        .env("GROKPTAH_MCP_COMPUTER_RUN_B", &run_b.run_id)
        .env("GROKPTAH_MCP_COMPUTER_RUN_C", &run_c.run_id)
        .output()
        .await
        .expect("spawn computer reads smoke");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "computer reads smoke failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["ok"], true, "computer reads smoke report={report}");
    if let Some(failed) = report["failed"].as_array() {
        assert!(failed.is_empty(), "failed checks: {failed:?}");
    }
    // Durable transcript for verifiers (`--nocapture`).
    eprintln!("LIVE_COMPUTER_READS_SMOKE_REPORT {report}");

    srv.stop_and_wait().await;
    set_grokptah_home_override(None);
    drop(env);
}
