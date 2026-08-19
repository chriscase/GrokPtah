use std::path::PathBuf;

use grokptah_agent_bridge::{
    desktop_auto_update_enabled, AuthState, BackgroundTask, ComputerAction, ComputerPermission,
    ComputerPermissionStatus, ComputerPlatformStatus, ComputerTargetCandidate, EffortLevel,
    JournalPage, McpServerInfo, ModelInfo, PermissionDecision, PluginInfo, PromptQueueEntry,
    PromptQueueRunNextResult, PromptQueueSnapshot, PromptQueueTakeResult, ProviderDeadlineClass,
    ProviderProfileUpdate, ProviderQualificationReport, RunExecutionMode, RunReview, RunState,
    SearchHit, SearchQuery, SessionCompletion, SessionKind, SessionSummary, SkillInfo,
    SteeringReceipt, SubagentInfo, TranscriptEntry, WorkspaceUiState, BRIDGE_VERSION, PRODUCT_NAME,
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::pty_host::PtyBacklog;
use crate::AppState;

fn map_err(e: impl ToString) -> String {
    e.to_string()
}

fn desktop_mcp_orchestration(
    state: &AppState,
) -> Result<
    (
        std::sync::Arc<grokptah_agent_bridge::OrchestrationService>,
        String,
    ),
    String,
> {
    let control = state
        .control
        .lock()
        .map_err(|_| "MCP control state is unavailable".to_string())?;
    let server = control
        .as_ref()
        .ok_or_else(|| "MCP control plane is not running".to_string())?;
    if server.token.is_empty() {
        return Err("MCP control authentication is unavailable".into());
    }
    Ok((server.orchestration_service(), server.token.clone()))
}

/// Run a blocking host/FS/git call off the UI thread (#137).
async fn run_blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("blocking task join: {e}"))?
}

#[tauri::command]
pub fn agent_start(state: State<'_, AppState>) -> Result<(), String> {
    state.host.start().map_err(map_err)
}

#[tauri::command]
pub fn agent_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.host.stop().map_err(map_err)
}

#[tauri::command]
pub fn agent_status(state: State<'_, AppState>) -> grokptah_agent_bridge::AgentStatus {
    state.host.status()
}

#[tauri::command]
pub async fn remote_service_connect(
    state: State<'_, AppState>,
    base_url: String,
    token: String,
) -> Result<crate::remote_service::RemoteServiceStatus, String> {
    state
        .remote_service
        .connect(base_url, token)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub async fn remote_service_disconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.remote_service.disconnect().await;
    Ok(())
}

#[tauri::command]
pub async fn remote_service_status(
    state: State<'_, AppState>,
) -> Result<crate::remote_service::RemoteServiceStatus, String> {
    Ok(state.remote_service.status().await)
}

#[tauri::command]
pub async fn remote_service_session_list(
    state: State<'_, AppState>,
) -> Result<Vec<crate::remote_service::RemoteSessionTarget>, String> {
    state
        .remote_service
        .list_sessions()
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_session_create(
    state: State<'_, AppState>,
    workspace: String,
    title: Option<String>,
) -> Result<crate::remote_service::RemoteSessionTarget, String> {
    state
        .remote_service
        .create_session(workspace, title)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_task_submit(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    prompt: String,
    execution_mode: Option<RunExecutionMode>,
    allow_queue: Option<bool>,
) -> Result<crate::remote_service::RemoteTaskSubmission, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .submit_task(
            session_id,
            workspace,
            prompt,
            execution_mode.unwrap_or_default(),
            allow_queue.unwrap_or(true),
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_run_list(
    state: State<'_, AppState>,
) -> Result<Vec<grokptah_agent_bridge::RunRecord>, String> {
    state
        .remote_service
        .list_runs()
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_list(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
) -> Result<Vec<grokptah_agent_bridge::WorkItem>, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .list_work(session_id, workspace)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_get(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    work_id: String,
) -> Result<crate::remote_service::RemoteWorkSnapshot, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .get_work(session_id, workspace, work_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn work_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<grokptah_agent_bridge::WorkItem>, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.list_work_items_for_session(session_id)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_get(
    state: State<'_, AppState>,
    session_id: String,
    work_id: String,
) -> Result<Option<grokptah_agent_bridge::WorkItemSnapshot>, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.get_work_item_snapshot(session_id, &work_id)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_create(
    state: State<'_, AppState>,
    session_id: String,
    kind: String,
    objective: String,
    priority: i32,
    requires_approval: bool,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.create_work_item(session_id, kind, objective, priority, requires_approval)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_assign(
    state: State<'_, AppState>,
    session_id: String,
    work_id: String,
    assigned_agent_id: Option<String>,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.assign_work_item(session_id, &work_id, assigned_agent_id, expected_revision)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_retry(
    state: State<'_, AppState>,
    session_id: String,
    work_id: String,
    reason: String,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.retry_work_item(session_id, &work_id, &reason, expected_revision)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_approve(
    state: State<'_, AppState>,
    session_id: String,
    work_id: String,
    note: Option<String>,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.approve_work_item(session_id, &work_id, note, expected_revision)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn work_cancel(
    state: State<'_, AppState>,
    session_id: String,
    work_id: String,
    reason: String,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.cancel_work_item(session_id, &work_id, &reason, expected_revision)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn remote_service_work_create(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    kind: String,
    objective: String,
    priority: i32,
    requires_approval: bool,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .create_work(
            session_id,
            workspace,
            kind,
            objective,
            priority,
            requires_approval,
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_assign(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    work_id: String,
    assigned_agent_id: Option<String>,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .assign_work(
            session_id,
            workspace,
            work_id,
            assigned_agent_id,
            expected_revision,
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_retry(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    work_id: String,
    reason: String,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .retry_work(session_id, workspace, work_id, reason, expected_revision)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_approve(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    work_id: String,
    note: Option<String>,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .approve_work(session_id, workspace, work_id, note, expected_revision)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_work_cancel(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    work_id: String,
    reason: String,
    expected_revision: Option<u64>,
) -> Result<grokptah_agent_bridge::WorkItem, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .cancel_work(session_id, workspace, work_id, reason, expected_revision)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_run_get(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    run_id: String,
) -> Result<grokptah_agent_bridge::RunRecord, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .get_run(session_id, workspace, run_id)
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_run_events(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    run_id: String,
    after_seq: Option<u64>,
    limit: Option<usize>,
) -> Result<grokptah_agent_bridge::JournalPage, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .get_events(
            session_id,
            workspace,
            run_id,
            after_seq.unwrap_or(0),
            limit.unwrap_or(80),
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_run_steer(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    text: String,
) -> Result<(), String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .steer(session_id, workspace, text, Uuid::new_v4().to_string())
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_run_cancel(
    state: State<'_, AppState>,
    session_id: String,
    workspace: String,
    run_id: String,
) -> Result<(), String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .remote_service
        .cancel(session_id, workspace, run_id, Uuid::new_v4().to_string())
        .await
        .map_err(map_err)?
        .ok_or_else(|| "remote service is not connected".to_string())
}

#[tauri::command]
pub async fn remote_service_watch_runs(
    state: State<'_, AppState>,
    app: AppHandle,
    scopes: Vec<crate::remote_service::RemoteRunScope>,
) -> Result<(), String> {
    state
        .remote_service
        .watch_runs(scopes, app)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub async fn persistent_agent_list(
    state: State<'_, AppState>,
) -> Result<Vec<grokptah_agent_bridge::AgentRecord>, String> {
    if let Some(agents) = state
        .remote_service
        .list_persistent_agents()
        .await
        .map_err(map_err)?
    {
        return Ok(agents);
    }
    let host = state.host.clone();
    run_blocking(move || host.list_persistent_agents().map_err(map_err)).await
}

#[tauri::command]
pub async fn persistent_agent_get(
    state: State<'_, AppState>,
    agent_id: String,
) -> Result<Option<grokptah_agent_bridge::AgentRecord>, String> {
    if let Some(agent) = state
        .remote_service
        .get_persistent_agent(&agent_id)
        .await
        .map_err(map_err)?
    {
        return Ok(Some(agent));
    }
    let host = state.host.clone();
    run_blocking(move || host.get_persistent_agent(&agent_id).map_err(map_err)).await
}

#[tauri::command]
pub async fn persistent_agent_attach_session(
    state: State<'_, AppState>,
    session_id: String,
    agent_id: String,
) -> Result<grokptah_agent_bridge::AgentRecord, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let host = state.host.clone();
    run_blocking(move || {
        host.attach_session_to_agent(session_id, &agent_id)
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub fn lane_list(
    state: State<'_, AppState>,
    include_archived: bool,
) -> Vec<grokptah_agent_bridge::LaneSummary> {
    state.host.list_lanes(include_archived)
}

#[tauri::command]
pub async fn persistent_agent_resume_plan(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<grokptah_agent_bridge::AgentResumePlan, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    if let Some(plan) = state
        .remote_service
        .resume_plan(session_id)
        .await
        .map_err(map_err)?
    {
        return Ok(plan);
    }
    let host = state.host.clone();
    run_blocking(move || host.prepare_agent_resume(session_id).map_err(map_err)).await
}

/// Manual continuation only: the caller supplies a fresh instruction and the
/// bridge validates the latest durable checkpoint before starting a run.
#[tauri::command]
pub async fn persistent_agent_resume(
    state: State<'_, AppState>,
    session_id: String,
    prompt: String,
    max_rounds: Option<u32>,
    request_id: Option<String>,
) -> Result<String, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    if let Some(response) = state
        .remote_service
        .resume(
            session_id,
            prompt.clone(),
            max_rounds,
            request_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
        )
        .await
        .map_err(map_err)?
    {
        return Ok(response);
    }
    state
        .host
        .resume_agent_with_request_id(session_id, prompt, max_rounds, request_id)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub fn computer_use_status(state: State<'_, AppState>) -> ComputerPlatformStatus {
    state.computer_use.status()
}

#[tauri::command]
pub async fn computer_use_request_permission(
    state: State<'_, AppState>,
    permission: String,
) -> Result<ComputerPermissionStatus, String> {
    let permission = match permission.as_str() {
        "screen_recording" => ComputerPermission::ScreenRecording,
        "accessibility" => ComputerPermission::Accessibility,
        _ => return Err("Unknown Computer Use permission".into()),
    };
    state.computer_use.request_permission(permission).await
}

#[tauri::command]
pub async fn computer_use_list_targets(
    state: State<'_, AppState>,
) -> Result<Vec<ComputerTargetCandidate>, String> {
    state.computer_use.list_targets().await
}

#[tauri::command]
pub async fn computer_use_observe_once(
    state: State<'_, AppState>,
    selection_token: String,
) -> Result<crate::computer_use::ObservationPreview, String> {
    let owner_session_id = state
        .host
        .status()
        .active_session
        .ok_or_else(|| "Open a session before observing another application".to_string())?;
    state
        .computer_use
        .observe_once(&selection_token, owner_session_id)
        .await
}

fn computer_owner(state: &AppState, session_id: &str) -> Result<Uuid, String> {
    let owner = Uuid::parse_str(session_id).map_err(map_err)?;
    state
        .host
        .list_all_sessions()
        .iter()
        .any(|session| session.id == owner)
        .then_some(owner)
        .ok_or_else(|| "Computer Use requires an existing local session".to_string())
}

fn computer_work_owner(state: &AppState, session_id: &str) -> Result<Uuid, String> {
    let owner = computer_owner(state, session_id)?;
    state
        .host
        .ensure_session_accepts_new_work(owner)
        .map_err(map_err)?;
    Ok(owner)
}

#[tauri::command]
pub fn computer_use_cockpit_snapshot(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .cockpit_snapshot(computer_owner(&state, &session_id)?)
}

#[tauri::command]
pub fn computer_use_cockpit_agent_eligibility(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<grokptah_agent_bridge::ComputerAgentEligibility, String> {
    state
        .host
        .computer_agent_eligibility(computer_owner(&state, &session_id)?)
        .map_err(map_err)
}

#[tauri::command]
pub async fn computer_use_cockpit_qualify_agent(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<grokptah_agent_bridge::ComputerAgentEligibility, String> {
    state
        .host
        .qualify_computer_agent(computer_owner(&state, &session_id)?)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub async fn computer_use_cockpit_propose_agent_action(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    expected_version: u64,
    observation_id: String,
    objective: String,
) -> Result<crate::computer_use::ComputerAgentProposalResult, String> {
    let owner = computer_owner(&state, &session_id)?;
    let observation = state.computer_use.model_proposal_context(
        owner,
        &run_id,
        expected_version,
        &observation_id,
    )?;
    let proposal = state
        .host
        .propose_computer_action(owner, &objective, &observation)
        .await
        .map_err(map_err)?;
    state
        .computer_use
        .apply_model_proposal(owner, &run_id, expected_version, &observation_id, proposal)
        .await
}

#[tauri::command]
pub fn computer_use_cockpit_cancel_agent(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<bool, String> {
    Ok(state
        .host
        .cancel_computer_agent(computer_owner(&state, &session_id)?))
}

#[tauri::command]
pub async fn computer_use_cockpit_start_simulator(
    state: State<'_, AppState>,
    session_id: String,
    reviewed_target_app_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .start_simulator(
            computer_work_owner(&state, &session_id)?,
            &reviewed_target_app_id,
        )
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_start_native(
    state: State<'_, AppState>,
    session_id: String,
    selection_token: String,
    reviewed_target_app_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .start_native(
            computer_work_owner(&state, &session_id)?,
            &selection_token,
            &reviewed_target_app_id,
        )
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_refresh(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    expected_version: u64,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .refresh_simulator(
            computer_work_owner(&state, &session_id)?,
            &run_id,
            expected_version,
        )
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_stage_action(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    expected_version: u64,
    observation_id: String,
    action: ComputerAction,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .stage_simulator_action(
            computer_work_owner(&state, &session_id)?,
            &run_id,
            expected_version,
            &observation_id,
            action,
        )
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_approve(
    state: State<'_, AppState>,
    session_id: String,
    approval_id: String,
    request_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .approve_simulator_action(
            computer_work_owner(&state, &session_id)?,
            &approval_id,
            &request_id,
        )
        .await
}

#[tauri::command]
pub fn computer_use_cockpit_discard_approval(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    state
        .computer_use
        .discard_simulator_approval(computer_owner(&state, &session_id)?)
}

#[tauri::command]
pub async fn computer_use_cockpit_pause(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    expected_version: u64,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    let owner = computer_owner(&state, &session_id)?;
    state.host.cancel_computer_agent(owner);
    state
        .computer_use
        .pause_simulator(owner, &run_id, expected_version)
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_take_over(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    expected_version: u64,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    let owner = computer_owner(&state, &session_id)?;
    state.host.cancel_computer_agent(owner);
    state
        .computer_use
        .take_over_simulator(owner, &run_id, expected_version)
        .await
}

#[tauri::command]
pub async fn computer_use_cockpit_stop(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<crate::computer_use::ComputerCockpitSnapshot, String> {
    let owner = computer_owner(&state, &session_id)?;
    state.host.cancel_computer_agent(owner);
    state.computer_use.stop_simulator(owner, &run_id).await
}

#[tauri::command]
pub fn set_project_cwd(state: State<'_, AppState>, path: String) -> Result<String, String> {
    state.host.set_project_cwd(path).map_err(map_err)
}

#[tauri::command]
pub async fn pick_project_folder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    let path = app
        .dialog()
        .file()
        .set_title("Open project folder")
        .blocking_pick_folder();
    match path {
        Some(p) => {
            let path_buf = p.into_path().map_err(map_err)?;
            let s = path_buf.to_string_lossy().into_owned();
            state.host.set_project_cwd(&s).map_err(map_err)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

#[tauri::command]
pub fn session_new(state: State<'_, AppState>) -> Result<SessionSummary, String> {
    state.host.session_new().map_err(map_err)
}

#[tauri::command]
pub fn session_new_kind(
    state: State<'_, AppState>,
    kind: String,
) -> Result<SessionSummary, String> {
    state
        .host
        .session_new_kind(SessionKind::parse(&kind))
        .map_err(map_err)
}

#[tauri::command]
pub fn session_list_by_kind(
    state: State<'_, AppState>,
    kind: String,
    include_archived: bool,
) -> Vec<SessionSummary> {
    state
        .host
        .list_sessions_by_kind(SessionKind::parse(&kind), include_archived)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // mirrors SearchQuery fields for the IPC boundary
pub async fn search_sessions(
    state: State<'_, AppState>,
    query: String,
    mode: Option<String>,
    kind: Option<String>,
    include_archived: Option<bool>,
    limit: Option<usize>,
    folder: Option<String>,
    tag: Option<String>,
) -> Result<Vec<SearchHit>, String> {
    let host = state.host.clone();
    let q = SearchQuery {
        query,
        mode: mode.unwrap_or_else(|| "hybrid".into()),
        kind: kind.unwrap_or_else(|| "all".into()),
        include_archived: include_archived.unwrap_or(false),
        limit: limit.unwrap_or(40),
        folder,
        tag,
    };
    run_blocking(move || host.search_sessions(q).map_err(map_err)).await
}

#[tauri::command]
pub async fn session_load(
    state: State<'_, AppState>,
    id: String,
) -> Result<SessionSummary, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&id).map_err(map_err)?;
    run_blocking(move || host.session_load(id).map_err(map_err)).await
}

#[tauri::command]
pub async fn session_inspect(
    state: State<'_, AppState>,
    id: String,
) -> Result<SessionSummary, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&id).map_err(map_err)?;
    run_blocking(move || host.session_inspect(id).map_err(map_err)).await
}

#[tauri::command]
pub fn session_list(state: State<'_, AppState>) -> Vec<SessionSummary> {
    state.host.list_sessions()
}

#[tauri::command]
pub fn session_list_archived(state: State<'_, AppState>) -> Vec<SessionSummary> {
    state.host.list_sessions_filtered(true)
}

#[tauri::command]
pub fn session_list_all(state: State<'_, AppState>) -> Vec<SessionSummary> {
    state.host.list_all_sessions()
}

#[tauri::command]
pub fn session_rename(
    state: State<'_, AppState>,
    session_id: String,
    title: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_rename(id, title).map_err(map_err)
}

#[tauri::command]
pub fn session_delete(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_delete(id).map_err(map_err)
}

#[tauri::command]
pub fn session_archive(
    state: State<'_, AppState>,
    session_id: String,
    archived: bool,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_archive(id, archived).map_err(map_err)
}

#[tauri::command]
pub fn session_set_folder(
    state: State<'_, AppState>,
    session_id: String,
    folder: Option<String>,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_set_folder(id, folder).map_err(map_err)
}

#[tauri::command]
pub fn session_set_cwd(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_set_cwd(id, path).map_err(map_err)
}

/// Set the execution policy for future Build turns in one session.
#[tauri::command]
pub fn session_set_execution_mode(
    state: State<'_, AppState>,
    session_id: String,
    mode: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mode = match mode.trim().to_ascii_lowercase().as_str() {
        "shared" => RunExecutionMode::Shared,
        "isolated_worktree" | "isolated" => RunExecutionMode::IsolatedWorktree,
        other => return Err(format!("unknown execution mode {other}")),
    };
    state
        .host
        .session_set_execution_mode(id, mode)
        .map_err(map_err)
}

/// Folder picker scoped to one session (does not require a global project first).
#[tauri::command]
pub async fn pick_session_folder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Option<SessionSummary>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let path = app
        .dialog()
        .file()
        .set_title("Set working directory for this build")
        .blocking_pick_folder();
    match path {
        Some(p) => {
            let path_buf = p.into_path().map_err(map_err)?;
            let s = path_buf.to_string_lossy().into_owned();
            let summary = state.host.session_set_cwd(id, &s).map_err(map_err)?;
            Ok(Some(summary))
        }
        None => Ok(None),
    }
}

#[tauri::command]
pub fn session_set_tags(
    state: State<'_, AppState>,
    session_id: String,
    tags: Vec<String>,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_set_tags(id, tags).map_err(map_err)
}

#[tauri::command]
pub fn session_list_folders(state: State<'_, AppState>, include_archived: bool) -> Vec<String> {
    state.host.list_folders(include_archived)
}

#[tauri::command]
pub fn session_list_tags(state: State<'_, AppState>, include_archived: bool) -> Vec<String> {
    state.host.list_tags(include_archived)
}

#[tauri::command]
pub fn workspace_state(state: State<'_, AppState>) -> WorkspaceUiState {
    state.host.workspace_ui_state()
}

#[tauri::command]
pub fn set_open_tabs(
    state: State<'_, AppState>,
    tab_ids: Vec<String>,
    active_id: Option<String>,
) -> Result<(), String> {
    let ids: Result<Vec<Uuid>, _> = tab_ids.iter().map(|s| Uuid::parse_str(s)).collect();
    let ids = ids.map_err(map_err)?;
    let active = match active_id {
        Some(s) if !s.is_empty() => Some(Uuid::parse_str(&s).map_err(map_err)?),
        _ => None,
    };
    state.host.set_open_tabs(ids, active);
    Ok(())
}

#[tauri::command]
pub async fn session_prompt(
    state: State<'_, AppState>,
    session_id: String,
    prompt: String,
    reservation: Option<String>,
) -> Result<String, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    // A queue drain reserves the session's turn slot under the same lock that
    // removed the batch, so the drained prompt must start the turn it reserved
    // rather than racing for a fresh one.
    match reservation {
        Some(owner) => state
            .host
            .session_prompt_reserved_with_max_rounds(id, prompt, None, &owner)
            .await
            .map_err(map_err),
        None => state.host.session_prompt(id, prompt).await.map_err(map_err),
    }
}

#[tauri::command]
pub fn session_queue_restore_drain(
    state: State<'_, AppState>,
    session_id: String,
    reservation: Option<String>,
    entries: Vec<PromptQueueEntry>,
) -> Result<Vec<PromptQueueEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_restore_drain(id, reservation.as_deref(), entries)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PromptQueueSnapshot, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    // Returns the revision alongside the entries so the GUI can order a
    // refetch against the event stream instead of always trusting whichever
    // arrived last.
    state.host.session_queue_snapshot(id).map_err(map_err)
}

#[tauri::command]
pub fn session_queue_add(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
    priority: bool,
) -> Result<Vec<PromptQueueEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_add(id, text, priority)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_edit(
    state: State<'_, AppState>,
    session_id: String,
    entry_id: String,
    version: u64,
    text: String,
) -> Result<Vec<PromptQueueEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_edit(id, &entry_id, version, text)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_remove(
    state: State<'_, AppState>,
    session_id: String,
    entry_id: String,
    expected_version: u64,
) -> Result<Vec<PromptQueueEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_remove(id, &entry_id, expected_version)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_clear(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<PromptQueueEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_queue_clear(id).map_err(map_err)
}

#[tauri::command]
pub fn session_queue_move(
    state: State<'_, AppState>,
    session_id: String,
    entry_id: String,
    to_index: usize,
    expected_version: u64,
    expected_revision: u64,
) -> Result<PromptQueueSnapshot, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    // An absolute reorder is a statement about an ordering, so the desktop
    // supplies the revision it computed against, exactly as a coordinator does.
    state
        .host
        .session_queue_move(id, &entry_id, to_index, expected_version, expected_revision)
        .map(|(entries, revision)| PromptQueueSnapshot { entries, revision })
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_take_next(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<PromptQueueTakeResult, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_queue_take_next(id).map_err(map_err)
}

#[tauri::command]
pub fn session_queue_run_next(
    state: State<'_, AppState>,
    session_id: String,
    entry_id: String,
    expected_version: u64,
) -> Result<PromptQueueRunNextResult, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_run_next(id, &entry_id, expected_version)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_queue_steer_entry(
    state: State<'_, AppState>,
    session_id: String,
    entry_id: String,
    expected_version: u64,
) -> Result<SteeringReceipt, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .session_queue_steer_entry(id, &entry_id, expected_version)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_steer(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> Result<SteeringReceipt, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_steer(id, text).map_err(map_err)
}

#[tauri::command]
pub fn session_cancel(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<(), String> {
    let id = match session_id {
        Some(s) if !s.is_empty() => Some(Uuid::parse_str(&s).map_err(map_err)?),
        _ => None,
    };
    state.host.cancel_turn(id).map_err(map_err)
}

#[tauri::command]
pub async fn session_transcript(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<TranscriptEntry>, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.session_transcript(id).map_err(map_err)).await
}

#[tauri::command]
pub async fn session_completion_history(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<SessionCompletion>, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.session_completion_history(id).map_err(map_err)).await
}

/// Read-only durable Build-run history for the requested session.
#[tauri::command]
pub async fn run_list(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<grokptah_agent_bridge::RunRecord>, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.list_session_runs(id).map_err(map_err)).await
}

/// Read one durable Build run, scoped to its owning session.
#[tauri::command]
pub async fn run_get(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<Option<grokptah_agent_bridge::RunRecord>, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.get_session_run(id, &run_id).map_err(map_err)).await
}

/// Read the bounded, durable event journal for one run.
#[tauri::command]
pub async fn run_events(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    after_seq: Option<u64>,
    limit: Option<usize>,
) -> Result<JournalPage, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || {
        host.get_session_run_events(id, &run_id, after_seq.unwrap_or(0), limit.unwrap_or(80))
            .map_err(map_err)
    })
    .await
}

/// Read the bounded diff for a completed isolated run.
#[tauri::command]
pub async fn run_review(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<RunReview, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.review_run(id, &run_id).map_err(map_err)).await
}

/// Persist an exact-scope approval for an MCP-owned isolated run. The desktop
/// uses the same durable approval contract as an external coordinator so an
/// approval remains visible and valid across restart.
#[tauri::command]
pub async fn run_approve(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    ttl_ms: Option<u64>,
) -> Result<grokptah_agent_bridge::RunRecord, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (orch, token) = desktop_mcp_orchestration(&state)?;
    let auth = orch
        .auth_header(Some(&format!("Bearer {token}")))
        .map_err(map_err)?;
    let session = state.host.session_load(session_id).map_err(map_err)?;
    if session.cwd.is_empty() {
        return Err("the session has no workspace".into());
    }
    let source = state
        .host
        .get_session_run(session_id, &run_id)
        .map_err(map_err)?
        .ok_or_else(|| "unknown run for this session".to_string())?;
    if source.client_id.as_deref() != Some("mcp") {
        return Err("desktop approval is limited to MCP-owned runs".into());
    }
    let execution = source
        .execution
        .clone()
        .ok_or_else(|| "run used shared execution and has no isolated diff".to_string())?;
    if execution.mode != RunExecutionMode::IsolatedWorktree {
        return Err("run used shared execution and cannot be approved".into());
    }
    let review = state
        .host
        .review_run(session_id, &run_id)
        .map_err(map_err)?;
    orch.approve_run(
        &auth,
        &Uuid::new_v4().to_string(),
        session_id,
        &PathBuf::from(&session.cwd),
        &run_id,
        execution.source_fingerprint,
        review.fingerprint,
        review.changed_files,
        ttl_ms,
    )
    .await
    .map_err(map_err)?;
    state
        .host
        .get_session_run(session_id, &run_id)
        .map_err(map_err)?
        .ok_or_else(|| "run disappeared after approval".into())
}

/// Promote a reviewed isolated run into its unchanged source workspace.
#[tauri::command]
pub async fn run_promote(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<grokptah_agent_bridge::RunRecord, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || {
        let run = host
            .get_session_run(id, &run_id)
            .map_err(map_err)?
            .ok_or_else(|| "unknown run for this session".to_string())?;
        match run.approval.as_ref() {
            Some(approval) => host
                .promote_run_with_approval(id, &run_id, Some(&approval.approval_id))
                .map_err(map_err),
            None => host.promote_run(id, &run_id).map_err(map_err),
        }
    })
    .await
}

/// Discard an isolated run and remove only its managed worktree.
#[tauri::command]
pub async fn run_discard(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<grokptah_agent_bridge::RunRecord, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.discard_run(id, &run_id).map_err(map_err)).await
}

/// Explicitly retry an interrupted MCP-owned run through the shared
/// orchestration policy service. The desktop supplies only a fresh prompt;
/// the service preserves ownership, mode, bounds, idempotency, and queueing.
#[tauri::command]
pub async fn run_retry(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    prompt: String,
) -> Result<String, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (orch, token) = desktop_mcp_orchestration(&state)?;
    let auth = orch
        .auth_header(Some(&format!("Bearer {token}")))
        .map_err(map_err)?;
    let session = state.host.session_load(session_id).map_err(map_err)?;
    if session.cwd.is_empty() {
        return Err("the session has no workspace".into());
    }
    let source = state
        .host
        .get_session_run(session_id, &run_id)
        .map_err(map_err)?
        .ok_or_else(|| "unknown run for this session".to_string())?;
    if source.client_id.as_deref() != Some("mcp") {
        return Err("desktop retry is limited to MCP-owned runs".into());
    }
    let response = orch
        .retry_run(
            &auth,
            &Uuid::new_v4().to_string(),
            session_id,
            &PathBuf::from(&session.cwd),
            &run_id,
            prompt,
            None,
            None,
            true,
        )
        .await
        .map_err(map_err)?;
    response["runId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "retry did not return a run id".into())
}

/// Deliver a non-cancelling steering prompt to the selected MCP-owned turn.
#[tauri::command]
pub async fn run_steer(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
    text: String,
) -> Result<(), String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (orch, token) = desktop_mcp_orchestration(&state)?;
    let auth = orch
        .auth_header(Some(&format!("Bearer {token}")))
        .map_err(map_err)?;
    let session = state.host.session_load(session_id).map_err(map_err)?;
    if session.cwd.is_empty() {
        return Err("the session has no workspace".into());
    }
    let source = state
        .host
        .get_session_run(session_id, &run_id)
        .map_err(map_err)?
        .ok_or_else(|| "unknown run for this session".to_string())?;
    if source.client_id.as_deref() != Some("mcp") {
        return Err("desktop steering is limited to MCP-owned runs".into());
    }
    if !matches!(source.state, RunState::Running | RunState::Queued) {
        return Err("only active or queued MCP runs can be steered".into());
    }
    orch.steer(
        &auth,
        &Uuid::new_v4().to_string(),
        session_id,
        &PathBuf::from(&session.cwd),
        text,
    )
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Cancel one MCP-owned run using the shared transactional cancellation path.
#[tauri::command]
pub async fn run_cancel(
    state: State<'_, AppState>,
    session_id: String,
    run_id: String,
) -> Result<(), String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (orch, token) = desktop_mcp_orchestration(&state)?;
    let auth = orch
        .auth_header(Some(&format!("Bearer {token}")))
        .map_err(map_err)?;
    let session = state.host.session_load(session_id).map_err(map_err)?;
    if session.cwd.is_empty() {
        return Err("the session has no workspace".into());
    }
    let source = state
        .host
        .get_session_run(session_id, &run_id)
        .map_err(map_err)?
        .ok_or_else(|| "unknown run for this session".to_string())?;
    if source.client_id.as_deref() != Some("mcp") {
        return Err("desktop cancellation is limited to MCP-owned runs".into());
    }
    orch.cancel(
        &auth,
        &Uuid::new_v4().to_string(),
        session_id,
        &PathBuf::from(&session.cwd),
        Some(&run_id),
    )
    .await
    .map_err(map_err)?;
    Ok(())
}

#[tauri::command]
pub fn session_fork(
    state: State<'_, AppState>,
    source_id: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&source_id).map_err(map_err)?;
    state.host.fork_session(id).map_err(map_err)
}

#[tauri::command]
pub fn session_rewind(
    state: State<'_, AppState>,
    session_id: String,
    keep_messages: usize,
    mode: Option<String>,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mode = mode.unwrap_or_else(|| "conversation".into());
    state
        .host
        .rewind_session(id, keep_messages, &mode)
        .map_err(map_err)
}

#[tauri::command]
pub async fn session_compact(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    // Prefer model-backed summary when online (same path as slash `/compact`).
    state.host.compact_session_async(id).await.map_err(map_err)
}

#[tauri::command]
pub fn permission_respond(
    state: State<'_, AppState>,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&request_id).map_err(map_err)?;
    let d = match decision.as_str() {
        "allow" => PermissionDecision::Allow,
        "deny" => PermissionDecision::Deny,
        "always_allow" | "alwaysAllow" => PermissionDecision::AlwaysAllow,
        other => return Err(format!("unknown decision {other}")),
    };
    state.host.permission_respond(id, d).map_err(map_err)
}

#[tauri::command]
pub fn list_models(state: State<'_, AppState>) -> Vec<ModelInfo> {
    state.host.models()
}

#[tauri::command]
pub fn set_model(state: State<'_, AppState>, model: String) {
    state.host.set_model(model);
}

#[tauri::command]
pub fn set_effort(state: State<'_, AppState>, effort: String) -> Result<(), String> {
    let e = match effort.as_str() {
        "none" => EffortLevel::None,
        "minimal" => EffortLevel::Minimal,
        "low" => EffortLevel::Low,
        "medium" => EffortLevel::Medium,
        "high" => EffortLevel::High,
        "xhigh" => EffortLevel::Xhigh,
        "max" => EffortLevel::Max,
        other => return Err(format!("unknown effort {other}")),
    };
    state.host.set_effort(e);
    Ok(())
}

#[tauri::command]
pub fn set_always_approve(state: State<'_, AppState>, value: bool) {
    state.host.set_always_approve(value);
}

#[tauri::command]
pub fn auth_state(state: State<'_, AppState>) -> AuthState {
    state.host.auth_state()
}

#[tauri::command]
pub fn sign_in_local(state: State<'_, AppState>, display_name: String) -> AuthState {
    state.host.sign_in_local(display_name)
}

#[tauri::command]
pub fn sign_out(state: State<'_, AppState>) -> AuthState {
    state.host.sign_out()
}

#[tauri::command]
pub fn auth_set_api_key(
    state: State<'_, AppState>,
    api_key: String,
    display_name: String,
) -> Result<AuthState, String> {
    state
        .host
        .set_api_key(api_key, display_name)
        .map_err(map_err)
}

#[tauri::command]
pub fn auth_open_login(state: State<'_, AppState>) -> Result<String, String> {
    state.host.open_login().map_err(map_err)
}

#[tauri::command]
pub async fn file_tree(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let host = state.host.clone();
    run_blocking(move || host.file_tree().map_err(map_err)).await
}

#[tauri::command]
pub async fn fuzzy_open(state: State<'_, AppState>, query: String) -> Result<Vec<String>, String> {
    let host = state.host.clone();
    run_blocking(move || host.fuzzy_open(&query).map_err(map_err)).await
}

#[tauri::command]
pub async fn git_status(state: State<'_, AppState>) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.git_status().map_err(map_err)).await
}

#[tauri::command]
pub async fn git_diff(state: State<'_, AppState>) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.git_diff().map_err(map_err)).await
}

#[tauri::command]
pub async fn agent_edit_diffs(state: State<'_, AppState>) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.agent_edit_diffs().map_err(map_err)).await
}

#[tauri::command]
pub fn last_edited_path(state: State<'_, AppState>) -> Option<String> {
    state.host.last_edited_path()
}

#[tauri::command]
pub async fn export_transcript(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<String, String> {
    let host = state.host.clone();
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    run_blocking(move || host.export_transcript(id).map_err(map_err)).await
}

#[tauri::command]
pub fn memory_list(
    state: State<'_, AppState>,
    session_id: String,
    scope: grokptah_agent_bridge::MemoryScope,
) -> Result<Vec<serde_json::Value>, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let facts = state.host.memory_list(session_id, scope).map_err(map_err)?;
    Ok(facts
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "id": f.id,
                "text": f.text,
                "tags": f.tags,
                "updated_at": f.updated_at,
            })
        })
        .collect())
}

#[tauri::command]
pub fn memory_remember(
    state: State<'_, AppState>,
    session_id: String,
    scope: grokptah_agent_bridge::MemoryScope,
    text: String,
) -> Result<String, String> {
    let session_id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .memory_remember(session_id, scope, &text)
        .map_err(map_err)
}

#[tauri::command]
pub async fn git_stage_all(state: State<'_, AppState>) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.git_stage_all().map_err(map_err)).await
}

#[tauri::command]
pub async fn git_commit(state: State<'_, AppState>, message: String) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.git_commit(&message).map_err(map_err)).await
}

#[tauri::command]
pub async fn list_worktrees(state: State<'_, AppState>) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.list_worktrees().map_err(map_err)).await
}

#[tauri::command]
pub async fn create_worktree(
    state: State<'_, AppState>,
    path: String,
    branch: Option<String>,
) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || {
        host.create_worktree(&path, branch.as_deref())
            .map_err(map_err)
    })
    .await
}

#[tauri::command]
pub async fn remove_worktree(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let host = state.host.clone();
    run_blocking(move || host.remove_worktree(&path).map_err(map_err)).await
}

#[tauri::command]
pub fn mcp_list(state: State<'_, AppState>) -> Vec<McpServerInfo> {
    state.host.mcp_list()
}

#[tauri::command]
pub fn mcp_project_trust(state: State<'_, AppState>) -> grokptah_agent_bridge::McpProjectTrust {
    state.host.mcp_project_trust()
}

#[tauri::command]
pub fn mcp_set_project_trust(
    state: State<'_, AppState>,
    trusted: bool,
) -> Result<grokptah_agent_bridge::McpProjectTrust, String> {
    state.host.mcp_set_project_trust(trusted).map_err(map_err)
}

#[tauri::command]
pub fn mcp_set_enabled(
    state: State<'_, AppState>,
    name: String,
    enabled: bool,
) -> Result<McpServerInfo, String> {
    state.host.mcp_set_enabled(&name, enabled).map_err(map_err)
}

#[tauri::command]
pub fn mcp_doctor(state: State<'_, AppState>) -> Vec<String> {
    state.host.mcp_doctor()
}

#[tauri::command]
pub fn mcp_add_stdio(
    state: State<'_, AppState>,
    name: String,
    command: String,
    args: Vec<String>,
) -> Result<(), String> {
    state
        .host
        .mcp_add_stdio(&name, &command, args)
        .map_err(map_err)
}

#[tauri::command]
pub fn plugins_list(state: State<'_, AppState>) -> Vec<PluginInfo> {
    state.host.plugins()
}

#[tauri::command]
pub fn plugin_install(state: State<'_, AppState>, id: String) -> Result<PluginInfo, String> {
    state.host.plugin_install(&id).map_err(map_err)
}

#[tauri::command]
pub fn skills_list(state: State<'_, AppState>) -> Vec<SkillInfo> {
    state.host.skills()
}

#[tauri::command]
pub fn hooks_config(state: State<'_, AppState>) -> String {
    state.host.hooks_config()
}

#[tauri::command]
pub fn cancel_subagent(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.host.cancel_subagent(&id).map_err(map_err)
}

#[tauri::command]
pub fn list_agents(state: State<'_, AppState>) -> Vec<grokptah_agent_bridge::AgentDef> {
    state.host.list_agents()
}

#[tauri::command]
pub fn list_personas(state: State<'_, AppState>) -> Vec<grokptah_agent_bridge::PersonaDef> {
    state.host.list_personas()
}

#[tauri::command]
pub fn fleet_observability(state: State<'_, AppState>) -> serde_json::Value {
    state.host.fleet_observability()
}

#[tauri::command]
pub fn subagents_list(state: State<'_, AppState>) -> Vec<SubagentInfo> {
    state.host.subagents()
}

#[tauri::command]
pub fn background_tasks(state: State<'_, AppState>) -> Vec<BackgroundTask> {
    state.host.background_tasks()
}

#[tauri::command]
pub fn cancel_background_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.host.cancel_background_task(&id).map_err(map_err)
}

#[tauri::command]
pub fn schedule_background_task(state: State<'_, AppState>, title: String) -> BackgroundTask {
    state.host.schedule_background_task(title)
}

#[tauri::command]
pub fn settings_snapshot(state: State<'_, AppState>) -> serde_json::Value {
    state.host.settings_snapshot()
}

#[tauri::command]
pub fn set_sandbox(state: State<'_, AppState>, profile: String) {
    state.host.set_sandbox(profile);
}

#[tauri::command]
pub fn set_subagent_isolation(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    state.host.set_subagent_isolation(mode).map_err(map_err)
}

#[tauri::command]
pub fn set_appearance(state: State<'_, AppState>, appearance: String) {
    state.host.set_appearance(appearance);
}

#[tauri::command]
pub fn set_permission_mode(state: State<'_, AppState>, mode: String) {
    state.host.set_permission_mode(mode);
}

#[tauri::command]
pub fn set_allow_deny_rules(state: State<'_, AppState>, allow: Vec<String>, deny: Vec<String>) {
    state.host.set_allow_deny_rules(allow, deny);
}

/// Corporate OpenAI-compatible gateway (#169). Empty api_key leaves existing key.
#[tauri::command]
pub fn set_gateway_config(
    state: State<'_, AppState>,
    provider_id: String,
    base_url: String,
    api_key: Option<String>,
) -> Result<(), String> {
    state
        .host
        .set_gateway_config(provider_id, base_url, api_key)
        .map_err(map_err)
}

#[tauri::command]
// Tauri maps this stable, flat JavaScript payload into named command arguments.
#[allow(clippy::too_many_arguments)]
pub fn upsert_provider_profile(
    state: State<'_, AppState>,
    provider_id: String,
    label: String,
    base_url: String,
    model_id: String,
    deadline_class: ProviderDeadlineClass,
    effort_options: Vec<String>,
    api_key: Option<String>,
) -> Result<(), String> {
    state
        .host
        .upsert_provider_profile(ProviderProfileUpdate {
            provider_id,
            label,
            base_url,
            model_id,
            deadline_class,
            effort_options,
            api_key,
        })
        .map_err(map_err)
}

#[tauri::command]
pub async fn discover_provider_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<Vec<ModelInfo>, String> {
    state
        .host
        .discover_provider_models(&provider_id)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub async fn qualify_provider_model(
    state: State<'_, AppState>,
    provider_id: String,
    model_id: String,
) -> Result<ProviderQualificationReport, String> {
    state
        .host
        .qualify_provider_model(&provider_id, &model_id)
        .await
        .map_err(map_err)
}

#[tauri::command]
pub fn delete_provider_profile(
    state: State<'_, AppState>,
    provider_id: String,
) -> Result<(), String> {
    state
        .host
        .delete_provider_profile(&provider_id)
        .map_err(map_err)
}

#[tauri::command]
pub fn project_rules(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state.host.project_rules().map_err(map_err)
}

#[tauri::command]
pub fn set_plan_mode(
    state: State<'_, AppState>,
    session_id: String,
    enabled: bool,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.set_plan_mode(id, enabled).map_err(map_err)
}

#[tauri::command]
pub async fn accept_plan(state: State<'_, AppState>, session_id: String) -> Result<String, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.accept_plan(id).await.map_err(map_err)
}

#[tauri::command]
pub fn reject_plan(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.reject_plan(id).map_err(map_err)
}

#[tauri::command]
pub fn product_info() -> serde_json::Value {
    serde_json::json!({
        "name": PRODUCT_NAME,
        "bridgeVersion": BRIDGE_VERSION,
        "autoUpdateEnabled": desktop_auto_update_enabled(),
    })
}

#[tauri::command]
pub fn pty_create(state: State<'_, AppState>, cols: u16, rows: u16) -> Result<String, String> {
    state.pty.create(cols, rows).map_err(map_err)
}

#[tauri::command]
pub fn pty_write(state: State<'_, AppState>, id: String, data: String) -> Result<(), String> {
    state.pty.write(&id, data.as_bytes()).map_err(map_err)
}

#[tauri::command]
pub fn pty_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.pty.resize(&id, cols, rows).map_err(map_err)
}

#[tauri::command]
pub fn pty_kill(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.pty.kill(&id).map_err(map_err)
}

#[tauri::command]
pub fn pty_list(state: State<'_, AppState>) -> Vec<String> {
    state.pty.list()
}

#[tauri::command]
pub fn pty_backlog(state: State<'_, AppState>, id: String) -> Result<PtyBacklog, String> {
    state.pty.backlog(&id).map_err(map_err)
}

#[tauri::command]
pub fn pty_create_command(
    state: State<'_, AppState>,
    command: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    state
        .pty
        .create_command(&command, cols, rows)
        .map_err(map_err)
}
