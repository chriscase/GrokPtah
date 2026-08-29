//! The one canonical authority store.
//!
//! There is a single durable root and a single set of receipt types. Gates 1
//! to 4 are operations on this store rather than four cooperating stores, so
//! there is no way for two authority views to disagree.
//!
//! Durability discipline: every mutation takes an exclusive file lock, reads
//! the current state from disk, applies the change, writes a temporary file,
//! fsyncs it, renames it over the root, and fsyncs the directory. A caller
//! that sees `Ok` may rely on the change surviving a crash; a caller that sees
//! [`AuthorityError::Durability`] may rely on no effect having been permitted.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::audit::AuditLog;
use crate::digest::ContentDigest;
use crate::error::AuthorityError;
use crate::ids::*;
use crate::receipt::*;
use crate::state::*;

/// Wall-clock milliseconds since the Unix epoch.
pub(crate) fn unix_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Digest a bearer secret for durable comparison. The secret is never stored.
fn credential_fingerprint(secret: &str) -> String {
    ContentDigest::of_fields(&[("bearer", secret.as_bytes())]).to_hex()
}

fn admin_credential_fingerprint(secret: &str) -> String {
    ContentDigest::of_fields(&[("host-admin-custody-v1", secret.as_bytes())]).to_hex()
}

/// Constant-time comparison for equal-length secrets.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// One credential slot the host offers.
#[derive(Clone)]
pub struct HostCredential {
    id: String,
    secret: String,
}

impl HostCredential {
    pub fn new(id: impl Into<String>, secret: impl Into<String>) -> Result<Self, AuthorityError> {
        let id = id.into().trim().to_string();
        let secret = secret.into();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(AuthorityError::Invalid("credential id"));
        }
        if secret.trim().is_empty() {
            return Err(AuthorityError::Invalid("credential secret"));
        }
        Ok(Self { id, secret })
    }
}

impl std::fmt::Debug for HostCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCredential")
            .field("id", &self.id)
            .field("secret", &"[redacted]")
            .finish()
    }
}

/// Host-owned secret used to establish and later reopen administrative
/// custody of one authority root.
///
/// This is deliberately distinct from served principal credentials. The
/// first opener must prove possession of this secret and only its fingerprint
/// is persisted. Reopening with a caller-selected owner string is impossible.
pub struct HostAdminCredential {
    secret: String,
}

impl HostAdminCredential {
    /// Construct a custody credential. Production callers should load at
    /// least 32 bytes of random material from a mode-0600 host secret.
    pub fn new(secret: impl Into<String>) -> Result<Self, AuthorityError> {
        let secret = secret.into();
        if secret.len() < 32 {
            return Err(AuthorityError::Invalid(
                "admin credential must contain at least 32 bytes",
            ));
        }
        Ok(Self { secret })
    }
}

impl std::fmt::Debug for HostAdminCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostAdminCredential([redacted])")
    }
}

/// Non-forgeable proof of host/operator admin authority.
///
/// Returned exactly once, by [`HostAuthority::open`], and constructible
/// nowhere else: the field is private, and the type is deliberately neither
/// `Clone` nor `Copy` so it cannot be duplicated into a component that was
/// only meant to serve requests.
///
/// Replacing the credential set, rotating the control epoch or the capability
/// generation, and exporting the audit log all require it. A component handed
/// only a `&HostAuthority` can authenticate principals and issue work under
/// them, but cannot replace the authority root out from under them.
#[must_use = "the admin authority is the only way to administer this root"]
pub struct HostAdminAuthority {
    root_binding: ContentDigest,
}

/// A root already has a live holder. Admin authority is scarce: exactly one
/// `HostAuthority` may hold a root at a time, process-wide and machine-wide,
/// so a second caller cannot open the same root to mint itself an admin.
const ADMIN_LOCK_FILE: &str = "admin.lock";

impl std::fmt::Debug for HostAdminAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostAdminAuthority")
    }
}

/// The canonical host authority.
pub struct HostAuthority {
    pub(crate) root: PathBuf,
    root_binding: ContentDigest,
    /// Held for this object's lifetime. Dropping it releases the root.
    _admin_lock: File,
    pub(crate) audit: std::sync::Mutex<AuditLog>,
}

impl std::fmt::Debug for HostAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAuthority")
            .field("root", &"[opaque]")
            .finish_non_exhaustive()
    }
}

impl HostAuthority {
    /// Open or create the authority root at `root`.
    ///
    /// Returns the store together with the single [`HostAdminAuthority`] for
    /// this process. Hand out `&HostAuthority` freely; hand out the admin
    /// authority only to the component that genuinely administers the root.
    pub fn open(
        root: impl AsRef<Path>,
        admin_credential: &HostAdminCredential,
    ) -> Result<(Self, HostAdminAuthority), AuthorityError> {
        let requested_root = root.as_ref();
        let credential_fingerprint = admin_credential_fingerprint(&admin_credential.secret);
        std::fs::create_dir_all(requested_root)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        let root = dunce::canonicalize(requested_root)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        let root_binding = ContentDigest::of_fields(&[
            ("authority-root", root.as_os_str().as_encoded_bytes()),
            ("custody", credential_fingerprint.as_bytes()),
        ]);
        // Take the root exclusively before reading anything. Admin authority is
        // handed out by this call, so a second holder would be a second admin.
        let admin_lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.join(ADMIN_LOCK_FILE))
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        admin_lock.try_lock_exclusive().map_err(|_| {
            AuthorityError::Durability(
                "authority root is already held by another host; admin authority is exclusive"
                    .into(),
            )
        })?;
        let audit = AuditLog::open(&root)?;
        // Evidence of prior service with no authority root means the root was
        // removed under us. Minting a fresh lineage here would silently orphan
        // every record the old lineage produced and let removed credentials
        // come back as new ones, so refuse instead of quietly re-establishing.
        // Cheap presence check rather than a full parse: a damaged log must
        // still open far enough for an operator to inspect it, while appends
        // stay refused and the chain reports itself broken.
        if !root.join("authority.json").exists() && audit.has_content()? {
            return Err(AuthorityError::CorruptState(
                "authority root is missing but prior audit evidence exists".into(),
            ));
        }
        let this = Self {
            root,
            root_binding,
            _admin_lock: admin_lock,
            audit: std::sync::Mutex::new(audit),
        };
        // Establish the root if absent, and advance the control epoch for this
        // host incarnation so work admitted by a previous one cannot complete.
        this.with_state_init(&credential_fingerprint, |state| {
            state.control_epoch = state
                .control_epoch
                .checked_add(1)
                .ok_or_else(|| AuthorityError::Durability("control epoch exhausted".into()))?;
            Ok(())
        })?;
        // A previous post-dispatch audit append may have succeeded immediately
        // before its state write failed. Replay the typed WAL before exposing
        // this incarnation, so state and audit converge rather than recovery
        // incorrectly downgrading a recorded outcome to crash ambiguity.
        this.replay_attempt_settlements()?;
        Ok((this, HostAdminAuthority { root_binding }))
    }

    pub(crate) fn state_path(&self) -> PathBuf {
        self.root.join("authority.json")
    }

    pub(crate) fn lock_path(&self) -> PathBuf {
        self.root.join("authority.lock")
    }

    pub(crate) fn lock(&self) -> Result<File, AuthorityError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path())
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        file.lock_exclusive()
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        Ok(file)
    }

    pub(crate) fn read_state(&self) -> Result<Option<StoredAuthority>, AuthorityError> {
        let text = match std::fs::read_to_string(self.state_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AuthorityError::Durability(e.to_string())),
        };
        let state: StoredAuthority =
            serde_json::from_str(&text).map_err(|e| AuthorityError::CorruptState(e.to_string()))?;
        if state.schema_version != SCHEMA_VERSION {
            return Err(AuthorityError::CorruptState(format!(
                "unsupported authority schema version {}",
                state.schema_version
            )));
        }
        Ok(Some(state))
    }

    pub(crate) fn persist(&self, state: &StoredAuthority) -> Result<(), AuthorityError> {
        let text = serde_json::to_string_pretty(state)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        let tmp = self.root.join("authority.json.tmp");
        {
            let mut f =
                File::create(&tmp).map_err(|e| AuthorityError::Durability(e.to_string()))?;
            f.write_all(text.as_bytes())
                .map_err(|e| AuthorityError::Durability(e.to_string()))?;
            f.sync_all()
                .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        }
        std::fs::rename(&tmp, self.state_path())
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        // fsync the directory so the rename itself is durable.
        let dir = File::open(&self.root).map_err(|e| AuthorityError::Durability(e.to_string()))?;
        dir.sync_all()
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        Ok(())
    }

    fn with_state_init<T>(
        &self,
        credential_fingerprint: &str,
        f: impl FnOnce(&mut StoredAuthority) -> Result<T, AuthorityError>,
    ) -> Result<T, AuthorityError> {
        let _guard = self.lock()?;
        let mut state = match self.read_state()? {
            Some(state) => {
                if !constant_time_eq(
                    state.admin_credential_fingerprint.as_bytes(),
                    credential_fingerprint.as_bytes(),
                ) {
                    return Err(AuthorityError::Unauthenticated);
                }
                state
            }
            None => {
                StoredAuthority::new(PrincipalId::mint().to_hex(), credential_fingerprint.into())
            }
        };
        let out = f(&mut state)?;
        self.persist(&state)?;
        Ok(out)
    }

    pub(crate) fn require_admin(&self, admin: &HostAdminAuthority) -> Result<(), AuthorityError> {
        if admin.root_binding == self.root_binding {
            Ok(())
        } else {
            Err(AuthorityError::Unauthenticated)
        }
    }

    /// Run a mutation under the lock against existing state.
    pub(crate) fn with_state<T>(
        &self,
        f: impl FnOnce(&mut StoredAuthority) -> Result<T, AuthorityError>,
    ) -> Result<T, AuthorityError> {
        let _guard = self.lock()?;
        let mut state = self
            .read_state()?
            .ok_or_else(|| AuthorityError::CorruptState("authority root is missing".into()))?;
        let out = f(&mut state)?;
        self.persist(&state)?;
        Ok(out)
    }

    /// Read state under the lock without mutating.
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&StoredAuthority) -> Result<T, AuthorityError>,
    ) -> Result<T, AuthorityError> {
        let _guard = self.lock()?;
        let state = self
            .read_state()?
            .ok_or_else(|| AuthorityError::CorruptState("authority root is missing".into()))?;
        f(&state)
    }

    // ───────────────────────── Gate 1: principal root ─────────────────────────

    /// Install the host's credential set.
    ///
    /// The host is the only issuer of principal identity. For each slot:
    ///
    /// * a slot the store has never seen mints a fresh principal, incarnation,
    ///   and authentication generation;
    /// * a slot whose secret changed mints a fresh **incarnation and
    ///   generation** while keeping the principal, and invalidates every
    ///   capability, lease, and resource binding held under the old
    ///   incarnation — this is what stops a captured bearer from being
    ///   resurrected by re-installing a credential under the same name;
    /// * a slot that disappears has its principal's derived authority revoked.
    ///
    /// There is no path that adopts a new secret onto an existing generation.
    pub fn set_credentials(
        &self,
        admin: &HostAdminAuthority,
        credentials: &[HostCredential],
    ) -> Result<(), AuthorityError> {
        self.require_admin(admin)?;
        if credentials.is_empty() {
            return Err(AuthorityError::Invalid("credential set"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in credentials {
            if !seen.insert(c.id.clone()) {
                return Err(AuthorityError::Invalid("duplicate credential id"));
            }
        }
        self.with_state(|state| {
            let mut next: Vec<StoredCredential> = Vec::with_capacity(credentials.len());
            let mut invalidated: Vec<String> = Vec::new();

            for credential in credentials {
                let fingerprint = credential_fingerprint(&credential.secret);
                // Clone out first: allocating a generation needs `state`
                // mutably, so no borrow of `state.credentials` may be live.
                let existing = state
                    .credentials
                    .iter()
                    .find(|r| r.credential_id == credential.id)
                    .cloned();
                let record = match existing {
                    // Same slot and same secret: authority continues.
                    Some(r) if r.credential_fingerprint == fingerprint => r,
                    // Same slot, different secret: rotate incarnation and
                    // generation, and invalidate everything held under the old
                    // incarnation.
                    Some(r) => {
                        invalidated.push(r.incarnation.clone());
                        let generation = allocate_auth_generation(state)?;
                        StoredCredential {
                            credential_id: r.credential_id,
                            principal: r.principal,
                            incarnation: CredentialIncarnation::mint().to_hex(),
                            auth_generation: generation,
                            credential_fingerprint: fingerprint,
                            owner_id: state.owner_id.clone(),
                        }
                    }
                    // A new slot receives a wholly new principal.
                    None => {
                        let generation = allocate_auth_generation(state)?;
                        StoredCredential {
                            credential_id: credential.id.clone(),
                            principal: PrincipalId::mint().to_hex(),
                            incarnation: CredentialIncarnation::mint().to_hex(),
                            auth_generation: generation,
                            credential_fingerprint: fingerprint,
                            owner_id: state.owner_id.clone(),
                        }
                    }
                };
                next.push(record);
            }

            // Any credential that is gone loses its derived authority too.
            for old in &state.credentials {
                if !next.iter().any(|r| r.incarnation == old.incarnation) {
                    invalidated.push(old.incarnation.clone());
                }
            }

            for incarnation in &invalidated {
                invalidate_incarnation(state, incarnation);
            }

            state.credentials = next;
            Ok(())
        })
    }

    /// Authenticate a bearer against the durable credential set.
    ///
    /// The durable record is the authority: the fingerprint stored there must
    /// match the presented secret. There is no "fingerprint absent, accept
    /// anything" branch, and no caller-supplied credential list to compare
    /// against.
    pub fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthorityError> {
        let presented = bearer.trim();
        let token = presented
            .strip_prefix("Bearer ")
            .or_else(|| presented.strip_prefix("bearer "))
            .unwrap_or(presented)
            .trim();
        if token.is_empty() {
            return Err(AuthorityError::Unauthenticated);
        }
        let fingerprint = credential_fingerprint(token);
        let auth = self.read(|state| {
            if state.credentials.is_empty() {
                return Err(AuthorityError::Unauthenticated);
            }
            let record = state
                .credentials
                .iter()
                .find(|r| {
                    constant_time_eq(r.credential_fingerprint.as_bytes(), fingerprint.as_bytes())
                })
                .ok_or(AuthorityError::Unauthenticated)?;
            Ok(AuthContext {
                principal: decode_id(&record.principal, "principal")?,
                incarnation: decode_id(&record.incarnation, "incarnation")?,
                auth_generation: AuthGeneration::from_raw(record.auth_generation),
                capability_generation: CapabilityGeneration::from_raw(state.capability_generation),
                control_epoch: ControlEpoch::from_raw(state.control_epoch),
                credential_id: record.credential_id.clone(),
                owner_id: record.owner_id.clone(),
            })
        })?;
        // An authentication that cannot be recorded grants nothing: the
        // context is discarded rather than returned unaudited.
        self.append_audit(
            auth.control_epoch.raw(),
            crate::audit::AuditEvent::Authenticated {
                principal: auth.principal.public_handle(),
            },
        )?;
        Ok(auth)
    }

    /// Confirm a previously minted context is still current.
    pub fn require_current(&self, auth: &AuthContext) -> Result<(), AuthorityError> {
        self.read(|state| require_current_state(state, auth))
    }

    /// Issue a host-owned session for an authenticated principal.
    pub fn issue_session(&self, auth: &AuthContext) -> Result<SessionId, AuthorityError> {
        self.with_state(|state| {
            require_current_state(state, auth)?;
            Ok(SessionId::mint())
        })
    }

    /// Issue a host-owned workspace identity for a canonical path.
    ///
    /// The path is digested, never stored, so the durable root and every
    /// projection stay path-free.
    pub fn issue_workspace(
        &self,
        auth: &AuthContext,
        canonical_path: &Path,
    ) -> Result<WorkspaceId, AuthorityError> {
        let bytes = canonical_path.as_os_str().as_encoded_bytes().to_vec();
        self.with_state(|state| {
            require_current_state(state, auth)?;
            if bytes.is_empty() {
                return Err(AuthorityError::Invalid("workspace path"));
            }
            // Deterministic in the path so the same workspace keeps one identity.
            let digest = ContentDigest::of_fields(&[("workspace", &bytes)]);
            let mut id = [0u8; 16];
            id.copy_from_slice(&digest.as_bytes()[..16]);
            Ok(WorkspaceId::from_bytes(id))
        })
    }

    /// Issue a resource incarnation.
    ///
    /// This is the only way a resource comes into existence. A caller cannot
    /// name an unknown resource into being: [`Self::resource_binding`] returns
    /// [`AuthorityError::UnknownResource`] for anything the host did not issue
    /// here, which closes the caller-first-claim hole.
    pub fn issue_resource(
        &self,
        auth: &AuthContext,
        session: SessionId,
        workspace: WorkspaceId,
        initial_observation: ContentDigest,
    ) -> Result<ResourceIncarnation, AuthorityError> {
        let issued = self.with_state(|state| {
            require_current_state(state, auth)?;
            let incarnation = ResourceIncarnation::mint();
            let record = StoredResource {
                incarnation: incarnation.to_hex(),
                principal: auth.principal.to_hex(),
                credential_incarnation: auth.incarnation.to_hex(),
                auth_generation: auth.auth_generation.raw(),
                session: session.to_hex(),
                workspace: workspace.to_hex(),
                control_epoch: state.control_epoch,
                observation_revision: 1,
                observation_digest: initial_observation.to_hex(),
            };
            state.resources.insert(incarnation.to_hex(), record);
            Ok((incarnation, session, workspace))
        })?;
        let (incarnation, session, workspace) = issued;
        self.append_audit(
            auth.control_epoch.raw(),
            crate::audit::AuditEvent::ResourceIssued {
                resource: incarnation.public_handle(),
                principal: auth.principal.public_handle(),
                session: session.public_handle(),
                workspace: workspace.public_handle(),
            },
        )?;
        Ok(incarnation)
    }

    /// The binding a host-issued resource carries.
    pub fn resource_binding(
        &self,
        auth: &AuthContext,
        resource: ResourceIncarnation,
    ) -> Result<AuthorityBinding, AuthorityError> {
        self.read(|state| {
            require_current_state(state, auth)?;
            let record = state
                .resources
                .get(&resource.to_hex())
                .ok_or(AuthorityError::UnknownResource)?;
            binding_from_resource(state, auth, record)
        })
    }

    /// Record a new accepted observation of a governed surface.
    ///
    /// Advancing the revision invalidates any lease minted against an older
    /// one, so a queued action cannot be applied to a surface that moved.
    pub fn record_observation(
        &self,
        auth: &AuthContext,
        resource: ResourceIncarnation,
        observation: ContentDigest,
    ) -> Result<ObservationRevision, AuthorityError> {
        self.with_state(|state| {
            require_current_state(state, auth)?;
            let control_epoch = state.control_epoch;
            let record = state
                .resources
                .get_mut(&resource.to_hex())
                .ok_or(AuthorityError::UnknownResource)?;
            if record.principal != auth.principal.to_hex()
                || record.credential_incarnation != auth.incarnation.to_hex()
            {
                return Err(AuthorityError::ResourceOwnershipMismatch);
            }
            if record.control_epoch != control_epoch {
                return Err(AuthorityError::StaleControlEpoch);
            }
            record.observation_revision =
                record.observation_revision.checked_add(1).ok_or_else(|| {
                    AuthorityError::Durability("observation revision exhausted".into())
                })?;
            record.observation_digest = observation.to_hex();
            Ok(ObservationRevision::from_raw(record.observation_revision))
        })
    }

    /// Rotate the control epoch, retiring every in-flight admission.
    pub fn rotate_control_epoch(
        &self,
        admin: &HostAdminAuthority,
    ) -> Result<ControlEpoch, AuthorityError> {
        self.require_admin(admin)?;
        self.with_state(|state| {
            state.control_epoch = state
                .control_epoch
                .checked_add(1)
                .ok_or_else(|| AuthorityError::Durability("control epoch exhausted".into()))?;
            state.capabilities.clear();
            state.leases.clear();
            Ok(ControlEpoch::from_raw(state.control_epoch))
        })
    }

    /// Rotate the capability generation, invalidating every sealed grant.
    pub fn rotate_capability_generation(
        &self,
        admin: &HostAdminAuthority,
    ) -> Result<CapabilityGeneration, AuthorityError> {
        self.require_admin(admin)?;
        self.with_state(|state| {
            state.capability_generation =
                state.capability_generation.checked_add(1).ok_or_else(|| {
                    AuthorityError::Durability("capability generation exhausted".into())
                })?;
            state.capabilities.clear();
            state.leases.clear();
            Ok(CapabilityGeneration::from_raw(state.capability_generation))
        })
    }

    /// The owner account this root serves. An operator read.
    pub fn owner_id(&self, admin: &HostAdminAuthority) -> Result<String, AuthorityError> {
        self.require_admin(admin)?;
        self.read(|state| Ok(state.owner_id.clone()))
    }
}

pub(crate) fn allocate_auth_generation(state: &mut StoredAuthority) -> Result<u64, AuthorityError> {
    let generation = state.next_auth_generation;
    state.next_auth_generation = generation
        .checked_add(1)
        .ok_or_else(|| AuthorityError::Durability("authentication generation exhausted".into()))?;
    Ok(generation)
}

/// Drop every capability, lease, and resource derived from an incarnation.
pub(crate) fn invalidate_incarnation(state: &mut StoredAuthority, incarnation: &str) {
    let dead_resources: Vec<String> = state
        .resources
        .iter()
        .filter(|(_, r)| r.credential_incarnation == incarnation)
        .map(|(k, _)| k.clone())
        .collect();
    for key in &dead_resources {
        state.resources.remove(key);
    }
    let dead_caps: Vec<String> = state
        .capabilities
        .iter()
        .filter(|(_, c)| c.credential_incarnation == incarnation)
        .map(|(k, _)| k.clone())
        .collect();
    for key in &dead_caps {
        state.capabilities.remove(key);
    }
    let dead_leases: Vec<String> = state
        .leases
        .iter()
        .filter(|(_, l)| l.credential_incarnation == incarnation)
        .map(|(k, _)| k.clone())
        .collect();
    for key in &dead_leases {
        state.leases.remove(key);
    }
}

pub(crate) fn require_current_state(
    state: &StoredAuthority,
    auth: &AuthContext,
) -> Result<(), AuthorityError> {
    let record = state
        .credentials
        .iter()
        .find(|r| r.principal == auth.principal.to_hex())
        .ok_or(AuthorityError::StalePrincipal)?;
    if record.incarnation != auth.incarnation.to_hex()
        || record.auth_generation != auth.auth_generation.raw()
    {
        return Err(AuthorityError::StalePrincipal);
    }
    if state.capability_generation != auth.capability_generation.raw() {
        return Err(AuthorityError::StaleCapability);
    }
    if state.control_epoch != auth.control_epoch.raw() {
        return Err(AuthorityError::StaleControlEpoch);
    }
    Ok(())
}

pub(crate) fn binding_from_resource(
    state: &StoredAuthority,
    auth: &AuthContext,
    record: &StoredResource,
) -> Result<AuthorityBinding, AuthorityError> {
    if record.principal != auth.principal.to_hex()
        || record.credential_incarnation != auth.incarnation.to_hex()
        || record.auth_generation != auth.auth_generation.raw()
    {
        return Err(AuthorityError::ResourceOwnershipMismatch);
    }
    if record.control_epoch != state.control_epoch {
        return Err(AuthorityError::StaleControlEpoch);
    }
    Ok(AuthorityBinding {
        principal: auth.principal,
        incarnation: auth.incarnation,
        auth_generation: auth.auth_generation,
        capability_generation: CapabilityGeneration::from_raw(state.capability_generation),
        session: decode_id(&record.session, "session")?,
        workspace: decode_id(&record.workspace, "workspace")?,
        resource: decode_id(&record.incarnation, "resource")?,
        control_epoch: ControlEpoch::from_raw(state.control_epoch),
    })
}

/// Decode a 16-byte hex identifier from a durable record.
pub(crate) fn decode_id<T: FromHex16>(
    value: &str,
    what: &'static str,
) -> Result<T, AuthorityError> {
    if value.len() != 32 {
        return Err(AuthorityError::CorruptState(format!(
            "durable {what} identifier is malformed"
        )));
    }
    let mut bytes = [0u8; 16];
    for (i, chunk) in value.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or_else(|| AuthorityError::CorruptState(format!("durable {what} is not hex")))?;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or_else(|| AuthorityError::CorruptState(format!("durable {what} is not hex")))?;
        bytes[i] = ((hi << 4) | lo) as u8;
    }
    Ok(T::from_hex16(bytes))
}

/// Rebuild an opaque identifier from durable bytes.
pub trait FromHex16: Sized {
    #[doc(hidden)]
    fn from_hex16(bytes: [u8; 16]) -> Self;
}

macro_rules! impl_from_hex16 {
    ($($t:ty),*) => {
        $(impl FromHex16 for $t {
            fn from_hex16(bytes: [u8; 16]) -> Self { <$t>::from_bytes(bytes) }
        })*
    };
}

impl_from_hex16!(
    PrincipalId,
    CredentialIncarnation,
    ResourceIncarnation,
    CapabilityId,
    EffectLeaseId,
    AttemptId,
    SessionId,
    WorkspaceId
);

/// Decode a 32-byte hex content digest from a durable record.
pub(crate) fn decode_digest(
    value: &str,
    what: &'static str,
) -> Result<ContentDigest, AuthorityError> {
    if value.len() != 64 {
        return Err(AuthorityError::CorruptState(format!(
            "durable {what} digest is malformed"
        )));
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in value.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char)
            .to_digit(16)
            .ok_or_else(|| AuthorityError::CorruptState(format!("durable {what} is not hex")))?;
        let lo = (chunk[1] as char)
            .to_digit(16)
            .ok_or_else(|| AuthorityError::CorruptState(format!("durable {what} is not hex")))?;
        bytes[i] = ((hi << 4) | lo) as u8;
    }
    Ok(ContentDigest::from_bytes(bytes))
}
