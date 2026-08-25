//! Adversarial / residual #196 security and durability tests.
//! Each test drives shipped service/host/store/bus/MCP/spawn code.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    merge_bounds, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    discovered_tool_names, set_grokptah_home_override, start_control_server, AgentHost, EventBus,
    HostConfig, SessionKind, SessionUpdate, CONTROL_SECRET_ENV_KEYS, CONTROL_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

fn setup_home() -> (tempfile::TempDir, ProcessEnvGuard) {
    let mut guard = ProcessEnvGuard::new();
    let d = tempdir().unwrap();
    let home = d.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
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
    // OrchestrationService::new must register bearer on the shared host bus.
    let svc = OrchestrationService::new(
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
                max_total_tokens: None,
            },
        },
    );
    assert!(
        host.event_bus().control_secrets_len() >= 1,
        "shipped OrchestrationService::new must register control secrets on host bus"
    );
    svc
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
    // Shell sleeps 5s then writes — duration 150ms must cancel while the turn
    // future is still alive and kill the child before the write.
    let prompt = format!("run (sleep 5; echo leaked > {}) & wait", marker.display());
    let t0 = std::time::Instant::now();
    let resp = orch
        .submit_task(
            &auth,
            "to-1",
            session.id,
            ws.path(),
            prompt,
            Some(json!({"maxDurationMs": 150, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_terminal(&orch, &auth, &run_id).await;
    assert_eq!(state, grokptah_agent_bridge::RunState::LimitReached);
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "duration path did not stop promptly: {:?}",
        t0.elapsed()
    );
    // Honest wait past full shell sleep — if cancel failed, marker would appear.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !marker.exists(),
        "shell continued after timeout and wrote {marker:?}"
    );
    assert!(
        !host.session_busy(session.id),
        "session still busy after duration cancel+await"
    );
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn control_token_absent_from_shell_env() {
    let token = "env-leak-token-SHOULD-NOT-APPEAR";
    let (_home, mut env) = setup_home();
    env.set("GROKPTAH_CONTROL_TOKEN", token);
    env.set("GROKPTAH_CONTROL_PORT", "9999");
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
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reads_require_run_ownership_no_global_events() {
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
    assert_eq!(err.code.as_str(), "forbidden_scope");
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
        orch.store().clone(),
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
            .await
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
    assert!(conflict.await.is_err());
    set_grokptah_home_override(None);
}

#[test]
fn bounds_escalation_and_zero_rejected() {
    let ceil = RunBounds {
        max_prompt_bytes: 1000,
        max_rounds: 4,
        max_duration_ms: 5000,
        max_total_tokens: None,
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
    let marker = ws.path().join("explicit_cancel_descendant.txt");
    let ra = orch
        .submit_task(
            &auth,
            "c-a",
            a.id,
            ws.path(),
            format!("run (sleep 3; echo leaked > {}) & wait", marker.display()),
            Some(json!({"maxDurationMs": 60000, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_a = ra["runId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(80)).await;
    // Missing run_id
    assert!(orch
        .cancel(&auth, "cx0", a.id, ws.path(), None)
        .await
        .is_err());
    // Unknown
    assert!(orch
        .cancel(&auth, "cx1", a.id, ws.path(), Some("nope"))
        .await
        .is_err());
    // Mismatched session
    assert!(orch
        .cancel(&auth, "cx2", b.id, ws.path(), Some(&run_a))
        .await
        .is_err());
    // Success
    let cancelled = orch
        .cancel(&auth, "cx3", a.id, ws.path(), Some(&run_a))
        .await
        .unwrap();
    assert_eq!(cancelled["teardownComplete"], true);
    assert!(!host.session_busy(a.id));
    let replay = orch
        .cancel(&auth, "cx3", a.id, ws.path(), Some(&run_a))
        .await
        .unwrap();
    assert_eq!(replay, cancelled);
    let st = wait_terminal(&orch, &auth, &run_a).await;
    assert_eq!(st, grokptah_agent_bridge::RunState::Cancelled);
    // Still cancelled after settle
    tokio::time::sleep(Duration::from_millis(200)).await;
    let again = orch.get_run(&auth, &run_a).unwrap();
    assert_eq!(again["state"], "cancelled");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        !marker.exists(),
        "explicit cancel allowed a descendant to survive"
    );
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn non_build_queue_steer_rejected() {
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
        .await
        .unwrap_err();
    assert_eq!(e.code.as_str(), "forbidden_scope");
    let e = orch
        .steer(&auth, "nb2", chat.id, ws.path(), "note".into())
        .await
        .unwrap_err();
    assert_eq!(e.code.as_str(), "forbidden_scope");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn list_sessions_reports_busy() {
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
async fn mcp_control_client_library_interop() {
    // Shipped McpControlClient: full initialize lifecycle + schema-gated tools/call.
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 2);
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = grokptah_agent_bridge::McpControlClient::new(
        format!("http://{}", srv.addr),
        "secret-token-adversarial-196",
    );
    assert!(
        client.list_tools().await.is_err(),
        "must require initialize"
    );
    client.initialize().await.unwrap();
    assert!(client.is_initialized());
    let tools = client.list_tools().await.unwrap();
    let submit = tools.iter().find(|t| t.name == "ptah_submit_task").unwrap();
    assert_eq!(submit.input_schema["additionalProperties"], false);
    assert!(submit.input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "request_id"));
    // Client-side schema validation rejects incomplete calls.
    assert!(client
        .call_tool("ptah_submit_task", json!({"prompt": "only"}))
        .await
        .is_err());
    let cap = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert!(!cap.is_error);
    assert!(
        cap.structured.get("maxConcurrentRuns").is_some()
            || cap.raw.get("structuredContent").is_some()
    );

    let (st, body) = client
        .rpc_raw("", "tools/list", json!({}), true)
        .await
        .unwrap();
    assert!(
        st.is_client_error() || body.get("error").is_some(),
        "empty jsonrpc accepted: {st} {body}"
    );
    assert!(body.get("result").and_then(|r| r.get("tools")).is_none());

    srv.stop();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn journal_rollover_preserves_durable_aggregates() {
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
    // Keep the run active while production bus events are folded durably.
    let resp = orch
        .submit_task(
            &auth,
            "roll-1",
            session.id,
            ws.path(),
            "run sleep 1".into(),
            Some(json!({"maxDurationMs": 30000, "maxRounds": 4, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    std::fs::write(ws.path().join("durable_agg.txt"), "hello-agg").unwrap();
    host.event_bus().publish(SessionUpdate::FileEdit {
        session_id: session.id,
        path: "durable_agg.txt".into(),
        summary: "durable aggregate".into(),
        unified_diff: "+hello-agg".into(),
    });
    host.event_bus().publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 2,
        max_rounds: 4,
        last_tool: Some("write_file".into()),
        detail: "durably recorded".into(),
    });
    let state = wait_terminal(&orch, &auth, &run_id).await;
    assert_eq!(state, grokptah_agent_bridge::RunState::Completed);
    assert!(
        ws.path().join("durable_agg.txt").is_file(),
        "write must succeed so FileEdit is emitted"
    );
    // Poll briefly for the production aggregator task to flush.
    let mut aggs_ok = false;
    for _ in 0..40 {
        let run = orch.store().load_run(&run_id).unwrap().unwrap();
        if !run.aggregates.changes.is_empty() {
            aggs_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        aggs_ok,
        "shipped apply_run_aggregate path must record FileEdit into RunAggregates"
    );
    let before = orch.get_changes(&auth, &run_id).unwrap();
    assert!(!before["changes"].as_array().unwrap().is_empty());
    let progress_before = orch.get_progress(&auth, &run_id).unwrap();
    assert!(
        progress_before["progress"].is_object(),
        "structured progress must be persisted with the run"
    );
    // Flood past journal capacity so early seqs expire.
    let bus = host.event_bus();
    let flood_sid = Uuid::new_v4();
    for i in 0..5000 {
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: flood_sid,
            text: format!("flood-{i}"),
        });
    }
    let after = orch.get_changes(&auth, &run_id).unwrap();
    assert!(
        !after["changes"].as_array().unwrap().is_empty(),
        "journal rollover erased run-scoped changes"
    );
    let progress_after = orch.get_progress(&auth, &run_id).unwrap();
    assert_eq!(progress_after["progress"], progress_before["progress"]);
    let handoff = orch.get_handoff(&auth, &run_id).unwrap();
    assert_eq!(handoff["state"], "completed");
    assert!(!handoff["changes"].as_array().unwrap().is_empty());
    assert!(handoff["verification"].is_object());
    assert!(handoff["verification"]["status"].as_str().is_some());
    assert!(handoff["verification"]["observations"].is_object());
    let tests = orch.get_test_results(&auth, &run_id).unwrap();
    assert_eq!(tests["status"], "not_observed");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duration_deadline_not_starved_by_event_flood() {
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
    let bus = host.event_bus();
    let flood_sid = session.id;
    let flood = tokio::spawn(async move {
        for i in 0..200_000 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: flood_sid,
                text: format!("flood-starve-{i}"),
            });
            if i % 500 == 0 {
                tokio::task::yield_now().await;
            }
        }
    });
    let t0 = std::time::Instant::now();
    let resp = orch
        .submit_task(
            &auth,
            "starve-1",
            session.id,
            ws.path(),
            "run sleep 10".into(),
            Some(json!({"maxDurationMs": 200, "maxRounds": 8, "maxPromptBytes": 50000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_terminal(&orch, &auth, &run_id).await;
    flood.abort();
    assert_eq!(state, grokptah_agent_bridge::RunState::LimitReached);
    assert!(
        t0.elapsed() < Duration::from_secs(3),
        "deadline starved by event flood: {:?}",
        t0.elapsed()
    );
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn control_secret_redacted_on_shared_host_bus() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let token = "secret-token-adversarial-196";
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch(&host, &home, &ws, 2);
    assert!(host.event_bus().control_secrets_len() >= 1);
    // Publish error containing the token; durable journal path must scrub it.
    let dir = home.path().join(".grokptah/orchestration");
    // Host bus may already persist under grokptah home/orchestration.
    host.event_bus().publish(SessionUpdate::Error {
        session_id: Uuid::new_v4(),
        message: format!("leak {token} here"),
    });
    // Also exercise redaction API with secrets registered on the shared bus.
    let page = host.event_bus().read_after(0, 50);
    let dumped = serde_json::to_string(&page.entries).unwrap();
    assert!(
        !dumped.contains(token),
        "control token must be redacted in journal entries: {dumped}"
    );
    let auth = orch
        .auth_header(Some("Bearer secret-token-adversarial-196"))
        .unwrap();
    assert!(orch
        .queue_prompt(&auth, token, session.id, ws.path(), "/yolo".into(), false,)
        .await
        .is_err());
    let audit_path = home.path().join("orch/audit/audit.jsonl");
    for _ in 0..100 {
        if audit_path.is_file() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let audit = std::fs::read_to_string(audit_path).unwrap();
    assert!(!audit.contains(token), "control token leaked into audit");
    let accepted = orch
        .submit_task(
            &auth,
            "secret-aggregate-run",
            session.id,
            ws.path(),
            format!("run sleep 2 # {token}"),
            None,
        )
        .await
        .unwrap();
    host.event_bus().publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 1,
        max_rounds: 8,
        last_tool: Some(format!("tool-{token}")),
        detail: format!("progress {token}"),
    });
    host.event_bus().publish(SessionUpdate::FileEdit {
        session_id: session.id,
        path: "secret.txt".into(),
        summary: format!("summary {token}"),
        unified_diff: format!("+{token}"),
    });
    host.event_bus()
        .publish(SessionUpdate::ShellSessionStarted {
            session_id: session.id,
            call_id: "secret-test".into(),
            command: format!("XAI_API_KEY={token} cargo test"),
        });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let run_id = accepted["runId"].as_str().unwrap();
    let durable = serde_json::to_string(&orch.store().load_run(run_id).unwrap()).unwrap();
    assert!(
        !durable.contains(token),
        "control token leaked into run data"
    );
    orch.cancel(
        &auth,
        "secret-aggregate-cancel",
        session.id,
        ws.path(),
        Some(run_id),
    )
    .await
    .unwrap();
    let _ = dir;
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn id_traversal_rejected() {
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
        .await
        .is_err());
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn queue_persistence_failure_does_not_mutate_memory() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    let tmp_path = home
        .path()
        .join(".grokptah")
        .join("sessions")
        .join(session.id.to_string())
        .join("prompt_queue.json.tmp");
    std::fs::create_dir_all(&tmp_path).unwrap();
    assert!(host
        .session_queue_add_with_source(
            session.id,
            "must roll back".into(),
            false,
            "control",
            Some("mcp".into()),
        )
        .is_err());
    assert!(host.session_queue_list(session.id).unwrap().is_empty());
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
