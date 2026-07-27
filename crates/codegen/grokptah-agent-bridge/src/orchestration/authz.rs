//! Bearer auth + workspace allowlist (#196).

use std::path::{Path, PathBuf};

use super::types::{OrchError, OrchErrorCode};

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub token_id: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceAllowlist {
    roots: Vec<PathBuf>,
}

impl WorkspaceAllowlist {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots
                .into_iter()
                .filter_map(|p| canonical_workspace(&p).ok())
                .collect(),
        }
    }

    pub fn contains(&self, workspace: &Path) -> bool {
        let Ok(c) = canonical_workspace(workspace) else {
            return false;
        };
        self.roots.iter().any(|r| r == &c)
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

/// Canonical absolute path for workspace identity comparisons.
pub fn canonical_workspace(path: &Path) -> Result<PathBuf, OrchError> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .join(path)
    };
    dunce::canonicalize(&abs).map_err(|e| {
        OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            format!("cannot canonicalize {}: {e}", abs.display()),
        )
    })
}

pub fn require_bearer(header: Option<&str>, expected: &str) -> Result<AuthContext, OrchError> {
    if expected.is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "control plane token not configured",
        ));
    }
    let Some(h) = header else {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "missing Authorization bearer token",
        ));
    };
    let h = h.trim();
    let token = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
        .unwrap_or("")
        .trim();
    if token.is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "missing bearer token",
        ));
    }
    if token != expected {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "invalid bearer token",
        ));
    }
    Ok(AuthContext {
        token_id: "primary".into(),
    })
}

pub fn require_workspace_match(
    allowlist: &WorkspaceAllowlist,
    session_cwd: Option<&Path>,
    claimed: &Path,
) -> Result<PathBuf, OrchError> {
    let claimed_c = canonical_workspace(claimed)?;
    if !allowlist.contains(&claimed_c) {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "workspace not in allowlist",
        ));
    }
    let Some(scwd) = session_cwd else {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "session has no project cwd",
        ));
    };
    let session_c = canonical_workspace(scwd)?;
    if session_c != claimed_c {
        return Err(OrchError::new(
            OrchErrorCode::WorkspaceMismatch,
            "session workspace does not match claimed workspace",
        ));
    }
    Ok(claimed_c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bearer_fail_closed() {
        assert!(require_bearer(None, "tok").is_err());
        assert!(require_bearer(Some("tok"), "tok").is_err());
        assert!(require_bearer(Some("Bearer wrong"), "tok").is_err());
        assert!(require_bearer(Some("Bearer tok"), "tok").is_ok());
    }

    #[test]
    fn allowlist_canonical() {
        let d = tempdir().unwrap();
        let root = d.path().to_path_buf();
        let al = WorkspaceAllowlist::new([root.clone()]);
        assert!(al.contains(&root));
        let other = tempdir().unwrap();
        assert!(!al.contains(other.path()));
    }
}
