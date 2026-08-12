//! End-to-end reference coordinator campaign over the real MCP HTTP server.
//!
//! The Node harness is intentionally protocol-level. This test supplies a
//! deterministic offline host and a disposable Git workspace so CI proves the
//! coordinator contract without network credentials.

use std::path::PathBuf;
use std::process::Command;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
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
    set_grokptah_home_override(Some(home.path().join(".grokptah")));

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    let service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "coordinator-campaign-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
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
        .env("GROKPTAH_MCP_WORKSPACE", workspace.path())
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
        "cursorReplayNoGaps",
        "busySteerNonCancelling",
        "explicitCancel",
        "isolatedRun",
        "boundedDiffReview",
        "scopedApproval",
        "scopedPromotion",
        "idleSteerQueues",
        "queueIdempotency",
        "sessionReconnectFailClosed",
        "durableReadAfterReconnect",
    ] {
        assert_eq!(
            report["checks"][check], true,
            "failed coordinator check {check}: {report}"
        );
    }
    assert!(
        workspace.path().join("coordinator-campaign.txt").is_file(),
        "promotion must apply the isolated file to the source workspace"
    );

    server.stop();
    set_grokptah_home_override(None);
    match previous_offline {
        Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
        None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
    }
}
