//! GrokPtah Tauri backend — thin adapters over grokptah-agent-bridge.

mod commands;
mod event_forward;
mod pty_host;

use grokptah_agent_bridge::{AgentHost, HostConfig};
use tauri::Manager;

pub struct AppState {
    pub host: grokptah_agent_bridge::AgentHostHandle,
    pub pty: pty_host::PtyHub,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host = AgentHost::create(HostConfig::default());
    let event_rx = host.take_event_receiver();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            host: host.clone(),
            pty: pty_host::PtyHub::new(),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            app.state::<AppState>().pty.set_app(handle.clone());
            let _ = host.start();
            if let Some(rx) = event_rx {
                event_forward::spawn_event_forwarder(handle, rx);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_start,
            commands::agent_stop,
            commands::agent_status,
            commands::set_project_cwd,
            commands::pick_project_folder,
            commands::session_new,
            commands::session_load,
            commands::session_list,
            commands::workspace_state,
            commands::set_open_tabs,
            commands::session_prompt,
            commands::session_cancel,
            commands::session_transcript,
            commands::session_fork,
            commands::session_rewind,
            commands::session_compact,
            commands::permission_respond,
            commands::list_models,
            commands::set_model,
            commands::set_effort,
            commands::set_always_approve,
            commands::auth_state,
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
            commands::agent_edit_diffs,
            commands::mcp_list,
            commands::mcp_set_enabled,
            commands::mcp_doctor,
            commands::mcp_add_stdio,
            commands::plugins_list,
            commands::plugin_install,
            commands::skills_list,
            commands::hooks_config,
            commands::subagents_list,
            commands::background_tasks,
            commands::cancel_background_task,
            commands::schedule_background_task,
            commands::settings_snapshot,
            commands::set_sandbox,
            commands::set_appearance,
            commands::set_permission_mode,
            commands::set_allow_deny_rules,
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
