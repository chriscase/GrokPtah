//! Bounded cleanup for agent-managed worktrees (#166).
//!
//! Cleanup is deliberately fail-closed. Only direct children of the exact
//! `<project>/.grokptah/worktrees` directory are candidates, and active paths
//! supplied by the host are never removed.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(7 * 86400);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GcReport {
    pub scanned: usize,
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
    pub dry_run: bool,
}

pub fn candidates_older_than(root: &Path, max_age: Duration, now: SystemTime) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        let modified = meta.modified().ok().or_else(|| meta.created().ok());
        let Some(m) = modified else { continue };
        let Ok(age) = now.duration_since(m) else {
            continue;
        };
        if age >= max_age {
            out.push(p);
        }
    }
    out
}

/// Conservative compatibility entry point. Git worktrees require the host's
/// protected-path-aware entry so callers cannot delete Git metadata blindly.
pub fn gc_worktrees(root: &Path, max_age: Duration, dry_run: bool) -> GcReport {
    gc_impl(root, max_age, dry_run, &[], false)
}

pub fn gc_worktrees_with_protected(
    root: &Path,
    max_age: Duration,
    dry_run: bool,
    protected: &[PathBuf],
) -> GcReport {
    gc_impl(root, max_age, dry_run, protected, true)
}

fn gc_impl(
    root: &Path,
    max_age: Duration,
    dry_run: bool,
    protected: &[PathBuf],
    allow_git_worktrees: bool,
) -> GcReport {
    let mut report = GcReport {
        scanned: 0,
        removed: Vec::new(),
        skipped: Vec::new(),
        dry_run,
    };
    let root = match exact_managed_root(root) {
        Ok(root) => root,
        Err(error) => {
            report.skipped.push((root.to_path_buf(), error));
            return report;
        }
    };
    let protected: Vec<PathBuf> = protected
        .iter()
        .filter_map(|p| dunce::canonicalize(p).ok())
        .collect();
    let candidates = candidates_older_than(&root, max_age, SystemTime::now());
    report.scanned = candidates.len();
    for candidate in candidates {
        if protected.iter().any(|path| {
            path == &candidate || path.starts_with(&candidate) || candidate.starts_with(path)
        }) {
            report
                .skipped
                .push((candidate, "active managed path".into()));
            continue;
        }
        let name = candidate.file_name().and_then(|value| value.to_str());
        if !name.is_some_and(|name| name.starts_with("sub-") || name.starts_with("run-")) {
            report
                .skipped
                .push((candidate, "unknown managed worktree name".into()));
            continue;
        }
        let git_worktree = candidate.join(".git").is_file();
        if git_worktree && !allow_git_worktrees {
            report.skipped.push((
                candidate,
                "Git worktree requires protected-path-aware cleanup".into(),
            ));
            continue;
        }
        if dry_run {
            report.removed.push(candidate);
            continue;
        }
        let result = if git_worktree {
            let source = root
                .parent()
                .and_then(Path::parent)
                .expect("validated managed root has project parent");
            match Command::new("git")
                .args(["worktree", "remove", "--force", "--"])
                .arg(&candidate)
                .current_dir(source)
                .output()
            {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(format!(
                    "git worktree remove failed: {}",
                    command_error(&output)
                )),
                Err(error) => Err(format!("git worktree remove unavailable: {error}")),
            }
        } else {
            std::fs::remove_dir_all(&candidate).map_err(|error| error.to_string())
        };
        match result {
            Ok(()) => report.removed.push(candidate),
            Err(error) => report.skipped.push((candidate, error)),
        }
    }
    report
}

fn exact_managed_root(root: &Path) -> Result<PathBuf, String> {
    let root = dunce::canonicalize(root).map_err(|error| format!("canonicalize root: {error}"))?;
    if !root.is_dir()
        || root.file_name().and_then(|value| value.to_str()) != Some("worktrees")
        || root
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            != Some(".grokptah")
    {
        return Err("path is not the exact .grokptah/worktrees root".into());
    }
    let project = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "managed root has no project parent".to_string())?;
    if !project.join(".git").exists() {
        return Err("managed root has no Git project parent".into());
    }
    Ok(root)
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn managed_root(dir: &Path) -> PathBuf {
        let root = dir.join(".grokptah").join("worktrees");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(dir.join(".git"), "gitdir: test").unwrap();
        root
    }

    #[test]
    fn finds_old_dirs_without_treating_future_timestamps_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".grokptah").join("worktrees");
        std::fs::create_dir_all(root.join("sub-old")).unwrap();
        let c = candidates_older_than(&root, Duration::from_secs(0), SystemTime::now());
        assert!(c.iter().any(|p| p.ends_with("sub-old")));
    }

    #[test]
    fn refuses_broad_or_non_git_roots() {
        let dir = tempfile::tempdir().unwrap();
        let report = gc_worktrees(dir.path(), Duration::from_secs(0), true);
        assert!(report.removed.is_empty());
        assert!(report
            .skipped
            .iter()
            .any(|(_, reason)| reason.contains("exact .grokptah/worktrees")));
    }

    #[test]
    fn removes_only_named_fallback_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = managed_root(dir.path());
        std::fs::create_dir_all(root.join("sub-old")).unwrap();
        std::fs::create_dir_all(root.join("manual")).unwrap();
        let report = gc_worktrees(&root, Duration::from_secs(0), false);
        assert!(report.removed.iter().any(|p| p.ends_with("sub-old")));
        assert!(root.join("manual").exists());
    }

    #[test]
    fn protects_active_paths_and_git_worktrees_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let root = managed_root(dir.path());
        let active = root.join("sub-active");
        std::fs::create_dir_all(&active).unwrap();
        let report = gc_worktrees_with_protected(
            &root,
            Duration::from_secs(0),
            false,
            std::slice::from_ref(&active),
        );
        assert!(active.exists());
        let active = dunce::canonicalize(active).unwrap();
        assert!(report
            .skipped
            .iter()
            .any(|(path, reason)| path == &active && reason == "active managed path"));
    }

    #[test]
    fn removes_registered_git_worktrees_through_git() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        let root = project.join(".grokptah").join("worktrees");
        std::fs::create_dir_all(&root).unwrap();
        let run = root.join("run-old");
        let run_git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(project)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                command_error(&output)
            );
        };
        run_git(&["init", "-q"]);
        std::fs::write(project.join("README"), "fixture").unwrap();
        run_git(&["add", "README"]);
        run_git(&[
            "-c",
            "user.name=GrokPtah test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "initial",
        ]);
        // The path is passed separately so the test exercises the same direct
        // child validation as production cleanup.
        let output = std::process::Command::new("git")
            .args(["worktree", "add", "--detach", "-q"])
            .arg(&run)
            .arg("HEAD")
            .current_dir(project)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));

        let report = gc_worktrees_with_protected(&root, Duration::from_secs(0), false, &[]);
        assert!(report.removed.iter().any(|path| path.ends_with("run-old")));
        assert!(!run.exists());
    }
}
