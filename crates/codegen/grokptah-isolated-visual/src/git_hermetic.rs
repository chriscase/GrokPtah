use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{IsolatedError, IsolatedResult};

const PINNED_GIT_BINARIES: &[&str] = &["/usr/bin/git", "/opt/homebrew/bin/git"];

/// Filesystem-only inspection of a Git directory. Ambient `GIT_*` environment,
/// worktree, index, hooks, replacements, alternates, and credentials are never
/// inherited. This does not parse porcelain or walk history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirProbe {
    pub git_dir: PathBuf,
    pub git_binary: PathBuf,
}

impl GitDirProbe {
    pub fn inspect(git_dir: &Path, git_binary: &Path) -> IsolatedResult<Self> {
        if !git_binary.is_absolute()
            || !PINNED_GIT_BINARIES
                .iter()
                .any(|allowed| Path::new(allowed) == git_binary)
        {
            return Err(IsolatedError::forbidden(
                "git binary is not an allowlisted absolute path",
            ));
        }
        let meta = fs::symlink_metadata(git_binary)
            .map_err(|_| IsolatedError::unavailable("pinned git binary is not readable"))?;
        if meta.file_type().is_symlink() {
            return Err(IsolatedError::forbidden(
                "pinned git binary must not be a symlink",
            ));
        }
        if !git_dir.is_absolute() {
            return Err(IsolatedError::forbidden("GIT_DIR must be an absolute path"));
        }
        let git_dir = fs::canonicalize(git_dir)
            .map_err(|_| IsolatedError::forbidden("GIT_DIR cannot be canonicalized"))?;
        reject_if_exists(
            &git_dir.join("objects/info/alternates"),
            "alternate object store",
        )?;
        reject_if_exists(
            &git_dir.join("objects/info/http-alternates"),
            "http alternate object store",
        )?;
        reject_if_dir_nonempty(&git_dir.join("refs/replace"), "replacement refs")?;
        reject_if_exists(
            &git_dir.join("commondir"),
            "shared object store via commondir",
        )?;
        reject_if_exists(&git_dir.join("worktrees"), "linked worktrees")?;
        reject_if_exists(&git_dir.join("index"), "ambient index")?;
        reject_if_dir_nonempty(&git_dir.join("hooks"), "repository hooks")?;
        reject_if_exists(&git_dir.join("config"), "repository config")?;
        reject_if_exists(&git_dir.join("shallow"), "shallow/promisor history")?;
        reject_if_exists(&git_dir.join(".gitmodules"), "submodule contract")?;
        Ok(Self {
            git_dir,
            git_binary: git_binary.to_path_buf(),
        })
    }

    /// Invoke git with a cleared environment. Callers must not pass object
    /// names from porcelain. Used only to prove object closure of one pinned
    /// commit; history walking and rename detection are out of scope.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.git_binary);
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("GIT_DIR", &self.git_dir);
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        cmd.env("GCM_INTERACTIVE", "never");
        cmd.env("LC_ALL", "C");
        cmd.env("LANG", "C");
        cmd
    }
}

fn reject_if_exists(path: &Path, label: &str) -> IsolatedResult<()> {
    if path.exists() {
        return Err(IsolatedError::forbidden(format!(
            "git directory has {label}; hermetic resolve fails closed"
        )));
    }
    Ok(())
}

fn reject_if_dir_nonempty(path: &Path, label: &str) -> IsolatedResult<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        return Err(IsolatedError::forbidden(format!(
            "git directory has {label}"
        )));
    }
    let empty = fs::read_dir(path)
        .map_err(|_| IsolatedError::internal(format!("cannot read {label}")))?
        .next()
        .is_none();
    if !empty {
        return Err(IsolatedError::forbidden(format!(
            "git directory has {label}; hermetic resolve fails closed"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rejects_alternates_replace_index_hooks_and_unpinned_git() {
        let dir = tempdir().unwrap();
        let git_dir = dir.path().join("git");
        fs::create_dir_all(git_dir.join("objects/info")).unwrap();
        fs::create_dir_all(git_dir.join("refs")).unwrap();
        let fake_git = dir.path().join("git-bin");
        fs::write(&fake_git, "#!/bin/sh\n").unwrap();
        assert!(GitDirProbe::inspect(&git_dir, &fake_git).is_err());

        fs::write(
            git_dir.join("objects/info/alternates"),
            "/tmp/other.git/objects\n",
        )
        .unwrap();
        let pinned = Path::new("/usr/bin/git");
        if pinned.exists() {
            assert!(GitDirProbe::inspect(&git_dir, pinned).is_err());
            fs::remove_file(git_dir.join("objects/info/alternates")).unwrap();
            fs::create_dir_all(git_dir.join("refs/replace")).unwrap();
            fs::write(git_dir.join("refs/replace/deadbeef"), "cafebabe\n").unwrap();
            assert!(GitDirProbe::inspect(&git_dir, pinned).is_err());
        }
    }
}
