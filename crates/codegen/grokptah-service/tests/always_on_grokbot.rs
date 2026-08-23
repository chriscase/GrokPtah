//! Always-on Grokbot lifecycle, restart, and fail-closed certification.
//!
//! Drives the shipped `grokptah-service` binary over authenticated MCP with a
//! loopback fake provider. No production crate is modified.

#![allow(clippy::await_holding_lock, clippy::too_many_arguments)]

mod always_on_support;

use std::path::Path;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use uuid::Uuid;

use always_on_support::{
    assert_exact_after_restart, assert_no_quota_ledger, assert_no_secret_leak, call,
    call_expect_error, collect_identity, git_head, home_bytes, mcp, openssl_sha256_hex,
    pending_usage, pending_usage_opt, poll_json, rid, rss_kb, serial_lock, snapshot,
    succeeded_kind_count, usage_complete, work_id_of, work_items, work_kind_count, FakeProvider,
    ProviderScript, ServiceProcess,
};

const CUTS: &[&str] = &[
    "decision-work-persisted",
    "native-intent-persisted",
    "run-submitted",
    "directive-proposed",
    "orchestration-mutation-persisted",
    "decision-applied-pending",
    "notification-accepted-fence-pending",
    "terminal-run-before-settlement",
];
const SEED: &str = "always-on-grokbot-v1";

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

async fn bootstrap_agent(
    client: &mut McpControlClient,
    workspace: &Path,
) -> (Uuid, String, String) {
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
    (session, agent_id, run_id)
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
                        continue;
                    }
                    panic!("ptah_tick_manager_plan error: {:?}", result.raw);
                }
                Err(error) => {
                    let text = error.to_string().to_lowercase();
                    if text.contains("conflict") || text.contains("stale") {
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

fn plan_has_step(plan: &Value, step_id: &str) -> bool {
    plan.pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|step| step["stepId"].as_str() == Some(step_id))
}

fn plan_step<'a>(plan: &'a Value, step_id: &str) -> Option<&'a Value> {
    plan.pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|step| step["stepId"].as_str() == Some(step_id))
}

fn decision_item(work: &Value) -> Option<&Value> {
    work_items(work)
        .iter()
        .find(|item| item["kind"].as_str() == Some("manager-decision"))
}

fn intent_for_work<'a>(intents: &'a Value, work_id: &str) -> Option<&'a Value> {
    intents["intents"].as_array()?.iter().find(|intent| {
        intent["workId"].as_str() == Some(work_id) || intent["work_id"].as_str() == Some(work_id)
    })
}

fn linked_run_ids(detail: &Value) -> Vec<String> {
    detail["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|attempt| {
            attempt["linkedRunIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn step_fence_message_id(step: &Value) -> Option<&str> {
    step["lastNotificationMessageId"]
        .as_str()
        .or_else(|| step["last_notification_message_id"].as_str())
}

fn fence_pending(plan: &Value, work: &Value, messages: &Value) -> bool {
    let inbox: Vec<&str> = messages["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| {
            message["messageId"]
                .as_str()
                .or_else(|| message["message_id"].as_str())
        })
        .collect();
    let fenced: Vec<&str> = plan
        .pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(step_fence_message_id)
        .collect();
    (!inbox.is_empty() && inbox.iter().any(|id| !fenced.contains(id)))
        || plan
            .pointer("/plan/steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|step| {
                let Some(work_id) = step["workId"].as_str().or_else(|| step["work_id"].as_str())
                else {
                    return false;
                };
                let Some(item) = work_items(work)
                    .iter()
                    .find(|item| work_id_of(item) == Some(work_id))
                else {
                    return false;
                };
                let work_rev = item["revision"].as_u64().unwrap_or(0);
                let fenced_rev = step["lastNotificationWorkRevision"]
                    .as_u64()
                    .or_else(|| step["last_notification_work_revision"].as_u64());
                step_fence_message_id(step).is_some()
                    && fenced_rev.is_some_and(|rev| rev != work_rev)
            })
}

fn unfenced_terminal_step(plan: &Value, work: &Value) -> bool {
    plan.pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|step| {
            let Some(work_id) = step["workId"].as_str().or_else(|| step["work_id"].as_str()) else {
                return false;
            };
            if step_fence_message_id(step).is_some() {
                return false;
            }
            work_items(work).iter().any(|item| {
                work_id_of(item) == Some(work_id)
                    && matches!(
                        item["state"].as_str(),
                        Some("succeeded" | "failed" | "cancelled" | "blocked")
                    )
            })
        })
}

fn failed_native_campaign_run(run: &Value, setup_run: &str) -> bool {
    run_id_of_value(run) != Some(setup_run)
        && run["purpose"].as_str() != Some("manager_proposal")
        && matches!(run["state"].as_str(), Some("failed" | "limit_reached"))
}

fn run_id_of_value(run: &Value) -> Option<&str> {
    run["runId"].as_str().or_else(|| run["run_id"].as_str())
}

fn unique_cut(
    cut: &str,
    plan: &Value,
    work: &Value,
    runs: &Value,
    intents: &Value,
    messages: &Value,
    decision_detail: Option<&Value>,
    setup_run: &str,
) -> bool {
    let plan_state = plan["plan"]["state"].as_str().unwrap_or_default();
    let decisions = work_kind_count(work, "manager-decision");
    let decision = decision_item(work);
    let decision_id = decision.and_then(work_id_of);
    let decision_intent = decision_id.and_then(|id| intent_for_work(intents, id));
    let decision_run = decision_intent.and_then(|intent| {
        intent["runId"]
            .as_str()
            .or_else(|| intent["run_id"].as_str())
            .filter(|id| !id.is_empty())
    });
    let decision_links = decision_detail.map(linked_run_ids).unwrap_or_default();
    let decision_succeeded = succeeded_kind_count(work, "manager-decision") >= 1;
    let has_fix = plan_has_step(plan, "step-b-fix");
    let fix_work_id = plan_step(plan, "step-b-fix").and_then(|step| step["workId"].as_str());
    let fix_succeeded = fix_work_id.is_some_and(|id| {
        work_items(work)
            .iter()
            .any(|item| work_id_of(item) == Some(id) && item["state"].as_str() == Some("succeeded"))
    });
    let campaign_runs: Vec<&Value> = runs["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|run| run_id_of_value(run) != Some(setup_run))
        .collect();
    let native_failed = campaign_runs
        .iter()
        .any(|run| failed_native_campaign_run(run, setup_run));
    match cut {
        "occurrence-reserved" => {
            plan_state == "active" && decisions == 0 && !has_fix && native_failed
        }
        "decision-work-persisted" => {
            decisions == 1
                && decision_intent.is_none()
                && decision_links.is_empty()
                && !decision_succeeded
                && !has_fix
        }
        "native-intent-persisted" => {
            decision_intent.is_some()
                && decision_run.is_none()
                && decision_links.is_empty()
                && !has_fix
        }
        "run-submitted" => {
            (!decision_links.is_empty() || decision_run.is_some())
                && !decision_succeeded
                && !has_fix
        }
        "directive-proposed" => {
            plan_state == "needs_replan"
                && !has_fix
                && campaign_runs.iter().any(|run| {
                    run["purpose"].as_str() == Some("manager_proposal")
                        && matches!(
                            run["state"].as_str(),
                            Some("completed" | "failed" | "limit_reached")
                        )
                })
        }
        "orchestration-mutation-persisted" => {
            has_fix && fix_work_id.is_none() && plan_state == "active"
        }
        "decision-applied-pending" => {
            has_fix && fix_work_id.is_some() && !fix_succeeded && plan_state == "active"
        }
        "notification-accepted-fence-pending" => {
            plan_state == "active"
                && decisions == 0
                && !has_fix
                && !native_failed
                && (fence_pending(plan, work, messages) || unfenced_terminal_step(plan, work))
        }
        "terminal-run-before-settlement" => {
            decisions == 0
                && !has_fix
                && succeeded_kind_count(work, "native") == 0
                && campaign_runs.iter().any(|run| {
                    run["state"].as_str() == Some("running")
                        && run["purpose"].as_str() != Some("manager_proposal")
                        && (pending_usage(run) > 0 || !usage_complete(run))
                })
        }
        _ => false,
    }
}

fn missed_unique_cut(
    cut: &str,
    plan: &Value,
    work: &Value,
    runs: &Value,
    intents: &Value,
    decision_detail: Option<&Value>,
    setup_run: &str,
) -> bool {
    let has_fix = plan_has_step(plan, "step-b-fix");
    let decisions = work_kind_count(work, "manager-decision");
    let decision = decision_item(work);
    let decision_id = decision.and_then(work_id_of);
    let decision_intent = decision_id.and_then(|id| intent_for_work(intents, id));
    let decision_run = decision_intent.and_then(|intent| {
        intent["runId"]
            .as_str()
            .or_else(|| intent["run_id"].as_str())
            .filter(|id| !id.is_empty())
    });
    let decision_succeeded = succeeded_kind_count(work, "manager-decision") >= 1;
    let plan_state = plan["plan"]["state"].as_str().unwrap_or_default();
    match cut {
        "native-intent-persisted" => {
            decision_run.is_some()
                || !decision_detail
                    .map(linked_run_ids)
                    .unwrap_or_default()
                    .is_empty()
                || decision_succeeded
                || has_fix
        }
        "decision-work-persisted" => decision_intent.is_some() || decision_succeeded || has_fix,
        "occurrence-reserved" => decisions >= 1 || has_fix,
        "terminal-run-before-settlement" => {
            succeeded_kind_count(work, "native") >= 1 || decisions >= 1 || has_fix
        }
        "notification-accepted-fence-pending" => {
            runs["runs"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|run| failed_native_campaign_run(run, setup_run))
                || decisions >= 1
                || has_fix
        }
        "directive-proposed" => has_fix || plan_state == "succeeded" || plan_state == "active",
        "orchestration-mutation-persisted" => {
            has_fix
                && plan_step(plan, "step-b-fix")
                    .and_then(|step| step["workId"].as_str())
                    .is_some()
        }
        "run-submitted" => decision_succeeded || has_fix,
        "decision-applied-pending" => {
            has_fix
                && plan_step(plan, "step-b-fix")
                    .and_then(|step| step["workId"].as_str())
                    .is_some_and(|id| {
                        work_items(work).iter().any(|item| {
                            work_id_of(item) == Some(id)
                                && item["state"].as_str() == Some("succeeded")
                        })
                    })
        }
        _ => plan_state == "succeeded",
    }
}

async fn observe(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
) -> (Value, Value, Value, Value, Value, Option<Value>) {
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
    let decision_detail = match decision_item(&work).and_then(work_id_of) {
        Some(id) => Some(
            call(
                client,
                "ptah_get_work",
                json!({
                    "session_id": session,
                    "workspace": workspace,
                    "work_id": id
                }),
            )
            .await,
        ),
        None => None,
    };
    (plan, work, runs, intents, messages, decision_detail)
}

async fn observe_for_cut(
    cut: &str,
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
) -> (Value, Value, Value, Value, Value, Option<Value>) {
    match cut {
        "native-intent-persisted" | "run-submitted" | "decision-work-persisted" => {
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
            let intents = list_intents(client, session, workspace).await;
            (
                plan,
                work,
                json!({"runs": []}),
                intents,
                json!({"messages": []}),
                None,
            )
        }
        "directive-proposed" => {
            let runs = list_runs(client, session, workspace).await;
            let proposal_done = runs["runs"].as_array().into_iter().flatten().any(|run| {
                run["purpose"].as_str() == Some("manager_proposal")
                    && matches!(
                        run["state"].as_str(),
                        Some("completed" | "failed" | "limit_reached")
                    )
            });
            if !proposal_done {
                return (
                    json!({"plan": {"state": "needs_replan"}}),
                    json!({"work": []}),
                    runs,
                    json!({"intents": []}),
                    json!({"messages": []}),
                    None,
                );
            }
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
            (
                plan,
                json!({"work": []}),
                runs,
                json!({"intents": []}),
                json!({"messages": []}),
                None,
            )
        }
        "orchestration-mutation-persisted" | "decision-applied-pending" => {
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
            (
                plan,
                work,
                json!({"runs": []}),
                json!({"intents": []}),
                json!({"messages": []}),
                None,
            )
        }
        "terminal-run-before-settlement" | "occurrence-reserved" => {
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
            (
                plan,
                work,
                runs,
                json!({"intents": []}),
                json!({"messages": []}),
                None,
            )
        }
        _ => observe(client, session, workspace, plan_id).await,
    }
}

fn assert_no_uncertain_resume(runs: &Value) {
    for run in runs["runs"].as_array().unwrap_or(&vec![]) {
        assert_no_secret_leak(run);
        if run["state"].as_str() == Some("interrupted") {
            assert_eq!(
                pending_usage_opt(run),
                Some(0),
                "interrupted run must project usagePendingRequests=0 immediately after restart: {run}"
            );
        }
    }
}

fn plan_projects_occurrence(plan: &Value) -> bool {
    let encoded = plan.to_string();
    encoded.contains("occurrenceId")
        || encoded.contains("occurrence_id")
        || encoded.contains("\"decisions\"")
        || encoded.contains("decisionId")
}

fn send_needle_for_cut(cut: &str) -> &'static str {
    match cut {
        "decision-work-persisted"
        | "native-intent-persisted"
        | "run-submitted"
        | "directive-proposed" => "Return exactly this JSON envelope",
        "orchestration-mutation-persisted" | "decision-applied-pending" => {
            "GROKBOT_SUCCESS complete the replacement step"
        }
        "notification-accepted-fence-pending" | "terminal-run-before-settlement" => {
            "GROKBOT_SUCCESS first native unit"
        }
        _ => "GROKBOT_SUCCESS",
    }
}

fn send_count_for_cut(provider: &FakeProvider, cut: &str) -> u64 {
    let needle = send_needle_for_cut(cut);
    match cut {
        "decision-work-persisted"
        | "native-intent-persisted"
        | "run-submitted"
        | "directive-proposed" => provider.sends_matching(needle),
        _ => provider.sends_matching_execution(needle),
    }
}

async fn focal_work_detail(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    cut: &str,
    plan: &Value,
    work: &Value,
) -> Option<Value> {
    let work_id = match cut {
        "orchestration-mutation-persisted" | "decision-applied-pending" => {
            plan_step(plan, "step-b-fix").and_then(|step| step["workId"].as_str())
        }
        "notification-accepted-fence-pending" | "terminal-run-before-settlement" => {
            work_items(work)
                .iter()
                .find(|item| item["kind"].as_str() == Some("native"))
                .and_then(work_id_of)
        }
        _ => decision_item(work).and_then(work_id_of),
    }?;
    Some(
        call(
            client,
            "ptah_get_work",
            json!({
                "session_id": session,
                "workspace": workspace,
                "work_id": work_id
            }),
        )
        .await,
    )
}

fn allowed_after_sends(before: u64) -> std::ops::RangeInclusive<u64> {
    if before == 0 {
        0..=1
    } else {
        before..=before
    }
}

fn assert_focal_after_restart(
    cut: &str,
    before_decision_ids: &std::collections::BTreeSet<String>,
    after_decision_ids: &std::collections::BTreeSet<String>,
    again_decision_ids: &std::collections::BTreeSet<String>,
    before_links: &[String],
    after_detail: Option<&Value>,
    again_detail: Option<&Value>,
    before_sends: u64,
    after_sends: u64,
    again_sends: u64,
) {
    if !before_decision_ids.is_empty() {
        assert_eq!(
            after_decision_ids, before_decision_ids,
            "restart replaced decision workId at {cut}: {before_decision_ids:?} vs {after_decision_ids:?}"
        );
    } else {
        assert!(
            after_decision_ids.len() <= 1,
            "restart created duplicate decisions at {cut}: {after_decision_ids:?}"
        );
    }
    assert_eq!(
        after_decision_ids, again_decision_ids,
        "duplicate tick created a new decision at {cut}"
    );
    let after_links = after_detail.map(linked_run_ids).unwrap_or_default();
    let again_links = again_detail.map(linked_run_ids).unwrap_or_default();
    if !before_links.is_empty() {
        assert_eq!(
            after_links, before_links,
            "restart changed linkedRunIds for the cut work at {cut}: {before_links:?} vs {after_links:?}"
        );
        assert_eq!(
            after_links.len(),
            1,
            "cut work must keep exactly one linkedRunId at {cut}: {after_links:?}"
        );
    } else {
        assert!(
            after_links.len() <= 1,
            "restart linked more than one Run for the cut work at {cut}: {after_links:?}"
        );
    }
    assert_eq!(
        after_links, again_links,
        "duplicate tick created linkedRunIds at {cut}: {after_links:?} vs {again_links:?}"
    );
    assert!(
        allowed_after_sends(before_sends).contains(&after_sends) && after_sends == again_sends,
        "restart provider sends for the cut attempt at {cut}: before={before_sends} after={after_sends} again={again_sends}"
    );
    if let Some(detail) = after_detail {
        let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        if !attempts.is_empty() {
            assert_eq!(
                attempts.len(),
                1,
                "cut work must have exactly one attempt at {cut}: {detail}"
            );
            let links = linked_run_ids(detail);
            assert!(
                links.len() <= 1,
                "cut work attempt must have at most one linkedRunId at {cut}: {detail}"
            );
        }
    }
}

#[test]
fn allowed_after_sends_known_not_sent_may_send_once() {
    let cases: &[(u64, u64, bool)] = &[
        (0, 0, true),
        (0, 1, true),
        (1, 1, true),
        (1, 2, false),
        (2, 3, false),
    ];
    for (before, after, ok) in cases {
        assert_eq!(
            allowed_after_sends(*before).contains(after),
            *ok,
            "allowed_after_sends({before}).contains({after}) should be {ok}"
        );
    }
}

async fn assert_attempt_run_cardinalities(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    work: &Value,
    campaign_sends: u64,
) {
    let mut attempts = 0usize;
    let mut linked = 0usize;
    for item in work_items(work) {
        let kind = item["kind"].as_str().unwrap_or("");
        if kind != "native" && kind != "manager-decision" {
            continue;
        }
        let id = work_id_of(item).expect("workId");
        let detail = call(
            client,
            "ptah_get_work",
            json!({
                "session_id": session,
                "workspace": workspace,
                "work_id": id
            }),
        )
        .await;
        let item_attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            item_attempts.len(),
            1,
            "expected exactly one attempt for {id}: {detail}"
        );
        for attempt in &item_attempts {
            attempts += 1;
            let runs = attempt["linkedRunIds"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                runs.len(),
                1,
                "expected exactly one linkedRunId for attempt {}: {attempt}",
                attempt["attemptId"]
            );
            linked += 1;
        }
    }
    assert_eq!(attempts, 4, "expected attempts for a, b, decision, b-fix");
    assert_eq!(linked, 4, "expected four linkedRunIds");
    assert_eq!(
        campaign_sends, attempts as u64,
        "expected one provider send per Work attempt, got {campaign_sends} sends for {attempts} attempts"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_lifecycle_reaches_succeeded_without_duplicate_sends() {
    let _serial = serial_lock();
    let provider = FakeProvider::start();
    let service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id, _setup_run) = bootstrap_agent(&mut client, &service.workspace).await;
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
    let decision_id = decision_item(&work)
        .and_then(work_id_of)
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
    let messages = call(
        &mut client,
        "ptah_list_inbox",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "agent_id": agent_id
        }),
    )
    .await;

    assert_eq!(plan["plan"]["state"].as_str(), Some("succeeded"));
    assert_eq!(work_kind_count(&work, "manager-decision"), 1);
    assert_eq!(succeeded_kind_count(&work, "manager-decision"), 1);
    assert_eq!(
        succeeded_kind_count(&work, "native"),
        2,
        "expected native success for step-a and replacement: {work}"
    );
    assert!(
        plan_has_step(&plan, "step-b-fix"),
        "replacement step missing: {plan}"
    );
    let campaign_sends = provider.send_count().saturating_sub(setup_sends);
    assert_attempt_run_cardinalities(
        &mut client,
        session,
        &service.workspace,
        &work,
        campaign_sends,
    )
    .await;
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
    assert!(
        messages["messages"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "expected manager notifications: {messages}"
    );
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
    assert_eq!(after.plan_revision, again.plan_revision);
    assert_eq!(after.quota_reservations, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occurrence_is_not_a_public_mcp_field() {
    let _serial = serial_lock();
    let provider = FakeProvider::start();
    let service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id, _setup) = bootstrap_agent(&mut client, &service.workspace).await;
    let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
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
    let work = list_work(&mut client, session, &service.workspace).await;
    assert!(
        !plan_projects_occurrence(&plan),
        "ptah_get_manager_plan must not project occurrence at 67e29bd: {plan}"
    );
    assert_eq!(
        work_kind_count(&work, "manager-decision"),
        0,
        "fresh plan must not already have decision Work: {work}"
    );
    let encoded = format!("{plan}{work}");
    assert!(
        !encoded.contains("occurrenceId") && !encoded.contains("occurrence_id"),
        "occurrence must not appear on public MCP at this SHA: {encoded}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_restart_cuts_do_not_duplicate_or_resume_uncertain_attempts() {
    let _serial = serial_lock();
    for cut in CUTS {
        let mut caught = false;
        for attempt in 0..20 {
            let provider = FakeProvider::start();
            let mut service = ServiceProcess::spawn(&provider.base_url);
            let mut client = mcp(&service.addr).await;
            let (session, agent_id, setup_run) =
                bootstrap_agent(&mut client, &service.workspace).await;
            let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
            tick_twice(&mut client, session, &service.workspace, &plan_id).await;
            let deadline = Instant::now() + Duration::from_secs(40);
            let mut hit = false;
            let mut last_dump = json!({});
            while Instant::now() < deadline {
                let (plan, work, runs, intents, messages, decision_detail) =
                    observe_for_cut(cut, &mut client, session, &service.workspace, &plan_id).await;
                last_dump = json!({
                    "planState": plan["plan"]["state"],
                    "decisions": work_kind_count(&work, "manager-decision"),
                    "intentStates": intents["intents"].as_array().map(|items| {
                        items.iter().map(|intent| json!({
                            "workId": intent["workId"],
                            "runId": intent["runId"],
                            "state": intent["state"]
                        })).collect::<Vec<_>>()
                    }),
                    "runStates": runs["runs"].as_array().map(|items| {
                        items.iter().map(|run| json!({
                            "state": run["state"],
                            "purpose": run["purpose"],
                            "pending": pending_usage(run)
                        })).collect::<Vec<_>>()
                    }),
                    "hasFix": plan_has_step(&plan, "step-b-fix"),
                });
                if unique_cut(
                    cut,
                    &plan,
                    &work,
                    &runs,
                    &intents,
                    &messages,
                    decision_detail.as_ref(),
                    &setup_run,
                ) {
                    hit = true;
                    break;
                }
                if missed_unique_cut(
                    cut,
                    &plan,
                    &work,
                    &runs,
                    &intents,
                    decision_detail.as_ref(),
                    &setup_run,
                ) {
                    last_dump["missed"] = json!(true);
                    break;
                }
            }
            if !hit {
                eprintln!(
                    "cut {cut} attempt {attempt} missed unique window last={last_dump}; retrying"
                );
                continue;
            }
            let before_plan = call(
                &mut client,
                "ptah_get_manager_plan",
                json!({
                    "session_id": session,
                    "workspace": &service.workspace,
                    "plan_id": plan_id
                }),
            )
            .await;
            let before_work = list_work(&mut client, session, &service.workspace).await;
            let before_detail = focal_work_detail(
                &mut client,
                session,
                &service.workspace,
                cut,
                &before_plan,
                &before_work,
            )
            .await;
            let before_links = before_detail
                .as_ref()
                .map(linked_run_ids)
                .unwrap_or_default();
            let before = collect_identity(
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
            let post_respawn_runs = list_runs(&mut client, session, &service.workspace).await;
            assert_no_uncertain_resume(&post_respawn_runs);
            let reopen_sends = send_count_for_cut(&provider, cut);
            tick_twice(&mut client, session, &service.workspace, &plan_id).await;
            let after_plan = call(
                &mut client,
                "ptah_get_manager_plan",
                json!({
                    "session_id": session,
                    "workspace": &service.workspace,
                    "plan_id": plan_id
                }),
            )
            .await;
            let after_work = list_work(&mut client, session, &service.workspace).await;
            let after_detail = focal_work_detail(
                &mut client,
                session,
                &service.workspace,
                cut,
                &after_plan,
                &after_work,
            )
            .await;
            let after = collect_identity(
                &mut client,
                session,
                &service.workspace,
                &plan_id,
                provider.send_count(),
            )
            .await;
            let after_sends = send_count_for_cut(&provider, cut);
            assert_no_uncertain_resume(&list_runs(&mut client, session, &service.workspace).await);
            tick_twice(&mut client, session, &service.workspace, &plan_id).await;
            let again_plan = call(
                &mut client,
                "ptah_get_manager_plan",
                json!({
                    "session_id": session,
                    "workspace": &service.workspace,
                    "plan_id": plan_id
                }),
            )
            .await;
            let again_work = list_work(&mut client, session, &service.workspace).await;
            let again_detail = focal_work_detail(
                &mut client,
                session,
                &service.workspace,
                cut,
                &again_plan,
                &again_work,
            )
            .await;
            let again = collect_identity(
                &mut client,
                session,
                &service.workspace,
                &plan_id,
                provider.send_count(),
            )
            .await;
            let again_sends = send_count_for_cut(&provider, cut);
            assert_exact_after_restart(&before, &after, &again, cut);
            assert_focal_after_restart(
                cut,
                &before.decision_ids,
                &after.decision_ids,
                &again.decision_ids,
                &before_links,
                after_detail.as_ref(),
                again_detail.as_ref(),
                reopen_sends,
                after_sends,
                again_sends,
            );
            caught = true;
            break;
        }
        assert!(caught, "never observed unique durable state for cut {cut}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_closed_permission_stale_invalid_cancel_quota_absent() {
    let _serial = serial_lock();
    let provider = FakeProvider::start_with(ProviderScript::InvalidDirective);
    let service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let (session, agent_id, _setup_run) = bootstrap_agent(&mut client, &service.workspace).await;

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
            "prompt": "GROKBOT_CANCEL_TARGET cancel target"
        }),
    )
    .await;
    let run_id = submitted["runId"].as_str().unwrap().to_string();
    let cancelled = call(
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
    assert!(
        cancelled["cancelled"].as_bool() == Some(true)
            || cancelled["state"].as_str() == Some("cancelled")
            || cancelled["run"]["state"].as_str() == Some("cancelled"),
        "ptah_cancel did not report cancellation: {cancelled}"
    );
    let run = poll_json(
        &mut client,
        "ptah_get_run",
        json!({
            "session_id": session,
            "workspace": &service.workspace,
            "run_id": run_id
        }),
        |value| {
            !matches!(value["state"].as_str(), Some("running" | "queued"))
                && value["state"].as_str().is_some()
        },
    )
    .await;
    assert_eq!(
        run["state"].as_str(),
        Some("cancelled"),
        "cancelled run did not stop: {run}"
    );

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
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let provider = FakeProvider::start();
    let mut service = ServiceProcess::spawn(&provider.base_url);
    let mut client = mcp(&service.addr).await;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let commit = git_head(&repo);
    let disk_start = home_bytes(&service.home);
    let rss_start = rss_kb(service.pid());
    let mut cycles = 0u64;
    let mut restarts = 0u64;
    let mut last_identity = None;
    while Instant::now() < deadline {
        let (session, agent_id, _setup) = bootstrap_agent(&mut client, &service.workspace).await;
        let plan_id = create_plan(&mut client, session, &service.workspace, &agent_id).await;
        tick_twice(&mut client, session, &service.workspace, &plan_id).await;
        let _ = wait_plan_state(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            "succeeded",
        )
        .await;
        let identity = collect_identity(
            &mut client,
            session,
            &service.workspace,
            &plan_id,
            provider.send_count(),
        )
        .await;
        assert_eq!(
            identity.decision_ids.len(),
            1,
            "soak cycle expected exactly one decision: {identity:?}"
        );
        last_identity = Some(identity);
        cycles += 1;
        if cycles % 2 == 1 && Instant::now() < deadline {
            service.respawn(&provider.base_url);
            client = mcp(&service.addr).await;
            restarts += 1;
        }
    }
    assert!(cycles >= 1, "soak completed zero cycles");
    let disk_end = home_bytes(&service.home);
    let rss_end = rss_kb(service.pid());
    assert!(
        disk_end <= disk_start.saturating_add(cycles.saturating_mul(4 * 1024 * 1024)),
        "runtime home grew unbounded: {disk_start} -> {disk_end} over {cycles} cycles"
    );
    if rss_start > 0 && rss_end > 0 {
        assert!(
            rss_end <= rss_start.saturating_mul(8).max(rss_start + 512 * 1024),
            "rss grew unbounded: {rss_start} -> {rss_end} KB"
        );
    }
    let duration = started.elapsed().as_secs();
    let identity = last_identity.unwrap_or_default();
    let report = json!({
        "commit": commit,
        "seed": SEED,
        "durationSeconds": duration,
        "restartCount": restarts,
        "cycleCount": cycles,
        "providerSends": provider.send_count(),
        "invariantCounts": {
            "workIds": identity.work_ids.len(),
            "runIds": identity.run_ids.len(),
            "linkedRunIds": identity.linked_run_ids.len(),
            "decisions": identity.decision_ids.len(),
            "messages": identity.message_ids.len(),
            "planRevision": identity.plan_revision,
            "quotaReservations": 0
        },
        "homeBytesStart": disk_start,
        "homeBytesEnd": disk_end,
        "rssKbStart": rss_start,
        "rssKbEnd": rss_end,
        "snapshotHash": identity.hash_hex()
    });
    let encoded = serde_json::to_vec_pretty(&report).expect("encode soak json");
    let sha = openssl_sha256_hex(&encoded);
    let mut report = report;
    report["reportSha256"] = json!(sha);
    let markdown = format!(
        "# Always-on Grokbot soak\n\n- commit: `{commit}`\n- seed: `{SEED}`\n- durationSeconds: {duration}\n- restartCount: {restarts}\n- cycleCount: {cycles}\n- providerSends: {}\n- snapshotHash: `{}`\n- reportSha256: `{sha}`\n- homeBytes: {disk_start} -> {disk_end}\n- rssKb: {rss_start} -> {rss_end}\n",
        provider.send_count(),
        identity.hash_hex()
    );
    let out_dir = std::env::var("GROKBOT_SOAK_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| service.home.join("soak"));
    std::fs::create_dir_all(&out_dir).expect("soak out dir");
    let json_path = out_dir.join("soak-report.json");
    let md_path = out_dir.join("soak-report.md");
    std::fs::write(&json_path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    std::fs::write(&md_path, markdown.as_bytes()).unwrap();
    eprintln!(
        "soak report json={} markdown={}",
        json_path.display(),
        md_path.display()
    );
}
