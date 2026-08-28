//! Bearer authentication and workspace policy.
//!
//! Authentication is deliberately split into two layers:
//!
//! * [`AuthContext`] is a host-issued, opaque capability. It has no public
//!   constructor and cannot be deserialized into authority.
//! * [`AuthRegistry`] is the host-owned durable authority. It persists the
//!   credential incarnation and authentication generation, but never persists
//!   bearer material.
//!
//! The registry is intentionally independent from the durable run/work
//! records. Records store only an opaque binding digest; the host revalidates
//! that digest against this registry before every effect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{OrchError, OrchErrorCode};

const AUTHORITY_SCHEMA_VERSION: u32 = 1;
const AUTHORITY_FILE: &str = "auth-authority.json";
const MAX_AUTH_ID_BYTES: usize = 128;
const MAX_AUTH_OWNER_BYTES: usize = 128;
const EFFECT_LEASE_TTL: Duration = Duration::from_secs(30);

/// Host-issued opaque principal identity.
///
/// The bytes and constructor are private by design. In particular, this type
/// intentionally has no `Serialize` or `Deserialize` implementation: JSON
/// received from a client can never become a principal.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PrincipalRef([u8; 16]);

impl std::fmt::Debug for PrincipalRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrincipalRef([redacted])")
    }
}

/// Stable identity for one credential incarnation. Replacing a removed
/// credential creates a new incarnation even when its textual id is reused.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CredentialIncarnation([u8; 16]);

impl std::fmt::Debug for CredentialIncarnation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialIncarnation([redacted])")
    }
}

/// Host authentication generation. Its numeric value never crosses a public
/// DTO; it is only compared inside the host authority.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthenticationGeneration(u64);

impl std::fmt::Debug for AuthenticationGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthenticationGeneration([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AuthorityStamp {
    principal: PrincipalRef,
    incarnation: CredentialIncarnation,
    generation: AuthenticationGeneration,
    credential_id: String,
    owner_id: String,
}

impl std::fmt::Debug for AuthorityStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorityStamp")
            .field("principal", &self.principal)
            .field("incarnation", &self.incarnation)
            .field("generation", &self.generation)
            .field("credential_id", &"[redacted]")
            .field("owner_id", &"[redacted]")
            .finish()
    }
}

/// A public actor handle is opaque and stable for one credential incarnation.
/// It is safe to use for display and correlation, but cannot be used to
/// construct an [`AuthContext`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicActorHandle(String);

impl PublicActorHandle {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A host-issued authenticated request capability.
///
/// This type intentionally does not implement serde and its identity fields
/// are private. `Clone` only clones a capability; it cannot change who it is.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthContext {
    stamp: AuthorityStamp,
    delegation: Option<DelegationScope>,
}

impl std::fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthContext")
            .field("stamp", &self.stamp)
            .field("delegated", &self.delegation.is_some())
            .finish()
    }
}

impl AuthContext {
    pub fn principal_ref(&self) -> &PrincipalRef {
        &self.stamp.principal
    }

    pub fn actor_handle(&self) -> PublicActorHandle {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.stamp.principal.0);
        bytes.extend_from_slice(&self.stamp.incarnation.0);
        PublicActorHandle(format!("actor_{}", hex_sha256(&bytes)[..32].to_string()))
    }

    pub(crate) fn credential_id(&self) -> &str {
        &self.stamp.credential_id
    }

    pub(crate) fn owner_id(&self) -> &str {
        &self.stamp.owner_id
    }

    /// Opaque digest stored alongside durable resources. It includes the
    /// incarnation and generation but reveals neither to a client.
    pub(crate) fn binding_digest(&self) -> String {
        let mut bytes = Vec::with_capacity(16 + 16 + 8);
        bytes.extend_from_slice(&self.stamp.principal.0);
        bytes.extend_from_slice(&self.stamp.incarnation.0);
        bytes.extend_from_slice(&self.stamp.generation.0.to_be_bytes());
        hex_sha256(&bytes)
    }

    fn matches(&self, record: &StoredCredential) -> bool {
        self.stamp.credential_id == record.credential_id
            && self.stamp.owner_id == record.owner_id
            && decode_fixed_hex(&record.principal)
                .is_some_and(|principal| self.stamp.principal.0 == principal)
            && decode_fixed_hex(&record.incarnation)
                .is_some_and(|incarnation| self.stamp.incarnation.0 == incarnation)
            && self.stamp.generation.0 == record.generation
    }

    pub(crate) fn require_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
        agent_id: Option<&str>,
    ) -> Result<(), OrchError> {
        let Some(scope) = &self.delegation else {
            return Ok(());
        };
        if scope.session_id != session_id
            || scope.workspace != workspace
            || scope.agent_id.as_deref() != agent_id
        {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "host delegation is outside its exact scope",
            ));
        }
        Ok(())
    }

    fn delegated(mut self, scope: DelegationScope) -> Self {
        self.delegation = Some(scope);
        self
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DelegationScope {
    pub session_id: Uuid,
    pub workspace: PathBuf,
    pub agent_id: Option<String>,
}

impl std::fmt::Debug for DelegationScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegationScope")
            .field("session_id", &self.session_id)
            .field("workspace", &"[redacted]")
            .field("agent_id", &"[redacted]")
            .finish()
    }
}

/// A bounded one-shot permission to cross one physical effect boundary.
///
/// The lease is not a replacement for checking the request at admission. It
/// is an additional fence: consumption revalidates the live authority and
/// fails if a generation changed after admission.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectLease {
    binding: String,
    scope: String,
    expires_at: Instant,
    consumed: bool,
}

impl std::fmt::Debug for EffectLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectLease")
            .field("binding", &"[redacted]")
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .field("consumed", &self.consumed)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCredential {
    credential_id: String,
    owner_id: String,
    principal: String,
    incarnation: String,
    generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAuthority {
    schema_version: u32,
    next_generation: u64,
    credentials: Vec<StoredCredential>,
    /// Resource bindings are opaque digests. Keeping this ledger separate
    /// from public record serde prevents internal authority fields from
    /// crossing MCP/SDK/Desktop projections.
    #[serde(default)]
    bindings: BTreeMap<String, String>,
}

impl Default for StoredAuthority {
    fn default() -> Self {
        Self {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            next_generation: 1,
            credentials: Vec::new(),
            bindings: BTreeMap::new(),
        }
    }
}

/// Host-owned registry loaded from and persisted to the canonical orchestration
/// store. Bearer secrets are supplied by the caller and never enter this
/// structure or its JSON representation.
pub(crate) struct AuthRegistry {
    root: PathBuf,
    state: StoredAuthority,
    durable_error: Option<String>,
}

impl AuthRegistry {
    pub(crate) fn unavailable(root: &Path, error: impl Into<String>) -> Self {
        Self {
            root: root.to_path_buf(),
            state: StoredAuthority::default(),
            durable_error: Some(error.into()),
        }
    }

    pub(crate) fn open(
        root: &Path,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<Self, OrchError> {
        validate_owner(owner_id)?;
        let path = root.join(AUTHORITY_FILE);
        let (state, existed) = if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("read durable auth authority: {error}"),
                )
            })?;
            let state: StoredAuthority = serde_json::from_str(&text).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("parse durable auth authority: {error}"),
                )
            })?;
            validate_stored_authority(&state)?;
            (state, true)
        } else {
            (StoredAuthority::default(), false)
        };
        let mut registry = Self {
            root: root.to_path_buf(),
            state,
            durable_error: None,
        };
        let changed = registry.reconcile(credentials, owner_id)?;
        if changed || !existed {
            registry.persist()?;
        }
        Ok(registry)
    }

    fn persist(&self) -> Result<(), OrchError> {
        if let Some(error) = &self.durable_error {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                format!("durable auth authority is unavailable: {error}"),
            ));
        }
        let path = self.root.join(AUTHORITY_FILE);
        let tmp = self.root.join("auth-authority.json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("serialize durable auth authority: {error}"),
            )
        })?;
        std::fs::write(&tmp, bytes).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("write durable auth authority: {error}"),
            )
        })?;
        std::fs::rename(&tmp, path).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("commit durable auth authority: {error}"),
            )
        })
    }

    pub(crate) fn reconcile(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<bool, OrchError> {
        validate_owner(owner_id)?;
        validate_credentials(credentials)?;
        let mut changed = false;
        let owner_changed = self
            .state
            .credentials
            .iter()
            .any(|record| record.owner_id != owner_id);
        let mut next = Vec::with_capacity(credentials.len());
        for credential in credentials {
            let existing = if owner_changed {
                None
            } else {
                self.state
                    .credentials
                    .iter()
                    .find(|record| record.credential_id == credential.id)
            };
            let record = match existing {
                Some(record) => record.clone(),
                None => {
                    changed = true;
                    new_stored_credential(&credential.id, owner_id, self.allocate_generation()?)
                }
            };
            next.push(record);
        }
        if next.len() != self.state.credentials.len()
            || next != self.state.credentials
            || owner_changed
        {
            changed = true;
            self.state.credentials = next;
        }
        Ok(changed)
    }

    pub(crate) fn set_credentials(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<(), OrchError> {
        self.reconcile(credentials, owner_id)?;
        self.persist()
    }

    /// Replace a credential identity rather than rotating its secret. A
    /// replacement receives a fresh principal and incarnation even when its
    /// textual credential id is unchanged.
    pub(crate) fn replace_credential(
        &mut self,
        credential_id: &str,
        owner_id: &str,
    ) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        let Some(index) = self
            .state
            .credentials
            .iter()
            .position(|record| record.credential_id == credential_id)
        else {
            return Err(stale_authority());
        };
        let generation = self.allocate_generation()?;
        self.state.credentials[index] =
            new_stored_credential(credential_id, owner_id, generation);
        self.persist()
    }

    pub(crate) fn change_owner(&mut self, owner_id: &str) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        if self.state.credentials.is_empty() {
            return Ok(());
        }
        let old = std::mem::take(&mut self.state.credentials);
        let mut replacement = Vec::with_capacity(old.len());
        for record in old {
            let generation = self.allocate_generation()?;
            replacement.push(new_stored_credential(
                &record.credential_id,
                owner_id,
                generation,
            ));
        }
        self.state.credentials = replacement;
        self.persist()
    }

    fn allocate_generation(&mut self) -> Result<u64, OrchError> {
        let generation = self.state.next_generation;
        self.state.next_generation = generation.checked_add(1).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Internal,
                "authentication generation exhausted",
            )
        })?;
        Ok(generation)
    }

    pub(crate) fn authenticate(
        &self,
        header: Option<&str>,
        credentials: &[AuthCredential],
    ) -> Result<AuthContext, OrchError> {
        if credentials.is_empty() || self.state.credentials.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "control plane credentials are not configured",
            ));
        }
        let token = bearer_token(header)?;
        let Some((credential, record)) = credentials.iter().find_map(|credential| {
            self.state
                .credentials
                .iter()
                .find(|record| record.credential_id == credential.id)
                .filter(|_| constant_time_eq(token.as_bytes(), credential.token.as_bytes()))
                .map(|record| (credential, record))
        }) else {
            return Err(OrchError::new(
                OrchErrorCode::Unauthenticated,
                "invalid bearer token",
            ));
        };
        let principal = decode_fixed_hex(&record.principal).ok_or_else(|| {
            OrchError::new(OrchErrorCode::Internal, "durable auth principal is invalid")
        })?;
        let incarnation = decode_fixed_hex(&record.incarnation).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Internal,
                "durable auth incarnation is invalid",
            )
        })?;
        Ok(AuthContext {
            stamp: AuthorityStamp {
                principal: PrincipalRef(principal),
                incarnation: CredentialIncarnation(incarnation),
                generation: AuthenticationGeneration(record.generation),
                credential_id: credential.id.clone(),
                owner_id: record.owner_id.clone(),
            },
            delegation: None,
        })
    }

    pub(crate) fn require_current(&self, auth: &AuthContext) -> Result<(), OrchError> {
        let Some(record) = self
            .state
            .credentials
            .iter()
            .find(|record| record.credential_id == auth.stamp.credential_id)
        else {
            return Err(stale_authority());
        };
        if auth.matches(record) {
            Ok(())
        } else {
            Err(stale_authority())
        }
    }

    pub(crate) fn public_actor_handle(&self, credential_id: &str) -> Option<PublicActorHandle> {
        self.state
            .credentials
            .iter()
            .find(|record| record.credential_id == credential_id)
            .and_then(actor_handle_for_record)
    }

    pub(crate) fn ensure_resource_binding(
        &mut self,
        resource: &str,
        auth: &AuthContext,
    ) -> Result<(), OrchError> {
        self.require_current(auth)?;
        if resource.is_empty() || resource.len() > 512 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "authority resource key is empty or exceeds its bound",
            ));
        }
        let digest = auth.binding_digest();
        match self.state.bindings.get(resource) {
            Some(existing) if existing == &digest => Ok(()),
            Some(_) => Err(stale_authority()),
            None => {
                self.state.bindings.insert(resource.to_string(), digest);
                self.persist()
            }
        }
    }

    pub(crate) fn mint_effect_lease(
        &self,
        auth: &AuthContext,
        scope: impl Into<String>,
    ) -> Result<EffectLease, OrchError> {
        self.require_current(auth)?;
        let scope = scope.into();
        if scope.is_empty() || scope.len() > 512 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "effect lease scope is empty or exceeds its bound",
            ));
        }
        Ok(EffectLease {
            binding: auth.binding_digest(),
            scope,
            expires_at: Instant::now() + EFFECT_LEASE_TTL,
            consumed: false,
        })
    }

    pub(crate) fn consume_effect_lease(
        &self,
        auth: &AuthContext,
        lease: &mut EffectLease,
        scope: &str,
    ) -> Result<(), OrchError> {
        self.require_current(auth)?;
        if lease.consumed
            || lease.scope != scope
            || lease.binding != auth.binding_digest()
            || Instant::now() >= lease.expires_at
        {
            return Err(stale_authority());
        }
        lease.consumed = true;
        Ok(())
    }

    pub(crate) fn issue_delegation(
        &self,
        auth: &AuthContext,
        session_id: Uuid,
        workspace: PathBuf,
        agent_id: Option<String>,
    ) -> Result<AuthContext, OrchError> {
        self.require_current(auth)?;
        Ok(auth.clone().delegated(DelegationScope {
            session_id,
            workspace,
            agent_id,
        }))
    }

    pub(crate) fn primary_context(
        &self,
        credentials: &[AuthCredential],
    ) -> Result<AuthContext, OrchError> {
        let primary = credentials
            .iter()
            .find(|credential| credential.id == "primary")
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    "primary credential is not configured",
                )
            })?;
        self.authenticate(Some(&format!("Bearer {}", primary.token())), credentials)
    }

    pub(crate) fn rotate_generation(&mut self, credential_id: &str) -> Result<(), OrchError> {
        let generation = self.allocate_generation()?;
        let Some(record) = self
            .state
            .credentials
            .iter_mut()
            .find(|record| record.credential_id == credential_id)
        else {
            return Err(stale_authority());
        };
        record.generation = generation;
        self.persist()
    }

    pub(crate) fn revoke(&mut self, credential_id: &str) -> Result<(), OrchError> {
        let old_len = self.state.credentials.len();
        self.state
            .credentials
            .retain(|record| record.credential_id != credential_id);
        if old_len == self.state.credentials.len() {
            return Err(stale_authority());
        }
        self.persist()
    }
}

fn stale_authority() -> OrchError {
    OrchError::new(
        OrchErrorCode::Unauthenticated,
        "authentication authority is stale",
    )
}

fn validate_owner(owner_id: &str) -> Result<(), OrchError> {
    if owner_id.trim().is_empty() || owner_id.len() > MAX_AUTH_OWNER_BYTES {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "Agent owner id must be between 1 and 128 bytes",
        ));
    }
    Ok(())
}

fn validate_credentials(credentials: &[AuthCredential]) -> Result<(), OrchError> {
    let mut ids = std::collections::HashSet::new();
    for credential in credentials {
        if !ids.insert(credential.id.as_str()) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "auth credential ids must be unique",
            ));
        }
    }
    Ok(())
}

fn validate_stored_authority(state: &StoredAuthority) -> Result<(), OrchError> {
    if state.schema_version != AUTHORITY_SCHEMA_VERSION || state.next_generation == 0 {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "durable auth authority schema is invalid",
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for record in &state.credentials {
        if !ids.insert(record.credential_id.as_str())
            || record.credential_id.is_empty()
            || record.credential_id.len() > MAX_AUTH_ID_BYTES
            || record.owner_id.is_empty()
            || record.owner_id.len() > MAX_AUTH_OWNER_BYTES
            || record.generation == 0
            || decode_fixed_hex(&record.principal).is_none()
            || decode_fixed_hex(&record.incarnation).is_none()
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "durable auth authority record is invalid",
            ));
        }
    }
    for (resource, binding) in &state.bindings {
        if resource.is_empty()
            || resource.len() > 512
            || binding.len() != 64
            || !binding.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "durable auth resource binding is invalid",
            ));
        }
    }
    Ok(())
}

fn new_stored_credential(id: &str, owner_id: &str, generation: u64) -> StoredCredential {
    StoredCredential {
        credential_id: id.to_string(),
        owner_id: owner_id.to_string(),
        principal: hex_sha256(&Uuid::new_v4().into_bytes()),
        incarnation: hex_sha256(&Uuid::new_v4().into_bytes()),
        generation,
    }
}

fn actor_handle_for_record(record: &StoredCredential) -> Option<PublicActorHandle> {
    let principal = decode_fixed_hex(&record.principal)?;
    let incarnation = decode_fixed_hex(&record.incarnation)?;
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(&principal);
    bytes.extend_from_slice(&incarnation);
    Some(PublicActorHandle(format!(
        "actor_{}",
        hex_sha256(&bytes)[..32].to_string()
    )))
}

fn decode_fixed_hex(value: &str) -> Option<[u8; 16]> {
    if value.len() != 64 {
        return None;
    }
    let digest = (0..32)
        .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    digest[..16].try_into().ok()
}

fn bearer_token(header: Option<&str>) -> Result<&str, OrchError> {
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
    Ok(token)
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

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bearer_fail_closed() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "tok").unwrap();
        let registry = AuthRegistry::open(root.path(), &[credential], "primary").unwrap();
        assert!(registry
            .authenticate(None, &[AuthCredential::new("primary", "tok").unwrap()])
            .is_err());
        assert!(registry
            .authenticate(
                Some("tok"),
                &[AuthCredential::new("primary", "tok").unwrap()]
            )
            .is_err());
        assert!(registry
            .authenticate(
                Some("Bearer wrong"),
                &[AuthCredential::new("primary", "tok").unwrap()]
            )
            .is_err());
        assert_eq!(
            registry
                .authenticate(
                    Some("Bearer tok"),
                    &[AuthCredential::new("primary", "tok").unwrap()]
                )
                .unwrap()
                .credential_id(),
            "primary"
        );
    }

    #[test]
    fn named_credentials_return_client_identity_and_shared_owner() {
        let credentials = vec![
            AuthCredential::new("primary", "tok").unwrap(),
            AuthCredential::new("laptop", "other-tok").unwrap(),
        ];
        let root = tempdir().unwrap();
        let registry = AuthRegistry::open(root.path(), &credentials, "account-1").unwrap();
        let auth = registry
            .authenticate(Some("Bearer other-tok"), &credentials)
            .unwrap();
        assert_eq!(auth.credential_id(), "laptop");
        assert_eq!(auth.owner_id(), "account-1");
        assert!(registry
            .authenticate(Some("Bearer unknown"), &credentials)
            .is_err());
    }

    #[test]
    fn explicit_same_incarnation_secret_rotation_preserves_actor_binding() {
        let root = tempdir().unwrap();
        let old = AuthCredential::new("primary", "old-secret").unwrap();
        let mut registry = AuthRegistry::open(root.path(), &[old.clone()], "account-1").unwrap();
        let before = registry
            .authenticate(Some("Bearer old-secret"), &[old])
            .unwrap();
        let rotated = AuthCredential::new("primary", "new-secret").unwrap();
        registry
            .set_credentials(std::slice::from_ref(&rotated), "account-1")
            .unwrap();
        let after = registry
            .authenticate(Some("Bearer new-secret"), &[rotated])
            .unwrap();
        assert_eq!(before.actor_handle(), after.actor_handle());
        assert_eq!(before.binding_digest(), after.binding_digest());
        assert!(registry.require_current(&before).is_ok());
    }

    #[test]
    fn credential_replacement_gets_a_new_incarnation_and_cannot_read_old_binding() {
        let root = tempdir().unwrap();
        let old = AuthCredential::new("laptop", "old-secret").unwrap();
        let mut registry = AuthRegistry::open(root.path(), std::slice::from_ref(&old), "owner")
            .unwrap();
        let old_auth = registry
            .authenticate(Some("Bearer old-secret"), std::slice::from_ref(&old))
            .unwrap();
        registry.replace_credential("laptop", "owner").unwrap();
        let replacement = AuthCredential::new("laptop", "replacement-secret").unwrap();
        let new_auth = registry
            .authenticate(
                Some("Bearer replacement-secret"),
                std::slice::from_ref(&replacement),
            )
            .unwrap();
        assert_ne!(old_auth.actor_handle(), new_auth.actor_handle());
        assert!(registry.require_current(&old_auth).is_err());
        registry
            .ensure_resource_binding("run:old", &new_auth)
            .unwrap();
        assert!(registry
            .ensure_resource_binding("run:old", &old_auth)
            .is_err());
    }

    #[test]
    fn owner_change_and_restart_preserve_revocation() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner-a")
                .unwrap();
        let old_auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        registry.change_owner("owner-b").unwrap();
        assert!(registry.require_current(&old_auth).is_err());
        drop(registry);

        let reopened =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner-b").unwrap();
        assert!(reopened.require_current(&old_auth).is_err());
        let new_auth = reopened
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        assert!(reopened.require_current(&new_auth).is_ok());
    }

    #[test]
    fn generation_barrier_invalidates_a_pre_admission_effect_lease() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let mut lease = registry.mint_effect_lease(&auth, "provider:run-1").unwrap();
        registry.rotate_generation("primary").unwrap();
        assert!(registry
            .consume_effect_lease(&auth, &mut lease, "provider:run-1")
            .is_err());
        assert!(!lease.consumed);
    }

    #[test]
    fn public_debug_does_not_reveal_credential_identity_or_owner() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("private-device", "secret").unwrap();
        let registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "private-owner")
                .unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let debug = format!("{auth:?}");
        assert!(!debug.contains("private-device"));
        assert!(!debug.contains("private-owner"));
        assert!(!debug.contains("secret"));
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
