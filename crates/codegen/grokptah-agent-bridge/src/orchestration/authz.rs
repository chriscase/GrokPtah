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
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{OrchError, OrchErrorCode};

const AUTHORITY_SCHEMA_VERSION: u32 = 1;
const AUTHORITY_FILE: &str = "auth-authority.json";
const AUTHORITY_LOCK_FILE: &str = "auth-authority.lock";
const MAX_AUTH_ID_BYTES: usize = 128;
const MAX_AUTH_OWNER_BYTES: usize = 128;
const MAX_EFFECT_LEASE_ID_BYTES: usize = 64;
const MAX_EFFECT_LEASES: usize = 1_024;
const EFFECT_LEASE_TTL: Duration = Duration::from_secs(30);

#[cfg(test)]
static TEST_AUTHORITY_FAULT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cfg(test)]
pub(crate) fn set_test_authority_fault(point: Option<&str>) {
    let value = match point {
        None => 0,
        Some("write") => 1,
        Some("file_sync") => 2,
        Some("rename") => 3,
        Some("dir_sync") => 4,
        Some(other) => panic!("unknown authority fault point: {other}"),
    };
    TEST_AUTHORITY_FAULT.store(value, Ordering::Release);
}

fn authority_fault(point: &str) -> Result<(), OrchError> {
    #[cfg(test)]
    {
        let value = match point {
            "write" => 1,
            "file_sync" => 2,
            "rename" => 3,
            "dir_sync" => 4,
            _ => 0,
        };
        if value != 0
            && TEST_AUTHORITY_FAULT
                .compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            return Err(internal_error(format!(
                "injected authority persistence failure at {point}"
            )));
        }
    }
    let _ = point;
    Ok(())
}

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

/// Revision of the host policy that issued an authenticated capability.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PolicyRevision(u64);

impl std::fmt::Debug for PolicyRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PolicyRevision([redacted])")
    }
}

/// Generation of the capability policy used at the authority boundary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityGeneration(u64);

impl std::fmt::Debug for CapabilityGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CapabilityGeneration([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AuthorityStamp {
    principal: PrincipalRef,
    incarnation: CredentialIncarnation,
    generation: AuthenticationGeneration,
    policy_revision: PolicyRevision,
    capability_generation: CapabilityGeneration,
    credential_id: String,
    owner_id: String,
}

impl std::fmt::Debug for AuthorityStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorityStamp")
            .field("principal", &self.principal)
            .field("incarnation", &self.incarnation)
            .field("generation", &self.generation)
            .field("policy_revision", &self.policy_revision)
            .field("capability_generation", &self.capability_generation)
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
        PublicActorHandle(format!("actor_{}", &hex_sha256(&bytes)[..32]))
    }

    pub(crate) fn owner_id(&self) -> &str {
        &self.stamp.owner_id
    }

    /// Opaque stable ownership digest stored alongside durable resources.
    /// Current authentication and policy authority are separate fences.
    pub(crate) fn binding_digest(&self) -> String {
        let mut bytes = Vec::with_capacity(16 + 16);
        bytes.extend_from_slice(&self.stamp.principal.0);
        bytes.extend_from_slice(&self.stamp.incarnation.0);
        hex_sha256(&bytes)
    }

    fn authority_digest(&self) -> String {
        let mut bytes = Vec::with_capacity(16 + 16 + 8 + 8 + 8);
        bytes.extend_from_slice(&self.stamp.principal.0);
        bytes.extend_from_slice(&self.stamp.incarnation.0);
        bytes.extend_from_slice(&self.stamp.generation.0.to_be_bytes());
        bytes.extend_from_slice(&self.stamp.policy_revision.0.to_be_bytes());
        bytes.extend_from_slice(&self.stamp.capability_generation.0.to_be_bytes());
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
pub struct EffectLease {
    lease_id: String,
    binding: String,
    scope: String,
    expires_at: Instant,
    expires_at_unix_ms: u64,
    consumed: Arc<AtomicBool>,
}

impl Clone for EffectLease {
    fn clone(&self) -> Self {
        Self {
            lease_id: self.lease_id.clone(),
            binding: self.binding.clone(),
            scope: self.scope.clone(),
            expires_at: self.expires_at,
            expires_at_unix_ms: self.expires_at_unix_ms,
            consumed: Arc::clone(&self.consumed),
        }
    }
}

impl PartialEq for EffectLease {
    fn eq(&self, other: &Self) -> bool {
        self.lease_id == other.lease_id
            && self.binding == other.binding
            && self.scope == other.scope
            && self.expires_at == other.expires_at
            && self.expires_at_unix_ms == other.expires_at_unix_ms
            && self.consumed.load(Ordering::Acquire) == other.consumed.load(Ordering::Acquire)
    }
}

impl Eq for EffectLease {}

impl EffectLease {
    fn is_locally_consumed(&self) -> bool {
        self.consumed.load(Ordering::Acquire)
    }

    fn mark_consumed(&self) {
        self.consumed.store(true, Ordering::Release);
    }
}

impl std::fmt::Debug for EffectLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EffectLease")
            .field("lease_id", &"[redacted]")
            .field("binding", &"[redacted]")
            .field("scope", &self.scope)
            .field("expires_at", &self.expires_at)
            .field("consumed", &self.is_locally_consumed())
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct SessionBindingReservation {
    transaction_id: String,
    resource: String,
    digest: String,
    inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCredential {
    credential_id: String,
    owner_id: String,
    principal: String,
    incarnation: String,
    generation: u64,
    /// Hash of credential material used only to detect rotation. The material
    /// itself never enters durable authority state.
    #[serde(default)]
    credential_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEffectLease {
    binding: String,
    scope: String,
    expires_at_unix_ms: u64,
    consumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAuthority {
    schema_version: u32,
    next_generation: u64,
    #[serde(default = "default_authority_revision")]
    policy_revision: u64,
    #[serde(default = "default_authority_revision")]
    capability_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_id: Option<String>,
    credentials: Vec<StoredCredential>,
    /// Resource bindings are opaque digests. Keeping this ledger separate
    /// from public record serde prevents internal authority fields from
    /// crossing MCP/SDK/Desktop projections.
    #[serde(default)]
    bindings: BTreeMap<String, String>,
    /// The one-shot effect ledger is durable so a replay in another process
    /// cannot reacquire a lease consumed by this process.
    #[serde(default)]
    effect_leases: BTreeMap<String, StoredEffectLease>,
    /// Session bindings that have been authorized but not yet published.
    /// Service startup resolves these against the session ledger.
    #[serde(default)]
    pending_session_bindings: BTreeMap<String, StoredPendingSessionBinding>,
}

impl Default for StoredAuthority {
    fn default() -> Self {
        Self {
            schema_version: AUTHORITY_SCHEMA_VERSION,
            next_generation: 1,
            policy_revision: 1,
            capability_generation: 1,
            owner_id: None,
            credentials: Vec::new(),
            bindings: BTreeMap::new(),
            effect_leases: BTreeMap::new(),
            pending_session_bindings: BTreeMap::new(),
        }
    }
}

fn default_authority_revision() -> u64 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredPendingSessionBinding {
    session_id: Uuid,
    resource: String,
    digest: String,
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
    pub(crate) fn initialize_host_anchor(root: &Path) -> Result<(), OrchError> {
        let path = root.join(AUTHORITY_FILE);
        if path.is_file() {
            return Ok(());
        }
        std::fs::create_dir_all(root).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("create durable auth authority root: {error}"),
            )
        })?;
        let registry = Self {
            root: root.to_path_buf(),
            state: StoredAuthority::default(),
            durable_error: None,
        };
        let _lock = registry.durable_lock()?;
        registry.persist_state_locked(&registry.state)
    }

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
        let mut registry = Self {
            root: root.to_path_buf(),
            state: StoredAuthority::default(),
            durable_error: None,
        };
        let _lock = registry.durable_lock()?;
        let path = root.join(AUTHORITY_FILE);
        let existed = path.is_file();
        let mut state = registry.read_durable_state_locked()?;
        let previous = state.clone();
        let effective_owner = state
            .owner_id
            .clone()
            .or_else(|| {
                state
                    .credentials
                    .first()
                    .map(|record| record.owner_id.clone())
            })
            .unwrap_or_else(|| owner_id.trim().to_string());
        let changed = reconcile_state(&mut state, credentials, &effective_owner)?;
        if changed || !existed {
            if let Err(error) = registry.persist_state_locked(&state) {
                if existed {
                    let _ = registry.persist_state_locked(&previous);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
                return Err(error);
            }
        }
        registry.state = state;
        Ok(registry)
    }

    fn check_durable_available(&self) -> Result<(), OrchError> {
        if let Some(error) = &self.durable_error {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                format!("durable auth authority is unavailable: {error}"),
            ));
        }
        Ok(())
    }

    fn durable_lock(&self) -> Result<File, OrchError> {
        self.check_durable_available()?;
        std::fs::create_dir_all(&self.root).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("create durable auth authority root: {error}"),
            )
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(AUTHORITY_LOCK_FILE))
            .map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("open durable auth authority lock: {error}"),
                )
            })?;
        lock.lock_exclusive().map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("lock durable auth authority: {error}"),
            )
        })?;
        Ok(lock)
    }

    fn read_durable_state_locked(&self) -> Result<StoredAuthority, OrchError> {
        let path = self.root.join(AUTHORITY_FILE);
        if !path.is_file() {
            if durable_records_exist(&self.root) {
                return Err(OrchError::new(
                    OrchErrorCode::Internal,
                    "durable auth authority is missing; refusing to resurrect stale resources",
                ));
            }
            return Ok(StoredAuthority::default());
        }
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
        Ok(state)
    }

    fn persist_state_locked(&self, state: &StoredAuthority) -> Result<(), OrchError> {
        let path = self.root.join(AUTHORITY_FILE);
        let tmp = self.root.join(format!(
            "auth-authority.json.tmp-{}",
            Uuid::new_v4().simple()
        ));
        let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("serialize durable auth authority: {error}"),
            )
        })?;
        let result = (|| {
            authority_fault("write")?;
            {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&tmp)
                    .map_err(|error| {
                        OrchError::new(
                            OrchErrorCode::Internal,
                            format!("write durable auth authority: {error}"),
                        )
                    })?;
                file.write_all(&bytes).map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("write durable auth authority: {error}"),
                    )
                })?;
                authority_fault("file_sync")?;
                file.sync_all().map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("flush durable auth authority: {error}"),
                    )
                })?;
            }
            if let Some(parent) = path.parent() {
                let directory = std::fs::File::open(parent).map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("open durable auth directory: {error}"),
                    )
                })?;
                authority_fault("dir_sync")?;
                directory.sync_all().map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("flush durable auth directory: {error}"),
                    )
                })?;
            }
            authority_fault("rename")?;
            std::fs::rename(&tmp, &path).map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("commit durable auth authority: {error}"),
                )
            })?;
            if let Some(parent) = path.parent() {
                let directory = std::fs::File::open(parent).map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("open durable auth directory: {error}"),
                    )
                })?;
                authority_fault("dir_sync")?;
                directory.sync_all().map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Internal,
                        format!("flush durable auth directory: {error}"),
                    )
                })?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// Apply an authority mutation against the latest durable state while
    /// holding a stable inter-process lock. The in-memory view is replaced
    /// only after the new state has committed to disk.
    fn transactional<T>(
        &mut self,
        update: impl FnOnce(&mut StoredAuthority) -> Result<T, OrchError>,
    ) -> Result<T, OrchError> {
        let _lock = self.durable_lock()?;
        let mut candidate = self.read_durable_state_locked()?;
        let previous = candidate.clone();
        let value = update(&mut candidate)?;
        prune_effect_leases(&mut candidate, false)?;
        if let Err(error) = self.persist_state_locked(&candidate) {
            if let Err(rollback_error) = self.persist_state_locked(&previous) {
                return Err(internal_error(format!(
                    "{error}; authority rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        self.state = candidate;
        Ok(value)
    }

    fn with_latest_state<T>(
        &mut self,
        operation: impl FnOnce(&StoredAuthority) -> Result<T, OrchError>,
    ) -> Result<T, OrchError> {
        let _lock = self.durable_lock()?;
        let state = self.read_durable_state_locked()?;
        let result = operation(&state);
        self.state = state;
        result
    }

    pub(crate) fn set_credentials(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<(), OrchError> {
        self.transactional(|state| {
            reconcile_state(state, credentials, owner_id)?;
            Ok(())
        })
    }

    /// Replace a credential identity rather than rotating its secret. A
    /// replacement receives a fresh principal and incarnation even when its
    /// textual credential id is unchanged.
    #[allow(dead_code)]
    pub(crate) fn replace_credential(
        &mut self,
        credential_id: &str,
        owner_id: &str,
    ) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        self.transactional(|state| {
            let Some(index) = state
                .credentials
                .iter()
                .position(|record| record.credential_id == credential_id)
            else {
                return Err(stale_authority());
            };
            let old_authority = authority_digest_for_record(
                &state.credentials[index],
                state.policy_revision,
                state.capability_generation,
            );
            let generation = allocate_generation(state)?;
            state.credentials[index] =
                new_stored_credential(credential_id, owner_id, generation, String::new());
            invalidate_effect_leases(state, &old_authority);
            Ok(())
        })
    }

    pub(crate) fn change_owner(&mut self, owner_id: &str) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        self.transactional(|state| {
            let old_leases = state
                .credentials
                .iter()
                .map(|record| {
                    authority_digest_for_record(
                        record,
                        state.policy_revision,
                        state.capability_generation,
                    )
                })
                .collect::<Vec<_>>();
            state.owner_id = Some(owner_id.to_string());
            if state.credentials.is_empty() {
                return Ok(());
            }
            let old = std::mem::take(&mut state.credentials);
            let mut replacement = Vec::with_capacity(old.len());
            for record in old {
                let generation = allocate_generation(state)?;
                replacement.push(new_stored_credential(
                    &record.credential_id,
                    owner_id,
                    generation,
                    record.credential_fingerprint.clone(),
                ));
            }
            state.credentials = replacement;
            for digest in old_leases {
                invalidate_effect_leases(state, &digest);
            }
            Ok(())
        })
    }

    pub(crate) fn authenticate(
        &mut self,
        header: Option<&str>,
        credentials: &[AuthCredential],
    ) -> Result<AuthContext, OrchError> {
        self.with_latest_state(|state| {
            if credentials.is_empty() || state.credentials.is_empty() {
                return Err(OrchError::new(
                    OrchErrorCode::Internal,
                    "control plane credentials are not configured",
                ));
            }
            let token = bearer_token(header)?;
            let Some((credential, record)) = credentials.iter().find_map(|credential| {
                state
                    .credentials
                    .iter()
                    .find(|record| record.credential_id == credential.id)
                    .filter(|record| {
                        constant_time_eq(token.as_bytes(), credential.token.as_bytes())
                            && (record.credential_fingerprint.is_empty()
                                || record.credential_fingerprint == credential_fingerprint(token))
                    })
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
                    policy_revision: PolicyRevision(state.policy_revision),
                    capability_generation: CapabilityGeneration(state.capability_generation),
                    credential_id: credential.id.clone(),
                    owner_id: record.owner_id.clone(),
                },
                delegation: None,
            })
        })
    }

    pub(crate) fn require_current(&mut self, auth: &AuthContext) -> Result<(), OrchError> {
        self.with_latest_state(|state| require_current_state(state, auth))
    }

    pub(crate) fn public_actor_handle(&mut self, credential_id: &str) -> Option<PublicActorHandle> {
        self.with_latest_state(|state| {
            Ok(state
                .credentials
                .iter()
                .find(|record| record.credential_id == credential_id)
                .and_then(actor_handle_for_record))
        })
        .ok()
        .flatten()
    }

    pub(crate) fn ensure_resource_binding(
        &mut self,
        resource: &str,
        auth: &AuthContext,
    ) -> Result<(), OrchError> {
        self.claim_resource_bindings(&[resource.to_string()], auth)
    }

    pub(crate) fn claim_resource_bindings(
        &mut self,
        resources: &[String],
        auth: &AuthContext,
    ) -> Result<(), OrchError> {
        for resource in resources {
            if resource.is_empty() || resource.len() > 512 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "authority resource key is empty or exceeds its bound",
                ));
            }
        }
        let resources = resources.to_vec();
        let digest = auth.binding_digest();
        self.transactional(|state| {
            require_current_state(state, auth)?;
            for resource in resources {
                match state.bindings.get(&resource) {
                    Some(existing) if existing == &digest => {}
                    Some(_) => return Err(stale_authority()),
                    None => {
                        state.bindings.insert(resource, digest.clone());
                    }
                }
            }
            Ok(())
        })
    }

    #[allow(dead_code)]
    pub(crate) fn migrate_resource_bindings(
        &mut self,
        resources: &[String],
        from: &AuthContext,
        to: &AuthContext,
    ) -> Result<(), OrchError> {
        for resource in resources {
            if resource.is_empty() || resource.len() > 512 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "authority resource key is empty or exceeds its bound",
                ));
            }
        }
        let resources = resources.to_vec();
        let from_digest = from.binding_digest();
        let to_digest = to.binding_digest();
        self.transactional(|state| {
            require_current_state(state, from)?;
            require_current_state(state, to)?;
            for resource in resources {
                if state.bindings.get(&resource) == Some(&from_digest) {
                    state.bindings.insert(resource, to_digest.clone());
                } else {
                    return Err(stale_authority());
                }
            }
            Ok(())
        })
    }

    pub(crate) fn verify_resource_binding(
        &mut self,
        resource: &str,
        auth: &AuthContext,
    ) -> Result<(), OrchError> {
        if resource.is_empty() || resource.len() > 512 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "authority resource key is empty or exceeds its bound",
            ));
        }
        let resource = resource.to_string();
        let digest = auth.binding_digest();
        self.with_latest_state(|state| {
            require_current_state(state, auth)?;
            match state.bindings.get(&resource) {
                Some(existing) if existing == &digest => Ok(()),
                _ => Err(stale_authority()),
            }
        })
    }

    pub(crate) fn begin_session_binding(
        &mut self,
        session_id: Uuid,
        auth: &AuthContext,
    ) -> Result<SessionBindingReservation, OrchError> {
        let resource = format!("session:{session_id}");
        let digest = auth.binding_digest();
        let transaction_id = Uuid::new_v4().simple().to_string();
        let inserted = self.transactional(|state| {
            require_current_state(state, auth)?;
            match state.bindings.get(&resource) {
                Some(existing) if existing == &digest => Ok(false),
                Some(_) => Err(stale_authority()),
                None => {
                    state.pending_session_bindings.insert(
                        transaction_id.clone(),
                        StoredPendingSessionBinding {
                            session_id,
                            resource: resource.clone(),
                            digest: digest.clone(),
                        },
                    );
                    Ok(true)
                }
            }
        })?;
        Ok(SessionBindingReservation {
            transaction_id,
            resource,
            digest,
            inserted,
        })
    }

    pub(crate) fn commit_session_binding(
        &mut self,
        reservation: &SessionBindingReservation,
    ) -> Result<(), OrchError> {
        if !reservation.inserted {
            return Ok(());
        }
        self.transactional(|state| {
            let Some(pending) = state
                .pending_session_bindings
                .get(&reservation.transaction_id)
            else {
                return Err(stale_authority());
            };
            if pending.resource != reservation.resource || pending.digest != reservation.digest {
                return Err(stale_authority());
            }
            state
                .pending_session_bindings
                .remove(&reservation.transaction_id);
            state
                .bindings
                .insert(reservation.resource.clone(), reservation.digest.clone());
            Ok(())
        })
    }

    pub(crate) fn rollback_session_binding(
        &mut self,
        reservation: &SessionBindingReservation,
    ) -> Result<(), OrchError> {
        if !reservation.inserted {
            return Ok(());
        }
        self.transactional(|state| {
            state
                .pending_session_bindings
                .remove(&reservation.transaction_id);
            if state
                .bindings
                .get(&reservation.resource)
                .is_some_and(|digest| digest == &reservation.digest)
            {
                state.bindings.remove(&reservation.resource);
            }
            Ok(())
        })
    }

    pub(crate) fn recover_pending_session_bindings(
        &mut self,
        sessions: &std::collections::HashSet<Uuid>,
    ) -> Result<(), OrchError> {
        self.transactional(|state| {
            let pending = std::mem::take(&mut state.pending_session_bindings);
            for (_, binding) in pending {
                if sessions.contains(&binding.session_id) {
                    match state.bindings.get(&binding.resource) {
                        Some(existing) if existing != &binding.digest => {
                            return Err(stale_authority());
                        }
                        _ => {
                            state.bindings.insert(binding.resource, binding.digest);
                        }
                    }
                }
            }
            Ok(())
        })
    }

    pub(crate) fn rotate_policy_generation(&mut self) -> Result<(), OrchError> {
        self.transactional(|state| {
            state.policy_revision = state
                .policy_revision
                .checked_add(1)
                .ok_or_else(|| internal_error("policy revision exhausted"))?;
            state.capability_generation = state
                .capability_generation
                .checked_add(1)
                .ok_or_else(|| internal_error("capability generation exhausted"))?;
            state.effect_leases.clear();
            Ok(())
        })
    }

    pub(crate) fn mint_effect_lease(
        &mut self,
        auth: &AuthContext,
        scope: impl Into<String>,
    ) -> Result<EffectLease, OrchError> {
        let scope = scope.into();
        if scope.is_empty() || scope.len() > 512 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "effect lease scope is empty or exceeds its bound",
            ));
        }
        let lease_id = Uuid::new_v4().simple().to_string();
        let binding = auth.authority_digest();
        let expires_at = Instant::now() + EFFECT_LEASE_TTL;
        let expires_at_unix_ms = unix_time_millis()
            .checked_add(
                u64::try_from(EFFECT_LEASE_TTL.as_millis())
                    .map_err(|_| internal_error("effect lease TTL exceeds its bound"))?,
            )
            .ok_or_else(|| internal_error("effect lease expiry overflow"))?;
        let stored = StoredEffectLease {
            binding: binding.clone(),
            scope: scope.clone(),
            expires_at_unix_ms,
            consumed: false,
        };
        self.transactional(|state| {
            require_current_state(state, auth)?;
            prune_effect_leases(state, true)?;
            state.effect_leases.insert(lease_id.clone(), stored);
            Ok(())
        })?;
        Ok(EffectLease {
            lease_id,
            binding,
            scope,
            expires_at,
            expires_at_unix_ms,
            consumed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn consume_effect_lease(
        &mut self,
        auth: &AuthContext,
        lease: &EffectLease,
        scope: &str,
    ) -> Result<(), OrchError> {
        if lease.is_locally_consumed()
            || lease.scope != scope
            || Instant::now() >= lease.expires_at
            || unix_time_millis() >= lease.expires_at_unix_ms
        {
            return Err(stale_authority());
        }
        let scope = scope.to_string();
        let lease_id = lease.lease_id.clone();
        let binding = lease.binding.clone();
        self.transactional(|state| {
            require_current_state(state, auth)?;
            let Some(stored) = state.effect_leases.get_mut(&lease_id) else {
                return Err(stale_authority());
            };
            if stored.consumed
                || stored.scope != scope
                || stored.binding != binding
                || unix_time_millis() >= stored.expires_at_unix_ms
            {
                return Err(stale_authority());
            }
            stored.consumed = true;
            Ok(())
        })?;
        lease.mark_consumed();
        Ok(())
    }

    pub(crate) fn issue_delegation(
        &mut self,
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
        &mut self,
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
        self.transactional(|state| {
            let Some(index) = state
                .credentials
                .iter()
                .position(|record| record.credential_id == credential_id)
            else {
                return Err(stale_authority());
            };
            let old_authority = authority_digest_for_record(
                &state.credentials[index],
                state.policy_revision,
                state.capability_generation,
            );
            let generation = allocate_generation(state)?;
            state.credentials[index].generation = generation;
            invalidate_effect_leases(state, &old_authority);
            Ok(())
        })
    }

    pub(crate) fn owner_id(&self) -> &str {
        self.state
            .owner_id
            .as_deref()
            .or_else(|| {
                self.state
                    .credentials
                    .first()
                    .map(|record| record.owner_id.as_str())
            })
            .unwrap_or("primary")
    }

    pub(crate) fn revoke(&mut self, credential_id: &str) -> Result<(), OrchError> {
        self.transactional(|state| {
            let old_authorities = state
                .credentials
                .iter()
                .filter(|record| record.credential_id == credential_id)
                .map(|record| {
                    authority_digest_for_record(
                        record,
                        state.policy_revision,
                        state.capability_generation,
                    )
                })
                .collect::<Vec<_>>();
            let old_len = state.credentials.len();
            state
                .credentials
                .retain(|record| record.credential_id != credential_id);
            if old_len == state.credentials.len() {
                return Err(stale_authority());
            }
            for digest in old_authorities {
                invalidate_effect_leases(state, &digest);
            }
            Ok(())
        })
    }
}

fn reconcile_state(
    state: &mut StoredAuthority,
    credentials: &[AuthCredential],
    owner_id: &str,
) -> Result<bool, OrchError> {
    validate_owner(owner_id)?;
    validate_credentials(credentials)?;
    let mut changed = false;
    let owner_changed = state
        .credentials
        .iter()
        .any(|record| record.owner_id != owner_id);
    let removed_authorities = state
        .credentials
        .iter()
        .filter(|record| {
            owner_changed
                || !credentials
                    .iter()
                    .any(|credential| credential.id == record.credential_id)
        })
        .map(|record| {
            authority_digest_for_record(record, state.policy_revision, state.capability_generation)
        })
        .collect::<Vec<_>>();
    for digest in removed_authorities {
        invalidate_effect_leases(state, &digest);
    }
    let mut next = Vec::with_capacity(credentials.len());
    for credential in credentials {
        let existing = if owner_changed {
            None
        } else {
            state
                .credentials
                .iter()
                .find(|record| record.credential_id == credential.id)
        };
        let mut record = match existing {
            Some(record) => record.clone(),
            None => {
                changed = true;
                new_stored_credential(
                    &credential.id,
                    owner_id,
                    allocate_generation(state)?,
                    credential_fingerprint(credential.token()),
                )
            }
        };
        let fingerprint = credential_fingerprint(credential.token());
        if !record.credential_fingerprint.is_empty() && record.credential_fingerprint != fingerprint
        {
            let old_owner = ownership_digest_for_record(&record);
            let old_authority = authority_digest_for_record(
                &record,
                state.policy_revision,
                state.capability_generation,
            );
            record.generation = allocate_generation(state)?;
            record.incarnation = hex_sha256(&Uuid::new_v4().into_bytes());
            invalidate_effect_leases(state, &old_authority);
            let new_owner = ownership_digest_for_record(&record);
            migrate_binding_digest(state, &old_owner, &new_owner);
            changed = true;
        }
        if record.credential_fingerprint != fingerprint {
            record.credential_fingerprint = fingerprint;
            changed = true;
        }
        next.push(record);
    }
    if next.len() != state.credentials.len() || next != state.credentials || owner_changed {
        changed = true;
        state.owner_id = Some(owner_id.to_string());
        state.credentials = next;
    }
    if state.owner_id.as_deref() != Some(owner_id) {
        state.owner_id = Some(owner_id.to_string());
        changed = true;
    }
    Ok(changed)
}

fn allocate_generation(state: &mut StoredAuthority) -> Result<u64, OrchError> {
    let generation = state.next_generation;
    state.next_generation = generation
        .checked_add(1)
        .ok_or_else(|| internal_error("authentication generation exhausted"))?;
    Ok(generation)
}

fn require_current_state(state: &StoredAuthority, auth: &AuthContext) -> Result<(), OrchError> {
    let Some(record) = state
        .credentials
        .iter()
        .find(|record| record.credential_id == auth.stamp.credential_id)
    else {
        return Err(stale_authority());
    };
    if auth.matches(record) {
        if auth.stamp.policy_revision == PolicyRevision(state.policy_revision)
            && auth.stamp.capability_generation == CapabilityGeneration(state.capability_generation)
        {
            Ok(())
        } else {
            Err(stale_authority())
        }
    } else {
        Err(stale_authority())
    }
}

fn unix_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn prune_effect_leases(
    state: &mut StoredAuthority,
    require_capacity: bool,
) -> Result<(), OrchError> {
    let now = unix_time_millis();
    state
        .effect_leases
        .retain(|_, lease| lease.expires_at_unix_ms > now);
    if require_capacity && state.effect_leases.len() >= MAX_EFFECT_LEASES {
        let excess = state.effect_leases.len() - (MAX_EFFECT_LEASES - 1);
        let consumed = state
            .effect_leases
            .iter()
            .filter(|(_, lease)| lease.consumed)
            .map(|(id, _)| id.clone())
            .take(excess)
            .collect::<Vec<_>>();
        for lease_id in consumed {
            state.effect_leases.remove(&lease_id);
        }
        if state.effect_leases.len() >= MAX_EFFECT_LEASES {
            return Err(internal_error("effect lease ledger capacity is exhausted"));
        }
    }
    Ok(())
}

fn internal_error(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, message)
}

fn stale_authority() -> OrchError {
    OrchError::new(
        OrchErrorCode::Unauthenticated,
        "authentication authority is stale",
    )
}

fn durable_records_exist(root: &Path) -> bool {
    [
        "runs",
        "work-items",
        "work-attempts",
        "messages",
        "routines",
        "routine-activations",
        "manager-plans",
        "managed-intents",
    ]
    .into_iter()
    .any(|directory| {
        std::fs::read_dir(root.join(directory))
            .ok()
            .is_some_and(|entries| entries.flatten().any(|entry| entry.path().is_file()))
    })
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
    if state.schema_version != AUTHORITY_SCHEMA_VERSION
        || state.next_generation == 0
        || state.policy_revision == 0
        || state.capability_generation == 0
    {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "durable auth authority schema is invalid",
        ));
    }
    if state
        .owner_id
        .as_deref()
        .is_some_and(|owner| validate_owner(owner).is_err())
    {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "durable auth owner is invalid",
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
            || (!record.credential_fingerprint.is_empty()
                && (record.credential_fingerprint.len() != 64
                    || !record
                        .credential_fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())))
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
    if state.effect_leases.len() > MAX_EFFECT_LEASES {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "durable auth effect lease ledger exceeds its bound",
        ));
    }
    for (lease_id, lease) in &state.effect_leases {
        if lease_id.is_empty()
            || lease_id.len() > MAX_EFFECT_LEASE_ID_BYTES
            || lease.binding.len() != 64
            || !lease.binding.bytes().all(|byte| byte.is_ascii_hexdigit())
            || lease.scope.is_empty()
            || lease.scope.len() > 512
            || lease.expires_at_unix_ms == 0
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "durable auth effect lease is invalid",
            ));
        }
    }
    for (transaction_id, pending) in &state.pending_session_bindings {
        if transaction_id.is_empty()
            || transaction_id.len() > MAX_EFFECT_LEASE_ID_BYTES
            || pending.resource.len() > 512
            || !pending.resource.starts_with("session:")
            || pending.digest.len() != 64
            || !pending.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "durable pending session binding is invalid",
            ));
        }
    }
    Ok(())
}

fn new_stored_credential(
    id: &str,
    owner_id: &str,
    generation: u64,
    credential_fingerprint: String,
) -> StoredCredential {
    StoredCredential {
        credential_id: id.to_string(),
        owner_id: owner_id.to_string(),
        principal: hex_sha256(&Uuid::new_v4().into_bytes()),
        incarnation: hex_sha256(&Uuid::new_v4().into_bytes()),
        generation,
        credential_fingerprint,
    }
}

fn credential_fingerprint(token: &str) -> String {
    hex_sha256(token.as_bytes())
}

fn authority_digest_for_record(
    record: &StoredCredential,
    policy_revision: u64,
    capability_generation: u64,
) -> String {
    let Some(principal) = decode_fixed_hex(&record.principal) else {
        return String::new();
    };
    let Some(incarnation) = decode_fixed_hex(&record.incarnation) else {
        return String::new();
    };
    let mut bytes = Vec::with_capacity(16 + 16 + 8 + 8 + 8);
    bytes.extend_from_slice(&principal);
    bytes.extend_from_slice(&incarnation);
    bytes.extend_from_slice(&record.generation.to_be_bytes());
    bytes.extend_from_slice(&policy_revision.to_be_bytes());
    bytes.extend_from_slice(&capability_generation.to_be_bytes());
    hex_sha256(&bytes)
}

fn ownership_digest_for_record(record: &StoredCredential) -> String {
    let Some(principal) = decode_fixed_hex(&record.principal) else {
        return String::new();
    };
    let Some(incarnation) = decode_fixed_hex(&record.incarnation) else {
        return String::new();
    };
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(&principal);
    bytes.extend_from_slice(&incarnation);
    hex_sha256(&bytes)
}

fn migrate_binding_digest(state: &mut StoredAuthority, old: &str, new: &str) {
    if old.is_empty() || old == new {
        return;
    }
    for digest in state.bindings.values_mut() {
        if digest == old {
            *digest = new.to_string();
        }
    }
}

fn invalidate_effect_leases(state: &mut StoredAuthority, authority_digest: &str) {
    if !authority_digest.is_empty() {
        state
            .effect_leases
            .retain(|_, lease| lease.binding != authority_digest);
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
        &hex_sha256(&bytes)[..32]
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
        let Ok(c) = self.resolve_claimed(workspace) else {
            return false;
        };
        self.roots.iter().any(|r| r == &c)
    }

    pub(crate) fn resolve_claimed(&self, workspace: &Path) -> Result<PathBuf, OrchError> {
        let Some(value) = workspace.to_str() else {
            return canonical_workspace(workspace);
        };
        if let Some(handle) = value.strip_prefix("workspace_") {
            let resolved = self.roots.iter().find(|root| {
                workspace_handle(&root.display().to_string()).strip_prefix("workspace_")
                    == Some(handle)
            });
            return resolved.cloned().ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::WorkspaceMismatch,
                    "workspace handle is not allowlisted",
                )
            });
        }
        canonical_workspace(workspace)
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
    let claimed_c = allowlist.resolve_claimed(claimed)?;
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

fn workspace_handle(workspace: &str) -> String {
    let digest = {
        use sha2::{Digest, Sha256};
        let encoded = serde_json::to_string(workspace).unwrap_or_default();
        Sha256::digest(encoded.as_bytes())
    };
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("workspace_{}", &hex[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bearer_fail_closed() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "tok").unwrap();
        let mut registry = AuthRegistry::open(root.path(), &[credential], "primary").unwrap();
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
        assert!(registry
            .authenticate(
                Some("Bearer tok"),
                &[AuthCredential::new("primary", "tok").unwrap()]
            )
            .is_ok());
    }

    #[test]
    fn named_credentials_return_client_identity_and_shared_owner() {
        let credentials = vec![
            AuthCredential::new("primary", "tok").unwrap(),
            AuthCredential::new("laptop", "other-tok").unwrap(),
        ];
        let root = tempdir().unwrap();
        let mut registry = AuthRegistry::open(root.path(), &credentials, "account-1").unwrap();
        let auth = registry
            .authenticate(Some("Bearer other-tok"), &credentials)
            .unwrap();
        assert_eq!(auth.owner_id(), "account-1");
        assert!(registry
            .authenticate(Some("Bearer unknown"), &credentials)
            .is_err());
    }

    #[test]
    fn credential_material_rotation_advances_authority_atomically() {
        let root = tempdir().unwrap();
        let old = AuthCredential::new("primary", "old-secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&old), "account-1").unwrap();
        let before = registry
            .authenticate(Some("Bearer old-secret"), &[old])
            .unwrap();
        registry
            .ensure_resource_binding("run:rotation", &before)
            .unwrap();
        let lease = registry
            .mint_effect_lease(&before, "provider:rotation")
            .unwrap();
        let rotated = AuthCredential::new("primary", "new-secret").unwrap();
        registry
            .set_credentials(std::slice::from_ref(&rotated), "account-1")
            .unwrap();
        let after = registry
            .authenticate(Some("Bearer new-secret"), &[rotated])
            .unwrap();
        assert_ne!(before.actor_handle(), after.actor_handle());
        assert_ne!(before.binding_digest(), after.binding_digest());
        assert!(registry.require_current(&before).is_err());
        assert!(registry.require_current(&after).is_ok());
        assert!(registry
            .ensure_resource_binding("run:rotation", &after)
            .is_ok());
        assert!(registry
            .consume_effect_lease(&after, &lease, "provider:rotation")
            .is_err());
        let durable = std::fs::read_to_string(root.path().join(AUTHORITY_FILE)).unwrap();
        assert!(!durable.contains("old-secret"));
        assert!(!durable.contains("new-secret"));
        assert!(durable.contains("credentialFingerprint"));
    }

    #[test]
    fn policy_rotation_revokes_cached_contexts_and_effect_leases() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        registry
            .ensure_resource_binding("run:policy", &auth)
            .unwrap();
        let lease = registry
            .mint_effect_lease(&auth, "provider:policy")
            .unwrap();

        registry.rotate_policy_generation().unwrap();
        assert!(registry.require_current(&auth).is_err());
        assert!(registry
            .consume_effect_lease(&auth, &lease, "provider:policy")
            .is_err());

        let fresh = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        assert!(registry.require_current(&fresh).is_ok());
        assert!(registry
            .ensure_resource_binding("run:policy", &fresh)
            .is_ok());
    }

    #[test]
    fn pending_session_binding_recovery_commits_or_discards_by_session_truth() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let committed = Uuid::new_v4();
        let discarded = Uuid::new_v4();
        let committed_reservation = registry.begin_session_binding(committed, &auth).unwrap();
        let discarded_reservation = registry.begin_session_binding(discarded, &auth).unwrap();
        assert!(committed_reservation.inserted);
        assert!(discarded_reservation.inserted);

        registry
            .recover_pending_session_bindings(&std::collections::HashSet::from([committed]))
            .unwrap();
        assert!(registry
            .verify_resource_binding(&format!("session:{committed}"), &auth)
            .is_ok());
        assert!(registry
            .verify_resource_binding(&format!("session:{discarded}"), &auth)
            .is_err());
    }

    #[test]
    fn credential_replacement_gets_a_new_incarnation_and_cannot_read_old_binding() {
        let root = tempdir().unwrap();
        let old = AuthCredential::new("laptop", "old-secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&old), "owner").unwrap();
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
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner-a").unwrap();
        let old_auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        registry.change_owner("owner-b").unwrap();
        assert!(registry.require_current(&old_auth).is_err());
        drop(registry);

        let mut reopened =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "primary").unwrap();
        assert!(reopened.require_current(&old_auth).is_err());
        let new_auth = reopened
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        assert_eq!(new_auth.owner_id(), "owner-b");
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
        let lease = registry.mint_effect_lease(&auth, "provider:run-1").unwrap();
        registry.rotate_generation("primary").unwrap();
        assert!(registry
            .consume_effect_lease(&auth, &lease, "provider:run-1")
            .is_err());
        assert!(!lease.is_locally_consumed());
    }

    #[test]
    fn cloned_effect_leases_have_one_durable_winner() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let lease = registry
            .mint_effect_lease(&auth, "provider:run-race")
            .unwrap();
        let detached = EffectLease {
            lease_id: lease.lease_id.clone(),
            binding: lease.binding.clone(),
            scope: lease.scope.clone(),
            expires_at: lease.expires_at,
            expires_at_unix_ms: lease.expires_at_unix_ms,
            consumed: Arc::new(AtomicBool::new(false)),
        };
        let first = Arc::new(std::sync::Mutex::new(
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap(),
        ));
        let second = Arc::new(std::sync::Mutex::new(
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap(),
        ));
        let mut threads = Vec::new();
        for candidate in [lease, detached] {
            let registry = if threads.is_empty() {
                Arc::clone(&first)
            } else {
                Arc::clone(&second)
            };
            let auth = auth.clone();
            threads.push(std::thread::spawn(move || {
                registry
                    .lock()
                    .unwrap()
                    .consume_effect_lease(&auth, &candidate, "provider:run-race")
                    .is_ok()
            }));
        }
        let winners = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "a one-shot effect may have one winner");
        let durable: StoredAuthority = serde_json::from_str(
            &std::fs::read_to_string(root.path().join(AUTHORITY_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            durable
                .effect_leases
                .values()
                .filter(|lease| lease.consumed)
                .count(),
            1
        );
    }

    #[test]
    fn failed_binding_save_keeps_memory_and_disk_at_the_previous_truth() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let path = root.path().join(AUTHORITY_FILE);
        let before = std::fs::read(&path).unwrap();
        set_test_authority_fault(Some("write"));

        assert!(registry
            .ensure_resource_binding("run:rollback", &auth)
            .is_err());
        assert!(!registry.state.bindings.contains_key("run:rollback"));
        assert_eq!(std::fs::read(&path).unwrap(), before);

        registry
            .ensure_resource_binding("run:rollback", &auth)
            .unwrap();
        let reopened =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        assert!(reopened.state.bindings.contains_key("run:rollback"));
    }

    #[test]
    fn authority_write_sync_rename_and_directory_faults_fail_closed() {
        for fault in ["write", "file_sync", "rename", "dir_sync"] {
            let root = tempdir().unwrap();
            let credential = AuthCredential::new("primary", "secret").unwrap();
            let mut registry =
                AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner")
                    .unwrap();
            let auth = registry
                .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
                .unwrap();
            let authority = root.path().join(AUTHORITY_FILE);
            let before = std::fs::read(&authority).unwrap();
            set_test_authority_fault(Some(fault));

            assert!(registry
                .ensure_resource_binding("run:fault", &auth)
                .is_err());
            set_test_authority_fault(None);

            assert!(!registry.state.bindings.contains_key("run:fault"));
            assert_eq!(std::fs::read(&authority).unwrap(), before);
        }
    }

    #[test]
    fn expired_effect_leases_are_pruned_without_reopening_replay() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let lease = registry.mint_effect_lease(&auth, "provider:prune").unwrap();
        let authority = root.path().join(AUTHORITY_FILE);
        let mut durable: StoredAuthority =
            serde_json::from_str(&std::fs::read_to_string(&authority).unwrap()).unwrap();
        durable
            .effect_leases
            .get_mut(&lease.lease_id)
            .unwrap()
            .expires_at_unix_ms = 1;
        std::fs::write(&authority, serde_json::to_vec_pretty(&durable).unwrap()).unwrap();

        registry
            .ensure_resource_binding("run:prune", &auth)
            .unwrap();
        let pruned: StoredAuthority =
            serde_json::from_str(&std::fs::read_to_string(&authority).unwrap()).unwrap();
        assert!(pruned.effect_leases.is_empty());
        assert!(registry
            .consume_effect_lease(&auth, &lease, "provider:prune")
            .is_err());
    }

    #[test]
    fn a_rotated_generation_wins_against_a_stale_cross_process_effect() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut first =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let mut second =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = first
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let lease = first
            .mint_effect_lease(&auth, "provider:rotation-race")
            .unwrap();
        second.rotate_generation("primary").unwrap();
        assert!(first
            .consume_effect_lease(&auth, &lease, "provider:rotation-race")
            .is_err());
        let durable: StoredAuthority = serde_json::from_str(
            &std::fs::read_to_string(root.path().join(AUTHORITY_FILE)).unwrap(),
        )
        .unwrap();
        assert!(durable
            .effect_leases
            .values()
            .all(|stored_lease| !stored_lease.consumed));
    }

    #[test]
    fn two_real_processes_refresh_cached_revocation_before_read() {
        if std::env::var("GROKPTAH_AUTH_CHILD_MODE").is_ok() {
            return;
        }
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let reader_ready = root.path().join("reader-ready");
        let revoked = root.path().join("revoked");
        let exe = std::env::current_exe().unwrap();
        let reader = std::process::Command::new(&exe)
            .args([
                "--exact",
                "orchestration::authz::tests::child_revocation_reader",
                "--nocapture",
            ])
            .env("GROKPTAH_AUTH_CHILD_MODE", "reader")
            .env("GROKPTAH_AUTH_CHILD_ROOT", root.path())
            .env("GROKPTAH_AUTH_READER_READY", &reader_ready)
            .env("GROKPTAH_AUTH_REVOKED", &revoked)
            .spawn()
            .unwrap();
        let rotator = std::process::Command::new(&exe)
            .args([
                "--exact",
                "orchestration::authz::tests::child_revocation_rotator",
                "--nocapture",
            ])
            .env("GROKPTAH_AUTH_CHILD_MODE", "rotator")
            .env("GROKPTAH_AUTH_CHILD_ROOT", root.path())
            .env("GROKPTAH_AUTH_READER_READY", &reader_ready)
            .env("GROKPTAH_AUTH_REVOKED", &revoked)
            .spawn()
            .unwrap();
        let reader_status = reader.wait_with_output().unwrap();
        let rotator_status = rotator.wait_with_output().unwrap();
        assert!(
            reader_status.status.success(),
            "reader process failed: {}",
            String::from_utf8_lossy(&reader_status.stderr)
        );
        assert!(
            rotator_status.status.success(),
            "rotator process failed: {}",
            String::from_utf8_lossy(&rotator_status.stderr)
        );
    }

    #[test]
    fn child_revocation_reader() {
        if std::env::var("GROKPTAH_AUTH_CHILD_MODE").as_deref() != Ok("reader") {
            return;
        }
        let root = PathBuf::from(std::env::var("GROKPTAH_AUTH_CHILD_ROOT").unwrap());
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(&root, std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        std::fs::write(
            std::env::var("GROKPTAH_AUTH_READER_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        let revoked = PathBuf::from(std::env::var("GROKPTAH_AUTH_REVOKED").unwrap());
        for _ in 0..500 {
            if revoked.is_file() {
                assert!(registry.require_current(&auth).is_err());
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("revocation child did not observe the rotator");
    }

    #[test]
    fn child_revocation_rotator() {
        if std::env::var("GROKPTAH_AUTH_CHILD_MODE").as_deref() != Ok("rotator") {
            return;
        }
        let root = PathBuf::from(std::env::var("GROKPTAH_AUTH_CHILD_ROOT").unwrap());
        let ready = PathBuf::from(std::env::var("GROKPTAH_AUTH_READER_READY").unwrap());
        for _ in 0..500 {
            if ready.is_file() {
                let credential = AuthCredential::new("primary", "secret").unwrap();
                let mut registry =
                    AuthRegistry::open(&root, std::slice::from_ref(&credential), "owner").unwrap();
                registry.revoke("primary").unwrap();
                std::fs::write(std::env::var("GROKPTAH_AUTH_REVOKED").unwrap(), b"revoked")
                    .unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("rotator child did not observe the reader");
    }

    #[test]
    fn delegated_authority_is_limited_to_the_exact_host_scope() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("primary", "secret").unwrap();
        let mut registry =
            AuthRegistry::open(root.path(), std::slice::from_ref(&credential), "owner").unwrap();
        let auth = registry
            .authenticate(Some("Bearer secret"), std::slice::from_ref(&credential))
            .unwrap();
        let session_id = Uuid::new_v4();
        let workspace = PathBuf::from("/exact/workspace");
        let delegated = registry
            .issue_delegation(&auth, session_id, workspace.clone(), Some("agent-1".into()))
            .unwrap();
        assert!(delegated
            .require_scope(session_id, &workspace, Some("agent-1"))
            .is_ok());
        assert!(delegated
            .require_scope(session_id, &workspace, Some("agent-2"))
            .is_err());
        assert!(delegated
            .require_scope(Uuid::new_v4(), &workspace, Some("agent-1"))
            .is_err());
    }

    #[test]
    fn public_debug_does_not_reveal_credential_identity_or_owner() {
        let root = tempdir().unwrap();
        let credential = AuthCredential::new("private-device", "secret").unwrap();
        let mut registry = AuthRegistry::open(
            root.path(),
            std::slice::from_ref(&credential),
            "private-owner",
        )
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
