//! Integration tests for #196 orchestration control plane.

use std::path::PathBuf;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    discovered_tool_names, home_override_serial, set_grokptah_home_override, start_control_server,
    AgentHost, EventBus, HostConfig, SessionKind, SessionUpdate, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

/// Serializes home-override + instance-lock across tests (same as bridge lifecycle tests).
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
    host.start().expect("start host");
    host
}

#[test]
fn dual_subscriber_same_ordered_sequences() {
    let (_home, _lock) = setup_home();
    let host = started_host();
    let bus = host.event_bus();
    let mut gui = bus.subscribe();
    let mut mcp = bus.subscribe();
    let sid = Uuid::new_v4();
    for i in 0..10 {
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: format!("x{i}"),
        });
    }
    for i in 0..10 {
        let a = gui.try_recv().unwrap();
        let b = mcp.try_recv().unwrap();
        match (a, b) {
            (
                SessionUpdate::AgentMessageChunk { text: ta, .. },
                SessionUpdate::AgentMessageChunk { text: tb, .. },
            ) => {
                assert_eq!(ta, format!("x{i}"));
                assert_eq!(tb, ta);
            }
            _ => panic!("variant"),
        }
    }
    // journal seq monotonic
    let page = bus.read_after(0, 100);
    assert!(!page.cursor_expired);
    let mut last = 0u64;
    for e in &page.entries {
        assert!(e.seq > last);
        last = e.seq;
    }
    set_grokptah_home_override(None);
}

#[test]
fn schema_snapshot_excludes_forbidden() {
    let names = discovered_tool_names();
    for t in CONTROL_TOOLS {
        assert!(names.contains(t), "missing {t}");
    }
    for f in FORBIDDEN_TOOLS {
        assert!(!names.contains(f), "forbidden {f}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn idempotency_conflict_and_replay() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    let bus = host.event_bus();
    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        bus,
        store,
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let r1 = orch
        .queue_prompt(
            &auth,
            "req-1",
            session.id,
            ws.path(),
            "hello world".into(),
            false,
        )
        .await
        .unwrap();
    let r2 = orch
        .queue_prompt(
            &auth,
            "req-1",
            session.id,
            ws.path(),
            "hello world".into(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(r1, r2);
    let conflict = orch.queue_prompt(
        &auth,
        "req-1",
        session.id,
        ws.path(),
        "different payload".into(),
        false,
    );
    assert!(conflict.await.is_err());
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_mismatch_fail_closed() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let other = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host,
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let err = orch
        .queue_prompt(&auth, "r", session.id, other.path(), "x".into(), false)
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "workspace_mismatch");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reject_shell_and_admin_prompts() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host,
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    assert!(orch
        .queue_prompt(&auth, "a", session.id, ws.path(), "!rm -rf /".into(), false)
        .await
        .is_err());
    assert!(orch
        .queue_prompt(&auth, "b", session.id, ws.path(), "/mcp list".into(), false)
        .await
        .is_err());
    assert!(orch
        .queue_prompt(&auth, "c", session.id, ws.path(), "/yolo".into(), false)
        .await
        .is_err());
    // Validation happens before idempotency is claimed: a rejected payload
    // cannot poison the request ID for a later valid request.
    orch.queue_prompt(
        &auth,
        "c",
        session.id,
        ws.path(),
        "valid follow-up".into(),
        false,
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[test]
fn restart_interrupted_no_auto_resume() {
    let d = tempdir().unwrap();
    let store = OrchStore::open(d.path()).unwrap();
    use chrono::Utc;
    use grokptah_agent_bridge::orchestration::RunRecord;
    let run = RunRecord {
        run_id: "run-x".into(),
        session_id: Uuid::new_v4(),
        workspace: "/w".into(),
        request_id: "q".into(),
        client_id: None,
        state: RunState::Running,
        bounds: RunBounds::default(),
        prompt_preview: "p".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        aggregates: Default::default(),
        progress: None,
    };
    store.save_run(&run).unwrap();
    drop(store);
    let store2 = OrchStore::open(d.path()).unwrap();
    let loaded = store2.load_run("run-x").unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Interrupted);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // home_override_serial MutexGuard must span the whole test
async fn e2e_mcp_client_valid_and_invalid_token() {
    let (home, _lock) = setup_home();
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let bus = host.event_bus();
    let _gui = bus.subscribe();
    let orch = OrchestrationService::new(
        host.clone(),
        bus,
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "secret-196".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let client = reqwest::Client::new();

    let unauth = client
        .post(&url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ptah_list_sessions","arguments":{}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    let list = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ptah_list_sessions","arguments":{}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);

    // benign mutation: queue prompt
    let q = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"ptah_queue_prompt","arguments":{
                "request_id":"e2e-q1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "please summarize later"
            }}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        q.status(),
        200,
        "body={}",
        q.text().await.unwrap_or_default()
    );

    // workspace mismatch does not mutate
    let before = host.session_queue_list(session.id).unwrap().len();
    let bad_ws = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"ptah_queue_prompt","arguments":{
                "request_id":"e2e-q2",
                "session_id": session.id.to_string(),
                "workspace": "/tmp/not-allowlisted-196",
                "prompt": "nope"
            }}
        }))
        .send()
        .await
        .unwrap();
    assert!(bad_ws.status().is_client_error() || bad_ws.status().is_server_error());
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), before);

    srv.stop();
    set_grokptah_home_override(None);
    let _ = Duration::from_millis(1);
    let _ = PathBuf::from(".");
    let _ = hash_payload(&json!({}));
}

fn orch_for(
    host: &grokptah_agent_bridge::AgentHostHandle,
    home: &tempfile::TempDir,
    ws: &tempfile::TempDir,
    max_concurrent: usize,
) -> std::sync::Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds::default(),
        },
    )
}

async fn wait_run_terminal(
    orch: &OrchestrationService,
    auth: &grokptah_agent_bridge::orchestration::AuthContext,
    run_id: &str,
    timeout: Duration,
) -> RunState {
    let start = std::time::Instant::now();
    loop {
        let v = orch.get_run(auth, run_id).unwrap();
        let state: RunState = serde_json::from_value(v["state"].clone()).unwrap();
        if !matches!(state, RunState::Running | RunState::Queued) {
            return state;
        }
        if start.elapsed() > timeout {
            panic!("run {run_id} still {state:?} after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_task_reaches_terminal_offline() {
    let _offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let resp = orch
        .submit_task(
            &auth,
            "sub-1",
            session.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::Completed);
    let handoff = orch.get_handoff(&auth, &run_id).unwrap();
    assert!(handoff["finalResponse"].as_str().is_some());
    // Idempotent retry
    let again = orch
        .submit_task(
            &auth,
            "sub-1",
            session.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    assert_eq!(again["runId"], run_id);
    set_grokptah_home_override(None);
    if _offline.is_none() {
        std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_duration_limit_reached() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let resp = orch
        .submit_task(
            &auth,
            "lim-dur",
            session.id,
            ws.path(),
            "run sleep 5".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 80})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(15)).await;
    assert_eq!(state, RunState::LimitReached);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_session_busy_and_capacity() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s1 = host.session_new_kind(SessionKind::Build).unwrap();
    let s2 = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s1.id, ws.path()).unwrap();
    host.session_set_cwd(s2.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 1);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let _r1 = orch
        .submit_task(
            &auth,
            "cap-1",
            s1.id,
            ws.path(),
            "run sleep 2".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    // Give the first turn a moment to mark session busy / reserve capacity.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let busy = orch
        .submit_task(
            &auth,
            "cap-busy",
            s1.id,
            ws.path(),
            "list files".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(busy.code.as_str(), "session_busy");

    let cap = orch
        .submit_task(&auth, "cap-2", s2.id, ws.path(), "list files".into(), None)
        .await
        .unwrap_err();
    assert_eq!(cap.code.as_str(), "capacity_exhausted");

    // Atomic capacity: concurrent second reserves must not oversubscribe max=1.
    let cap_snap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap_snap["maxConcurrentRuns"], 1);
    assert!(cap_snap["activeRuns"].as_u64().unwrap() >= 1);

    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancel_isolates_sessions() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let a = host.session_new_kind(SessionKind::Build).unwrap();
    let b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(a.id, ws.path()).unwrap();
    host.session_set_cwd(b.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let ra = orch
        .submit_task(
            &auth,
            "can-a",
            a.id,
            ws.path(),
            "run sleep 8".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 60000})),
        )
        .await
        .unwrap();
    let rb = orch
        .submit_task(
            &auth,
            "can-b",
            b.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let run_a = ra["runId"].as_str().unwrap().to_string();
    orch.cancel(&auth, "can-req", a.id, ws.path(), Some(&run_a))
        .await
        .unwrap();
    let state_a = wait_run_terminal(&orch, &auth, &run_a, Duration::from_secs(10)).await;
    assert!(
        matches!(
            state_a,
            RunState::Cancelled | RunState::Completed | RunState::Failed
        ),
        "got {state_a:?}"
    );
    // Session B still finishes independently.
    let run_b = rb["runId"].as_str().unwrap().to_string();
    let state_b = wait_run_terminal(&orch, &auth, &run_b, Duration::from_secs(10)).await;
    assert_eq!(state_b, RunState::Completed);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn steer_via_orchestration_service() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &_home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    // Idle session → steer defers to queue (non-cancelling).
    let idle = orch
        .steer(
            &auth,
            "steer-idle",
            session.id,
            ws.path(),
            "please prefer tests".into(),
        )
        .await
        .unwrap();
    assert_eq!(idle["disposition"], "queued");

    let _run = orch
        .submit_task(
            &auth,
            "steer-run",
            session.id,
            ws.path(),
            "run sleep 3".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let pending = orch
        .steer(
            &auth,
            "steer-live",
            session.id,
            ws.path(),
            "keep going carefully".into(),
        )
        .await
        .unwrap();
    assert_eq!(pending["disposition"], "pending");
    set_grokptah_home_override(None);
}

#[test]
fn queue_survives_host_restart() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (_home, _lock) = setup_home();
    let ws = tempdir().unwrap();
    let session_id = {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.session_queue_add(session.id, "follow-up after restart".into(), false)
            .unwrap();
        host.session_queue_add(session.id, "second item".into(), true)
            .unwrap();
        let listed = host.session_queue_list(session.id).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].text, "second item"); // priority front
        session.id
    };
    // New host process-equivalent: same home, fresh AgentHost.
    let host2 = started_host();
    let listed = host2.session_queue_list(session_id).unwrap();
    assert_eq!(listed.len(), 2, "queue must reload from disk");
    assert_eq!(listed[0].text, "second item");
    assert_eq!(listed[1].text, "follow-up after restart");
    set_grokptah_home_override(None);
}

#[test]
fn journal_reload_supports_run_scoped_reads() {
    let dir = tempdir().unwrap();
    let sid = Uuid::new_v4();
    let bus1 = EventBus::new(64).with_persist_dir(dir.path());
    bus1.publish(SessionUpdate::FileEdit {
        session_id: sid,
        path: "a.rs".into(),
        summary: "edited".into(),
        unified_diff: "diff".into(),
    });
    let start = bus1.current_seq();
    drop(bus1);
    let bus2 = EventBus::new(64).with_persist_dir(dir.path());
    let page = bus2.read_after(0, 50);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].seq, start);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn capacity_race_against_real_submit_task() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    // max_concurrent_runs=2; flood 8 distinct sessions so capacity (not busy) is the gate.
    let mut sessions = Vec::new();
    for _ in 0..8 {
        let s = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(s.id, ws.path()).unwrap();
        sessions.push(s.id);
    }
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let mut futs = Vec::new();
    for (i, sid) in sessions.into_iter().enumerate() {
        let orch = orch.clone();
        let auth = auth.clone();
        let ws_path = ws.path().to_path_buf();
        futs.push(async move {
            orch.submit_task(
                &auth,
                &format!("race-{i}"),
                sid,
                &ws_path,
                "run sleep 3".into(),
                Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
            )
            .await
        });
    }
    let results = futures::future::join_all(futs).await;
    let accepted = results.iter().filter(|r| r.is_ok()).count();
    let exhausted = results
        .iter()
        .filter(|r| {
            r.as_ref()
                .err()
                .map(|e| e.code.as_str() == "capacity_exhausted")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        accepted, 2,
        "exactly max_concurrent_runs must accept under race"
    );
    assert_eq!(exhausted, 6, "remainder must fail capacity_exhausted");
    let cap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap["activeRuns"].as_u64().unwrap(), 2);
    assert_eq!(cap["maxConcurrentRuns"].as_u64().unwrap(), 2);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_run_reserves_session_against_desktop_prompt() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "reserve-1",
            session.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    assert!(host
        .session_prompt(session.id, "desktop collision".into())
        .await
        .is_err());
    let run_id = accepted["runId"].as_str().unwrap();
    orch.cancel(&auth, "reserve-cancel", session.id, ws.path(), Some(run_id))
        .await
        .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_same_session_submits_accept_exactly_one() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let one = orch.submit_task(
        &auth,
        "same-session-1",
        session.id,
        ws.path(),
        "run sleep 3".into(),
        None,
    );
    let two = orch.submit_task(
        &auth,
        "same-session-2",
        session.id,
        ws.path(),
        "run sleep 3".into(),
        None,
    );
    let (one, two) = tokio::join!(one, two);
    let results = [one, two];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let rejected = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .expect("one submit must be rejected");
    assert_eq!(rejected.code.as_str(), "session_busy");
    let accepted = results
        .iter()
        .find_map(|result| result.as_ref().ok())
        .expect("one submit must be accepted");
    orch.cancel(
        &auth,
        "same-session-cancel",
        session.id,
        ws.path(),
        accepted["runId"].as_str(),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn capacity_is_shared_across_control_service_instances() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let first = host.session_new_kind(SessionKind::Build).unwrap();
    let second = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(first.id, ws.path()).unwrap();
    host.session_set_cwd(second.id, ws.path()).unwrap();
    let one = orch_for(&host, &home, &ws, 1);
    let two = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        one.store().clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 8,
            bounds: RunBounds::default(),
        },
    );
    let auth_one = one.auth_header(Some("Bearer t")).unwrap();
    let auth_two = two.auth_header(Some("Bearer t")).unwrap();
    assert_eq!(two.get_capacity(&auth_two).unwrap()["maxConcurrentRuns"], 1);
    let accepted = one
        .submit_task(
            &auth_one,
            "global-cap-1",
            first.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let error = two
        .submit_task(
            &auth_two,
            "global-cap-2",
            second.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "capacity_exhausted");
    one.cancel(
        &auth_one,
        "global-cap-cancel",
        first.id,
        ws.path(),
        accepted["runId"].as_str(),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_progress_is_durable_outside_event_retention() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "progress-1",
            session.id,
            ws.path(),
            "run sleep 2".into(),
            None,
        )
        .await
        .unwrap();
    host.event_bus().publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 3,
        max_rounds: 8,
        last_tool: Some("shell".into()),
        detail: "verifying".into(),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let run_id = accepted["runId"].as_str().unwrap();
    let progress = orch.get_progress(&auth, run_id).unwrap();
    assert_eq!(progress["progress"]["round"], 3);
    assert_eq!(progress["progress"]["lastTool"], "shell");
    orch.cancel(
        &auth,
        "progress-cancel",
        session.id,
        ws.path(),
        Some(run_id),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_round_limit_reached_via_wired_max_rounds() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    // Offline path honors turn_max_rounds for simulate_tool_rounds prompts.
    let resp = orch
        .submit_task(
            &auth,
            "round-lim",
            session.id,
            ws.path(),
            "simulate_tool_rounds please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::LimitReached);
    let handoff = orch.get_handoff(&auth, &run_id).unwrap();
    let text = handoff["finalResponse"].as_str().unwrap_or("");
    assert!(
        text.contains("Stopped after 2 tool rounds"),
        "expected stop message reflecting max_rounds=2, got {text:?}"
    );
    set_grokptah_home_override(None);
}
