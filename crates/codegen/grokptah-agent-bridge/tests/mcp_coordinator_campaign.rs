//! End-to-end reference coordinator campaign over the real MCP HTTP server.
//!
//! The Node harness is intentionally protocol-level. This test supplies a
//! deterministic offline host and a disposable Git workspace so CI proves the
//! coordinator contract without network credentials.

use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost, HostConfig,
    SessionKind,
};
use tempfile::tempdir;

fn git(args: &[&str], cwd: &std::path::Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git must be installed for coordinator campaign");
    assert!(status.success(), "git {:?} failed", args);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn reference_coordinator_campaign_is_protocol_complete() {
    let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let _lock = home_override_serial();
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    std::fs::write(
        workspace.path().join("README.md"),
        "coordinator campaign baseline\n",
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
    let discard_workspace = tempdir().unwrap();
    std::fs::write(
        discard_workspace.path().join("README.md"),
        "discard campaign baseline\n",
    )
    .unwrap();
    git(&["init"], discard_workspace.path());
    git(&["add", "README.md"], discard_workspace.path());
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
        discard_workspace.path(),
    );
    set_grokptah_home_override(Some(home.path().join(".grokptah")));

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    let other_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other_session.id, workspace.path())
        .unwrap();
    let discard_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(discard_session.id, discard_workspace.path())
        .unwrap();
    let interrupted_run_id = "coordinator-interrupted-source";
    let store = OrchStore::open(home.path().join("orch")).unwrap();
    service_store_seed(&store, interrupted_run_id, session.id, workspace.path());
    let service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "coordinator-campaign-token".into(),
            allowlist: WorkspaceAllowlist::new([
                workspace.path().to_path_buf(),
                discard_workspace.path().to_path_buf(),
            ]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(service, 0).await.unwrap();
    let harness = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/mcp_sdk_interop/run_coordinator_campaign.mjs");
    assert!(harness.is_file(), "reference coordinator harness missing");
    let output = tokio::process::Command::new("node")
        .arg(harness)
        .env("GROKPTAH_MCP_URL", format!("http://{}/mcp", server.addr))
        .env("GROKPTAH_MCP_TOKEN", "coordinator-campaign-token")
        .env("GROKPTAH_MCP_SESSION_ID", session.id.to_string())
        .env(
            "GROKPTAH_MCP_OTHER_SESSION_ID",
            other_session.id.to_string(),
        )
        .env(
            "GROKPTAH_MCP_DISCARD_SESSION_ID",
            discard_session.id.to_string(),
        )
        .env("GROKPTAH_MCP_WORKSPACE", workspace.path())
        .env("GROKPTAH_MCP_DISCARD_WORKSPACE", discard_workspace.path())
        .env("GROKPTAH_MCP_INTERRUPTED_RUN_ID", interrupted_run_id)
        .output()
        .await
        .expect("spawn reference coordinator harness");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "coordinator campaign failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("invalid coordinator report: {error}\n{stdout}"));
    assert_eq!(report["ok"], true, "coordinator report: {report}");
    for check in [
        "boundedToolCatalog",
        "sharedSubmit",
        "submitIdempotency",
        "workspaceMismatchFailClosed",
        "cursorReplayNoGaps",
        "busySteerNonCancelling",
        "crossSessionRunOwnershipFailClosed",
        "crossWorkspaceRunOwnershipFailClosed",
        "explicitCancel",
        "isolatedRun",
        "boundedDiffReview",
        "scopedApproval",
        "staleApprovalFailClosed",
        "approvalScopeConflictFailClosed",
        "scopedPromotion",
        "isolatedDiscard",
        "idleSteerQueues",
        "queueIdempotency",
        "sessionReconnectFailClosed",
        "durableReadAfterReconnect",
        "explicitRestartRetry",
    ] {
        assert_eq!(
            report["checks"][check], true,
            "failed coordinator check {check}: {report}"
        );
    }
    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}

fn service_store_seed(
    store: &OrchStore,
    run_id: &str,
    session_id: uuid::Uuid,
    workspace: &std::path::Path,
) {
    store
        .save_run(&RunRecord {
            run_id: run_id.into(),
            session_id,
            workspace: dunce::canonicalize(workspace)
                .unwrap()
                .display()
                .to_string(),
            request_id: "coordinator-interrupted-request".into(),
            client_id: Some("mcp".into()),
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
            prompt_preview: "previous coordinator attempt".into(),
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
}
