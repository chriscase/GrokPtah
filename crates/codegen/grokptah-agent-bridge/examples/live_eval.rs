//! Live headless Build eval runner for GrokPtah bridge (#171).
//!
//! Usage (from bridge crate root):
//! ```sh
//! GROKPTAH_LIVE_EVAL=1 cargo run --example live_eval -- \
//!   --tasks ../../../evals/tasks.json \
//!   --fixtures ../../../evals/fixtures \
//!   --out /tmp/ptah-eval.json
//! ```
//!
//! Does **not** set `GROKPTAH_AGENT_OFFLINE` — requires real credentials
//! (`XAI_API_KEY` or `~/.grok/auth.json`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, HostConfig, SessionUpdate, ToolCallStatus,
};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    fixture: String,
    prompt: String,
    success: SuccessSpec,
    #[serde(default = "default_max_turns")]
    #[allow(dead_code)]
    max_turns: u32,
}

fn default_max_turns() -> u32 {
    12
}

#[derive(Debug, Deserialize)]
struct SuccessSpec {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    must_contain: Vec<String>,
    #[serde(default)]
    must_not_contain: Vec<String>,
    #[serde(default)]
    must_not_remove: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TaskResult {
    id: String,
    success: bool,
    wall_ms: u128,
    tool_calls: u32,
    tool_errors: u32,
    permission_prompts: u32,
    rounds_est: u32,
    error: Option<String>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct RunReport {
    side: String,
    model: String,
    started_at: String,
    results: Vec<TaskResult>,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("GROKPTAH_LIVE_EVAL").is_none() {
        bail!("set GROKPTAH_LIVE_EVAL=1 to run live evals (uses network + credentials)");
    }
    // Ensure offline flag is not set.
    unsafe {
        std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
    }

    let mut args = std::env::args().skip(1);
    let mut tasks_path = PathBuf::from("../../../evals/tasks.json");
    let mut fixtures_root = PathBuf::from("../../../evals/fixtures");
    let mut out_path = PathBuf::from("live_eval_ptah.json");
    let mut model = String::from("grok-build");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tasks" => tasks_path = PathBuf::from(args.next().context("--tasks value")?),
            "--fixtures" => fixtures_root = PathBuf::from(args.next().context("--fixtures value")?),
            "--out" => out_path = PathBuf::from(args.next().context("--out value")?),
            "--model" => model = args.next().context("--model value")?,
            other => bail!("unknown arg: {other}"),
        }
    }

    let tasks: Vec<Task> = serde_json::from_str(
        &fs::read_to_string(&tasks_path)
            .with_context(|| format!("read tasks {}", tasks_path.display()))?,
    )?;

    let home_tmp = tempfile::tempdir()?;
    let home = home_tmp.path().join(".grokptah");
    fs::create_dir_all(home.join("sessions"))?;
    set_grokptah_home_override(Some(home));

    let mut results = Vec::new();
    for task in &tasks {
        let r = run_one(task, &fixtures_root, &model).await;
        results.push(r);
    }

    set_grokptah_home_override(None);

    let report = RunReport {
        side: "grokptah-bridge".into(),
        model,
        started_at: chrono::Utc::now().to_rfc3339(),
        results,
    };
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&out_path, &json)?;
    println!("{json}");
    Ok(())
}

async fn run_one(task: &Task, fixtures_root: &Path, model: &str) -> TaskResult {
    let t0 = Instant::now();
    let fixture_src = fixtures_root.join(&task.fixture);
    let work = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            return TaskResult {
                id: task.id.clone(),
                success: false,
                wall_ms: t0.elapsed().as_millis(),
                tool_calls: 0,
                tool_errors: 0,
                permission_prompts: 0,
                rounds_est: 0,
                error: Some(e.to_string()),
                detail: "tempdir failed".into(),
            };
        }
    };
    if let Err(e) = copy_dir(&fixture_src, work.path()) {
        return TaskResult {
            id: task.id.clone(),
            success: false,
            wall_ms: t0.elapsed().as_millis(),
            tool_calls: 0,
            tool_errors: 0,
            permission_prompts: 0,
            rounds_est: 0,
            error: Some(e.to_string()),
            detail: "copy fixture failed".into(),
        };
    }

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = match host.take_event_receiver() {
        Some(r) => r,
        None => {
            return TaskResult {
                id: task.id.clone(),
                success: false,
                wall_ms: t0.elapsed().as_millis(),
                tool_calls: 0,
                tool_errors: 0,
                permission_prompts: 0,
                rounds_est: 0,
                error: Some("no event receiver".into()),
                detail: String::new(),
            };
        }
    };
    if let Err(e) = host.start() {
        return TaskResult {
            id: task.id.clone(),
            success: false,
            wall_ms: t0.elapsed().as_millis(),
            tool_calls: 0,
            tool_errors: 0,
            permission_prompts: 0,
            rounds_est: 0,
            error: Some(e.to_string()),
            detail: "start failed".into(),
        };
    }
    host.set_model(model.to_string());
    if let Err(e) = host.set_project_cwd(work.path()) {
        return TaskResult {
            id: task.id.clone(),
            success: false,
            wall_ms: t0.elapsed().as_millis(),
            tool_calls: 0,
            tool_errors: 0,
            permission_prompts: 0,
            rounds_est: 0,
            error: Some(e.to_string()),
            detail: "set_project_cwd failed".into(),
        };
    }
    let session = match host.session_new() {
        Ok(s) => s,
        Err(e) => {
            return TaskResult {
                id: task.id.clone(),
                success: false,
                wall_ms: t0.elapsed().as_millis(),
                tool_calls: 0,
                tool_errors: 0,
                permission_prompts: 0,
                rounds_est: 0,
                error: Some(e.to_string()),
                detail: "session_new failed".into(),
            };
        }
    };

    let prompt_fut = host.session_prompt(session.id, task.prompt.clone());
    let mut tool_calls = 0u32;
    let mut tool_errors = 0u32;
    let mut permission_prompts = 0u32;
    let mut rounds_est = 0u32;
    let mut err_msg = None;

    let drain = async {
        loop {
            match timeout(Duration::from_secs(180), rx.recv()).await {
                Ok(Some(SessionUpdate::TurnComplete { .. })) => break,
                Ok(Some(SessionUpdate::ToolCall { .. })) => tool_calls += 1,
                Ok(Some(SessionUpdate::ToolCallUpdate { status, .. })) => {
                    if matches!(status, ToolCallStatus::Failed | ToolCallStatus::Denied) {
                        tool_errors += 1;
                    }
                }
                Ok(Some(SessionUpdate::PermissionRequired { .. })) => {
                    permission_prompts += 1;
                }
                Ok(Some(SessionUpdate::AgentProgress { round, .. })) => {
                    rounds_est = rounds_est.max(round);
                }
                Ok(Some(SessionUpdate::Error { message, .. })) => {
                    err_msg = Some(message);
                }
                Ok(None) => break,
                Err(_) => {
                    err_msg = Some("timeout waiting for TurnComplete".into());
                    break;
                }
                _ => {}
            }
        }
    };

    let (prompt_res, _) = tokio::join!(prompt_fut, drain);
    if let Err(e) = prompt_res {
        err_msg = Some(e.to_string());
    }

    let success = check_success(work.path(), &task.success);
    TaskResult {
        id: task.id.clone(),
        success,
        wall_ms: t0.elapsed().as_millis(),
        tool_calls,
        tool_errors,
        permission_prompts,
        rounds_est,
        error: err_msg,
        detail: if success {
            "ok".into()
        } else {
            "success predicate failed".into()
        },
    }
}

fn check_success(root: &Path, spec: &SuccessSpec) -> bool {
    if spec.kind != "file_contains" {
        return false;
    }
    let path = root.join(&spec.path);
    let Ok(body) = fs::read_to_string(&path) else {
        return false;
    };
    for s in &spec.must_contain {
        if !body.contains(s) {
            return false;
        }
    }
    for s in &spec.must_not_contain {
        if body.contains(s) {
            return false;
        }
    }
    for s in &spec.must_not_remove {
        if !body.contains(s) {
            return false;
        }
    }
    true
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = entry.path().strip_prefix(src)?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
