//! Always-on Grokbot process certification.
//!
//! Fresh-home, exact-identity scenarios against the shipped `grokptah-service`
//! binary, authenticated MCP, and a loopback provider with an explicit POST
//! barrier. No production crate is modified. This slice proves a bounded
//! process smoke and one accepted-request restart fence, not durable
//! always-on / UncertainAccept / quota / soak certification.

#![allow(clippy::await_holding_lock)]

mod always_on_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use uuid::Uuid;

use always_on_support::{
    call, call_expect_error, causal_join, certify, clear_assertions, fingerprint_tree,
    intents_array, mcp, parse_fixture, pending_usage, plans_len, poll_json, recorded_assertions,
    repository_commit, require_causal_join, require_unique_step_work, rid, runs_array,
    scan_service_artifacts, scan_text, serial_lock, sessions_len, try_mcp, work_for_step,
    work_items, work_kind_count, CausalJoin, EntityCardinalities, FakeProvider, Fixture,
    ProviderDisposition, ProviderScript, ResourceSample, ServiceProcess, FIXTURE_BYTES,
    FIXTURE_SCHEMA, TOKEN,
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

fn run_error_code(run: &Value) -> &str {
    run["errorCode"].as_str().unwrap_or("")
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
    let listed = agents["agents"]
        .as_array()
        .expect("agents array after setup");
    assert_eq!(
        listed.len(),
        1,
        "bootstrap must materialize exactly one Agent: {agents}"
    );
    let agent_id = listed[0]["agentId"].as_str().expect("agentId").to_string();
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

async fn list_sessions(client: &mut McpControlClient) -> Value {
    call(client, "ptah_list_sessions", json!({})).await
}

async fn list_plans(client: &mut McpControlClient, session: Uuid, workspace: &Path) -> Value {
    call(
        client,
        "ptah_list_manager_plans",
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
        |value| {
            value["plan"]["planId"].as_str() == Some(plan_id)
                && value["plan"]["state"].as_str() == Some(want)
        },
    )
    .await
}

async fn wait_unique_step_state(
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
            let items = work_for_step(value, step_id);
            items.len() == 1
                && items[0]["workId"].as_str().is_some()
                && items[0]["state"].as_str() == Some(state)
        },
    )
    .await
}

async fn wait_unique_step_identity(
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
        |value| {
            let items = work_for_step(value, step_id);
            items.len() == 1 && items[0]["workId"].as_str().is_some()
        },
    )
    .await
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

#[allow(clippy::too_many_arguments)]
async fn scheduler_window_identity(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    work: &Value,
    intents: &Value,
    runs: &Value,
    provider: &FakeProvider,
    step_id: &str,
    semantic_id: &str,
) -> (String, Option<CausalJoin>) {
    let item = require_unique_step_work(work, step_id);
    let work_id = item["workId"].as_str().expect("workId").to_string();
    let detailed = get_work(client, session, workspace, &work_id).await;
    match causal_join(
        work,
        &detailed,
        intents,
        runs,
        provider,
        step_id,
        semantic_id,
    ) {
        Ok(join) => (work_id, Some(join)),
        Err(_) => (work_id, None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn join_step(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    work: &Value,
    intents: &Value,
    runs: &Value,
    provider: &FakeProvider,
    step_id: &str,
    semantic_id: &str,
) -> CausalJoin {
    let work_id = require_unique_step_work(work, step_id)["workId"]
        .as_str()
        .expect("workId")
        .to_string();
    let detailed = get_work(client, session, workspace, &work_id).await;
    require_causal_join(
        work,
        &detailed,
        intents,
        runs,
        provider,
        step_id,
        semantic_id,
    )
}

async fn wait_in_flight_join(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    provider: &FakeProvider,
    step_id: &str,
    semantic_id: &str,
) -> CausalJoin {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = String::new();
    while Instant::now() < deadline {
        let work = list_work(client, session, workspace).await;
        let runs = list_runs(client, session, workspace).await;
        let intents = list_intents(client, session, workspace).await;
        if work_for_step(&work, step_id).len() == 1 {
            let work_id = work_for_step(&work, step_id)[0]["workId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !work_id.is_empty() {
                let detailed = get_work(client, session, workspace, &work_id).await;
                match causal_join(
                    &work,
                    &detailed,
                    &intents,
                    &runs,
                    provider,
                    step_id,
                    semantic_id,
                ) {
                    Ok(join)
                        if matches!(join.work_state.as_str(), "running" | "leased")
                            && join.run_state == "running" =>
                    {
                        return join;
                    }
                    Ok(join) => last = format!("{join:?}"),
                    Err(error) => last = error,
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("target {step_id} never reached an in-flight causal join: {last}");
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

async fn cardinalities(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
) -> EntityCardinalities {
    EntityCardinalities {
        sessions: sessions_len(&list_sessions(client).await),
        plans: plans_len(&list_plans(client, session, workspace).await),
        work: work_items(&list_work(client, session, workspace).await).len(),
        intents: intents_array(&list_intents(client, session, workspace).await).len(),
        runs: runs_array(&list_runs(client, session, workspace).await).len(),
    }
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
        Self::start_with(ProviderScript::Lifecycle).await
    }

    async fn start_with(script: ProviderScript) -> Self {
        let fixture = Fixture::load();
        let provider = if script == ProviderScript::Lifecycle {
            FakeProvider::start()
        } else {
            FakeProvider::start_with(script)
        };
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
        self.client = McpControlClient::new("http://127.0.0.1:1", TOKEN);
        tokio::task::yield_now().await;
        self.service.respawn(&self.provider.base_url);
        self.client = mcp(&self.service.addr).await;
    }

    fn scan(&self) {
        scan_service_artifacts(&self.service);
        self.provider.assert_route_and_auth();
    }
}

async fn exact_happy_path_oracle(campaign: &mut Campaign, plan_id: &str) {
    let (plan, work, runs, intents) = observe(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        plan_id,
    )
    .await;
    assert_eq!(
        plan["plan"]["state"].as_str(),
        Some("succeeded"),
        "plan not succeeded: {plan}; posts={:?}",
        campaign.provider.records()
    );
    certify("plan-succeeded");
    assert_eq!(
        work_kind_count(&work, "manager-decision") as u64,
        campaign.fixture.decision_work
    );
    assert_eq!(
        always_on_support::succeeded_kind_count(&work, "manager-decision") as u64,
        campaign.fixture.decision_work
    );
    certify("exact-decision-work");
    assert_eq!(
        proposal_run_count(&runs) as u64,
        campaign.fixture.proposal_runs,
        "observed manager_proposal Run count must match fixture; enforcement is unverified: {runs}"
    );
    certify("exact-proposal-run-observed");
    assert_eq!(
        campaign.fixture.proposal_only, "unverified-pending-pr-352",
        "proposal-only enforcement is pending PR #352"
    );
    certify("proposal-only-enforcement-unverified-pr-352");
    assert!(
        plan_has_step(&plan, &campaign.fixture.step_replacement),
        "replacement step missing: {plan}"
    );
    for (step, assertion) in [
        (
            campaign.fixture.step_first.as_str(),
            "native-step-a-causal-join",
        ),
        (
            campaign.fixture.step_failing.as_str(),
            "native-step-b-causal-join",
        ),
        (
            campaign.fixture.step_replacement.as_str(),
            "native-step-b-fix-causal-join",
        ),
    ] {
        let expected_posts = *campaign
            .fixture
            .posts_by_semantic
            .get(step)
            .unwrap_or_else(|| panic!("fixture missing posts for {step}"));
        let expected_work = *campaign
            .fixture
            .native_work_by_step
            .get(step)
            .unwrap_or_else(|| panic!("fixture missing native work for {step}"));
        assert_eq!(
            work_for_step(&work, step).len() as u64,
            expected_work,
            "native work cardinality for {step}"
        );
        let join = join_step(
            &mut campaign.client,
            campaign.session,
            &campaign.workspace,
            &work,
            &intents,
            &runs,
            &campaign.provider,
            step,
            step,
        )
        .await;
        assert_eq!(join.provider_posts, expected_posts);
        certify(assertion);
    }
    assert_eq!(
        campaign.provider.count_for("manager-decision"),
        *campaign
            .fixture
            .posts_by_semantic
            .get("manager-decision")
            .expect("manager-decision posts")
    );
    assert_terminal_runs_pending_zero(&runs);
    certify("all-terminal-runs-pending-0");
    assert_eq!(live_intent_count(&intents), 0);
    let capacity = call(&mut campaign.client, "ptah_get_capacity", json!({})).await;
    always_on_support::assert_no_quota_ledger(&capacity);
    campaign.provider.assert_route_and_auth();
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

async fn assert_zero_growth_window(campaign: &mut Campaign, join: &CausalJoin, semantic: &str) {
    let before = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let posts = campaign.provider.count_for(semantic);
    let window = campaign.fixture.supervisor_period
        * u32::try_from(campaign.fixture.zero_growth_periods).expect("periods");
    tokio::time::sleep(window).await;
    let after = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    assert_eq!(
        after.sessions, before.sessions,
        "session growth during zero-growth window"
    );
    assert_eq!(
        after.plans, before.plans,
        "plan growth during zero-growth window"
    );
    assert_eq!(campaign.provider.count_for(semantic), posts);
    assert_eq!(campaign.provider.count_for(semantic), 1);
    let work = list_work(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let runs = list_runs(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let intents = list_intents(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let step_id = if semantic == "manager-decision" {
        "__manager_decision__"
    } else {
        semantic
    };
    assert_eq!(
        work_for_step(&work, step_id).len(),
        1,
        "target Work cardinality grew during zero-growth window: {work}"
    );
    let detailed = get_work(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &join.work_id,
    )
    .await;
    let again = require_causal_join(
        &work,
        &detailed,
        &intents,
        &runs,
        &campaign.provider,
        step_id,
        semantic,
    );
    assert_eq!(again.work_id, join.work_id);
    assert_eq!(again.attempt_id, join.attempt_id);
    assert_eq!(again.intent_id, join.intent_id);
    assert_eq!(again.run_id, join.run_id);
    assert_eq!(again.provider_digest, join.provider_digest);
    assert_eq!(again.provider_posts, 1);
    assert_ne!(again.work_state, "queued");
}

async fn assert_interrupted_fence(campaign: &mut Campaign, join: &CausalJoin, semantic: &str) {
    let run = poll_json(
        &mut campaign.client,
        "ptah_get_run",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "run_id": join.run_id
        }),
        |value| {
            value["runId"].as_str() == Some(join.run_id.as_str())
                && is_terminal_run(value["state"].as_str())
        },
    )
    .await;
    assert_eq!(run["state"].as_str(), Some("interrupted"));
    assert_eq!(pending_usage(&run), 0);
    assert_eq!(campaign.provider.count_for(semantic), 1);
    let detailed = poll_json(
        &mut campaign.client,
        "ptah_get_work",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "work_id": join.work_id
        }),
        |value| {
            value["work"]["workId"].as_str() == Some(join.work_id.as_str())
                && value["work"]["state"].as_str() == Some("failed")
        },
    )
    .await;
    assert_ne!(detailed["work"]["state"].as_str(), Some("queued"));
    let work = list_work(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let runs = list_runs(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let intents = list_intents(&mut campaign.client, campaign.session, &campaign.workspace).await;
    let recovered = require_causal_join(
        &work,
        &detailed,
        &intents,
        &runs,
        &campaign.provider,
        semantic,
        semantic,
    );
    assert_eq!(recovered.work_id, join.work_id);
    assert_eq!(recovered.attempt_id, join.attempt_id);
    assert_eq!(recovered.intent_id, join.intent_id);
    assert_eq!(recovered.run_id, join.run_id);
    assert_eq!(recovered.run_request_id, join.run_request_id);
    assert_eq!(recovered.provider_digest, join.provider_digest);
    assert_eq!(recovered.provider_posts, 1);
    assert_ne!(recovered.work_state, "queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_schema_is_consumed() {
    let _serial = serial_lock();
    clear_assertions();
    let fixture = Fixture::load();
    assert_eq!(fixture.schema, FIXTURE_SCHEMA);
    certify("fixture-schema-consumed");
    assert_eq!(fixture.schema_version, 2);
    assert_eq!(fixture.base_sha, "67e29bd34dc64049432c715c93c2cef2185c63ea");
    certify("fixture-version-matches");
    parse_fixture(FIXTURE_BYTES).expect("canonical parse");
    let mut unknown = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
    unknown["bonus"] = json!(true);
    assert!(parse_fixture(&serde_json::to_vec(&unknown).unwrap()).is_err());
    certify("fixture-unknown-fields-rejected");
    assert_eq!(fixture.clock, "bounded-race-controlled-no-fake-clock-seam");
    certify("clock-is-bounded-not-deterministic");
    assert_eq!(
        fixture.claim,
        "bounded-process-smoke-and-one-accepted-request-restart-fence"
    );
    certify("claim-is-bounded-process-smoke");
    assert_eq!(fixture.quota_ledger, "absent-at-67e29bd");
    assert_eq!(fixture.provider_attempt_projection, "absent-on-base-main");
    assert_eq!(fixture.uncertain_accept_projection, "absent-on-base-main");
    assert_eq!(fixture.retry_class_projection, "absent-on-base-main");
    assert_eq!(
        fixture.proved_oracle,
        "interrupted_run_not_readmitted_within_window"
    );
    certify("quota-and-uncertain-accept-absent");
    assert_eq!(
        fixture.next_required_campaign,
        "pr-352-plus-provider-attempt-quota-uncertain-accept-integration"
    );
    certify("next-campaign-is-pr-352-provider-attempt-quota");
    assert_eq!(fixture.decision_work, 1);
    assert_eq!(fixture.proposal_runs, 1);
    assert_eq!(fixture.native_work_by_step.get("step-a"), Some(&1));
    assert_eq!(fixture.posts_by_semantic.get("step-a"), Some(&1));
    certify("fixture-cardinalities-drive-runtime-oracle");
    assert_eq!(fixture.ci_mode, "one-cycle-smoke");
    assert_eq!(
        fixture.soak10m,
        "unverified-ignored-harness-no-pinned-artifact"
    );
    assert_eq!(fixture.soak24h, "unverified-no-pinned-head-artifact");
    let campaign = include_str!("../../../../evals/certification-lab/campaign.v1.json");
    assert!(campaign.contains("interrupted_run_not_readmitted_within_window"));
    assert!(!campaign.contains("\"uncertain_attempt_not_resumed\""));
    let probes = include_str!("../../../../evals/certification-lab/src/probes.rs");
    assert!(probes.contains("InterruptedRunNotReadmittedWithinWindow"));
    assert!(!probes.contains("observe_oracle(OracleCode::UncertainAttemptNotResumed)"));
    certify("lab-probe-does-not-synthesize");
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
    certify("no-manual-tick-before-success");
    let _ = wait_plan_state(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &plan_id,
        "succeeded",
    )
    .await;
    exact_happy_path_oracle(&mut campaign, &plan_id).await;

    let before = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
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
    let after = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    assert_eq!(before, after);
    assert_eq!(
        before_posts,
        (
            campaign.provider.count_for("step-a"),
            campaign.provider.count_for("step-b"),
            campaign.provider.count_for("manager-decision"),
            campaign.provider.count_for("step-b-fix"),
        )
    );
    certify("post-success-tick-idempotent");

    let replay = call(
        &mut campaign.client,
        "ptah_create_manager_plan",
        args.clone(),
    )
    .await;
    assert_eq!(replay["plan"]["planId"].as_str(), Some(plan_id.as_str()));
    assert_eq!(
        cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
        after
    );
    certify("replay-same-payload");

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
        cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
        after
    );
    certify("replay-changed-payload-conflict");
    campaign.scan();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduler_window_restart_observations_keep_exact_identities() {
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
            let _ = wait_unique_step_state(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                step_id,
                want,
            )
            .await;
        } else {
            let _ = wait_unique_step_identity(
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
        let before_join = if step_id.is_empty() {
            None
        } else if state.is_some() {
            Some(
                join_step(
                    &mut campaign.client,
                    campaign.session,
                    &campaign.workspace,
                    &work,
                    &intents,
                    &runs,
                    &campaign.provider,
                    step_id,
                    semantic,
                )
                .await,
            )
        } else {
            None
        };
        let before_window = if step_id.is_empty() || state.is_some() {
            None
        } else {
            Some(
                scheduler_window_identity(
                    &mut campaign.client,
                    campaign.session,
                    &campaign.workspace,
                    &work,
                    &intents,
                    &runs,
                    &campaign.provider,
                    step_id,
                    semantic,
                )
                .await,
            )
        };
        campaign.reopen().await;
        let (plan, work, runs, intents) = observe(
            &mut campaign.client,
            campaign.session,
            &campaign.workspace,
            &plan_id,
        )
        .await;
        if name == "plan-succeeded" {
            assert_eq!(plan["plan"]["planId"].as_str(), Some(plan_id.as_str()));
            assert_eq!(plan["plan"]["state"].as_str(), Some("succeeded"));
            for key in ["step-a", "step-b", "manager-decision", "step-b-fix"] {
                assert_eq!(
                    campaign.provider.count_for(key),
                    1,
                    "{key} posts after {name}: {:?}",
                    campaign.provider.records()
                );
            }
        } else if let Some(before) = before_join {
            let after = join_step(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &work,
                &intents,
                &runs,
                &campaign.provider,
                step_id,
                semantic,
            )
            .await;
            assert_eq!(after.work_id, before.work_id, "{name} work id changed");
            assert_eq!(
                after.attempt_id, before.attempt_id,
                "{name} attempt changed"
            );
            assert_eq!(after.intent_id, before.intent_id, "{name} intent changed");
            assert_eq!(after.run_id, before.run_id, "{name} run changed");
            if matches!(state, Some("succeeded" | "failed")) {
                assert_eq!(after.work_state.as_str(), state.unwrap());
                assert_eq!(after.provider_posts, before.provider_posts);
            }
        } else {
            let (before_work_id, before_join) = before_window.expect("scheduler-window work id");
            let after_item = require_unique_step_work(&work, step_id);
            assert_eq!(
                after_item["workId"].as_str(),
                Some(before_work_id.as_str()),
                "{name} work id changed"
            );
            if let Some(before) = before_join {
                let after = join_step(
                    &mut campaign.client,
                    campaign.session,
                    &campaign.workspace,
                    &work,
                    &intents,
                    &runs,
                    &campaign.provider,
                    step_id,
                    semantic,
                )
                .await;
                assert_eq!(
                    after.attempt_id, before.attempt_id,
                    "{name} attempt changed"
                );
                assert_eq!(after.intent_id, before.intent_id, "{name} intent changed");
                assert_eq!(after.run_id, before.run_id, "{name} run changed");
            }
        }
        campaign.scan();
        certify("scheduler-window-not-a-cutpoint");
        certify("restart-exact-target-identities");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_same_home_restarts_keep_held_request_identities() {
    let _serial = serial_lock();
    clear_assertions();
    let mut campaign = Campaign::start().await;
    campaign
        .provider
        .arm(&campaign.fixture.step_first, ProviderDisposition::Hold);
    let (_plan_id, _) = campaign.create_plan().await;
    campaign
        .provider
        .wait_accepted(&campaign.fixture.step_first, Duration::from_secs(90));
    assert_eq!(campaign.provider.count_for(&campaign.fixture.step_first), 1);
    let pid0 = campaign.service.pid();
    let join = wait_in_flight_join(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &campaign.provider,
        &campaign.fixture.step_first,
        &campaign.fixture.step_first,
    )
    .await;
    campaign.reopen().await;
    let pid1 = campaign.service.pid();
    assert_ne!(pid1, pid0);
    let step = campaign.fixture.step_first.clone();
    assert_interrupted_fence(&mut campaign, &join, &step).await;
    assert_zero_growth_window(&mut campaign, &join, &step).await;
    campaign.reopen().await;
    let pid2 = campaign.service.pid();
    assert_ne!(pid2, pid1);
    assert_ne!(pid2, pid0);
    assert_interrupted_fence(&mut campaign, &join, &step).await;
    assert_zero_growth_window(&mut campaign, &join, &step).await;
    certify("interrupted-run-not-readmitted-within-window");
    certify("two-same-home-restarts");
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
            let items = work_for_step(value, "__manager_decision__");
            items.len() == 1
                && items[0]["workId"].as_str().is_some()
                && items[0]["state"]
                    .as_str()
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
    certify("invalid-directive-zero-replacement");
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
    assert_eq!(cancel.provider.count_for("fail-cancel"), 1);
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
    let expect = cancel.fixture.fail_closed_case("cancel").clone();
    fail_closed_two_restarts(&mut cancel, &run_id, "fail-cancel", &expect).await;
    certify("cancel-two-restarts-no-resend");
    cancel.scan();

    for (prompt, semantic, disposition, fixture_key, assertion) in [
        (
            "CERT_MALFORMED provider body",
            "fail-malformed",
            ProviderDisposition::Malformed,
            "malformed",
            "malformed-provider-exact-terminal",
        ),
        (
            "CERT_DROP provider disconnect",
            "fail-drop",
            ProviderDisposition::Drop,
            "disconnect",
            "disconnect-provider-exact-terminal",
        ),
        (
            "CERT_500 provider error",
            "fail-500",
            ProviderDisposition::Status500,
            "status500",
            "status500-provider-exact-terminal",
        ),
        (
            "CERT_SLOW provider stall",
            "fail-slow",
            ProviderDisposition::Slow,
            "slow",
            "timeout-provider-exact-terminal",
        ),
    ] {
        let mut faults = Campaign::start().await;
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
        let expect = faults.fixture.fail_closed_case(fixture_key).clone();
        fail_closed_two_restarts(&mut faults, &run_id, semantic, &expect).await;
        certify(assertion);
        faults.scan();
    }

    let mut auth = Campaign::start().await;
    let before_home = auth.service.durable_home_entries();
    let before = cardinalities(&mut auth.client, auth.session, &auth.workspace).await;
    assert!(
        try_mcp(&auth.service.addr, "").await.is_err(),
        "missing MCP bearer must reject"
    );
    certify("missing-mcp-bearer-rejected");
    assert!(
        try_mcp(&auth.service.addr, "wrong-mcp-bearer-value-00000000")
            .await
            .is_err(),
        "wrong MCP bearer must reject"
    );
    certify("wrong-mcp-bearer-rejected");
    let sends_before = auth.provider.send_count();
    let (missing_status, _) = auth.provider.post_chat(
        None,
        r#"{"model":"grok-build","messages":[{"role":"user","content":"probe"}]}"#,
    );
    assert_eq!(missing_status, 401);
    assert_eq!(auth.provider.rejected_auth_count(), 1);
    certify("provider-rejects-missing-bearer");
    let (wrong_status, _) = auth.provider.post_chat(
        Some("Bearer wrong-provider-token-not-the-key"),
        r#"{"model":"grok-build","messages":[{"role":"user","content":"probe"}]}"#,
    );
    assert_eq!(wrong_status, 401);
    assert_eq!(auth.provider.rejected_auth_count(), 2);
    certify("provider-rejects-wrong-bearer");
    assert_eq!(auth.provider.send_count(), sends_before);
    assert_durable_home_unchanged(&auth.service, &before_home);
    assert_eq!(
        cardinalities(&mut auth.client, auth.session, &auth.workspace).await,
        before
    );

    let outside = tempfile::tempdir().expect("outside workspace");
    reject_workspace_and_home_unchanged(
        &mut auth,
        &before_home,
        &before,
        outside.path(),
        "outside",
        "outside-workspace-home-unchanged",
    )
    .await;

    let traversal = auth.workspace.join("..").join("traversal-outside");
    reject_workspace_and_home_unchanged(
        &mut auth,
        &before_home,
        &before,
        &traversal,
        "traversal",
        "traversal-workspace-home-unchanged",
    )
    .await;

    let wrong_session = Uuid::new_v4();
    let wrong = call_expect_error(
        &mut auth.client,
        "ptah_list_work",
        json!({
            "session_id": wrong_session,
            "workspace": &auth.workspace
        }),
    )
    .await
    .to_lowercase();
    assert!(
        wrong.contains("session")
            || wrong.contains("mismatch")
            || wrong.contains("forbidden")
            || wrong.contains("not_found")
            || wrong.contains("unknown")
            || wrong.contains("invalid_request"),
        "wrong session must reject: {wrong}"
    );
    assert_durable_home_unchanged(&auth.service, &before_home);
    assert_eq!(
        cardinalities(&mut auth.client, auth.session, &auth.workspace).await,
        before
    );
    certify("wrong-session-home-unchanged");

    let escape_root = tempfile::tempdir().expect("escape target");
    let link = auth.workspace.join("escape-link");
    std::os::unix::fs::symlink(escape_root.path(), &link).expect("escaping symlink");
    reject_workspace_and_home_unchanged(
        &mut auth,
        &before_home,
        &before,
        &link,
        "escaped",
        "escaping-symlink-home-unchanged",
    )
    .await;

    let swap_dir = auth.workspace.join("swap-me");
    std::fs::create_dir_all(&swap_dir).expect("swap dir");
    let outside_swap = tempfile::tempdir().expect("swap outside");
    std::fs::remove_dir_all(&swap_dir).expect("remove swap dir");
    std::os::unix::fs::symlink(outside_swap.path(), &swap_dir).expect("swap symlink");
    reject_workspace_and_home_unchanged(
        &mut auth,
        &before_home,
        &before,
        &swap_dir,
        "swapped",
        "symlink-swap-home-unchanged",
    )
    .await;
    auth.scan();
}

async fn reject_workspace_and_home_unchanged(
    campaign: &mut Campaign,
    before_home: &[(String, String, u64)],
    before: &EntityCardinalities,
    workspace: &Path,
    title: &str,
    assertion: &str,
) {
    let err = call_expect_error(
        &mut campaign.client,
        "ptah_create_session",
        json!({
            "workspace": workspace,
            "title": title
        }),
    )
    .await
    .to_lowercase();
    assert!(
        err.contains("workspace")
            || err.contains("allowlist")
            || err.contains("forbidden")
            || err.contains("mismatch")
            || err.contains("canonical")
            || err.contains("symlink"),
        "{title} workspace must reject: {err}"
    );
    assert_durable_home_unchanged(&campaign.service, before_home);
    assert_eq!(
        cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
        *before
    );
    certify(assertion);
}

fn assert_durable_home_unchanged(service: &ServiceProcess, before: &[(String, String, u64)]) {
    let after = service.durable_home_entries();
    if after == before {
        return;
    }
    let before_keys: std::collections::BTreeSet<_> = before.iter().map(|row| &row.0).collect();
    let after_keys: std::collections::BTreeSet<_> = after.iter().map(|row| &row.0).collect();
    let added: Vec<_> = after_keys.difference(&before_keys).cloned().collect();
    let removed: Vec<_> = before_keys.difference(&after_keys).cloned().collect();
    let changed: Vec<_> = before
        .iter()
        .filter_map(|(path, hash, len)| {
            after.iter().find(|(other, _, _)| other == path).and_then(
                |(_, after_hash, after_len)| {
                    (*hash != *after_hash || *len != *after_len).then_some(path.clone())
                },
            )
        })
        .collect();
    panic!(
        "durable home identity changed: added={added:?} removed={removed:?} changed={changed:?}"
    );
}

async fn fail_closed_two_restarts(
    campaign: &mut Campaign,
    run_id: &str,
    semantic: &str,
    expect: &always_on_support::FailClosedExpect,
) {
    let run = poll_json(
        &mut campaign.client,
        "ptah_get_run",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "run_id": run_id
        }),
        |value| {
            value["runId"].as_str() == Some(run_id)
                && value["state"].as_str() == Some(expect.run_state.as_str())
                && run_error_code(value) == expect.error_code.as_str()
                && pending_usage(value) == 0
        },
    )
    .await;
    assert_eq!(
        run["state"].as_str(),
        Some(expect.run_state.as_str()),
        "{semantic} terminal state: {run}"
    );
    assert_eq!(
        run_error_code(&run),
        expect.error_code.as_str(),
        "{semantic} error code: {run}"
    );
    assert_eq!(pending_usage(&run), 0);
    assert_eq!(campaign.provider.count_for(semantic), expect.posts);
    let before = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    for _ in 0..2 {
        campaign.reopen().await;
        let recovered = poll_json(
            &mut campaign.client,
            "ptah_get_run",
            json!({
                "session_id": campaign.session,
                "workspace": &campaign.workspace,
                "run_id": run_id
            }),
            |value| {
                value["runId"].as_str() == Some(run_id)
                    && value["state"].as_str() == Some(expect.run_state.as_str())
                    && run_error_code(value) == expect.error_code.as_str()
                    && pending_usage(value) == 0
            },
        )
        .await;
        assert_eq!(recovered["state"].as_str(), Some(expect.run_state.as_str()));
        assert_eq!(run_error_code(&recovered), expect.error_code.as_str());
        assert_eq!(pending_usage(&recovered), 0);
        assert_eq!(campaign.provider.count_for(semantic), expect.posts);
        let window = campaign.fixture.supervisor_period
            * u32::try_from(campaign.fixture.zero_growth_periods).expect("periods");
        tokio::time::sleep(window).await;
        assert_eq!(
            cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
            before
        );
        assert_eq!(campaign.provider.count_for(semantic), expect.posts);
    }
}

fn soak_seconds_for_mode(mode: &str, default: u64) -> u64 {
    match std::env::var("GROKBOT_SOAK_SECS") {
        Err(_) => default,
        Ok(value) => {
            let parsed: u64 = value.parse().expect("GROKBOT_SOAK_SECS u64");
            assert_eq!(
                parsed, default,
                "GROKBOT_SOAK_SECS={parsed} is inconsistent with soak mode {mode} (expected {default})"
            );
            parsed
        }
    }
}

fn persist_soak_report(report: &Value) {
    scan_text("soak-report", &report.to_string());
    let encoded = serde_json::to_vec_pretty(report).expect("soak report bytes");
    let path = std::env::temp_dir().join(format!(
        "always-on-grokbot-soak-{}.json",
        report["commitSha"].as_str().unwrap_or("head")
    ));
    std::fs::write(&path, &encoded).expect("persist soak report");
    let roundtrip: Value = serde_json::from_slice(&encoded).expect("soak report roundtrip");
    assert_eq!(
        roundtrip["schema"],
        "grokptah.always_on_grokbot_soak_report.v1"
    );
    assert_eq!(roundtrip["sha256"], report["sha256"]);
}

fn assert_resource_ceilings(max: &ResourceSample, growth: &ResourceSample, fixture: &Fixture) {
    assert!(
        max.rss_bytes <= fixture.ceilings.max_rss_bytes,
        "rss {} exceeds {}",
        max.rss_bytes,
        fixture.ceilings.max_rss_bytes
    );
    assert!(max.fd_count <= fixture.ceilings.max_fd_count);
    assert!(max.threads <= fixture.ceilings.max_threads);
    assert!(max.disk_bytes <= fixture.ceilings.max_disk_bytes);
    assert!(growth.rss_bytes <= fixture.ceilings.max_rss_growth_bytes);
    assert!(growth.fd_count <= fixture.ceilings.max_fd_growth);
    assert!(growth.threads <= fixture.ceilings.max_thread_growth);
    assert!(growth.disk_bytes <= fixture.ceilings.max_disk_growth_bytes);
}

async fn soak_hold_restart(campaign: &mut Campaign, cycle: u64) {
    let token = format!("cycle-{cycle}");
    let semantic = format!("hold-{token}");
    campaign.provider.arm(&semantic, ProviderDisposition::Hold);
    let submitted = call(
        &mut campaign.client,
        "ptah_submit_task",
        json!({
            "request_id": rid(&format!("soak-{cycle}")),
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "prompt": format!("CERT_HOLD {token} hold this provider POST")
        }),
    )
    .await;
    let run_id = submitted["runId"].as_str().expect("soak runId").to_string();
    campaign
        .provider
        .wait_accepted(&semantic, Duration::from_secs(90));
    assert_eq!(campaign.provider.count_for(&semantic), 1);
    let _ = poll_json(
        &mut campaign.client,
        "ptah_get_run",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "run_id": run_id
        }),
        |value| {
            value["runId"].as_str() == Some(run_id.as_str())
                && value["state"].as_str() == Some("running")
        },
    )
    .await;
    let pid_before = campaign.service.pid();
    let before = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    campaign.reopen().await;
    assert_ne!(campaign.service.pid(), pid_before);
    let recovered = poll_json(
        &mut campaign.client,
        "ptah_get_run",
        json!({
            "session_id": campaign.session,
            "workspace": &campaign.workspace,
            "run_id": run_id
        }),
        |value| {
            value["runId"].as_str() == Some(run_id.as_str())
                && value["state"].as_str() == Some("interrupted")
                && pending_usage(value) == 0
        },
    )
    .await;
    assert_eq!(recovered["state"].as_str(), Some("interrupted"));
    assert_eq!(pending_usage(&recovered), 0);
    assert_eq!(campaign.provider.count_for(&semantic), 1);
    let window = campaign.fixture.supervisor_period
        * u32::try_from(campaign.fixture.zero_growth_periods).expect("periods");
    tokio::time::sleep(window).await;
    assert_eq!(
        cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
        before
    );
    assert_eq!(campaign.provider.count_for(&semantic), 1);
}

async fn barrier_restart_on_campaign(campaign: &mut Campaign) -> CausalJoin {
    campaign
        .provider
        .arm(&campaign.fixture.step_first, ProviderDisposition::Hold);
    let (_plan_id, _) = campaign.create_plan().await;
    campaign
        .provider
        .wait_accepted(&campaign.fixture.step_first, Duration::from_secs(90));
    let posts = campaign.provider.count_for(&campaign.fixture.step_first);
    let join = wait_in_flight_join(
        &mut campaign.client,
        campaign.session,
        &campaign.workspace,
        &campaign.provider,
        &campaign.fixture.step_first,
        &campaign.fixture.step_first,
    )
    .await;
    let step = campaign.fixture.step_first.clone();
    campaign.reopen().await;
    assert_interrupted_fence(campaign, &join, &step).await;
    assert_zero_growth_window(campaign, &join, &step).await;
    assert_eq!(campaign.provider.count_for(&step), posts);
    join
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_one_bounded_cycle() {
    let _serial = serial_lock();
    clear_assertions();
    let started = Instant::now();
    let mut campaign = Campaign::start().await;
    assert_eq!(campaign.fixture.ci_mode, "one-cycle-smoke");
    let baseline = campaign.service.sample_tree();
    let mut max = baseline.clone();
    let _ = barrier_restart_on_campaign(&mut campaign).await;
    max.max_with(&campaign.service.sample_tree());
    let duration_ms = started.elapsed().as_millis() as u64;
    assert!(duration_ms <= campaign.fixture.ceilings.max_cycle_latency_ms);
    let growth = max.growth_from(&baseline);
    assert_resource_ceilings(&max, &growth, &campaign.fixture);
    certify("soak-one-cycle-smoke-not-10m-or-24h");
    let mut report = json!({
        "schema": "grokptah.always_on_grokbot_soak_report.v1",
        "commitSha": repository_commit(),
        "mode": campaign.fixture.ci_mode,
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
        "soak10m": campaign.fixture.soak10m,
        "soak24h": campaign.fixture.soak24h,
        "clock": campaign.fixture.clock
    });
    let digest = hash_payload(&report);
    report["sha256"] = json!(digest);
    persist_soak_report(&report);
    certify("soak-report-schema");
    campaign.scan();
}

async fn soak_loop(mode: &str, default_secs: u64) {
    let seconds = soak_seconds_for_mode(mode, default_secs);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let started = Instant::now();
    let mut campaign = Campaign::start().await;
    let baseline = campaign.service.sample_tree();
    let mut max = baseline.clone();
    let mut cycles = 0u64;
    let mut restarts = 0u64;
    let mut max_cycle_latency_ms = 0u64;
    while Instant::now() < deadline {
        let cycle_started = Instant::now();
        max.max_with(&campaign.service.sample_tree());
        soak_hold_restart(&mut campaign, cycles.saturating_add(1)).await;
        max.max_with(&campaign.service.sample_tree());
        restarts += 1;
        cycles += 1;
        max_cycle_latency_ms = max_cycle_latency_ms.max(cycle_started.elapsed().as_millis() as u64);
        campaign.scan();
    }
    assert_ne!(
        cycles, 0,
        "soak window produced zero barrier-restart cycles"
    );
    let growth = max.growth_from(&baseline);
    assert_resource_ceilings(&max, &growth, &campaign.fixture);
    assert!(max_cycle_latency_ms <= campaign.fixture.ceilings.max_cycle_latency_ms);
    let soak24h = if mode == "24h" {
        if std::env::var("GROKBOT_SOAK_PINNED_HEAD_ARTIFACT")
            .ok()
            .is_some_and(|path| Path::new(&path).is_file())
        {
            "executed-with-pinned-head-artifact"
        } else {
            "unverified-no-pinned-head-artifact"
        }
    } else {
        campaign.fixture.soak24h.as_str()
    };
    let soak10m = if mode == "10m" {
        "executed-ignored-harness"
    } else {
        campaign.fixture.soak10m.as_str()
    };
    let mut report = json!({
        "schema": "grokptah.always_on_grokbot_soak_report.v1",
        "commitSha": repository_commit(),
        "mode": mode,
        "fixtureSchema": campaign.fixture.schema,
        "fixtureSchemaVersion": campaign.fixture.schema_version,
        "fixtureHash": campaign.fixture.digest(),
        "durationMs": started.elapsed().as_millis() as u64,
        "cycles": cycles,
        "restarts": restarts,
        "sends": campaign.provider.send_count(),
        "maxRssBytes": max.rss_bytes,
        "maxFdCount": max.fd_count,
        "maxThreads": max.threads,
        "maxDiskBytes": max.disk_bytes,
        "maxCycleLatencyMs": max_cycle_latency_ms,
        "redaction": "passed",
        "soak10m": soak10m,
        "soak24h": soak24h,
        "clock": campaign.fixture.clock
    });
    let digest = hash_payload(&report);
    report["sha256"] = json!(digest);
    persist_soak_report(&report);
    eprintln!("{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn soak_always_on_grokbot_10m() {
    let _serial = serial_lock();
    soak_loop("10m", 600).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn soak_always_on_grokbot_24h() {
    let _serial = serial_lock();
    soak_loop("24h", 86400).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn home_fingerprint_rejects_non_utf8_and_oversize() {
    let limits = Fixture::load().artifact_scan;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ok.txt"), "hello").unwrap();
    fingerprint_tree(dir.path(), &limits).unwrap();
    std::fs::write(dir.path().join("bin"), [0xff, 0xfe, 0x00]).unwrap();
    assert!(fingerprint_tree(dir.path(), &limits).is_err());
    let mut oversize = Fixture::load().artifact_scan;
    oversize.max_file_bytes = 4;
    assert!(fingerprint_tree(dir.path(), &oversize).is_err());
}
