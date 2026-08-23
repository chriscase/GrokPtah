use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::orchestration::WorkPolicy;
use grokptah_agent_bridge::{
    ActivationRecord, AgentRecord, AgentResumePlan, JournalPage, McpControlClient, McpRemoteError,
    PublicRun, PublicRunPage, RoutineRecord, RoutineSnapshot, RunExecutionMode, RunScope, RunState,
    RuntimeConnectionState, RuntimeTarget, SessionUpdate, WorkAttemptView, WorkItem,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServiceStatus {
    pub connected: bool,
    pub base_url: Option<String>,
    pub runtime_target: Option<RuntimeTarget>,
    pub connection_state: RuntimeConnectionState,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionTarget {
    pub session_id: Uuid,
    pub title: String,
    pub workspace: String,
    pub updated_at: String,
    pub busy: bool,
    #[serde(default)]
    pub runtime_target: RuntimeTarget,
    #[serde(default)]
    pub runtime_connection: RuntimeConnectionState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskSubmission {
    pub run_id: String,
    pub session_id: Uuid,
    pub state: RunState,
    pub request_id: String,
    pub execution_mode: RunExecutionMode,
    pub queued_position: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkSnapshot {
    pub work: WorkItem,
    pub attempts: Vec<WorkAttemptView>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteSessionWire {
    session_id: Uuid,
    title: String,
    cwd: String,
    updated_at: String,
    busy: bool,
}

impl From<RemoteSessionWire> for RemoteSessionTarget {
    fn from(session: RemoteSessionWire) -> Self {
        Self {
            session_id: session.session_id,
            title: session.title,
            workspace: session.cwd,
            updated_at: session.updated_at,
            busy: session.busy,
            runtime_target: RuntimeTarget::LocalService,
            runtime_connection: RuntimeConnectionState::Connected,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunEvent {
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub seq: u64,
    pub ts: String,
    pub update: SessionUpdate,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunRecovery {
    pub run_id: String,
    pub session_id: Uuid,
    pub workspace: String,
    pub after_seq: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunScope {
    pub session_id: Uuid,
    pub workspace: String,
    pub run_id: String,
}

impl From<RemoteRunScope> for RunScope {
    fn from(scope: RemoteRunScope) -> Self {
        Self {
            session_id: scope.session_id,
            workspace: scope.workspace,
            run_id: scope.run_id,
        }
    }
}

pub struct RemoteServiceState {
    client: Mutex<Option<RemoteServiceClient>>,
    watchers: Mutex<std::collections::HashMap<String, watch::Sender<bool>>>,
    status: Mutex<RemoteServiceStatus>,
}

impl RemoteServiceState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            client: Mutex::new(None),
            watchers: Mutex::new(std::collections::HashMap::new()),
            status: Mutex::new(RemoteServiceStatus {
                connected: false,
                base_url: None,
                runtime_target: None,
                connection_state: RuntimeConnectionState::Disconnected,
                last_error: None,
            }),
        })
    }

    pub async fn status(&self) -> RemoteServiceStatus {
        self.status.lock().await.clone()
    }

    pub async fn connect(&self, base_url: String, token: String) -> Result<RemoteServiceStatus> {
        let base_url_hint = normalize_base_url(&base_url).ok();
        {
            let mut status = self.status.lock().await;
            status.connected = false;
            status.base_url = base_url_hint.clone();
            status.runtime_target = base_url_hint.as_deref().map(runtime_target_for_base_url);
            status.connection_state = RuntimeConnectionState::Reconnecting;
            status.last_error = None;
        }
        let client = match RemoteServiceClient::connect(base_url, token).await {
            Ok(client) => client,
            Err(error) => {
                self.client.lock().await.take();
                let mut status = self.status.lock().await;
                status.connected = false;
                status.connection_state = RuntimeConnectionState::Error;
                status.last_error = Some(error.to_string());
                return Err(error);
            }
        };
        let base_url = client.base_url.clone();
        let runtime_target = runtime_target_for_base_url(&base_url);
        let mut current = self.client.lock().await;
        *current = Some(client);
        let mut status = self.status.lock().await;
        *status = RemoteServiceStatus {
            connected: true,
            base_url: Some(base_url),
            runtime_target: Some(runtime_target),
            connection_state: RuntimeConnectionState::Connected,
            last_error: None,
        };
        Ok(status.clone())
    }

    pub async fn disconnect(&self) {
        let watchers = std::mem::take(&mut *self.watchers.lock().await);
        for cancel in watchers.into_values() {
            let _ = cancel.send(true);
        }
        self.client.lock().await.take();
        let mut status = self.status.lock().await;
        status.connected = false;
        status.connection_state = RuntimeConnectionState::Disconnected;
        status.last_error = None;
    }

    pub async fn list_persistent_agents(&self) -> Result<Option<Vec<AgentRecord>>> {
        let mut client = self.client.lock().await;
        let result = {
            let Some(client) = client.as_mut() else {
                return Ok(None);
            };
            client.list_persistent_agents().await
        };
        drop(client);
        self.record_remote_result(&result).await;
        Ok(Some(result?))
    }

    pub async fn list_sessions(&self) -> Result<Option<Vec<RemoteSessionTarget>>> {
        let mut client = self.client.lock().await;
        let result = {
            let Some(client) = client.as_mut() else {
                return Ok(None);
            };
            client.list_sessions().await
        };
        drop(client);
        self.record_remote_result(&result).await;
        Ok(Some(result?))
    }

    pub async fn create_session(
        &self,
        workspace: String,
        title: Option<String>,
    ) -> Result<Option<RemoteSessionTarget>> {
        let mut client = self.client.lock().await;
        let result = {
            let Some(client) = client.as_mut() else {
                return Ok(None);
            };
            client.create_session(workspace, title).await
        };
        drop(client);
        self.record_remote_result(&result).await;
        Ok(Some(result?))
    }

    pub async fn submit_task(
        &self,
        session_id: Uuid,
        workspace: String,
        prompt: String,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
    ) -> Result<Option<RemoteTaskSubmission>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .submit_task(
                    session_id,
                    workspace,
                    prompt,
                    execution_mode,
                    allow_queue,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn get_persistent_agent(&self, agent_id: &str) -> Result<Option<AgentRecord>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.get_persistent_agent(agent_id).await?))
    }

    pub async fn resume_plan(&self, session_id: Uuid) -> Result<Option<AgentResumePlan>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.resume_plan(session_id).await?))
    }

    pub async fn resume(
        &self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        request_id: String,
    ) -> Result<Option<String>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .resume(session_id, prompt, max_rounds, request_id)
                .await?,
        ))
    }

    pub async fn list_runs(&self) -> Result<Option<PublicRunPage>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_runs().await?))
    }

    pub async fn list_work(
        &self,
        session_id: Uuid,
        workspace: String,
    ) -> Result<Option<Vec<WorkItem>>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_work(session_id, workspace).await?))
    }

    pub async fn get_work(
        &self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
    ) -> Result<Option<RemoteWorkSnapshot>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.get_work(session_id, workspace, work_id).await?))
    }

    pub async fn create_work(
        &self,
        session_id: Uuid,
        workspace: String,
        kind: String,
        objective: String,
        priority: i32,
        requires_approval: bool,
    ) -> Result<Option<WorkItem>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .create_work(
                    session_id,
                    workspace,
                    kind,
                    objective,
                    priority,
                    requires_approval,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn assign_work(
        &self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        assigned_agent_id: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<Option<WorkItem>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .assign_work(
                    session_id,
                    workspace,
                    work_id,
                    assigned_agent_id,
                    expected_revision,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn retry_work(
        &self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<Option<WorkItem>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .retry_work(
                    session_id,
                    workspace,
                    work_id,
                    reason,
                    expected_revision,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn approve_work(
        &self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        note: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<Option<WorkItem>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .approve_work(
                    session_id,
                    workspace,
                    work_id,
                    note,
                    expected_revision,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn list_routines(
        &self,
        session_id: Uuid,
        workspace: String,
    ) -> Result<Option<Vec<RoutineRecord>>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_routines(session_id, workspace).await?))
    }

    pub async fn get_routine(
        &self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
    ) -> Result<Option<RoutineSnapshot>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .get_routine(session_id, workspace, routine_id)
                .await?,
        ))
    }

    pub async fn create_routine(
        &self,
        session_id: Uuid,
        workspace: String,
        name: String,
        agent_id: String,
        objective: String,
    ) -> Result<Option<RoutineRecord>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .create_routine(
                    session_id,
                    workspace,
                    name,
                    agent_id,
                    objective,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn set_routine_lifecycle(
        &self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
        lifecycle: String,
        expected_revision: Option<u64>,
    ) -> Result<Option<RoutineRecord>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .set_routine_lifecycle(
                    session_id,
                    workspace,
                    routine_id,
                    lifecycle,
                    expected_revision,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn fire_routine(
        &self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
    ) -> Result<Option<ActivationRecord>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .fire_routine(
                    session_id,
                    workspace,
                    routine_id,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn cancel_work(
        &self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        reason: String,
        expected_revision: Option<u64>,
    ) -> Result<Option<WorkItem>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .cancel_work(
                    session_id,
                    workspace,
                    work_id,
                    reason,
                    expected_revision,
                    Uuid::new_v4().to_string(),
                )
                .await?,
        ))
    }

    pub async fn get_run(
        &self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
    ) -> Result<Option<PublicRun>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.get_run(session_id, workspace, run_id).await?))
    }

    pub async fn get_events(
        &self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
        after_seq: u64,
        limit: usize,
    ) -> Result<Option<JournalPage>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(
            client
                .get_events(session_id, workspace, run_id, after_seq, limit)
                .await?,
        ))
    }

    pub async fn steer(
        &self,
        session_id: Uuid,
        workspace: String,
        text: String,
        request_id: String,
    ) -> Result<Option<()>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        client
            .steer(session_id, workspace, text, request_id)
            .await?;
        Ok(Some(()))
    }

    pub async fn cancel(
        &self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
        request_id: String,
    ) -> Result<Option<()>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        client
            .cancel(session_id, workspace, run_id, request_id)
            .await?;
        Ok(Some(()))
    }

    pub async fn watch_runs(
        self: &Arc<Self>,
        scopes: Vec<RemoteRunScope>,
        app: AppHandle,
    ) -> Result<()> {
        if self.client.lock().await.is_none() {
            bail!("remote service is not connected");
        }
        let mut watchers = self.watchers.lock().await;
        for scope in scopes.into_iter().map(RunScope::from) {
            if watchers.contains_key(&scope.run_id) {
                continue;
            }
            let (cancel, cancel_rx) = watch::channel(false);
            watchers.insert(scope.run_id.clone(), cancel);
            let weak = Arc::downgrade(self);
            let app = app.clone();
            tokio::spawn(async move {
                run_watcher(weak, scope, app, cancel_rx).await;
            });
        }
        Ok(())
    }

    async fn record_remote_result<T>(&self, result: &Result<T>) {
        let mut status = self.status.lock().await;
        match result {
            Ok(_) => {
                if status.connection_state != RuntimeConnectionState::Disconnected {
                    status.connected = true;
                    status.connection_state = RuntimeConnectionState::Connected;
                    status.last_error = None;
                }
            }
            Err(error) if should_reconnect_remote_error(error) => {
                status.connected = false;
                status.connection_state = RuntimeConnectionState::Error;
                status.last_error = Some(error.to_string());
            }
            Err(_) => {}
        }
    }
}

struct RemoteServiceClient {
    base_url: String,
    token: String,
    mcp: McpControlClient,
}

impl RemoteServiceClient {
    async fn connect(base_url: String, token: String) -> Result<Self> {
        let base_url = normalize_base_url(&base_url)?;
        if token.trim().is_empty() {
            bail!("remote service token is required");
        }
        let token = token.trim().to_string();
        let mut client = Self {
            base_url: base_url.clone(),
            token: token.clone(),
            mcp: McpControlClient::new(base_url, token),
        };
        client.reconnect().await?;
        let tools = client
            .mcp
            .list_tools()
            .await
            .context("list remote service tools")?;
        for required in [
            "ptah_list_sessions",
            "ptah_create_session",
            "ptah_submit_task",
            "ptah_list_persistent_agents",
            "ptah_get_persistent_agent",
            "ptah_resume_persistent_agent",
            "ptah_list_runs",
            "ptah_get_run",
            "ptah_create_work",
            "ptah_assign_work",
            "ptah_list_work",
            "ptah_get_work",
            "ptah_retry_work",
            "ptah_approve_work",
            "ptah_cancel_work",
            "ptah_create_routine",
            "ptah_list_routines",
            "ptah_get_routine",
            "ptah_fire_routine",
            "ptah_pause_routine",
            "ptah_enable_routine",
            "ptah_disable_routine",
            "ptah_list_activations",
            "ptah_get_events",
            "ptah_steer",
            "ptah_cancel",
        ] {
            if !tools.iter().any(|tool| tool.name == required) {
                bail!("remote service does not advertise {required}");
            }
        }
        Ok(client)
    }

    async fn reconnect(&mut self) -> Result<()> {
        let mut mcp = McpControlClient::new(self.base_url.clone(), self.token.clone());
        mcp.initialize()
            .await
            .context("initialize remote MCP service")?;
        self.mcp = mcp;
        Ok(())
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let first = self.mcp.call_tool(name, arguments.clone()).await;
        let result = match first {
            Ok(result) => result,
            Err(first_error) => {
                if !should_reconnect_remote_error(&first_error) {
                    return Err(first_error);
                }
                self.reconnect()
                    .await
                    .context("reconnect remote MCP service")?;
                self.mcp.call_tool(name, arguments).await.with_context(|| {
                    format!(
                        "remote MCP request failed after reconnect (initial error: {first_error})"
                    )
                })?
            }
        };
        if result.is_error {
            bail!("remote service rejected {name}: {}", result.raw);
        }
        Ok(result.structured)
    }

    async fn list_persistent_agents(&mut self) -> Result<Vec<AgentRecord>> {
        let value = self
            .call_tool("ptah_list_persistent_agents", json!({}))
            .await?;
        serde_json::from_value(
            value
                .get("agents")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote agent list omitted agents"))?,
        )
        .context("decode remote persistent agents")
    }

    async fn list_sessions(&mut self) -> Result<Vec<RemoteSessionTarget>> {
        let value = self.call_tool("ptah_list_sessions", json!({})).await?;
        let sessions: Vec<RemoteSessionWire> = serde_json::from_value(
            value
                .get("sessions")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote session list omitted sessions"))?,
        )
        .context("decode remote session list")?;
        let runtime_target = runtime_target_for_base_url(&self.base_url);
        Ok(sessions
            .into_iter()
            .map(|session| {
                let mut target = RemoteSessionTarget::from(session);
                target.runtime_target = runtime_target;
                target.runtime_connection = RuntimeConnectionState::Connected;
                target
            })
            .collect())
    }

    async fn create_session(
        &mut self,
        workspace: String,
        title: Option<String>,
    ) -> Result<RemoteSessionTarget> {
        let mut args = json!({"workspace": workspace});
        if let Some(title) = title {
            args["title"] = json!(title);
        }
        let value = self.call_tool("ptah_create_session", args).await?;
        let mut session: RemoteSessionTarget =
            serde_json::from_value(value).context("decode remote session creation")?;
        session.runtime_target = runtime_target_for_base_url(&self.base_url);
        session.runtime_connection = RuntimeConnectionState::Connected;
        Ok(session)
    }

    async fn submit_task(
        &mut self,
        session_id: Uuid,
        workspace: String,
        prompt: String,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
        request_id: String,
    ) -> Result<RemoteTaskSubmission> {
        let value = self
            .call_tool(
                "ptah_submit_task",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "prompt": prompt,
                    "execution_mode": execution_mode,
                    "allow_queue": allow_queue,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote task submission")
    }

    async fn get_persistent_agent(&mut self, agent_id: &str) -> Result<AgentRecord> {
        let agents = self.list_persistent_agents().await?;
        let agent = agents
            .into_iter()
            .find(|agent| agent.agent_id == agent_id)
            .ok_or_else(|| anyhow::anyhow!("remote agent is outside the service scope"))?;
        let plan = self
            .call_tool(
                "ptah_get_persistent_agent",
                json!({
                    "session_id": agent.session_id,
                    "workspace": agent.workspace,
                    "agent_id": agent.agent_id,
                }),
            )
            .await?;
        let plan: AgentResumePlan = serde_json::from_value(plan)
            .context("decode remote persistent-agent checkpoint plan")?;
        Ok(plan.agent)
    }

    async fn resume_plan(&mut self, session_id: Uuid) -> Result<AgentResumePlan> {
        let agents = self.list_persistent_agents().await?;
        let agent = agents
            .into_iter()
            .find(|agent| agent.known_lane_ids().contains(&session_id))
            .ok_or_else(|| anyhow::anyhow!("remote session has no persistent agent"))?;
        let value = self
            .call_tool(
                "ptah_get_persistent_agent",
                json!({
                    "session_id": session_id,
                    "workspace": agent.workspace,
                    "agent_id": agent.agent_id,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote resume plan")
    }

    async fn resume(
        &mut self,
        session_id: Uuid,
        prompt: String,
        max_rounds: Option<u32>,
        request_id: String,
    ) -> Result<String> {
        let agents = self.list_persistent_agents().await?;
        let agent = agents
            .into_iter()
            .find(|agent| agent.known_lane_ids().contains(&session_id))
            .ok_or_else(|| anyhow::anyhow!("remote session has no persistent agent"))?;
        let value = self
            .call_tool(
                "ptah_resume_persistent_agent",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": agent.workspace,
                    "agent_id": agent.agent_id,
                    "prompt": prompt,
                    "max_rounds": max_rounds,
                }),
            )
            .await?;
        value
            .get("response")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("remote resume response omitted response"))
    }

    async fn list_runs(&mut self) -> Result<PublicRunPage> {
        let sessions = self.list_sessions().await?;
        let mut pages = Vec::with_capacity(sessions.len());
        for session in sessions {
            let value = self
                .call_tool(
                    "ptah_list_runs",
                    json!({
                        "session_id": session.session_id,
                        "workspace": session.workspace,
                    }),
                )
                .await?;
            pages.push(decode_remote_run_page(value)?);
        }
        Ok(merge_remote_run_pages(pages))
    }

    async fn list_work(&mut self, session_id: Uuid, workspace: String) -> Result<Vec<WorkItem>> {
        let value = self
            .call_tool(
                "ptah_list_work",
                json!({"session_id": session_id, "workspace": workspace}),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote work list omitted work"))?,
        )
        .context("decode remote durable work list")
    }

    async fn get_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
    ) -> Result<RemoteWorkSnapshot> {
        let value = self
            .call_tool(
                "ptah_get_work",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote durable work item")
    }

    async fn create_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        kind: String,
        objective: String,
        priority: i32,
        requires_approval: bool,
        request_id: String,
    ) -> Result<WorkItem> {
        let mut policy = WorkPolicy::default();
        policy.requires_approval = requires_approval;
        let value = self
            .call_tool(
                "ptah_create_work",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "kind": kind,
                    "objective": objective,
                    "priority": priority,
                    "policy": policy,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote create work omitted work"))?,
        )
        .context("decode remote work creation")
    }

    async fn assign_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        assigned_agent_id: Option<String>,
        expected_revision: Option<u64>,
        request_id: String,
    ) -> Result<WorkItem> {
        let value = self
            .call_tool(
                "ptah_assign_work",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "assigned_agent_id": assigned_agent_id,
                    "expected_revision": expected_revision,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote assignment omitted work"))?,
        )
        .context("decode remote work assignment")
    }

    async fn retry_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        reason: String,
        expected_revision: Option<u64>,
        request_id: String,
    ) -> Result<WorkItem> {
        let value = self
            .call_tool(
                "ptah_retry_work",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote retry omitted work"))?,
        )
        .context("decode remote work retry")
    }

    async fn approve_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        note: Option<String>,
        expected_revision: Option<u64>,
        request_id: String,
    ) -> Result<WorkItem> {
        let value = self
            .call_tool(
                "ptah_approve_work",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "note": note,
                    "expected_revision": expected_revision,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote approval omitted work"))?,
        )
        .context("decode remote work approval")
    }

    async fn cancel_work(
        &mut self,
        session_id: Uuid,
        workspace: String,
        work_id: String,
        reason: String,
        expected_revision: Option<u64>,
        request_id: String,
    ) -> Result<WorkItem> {
        let value = self
            .call_tool(
                "ptah_cancel_work",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "work_id": work_id,
                    "reason": reason,
                    "expected_revision": expected_revision,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("work")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote cancellation omitted work"))?,
        )
        .context("decode remote work cancellation")
    }

    async fn list_routines(
        &mut self,
        session_id: Uuid,
        workspace: String,
    ) -> Result<Vec<RoutineRecord>> {
        let value = self
            .call_tool(
                "ptah_list_routines",
                json!({"session_id": session_id, "workspace": workspace}),
            )
            .await?;
        serde_json::from_value(
            value
                .get("routines")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote routine list omitted routines"))?,
        )
        .context("decode remote routines")
    }

    async fn get_routine(
        &mut self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
    ) -> Result<RoutineSnapshot> {
        let value = self
            .call_tool(
                "ptah_get_routine",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "routine_id": routine_id,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote routine snapshot")
    }

    async fn create_routine(
        &mut self,
        session_id: Uuid,
        workspace: String,
        name: String,
        agent_id: String,
        objective: String,
        request_id: String,
    ) -> Result<RoutineRecord> {
        let template = grokptah_agent_bridge::WorkTemplate {
            kind: "routine".into(),
            objective,
            priority: 0,
            policy: WorkPolicy::default(),
        };
        let value = self
            .call_tool(
                "ptah_create_routine",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "name": name,
                    "agent_id": agent_id,
                    "trigger": { "kind": "manual" },
                    "work_template": template,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("routine")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote create routine omitted routine"))?,
        )
        .context("decode remote routine creation")
    }

    async fn set_routine_lifecycle(
        &mut self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
        lifecycle: String,
        expected_revision: Option<u64>,
        request_id: String,
    ) -> Result<RoutineRecord> {
        let tool = match lifecycle.as_str() {
            "enabled" => "ptah_enable_routine",
            "paused" => "ptah_pause_routine",
            "disabled" => "ptah_disable_routine",
            _ => bail!("lifecycle must be enabled, paused, or disabled"),
        };
        let value = self
            .call_tool(
                tool,
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "routine_id": routine_id,
                    "expected_revision": expected_revision,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("routine")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote routine lifecycle omitted routine"))?,
        )
        .context("decode remote routine lifecycle")
    }

    async fn fire_routine(
        &mut self,
        session_id: Uuid,
        workspace: String,
        routine_id: String,
        request_id: String,
    ) -> Result<ActivationRecord> {
        let value = self
            .call_tool(
                "ptah_fire_routine",
                json!({
                    "request_id": request_id,
                    "session_id": session_id,
                    "workspace": workspace,
                    "routine_id": routine_id,
                }),
            )
            .await?;
        serde_json::from_value(
            value
                .get("activation")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("remote fire omitted activation"))?,
        )
        .context("decode remote routine fire")
    }

    async fn get_run(
        &mut self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
    ) -> Result<PublicRun> {
        let value = self
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "run_id": run_id,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote durable run")
    }

    async fn get_events(
        &mut self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
        after_seq: u64,
        limit: usize,
    ) -> Result<JournalPage> {
        let value = self
            .call_tool(
                "ptah_get_events",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "run_id": run_id,
                    "after_seq": after_seq,
                    "limit": limit,
                }),
            )
            .await?;
        serde_json::from_value(value).context("decode remote durable event page")
    }

    async fn open_event_stream(
        &self,
        scope: RunScope,
        after_seq: Option<u64>,
    ) -> Result<grokptah_agent_bridge::McpEventStream> {
        self.mcp
            .open_event_stream(scope, after_seq)
            .await
            .context("open remote run event stream")
    }

    async fn steer(
        &mut self,
        session_id: Uuid,
        workspace: String,
        text: String,
        request_id: String,
    ) -> Result<()> {
        self.call_tool(
            "ptah_steer",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "workspace": workspace,
                "text": text,
            }),
        )
        .await?;
        Ok(())
    }

    async fn cancel(
        &mut self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
        request_id: String,
    ) -> Result<()> {
        self.call_tool(
            "ptah_cancel",
            json!({
                "request_id": request_id,
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id,
            }),
        )
        .await?;
        Ok(())
    }
}

async fn run_watcher(
    state: std::sync::Weak<RemoteServiceState>,
    scope: RunScope,
    app: AppHandle,
    mut cancel: watch::Receiver<bool>,
) {
    let mut cursor = 0_u64;
    loop {
        if *cancel.borrow() {
            break;
        }
        match open_remote_stream(&state, &scope, cursor).await {
            Ok(mut stream) => loop {
                let next = tokio::select! {
                    _ = cancel.changed() => break,
                    result = stream.next_notification() => result,
                };
                match next {
                    Ok(Some(frame)) => match frame.notification {
                        grokptah_agent_bridge::LiveNotification::Event(event) => {
                            cursor = cursor.max(event.seq);
                            let update = RemoteRunEvent {
                                run_id: event.run_id,
                                session_id: event.session_id,
                                workspace: event.workspace,
                                seq: event.seq,
                                ts: event.ts,
                                update: event.update,
                            };
                            let terminal =
                                matches!(update.update, SessionUpdate::TurnComplete { .. });
                            let _ = app.emit("remote://run-event", update);
                            if terminal {
                                return;
                            }
                        }
                        grokptah_agent_bridge::LiveNotification::Recovery(recovery) => {
                            cursor = cursor.max(recovery.after_seq);
                            match replay_remote_events(&state, &scope, &app, &mut cursor).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    let _ = app.emit(
                                        "remote://run-recovery",
                                        RemoteRunRecovery {
                                            run_id: recovery.run_id,
                                            session_id: recovery.session_id,
                                            workspace: recovery.workspace,
                                            after_seq: cursor,
                                            reason: format!(
                                                "{}; durable cursor is no longer retained",
                                                recovery.reason
                                            ),
                                        },
                                    );
                                    return;
                                }
                                Err(error) => {
                                    let _ = app.emit(
                                        "remote://run-recovery",
                                        RemoteRunRecovery {
                                            run_id: recovery.run_id,
                                            session_id: recovery.session_id,
                                            workspace: recovery.workspace,
                                            after_seq: cursor,
                                            reason: format!(
                                                "{}; durable replay failed: {error}",
                                                recovery.reason
                                            ),
                                        },
                                    );
                                }
                            }
                            break;
                        }
                        grokptah_agent_bridge::LiveNotification::Unknown { .. } => {}
                    },
                    Ok(None) | Err(_) => break,
                }
            },
            Err(error) => {
                if error.to_string().contains("cursor_expired") {
                    let _ = app.emit(
                        "remote://run-recovery",
                        RemoteRunRecovery {
                            run_id: scope.run_id.clone(),
                            session_id: scope.session_id,
                            workspace: scope.workspace.clone(),
                            after_seq: cursor,
                            reason: "remote event cursor is no longer retained".into(),
                        },
                    );
                    return;
                }
                tokio::select! {
                    _ = cancel.changed() => break,
                    _ = tokio::time::sleep(Duration::from_millis(500)) => continue,
                }
            }
        }

        if *cancel.borrow() {
            break;
        }
        tokio::select! {
            _ = cancel.changed() => break,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

async fn open_remote_stream(
    state: &std::sync::Weak<RemoteServiceState>,
    scope: &RunScope,
    cursor: u64,
) -> Result<grokptah_agent_bridge::McpEventStream> {
    let Some(state) = state.upgrade() else {
        bail!("remote service state is unavailable");
    };
    let mut client = state.client.lock().await;
    let Some(client) = client.as_mut() else {
        bail!("remote service is not connected");
    };
    let first = client
        .open_event_stream(scope.clone(), (cursor > 0).then_some(cursor))
        .await;
    match first {
        Ok(stream) => Ok(stream),
        Err(first_error) => {
            if !should_reconnect_remote_error(&first_error) {
                return Err(first_error);
            }
            client
                .reconnect()
                .await
                .context("reconnect remote MCP stream")?;
            client
                .open_event_stream(scope.clone(), (cursor > 0).then_some(cursor))
                .await
                .with_context(|| {
                    format!("open remote stream after reconnect (initial error: {first_error})")
                })
        }
    }
}

/// Replay the durable tail after a live channel reports a continuity gap.
/// `false` means the cursor expired; callers must surface that rather than
/// silently starting from a newer window.
async fn replay_remote_events(
    state: &std::sync::Weak<RemoteServiceState>,
    scope: &RunScope,
    app: &AppHandle,
    cursor: &mut u64,
) -> Result<bool> {
    loop {
        let Some(state) = state.upgrade() else {
            bail!("remote service state is unavailable");
        };
        let page = match state
            .get_events(
                scope.session_id,
                scope.workspace.clone(),
                scope.run_id.clone(),
                *cursor,
                500,
            )
            .await
        {
            Ok(Some(page)) => page,
            Ok(None) => bail!("remote service is not connected"),
            Err(error) if error.to_string().contains("cursor_expired") => return Ok(false),
            Err(error) => return Err(error),
        };
        if page.cursor_expired {
            return Ok(false);
        }
        for entry in page.entries {
            if entry.seq <= *cursor {
                continue;
            }
            *cursor = entry.seq;
            let _ = app.emit(
                "remote://run-event",
                RemoteRunEvent {
                    run_id: scope.run_id.clone(),
                    session_id: scope.session_id,
                    workspace: scope.workspace.clone(),
                    seq: entry.seq,
                    ts: entry.ts,
                    update: entry.update,
                },
            );
        }
        match page.next_cursor {
            Some(next) if next > *cursor => *cursor = next,
            _ => return Ok(true),
        }
    }
}

fn normalize_base_url(value: &str) -> Result<String> {
    let parsed = Url::parse(value.trim()).context("remote service URL is invalid")?;
    match parsed.scheme() {
        "https" => {}
        "http" if parsed.host_str().is_some_and(is_loopback_host) => {}
        "http" => bail!("remote service connections must use HTTPS unless loopback"),
        scheme => bail!("unsupported remote service URL scheme {scheme}"),
    }
    if parsed.username() != "" || parsed.password().is_some() {
        bail!("remote service URL must not embed credentials");
    }
    Ok(value.trim().trim_end_matches('/').to_string())
}

/// Reconnect only for transport failures, retryable HTTP statuses, or a
/// server-side MCP session that disappeared during a restart. Application
/// errors such as `invalid_request`, `admission_uncertain`, `conflict`, or a
/// terminal run must be returned directly; replaying those requests can
/// duplicate mutations.
fn should_reconnect_remote_error(error: &anyhow::Error) -> bool {
    if is_stale_mcp_session_error(error) {
        return true;
    }
    let message = error.to_string();
    if let Some(rest) = message.strip_prefix("MCP HTTP ") {
        let status = rest
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u16>().ok());
        return status.is_some_and(|code| code == 408 || code == 429 || code >= 500);
    }
    if message.starts_with("MCP error:")
        || message.starts_with("MCP remote error:")
        || message.starts_with("missing required argument ")
        || message.starts_with("unexpected argument ")
        || message.starts_with("unknown tool ")
        || message.starts_with("MCP client not initialized")
    {
        return false;
    }
    true
}

fn is_stale_mcp_session_error(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<McpRemoteError>()
        .and_then(McpRemoteError::data_code)
        == Some("unknown_session")
    {
        return true;
    }
    let message = error.to_string();
    message == "MCP remote error: unknown_session" || message.contains("unknown mcp-session-id")
}

fn decode_remote_run_page(value: Value) -> Result<PublicRunPage> {
    serde_json::from_value(value).context("decode remote durable run page")
}

/// Merge first pages from each remote session without following `nextCursor`.
///
/// Counts are summed. The combined first-page list is capped at
/// [`grokptah_agent_bridge::orchestration::MAX_PUBLIC_RUN_LIST`]. The aggregated
/// view has no unified cursor, so `next_cursor` is always omitted.
fn merge_remote_run_pages(pages: Vec<PublicRunPage>) -> PublicRunPage {
    use grokptah_agent_bridge::orchestration::MAX_PUBLIC_RUN_LIST;

    let mut total_count = 0usize;
    let mut source_truncated = false;
    let mut runs = Vec::new();
    for page in pages {
        total_count = total_count.saturating_add(page.total_count);
        source_truncated |= page.truncated;
        runs.extend(page.runs);
    }
    runs.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then(right.run_id.cmp(&left.run_id))
    });
    let overflow = runs.len() > MAX_PUBLIC_RUN_LIST;
    runs.truncate(MAX_PUBLIC_RUN_LIST);
    let truncated = source_truncated || overflow || total_count > runs.len();
    PublicRunPage {
        runs,
        total_count,
        truncated,
        next_cursor: None,
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn runtime_target_for_base_url(base_url: &str) -> RuntimeTarget {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(is_loopback_host))
        .map(|loopback| {
            if loopback {
                RuntimeTarget::LocalService
            } else {
                RuntimeTarget::HostedService
            }
        })
        .unwrap_or(RuntimeTarget::HostedService)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use grokptah_agent_bridge::{
        home_override_serial, set_grokptah_home_override, start_control_server,
        start_control_server_with_bind, AgentHost, ControlServerLimits, HostConfig, OrchStore,
        OrchestrationConfig, OrchestrationService, PublicRun, RunBounds, RunExecutionMode,
        RuntimeConnectionState, RuntimeTarget, WorkspaceAllowlist,
    };
    use tempfile::tempdir;

    use super::{
        decode_remote_run_page, merge_remote_run_pages, normalize_base_url,
        runtime_target_for_base_url, should_reconnect_remote_error, RemoteServiceClient,
        RemoteServiceState,
    };

    #[test]
    fn remote_run_decode_rejects_leaked_provider_route() {
        let leaked = serde_json::json!({
            "runId": "public-run-leak",
            "sessionId": "11111111-1111-1111-1111-111111111111",
            "workspace": "/tmp/public-run",
            "requestId": "public-run-req",
            "clientId": "mcp",
            "state": "running",
            "purpose": "execution",
            "bounds": {
                "maxPromptBytes": 1024,
                "maxRounds": 4,
                "maxDurationMs": 1000
            },
            "promptPreview": "inspect",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "aggregates": {
                "changes": [],
                "tests": []
            },
            "providerRoute": {
                "baseUrl": "http://leak-base-url-sentinel-pr352.example/v1",
                "credentialRef": "keychain:provider/leak-cred-ref-sentinel-pr352",
                "credentialFingerprint": "v1-sha256:leak-cred-fp-sentinel-pr352",
                "endpointFingerprint": "v1-sha256:leak-endpoint-fp-sentinel-pr352",
                "qualificationRecordId": "leak-qualification-record-sentinel-pr352",
                "selectionKey": "ptah.model.v1:leak-selection-key-sentinel-pr352",
                "quotaReservationId": "quota-leak-reservation-sentinel-pr352"
            },
            "providerExecution": {
                "route": {
                    "providerId": "company-gateway",
                    "kind": "open_ai_compatible",
                    "dialect": "open_ai_chat_completions",
                    "modelId": "leak-model",
                    "wireModelId": "leak-model",
                    "capabilitySource": "declared",
                    "deadlineClass": "standard",
                    "effort": "medium",
                    "snapshotHash": "keep-public-route-hash"
                },
                "quota": null,
                "attempts": [],
                "attemptCount": 0,
                "attemptsTruncated": false,
                "usageComplete": false,
                "pendingRequests": 0
            }
        });
        let decoded = serde_json::from_value::<PublicRun>(leaked);
        assert!(
            decoded.is_err(),
            "unknown nested providerRoute must fail remote decode: {decoded:?}"
        );
    }

    fn sample_public_run(run_id: &str, updated_at: &str) -> PublicRun {
        serde_json::from_value(serde_json::json!({
            "runId": run_id,
            "sessionId": "11111111-1111-1111-1111-111111111111",
            "workspace": "/tmp/public-run",
            "requestId": run_id,
            "clientId": "mcp",
            "state": "completed",
            "purpose": "execution",
            "bounds": {
                "maxPromptBytes": 1024,
                "maxRounds": 4,
                "maxDurationMs": 1000
            },
            "promptPreview": "inspect",
            "startSeq": null,
            "endSeq": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": updated_at,
            "terminalResult": null,
            "finalResponse": null,
            "errorCode": null,
            "aggregates": {
                "changes": [],
                "tests": []
            }
        }))
        .expect("sample PublicRun")
    }

    #[test]
    fn remote_run_page_decode_rejects_runs_only_payload() {
        let runs_only = serde_json::json!({
            "runs": [sample_public_run("keep-count", "2026-01-01T00:00:01Z")]
        });
        let decoded = decode_remote_run_page(runs_only);
        assert!(
            decoded.is_err(),
            "remote list must not drop totalCount/truncated: {decoded:?}"
        );
    }

    #[test]
    fn remote_run_page_decode_preserves_count_and_truncation() {
        let page = decode_remote_run_page(serde_json::json!({
            "runs": [sample_public_run("keep-count", "2026-01-01T00:00:01Z")],
            "totalCount": 200,
            "truncated": true,
            "nextCursor": "keep-count"
        }))
        .expect("decode public run page");
        assert_eq!(page.runs.len(), 1);
        assert_eq!(page.runs[0].run_id, "keep-count");
        assert_eq!(page.total_count, 200);
        assert!(page.truncated);
        assert_eq!(page.next_cursor.as_deref(), Some("keep-count"));
    }

    #[test]
    fn remote_run_page_decode_rejects_leaked_provider_route() {
        let leaked = serde_json::json!({
            "runs": [{
                "runId": "public-run-leak",
                "sessionId": "11111111-1111-1111-1111-111111111111",
                "workspace": "/tmp/public-run",
                "requestId": "public-run-req",
                "clientId": "mcp",
                "state": "running",
                "purpose": "execution",
                "bounds": {
                    "maxPromptBytes": 1024,
                    "maxRounds": 4,
                    "maxDurationMs": 1000
                },
                "promptPreview": "inspect",
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:00:00Z",
                "aggregates": {
                    "changes": [],
                    "tests": []
                },
                "providerRoute": {
                    "baseUrl": "http://leak-base-url-sentinel-pr352.example/v1"
                }
            }],
            "totalCount": 1,
            "truncated": false
        });
        assert!(
            decode_remote_run_page(leaked).is_err(),
            "unknown nested providerRoute must fail remote page decode"
        );
    }

    #[test]
    fn merge_remote_run_pages_sums_counts_without_draining_cursors() {
        let newer = sample_public_run("newer-run", "2026-01-01T00:00:02Z");
        let older = sample_public_run("older-run", "2026-01-01T00:00:01Z");
        let merged = merge_remote_run_pages(vec![
            grokptah_agent_bridge::PublicRunPage {
                runs: vec![older],
                total_count: 80,
                truncated: true,
                next_cursor: Some("older-run".into()),
            },
            grokptah_agent_bridge::PublicRunPage {
                runs: vec![newer],
                total_count: 120,
                truncated: false,
                next_cursor: Some("newer-run".into()),
            },
        ]);
        assert_eq!(merged.total_count, 200);
        assert!(merged.truncated);
        assert_eq!(
            merged
                .runs
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer-run", "older-run"]
        );
        assert_eq!(merged.next_cursor, None);
    }

    #[test]
    fn merge_remote_run_pages_caps_first_pages_at_public_limit() {
        let pages = (0..129)
            .map(|index| grokptah_agent_bridge::PublicRunPage {
                runs: vec![sample_public_run(
                    &format!("run-{index:03}"),
                    "2026-01-01T00:00:00Z",
                )],
                total_count: 1,
                truncated: false,
                next_cursor: None,
            })
            .collect();
        let merged = merge_remote_run_pages(pages);
        assert_eq!(
            merged.runs.len(),
            grokptah_agent_bridge::orchestration::MAX_PUBLIC_RUN_LIST
        );
        assert_eq!(merged.total_count, 129);
        assert!(merged.truncated);
        assert_eq!(merged.next_cursor, None);
    }

    #[test]
    fn tauri_run_surfaces_must_return_public_runs() {
        let commands = include_str!("commands.rs");
        assert!(commands.contains("list_public_session_runs_page"));
        assert!(commands.contains("get_public_session_run"));
        assert!(commands.contains("promote_public_session_run"));
        assert!(commands.contains("discard_public_session_run"));
        assert!(commands.contains("Result<PublicRunPage"));
        assert!(commands.contains("Result<Option<PublicRun>"));
        assert!(!commands.contains("host.get_session_run("));
        assert!(!commands.contains("host.promote_run("));
        assert!(!commands.contains("host.discard_run("));
        assert!(!commands.contains("Result<Vec<grokptah_agent_bridge::PublicRun>"));
        let remote = include_str!("remote_service.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(remote.contains("Result<PublicRunPage>"));
        assert!(remote.contains("decode_remote_run_page"));
        assert!(remote.contains("merge_remote_run_pages"));
        assert!(
            !remote.contains("RunRecord"),
            "remote list/get must decode PublicRun instead of RunRecord"
        );
    }

    #[test]
    fn local_http_is_allowed_but_remote_http_and_embedded_credentials_are_not() {
        assert_eq!(
            normalize_base_url("http://127.0.0.1:39200/").unwrap(),
            "http://127.0.0.1:39200"
        );
        assert!(normalize_base_url("http://service.example:39200").is_err());
        assert!(normalize_base_url("https://user:pass@service.example").is_err());
        assert!(normalize_base_url("ftp://service.example").is_err());
    }

    #[test]
    fn remote_client_reconnects_only_for_transport_or_stale_session_errors() {
        assert!(should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP HTTP 400 Bad Request: unknown mcp-session-id"
        )));
        assert!(should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: unknown_session"
        )));
        assert!(should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP HTTP 503 Service Unavailable"
        )));
        assert!(should_reconnect_remote_error(&anyhow::anyhow!(
            "error sending request for url"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP HTTP 400 Bad Request: run already terminal (Completed)"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP HTTP 401 Unauthorized"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP HTTP 409 Conflict"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP error: invalid request"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: invalid_request"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: admission_uncertain"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: conflict"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: workspace_mismatch"
        )));
        assert!(!should_reconnect_remote_error(&anyhow::anyhow!(
            "MCP remote error: capacity_exhausted"
        )));
    }

    #[test]
    fn service_url_projects_local_and_hosted_runtime_ownership() {
        assert_eq!(
            runtime_target_for_base_url("http://127.0.0.1:39200"),
            RuntimeTarget::LocalService
        );
        assert_eq!(
            runtime_target_for_base_url("https://grokptah.example/mcp"),
            RuntimeTarget::HostedService
        );
    }

    #[tokio::test]
    async fn failed_connect_is_exposed_as_error_without_claiming_connected() {
        let state = RemoteServiceState::new();
        assert!(state
            .connect("http://127.0.0.1:1".into(), "test-token".into())
            .await
            .is_err());
        let status = state.status().await;
        assert!(!status.connected);
        assert_eq!(status.connection_state, RuntimeConnectionState::Error);
        assert!(status.last_error.is_some());
    }

    struct IsolatedOffline {
        previous: Option<std::ffi::OsString>,
    }

    impl IsolatedOffline {
        fn enable() -> Self {
            let previous = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
            // SAFETY: this test holds `home_override_serial` and restores the
            // previous value on drop. Isolated reconnect must not capture a
            // live provider route.
            unsafe {
                std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
            }
            Self { previous }
        }
    }

    impl Drop for IsolatedOffline {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
                    None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
                }
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn remote_client_authenticates_and_reconnects_after_service_restart() {
        let _guard = home_override_serial();
        let _offline = IsolatedOffline::enable();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let workspace = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.path().join("orch")).unwrap(),
            OrchestrationConfig {
                bearer_token: "desktop-remote-token".into(),
                allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );

        let first_server = start_control_server(orch.clone(), 0).await.unwrap();
        let base_url = format!("http://{}", first_server.addr);
        host.start().unwrap();
        let mut client =
            RemoteServiceClient::connect(base_url.clone(), "desktop-remote-token".into())
                .await
                .unwrap();
        assert!(client.list_persistent_agents().await.unwrap().is_empty());
        let empty_page = client.list_runs().await.unwrap();
        assert!(empty_page.runs.is_empty());
        assert_eq!(empty_page.total_count, 0);
        assert!(!empty_page.truncated);
        let session = client
            .create_session(
                workspace.path().display().to_string(),
                Some("Desktop remote smoke".into()),
            )
            .await
            .unwrap();
        assert_eq!(session.title, "Desktop remote smoke");
        let submission = client
            .submit_task(
                session.session_id,
                session.workspace.clone(),
                "return a short acknowledgement and stop".into(),
                RunExecutionMode::Shared,
                true,
                "desktop-remote-submit".into(),
            )
            .await
            .unwrap();
        assert!(client
            .list_runs()
            .await
            .unwrap()
            .runs
            .iter()
            .any(|run| run.run_id == submission.run_id));
        let remote_run = client
            .get_run(
                session.session_id,
                session.workspace.clone(),
                submission.run_id.clone(),
            )
            .await
            .unwrap();
        assert_eq!(remote_run.run_id, submission.run_id);
        let encoded = serde_json::to_value(&remote_run).unwrap();
        assert!(encoded.get("providerRoute").is_none());
        assert!(
            encoded.get("providerExecution").is_some(),
            "public Run keeps the providerExecution key even when the Run is offline"
        );
        assert!(RemoteServiceClient::connect(base_url, "wrong-token".into())
            .await
            .is_err());

        let address = first_server.addr;
        first_server.stop_and_wait().await;
        let restarted_server = start_control_server_with_bind(
            orch,
            address,
            ControlServerLimits {
                request_timeout: Duration::from_secs(5),
                ..ControlServerLimits::default()
            },
            false,
        )
        .await
        .unwrap();

        // The first request hits the stale session, then the client rebuilds
        // its MCP session and transparently retries against the restarted service.
        assert!(!client.list_persistent_agents().await.unwrap().is_empty());
        assert!(client
            .list_runs()
            .await
            .unwrap()
            .runs
            .iter()
            .any(|run| run.run_id == submission.run_id));
        restarted_server.stop_and_wait().await;
        set_grokptah_home_override(None);
    }
}
