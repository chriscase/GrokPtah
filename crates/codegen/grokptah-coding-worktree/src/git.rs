//! Git-backed worktree operations for disposable coding sessions.
//!
//! All helpers are scoped to the disposable worktree path or an explicit apply
//! target. The main checkout path is never written by these functions.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::{SessionError, SessionErrorCode, SessionResult};
use crate::patch::{digest_bytes, PatchArtifact};

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
    let untracked_before = list_untracked_paths(worktree_path)?;
    mark_untracked_intent_to_add(worktree_path)?;
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
    let had_untracked = !untracked_before.is_empty();
    let mut all_paths = paths;
    for path in untracked_before {
        if !all_paths.iter().any(|existing| existing == &path) {
            all_paths.push(path);
        }
    }
    if had_untracked && diff.stdout.is_empty() {
        return Err(SessionError::invalid_state(
            "untracked worktree files present but patch capture produced empty diff",
        ));
    }
    PatchArtifact::from_diff(diff.stdout, all_paths)
}

fn mark_untracked_intent_to_add(worktree_path: &Path) -> SessionResult<()> {
    let output = git_in(
        worktree_path,
        &["ls-files", "-o", "--exclude-standard", "-z"],
    )?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!(
                "git ls-files for untracked failed: {}",
                command_error(&output)
            ),
        ));
    }
    for file in parse_null_separated(&output.stdout) {
        let add = git_in(worktree_path, &["add", "-N", &file])?;
        if !add.status.success() {
            return Err(SessionError::new(
                SessionErrorCode::Internal,
                format!(
                    "git add -N for untracked file failed: {}",
                    command_error(&add)
                ),
            ));
        }
    }
    Ok(())
}

fn list_untracked_paths(worktree_path: &Path) -> SessionResult<Vec<String>> {
    let output = git_in(worktree_path, &["ls-files", "-o", "--exclude-standard"])?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git ls-files --others failed: {}", command_error(&output)),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn parse_null_separated(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for part in bytes.split(|byte| *byte == 0) {
        if part.is_empty() {
            continue;
        }
        out.push(String::from_utf8_lossy(part).to_string());
    }
    out
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

pub fn apply_patch(
    target_root: &Path,
    patch: &PatchArtifact,
    expected_digest: &str,
) -> SessionResult<()> {
    assert_target_not_empty(target_root)?;
    let actual = digest_bytes(&patch.bytes);
    if actual != expected_digest {
        return Err(SessionError::patch_digest_mismatch(
            "apply refused: digest of patch bytes does not match operator digest",
        ));
    }
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
    if worktree_path.exists() {
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
    }
    prune_worktrees(repo_root)?;
    Ok(())
}

pub fn prune_worktrees(repo_root: &Path) -> SessionResult<()> {
    let output = git_in(repo_root, &["worktree", "prune", "--expire", "now"])?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git worktree prune failed: {}", command_error(&output)),
        ));
    }
    Ok(())
}

pub fn listed_worktree_paths(repo_root: &Path) -> SessionResult<Vec<PathBuf>> {
    let output = git_in(repo_root, &["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        return Err(SessionError::new(
            SessionErrorCode::Internal,
            format!("git worktree list failed: {}", command_error(&output)),
        ));
    }
    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            paths.push(PathBuf::from(path.trim()));
        }
    }
    Ok(paths)
}

pub fn worktree_is_listed(repo_root: &Path, worktree_path: &Path) -> SessionResult<bool> {
    for listed in listed_worktree_paths(repo_root)? {
        if paths_refer_to_same_worktree(&listed, worktree_path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn paths_refer_to_same_worktree(left: &Path, right: &Path) -> SessionResult<bool> {
    if left == right {
        return Ok(true);
    }
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(a), Ok(b)) => Ok(a == b),
        _ => Ok(left.to_string_lossy() == right.to_string_lossy()),
    }
}

pub fn assert_worktree_removed(repo_root: &Path, worktree_path: &Path) -> SessionResult<()> {
    prune_worktrees(repo_root)?;
    if worktree_path.exists() {
        return Err(SessionError::worktree_still_active(format!(
            "worktree path still exists: {}",
            worktree_path.display()
        )));
    }
    if worktree_is_listed(repo_root, worktree_path)? {
        return Err(SessionError::worktree_still_active(
            "worktree still registered in git worktree list",
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
