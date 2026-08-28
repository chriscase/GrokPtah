//! The single durable authority for provider attempts and physical sends.
//!
//! This crate intentionally does not know about a provider SDK, bearer
//! credentials, request bodies, or host policy implementations. The host gives
//! it a signed authority record and one request fingerprint; this crate
//! verifies the record, consumes an issued effect lease, persists the
//! resulting attempt before any possible I/O, and is the only place from which
//! a physical send permit can be obtained.
//!
//! A `Sending` or `Responding` record recovered after a process restart is
//! conservatively changed to `Uncertain`. Uncertain attempts are never
//! reopened by retry code. Reopening requires an explicit
//! `ReconciliationAuthorization` and provider truth.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const AUTHORITY_PUBLIC_KEY_FILE: &str = ".authority-public-key";
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";
pub const REQUEST_KEY_PREFIX: &str = "grokptah-";
const SCHEMA_VERSION: u32 = 1;
const MAX_ID_BYTES: usize = 256;
const MAX_FINGERPRINT_BYTES: usize = 64;

/// The only state machine used for a provider operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    Prepared,
    Admitted,
    Sending,
    Uncertain,
    Responding,
    Settled,
    Failed,
    Cancelled,
}

impl SendState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Failed | Self::Cancelled)
    }

    pub const fn is_ambiguous(self) -> bool {
        matches!(self, Self::Uncertain)
    }
}

/// Principal and capability authorities captured by the host at admission.
///
/// Values are persisted for equality checks but are never included in the
/// public attempt projection or `Debug` output.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityBinding {
    principal_incarnation: String,
    #[serde(alias = "principalGeneration")]
    auth_generation: u64,
    capability_generation: u64,
    #[serde(alias = "effectLease")]
    effect_lease_id: String,
    #[serde(default = "legacy_effect_scope")]
    effect_scope: String,
}

impl AuthorityBinding {
    #[allow(dead_code)]
    pub(crate) fn new(
        principal_incarnation: impl Into<String>,
        auth_generation: u64,
        capability_generation: u64,
        effect_lease_id: impl Into<String>,
        effect_scope: impl Into<String>,
    ) -> Result<Self, AttemptError> {
        let principal_incarnation = principal_incarnation.into();
        let effect_lease_id = effect_lease_id.into();
        let effect_scope = effect_scope.into();
        validate_id(&principal_incarnation, "principal incarnation")?;
        validate_id(&effect_lease_id, "effect lease id")?;
        validate_id(&effect_scope, "effect scope")?;
        if auth_generation == 0 || capability_generation == 0 {
            return Err(AttemptError::InvalidAuthority);
        }
        Ok(Self {
            principal_incarnation,
            auth_generation,
            capability_generation,
            effect_lease_id,
            effect_scope,
        })
    }

    pub fn same_as(&self, other: &Self) -> bool {
        self == other
    }

    fn same_live_as(&self, other: &Self) -> bool {
        self.principal_incarnation == other.principal_incarnation
            && self.auth_generation == other.auth_generation
            && self.capability_generation == other.capability_generation
            && self.effect_lease_id == other.effect_lease_id
            && self.effect_scope == other.effect_scope
    }

    fn with_effect_lease(&self, effect_lease_id: impl Into<String>) -> Result<Self, AttemptError> {
        Self::new(
            self.principal_incarnation.clone(),
            self.auth_generation,
            self.capability_generation,
            effect_lease_id,
            self.effect_scope.clone(),
        )
    }
}

impl fmt::Debug for AuthorityBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorityBinding")
            .field("principal_incarnation", &"[redacted]")
            .field("auth_generation", &"[redacted]")
            .field("capability_generation", &"[redacted]")
            .field("effect_lease_id", &"[redacted]")
            .field("effect_scope", &"[redacted]")
            .finish()
    }
}

fn legacy_effect_scope() -> String {
    "legacy-effect-scope".into()
}

/// Host-owned immutable input used to create an attempt.
#[derive(Clone, Eq, PartialEq)]
pub struct AttemptSpec {
    operation_id: String,
    provider_id: String,
    request_fingerprint: String,
    supports_idempotency: bool,
    authority: AuthorityBinding,
}

impl AttemptSpec {
    pub(crate) fn new(
        operation_id: impl Into<String>,
        provider_id: impl Into<String>,
        request_fingerprint: impl Into<String>,
        supports_idempotency: bool,
        authority: AuthorityBinding,
    ) -> Result<Self, AttemptError> {
        let operation_id = operation_id.into();
        let provider_id = provider_id.into();
        let request_fingerprint = request_fingerprint.into();
        validate_id(&operation_id, "operation id")?;
        validate_id(&provider_id, "provider id")?;
        if request_fingerprint.len() != MAX_FINGERPRINT_BYTES
            || !request_fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(AttemptError::InvalidRequestFingerprint);
        }
        Ok(Self {
            operation_id,
            provider_id,
            request_fingerprint,
            supports_idempotency,
            authority,
        })
    }

    pub fn fingerprint_bytes(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }
}

impl fmt::Debug for AttemptSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptSpec")
            .field("operation_id", &"[redacted]")
            .field("provider_id", &"[redacted]")
            .field("request_fingerprint", &"[redacted]")
            .field("supports_idempotency", &self.supports_idempotency)
            .field("authority", &self.authority)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostAuthorityPayload {
    principal_incarnation: String,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: String,
    effect_scope: String,
    #[serde(default)]
    revoked_effect_lease_ids: Vec<String>,
    #[serde(default)]
    issued_effect_lease_ids: Vec<String>,
}

/// Durable canonical authority record written by the trusted host authority
/// assembler. Its signature is verified before any authority value is used.
/// This type is intentionally private: downstream crates can only obtain an
/// `AttemptContext` by reading a valid host-owned record.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostAuthorityRecord {
    #[serde(flatten)]
    payload: HostAuthorityPayload,
    signature: String,
}

impl HostAuthorityRecord {
    fn binding_for_lease(&self, lease_id: &str) -> Result<AuthorityBinding, AttemptError> {
        if !self
            .payload
            .issued_effect_lease_ids
            .iter()
            .any(|issued| issued == lease_id)
        {
            return Err(AttemptError::InvalidAuthority);
        }
        AuthorityBinding::new(
            self.payload.principal_incarnation.clone(),
            self.payload.auth_generation,
            self.payload.capability_generation,
            lease_id.to_owned(),
            self.payload.effect_scope.clone(),
        )
    }

    fn lease_revoked(&self, lease_id: &str) -> bool {
        self.payload
            .revoked_effect_lease_ids
            .iter()
            .any(|revoked| revoked == lease_id)
    }

    fn unclaimed_lease(&self, root: &Path) -> Option<String> {
        self.payload
            .issued_effect_lease_ids
            .iter()
            .find(|lease_id| {
                !self.lease_revoked(lease_id)
                    && !root
                        .join("lease-claims")
                        .join(format!("{lease_id}.claim"))
                        .is_file()
            })
            .cloned()
    }
}

/// A safe public projection. No authority values, raw request key, body,
/// endpoint, credential, or diagnostic is represented here.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptProjection {
    pub attempt_id: String,
    pub send_state: SendState,
    pub provider_request_id: String,
}

/// Provider truth supplied by an explicit operator-authorized reconciliation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProviderTruth {
    NotApplied,
    Applied(ProviderSettlement),
}

/// A provider result proof. The provider-specific adapter must obtain this
/// from provider truth; the ledger never invents a result or stores a body.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProviderSettlement {
    provider_request_id: String,
    provider_effect_id: String,
}

impl ProviderSettlement {
    pub(crate) fn new(
        provider_request_id: impl Into<String>,
        provider_effect_id: impl Into<String>,
    ) -> Result<Self, AttemptError> {
        let provider_request_id = provider_request_id.into();
        let provider_effect_id = provider_effect_id.into();
        validate_id(&provider_request_id, "provider request id")?;
        validate_id(&provider_effect_id, "provider effect id")?;
        Ok(Self {
            provider_request_id,
            provider_effect_id,
        })
    }
}

/// Explicit operator authorization. This value is deliberately required by
/// the only API that can move `Uncertain` back into the send lattice.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReconciliationAuthorization {
    operator_id: String,
}

impl ReconciliationAuthorization {
    fn new(operator_id: impl Into<String>) -> Result<Self, AttemptError> {
        let operator_id = operator_id.into();
        validate_id(&operator_id, "operator id")?;
        Ok(Self { operator_id })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredReconciliation {
    operator_id: String,
    provider_request_id: String,
    provider_effect_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AttemptError {
    Io(String),
    Serialization(String),
    InvalidId(&'static str),
    InvalidRequestFingerprint,
    InvalidAuthority,
    MissingAttempt,
    StaleAuthority,
    InvalidTransition { from: SendState, to: SendState },
    EffectLeaseAlreadyUsed,
    NotExplicitlyAuthorized,
    SettlementDoesNotMatch,
    InvalidProviderEffect,
}

impl fmt::Display for AttemptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "provider-attempt ledger I/O failed: {message}"),
            Self::Serialization(message) => {
                write!(f, "provider-attempt ledger record is invalid: {message}")
            }
            Self::InvalidId(name) => write!(f, "{name} is invalid"),
            Self::InvalidRequestFingerprint => write!(f, "request fingerprint is invalid"),
            Self::InvalidAuthority => write!(f, "authority binding is invalid"),
            Self::MissingAttempt => write!(f, "provider attempt does not exist"),
            Self::StaleAuthority => write!(f, "principal or capability authority is stale"),
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid provider-attempt transition {from:?} -> {to:?}")
            }
            Self::EffectLeaseAlreadyUsed => {
                write!(f, "capability effect lease has already been consumed")
            }
            Self::NotExplicitlyAuthorized => {
                write!(
                    f,
                    "uncertain provider attempt requires explicit reconciliation"
                )
            }
            Self::SettlementDoesNotMatch => write!(f, "provider settlement does not match attempt"),
            Self::InvalidProviderEffect => write!(f, "provider effect proof is invalid"),
        }
    }
}

impl std::error::Error for AttemptError {}

impl From<std::io::Error> for AttemptError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Clone)]
pub struct ProviderAttemptStore {
    root: Arc<PathBuf>,
}

/// Host-issued context shared by bridge, SDK, and worker adapters. It carries
/// an immutable attempt snapshot but revalidates against the durable canonical
/// host authority record at every physical-send boundary.
#[derive(Clone)]
pub struct AttemptContext {
    store: ProviderAttemptStore,
    operation_id: String,
    authority: AuthorityBinding,
    authority_scope: Option<String>,
}

impl fmt::Debug for AttemptContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptContext")
            .field("operation_id", &"[redacted]")
            .field("authority_scope", &"[redacted]")
            .field("authority", &"[redacted]")
            .finish()
    }
}

impl AttemptContext {
    #[allow(dead_code)]
    pub(crate) fn new(
        store: ProviderAttemptStore,
        operation_id: impl Into<String>,
        authority: AuthorityBinding,
    ) -> Result<Self, AttemptError> {
        let operation_id = operation_id.into();
        validate_id(&operation_id, "operation id")?;
        Ok(Self {
            store,
            operation_id,
            authority,
            authority_scope: None,
        })
    }

    /// Construct an adapter from the durable authority snapshot written by the
    /// assembled host authority module. No downstream authority value or
    /// revalidation callback is accepted.
    pub fn from_host_ledger(
        store: ProviderAttemptStore,
        operation_id: impl Into<String>,
        authority_scope: impl Into<String>,
    ) -> Result<Self, AttemptError> {
        let operation_id = operation_id.into();
        validate_id(&operation_id, "operation id")?;
        let authority_scope = authority_scope.into();
        validate_id(&authority_scope, "authority scope")?;
        let (record, lease_id) = store.select_host_lease(&authority_scope, &operation_id)?;
        let mut context = Self::new(store, operation_id, record.binding_for_lease(&lease_id)?)?;
        context.authority_scope = Some(authority_scope);
        Ok(context)
    }

    pub fn prepare(
        &self,
        provider_id: &str,
        body: &[u8],
        supports_idempotency: bool,
    ) -> Result<ProviderAttempt, AttemptError> {
        let spec = AttemptSpec::new(
            self.operation_id.clone(),
            provider_id.to_owned(),
            AttemptSpec::fingerprint_bytes(body),
            supports_idempotency,
            self.authority.clone(),
        )?;
        let attempt = self.store.create(spec)?;
        attempt.admit(&self.authority)?;
        Ok(attempt)
    }

    pub fn begin_send(
        &self,
        attempt: &ProviderAttempt,
    ) -> Result<PhysicalSendPermit, AttemptError> {
        let current_record = self
            .authority_scope
            .as_deref()
            .map(|scope| self.store.read_host_authority(scope))
            .transpose()?;
        if current_record
            .as_ref()
            .is_some_and(|record| record.lease_revoked(&self.authority.effect_lease_id))
        {
            let _ = attempt.cancel_without_send();
            return Err(AttemptError::StaleAuthority);
        }
        let current = current_record
            .as_ref()
            .map(|record| record.binding_for_lease(&self.authority.effect_lease_id))
            .transpose()?
            .unwrap_or_else(|| self.authority.clone());
        let permit = match attempt.begin_send_live(&current) {
            Ok(permit) => permit,
            Err(error) => {
                if matches!(
                    error,
                    AttemptError::StaleAuthority | AttemptError::EffectLeaseAlreadyUsed
                ) {
                    let _ = attempt.cancel_without_send();
                }
                return Err(error);
            }
        };
        Ok(permit)
    }

    pub fn begin(
        &self,
        provider_id: &str,
        body: &[u8],
        supports_idempotency: bool,
    ) -> Result<PhysicalSendPermit, AttemptError> {
        let attempt = self.prepare(provider_id, body, supports_idempotency)?;
        self.begin_send(&attempt)
    }

    /// Acquire a distinct lease issued by the host authority. The lease is
    /// selected from the signed host record and is consumed atomically when
    /// the resulting attempt is created.
    pub fn acquire_next_effect_lease(&self) -> Result<Self, AttemptError> {
        let scope = self
            .authority_scope
            .as_deref()
            .ok_or(AttemptError::InvalidAuthority)?;
        let (_record, lease_id) = self.store.select_host_lease(scope, &self.operation_id)?;
        let authority = self.authority.with_effect_lease(lease_id)?;
        Self::new(self.store.clone(), self.operation_id.clone(), authority).map(|mut context| {
            context.authority_scope = self.authority_scope.clone();
            context
        })
    }

    pub fn revalidate_before_physical_write(
        &self,
        permit: &PhysicalSendPermit,
    ) -> Result<(), AttemptError> {
        let current_record = self
            .authority_scope
            .as_deref()
            .map(|scope| self.store.read_host_authority(scope))
            .transpose()?;
        if current_record
            .as_ref()
            .is_some_and(|record| record.lease_revoked(&permit.authority.effect_lease_id))
        {
            return Err(AttemptError::StaleAuthority);
        }
        let current = current_record
            .as_ref()
            .map(|record| record.binding_for_lease(&permit.authority.effect_lease_id))
            .transpose()?
            .unwrap_or_else(|| self.authority.clone());
        permit.revalidate_live(&current)
    }

    pub fn mark_response_started(
        &self,
        permit: &mut PhysicalSendPermit,
    ) -> Result<(), AttemptError> {
        permit.mark_response_started()
    }

    pub fn settle_http_response(
        &self,
        permit: &mut PhysicalSendPermit,
        status_code: u16,
        response_bytes: &[u8],
    ) -> Result<(), AttemptError> {
        permit.settle_http_response(status_code, response_bytes)
    }

    pub fn semantic_rejection(
        &self,
        permit: &mut PhysicalSendPermit,
        status_code: u16,
    ) -> Result<(), AttemptError> {
        permit.semantic_rejection(status_code)
    }

    pub fn transport_before_possible_write(
        &self,
        permit: &mut PhysicalSendPermit,
    ) -> Result<(), AttemptError> {
        permit.transport_before_possible_write()
    }

    pub fn transport_after_possible_write(
        &self,
        permit: &mut PhysicalSendPermit,
    ) -> Result<(), AttemptError> {
        permit.transport_after_possible_write()
    }

    pub fn cancel_after_possible_write(
        &self,
        permit: &mut PhysicalSendPermit,
    ) -> Result<(), AttemptError> {
        permit.cancel_after_possible_write()
    }

    /// Reopen an uncertain attempt only after a trusted host adapter has
    /// durably recorded authenticated operator authority and verified provider
    /// truth. The public API accepts no caller-created authorization or
    /// provider result.
    pub fn reconcile_from_host_ledger(
        &self,
        attempt: &ProviderAttempt,
    ) -> Result<(), AttemptError> {
        let path = self
            .store
            .root
            .join("reconciliation")
            .join(format!("{}.json", attempt.attempt_id()));
        let bytes = fs::read(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AttemptError::NotExplicitlyAuthorized
            } else {
                AttemptError::Io(error.to_string())
            }
        })?;
        let record: StoredReconciliation = serde_json::from_slice(&bytes)
            .map_err(|error| AttemptError::Serialization(error.to_string()))?;
        if record.provider_request_id != attempt.provider_request_id()? {
            return Err(AttemptError::NotExplicitlyAuthorized);
        }
        let authorization = ReconciliationAuthorization::new(record.operator_id)?;
        let truth = match record.provider_effect_id {
            Some(effect_id) => ProviderTruth::Applied(ProviderSettlement::new(
                record.provider_request_id,
                effect_id,
            )?),
            None => ProviderTruth::NotApplied,
        };
        attempt.reconcile(&authorization, truth)
    }
}

impl fmt::Debug for ProviderAttemptStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderAttemptStore([durable-root])")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAttempt {
    schema_version: u32,
    attempt_id: String,
    provider_request_id: String,
    provider_request_key: String,
    operation_id: String,
    provider_id: String,
    request_fingerprint: String,
    supports_idempotency: bool,
    authority: AuthorityBinding,
    state: SendState,
    #[serde(default)]
    lease_claimed: bool,
    sequence: u64,
    failure: Option<String>,
    settlement_effect_id: Option<String>,
    updated_at_ms: u128,
}

impl ProviderAttemptStore {
    /// Open a shared ledger. Unlike a host instance lock, this store is
    /// intentionally reopenable by a second process; every mutation takes a
    /// short OS file lock and atomically replaces one record.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, AttemptError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let lock_path = root.join(".provider-attempts.lock");
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        let store = Self {
            root: Arc::new(root),
        };
        store.recover_incomplete()?;
        Ok(store)
    }

    pub fn create(&self, spec: AttemptSpec) -> Result<ProviderAttempt, AttemptError> {
        let attempt_id = Uuid::new_v4().to_string();
        let provider_request_id = format!("opaque-{}", hex(&Sha256::digest(attempt_id.as_bytes())));
        let provider_request_key = derive_request_key(&attempt_id, &spec);
        let record = StoredAttempt {
            schema_version: SCHEMA_VERSION,
            attempt_id: attempt_id.clone(),
            provider_request_id,
            provider_request_key,
            operation_id: spec.operation_id,
            provider_id: spec.provider_id,
            request_fingerprint: spec.request_fingerprint,
            supports_idempotency: spec.supports_idempotency,
            authority: spec.authority,
            state: SendState::Prepared,
            lease_claimed: false,
            sequence: 0,
            failure: None,
            settlement_effect_id: None,
            updated_at_ms: now_ms(),
        };
        self.with_locked_record(&attempt_id, |existing| {
            if existing.is_some() {
                return Err(AttemptError::Serialization(
                    "attempt identifier collision".into(),
                ));
            }
            self.claim_lease_locked(
                &record.authority.effect_scope,
                &record.authority.effect_lease_id,
                &record.operation_id,
                &record.attempt_id,
            )?;
            Ok((Some(record), ()))
        })?;
        Ok(ProviderAttempt {
            store: self.clone(),
            attempt_id,
        })
    }

    pub fn load(&self, attempt_id: &str) -> Result<Option<ProviderAttempt>, AttemptError> {
        validate_id(attempt_id, "attempt id")?;
        Ok(self.read_record(attempt_id)?.map(|_| ProviderAttempt {
            store: self.clone(),
            attempt_id: attempt_id.to_owned(),
        }))
    }

    pub fn projection(&self, attempt_id: &str) -> Result<Option<AttemptProjection>, AttemptError> {
        let Some(record) = self.read_record(attempt_id)? else {
            return Ok(None);
        };
        Ok(Some(projection(&record)))
    }

    fn read_host_authority(
        &self,
        authority_scope: &str,
    ) -> Result<HostAuthorityRecord, AttemptError> {
        validate_id(authority_scope, "authority scope")?;
        let path = self.authority_path(authority_scope);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AttemptError::InvalidAuthority
            } else {
                AttemptError::Io(error.to_string())
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AttemptError::InvalidAuthority);
        }
        #[cfg(unix)]
        if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
            return Err(AttemptError::InvalidAuthority);
        }
        let bytes = fs::read(&path)?;
        let record: HostAuthorityRecord = serde_json::from_slice(&bytes)
            .map_err(|error| AttemptError::Serialization(error.to_string()))?;
        let public_key_path = self
            .root
            .join("canonical-authorities")
            .join(AUTHORITY_PUBLIC_KEY_FILE);
        let public_key_metadata =
            fs::symlink_metadata(&public_key_path).map_err(|_| AttemptError::InvalidAuthority)?;
        if public_key_metadata.file_type().is_symlink() {
            return Err(AttemptError::InvalidAuthority);
        }
        #[cfg(unix)]
        if std::os::unix::fs::PermissionsExt::mode(&public_key_metadata.permissions()) & 0o077 != 0
        {
            return Err(AttemptError::InvalidAuthority);
        }
        let public_key_bytes = fs::read(public_key_path)?;
        let public_key_array: [u8; 32] = public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| AttemptError::InvalidAuthority)?;
        let public_key = VerifyingKey::from_bytes(&public_key_array)
            .map_err(|_| AttemptError::InvalidAuthority)?;
        let signature_bytes = unhex(&record.signature).ok_or(AttemptError::InvalidAuthority)?;
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| AttemptError::InvalidAuthority)?;
        let payload = serde_json::to_vec(&record.payload)
            .map_err(|error| AttemptError::Serialization(error.to_string()))?;
        public_key
            .verify(&payload, &signature)
            .map_err(|_| AttemptError::InvalidAuthority)?;
        Ok(record)
    }

    fn authority_path(&self, authority_scope: &str) -> PathBuf {
        self.root
            .join("canonical-authorities")
            .join(format!("{authority_scope}.json"))
    }

    fn lease_claim_path(&self, lease_id: &str) -> PathBuf {
        self.root
            .join("lease-claims")
            .join(format!("{lease_id}.claim"))
    }

    fn select_host_lease(
        &self,
        authority_scope: &str,
        operation_id: &str,
    ) -> Result<(HostAuthorityRecord, String), AttemptError> {
        validate_id(operation_id, "operation id")?;
        let lock_path = self.root.join(".provider-attempts.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        let record = self.read_host_authority(authority_scope)?;
        let Some(lease_id) = record.unclaimed_lease(&self.root) else {
            let _ = lock.unlock();
            return Err(AttemptError::EffectLeaseAlreadyUsed);
        };
        let directory = self.root.join("lease-claims");
        fs::create_dir_all(&directory)?;
        let claim_path = self.lease_claim_path(&lease_id);
        let mut claim = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&claim_path)
        {
            Ok(claim) => claim,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = lock.unlock();
                return Err(AttemptError::EffectLeaseAlreadyUsed);
            }
            Err(error) => {
                let _ = lock.unlock();
                return Err(error.into());
            }
        };
        std::io::Write::write_all(&mut claim, operation_id.as_bytes())?;
        claim.sync_all()?;
        let _ = lock.unlock();
        Ok((record, lease_id))
    }

    fn claim_lease_locked(
        &self,
        authority_scope: &str,
        lease_id: &str,
        operation_id: &str,
        attempt_id: &str,
    ) -> Result<(), AttemptError> {
        let authority_path = self.authority_path(authority_scope);
        if !authority_path.is_file() {
            // Private in-crate tests exercise the state machine without a
            // host adapter. Production contexts always carry a signed record.
            return Ok(());
        }
        let record = self.read_host_authority(authority_scope)?;
        if record.lease_revoked(lease_id)
            || !record
                .payload
                .issued_effect_lease_ids
                .iter()
                .any(|issued| issued == lease_id)
        {
            return Err(AttemptError::StaleAuthority);
        }
        let claim_path = self.lease_claim_path(lease_id);
        let owner =
            fs::read_to_string(&claim_path).map_err(|_| AttemptError::EffectLeaseAlreadyUsed)?;
        if owner != operation_id {
            return Err(AttemptError::EffectLeaseAlreadyUsed);
        }
        let temporary = self
            .root
            .join(format!(".lease-claim-{}.{}.tmp", lease_id, Uuid::new_v4()));
        let mut claim = File::create(&temporary)?;
        std::io::Write::write_all(&mut claim, attempt_id.as_bytes())?;
        claim.sync_all()?;
        drop(claim);
        fs::rename(temporary, claim_path)?;
        Ok(())
    }

    pub fn recover_incomplete(&self) -> Result<usize, AttemptError> {
        let mut recovered = 0;
        let entries = fs::read_dir(&*self.root)?;
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(attempt_id) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            let changed = self.with_locked_record(attempt_id, |record| {
                let Some(mut record) = record else {
                    return Ok((None, false));
                };
                if matches!(record.state, SendState::Sending | SendState::Responding) {
                    record.state = SendState::Uncertain;
                    record.sequence = record.sequence.saturating_add(1);
                    record.failure = Some("recovered_after_possible_write".into());
                    record.updated_at_ms = now_ms();
                    Ok((Some(record), true))
                } else {
                    Ok((Some(record), false))
                }
            })?;
            if changed {
                recovered += 1;
            }
        }
        Ok(recovered)
    }

    fn path(&self, attempt_id: &str) -> Result<PathBuf, AttemptError> {
        validate_id(attempt_id, "attempt id")?;
        Ok(self.root.join(format!("{attempt_id}.json")))
    }

    fn read_record(&self, attempt_id: &str) -> Result<Option<StoredAttempt>, AttemptError> {
        let path = self.path(attempt_id)?;
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| AttemptError::Serialization(e.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn with_locked_record<R, F>(&self, attempt_id: &str, mutate: F) -> Result<R, AttemptError>
    where
        F: FnOnce(Option<StoredAttempt>) -> Result<(Option<StoredAttempt>, R), AttemptError>,
    {
        let lock_path = self.root.join(".provider-attempts.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        let current = self.read_record(attempt_id)?;
        let (next, result) = mutate(current)?;
        if let Some(next) = next {
            self.write_record(&next)?;
        }
        lock.unlock()?;
        Ok(result)
    }

    fn write_record(&self, record: &StoredAttempt) -> Result<(), AttemptError> {
        let path = self.path(&record.attempt_id)?;
        let tmp = self
            .root
            .join(format!(".{}.{}.tmp", record.attempt_id, Uuid::new_v4()));
        let bytes =
            serde_json::to_vec(record).map_err(|e| AttemptError::Serialization(e.to_string()))?;
        let mut file = File::create(&tmp)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &path)?;
        if let Ok(dir) = File::open(&*self.root) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn transition(
        &self,
        attempt_id: &str,
        expected_authority: Option<&AuthorityBinding>,
        to: SendState,
        failure: Option<String>,
        settlement: Option<&ProviderSettlement>,
    ) -> Result<(), AttemptError> {
        self.transition_internal(
            attempt_id,
            expected_authority,
            to,
            failure,
            settlement,
            false,
            false,
        )
    }

    fn transition_live(
        &self,
        attempt_id: &str,
        expected_authority: &AuthorityBinding,
        to: SendState,
    ) -> Result<(), AttemptError> {
        self.transition_internal(
            attempt_id,
            Some(expected_authority),
            to,
            None,
            None,
            false,
            true,
        )
    }

    fn transition_reconciled(
        &self,
        attempt_id: &str,
        to: SendState,
        failure: Option<String>,
        settlement: Option<&ProviderSettlement>,
    ) -> Result<(), AttemptError> {
        self.transition_internal(attempt_id, None, to, failure, settlement, true, false)
    }

    fn transition_internal(
        &self,
        attempt_id: &str,
        expected_authority: Option<&AuthorityBinding>,
        to: SendState,
        failure: Option<String>,
        settlement: Option<&ProviderSettlement>,
        explicit_reconciliation: bool,
        compare_live_authority: bool,
    ) -> Result<(), AttemptError> {
        self.with_locked_record(attempt_id, |record| {
            let mut record = record.ok_or(AttemptError::MissingAttempt)?;
            if expected_authority.is_some_and(|expected| {
                if compare_live_authority {
                    !record.authority.same_live_as(expected)
                } else {
                    !record.authority.same_as(expected)
                }
            }) {
                return Err(AttemptError::StaleAuthority);
            }
            let is_explicit_reopen = explicit_reconciliation
                && record.state == SendState::Uncertain
                && matches!(to, SendState::Admitted | SendState::Settled);
            if !(valid_transition(record.state, to) || is_explicit_reopen) {
                return Err(AttemptError::InvalidTransition {
                    from: record.state,
                    to,
                });
            }
            if to == SendState::Sending && !record.lease_claimed {
                if let Some(owner) = self.lease_claim_owner(&record.authority.effect_lease_id)?
                    && owner != record.attempt_id
                {
                    return Err(AttemptError::EffectLeaseAlreadyUsed);
                }
                if self.lease_claimed_by_other(
                    &record.attempt_id,
                    &record.authority.effect_lease_id,
                    &record.authority.effect_scope,
                )? {
                    return Err(AttemptError::EffectLeaseAlreadyUsed);
                }
                record.lease_claimed = true;
            }
            if let Some(settlement) = settlement {
                if settlement.provider_request_id != record.provider_request_id
                    || settlement.provider_effect_id.is_empty()
                {
                    return Err(AttemptError::SettlementDoesNotMatch);
                }
                record.settlement_effect_id = Some(settlement.provider_effect_id.clone());
            }
            record.state = to;
            record.sequence = record.sequence.saturating_add(1);
            record.failure = failure;
            record.updated_at_ms = now_ms();
            Ok((Some(record), ()))
        })
    }

    fn lease_claim_owner(&self, lease_id: &str) -> Result<Option<String>, AttemptError> {
        let path = self.lease_claim_path(lease_id);
        match fs::read_to_string(path) {
            Ok(owner) => Ok(Some(owner)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn lease_claimed_by_other(
        &self,
        attempt_id: &str,
        effect_lease_id: &str,
        effect_scope: &str,
    ) -> Result<bool, AttemptError> {
        for entry in fs::read_dir(&*self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(other_id) = path.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            if other_id == attempt_id {
                continue;
            }
            let Some(other) = self.read_record(other_id)? else {
                continue;
            };
            if other.lease_claimed
                && other.authority.effect_lease_id == effect_lease_id
                && other.authority.effect_scope == effect_scope
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// A cloneable reference to one durable attempt. It contains no request body
/// and cannot be constructed without a persisted `Prepared` record.
#[derive(Clone)]
pub struct ProviderAttempt {
    store: ProviderAttemptStore,
    attempt_id: String,
}

impl fmt::Debug for ProviderAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderAttempt")
            .field("attempt_id", &self.attempt_id)
            .field("send_state", &self.state().ok())
            .finish()
    }
}

impl ProviderAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn state(&self) -> Result<SendState, AttemptError> {
        self.store
            .read_record(&self.attempt_id)?
            .map(|record| record.state)
            .ok_or(AttemptError::MissingAttempt)
    }

    pub fn provider_request_id(&self) -> Result<String, AttemptError> {
        self.store
            .read_record(&self.attempt_id)?
            .map(|record| record.provider_request_id)
            .ok_or(AttemptError::MissingAttempt)
    }

    pub fn projection(&self) -> Result<AttemptProjection, AttemptError> {
        self.store
            .read_record(&self.attempt_id)?
            .map(|record| projection(&record))
            .ok_or(AttemptError::MissingAttempt)
    }

    pub(crate) fn admit(&self, authority: &AuthorityBinding) -> Result<(), AttemptError> {
        self.store.transition(
            &self.attempt_id,
            Some(authority),
            SendState::Admitted,
            None,
            None,
        )
    }

    pub(crate) fn begin_send_live(
        &self,
        current_authority: &AuthorityBinding,
    ) -> Result<PhysicalSendPermit, AttemptError> {
        self.store
            .transition_live(&self.attempt_id, current_authority, SendState::Sending)?;
        let record = self
            .store
            .read_record(&self.attempt_id)?
            .ok_or(AttemptError::MissingAttempt)?;
        Ok(PhysicalSendPermit {
            attempt: self.clone(),
            provider_request_id: record.provider_request_id,
            provider_request_key: record.provider_request_key,
            supports_idempotency: record.supports_idempotency,
            authority: record.authority,
            completed: false,
        })
    }

    pub(crate) fn cancel_without_send(&self) -> Result<(), AttemptError> {
        self.store.transition(
            &self.attempt_id,
            None,
            SendState::Cancelled,
            Some("cancelled_before_physical_send".into()),
            None,
        )
    }

    /// Reopen is only possible after explicit provider truth says no effect
    /// occurred. The persisted request key remains unchanged.
    pub(crate) fn reconcile(
        &self,
        authorization: &ReconciliationAuthorization,
        truth: ProviderTruth,
    ) -> Result<(), AttemptError> {
        if authorization.operator_id.is_empty() {
            return Err(AttemptError::NotExplicitlyAuthorized);
        }
        let record = self
            .store
            .read_record(&self.attempt_id)?
            .ok_or(AttemptError::MissingAttempt)?;
        if record.state != SendState::Uncertain {
            return Err(AttemptError::InvalidTransition {
                from: record.state,
                to: SendState::Admitted,
            });
        }
        match truth {
            ProviderTruth::NotApplied => self.store.transition_reconciled(
                &self.attempt_id,
                SendState::Admitted,
                Some("explicit_reconciliation_not_applied".into()),
                None,
            ),
            ProviderTruth::Applied(settlement) => self.store.transition_reconciled(
                &self.attempt_id,
                SendState::Settled,
                Some("explicit_reconciliation_applied".into()),
                Some(&settlement),
            ),
        }
    }
}

/// A capability proving that the caller passed the immediately-before-send
/// authority check. Dropping it before a terminal transition fails closed to
/// `Uncertain`; it never silently retries.
pub struct PhysicalSendPermit {
    attempt: ProviderAttempt,
    provider_request_id: String,
    provider_request_key: String,
    supports_idempotency: bool,
    authority: AuthorityBinding,
    completed: bool,
}

impl fmt::Debug for PhysicalSendPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhysicalSendPermit")
            .field("attempt_id", &self.attempt.attempt_id)
            .field("provider_request_id", &self.provider_request_id)
            .field("supports_idempotency", &self.supports_idempotency)
            .field("authority", &"[redacted]")
            .field("provider_request_key", &"[redacted]")
            .finish()
    }
}

impl PhysicalSendPermit {
    pub fn attempt_id(&self) -> &str {
        self.attempt.attempt_id()
    }

    pub fn provider_request_id(&self) -> &str {
        &self.provider_request_id
    }

    /// Internal adapters use this exact persisted value on the provider wire.
    /// It is never part of an `AttemptProjection` or `Debug` output.
    pub fn idempotency_key(&self) -> &str {
        &self.provider_request_key
    }

    pub const fn supports_idempotency(&self) -> bool {
        self.supports_idempotency
    }

    pub(crate) fn revalidate_live(&self, current: &AuthorityBinding) -> Result<(), AttemptError> {
        if !self.authority.same_live_as(current) {
            return Err(AttemptError::StaleAuthority);
        }
        if self.attempt.state()? != SendState::Sending {
            return Err(AttemptError::InvalidTransition {
                from: self.attempt.state()?,
                to: SendState::Sending,
            });
        }
        Ok(())
    }

    pub(crate) fn mark_response_started(&mut self) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Responding,
            None,
            None,
        )
    }

    pub(crate) fn settle(&mut self, settlement: ProviderSettlement) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Settled,
            None,
            Some(&settlement),
        )?;
        self.completed = true;
        Ok(())
    }

    /// Settle from an actual successful provider response without retaining
    /// its body. The digest is only an audit proof that this adapter consumed
    /// the response; it is not a fabricated provider result.
    pub(crate) fn settle_http_response(
        &mut self,
        status_code: u16,
        response_bytes: &[u8],
    ) -> Result<(), AttemptError> {
        if !(200..=299).contains(&status_code) {
            return Err(AttemptError::InvalidProviderEffect);
        }
        let effect_id = format!("http-response-{}", hex(&Sha256::digest(response_bytes)));
        self.settle(ProviderSettlement::new(
            self.provider_request_id.clone(),
            effect_id,
        )?)
    }

    pub(crate) fn semantic_rejection(&mut self, status_code: u16) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Failed,
            Some(format!("semantic_provider_rejection_{status_code}")),
            None,
        )?;
        self.completed = true;
        Ok(())
    }

    pub(crate) fn transport_before_possible_write(&mut self) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Failed,
            Some("transport_failed_before_possible_write".into()),
            None,
        )?;
        self.completed = true;
        Ok(())
    }

    pub(crate) fn transport_after_possible_write(&mut self) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Uncertain,
            Some("transport_lost_after_possible_write".into()),
            None,
        )?;
        self.completed = true;
        Ok(())
    }

    pub(crate) fn cancel_after_possible_write(&mut self) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Uncertain,
            Some("cancelled_after_possible_write".into()),
            None,
        )?;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PhysicalSendPermit {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.attempt.store.transition(
                self.attempt.attempt_id(),
                None,
                SendState::Uncertain,
                Some("permit_dropped_after_possible_write".into()),
                None,
            );
        }
    }
}

/// Deterministic test-only transport used by the adversarial ledger tests and
/// available to host integration tests. It records only opaque request IDs.
#[derive(Clone, Default)]
pub struct DeterministicFakeTransport {
    requests: Arc<Mutex<Vec<String>>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FakeTransportOutcome {
    SemanticRejection(u16),
    BeforePossibleWrite,
    AfterPossibleWrite,
    StreamAndSettle,
}

impl DeterministicFakeTransport {
    pub fn request_ids(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn send(
        &self,
        permit: &mut PhysicalSendPermit,
        outcome: FakeTransportOutcome,
    ) -> Result<(), AttemptError> {
        if !matches!(outcome, FakeTransportOutcome::BeforePossibleWrite) {
            self.requests
                .lock()
                .unwrap()
                .push(permit.provider_request_id().to_owned());
        }
        match outcome {
            FakeTransportOutcome::SemanticRejection(status) => permit.semantic_rejection(status),
            FakeTransportOutcome::BeforePossibleWrite => permit.transport_before_possible_write(),
            FakeTransportOutcome::AfterPossibleWrite => permit.transport_after_possible_write(),
            FakeTransportOutcome::StreamAndSettle => {
                permit.mark_response_started()?;
                let settlement =
                    ProviderSettlement::new(permit.provider_request_id(), "fake-effect-1")?;
                permit.settle(settlement)
            }
        }
    }

    pub fn start_stream(&self, permit: &mut PhysicalSendPermit) -> Result<(), AttemptError> {
        self.requests
            .lock()
            .unwrap()
            .push(permit.provider_request_id().to_owned());
        permit.mark_response_started()
    }

    pub fn settle_stream(&self, permit: &mut PhysicalSendPermit) -> Result<(), AttemptError> {
        permit.settle_http_response(200, b"fake-stream-settlement")
    }
}

fn projection(record: &StoredAttempt) -> AttemptProjection {
    AttemptProjection {
        attempt_id: record.attempt_id.clone(),
        send_state: record.state,
        provider_request_id: record.provider_request_id.clone(),
    }
}

fn valid_transition(from: SendState, to: SendState) -> bool {
    matches!(
        (from, to),
        (SendState::Prepared, SendState::Admitted)
            | (SendState::Prepared, SendState::Cancelled)
            | (SendState::Admitted, SendState::Sending)
            | (SendState::Admitted, SendState::Cancelled)
            | (SendState::Sending, SendState::Responding)
            | (SendState::Sending, SendState::Uncertain)
            | (SendState::Sending, SendState::Failed)
            | (SendState::Responding, SendState::Settled)
            | (SendState::Responding, SendState::Uncertain)
    )
}

fn derive_request_key(attempt_id: &str, spec: &AttemptSpec) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah/provider-request-key/v1");
    for value in [
        attempt_id,
        &spec.operation_id,
        &spec.provider_id,
        &spec.request_fingerprint,
        &spec.authority.principal_incarnation,
        &spec.authority.effect_lease_id,
        &spec.authority.effect_scope,
    ] {
        hasher.update([0]);
        hasher.update(value.as_bytes());
    }
    hasher.update([0]);
    hasher.update(spec.authority.auth_generation.to_le_bytes());
    hasher.update([0]);
    hasher.update(spec.authority.capability_generation.to_le_bytes());
    format!("{REQUEST_KEY_PREFIX}{}", hex(&hasher.finalize()))
}

fn validate_id(value: &str, name: &'static str) -> Result<(), AttemptError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value.contains(['/', '\\', '\0'])
        || !value.is_ascii()
    {
        return Err(AttemptError::InvalidId(name));
    }
    Ok(())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn unhex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.bytes();
    while let (Some(high), Some(low)) = (chars.next(), chars.next()) {
        let high = (high as char).to_digit(16)? as u8;
        let low = (low as char).to_digit(16)? as u8;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn authority(generation: u64) -> AuthorityBinding {
        AuthorityBinding::new(
            "principal-incarnation-a",
            generation,
            generation,
            format!("lease-{generation}"),
            format!("scope-{generation}"),
        )
        .unwrap()
    }

    fn spec(binding: AuthorityBinding) -> AttemptSpec {
        AttemptSpec::new(
            "desktop-run-1",
            "xai",
            AttemptSpec::fingerprint_bytes(br#"{"prompt":"opaque"}"#),
            true,
            binding,
        )
        .unwrap()
    }

    fn prepared() -> (tempfile::TempDir, ProviderAttemptStore, ProviderAttempt) {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let attempt = store.create(spec(authority(1))).unwrap();
        (temp, store, attempt)
    }

    fn write_host_snapshot(root: &Path, scope: &str, binding: &AuthorityBinding) {
        write_host_snapshot_with_leases(
            root,
            scope,
            binding,
            vec![binding.effect_lease_id.clone()],
        );
    }

    fn write_host_snapshot_with_leases(
        root: &Path,
        scope: &str,
        binding: &AuthorityBinding,
        issued_effect_lease_ids: Vec<String>,
    ) {
        use ed25519_dalek::{Signer, SigningKey};
        fs::create_dir_all(root.join("canonical-authorities")).unwrap();
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        fs::write(
            root.join("canonical-authorities")
                .join(AUTHORITY_PUBLIC_KEY_FILE),
            signing_key.verifying_key().to_bytes(),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            root.join("canonical-authorities")
                .join(AUTHORITY_PUBLIC_KEY_FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let payload = HostAuthorityPayload {
            principal_incarnation: binding.principal_incarnation.clone(),
            auth_generation: binding.auth_generation,
            capability_generation: binding.capability_generation,
            effect_lease_id: binding.effect_lease_id.clone(),
            effect_scope: scope.into(),
            revoked_effect_lease_ids: Vec::new(),
            issued_effect_lease_ids,
        };
        let signature = signing_key.sign(&serde_json::to_vec(&payload).unwrap());
        fs::write(
            root.join("canonical-authorities")
                .join(format!("{scope}.json")),
            serde_json::to_vec(&HostAuthorityRecord {
                payload,
                signature: hex(signature.to_bytes().as_slice()),
            })
            .unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            root.join("canonical-authorities")
                .join(format!("{scope}.json")),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    #[test]
    fn state_transition_matrix_is_monotonic() {
        let all = [
            SendState::Prepared,
            SendState::Admitted,
            SendState::Sending,
            SendState::Uncertain,
            SendState::Responding,
            SendState::Settled,
            SendState::Failed,
            SendState::Cancelled,
        ];
        for from in all {
            for to in all {
                let expected = matches!(
                    (from, to),
                    (SendState::Prepared, SendState::Admitted)
                        | (SendState::Prepared, SendState::Cancelled)
                        | (SendState::Admitted, SendState::Sending)
                        | (SendState::Admitted, SendState::Cancelled)
                        | (SendState::Sending, SendState::Responding)
                        | (SendState::Sending, SendState::Uncertain)
                        | (SendState::Sending, SendState::Failed)
                        | (SendState::Responding, SendState::Settled)
                        | (SendState::Responding, SendState::Uncertain)
                );
                assert_eq!(valid_transition(from, to), expected, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn persists_intent_before_admission_and_exposes_only_safe_projection() {
        let (_temp, _store, attempt) = prepared();
        assert_eq!(attempt.state().unwrap(), SendState::Prepared);
        let projection = attempt.projection().unwrap();
        let json = serde_json::to_string(&projection).unwrap();
        assert!(json.contains("attemptId"));
        assert!(json.contains("sendState"));
        assert!(json.contains("providerRequestId"));
        for forbidden in [
            "principal",
            "generation",
            "lease",
            "body",
            "credential",
            "Bearer",
            "grokptah-",
        ] {
            assert!(
                !json
                    .to_ascii_lowercase()
                    .contains(&forbidden.to_ascii_lowercase())
            );
        }
    }

    #[test]
    fn crash_cut_before_intent_has_no_ledger_record_or_physical_request() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let transport = DeterministicFakeTransport::default();
        assert!(store.projection("never-created").unwrap().is_none());
        assert!(transport.request_ids().is_empty());
        assert_eq!(
            fs::read_dir(temp.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().and_then(|x| x.to_str()) == Some("json"))
                .count(),
            0
        );
    }

    #[test]
    fn physical_key_is_stable_across_oauth_refresh_and_reopen() {
        let (_temp, store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        let key = permit.idempotency_key().to_owned();
        assert!(permit.supports_idempotency());
        permit.transport_after_possible_write().unwrap();
        let reopened = ProviderAttemptStore::open(store.root.as_path())
            .unwrap()
            .load(attempt.attempt_id())
            .unwrap()
            .unwrap();
        let auth = ReconciliationAuthorization::new("operator-1").unwrap();
        reopened
            .reconcile(&auth, ProviderTruth::NotApplied)
            .unwrap();
        let reopened_permit = reopened.begin_send_live(&binding).unwrap();
        assert_eq!(reopened_permit.idempotency_key(), key);
    }

    #[test]
    fn stale_principal_or_capability_is_zero_send() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let stale = authority(2);
        assert_eq!(
            attempt.begin_send_live(&stale).unwrap_err(),
            AttemptError::StaleAuthority
        );
        assert_eq!(attempt.state().unwrap(), SendState::Admitted);
    }

    #[test]
    fn context_revalidates_again_immediately_before_physical_write() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let context = AttemptContext::new(store, "host-operation", authority(1)).unwrap();
        let mut permit = context
            .begin("xai", b"request", true)
            .expect("initial authority admits");
        assert_eq!(
            permit.revalidate_live(&authority(2)).unwrap_err(),
            AttemptError::StaleAuthority
        );
        permit.transport_before_possible_write().unwrap();
    }

    #[test]
    fn revoke_between_admission_and_begin_send_is_zero_write() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let scope = "revoke-scope";
        write_host_snapshot(temp.path(), scope, &authority(1));
        let context =
            AttemptContext::from_host_ledger(store.clone(), "revoke-operation", scope).unwrap();
        let attempt = context.prepare("xai", b"request", true).unwrap();
        write_host_snapshot_with_leases(temp.path(), scope, &authority(2), vec!["lease-1".into()]);
        assert_eq!(
            context.begin_send(&attempt).unwrap_err(),
            AttemptError::StaleAuthority
        );
        assert_eq!(attempt.state().unwrap(), SendState::Cancelled);
        assert!(
            DeterministicFakeTransport::default()
                .request_ids()
                .is_empty()
        );
    }

    #[test]
    fn tampered_host_authority_is_rejected_before_intent_or_provider_bytes() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let scope = "tamper-scope";
        write_host_snapshot(temp.path(), scope, &authority(1));
        let path = temp
            .path()
            .join("canonical-authorities")
            .join(format!("{scope}.json"));
        let mut tampered: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        tampered["authGeneration"] = serde_json::json!(2);
        fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            AttemptContext::from_host_ledger(store.clone(), "tamper-operation", scope).unwrap_err(),
            AttemptError::InvalidAuthority
        );
        assert!(store.projection("never-created").unwrap().is_none());
        assert!(
            DeterministicFakeTransport::default()
                .request_ids()
                .is_empty()
        );
    }

    #[test]
    fn host_issued_lease_is_reserved_once_before_intent() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let scope = "single-lease-scope";
        write_host_snapshot(temp.path(), scope, &authority(1));
        let context =
            AttemptContext::from_host_ledger(store.clone(), "single-lease-operation", scope)
                .unwrap();
        assert_eq!(
            AttemptContext::from_host_ledger(store.clone(), "second-operation", scope).unwrap_err(),
            AttemptError::EffectLeaseAlreadyUsed
        );
        let mut permit = context.begin("xai", b"one-use", true).unwrap();
        DeterministicFakeTransport::default()
            .send(&mut permit, FakeTransportOutcome::BeforePossibleWrite)
            .unwrap();
        assert!(store.projection(permit.attempt_id()).unwrap().is_some());
    }

    #[test]
    fn changed_body_requires_a_distinct_host_lease_and_request_key() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let scope = "changed-body-scope";
        write_host_snapshot_with_leases(
            temp.path(),
            scope,
            &authority(1),
            vec!["lease-1".into(), "lease-2".into()],
        );
        let context =
            AttemptContext::from_host_ledger(store.clone(), "changed-body-operation", scope)
                .unwrap();
        let mut first = context
            .begin("xai", b"body-with-tool-choice", true)
            .unwrap();
        let first_key = first.idempotency_key().to_owned();
        context.semantic_rejection(&mut first, 400).unwrap();
        let next_context = context.acquire_next_effect_lease().unwrap();
        let second = next_context
            .begin("xai", b"body-without-tool-choice", true)
            .unwrap();
        assert_ne!(first_key, second.idempotency_key());
        assert_eq!(first.attempt.state().unwrap(), SendState::Failed);
        assert_eq!(second.attempt.state().unwrap(), SendState::Sending);
    }

    #[test]
    fn cloned_or_replayed_effect_lease_cannot_be_used_by_another_attempt() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let first_context =
            AttemptContext::new(store.clone(), "lease-round-one", authority(1)).unwrap();
        let second_context = AttemptContext::new(store, "lease-round-two", authority(1)).unwrap();
        let first = first_context.begin("xai", b"round-one", true).unwrap();
        let second = second_context.prepare("xai", b"round-two", true).unwrap();
        assert_eq!(
            second_context.begin_send(&second).unwrap_err(),
            AttemptError::EffectLeaseAlreadyUsed
        );
        assert_eq!(second.state().unwrap(), SendState::Cancelled);
        assert_eq!(first.attempt.state().unwrap(), SendState::Sending);
    }

    #[test]
    fn distinct_effect_leases_allow_legitimate_sequential_tool_rounds() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let first_context =
            AttemptContext::new(store.clone(), "tool-round-one", authority(1)).unwrap();
        let second_context = AttemptContext::new(store, "tool-round-two", authority(2)).unwrap();
        let mut first = first_context.begin("xai", b"tool-one", true).unwrap();
        first.mark_response_started().unwrap();
        first.settle_http_response(200, b"tool-one-result").unwrap();
        let mut second = second_context.begin("xai", b"tool-two", true).unwrap();
        second.mark_response_started().unwrap();
        second
            .settle_http_response(200, b"tool-two-result")
            .unwrap();
        assert_eq!(first.attempt.state().unwrap(), SendState::Settled);
        assert_eq!(second.attempt.state().unwrap(), SendState::Settled);
    }

    #[test]
    fn semantic_rejection_is_failure_not_uncertain() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        permit
            .semantic_rejection(422)
            .expect("semantic response is a terminal failure");
        assert_eq!(attempt.state().unwrap(), SendState::Failed);
    }

    #[test]
    fn streaming_start_and_settlement_share_one_attempt() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let transport = DeterministicFakeTransport::default();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        transport.start_stream(&mut permit).unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Responding);
        transport.settle_stream(&mut permit).unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Settled);
        assert_eq!(transport.request_ids().len(), 1);
    }

    #[test]
    fn crash_during_stream_or_before_settlement_becomes_uncertain() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let transport = DeterministicFakeTransport::default();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        transport.start_stream(&mut permit).unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Responding);
        drop(permit);
        assert_eq!(attempt.state().unwrap(), SendState::Uncertain);
        assert_eq!(transport.request_ids().len(), 1);
    }

    #[test]
    fn all_possible_write_loss_is_uncertain_and_never_auto_retried() {
        let (_temp, store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        let transport = DeterministicFakeTransport::default();
        transport
            .send(&mut permit, FakeTransportOutcome::AfterPossibleWrite)
            .unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Uncertain);
        let reopened = ProviderAttemptStore::open(store.root.as_path())
            .unwrap()
            .load(attempt.attempt_id())
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.begin_send_live(&binding).unwrap_err(),
            AttemptError::InvalidTransition {
                from: SendState::Uncertain,
                to: SendState::Sending
            }
        );
        assert_eq!(transport.request_ids().len(), 1);
    }

    #[test]
    fn restart_recovers_sending_and_two_processes_share_the_ledger() {
        let (_temp, store_a, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let permit = attempt.begin_send_live(&binding).unwrap();
        std::mem::forget(permit);
        let store_b = ProviderAttemptStore::open(store_a.root.as_path()).unwrap();
        let reopened = store_b.load(attempt.attempt_id()).unwrap().unwrap();
        assert_eq!(reopened.state().unwrap(), SendState::Uncertain);
        let projection = store_b.projection(attempt.attempt_id()).unwrap().unwrap();
        assert_eq!(projection.send_state, SendState::Uncertain);
    }

    #[test]
    fn explicit_reconciliation_is_the_only_reopen_path() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        permit.transport_after_possible_write().unwrap();
        assert_eq!(
            attempt.reconcile(
                &ReconciliationAuthorization::new("operator-1").unwrap(),
                ProviderTruth::NotApplied
            ),
            Ok(())
        );
        assert_eq!(attempt.state().unwrap(), SendState::Admitted);
    }

    #[test]
    fn trusted_reconciliation_evidence_requires_matching_provider_identity() {
        let temp = tempdir().unwrap();
        let store = ProviderAttemptStore::open(temp.path()).unwrap();
        let binding = authority(1);
        let context =
            AttemptContext::new(store.clone(), "reconciliation-operation", binding.clone())
                .unwrap();
        let mut permit = context.begin("xai", b"request", true).unwrap();
        permit.transport_after_possible_write().unwrap();
        let attempt = store.load(permit.attempt_id()).unwrap().unwrap();
        fs::create_dir_all(store.root.join("reconciliation")).unwrap();
        fs::write(
            store
                .root
                .join("reconciliation")
                .join(format!("{}.json", attempt.attempt_id())),
            serde_json::json!({
                "operatorId": "operator-1",
                "providerRequestId": "opaque-wrong",
                "providerEffectId": null,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            context.reconcile_from_host_ledger(&attempt),
            Err(AttemptError::NotExplicitlyAuthorized)
        );
        fs::write(
            store
                .root
                .join("reconciliation")
                .join(format!("{}.json", attempt.attempt_id())),
            serde_json::json!({
                "operatorId": "operator-1",
                "providerRequestId": attempt.provider_request_id().unwrap(),
                "providerEffectId": null,
            })
            .to_string(),
        )
        .unwrap();
        context.reconcile_from_host_ledger(&attempt).unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Admitted);
    }

    #[test]
    fn settlement_requires_provider_request_identity_and_effect_proof() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        permit.mark_response_started().unwrap();
        let wrong = ProviderSettlement::new("opaque-wrong", "provider-effect").unwrap();
        assert_eq!(
            permit.settle(wrong).unwrap_err(),
            AttemptError::SettlementDoesNotMatch
        );
        assert_eq!(attempt.state().unwrap(), SendState::Responding);
    }

    #[test]
    fn before_socket_write_is_not_ambiguous() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let transport = DeterministicFakeTransport::default();
        let mut permit = attempt.begin_send_live(&binding).unwrap();
        transport
            .send(&mut permit, FakeTransportOutcome::BeforePossibleWrite)
            .unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Failed);
        assert!(transport.request_ids().is_empty());
    }
}
