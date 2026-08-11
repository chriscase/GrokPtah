//! Run the deterministic bridge reliability campaign and write a safe report.
//!
//! Usage:
//!   cargo run --example reliability_eval -- --out ../../../evals/runs/reliability
//!
//! This runner is intentionally offline. Use `live_eval` for explicit,
//! credentialed model evaluation; keeping the deterministic campaign separate
//! makes its results reproducible and CI-safe.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grokptah_agent_bridge::eval_report::{diff_workspace, snapshot_workspace};
use grokptah_agent_bridge::reliability_eval::{
    write_report, EventLedger, ReliabilityReport, ScenarioCheck, ScenarioResult,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHost, AgentHostHandle, EventReceiver,
    HostConfig, PermissionDecision, SessionUpdate, SteeringDisposition, ToolCallStatus,
};
use tokio::time::timeout;

struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl IsolatedHome {
    fn install() -> Result<Self> {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().context("create isolated evaluator home")?;
        let home = tmp.path().join(".grokptah");
        fs::create_dir_all(home.join("sessions"))?;
        fs::create_dir_all(home.join("memory"))?;
        set_grokptah_home_override(Some(home));
        unsafe { std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1") };
        Ok(Self {
            _tmp: tmp,
            _lock: lock,
        })
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        unsafe { std::env::remove_var("GROKPTAH_AGENT_OFFLINE") };
    }
}

fn fixture_repo() -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir().context("create disposable fixture")?;
    fs::create_dir_all(dir.path().join("src"))?;
    fs::write(
        dir.path().join("AGENTS.md"),
        "Keep edits bounded and tested.\n",
    )?;
    fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )?;
    Ok(dir)
}

fn started_host(always_approve: bool) -> Result<AgentHostHandle> {
    let host = AgentHost::create(HostConfig {
        always_approve,
        ..HostConfig::default()
    });
    host.start()?;
    // Persisted chrome can outlive a scenario, so make policy explicit.
    host.set_always_approve(always_approve);
    Ok(host)
}

async fn drain_until_complete(rx: &mut EventReceiver) -> Result<Vec<SessionUpdate>> {
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(8), rx.recv())
            .await
            .context("timed out waiting for bridge event")?
            .context("bridge event stream closed")?;
        let done = matches!(event, SessionUpdate::TurnComplete { .. });
        events.push(event);
        if done {
            return Ok(events);
        }
    }
}

fn finish(
    id: &str,
    started: Instant,
    ledger: &EventLedger,
    checks: Vec<ScenarioCheck>,
    changed: Vec<String>,
) -> ScenarioResult {
    ScenarioResult::from_ledger(id, started.elapsed().as_millis(), ledger, checks, changed)
}

fn record_all(ledger: &mut EventLedger, events: &[SessionUpdate]) {
    for event in events {
        ledger.record(event);
    }
}

async fn coding_flow() -> Result<ScenarioResult> {
    let started = Instant::now();
    let dir = fixture_repo()?;
    let before = snapshot_workspace(dir.path()).map_err(anyhow::Error::msg)?;
    let host = started_host(true)?;
    let mut rx = host
        .take_event_receiver()
        .context("event receiver unavailable")?;
    host.set_project_cwd(dir.path())?;
    let session = host.session_new()?;
    host.session_prompt(session.id, "write result.txt: deterministic success".into())
        .await?;
    let events = drain_until_complete(&mut rx).await?;
    let after = snapshot_workspace(dir.path()).map_err(anyhow::Error::msg)?;
    let changes = diff_workspace(&before, &after);
    let mut ledger = EventLedger::default();
    record_all(&mut ledger, &events);
    let terminal = ledger.terminal_contract();
    let checks = vec![
        if dir.path().join("result.txt").is_file() {
            ScenarioCheck::pass("write_applied", "result.txt was created")
        } else {
            ScenarioCheck::fail("write_applied", "result.txt was not created")
        },
        if ledger.saw("file_edit") {
            ScenarioCheck::pass("file_edit_event", "typed edit event observed")
        } else {
            ScenarioCheck::fail("file_edit_event", "typed edit event missing")
        },
        if terminal.correlated_evidence == terminal.completion_evidence
            && terminal.completion_evidence == 1
        {
            ScenarioCheck::pass(
                "terminal_evidence_correlated",
                "one current completion evidence",
            )
        } else {
            ScenarioCheck::fail(
                "terminal_evidence_correlated",
                "completion evidence mismatch",
            )
        },
        if terminal.evidence_precedes_completion {
            ScenarioCheck::pass(
                "evidence_before_complete",
                "evidence preceded terminal event",
            )
        } else {
            ScenarioCheck::fail("evidence_before_complete", "terminal ordering was invalid")
        },
    ];
    Ok(finish(
        "coding_flow",
        started,
        &ledger,
        checks,
        changes
            .changed_files
            .into_iter()
            .map(|file| file.path)
            .collect(),
    ))
}

async fn queue_and_steering() -> Result<ScenarioResult> {
    let started = Instant::now();
    let dir = fixture_repo()?;
    let host = started_host(true)?;
    let mut rx = host
        .take_event_receiver()
        .context("event receiver unavailable")?;
    host.set_project_cwd(dir.path())?;
    let session = host.session_new()?;
    let first = host.session_queue_add(session.id, "first follow-up".into(), false)?[0].clone();
    let second = host.session_queue_add(session.id, "priority follow-up".into(), true)?[1].clone();
    let moved = host.session_queue_move(session.id, &second.id, 0)?;
    let run_next = host.session_queue_run_next(session.id, &first.id)?;
    let drained = host.session_queue_take_next(session.id)?;

    let host_for_turn = host.clone();
    let session_id = session.id;
    let runner = tokio::spawn(async move {
        host_for_turn
            .session_prompt(session_id, "run sleep 1".into())
            .await
    });
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(8), rx.recv())
            .await
            .context("shell start timed out")?
            .context("event stream closed")?;
        let shell_started = matches!(event, SessionUpdate::ShellSessionStarted { .. });
        events.push(event);
        if shell_started {
            break;
        }
    }
    let receipt = host.session_steer(session.id, "focus on the failing test".into())?;
    let reply = timeout(Duration::from_secs(8), runner)
        .await
        .context("steered turn timed out")??;
    let reply = reply?;
    events.extend(drain_until_complete(&mut rx).await?);
    let mut ledger = EventLedger::default();
    record_all(&mut ledger, &events);
    let checks = vec![
        if moved.first().map(|entry| entry.id.as_str()) == Some(second.id.as_str()) {
            ScenarioCheck::pass("queue_reorder", "priority entry moved to front")
        } else {
            ScenarioCheck::fail("queue_reorder", "priority entry was not moved")
        },
        if run_next.entries.first().map(|entry| entry.priority) == Some(true) {
            ScenarioCheck::pass("run_next_priority", "run-next marked the requested entry")
        } else {
            ScenarioCheck::fail("run_next_priority", "run-next did not mark priority")
        },
        if drained.batch.as_ref().map(|batch| batch.text.as_str()) == Some("first follow-up") {
            ScenarioCheck::pass("queue_drain", "run-next remained authoritative")
        } else {
            ScenarioCheck::fail("queue_drain", "queue drain returned an unexpected batch")
        },
        if receipt.disposition == SteeringDisposition::Pending {
            ScenarioCheck::pass(
                "steer_pending",
                "active build accepted non-cancelling steering",
            )
        } else {
            ScenarioCheck::fail(
                "steer_pending",
                "active build did not return pending steering",
            )
        },
        if ledger.metrics().steering_injected == 1 {
            ScenarioCheck::pass(
                "steer_injected_once",
                "one steering event reached the boundary",
            )
        } else {
            ScenarioCheck::fail(
                "steer_injected_once",
                "steering injection count was not one",
            )
        },
        if !reply.is_empty() {
            ScenarioCheck::pass(
                "turn_completed",
                "active turn returned without cancellation",
            )
        } else {
            ScenarioCheck::fail("turn_completed", "active turn returned an empty response")
        },
    ];
    Ok(finish(
        "queue_and_steering",
        started,
        &ledger,
        checks,
        Vec::new(),
    ))
}

async fn permission_deny() -> Result<ScenarioResult> {
    let started = Instant::now();
    let dir = fixture_repo()?;
    let host = started_host(false)?;
    let mut rx = host
        .take_event_receiver()
        .context("event receiver unavailable")?;
    host.set_project_cwd(dir.path())?;
    let session = host.session_new()?;
    let host_for_turn = host.clone();
    let runner = tokio::spawn(async move {
        host_for_turn
            .session_prompt(session.id, "run echo should-deny".into())
            .await
    });
    let mut events = Vec::new();
    let request_id = loop {
        let event = timeout(Duration::from_secs(8), rx.recv())
            .await
            .context("permission request timed out")?
            .context("event stream closed")?;
        if let SessionUpdate::PermissionRequired { request, .. } = &event {
            let id = request.id;
            events.push(event);
            break id;
        }
        events.push(event);
    };
    host.permission_respond(request_id, PermissionDecision::Deny)?;
    runner.await??;
    events.extend(drain_until_complete(&mut rx).await?);
    let mut ledger = EventLedger::default();
    record_all(&mut ledger, &events);
    let denied = events.iter().any(|event| {
        matches!(
            event,
            SessionUpdate::ToolCall {
                status: ToolCallStatus::Denied,
                ..
            }
        )
    });
    let checks = vec![
        if ledger.metrics().permission_requests == 1 {
            ScenarioCheck::pass("permission_requested", "one permission request observed")
        } else {
            ScenarioCheck::fail(
                "permission_requested",
                "permission request count was unexpected",
            )
        },
        if denied {
            ScenarioCheck::pass("permission_denied", "tool execution was denied")
        } else {
            ScenarioCheck::fail("permission_denied", "no denied tool event observed")
        },
        ScenarioCheck::pass("turn_recovered", "denied tool returned control to the turn"),
    ];
    Ok(finish(
        "permission_deny",
        started,
        &ledger,
        checks,
        Vec::new(),
    ))
}

async fn cancellation() -> Result<ScenarioResult> {
    let started = Instant::now();
    let dir = fixture_repo()?;
    let host = started_host(true)?;
    let mut rx = host
        .take_event_receiver()
        .context("event receiver unavailable")?;
    host.set_project_cwd(dir.path())?;
    let session = host.session_new()?;
    let host_for_turn = host.clone();
    let runner = tokio::spawn(async move {
        host_for_turn
            .session_prompt(session.id, "run sleep 10".into())
            .await
    });
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(8), rx.recv())
            .await
            .context("shell start timed out")?
            .context("event stream closed")?;
        let shell_started = matches!(event, SessionUpdate::ShellSessionStarted { .. });
        events.push(event);
        if shell_started {
            break;
        }
    }
    host.cancel_turn(Some(session.id))?;
    let _ = timeout(Duration::from_secs(8), runner)
        .await
        .context("cancelled turn timed out")??;
    events.extend(drain_until_complete(&mut rx).await?);
    let mut ledger = EventLedger::default();
    record_all(&mut ledger, &events);
    let shell_cancelled = events.iter().any(|event| {
        matches!(
            event,
            SessionUpdate::ShellSessionEnded {
                cancelled: true,
                ..
            }
        )
    });
    let turn_cancelled = events.iter().any(|event| {
        matches!(
            event,
            SessionUpdate::TurnComplete {
                cancelled: true,
                ..
            }
        )
    });
    let checks = vec![
        if shell_cancelled {
            ScenarioCheck::pass("shell_cancelled", "live child reported cancellation")
        } else {
            ScenarioCheck::fail("shell_cancelled", "live child did not report cancellation")
        },
        if turn_cancelled {
            ScenarioCheck::pass("turn_cancelled", "terminal event preserved cancellation")
        } else {
            ScenarioCheck::fail("turn_cancelled", "turn completion lost cancellation state")
        },
    ];
    Ok(finish("cancellation", started, &ledger, checks, Vec::new()))
}

async fn restart_durability() -> Result<ScenarioResult> {
    let started = Instant::now();
    let dir = fixture_repo()?;
    let host = started_host(true)?;
    let mut rx = host
        .take_event_receiver()
        .context("event receiver unavailable")?;
    host.set_project_cwd(dir.path())?;
    let session = host.session_new()?;
    host.session_prompt(session.id, "write durable.txt: survives restart".into())
        .await?;
    let events = drain_until_complete(&mut rx).await?;
    let before_history = host.session_completion_history(session.id)?;
    let before_transcript = host.session_transcript(session.id)?;
    drop(rx);
    drop(host);

    let restored_host = AgentHost::create(HostConfig::default());
    let after_history = restored_host.session_completion_history(session.id)?;
    let after_transcript = restored_host.session_transcript(session.id)?;
    let mut ledger = EventLedger::default();
    record_all(&mut ledger, &events);
    let checks = vec![
        if after_history.len() == before_history.len() && !after_history.is_empty() {
            ScenarioCheck::pass(
                "completion_history_reloaded",
                "completion evidence survived restart",
            )
        } else {
            ScenarioCheck::fail(
                "completion_history_reloaded",
                "completion history changed after restart",
            )
        },
        if after_transcript.len() == before_transcript.len() && !after_transcript.is_empty() {
            ScenarioCheck::pass("transcript_reloaded", "transcript survived restart")
        } else {
            ScenarioCheck::fail("transcript_reloaded", "transcript was not restored")
        },
    ];
    Ok(finish(
        "restart_durability",
        started,
        &ledger,
        checks,
        vec!["durable.txt".into()],
    ))
}

fn event_fanout_and_journal() -> Result<ScenarioResult> {
    let started = Instant::now();
    let host = started_host(true)?;
    let bus = host.event_bus();
    let mut gui = bus.subscribe();
    let mut coordinator = bus.subscribe();
    let start_seq = bus.current_seq();
    let session_id = uuid::Uuid::new_v4();
    bus.publish(SessionUpdate::AgentMessageChunk {
        session_id,
        text: "hello".into(),
    });
    bus.publish(SessionUpdate::TurnStarted {
        session_id,
        turn_id: uuid::Uuid::new_v4(),
    });
    let gui_first = gui.try_recv()?;
    let coordinator_first = coordinator.try_recv()?;
    let gui_second = gui.try_recv()?;
    let coordinator_second = coordinator.try_recv()?;
    let page = bus.read_after(start_seq, 10);
    let same_first = serde_json::to_value(&gui_first)? == serde_json::to_value(&coordinator_first)?;
    let same_second =
        serde_json::to_value(&gui_second)? == serde_json::to_value(&coordinator_second)?;
    let monotonic = page
        .entries
        .windows(2)
        .all(|pair| pair[0].seq < pair[1].seq);
    let mut ledger = EventLedger::default();
    ledger.record(&gui_first);
    ledger.record(&gui_second);
    let checks = vec![
        if same_first && same_second {
            ScenarioCheck::pass("fanout_order", "GUI and coordinator saw the same order")
        } else {
            ScenarioCheck::fail("fanout_order", "subscribers diverged")
        },
        if page.entries.len() == 2 && monotonic {
            ScenarioCheck::pass("journal_sequence", "journal retained two monotonic entries")
        } else {
            ScenarioCheck::fail(
                "journal_sequence",
                "journal sequence or count was unexpected",
            )
        },
    ];
    Ok(finish(
        "event_fanout_and_journal",
        started,
        &ledger,
        checks,
        Vec::new(),
    ))
}

fn stale_evidence_detection() -> Result<ScenarioResult> {
    let started = Instant::now();
    let mut ledger = EventLedger::default();
    let session_id = uuid::Uuid::new_v4();
    let first_turn = uuid::Uuid::new_v4();
    let second_turn = uuid::Uuid::new_v4();
    ledger.record(&SessionUpdate::TurnStarted {
        session_id,
        turn_id: first_turn,
    });
    ledger.record(&SessionUpdate::CompletionEvidence {
        session_id,
        turn_id: first_turn,
        evidence: Default::default(),
    });
    ledger.record(&SessionUpdate::TurnComplete {
        session_id,
        cancelled: false,
    });
    ledger.record(&SessionUpdate::TurnStarted {
        session_id,
        turn_id: second_turn,
    });
    ledger.record(&SessionUpdate::CompletionEvidence {
        session_id,
        turn_id: first_turn,
        evidence: Default::default(),
    });
    let terminal = ledger.terminal_contract();
    let checks = vec![if terminal.stale_evidence == 1 {
        ScenarioCheck::pass(
            "stale_terminal_rejected",
            "late evidence was classified as stale",
        )
    } else {
        ScenarioCheck::fail(
            "stale_terminal_rejected",
            "late evidence was not classified as stale",
        )
    }];
    Ok(finish(
        "stale_evidence_detection",
        started,
        &ledger,
        checks,
        Vec::new(),
    ))
}

pub async fn run_campaign() -> Result<ReliabilityReport> {
    let _home = IsolatedHome::install()?;
    let report_start = ReliabilityReport::new("offline", "deterministic-fixture");
    let scenarios = vec![
        coding_flow().await?,
        queue_and_steering().await?,
        permission_deny().await?,
        cancellation().await?,
        restart_durability().await?,
        event_fanout_and_journal()?,
        stale_evidence_detection()?,
    ];
    Ok(report_start.finish(scenarios))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("../../../evals/runs/reliability");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = PathBuf::from(args.next().context("--out needs a directory")?),
            "--live" => {
                anyhow::bail!("live mode is not part of the deterministic runner; use live_eval")
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    let report = run_campaign().await?;
    let report_path = write_report(&out, &report).map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    eprintln!("report: {}", report_path.display());
    if report.summary.failed > 0 {
        anyhow::bail!(
            "reliability campaign reported {} failed scenario(s)",
            report.summary.failed
        );
    }
    Ok(())
}
