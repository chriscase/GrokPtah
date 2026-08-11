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

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::eval_oracle::{self, SuccessSpec};
use grokptah_agent_bridge::eval_report::{
    diff_workspace, path_is_within_workspace, report_path, snapshot_workspace, summarize_handoff,
    HandoffEvidence, WorkspaceChangeSummary,
};
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

#[derive(Debug, Serialize, Default)]
struct EventEvidence {
    turn_complete_seen: bool,
    cancelled: bool,
    assistant_chunks: u32,
    assistant_chars: usize,
    thought_chunks: u32,
    thought_chars: usize,
    file_edits: Vec<String>,
    error_count: u32,
}

#[derive(Debug, Serialize, Default)]
struct VerificationEvidence {
    test_commands: u32,
    tests_passed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct TaskResult {
    id: String,
    /// Structured oracle result only; retained for parity compatibility.
    success: bool,
    /// Oracle success plus terminal completion, no failed test command, and no
    /// safety finding. This is the stricter operator-facing quality signal.
    verified: bool,
    wall_ms: u128,
    tool_calls: u32,
    tool_errors: u32,
    permission_prompts: u32,
    rounds_est: u32,
    error: Option<String>,
    detail: String,
    difficulty: Option<String>,
    /// Ordered tool titles seen during the turn (#187/#188 instrumentation).
    tool_names: Vec<String>,
    /// Whether any shell tool looked like cargo test.
    cargo_test_ran: bool,
    /// Model round when cargo test was first observed (if any).
    cargo_test_first_round: Option<u32>,
    completion: String,
    incomplete: bool,
    events: EventEvidence,
    verification: VerificationEvidence,
    handoff: HandoffEvidence,
    changes: WorkspaceChangeSummary,
    failure_reasons: Vec<String>,
    quality_findings: Vec<String>,
    safety_violations: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RunReport {
    schema_version: u32,
    side: String,
    model: String,
    started_at: String,
    finished_at: String,
    summary: RunSummary,
    results: Vec<TaskResult>,
}

#[derive(Debug, Serialize, Default)]
struct RunSummary {
    tasks: u32,
    oracle_passed: u32,
    verified: u32,
    incomplete: u32,
    safety_findings: u32,
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

    let started_at = chrono::Utc::now().to_rfc3339();
    let mut results = Vec::new();
    for task in &tasks {
        let r = run_one(task, &fixtures_root, &model).await;
        results.push(r);
    }

    set_grokptah_home_override(None);

    let summary = RunSummary {
        tasks: results.len() as u32,
        oracle_passed: results.iter().filter(|result| result.success).count() as u32,
        verified: results.iter().filter(|result| result.verified).count() as u32,
        incomplete: results.iter().filter(|result| result.incomplete).count() as u32,
        safety_findings: results
            .iter()
            .map(|result| result.safety_violations.len() as u32)
            .sum(),
    };
    let report = RunReport {
        schema_version: 3,
        side: "grokptah-bridge".into(),
        model,
        started_at,
        finished_at: chrono::Utc::now().to_rfc3339(),
        summary,
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
    let before = match snapshot_workspace(work.path()) {
        Ok(snapshot) => snapshot,
        Err(error) => return fail_early(task, t0, error, "workspace snapshot failed"),
    };

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        // Align host loop budget with task max_turns so the model sees real scarcity
        // (CLI --max-turns) instead of the default 24-step ceiling (#187/#188).
        max_agent_rounds: Some(task.max_turns.max(1)),
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
    let mut tool_names: Vec<String> = Vec::new();
    let mut cargo_test_ran = false;
    let mut cargo_test_first_round: Option<u32> = None;
    let mut events = EventEvidence::default();
    let mut verification = VerificationEvidence::default();
    let mut cargo_test_call_ids = HashSet::new();
    let mut safety_violations = Vec::new();
    let max_turns = task.max_turns.max(1);
    let mut advertised_max_rounds = max_turns;
    let session_id = session.id;

    let drain = async {
        loop {
            match timeout(Duration::from_secs(300), rx.recv()).await {
                Ok(Some(SessionUpdate::AgentMessageChunk { text, .. })) => {
                    events.assistant_chunks += 1;
                    events.assistant_chars += text.chars().count();
                }
                Ok(Some(SessionUpdate::AgentThoughtChunk { text, .. })) => {
                    events.thought_chunks += 1;
                    events.thought_chars += text.chars().count();
                }
                Ok(Some(SessionUpdate::TurnComplete { cancelled, .. })) => {
                    events.turn_complete_seen = true;
                    events.cancelled |= cancelled;
                    break;
                }
                Ok(Some(SessionUpdate::ToolCall {
                    call_id,
                    title,
                    input,
                    ..
                })) => {
                    tool_calls += 1;
                    tool_names.push(title.clone());
                    if looks_like_cargo_test(&title, &input) {
                        cargo_test_call_ids.insert(call_id);
                        verification.test_commands += 1;
                        cargo_test_ran = true;
                        if cargo_test_first_round.is_none() {
                            cargo_test_first_round = Some(rounds_est.max(1));
                        }
                    }
                }
                Ok(Some(SessionUpdate::ToolCallUpdate {
                    call_id,
                    status,
                    output,
                    ..
                })) => {
                    if matches!(status, ToolCallStatus::Failed | ToolCallStatus::Denied) {
                        tool_errors += 1;
                    }
                    if cargo_test_call_ids.contains(&call_id) {
                        observe_test_update(&mut verification, status, output.as_deref());
                    }
                }
                Ok(Some(SessionUpdate::PermissionRequired { .. })) => {
                    permission_prompts += 1;
                }
                Ok(Some(SessionUpdate::FileEdit { path, .. })) => {
                    if !path_is_within_workspace(work.path(), &path) {
                        safety_violations.push("file_edit_outside_workspace".into());
                    }
                    events.file_edits.push(report_path(work.path(), &path));
                }
                Ok(Some(SessionUpdate::AgentProgress {
                    round,
                    max_rounds,
                    last_tool,
                    detail,
                    ..
                })) => {
                    rounds_est = rounds_est.max(round);
                    advertised_max_rounds = advertised_max_rounds.max(max_rounds);
                    if let Some(t) = last_tool {
                        if t == "run_terminal_cmd" && detail.to_ascii_lowercase().contains("cargo")
                        {
                            cargo_test_ran = true;
                            if cargo_test_first_round.is_none() {
                                cargo_test_first_round = Some(round);
                            }
                        }
                    }
                    // Enforce task max_turns while honoring one explicitly
                    // advertised recovery boundary from the bridge. The host
                    // reports max_rounds=max_turns+1 only for that bounded step;
                    // never let a stale or inflated report extend the harness
                    // beyond one extra model boundary.
                    if !progress_within_turn_budget(round, max_turns, advertised_max_rounds) {
                        let _ = host.cancel_turn(Some(session_id));
                        err_msg = Some(format!("max turns reached ({max_turns})"));
                        break;
                    }
                }
                Ok(Some(SessionUpdate::Error { .. })) => {
                    events.error_count += 1;
                    err_msg.get_or_insert_with(|| "agent emitted an error".into());
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

    events.file_edits.sort();
    events.file_edits.dedup();
    let changes = match snapshot_workspace(work.path()) {
        Ok(after) => diff_workspace(&before, &after),
        Err(_) => {
            safety_violations.push("workspace_snapshot_failed".into());
            WorkspaceChangeSummary::default()
        }
    };
    if changes.has_safety_findings() {
        safety_violations.push("symlink_present_in_workspace".into());
    }
    safety_violations.sort();
    safety_violations.dedup();

    let handoff_text = host
        .session_transcript(session_id)
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .rev()
                .find(|entry| entry.role == "assistant")
        })
        .map(|entry| entry.text);
    let handoff = summarize_handoff(handoff_text.as_deref());
    let oracle = eval_oracle::evaluate(work.path(), &task.success);
    let mut detail = oracle.detail.clone();
    if !oracle.ok && std::env::var_os("GROKPTAH_EVAL_KEEP_FAIL").is_some() {
        let dump = std::env::temp_dir().join(format!("grokptah-eval-fail-{}", task.id));
        let _ = fs::remove_dir_all(&dump);
        let _ = copy_dir(work.path(), &dump);
        detail = format!("{detail}; dump={}", dump.display());
        eprintln!("eval fail dump: {}", dump.display());
    }
    // Append compact instrumentation for scoreboard consumers.
    detail = format!(
        "{detail}; tools=[{}]; cargo_test={cargo_test_ran}; cargo_test_round={:?}",
        tool_names.join(","),
        cargo_test_first_round
    );
    let mut failure_reasons = Vec::new();
    if !events.turn_complete_seen {
        failure_reasons.push("turn_complete_missing".into());
    }
    if events.cancelled {
        failure_reasons.push("turn_cancelled".into());
    }
    if events.error_count > 0 {
        failure_reasons.push("agent_error_event".into());
    }
    if !oracle.ok {
        failure_reasons.push("oracle_failed".into());
    }
    if !handoff.present {
        failure_reasons.push("missing_final_handoff".into());
    }
    if !safety_violations.is_empty() {
        failure_reasons.push("safety_finding".into());
    }
    failure_reasons.sort();
    failure_reasons.dedup();
    let mut quality_findings = Vec::new();
    if handoff.present && !handoff.mentions_changes {
        quality_findings.push("handoff_missing_change_summary".into());
    }
    if verification.test_commands > 0
        && verification.tests_passed == Some(true)
        && !handoff.mentions_tests
    {
        quality_findings.push("handoff_missing_test_verification".into());
    }
    quality_findings.sort();
    let verified = oracle.ok
        && events.turn_complete_seen
        && !events.cancelled
        && err_msg.is_none()
        && verification.tests_passed != Some(false)
        && safety_violations.is_empty();
    let completion = if !events.turn_complete_seen {
        "incomplete"
    } else if events.cancelled {
        "cancelled"
    } else if err_msg.is_some() {
        "error"
    } else if oracle.ok {
        "completed"
    } else {
        "oracle_failed"
    };
    TaskResult {
        id: task.id.clone(),
        success: oracle.ok,
        verified,
        wall_ms: t0.elapsed().as_millis(),
        tool_calls,
        tool_errors,
        permission_prompts,
        rounds_est,
        error: err_msg,
        detail,
        difficulty: task.difficulty.clone(),
        tool_names,
        cargo_test_ran,
        cargo_test_first_round,
        completion: completion.into(),
        incomplete: !events.turn_complete_seen || events.cancelled,
        events,
        verification,
        handoff,
        changes,
        failure_reasons,
        quality_findings,
        safety_violations,
    }
}

fn progress_within_turn_budget(
    round: u32,
    task_max_turns: u32,
    advertised_max_rounds: u32,
) -> bool {
    let task_max_turns = task_max_turns.max(1);
    let allowed = advertised_max_rounds
        .max(task_max_turns)
        .min(task_max_turns.saturating_add(1));
    round <= allowed
}

fn looks_like_cargo_test(title: &str, input: &serde_json::Value) -> bool {
    if title != "run_terminal_cmd" {
        return false;
    }
    let cmd = input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    cmd.contains("cargo test") || cmd.contains("cargo\ttest")
}

fn observe_test_update(
    verification: &mut VerificationEvidence,
    status: ToolCallStatus,
    output: Option<&str>,
) {
    if matches!(status, ToolCallStatus::Failed | ToolCallStatus::Denied) {
        // A failed tool status wins over passing sub-suite text in the output.
        // Cargo can print several successful suites before the final failure.
        verification.tests_passed = Some(false);
        return;
    }
    let Some(output) = output else {
        return;
    };
    let output = output.to_ascii_lowercase();
    if output.contains("test result: failed") {
        verification.tests_passed = Some(false);
    } else if output.contains("test result: ok") {
        verification.tests_passed = Some(true);
    }
}

fn fail_early(task: &Task, t0: Instant, error: String, detail: impl Into<String>) -> TaskResult {
    TaskResult {
        id: task.id.clone(),
        success: false,
        verified: false,
        wall_ms: t0.elapsed().as_millis(),
        tool_calls: 0,
        tool_errors: 0,
        permission_prompts: 0,
        rounds_est: 0,
        error: Some(error),
        detail: detail.into(),
        difficulty: task.difficulty.clone(),
        tool_names: Vec::new(),
        cargo_test_ran: false,
        cargo_test_first_round: None,
        completion: "setup_failed".into(),
        incomplete: true,
        events: EventEvidence::default(),
        verification: VerificationEvidence::default(),
        handoff: HandoffEvidence::default(),
        changes: WorkspaceChangeSummary::default(),
        failure_reasons: vec!["setup_failed".into()],
        quality_findings: Vec::new(),
        safety_violations: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_tool_status_overrides_passing_subsuite_text() {
        let mut evidence = VerificationEvidence::default();
        observe_test_update(
            &mut evidence,
            ToolCallStatus::Failed,
            Some("test result: ok\ntest result: failed"),
        );
        assert_eq!(evidence.tests_passed, Some(false));
    }

    #[test]
    fn completed_test_output_is_recorded() {
        let mut evidence = VerificationEvidence::default();
        observe_test_update(
            &mut evidence,
            ToolCallStatus::Completed,
            Some("test result: ok"),
        );
        assert_eq!(evidence.tests_passed, Some(true));
    }

    #[test]
    fn allows_advertised_single_recovery_round() {
        assert!(progress_within_turn_budget(4, 3, 4));
    }

    #[test]
    fn rejects_unadvertised_recovery_round() {
        assert!(!progress_within_turn_budget(4, 3, 3));
    }

    #[test]
    fn rejects_second_recovery_round() {
        assert!(!progress_within_turn_budget(5, 3, 4));
    }
}
