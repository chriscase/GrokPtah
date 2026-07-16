use grokptah_agent_bridge::{
    desktop_auto_update_enabled, AuthState, BackgroundTask, EffortLevel, McpServerInfo, ModelInfo,
    PermissionDecision, PluginInfo, SessionSummary, SkillInfo, SubagentInfo, TranscriptEntry,
    BRIDGE_VERSION, PRODUCT_NAME,
};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use crate::AppState;

fn map_err(e: impl ToString) -> String {
    e.to_string()
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
pub fn session_load(state: State<'_, AppState>, id: String) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&id).map_err(map_err)?;
    state.host.session_load(id).map_err(map_err)
}

#[tauri::command]
pub fn session_list(state: State<'_, AppState>) -> Vec<SessionSummary> {
    state.host.list_sessions()
}

#[tauri::command]
pub async fn session_prompt(
    state: State<'_, AppState>,
    session_id: String,
    prompt: String,
) -> Result<String, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_prompt(id, prompt).await.map_err(map_err)
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
pub fn session_transcript(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<TranscriptEntry>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.session_transcript(id).map_err(map_err)
}

#[tauri::command]
pub fn session_fork(state: State<'_, AppState>, source_id: String) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&source_id).map_err(map_err)?;
    state.host.fork_session(id).map_err(map_err)
}

#[tauri::command]
pub fn session_rewind(
    state: State<'_, AppState>,
    session_id: String,
    keep_messages: usize,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state
        .host
        .rewind_session(id, keep_messages)
        .map_err(map_err)
}

#[tauri::command]
pub fn session_compact(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionSummary, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.compact_session(id).map_err(map_err)
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
pub fn file_tree(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    state.host.file_tree().map_err(map_err)
}

#[tauri::command]
pub fn fuzzy_open(state: State<'_, AppState>, query: String) -> Result<Vec<String>, String> {
    state.host.fuzzy_open(&query).map_err(map_err)
}

#[tauri::command]
pub fn git_status(state: State<'_, AppState>) -> Result<String, String> {
    state.host.git_status().map_err(map_err)
}

#[tauri::command]
pub fn git_diff(state: State<'_, AppState>) -> Result<String, String> {
    state.host.git_diff().map_err(map_err)
}

#[tauri::command]
pub fn agent_edit_diffs(state: State<'_, AppState>) -> Result<String, String> {
    state.host.agent_edit_diffs().map_err(map_err)
}

#[tauri::command]
pub fn git_stage_all(state: State<'_, AppState>) -> Result<String, String> {
    state.host.git_stage_all().map_err(map_err)
}

#[tauri::command]
pub fn git_commit(state: State<'_, AppState>, message: String) -> Result<String, String> {
    state.host.git_commit(&message).map_err(map_err)
}

#[tauri::command]
pub fn list_worktrees(state: State<'_, AppState>) -> Result<String, String> {
    state.host.list_worktrees().map_err(map_err)
}

#[tauri::command]
pub fn mcp_list(state: State<'_, AppState>) -> Vec<McpServerInfo> {
    state.host.mcp_list()
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
pub fn schedule_background_task(
    state: State<'_, AppState>,
    title: String,
) -> BackgroundTask {
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
pub fn set_appearance(state: State<'_, AppState>, appearance: String) {
    state.host.set_appearance(appearance);
}

#[tauri::command]
pub fn set_permission_mode(state: State<'_, AppState>, mode: String) {
    state.host.set_permission_mode(mode);
}

#[tauri::command]
pub fn set_allow_deny_rules(
    state: State<'_, AppState>,
    allow: Vec<String>,
    deny: Vec<String>,
) {
    state.host.set_allow_deny_rules(allow, deny);
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
pub fn accept_plan(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    state.host.accept_plan(id).map_err(map_err)
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
pub fn pty_backlog(state: State<'_, AppState>, id: String) -> Result<String, String> {
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
