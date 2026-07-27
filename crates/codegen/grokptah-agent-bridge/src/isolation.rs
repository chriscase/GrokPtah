//! Subagent worktree isolation helpers (#162).

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Prepare an isolated cwd for a subagent.
///
/// 1. Prefer a detached Git worktree under `.grokptah/worktrees/sub-<id>`,
///    then apply the live tracked delta and copy untracked files.
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
        match sync_live_working_tree(project, &wt) {
            Ok(()) => return Ok(wt),
            Err(error) => {
                let _ = std::process::Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&wt)
                    .current_dir(project)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let _ = std::fs::remove_dir_all(&wt);
                eprintln!("[grokptah] worktree sync failed, trying copy: {error:#}");
            }
        }
    }

    // Fail soft into a full snapshot copy — never empty-dir "isolation".
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

fn sync_live_working_tree(project: &Path, worktree: &Path) -> Result<()> {
    let diff = std::process::Command::new("git")
        .args([
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ])
        .current_dir(project)
        .output()
        .context("read live working-tree diff")?;
    if !diff.status.success() {
        bail!(
            "read live working-tree diff: {}",
            String::from_utf8_lossy(&diff.stderr).trim()
        );
    }
    if !diff.stdout.is_empty() {
        let mut child = std::process::Command::new("git")
            .args(["apply", "--binary", "--whitespace=nowarn", "-"])
            .current_dir(worktree)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("start applying live working-tree diff")?;
        child
            .stdin
            .take()
            .context("open git apply stdin")?
            .write_all(&diff.stdout)
            .context("send live working-tree diff")?;
        let output = child
            .wait_with_output()
            .context("wait for live working-tree diff")?;
        if !output.status.success() {
            bail!(
                "apply live working-tree diff: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }

    let untracked = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z", "--"])
        .current_dir(project)
        .output()
        .context("list untracked project files")?;
    if !untracked.status.success() {
        bail!(
            "list untracked project files: {}",
            String::from_utf8_lossy(&untracked.stderr).trim()
        );
    }
    let untracked =
        String::from_utf8(untracked.stdout).context("untracked path is not valid UTF-8")?;
    for raw in untracked.split('\0').filter(|path| !path.is_empty()) {
        let rel = Path::new(raw);
        if rel.is_absolute()
            || rel.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            bail!("unsafe untracked path reported by Git: {raw}");
        }
        if skip_isolation_path(rel) {
            continue;
        }
        let source = project.join(rel);
        let target = worktree.join(rel);
        let metadata = std::fs::symlink_metadata(&source)
            .with_context(|| format!("inspect untracked file {}", source.display()))?;
        if metadata.file_type().is_symlink() {
            copy_symlink(&source, &target)?;
        } else if metadata.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source, &target).with_context(|| {
                format!(
                    "copy untracked isolation file {} to {}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn copy_project_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    let entries = walkdir::WalkDir::new(src)
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(src)
                .map(|rel| !skip_isolation_path(rel))
                .unwrap_or(false)
        });
    for entry in entries {
        let entry = entry.context("walk project for isolation")?;
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        if skip_isolation_path(rel) {
            continue;
        }
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(p) = target.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "copy isolation file {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else if entry.file_type().is_symlink() {
            copy_symlink(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn skip_isolation_path(rel: &Path) -> bool {
    let parts: Vec<_> = rel.components().map(|part| part.as_os_str()).collect();
    parts.iter().any(|part| *part == ".git")
        || parts.first().is_some_and(|part| *part == "target")
        || parts.first().is_some_and(|part| *part == "node_modules")
        || (parts.first().is_some_and(|part| *part == ".grokptah")
            && parts.get(1).is_some_and(|part| *part == "worktrees"))
}

fn copy_symlink(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let link = std::fs::read_link(source)
        .with_context(|| format!("read isolation symlink {}", source.display()))?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&link, target)
            .with_context(|| format!("copy isolation symlink {}", target.display()))?;
    }
    #[cfg(windows)]
    {
        let resolved = source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&link);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(&link, target)
        } else {
            std::os::windows::fs::symlink_file(&link, target)
        }
        .with_context(|| format!("copy isolation symlink {}", target.display()))?;
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

    #[test]
    fn git_worktree_starts_from_live_uncommitted_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).success());
        assert!(git(&["config", "user.email", "test@example.com"]).success());
        assert!(git(&["config", "user.name", "GrokPtah Test"]).success());
        std::fs::write(dir.path().join("tracked.txt"), "committed").unwrap();
        std::fs::write(dir.path().join("removed.txt"), "remove me").unwrap();
        assert!(git(&["add", "tracked.txt", "removed.txt"]).success());
        assert!(git(&["commit", "-qm", "fixture"]).success());

        std::fs::write(dir.path().join("tracked.txt"), "live edit").unwrap();
        std::fs::remove_file(dir.path().join("removed.txt")).unwrap();
        std::fs::write(dir.path().join("untracked.txt"), "live untracked").unwrap();
        let wt = prepare_isolation_cwd(dir.path(), "live-snapshot").unwrap();

        assert_eq!(
            std::fs::read_to_string(wt.join("tracked.txt")).unwrap(),
            "live edit"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("untracked.txt")).unwrap(),
            "live untracked"
        );
        assert!(!wt.join("removed.txt").exists());
        assert!(wt.join(".git").is_file(), "expected a real Git worktree");
    }
}
