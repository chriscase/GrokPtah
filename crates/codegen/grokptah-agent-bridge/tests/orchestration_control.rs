//! Integration tests for #196 orchestration control plane.

use std::path::PathBuf;
use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    hash_payload, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    discovered_tool_names, set_grokptah_home_override, start_control_server, AgentHost, EventBus,
    HostConfig, SessionKind, SessionUpdate, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

fn setup_home() -> tempfile::TempDir {
    let d = tempdir().unwrap();
    let home = d.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    d
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
    let _home = setup_home();
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

#[test]
fn idempotency_conflict_and_replay() {
    let home = setup_home();
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
    assert!(conflict.is_err());
    set_grokptah_home_override(None);
}

#[test]
fn workspace_mismatch_fail_closed() {
    let home = setup_home();
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
        .unwrap_err();
    assert_eq!(err.code.as_str(), "workspace_mismatch");
    set_grokptah_home_override(None);
}

#[test]
fn reject_shell_and_admin_prompts() {
    let home = setup_home();
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
        .is_err());
    assert!(orch
        .queue_prompt(&auth, "b", session.id, ws.path(), "/mcp list".into(), false)
        .is_err());
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
    };
    store.save_run(&run).unwrap();
    let store2 = OrchStore::open(d.path()).unwrap();
    let loaded = store2.load_run("run-x").unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Interrupted);
}

#[tokio::test]
async fn e2e_mcp_client_valid_and_invalid_token() {
    let home = setup_home();
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
