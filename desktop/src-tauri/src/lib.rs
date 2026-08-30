//! GrokPtah Tauri backend — thin adapters over grokptah-agent-bridge.

mod commands;
mod computer_use;
mod event_forward;
mod help;
mod pty_host;
mod remote_public_run;
mod remote_service;

use anyhow::Context;
use grokptah_agent_bridge::{
    start_control_from_env, AgentHost, ControlServerHandle, HostConfig, HostRuntime,
};
use tauri::Manager;

pub struct AppState {
    /// Cloneable *request handle* used by every command. It carries no process
    /// authority of its own and fails closed after shutdown (#455).
    pub host: grokptah_agent_bridge::AgentHostHandle,
    /// The single non-cloneable owner of the process instance lock and of the
    /// task supervisor. Held in app state so it outlives setup and so exit can
    /// run the ordered shutdown (#455).
    pub runtime: HostRuntime,
    pub pty: pty_host::PtyHub,
    pub computer_use: computer_use::DesktopComputerUse,
    pub remote_service: std::sync::Arc<remote_service::RemoteServiceState>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(error) = run_inner() {
        eprintln!("[grokptah] desktop startup refused: {error:#}");
    }
}

fn run_inner() -> anyhow::Result<()> {
    let runtime =
        AgentHost::create(HostConfig::default()).context("acquire the GrokPtah instance lock")?;
    // Prefer fan-out subscribe so MCP can also attach; fall back to take for compat.
    let event_rx = runtime.subscribe_events();
    let _primary = runtime.take_event_receiver();
    let host = runtime.handle();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            host: host.clone(),
            runtime,
            pty: pty_host::PtyHub::new(),
            computer_use: computer_use::DesktopComputerUse::new(&host),
            remote_service: remote_service::RemoteServiceState::new(),
        })
        .manage(help::HelpState::new())
        .setup(move |app| {
            let handle = app.handle().clone();
            app.state::<AppState>().pty.set_app(handle.clone());
            let _ = host.start();
            event_forward::spawn_event_forwarder(handle, event_rx);

            // Start authenticated loopback MCP control plane when token is set.
            //
            // The bootstrap is *tracked* on the host's shutdown join barrier
            // even though it runs on the Tauri async runtime (#455). Without
            // that, an exit racing this task could finish its ordered shutdown,
            // release the instance lock, and only then have the bootstrap
            // publish a live listener into `AppState.control` — serving a
            // closed runtime whose home another process now owns.
            let host2 = host.clone();
            let app_handle = app.handle().clone();
            let bootstrap = async move {
                let Some(srv) = start_embedded_control(host2).await else {
                    return;
                };
                let addr = srv.addr;
                let Some(state) = app_handle.try_state::<AppState>() else {
                    srv.stop();
                    return;
                };
                // The runtime is the authority on whether a control plane may
                // still be adopted. A refusal hands the server back so the
                // listener we just bound is stopped rather than orphaned.
                match state.runtime.attach_control_server(srv) {
                    Ok(()) => {
                        eprintln!("[grokptah] MCP control plane listening on http://{addr}/mcp");
                    }
                    Err(rejected) => {
                        eprintln!(
                            "[grokptah] MCP control plane bootstrap refused: host runtime is {}",
                            rejected.phase.label()
                        );
                        rejected.server.stop();
                    }
                }
            };
            match host.track_supervised("bootstrapping the MCP control plane", bootstrap) {
                Ok(tracked) => {
                    tauri::async_runtime::spawn(tracked);
                }
                Err(error) => {
                    eprintln!("[grokptah] MCP control plane bootstrap refused: {error:#}");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            help::help_ask,
            help::help_follow,
            help::help_cancel,
            help::help_bounds,
            help::help_visible_corpus,
            help::help_session,
            commands::agent_start,
            commands::agent_stop,
            commands::agent_status,
            commands::remote_service_connect,
            commands::remote_service_disconnect,
            commands::remote_service_status,
            commands::remote_service_session_list,
            commands::remote_service_session_create,
            commands::remote_service_task_submit,
            commands::remote_service_run_list,
            commands::remote_service_public_run_list,
            commands::remote_service_work_list,
            commands::remote_service_work_get,
            commands::remote_service_work_create,
            commands::remote_service_work_assign,
            commands::remote_service_work_retry,
            commands::remote_service_work_approve,
            commands::remote_service_work_cancel,
            commands::work_list,
            commands::work_get,
            commands::work_create,
            commands::work_assign,
            commands::work_retry,
            commands::work_approve,
            commands::work_cancel,
            commands::routine_list,
            commands::routine_get,
            commands::routine_create,
            commands::routine_set_lifecycle,
            commands::routine_fire,
            commands::remote_service_routine_list,
            commands::remote_service_routine_get,
            commands::remote_service_routine_create,
            commands::remote_service_routine_set_lifecycle,
            commands::remote_service_routine_fire,
            commands::remote_service_run_get,
            commands::remote_service_public_run_get,
            commands::remote_service_run_events,
            commands::remote_service_run_steer,
            commands::remote_service_run_cancel,
            commands::remote_service_watch_runs,
            commands::persistent_agent_list,
            commands::persistent_agent_get,
            commands::persistent_agent_set_managed_execution,
            commands::persistent_agent_attach_session,
            commands::lane_list,
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
            commands::session_inspect,
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
        .build(tauri::generate_context!())
        .context("build the GrokPtah desktop runtime")?;
    app.run(|app_handle, event| {
        // Ordered shutdown on exit (#455): stop and join the control plane
        // first, then let the runtime cancel and join every supervised
        // task, flush durable state, and release the single-instance lock
        // exactly once. Without this the next launch could find
        // `.instance.lock` still held.
        if matches!(event, tauri::RunEvent::Exit) {
            let Some(state) = app_handle.try_state::<AppState>() else {
                return;
            };
            // The runtime owns the control plane, so one ordered shutdown
            // stops HTTP/SSE acceptance, joins every supervised task
            // (including a bootstrap still in flight), flushes durable
            // state and releases the instance lock exactly once.
            let report = tauri::async_runtime::block_on(state.runtime.shutdown());
            eprintln!("[grokptah] host shutdown: {}", report.operator_summary());
        }
    });
    Ok(())
}

/// Start control plane when `GROKPTAH_CONTROL_TOKEN` is set (loopback only).
/// Delegates to the shared bridge bootstrap used by the live coordinator smoke.
async fn start_embedded_control(
    host: grokptah_agent_bridge::AgentHostHandle,
) -> Option<ControlServerHandle> {
    start_control_from_env(host).await
}
