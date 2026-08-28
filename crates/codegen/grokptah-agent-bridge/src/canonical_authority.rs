//! Canonical host-issued authority spine (G1–G4).
//!
//! One opaque principal, one durable owner, atomic single-writer persistence,
//! process-generation recovery, and creation-bound resource admission.
//! Public projections cannot mint or replay this type.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::durable_fs::{
    atomic_write_bytes, ensure_secure_dir, migration_label, quarantine, reject_insecure_path,
    restrict_mode,
};
use crate::orchestration::{
    canonical_workspace, match_bearer, AuthCredential, OrchError, OrchErrorCode,
};

const AUTHORITY_FILE: &str = "authority.json";
const LOCK_FILE: &str = "authority.lock";
const SCHEMA: u32 = 1;

/// Host-issued opaque principal. Not serializable; JSON cannot become one.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PrincipalRef([u8; 16]);

impl std::fmt::Debug for PrincipalRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrincipalRef([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CredentialIncarnation([u8; 16]);

impl std::fmt::Debug for CredentialIncarnation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialIncarnation([redacted])")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AuthenticationGeneration(u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ProcessGeneration(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Session,
    Run,
    Work,
    WorkAttempt,
    Agent,
    Workspace,
    ComputerRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PrincipalClass {
    Owner,
    InternalHealth,
    InternalExecutor,
}

/// Correlation handle. Cannot construct [`IssuedAuth`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicActorHandle(String);

impl PublicActorHandle {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Host-issued capability. No serde; Clone cannot change identity.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedAuth {
    principal: PrincipalRef,
    incarnation: CredentialIncarnation,
    generation: AuthenticationGeneration,
    process_generation: ProcessGeneration,
    credential_id: String,
    owner_id: String,
    class: PrincipalClass,
}

impl std::fmt::Debug for IssuedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedAuth")
            .field("principal", &self.principal)
            .field("credential_id", &"[redacted]")
            .field("class", &self.class)
            .finish()
    }
}

impl IssuedAuth {
    pub fn token_id(&self) -> &str {
        &self.credential_id
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn actor_handle(&self) -> PublicActorHandle {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.principal.0);
        bytes.extend_from_slice(&self.incarnation.0);
        PublicActorHandle(format!("actor_{}", &hex_sha256(&bytes)[..32]))
    }

    pub fn binding_digest(&self) -> String {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.principal.0);
        bytes.extend_from_slice(&self.incarnation.0);
        hex_sha256(&bytes)
    }

    pub fn is_internal(&self) -> bool {
        !matches!(self.class, PrincipalClass::Owner)
    }
}

/// In-process admission handle. Private issued stamp cannot be minted from
/// public `token_id` / `owner_id` strings or JSON.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthContext {
    issued: IssuedAuth,
    /// Attribution identity. Not a secret and may appear in durable records.
    pub token_id: String,
    /// Durable workspace owner identity. Not a secret.
    pub owner_id: String,
}

impl std::fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthContext")
            .field("token_id", &self.token_id)
            .field("owner_id", &"[redacted]")
            .finish()
    }
}

impl AuthContext {
    pub(crate) fn from_issued(issued: IssuedAuth) -> Self {
        Self {
            token_id: issued.token_id().to_string(),
            owner_id: issued.owner_id().to_string(),
            issued,
        }
    }

    pub fn issued(&self) -> &IssuedAuth {
        &self.issued
    }

    pub fn actor_handle(&self) -> PublicActorHandle {
        self.issued.actor_handle()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredCredential {
    credential_id: String,
    owner_id: String,
    principal: String,
    incarnation: String,
    generation: u64,
    class: PrincipalClass,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredBinding {
    kind: ResourceKind,
    resource_id: String,
    binding_digest: String,
    session_id: String,
    workspace_fingerprint: String,
    created_generation: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredAuthority {
    schema_version: u32,
    migration_label: String,
    owner_id: Option<String>,
    next_generation: u64,
    process_generation: u64,
    writer_pid: u32,
    process_nonce: String,
    credentials: Vec<StoredCredential>,
    bindings: Vec<StoredBinding>,
}

pub struct HostAuthority {
    root: PathBuf,
    _lock: File,
    state: StoredAuthority,
}

impl std::fmt::Debug for HostAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAuthority")
            .field("process_generation", &self.state.process_generation)
            .finish_non_exhaustive()
    }
}

impl HostAuthority {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, OrchError> {
        let root = root.as_ref().join("canonical-authority");
        ensure_secure_dir(&root)?;
        let lock_path = root.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                OrchError::new(
                    OrchErrorCode::Internal,
                    format!("open authority lock: {error}"),
                )
            })?;
        restrict_mode(&lock_path, false)?;
        reject_insecure_path(&lock_path)?;
        lock.try_lock_exclusive().map_err(|_| {
            OrchError::new(
                OrchErrorCode::Conflict,
                "canonical authority already has a live writer",
            )
        })?;
        let path = root.join(AUTHORITY_FILE);
        let mut state = if path.exists() {
            reject_insecure_path(&path)?;
            match load_state(&path) {
                Ok(state) => state,
                Err(error) => {
                    let _ = quarantine(&path, "corrupt");
                    return Err(error);
                }
            }
        } else {
            StoredAuthority {
                schema_version: SCHEMA,
                migration_label: migration_label().into(),
                owner_id: None,
                next_generation: 1,
                process_generation: 0,
                writer_pid: std::process::id(),
                process_nonce: Uuid::new_v4().to_string(),
                credentials: Vec::new(),
                bindings: Vec::new(),
            }
        };
        if state.migration_label != migration_label() || state.schema_version != SCHEMA {
            if path.exists() {
                let _ = quarantine(&path, "legacy");
            }
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "canonical authority migration label is not authenticated",
            ));
        }
        state.process_generation = state.process_generation.checked_add(1).ok_or_else(|| {
            OrchError::new(OrchErrorCode::Internal, "process generation exhausted")
        })?;
        state.writer_pid = std::process::id();
        state.process_nonce = Uuid::new_v4().to_string();
        let mut host = Self {
            root,
            _lock: lock,
            state: state.clone(),
        };
        host.commit(state)?;
        Ok(host)
    }

    pub fn install_credentials(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<(), OrchError> {
        self.install_credentials_inner(credentials, owner_id, false)
    }

    pub fn replace_secrets(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<(), OrchError> {
        self.install_credentials_inner(credentials, owner_id, true)
    }

    fn install_credentials_inner(
        &mut self,
        credentials: &[AuthCredential],
        owner_id: &str,
        rotate_existing: bool,
    ) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        let mut next = self.state.clone();
        if let Some(existing) = &next.owner_id {
            if existing != owner_id {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "durable workspace owner cannot be first-claimed by another principal",
                ));
            }
        } else {
            next.owner_id = Some(owner_id.to_string());
        }
        let mut rebuilt = Vec::new();
        let mut rotate_ids: Vec<String> = Vec::new();
        for credential in credentials {
            if let Some(existing) = next
                .credentials
                .iter()
                .find(|record| record.credential_id == credential.id)
            {
                rebuilt.push(existing.clone());
                if rotate_existing {
                    rotate_ids.push(credential.id.clone());
                }
            } else {
                rebuilt.push(mint_credential(
                    &mut next,
                    &credential.id,
                    owner_id,
                    PrincipalClass::Owner,
                )?);
            }
        }
        next.credentials = rebuilt;
        self.commit(next)?;
        for credential_id in rotate_ids {
            self.rotate_generation(&credential_id)?;
        }
        Ok(())
    }

    pub fn authenticate(
        &mut self,
        header: Option<&str>,
        credentials: &[AuthCredential],
        owner_id: &str,
    ) -> Result<IssuedAuth, OrchError> {
        let matched = match_bearer(header, credentials)?;
        self.require_owner(owner_id)?;
        let record = self
            .state
            .credentials
            .iter()
            .find(|record| record.credential_id == matched.id && record.owner_id == owner_id)
            .ok_or_else(|| unauth("credential is not the current durable incarnation"))?;
        self.issue_from_record(record)
    }

    pub fn rotate_generation(&mut self, credential_id: &str) -> Result<(), OrchError> {
        let mut next = self.state.clone();
        let generation = allocate(&mut next.next_generation)?;
        let record = next
            .credentials
            .iter_mut()
            .find(|record| record.credential_id == credential_id)
            .ok_or_else(|| unauth("unknown credential"))?;
        if generation <= record.generation {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "generation must be strictly newer",
            ));
        }
        record.generation = generation;
        self.commit(next)
    }

    pub fn replace_credential(
        &mut self,
        credential_id: &str,
        owner_id: &str,
    ) -> Result<(), OrchError> {
        validate_owner(owner_id)?;
        self.require_owner(owner_id)?;
        let mut next = self.state.clone();
        let replacement =
            mint_credential(&mut next, credential_id, owner_id, PrincipalClass::Owner)?;
        if let Some(index) = next
            .credentials
            .iter()
            .position(|record| record.credential_id == credential_id)
        {
            next.credentials[index] = replacement;
        } else {
            next.credentials.push(replacement);
        }
        self.commit(next)
    }

    pub fn revoke(&mut self, credential_id: &str) -> Result<(), OrchError> {
        let mut next = self.state.clone();
        next.credentials
            .retain(|record| record.credential_id != credential_id);
        allocate(&mut next.next_generation)?;
        self.commit(next)
    }

    pub fn issue_internal(&mut self, class: &'static str) -> Result<IssuedAuth, OrchError> {
        let kind = match class {
            "health-probe" => PrincipalClass::InternalHealth,
            "native-executor" => PrincipalClass::Owner,
            _ => {
                return Err(OrchError::new(
                    OrchErrorCode::Internal,
                    "unknown internal authority class",
                ));
            }
        };
        let owner = self
            .state
            .owner_id
            .clone()
            .ok_or_else(|| unauth("durable owner has not been installed"))?;
        if !self
            .state
            .credentials
            .iter()
            .any(|record| record.credential_id == class)
        {
            let mut next = self.state.clone();
            let record = mint_credential(&mut next, class, &owner, kind)?;
            next.credentials.push(record);
            self.commit(next)?;
        }
        let record = self
            .state
            .credentials
            .iter()
            .find(|record| record.credential_id == class)
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::Internal, "internal credential missing")
            })?;
        self.issue_from_record(record)
    }

    pub fn require_current(&self, auth: &IssuedAuth) -> Result<(), OrchError> {
        if auth.process_generation.0 != self.state.process_generation {
            return Err(unauth("stale process generation"));
        }
        let record = self
            .state
            .credentials
            .iter()
            .find(|record| record.credential_id == auth.credential_id)
            .ok_or_else(|| unauth("credential was revoked"))?;
        if record.generation != auth.generation.0
            || record.owner_id != auth.owner_id
            || decode_hex16(&record.principal) != Some(auth.principal.0)
            || decode_hex16(&record.incarnation) != Some(auth.incarnation.0)
        {
            return Err(unauth("stale authentication generation"));
        }
        Ok(())
    }

    pub fn require_mutation(&self, auth: &IssuedAuth) -> Result<(), OrchError> {
        self.require_current(auth)?;
        if auth.is_internal() {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "internal authority cannot admit mutations",
            ));
        }
        Ok(())
    }

    pub fn bind_resource(
        &mut self,
        auth: &IssuedAuth,
        kind: ResourceKind,
        resource_id: &str,
        session_id: &str,
        workspace: &Path,
    ) -> Result<(), OrchError> {
        self.require_mutation(auth)?;
        let workspace_fingerprint = workspace_fingerprint(workspace, kind)?;
        let session_key = session_key(kind, session_id);
        if let Some(existing) = self.find_binding(kind, resource_id) {
            if existing.binding_digest != auth.binding_digest()
                || existing.session_id != session_key
                || existing.workspace_fingerprint != workspace_fingerprint
            {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "resource is already bound to another authority",
                ));
            }
            return Ok(());
        }
        let mut next = self.state.clone();
        next.bindings.push(StoredBinding {
            kind,
            resource_id: resource_id.to_string(),
            binding_digest: auth.binding_digest(),
            session_id: session_key,
            workspace_fingerprint,
            created_generation: auth.generation.0,
        });
        self.commit(next)
    }

    pub fn require_resource(
        &self,
        auth: &IssuedAuth,
        kind: ResourceKind,
        resource_id: &str,
        session_id: &str,
        workspace: &Path,
    ) -> Result<(), OrchError> {
        self.require_current(auth)?;
        if auth.is_internal() {
            return Err(unauth("internal authority cannot read owner resources"));
        }
        let workspace_fingerprint = workspace_fingerprint(workspace, kind)?;
        let session_key = session_key(kind, session_id);
        let Some(existing) = self.find_binding(kind, resource_id) else {
            return Err(unknown_resource());
        };
        if existing.binding_digest != auth.binding_digest()
            || existing.session_id != session_key
            || existing.workspace_fingerprint != workspace_fingerprint
        {
            return Err(unknown_resource());
        }
        if auth.generation.0 < existing.created_generation {
            return Err(unauth("stale generation cannot reuse a resource"));
        }
        Ok(())
    }

    pub fn process_generation(&self) -> u64 {
        self.state.process_generation
    }

    pub fn next_generation(&self) -> u64 {
        self.state.next_generation
    }

    pub fn owner_id(&self) -> Option<&str> {
        self.state.owner_id.as_deref()
    }

    fn find_binding(&self, kind: ResourceKind, resource_id: &str) -> Option<&StoredBinding> {
        self.state
            .bindings
            .iter()
            .find(|binding| binding.kind == kind && binding.resource_id == resource_id)
    }

    fn issue_from_record(&self, record: &StoredCredential) -> Result<IssuedAuth, OrchError> {
        Ok(IssuedAuth {
            principal: PrincipalRef(
                decode_hex16(&record.principal).ok_or_else(|| unauth("corrupt principal"))?,
            ),
            incarnation: CredentialIncarnation(
                decode_hex16(&record.incarnation).ok_or_else(|| unauth("corrupt incarnation"))?,
            ),
            generation: AuthenticationGeneration(record.generation),
            process_generation: ProcessGeneration(self.state.process_generation),
            credential_id: record.credential_id.clone(),
            owner_id: record.owner_id.clone(),
            class: record.class,
        })
    }

    fn require_owner(&self, owner_id: &str) -> Result<(), OrchError> {
        match &self.state.owner_id {
            Some(existing) if existing == owner_id => Ok(()),
            Some(_) => Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "owner does not match durable workspace owner",
            )),
            None => Err(unauth("durable owner has not been installed")),
        }
    }

    fn commit(&mut self, next: StoredAuthority) -> Result<(), OrchError> {
        let path = self.root.join(AUTHORITY_FILE);
        let bytes = serde_json::to_vec_pretty(&next).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("serialize authority: {error}"),
            )
        })?;
        atomic_write_bytes(&path, &bytes)?;
        reject_insecure_path(&path)?;
        self.state = next;
        Ok(())
    }
}

fn mint_credential(
    state: &mut StoredAuthority,
    credential_id: &str,
    owner_id: &str,
    class: PrincipalClass,
) -> Result<StoredCredential, OrchError> {
    let generation = allocate(&mut state.next_generation)?;
    Ok(StoredCredential {
        credential_id: credential_id.to_string(),
        owner_id: owner_id.to_string(),
        principal: hex_bytes(&Uuid::new_v4().into_bytes()),
        incarnation: hex_bytes(&Uuid::new_v4().into_bytes()),
        generation,
        class,
    })
}

fn load_state(path: &Path) -> Result<StoredAuthority, OrchError> {
    let mut file = File::open(path).map_err(|error| {
        OrchError::new(OrchErrorCode::Internal, format!("open authority: {error}"))
    })?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|error| {
        OrchError::new(OrchErrorCode::Internal, format!("read authority: {error}"))
    })?;
    if text.contains("token") && text.contains("Bearer") {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "corrupt canonical authority: secret material is not permitted in records",
        ));
    }
    let state: StoredAuthority = serde_json::from_str(&text).map_err(|error| {
        OrchError::new(
            OrchErrorCode::Internal,
            format!("corrupt canonical authority: {error}"),
        )
    })?;
    if state.next_generation == 0 {
        return Err(OrchError::new(
            OrchErrorCode::Internal,
            "corrupt canonical authority generations",
        ));
    }
    Ok(state)
}

fn allocate(next: &mut u64) -> Result<u64, OrchError> {
    let value = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| OrchError::new(OrchErrorCode::Internal, "generation exhausted"))?;
    Ok(value)
}

fn validate_owner(owner_id: &str) -> Result<(), OrchError> {
    let owner_id = owner_id.trim();
    if owner_id.is_empty() || owner_id.len() > 128 {
        return Err(OrchError::new(
            OrchErrorCode::InvalidRequest,
            "owner id is invalid",
        ));
    }
    Ok(())
}

fn session_key(kind: ResourceKind, session_id: &str) -> String {
    if matches!(kind, ResourceKind::Workspace) {
        String::new()
    } else {
        session_id.to_string()
    }
}

fn workspace_fingerprint(workspace: &Path, _kind: ResourceKind) -> Result<String, OrchError> {
    let canonical = canonical_workspace(workspace)?;
    Ok(hex_sha256(canonical.to_string_lossy().as_bytes()))
}

fn unauth(message: &str) -> OrchError {
    OrchError::new(OrchErrorCode::Unauthenticated, message)
}

fn unknown_resource() -> OrchError {
    OrchError::new(OrchErrorCode::Unauthenticated, "resource is not available")
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_bytes(digest.as_ref())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex16(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable_fs::inject_fault;
    use crate::orchestration::AuthCredential;
    use std::process::Command;

    const CHILD_LOCK: &str = "--canonical-authority-lock-child";

    fn creds(pairs: &[(&str, &str)]) -> Vec<AuthCredential> {
        pairs
            .iter()
            .map(|(id, token)| AuthCredential::new(*id, *token).unwrap())
            .collect()
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    #[test]
    fn two_process_first_claim_is_single_writer() {
        let args: Vec<String> = std::env::args().collect();
        if let Some(pos) = args.iter().position(|arg| arg == CHILD_LOCK) {
            let root = PathBuf::from(&args[pos + 1]);
            match HostAuthority::open(&root) {
                Err(error) if error.code == OrchErrorCode::Conflict => std::process::exit(0),
                Ok(_) => std::process::exit(12),
                Err(_) => std::process::exit(13),
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let _host = HostAuthority::open(dir.path()).unwrap();
        let exe = std::env::current_exe().unwrap();
        let output = Command::new(&exe)
            .arg("--exact")
            .arg("canonical_authority::tests::two_process_first_claim_is_single_writer")
            .arg("--nocapture")
            .arg("--")
            .arg(CHILD_LOCK)
            .arg(dir.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "child: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn two_credentials_are_distinct_and_cannot_steal_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "alpha-secret"), ("laptop", "beta-secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let primary = host
            .authenticate(Some(&bearer("alpha-secret")), &credentials, "owner-1")
            .unwrap();
        let laptop = host
            .authenticate(Some(&bearer("beta-secret")), &credentials, "owner-1")
            .unwrap();
        assert_ne!(primary.binding_digest(), laptop.binding_digest());
        host.bind_resource(
            &primary,
            ResourceKind::Work,
            "work-1",
            "session-a",
            workspace.path(),
        )
        .unwrap();
        let denied = host
            .require_resource(
                &laptop,
                ResourceKind::Work,
                "work-1",
                "session-a",
                workspace.path(),
            )
            .unwrap_err();
        assert_eq!(denied.code, OrchErrorCode::Unauthenticated);
        assert_eq!(denied.message, unknown_resource().message);
    }

    #[test]
    fn first_claim_owner_cannot_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let err = host
            .install_credentials(&credentials, "owner-2")
            .unwrap_err();
        assert_eq!(err.code, OrchErrorCode::ForbiddenScope);
        drop(host);
        let mut host = HostAuthority::open(dir.path()).unwrap();
        assert_eq!(host.owner_id(), Some("owner-1"));
        assert!(host.install_credentials(&credentials, "owner-2").is_err());
    }

    #[test]
    fn stale_process_generation_cannot_be_reused() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let credentials = creds(&[("primary", "secret")]);
        let auth;
        {
            let mut host = HostAuthority::open(dir.path()).unwrap();
            host.install_credentials(&credentials, "owner-1").unwrap();
            auth = host
                .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
                .unwrap();
            host.bind_resource(
                &auth,
                ResourceKind::Session,
                "session-a",
                "session-a",
                workspace.path(),
            )
            .unwrap();
        }
        let mut host = HostAuthority::open(dir.path()).unwrap();
        assert!(host.require_current(&auth).is_err());
        assert!(host
            .require_resource(
                &auth,
                ResourceKind::Session,
                "session-a",
                "session-a",
                workspace.path()
            )
            .is_err());
        let fresh = host
            .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
            .unwrap();
        host.require_resource(
            &fresh,
            ResourceKind::Session,
            "session-a",
            "session-a",
            workspace.path(),
        )
        .unwrap();
        assert!(fresh.generation.0 >= auth.generation.0);
        assert_ne!(fresh.process_generation.0, auth.process_generation.0);
    }

    #[test]
    fn rotation_revoke_and_id_reuse_are_fenced() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let original = host
            .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
            .unwrap();
        host.bind_resource(
            &original,
            ResourceKind::Run,
            "run-1",
            "session-a",
            workspace.path(),
        )
        .unwrap();
        let before = original.generation.0;
        host.rotate_generation("primary").unwrap();
        assert!(host.next_generation() > before);
        assert_eq!(
            host.require_current(&original).unwrap_err().code,
            OrchErrorCode::Unauthenticated
        );
        let rotated = host
            .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
            .unwrap();
        assert!(rotated.generation.0 > before);
        host.require_resource(
            &rotated,
            ResourceKind::Run,
            "run-1",
            "session-a",
            workspace.path(),
        )
        .unwrap();

        host.revoke("primary").unwrap();
        assert_eq!(
            host.authenticate(Some(&bearer("secret")), &credentials, "owner-1")
                .unwrap_err()
                .code,
            OrchErrorCode::Unauthenticated
        );
        host.replace_credential("primary", "owner-1").unwrap();
        let reused = host
            .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
            .unwrap();
        assert_ne!(reused.binding_digest(), original.binding_digest());
        let missing = host
            .require_resource(
                &reused,
                ResourceKind::Run,
                "run-1",
                "session-a",
                workspace.path(),
            )
            .unwrap_err();
        assert_eq!(missing.code, OrchErrorCode::Unauthenticated);
        assert_eq!(missing.message, unknown_resource().message);
    }

    #[test]
    fn cross_session_rebinding_matches_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let auth = host
            .authenticate(Some(&bearer("secret")), &credentials, "owner-1")
            .unwrap();
        host.bind_resource(
            &auth,
            ResourceKind::Work,
            "work-1",
            "session-a",
            workspace.path(),
        )
        .unwrap();
        let cross = host
            .require_resource(
                &auth,
                ResourceKind::Work,
                "work-1",
                "session-b",
                workspace.path(),
            )
            .unwrap_err();
        let unknown = host
            .require_resource(
                &auth,
                ResourceKind::Work,
                "missing",
                "session-a",
                workspace.path(),
            )
            .unwrap_err();
        assert_eq!(cross.code, unknown.code);
        assert_eq!(cross.message, unknown.message);
        assert_eq!(
            host.bind_resource(
                &auth,
                ResourceKind::Work,
                "work-1",
                "session-b",
                workspace.path(),
            )
            .unwrap_err()
            .code,
            OrchErrorCode::ForbiddenScope
        );
    }

    #[test]
    fn generations_are_monotonic_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let first = host.next_generation();
        host.rotate_generation("primary").unwrap();
        let mid = host.next_generation();
        assert!(mid > first);
        drop(host);
        let mut host = HostAuthority::open(dir.path()).unwrap();
        assert!(host.process_generation() >= 2);
        assert!(host.next_generation() >= mid);
        host.rotate_generation("primary").unwrap();
        assert!(host.next_generation() > mid);
    }

    #[test]
    fn corruption_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut host = HostAuthority::open(dir.path()).unwrap();
            host.install_credentials(&creds(&[("primary", "secret")]), "owner-1")
                .unwrap();
        }
        let path = dir.path().join("canonical-authority").join(AUTHORITY_FILE);
        std::fs::write(&path, "{not-json").unwrap();
        restrict_mode(&path, false).unwrap();
        let err = HostAuthority::open(dir.path()).unwrap_err();
        assert_eq!(err.code, OrchErrorCode::Internal);
        assert!(!path.exists());
        let quarantine_dir = dir.path().join("canonical-authority").join("quarantine");
        let entries: Vec<_> = std::fs::read_dir(quarantine_dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn interrupted_authority_write_keeps_old_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let credentials = creds(&[("primary", "secret")]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let path = dir.path().join("canonical-authority").join(AUTHORITY_FILE);
        let before = std::fs::read(&path).unwrap();
        inject_fault(Some("rename"));
        let interrupted = host.rotate_generation("primary");
        inject_fault(None);
        assert!(interrupted.is_err(), "{interrupted:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        serde_json::from_slice::<StoredAuthority>(&before).unwrap();
    }

    #[test]
    fn health_probe_cannot_mutate_and_native_executor_is_owner() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        host.install_credentials(&creds(&[("primary", "secret")]), "owner-1")
            .unwrap();
        let health = host.issue_internal("health-probe").unwrap();
        assert!(health.is_internal());
        assert_eq!(
            host.require_mutation(&health).unwrap_err().code,
            OrchErrorCode::ForbiddenScope
        );
        let native = host.issue_internal("native-executor").unwrap();
        host.require_mutation(&native).unwrap();
        host.bind_resource(
            &native,
            ResourceKind::ComputerRun,
            "cu-1",
            "session-a",
            workspace.path(),
        )
        .unwrap();
    }

    #[test]
    fn secrets_never_appear_in_records_or_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = HostAuthority::open(dir.path()).unwrap();
        let secret = "super-secret-token-value";
        let credentials = creds(&[("primary", secret)]);
        host.install_credentials(&credentials, "owner-1").unwrap();
        let err = host
            .authenticate(Some(&bearer("wrong")), &credentials, "owner-1")
            .unwrap_err();
        let rendered = format!("{err:?}{err}");
        assert!(!rendered.contains(secret));
        let path = dir.path().join("canonical-authority").join(AUTHORITY_FILE);
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains(secret));
        assert!(!text.contains("Bearer"));
    }
}
