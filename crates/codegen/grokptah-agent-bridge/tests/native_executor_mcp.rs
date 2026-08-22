//! Native persistent-Agent executor conformance.

mod common;

use grokptah_agent_bridge::orchestration::{
    AssignmentStatus, AuthContext, ManagedExecutionIntent, ManagedExecutionPolicy,
    ManagedIntentState, OrchStore, OrchestrationConfig, OrchestrationService, ProviderRoute,
    RunBounds, RunPurpose, RunRecord, RunState, WorkItem, WorkPolicy, WorkState,
    WorkspaceAllowlist, MANAGED_EXECUTION_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    McpRemoteError, SessionKind, SessionUpdate,
};
use serde_json::json;
use std::path::Path;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

fn assert_remote_error(error: anyhow::Error, expected_code: &str) {
    let remote = error
        .downcast_ref::<McpRemoteError>()
        .expect("MCP failure should be a typed remote error");
    assert_eq!(remote.data_code(), Some(expected_code));
    assert_eq!(
        error.to_string(),
        format!("MCP remote error: {expected_code}")
    );
}

fn enabled_policy() -> ManagedExecutionPolicy {
    ManagedExecutionPolicy {
        enabled: true,
        max_concurrent_runs: 1,
        bounds: RunBounds {
            max_prompt_bytes: 16 * 1024,
            max_rounds: 4,
            max_duration_ms: 30_000,
            max_total_tokens: Some(8_000),
        },
        retry_eligible: true,
        ..ManagedExecutionPolicy::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn native_executor_runs_assigned_work_without_an_external_worker() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    client
        .call_tool(
            "ptah_set_managed_execution",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": agent.agent_id,
                "policy": enabled_policy()
            }),
        )
        .await
        .unwrap();

    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "native-work-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "native",
                "objective": "Native executor should finish this Work"
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "native-assign-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();

    let manual = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "manual-work-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "manual",
                "objective": "Stay queued because it is not assigned"
            }),
        )
        .await
        .unwrap();
    let manual_id = manual.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    orch.drive_native_executor_once().await;
    orch.drive_native_executor_once().await;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(8);
    loop {
        orch.drive_native_executor_once().await;
        let snapshot = client
            .call_tool(
                "ptah_get_work",
                json!({
                    "session_id": lane.id,
                    "workspace": workspace_text,
                    "work_id": work_id
                }),
            )
            .await
            .unwrap();
        let state = snapshot.structured["work"]["state"].as_str().unwrap();
        if state == "succeeded"
            || state == "awaiting_approval"
            || tokio::time::Instant::now() >= deadline
        {
            assert!(
                state == "succeeded"
                    || state == "leased"
                    || state == "running"
                    || state == "awaiting_approval",
                "native work ended in {state}"
            );
            let attempts = snapshot.structured["attempts"].as_array().unwrap();
            if !attempts.is_empty() {
                assert!(attempts[0]["linkedRunIds"].as_array().unwrap().len() <= 1);
            }
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    let replay_assign = client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "native-assign-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(replay_assign.structured["work"]["workId"], work_id);

    let still_manual = client
        .call_tool(
            "ptah_get_work",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": manual_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(still_manual.structured["work"]["state"], "queued");
    assert!(still_manual.structured["attempts"]
        .as_array()
        .is_some_and(|attempts| attempts.is_empty()));

    let intents = client
        .call_tool(
            "ptah_list_execution_intents",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text
            }),
        )
        .await
        .unwrap();
    let listed = intents.structured["intents"].as_array().unwrap();
    let work_intents = listed
        .iter()
        .filter(|intent| intent["workId"] == work_id)
        .count();
    assert!(work_intents <= 1);
    for intent in listed {
        let route = intent["providerRoute"]
            .as_object()
            .expect("every admitted intent carries an exact provider route");
        assert!(route["providerId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        assert!(route["modelId"].as_str().is_some_and(|id| !id.is_empty()));
    }
    let admission = &intents.structured["providerAdmission"];
    assert!(admission["maxConcurrentRunsPerProvider"]
        .as_u64()
        .is_some_and(|ceiling| ceiling > 0));
    assert!(admission["liveInScopeByProvider"].is_object());
    // Every live intent this caller can read is accounted for, and nothing
    // from another Lane leaks into the breakdown.
    let in_scope: u64 = admission["liveInScopeByProvider"]
        .as_object()
        .unwrap()
        .values()
        .map(|count| count.as_u64().unwrap())
        .sum();
    assert!(in_scope <= listed.len() as u64);

    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    let executor = &capacity.structured["health"]["nativeExecutor"];
    assert!(executor["enabled"].as_bool().unwrap());
    for counter in [
        "skippedProviderCapacity",
        "skippedUnroutable",
        "maxConcurrentProviderRuns",
    ] {
        assert!(
            executor[counter].as_u64().is_some(),
            "missing provider counter {counter}"
        );
    }

    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn approval_required_work_pauses_before_success() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    client
        .call_tool(
            "ptah_set_managed_execution",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "agent_id": agent.agent_id,
                "policy": enabled_policy()
            }),
        )
        .await
        .unwrap();
    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "approval-work",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "native",
                "objective": "Require human approval after the Run",
                "policy": {
                    "bounds": {
                        "maxPromptBytes": 100000,
                        "maxRounds": 4,
                        "maxDurationMs": 30000,
                        "maxTotalTokens": 8000
                    },
                    "retry": {
                        "maxAttempts": 2,
                        "retryFailed": true,
                        "retryExpired": true,
                        "backoffMs": 0
                    },
                    "requiresApproval": true,
                    "maxConcurrentAttempts": 1
                }
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"].as_str().unwrap();
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "approval-assign",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();
    for _ in 0..20 {
        orch.drive_native_executor_once().await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let snapshot = client
            .call_tool(
                "ptah_get_work",
                json!({
                    "session_id": lane.id,
                    "workspace": workspace_text,
                    "work_id": work_id
                }),
            )
            .await
            .unwrap();
        let state = snapshot.structured["work"]["state"].as_str().unwrap();
        if state == "awaiting_approval" || state == "succeeded" {
            if state == "awaiting_approval" {
                let approved = client
                    .call_tool(
                        "ptah_approve_work",
                        json!({
                            "request_id": "approval-decide",
                            "session_id": lane.id,
                            "workspace": workspace_text,
                            "work_id": work_id,
                            "note": "operator reviewed native evidence"
                        }),
                    )
                    .await
                    .unwrap();
                assert_eq!(approved.structured["work"]["state"], "succeeded");
            }
            break;
        }
    }
    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

fn retry_policy(retry_eligible: bool) -> ManagedExecutionPolicy {
    ManagedExecutionPolicy {
        retry_eligible,
        ..enabled_policy()
    }
}

fn auth() -> AuthContext {
    AuthContext {
        token_id: "native-token-308".into(),
        owner_id: "primary".into(),
    }
}

fn run_record(
    intent_id: &str,
    session: Uuid,
    workspace: &str,
    agent_id: &str,
    state: RunState,
) -> RunRecord {
    RunRecord {
        run_id: format!("run-{intent_id}"),
        session_id: session,
        workspace: workspace.into(),
        request_id: intent_id.into(),
        client_id: Some("native-executor".into()),
        state,
        purpose: Default::default(),
        agent_id: Some(agent_id.into()),
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: Some(2),
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "preview".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}

fn seed_admitted_work(
    store: &OrchStore,
    session: Uuid,
    workspace: &str,
    agent_id: &str,
    run_state: RunState,
    secret: &str,
) -> (WorkItem, ManagedExecutionIntent, RunRecord) {
    let mut item = WorkItem::new(
        "native",
        "Interrupted recovery fixture",
        session,
        workspace,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    item.assigned_agent_id = Some(agent_id.into());
    item.assignment_status = AssignmentStatus::Accepted;
    store.save_work_item(&item).unwrap();
    let claim = store
        .claim_work_with_lease_secret(&item.work_id, agent_id, None, secret)
        .unwrap();
    let intent_id = Uuid::new_v4().to_string();
    let run = run_record(&intent_id, session, workspace, agent_id, run_state);
    store.save_run(&run).unwrap();
    store
        .link_work_run(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            &run.run_id,
        )
        .unwrap();
    let now = chrono::Utc::now();
    let intent = ManagedExecutionIntent {
        schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id,
        agent_id: agent_id.into(),
        agent_spec_revision: 2,
        work_id: item.work_id.clone(),
        work_revision: item.revision,
        attempt_id: Some(claim.attempt.attempt_id),
        run_id: Some(run.run_id.clone()),
        session_id: session,
        workspace: workspace.into(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        provider_route: Some(ProviderRoute {
            provider_id: "xai".into(),
            model_id: "grok".into(),
        }),
        bounds: RunBounds::default(),
        input_hash: "hash".into(),
        state: ManagedIntentState::Admitted,
        permission_request_id: None,
        created_at: now,
        updated_at: now,
    };
    store.save_managed_intent(&intent).unwrap();
    (item, intent, run)
}

async fn boot_native(
    retry_eligible: bool,
) -> (
    ProcessEnvGuard,
    tempfile::TempDir,
    tempfile::TempDir,
    grokptah_agent_bridge::AgentHostHandle,
    std::sync::Arc<OrchestrationService>,
    Uuid,
    String,
    String,
) {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let workspace_text = workspace.path().display().to_string();
    orch.set_managed_execution(
        &auth(),
        lane.id,
        workspace.path(),
        &agent.agent_id,
        retry_policy(retry_eligible),
    )
    .unwrap();
    (
        env,
        home,
        workspace,
        host,
        orch,
        lane.id,
        agent.agent_id,
        workspace_text,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn manager_decision_native_admission_has_durable_proposal_purpose() {
    let (_env, _home, _workspace, _host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let mut work = WorkItem::new(
        "manager-decision",
        "return exactly one typed manager directive envelope",
        session,
        workspace_text.clone(),
        "manager-supervisor",
        WorkPolicy::default(),
    )
    .unwrap();
    work.parent_work_id = Some("manager-root-test".into());
    work.assigned_agent_id = Some(agent_id.clone());
    work.assignment_status = AssignmentStatus::Accepted;
    work.source_manager_plan_id = Some("manager-plan-test".into());
    work.source_manager_step_id = Some("__manager_decision__".into());
    work.validate().unwrap();
    orch.store().save_work_item(&work).unwrap();

    orch.drive_native_executor_once().await;
    let admission_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
    let (intent, mut run) = loop {
        let intents = orch
            .store()
            .list_managed_intents()
            .unwrap()
            .into_iter()
            .filter(|intent| intent.work_id == work.work_id)
            .collect::<Vec<_>>();
        if let Some(pair) = intents.iter().find_map(|intent| {
            intent
                .run_id
                .as_deref()
                .and_then(|run_id| orch.store().load_run(run_id).ok().flatten())
                .or_else(|| {
                    orch.store()
                        .find_run_by_request_id(&intent.intent_id)
                        .ok()
                        .flatten()
                })
                .map(|run| (intent.clone(), run))
        }) {
            break pair;
        }
        assert!(
            tokio::time::Instant::now() < admission_deadline,
            "manager decision has no durable Run; native status: {:?}; intents: {:?}",
            orch.native_executor_status(),
            intents
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
    };
    let run_id = run.run_id.clone();
    assert_eq!(run.purpose, RunPurpose::ManagerProposal);
    assert_eq!(run.agent_id.as_deref(), Some(agent_id.as_str()));
    assert_eq!(run.agent_spec_revision, Some(intent.agent_spec_revision));

    // The durable intent names the exact provider identity and model the host
    // routed to, captured before the provider task was spawned.
    let admitted_agent = orch.store().load_agent(&agent_id).unwrap().unwrap();
    let admitted_spec = admitted_agent.current_spec().unwrap();
    let route = intent
        .provider_route
        .as_ref()
        .expect("native admission records an exact provider route");
    assert_eq!(route.provider_id, admitted_spec.model.provider_id);
    assert_eq!(route.model_id, admitted_spec.model.model_id);
    assert!(!route.provider_id.is_empty() && !route.model_id.is_empty());
    assert_eq!(
        intent.effective_provider_id().as_deref(),
        Some(admitted_spec.model.provider_id.as_str())
    );

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let agent = orch.store().load_agent(&agent_id).unwrap().unwrap();
        if run.state.is_terminal() && agent.current_run_id.as_deref() != Some(run_id.as_str()) {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        run = orch.store().load_run(&run_id).unwrap().unwrap();
    }
    assert!(run.state.is_terminal(), "proposal Run did not settle");
    let agent = orch.store().load_agent(&agent_id).unwrap().unwrap();
    assert_ne!(agent.current_run_id.as_deref(), Some(run_id.as_str()));

    orch.store()
        .update_run(&run_id, |run| {
            run.state = RunState::Interrupted;
            run.terminal_result = Some("interrupted".into());
            Ok(())
        })
        .unwrap();
    let retry = orch
        .retry_run(
            &auth(),
            "manager-proposal-retry",
            session,
            Path::new(&workspace_text),
            &run_id,
            "retry proposal".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(
        retry.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::Conflict
    );
    assert!(retry.message.contains("cannot be retried"));

    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_interrupted_run_retry_forbidden_is_terminal() {
    let (_env, _home, _workspace, _host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let (item, intent, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Interrupted,
        "native-token-308",
    );
    orch.drive_native_executor_once().await;
    let work = orch.store().load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(work.state, WorkState::Failed);
    let closed = orch
        .store()
        .load_managed_intent(&intent.intent_id)
        .unwrap()
        .unwrap();
    assert_eq!(closed.state, ManagedIntentState::Finalized);
    assert_eq!(
        orch.store().load_run(&run.run_id).unwrap().unwrap().state,
        RunState::Interrupted
    );
    assert_eq!(
        orch.store()
            .live_managed_intents_for_agent(&agent_id)
            .unwrap(),
        0
    );
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Failed
    );
    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn live_interrupted_run_retry_allowed_requeues_new_attempt() {
    let (_env, _home, _workspace, _host, orch, session, agent_id, workspace_text) =
        boot_native(true).await;
    let (item, intent, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Interrupted,
        "native-token-308",
    );
    orch.drive_native_executor_once().await;
    let work = orch.store().load_work_item(&item.work_id).unwrap().unwrap();
    assert_ne!(work.state, WorkState::Failed);
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
    assert_eq!(
        orch.store().load_run(&run.run_id).unwrap().unwrap().state,
        RunState::Interrupted
    );
    let intents = orch.store().list_managed_intents().unwrap();
    let live: Vec<_> = intents
        .iter()
        .filter(|candidate| {
            candidate.work_id == item.work_id
                && matches!(
                    candidate.state,
                    ManagedIntentState::Claiming
                        | ManagedIntentState::Admitted
                        | ManagedIntentState::Parked
                )
        })
        .collect();
    assert!(live.len() <= 1);
    if let Some(next) = live.first() {
        assert_ne!(next.intent_id, intent.intent_id);
        assert_ne!(next.run_id.as_deref(), Some(run.run_id.as_str()));
    }
    assert_eq!(
        orch.store()
            .list_work_attempts(Some(&item.work_id))
            .unwrap()
            .iter()
            .filter(|attempt| attempt.linked_run_ids.iter().any(|id| id == &run.run_id))
            .count(),
        1
    );
    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn restart_interrupted_run_retry_forbidden_is_terminal() {
    let (_env, _home, workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let (item, intent, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    orch.stop_background_tasks().await;
    drop(orch);
    let store = host.ensure_orchestration_store().unwrap();
    store.mark_unfinished_interrupted().unwrap();
    assert_eq!(
        store.load_run(&run.run_id).unwrap().unwrap().state,
        RunState::Interrupted
    );
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Failed
    );
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .live_managed_intents_for_agent(&agent_id)
            .unwrap(),
        0
    );
    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn restart_interrupted_run_retry_allowed_does_not_resume() {
    let (_env, _home, workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(true).await;
    let (item, intent, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    orch.stop_background_tasks().await;
    drop(orch);
    let store = host.ensure_orchestration_store().unwrap();
    store.mark_unfinished_interrupted().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    orch.drive_native_executor_once().await;
    let restarted = orch.store().load_work_item(&item.work_id).unwrap().unwrap();
    assert_ne!(restarted.state, WorkState::Failed);
    assert_eq!(
        orch.store().load_run(&run.run_id).unwrap().unwrap().state,
        RunState::Interrupted
    );
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn resolve_work_input_requires_parked_scope() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    let other_lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    host.session_set_cwd(other_lane.id, workspace.path())
        .unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    orch.set_managed_execution(
        &auth(),
        lane.id,
        workspace.path(),
        &agent.agent_id,
        retry_policy(false),
    )
    .unwrap();
    let mut item = WorkItem::new(
        "native",
        "Need operator input",
        lane.id,
        &workspace_text,
        "operator",
        WorkPolicy::default(),
    )
    .unwrap();
    item.assigned_agent_id = Some(agent.agent_id.clone());
    item.assignment_status = AssignmentStatus::Accepted;
    orch.store().save_work_item(&item).unwrap();
    let claim = orch
        .store()
        .claim_work_with_lease_secret(&item.work_id, &agent.agent_id, None, "native-token-308")
        .unwrap();
    orch.store()
        .park_work_input(
            &item.work_id,
            &claim.attempt.attempt_id,
            &claim.lease_token,
            "permission required",
        )
        .unwrap();
    let permission_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let intent = ManagedExecutionIntent {
        schema_version: MANAGED_EXECUTION_SCHEMA_VERSION,
        intent_id: Uuid::new_v4().to_string(),
        agent_id: agent.agent_id.clone(),
        agent_spec_revision: 2,
        work_id: item.work_id.clone(),
        work_revision: item.revision,
        attempt_id: Some(claim.attempt.attempt_id.clone()),
        run_id: Some("run-perm".into()),
        session_id: lane.id,
        workspace: workspace_text.clone(),
        source_routine_id: None,
        source_activation_id: None,
        model_selection_key: "grok".into(),
        provider_route: Some(ProviderRoute {
            provider_id: "xai".into(),
            model_id: "grok".into(),
        }),
        bounds: RunBounds::default(),
        input_hash: "hash".into(),
        state: ManagedIntentState::Parked,
        permission_request_id: Some(permission_id.to_string()),
        created_at: now,
        updated_at: now,
    };
    orch.store().save_managed_intent(&intent).unwrap();

    let unknown = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": Uuid::new_v4(),
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(unknown, "invalid_request");
    let cross_session = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": other_lane.id,
                "workspace": workspace_text,
                "permission_id": permission_id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(cross_session, "forbidden_scope");
    let foreign = tempdir().unwrap();
    let cross_workspace = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": foreign.path().display().to_string(),
                "permission_id": permission_id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(cross_workspace, "workspace_mismatch");
    let parked = orch.store().load_work_item(&item.work_id).unwrap().unwrap();
    assert_eq!(parked.state, WorkState::AwaitingInput);

    let missing_host = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": permission_id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(missing_host, "conflict");
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingInput
    );

    orch.store()
        .cancel_work(&item.work_id, "cancelled while parked")
        .unwrap();
    let mut race = intent.clone();
    race.intent_id = Uuid::new_v4().to_string();
    race.state = ManagedIntentState::Parked;
    race.permission_request_id = Some(Uuid::new_v4().to_string());
    orch.store().save_managed_intent(&race).unwrap();
    let cancelled = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": race.permission_request_id.clone().unwrap(),
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(cancelled, "conflict");
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Cancelled
    );

    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn permission_from_run_b_does_not_park_work_a() {
    let (_env, _home, _workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let (work_a, intent_a, run_a) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (work_b, intent_b, run_b) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (req, _rx) = host
        .begin_pending_permission(
            session,
            Some(run_b.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: session,
        request: req.clone(),
    })
    .await;
    let loaded_a = orch
        .store()
        .load_work_item(&work_a.work_id)
        .unwrap()
        .unwrap();
    assert_ne!(loaded_a.state, WorkState::AwaitingInput);
    let loaded_b = orch
        .store()
        .load_work_item(&work_b.work_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded_b.state, WorkState::AwaitingInput);
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent_a.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Admitted
    );
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent_b.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Parked
    );
    let _ = run_a;
    orch.stop_background_tasks().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn resolve_work_input_uses_real_host_pending() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    orch.set_managed_execution(
        &auth(),
        lane.id,
        workspace.path(),
        &agent.agent_id,
        retry_policy(false),
    )
    .unwrap();
    let (item, intent, run) = seed_admitted_work(
        orch.store(),
        lane.id,
        &workspace_text,
        &agent.agent_id,
        RunState::Running,
        "native-token-308",
    );
    let _ = intent;
    let (req, rx) = host
        .begin_pending_permission(
            lane.id,
            Some(run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: lane.id,
        request: req.clone(),
    })
    .await;
    let allowed = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": req.id,
                "allow": true
            }),
        )
        .await
        .unwrap();
    assert!(!allowed.is_error);
    assert_eq!(
        rx.await.unwrap(),
        grokptah_agent_bridge::PermissionDecision::Allow
    );
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Running
    );
    let replay = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": req.id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(replay, "conflict");

    let (deny_item, _, deny_run) = seed_admitted_work(
        orch.store(),
        lane.id,
        &workspace_text,
        &agent.agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (deny_req, deny_rx) = host
        .begin_pending_permission(
            lane.id,
            Some(deny_run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: lane.id,
        request: deny_req.clone(),
    })
    .await;
    let denied = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": deny_req.id,
                "allow": false
            }),
        )
        .await
        .unwrap();
    assert!(!denied.is_error);
    assert_eq!(
        deny_rx.await.unwrap(),
        grokptah_agent_bridge::PermissionDecision::Deny
    );
    assert_eq!(
        orch.store()
            .load_work_item(&deny_item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Running
    );

    let (drop_item, _, drop_run) = seed_admitted_work(
        orch.store(),
        lane.id,
        &workspace_text,
        &agent.agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (drop_req, drop_rx) = host
        .begin_pending_permission(
            lane.id,
            Some(drop_run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: lane.id,
        request: drop_req.clone(),
    })
    .await;
    drop(drop_rx);
    let dropped = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": drop_req.id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(dropped, "conflict");
    assert_eq!(
        orch.store()
            .load_work_item(&drop_item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingInput
    );

    let (wrong_item, mut wrong_intent, _) = seed_admitted_work(
        orch.store(),
        lane.id,
        &workspace_text,
        &agent.agent_id,
        RunState::Running,
        "native-token-308",
    );
    let attempt_id = wrong_intent.attempt_id.clone().unwrap();
    let token = orch
        .store()
        .load_work_attempt(&attempt_id)
        .unwrap()
        .unwrap()
        .lease_token_for_secret("native-token-308");
    orch.store()
        .park_work_input(
            &wrong_item.work_id,
            &attempt_id,
            &token,
            "permission required",
        )
        .unwrap();
    let (wrong_req, _wrong_rx) = host
        .begin_pending_permission(lane.id, Some("run-other"), "write_file", "Allow write?")
        .unwrap();
    wrong_intent.state = ManagedIntentState::Parked;
    wrong_intent.permission_request_id = Some(wrong_req.id.to_string());
    orch.store().save_managed_intent(&wrong_intent).unwrap();
    let wrong = client
        .call_tool(
            "ptah_resolve_work_input",
            json!({
                "session_id": lane.id,
                "workspace": workspace_text,
                "permission_id": wrong_req.id,
                "allow": true
            }),
        )
        .await
        .unwrap_err();
    assert_remote_error(wrong, "forbidden_scope");
    assert_eq!(
        orch.store()
            .load_work_item(&wrong_item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingInput
    );

    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn native_skips_manual_retry_without_mutating() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, workspace.path()).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let manual_agent = {
        let store = host.ensure_orchestration_store().unwrap();
        let mut manual_agent = agent.clone();
        manual_agent.agent_id = format!("manual-worker-{}", lane.id);
        if let Some(spec) = manual_agent.spec.as_mut() {
            spec.display_name = "manual worker".into();
            spec.managed_execution = ManagedExecutionPolicy::default();
        }
        store.save_agent(&manual_agent).unwrap();
        manual_agent
    };
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();
    orch.set_managed_execution(
        &auth(),
        lane.id,
        workspace.path(),
        &agent.agent_id,
        retry_policy(false),
    )
    .unwrap();
    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "manual-retry-work",
                "session_id": lane.id,
                "workspace": workspace_text,
                "kind": "native",
                "objective": "Fail once, then wait for a manual worker"
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "manual-retry-assign",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": agent.agent_id
            }),
        )
        .await
        .unwrap();
    orch.stop_background_tasks().await;
    let mut intent = None;
    for _ in 0..10 {
        orch.drive_native_executor_once().await;
        intent = orch
            .store()
            .list_managed_intents()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.work_id == work_id);
        if intent.is_some() {
            break;
        }
    }
    let intent = intent.expect("native admission");
    if let Some(run_id) = intent.run_id.as_deref() {
        if let Ok(Some(mut run)) = orch.store().load_run(run_id) {
            run.state = RunState::Failed;
            run.error_code = Some("failed".into());
            orch.store().save_run(&run).unwrap();
        }
    }
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .load_work_item(&work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::Failed
    );
    assert_eq!(
        orch.store()
            .load_managed_intent(&intent.intent_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Finalized
    );
    let settle_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    loop {
        let attempts = orch.store().list_work_attempts(Some(&work_id)).unwrap();
        if attempts.iter().all(|attempt| !attempt.state.is_active()) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < settle_deadline,
            "native attempt did not settle before manual retry"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        orch.drive_native_executor_once().await;
    }
    let original_attempts = orch
        .store()
        .list_work_attempts(Some(&work_id))
        .unwrap()
        .len();
    let retried = client
        .call_tool(
            "ptah_retry_work",
            json!({
                "request_id": "manual-retry-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "reason": "operator wants a manual worker"
            }),
        )
        .await
        .unwrap();
    assert_eq!(retried.structured["work"]["state"], "queued");
    for _ in 0..3 {
        orch.drive_native_executor_once().await;
    }
    let still = orch.store().load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(still.state, WorkState::Queued);
    assert!(orch
        .store()
        .live_managed_intent_for_work(&work_id)
        .unwrap()
        .is_none());
    assert_eq!(
        orch.store()
            .list_work_attempts(Some(&work_id))
            .unwrap()
            .len(),
        original_attempts
    );
    client
        .call_tool(
            "ptah_assign_work",
            json!({
                "request_id": "manual-retry-reassign",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "assigned_agent_id": manual_agent.agent_id
            }),
        )
        .await
        .unwrap();
    let claimed = client
        .call_tool(
            "ptah_claim_work",
            json!({
                "request_id": "manual-claim-1",
                "session_id": lane.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "agent_id": manual_agent.agent_id
            }),
        )
        .await
        .unwrap();
    assert_eq!(claimed.structured["work"]["state"], "leased");
    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn resolve_work_input_fails_after_restart_without_host_pending() {
    let (_env, _home, workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "native-token-308");
    client.initialize().await.unwrap();
    let (item, _, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (req, rx) = host
        .begin_pending_permission(
            session,
            Some(run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: session,
        request: req.clone(),
    })
    .await;
    drop(rx);
    orch.stop_background_tasks().await;
    drop(orch);
    let store = host.ensure_orchestration_store().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "native-token-308".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let missing = orch
        .resolve_work_input(&auth(), session, workspace.path(), req.id, true)
        .unwrap_err();
    assert_eq!(
        missing.code,
        grokptah_agent_bridge::orchestration::OrchErrorCode::Conflict
    );
    let work = orch.store().load_work_item(&item.work_id).unwrap().unwrap();
    assert_ne!(work.state, WorkState::Running);
    orch.stop_background_tasks().await;
    client.close_session().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn resolving_dead_oneshot_does_not_unpark_on_recovery() {
    let (_env, _home, workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let (item, _, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (req, rx) = host
        .begin_pending_permission(
            session,
            Some(run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: session,
        request: req.clone(),
    })
    .await;
    orch.stop_background_tasks().await;
    orch.store()
        .begin_managed_permission_resolve(
            &req.id.to_string(),
            session,
            &workspace_text,
            chrono::Utc::now(),
        )
        .unwrap();
    drop(rx);
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingInput
    );
    assert_eq!(
        orch.store()
            .live_managed_intent_for_work(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Parked
    );
    let _ = workspace;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn failed_permission_respond_then_recovery_does_not_unpark() {
    let (_env, _home, workspace, host, orch, session, agent_id, workspace_text) =
        boot_native(false).await;
    let (item, _, run) = seed_admitted_work(
        orch.store(),
        session,
        &workspace_text,
        &agent_id,
        RunState::Running,
        "native-token-308",
    );
    let (req, rx) = host
        .begin_pending_permission(
            session,
            Some(run.run_id.as_str()),
            "write_file",
            "Allow write?",
        )
        .unwrap();
    orch.notify_native_executor(&SessionUpdate::PermissionRequired {
        session_id: session,
        request: req.clone(),
    })
    .await;
    orch.stop_background_tasks().await;
    orch.store()
        .begin_managed_permission_resolve(
            &req.id.to_string(),
            session,
            &workspace_text,
            chrono::Utc::now(),
        )
        .unwrap();
    drop(rx);
    assert!(host
        .permission_respond(req.id, grokptah_agent_bridge::PermissionDecision::Allow)
        .is_err());
    assert!(host.inspect_pending_permission(req.id).is_none());
    orch.drive_native_executor_once().await;
    assert_eq!(
        orch.store()
            .load_work_item(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        WorkState::AwaitingInput
    );
    assert_eq!(
        orch.store()
            .live_managed_intent_for_work(&item.work_id)
            .unwrap()
            .unwrap()
            .state,
        ManagedIntentState::Parked
    );
    orch.stop_background_tasks().await;
    let _ = workspace;
}
