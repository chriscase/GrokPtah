//! The single durable authority for provider attempts and physical sends.
//!
//! This crate intentionally does not know about a provider SDK, bearer
//! credentials, request bodies, or host policy implementations. The host gives
//! it one immutable authority binding and one request fingerprint; this crate
//! persists the resulting attempt before any possible I/O and is the only
//! place from which a physical send permit can be obtained.
//!
//! A `Sending` or `Responding` record recovered after a process restart is
//! conservatively changed to `Uncertain`. Uncertain attempts are never
//! reopened by retry code. Reopening requires an explicit
//! `ReconciliationAuthorization` and provider truth.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

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
    principal_generation: u64,
    capability_generation: u64,
    effect_lease: String,
}

impl AuthorityBinding {
    pub fn new(
        principal_incarnation: impl Into<String>,
        principal_generation: u64,
        capability_generation: u64,
        effect_lease: impl Into<String>,
    ) -> Result<Self, AttemptError> {
        let principal_incarnation = principal_incarnation.into();
        let effect_lease = effect_lease.into();
        validate_id(&principal_incarnation, "principal incarnation")?;
        validate_id(&effect_lease, "effect lease")?;
        if principal_generation == 0 || capability_generation == 0 {
            return Err(AttemptError::InvalidAuthority);
        }
        Ok(Self {
            principal_incarnation,
            principal_generation,
            capability_generation,
            effect_lease,
        })
    }

    pub fn same_as(&self, other: &Self) -> bool {
        self == other
    }
}

impl fmt::Debug for AuthorityBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorityBinding")
            .field("principal_incarnation", &"[redacted]")
            .field("principal_generation", &"[redacted]")
            .field("capability_generation", &"[redacted]")
            .field("effect_lease", &"[redacted]")
            .finish()
    }
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
    pub fn new(
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
    pub fn new(
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
    pub fn new(operator_id: impl Into<String>) -> Result<Self, AttemptError> {
        let operator_id = operator_id.into();
        validate_id(&operator_id, "operator id")?;
        Ok(Self { operator_id })
    }
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
            Self::NotExplicitlyAuthorized => {
                write!(f, "uncertain provider attempt requires explicit reconciliation")
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
            Ok((Some(record), ()))
        })?;
        Ok(ProviderAttempt {
            store: self.clone(),
            attempt_id,
        })
    }

    pub fn load(&self, attempt_id: &str) -> Result<Option<ProviderAttempt>, AttemptError> {
        validate_id(attempt_id, "attempt id")?;
        Ok(self
            .read_record(attempt_id)?
            .map(|_| ProviderAttempt {
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
        )
    }

    fn transition_reconciled(
        &self,
        attempt_id: &str,
        to: SendState,
        failure: Option<String>,
        settlement: Option<&ProviderSettlement>,
    ) -> Result<(), AttemptError> {
        self.transition_internal(attempt_id, None, to, failure, settlement, true)
    }

    fn transition_internal(
        &self,
        attempt_id: &str,
        expected_authority: Option<&AuthorityBinding>,
        to: SendState,
        failure: Option<String>,
        settlement: Option<&ProviderSettlement>,
        explicit_reconciliation: bool,
    ) -> Result<(), AttemptError> {
        self.with_locked_record(attempt_id, |record| {
            let mut record = record.ok_or(AttemptError::MissingAttempt)?;
            if let Some(expected) = expected_authority {
                if !record.authority.same_as(expected) {
                    return Err(AttemptError::StaleAuthority);
                }
            }
            if !valid_transition(record.state, to)
                && !(explicit_reconciliation
                    && record.state == SendState::Uncertain
                    && matches!(to, SendState::Admitted | SendState::Settled))
            {
                return Err(AttemptError::InvalidTransition {
                    from: record.state,
                    to,
                });
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

    pub fn admit(&self, authority: &AuthorityBinding) -> Result<(), AttemptError> {
        self.store.transition(
            &self.attempt_id,
            Some(authority),
            SendState::Admitted,
            None,
            None,
        )
    }

    /// Persist `Sending` and return the only physical-send permit. Call this
    /// immediately before constructing/executing a provider request.
    pub fn begin_send(
        &self,
        current_authority: &AuthorityBinding,
    ) -> Result<PhysicalSendPermit, AttemptError> {
        self.store.transition(
            &self.attempt_id,
            Some(current_authority),
            SendState::Sending,
            None,
            None,
        )?;
        let record = self
            .store
            .read_record(&self.attempt_id)?
            .ok_or(AttemptError::MissingAttempt)?;
        Ok(PhysicalSendPermit {
            attempt: self.clone(),
            provider_request_id: record.provider_request_id,
            provider_request_key: record.provider_request_key,
            supports_idempotency: record.supports_idempotency,
            completed: false,
        })
    }

    pub fn cancel_without_send(&self) -> Result<(), AttemptError> {
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
    pub fn reconcile(
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
    completed: bool,
}

impl fmt::Debug for PhysicalSendPermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhysicalSendPermit")
            .field("attempt_id", &self.attempt.attempt_id)
            .field("provider_request_id", &self.provider_request_id)
            .field("supports_idempotency", &self.supports_idempotency)
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

    pub fn mark_response_started(&mut self) -> Result<(), AttemptError> {
        self.attempt.store.transition(
            self.attempt.attempt_id(),
            None,
            SendState::Responding,
            None,
            None,
        )
    }

    pub fn settle(&mut self, settlement: ProviderSettlement) -> Result<(), AttemptError> {
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
    pub fn settle_http_response(
        &mut self,
        status_code: u16,
        response_bytes: &[u8],
    ) -> Result<(), AttemptError> {
        if !(200..=299).contains(&status_code) {
            return Err(AttemptError::InvalidProviderEffect);
        }
        let effect_id = format!(
            "http-response-{}",
            hex(&Sha256::digest(response_bytes))
        );
        self.settle(ProviderSettlement::new(
            self.provider_request_id.clone(),
            effect_id,
        )?)
    }

    pub fn semantic_rejection(&mut self, status_code: u16) -> Result<(), AttemptError> {
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

    pub fn transport_before_possible_write(&mut self) -> Result<(), AttemptError> {
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

    pub fn transport_after_possible_write(&mut self) -> Result<(), AttemptError> {
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

    pub fn cancel_after_possible_write(&mut self) -> Result<(), AttemptError> {
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
            | (SendState::Sending, SendState::Settled)
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
    ] {
        hasher.update([0]);
        hasher.update(value.as_bytes());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn authority(generation: u64) -> AuthorityBinding {
        AuthorityBinding::new(
            "principal-incarnation-a",
            generation,
            generation,
            format!("lease-{generation}"),
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
                        | (SendState::Sending, SendState::Settled)
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
            assert!(!json.to_ascii_lowercase().contains(&forbidden.to_ascii_lowercase()));
        }
    }

    #[test]
    fn physical_key_is_stable_across_oauth_refresh_and_reopen() {
        let (_temp, store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send(&binding).unwrap();
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
        let reopened_permit = reopened.begin_send(&binding).unwrap();
        assert_eq!(reopened_permit.idempotency_key(), key);
    }

    #[test]
    fn stale_principal_or_capability_is_zero_send() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let stale = authority(2);
        assert_eq!(
            attempt.begin_send(&stale).unwrap_err(),
            AttemptError::StaleAuthority
        );
        assert_eq!(attempt.state().unwrap(), SendState::Admitted);
    }

    #[test]
    fn semantic_rejection_is_failure_not_uncertain() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send(&binding).unwrap();
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
        let mut permit = attempt.begin_send(&binding).unwrap();
        transport
            .send(&mut permit, FakeTransportOutcome::StreamAndSettle)
            .unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Settled);
        assert_eq!(transport.request_ids().len(), 1);
    }

    #[test]
    fn all_possible_write_loss_is_uncertain_and_never_auto_retried() {
        let (_temp, store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let mut permit = attempt.begin_send(&binding).unwrap();
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
            reopened.begin_send(&binding).unwrap_err(),
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
        let permit = attempt.begin_send(&binding).unwrap();
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
        let mut permit = attempt.begin_send(&binding).unwrap();
        permit.transport_after_possible_write().unwrap();
        assert_eq!(
            attempt.reconcile(&ReconciliationAuthorization::new("operator-1").unwrap(), ProviderTruth::NotApplied),
            Ok(())
        );
        assert_eq!(attempt.state().unwrap(), SendState::Admitted);
    }

    #[test]
    fn before_socket_write_is_not_ambiguous() {
        let (_temp, _store, attempt) = prepared();
        let binding = authority(1);
        attempt.admit(&binding).unwrap();
        let transport = DeterministicFakeTransport::default();
        let mut permit = attempt.begin_send(&binding).unwrap();
        transport
            .send(&mut permit, FakeTransportOutcome::BeforePossibleWrite)
            .unwrap();
        assert_eq!(attempt.state().unwrap(), SendState::Failed);
        assert!(transport.request_ids().is_empty());
    }
}
