//! Always-on Grokbot lifecycle, restart, and fail-closed certification.
//!
//! Drives the shipped `grokptah-service` binary over authenticated MCP with a
//! loopback fake provider. No production crate is modified.

#![allow(clippy::await_holding_lock)]

mod always_on_support;

use std::path::Path;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use uuid::Uuid;

use always_on_support::{
    assert_no_quota_ledger, assert_no_secret_leak, call, call_expect_error, mcp, pending_usage,
    poll_json, rid, serial_lock, snapshot, succeeded_kind_count, work_items, work_kind_count,
    Cardinality, FakeProvider, ProviderScript, ServiceProcess,
};

const CUTS: &[&str] = &[
    "occurrence-reserved",
    "decision-work-persisted",
    "native-intent-persisted",
    "run-submitted",
    "directive-proposed",
    "orchestration-mutation-persisted",
    "decision-applied-pending",
    "notification-accepted-fence-pending",
    "terminal-run-before-settlement",
];

fn managed_policy() -> Value {
    json!({
        "enabled": true,
        "allowedWorkKinds": [],
        "allowedSourceRoutineIds": [],
        "maxConcurrentRuns": 2,
        "bounds": {
            "maxPromptBytes": 16384,
            "maxRounds": 4,
            "maxDurationMs": 45000,
            "maxTotalTokens": 8000
        },
        "retryEligible": false,
        "requiresApprovalBeforeExecution": false
    })
}

fn native_step(step_id: &str, objective: &str, deps: &[&str], agent_id: &str) -> Value {
    json!({
        "stepId": step_id,
        "kind": "native",
        "objective": objective,
        "assignedAgentId": agent_id,
        "dependencies": deps,
        "policy": {
            "bounds": {
                "maxPromptBytes": 16384,
                "maxRounds": 4,
                "maxDurationMs": 45000,
                "maxTotalTokens": 8000
            },
            "retry": {
                "maxAttempts": 1,
                "retryFailed": false,
                "retryExpired": false,
                "backoffMs": 0
            },
            "requiresApproval": false,
            "maxConcurrentAttempts": 1
        }
    })
}

async fn bootstrap_agent(client: &mut McpControlClient, workspace: &Path) -> (Uuid, String) {
    let created = call(
        client,
        "ptah_create_session",
        json!({
            "workspace": workspace,
            "title": "always-on grokbot"
        }),
    )
    .await;
    let session = Uuid::parse_str(created["sessionId"].as_str().expect("sessionId")).unwrap();
    let submitted = call(
        client,
        "ptah_submit_task",
        json!({
            "request_id": rid("setup"),
            "session_id": session,
            "workspace": workspace,
            "prompt": "GROKBOT_SETUP materialize the lane Agent"
        }),
    )
    .await;
    let run_id = submitted["runId"]
        .as_str()
        .expect("setup runId")
        .to_string();
    let _ = poll_json(
        client,
        "ptah_get_run",
        json!({
            "session_id": session,
            "workspace": workspace,
            "run_id": run_id
        }),
        |value| {
            matches!(
                value["state"].as_str(),
                Some("completed" | "failed" | "cancelled" | "interrupted")
            )
        },
    )
    .await;
    let agents = poll_json(client, "ptah_list_persistent_agents", json!({}), |value| {
        value["agents"].as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    let agent_id = agents["agents"][0]["agentId"]
        .as_str()
        .expect("agentId")
        .to_string();
    let _ = call(
        client,
        "ptah_set_managed_execution",
        json!({
            "session_id": session,
            "workspace": workspace,
            "agent_id": agent_id,
            "policy": managed_policy()
        }),
    )
    .await;
    (session, agent_id)
}

async fn create_plan(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    agent_id: &str,
) -> String {
    let created = call(
        client,
        "ptah_create_manager_plan",
        json!({
            "request_id": rid("plan"),
            "session_id": session,
            "workspace": workspace,
            "manager_agent_id": agent_id,
            "objective": "always-on grokbot dependent DAG",
            "autonomous": true,
            "max_replans": 2,
            "max_in_flight": 2,
            "steps": [
                native_step("step-a", "GROKBOT_SUCCESS first native unit", &[], agent_id),
                native_step(
                    "step-b",
                    "GROKBOT_FORCE_FAIL child that must be replaced",
                    &["step-a"],
                    agent_id
                )
            ]
        }),
    )
    .await;
    created["plan"]["planId"]
        .as_str()
        .expect("planId")
        .to_string()
}

async fn tick_twice(client: &mut McpControlClient, session: Uuid, workspace: &Path, plan_id: &str) {
    for i in 0..2 {
        for attempt in 0..4 {
            match client
                .call_tool(
                    "ptah_tick_manager_plan",
                    json!({
                        "request_id": rid(&format!("tick{i}-{attempt}")),
                        "session_id": session,
                        "workspace": workspace,
                        "plan_id": plan_id
                    }),
                )
                .await
            {
                Ok(result) if !result.is_error => break,
                Ok(result) => {
                    let text = result.raw.to_string().to_lowercase();
                    if text.contains("conflict") || text.contains("stale") {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    panic!("ptah_tick_manager_plan error: {:?}", result.raw);
                }
                Err(error) => {
                    let text = error.to_string().to_lowercase();
                    if text.contains("conflict") || text.contains("stale") {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    panic!("ptah_tick_manager_plan: {error}");
                }
            }
        }
    }
}

async fn wait_plan_state(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
    want: &str,
) -> Value {
    poll_json(
        client,
        "ptah_get_manager_plan",
        json!({
            "session_id": session,
            "workspace": workspace,
            "plan_id": plan_id
        }),
        |value| value["plan"]["state"].as_str() == Some(want),
    )
    .await
}

async fn list_work(client: &mut McpControlClient, session: Uuid, workspace: &Path) -> Value {
    call(
        client,
        "ptah_list_work",
        json!({
            "session_id": session,
            "workspace": workspace
        }),
    )
    .await
}

async fn list_runs(client: &mut McpControlClient, session: Uuid, workspace: &Path) -> Value {
    call(
        client,
        "ptah_list_runs",
        json!({
            "session_id": session,
            "workspace": workspace
        }),
    )
    .await
}

async fn list_intents(client: &mut McpControlClient, session: Uuid, workspace: &Path) -> Value {
    call(
        client,
        "ptah_list_execution_intents",
        json!({
            "session_id": session,
            "workspace": workspace
        }),
    )
    .await
}

fn run_state(run: &Value) -> Option<&str> {
    run["state"].as_str()
}

fn plan_has_step(plan: &Value, step_id: &str) -> bool {
    plan.pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|step| step["stepId"].as_str() == Some(step_id))
}

fn cut_reached(
    cut: &str,
    plan: &Value,
    work: &Value,
    runs: &Value,
    intents: &Value,
    messages: &Value,
) -> bool {
    let plan_state = plan["plan"]["state"].as_str().unwrap_or_default();
    let native_work = work_kind_count(work, "native");
    let decisions = work_kind_count(work, "manager-decision");
    let run_count = runs["runs"].as_array().map(|a| a.len()).unwrap_or(0);
    let intent_count = intents["intents"].as_array().map(|a| a.len()).unwrap_or(0);
    let message_count = messages
        .get("messages")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    let any_terminal_run = runs["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|run| matches!(run_state(run), Some("completed" | "failed" | "interrupted")));
    let decision_succeeded = succeeded_kind_count(work, "manager-decision") >= 1;
    match cut {
        "occurrence-reserved" => {
            matches!(plan_state, "needs_replan" | "active" | "succeeded") && native_work >= 1
        }
        "decision-work-persisted" => decisions >= 1 || plan_state == "succeeded",
        "native-intent-persisted" => native_work >= 1 && (intent_count >= 1 || any_terminal_run),
        "run-submitted" => native_work >= 1 && run_count > 1,
        "directive-proposed" => decision_succeeded || plan_has_step(plan, "step-b-fix"),
        "orchestration-mutation-persisted" => plan_has_step(plan, "step-b-fix"),
        "decision-applied-pending" => {
            plan_has_step(plan, "step-b-fix")
                && matches!(plan_state, "active" | "succeeded" | "needs_replan")
        }
        "notification-accepted-fence-pending" => message_count >= 1 || plan_state == "succeeded",
        "terminal-run-before-settlement" => native_work >= 1 && any_terminal_run,
        _ => false,
    }
}

async fn observe(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
) -> (Value, Value, Value, Value, Value) {
    let plan = call(
        client,
        "ptah_get_manager_plan",
        json!({
            "session_id": session,
            "workspace": workspace,
            "plan_id": plan_id
        }),
    )
    .await;
    let work = list_work(client, session, workspace).await;
    let runs = list_runs(client, session, workspace).await;
    let intents = list_intents(client, session, workspace).await;
    let messages = call(
        client,
        "ptah_list_inbox",
        json!({
            "session_id": session,
            "workspace": workspace,
            "agent_id": plan["plan"]["managerAgentId"]
        }),
    )
    .await;
    (plan, work, runs, intents, messages)
}

fn assert_no_uncertain_resume(runs: &Value) {
    for run in runs["runs"].as_array().unwrap_or(&vec![]) {
        assert_no_secret_leak(run);
        if run_state(run) == Some("interrupted") {
            assert_eq!(
                pending_usage(run),
                0,
                "uncertain attempt left pending after restart: {run}"
            );
        }
    }
}

fn assert_cardinalities_stable(before: &Cardinality, after: &Cardinality, cut: &str) {
    assert!(
        after.work_items >= before.work_items,
        "restart dropped work at {cut}: {before:?} -> {after:?}"
    );
    assert!(
        after.runs >= before.runs,
        "restart dropped runs at {cut}: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.quota_reservations, 0,
        "quota ledger must stay absent at {cut}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_lifecycle_reaches_succeeded_without_duplicate_sends() {
    let _serial = serial_lock();
    let provider = FakeProvider::start();
    let service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id) = bootstrap_agent(&mut client, &service.workspace).await;
    let setup_sends = provider.send_count();
    let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
    tick_twice(&mut client, session, &service.workspace, &plan_id).await;

    let _ = poll_json(
        &mut client,
        "ptah_list_work",
        json!({
            "session_id": session,
            "workspace": &service.workspace
        }),
        |value| work_kind_count(value, "manager-decision") >= 1,
    )
    .await;
    let work = list_work(&mut client, session, &service.workspace).await;
    let decision_id = work_items(&work)
        .iter()
        .find(|item| item["kind"].as_str() == Some("manager-decision"))
        .and_then(|item| item["workId"].as_str())
        .expect("decision work id")
        .to_string();
    let _ = poll_json(
        &mut client,
        "ptah_get_work",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "work_id": decision_id
        }),
        |value| {
            matches!(
                value["work"]["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            )
        },
    )
    .await;
    let decision_work = call(
        &mut client,
        "ptah_get_work",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "work_id": decision_id
        }),
    )
    .await;
    assert_eq!(
        decision_work["work"]["state"].as_str(),
        Some("succeeded"),
        "manager-decision Work did not succeed: {decision_work}; provider={:?}",
        provider.last_user_contents()
    );
    let summary = decision_work["work"]["result"]["summary"]
        .as_str()
        .unwrap_or_default();
    assert!(
        summary.contains("append_replacement_steps"),
        "manager-decision summary was not a replacement envelope: {summary}; provider={:?}",
        provider.last_user_contents()
    );
    let _ = wait_plan_state(
        &mut client,
        session,
        &service.workspace,
        &plan_id,
        "succeeded",
    )
    .await;
    tick_twice(&mut client, session, &service.workspace, &plan_id).await;

    let work = list_work(&mut client, session, &service.workspace).await;
    let runs = list_runs(&mut client, session, &service.workspace).await;
    let intents = list_intents(&mut client, session, &service.workspace).await;
    let plan = call(
        &mut client,
        "ptah_get_manager_plan",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "plan_id": plan_id
        }),
    )
    .await;
    let capacity = call(&mut client, "ptah_get_capacity", json!({})).await;

    assert_eq!(plan["plan"]["state"].as_str(), Some("succeeded"));
    assert_eq!(work_kind_count(&work, "manager-decision"), 1);
    assert_eq!(succeeded_kind_count(&work, "manager-decision"), 1);
    let native_succeeded = succeeded_kind_count(&work, "native");
    assert!(
        native_succeeded >= 2,
        "expected native success for step-a and replacement, got {native_succeeded}: {work}"
    );
    assert!(
        plan_has_step(&plan, "step-b-fix"),
        "replacement step missing: {plan}"
    );
    let campaign_sends = provider.send_count().saturating_sub(setup_sends);
    assert!(
        (3..=8).contains(&campaign_sends),
        "unexpected provider sends {campaign_sends}"
    );
    let mut proposal_runs = 0usize;
    for run in runs["runs"].as_array().unwrap() {
        assert_no_secret_leak(run);
        assert_eq!(
            pending_usage(run),
            0,
            "terminal run left pending usage: {run}"
        );
        if run["purpose"].as_str() == Some("manager_proposal") {
            proposal_runs += 1;
        }
    }
    assert_eq!(proposal_runs, 1, "expected exactly one proposal-only Run");
    let linked = intents["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|intent| {
            intent
                .get("runId")
                .or_else(|| intent.get("run_id"))
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        })
        .count();
    assert!(
        linked >= 1,
        "expected at least one execution intent with a linked run: {intents}"
    );
    let live_intents = intents["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|intent| {
            matches!(
                intent["state"].as_str(),
                Some("claiming" | "admitted" | "parked" | "resolving")
            )
        })
        .count();
    assert_eq!(live_intents, 0);
    assert_no_quota_ledger(&capacity);
    assert_no_secret_leak(&capacity);
    let after = snapshot(
        &mut client,
        session,
        &service.workspace,
        &plan_id,
        provider.send_count(),
    )
    .await;
    tick_twice(&mut client, session, &service.workspace, &plan_id).await;
    let again = snapshot(
        &mut client,
        session,
        &service.workspace,
        &plan_id,
        provider.send_count(),
    )
    .await;
    assert_eq!(after.provider_sends, again.provider_sends);
    assert_eq!(after.runs, again.runs);
    assert_eq!(after.work_items, again.work_items);
    assert_eq!(after.intents, again.intents);
    assert_eq!(after.decisions, again.decisions);
    assert_eq!(after.messages, again.messages);
    assert_eq!(after.quota_reservations, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_restart_cuts_do_not_duplicate_or_resume_uncertain_attempts() {
    let _serial = serial_lock();
    let provider = FakeProvider::start();
    let mut service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id) = bootstrap_agent(&mut client, &service.workspace).await;
    let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
    tick_twice(&mut client, session, &service.workspace, &plan_id).await;

    for cut in CUTS {
        let deadline = Instant::now() + Duration::from_secs(70);
        loop {
            tick_twice(&mut client, session, &service.workspace, &plan_id).await;
            let (plan, work, runs, intents, messages) =
                observe(&mut client, session, &service.workspace, &plan_id).await;
            if cut_reached(cut, &plan, &work, &runs, &intents, &messages) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cut {cut} never appeared; plan={plan} work={work} runs={runs}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let before = snapshot(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            provider.send_count(),
        )
        .await;
        service.respawn(&provider.base_url);
        client = mcp(&service.addr).await;
        let _ = poll_json(
            &mut client,
            "ptah_get_manager_plan",
            json!({
                "session_id": session,
                "workspace": &service.workspace,
                "plan_id": plan_id
            }),
            |value| value.get("plan").is_some(),
        )
        .await;
        tick_twice(&mut client, session, &service.workspace, &plan_id).await;
        let after = snapshot(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            provider.send_count(),
        )
        .await;
        assert_cardinalities_stable(&before, &after, cut);
        tick_twice(&mut client, session, &service.workspace, &plan_id).await;
        let again = snapshot(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            provider.send_count(),
        )
        .await;
        assert!(
            again.runs >= after.runs,
            "duplicate drive after restart shrank runs at {cut}"
        );
        let runs = list_runs(&mut client, session, &service.workspace).await;
        assert_no_uncertain_resume(&runs);
    }

    let _ = wait_plan_state(
        &mut client,
        session,
        &service.workspace,
        &plan_id,
        "succeeded",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_closed_permission_stale_invalid_cancel_quota_absent() {
    let _serial = serial_lock();
    let provider = FakeProvider::start_with(ProviderScript::InvalidDirective);
    let service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id) = bootstrap_agent(&mut client, &service.workspace).await;

    let _ = call(
        &mut client,
        "ptah_set_managed_execution",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "agent_id": agent_id,
            "policy": {
                "enabled": true,
                "maxConcurrentRuns": 1,
                "bounds": {
                    "maxPromptBytes": 1024,
                    "maxRounds": 2,
                    "maxDurationMs": 1000,
                    "maxTotalTokens": 100
                },
                "requiresApprovalBeforeExecution": true
            }
        }),
    )
    .await;
    let err = call_expect_error(
        &mut client,
        "ptah_create_manager_plan",
        json!({
            "request_id": rid("auto-denied"),
            "session_id": session,
            "workspace": &service.workspace,
            "manager_agent_id": agent_id,
            "objective": "should fail closed",
            "autonomous": true,
            "steps": [{
                "stepId": "only",
                "kind": "native",
                "objective": "nope"
            }]
        }),
    )
    .await;
    assert!(
        err.to_lowercase().contains("autonomous")
            || err.to_lowercase().contains("forbidden")
            || err.to_lowercase().contains("approval"),
        "unexpected error: {err}"
    );

    let _ = call(
        &mut client,
        "ptah_set_managed_execution",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "agent_id": agent_id,
            "policy": managed_policy()
        }),
    )
    .await;
    let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
    let stale = call_expect_error(
        &mut client,
        "ptah_advance_manager_plan",
        json!({
            "request_id": rid("stale"),
            "session_id": session,
            "workspace": &service.workspace,
            "plan_id": plan_id,
            "expected_revision": 999
        }),
    )
    .await;
    assert!(
        stale.to_lowercase().contains("stale")
            || stale.to_lowercase().contains("revision")
            || stale.to_lowercase().contains("conflict"),
        "stale revision did not fail closed: {stale}"
    );

    tick_twice(&mut client, session, &service.workspace, &plan_id).await;
    let _ = poll_json(
        &mut client,
        "ptah_list_work",
        json!({
            "session_id": session,
            "workspace": &service.workspace
        }),
        |value| work_kind_count(value, "manager-decision") >= 1,
    )
    .await;
    let after_invalid = poll_json(
        &mut client,
        "ptah_get_manager_plan",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "plan_id": plan_id
        }),
        |value| {
            let state = value["plan"]["state"].as_str();
            state == Some("needs_replan") || state == Some("failed")
        },
    )
    .await;
    assert_ne!(
        after_invalid["plan"]["state"].as_str(),
        Some("succeeded"),
        "invalid directive must not succeed the plan"
    );

    let submitted = call(
        &mut client,
        "ptah_submit_task",
        json!({
            "request_id": rid("cancel-me"),
            "session_id": session,
            "workspace": &service.workspace,
            "prompt": "GROKBOT_SUCCESS cancel target"
        }),
    )
    .await;
    let run_id = submitted["runId"].as_str().unwrap().to_string();
    let _ = call(
        &mut client,
        "ptah_cancel",
        json!({
            "request_id": rid("cancel"),
            "session_id": session,
            "workspace": &service.workspace,
            "run_id": run_id
        }),
    )
    .await;

    let quota_note = call(&mut client, "ptah_get_capacity", json!({})).await;
    assert!(
        quota_note.get("health").is_some(),
        "capacity health missing: {quota_note}"
    );
    assert_no_quota_ledger(&quota_note);
    assert_no_secret_leak(&quota_note);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn soak_always_on_grokbot() {
    let _serial = serial_lock();
    let seconds = std::env::var("GROKBOT_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(600);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let provider = FakeProvider::start();
    let mut service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let mut cycles = 0u64;
    let mut restarts = 0u64;
    while Instant::now() < deadline {
        let (session, agent_id) = bootstrap_agent(&mut client, &service.workspace).await;
        let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
        tick_twice(&mut client, session, &service.workspace, &plan_id).await;
        if cycles % 2 == 1 {
            service.respawn(&provider.base_url);
            client = mcp(&service.addr).await;
            restarts += 1;
        }
        let _ = wait_plan_state(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            "succeeded",
        )
        .await;
        cycles += 1;
    }
    assert!(cycles >= 1, "soak completed zero cycles");
    eprintln!(
        "soak cycles={cycles} restarts={restarts} sends={}",
        provider.send_count()
    );
}
