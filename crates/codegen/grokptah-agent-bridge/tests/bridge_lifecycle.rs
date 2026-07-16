//! Integration tests drive the **shipped** AgentHostHandle API (not a reimplementation).

use std::time::Duration;

use grokptah_agent_bridge::{
    desktop_auto_update_enabled, AgentHost, HostConfig, PermissionDecision, SessionUpdate,
};
use tokio::time::timeout;

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
async fn session_lifecycle_prompt_streams_message_and_thought() {
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

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionUpdate::AgentThoughtChunk { .. })),
        "expected thought chunks, got {events:?}"
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
async fn permission_request_round_trip_allow() {
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

    let mut req_id = None;
    loop {
        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let SessionUpdate::PermissionRequired { request, .. } = ev {
            req_id = Some(request.id);
            break;
        }
    }
    let id = req_id.expect("permission id");
    host.permission_respond(id, PermissionDecision::Allow)
        .unwrap();

    prompt.await.unwrap().unwrap();

    let mut saw_tool = false;
    let mut saw_shell_started = false;
    let mut saw_complete = false;
    let mut saw_reattach_reexec = false;
    while let Ok(Some(ev)) = timeout(Duration::from_millis(800), rx.recv()).await {
        match &ev {
            SessionUpdate::ToolCall { title, .. } if title == "run_terminal_cmd" => {
                saw_tool = true;
            }
            SessionUpdate::ShellSessionStarted { command, .. } => {
                saw_shell_started = true;
                assert!(command.contains("echo"));
            }
            SessionUpdate::BackgroundTask {
                status, title, ..
            } if status == "attachable" && title.starts_with("terminal:") => {
                // This was the double-exec path — must never fire.
                saw_reattach_reexec = true;
            }
            SessionUpdate::TurnComplete { .. } => {
                saw_complete = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_tool, "shell tool should run after allow");
    assert!(
        saw_shell_started,
        "must emit ShellSessionStarted for live attach"
    );
    assert!(
        !saw_reattach_reexec,
        "must not emit attachable terminal: background task (re-exec)"
    );
    assert!(saw_complete);
}

#[tokio::test]
async fn permission_deny_skips_tool() {
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

    loop {
        let ev = timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let SessionUpdate::PermissionRequired { request, .. } = ev {
            host.permission_respond(request.id, PermissionDecision::Deny)
                .unwrap();
            break;
        }
    }
    prompt.await.unwrap().unwrap();

    let mut denied = false;
    while let Ok(Some(ev)) = timeout(Duration::from_millis(500), rx.recv()).await {
        if matches!(
            ev,
            SessionUpdate::ToolCall {
                status: grokptah_agent_bridge::ToolCallStatus::Denied,
                ..
            }
        ) {
            denied = true;
        }
        if matches!(ev, SessionUpdate::TurnComplete { .. }) {
            break;
        }
    }
    assert!(denied);
}

#[tokio::test]
async fn cancel_kills_real_shell_child_not_only_sleep_stub() {
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

    let cmd = format!(
        "sleep 30; echo done > '{}'",
        marker.display()
    );
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
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig::default());
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
    host.accept_plan(s.id).unwrap();
}
