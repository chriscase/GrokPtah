//! Bearer auth + workspace allowlist (#196).

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::types::{OrchError, OrchErrorCode};

/// Service-scoped authority stamp carried by every `AuthContext`.
///
/// `authority` identifies one live service instance; `counter` is that
/// instance's monotonic authentication/policy epoch. A context is *current*
/// only while both halves still match the issuing service, so a context
/// minted before a credential rotation or a workspace-allowlist change stops
/// being usable the moment that change lands.
///
/// The stamp carries no bearer material: it is an opaque pair of a random
/// instance id and a counter, and knowing it does not let a caller construct
/// one (the constructors below are crate-internal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthEpoch {
    authority: Uuid,
    counter: u64,
}

impl AuthEpoch {
    /// First epoch of a freshly minted authority. Each call produces an
    /// authority that no other service instance can match, so a context issued
    /// by one service is never current at another.
    pub(super) fn new_authority() -> Self {
        Self {
            authority: Uuid::new_v4(),
            counter: 0,
        }
    }

    /// Next epoch of the same authority.
    ///
    /// Overflow fails closed with an error instead of saturating or wrapping;
    /// a wrapped counter would silently make already-issued stale contexts
    /// current again.
    pub(super) fn next(self) -> Result<Self, OrchError> {
        let counter = self.counter.checked_add(1).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Internal,
                "authentication epoch exhausted; refusing to rotate credentials or workspace policy",
            )
        })?;
        Ok(Self {
            authority: self.authority,
            counter,
        })
    }

    /// Monotonic counter, exposed for diagnostics and health payloads. The
    /// authority id is deliberately not exposed.
    pub fn counter(self) -> u64 {
        self.counter
    }

    /// Test-only: the same authority pinned to the last representable counter,
    /// so exhaustion can be exercised without 2^64 rotations.
    #[cfg(test)]
    pub(super) fn exhausted(self) -> Self {
        Self {
            authority: self.authority,
            counter: u64::MAX,
        }
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
    /// Authority + epoch this context was issued under. Private so callers
    /// outside this module cannot build an `AuthContext` by struct literal:
    /// contexts must be issued by the service that will honour them.
    epoch: AuthEpoch,
}

impl AuthContext {
    /// Issue a context bound to `epoch`. Crate-internal: the orchestration
    /// service is the only issuer, and it always stamps its current epoch.
    pub(super) fn issue(
        token_id: impl Into<String>,
        owner_id: impl Into<String>,
        epoch: AuthEpoch,
    ) -> Self {
        Self {
            token_id: token_id.into(),
            owner_id: owner_id.into(),
            epoch,
        }
    }

    /// Authority + epoch this context was issued under.
    pub fn epoch(&self) -> AuthEpoch {
        self.epoch
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
}

impl std::fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCredential")
            .field("id", &self.id)
            .field("token", &"[redacted]")
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
        Ok(Self { id, token })
    }

    pub fn token(&self) -> &str {
        &self.token
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

/// Authenticate a bearer header against `credentials` and stamp the resulting
/// context with `epoch`.
///
/// The caller supplies the epoch because only the issuing service knows its own
/// authority; it must pass the epoch it read *before* reading `credentials`, so
/// a rotation racing this call yields a context that is already stale rather
/// than one that is current under freshly rotated credentials.
pub fn authenticate_bearer(
    header: Option<&str>,
    credentials: &[AuthCredential],
    owner_id: &str,
    epoch: AuthEpoch,
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
    Ok(AuthContext::issue(
        credential.id.clone(),
        owner_id.trim(),
        epoch,
    ))
}

/// Single-credential *policy* helper: checks header shape and token equality
/// without consulting a service.
///
/// The context it returns carries a fresh throwaway authority, so it is not
/// current at any `OrchestrationService` and every guarded entry point rejects
/// it. Callers that need a usable context must go through
/// `OrchestrationService::auth_header`.
pub fn require_bearer(header: Option<&str>, expected: &str) -> Result<AuthContext, OrchError> {
    let credential = AuthCredential::new("primary", expected)?;
    authenticate_bearer(header, &[credential], "primary", AuthEpoch::new_authority())
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
        let epoch = AuthEpoch::new_authority();
        let auth = authenticate_bearer(Some("Bearer other-tok"), &credentials, "account-1", epoch)
            .unwrap();
        assert_eq!(auth.token_id, "laptop");
        assert_eq!(auth.owner_id, "account-1");
        assert_eq!(auth.epoch(), epoch);
        assert!(
            authenticate_bearer(Some("Bearer unknown"), &credentials, "account-1", epoch).is_err()
        );
    }

    #[test]
    fn epoch_is_monotonic_and_fails_closed_on_overflow() {
        let first = AuthEpoch::new_authority();
        assert_eq!(first.counter(), 0);
        let second = first.next().unwrap();
        assert_eq!(second.counter(), 1);
        assert_ne!(
            first, second,
            "advancing the epoch must invalidate the old one"
        );

        // Same authority across advances: only the counter moves.
        let exhausted = AuthEpoch {
            authority: first.authority,
            counter: u64::MAX,
        };
        let err = exhausted.next().unwrap_err();
        assert_eq!(err.code, OrchErrorCode::Internal);
        assert!(
            err.message.contains("epoch exhausted"),
            "overflow must fail closed rather than wrap: {}",
            err.message
        );
    }

    #[test]
    fn distinct_authorities_never_compare_equal() {
        let a = AuthEpoch::new_authority();
        let b = AuthEpoch::new_authority();
        assert_ne!(
            a, b,
            "each authority must be unique to its service instance"
        );
        assert_eq!(a.counter(), b.counter());
    }

    #[test]
    fn require_bearer_context_is_not_bound_to_any_service_authority() {
        let one = require_bearer(Some("Bearer tok"), "tok").unwrap();
        let two = require_bearer(Some("Bearer tok"), "tok").unwrap();
        assert_eq!(one.token_id, "primary");
        assert_ne!(
            one.epoch(),
            two.epoch(),
            "the policy helper must not mint reusable service authority"
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
