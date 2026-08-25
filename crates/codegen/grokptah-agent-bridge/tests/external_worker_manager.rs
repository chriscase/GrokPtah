//! Focused fake-adapter coverage for the external-worker production slice.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use grokptah_agent_bridge::orchestration::{
    OrchErrorCode, OrchestrationConfig, OrchestrationService, RunBounds, RunState,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, ExternalWorkerAdapter,
    ExternalWorkerAdapterError, ExternalWorkerRegistry, HostConfig, McpControlClient, SessionKind,
    CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use grokptah_agent_sdk::{
    ExternalWorkerArtifact, ExternalWorkerEvent, ExternalWorkerExecutionMode,
    ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult,
    ExternalWorkerProvider, ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerState,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

const TS: &str = "2026-08-25T00:00:00Z";

struct FakeState {
    workers: HashMap<String, ExternalWorkerRecord>,
    runs: HashMap<(String, String), ExternalWorkerRunRecord>,
    artifacts: HashMap<(String, String), Vec<ExternalWorkerArtifact>>,
    stream_events: Vec<ExternalWorkerEvent>,
    stream_expired: bool,
    launch_count: u32,
    next_id: u32,
}

struct FakeAdapter {
    inner: Mutex<FakeState>,
}

impl FakeAdapter {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(FakeState {
                workers: HashMap::new(),
                runs: HashMap::new(),
                artifacts: HashMap::new(),
                stream_events: Vec::new(),
                stream_expired: false,
                launch_count: 0,
                next_id: 1,
            }),
        })
    }

    fn expire_stream(&self) {
        self.inner.lock().unwrap().stream_expired = true;
    }

    fn complete_with_terminal(&self, agent_id: &str, run_id: &str, terminal: Option<&str>) {
        let mut state = self.inner.lock().unwrap();
        if let Some(run) = state.runs.get_mut(&(agent_id.into(), run_id.into())) {
            run.state = ExternalWorkerState::Completed;
            run.terminal_result = terminal.map(ToOwned::to_owned);
            run.updated_at = TS.into();
        }
        if let Some(worker) = state.workers.get_mut(agent_id) {
            worker.state = ExternalWorkerState::Ready;
            worker.updated_at = TS.into();
        }
        state.artifacts.insert(
            (agent_id.into(), run_id.into()),
            vec![ExternalWorkerArtifact {
                path: "artifacts/report.md".into(),
                digest: "sha256:abc".into(),
                size_bytes: Some(12),
            }],
        );
    }

    fn plant_undigested_artifact(&self, agent_id: &str, run_id: &str) {
        self.inner.lock().unwrap().artifacts.insert(
            (agent_id.into(), run_id.into()),
            vec![ExternalWorkerArtifact {
                path: "artifacts/report.md".into(),
                digest: String::new(),
                size_bytes: Some(12),
            }],
        );
    }

    fn launch_count(&self) -> u32 {
        self.inner.lock().unwrap().launch_count
    }

    fn plant_stream_events(&self, start: u64, count: u64) {
        let mut state = self.inner.lock().unwrap();
        state.stream_events = (0..count)
            .map(|offset| ExternalWorkerEvent {
                seq: start + offset,
                ts: TS.into(),
                kind: "log".into(),
                detail: "status updated".into(),
            })
            .collect();
    }
}

fn worker_record(id: &str, request: &ExternalWorkerLaunchRequest) -> ExternalWorkerRecord {
    ExternalWorkerRecord {
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        external_agent_id: id.into(),
        repository: request.repository.clone(),
        starting_ref: request.starting_ref.clone(),
        state: ExternalWorkerState::Running,
        branch: Some("cursor/external-review".into()),
        worker_url: Some(format!("https://cursor.com/agents/{id}")),
        created_at: TS.into(),
        updated_at: TS.into(),
    }
}

fn run_record(agent_id: &str, run_id: &str, state: ExternalWorkerState) -> ExternalWorkerRunRecord {
    ExternalWorkerRunRecord {
        external_agent_id: agent_id.into(),
        external_run_id: run_id.into(),
        state,
        last_seq: 0,
        terminal_result: None,
        created_at: TS.into(),
        updated_at: TS.into(),
    }
}

#[async_trait]
impl ExternalWorkerAdapter for FakeAdapter {
    fn provider(&self) -> ExternalWorkerProvider {
        ExternalWorkerProvider::CursorCloud
    }

    async fn launch(
        &self,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<ExternalWorkerLaunchResult, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        let mut state = self.inner.lock().unwrap();
        state.launch_count += 1;
        let n = state.next_id;
        state.next_id += 1;
        let agent_id = format!("agent-{n}");
        let run_id = format!("run-{n}");
        let worker = worker_record(&agent_id, request);
        let run = run_record(&agent_id, &run_id, ExternalWorkerState::Running);
        state.workers.insert(agent_id.clone(), worker.clone());
        state.runs.insert((agent_id.clone(), run_id), run.clone());
        Ok(ExternalWorkerLaunchResult { worker, run })
    }

    async fn get_worker(
        &self,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        self.inner
            .lock()
            .unwrap()
            .workers
            .get(external_agent_id)
            .cloned()
            .ok_or(ExternalWorkerAdapterError::InvalidRequest("unknown worker"))
    }

    async fn get_run(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        self.inner
            .lock()
            .unwrap()
            .runs
            .get(&(external_agent_id.into(), external_run_id.into()))
            .cloned()
            .ok_or(ExternalWorkerAdapterError::InvalidRequest("unknown run"))
    }

    async fn follow_up(
        &self,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        request
            .validate()
            .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        let mut state = self.inner.lock().unwrap();
        if !state.workers.contains_key(external_agent_id) {
            return Err(ExternalWorkerAdapterError::InvalidRequest("unknown worker"));
        }
        if state.runs.values().any(|run| {
            run.external_agent_id == external_agent_id
                && matches!(
                    run.state,
                    ExternalWorkerState::Provisioning | ExternalWorkerState::Running
                )
        }) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor worker already has an active run",
            ));
        }
        let n = state.next_id;
        state.next_id += 1;
        let run_id = format!("run-{n}");
        let run = run_record(external_agent_id, &run_id, ExternalWorkerState::Running);
        state
            .runs
            .insert((external_agent_id.into(), run_id), run.clone());
        Ok(run)
    }

    async fn list_artifacts(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<Vec<ExternalWorkerArtifact>, ExternalWorkerAdapterError> {
        let artifacts = self
            .inner
            .lock()
            .unwrap()
            .artifacts
            .get(&(external_agent_id.into(), external_run_id.into()))
            .cloned()
            .unwrap_or_default();
        if artifacts.iter().any(|artifact| artifact.digest.is_empty()) {
            return Err(ExternalWorkerAdapterError::InvalidResponse(
                "Cursor artifact listing did not provide a content digest",
            ));
        }
        Ok(artifacts)
    }

    async fn cancel(
        &self,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<ExternalWorkerRunRecord, ExternalWorkerAdapterError> {
        let mut state = self.inner.lock().unwrap();
        let run = state
            .runs
            .get_mut(&(external_agent_id.into(), external_run_id.into()))
            .ok_or(ExternalWorkerAdapterError::InvalidRequest("unknown run"))?;
        if run.state == ExternalWorkerState::Cancelled {
            return Ok(run.clone());
        }
        if !matches!(
            run.state,
            ExternalWorkerState::Provisioning | ExternalWorkerState::Running
        ) {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "Cursor run is not cancellable",
            ));
        }
        run.state = ExternalWorkerState::Cancelled;
        run.updated_at = TS.into();
        Ok(run.clone())
    }

    async fn try_stream_events(
        &self,
        _external_agent_id: &str,
        _external_run_id: &str,
        after_seq: u64,
    ) -> Result<Option<Vec<ExternalWorkerEvent>>, ExternalWorkerAdapterError> {
        let state = self.inner.lock().unwrap();
        if state.stream_expired {
            return Ok(None);
        }
        Ok(Some(
            state
                .stream_events
                .iter()
                .filter(|event| event.seq > after_seq)
                .cloned()
                .collect(),
        ))
    }
}

fn launch_request(request_id: &str) -> ExternalWorkerLaunchRequest {
    ExternalWorkerLaunchRequest {
        request_id: request_id.into(),
        provider: ExternalWorkerProvider::CursorCloud,
        provider_id: None,
        repository: "chriscase/GrokPtah".into(),
        starting_ref: "refs/heads/codex/review".into(),
        prompt: "Review the exact candidate".into(),
        model: Some("composer".into()),
        execution_mode: ExternalWorkerExecutionMode::Isolated,
        auto_create_pr: false,
        bounds: None,
    }
}

fn setup() -> (
    tempfile::TempDir,
    ProcessEnvGuard,
    grokptah_agent_bridge::AgentHostHandle,
    tempfile::TempDir,
    std::sync::Arc<OrchestrationService>,
    Arc<FakeAdapter>,
) {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".grokptah")).unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let ws = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let store = host.ensure_orchestration_store().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let fake = FakeAdapter::new();
    let registry = Arc::new(ExternalWorkerRegistry::new());
    registry.register(fake.clone());
    orch.install_external_worker_registry(registry);
    (home, env, host, ws, orch, fake)
}

fn build_session(
    host: &grokptah_agent_bridge::AgentHostHandle,
    ws: &tempfile::TempDir,
) -> uuid::Uuid {
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    session.id
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn launch_status_follow_up_and_local_run_boundaries() {
    let (_home, _env, host, ws, orch, fake) = setup();
    let session = build_session(&host, &ws);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let launched = orch
        .launch_external_worker(&auth, launch_request("req-launch"), session, ws.path())
        .await
        .unwrap();
    assert_eq!(launched["worker"]["startingRef"], "refs/heads/codex/review");
    assert_eq!(launched["worker"]["repository"], "chriscase/GrokPtah");
    assert_eq!(launched["run"]["state"], "running");
    let agent_id = launched["worker"]["externalAgentId"]
        .as_str()
        .unwrap()
        .to_string();
    let run_id = launched["run"]["externalRunId"]
        .as_str()
        .unwrap()
        .to_string();
    let local_run_id = launched["localRunId"].as_str().unwrap().to_string();
    let local = orch
        .get_run_scoped(&auth, session, ws.path(), &local_run_id)
        .unwrap();
    assert_eq!(local["state"], "running");
    assert_eq!(local["external"]["externalAgentId"], agent_id);
    assert_eq!(local["clientId"], "external_worker");

    let busy = orch
        .launch_external_worker(&auth, launch_request("req-busy"), session, ws.path())
        .await
        .unwrap_err();
    assert_eq!(busy.code, OrchErrorCode::SessionBusy);

    let stale = orch
        .follow_up_external_worker(
            &auth,
            ExternalWorkerFollowUpRequest {
                request_id: "req-stale".into(),
                prompt: "Continue the review".into(),
                bounds: None,
            },
            session,
            ws.path(),
            &agent_id,
            0,
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, OrchErrorCode::StaleVersion);

    orch.cancel_external_worker(
        &auth,
        "req-cancel",
        session,
        ws.path(),
        &agent_id,
        &run_id,
        launched["version"].as_u64().unwrap(),
    )
    .await
    .unwrap();
    let cancelled = orch
        .get_external_worker_run_scoped(&auth, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap();
    assert_eq!(cancelled["run"]["state"], "cancelled");
    let cancelled_again = orch
        .cancel_external_worker(
            &auth,
            "req-cancel-again",
            session,
            ws.path(),
            &agent_id,
            &run_id,
            cancelled["version"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancelled_again["run"]["state"], "cancelled");

    let worker = orch
        .get_external_worker_scoped(&auth, session, ws.path(), &agent_id)
        .await
        .unwrap();
    let follow = orch
        .follow_up_external_worker(
            &auth,
            ExternalWorkerFollowUpRequest {
                request_id: "req-follow".into(),
                prompt: "Continue after explicit cancel".into(),
                bounds: None,
            },
            session,
            ws.path(),
            &agent_id,
            worker["version"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(follow["run"]["externalRunId"], run_id);
    assert_eq!(follow["run"]["state"], "running");
    assert_eq!(fake.launch_count(), 1);

    let approve = orch
        .approve_run(
            &auth,
            "req-approve",
            session,
            ws.path(),
            &local_run_id,
            "source".into(),
            "final".into(),
            Vec::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(approve.code, OrchErrorCode::ForbiddenScope);
    assert!(CONTROL_TOOLS.contains(&"ptah_launch_external_worker"));
    assert!(!CONTROL_TOOLS.contains(&"ptah_computer_act"));
    for forbidden in FORBIDDEN_TOOLS {
        assert!(!CONTROL_TOOLS.contains(forbidden));
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn duplicate_replay_restart_stream_fallback_redaction_and_artifacts() {
    let (home, _env, host, ws, orch, fake) = setup();
    let session = build_session(&host, &ws);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let request = launch_request("req-dup");
    let first = orch
        .launch_external_worker(&auth, request.clone(), session, ws.path())
        .await
        .unwrap();
    let second = orch
        .launch_external_worker(&auth, request, session, ws.path())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(fake.launch_count(), 1);
    let agent_id = first["worker"]["externalAgentId"]
        .as_str()
        .unwrap()
        .to_string();
    let run_id = first["run"]["externalRunId"].as_str().unwrap().to_string();
    let local_run_id = first["localRunId"].as_str().unwrap().to_string();

    fake.plant_stream_events(2, 300);
    let expired = orch
        .get_external_worker_events_scoped(&auth, session, ws.path(), &agent_id, &run_id, 1, 50)
        .await
        .unwrap_err();
    assert_eq!(expired.code, OrchErrorCode::CursorExpired);
    let poll_route = expired.data.as_ref().unwrap()["pollRoute"]
        .as_str()
        .unwrap();
    assert!(poll_route.starts_with("/external-workers/"));
    assert!(!poll_route.contains("http"));
    assert!(
        expired.data.as_ref().unwrap()["eventRange"]["startSeq"]
            .as_u64()
            .unwrap()
            > 1
    );

    fake.expire_stream();
    let status = orch
        .get_external_worker_run_scoped(&auth, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap();
    assert_eq!(status["run"]["state"], "running");
    assert_eq!(status["streamExpired"], true);

    fake.complete_with_terminal(&agent_id, &run_id, Some("api_key=super-secret"));
    let completed = orch
        .get_external_worker_run_scoped(&auth, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap();
    assert_eq!(completed["run"]["state"], "completed");
    assert!(completed["run"].get("terminalResult").is_none());
    let artifacts = orch
        .list_external_worker_artifacts_scoped(&auth, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap();
    assert_eq!(artifacts["artifacts"][0]["digest"], "sha256:abc");
    assert_eq!(artifacts["artifacts"][0]["path"], "artifacts/report.md");

    fake.plant_undigested_artifact(&agent_id, &run_id);
    let missing_digest = orch
        .list_external_worker_artifacts_scoped(&auth, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap_err();
    assert_eq!(missing_digest.code, OrchErrorCode::Internal);

    let events = orch
        .get_external_worker_events_scoped(&auth, session, ws.path(), &agent_id, &run_id, 0, 50)
        .await
        .unwrap();
    assert!(!events["events"].as_array().unwrap().is_empty());
    assert!(!events["pollRoute"].as_str().unwrap().contains("http"));

    drop(orch);
    drop(host);
    let host2 = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host2.start().unwrap();
    host2.set_project_cwd(ws.path()).unwrap();
    assert!(host2.session_load(session).is_ok());
    let store2 = host2.ensure_orchestration_store().unwrap();
    let loaded = store2.load_run(&local_run_id).unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Completed);
    assert!(loaded.external.is_some());
    let orch2 = OrchestrationService::new(
        host2.clone(),
        host2.event_bus(),
        store2,
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let registry = Arc::new(ExternalWorkerRegistry::new());
    registry.register(fake.clone());
    orch2.install_external_worker_registry(registry);
    let auth2 = orch2.auth_header(Some("Bearer t")).unwrap();
    let replay = orch2
        .launch_external_worker(&auth2, launch_request("req-dup"), session, ws.path())
        .await
        .unwrap();
    assert_eq!(replay["localRunId"], local_run_id);
    let reconnect = orch2
        .get_external_worker_run_scoped(&auth2, session, ws.path(), &agent_id, &run_id)
        .await
        .unwrap();
    assert_eq!(reconnect["run"]["state"], "completed");
    assert_eq!(fake.launch_count(), 1);

    let audit = std::fs::read_to_string(
        home.path()
            .join(".grokptah/orchestration/audit/audit.jsonl"),
    )
    .unwrap_or_default();
    assert!(!audit.contains("super-secret"));
    assert!(!audit.contains("api_key="));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unavailable_provider_and_mcp_projection() {
    let (_home, _env, host, ws, orch, _fake) = setup();
    orch.install_external_worker_registry(Arc::new(ExternalWorkerRegistry::new()));
    let session = build_session(&host, &ws);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let missing = orch
        .launch_external_worker(&auth, launch_request("req-missing"), session, ws.path())
        .await
        .unwrap_err();
    assert_eq!(missing.code, OrchErrorCode::Unsupported);

    let fake = FakeAdapter::new();
    let registry = Arc::new(ExternalWorkerRegistry::new());
    registry.register(fake.clone());
    orch.install_external_worker_registry(registry);
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", srv.addr), "t");
    client.initialize().await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert!(tools
        .iter()
        .any(|tool| tool.name == "ptah_launch_external_worker"));
    assert!(tools
        .iter()
        .any(|tool| tool.name == "ptah_get_external_worker_events"));
    let launched = client
        .call_tool(
            "ptah_launch_external_worker",
            json!({
                "request_id": "mcp-launch",
                "session_id": session.to_string(),
                "workspace": ws.path().display().to_string(),
                "provider": "cursor_cloud",
                "repository": "chriscase/GrokPtah",
                "starting_ref": "refs/heads/codex/review",
                "prompt": "Review the exact candidate",
                "execution_mode": "isolated",
                "auto_create_pr": false
            }),
        )
        .await
        .unwrap();
    assert!(!launched.is_error);
    assert_eq!(
        launched.structured["worker"]["startingRef"],
        "refs/heads/codex/review"
    );
    assert!(launched.structured.get("apiKey").is_none());
    let pr_err = client
        .call_tool(
            "ptah_launch_external_worker",
            json!({
                "request_id": "mcp-pr",
                "session_id": session.to_string(),
                "workspace": ws.path().display().to_string(),
                "provider": "cursor_cloud",
                "repository": "chriscase/GrokPtah",
                "starting_ref": "refs/heads/codex/review",
                "prompt": "Review the exact candidate",
                "auto_create_pr": true
            }),
        )
        .await
        .expect_err("pull-request creation must stay a separate approval action");
    let pr_msg = pr_err.to_string();
    assert!(pr_msg.contains("403") || pr_msg.contains("forbidden_scope"));
}

#[test]
fn external_running_records_survive_host_restart() {
    let d = tempdir().unwrap();
    let store = grokptah_agent_bridge::OrchStore::open(d.path()).unwrap();
    let session_id = uuid::Uuid::new_v4();
    let run = grokptah_agent_bridge::RunRecord {
        run_id: "ext-run".into(),
        session_id,
        workspace: "/tmp/w".into(),
        request_id: "req-ext".into(),
        client_id: Some("external_worker".into()),
        state: RunState::Running,
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "review".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
        external: Some(grokptah_agent_bridge::ExternalRunAttachment {
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            external_agent_id: "agent-1".into(),
            external_run_id: "run-1".into(),
            request_id: "req-ext".into(),
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "refs/heads/codex/review".into(),
        }),
    };
    store.save_run(&run).unwrap();
    drop(store);
    let reopened = grokptah_agent_bridge::OrchStore::open(d.path()).unwrap();
    let loaded = reopened.load_run("ext-run").unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Running);
    assert_eq!(
        loaded.external.as_ref().unwrap().starting_ref,
        "refs/heads/codex/review"
    );
}
