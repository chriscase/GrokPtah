//! Durable admission, receipt, and tombstone ledger for external workers.
//!
//! External-worker mutations create state on a third-party system that this
//! host cannot roll back. Three durable records make that safe:
//!
//! * **Admissions** are the host's own mint ledger. A presented admission is
//!   authority only if this ledger says this host minted it, for this exact
//!   binding, and has not spent it yet. Forged and replayed tickets therefore
//!   fail closed on lookup rather than on signature checking.
//! * **Receipts** carry one mutation attempt from claim to disposition. A
//!   receipt that was in flight when the process stopped reopens as
//!   `Uncertain`, never as retryable.
//! * **Tombstones** are the permanent record that a mutation was accepted by a
//!   provider. They are never pruned, so a duplicate request can still be
//!   recognized long after its receipt has aged out of the ledger.
//!
//! This module deliberately owns no provider client and no credential: it
//! stores opaque identities, digests, and bounded redacted reasons only.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use grokptah_agent_sdk::{
    ExternalWorkerAdmission, ExternalWorkerMutation, ExternalWorkerReceipt,
    ExternalWorkerReceiptState, ExternalWorkerScope, ExternalWorkerTarget,
    EXTERNAL_WORKER_CONTRACT_VERSION,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::external_worker::ExternalWorkerAdapterError;

/// Maximum receipts retained before terminal ones are pruned.
const MAX_RECEIPTS: usize = 2_048;
/// Maximum live admissions retained before minting is refused.
const MAX_ADMISSIONS: usize = 1_024;
/// Maximum bytes read from any single durable record.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
/// Age after which a settled receipt may be pruned.
const TERMINAL_RECEIPT_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// Age after which a spent or expired admission may be pruned.
const SPENT_ADMISSION_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

/// Durable lifecycle of one minted admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionState {
    /// Minted and not yet spent.
    Minted,
    /// Spent by exactly one mutation claim.
    Spent,
    /// Withdrawn by the host before it was spent.
    Revoked,
}

/// The durable mint record backing one public [`ExternalWorkerAdmission`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionRecord {
    /// The exact admission this host minted.
    pub admission: ExternalWorkerAdmission,
    /// Current durable lifecycle state.
    pub state: AdmissionState,
    /// Last transition time in milliseconds since the Unix epoch.
    pub updated_at_ms: u64,
}

/// The permanent record that a provider accepted one mutation.
///
/// A tombstone outlives its receipt on purpose. Receipt retention is a
/// capacity policy; provider effect is not something a capacity policy may
/// forget, because forgetting it would make a duplicate request look fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationTombstone {
    /// Contract identifier; must equal the current external-worker contract.
    pub contract: String,
    /// Caller idempotency key that produced the accepted mutation.
    pub request_id: String,
    /// Admission that authorized the accepted mutation.
    pub admission_id: String,
    /// Mutation kind that was accepted.
    pub mutation: ExternalWorkerMutation,
    /// Exact identity fence the mutation was accepted under.
    pub scope: ExternalWorkerScope,
    /// Stable provider-facing request identity.
    pub provider_request_id: String,
    /// `sha256:<hex>` digest of the exact accepted payload.
    pub payload_digest: String,
    /// Opaque provider target created or affected by the mutation.
    pub target: ExternalWorkerTarget,
    /// Acceptance time in milliseconds since the Unix epoch.
    pub accepted_at_ms: u64,
}

/// What the ledger says a caller may do with an incoming mutation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationClaim {
    /// No prior attempt exists; the caller owns this send.
    Perform(ExternalWorkerReceipt),
    /// An identical attempt is in flight and has not settled.
    Pending(ExternalWorkerReceipt),
    /// An earlier attempt left provider state unknown; retry is blocked.
    Uncertain(ExternalWorkerReceipt),
    /// The mutation already took effect; replay the recorded receipt.
    Replay(ExternalWorkerReceipt),
    /// An earlier attempt settled with no provider effect.
    Rejected(ExternalWorkerReceipt),
}

/// Durable ledger for external-worker admissions, receipts, and tombstones.
#[derive(Clone)]
pub struct ExternalWorkerStore {
    inner: Arc<ExternalWorkerStoreInner>,
}

struct ExternalWorkerStoreInner {
    root: PathBuf,
    _store_lock: fs::File,
    lock: Mutex<()>,
}

impl ExternalWorkerStore {
    /// Open (or create) the ledger under `root`, taking an exclusive lock.
    ///
    /// Opening also reconciles: any receipt that was in flight when the
    /// process stopped is reopened as `Uncertain`, and settled records past
    /// their retention are pruned. Tombstones are never pruned.
    pub fn open(root: impl AsRef<Path>, now_ms: u64) -> Result<Self, ExternalWorkerAdapterError> {
        let root = root.as_ref().to_path_buf();
        for directory in ["admissions", "receipts", "tombstones"] {
            fs::create_dir_all(root.join(directory)).map_err(durable)?;
        }
        let root = dunce::canonicalize(&root).map_err(durable)?;
        let lock_path = root.join(".store.lock");
        let store_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(durable)?;
        store_lock.try_lock_exclusive().map_err(|error| {
            ExternalWorkerAdapterError::Durable(format!(
                "external-worker store is already open ({error})"
            ))
        })?;
        let store = Self {
            inner: Arc::new(ExternalWorkerStoreInner {
                root,
                _store_lock: store_lock,
                lock: Mutex::new(()),
            }),
        };
        store.reopen_in_flight_as_uncertain(now_ms)?;
        store.prune(now_ms)?;
        Ok(store)
    }

    /// Persist a freshly minted admission.
    pub fn record_admission(
        &self,
        admission: &ExternalWorkerAdmission,
        now_ms: u64,
    ) -> Result<(), ExternalWorkerAdapterError> {
        admission
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        let _guard = self.inner.lock.lock();
        if count_json_files(&self.inner.root.join("admissions")).map_err(durable)? >= MAX_ADMISSIONS
        {
            return Err(ExternalWorkerAdapterError::Conflict(
                "external-worker admission ledger is full",
            ));
        }
        let path = self.admission_path(&admission.nonce)?;
        let record = AdmissionRecord {
            admission: admission.clone(),
            state: AdmissionState::Minted,
            updated_at_ms: now_ms,
        };
        // Exclusive creation: a nonce is never minted twice, so a collision is
        // a hard failure rather than an overwrite.
        write_json_exclusive(&path, &record)
    }

    /// Read the durable mint record for one nonce.
    pub fn load_admission(
        &self,
        nonce: &str,
    ) -> Result<Option<AdmissionRecord>, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        self.load_admission_unlocked(nonce)
    }

    /// Spend one minted admission, making it unusable for any further claim.
    pub fn spend_admission(
        &self,
        nonce: &str,
        now_ms: u64,
    ) -> Result<AdmissionRecord, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        let mut record = self.load_admission_unlocked(nonce)?.ok_or(
            ExternalWorkerAdapterError::AdmissionRejected("admission was not minted by this host"),
        )?;
        if record.state != AdmissionState::Minted {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission has already been spent or revoked",
            ));
        }
        record.state = AdmissionState::Spent;
        record.updated_at_ms = now_ms;
        atomic_write_json(&self.admission_path(nonce)?, &record)?;
        Ok(record)
    }

    /// Withdraw an unspent admission.
    pub fn revoke_admission(
        &self,
        nonce: &str,
        now_ms: u64,
    ) -> Result<(), ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        let Some(mut record) = self.load_admission_unlocked(nonce)? else {
            return Ok(());
        };
        if record.state == AdmissionState::Minted {
            record.state = AdmissionState::Revoked;
            record.updated_at_ms = now_ms;
            atomic_write_json(&self.admission_path(nonce)?, &record)?;
        }
        Ok(())
    }

    /// Claim the right to send one mutation, or explain why it is blocked.
    ///
    /// A tombstone wins over a receipt: once a mutation is known to have taken
    /// effect, the answer is a replay even if the receipt itself was pruned.
    pub fn claim_mutation(
        &self,
        receipt: &ExternalWorkerReceipt,
    ) -> Result<MutationClaim, ExternalWorkerAdapterError> {
        receipt
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        let _guard = self.inner.lock.lock();

        if let Some(tombstone) = self.load_tombstone_unlocked(&receipt.request_id)? {
            self.assert_same_intent(&tombstone, receipt)?;
            let existing = self.load_receipt_unlocked(&receipt.request_id)?;
            return Ok(MutationClaim::Replay(
                existing.unwrap_or_else(|| accepted_receipt_from(&tombstone, receipt)),
            ));
        }

        if let Some(existing) = self.load_receipt_unlocked(&receipt.request_id)? {
            if existing.mutation != receipt.mutation
                || existing.payload_digest != receipt.payload_digest
                || existing.scope != receipt.scope
                || existing.provider != receipt.provider
                || existing.provider_id != receipt.provider_id
            {
                return Err(ExternalWorkerAdapterError::Conflict(
                    "request id was reused for a different external-worker mutation",
                ));
            }
            return Ok(match existing.state {
                ExternalWorkerReceiptState::Claimed => MutationClaim::Pending(existing),
                ExternalWorkerReceiptState::Uncertain => MutationClaim::Uncertain(existing),
                ExternalWorkerReceiptState::Accepted => MutationClaim::Replay(existing),
                ExternalWorkerReceiptState::Rejected => MutationClaim::Rejected(existing),
            });
        }

        if count_json_files(&self.inner.root.join("receipts")).map_err(durable)? >= MAX_RECEIPTS {
            return Err(ExternalWorkerAdapterError::Conflict(
                "external-worker receipt ledger is full",
            ));
        }
        write_json_exclusive(&self.receipt_path(&receipt.request_id)?, receipt)?;
        Ok(MutationClaim::Perform(receipt.clone()))
    }

    /// Settle a claimed receipt and, when accepted, write its tombstone.
    ///
    /// The tombstone is written before the receipt transition so a crash
    /// between the two leaves the stronger record standing: a duplicate is
    /// recognized as already-effective rather than as fresh.
    pub fn settle_mutation(
        &self,
        receipt: &ExternalWorkerReceipt,
    ) -> Result<(), ExternalWorkerAdapterError> {
        receipt
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        if receipt.state == ExternalWorkerReceiptState::Claimed {
            return Err(ExternalWorkerAdapterError::Conflict(
                "a settled receipt must leave the claimed state",
            ));
        }
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(&receipt.request_id)?;
        let existing = self.load_receipt_unlocked(&receipt.request_id)?.ok_or(
            ExternalWorkerAdapterError::Conflict("no claimed receipt to settle"),
        )?;
        if existing.state != ExternalWorkerReceiptState::Claimed {
            return Err(ExternalWorkerAdapterError::Conflict(
                "external-worker receipt has already settled",
            ));
        }
        if receipt.state.is_accepted() {
            let target = receipt
                .target
                .clone()
                .ok_or(ExternalWorkerAdapterError::Conflict(
                    "accepted mutation has no provider target",
                ))?;
            let tombstone = MutationTombstone {
                contract: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
                request_id: receipt.request_id.clone(),
                admission_id: receipt.admission_id.clone(),
                mutation: receipt.mutation,
                scope: receipt.scope.clone(),
                provider_request_id: receipt.provider_request_id.clone(),
                payload_digest: receipt.payload_digest.clone(),
                target,
                accepted_at_ms: receipt.updated_at_ms,
            };
            atomic_write_json(&self.tombstone_path(&receipt.request_id)?, &tombstone)?;
        }
        atomic_write_json(&path, receipt)
    }

    /// Resolve an uncertain receipt into a settled disposition.
    ///
    /// Reconciliation is the only way out of `Uncertain`. It is an explicit
    /// operator or provider-read decision, never a timeout.
    pub fn reconcile_mutation(
        &self,
        request_id: &str,
        resolved: ExternalWorkerReceiptState,
        target: Option<ExternalWorkerTarget>,
        reason: &str,
        now_ms: u64,
    ) -> Result<ExternalWorkerReceipt, ExternalWorkerAdapterError> {
        if !matches!(
            resolved,
            ExternalWorkerReceiptState::Accepted | ExternalWorkerReceiptState::Rejected
        ) {
            return Err(ExternalWorkerAdapterError::Conflict(
                "reconciliation must settle as accepted or rejected",
            ));
        }
        let _guard = self.inner.lock.lock();
        let mut receipt =
            self.load_receipt_unlocked(request_id)?
                .ok_or(ExternalWorkerAdapterError::Conflict(
                    "no receipt to reconcile",
                ))?;
        if receipt.state != ExternalWorkerReceiptState::Uncertain {
            return Err(ExternalWorkerAdapterError::Conflict(
                "only an uncertain receipt can be reconciled",
            ));
        }
        receipt.state = resolved;
        receipt.reason = reason.to_owned();
        receipt.updated_at_ms = now_ms;
        if resolved.is_accepted() {
            receipt.target = target.or(receipt.target);
        }
        receipt
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        if resolved.is_accepted() {
            let tombstone = MutationTombstone {
                contract: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
                request_id: receipt.request_id.clone(),
                admission_id: receipt.admission_id.clone(),
                mutation: receipt.mutation,
                scope: receipt.scope.clone(),
                provider_request_id: receipt.provider_request_id.clone(),
                payload_digest: receipt.payload_digest.clone(),
                target: receipt
                    .target
                    .clone()
                    .expect("validated accepted receipt has a target"),
                accepted_at_ms: now_ms,
            };
            atomic_write_json(&self.tombstone_path(request_id)?, &tombstone)?;
        }
        atomic_write_json(&self.receipt_path(request_id)?, &receipt)?;
        Ok(receipt)
    }

    /// Read one receipt by its owning idempotency key.
    pub fn load_receipt(
        &self,
        request_id: &str,
    ) -> Result<Option<ExternalWorkerReceipt>, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        self.load_receipt_unlocked(request_id)
    }

    /// Read the permanent acceptance record for one idempotency key.
    pub fn load_tombstone(
        &self,
        request_id: &str,
    ) -> Result<Option<MutationTombstone>, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        self.load_tombstone_unlocked(request_id)
    }

    /// Number of receipts currently retained.
    pub fn receipt_count(&self) -> Result<usize, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        count_json_files(&self.inner.root.join("receipts")).map_err(durable)
    }

    /// Number of permanent acceptance records currently retained.
    pub fn tombstone_count(&self) -> Result<usize, ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        count_json_files(&self.inner.root.join("tombstones")).map_err(durable)
    }

    /// Apply retention. Tombstones are deliberately not considered here.
    pub fn prune(&self, now_ms: u64) -> Result<(), ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();

        let mut receipts = Vec::new();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(durable)? {
            let receipt: ExternalWorkerReceipt = self.read_receipt_path(&path)?;
            receipts.push((path, receipt));
        }
        receipts.sort_by(|left, right| right.1.updated_at_ms.cmp(&left.1.updated_at_ms));
        for (index, (path, receipt)) in receipts.into_iter().enumerate() {
            // Claimed and uncertain receipts are load-bearing: dropping one
            // would silently unblock a retry of a mutation whose provider
            // effect is still unknown.
            let settled = matches!(
                receipt.state,
                ExternalWorkerReceiptState::Accepted | ExternalWorkerReceiptState::Rejected
            );
            let expired = now_ms.saturating_sub(receipt.updated_at_ms) > TERMINAL_RECEIPT_AGE_MS;
            if settled && (index >= MAX_RECEIPTS || expired) {
                fs::remove_file(path).map_err(durable)?;
            }
        }

        for path in json_paths(&self.inner.root.join("admissions")).map_err(durable)? {
            let record = self.read_admission_path(&path)?;
            let settled = record.state != AdmissionState::Minted;
            let expired = now_ms >= record.admission.expires_at_ms;
            let stale = now_ms.saturating_sub(record.updated_at_ms) > SPENT_ADMISSION_AGE_MS;
            if (settled || expired) && stale {
                fs::remove_file(path).map_err(durable)?;
            }
        }
        Ok(())
    }

    fn reopen_in_flight_as_uncertain(&self, now_ms: u64) -> Result<(), ExternalWorkerAdapterError> {
        let _guard = self.inner.lock.lock();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(durable)? {
            let mut receipt = self.read_receipt_path(&path)?;
            if receipt.state != ExternalWorkerReceiptState::Claimed {
                continue;
            }
            receipt.state = ExternalWorkerReceiptState::Uncertain;
            receipt.reason =
                "host stopped while the external-worker mutation was in flight".to_owned();
            receipt.updated_at_ms = receipt.updated_at_ms.max(now_ms);
            atomic_write_json(&path, &receipt)?;
        }
        Ok(())
    }

    fn assert_same_intent(
        &self,
        tombstone: &MutationTombstone,
        receipt: &ExternalWorkerReceipt,
    ) -> Result<(), ExternalWorkerAdapterError> {
        if tombstone.mutation != receipt.mutation
            || tombstone.payload_digest != receipt.payload_digest
            || tombstone.scope != receipt.scope
            || tombstone.provider_request_id != receipt.provider_request_id
        {
            return Err(ExternalWorkerAdapterError::Conflict(
                "request id was reused for a different external-worker mutation",
            ));
        }
        Ok(())
    }

    fn load_admission_unlocked(
        &self,
        nonce: &str,
    ) -> Result<Option<AdmissionRecord>, ExternalWorkerAdapterError> {
        let path = self.admission_path(nonce)?;
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(self.read_admission_path(&path)?))
    }

    fn load_receipt_unlocked(
        &self,
        request_id: &str,
    ) -> Result<Option<ExternalWorkerReceipt>, ExternalWorkerAdapterError> {
        let path = self.receipt_path(request_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(self.read_receipt_path(&path)?))
    }

    fn load_tombstone_unlocked(
        &self,
        request_id: &str,
    ) -> Result<Option<MutationTombstone>, ExternalWorkerAdapterError> {
        let path = self.tombstone_path(request_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let tombstone: MutationTombstone = read_json(&path)?;
        if tombstone.contract != EXTERNAL_WORKER_CONTRACT_VERSION
            || self.tombstone_path(&tombstone.request_id)? != path
        {
            return Err(ExternalWorkerAdapterError::Durable(
                "external-worker tombstone identity does not match its durable path".into(),
            ));
        }
        Ok(Some(tombstone))
    }

    fn read_receipt_path(
        &self,
        path: &Path,
    ) -> Result<ExternalWorkerReceipt, ExternalWorkerAdapterError> {
        let receipt: ExternalWorkerReceipt = read_json(path)?;
        receipt
            .validate()
            .map_err(|reason| ExternalWorkerAdapterError::Durable(reason.into()))?;
        if self.receipt_path(&receipt.request_id)? != path {
            return Err(ExternalWorkerAdapterError::Durable(
                "external-worker receipt identity does not match its durable path".into(),
            ));
        }
        Ok(receipt)
    }

    fn read_admission_path(
        &self,
        path: &Path,
    ) -> Result<AdmissionRecord, ExternalWorkerAdapterError> {
        let record: AdmissionRecord = read_json(path)?;
        record
            .admission
            .validate()
            .map_err(|reason| ExternalWorkerAdapterError::Durable(reason.into()))?;
        if self.admission_path(&record.admission.nonce)? != path {
            return Err(ExternalWorkerAdapterError::Durable(
                "external-worker admission identity does not match its durable path".into(),
            ));
        }
        Ok(record)
    }

    fn admission_path(&self, nonce: &str) -> Result<PathBuf, ExternalWorkerAdapterError> {
        Ok(self
            .inner
            .root
            .join("admissions")
            .join(format!("{}.json", safe_file_id(nonce)?)))
    }

    fn receipt_path(&self, request_id: &str) -> Result<PathBuf, ExternalWorkerAdapterError> {
        Ok(self
            .inner
            .root
            .join("receipts")
            .join(format!("{}.json", safe_file_id(request_id)?)))
    }

    fn tombstone_path(&self, request_id: &str) -> Result<PathBuf, ExternalWorkerAdapterError> {
        Ok(self
            .inner
            .root
            .join("tombstones")
            .join(format!("{}.json", safe_file_id(request_id)?)))
    }
}

fn accepted_receipt_from(
    tombstone: &MutationTombstone,
    template: &ExternalWorkerReceipt,
) -> ExternalWorkerReceipt {
    ExternalWorkerReceipt {
        contract: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
        request_id: tombstone.request_id.clone(),
        admission_id: tombstone.admission_id.clone(),
        mutation: tombstone.mutation,
        scope: tombstone.scope.clone(),
        provider: template.provider,
        provider_id: template.provider_id.clone(),
        provider_request_id: tombstone.provider_request_id.clone(),
        attempt: template.attempt,
        state: ExternalWorkerReceiptState::Accepted,
        target: Some(tombstone.target.clone()),
        payload_digest: tombstone.payload_digest.clone(),
        reason: "replayed from the durable acceptance tombstone".to_owned(),
        created_at_ms: tombstone.accepted_at_ms,
        updated_at_ms: tombstone.accepted_at_ms,
    }
}

/// Stable, path-safe filename for an opaque durable identity.
///
/// The digest keeps a caller-supplied identity from ever reaching the
/// filesystem verbatim, so a separator or a reserved name cannot escape the
/// ledger directory even if an upstream validator is bypassed.
fn safe_file_id(id: &str) -> Result<String, ExternalWorkerAdapterError> {
    if id.is_empty() || id.len() > 256 {
        return Err(ExternalWorkerAdapterError::InvalidRequest(
            "durable identity length is out of range",
        ));
    }
    if id.contains("..") || id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(ExternalWorkerAdapterError::InvalidRequest(
            "durable identity contains a path separator",
        ));
    }
    use sha2::{Digest, Sha256};
    Ok(hex_lower(&Sha256::digest(id.as_bytes())))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn json_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn count_json_files(dir: &Path) -> std::io::Result<usize> {
    Ok(json_paths(dir)?.len())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ExternalWorkerAdapterError> {
    let metadata = fs::metadata(path).map_err(durable)?;
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(ExternalWorkerAdapterError::Durable(
            "external-worker durable record exceeds its byte bound".into(),
        ));
    }
    let bytes = fs::read(path).map_err(durable)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ExternalWorkerAdapterError::Durable(error.to_string()))
}

fn atomic_write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ExternalWorkerAdapterError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ExternalWorkerAdapterError::Durable(error.to_string()))?;
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::File::create(&temporary).map_err(durable)?;
    file.write_all(&bytes).map_err(durable)?;
    file.sync_all().map_err(durable)?;
    drop(file);
    fs::rename(&temporary, path).map_err(durable)
}

fn write_json_exclusive<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ExternalWorkerAdapterError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ExternalWorkerAdapterError::Durable(error.to_string()))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ExternalWorkerAdapterError::Conflict(
                    "durable external-worker record already exists",
                )
            } else {
                durable(error)
            }
        })?;
    file.write_all(&bytes).map_err(durable)?;
    file.sync_all().map_err(durable)
}

fn durable(error: impl ToString) -> ExternalWorkerAdapterError {
    ExternalWorkerAdapterError::Durable(error.to_string())
}
