//! GrokPtah Tauri backend — thin adapters over grokptah-agent-bridge.

mod commands;
mod computer_use;
mod event_forward;
mod pty_host;

use std::sync::Mutex;

use grokptah_agent_bridge::{start_control_from_env, AgentHost, ControlServerHandle, HostConfig};
use tauri::Manager;

pub struct AppState {
    pub host: grokptah_agent_bridge::AgentHostHandle,
    pub pty: pty_host::PtyHub,
    /// Loopback MCP control plane (#196); optional when token not configured.
    pub control: Mutex<Option<ControlServerHandle>>,
    pub computer_use: std::sync::Arc<computer_use::DesktopComputerUse>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host = AgentHost::create(HostConfig::default());
    // Prefer fan-out subscribe so MCP can also attach; fall back to take for compat.
    let event_rx = host.subscribe_events();
    let _primary = host.take_event_receiver();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            host: host.clone(),
            pty: pty_host::PtyHub::new(),
            control: Mutex::new(None),
            computer_use: std::sync::Arc::new(computer_use::DesktopComputerUse::new(&host)),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            app.state::<AppState>().pty.set_app(handle.clone());
            host.set_computer_run_controller(app.state::<AppState>().computer_use.clone());
            host.set_computer_run_agent_controller(app.state::<AppState>().computer_use.clone());
            let _ = host.start();
            event_forward::spawn_event_forwarder(handle, event_rx);

            // Start authenticated loopback MCP control plane when token is set.
            let host2 = host.clone();
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(srv) = start_embedded_control(host2).await {
                    eprintln!(
                        "[grokptah] MCP control plane listening on http://{}/mcp",
                        srv.addr
                    );
                    if let Some(state) = app_handle.try_state::<AppState>() {
                        *state.control.lock().unwrap() = Some(srv);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_start,
            commands::agent_stop,
            commands::agent_status,
            commands::capability_contract,
            commands::persistent_agent_list,
            commands::persistent_agent_get,
            commands::persistent_agent_resume_plan,
            commands::persistent_agent_resume,
            commands::computer_use_status,
            commands::computer_use_request_permission,
            commands::computer_use_list_targets,
            commands::computer_use_observe_once,
            commands::computer_use_cockpit_snapshot,
            commands::computer_use_cockpit_agent_eligibility,
            commands::computer_use_cockpit_qualify_agent,
            commands::computer_use_cockpit_propose_agent_action,
            commands::computer_use_cockpit_cancel_agent,
            commands::computer_use_cockpit_start_simulator,
            commands::computer_use_cockpit_start_native,
            commands::computer_use_cockpit_refresh,
            commands::computer_use_cockpit_stage_action,
            commands::computer_use_cockpit_approve,
            commands::computer_use_cockpit_discard_approval,
            commands::computer_use_cockpit_pause,
            commands::computer_use_cockpit_take_over,
            commands::computer_use_cockpit_stop,
            commands::set_project_cwd,
            commands::pick_project_folder,
            commands::session_new,
            commands::session_new_kind,
            commands::session_list_by_kind,
            commands::search_sessions,
            commands::session_load,
            commands::session_list,
            commands::session_list_archived,
            commands::session_list_all,
            commands::session_rename,
            commands::session_delete,
            commands::session_archive,
            commands::session_set_folder,
            commands::session_set_cwd,
            commands::session_set_execution_mode,
            commands::pick_session_folder,
            commands::session_set_tags,
            commands::session_list_folders,
            commands::session_list_tags,
            commands::workspace_state,
            commands::set_open_tabs,
            commands::session_prompt,
            commands::session_queue_list,
            commands::session_queue_add,
            commands::session_queue_edit,
            commands::session_queue_remove,
            commands::session_queue_clear,
            commands::session_queue_restore_drain,
            commands::session_queue_move,
            commands::session_queue_take_next,
            commands::session_queue_run_next,
            commands::session_queue_steer_entry,
            commands::session_steer,
            commands::session_cancel,
            commands::session_transcript,
            commands::session_completion_history,
            commands::run_list,
            commands::run_get,
            commands::run_events,
            commands::run_review,
            commands::run_approve,
            commands::run_promote,
            commands::run_discard,
            commands::run_submit,
            commands::run_retry,
            commands::run_steer,
            commands::run_cancel,
            commands::session_fork,
            commands::session_rewind,
            commands::session_compact,
            commands::permission_respond,
            commands::list_models,
            commands::set_model,
            commands::set_effort,
            commands::set_always_approve,
            commands::auth_state,
            commands::grok_account_facts,
            commands::sign_in_local,
            commands::sign_out,
            commands::auth_set_api_key,
            commands::auth_open_login,
            commands::file_tree,
            commands::fuzzy_open,
            commands::git_status,
            commands::git_diff,
            commands::git_stage_all,
            commands::git_commit,
            commands::list_worktrees,
            commands::create_worktree,
            commands::remove_worktree,
            commands::agent_edit_diffs,
            commands::last_edited_path,
            commands::export_transcript,
            commands::memory_list,
            commands::memory_remember,
            commands::mcp_list,
            commands::mcp_project_trust,
            commands::mcp_set_project_trust,
            commands::mcp_set_enabled,
            commands::mcp_doctor,
            commands::mcp_add_stdio,
            commands::plugins_list,
            commands::plugin_install,
            commands::skills_list,
            commands::hooks_config,
            commands::subagents_list,
            commands::list_agents,
            commands::list_personas,
            commands::fleet_observability,
            commands::cancel_subagent,
            commands::background_tasks,
            commands::cancel_background_task,
            commands::schedule_background_task,
            commands::settings_snapshot,
            commands::set_sandbox,
            commands::set_subagent_isolation,
            commands::set_appearance,
            commands::set_permission_mode,
            commands::set_allow_deny_rules,
            commands::set_gateway_config,
            commands::upsert_provider_profile,
            commands::discover_provider_models,
            commands::qualify_provider_model,
            commands::delete_provider_profile,
            commands::project_rules,
            commands::set_plan_mode,
            commands::accept_plan,
            commands::reject_plan,
            commands::product_info,
            commands::pty_create,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_kill,
            commands::pty_list,
            commands::pty_backlog,
            commands::pty_create_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running GrokPtah");
}

/// Start control plane when `GROKPTAH_CONTROL_TOKEN` is set (loopback only).
/// Delegates to the shared bridge bootstrap used by the live coordinator smoke.
async fn start_embedded_control(
    host: grokptah_agent_bridge::AgentHostHandle,
) -> Option<ControlServerHandle> {
    start_control_from_env(host).await
}
