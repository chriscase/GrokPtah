//! Bounded soak + failure-injection campaign against desktop control bootstrap.
//! Drives `start_control_from_env` (production entry) + independent Node TCP client.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_from_env,
    start_control_server_with, AgentHost, ControlServerLimits, HostConfig, SessionKind,
};
use serde_json::json;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn soak_desktop_bootstrap_node_campaign() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let _guard = home_override_serial();
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    // Dummy git workspace (harmless).
    let _ = Command::new("git")
        .args(["init"])
        .current_dir(ws.path())
        .status();
    std::fs::write(ws.path().join("README.md"), "soak fixture\n").unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();

    // Multiple Build sessions for multi-session matrix.
    let mut session_ids = Vec::new();
    for _ in 0..3 {
        let s = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(s.id, ws.path()).unwrap();
        session_ids.push(s.id.to_string());
    }

    let token = format!("soak-token-{}", uuid::Uuid::new_v4());
    let prev_token = std::env::var("GROKPTAH_CONTROL_TOKEN").ok();
    let prev_port = std::env::var("GROKPTAH_CONTROL_PORT").ok();
    let prev_ws = std::env::var("GROKPTAH_CONTROL_WORKSPACES").ok();
    std::env::set_var("GROKPTAH_CONTROL_TOKEN", &token);
    std::env::set_var("GROKPTAH_CONTROL_PORT", "0");
    std::env::set_var("GROKPTAH_CONTROL_WORKSPACES", ws.path().as_os_str());

    let wall0 = Instant::now();
    let srv = start_control_from_env(host.clone())
        .await
        .expect("desktop bootstrap must start");
    assert!(srv.addr.ip().is_loopback());
    assert_eq!(srv.token, token);

    // Journal path for size sampling.
    let orch_dir = home.path().join(".grokptah").join("orchestration");
    let journal_before = dir_size_bytes(&orch_dir);

    let sdk_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop");
    if !sdk_dir
        .join("node_modules/@modelcontextprotocol/sdk")
        .is_dir()
    {
        let st = tokio::process::Command::new("npm")
            .args(["install", "--no-fund", "--no-audit"])
            .current_dir(&sdk_dir)
            .status()
            .await
            .expect("npm install");
        assert!(st.success());
    }

    // Resource sample of parent test process around soak.
    let rss_before = process_rss_kb();

    let url = format!("http://{}/mcp", srv.addr);
    let output = tokio::process::Command::new("node")
        .arg(sdk_dir.join("run_soak.mjs"))
        .env("GROKPTAH_MCP_URL", &url)
        .env("GROKPTAH_MCP_TOKEN", &token)
        .env("GROKPTAH_MCP_WORKSPACE", ws.path().display().to_string())
        .env("GROKPTAH_MCP_SESSION_IDS", session_ids.join(","))
        .env("GROKPTAH_SOAK_SECONDS", "20")
        .env("GROKPTAH_SOAK_CONCURRENCY", "6")
        .output()
        .await
        .expect("spawn soak");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("MCP_SOAK_STDERR {stderr}");
    assert!(
        output.status.success(),
        "soak campaign failed\nstdout={stdout}\nstderr={stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["ok"], true, "soak report={report}");
    if let Some(failed) = report["failed"].as_array() {
        assert!(failed.is_empty(), "failed cells: {failed:?}");
    }
    eprintln!("MCP_SOAK_REPORT {report}");

    let rss_after = process_rss_kb();
    let journal_after = dir_size_bytes(&orch_dir);
    let wall_ms = wall0.elapsed().as_millis();
    let metrics = &report["metrics"];
    eprintln!(
        "MCP_SOAK_RESOURCES {}",
        json!({
            "wallMs": wall_ms,
            "rssBeforeKb": rss_before,
            "rssAfterKb": rss_after,
            "journalBytesBefore": journal_before,
            "journalBytesAfter": journal_after,
            "nodeWallMs": metrics["wallMs"],
            "requests": metrics["requests"],
            "capacity429": metrics["capacity429"],
            "mcpSessionsOpened": metrics["mcpSessionsOpened"],
            "submits": metrics["submits"],
            "samples": metrics["samples"],
        })
    );

    // Hard requirements from matrix present in checks.
    for key in [
        "loopbackOnlyBind",
        "missingToken",
        "wrongToken",
        "authBeforeBody",
        "unknownTool",
        "symlinkEscapeFailClosed",
        "pathTraversalFailClosed",
        "multiSessionList",
        "multiSessionSubmit",
        "queueIdempotent",
        "disconnectFullBodyIdempotent",
        "staleSessionFailClosed",
        "reconnect",
        "eventOrdering",
        "changesShape",
        "testResultsShape",
        "sustainedPolling",
    ] {
        assert_eq!(
            report["checks"][key], true,
            "required matrix cell {key} not true: {report}"
        );
    }

    srv.stop();
    restore_env("GROKPTAH_CONTROL_TOKEN", prev_token);
    restore_env("GROKPTAH_CONTROL_PORT", prev_port);
    restore_env("GROKPTAH_CONTROL_WORKSPACES", prev_ws);
    set_grokptah_home_override(None);
}

/// Restart recovery: unfinished runs become interrupted; queue survives host restart.
#[test]
#[allow(clippy::await_holding_lock)]
fn soak_restart_recovery_running_queued_interrupted() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let _guard = home_override_serial();
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let store_path = home.path().join(".grokptah").join("orchestration");

    let session_id = {
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        host.start().unwrap();
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.session_queue_add(session.id, "queued across restart".into(), false)
            .unwrap();
        session.id
    };

    // Create store dirs, then plant a durable Running run without a live worker.
    {
        let _bootstrap = OrchStore::open(&store_path).unwrap();
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let run_path = store_path.join("runs").join(format!("{run_id}.json"));
    let planted = json!({
        "runId": run_id,
        "sessionId": session_id,
        "workspace": ws.path().display().to_string(),
        "requestId": "restart-synth-1",
        "clientId": null,
        "state": "running",
        "bounds": {
            "maxPromptBytes": 100000,
            "maxRounds": 24,
            "maxDurationMs": 600000
        },
        "promptPreview": "crashed mid-run",
        "startSeq": 1,
        "endSeq": null,
        "createdAt": chrono::Utc::now().to_rfc3339(),
        "updatedAt": chrono::Utc::now().to_rfc3339(),
        "terminalResult": null,
        "finalResponse": null,
        "errorCode": null,
        "aggregates": {"changes": [], "tests": []},
        "progress": null
    });
    std::fs::write(&run_path, planted.to_string()).unwrap();

    // Production crash recovery: open marks unfinished runs interrupted.
    let reopened = OrchStore::open(&store_path).unwrap();
    let loaded = reopened.load_run(&run_id).unwrap().expect("run record");
    assert_eq!(
        loaded.state,
        RunState::Interrupted,
        "running run must become interrupted on store reopen"
    );
    assert_eq!(loaded.terminal_result.as_deref(), Some("interrupted"));
    drop(reopened);

    let host2 = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host2.start().unwrap();
    let queued = host2.session_queue_list(session_id).unwrap();
    assert!(
        !queued.is_empty(),
        "queued prompts must survive host restart"
    );
    assert_eq!(queued[0].text, "queued across restart");

    set_grokptah_home_override(None);
}

/// Transport-level 429 under held concurrency (production path + lowered limits).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::await_holding_lock)]
async fn soak_mcp_request_capacity_429() {
    let _guard = home_override_serial();
    let home = tempdir().unwrap();
    let ws = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "cap-tok".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let limits = ControlServerLimits {
        max_concurrent: 2,
        request_timeout: Duration::from_secs(5),
        inject_work_delay: Some(Duration::from_millis(600)),
    };
    let srv = start_control_server_with(orch, 0, limits).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let http = reqwest::Client::new();
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
    let a = {
        let http = http.clone();
        let url = url.clone();
        let body = body.clone();
        tokio::spawn(async move {
            http.post(url)
                .header("Authorization", "Bearer cap-tok")
                .json(&body)
                .send()
                .await
        })
    };
    let b = {
        let http = http.clone();
        let url = url.clone();
        let body = body.clone();
        tokio::spawn(async move {
            http.post(url)
                .header("Authorization", "Bearer cap-tok")
                .json(&body)
                .send()
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    let overflow = http
        .post(&url)
        .header("Authorization", "Bearer cap-tok")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(overflow.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let _ = a.await;
    let _ = b.await;
    srv.stop();
    set_grokptah_home_override(None);
}

/// Structural: soak harness file exists and desktop bootstrap still shared.
#[test]
fn soak_harness_and_desktop_bootstrap_present() {
    let soak = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_sdk_interop/run_soak.mjs");
    assert!(soak.is_file(), "run_soak.mjs must exist");
    let body = std::fs::read_to_string(&soak).unwrap();
    assert!(body.contains("disconnectFullBodyIdempotent") || body.contains("disconnectFullBody"));
    assert!(body.contains("symlinkEscapeFailClosed"));
    assert!(body.contains("multiSessionSubmit"));
    let desktop =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/src-tauri/src/lib.rs");
    let dsrc = std::fs::read_to_string(desktop).unwrap();
    assert!(dsrc.contains("start_control_from_env"));
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn dir_size_bytes(path: &std::path::Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0u64;
    let walker = walkdir_simple(path);
    for p in walker {
        if let Ok(md) = std::fs::metadata(&p) {
            if md.is_file() {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

fn walkdir_simple(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    stack.push(e.path());
                }
            }
        } else {
            out.push(p);
        }
    }
    out
}

fn process_rss_kb() -> Option<u64> {
    let pid = std::process::id();
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.parse().ok()
}
