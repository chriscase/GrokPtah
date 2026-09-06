//! Git-backed worktree operations for disposable coding sessions.
//!
//! All helpers are scoped to the disposable worktree path or an explicit apply
//! target. The main checkout path is never written by these functions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::{SessionError, SessionErrorCode, SessionResult};
use crate::patch::PatchArtifact;

pub fn resolve_sha(repo_root: &Path, base_sha: &str) -> SessionResult<String> {
    let output = git_in(repo_root, &["rev-parse", "--verify", base_sha])?;
    if !output.status.success() {
        return Err(SessionError::invalid_state(format!(
            "base SHA not found: {}",
            command_error(&output)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    base_sha: &str,
) -> SessionResult<()> {
    if worktree_path.exists() {
        let _ = remove_worktree(repo_root, worktree_path);
        let _ = std::fs::remove_dir_all(worktree_path);
    }
    let output = git_in(
        repo_root,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().unwrap_or("."),
            base_sha,
        ],
    )?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git worktree add failed: {}", command_error(&output)),
        ));
    }
    if !worktree_path.is_dir() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            "worktree path missing after git worktree add",
        ));
    }
    Ok(())
}

pub fn capture_patch(worktree_path: &Path, base_sha: &str) -> SessionResult<PatchArtifact> {
    let diff = git_in(
        worktree_path,
        &[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            base_sha,
            "--",
        ],
    )?;
    if !diff.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git diff failed: {}", command_error(&diff)),
        ));
    }
    let paths = list_changed_paths(worktree_path, base_sha)?;
    PatchArtifact::from_diff(diff.stdout, paths)
}

fn list_changed_paths(worktree_path: &Path, base_sha: &str) -> SessionResult<Vec<String>> {
    let output = git_in(worktree_path, &["diff", "--name-only", base_sha, "--"])?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git diff --name-only failed: {}", command_error(&output)),
        ));
    }
    let paths = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Ok(paths)
}

pub fn apply_patch(target_root: &Path, patch: &PatchArtifact) -> SessionResult<()> {
    assert_target_not_empty(target_root)?;
    let mut child = Command::new("git")
        .args(["apply", "--binary", "--whitespace=nowarn", "-"])
        .current_dir(target_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| SessionError::apply_failed(format!("spawn git apply: {error}")))?;
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&patch.bytes)
        .map_err(|error| SessionError::apply_failed(format!("write patch: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| SessionError::apply_failed(format!("wait git apply: {error}")))?;
    if !output.status.success() {
        return Err(SessionError::apply_failed(command_error(&output)));
    }
    Ok(())
}

pub fn remove_worktree(repo_root: &Path, worktree_path: &Path) -> SessionResult<()> {
    if !worktree_path.exists() {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["worktree", "remove", "--force", "--"])
        .arg(worktree_path)
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("git worktree remove unavailable: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git worktree remove failed: {}", command_error(&output)),
        ));
    }
    Ok(())
}

pub fn worktree_is_registered(repo_root: &Path, worktree_path: &Path) -> SessionResult<bool> {
    if !worktree_path.exists() {
        return Ok(false);
    }
    let canonical = dunce::canonicalize(worktree_path).map_err(|error| {
        SessionError::new(
            SessionErrorCode::Internal,
            format!("canonicalize worktree: {error}"),
        )
    })?;
    let output = git_in(repo_root, &["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git worktree list failed: {}", command_error(&output)),
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.starts_with("worktree ") {
            let path = line.trim_start_matches("worktree ").trim();
            if let Ok(path) = dunce::canonicalize(path) {
                if path == canonical {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub fn assert_worktree_removed(repo_root: &Path, worktree_path: &Path) -> SessionResult<()> {
    if worktree_path.exists() {
        return Err(SessionError::worktree_still_active(format!(
            "worktree path still exists: {}",
            worktree_path.display()
        )));
    }
    if worktree_is_registered(repo_root, worktree_path)? {
        return Err(SessionError::worktree_still_active(
            "worktree still registered with git",
        ));
    }
    Ok(())
}

pub fn managed_worktree_path(repo_root: &Path, session_id: &str) -> PathBuf {
    repo_root
        .join(".grokptah")
        .join("worktrees")
        .join(format!("run-{session_id}"))
}

pub fn assert_path_not_main_checkout(
    main_checkout: &Path,
    target: &Path,
    op: &str,
) -> SessionResult<()> {
    let main = canonical_or_same(main_checkout)?;
    let candidate = canonical_or_same(target)?;
    if main == candidate {
        return Err(SessionError::main_checkout_protected(format!(
            "{op} cannot target the protected main checkout"
        )));
    }
    Ok(())
}

fn canonical_or_same(path: &Path) -> SessionResult<PathBuf> {
    Ok(dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn assert_target_not_empty(target_root: &Path) -> SessionResult<()> {
    if !target_root.is_dir() {
        return Err(SessionError::apply_failed(
            "apply target is not a directory",
        ));
    }
    Ok(())
}

fn git_in(repo_root: &Path, args: &[&str]) -> SessionResult<Output> {
    Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("git {:?} unavailable: {error}", args),
            )
        })
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    }
}
