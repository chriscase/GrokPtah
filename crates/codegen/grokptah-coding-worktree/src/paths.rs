//! Canonical path containment for fail-closed worktree writes and apply targets.

use std::path::{Component, Path, PathBuf};

use crate::error::{SessionError, SessionResult};

/// Resolve a relative path strictly inside `worktree_root`.
pub fn resolve_worktree_write_path(worktree_root: &Path, relative: &str) -> SessionResult<PathBuf> {
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return Err(SessionError::main_checkout_protected(
            "write_worktree_file rejects absolute paths",
        ));
    }
    for component in rel.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SessionError::main_checkout_protected(
                    "write_worktree_file rejects path traversal components",
                ));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let worktree_root = canonicalize_existing(worktree_root, "worktree root")?;
    let resolved = resolve_relative(&worktree_root, rel)?;

    if !is_same_or_strict_descendant(&resolved, &worktree_root) {
        return Err(SessionError::main_checkout_protected(
            "write_worktree_file path escapes disposable worktree root",
        ));
    }
    Ok(resolved)
}

/// Apply targets must be outside the protected main checkout prefix.
pub fn assert_apply_target_allowed(
    main_checkout: &Path,
    target: &Path,
    op: &str,
) -> SessionResult<()> {
    let main_checkout = canonicalize_existing(main_checkout, "main checkout")?;
    let target = canonicalize_existing(target, "apply target")?;
    if is_same_or_strict_descendant(&target, &main_checkout) {
        return Err(SessionError::main_checkout_protected(format!(
            "{op} cannot target a path inside the protected main checkout"
        )));
    }
    Ok(())
}

pub fn is_same_or_strict_descendant(path: &Path, root: &Path) -> bool {
    let path_components: Vec<_> = path.components().collect();
    let root_components: Vec<_> = root.components().collect();
    if path_components.len() < root_components.len() {
        return false;
    }
    path_components
        .iter()
        .zip(root_components.iter())
        .all(|(left, right)| left == right)
}

fn resolve_relative(base: &Path, relative: &Path) -> SessionResult<PathBuf> {
    let mut resolved = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return Err(SessionError::main_checkout_protected(
                        "write_worktree_file path escapes disposable worktree root",
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(SessionError::main_checkout_protected(
                    "write_worktree_file rejects absolute paths",
                ));
            }
        }
    }
    Ok(resolved)
}

fn canonicalize_existing(path: &Path, label: &str) -> SessionResult<PathBuf> {
    dunce::canonicalize(path).map_err(|error| {
        SessionError::new(
            crate::error::SessionErrorCode::Internal,
            format!("canonicalize {label}: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rejects_parent_dir_escape_from_worktree() {
        let dir = TempDir::new().expect("tempdir");
        let main = dir.path().join("project");
        let worktree = main.join(".grokptah").join("worktrees").join("run-1");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::write(main.join("README.md"), "main\n").expect("main readme");

        let err = resolve_worktree_write_path(&worktree, "../../../README.md").expect_err("escape");
        assert_eq!(
            err.code,
            crate::error::SessionErrorCode::MainCheckoutProtected
        );
        assert_eq!(
            std::fs::read_to_string(main.join("README.md")).expect("read"),
            "main\n"
        );
    }
}
