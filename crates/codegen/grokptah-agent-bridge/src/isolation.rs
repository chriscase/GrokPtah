//! Subagent worktree isolation helpers (#162).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Prepare an isolated cwd for a subagent.
///
/// 1. Prefer `git worktree add --detach` under `.grokptah/worktrees/sub-<id>`.
/// 2. If that fails, **copy** the project tree (excluding nested worktrees) so
///    the child still has the repo — never an empty directory.
pub fn prepare_isolation_cwd(project: &Path, sub_id: &str) -> Result<PathBuf> {
    let wt_root = project.join(".grokptah").join("worktrees");
    std::fs::create_dir_all(&wt_root).context("create worktrees root")?;
    let wt = wt_root.join(format!("sub-{sub_id}"));
    if wt.exists() {
        let _ = std::fs::remove_dir_all(&wt);
    }

    let status = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            wt.to_str().unwrap_or("."),
            "HEAD",
        ])
        .current_dir(project)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if status.map(|s| s.success()).unwrap_or(false) && wt.is_dir() {
        // Sanity: must contain something from the project (e.g. .git file or dir)
        return Ok(wt);
    }

    // Fail soft into full copy — never empty dir "isolation".
    let _ = std::fs::remove_dir_all(&wt);
    copy_project_tree(project, &wt).context("copy project for isolation")?;
    if !wt.is_dir() {
        bail!("isolation cwd missing after copy");
    }
    // Must not be empty
    let has_any = std::fs::read_dir(&wt)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if !has_any {
        bail!("isolation copy produced empty directory");
    }
    Ok(wt)
}

fn copy_project_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        // Skip nested isolation worktrees and target/node_modules noise.
        let rel_s = rel.to_string_lossy();
        if rel_s.starts_with(".grokptah/worktrees")
            || rel_s.starts_with(".grokptah\\worktrees")
            || rel_s.starts_with("target/")
            || rel_s.starts_with("target\\")
            || rel_s.starts_with("node_modules/")
            || rel_s.starts_with("node_modules\\")
        {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_fallback_has_project_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "fn x() {}").unwrap();
        // Force copy path: not a git repo
        let wt = prepare_isolation_cwd(dir.path(), "test-id").unwrap();
        assert!(wt.join("hello.txt").is_file());
        assert!(wt.join("src/lib.rs").is_file());
        // Not empty
        assert!(std::fs::read_to_string(wt.join("hello.txt"))
            .unwrap()
            .contains("hi"));
    }
}
