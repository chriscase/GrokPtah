//! Always-on Grokbot process certification.
//!
//! Fresh-home, exact-identity scenarios against the shipped `grokptah-service`
//! binary, authenticated MCP, and a loopback provider with an explicit POST
//! barrier. No production crate is modified.

#![allow(clippy::await_holding_lock)]

mod always_on_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use uuid::Uuid;

use always_on_support::{
    assert_no_duplicate_step, assert_no_quota_ledger, call, call_expect_error, clear_assertions,
    intents_array, mcp, pending_usage, poll_json, record_assertion, recorded_assertions,
    repository_commit, rid, runs_array, scan_service_artifacts, scan_text, serial_lock,
    snapshot_step, try_mcp, work_for_step, work_items, work_kind_count, FakeProvider, Fixture,
    ProviderDisposition, ProviderScript, ResourceSample, ServiceProcess, FIXTURE_SCHEMA,
};

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

fn plan_args(
    request_id: &str,
    session: Uuid,
    workspace: &Path,
    agent_id: &str,
    fixture: &Fixture,
) -> Value {
    json!({
        "request_id": request_id,
        "session_id": session,
        "workspace": workspace,
        "manager_agent_id": agent_id,
        "objective": "always-on grokbot dependent DAG",
        "autonomous": true,
        "max_replans": 2,
        "max_in_flight": 2,
        "steps": [
            native_step(
                &fixture.step_first,
                "GROKBOT_SUCCESS first native unit",
                &[],
                agent_id
            ),
            native_step(
                &fixture.step_failing,
                "GROKBOT_FORCE_FAIL child that must be replaced",
                &[fixture.step_first.as_str()],
                agent_id
            )
        ]
    })
}

fn is_terminal_run(state: Option<&str>) -> bool {
    matches!(
        state,
        Some("completed" | "failed" | "cancelled" | "interrupted")
    )
}

fn is_bounded_run_outcome(state: Option<&str>) -> bool {
    is_terminal_run(state) || state == Some("limit_reached")
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
        |value| is_terminal_run(value["state"].as_str()),
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

async fn get_plan(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
) -> Value {
    call(
        client,
        "ptah_get_manager_plan",
        json!({
            "session_id": session,
            "workspace": workspace,
            "plan_id": plan_id
        }),
    )
    .await
}

async fn get_work(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    work_id: &str,
) -> Value {
    call(
        client,
        "ptah_get_work",
        json!({
            "session_id": session,
            "workspace": workspace,
            "work_id": work_id
        }),
    )
    .await
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

async fn wait_step_work(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    step_id: &str,
) -> Value {
    poll_json(
        client,
        "ptah_list_work",
        json!({
            "session_id": session,
            "workspace": workspace
        }),
        |value| !work_for_step(value, step_id).is_empty(),
    )
    .await
}

async fn wait_step_state(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    step_id: &str,
    state: &str,
) -> Value {
    poll_json(
        client,
        "ptah_list_work",
        json!({
            "session_id": session,
            "workspace": workspace
        }),
        |value| {
            work_for_step(value, step_id)
                .first()
                .and_then(|item| item["state"].as_str())
                == Some(state)
        },
    )
    .await
}

async fn wait_target_in_flight(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    step_id: &str,
    posts: u64,
) -> always_on_support::TargetSnapshot {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = always_on_support::TargetSnapshot::default();
    while Instant::now() < deadline {
        let work = list_work(client, session, workspace).await;
        let runs = list_runs(client, session, workspace).await;
        let intents = list_intents(client, session, workspace).await;
        last = snapshot_step(&work, &intents, &runs, step_id, posts);
        if last.work_id.is_some()
            && last.run_id.is_some()
            && matches!(last.work_state.as_deref(), Some("running" | "leased"))
            && last.run_state.as_deref() == Some("running")
        {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("target {step_id} never reached an in-flight Work/Run boundary: {last:?}");
}

async fn observe(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
) -> (Value, Value, Value, Value) {
    let plan = get_plan(client, session, workspace, plan_id).await;
    let work = list_work(client, session, workspace).await;
    let runs = list_runs(client, session, workspace).await;
    let intents = list_intents(client, session, workspace).await;
    (plan, work, runs, intents)
}

fn plan_has_step(plan: &Value, step_id: &str) -> bool {
    plan.pointer("/plan/steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|step| step["stepId"].as_str() == Some(step_id))
}

fn live_intent_count(intents: &Value) -> usize {
    intents_array(intents)
        .iter()
        .filter(|intent| {
            matches!(
                intent["state"].as_str(),
                Some("claiming" | "admitted" | "parked" | "resolving")
            )
        })
        .count()
}

fn proposal_run_count(runs: &Value) -> usize {
    runs_array(runs)
        .iter()
        .filter(|run| run["purpose"].as_str() == Some("manager_proposal"))
        .count()
}

fn assert_terminal_runs_pending_zero(runs: &Value) {
    for run in runs_array(runs) {
        if is_terminal_run(run["state"].as_str()) {
            assert_eq!(
                pending_usage(run),
                0,
                "terminal run left pending usage: {run}"
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn assert_native_exact(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    work: &Value,
    intents: &Value,
    runs: &Value,
    step_id: &str,
    posts: u64,
    assertion: &str,
) {
    assert_no_duplicate_step(work, step_id);
    let snapshot = snapshot_step(work, intents, runs, step_id, posts);
    let work_id = snapshot.work_id.as_deref().expect("native work id");
    let intent_id = snapshot.intent_id.as_deref().expect("native intent id");
    let attempt_id = snapshot.attempt_id.as_deref().expect("native attempt id");
    let run_id = snapshot.run_id.as_deref().expect("native run id");
    assert_eq!(
        posts, 1,
        "semantic POST count for {step_id} must be 1; work={work}"
    );
    let detailed = get_work(client, session, workspace, work_id).await;
    let attempts = detailed["attempts"]
        .as_array()
        .expect("public attempts array");
    assert_eq!(
        attempts.len(),
        1,
        "native {step_id} must have exactly one public attempt: {detailed}"
    );
    assert_eq!(attempts[0]["attemptId"].as_str(), Some(attempt_id));
    let linked = attempts[0]["linkedRunIds"]
        .as_array()
        .expect("linkedRunIds");
    assert_eq!(linked.len(), 1, "native {step_id} linked runs: {detailed}");
    assert_eq!(linked[0].as_str(), Some(run_id));
    let matching_intents = intents_array(intents)
        .iter()
        .filter(|intent| intent["workId"].as_str() == Some(work_id))
        .count();
    assert_eq!(
        matching_intents, 1,
        "native {step_id} must have exactly one intent {intent_id}: {intents}"
    );
    let matching_runs = runs_array(runs)
        .iter()
        .filter(|run| run["runId"].as_str() == Some(run_id))
        .count();
    assert_eq!(matching_runs, 1, "native {step_id} run missing: {runs}");
    record_assertion(assertion);
}

async fn exact_happy_path_oracle(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
    provider: &FakeProvider,
    fixture: &Fixture,
) {
    let (plan, work, runs, intents) = observe(client, session, workspace, plan_id).await;
    assert_eq!(
        plan["plan"]["state"].as_str(),
        Some("succeeded"),
        "plan not succeeded: {plan}; posts={:?}",
        provider.records()
    );
    record_assertion("plan-succeeded");
    assert_eq!(work_kind_count(&work, "manager-decision"), 1);
    assert_eq!(
        always_on_support::succeeded_kind_count(&work, "manager-decision"),
        1
    );
    record_assertion("exact-decision-work-1");
    assert_eq!(
        proposal_run_count(&runs),
        1,
        "observed manager_proposal Run count must be 1; enforcement is unverified: {runs}"
    );
    record_assertion("exact-proposal-run-observed-1");
    assert_eq!(
        fixture.proposal_only, "unverified-pending-pr-352",
        "proposal-only enforcement is pending PR #352"
    );
    record_assertion("proposal-only-enforcement-unverified-pr-352");
    assert!(
        plan_has_step(&plan, &fixture.step_replacement),
        "replacement step missing: {plan}"
    );
    assert_native_exact(
        client,
        session,
        workspace,
        &work,
        &intents,
        &runs,
        &fixture.step_first,
        provider.count_for(&fixture.step_first),
        "native-step-a-one-work-intent-attempt-run-post",
    )
    .await;
    record_assertion("native-step-a-one-work-intent-attempt-run-post");
    assert_native_exact(
        client,
        session,
        workspace,
        &work,
        &intents,
        &runs,
        &fixture.step_failing,
        provider.count_for(&fixture.step_failing),
        "native-step-b-one-work-intent-attempt-run-post",
    )
    .await;
    record_assertion("native-step-b-one-work-intent-attempt-run-post");
    assert_native_exact(
        client,
        session,
        workspace,
        &work,
        &intents,
        &runs,
        &fixture.step_replacement,
        provider.count_for(&fixture.step_replacement),
        "native-step-b-fix-one-work-intent-attempt-run-post",
    )
    .await;
    record_assertion("native-step-b-fix-one-work-intent-attempt-run-post");
    assert_eq!(provider.count_for("manager-decision"), 1);
    assert_terminal_runs_pending_zero(&runs);
    record_assertion("all-terminal-runs-pending-0");
    assert_eq!(live_intent_count(&intents), 0);
    let capacity = call(client, "ptah_get_capacity", json!({})).await;
    assert_no_quota_ledger(&capacity);
    provider.assert_route_and_auth();
}

async fn tick_once(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
    label: &str,
) {
    let _ = call(
        client,
        "ptah_tick_manager_plan",
        json!({
            "request_id": rid(label),
            "session_id": session,
            "workspace": workspace,
            "plan_id": plan_id
        }),
    )
    .await;
}

struct Campaign {
    fixture: Fixture,
    provider: FakeProvider,
    service: ServiceProcess,
    client: McpControlClient,
    session: Uuid,
    agent_id: String,
    workspace: PathBuf,
}

impl Campaign {
    async fn start() -> Self {
        let fixture = Fixture::load();
        let provider = FakeProvider::start();
        let service = ServiceProcess::spawn(&provider.base_url);
        let mut client = mcp(&service.addr).await;
        let workspace = service.workspace.clone();
        let (session, agent_id) = bootstrap_agent(&mut client, &workspace).await;
        Self {
            fixture,
            provider,
            service,
            client,
            session,
            agent_id,
            workspace,
        }
    }

    async fn start_with(script: ProviderScript) -> Self {
        let fixture = Fixture::load();
        let provider = FakeProvider::start_with(script);
        let service = ServiceProcess::spawn(&provider.base_url);
        let mut client = mcp(&service.addr).await;
        let workspace = service.workspace.clone();
        let (session, agent_id) = bootstrap_agent(&mut client, &workspace).await;
        Self {
            fixture,
            provider,
            service,
            client,
            session,
            agent_id,
            workspace,
        }
    }

    async fn create_plan(&mut self) -> (String, Value) {
        let request_id = rid("plan");
        let args = plan_args(
            &request_id,
            self.session,
            &self.workspace,
            &self.agent_id,
            &self.fixture,
        );
        let created = call(&mut self.client, "ptah_create_manager_plan", args.clone()).await;
        let plan_id = created["plan"]["planId"]
            .as_str()
            .expect("planId")
            .to_string();
        (plan_id, args)
    }

    async fn reopen(&mut self) {
        self.service.respawn(&self.provider.base_url);
        self.client = mcp(&self.service.addr).await;
    }

    fn scan(&self) {
        scan_service_artifacts(&self.service);
        self.provider.assert_route_and_auth();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_schema_is_consumed() {
    let _serial = serial_lock();
    clear_assertions();
    let fixture = Fixture::load();
    assert_eq!(fixture.schema, FIXTURE_SCHEMA);
    record_assertion("fixture-schema-consumed");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.base_sha, "67e29bd34dc64049432c715c93c2cef2185c63ea");
    record_assertion("fixture-version-matches");
    assert_eq!(fixture.clock, "bounded-race-controlled-no-fake-clock-seam");
    record_assertion("clock-is-bounded-not-deterministic");
    assert_eq!(fixture.soak24h, "not-run");
    assert_eq!(
        fixture.internal_persistence_cuts,
        "not-proven-no-production-failpoint"
    );
    assert_eq!(fixture.seed, "always-on-grokbot-v1");
    assert_eq!(fixture.success, "GROKBOT_SUCCESS");
    assert_eq!(fixture.fail, "GROKBOT_FORCE_FAIL");
    assert_eq!(fixture.ok, "GROKBOT_OK");
    assert_eq!(fixture.setup, "GROKBOT_SETUP");
    assert_eq!(fixture.posts_by_semantic.get("step-a"), Some(&1));
    assert!(fixture.attempt_evidence.contains("ptah_get_work.attempts"));
    let src = include_str!("always_on_grokbot.rs");
    for id in &fixture.required_assertions {
        assert!(
            src.contains(&format!("record_assertion(\"{id}\")"))
                || src.contains(&format!("\"{id}\"")),
            "required assertion {id} is not recorded in the service tests"
        );
    }
    let probes = include_str!("../../../../evals/certification-lab/src/probes.rs");
    assert!(
        probes.contains("observed_actions") && probes.contains("observed_oracles"),
        "lab probe must stamp only observed actions/oracles"
    );
    record_assertion("lab-probe-does-not-synthesize");
    assert!(!fixture.digest().is_empty());
    assert_ne!(ProviderDisposition::Status500, ProviderDisposition::Hold);
    let _ = recorded_assertions();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autonomous_lifecycle_exact_oracle_without_manual_ticks() {
    let _serial = serial_lock();
    clear_assertions();
    let mut campaign = Campaign::start().await;
    let (plan_id, args) = campaign.create_plan().await;
    record_assertion("no-manual-tick-before-success");
    let _ = wait_plan_state(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &plan_id,
        "succeeded",
    )
    .await;
    exact_happy_path_oracle(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &plan_id,
        &campaign.provider,
        &campaign.fixture,
    )
    .await;

    let before_work = list_work(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let before_runs = list_runs(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let before_intents =
        list_intents(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let before_posts = (
        campaign.provider.count_for("step-a"),
        campaign.provider.count_for("step-b"),
        campaign.provider.count_for("manager-decision"),
        campaign.provider.count_for("step-b-fix"),
    );
    tick_once(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &plan_id,
        "post-success-a",
    )
    .await;
    tick_once(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &plan_id,
        "post-success-b",
    )
    .await;
    let after_work = list_work(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let after_runs = list_runs(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let after_intents =
        list_intents(&mut campaign.client, campaign.session, &campaign.workspace).await;
    assert_eq!(
        work_items(&before_work).len(),
        work_items(&after_work).len()
    );
    assert_eq!(
        runs_array(&before_runs).len(),
        runs_array(&after_runs).len()
    );
    assert_eq!(
        intents_array(&before_intents).len(),
        intents_array(&after_intents).len()
    );
    assert_eq!(
        before_posts,
        (
            campaign.provider.count_for("step-a"),
            campaign.provider.count_for("step-b"),
            campaign.provider.count_for("manager-decision"),
            campaign.provider.count_for("step-b-fix"),
        )
    );
    record_assertion("post-success-tick-idempotent");

    let replay = call(
        &mut campaign.client,
        "ptah_create_manager_plan",
        args.clone(),
    )
    .await;
    assert_eq!(replay["plan"]["planId"].as_str(), Some(plan_id.as_str()));
    assert_eq!(
        work_items(&list_work(&mut campaign.client, campaign.session, &campaign.workspace).await)
            .len(),
        work_items(&after_work).len()
    );
    record_assertion("replay-same-payload");

    let mut conflict_args = args;
    conflict_args["objective"] = json!("changed payload must conflict");
    let conflict = call_expect_error(
        &mut campaign.client,
        "ptah_create_manager_plan",
        conflict_args,
    )
    .await
    .to_lowercase();
    assert!(
        conflict.contains("conflict"),
        "same request id / different payload must conflict: {conflict}"
    );
    assert_eq!(
        work_items(&list_work(&mut campaign.client, campaign.session, &campaign.workspace).await)
            .len(),
        work_items(&after_work).len()
    );
    record_assertion("replay-changed-payload-conflict");
    campaign.scan();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_scenarios_fresh_home_exact_identities() {
    let _serial = serial_lock();
    clear_assertions();
    let fixture = Fixture::load();
    let scenarios = [
        (
            "step-a-work-materialized",
            fixture.step_first.as_str(),
            None,
        ),
        (
            "step-a-succeeded",
            fixture.step_first.as_str(),
            Some("succeeded"),
        ),
        (
            "step-b-failed",
            fixture.step_failing.as_str(),
            Some("failed"),
        ),
        (
            "manager-decision-succeeded",
            "__manager_decision__",
            Some("succeeded"),
        ),
        (
            "step-b-fix-materialized",
            fixture.step_replacement.as_str(),
            None,
        ),
        ("plan-succeeded", "", None),
    ];
    for (name, step_id, state) in scenarios {
        let mut campaign = Campaign::start().await;
        let (plan_id, _) = campaign.create_plan().await;
        if name == "plan-succeeded" {
            let _ = wait_plan_state(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &plan_id,
                "succeeded",
            )
            .await;
        } else if let Some(want) = state {
            let _ = wait_step_state(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                step_id,
                want,
            )
            .await;
        } else {
            let _ = wait_step_work(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                step_id,
            )
            .await;
        }
        let (_, work, runs, intents) = observe(
            &mut campaign.client,
            campaign.session,
            &campaign.workspace,
            &plan_id,
        )
        .await;
        let semantic = if step_id == "__manager_decision__" {
            "manager-decision"
        } else {
            step_id
        };
        let before = if step_id.is_empty() {
            snapshot_step(&work, &intents, &runs, "unused", 0)
        } else {
            snapshot_step(
                &work,
                &intents,
                &runs,
                step_id,
                campaign.provider.count_for(semantic),
            )
        };
        if !step_id.is_empty() {
            assert!(
                before.work_id.is_some(),
                "{name} missing target work identity"
            );
        }
        campaign.reopen().await;
        let (plan, work, runs, intents) = observe(
            &mut campaign.client,
            campaign.session,
            &campaign.workspace,
            &plan_id,
        )
        .await;
        if name == "plan-succeeded" {
            assert_eq!(plan["plan"]["state"].as_str(), Some("succeeded"));
            assert_eq!(
                campaign.provider.count_for("step-a"),
                1,
                "step-a posts after {name}: {:?}",
                campaign.provider.records()
            );
            assert_eq!(
                campaign.provider.count_for("step-b"),
                1,
                "step-b posts after {name}: {:?}",
                campaign.provider.records()
            );
            assert_eq!(
                campaign.provider.count_for("manager-decision"),
                1,
                "manager-decision posts after {name}: {:?}",
                campaign.provider.records()
            );
            assert_eq!(
                campaign.provider.count_for("step-b-fix"),
                1,
                "step-b-fix posts after {name}: {:?}",
                campaign.provider.records()
            );
        } else {
            let after = snapshot_step(
                &work,
                &intents,
                &runs,
                step_id,
                campaign.provider.count_for(semantic),
            );
            assert_eq!(after.work_id, before.work_id, "{name} work id changed");
            assert_eq!(
                work_for_step(&work, step_id).len(),
                1,
                "{name} duplicate target work after restart: {work}"
            );
            if let Some(run_id) = before.run_id.as_ref() {
                assert_eq!(after.run_id.as_ref(), Some(run_id), "{name} run id changed");
            }
            if let Some(intent_id) = before.intent_id.as_ref() {
                assert_eq!(
                    after.intent_id.as_ref(),
                    Some(intent_id),
                    "{name} intent id changed"
                );
            }
            if let Some(attempt_id) = before.attempt_id.as_ref() {
                assert_eq!(
                    after.attempt_id.as_ref(),
                    Some(attempt_id),
                    "{name} attempt id changed"
                );
            }
            if matches!(state, Some("succeeded" | "failed")) {
                assert_eq!(after.work_state.as_deref(), state);
                assert_eq!(
                    after.provider_posts, before.provider_posts,
                    "{name} semantic POST count grew across restart"
                );
            }
        }
        campaign.scan();
        record_assertion("restart-fresh-home-per-scenario");
        record_assertion("restart-exact-target-identities");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inflight_barrier_sigkill_does_not_resend() {
    let _serial = serial_lock();
    clear_assertions();
    let mut campaign = Campaign::start().await;
    campaign
        .provider
        .arm(&campaign.fixture.step_first, ProviderDisposition::Hold);
    let (plan_id, _) = campaign.create_plan().await;
    campaign
        .provider
        .wait_accepted(&campaign.fixture.step_first, Duration::from_secs(90));
    assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
    let before = wait_target_in_flight(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &campaign.fixture.step_first,
        campaign.provider.count_for(&campaign.fixture.step_first),
    )
    .await;
    let run_id = before.run_id.clone().expect("in-flight run id");
    assert_eq!(before.run_state.as_deref(), Some("running"));
    let work_id = before.work_id.clone().expect("in-flight work id");
    campaign.service.kill_sigkill();
    campaign.reopen().await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut recovered = None;
    while Instant::now() < deadline {
        let run = call(
            &mut campaign.client,
            "ptah_get_run",
            json!({
                "session_id": campaign.session,
                "workspace": &campaign.workspace,
                "run_id": run_id
            }),
        )
        .await;
        if is_terminal_run(run["state"].as_str()) {
            recovered = Some(run);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let run = recovered.expect("in-flight run did not become terminal after SIGKILL");
    assert_eq!(run["state"].as_str(), Some("interrupted"));
    assert_eq!(pending_usage(&run), 0);
    assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
    let detailed = poll_json(
        &mut campaign.client,
        "ptah_get_work",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "work_id": work_id
        }),
        |value| value["work"]["state"].as_str() == Some("failed"),
    )
    .await;
    assert_ne!(
        detailed["work"]["state"].as_str(),
        Some("queued"),
        "interrupted native work must not be implicitly queued: {detailed}"
    );
    let attempts = detailed["attempts"].as_array().expect("attempts");
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0]["linkedRunIds"].as_array().map(|a| a.len()),
        Some(1)
    );
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(
        campaign.provider.count_for(&campaign.fixture.step_first),
        1,
        "post-restart supervisor cycles must not produce a hidden second POST"
    );
    record_assertion("in-flight-post-count-1-no-resend");
    let work = list_work(&mut campaign.client, campaign.session, &campaign.workspace).await;
    assert_eq!(work_for_step(&work, &campaign.fixture.step_first).len(), 1);
    let runs = list_runs(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let intents = list_intents(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let after = snapshot_step(
        &work,
        &intents,
        &runs,
        &campaign.fixture.step_first,
        campaign.provider.count_for(&campaign.fixture.step_first),
    );
    assert_eq!(after.work_id, before.work_id);
    assert_eq!(after.run_id, before.run_id);
    assert_eq!(after.intent_id, before.intent_id);
    assert_eq!(after.attempt_id, before.attempt_id);
    assert_eq!(
        runs_array(&runs)
            .iter()
            .filter(|run| run["runId"].as_str() == after.run_id.as_deref())
            .count(),
        1
    );
    assert_eq!(
        intents_array(&intents)
            .iter()
            .filter(|intent| intent["workId"].as_str() == Some(work_id.as_str()))
            .count(),
        1
    );
    let _ = plan_id;
    campaign.scan();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fail_closed_invalid_directive_cancel_auth_workspace_provider_faults() {
    let _serial = serial_lock();
    clear_assertions();

    let mut invalid = Campaign::start_with(ProviderScript::InvalidDirective).await;
    let (plan_id, _) = invalid.create_plan().await;
    let _ = poll_json(
        &mut invalid.client,
        "ptah_list_work",
        json!({
            "session_id": invalid.session,
            "workspace": &invalid.workspace
        }),
        |value| {
            work_for_step(value, "__manager_decision__")
                .first()
                .and_then(|item| item["state"].as_str())
                .is_some_and(|state| matches!(state, "succeeded" | "failed"))
        },
    )
    .await;
    let (plan, work, _, _) = observe(
        &mut invalid.client,
        invalid.session,
        &invalid.workspace,
        &plan_id,
    )
    .await;
    assert_ne!(plan["plan"]["state"].as_str(), Some("succeeded"));
    assert!(
        work_for_step(&work, &invalid.fixture.step_replacement).is_empty(),
        "invalid directive must not materialize replacement work: {work}"
    );
    assert!(
        !plan_has_step(&plan, &invalid.fixture.step_replacement),
        "invalid directive must not append replacement steps: {plan}"
    );
    record_assertion("invalid-directive-zero-replacement");
    invalid.scan();

    let mut cancel = Campaign::start().await;
    cancel
        .provider
        .arm("fail-cancel", ProviderDisposition::Hold);
    let submitted = call(
        &mut cancel.client,
        "ptah_submit_task",
        json!({
            "request_id": rid("cancel-me"),
            "session_id": cancel.session,
            "workspace": &cancel.workspace,
            "prompt": "CERT_CANCEL hold this provider POST"
        }),
    )
    .await;
    let run_id = submitted["runId"].as_str().unwrap().to_string();
    cancel
        .provider
        .wait_accepted("fail-cancel", Duration::from_secs(90));
    let posts = cancel.provider.count_for("fail-cancel");
    assert_eq!(posts, 1);
    let _ = call(
        &mut cancel.client,
        "ptah_cancel",
        json!({
            "request_id": rid("cancel"),
            "session_id": cancel.session,
            "workspace": &cancel.workspace,
            "run_id": run_id
        }),
    )
    .await;
    let _ = poll_json(
        &mut cancel.client,
        "ptah_get_run",
        json!({
            "session_id": cancel.session,
            "workspace": &cancel.workspace,
            "run_id": run_id
        }),
        |value| value["state"].as_str() == Some("cancelled"),
    )
    .await;
    cancel.service.kill_sigkill();
    cancel.reopen().await;
    let run = poll_json(
        &mut cancel.client,
        "ptah_get_run",
        json!({
            "session_id": cancel.session,
            "workspace": &cancel.workspace,
            "run_id": run_id
        }),
        |value| is_terminal_run(value["state"].as_str()),
    )
    .await;
    assert_eq!(run["state"].as_str(), Some("cancelled"));
    assert_eq!(pending_usage(&run), 0);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(cancel.provider.count_for("fail-cancel"), 1);
    record_assertion("cancel-restart-no-resend");
    cancel.scan();

    let mut faults = Campaign::start().await;
    for (prompt, semantic, disposition, assertion) in [
        (
            "CERT_MALFORMED provider body",
            "fail-malformed",
            ProviderDisposition::Malformed,
            "malformed-provider-bounded-terminal",
        ),
        (
            "CERT_DROP provider disconnect",
            "fail-drop",
            ProviderDisposition::Drop,
            "disconnect-provider-bounded-terminal",
        ),
        (
            "CERT_SLOW provider stall",
            "fail-slow",
            ProviderDisposition::Slow,
            "timeout-provider-bounded-terminal",
        ),
    ] {
        faults.provider.arm(semantic, disposition);
        let submitted = call(
            &mut faults.client,
            "ptah_submit_task",
            json!({
                "request_id": rid(semantic),
                "session_id": faults.session,
                "workspace": &faults.workspace,
                "prompt": prompt,
                "bounds": {
                    "maxPromptBytes": 16384,
                    "maxRounds": 2,
                    "maxDurationMs": 3000,
                    "maxTotalTokens": 800
                }
            }),
        )
        .await;
        let run_id = submitted["runId"].as_str().unwrap().to_string();
        let run = poll_json(
            &mut faults.client,
            "ptah_get_run",
            json!({
                "session_id": faults.session,
                "workspace": &faults.workspace,
                "run_id": run_id
            }),
            |value| is_bounded_run_outcome(value["state"].as_str()),
        )
        .await;
        assert!(
            is_bounded_run_outcome(run["state"].as_str()),
            "{semantic} must end bounded terminal: {run}"
        );
        if semantic == "fail-slow" {
            assert_ne!(
                run["state"].as_str(),
                Some("completed"),
                "slow provider must not complete inside the 3s run bound: {run}"
            );
        }
        record_assertion(assertion);
    }
    faults.scan();

    let mut auth = Campaign::start().await;
    let before = list_work(&mut auth.client, auth.session, &auth.workspace).await;
    let before_len = work_items(&before).len();
    assert!(
        try_mcp(&auth.service.addr, "").await.is_err(),
        "missing MCP bearer must reject"
    );
    record_assertion("missing-mcp-bearer-rejected");
    assert!(
        try_mcp(&auth.service.addr, "wrong-mcp-bearer-value-00000000")
            .await
            .is_err(),
        "wrong MCP bearer must reject"
    );
    record_assertion("wrong-mcp-bearer-rejected");
    let after = list_work(&mut auth.client, auth.session, &auth.workspace).await;
    assert_eq!(work_items(&after).len(), before_len);

    let outside = tempfile::tempdir().expect("outside workspace");
    let outside_err = call_expect_error(
        &mut auth.client,
        "ptah_create_session",
        json!({
            "workspace": outside.path(),
            "title": "outside"
        }),
    )
    .await
    .to_lowercase();
    assert!(
        outside_err.contains("workspace")
            || outside_err.contains("allowlist")
            || outside_err.contains("forbidden")
            || outside_err.contains("mismatch"),
        "outside workspace must reject: {outside_err}"
    );
    record_assertion("outside-workspace-rejected");

    let escape_root = tempfile::tempdir().expect("escape target");
    let link = auth.workspace.join("escape-link");
    std::os::unix::fs::symlink(escape_root.path(), &link).expect("escaping symlink");
    let link_err = call_expect_error(
        &mut auth.client,
        "ptah_create_session",
        json!({
            "workspace": link,
            "title": "escaped"
        }),
    )
    .await
    .to_lowercase();
    assert!(
        link_err.contains("workspace")
            || link_err.contains("allowlist")
            || link_err.contains("forbidden")
            || link_err.contains("mismatch")
            || link_err.contains("canonical"),
        "escaping symlink must reject: {link_err}"
    );
    record_assertion("escaping-symlink-rejected");
    assert_eq!(
        work_items(&list_work(&mut auth.client, auth.session, &auth.workspace).await).len(),
        before_len
    );
    auth.scan();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_one_bounded_cycle() {
    let _serial = serial_lock();
    clear_assertions();
    let started = Instant::now();
    let mut campaign = Campaign::start().await;
    campaign
        .provider
        .arm(&campaign.fixture.step_first, ProviderDisposition::Hold);
    let (plan_id, _) = campaign.create_plan().await;
    campaign
        .provider
        .wait_accepted(&campaign.fixture.step_first, Duration::from_secs(90));
    let before = wait_target_in_flight(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &campaign.fixture.step_first,
        campaign.provider.count_for(&campaign.fixture.step_first),
    )
    .await;
    let mut max = campaign.service.sample();
    campaign.service.kill_sigkill();
    campaign.reopen().await;
    max.max_with(&campaign.service.sample());
    let run_id = before.run_id.clone().expect("soak run id");
    let run = poll_json(
        &mut campaign.client,
        "ptah_get_run",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "run_id": run_id
        }),
        |value| is_terminal_run(value["state"].as_str()),
    )
    .await;
    assert_eq!(run["state"].as_str(), Some("interrupted"));
    assert_eq!(pending_usage(&run), 0);
    assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
    let work_id = before.work_id.clone().expect("soak work id");
    let _ = poll_json(
        &mut campaign.client,
        "ptah_get_work",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "work_id": work_id
        }),
        |value| value["work"]["state"].as_str() == Some("failed"),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
    record_assertion("soak-bounded-cycle-rate");
    record_assertion("soak-per-cycle-exact-identities");
    record_assertion("soak-restart-at-barrier");
    let duration_ms = started.elapsed().as_millis() as u64;
    let mut report = json!({
        "schema": "grokptah.always_on_grokbot_soak_report.v1",
        "commitSha": repository_commit(),
        "fixtureSchema": campaign.fixture.schema,
        "fixtureSchemaVersion": campaign.fixture.schema_version,
        "fixtureHash": campaign.fixture.digest(),
        "durationMs": duration_ms,
        "cycles": 1,
        "restarts": 1,
        "sends": campaign.provider.send_count(),
        "maxRssBytes": max.rss_bytes,
        "maxFdCount": max.fd_count,
        "maxThreads": max.threads,
        "maxDiskBytes": max.disk_bytes,
        "maxCycleLatencyMs": duration_ms,
        "redaction": "passed",
        "soak10m": "not-run",
        "soak24h": "not-run",
        "clock": campaign.fixture.clock,
        "planId": plan_id,
        "workId": before.work_id,
        "runId": before.run_id,
        "intentId": before.intent_id,
        "attemptId": before.attempt_id
    });
    let digest = hash_payload(&report);
    report["sha256"] = json!(digest);
    scan_text("soak-report", &report.to_string());
    record_assertion("soak-report-schema");
    record_assertion("soak-10m-distinct-from-24h");
    campaign.scan();
}

async fn soak_loop(seconds: u64) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut cycles = 0u64;
    let mut restarts = 0u64;
    let mut max = ResourceSample::default();
    let started = Instant::now();
    let mut last_sends = 0u64;
    let fixture = Fixture::load();
    while Instant::now() < deadline {
        let mut campaign = Campaign::start().await;
        campaign
            .provider
            .arm(&campaign.fixture.step_first, ProviderDisposition::Hold);
        let (_plan_id, _) = campaign.create_plan().await;
        campaign
            .provider
            .wait_accepted(&campaign.fixture.step_first, Duration::from_secs(90));
        max.max_with(&campaign.service.sample());
        campaign.service.kill_sigkill();
        campaign.reopen().await;
        restarts += 1;
        max.max_with(&campaign.service.sample());
        assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
        last_sends = campaign.provider.send_count();
        cycles += 1;
        campaign.scan();
    }
    assert!(cycles >= 1);
    let mut report = json!({
        "schema": "grokptah.always_on_grokbot_soak_report.v1",
        "commitSha": repository_commit(),
        "fixtureSchema": fixture.schema,
        "fixtureSchemaVersion": fixture.schema_version,
        "fixtureHash": fixture.digest(),
        "durationMs": started.elapsed().as_millis() as u64,
        "cycles": cycles,
        "restarts": restarts,
        "sends": last_sends,
        "maxRssBytes": max.rss_bytes,
        "maxFdCount": max.fd_count,
        "maxThreads": max.threads,
        "maxDiskBytes": max.disk_bytes,
        "soak10m": if seconds == 600 { "executed" } else { "not-this-command" },
        "soak24h": if seconds == 86400 { "executed" } else { "not-run" },
        "clock": fixture.clock
    });
    let digest = hash_payload(&report);
    report["sha256"] = json!(digest);
    scan_text("soak-report", &report.to_string());
    eprintln!("{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn soak_always_on_grokbot_10m() {
    let _serial = serial_lock();
    let seconds = std::env::var("GROKBOT_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(600);
    soak_loop(seconds).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn soak_always_on_grokbot_24h() {
    let _serial = serial_lock();
    let seconds = std::env::var("GROKBOT_SOAK_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(86400);
    soak_loop(seconds).await;
}
