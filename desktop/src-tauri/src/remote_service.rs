use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    AgentRecord, AgentResumePlan, JournalPage, McpControlClient, RunExecutionMode, RunRecord,
    RunScope, RunState, SessionUpdate,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSessionTarget {
    pub session_id: Uuid,
    pub title: String,
    pub workspace: String,
    pub updated_at: String,
    pub busy: bool,
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
}

impl RemoteServiceState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            client: Mutex::new(None),
            watchers: Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub async fn status(&self) -> RemoteServiceStatus {
        let client = self.client.lock().await;
        RemoteServiceStatus {
            connected: client.is_some(),
            base_url: client.as_ref().map(|client| client.base_url.clone()),
        }
    }

    pub async fn connect(&self, base_url: String, token: String) -> Result<RemoteServiceStatus> {
        let client = RemoteServiceClient::connect(base_url, token).await?;
        let mut current = self.client.lock().await;
        *current = Some(client);
        Ok(RemoteServiceStatus {
            connected: true,
            base_url: current.as_ref().map(|client| client.base_url.clone()),
        })
    }

    pub async fn disconnect(&self) {
        let watchers = std::mem::take(&mut *self.watchers.lock().await);
        for cancel in watchers.into_values() {
            let _ = cancel.send(true);
        }
        self.client.lock().await.take();
    }

    pub async fn list_persistent_agents(&self) -> Result<Option<Vec<AgentRecord>>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_persistent_agents().await?))
    }

    pub async fn list_sessions(&self) -> Result<Option<Vec<RemoteSessionTarget>>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_sessions().await?))
    }

    pub async fn create_session(
        &self,
        workspace: String,
        title: Option<String>,
    ) -> Result<Option<RemoteSessionTarget>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.create_session(workspace, title).await?))
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

    pub async fn list_runs(&self) -> Result<Option<Vec<RunRecord>>> {
        let mut client = self.client.lock().await;
        let Some(client) = client.as_mut() else {
            return Ok(None);
        };
        Ok(Some(client.list_runs().await?))
    }

    pub async fn get_run(
        &self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
    ) -> Result<Option<RunRecord>> {
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
        Ok(sessions.into_iter().map(Into::into).collect())
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
        serde_json::from_value(value).context("decode remote session creation")
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
            .find(|agent| agent.session_id == session_id)
            .ok_or_else(|| anyhow::anyhow!("remote session has no persistent agent"))?;
        let value = self
            .call_tool(
                "ptah_get_persistent_agent",
                json!({
                    "session_id": agent.session_id,
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
            .find(|agent| agent.session_id == session_id)
            .ok_or_else(|| anyhow::anyhow!("remote session has no persistent agent"))?;
        let value = self
            .call_tool(
                "ptah_resume_persistent_agent",
                json!({
                    "request_id": request_id,
                    "session_id": agent.session_id,
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

    async fn list_runs(&mut self) -> Result<Vec<RunRecord>> {
        let sessions = self.list_sessions().await?;
        let mut runs = Vec::new();
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
            let mut scoped: Vec<RunRecord> = serde_json::from_value(
                value
                    .get("runs")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("remote run list omitted runs"))?,
            )
            .context("decode remote durable run list")?;
            runs.append(&mut scoped);
        }
        runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(runs)
    }

    async fn get_run(
        &mut self,
        session_id: Uuid,
        workspace: String,
        run_id: String,
    ) -> Result<RunRecord> {
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
/// errors such as an invalid request or a terminal run must be returned
/// directly; replaying those requests can duplicate mutations.
fn should_reconnect_remote_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    if let Some(rest) = message.strip_prefix("MCP HTTP ") {
        let status = rest
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u16>().ok());
        return message.contains("unknown mcp-session-id")
            || status.is_some_and(|code| code == 408 || code == 429 || code >= 500);
    }
    if message.starts_with("MCP error:")
        || message.starts_with("missing required argument ")
        || message.starts_with("unexpected argument ")
        || message.starts_with("unknown tool ")
        || message.starts_with("MCP client not initialized")
    {
        return false;
    }
    true
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use grokptah_agent_bridge::{
        home_override_serial, set_grokptah_home_override, start_control_server,
        start_control_server_with_bind, AgentHost, ControlServerLimits, HostConfig, OrchStore,
        OrchestrationConfig, OrchestrationService, RunBounds, RunExecutionMode, WorkspaceAllowlist,
    };
    use tempfile::tempdir;

    use super::{normalize_base_url, should_reconnect_remote_error, RemoteServiceClient};

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
            "MCP error: invalid request"
        )));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn remote_client_authenticates_and_reconnects_after_service_restart() {
        let _guard = home_override_serial();
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
        assert!(client.list_runs().await.unwrap().is_empty());
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
            .iter()
            .any(|run| run.run_id == submission.run_id));
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
            .iter()
            .any(|run| run.run_id == submission.run_id));
        restarted_server.stop_and_wait().await;
        set_grokptah_home_override(None);
    }
}
