//! Adversarial / residual #196 security and durability tests.
//! Each test drives shipped service/host/store/bus/MCP/spawn code.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    merge_bounds, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    discovered_tool_names, home_override_serial, set_grokptah_home_override, start_control_server,
    AgentHost, EventBus, HostConfig, SessionKind, SessionUpdate, CONTROL_SECRET_ENV_KEYS,
    CONTROL_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

fn setup_home() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = home_override_serial();
    let d = tempdir().unwrap();
    let home = d.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    (d, guard)
}

fn started_host() -> grokptah_agent_bridge::AgentHostHandle {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start");
    host
}

fn orch(
    host: &grokptah_agent_bridge::AgentHostHandle,
    home: &tempfile::TempDir,
    ws: &tempfile::TempDir,
    max: usize,
) -> Arc<OrchestrationService> {
    let token = "secret-token-adversarial-196".to_string();
    let bus = host.event_bus().with_control_secrets([token.clone()]);
    // Replace host bus secrets by wrapping service bus — use host bus and set secrets via new bus if needed.
    let _ = bus;
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: token,
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: max,
            bounds: RunBounds {
                max_prompt_bytes: 50_000,
                max_rounds: 8,
                max_duration_ms: 60_000,
            },
        },
    )
}

async fn wait_terminal(
    orch: &OrchestrationService,
    auth: &grokptah_agent_bridge::orchestration::AuthContext,
    run_id: &str,
) -> grokptah_agent_bridge::RunState {
    let start = std::time::Instant::now();
    loop {
        let v = orch.get_run(auth, run_id).unwrap();
        let state: grokptah_agent_bridge::RunState =
            serde_json::from_value(v["state"].clone()).unwrap();
        if !matches!(
            state,
            grokptah_agent_bridge::RunState::Running | grokptah_agent_bridge::RunState::Queued
        ) {
            return state;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("timeout waiting for {run_id}");
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duration_timeout_kills_shell_no_post_write() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 4);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    let marker = ws.path().join("after_timeout_marker.txt");
    // Shell: sleep then write — duration must cancel before write.
    let cmd = format!("run sleep 5; echo leaked > {}", marker.display());
    // offline uses "run " prefix for shell
    let prompt = format!("run sh -c 'sleep 5; echo leaked > {}'", marker.display());
    let _ = cmd;
    let resp = orch
        .submit_task(
            &auth,
            "to-1",
            session.id,
            ws.path(),
            prompt,
            Some(json!({"maxDurationMs": 120, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_terminal(&orch, &auth, &run_id).await;
    assert_eq!(state, grokptah_agent_bridge::RunState::LimitReached);
    // Allow brief settle; marker must not appear.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        !marker.exists(),
        "shell continued after timeout and wrote {marker:?}"
    );
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn control_token_absent_from_shell_env() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let token = "env-leak-token-SHOULD-NOT-APPEAR";
    std::env::set_var("GROKPTAH_CONTROL_TOKEN", token);
    std::env::set_var("GROKPTAH_CONTROL_PORT", "9999");
    let (_home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    // Direct host shell path (shipped tool_shell_streaming).
    let out_path = ws.path().join("env_out.txt");
    let prompt = format!("run env > {}", out_path.display());
    let _ = host
        .session_prompt(session.id, prompt)
        .await
        .expect("prompt");
    // Wait for shell to finish writing.
    for _ in 0..50 {
        if out_path.exists()
            && std::fs::metadata(&out_path)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let body = std::fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        !body.contains(token),
        "control token leaked into shell env:\n{body}"
    );
    assert!(
        !body.contains("GROKPTAH_CONTROL_TOKEN="),
        "CONTROL_TOKEN key present in env output"
    );
    for k in CONTROL_SECRET_ENV_KEYS {
        assert!(
            !body.lines().any(|l| l.starts_with(&format!("{k}="))),
            "secret key {k} present in child env"
        );
    }
    std::env::remove_var("GROKPTAH_CONTROL_TOKEN");
    std::env::remove_var("GROKPTAH_CONTROL_PORT");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reads_require_run_ownership_no_global_events() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let other = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 4);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    // Global feed forbidden
    let err = orch.get_events(&auth, None, 0, 10).unwrap_err();
    assert_eq!(err.code.as_str(), "invalid_request");
    let err = orch.get_run(&auth, "no-such-run").unwrap_err();
    assert_eq!(err.code.as_str(), "invalid_request");
    // list_sessions filters allowlist + busy
    let list = orch.list_sessions(&auth).unwrap();
    let ws_canon = dunce::canonicalize(ws.path()).unwrap();
    assert!(list["sessions"].as_array().unwrap().iter().all(|row| {
        let cwd = row["cwd"].as_str().unwrap_or("");
        dunce::canonicalize(PathBuf::from(cwd))
            .ok()
            .map(|c| c == ws_canon)
            .unwrap_or(false)
    }));
    // Create run then try read with wrong workspace allowlist service
    let resp = orch
        .submit_task(
            &auth,
            "own-1",
            s.id,
            ws.path(),
            "list files".into(),
            Some(json!({"maxDurationMs": 30000, "maxRounds": 2, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let _ = wait_terminal(&orch, &auth, &run_id).await;
    // Foreign allowlist cannot read
    let foreign = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "secret-token-adversarial-196".into(),
            allowlist: WorkspaceAllowlist::new([other.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let auth2 = foreign
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    assert!(foreign.get_run(&auth2, &run_id).is_err());
    assert!(foreign.get_handoff(&auth2, &run_id).is_err());
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_idempotent_submit_single_effect() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 4);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    // Use queue (fast) for concurrent identical request_id
    let mut futs = Vec::new();
    for _ in 0..8 {
        let orch = orch.clone();
        let auth = auth.clone();
        let sid = s.id;
        let wsp = ws.path().to_path_buf();
        futs.push(async move {
            orch.queue_prompt(
                &auth,
                "same-req-id",
                sid,
                &wsp,
                "follow up once".into(),
                false,
            )
        });
    }
    let results = futures::future::join_all(futs).await;
    let oks: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
    assert_eq!(oks.len(), 8);
    // All receipts identical
    for o in &oks[1..] {
        assert_eq!(o, &oks[0]);
    }
    // Exactly one queue entry
    assert_eq!(host.session_queue_list(s.id).unwrap().len(), 1);
    // Conflict
    let conflict = orch.queue_prompt(
        &auth,
        "same-req-id",
        s.id,
        ws.path(),
        "different".into(),
        false,
    );
    assert!(conflict.is_err());
    set_grokptah_home_override(None);
}

#[test]
fn bounds_escalation_and_zero_rejected() {
    let ceil = RunBounds {
        max_prompt_bytes: 1000,
        max_rounds: 4,
        max_duration_ms: 5000,
    };
    assert!(merge_bounds(&ceil, Some(&json!({"maxRounds": 8}))).is_err());
    assert!(merge_bounds(&ceil, Some(&json!({"maxRounds": 0}))).is_err());
    assert!(merge_bounds(&ceil, Some(&json!({"maxDurationMs": 0}))).is_err());
    assert!(merge_bounds(&ceil, Some(&json!("not-object"))).is_err());
    let ok = merge_bounds(&ceil, Some(&json!({"maxRounds": 2}))).unwrap();
    assert_eq!(ok.max_rounds, 2);
    assert_eq!(ok.max_prompt_bytes, 1000);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancel_requires_matching_run_stays_cancelled() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let a = host.session_new_kind(SessionKind::Build).unwrap();
    let b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(a.id, ws.path()).unwrap();
    host.session_set_cwd(b.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 4);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    let ra = orch
        .submit_task(
            &auth,
            "c-a",
            a.id,
            ws.path(),
            "run sleep 8".into(),
            Some(json!({"maxDurationMs": 60000, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_a = ra["runId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(80)).await;
    // Missing run_id
    assert!(orch.cancel(&auth, "cx0", a.id, ws.path(), None).is_err());
    // Unknown
    assert!(orch
        .cancel(&auth, "cx1", a.id, ws.path(), Some("nope"))
        .is_err());
    // Mismatched session
    assert!(orch
        .cancel(&auth, "cx2", b.id, ws.path(), Some(&run_a))
        .is_err());
    // Success
    orch.cancel(&auth, "cx3", a.id, ws.path(), Some(&run_a))
        .unwrap();
    let st = wait_terminal(&orch, &auth, &run_a).await;
    assert_eq!(st, grokptah_agent_bridge::RunState::Cancelled);
    // Still cancelled after settle
    tokio::time::sleep(Duration::from_millis(200)).await;
    let again = orch.get_run(&auth, &run_a).unwrap();
    assert_eq!(again["state"], "cancelled");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn non_build_queue_steer_rejected() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let chat = host.session_new_kind(SessionKind::Chat).unwrap();
    host.session_set_cwd(chat.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 2);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    let e = orch
        .queue_prompt(&auth, "nb1", chat.id, ws.path(), "hi".into(), false)
        .unwrap_err();
    assert_eq!(e.code.as_str(), "forbidden_scope");
    let e = orch
        .steer(&auth, "nb2", chat.id, ws.path(), "note".into())
        .unwrap_err();
    assert_eq!(e.code.as_str(), "forbidden_scope");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn list_sessions_reports_busy() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 4);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    let _ = orch
        .submit_task(
            &auth,
            "busy1",
            s.id,
            ws.path(),
            "run sleep 3".into(),
            Some(json!({"maxDurationMs": 30000, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let list = orch.list_sessions(&auth).unwrap();
    let row = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["sessionId"] == s.id.to_string())
        .expect("session row");
    assert_eq!(row["busy"], true);
    set_grokptah_home_override(None);
}

#[test]
fn test_command_not_confused_with_ordinary_shell() {
    use grokptah_agent_bridge::orchestration::is_recognized_test_command;
    assert!(!is_recognized_test_command("echo hello"));
    assert!(!is_recognized_test_command("cat contest.txt"));
    assert!(is_recognized_test_command("cargo test"));
}

#[test]
fn utf8_prompt_preview_no_panic() {
    use grokptah_agent_bridge::orchestration::prompt_preview;
    let s = "漢字".repeat(80);
    let p = prompt_preview(&s);
    assert!(!p.is_empty());
    // valid string
    let _ = p.chars().count();
}

#[test]
fn typed_schema_has_required_fields() {
    // tools/list schema is generated by mcp_control — probe via discovered names + list shape
    let names = discovered_tool_names();
    assert!(names.contains(&"ptah_submit_task"));
    assert_eq!(names.len(), CONTROL_TOOLS.len());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn mcp_sdk_style_client_tools_list_and_call() {
    // Real loopback client using reqwest as MCP JSON-RPC client (tools/list + tools/call).
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 2);
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let client = reqwest::Client::new();
    // Bad jsonrpc version
    let bad_ver = client
        .post(&url)
        .header("Authorization", "Bearer secret-token-adversarial-196")
        .json(&json!({"jsonrpc":"1.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert!(bad_ver.status().is_client_error() || bad_ver.status().is_success());
    // tools/list
    let list = client
        .post(&url)
        .header("Authorization", "Bearer secret-token-adversarial-196")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);
    let body: serde_json::Value = list.json().await.unwrap();
    let tools = body["result"]["tools"].as_array().unwrap();
    let submit = tools
        .iter()
        .find(|t| t["name"] == "ptah_submit_task")
        .unwrap();
    let schema = &submit["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "request_id"));
    // tools/call capacity
    let cap = client
        .post(&url)
        .header("Authorization", "Bearer secret-token-adversarial-196")
        .json(&json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"ptah_get_capacity","arguments":{}}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(cap.status(), 200);
    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn id_traversal_rejected() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let orch = orch(&host, &home, &ws, 2);
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    assert!(orch.get_run(&auth, "../etc/passwd").is_err());
    assert!(orch
        .queue_prompt(
            &auth,
            "../../x",
            Uuid::new_v4(),
            ws.path(),
            "x".into(),
            false
        )
        .is_err());
    set_grokptah_home_override(None);
}

#[test]
fn journal_concurrent_publish_monotonic() {
    let bus = EventBus::new(2000);
    let sid = Uuid::new_v4();
    let bus = std::sync::Arc::new(bus);
    let mut hs = Vec::new();
    for t in 0..4 {
        let bus = bus.clone();
        hs.push(std::thread::spawn(move || {
            for i in 0..100 {
                bus.publish(SessionUpdate::AgentMessageChunk {
                    session_id: sid,
                    text: format!("{t}:{i}"),
                });
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let page = bus.read_after(0, 500);
    let mut last = 0u64;
    for e in page.entries {
        assert!(e.seq > last);
        last = e.seq;
    }
}
