//! Helpers the desktop boundary needs when deciding *which* roots to approve.
//!
//! These live here rather than in the Tauri adapter so they are covered by
//! tests that actually execute: the adapter stays a registration shim with no
//! policy of its own.

use std::path::Path;

use crate::error::SourceViewError;

/// Last two path segments, so a long absolute path still gets a short label.
/// The full path is always shown separately — this is chrome, not identity.
pub fn short_root_label(path: &str) -> String {
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    match parts.len() {
        0 => path.to_string(),
        1 => parts[0].to_string(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// Whether `execution_workspace` is the managed run worktree of
/// `source_workspace`.
///
/// This mirrors the containment the promotion path enforces before it touches
/// Git, so read-only inspection can never reach further than promotion can. A
/// directory that merely sits in the managed path is not enough: it must also
/// carry Git worktree metadata, and the managed root itself never qualifies.
pub fn is_managed_run_worktree(source_workspace: &str, execution_workspace: &str) -> bool {
    let Ok(source) = dunce::canonicalize(source_workspace) else {
        return false;
    };
    let Ok(worktree) = dunce::canonicalize(execution_workspace) else {
        return false;
    };
    let Ok(managed_root) =
        dunce::canonicalize(source.join(".grokptah").join("worktrees").join("runs"))
    else {
        return false;
    };
    managed_root.starts_with(&source)
        && worktree.starts_with(&managed_root)
        && worktree != managed_root
        && Path::new(&worktree).join(".git").exists()
}

/// Format a refusal for the desktop boundary as `code: human sentence`.
///
/// The code is stable and parsed by the frontend; the sentence is for people.
pub fn boundary_message(error: &SourceViewError) -> String {
    format!("{}: {error}", error.code())
}
