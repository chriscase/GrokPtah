//! Bearer auth + workspace allowlist (#196).

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::authority::{
    AuthorityCapabilityDocument, AuthorityOperation, AuthorityRole, AuthorityStamp,
    EffectiveAuthority, HostCapabilityProfile,
};
use super::types::{OrchError, OrchErrorCode};

fn credential_fingerprint(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.auth-context.v1\0");
    hasher.update(secret.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Immutable Computer-read capability bound to one credential.
///
/// A bearer cannot widen this binding through MCP/UI arguments. Legacy
/// credentials created with [`AuthCredential::new`] carry no grant.
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

#[derive(Clone)]
pub struct AuthContext {
    /// Stable credential identity used for audit and attribution. This is not
    /// the secret itself and may safely appear in durable records.
    pub token_id: String,
    /// Account/Agent owner identity shared by the service's authenticated
    /// device credentials. A later multi-tenant service can map credentials to
    /// different owner identities without changing the protocol shape.
    pub owner_id: String,
    authority: EffectiveAuthority,
    computer_read: Option<ComputerReadGrant>,
    /// Optional worker identity binding for least-privilege bearer tokens.
    /// When present, worker-scoped requests may address only this identity.
    bound_agent_id: Option<String>,
    /// Secret-free version of the bearer used to invalidate long-lived
    /// transports after token rotation.
    credential_fingerprint: String,
}

impl std::fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthContext")
            .field("token_id", &self.token_id)
            .field("owner_id", &self.owner_id)
            .field("stamp", &self.authority.stamp)
            .field("operations", &self.authority.operations)
            .finish_non_exhaustive()
    }
}

impl AuthContext {
    #[cfg(test)]
    pub(crate) fn remote_coordinator(
        token_id: impl Into<String>,
        owner_id: impl Into<String>,
        allowlist: &WorkspaceAllowlist,
    ) -> Result<Self, OrchError> {
        let token_id = token_id.into();
        Self::remote_with_role(
            token_id.clone(),
            owner_id,
            allowlist,
            AuthorityRole::RemoteCoordinator,
            None,
            None,
            &token_id,
        )
    }

    fn remote_with_role(
        token_id: impl Into<String>,
        owner_id: impl Into<String>,
        allowlist: &WorkspaceAllowlist,
        role: AuthorityRole,
        computer_read: Option<ComputerReadGrant>,
        bound_agent_id: Option<String>,
        credential_material: &str,
    ) -> Result<Self, OrchError> {
        let token_id = token_id.into();
        let owner_id = owner_id.into();
        let authority = EffectiveAuthority::remote_default(
            &token_id,
            &owner_id,
            allowlist.roots(),
            role,
            computer_read.is_some(),
        )?;
        let authority = match bound_agent_id.as_deref() {
            Some(agent_id) => authority.with_agent_scope(agent_id)?,
            None => authority,
        };
        Ok(Self {
            token_id,
            owner_id,
            authority,
            computer_read,
            bound_agent_id,
            credential_fingerprint: credential_fingerprint(credential_material),
        })
    }

    #[cfg(test)]
    pub(crate) fn trusted_local_test(
        token_id: impl Into<String>,
        owner_id: impl Into<String>,
        allowlist: &WorkspaceAllowlist,
    ) -> Result<Self, OrchError> {
        let token_id = token_id.into();
        let owner_id = owner_id.into();
        let authority = EffectiveAuthority::trusted_local_operator(&owner_id, allowlist.roots())?;
        let credential_fingerprint = credential_fingerprint(&token_id);
        Ok(Self {
            token_id,
            owner_id,
            authority,
            computer_read: None,
            bound_agent_id: None,
            credential_fingerprint,
        })
    }

    pub(crate) fn trusted_local_operator(
        owner_id: impl Into<String>,
        allowlist: &WorkspaceAllowlist,
    ) -> Result<Self, OrchError> {
        let owner_id = owner_id.into();
        let authority = EffectiveAuthority::trusted_local_operator(&owner_id, allowlist.roots())?;
        Ok(Self {
            token_id: "trusted-local-adapter".to_string(),
            owner_id,
            authority,
            computer_read: None,
            bound_agent_id: None,
            credential_fingerprint: credential_fingerprint("trusted-local-adapter"),
        })
    }

    pub fn require_operation(&self, operation: AuthorityOperation) -> Result<(), OrchError> {
        self.authority.require_operation(operation)
    }

    pub fn require_workspace(
        &self,
        operation: AuthorityOperation,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        self.authority.require_workspace(operation, workspace)
    }

    pub fn authority_stamp(&self) -> &AuthorityStamp {
        &self.authority.stamp
    }

    pub fn capability_document(&self) -> &AuthorityCapabilityDocument {
        &self.authority.capability_document
    }

    pub(crate) fn with_host_profile(
        mut self,
        profile: &HostCapabilityProfile,
    ) -> Result<Self, OrchError> {
        self.authority = self.authority.with_host_profile(profile)?;
        Ok(self)
    }

    pub fn role(&self) -> AuthorityRole {
        self.authority.stamp.role
    }

    pub fn computer_read_grant(&self) -> Option<&ComputerReadGrant> {
        self.computer_read.as_ref()
    }

    pub fn bound_agent_id(&self) -> Option<&str> {
        self.bound_agent_id.as_deref()
    }

    pub(crate) fn credential_fingerprint(&self) -> &str {
        &self.credential_fingerprint
    }

    /// Enforce an optional per-worker credential binding. Unbound coordinator
    /// credentials retain their existing multi-worker authority.
    pub fn require_agent_binding(&self, agent_id: &str) -> Result<(), OrchError> {
        if self
            .bound_agent_id
            .as_deref()
            .is_some_and(|bound| bound != agent_id)
        {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "credential is bound to a different worker identity",
            ));
        }
        Ok(())
    }

    /// Resolve an optional request identity without allowing a bound bearer
    /// to impersonate another worker. A bound worker defaults to its own id.
    pub fn resolve_agent_binding(
        &self,
        requested: Option<&str>,
    ) -> Result<Option<String>, OrchError> {
        if let Some(bound) = self.bound_agent_id() {
            let resolved = requested.unwrap_or(bound);
            self.require_agent_binding(resolved)?;
            return Ok(Some(resolved.to_string()));
        }
        Ok(requested.map(str::to_string))
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
    role: AuthorityRole,
    workspace_roots: Option<Vec<PathBuf>>,
    computer_read: Option<ComputerReadGrant>,
    bound_agent_id: Option<String>,
}

impl std::fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCredential")
            .field("id", &self.id)
            .field("token", &"[redacted]")
            .field("role", &self.role)
            .field(
                "workspace_grants",
                &self.workspace_roots.as_ref().map(Vec::len),
            )
            .field("computer_read", &self.computer_read)
            .field("bound_agent_id", &self.bound_agent_id)
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
            role: AuthorityRole::RemoteCoordinator,
            workspace_roots: None,
            computer_read: None,
            bound_agent_id: None,
        })
    }

    pub fn observer(id: impl Into<String>, token: impl Into<String>) -> Result<Self, OrchError> {
        let mut credential = Self::new(id, token)?;
        credential.role = AuthorityRole::Observer;
        Ok(credential)
    }

    /// Explicit operator bearer for deployments that require remote approval
    /// and promotion. Unlike the trusted local adapter, it never receives
    /// Computer Use authority.
    pub fn operator(id: impl Into<String>, token: impl Into<String>) -> Result<Self, OrchError> {
        let mut credential = Self::new(id, token)?;
        credential.role = AuthorityRole::RemoteOperator;
        Ok(credential)
    }

    /// Issue a fresh coordinator credential for one durable worker.
    ///
    /// The token is generated by the host, never derived from the Agent id,
    /// and the credential is required to carry at least one canonical
    /// workspace root. Callers receive the secret once so a trusted local
    /// adapter can hand it to the worker launcher; durable projections expose
    /// only the credential id, Agent binding, and fingerprint.
    pub fn issue_worker(
        agent_id: impl Into<String>,
        workspace_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, OrchError> {
        let id = format!("worker-{}", Uuid::new_v4().simple());
        let token = format!(
            "grok-worker-{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        Self::new(id, token)?
            .with_agent_binding(agent_id)?
            .with_workspace_roots(workspace_roots)
    }

    /// Rotate a worker token without changing its stable credential id,
    /// Agent binding, role, or workspace scope. The old bearer is invalid as
    /// soon as the returned credential replaces the prior configuration.
    pub fn rotate_worker_token(&self) -> Result<Self, OrchError> {
        if self.bound_agent_id.is_none() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "only an Agent-bound credential can be rotated as a worker",
            ));
        }
        let mut rotated = self.clone();
        rotated.token = format!(
            "grok-worker-{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        Ok(rotated)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn role(&self) -> AuthorityRole {
        self.role
    }

    pub fn computer_read_grant(&self) -> Option<&ComputerReadGrant> {
        self.computer_read.as_ref()
    }

    /// Narrow this bearer to one durable worker identity. The secret remains
    /// private; only the stable identity is carried into authorization/audit.
    pub fn with_agent_binding(mut self, agent_id: impl Into<String>) -> Result<Self, OrchError> {
        if self.computer_read.is_some() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "a Computer-read credential cannot be converted into a worker credential",
            ));
        }
        let agent_id = agent_id.into().trim().to_string();
        if agent_id.is_empty()
            || agent_id.len() > 256
            || agent_id.contains("..")
            || agent_id.contains('/')
            || agent_id.contains('\\')
            || agent_id.contains('\0')
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "worker identity binding is empty or invalid",
            ));
        }
        self.bound_agent_id = Some(agent_id);
        Ok(self)
    }

    pub fn bound_agent_id(&self) -> Option<&str> {
        self.bound_agent_id.as_deref()
    }

    /// Whether this credential carries an explicit canonical workspace
    /// narrowing instead of inheriting the service-wide allowlist.
    pub fn has_explicit_workspace_scope(&self) -> bool {
        self.workspace_roots.is_some()
    }

    /// Issue a credential bound to exactly one host-owned Computer-read
    /// session and canonical workspace.
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
        let mut credential = Self::new(id, token)?;
        let workspace = canonical_workspace(workspace.as_ref())?;
        credential.computer_read = Some(ComputerReadGrant {
            session_id,
            workspace: workspace.display().to_string(),
        });
        Ok(credential)
    }

    pub fn with_workspace_roots(
        mut self,
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, OrchError> {
        let roots = roots.into_iter().collect::<Vec<_>>();
        if roots.is_empty() || roots.len() > 64 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "credential workspace grants must contain between 1 and 64 roots",
            ));
        }
        let mut canonical = Vec::with_capacity(roots.len());
        for root in roots {
            canonical.push(canonical_workspace(&root)?);
        }
        canonical.sort();
        canonical.dedup();
        self.workspace_roots = Some(canonical);
        Ok(self)
    }

    pub(crate) fn effective_allowlist(
        &self,
        service_allowlist: &WorkspaceAllowlist,
    ) -> Result<WorkspaceAllowlist, OrchError> {
        let Some(roots) = self.workspace_roots.as_ref() else {
            if self.bound_agent_id.is_some() {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "an Agent-bound worker credential requires explicit workspace grants",
                ));
            }
            return Ok(service_allowlist.clone());
        };
        if roots.iter().any(|root| !service_allowlist.contains(root)) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "credential workspace grant exceeds the service allowlist",
            ));
        }
        Ok(WorkspaceAllowlist {
            roots: roots.clone(),
        })
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

pub(crate) fn authenticate_bearer(
    header: Option<&str>,
    credentials: &[AuthCredential],
    owner_id: &str,
    allowlist: &WorkspaceAllowlist,
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
    let credential_allowlist = credential.effective_allowlist(allowlist)?;
    #[cfg(test)]
    if credential.role == AuthorityRole::LocalOperator {
        return AuthContext::trusted_local_test(
            credential.id.clone(),
            owner_id.trim().to_string(),
            &credential_allowlist,
        );
    }
    AuthContext::remote_with_role(
        credential.id.clone(),
        owner_id.trim().to_string(),
        &credential_allowlist,
        credential.role,
        credential.computer_read.clone(),
        credential.bound_agent_id.clone(),
        credential.token(),
    )
}

/// Backward-compatible single-credential helper used by pure policy tests and
/// embedders that have not adopted named credentials yet.
#[cfg(test)]
pub(crate) fn require_bearer(
    header: Option<&str>,
    expected: &str,
) -> Result<AuthContext, OrchError> {
    let credential = AuthCredential::new("primary", expected)?;
    authenticate_bearer(
        header,
        &[credential],
        "primary",
        &WorkspaceAllowlist::default(),
    )
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
        let auth = authenticate_bearer(
            Some("Bearer other-tok"),
            &credentials,
            "account-1",
            &WorkspaceAllowlist::default(),
        )
        .unwrap();
        assert_eq!(auth.token_id, "laptop");
        assert_eq!(auth.owner_id, "account-1");
        assert!(authenticate_bearer(
            Some("Bearer unknown"),
            &credentials,
            "account-1",
            &WorkspaceAllowlist::default(),
        )
        .is_err());
    }

    #[test]
    fn worker_binding_is_narrow_and_defaults_missing_identity() {
        let workspace = tempdir().unwrap();
        let credential = AuthCredential::new("worker", "worker-token")
            .unwrap()
            .with_agent_binding("worker-a")
            .unwrap()
            .with_workspace_roots([workspace.path().to_path_buf()])
            .unwrap();
        let auth = authenticate_bearer(
            Some("Bearer worker-token"),
            &[credential],
            "account-1",
            &WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
        )
        .unwrap();
        assert_eq!(auth.bound_agent_id(), Some("worker-a"));
        assert_eq!(
            auth.capability_document().scopes.agent_ids,
            vec!["worker-a".to_string()]
        );
        assert_eq!(
            auth.authority_stamp().capability_document_hash,
            auth.capability_document().document_hash
        );
        assert_eq!(
            auth.resolve_agent_binding(None).unwrap().as_deref(),
            Some("worker-a")
        );
        assert_eq!(
            auth.resolve_agent_binding(Some("worker-a"))
                .unwrap()
                .as_deref(),
            Some("worker-a")
        );
        let error = auth.require_agent_binding("worker-b").unwrap_err();
        assert_eq!(error.code, OrchErrorCode::ForbiddenScope);
        assert!(
            auth.resolve_agent_binding(Some("worker-b")).is_err(),
            "a bound worker bearer must not impersonate another agent"
        );
    }

    #[test]
    fn worker_binding_rejects_path_like_identity() {
        for invalid in ["", "../worker", "worker/child", "worker\\child"] {
            assert!(
                AuthCredential::new("worker", "token")
                    .unwrap()
                    .with_agent_binding(invalid)
                    .is_err(),
                "invalid worker identity should fail closed: {invalid:?}"
            );
        }

        let unscoped = AuthCredential::new("worker", "token")
            .unwrap()
            .with_agent_binding("worker-a")
            .unwrap();
        assert!(unscoped
            .effective_allowlist(&WorkspaceAllowlist::default())
            .is_err());

        let workspace = tempdir().unwrap();
        let computer_read = AuthCredential::with_computer_read_grant(
            "computer-read",
            "read-token",
            Uuid::new_v4(),
            workspace.path(),
        )
        .unwrap();
        assert!(computer_read.with_agent_binding("worker-a").is_err());
    }

    #[test]
    fn host_issued_worker_credentials_are_scoped_and_rotatable() {
        let workspace = tempdir().unwrap();
        let credential =
            AuthCredential::issue_worker("worker-issued", [workspace.path().to_path_buf()])
                .unwrap();
        assert!(credential.id.starts_with("worker-"));
        assert!(credential.token().starts_with("grok-worker-"));
        assert_eq!(credential.bound_agent_id(), Some("worker-issued"));
        assert!(credential
            .effective_allowlist(&WorkspaceAllowlist::new([workspace.path().to_path_buf()]))
            .is_ok());

        let rotated = credential.rotate_worker_token().unwrap();
        assert_eq!(rotated.id, credential.id);
        assert_eq!(rotated.bound_agent_id(), credential.bound_agent_id());
        assert_ne!(rotated.token(), credential.token());
        assert_eq!(rotated.role(), AuthorityRole::RemoteCoordinator);
        assert!(AuthCredential::new("unbound", "token")
            .unwrap()
            .rotate_worker_token()
            .is_err());
    }

    #[test]
    fn legacy_credentials_have_no_computer_read_authority() {
        let credential = AuthCredential::new("legacy", "token").unwrap();
        assert!(credential.computer_read_grant().is_none());
        let auth = authenticate_bearer(
            Some("Bearer token"),
            &[credential],
            "owner",
            &WorkspaceAllowlist::default(),
        )
        .unwrap();
        assert!(auth.computer_read_grant().is_none());
    }

    #[test]
    fn computer_read_grant_is_canonical_and_immutable_on_authentication() {
        let workspace = tempdir().unwrap();
        let session_id = uuid::Uuid::new_v4();
        let credential = AuthCredential::with_computer_read_grant(
            "scoped",
            "scoped-token",
            session_id,
            workspace.path(),
        )
        .unwrap();
        let auth = authenticate_bearer(
            Some("Bearer scoped-token"),
            &[credential],
            "owner",
            &WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
        )
        .unwrap();
        let grant = auth.computer_read_grant().unwrap();
        assert_eq!(grant.session_id(), session_id);
        assert_eq!(
            grant.workspace(),
            canonical_workspace(workspace.path())
                .unwrap()
                .display()
                .to_string()
        );
    }

    #[test]
    fn credential_workspace_grants_can_only_narrow_the_service_scope() {
        let allowed_a = tempdir().unwrap();
        let allowed_b = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let service_allowlist = WorkspaceAllowlist::new([
            allowed_a.path().to_path_buf(),
            allowed_b.path().to_path_buf(),
        ]);
        let credential = AuthCredential::new("narrow", "token")
            .unwrap()
            .with_workspace_roots([allowed_a.path().to_path_buf()])
            .unwrap();
        let auth = authenticate_bearer(
            Some("Bearer token"),
            &[credential],
            "owner",
            &service_allowlist,
        )
        .unwrap();
        assert!(auth
            .require_workspace(AuthorityOperation::SessionsRead, allowed_a.path())
            .is_ok());
        assert!(auth
            .require_workspace(AuthorityOperation::SessionsRead, allowed_b.path())
            .is_err());

        let outside_credential = AuthCredential::new("outside", "outside-token")
            .unwrap()
            .with_workspace_roots([outside.path().to_path_buf()])
            .unwrap();
        assert!(authenticate_bearer(
            Some("Bearer outside-token"),
            &[outside_credential],
            "owner",
            &service_allowlist,
        )
        .is_err());
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
