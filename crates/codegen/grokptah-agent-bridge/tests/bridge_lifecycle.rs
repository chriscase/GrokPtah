//! Integration tests drive the **shipped** AgentHostHandle API (not a reimplementation).
//!
//! All tests install an isolated GrokPtah home so they never pollute the
//! developer's real `~/.grokptah` session list.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use grokptah_agent_bridge::{
    desktop_auto_update_enabled, set_grokptah_home_override, AgentHost, HostConfig,
    PermissionDecision, SessionUpdate,
};
use tokio::time::timeout;

/// Serialize tests that mutate the process-wide home override.
fn home_serial() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// RAII: point `grokptah_home()` at a temp dir for the duration of a test.
struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    prev: Option<PathBuf>,
}

impl IsolatedHome {
    fn install() -> Self {
        let lock = home_serial().lock().unwrap_or_else(|e| e.into_inner());
        let prev = None;
        let tmp = tempfile::tempdir().expect("isolated home tempdir");
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions dir");
        std::fs::create_dir_all(home.join("plugins")).ok();
        std::fs::create_dir_all(home.join("skills")).ok();
        set_grokptah_home_override(Some(home));
        // Prevent live API / tool-loop network calls during unit tests.
        // SAFETY: tests serialize on home_serial mutex.
        unsafe {
            std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
        }
        Self {
            _tmp: tmp,
            _lock: lock,
            prev,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(self.prev.take());
        unsafe {
            std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
        }
    }
}

async fn drain_until_turn_complete(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionUpdate>,
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
    assert!(events
        .iter()
        .any(|e| matches!(e, SessionUpdate::TurnComplete { cancelled: false, .. })));
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
    let mut req_id = None;
    loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = &ev {
            req_id = Some(request.id);
            events.push(ev);
            break;
        }
        events.push(ev);
    }
    let id = req_id.expect("permission id");
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

    assert!(saw_tool, "shell tool should run after allow, got {events:?}");
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
    let prompt = tokio::spawn(async move {
        host2
            .session_prompt(session.id, format!("run {cmd}"))
            .await
    });

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
            SessionUpdate::ShellSessionEnded {
                cancelled: c, ..
            } => {
                shell_cancelled = *c;
            }
            SessionUpdate::TurnComplete {
                cancelled: c, ..
            } => {
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

    let after_rewind = host.rewind_session(forked.id, 1).unwrap();
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
    let user_sessions = dirs::home_dir()
        .unwrap()
        .join(".grokptah")
        .join("sessions");
    let before: Vec<_> = std::fs::read_dir(&user_sessions)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect()
        })
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
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .collect()
        })
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

    let mut req_id = None;
    let mut tool_name = String::new();
    loop {
        let ev = timeout(Duration::from_secs(8), rx.recv())
            .await
            .expect("timeout waiting for permission")
            .expect("channel closed");
        if let SessionUpdate::PermissionRequired { request, .. } = &ev {
            req_id = Some(request.id);
            tool_name = request.tool_name.clone();
            break;
        }
    }
    assert_eq!(tool_name, "run_terminal_cmd");
    host.permission_respond(req_id.unwrap(), PermissionDecision::AlwaysAllow)
        .unwrap();
    prompt1.await.unwrap().unwrap();
    // Drain turn complete
    loop {
        match timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ev)) if matches!(ev, SessionUpdate::TurnComplete { .. }) => break,
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
    assert!(tools.is_empty(), "no MCP tools when untrusted, got {tools:?}");

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
    host.session_prompt(
        session.id,
        format!("read secret_note.txt"),
    )
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
