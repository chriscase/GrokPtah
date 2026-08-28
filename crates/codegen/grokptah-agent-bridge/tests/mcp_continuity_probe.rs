//! Evidence-first continuity probe over the public MCP HTTP contract (#296).
//!
//! The coordinator remains protocol-level: all orchestration actions happen in
//! the Node harness over Streamable HTTP. The Rust test only supplies a
//! deterministic offline host, disposable Git workspaces, and one controlled
//! event-bus flood so subscriber-gap recovery is reproducible without adding a
//! production hook.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost, HostConfig,
    SessionKind, SessionUpdate,
};
use tempfile::tempdir;

fn git(args: &[&str], cwd: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git must be installed for the continuity probe");
    assert!(status.success(), "git {:?} failed", args);
}

fn seed_run(
    store: &OrchStore,
    run_id: &str,
    session_id: uuid::Uuid,
    workspace: &Path,
    state: RunState,
) {
    store
        .save_run(&RunRecord {
            run_id: run_id.into(),
            session_id,
            workspace: dunce::canonicalize(workspace)
                .unwrap()
                .display()
                .to_string(),
            request_id: format!("{run_id}-request"),
            client_id: Some("mcp".into()),
            state,
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
                max_rounds: 8,
                max_duration_ms: 30_000,
                max_total_tokens: None,
            },
            prompt_preview: format!("seeded {state:?} continuity fixture"),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: state.is_terminal().then(|| match state {
                RunState::Interrupted => "interrupted".into(),
                RunState::Completed => "completed".into(),
                RunState::Failed => "failed".into(),
                RunState::Cancelled => "cancelled".into(),
                RunState::LimitReached => "limit_reached".into(),
                RunState::Queued | RunState::Running => "incomplete".into(),
            }),
            final_response: None,
            error_code: (state == RunState::Interrupted).then(|| "interrupted".into()),
            stop_cause: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();
}

async fn wait_for_file(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "continuity harness did not create readiness marker {}",
        path.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn continuity_probe_is_evidence_first_and_recoverable() {
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let _lock = home_override_serial();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    std::fs::write(
        workspace.path().join("README.md"),
        "continuity probe baseline\n",
    )
    .unwrap();
    git(&["init"], workspace.path());
    git(&["add", "README.md"], workspace.path());
    git(
        &[
            "-c",
            "user.name=GrokPtah Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "baseline",
        ],
        workspace.path(),
    );
    set_grokptah_home_override(Some(home.path().join(".grokptah")));

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        event_bus_capacity: Some(256),
        ..HostConfig::default()
    });
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    let gap_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(gap_session.id, workspace.path())
        .unwrap();
    let gap_start_seq = host.event_bus().current_seq().saturating_add(1);
    let service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "continuity-probe-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let interrupted_run_id = "continuity-interrupted-source";
    let gap_run_id = "continuity-gap-source";
    seed_run(
        service.store(),
        interrupted_run_id,
        session.id,
        workspace.path(),
        RunState::Interrupted,
    );
    seed_run(
        service.store(),
        gap_run_id,
        gap_session.id,
        workspace.path(),
        RunState::Running,
    );
    service
        .store()
        .update_run(gap_run_id, |run| {
            run.start_seq = Some(gap_start_seq);
            Ok(())
        })
        .unwrap();
    let server = start_control_server(service, 0).await.unwrap();
    let ready_file = home.path().join("gap-ready");
    let release_file = home.path().join("gap-release");
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/mcp_sdk_interop/run_continuity_probe.mjs");
    assert!(harness.is_file(), "continuity probe harness missing");

    let child = tokio::process::Command::new("node")
        .arg(harness)
        .env("GROKPTAH_MCP_URL", format!("http://{}/mcp", server.addr))
        .env("GROKPTAH_MCP_TOKEN", "continuity-probe-token")
        .env("GROKPTAH_MCP_SESSION_ID", session.id.to_string())
        .env("GROKPTAH_MCP_GAP_SESSION_ID", gap_session.id.to_string())
        .env("GROKPTAH_MCP_WORKSPACE", workspace.path())
        .env("GROKPTAH_MCP_INTERRUPTED_RUN_ID", interrupted_run_id)
        .env("GROKPTAH_MCP_GAP_RUN_ID", gap_run_id)
        .env("GROKPTAH_MCP_GAP_READY_FILE", &ready_file)
        .env("GROKPTAH_MCP_GAP_RELEASE_FILE", &release_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn continuity probe harness");

    // The harness has opened the run-scoped GET and intentionally holds its
    // body unread. Flooding the bounded broadcast subscriber now forces the
    // server to emit the explicit recovery notification when the client
    // resumes reading.
    wait_for_file(&ready_file).await;
    // Fill the bounded live subscriber without overwhelming the durable
    // writer. The probe needs a subscriber lag, not a persistence gap: the
    // latter is a distinct, correctly fail-closed condition that would make
    // the follow-up durable read return cursor_expired.
    for index in 0..2_048 {
        host.event_bus().publish(SessionUpdate::AgentMessageChunk {
            session_id: gap_session.id,
            text: format!("continuity-gap-{index}"),
        });
        if index % 8 == 7 {
            // Keep the persistence writer ahead of its small queue while the
            // unread HTTP body continues to accumulate broadcast messages.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    // Let the durable writer drain before releasing the unread broadcast
    // subscriber. This separates live-subscriber recovery from a
    // durable-journal persistence failure.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(&release_file, "release").unwrap();

    let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
        .await
        .expect("continuity probe harness timed out")
        .expect("wait for continuity probe harness");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "continuity probe failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("invalid continuity report: {error}\n{stdout}"));
    assert_eq!(report["ok"], true, "continuity report: {report}");
    for check in [
        "chainHasFiveRuns",
        "durableDerivationChain",
        "promptHashesRechecked",
        "explicitInterruptedTerminal",
        "retryCreatesFreshRun",
        "freshSubmitAfterRetry",
        "mutationsReplayByRequestId",
        "isolatedReadyBeforeReview",
        "explicitReviewApprovePromote",
        "noAutoPromotion",
        "dropReconnectsByLastEventId",
        "gapNoticeObserved",
        "durableReadReconcilesGap",
    ] {
        assert_eq!(
            report["checks"][check], true,
            "failed continuity check {check}: {report}"
        );
    }

    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}
