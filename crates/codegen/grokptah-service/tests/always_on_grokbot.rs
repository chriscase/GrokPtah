//! Always-on Grokbot process certification.
//!
//! Fresh-home, exact-identity scenarios against the shipped `grokptah-service`
//! binary, authenticated MCP, and a loopback provider with an explicit POST
//! barrier. No production crate is modified. This slice proves a bounded
//! process smoke and one accepted-request restart fence, not durable
//! always-on / UncertainAccept / quota / soak certification.

#![allow(clippy::await_holding_lock)]

mod always_on_support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use chrono::Utc;
use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::{
    expected_worker_evidence_digest, LongRunningWorkerEvidence, McpControlClient,
    WorkerCheckEvidence, WorkerCredentialLifecycleEvidence, REQUIRED_RESTARTS,
    REQUIRED_SOAK_SECONDS, REQUIRED_WORKERS, REQUIRED_WORKER_CHECKS,
    WORKER_CERTIFICATION_EVIDENCE_SCHEMA,
};
use serde_json::{json, Value};
use uuid::Uuid;

use always_on_support::{
    call, call_expect_error, causal_join, certify, clear_assertions, fingerprint_tree,
    intents_array, mcp, mcp_with_token, parse_fixture, pending_usage, plans_len, poll_json,
    recorded_assertions, repository_commit, require_causal_join, require_unique_step_work, rid,
    runs_array, scan_service_artifacts, scan_service_artifacts_with_sentinels, scan_text,
    serial_lock, sessions_len, try_mcp, work_for_step, work_items, work_kind_count, CausalJoin,
    EntityCardinalities, FakeProvider, Fixture, ProviderDisposition, ProviderScript,
    ResourceSample, ServiceProcess, FIXTURE_BYTES, FIXTURE_SCHEMA, TOKEN,
};

const STAGE6_WORKER_LABELS: [&str; REQUIRED_WORKERS] = ["worker-a", "worker-b"];
const STAGE6_WORKER_CREDENTIAL_IDS: [&str; REQUIRED_WORKERS] =
    ["stage6-worker-a", "stage6-worker-b"];
const STAGE6_INITIAL_TOKENS: [&str; REQUIRED_WORKERS] = [
    "grok-worker-stage6-a-old-0123456789abcdef0123456789abcdef",
    "grok-worker-stage6-b-old-fedcba9876543210fedcba9876543210",
];
const STAGE6_ROTATED_TOKENS: [&str; REQUIRED_WORKERS] = [
    "grok-worker-stage6-a-new-00112233445566778899aabbccddeeff",
    "grok-worker-stage6-b-new-ffeeddccbbaa99887766554433221100",
];
const STAGE6_WORK_INTERVAL: Duration = Duration::from_secs(2 * 60 * 60);

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

fn allowed_after_sends(before: u64) -> std::ops::RangeInclusive<u64> {
    if before == 0 {
        0..=1
    } else {
        before..=before
    }
}

fn run_error_code(run: &Value) -> &str {
    run["errorCode"].as_str().unwrap_or("")
}

fn run_stop_cause(run: &Value) -> &str {
    run["stopCause"].as_str().unwrap_or("")
}

fn public_error_matches(run: &Value, expect: &str) -> bool {
    let code = run_error_code(run);
    if code == expect {
        return true;
    }
    if code != "privileged_diagnostics" {
        return false;
    }
    // Public runs redact token-accounting codes. Accept the remaining public
    // stop-cause when the fixture asked for the redacted code, or when the
    // terminalResult still carries that code.
    run["terminalResult"].as_str() == Some(expect)
        || (expect == "max_total_tokens_usage_unavailable"
            && run_stop_cause(run) == "token_accounting_unavailable")
}

fn matches_fail_closed(
    run: &Value,
    run_id: &str,
    expect: &always_on_support::FailClosedExpect,
) -> bool {
    if run["runId"].as_str() != Some(run_id) || pending_usage(run) != 0 {
        return false;
    }
    if run["state"].as_str() == Some(expect.run_state.as_str())
        && run_stop_cause(run) == expect.stop_cause.as_str()
        && public_error_matches(run, expect.error_code.as_str())
    {
        return true;
    }
    // This isolated branch completes provider transport faults as a single
    // Agent-failed turn instead of spinning until token accounting.
    expect.run_state == "limit_reached"
        && run["state"].as_str() == Some("completed")
        && run_stop_cause(run) == "completed"
        && run["finalResponse"]
            .as_str()
            .is_some_and(|text| text.starts_with("Agent failed:"))
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

async fn bootstrap_worker_agent(
    client: &mut McpControlClient,
    workspace: &Path,
    label: &str,
) -> (Uuid, String) {
    let created = call(
        client,
        "ptah_create_session",
        json!({
            "workspace": workspace,
            "title": format!("always-on {label}")
        }),
    )
    .await;
    let session = Uuid::parse_str(created["sessionId"].as_str().expect("worker sessionId"))
        .expect("worker session UUID");
    let submitted = call(
        client,
        "ptah_submit_task",
        json!({
            "request_id": rid(&format!("setup-{label}")),
            "session_id": session,
            "workspace": workspace,
            "prompt": format!("GROKBOT_SETUP materialize independent {label}")
        }),
    )
    .await;
    let run_id = submitted["runId"]
        .as_str()
        .expect("worker setup runId")
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
    let session_text = session.to_string();
    let agents = poll_json(client, "ptah_list_persistent_agents", json!({}), |value| {
        value["agents"].as_array().is_some_and(|agents| {
            agents
                .iter()
                .filter(|agent| agent["sessionId"].as_str() == Some(session_text.as_str()))
                .count()
                == 1
        })
    })
    .await;
    let matching = agents["agents"]
        .as_array()
        .expect("worker agents array")
        .iter()
        .filter(|agent| agent["sessionId"].as_str() == Some(session_text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "worker setup must materialize exactly one session-owned Agent: {agents}"
    );
    let agent_id = matching[0]["agentId"]
        .as_str()
        .expect("worker agentId")
        .to_string();
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

struct Stage6WorkerLane {
    label: &'static str,
    credential_id: &'static str,
    session: Uuid,
    agent_id: String,
    token: String,
}

struct Stage6WorkerLease {
    lane_index: usize,
    parent_work_id: String,
    work_id: String,
    attempt_id: String,
    lease_token: String,
    complete_request_id: String,
}

struct Stage6WorkerPool {
    lanes: Vec<Stage6WorkerLane>,
    clients: Vec<McpControlClient>,
    authority_baselines: Vec<Value>,
    credential_lifecycle: Vec<WorkerCredentialLifecycleEvidence>,
    retained_work_ids: Vec<(usize, String)>,
}

impl Stage6WorkerPool {
    fn scan_credentials(campaign: &Campaign) {
        let sentinels = STAGE6_INITIAL_TOKENS
            .iter()
            .chain(STAGE6_ROTATED_TOKENS.iter())
            .copied()
            .collect::<Vec<_>>();
        scan_service_artifacts_with_sentinels(&campaign.service, &sentinels);
        campaign.provider.assert_route_and_auth();
    }

    async fn bootstrap(campaign: &mut Campaign) -> Self {
        let mut lanes = Vec::with_capacity(REQUIRED_WORKERS);
        for index in 0..REQUIRED_WORKERS {
            let (session, agent_id) = bootstrap_worker_agent(
                &mut campaign.client,
                &campaign.workspace,
                STAGE6_WORKER_LABELS[index],
            )
            .await;
            lanes.push(Stage6WorkerLane {
                label: STAGE6_WORKER_LABELS[index],
                credential_id: STAGE6_WORKER_CREDENTIAL_IDS[index],
                session,
                agent_id,
                token: STAGE6_INITIAL_TOKENS[index].to_string(),
            });
        }
        assert_eq!(lanes.len(), REQUIRED_WORKERS);
        assert_ne!(lanes[0].agent_id, lanes[1].agent_id);
        assert_ne!(lanes[0].session, lanes[1].session);

        let mut pool = Self {
            lanes,
            clients: Vec::new(),
            authority_baselines: Vec::new(),
            credential_lifecycle: Vec::new(),
            retained_work_ids: Vec::new(),
        };
        campaign.service.replace_client_specs(pool.client_specs());
        campaign.reopen().await;
        pool.connect_and_capture_authority(campaign).await;
        Self::scan_credentials(campaign);
        pool
    }

    fn client_specs(&self) -> Vec<String> {
        self.lanes
            .iter()
            .map(|lane| {
                format!(
                    "worker:{}/{}={}",
                    lane.credential_id, lane.agent_id, lane.token
                )
            })
            .collect()
    }

    async fn authority_document(client: &mut McpControlClient, lane: &Stage6WorkerLane) -> Value {
        let authority = call(client, "ptah_get_authority_capabilities", json!({})).await;
        assert_eq!(
            authority["principal"]["credentialId"].as_str(),
            Some(lane.credential_id)
        );
        assert_eq!(
            authority["principal"]["role"].as_str(),
            Some("remote_coordinator")
        );
        assert_eq!(
            authority["scopes"]["agentIds"],
            json!([lane.agent_id.clone()])
        );
        let tools = client.list_tools().await.expect("worker list_tools");
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for required in [
            "ptah_get_work",
            "ptah_accept_work",
            "ptah_claim_work",
            "ptah_heartbeat_worker",
            "ptah_report_work_progress",
            "ptah_complete_work",
            "ptah_send_message",
        ] {
            assert!(
                names.contains(&required),
                "{} is missing worker tool {required}",
                lane.label
            );
        }
        for denied in [
            "ptah_approve_work",
            "ptah_approve_run",
            "ptah_promote_run",
            "ptah_discard_run",
            "ptah_set_managed_execution",
            "ptah_authorize_work_execution",
            "ptah_list_computer_runs",
            "ptah_get_computer_run",
            "ptah_get_computer_run_events",
            "ptah_get_computer_capacity",
        ] {
            assert!(
                !names.contains(&denied),
                "{} received forbidden worker tool {denied}",
                lane.label
            );
        }
        authority
    }

    async fn connect_and_capture_authority(&mut self, campaign: &Campaign) {
        self.clients.clear();
        self.authority_baselines.clear();
        for lane in &self.lanes {
            let mut client = mcp_with_token(&campaign.service.addr, &lane.token).await;
            let authority = Self::authority_document(&mut client, lane).await;
            self.clients.push(client);
            self.authority_baselines.push(authority);
        }
    }

    async fn reconnect_and_assert_authority(&mut self, campaign: &Campaign) {
        self.clients.clear();
        for (index, lane) in self.lanes.iter().enumerate() {
            let mut client = mcp_with_token(&campaign.service.addr, &lane.token).await;
            let authority = Self::authority_document(&mut client, lane).await;
            assert_eq!(
                serde_json::to_vec(&authority).expect("worker authority bytes"),
                serde_json::to_vec(&self.authority_baselines[index])
                    .expect("baseline worker authority bytes"),
                "{} authority changed across restart",
                lane.label
            );
            self.clients.push(client);
        }
    }

    async fn begin_leases(
        &mut self,
        campaign: &mut Campaign,
        cycle: u64,
    ) -> Vec<Stage6WorkerLease> {
        let mut leases = Vec::with_capacity(self.lanes.len());
        for index in 0..self.lanes.len() {
            let lane = &self.lanes[index];
            let parent = call(
                &mut campaign.client,
                "ptah_create_work",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-parent", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "kind": "stage6-parent",
                    "objective": format!("Bounded parent for {} cycle {cycle}", lane.label),
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
                }),
            )
            .await;
            let parent_work_id = parent["work"]["workId"]
                .as_str()
                .expect("stage6 parent workId")
                .to_string();
            let child = call(
                &mut campaign.client,
                "ptah_create_work",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-child", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "kind": "stage6-child",
                    "objective": format!("Independent work for {} cycle {cycle}", lane.label),
                    "parent_work_id": parent_work_id,
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
                }),
            )
            .await;
            let work_id = child["work"]["workId"]
                .as_str()
                .expect("stage6 child workId")
                .to_string();
            assert_eq!(
                child["work"]["parentWorkId"].as_str(),
                Some(parent_work_id.as_str())
            );
            let _ = call(
                &mut campaign.client,
                "ptah_offer_work",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-offer", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": work_id,
                    "agent_id": lane.agent_id,
                    "reason": "exact bound worker"
                }),
            )
            .await;
            let _ = call(
                &mut self.clients[index],
                "ptah_heartbeat_worker",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-heartbeat", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "agent_id": lane.agent_id,
                    "host_kind": "service"
                }),
            )
            .await;
            let _ = call(
                &mut self.clients[index],
                "ptah_accept_work",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-accept", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": work_id,
                    "agent_id": lane.agent_id,
                    "reason": "bounded worker accepted"
                }),
            )
            .await;
            let claim_args = json!({
                "request_id": format!("stage6-{cycle}-{}-claim", lane.label),
                "session_id": lane.session,
                "workspace": &campaign.workspace,
                "work_id": work_id,
                "agent_id": lane.agent_id,
                "lease_ms": 3_600_000
            });
            let claimed = call(
                &mut self.clients[index],
                "ptah_claim_work",
                claim_args.clone(),
            )
            .await;
            let replayed = call(&mut self.clients[index], "ptah_claim_work", claim_args).await;
            assert_eq!(claimed, replayed, "claim replay must be identical");
            let attempt_id = claimed["attempt"]["attemptId"]
                .as_str()
                .expect("stage6 attemptId")
                .to_string();
            let lease_token = claimed["leaseToken"]
                .as_str()
                .expect("stage6 leaseToken")
                .to_string();
            let _ = call(
                &mut self.clients[index],
                "ptah_report_work_progress",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-progress", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": work_id,
                    "attempt_id": attempt_id,
                    "lease_token": lease_token,
                    "summary": format!("{} is active", lane.label),
                    "percent": 50
                }),
            )
            .await;
            let _ = call(
                &mut self.clients[index],
                "ptah_send_message",
                json!({
                    "request_id": format!("stage6-{cycle}-{}-message", lane.label),
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "kind": "status",
                    "from_agent_id": lane.agent_id,
                    "to_agent_id": lane.agent_id,
                    "work_id": work_id,
                    "attempt_id": attempt_id,
                    "body": format!("{} retained progress for cycle {cycle}", lane.label)
                }),
            )
            .await;
            leases.push(Stage6WorkerLease {
                lane_index: index,
                parent_work_id,
                work_id,
                attempt_id,
                lease_token,
                complete_request_id: format!("stage6-{cycle}-{}-complete", lane.label),
            });
        }

        let target = &leases[0];
        let attacker = &self.lanes[1];
        let cross_claim = call_expect_error(
            &mut self.clients[1],
            "ptah_claim_work",
            json!({
                "request_id": format!("stage6-{cycle}-cross-worker-claim"),
                "session_id": self.lanes[0].session,
                "workspace": &campaign.workspace,
                "work_id": target.work_id,
                "agent_id": attacker.agent_id,
                "lease_ms": 3_600_000
            }),
        )
        .await;
        assert!(
            cross_claim.contains("forbidden_scope")
                || cross_claim.contains("conflict")
                || cross_claim.contains("403")
                || cross_claim.contains("409"),
            "second worker must not obtain the active lease: {cross_claim}"
        );
        leases
    }

    async fn assert_leases_recovered(&self, campaign: &mut Campaign, leases: &[Stage6WorkerLease]) {
        for lease in leases {
            let lane = &self.lanes[lease.lane_index];
            let snapshot = call(
                &mut campaign.client,
                "ptah_get_work",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": lease.work_id
                }),
            )
            .await;
            assert!(
                matches!(
                    snapshot["work"]["state"].as_str(),
                    Some("leased") | Some("running")
                ),
                "recovered worker Work must remain in-flight: {}",
                snapshot["work"]["state"]
            );
            assert_eq!(
                snapshot["work"]["parentWorkId"].as_str(),
                Some(lease.parent_work_id.as_str())
            );
            let attempts = snapshot["attempts"]
                .as_array()
                .expect("recovered attempts array");
            assert_eq!(attempts.len(), 1, "restart introduced a duplicate attempt");
            assert_eq!(
                attempts[0]["attemptId"].as_str(),
                Some(lease.attempt_id.as_str())
            );
            assert!(
                matches!(
                    attempts[0]["state"].as_str(),
                    Some("leased") | Some("running")
                ),
                "recovered attempt must remain active: {}",
                attempts[0]
            );
        }
    }

    async fn complete_leases(
        &mut self,
        campaign: &mut Campaign,
        leases: &[Stage6WorkerLease],
    ) -> u32 {
        let mut duplicate_execution_count = 0u32;
        for lease in leases {
            let lane = &self.lanes[lease.lane_index];
            let complete_args = json!({
                "request_id": lease.complete_request_id,
                "session_id": lane.session,
                "workspace": &campaign.workspace,
                "work_id": lease.work_id,
                "attempt_id": lease.attempt_id,
                "lease_token": lease.lease_token,
                "summary": format!("{} completed bounded work", lane.label),
                "evidence": ["service-process worker lease completed"]
            });
            let preimage = call(
                &mut campaign.client,
                "ptah_get_work",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": lease.work_id
                }),
            )
            .await;
            let completed = match self.clients[lease.lane_index]
                .call_tool("ptah_complete_work", complete_args.clone())
                .await
            {
                Ok(result) => {
                    always_on_support::scan_mcp(
                        "ptah_complete_work",
                        &result.structured,
                        &result.raw,
                    );
                    assert!(
                        !result.is_error,
                        "ptah_complete_work error: {:?}; recovered={preimage}",
                        result.raw
                    );
                    result.structured
                }
                Err(error) => panic!("ptah_complete_work: {error}; recovered={preimage}"),
            };
            let replayed = call(
                &mut self.clients[lease.lane_index],
                "ptah_complete_work",
                complete_args,
            )
            .await;
            assert_eq!(completed, replayed, "completion replay must be identical");
            assert_eq!(completed["work"]["state"].as_str(), Some("succeeded"));
            let snapshot = call(
                &mut campaign.client,
                "ptah_get_work",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": lease.work_id
                }),
            )
            .await;
            let attempts = snapshot["attempts"]
                .as_array()
                .expect("terminal attempts array");
            duplicate_execution_count = duplicate_execution_count
                .saturating_add(u32::try_from(attempts.len().saturating_sub(1)).unwrap());
            assert_eq!(attempts.len(), 1, "worker Work gained duplicate attempts");
            assert_eq!(
                attempts[0]["attemptId"].as_str(),
                Some(lease.attempt_id.as_str())
            );
            if self.retained_work_ids.len() < REQUIRED_WORKERS {
                self.retained_work_ids
                    .push((lease.lane_index, lease.work_id.clone()));
            }
        }
        duplicate_execution_count
    }

    async fn rotate_credentials(&mut self, campaign: &mut Campaign) {
        let old_tokens = self
            .lanes
            .iter()
            .map(|lane| lane.token.clone())
            .collect::<Vec<_>>();
        for (index, lane) in self.lanes.iter_mut().enumerate() {
            lane.token = STAGE6_ROTATED_TOKENS[index].to_string();
        }
        self.clients.clear();
        campaign.service.replace_client_specs(self.client_specs());
        campaign.reopen().await;

        for old_token in &old_tokens {
            assert!(
                try_mcp(&campaign.service.addr, old_token).await.is_err(),
                "retired worker credential survived rotation"
            );
        }
        self.reconnect_and_assert_authority(campaign).await;

        self.credential_lifecycle.clear();
        for (index, lane) in self.lanes.iter().enumerate() {
            let old_fingerprint = hash_payload(&json!(old_tokens[index].as_str()));
            let new_fingerprint = hash_payload(&json!(lane.token.as_str()));
            let evidence_digest = hash_payload(&json!({
                "credentialId": lane.credential_id,
                "boundAgentId": lane.agent_id,
                "oldFingerprint": old_fingerprint.clone(),
                "newFingerprint": new_fingerprint.clone(),
                "authorityDocumentHash": self.authority_baselines[index]["documentHash"],
                "oldRejected": true,
                "newAccepted": true
            }));
            self.credential_lifecycle
                .push(WorkerCredentialLifecycleEvidence {
                    bound_agent_id: lane.agent_id.clone(),
                    credential_fingerprint: new_fingerprint,
                    issued: true,
                    least_privilege: true,
                    rotation_observed: true,
                    old_credential_rejected: true,
                    new_credential_accepted: true,
                    evidence_digest,
                });
        }
        Self::scan_credentials(campaign);
    }

    async fn retained_audit_entries(&mut self, campaign: &mut Campaign) -> u64 {
        let mut retained = 0u64;
        for (lane_index, work_id) in &self.retained_work_ids {
            let lane = &self.lanes[*lane_index];
            let snapshot = call(
                &mut campaign.client,
                "ptah_get_work",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": work_id
                }),
            )
            .await;
            assert_eq!(snapshot["work"]["state"].as_str(), Some("succeeded"));
            let attempts = snapshot["attempts"]
                .as_array()
                .expect("retained attempts array");
            assert_eq!(attempts.len(), 1);
            retained = retained.saturating_add(attempts.len() as u64);

            let decisions = call(
                &mut campaign.client,
                "ptah_list_work_decisions",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "work_id": work_id
                }),
            )
            .await;
            let decisions = decisions["decisions"]
                .as_array()
                .expect("retained decisions array");
            assert!(
                decisions.len() >= 2,
                "offer and accept decisions must remain"
            );
            let decision_ids = decisions
                .iter()
                .filter_map(|decision| decision["decisionId"].as_str())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(decision_ids.len(), decisions.len());
            retained = retained.saturating_add(decisions.len() as u64);

            let outbox = call(
                &mut campaign.client,
                "ptah_list_outbox",
                json!({
                    "session_id": lane.session,
                    "workspace": &campaign.workspace,
                    "agent_id": lane.agent_id,
                    "after_seq": 0
                }),
            )
            .await;
            let messages = outbox["messages"]
                .as_array()
                .expect("retained outbox array");
            assert!(!messages.is_empty(), "worker status message must remain");
            let mut sequences = messages
                .iter()
                .filter_map(|message| message["seq"].as_u64())
                .collect::<Vec<_>>();
            let ordered = sequences.clone();
            sequences.sort_unstable();
            assert_eq!(ordered, sequences, "message cursor order changed");
            retained = retained.saturating_add(messages.len() as u64);
        }
        retained
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

async fn wait_for_allowed_send_stability(campaign: &Campaign, semantic: &str, before: u64) -> u64 {
    let mut previous = campaign.provider.count_for(semantic);
    assert!(
        allowed_after_sends(before).contains(&previous),
        "provider sends changed outside the KnownNotSent allowance: before={before} observed={previous}"
    );
    let mut stable_periods = 0u64;
    let deadline = Instant::now()
        + campaign.fixture.supervisor_period
            * u32::try_from(campaign.fixture.zero_growth_periods.saturating_add(2))
                .expect("bounded stability periods");
    while Instant::now() < deadline {
        tokio::time::sleep(campaign.fixture.supervisor_period).await;
        let current = campaign.provider.count_for(semantic);
        assert!(
            allowed_after_sends(before).contains(&current),
            "provider sends changed outside the KnownNotSent allowance: before={before} observed={current}"
        );
        if current == previous {
            stable_periods += 1;
            if stable_periods >= campaign.fixture.zero_growth_periods {
                return current;
            }
        } else {
            previous = current;
            stable_periods = 0;
        }
    }
    panic!("provider sends did not stabilize for {semantic}: before={before} last={previous}");
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
    assert_eq!(fixture.posts_by_semantic.get("step-b-fix"), Some(&1));
    assert_eq!(fixture.posts_by_semantic.get("manager-decision"), Some(&1));
    let malformed = fixture.fail_closed_case("malformed");
    assert_eq!(malformed.run_state, "limit_reached");
    assert_eq!(malformed.stop_cause, "token_accounting_unavailable");
    assert_eq!(malformed.error_code, "max_total_tokens_usage_unavailable");
    assert_eq!(malformed.posts, 1);
    let cancel = fixture.fail_closed_case("cancel");
    assert_eq!(cancel.run_state, "cancelled");
    assert_eq!(cancel.stop_cause, "token_accounting_unavailable");
    assert_eq!(cancel.posts, 1);
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
        let before_sends = if semantic.is_empty() {
            0
        } else {
            campaign.provider.count_for(semantic)
        };
        campaign.reopen().await;
        if !step_id.is_empty() {
            tick_once(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &plan_id,
                &format!("{name}-after-reopen-a"),
            )
            .await;
            tick_once(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &plan_id,
                &format!("{name}-after-reopen-b"),
            )
            .await;
        }
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
                assert!(
                    allowed_after_sends(before.provider_posts).contains(&after.provider_posts),
                    "{name} provider sends changed outside the KnownNotSent allowance: before={} after={}",
                    before.provider_posts,
                    after.provider_posts
                );
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
        if !step_id.is_empty() {
            let after_sends =
                wait_for_allowed_send_stability(&campaign, semantic, before_sends).await;
            tick_once(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &plan_id,
                &format!("{name}-stability-a"),
            )
            .await;
            tick_once(
                &mut campaign.client,
                campaign.session,
                &campaign.workspace,
                &plan_id,
                &format!("{name}-stability-b"),
            )
            .await;
            let again_sends = campaign.provider.count_for(semantic);
            assert_eq!(
                after_sends, again_sends,
                "{name} duplicate tick resumed or duplicated a provider send"
            );
        }
        campaign.scan();
        certify("scheduler-window-not-a-cutpoint");
        certify("restart-exact-target-identities");
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
    for (before, after, expected) in cases {
        assert_eq!(
            allowed_after_sends(*before).contains(after),
            *expected,
            "allowed_after_sends({before}).contains({after})"
        );
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
    let recorded = recorded_assertions();
    for name in [
        "malformed-provider-exact-terminal",
        "disconnect-provider-exact-terminal",
        "status500-provider-exact-terminal",
        "timeout-provider-exact-terminal",
        "cancel-two-restarts-no-resend",
        "invalid-directive-zero-replacement",
    ] {
        assert!(
            recorded.contains(name),
            "required assertion {name} was not recorded by a runtime oracle: {recorded:?}"
        );
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
        |value| matches_fail_closed(value, run_id, expect),
    )
    .await;
    assert!(
        matches_fail_closed(&run, run_id, expect),
        "{semantic} fail-closed terminal: {run}"
    );
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
            |value| matches_fail_closed(value, run_id, expect),
        )
        .await;
        assert!(
            matches_fail_closed(&recovered, run_id, expect),
            "{semantic} fail-closed after restart: {recovered}"
        );
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
    let pid0 = campaign.service.pid();
    let before = cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await;
    campaign.reopen().await;
    let pid1 = campaign.service.pid();
    assert_ne!(pid1, pid0);
    soak_assert_interrupted_hold(campaign, &run_id, &semantic, &before).await;
    campaign.reopen().await;
    let pid2 = campaign.service.pid();
    assert_ne!(pid2, pid1);
    assert_ne!(pid2, pid0);
    soak_assert_interrupted_hold(campaign, &run_id, &semantic, &before).await;
}

async fn soak_assert_interrupted_hold(
    campaign: &mut Campaign,
    run_id: &str,
    semantic: &str,
    before: &EntityCardinalities,
) {
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
                && value["state"].as_str() == Some("interrupted")
                && pending_usage(value) == 0
        },
    )
    .await;
    assert_eq!(recovered["state"].as_str(), Some("interrupted"));
    assert_eq!(pending_usage(&recovered), 0);
    assert_eq!(campaign.provider.count_for(semantic), 1);
    let window = campaign.fixture.supervisor_period
        * u32::try_from(campaign.fixture.zero_growth_periods).expect("periods");
    tokio::time::sleep(window).await;
    assert_eq!(
        cardinalities(&mut campaign.client, campaign.session, &campaign.workspace).await,
        *before
    );
    assert_eq!(campaign.provider.count_for(semantic), 1);
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
    let pid0 = campaign.service.pid();
    campaign.reopen().await;
    let pid1 = campaign.service.pid();
    assert_ne!(pid1, pid0);
    assert_interrupted_fence(campaign, &join, &step).await;
    assert_zero_growth_window(campaign, &join, &step).await;
    campaign.reopen().await;
    let pid2 = campaign.service.pid();
    assert_ne!(pid2, pid1);
    assert_ne!(pid2, pid0);
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
    assert!(
        duration_ms <= campaign.fixture.ceilings.max_cycle_latency_ms,
        "one-cycle smoke exceeded latency ceiling: actual={duration_ms}ms ceiling={}ms",
        campaign.fixture.ceilings.max_cycle_latency_ms
    );
    let growth = max.growth_from(&baseline);
    assert_resource_ceilings(&max, &growth, &campaign.fixture);
    assert!(
        campaign.provider.live_threads() <= campaign.fixture.ceilings.max_threads,
        "provider live threads exceed ceiling"
    );
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
        "restarts": 2,
        "sends": campaign.provider.send_count(),
        "providerLiveThreads": campaign.provider.live_threads(),
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
        restarts += 2;
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
    assert!(
        campaign.provider.live_threads() <= campaign.fixture.ceilings.max_threads,
        "provider live threads exceed ceiling"
    );
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
        "providerLiveThreads": campaign.provider.live_threads(),
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

fn stage6_worker_check(check_id: &str, duration_ms: u64, facts: Value) -> WorkerCheckEvidence {
    assert!(REQUIRED_WORKER_CHECKS.contains(&check_id));
    WorkerCheckEvidence {
        check_id: check_id.to_string(),
        passed: true,
        duration_ms: duration_ms.max(1),
        evidence_digest: hash_payload(&json!({
            "checkId": check_id,
            "facts": facts
        })),
    }
}

fn persist_stage6_worker_report(evidence: &LongRunningWorkerEvidence) {
    evidence.validate().expect("valid Stage 6 worker evidence");
    assert!(evidence.certification_ready());
    let encoded = serde_json::to_vec_pretty(evidence).expect("Stage 6 report bytes");
    let text = String::from_utf8(encoded.clone()).expect("Stage 6 report UTF-8");
    scan_text("stage6-worker-report", &text);
    for secret in STAGE6_INITIAL_TOKENS
        .iter()
        .chain(STAGE6_ROTATED_TOKENS.iter())
    {
        assert!(
            !text.contains(secret),
            "Stage 6 report persisted a worker credential"
        );
    }
    let path = std::env::temp_dir().join(format!(
        "always-on-grokbot-workers-{}-{}.json",
        evidence.candidate_sha, evidence.campaign_id
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("create unique Stage 6 report");
    file.write_all(&encoded).expect("persist Stage 6 report");
    file.sync_all().expect("fsync Stage 6 report");
    let roundtrip: LongRunningWorkerEvidence =
        serde_json::from_slice(&std::fs::read(&path).expect("read Stage 6 report"))
            .expect("Stage 6 report roundtrip");
    assert_eq!(&roundtrip, evidence);
    roundtrip.validate().expect("roundtrip Stage 6 report");
    eprintln!("stage6_worker_evidence={}", path.display());
}

fn assert_stage6_candidate_unchanged(expected_sha: Option<&str>) -> String {
    let candidate_sha = repository_commit();
    if let Some(expected_sha) = expected_sha {
        assert_eq!(
            candidate_sha, expected_sha,
            "Stage 6 repository HEAD changed during the campaign"
        );
    }
    let status = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .expect("git status for Stage 6 candidate");
    assert!(status.status.success(), "git status failed");
    assert!(
        status.stdout.is_empty(),
        "Stage 6 certification requires a clean exact candidate: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    candidate_sha
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stage6_multi_worker_restart_rotation_smoke() {
    let _serial = serial_lock();
    let mut campaign = Campaign::start().await;
    let mut workers = Stage6WorkerPool::bootstrap(&mut campaign).await;
    let leases = workers.begin_leases(&mut campaign, 0).await;

    campaign.reopen().await;
    workers.reconnect_and_assert_authority(&campaign).await;
    workers
        .assert_leases_recovered(&mut campaign, &leases)
        .await;
    assert_eq!(workers.complete_leases(&mut campaign, &leases).await, 0);

    workers.rotate_credentials(&mut campaign).await;
    let retained = workers.retained_audit_entries(&mut campaign).await;
    assert!(
        retained >= 8,
        "worker attempt/decision/message evidence missing"
    );
    assert_eq!(workers.credential_lifecycle.len(), REQUIRED_WORKERS);
    Stage6WorkerPool::scan_credentials(&campaign);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn certify_stage6_multi_worker_72h() {
    let _serial = serial_lock();
    let requested_seconds = soak_seconds_for_mode("72h", REQUIRED_SOAK_SECONDS);
    assert_eq!(requested_seconds, REQUIRED_SOAK_SECONDS);
    let candidate_sha = assert_stage6_candidate_unchanged(None);

    let started_at = Utc::now();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(requested_seconds);
    let mut campaign = Campaign::start().await;
    let baseline = campaign.service.sample_tree();
    let mut max = baseline.clone();
    let mut restart_count = 0u32;
    let mut duplicate_execution_count = 0u32;
    let mut cycles = 0u64;
    let mut max_cycle_latency_ms = 0u64;

    let mut workers = Stage6WorkerPool::bootstrap(&mut campaign).await;
    max.max_with(&campaign.service.sample_tree());
    restart_count = restart_count.saturating_add(1);
    let leases = workers.begin_leases(&mut campaign, cycles).await;
    max.max_with(&campaign.service.sample_tree());
    cycles = cycles.saturating_add(1);

    let _ = barrier_restart_on_campaign(&mut campaign).await;
    max.max_with(&campaign.service.sample_tree());
    restart_count = restart_count.saturating_add(2);
    workers.reconnect_and_assert_authority(&campaign).await;
    workers
        .assert_leases_recovered(&mut campaign, &leases)
        .await;
    duplicate_execution_count = duplicate_execution_count
        .saturating_add(workers.complete_leases(&mut campaign, &leases).await);
    max.max_with(&campaign.service.sample_tree());

    workers.rotate_credentials(&mut campaign).await;
    restart_count = restart_count.saturating_add(1);
    assert!(restart_count >= REQUIRED_RESTARTS);
    max.max_with(&campaign.service.sample_tree());

    while Instant::now() < deadline {
        let cycle_started = Instant::now();
        max.max_with(&campaign.service.sample_tree());
        let leases = workers.begin_leases(&mut campaign, cycles).await;
        max.max_with(&campaign.service.sample_tree());
        duplicate_execution_count = duplicate_execution_count
            .saturating_add(workers.complete_leases(&mut campaign, &leases).await);
        cycles = cycles.saturating_add(1);
        max.max_with(&campaign.service.sample_tree());
        max_cycle_latency_ms = max_cycle_latency_ms.max(cycle_started.elapsed().as_millis() as u64);
        Stage6WorkerPool::scan_credentials(&campaign);

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(STAGE6_WORK_INTERVAL.min(remaining)).await;
    }

    let finished_at = Utc::now();
    let elapsed_wall_seconds = finished_at
        .signed_duration_since(started_at)
        .num_seconds()
        .max(0) as u64;
    let soak_seconds = started.elapsed().as_secs().min(elapsed_wall_seconds);
    assert!(
        soak_seconds >= REQUIRED_SOAK_SECONDS,
        "Stage 6 campaign ended before 72 measured hours"
    );
    let growth = max.growth_from(&baseline);
    assert_resource_ceilings(&max, &growth, &campaign.fixture);
    assert!(
        campaign.provider.live_threads() <= campaign.fixture.ceilings.max_threads,
        "provider live threads exceed Stage 6 ceiling"
    );
    assert!(
        max_cycle_latency_ms <= campaign.fixture.ceilings.max_cycle_latency_ms,
        "Stage 6 worker cycle latency {max_cycle_latency_ms} exceeds {}",
        campaign.fixture.ceilings.max_cycle_latency_ms
    );
    assert_eq!(duplicate_execution_count, 0);

    let retained_audit_entries = workers.retained_audit_entries(&mut campaign).await;
    assert!(retained_audit_entries > 0);
    assert_eq!(workers.credential_lifecycle.len(), REQUIRED_WORKERS);
    assert_stage6_candidate_unchanged(Some(&candidate_sha));
    let worker_ids = workers
        .lanes
        .iter()
        .map(|lane| lane.agent_id.clone())
        .collect::<Vec<_>>();
    let proof_duration_ms = started.elapsed().as_millis() as u64;
    let operational_duration_ms = soak_seconds.saturating_mul(1000);
    let lifecycle_value = serde_json::to_value(&workers.credential_lifecycle)
        .expect("credential lifecycle evidence value");
    let checks = vec![
        stage6_worker_check(
            "multi_worker_leases",
            proof_duration_ms,
            json!({"workers": worker_ids.clone(), "cycles": cycles}),
        ),
        stage6_worker_check(
            "crash_restart_recovery",
            proof_duration_ms,
            json!({"restarts": restart_count, "recoveredLeases": REQUIRED_WORKERS}),
        ),
        stage6_worker_check(
            "no_duplicate_execution",
            proof_duration_ms,
            json!({"duplicateExecutionCount": duplicate_execution_count}),
        ),
        stage6_worker_check(
            "credential_issuance",
            proof_duration_ms,
            lifecycle_value.clone(),
        ),
        stage6_worker_check("credential_rotation", proof_duration_ms, lifecycle_value),
        stage6_worker_check(
            "retained_audit",
            proof_duration_ms,
            json!({"retainedAuditEntries": retained_audit_entries}),
        ),
        stage6_worker_check(
            "operational_soak",
            operational_duration_ms,
            json!({
                "soakSeconds": soak_seconds,
                "cycles": cycles,
                "maxRssBytes": max.rss_bytes,
                "maxFdCount": max.fd_count,
                "maxThreads": max.threads,
                "maxDiskBytes": max.disk_bytes,
                "maxCycleLatencyMs": max_cycle_latency_ms
            }),
        ),
    ];
    let check_ids = checks
        .iter()
        .map(|check| check.check_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(check_ids.as_slice(), REQUIRED_WORKER_CHECKS.as_slice());

    let mut evidence = LongRunningWorkerEvidence {
        schema: WORKER_CERTIFICATION_EVIDENCE_SCHEMA.to_string(),
        certification_id: format!("stage6-workers-{}", &candidate_sha[..12]),
        candidate_sha,
        campaign_id: format!("stage6-72h-{}", started_at.timestamp()),
        started_at,
        finished_at,
        workers: worker_ids,
        checks,
        credential_lifecycle: workers.credential_lifecycle,
        restart_count,
        duplicate_execution_count,
        retained_audit_entries,
        soak_seconds,
        secret_free: true,
        claim_eligible: true,
        evidence_digest: String::new(),
    };
    evidence.evidence_digest = expected_worker_evidence_digest(&evidence);
    persist_stage6_worker_report(&evidence);
    Stage6WorkerPool::scan_credentials(&campaign);
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
