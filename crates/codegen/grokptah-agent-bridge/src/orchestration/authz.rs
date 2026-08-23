//! Bearer auth + workspace allowlist (#196).

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::types::{OrchError, OrchErrorCode};

/// Immutable Computer-read capability bound to one credential.
///
/// The grant is the only Computer-read scope a bearer may exercise. Callers
/// cannot widen it through MCP/UI session or workspace arguments. Legacy
/// credentials constructed with [`AuthCredential::new`] carry no grant and
/// receive no Computer-read authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerReadGrant {
    session_id: Uuid,
    workspace: String,
}

impl ComputerReadGrant {
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Stable credential identity used for audit and attribution. This is not
    /// the secret itself and may safely appear in durable records.
    pub token_id: String,
    /// Account/Agent owner identity shared by the service's authenticated
    /// device credentials. A later multi-tenant service can map credentials to
    /// different owner identities without changing the protocol shape.
    pub owner_id: String,
    computer_read: Option<ComputerReadGrant>,
}

impl AuthContext {
    pub fn new(token_id: impl Into<String>, owner_id: impl Into<String>) -> Self {
        Self {
            token_id: token_id.into(),
            owner_id: owner_id.into(),
            computer_read: None,
        }
    }

    pub fn computer_read_grant(&self) -> Option<&ComputerReadGrant> {
        self.computer_read.as_ref()
    }
}

/// One named bearer credential accepted by a service instance.
///
/// The token remains private so accidental debug/JSON output cannot expose
/// service secrets. The stable id is the identity carried into audits and
/// client-attributed Run records.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthCredential {
    pub id: String,
    token: String,
    computer_read: Option<ComputerReadGrant>,
}

impl std::fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCredential")
            .field("id", &self.id)
            .field("token", &"[redacted]")
            .field("computer_read", &self.computer_read)
            .finish()
    }
}

impl AuthCredential {
    pub fn new(id: impl Into<String>, token: impl Into<String>) -> Result<Self, OrchError> {
        let id = id.into().trim().to_string();
        let token = token.into().trim().to_string();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credential id must contain only ASCII letters, numbers, '-', '_', or '.'",
            ));
        }
        if token.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credential token must not be empty",
            ));
        }
        Ok(Self {
            id,
            token,
            computer_read: None,
        })
    }

    /// Issue a credential that may read Computer Runs for exactly one
    /// owner session and canonical workspace. Unscoped [`Self::new`]
    /// credentials never gain this authority.
    pub fn with_computer_read_grant(
        id: impl Into<String>,
        token: impl Into<String>,
        session_id: Uuid,
        workspace: impl AsRef<Path>,
    ) -> Result<Self, OrchError> {
        if session_id.is_nil() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "computer-read grant session is missing",
            ));
        }
        let workspace = canonical_workspace(workspace.as_ref())?;
        let mut credential = Self::new(id, token)?;
        credential.computer_read = Some(ComputerReadGrant {
            session_id,
            workspace: workspace.display().to_string(),
        });
        Ok(credential)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn computer_read_grant(&self) -> Option<&ComputerReadGrant> {
        self.computer_read.as_ref()
    }
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

/// Constant-time equality for equal-length secrets; length mismatch is not constant-time
/// (still fail closed without short-circuiting byte compares of equal lengths).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn authenticate_bearer(
    header: Option<&str>,
    credentials: &[AuthCredential],
    owner_id: &str,
) -> Result<AuthContext, OrchError> {
    if credentials.is_empty() || owner_id.trim().is_empty() {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "control plane credentials are not configured",
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
    let credential = credentials
        .iter()
        .find(|credential| constant_time_eq(token.as_bytes(), credential.token.as_bytes()));
    let Some(credential) = credential else {
        return Err(OrchError::new(
            OrchErrorCode::Unauthenticated,
            "invalid bearer token",
        ));
    };
    Ok(AuthContext {
        token_id: credential.id.clone(),
        owner_id: owner_id.trim().to_string(),
        computer_read: credential.computer_read.clone(),
    })
}

/// Backward-compatible single-credential helper used by pure policy tests and
/// embedders that have not adopted named credentials yet.
pub fn require_bearer(header: Option<&str>, expected: &str) -> Result<AuthContext, OrchError> {
    let credential = AuthCredential::new("primary", expected)?;
    authenticate_bearer(header, &[credential], "primary")
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
        assert_eq!(
            require_bearer(Some("Bearer tok"), "tok").unwrap().token_id,
            "primary"
        );
    }

    #[test]
    fn named_credentials_return_client_identity_and_shared_owner() {
        let credentials = vec![
            AuthCredential::new("primary", "tok").unwrap(),
            AuthCredential::new("laptop", "other-tok").unwrap(),
        ];
        let auth =
            authenticate_bearer(Some("Bearer other-tok"), &credentials, "account-1").unwrap();
        assert_eq!(auth.token_id, "laptop");
        assert_eq!(auth.owner_id, "account-1");
        assert!(authenticate_bearer(Some("Bearer unknown"), &credentials, "account-1").is_err());
        assert!(auth.computer_read_grant().is_none());
    }

    #[test]
    fn legacy_unscoped_credentials_have_no_computer_read_authority() {
        let credential = AuthCredential::new("primary", "tok").unwrap();
        assert!(credential.computer_read_grant().is_none());
        let auth = authenticate_bearer(Some("Bearer tok"), &[credential], "primary").unwrap();
        assert!(auth.computer_read_grant().is_none());
    }

    #[test]
    fn computer_read_grant_is_copied_immutably_onto_the_auth_context() {
        let dir = tempdir().unwrap();
        let session = Uuid::new_v4();
        let credential =
            AuthCredential::with_computer_read_grant("laptop", "scoped-tok", session, dir.path())
                .unwrap();
        let granted = credential.computer_read_grant().unwrap().clone();
        let auth =
            authenticate_bearer(Some("Bearer scoped-tok"), &[credential], "account-1").unwrap();
        let bound = auth.computer_read_grant().unwrap();
        assert_eq!(bound.session_id(), session);
        assert_eq!(bound.workspace(), granted.workspace());
        assert_eq!(
            bound.workspace(),
            canonical_workspace(dir.path())
                .unwrap()
                .display()
                .to_string()
        );
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

    #[test]
    fn allowlist_symlink_into_root_accepted_outside_rejected() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let al = WorkspaceAllowlist::new([root.path().to_path_buf()]);

        // Symlink whose target resolves inside the allowlisted root → allowed.
        let link_in = root.path().join("via-link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), &link_in).unwrap();
            assert!(
                al.contains(&link_in),
                "symlink canonicalizing into allowlisted root must be accepted"
            );
        }

        // Symlink whose target is outside the allowlist → fail closed.
        let link_out = root.path().join("escape");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), &link_out).unwrap();
            assert!(
                !al.contains(&link_out),
                "symlink escaping allowlisted root must be rejected"
            );
            // require_workspace_match also fails closed on escaped claim.
            let err = require_workspace_match(&al, Some(root.path()), &link_out);
            assert!(err.is_err());
        }

        // Path that canonicalizes outside the allowlisted root is rejected.
        assert!(!al.contains(outside.path()));
    }
}
