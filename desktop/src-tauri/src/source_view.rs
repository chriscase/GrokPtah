//! Read-only source inspection commands.
//!
//! Clicking a file used to compose a `read <path>` prompt for the model.
//! These commands replace that with a direct read: the frontend names an
//! approved boundary and a path, and gets bytes back. Nothing here writes,
//! spawns, or authenticates — all policy lives in `xai-source-view`, which
//! has no Tauri, network, or credential surface of its own.

use std::path::Path;

use grokptah_agent_bridge::{AgentHostHandle, RunExecutionMode};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;
use xai_source_view::{
    boundary_message, is_managed_run_worktree, short_root_label, RootKind, SourceDocument,
    SourceLimits, SourceRoot, SourceRootRegistry,
};

use crate::AppState;

/// One approved boundary, as the identity strip renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRootInfo {
    pub id: String,
    /// `"workspace"` or `"isolated_worktree"`.
    pub kind: String,
    pub label: String,
    /// Exact canonical path. The UI shows this verbatim; it is the only
    /// honest answer to "which tree am I looking at".
    pub path: String,
    pub run_id: Option<String>,
}

impl From<SourceRoot> for SourceRootInfo {
    fn from(root: SourceRoot) -> Self {
        Self {
            id: root.id,
            kind: match root.kind {
                RootKind::Workspace => "workspace".into(),
                RootKind::IsolatedWorktree => "isolated_worktree".into(),
            },
            label: root.label,
            path: root.path.display().to_string(),
            run_id: root.run_id,
        }
    }
}

/// Build the approved set for this request. Roots are derived fresh every
/// call from live host state, so revoking a workspace or discarding a run
/// removes its boundary immediately rather than at the next restart.
///
/// Blocking: canonicalises paths and reads the orchestration store, so this
/// always runs off the UI thread (#137).
fn build_registry(host: &AgentHostHandle, session_id: Option<Uuid>) -> SourceRootRegistry {
    let mut registry = SourceRootRegistry::new();

    let workspace = host.workspace_ui_state();
    if let Some(cwd) = workspace.project_cwd.as_deref() {
        if let Ok(root) = SourceRoot::workspace(cwd, short_root_label(cwd)) {
            registry.approve(root);
        }
    }

    let Some(session_id) = session_id else {
        return registry;
    };

    // A session may be pinned to a directory other than the open project.
    if let Ok(session) = host.session_load(session_id) {
        if !session.cwd.is_empty() {
            if let Ok(root) = SourceRoot::workspace(&session.cwd, short_root_label(&session.cwd)) {
                registry.approve(root);
            }
        }
    }

    for run in host.list_session_runs(session_id).unwrap_or_default() {
        let Some(execution) = run.execution.as_ref() else {
            continue;
        };
        if execution.mode != RunExecutionMode::IsolatedWorktree {
            continue;
        }
        if !is_managed_run_worktree(&execution.source_workspace, &execution.execution_workspace) {
            continue;
        }
        if !Path::new(&execution.execution_workspace).is_dir() {
            continue;
        }
        let label = format!("run {} worktree", &run.run_id);
        if let Ok(root) =
            SourceRoot::isolated_worktree(&execution.execution_workspace, label, &run.run_id)
        {
            registry.approve(root);
        }
    }

    registry
}

fn parse_session(session_id: Option<String>) -> Result<Option<Uuid>, String> {
    match session_id {
        Some(raw) => Uuid::parse_str(&raw).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

async fn run_blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("blocking task join: {e}"))?
}

/// List the boundaries this session may inspect: the approved workspace and
/// the managed worktree of each isolated run it owns.
#[tauri::command]
pub async fn source_view_roots(
    state: State<'_, AppState>,
    session_id: Option<String>,
) -> Result<Vec<SourceRootInfo>, String> {
    let host = state.host.clone();
    let session = parse_session(session_id)?;
    run_blocking(move || {
        Ok(build_registry(&host, session)
            .roots()
            .into_iter()
            .map(SourceRootInfo::from)
            .collect())
    })
    .await
}

/// Read one file inside one approved boundary. Refuses anything that leaves
/// the boundary, crosses a symlink, or is not a regular file.
#[tauri::command]
pub async fn source_view_open(
    state: State<'_, AppState>,
    root_id: String,
    path: String,
    session_id: Option<String>,
    max_bytes: Option<u64>,
    max_lines: Option<usize>,
    max_line_chars: Option<usize>,
) -> Result<SourceDocument, String> {
    let host = state.host.clone();
    let session = parse_session(session_id)?;
    let limits = SourceLimits::clamped(max_bytes, max_lines, max_line_chars);
    run_blocking(move || {
        let registry = build_registry(&host, session);
        xai_source_view::open_in_registry(&registry, &root_id, &path, limits)
            .map_err(|error| boundary_message(&error))
    })
    .await
}
