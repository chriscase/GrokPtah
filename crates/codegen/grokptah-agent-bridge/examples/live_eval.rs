//! Live headless Build eval runner for GrokPtah bridge.
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
use grokptah_agent_bridge::eval_oracle::{self, SuccessSpec};
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
    max_turns: u32,
    #[serde(default)]
    difficulty: Option<String>,
}

fn default_max_turns() -> u32 {
    12
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
    difficulty: Option<String>,
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
            return fail_early(task, t0, e.to_string(), "tempdir failed");
        }
    };
    if let Err(e) = copy_dir(&fixture_src, work.path()) {
        return fail_early(task, t0, e.to_string(), "copy fixture failed");
    }

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    let mut rx = match host.take_event_receiver() {
        Some(r) => r,
        None => {
            return fail_early(task, t0, "no event receiver".into(), String::new());
        }
    };
    if let Err(e) = host.start() {
        return fail_early(task, t0, e.to_string(), "start failed");
    }
    host.set_model(model.to_string());
    if let Err(e) = host.set_project_cwd(work.path()) {
        return fail_early(task, t0, e.to_string(), "set_project_cwd failed");
    }
    let session = match host.session_new() {
        Ok(s) => s,
        Err(e) => {
            return fail_early(task, t0, e.to_string(), "session_new failed");
        }
    };

    let prompt_fut = host.session_prompt(session.id, task.prompt.clone());
    let mut tool_calls = 0u32;
    let mut tool_errors = 0u32;
    let mut permission_prompts = 0u32;
    let mut rounds_est = 0u32;
    let mut err_msg = None;
    let max_turns = task.max_turns.max(1);
    let session_id = session.id;

    let drain = async {
        loop {
            match timeout(Duration::from_secs(300), rx.recv()).await {
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
                    // Enforce task max_turns (parity with CLI --max-turns).
                    // Round is 1-based model step index emitted at step *start*;
                    // allow rounds 1..=max_turns, cancel only when a step beyond budget begins.
                    if round > max_turns {
                        let _ = host.cancel_turn(Some(session_id));
                        err_msg = Some(format!("max turns reached ({max_turns})"));
                        break;
                    }
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
        if err_msg.is_none() {
            err_msg = Some(e.to_string());
        }
    }

    let oracle = eval_oracle::evaluate(work.path(), &task.success);
    let mut detail = oracle.detail.clone();
    if !oracle.ok && std::env::var_os("GROKPTAH_EVAL_KEEP_FAIL").is_some() {
        let dump = std::env::temp_dir().join(format!("grokptah-eval-fail-{}", task.id));
        let _ = fs::remove_dir_all(&dump);
        let _ = copy_dir(work.path(), &dump);
        detail = format!("{detail}; dump={}", dump.display());
        eprintln!("eval fail dump: {}", dump.display());
    }
    TaskResult {
        id: task.id.clone(),
        success: oracle.ok,
        wall_ms: t0.elapsed().as_millis(),
        tool_calls,
        tool_errors,
        permission_prompts,
        rounds_est,
        error: err_msg,
        detail,
        difficulty: task.difficulty.clone(),
    }
}

fn fail_early(task: &Task, t0: Instant, error: String, detail: impl Into<String>) -> TaskResult {
    TaskResult {
        id: task.id.clone(),
        success: false,
        wall_ms: t0.elapsed().as_millis(),
        tool_calls: 0,
        tool_errors: 0,
        permission_prompts: 0,
        rounds_est: 0,
        error: Some(error),
        detail: detail.into(),
        difficulty: task.difficulty.clone(),
    }
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
