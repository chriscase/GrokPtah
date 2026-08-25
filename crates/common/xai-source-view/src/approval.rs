//! Which directories a principal may be offered, and how they are labelled.
//!
//! The managed-worktree test here is deliberately *identical* to the one the
//! promotion path runs before it touches Git. Read-only inspection must never
//! reach a tree that promotion would refuse to promote from: if the two tests
//! could disagree, a reviewer could approve a change by reading one tree while
//! promotion applied another.

use std::path::Path;

/// Last two path segments, for a short label.
///
/// This is chrome. The identity a caller can rely on is the path digest, which
/// is what crosses the process boundary; the absolute path never does.
pub fn short_root_label(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match parts.len() {
        0 => "/".to_string(),
        1 => parts[0].clone(),
        n => format!("{}/{}", parts[n - 2], parts[n - 1]),
    }
}

/// Whether `execution_workspace` is the managed run worktree of
/// `source_workspace`, by the same test promotion applies.
///
/// Mirrors `run_promotion::validate_managed_worktree` +
/// `validate_worktree_path` in the agent bridge:
///
/// 1. all three paths canonicalise;
/// 2. `<source>/.grokptah/worktrees/runs` lies under the source;
/// 3. the candidate lies under that managed directory;
/// 4. the candidate has a `.git` **file** (not a directory — a worktree's
///    `.git` is a pointer file, a clone's is a directory);
/// 5. that file points into a `worktrees` administrative directory.
///
/// Any drift between this and the promotion validator is a defect; the pair is
/// asserted by test against the same fixtures.
pub fn is_managed_run_worktree(source_workspace: &Path, execution_workspace: &Path) -> bool {
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
    if !managed_root.starts_with(&source) || !worktree.starts_with(&managed_root) {
        return false;
    }
    if worktree == managed_root {
        return false;
    }
    is_git_worktree_pointer(&worktree)
}

/// Step 4 and 5 of the promotion test, split out so it can be asserted alone.
pub fn is_git_worktree_pointer(worktree: &Path) -> bool {
    let marker = worktree.join(".git");
    if !marker.is_file() {
        return false;
    }
    match std::fs::read_to_string(&marker) {
        Ok(contents) => contents.contains("worktrees"),
        Err(_) => false,
    }
}
