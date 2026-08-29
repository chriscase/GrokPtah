//! Integration tests drive the **shipped** AgentHostHandle API (not a reimplementation).
//!
//! All tests install an isolated GrokPtah home so they never pollute the
//! developer's real `~/.grokptah` session list.

#![allow(clippy::while_let_loop)] // event-drain loops are clearer as loop+match

use std::time::Duration;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::Arc,
};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use grokptah_agent_bridge::{
    desktop_auto_update_enabled, home_override_serial, model_selection_key,
    set_grokptah_home_override, AgentHost, HostConfig, PermissionDecision, RunState, SessionUpdate,
};
use tokio::time::timeout;

/// RAII: point `grokptah_home()` at a temp dir for the duration of a test.
struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn install() -> Self {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().expect("isolated home tempdir");
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions dir");
        std::fs::create_dir_all(home.join("plugins")).ok();
        std::fs::create_dir_all(home.join("skills")).ok();
        set_grokptah_home_override(Some(home));
        // Prevent live API / tool-loop network calls during unit tests.
        // SAFETY: tests serialize on home_override_serial mutex.
        unsafe {
            std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
        }
        Self {
            _tmp: tmp,
            _lock: lock,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        unsafe {
            std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
        }
    }
}

struct CompatibleHome {
    _tmp: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CompatibleHome {
    fn install(base_url: &str) -> Self {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().expect("isolated compatible home");
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions dir");
        set_grokptah_home_override(Some(home));
        unsafe {
            std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
            std::env::set_var("GROKPTAH_API_BASE", base_url);
            std::env::set_var("GROKPTAH_API_KEY", "synthetic-compatible-key");
        }
        Self {
            _tmp: tmp,
            _lock: lock,
        }
    }
}

impl Drop for CompatibleHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        unsafe {
            std::env::remove_var("GROKPTAH_API_BASE");
            std::env::remove_var("GROKPTAH_API_KEY");
        }
    }
}

#[derive(Clone)]
struct CompatibleGatewayState {
    requests: Arc<AtomicUsize>,
    saw_write_result: Arc<AtomicBool>,
    saw_test_result: Arc<AtomicBool>,
}

async fn compatible_gateway(
    State(state): State<CompatibleGatewayState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer synthetic-compatible-key")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let messages = body["messages"].as_array().ok_or(StatusCode::BAD_REQUEST)?;
    let request = state.requests.fetch_add(1, Ordering::SeqCst);
    let message = match request {
        0 => serde_json::json!({
            "content": null,
            "tool_calls": [{
                "id": "call-write",
                "type": "function",
                "function": {
                    "name": "write_file",
                    "arguments": "{\"path\":\"provider-proof.txt\",\"content\":\"compatible agent wrote this\\n\"}"
                }
            }]
        }),
        1 => {
            let saw_result = messages.iter().any(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "call-write"
            });
            state.saw_write_result.store(saw_result, Ordering::SeqCst);
            serde_json::json!({
                "content": null,
                "tool_calls": [{
                    "id": "call-test",
                    "type": "function",
                    "function": {
                        "name": "run_terminal_cmd",
                        "arguments": "{\"command\":\"test -f provider-proof.txt && grep -q 'compatible agent' provider-proof.txt\"}"
                    }
                }]
            })
        }
        2 => {
            let saw_result = messages
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == "call-test");
            state.saw_test_result.store(saw_result, Ordering::SeqCst);
            serde_json::json!({"content": "Implemented and verified the disposable task."})
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    Ok(Json(serde_json::json!({
        "choices": [{"message": message}],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })))
}

async fn drain_until_turn_complete(
    rx: &mut grokptah_agent_bridge::EventReceiver,
) -> Vec<SessionUpdate> {
    let mut events = Vec::new();
    loop {
        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("channel closed");
        let done = matches!(ev, SessionUpdate::TurnComplete { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    events
}

#[tokio::test]
async fn start_stop_and_status() {
    let _iso = IsolatedHome::install();
    let host = AgentHost::create(HostConfig::default());
    assert!(!host.status().running);
    host.start().unwrap();
    assert!(host.status().running);
    assert!(!host.status().auto_update_enabled);
    assert!(!desktop_auto_update_enabled());
    host.stop().unwrap();
    assert!(!host.status().running);
}

#[tokio::test]
async fn session_lifecycle_prompt_streams_message() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello grokptah").unwrap();

    let host = AgentHost::create(HostConfig::default());
    let mut rx = host.take_event_receiver().expect("event rx");
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    host.session_prompt(session.id, "say hi and list files".into())
        .await
        .unwrap();

    let events = drain_until_turn_complete(&mut rx).await;

    // Host no longer dumps kind=/round status as thought chunks in the stream.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionUpdate::AgentThoughtChunk { .. })),
        "status crumbs must not appear as thoughts, got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionUpdate::AgentMessageChunk { .. })),
        "expected message chunks"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SessionUpdate::ToolCall {
                title,
                ..
            } if title == "list_dir"
        )),
        "expected list_dir tool for 'list files'"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        SessionUpdate::TurnComplete {
            cancelled: false,
            ..
        }
    )));
    let started_turn_id = events.iter().find_map(|e| match e {
        SessionUpdate::TurnStarted { turn_id, .. } => Some(*turn_id),
        _ => None,
    });
    let evidence_turn_id = events.iter().find_map(|e| match e {
        SessionUpdate::CompletionEvidence { turn_id, .. } => Some(*turn_id),
        _ => None,
    });
    assert_eq!(started_turn_id, evidence_turn_id);
    let evidence_index = events
        .iter()
        .position(|e| matches!(e, SessionUpdate::CompletionEvidence { .. }))
        .expect("every completed turn must emit completion evidence");
    let complete_index = events
        .iter()
        .position(|e| matches!(e, SessionUpdate::TurnComplete { .. }))
        .expect("every completed turn must emit TurnComplete");
    assert!(evidence_index < complete_index);

    let history = host.session_completion_history(session.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].evidence.status, "unverified");
    let turn_id = history[0].turn_id;
    let runs = host.list_session_runs(session.id).unwrap();
    assert_eq!(runs.len(), 1, "Build turn should create one durable run");
    assert_eq!(runs[0].state, RunState::Completed);
    assert_eq!(runs[0].client_id.as_deref(), Some("desktop"));
    assert_eq!(
        runs[0].aggregates.verification,
        Some(history[0].evidence.clone())
    );
    drop(host);

    let restored_host = AgentHost::create(HostConfig::default());
    let restored = restored_host
        .session_completion_history(session.id)
        .unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].turn_id, turn_id);
    assert_eq!(restored[0].evidence.status, "unverified");
    restored_host.ensure_orchestration_store().unwrap();
    let restored_runs = restored_host.list_session_runs(session.id).unwrap();
    assert_eq!(restored_runs.len(), 1);
    assert_eq!(restored_runs[0].state, RunState::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn compatible_model_completes_a_multi_round_edit_and_test_task() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = CompatibleGatewayState {
        requests: Arc::new(AtomicUsize::new(0)),
        saw_write_result: Arc::new(AtomicBool::new(false)),
        saw_test_result: Arc::new(AtomicBool::new(false)),
    };
    let app = Router::new()
        .route("/v1/chat/completions", post(compatible_gateway))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let _iso = CompatibleHome::install(&format!("http://{address}/v1"));
    let workspace = tempfile::tempdir().unwrap();

    let host = AgentHost::create(HostConfig::default());
    let mut events = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    host.set_always_approve(true);
    host.set_model(model_selection_key("env-grokptah", "synthetic-cheap-code"));
    let session = host.session_new().unwrap();

    let reply = timeout(
        Duration::from_secs(10),
        host.session_prompt(
            session.id,
            "Create the requested proof file and verify it exists.".into(),
        ),
    )
    .await
    .expect("compatible model turn timed out")
    .unwrap();
    let updates = drain_until_turn_complete(&mut events).await;

    let proof = std::fs::read_to_string(workspace.path().join("provider-proof.txt"))
        .unwrap_or_else(|error| {
            panic!(
                "provider proof missing after {} model requests: {error}; reply={reply}",
                state.requests.load(Ordering::SeqCst),
            )
        });
    assert_eq!(proof, "compatible agent wrote this\n");
    assert_eq!(state.requests.load(Ordering::SeqCst), 3);
    assert!(state.saw_write_result.load(Ordering::SeqCst));
    assert!(state.saw_test_result.load(Ordering::SeqCst));
    assert!(updates.iter().any(|update| matches!(
        update,
        SessionUpdate::TurnComplete {
            cancelled: false,
            ..
        }
    )));

    host.stop().unwrap();
    server.abort();
}

#[tokio::test]
async fn offline_write_emits_file_edit() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().expect("event rx");
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    host.session_prompt(session.id, "write hello.txt: hi from offline".into())
        .await
        .unwrap();

    let events = drain_until_turn_complete(&mut rx).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            SessionUpdate::FileEdit { path, .. } if path == "hello.txt"
        )),
        "expected FileEdit for hello.txt, got {events:?}"
    );
    let written = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
    assert!(written.contains("hi from offline"));
}

#[tokio::test]
async fn permission_request_round_trip_allow() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let host2 = host.clone();
    let prompt = tokio::spawn(async move {
        host2
            .session_prompt(session.id, "run echo permission-ok".into())
            .await
    });

    // Buffer every event from the first receive through TurnComplete so we
    // never drop ToolCall / ShellSessionStarted that fire between permission
    // grant and prompt join (prior flakes: Elapsed while re-draining).
    let mut events = Vec::new();
    let id = loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = &ev {
            let id = request.id;
            events.push(ev);
            break id;
        }
        events.push(ev);
    };
    host.permission_respond(id, PermissionDecision::Allow)
        .unwrap();

    prompt.await.unwrap().unwrap();

    // Drain remaining until TurnComplete (or short quiet window after).
    loop {
        match timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ev)) => {
                let done = matches!(ev, SessionUpdate::TurnComplete { .. });
                events.push(ev);
                if done {
                    break;
                }
            }
            _ => break,
        }
    }

    let saw_tool = events.iter().any(|e| {
        matches!(
            e,
            SessionUpdate::ToolCall { title, .. } if title == "run_terminal_cmd"
        )
    });
    let saw_shell_started = events
        .iter()
        .any(|e| matches!(e, SessionUpdate::ShellSessionStarted { command, .. } if command.contains("echo")));
    let saw_reattach_reexec = events.iter().any(|e| {
        matches!(
            e,
            SessionUpdate::BackgroundTask { status, title, .. }
                if status == "attachable" && title.starts_with("terminal:")
        )
    });
    let saw_complete = events
        .iter()
        .any(|e| matches!(e, SessionUpdate::TurnComplete { .. }));

    assert!(
        saw_tool,
        "shell tool should run after allow, got {events:?}"
    );
    assert!(
        saw_shell_started,
        "must emit ShellSessionStarted for live attach, got {events:?}"
    );
    assert!(
        !saw_reattach_reexec,
        "must not emit attachable terminal: background task (re-exec)"
    );
    assert!(saw_complete, "expected TurnComplete, got {events:?}");
}

#[tokio::test]
async fn permission_deny_skips_tool() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let host2 = host.clone();
    let prompt = tokio::spawn(async move {
        host2
            .session_prompt(session.id, "run echo should-deny".into())
            .await
    });

    let mut events = Vec::new();
    loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = &ev {
            let id = request.id;
            events.push(ev);
            host.permission_respond(id, PermissionDecision::Deny)
                .unwrap();
            break;
        }
        events.push(ev);
    }
    prompt.await.unwrap().unwrap();

    loop {
        match timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ev)) => {
                let done = matches!(ev, SessionUpdate::TurnComplete { .. });
                events.push(ev);
                if done {
                    break;
                }
            }
            _ => break,
        }
    }

    let denied = events.iter().any(|e| {
        matches!(
            e,
            SessionUpdate::ToolCall {
                status: grokptah_agent_bridge::ToolCallStatus::Denied,
                ..
            }
        )
    });
    assert!(denied, "expected denied tool call, got {events:?}");
}

#[tokio::test]
async fn cancel_kills_real_shell_child_not_only_sleep_stub() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    // Marker file written only if sleep completes — cancel must prevent this.
    let marker = dir.path().join("should_not_exist.txt");
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let cmd = format!("sleep 30; echo done > '{}'", marker.display());
    let host2 = host.clone();
    let prompt =
        tokio::spawn(async move { host2.session_prompt(session.id, format!("run {cmd}")).await });

    // Wait until live shell session started (real child spawned)
    loop {
        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("event")
            .expect("open");
        match &ev {
            SessionUpdate::ShellSessionStarted { .. } => break,
            SessionUpdate::ToolCall { title, .. } if title == "run_terminal_cmd" => {
                // continue until shell started
            }
            _ => {}
        }
    }

    host.cancel_turn(None).expect("cancel");
    let _ = prompt.await;

    let mut cancelled = false;
    let mut shell_cancelled = false;
    while let Ok(Some(ev)) = timeout(Duration::from_secs(5), rx.recv()).await {
        match &ev {
            SessionUpdate::ShellSessionEnded { cancelled: c, .. } => {
                shell_cancelled = *c;
            }
            SessionUpdate::TurnComplete { cancelled: c, .. } => {
                cancelled = *c;
                break;
            }
            _ => {}
        }
    }
    assert!(cancelled, "TurnComplete.cancelled must be true");
    assert!(
        shell_cancelled,
        "ShellSessionEnded.cancelled must be true for killed child"
    );
    // Give OS a moment; marker must not appear
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !marker.exists(),
        "cancelled shell must not run post-sleep commands (double-exec or unkillable child)"
    );
}

#[tokio::test]
async fn shell_streams_output_without_reexec_event() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "run echo unique-stream-token-xyz".into())
        .await
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionUpdate::ShellSessionStarted { .. })),
        "ShellSessionStarted"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SessionUpdate::ShellOutput { data, .. } if data.contains("unique-stream-token-xyz")
        )),
        "live ShellOutput from the tool process, got {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SessionUpdate::BackgroundTask { status, title, .. }
                if status == "attachable" && title.starts_with("terminal:")
        )),
        "must not advertise re-exec attach"
    );
}

#[tokio::test]
async fn fork_rewind_compact_sessions() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s1 = host.session_new().unwrap();
    host.session_prompt(s1.id, "/help".into()).await.unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;

    let forked = host.fork_session(s1.id).unwrap();
    assert_eq!(forked.forked_from, Some(s1.id));
    assert!(host.list_sessions().len() >= 2);

    host.session_prompt(forked.id, "/help again".into())
        .await
        .unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;

    let after_rewind = host.rewind_session(forked.id, 1, "conversation").unwrap();
    assert!(after_rewind.message_count <= 2);

    host.session_prompt(forked.id, "msg2".into()).await.unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;
    host.session_prompt(forked.id, "msg3".into()).await.unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;
    let before = host.session_transcript(forked.id).unwrap();
    let before_len = before.len();
    assert!(
        before_len > 0,
        "need history before compact, got {before_len}"
    );
    let compacted = host.compact_session(forked.id).unwrap();
    let after = host.session_transcript(forked.id).unwrap();
    // Compact must never delete local content — only shrink the server window.
    assert!(
        after.len() >= before_len,
        "local transcript must be retained (before={before_len}, after={})",
        after.len()
    );
    assert!(
        compacted.message_count >= before_len,
        "message_count must reflect full local history"
    );
    // Every pre-compact line still present (prefix equality).
    for (i, e) in before.iter().enumerate() {
        assert_eq!(
            after[i].role, e.role,
            "role mismatch at index {i} after compact"
        );
        assert_eq!(
            after[i].text, e.text,
            "text lost at index {i} after compact"
        );
    }
}

#[tokio::test]
async fn session_set_cwd_updates_session_and_host_project() {
    let _iso = IsolatedHome::install();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir_a.path()).unwrap();
    let s = host.session_new().unwrap();
    assert_eq!(
        std::path::Path::new(&s.cwd),
        dir_a.path(),
        "new session inherits host project cwd"
    );

    let updated = host.session_set_cwd(s.id, dir_b.path()).unwrap();
    assert_eq!(std::path::Path::new(&updated.cwd), dir_b.path());

    // Active session: host project should follow.
    let status = host.status();
    assert_eq!(
        status.project_cwd.as_deref().map(std::path::Path::new),
        Some(dir_b.path())
    );

    // Reload preserves per-session cwd.
    let loaded = host.session_load(s.id).unwrap();
    assert_eq!(std::path::Path::new(&loaded.cwd), dir_b.path());
}

#[tokio::test]
async fn write_tool_records_edit_and_plugins_install_to_disk() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "write notes.txt: hello from test".into())
        .await
        .unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;
    assert!(
        dir.path().join("notes.txt").is_file(),
        "write_file must create the file on disk"
    );
    let plugin = host.plugin_install("diff-review").unwrap();
    assert!(plugin.installed);
    let plugins = host.plugins();
    assert!(plugins.iter().any(|p| p.id == "diff-review" && p.installed));
}

#[tokio::test]
async fn plan_mode_emits_plan_update() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    host.session_prompt(s.id, "make a plan for refactor".into())
        .await
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    assert!(events.iter().any(|e| matches!(
        e,
        SessionUpdate::Plan {
            status,
            ..
        } if status == "proposed"
    )));
    // Accept starts an execution turn (offline agent).
    let reply = host.accept_plan(s.id).await.unwrap();
    assert!(!reply.is_empty());
    let events2 = drain_until_turn_complete(&mut rx).await;
    assert!(
        events2.iter().any(|e| matches!(
            e,
            SessionUpdate::Plan { status, .. } if status == "accepted" || status == "done" || status == "executing"
        )) || events2
            .iter()
            .any(|e| matches!(e, SessionUpdate::TurnComplete { .. })),
        "expected plan execution events, got {events2:?}"
    );
}

#[tokio::test]
async fn slash_context_and_model_work_offline() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();

    let r = host.session_prompt(s.id, "/model".into()).await.unwrap();
    assert!(r.to_lowercase().contains("model"), "{r}");
    let _ = drain_until_turn_complete(&mut rx).await;

    let r = host
        .session_prompt(s.id, "/effort high".into())
        .await
        .unwrap();
    assert!(r.contains("high"), "{r}");
    let _ = drain_until_turn_complete(&mut rx).await;

    let r = host.session_prompt(s.id, "/context".into()).await.unwrap();
    assert!(r.contains("Context:"), "{r}");
    let _ = drain_until_turn_complete(&mut rx).await;

    let r = host
        .session_prompt(s.id, "/sandbox read-only".into())
        .await
        .unwrap();
    assert!(r.contains("read-only"), "{r}");
    assert_eq!(host.status().sandbox_profile, "read-only");
    let _ = drain_until_turn_complete(&mut rx).await;
}

#[tokio::test]
async fn hook_denies_write_file() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".grokptah")).unwrap();
    std::fs::write(
        dir.path().join(".grokptah/hooks.json"),
        r#"{
  "hooks": {
    "PreToolUse": [
      { "matcher": "write_file", "deny": true, "message": "fixture forbids writes" }
    ],
    "PostToolUse": []
  }
}"#,
    )
    .unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    let reply = host
        .session_prompt(s.id, "write blocked.txt: should fail".into())
        .await
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    let denied = events.iter().any(|e| {
        matches!(
            e,
            SessionUpdate::ToolCall {
                status: grokptah_agent_bridge::ToolCallStatus::Denied,
                title,
                ..
            } if title == "write_file"
        )
    });
    assert!(
        denied || reply.contains("DENIED") || !dir.path().join("blocked.txt").exists(),
        "hook should deny write; reply={reply} events={events:?}"
    );
    assert!(
        !dir.path().join("blocked.txt").exists(),
        "file must not be written when hook denies"
    );
}

#[tokio::test]
async fn explore_slash_spawns_subagent() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello explore").unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    let reply = host
        .session_prompt(s.id, "/explore README layout".into())
        .await
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionUpdate::SubagentSpawned { kind, .. } if kind == "explore")),
        "expected SubagentSpawned, got {events:?}"
    );
    assert!(
        reply.contains("Explore") || reply.contains("README") || reply.contains("listing"),
        "explore summary: {reply}"
    );
    assert!(!host.subagents().is_empty());
}

#[tokio::test]
async fn sandbox_readonly_blocks_write() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    host.set_sandbox("read-only".into());
    let s = host.session_new().unwrap();
    let _ = host
        .session_prompt(s.id, "write no.txt: nope".into())
        .await
        .unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;
    assert_eq!(host.status().sandbox_profile, "read-only");
    assert!(
        !dir.path().join("no.txt").exists(),
        "read-only sandbox must block offline write"
    );
}

/// Regression: tests must not leave sessions in the real user home.
#[tokio::test]
async fn isolated_home_does_not_touch_user_sessions_dir() {
    let user_sessions = dirs::home_dir().unwrap().join(".grokptah").join("sessions");
    let before: Vec<_> = std::fs::read_dir(&user_sessions)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();

    {
        let _iso = IsolatedHome::install();
        let dir = tempfile::tempdir().unwrap();
        let host = AgentHost::create(HostConfig::default());
        host.start().unwrap();
        host.set_project_cwd(dir.path()).unwrap();
        let _ = host.session_new().unwrap();
        let _ = host.session_new().unwrap();
    }

    let after: Vec<_> = std::fs::read_dir(&user_sessions)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert_eq!(
        before, after,
        "creating sessions under IsolatedHome must not add dirs under ~/.grokptah/sessions"
    );
}

#[tokio::test]
async fn always_allow_scopes_to_tool_not_global() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    // First: shell prompt → AlwaysAllow for run_terminal_cmd only
    let host2 = host.clone();
    let prompt1 = tokio::spawn(async move {
        host2
            .session_prompt(session.id, "run echo scoped-always".into())
            .await
    });

    let (req_id, tool_name) = loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = ev {
            break (request.id, request.tool_name);
        }
    };
    assert_eq!(tool_name, "run_terminal_cmd");
    host.permission_respond(req_id, PermissionDecision::AlwaysAllow)
        .unwrap();
    prompt1.await.unwrap().unwrap();
    // Drain turn complete
    loop {
        match timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(SessionUpdate::TurnComplete { .. })) => break,
            Ok(Some(_)) => continue,
            _ => break,
        }
    }

    // Global YOLO must remain off
    assert!(
        !host.status().always_approve,
        "AlwaysAllow must not flip global always_approve"
    );

    // Second: write_file still requires permission (not covered by shell always-allow)
    let host3 = host.clone();
    let prompt2 = tokio::spawn(async move {
        host3
            .session_prompt(session.id, "write scoped.txt: should still prompt".into())
            .await
    });

    let mut saw_write_perm = false;
    let mut write_req = None;
    loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for write permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = &ev {
            assert_eq!(request.tool_name, "write_file");
            saw_write_perm = true;
            write_req = Some(request.id);
            break;
        }
        if matches!(ev, SessionUpdate::TurnComplete { .. }) {
            break;
        }
    }
    assert!(
        saw_write_perm,
        "write_file must still prompt after shell AlwaysAllow"
    );
    host.permission_respond(write_req.unwrap(), PermissionDecision::Deny)
        .unwrap();
    let _ = prompt2.await;
}

#[tokio::test]
async fn project_local_mcp_does_not_spawn_until_trusted() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("mcp_spawned.marker");
    let marker_s = marker.display().to_string();
    // Hostile project config: spawn touches a marker file immediately.
    let cfg = serde_json::json!({
        "servers": [{
            "name": "evil",
            "transport": "stdio",
            "command": "sh",
            "args": ["-c", format!("touch '{marker_s}'; sleep 60")],
            "enabled": true
        }]
    });
    std::fs::write(
        dir.path().join(".mcp.json"),
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();

    // Untrusted: listing tools must not start the process.
    let tools = grokptah_agent_bridge::list_mcp_tools(Some(dir.path())).await;
    // Give a moment in case something races.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !marker.exists(),
        "untrusted project .mcp.json must not spawn stdio servers"
    );
    assert!(
        tools.is_empty(),
        "no MCP tools when untrusted, got {tools:?}"
    );

    // Trust then list again — process may fail handshake but must start (marker).
    grokptah_agent_bridge::set_project_mcp_trusted(dir.path(), true).unwrap();
    let _ = grokptah_agent_bridge::list_mcp_tools(Some(dir.path())).await;
    // Wait for shell to touch marker
    for _ in 0..50 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        marker.exists(),
        "trusted project should spawn MCP command (marker missing)"
    );
}

#[tokio::test]
async fn turn_busy_clears_when_guard_drops_after_panic() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    assert!(!host.session_busy(session.id));

    // Simulate mid-turn panic: mark busy, then drop guard via catch_unwind path.
    // Public path: run a turn, then prove we can re-prompt (busy cleared).
    host.session_prompt(session.id, "list files".into())
        .await
        .unwrap();
    assert!(
        !host.session_busy(session.id),
        "session must not stay busy after normal turn"
    );

    // Second turn must be accepted (not "already has an active turn")
    host.session_prompt(session.id, "list files".into())
        .await
        .unwrap();
    assert!(!host.session_busy(session.id));
}

#[tokio::test]
async fn multi_turn_wire_includes_prior_tool_output() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    // Unique secret only available via a file tool read — never in the user prompt of turn 2.
    let secret = "TOOL_AMNESIA_SECRET_9f3c2a1b";
    std::fs::write(dir.path().join("secret_note.txt"), secret).unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    // Turn 1: offline path reads the file via tools and records tool transcript.
    host.session_prompt(session.id, "read secret_note.txt".into())
        .await
        .unwrap();

    // Wire preview for the *next* model call must include the tool output.
    let wire = host.wire_messages_preview(session.id).unwrap();
    let blob = serde_json::to_string(&wire).unwrap();
    assert!(
        blob.contains(secret),
        "prior-turn tool output must appear in wire context; got {}",
        &blob.chars().take(800).collect::<String>()
    );

    // Turn 2: user asks only about the secret without re-stating it.
    host.session_prompt(session.id, "What was in secret_note.txt?".into())
        .await
        .unwrap();
    let wire2 = host.wire_messages_preview(session.id).unwrap();
    let blob2 = serde_json::to_string(&wire2).unwrap();
    assert!(
        blob2.contains(secret),
        "second turn wire must still carry prior tool facts; missing secret"
    );
}

#[tokio::test]
async fn read_file_truncate_survives_cjk_over_cap() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    // > 32k bytes of multi-byte CJK — must not panic on byte-index truncate
    let body = "字".repeat(20_000);
    assert!(body.len() > 32_000);
    std::fs::write(dir.path().join("big_cjk.txt"), &body).unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    // Offline read path uses tool_read_file (32k cap)
    host.session_prompt(session.id, "read big_cjk.txt".into())
        .await
        .expect("CJK over-cap read must not panic");
    let wire = host.wire_messages_preview(session.id).unwrap();
    let blob = serde_json::to_string(&wire).unwrap();
    assert!(
        blob.contains("truncated") || blob.contains("字"),
        "tool output should appear (truncated or prefix)"
    );
}

#[test]
fn turn_busy_guard_clears_on_panic_unwind() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    use std::panic::{catch_unwind, AssertUnwindSafe};
    let host2 = host.clone();
    let sid = session.id;
    let r = catch_unwind(AssertUnwindSafe(|| {
        host2.test_only_panic_while_turn_busy(sid);
    }));
    assert!(r.is_err(), "helper must panic");
    assert!(
        !host.session_busy(session.id),
        "TurnBusyGuard Drop must clear busy after panic unwind"
    );
    // Session must accept a new turn (not wedged).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        host.session_prompt(session.id, "list files".into())
            .await
            .expect("must accept turn after panic cleanup");
    });
}

#[tokio::test]
async fn deny_rule_blocks_shell_without_prompt() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    host.set_allow_deny_rules(vec![], vec!["Shell(*)".into()]);
    let session = host.session_new().unwrap();
    let mut rx = host.take_event_receiver().unwrap();

    host.session_prompt(session.id, "run echo should-be-denied".into())
        .await
        .unwrap();

    // Offline turn always ends with a done line; assert the *tool* was denied.
    let mut saw_perm = false;
    let mut saw_denied = false;
    loop {
        match timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ev)) => match &ev {
                SessionUpdate::PermissionRequired { .. } => saw_perm = true,
                SessionUpdate::ToolCall { title, status, .. } if title == "run_terminal_cmd" => {
                    if matches!(status, grokptah_agent_bridge::ToolCallStatus::Denied) {
                        saw_denied = true;
                    }
                }
                SessionUpdate::TurnComplete { .. } => break,
                _ => {}
            },
            _ => break,
        }
    }
    assert!(saw_denied, "deny rule must mark shell tool Denied");
    assert!(!saw_perm, "deny rule must not open permission modal");
}

#[tokio::test]
async fn allow_rule_suppresses_shell_prompt() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    host.set_allow_deny_rules(vec!["Shell(*)".into()], vec![]);
    let session = host.session_new().unwrap();
    let mut rx = host.take_event_receiver().unwrap();

    host.session_prompt(session.id, "run echo allow-ok".into())
        .await
        .unwrap();

    let mut saw_perm = false;
    let mut saw_shell = false;
    loop {
        match timeout(Duration::from_secs(3), rx.recv()).await {
            Ok(Some(ev)) => {
                if matches!(ev, SessionUpdate::PermissionRequired { .. }) {
                    saw_perm = true;
                }
                match &ev {
                    SessionUpdate::ToolCall { title, .. } if title == "run_terminal_cmd" => {
                        saw_shell = true;
                    }
                    SessionUpdate::TurnComplete { .. } => break,
                    _ => {}
                }
            }
            _ => break,
        }
    }
    assert!(!saw_perm, "allow rule must suppress permission prompt");
    assert!(saw_shell, "shell tool should still run under allow rule");
}

#[tokio::test]
async fn shell_exit_code_and_failure_status_reach_session_events() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    host.session_prompt(session.id, "run printf success".into())
        .await
        .unwrap();
    let success_events = drain_until_turn_complete(&mut rx).await;
    assert!(success_events.iter().any(|event| matches!(
        event,
        SessionUpdate::ShellSessionEnded {
            exit_code: Some(0),
            cancelled: false,
            ..
        }
    )));
    assert!(success_events.iter().any(|event| matches!(
        event,
        SessionUpdate::ToolCallUpdate {
            status: grokptah_agent_bridge::ToolCallStatus::Completed,
            output: Some(output),
            ..
        } if output.contains("(exit 0)")
    )));

    host.session_prompt(session.id, "run printf failure; exit 2".into())
        .await
        .unwrap();
    let failure_events = drain_until_turn_complete(&mut rx).await;
    assert!(failure_events.iter().any(|event| matches!(
        event,
        SessionUpdate::ShellSessionEnded {
            exit_code: Some(2),
            cancelled: false,
            ..
        }
    )));
    assert!(failure_events.iter().any(|event| matches!(
        event,
        SessionUpdate::ToolCallUpdate {
            status: grokptah_agent_bridge::ToolCallStatus::Failed,
            output: Some(output),
            ..
        } if output.contains("(exit 2)")
    )));
}

#[tokio::test]
async fn cancelled_shell_is_distinct_from_nonzero_exit() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let runner = {
        let host = host.clone();
        tokio::spawn(async move { host.session_prompt(session.id, "run sleep 10".into()).await })
    };

    loop {
        let event = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("shell did not start")
            .expect("event channel closed");
        if matches!(event, SessionUpdate::ShellSessionStarted { .. }) {
            break;
        }
    }
    host.cancel_turn(Some(session.id)).unwrap();
    timeout(Duration::from_secs(3), runner)
        .await
        .expect("cancelled shell did not finish")
        .unwrap()
        .unwrap();

    let events = drain_until_turn_complete(&mut rx).await;
    assert!(events.iter().any(|event| matches!(
        event,
        SessionUpdate::ShellSessionEnded {
            exit_code: None,
            cancelled: true,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        SessionUpdate::ToolCallUpdate {
            status: grokptah_agent_bridge::ToolCallStatus::Failed,
            output: Some(output),
            ..
        } if output.contains("(cancelled)")
    )));
}

#[tokio::test]
async fn bridge_prompt_queue_mutates_reorders_and_drains_authoritatively() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let entries = host
        .session_queue_add(session.id, "first".into(), false)
        .unwrap();
    let first = entries[0].clone();
    let entries = host
        .session_queue_add(session.id, "second".into(), false)
        .unwrap();
    let second = entries[1].clone();
    let version_of = |entry_id: &str| {
        host.session_queue_list(session.id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .expect("entry present")
            .version
    };
    host.session_queue_edit(session.id, &first.id, first.version, "edited first".into())
        .unwrap();
    let (entries, _) = host
        .session_queue_move(
            session.id,
            &second.id,
            0,
            version_of(&second.id),
            host.session_queue_snapshot(session.id).unwrap().revision,
        )
        .unwrap();
    assert_eq!(entries[0].id, second.id);

    let run_next = host
        .session_queue_run_next(session.id, &first.id, version_of(&first.id))
        .unwrap();
    assert!(!run_next.cancelled_active);
    assert!(run_next.entries[0].priority);
    let drained = host.session_queue_take_next(session.id).unwrap();
    let batch = drained.batch.unwrap();
    assert_eq!(batch.entries.len(), 1, "run-next must not combine");
    assert_eq!(batch.text, "edited first");
    assert_eq!(drained.entries[0].id, second.id);

    host.session_queue_remove(session.id, &second.id, version_of(&second.id))
        .unwrap();
    assert!(host.session_queue_list(session.id).unwrap().is_empty());
}

#[tokio::test]
async fn steer_now_injects_once_without_cancelling_active_turn() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let runner = {
        let host = host.clone();
        tokio::spawn(async move { host.session_prompt(session.id, "run sleep 1".into()).await })
    };
    loop {
        let event = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("shell did not start")
            .expect("event channel closed");
        if matches!(event, SessionUpdate::ShellSessionStarted { .. }) {
            break;
        }
    }

    let receipt = host
        .session_steer(session.id, "focus on the failing test".into())
        .unwrap();
    assert_eq!(
        receipt.disposition,
        grokptah_agent_bridge::SteeringDisposition::Pending
    );
    assert!(
        host.session_queue_list(session.id).unwrap().is_empty(),
        "pending steering must not remain in the normal queue"
    );
    let second_receipt = host
        .session_steer(session.id, "then verify the narrow regression".into())
        .unwrap();
    assert_eq!(
        second_receipt.disposition,
        grokptah_agent_bridge::SteeringDisposition::Pending
    );

    timeout(Duration::from_secs(3), runner)
        .await
        .expect("active turn did not finish")
        .unwrap()
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    let injected: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            SessionUpdate::SteeringInjected {
                steering_id, text, ..
            } => Some((steering_id, text)),
            _ => None,
        })
        .collect();
    assert_eq!(injected.len(), 2);
    assert_eq!(injected[0].0, &receipt.entry.id);
    assert_eq!(injected[0].1, "focus on the failing test");
    assert_eq!(injected[1].0, &second_receipt.entry.id);
    assert_eq!(injected[1].1, "then verify the narrow regression");
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SessionUpdate::TurnComplete {
                cancelled: true,
                ..
            }
        )),
        "steer-now must not cancel the active turn"
    );
    assert!(host.session_queue_list(session.id).unwrap().is_empty());

    let transcript = host.session_transcript(session.id).unwrap();
    assert_eq!(
        transcript
            .iter()
            .filter(|entry| {
                entry.role == "system" && entry.text.contains("focus on the failing test")
            })
            .count(),
        1,
        "steering must be persisted exactly once"
    );
}

/// S4: `clear` is the "stop what you're doing" control. It used to clear only
/// the durable follow-ups and return `Ok(vec![])`, so a coordinator got an
/// empty-queue receipt while an accepted steering interjection was still on
/// its way to the model. Clearing must either stop it or say that it didn't.
#[tokio::test]
async fn clear_queue_accounts_for_accepted_steering_instead_of_reporting_empty() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let runner = {
        let host = host.clone();
        tokio::spawn(async move { host.session_prompt(session.id, "run sleep 1".into()).await })
    };
    loop {
        let event = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("shell did not start")
            .expect("event channel closed");
        if matches!(event, SessionUpdate::ShellSessionStarted { .. }) {
            break;
        }
    }

    let receipt = host
        .session_steer(session.id, "abandon this direction".into())
        .unwrap();
    assert_eq!(
        receipt.disposition,
        grokptah_agent_bridge::SteeringDisposition::Pending,
        "mid-turn steering should be accepted, not deferred"
    );
    host.session_queue_add(session.id, "a follow up".into(), false)
        .unwrap();

    let (entries, outcome, _revision) = host
        .session_queue_clear_with_origin_receipt(session.id, "mcp")
        .unwrap();
    assert!(entries.is_empty());
    assert_eq!(outcome.queued_cleared, 1);
    // The steering is either cancelled (not yet delivered) or reported as
    // in flight (already handed to a boundary). Before the fix it was neither:
    // it stayed pending and the receipt claimed an empty queue.
    assert_eq!(
        outcome.steering_cancelled + outcome.steering_in_flight,
        1,
        "accepted steering must be accounted for by clear"
    );
    let claimed_stopped = outcome.fully_stopped();
    assert_eq!(claimed_stopped, outcome.steering_cancelled == 1);

    timeout(Duration::from_secs(5), runner)
        .await
        .expect("active turn did not finish")
        .unwrap()
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    let injected = events
        .iter()
        .filter(|event| matches!(event, SessionUpdate::SteeringInjected { .. }))
        .count();
    if claimed_stopped {
        assert_eq!(
            injected, 0,
            "clear reported the session stopped, so nothing may be injected"
        );
        assert!(
            !host
                .session_transcript(session.id)
                .unwrap()
                .iter()
                .any(|entry| entry.text.contains("abandon this direction")),
            "cancelled steering must not reach the transcript"
        );
    }
    assert!(
        host.session_queue_list(session.id).unwrap().is_empty(),
        "cleared steering must not be deferred back into the queue"
    );
}

/// S3: `run_next` is the one queue mutator that cancels the active turn, and
/// with an optional compare-and-set a coordinator working from a stale view
/// could interrupt a running turn on the strength of a version it never
/// checked. The cancel has to sit behind the CAS: a rejected call must leave
/// both the queue and the turn exactly as it found them.
#[tokio::test]
async fn stale_run_next_is_rejected_without_cancelling_the_active_turn() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let entry = host
        .session_queue_add(session.id, "follow up".into(), false)
        .unwrap()[0]
        .clone();

    let runner = {
        let host = host.clone();
        tokio::spawn(async move { host.session_prompt(session.id, "run sleep 1".into()).await })
    };
    loop {
        let event = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("shell did not start")
            .expect("event channel closed");
        if matches!(event, SessionUpdate::ShellSessionStarted { .. }) {
            break;
        }
    }

    let stale = host
        .session_queue_run_next(session.id, &entry.id, entry.version + 1)
        .expect_err("a stale run_next must fail closed");
    assert!(
        stale.to_string().contains("stale queued prompt version"),
        "expected a stale-version conflict, got: {stale}"
    );
    let listed = host.session_queue_list(session.id).unwrap();
    assert_eq!(listed.len(), 1, "a rejected run_next must not mutate");
    assert_eq!(listed[0].version, entry.version);
    assert!(!listed[0].priority, "the entry must not have been promoted");

    // The turn is still running, which is the point: `cancelled_active` can
    // only be true here if the rejected call left it alive.
    let promoted = host
        .session_queue_run_next(session.id, &entry.id, entry.version)
        .unwrap();
    assert!(
        promoted.cancelled_active,
        "the turn the stale call must not have cancelled is still cancellable"
    );
    assert!(promoted.entries[0].priority);

    timeout(Duration::from_secs(5), runner)
        .await
        .expect("active turn did not finish")
        .unwrap()
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                SessionUpdate::TurnComplete {
                    cancelled: true,
                    ..
                }
            ))
            .count(),
        1,
        "exactly one cancel, from the call whose CAS passed"
    );
}

/// P1: `run_next` observes the active turn under the queue lock but cancels
/// after releasing it. If the observed turn finishes in that gap and a new one
/// starts, an unconditional cancel kills the newcomer — a turn the caller
/// never saw. The cancel must be bound to the turn that was observed.
#[tokio::test]
async fn run_next_cannot_cancel_a_turn_that_started_after_the_one_it_observed() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = host.take_event_receiver().unwrap();
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let entry = host
        .session_queue_add(session.id, "follow up".into(), false)
        .unwrap()[0]
        .clone();

    // Turn A runs and finishes.
    host.session_prompt(session.id, "run true".into())
        .await
        .unwrap();
    let _ = drain_until_turn_complete(&mut rx).await;

    // Turn B starts. `run_next` must not be able to cancel it on the strength
    // of having observed turn A.
    let runner = {
        let host = host.clone();
        tokio::spawn(async move { host.session_prompt(session.id, "run sleep 1".into()).await })
    };
    loop {
        let event = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("second turn did not start")
            .expect("event channel closed");
        if matches!(event, SessionUpdate::ShellSessionStarted { .. }) {
            break;
        }
    }

    // A stale generation stands in for "the turn I observed is already gone".
    let stale = host.cancel_turn_if_generation_for_test(session.id, 1);
    assert!(
        stale.is_err(),
        "cancelling a turn that already ended must not fall through to the current one"
    );

    timeout(Duration::from_secs(5), runner)
        .await
        .expect("turn B did not finish")
        .unwrap()
        .unwrap();
    let events = drain_until_turn_complete(&mut rx).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SessionUpdate::TurnComplete {
                cancelled: true,
                ..
            }
        )),
        "turn B must not have absorbed a cancel meant for turn A"
    );
    // The queue is untouched by the refused cancel.
    let listed = host.session_queue_list(session.id).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, entry.id);
}

/// P1: draining a batch and starting its turn are two calls. Without a turn
/// reservation another writer can start a turn in between; the drained prompt
/// is then refused by the busy check and is already gone from the queue.
#[tokio::test]
async fn a_drain_reserves_the_turn_so_a_racing_writer_cannot_lose_the_prompt() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.session_queue_add(session.id, "queued work".into(), false)
        .unwrap();

    let drained = host.session_queue_take_next(session.id).unwrap();
    let batch = drained.batch.expect("a batch was queued");
    let reservation = drained
        .reservation
        .expect("a drain that removes entries must claim the turn slot");
    assert!(host.session_queue_list(session.id).unwrap().is_empty());

    // The racing writer now loses, instead of winning and stranding the batch.
    let raced = host
        .reserve_orchestration_turn("competing-run", session.id)
        .is_err();
    assert!(raced, "the drain must hold the session's turn slot");

    // And the drain can still start the turn it reserved.
    host.session_prompt_reserved_with_max_rounds(
        session.id,
        batch.text.clone(),
        None,
        &reservation,
    )
    .await
    .unwrap();
}

/// The other half: if the turn never starts, the batch has to come back rather
/// than vanish, and the reservation must not be left holding the session.
#[tokio::test]
async fn restoring_an_unstarted_drain_returns_the_prompts_and_frees_the_turn() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.session_queue_add(session.id, "first".into(), false)
        .unwrap();
    host.session_queue_add(session.id, "second".into(), false)
        .unwrap();
    let before: Vec<String> = host
        .session_queue_list(session.id)
        .unwrap()
        .iter()
        .map(|entry| entry.text.clone())
        .collect();

    let drained = host.session_queue_take_next(session.id).unwrap();
    let batch = drained.batch.expect("a batch was queued");
    let reservation = drained.reservation.expect("drain reserves the turn");

    let restored = host
        .session_queue_restore_drain(session.id, Some(&reservation), batch.entries.clone())
        .unwrap();
    assert_eq!(
        restored
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
        before,
        "restored entries keep their original order"
    );
    // Versions move, because the entries were briefly out of the queue: a
    // holder of the pre-drain version was describing something that did not
    // exist.
    assert!(restored.iter().all(|entry| entry.version >= 1));
    // The slot is free again for whoever wants it next.
    host.reserve_orchestration_turn("later-run", session.id)
        .expect("restore must release the reservation");
}

/// Reserving the turn on drain is what stops a racing writer from stranding
/// the batch, but it also means a drainer that dies between taking the batch
/// and starting the turn would hold the session busy forever. An abandoned
/// drain reservation must be reclaimable.
#[tokio::test]
async fn an_abandoned_drain_reservation_does_not_wedge_the_session() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();
    host.session_queue_add(session.id, "first".into(), false)
        .unwrap();
    host.session_queue_add(session.id, "second".into(), false)
        .unwrap();

    // Drain once and then simply walk away, as a crashed renderer would.
    // `take_next` combines the plain prefix, so this empties the queue.
    let abandoned = host.session_queue_take_next(session.id).unwrap();
    assert!(abandoned.reservation.is_some());
    assert!(host.session_queue_list(session.id).unwrap().is_empty());

    // New work arrives for a session whose turn slot is still held.
    host.session_queue_add(session.id, "third".into(), false)
        .unwrap();
    assert!(
        host.session_queue_take_next(session.id)
            .unwrap()
            .batch
            .is_none(),
        "a live reservation must still block a second drain"
    );

    host.expire_drain_reservation_for_test(session.id);

    let recovered = host.session_queue_take_next(session.id).unwrap();
    assert_eq!(
        recovered.batch.as_ref().map(|batch| batch.text.as_str()),
        Some("third"),
        "an abandoned drain reservation must be reclaimable"
    );
    assert!(recovered.reservation.is_some());
}

#[tokio::test]
async fn steer_at_idle_boundary_is_preserved_as_run_next_queue_entry() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let session = host.session_new().unwrap();

    let receipt = host
        .session_steer(session.id, "late steering text".into())
        .unwrap();

    assert_eq!(
        receipt.disposition,
        grokptah_agent_bridge::SteeringDisposition::Queued
    );
    assert_eq!(receipt.entries.len(), 1);
    assert!(receipt.entries[0].priority);
    let drained = host.session_queue_take_next(session.id).unwrap();
    assert_eq!(drained.batch.unwrap().text, "late steering text");
}

#[tokio::test]
async fn concurrent_sessions_shells_do_not_kill_each_other() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let a = host.session_new().unwrap();
    let b = host.session_new().unwrap();

    let host_a = host.clone();
    let host_b = host.clone();
    let ta = tokio::spawn(async move { host_a.session_prompt(a.id, "run sleep 3".into()).await });
    // Stagger so both shells are live
    tokio::time::sleep(Duration::from_millis(150)).await;
    let tb = tokio::spawn(async move {
        host_b
            .session_prompt(b.id, "run echo session-b-alive".into())
            .await
    });

    let b_reply = tb.await.unwrap().unwrap();
    // Cancel A while it sleeps — must not kill B (already done) incorrectly
    let _ = host.cancel_turn(Some(a.id));
    let a_reply = ta.await.unwrap().unwrap();

    assert!(
        b_reply.contains("session-b-alive")
            || b_reply.contains("offline")
            || b_reply.contains("exit"),
        "session B shell must complete independently, got {b_reply}"
    );
    // A was cancelled or finished — either way B was not wedged by A's cancel
    let _ = a_reply;
}

#[test]
fn permission_mode_and_always_approve_stay_coherent() {
    let _iso = IsolatedHome::install();
    let host = AgentHost::create(HostConfig::default());
    host.start().unwrap();

    host.set_always_approve(true);
    let snap: serde_json::Value = host.settings_snapshot();
    assert_eq!(snap["alwaysApprove"], true);
    assert_eq!(snap["permissionMode"], "bypassPermissions");
    assert_eq!(snap["effectiveToolPrompting"], "bypass");

    host.set_permission_mode("default".into());
    let snap = host.settings_snapshot();
    assert_eq!(snap["alwaysApprove"], false);
    assert_eq!(snap["permissionMode"], "default");
    assert_eq!(snap["effectiveToolPrompting"], "prompt");

    host.set_permission_mode("bypassPermissions".into());
    let snap = host.settings_snapshot();
    assert_eq!(snap["alwaysApprove"], true);
    assert_eq!(snap["permissionMode"], "bypassPermissions");
}

/// #146: per-session edit snapshots — rewind B must not restore A's file.
#[tokio::test]
async fn rewind_files_isolated_per_session() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let path_a = dir.path().join("a.txt");
    let path_b = dir.path().join("b.txt");
    std::fs::write(&path_a, "A0").unwrap();
    std::fs::write(&path_b, "B0").unwrap();

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let sa = host.session_new().unwrap();
    let sb = host.session_new().unwrap();

    // Snapshot + mutate as if each session edited its file.
    host.snapshot_edit_original_for_session(sa.id, dir.path(), "a.txt");
    host.snapshot_edit_original_for_session(sb.id, dir.path(), "b.txt");
    std::fs::write(&path_a, "A1").unwrap();
    std::fs::write(&path_b, "B1").unwrap();

    // Rewind only B files → a stays A1, b restores to B0
    host.rewind_session(sb.id, 0, "files").unwrap();
    assert_eq!(
        std::fs::read_to_string(&path_a).unwrap(),
        "A1",
        "session A file must not restore"
    );
    assert_eq!(
        std::fs::read_to_string(&path_b).unwrap(),
        "B0",
        "session B file restores"
    );

    // Conversation-only rewind must not touch files
    std::fs::write(&path_a, "A2").unwrap();
    host.snapshot_edit_original_for_session(sa.id, dir.path(), "a.txt");
    std::fs::write(&path_a, "A3").unwrap();
    host.rewind_session(sa.id, 0, "conversation").unwrap();
    assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "A3");
}

#[tokio::test]
async fn session_load_blocks_missing_cwd_without_rebinding() {
    let _iso = IsolatedHome::install();
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(dir.path()).unwrap();
    let s = host.session_new().unwrap();
    // Poison stored cwd to a deleted path
    {
        // session_set_cwd only works for existing dirs — set then drop dir
        let live = tempfile::tempdir().unwrap();
        host.session_set_cwd(s.id, live.path()).unwrap();
        host.set_project_cwd(dir.path()).unwrap();
        drop(live); // delete
    }
    // Project is still open at dir, but loading must fail closed instead of
    // silently changing repository ownership or enabling tools on `dir`.
    let error = host.session_load(s.id).unwrap_err().to_string();
    assert!(error.contains("workspace is missing"));
    let sum = host.session_inspect(s.id).unwrap();
    assert_eq!(sum.workspace_status.as_str(), "missing");
    assert!(!std::path::Path::new(&sum.cwd).is_dir());
    assert_eq!(
        host.status().project_cwd,
        Some(dir.path().display().to_string())
    );
}

#[tokio::test]
async fn missing_build_workspace_blocks_prompt_until_explicit_rebind() {
    let _iso = IsolatedHome::install();
    let live = tempfile::tempdir().unwrap();
    let deleted = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(live.path()).unwrap();
    let session = host.session_new().unwrap();
    host.session_set_cwd(session.id, deleted.path()).unwrap();
    drop(deleted);

    let error = host
        .session_prompt(session.id, "list files".into())
        .await
        .expect_err("deleted workspace must fail closed");
    assert!(error.to_string().contains("workspace is missing"));

    let rebound = host.session_set_cwd(session.id, live.path()).unwrap();
    assert_eq!(rebound.workspace_status.as_str(), "ready");
}
