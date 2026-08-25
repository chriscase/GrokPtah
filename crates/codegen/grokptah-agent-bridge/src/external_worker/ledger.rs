//! Durable idempotency ledger for external-worker mutations.
//!
//! Launch, follow-up, and cancel are keyed by `request_id` plus a canonical
//! payload hash. Identical retries replay the original result. Payload drift
//! is rejected. Pending and Uncertain outcomes are represented explicitly and
//! fail closed until an operator reconciles them. This ledger is namespaced
//! away from Computer Use and core-agent MCP receipts.

use super::{ExternalWorkerAdapterError, ProviderConflictCode};
use fs2::FileExt;
use grokptah_agent_sdk::{ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest};
use parking_lot::{Mutex, MutexGuard};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How long a terminal receipt is retained before the ledger prunes it.
///
/// Only `Complete` and `Failed` receipts age out. Once a receipt is pruned an
/// identical retry is performed again rather than replayed, so this window is
/// the horizon over which the ledger promises replay — not a cache hint.
/// `Pending` and `Uncertain` receipts are never pruned: they must stay
/// fail-closed until a process or an operator reconciles them.
pub const TERMINAL_RECEIPT_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// Cross-process advisory lock guarding the ledger directory.
const LEDGER_LOCK_FILE: &str = ".ledger.lock";
/// Directory of per-owner advisory locks used to detect dead claim owners.
const OWNERS_DIR: &str = "owners";

/// Namespaced mutation classes stored by the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerOperation {
    /// Create a worker and its initial run.
    Launch,
    /// Queue a follow-up run on an existing worker.
    FollowUp,
    /// Cancel one provider run.
    Cancel,
}

impl ExternalWorkerOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::FollowUp => "follow_up",
            Self::Cancel => "cancel",
        }
    }
}

/// Durable status for one external-worker request_id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalWorkerLedgerStatus {
    /// The mutation is in flight and has no durable result yet.
    Pending,
    /// The mutation completed and may be replayed.
    Complete,
    /// The mutation failed with a durable, replayable error.
    Failed,
    /// The mutation was interrupted; fail closed until reconciled.
    Uncertain,
}

/// Result of claiming a request_id + payload hash.
#[derive(Debug)]
pub enum ExternalWorkerLedgerClaim {
    /// Caller must perform the remote mutation.
    Perform,
    /// Replay a successful prior result.
    Replay(serde_json::Value),
    /// Replay a durable failure.
    ReplayError(ExternalWorkerAdapterError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerReceipt {
    request_id: String,
    operation: ExternalWorkerOperation,
    payload_hash: String,
    status: ExternalWorkerLedgerStatus,
    response: serde_json::Value,
    error: Option<String>,
    /// Ledger instance that holds this claim. Absent on receipts written
    /// before ownership was recorded; those are treated as unowned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    /// When the receipt reached a terminal status, for retention only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
}

/// Filesystem-backed idempotency ledger for external workers.
#[derive(Clone)]
pub struct ExternalWorkerLedger {
    inner: Arc<LedgerInner>,
}

struct LedgerInner {
    root: PathBuf,
    lock: Mutex<()>,
    /// Identity stamped on every claim this instance makes.
    owner: String,
    /// Held for this instance's whole life. Another process can only take it
    /// once this one is gone, which is how a dead owner is detected.
    _owner_lock: fs::File,
}

/// Held across one ledger critical section.
///
/// The in-process mutex is taken first and the file lock second, in that order
/// everywhere, so threads in this process never deadlock against each other
/// and processes serialize on the file lock.
struct LedgerGuard<'a> {
    _process: MutexGuard<'a, ()>,
    file: fs::File,
}

impl Drop for LedgerGuard<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

impl ExternalWorkerLedger {
    /// Open (or create) a ledger under `root/external-workers/idempotency`.
    /// Orphaned pending claims become Uncertain and stay fail-closed.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ExternalWorkerAdapterError> {
        let root = root.as_ref().join("external-workers").join("idempotency");
        super::durable::create_private_dir_all(&root.join(OWNERS_DIR)).map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger could not be created",
            )
        })?;
        // A fresh identity per instance, so a restarted process never inherits
        // the liveness of the one before it.
        let owner = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4());
        let owner_lock_path = root.join(OWNERS_DIR).join(format!("{owner}.lock"));
        let owner_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&owner_lock_path)
            .map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest(
                    "external worker ledger could not be created",
                )
            })?;
        owner_lock.try_lock_exclusive().map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger owner lock is unavailable",
            )
        })?;
        let ledger = Self {
            inner: Arc::new(LedgerInner {
                root,
                lock: Mutex::new(()),
                owner,
                _owner_lock: owner_lock,
            }),
        };
        ledger.recover_orphans_and_prune()?;
        Ok(ledger)
    }

    /// Take the in-process mutex and the cross-process advisory lock.
    ///
    /// `claim` creates its pending receipt with `O_EXCL`, which is already
    /// atomic across processes, but `finish` and the startup sweep are
    /// read-modify-write. Without a lock shared beyond this process, two hosts
    /// on one GrokPtah home would lose updates to each other.
    fn guard(&self) -> Result<LedgerGuard<'_>, ExternalWorkerAdapterError> {
        let process = self.inner.lock.lock();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.inner.root.join(LEDGER_LOCK_FILE))
            .map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest(
                    "external worker ledger lock is unavailable",
                )
            })?;
        file.lock_exclusive().map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest("external worker ledger lock is unavailable")
        })?;
        Ok(LedgerGuard {
            _process: process,
            file,
        })
    }

    /// Path of an owner's advisory lock, or `None` if the recorded owner is
    /// not a safe file name. Receipts are written by this host, but they sit
    /// on disk, so a tampered owner must never steer a filesystem path.
    fn owner_lock_path(&self, owner: &str) -> Option<PathBuf> {
        if owner.is_empty()
            || owner.len() > 128
            || !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return None;
        }
        Some(
            self.inner
                .root
                .join(OWNERS_DIR)
                .join(format!("{owner}.lock")),
        )
    }

    /// True when no live process holds the claim recorded on a receipt.
    ///
    /// Liveness is decided by the owner's advisory lock, not by a pid: a pid
    /// can be recycled by an unrelated process, and an unowned legacy receipt
    /// has nobody to speak for it.
    fn owner_is_dead(&self, owner: Option<&str>, dead: &mut BTreeSet<String>) -> bool {
        let Some(owner) = owner else {
            return true;
        };
        if owner == self.inner.owner {
            return false;
        }
        if dead.contains(owner) {
            return true;
        }
        let Some(path) = self.owner_lock_path(owner) else {
            return true;
        };
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            return true;
        };
        // Non-blocking: a live owner holds this for its whole life.
        if file.try_lock_exclusive().is_ok() {
            let _ = FileExt::unlock(&file);
            dead.insert(owner.to_string());
            return true;
        }
        false
    }

    fn path(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
    ) -> Result<PathBuf, ExternalWorkerAdapterError> {
        if request_id.is_empty() || request_id.len() > 256 {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "request_id length is out of range",
            ));
        }
        if request_id.contains("..")
            || request_id.contains('/')
            || request_id.contains('\\')
            || request_id.contains('\0')
        {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "request_id contains path separators",
            ));
        }
        let digest = hex_sha256(request_id.as_bytes());
        Ok(self
            .inner
            .root
            .join(operation.as_str())
            .join(format!("{digest}.json")))
    }

    /// Startup sweep: adopt genuinely orphaned claims and age out terminal
    /// receipts.
    ///
    /// A pending receipt is only adopted when its owner is gone. Adopting one
    /// that a live process still holds would be worse than leaving it: the
    /// owner's `finish` would then be refused, so a provider mutation that did
    /// happen could never be recorded, and the real worker would be stranded
    /// behind an Uncertain receipt.
    fn recover_orphans_and_prune(&self) -> Result<(), ExternalWorkerAdapterError> {
        let _g = self.guard()?;
        let mut dead_owners = BTreeSet::new();
        for operation in [
            ExternalWorkerOperation::Launch,
            ExternalWorkerOperation::FollowUp,
            ExternalWorkerOperation::Cancel,
        ] {
            let dir = self.inner.root.join(operation.as_str());
            if !dir.is_dir() {
                continue;
            }
            let entries = fs::read_dir(&dir).map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest("external worker ledger is unreadable")
            })?;
            for entry in entries {
                let path = entry
                    .map_err(|_| {
                        ExternalWorkerAdapterError::InvalidRequest(
                            "external worker ledger is unreadable",
                        )
                    })?
                    .path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(mut receipt) = serde_json::from_str::<LedgerReceipt>(&text) else {
                    continue;
                };
                match receipt.status {
                    ExternalWorkerLedgerStatus::Pending => {
                        if !self.owner_is_dead(receipt.owner.as_deref(), &mut dead_owners) {
                            continue;
                        }
                        receipt.status = ExternalWorkerLedgerStatus::Uncertain;
                        receipt.error =
                            Some("interrupted before a durable result was recorded".into());
                        receipt.finished_at = Some(now_rfc3339());
                        receipt.owner = None;
                        atomic_write_json(&path, &receipt)?;
                    }
                    ExternalWorkerLedgerStatus::Complete | ExternalWorkerLedgerStatus::Failed => {
                        if terminal_receipt_is_expired(&receipt, &path) {
                            let _ = fs::remove_file(&path);
                        }
                    }
                    // Never pruned: an unreconciled outcome must keep failing
                    // closed rather than ageing into a fresh Perform.
                    ExternalWorkerLedgerStatus::Uncertain => {}
                }
            }
        }
        for owner in dead_owners {
            if let Some(path) = self.owner_lock_path(&owner) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }

    /// Atomically claim `request_id` for one operation.
    pub fn claim(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        payload_hash: &str,
    ) -> Result<ExternalWorkerLedgerClaim, ExternalWorkerAdapterError> {
        let path = self.path(operation, request_id)?;
        let _g = self.guard()?;
        if path.is_file() {
            let text = fs::read_to_string(&path).map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest("external worker ledger is unreadable")
            })?;
            let prev: LedgerReceipt = serde_json::from_str(&text).map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest("external worker ledger is corrupt")
            })?;
            if prev.request_id != request_id
                || prev.operation != operation
                || prev.payload_hash != payload_hash
            {
                return Err(ExternalWorkerAdapterError::PayloadDrift);
            }
            return match prev.status {
                ExternalWorkerLedgerStatus::Complete => {
                    Ok(ExternalWorkerLedgerClaim::Replay(prev.response))
                }
                ExternalWorkerLedgerStatus::Failed => Ok(ExternalWorkerLedgerClaim::ReplayError(
                    replayed_error(prev.error.as_deref()),
                )),
                ExternalWorkerLedgerStatus::Pending => Err(ExternalWorkerAdapterError::Pending),
                ExternalWorkerLedgerStatus::Uncertain => Err(ExternalWorkerAdapterError::Uncertain),
            };
        }
        if let Some(parent) = path.parent() {
            super::durable::create_private_dir_all(parent).map_err(|_| {
                ExternalWorkerAdapterError::InvalidRequest(
                    "external worker ledger could not be created",
                )
            })?;
        }
        let pending = LedgerReceipt {
            request_id: request_id.into(),
            operation,
            payload_hash: payload_hash.into(),
            status: ExternalWorkerLedgerStatus::Pending,
            response: serde_json::Value::Null,
            error: None,
            owner: Some(self.inner.owner.clone()),
            finished_at: None,
        };
        match write_json_exclusive(&path, &pending) {
            Ok(()) => Ok(ExternalWorkerLedgerClaim::Perform),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(ExternalWorkerAdapterError::Pending)
            }
            Err(_) => Err(ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger could not be claimed",
            )),
        }
    }

    /// Persist a successful result for an in-flight claim.
    pub fn complete(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        payload_hash: &str,
        response: serde_json::Value,
    ) -> Result<(), ExternalWorkerAdapterError> {
        self.finish(
            operation,
            request_id,
            payload_hash,
            ExternalWorkerLedgerStatus::Complete,
            response,
            None,
        )
    }

    /// Persist a durable failure for an in-flight claim.
    pub fn fail(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        payload_hash: &str,
        error: &ExternalWorkerAdapterError,
    ) -> Result<(), ExternalWorkerAdapterError> {
        self.finish(
            operation,
            request_id,
            payload_hash,
            ExternalWorkerLedgerStatus::Failed,
            serde_json::Value::Null,
            Some(durable_error_label(error)),
        )
    }

    /// Mark an in-flight claim Uncertain when remote side-effects cannot be proven.
    pub fn uncertain(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        payload_hash: &str,
    ) -> Result<(), ExternalWorkerAdapterError> {
        self.finish(
            operation,
            request_id,
            payload_hash,
            ExternalWorkerLedgerStatus::Uncertain,
            serde_json::Value::Null,
            Some("provider outcome could not be reconciled".into()),
        )
    }

    fn finish(
        &self,
        operation: ExternalWorkerOperation,
        request_id: &str,
        payload_hash: &str,
        status: ExternalWorkerLedgerStatus,
        response: serde_json::Value,
        error: Option<String>,
    ) -> Result<(), ExternalWorkerAdapterError> {
        let path = self.path(operation, request_id)?;
        let _g = self.guard()?;
        if !path.is_file() {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger claim is missing",
            ));
        }
        let text = fs::read_to_string(&path).map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest("external worker ledger is unreadable")
        })?;
        let previous: LedgerReceipt = serde_json::from_str(&text).map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest("external worker ledger is corrupt")
        })?;
        if previous.request_id != request_id
            || previous.operation != operation
            || previous.payload_hash != payload_hash
        {
            return Err(ExternalWorkerAdapterError::PayloadDrift);
        }
        if previous.status != ExternalWorkerLedgerStatus::Pending {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger claim is no longer pending",
            ));
        }
        // Only the instance that took the claim may finish it. A receipt with
        // no recorded owner predates ownership and is still finishable.
        if previous
            .owner
            .as_deref()
            .is_some_and(|owner| owner != self.inner.owner)
        {
            return Err(ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger claim belongs to another owner",
            ));
        }
        let receipt = LedgerReceipt {
            request_id: request_id.into(),
            operation,
            payload_hash: payload_hash.into(),
            status,
            response,
            error,
            owner: previous.owner,
            finished_at: Some(now_rfc3339()),
        };
        atomic_write_json(&path, &receipt)
    }
}

/// Canonical hash of a launch request excluding `request_id`.
pub fn canonical_launch_payload_hash(
    request: &ExternalWorkerLaunchRequest,
) -> Result<String, ExternalWorkerAdapterError> {
    let mut value = serde_json::to_value(request).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("launch payload could not be canonicalized")
    })?;
    value
        .as_object_mut()
        .ok_or(ExternalWorkerAdapterError::InvalidRequest(
            "launch payload could not be canonicalized",
        ))?
        .remove("requestId");
    Ok(hash_canonical(&value))
}

/// Canonical hash of a follow-up request plus the targeted worker.
pub fn canonical_follow_up_payload_hash(
    external_agent_id: &str,
    request: &ExternalWorkerFollowUpRequest,
) -> Result<String, ExternalWorkerAdapterError> {
    let mut value = serde_json::to_value(request).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("follow-up payload could not be canonicalized")
    })?;
    let object = value
        .as_object_mut()
        .ok_or(ExternalWorkerAdapterError::InvalidRequest(
            "follow-up payload could not be canonicalized",
        ))?;
    object.remove("requestId");
    object.insert(
        "externalAgentId".into(),
        serde_json::Value::String(external_agent_id.to_string()),
    );
    Ok(hash_canonical(&value))
}

/// Canonical hash of a cancel intent.
pub fn canonical_cancel_payload_hash(external_agent_id: &str, external_run_id: &str) -> String {
    hash_canonical(&serde_json::json!({
        "externalAgentId": external_agent_id,
        "externalRunId": external_run_id,
    }))
}

fn hash_canonical(value: &serde_json::Value) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_default();
    hex_sha256(encoded.as_bytes())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn durable_error_label(error: &ExternalWorkerAdapterError) -> String {
    match error {
        ExternalWorkerAdapterError::InvalidRequest(message) => (*message).to_string(),
        ExternalWorkerAdapterError::InvalidResponse(message) => (*message).to_string(),
        ExternalWorkerAdapterError::UnsupportedProvider => "unsupported provider".into(),
        ExternalWorkerAdapterError::Pending => "pending".into(),
        ExternalWorkerAdapterError::Uncertain => "uncertain".into(),
        ExternalWorkerAdapterError::PayloadDrift => "payload drift".into(),
        ExternalWorkerAdapterError::InvalidBaseUrl => "invalid API base".into(),
        ExternalWorkerAdapterError::Provider { status, code } => match code {
            Some(code) => format!("provider {} {}", status.as_u16(), code.as_str()),
            None => format!("provider {}", status.as_u16()),
        },
        ExternalWorkerAdapterError::Transport(_) => "transport failed".into(),
    }
}

fn replayed_error(label: Option<&str>) -> ExternalWorkerAdapterError {
    match label {
        Some("unsupported provider") => ExternalWorkerAdapterError::UnsupportedProvider,
        Some("pending") => ExternalWorkerAdapterError::Pending,
        Some("uncertain") => ExternalWorkerAdapterError::Uncertain,
        Some("payload drift") => ExternalWorkerAdapterError::PayloadDrift,
        Some("invalid API base") => ExternalWorkerAdapterError::InvalidBaseUrl,
        Some(message) => {
            if let Some(rest) = message.strip_prefix("provider ") {
                let mut parts = rest.splitn(2, ' ');
                if let Some(status) = parts
                    .next()
                    .and_then(|value| value.parse::<u16>().ok())
                    .and_then(|code| StatusCode::from_u16(code).ok())
                {
                    let code = parts.next().and_then(ProviderConflictCode::parse);
                    return ExternalWorkerAdapterError::Provider { status, code };
                }
            }
            ExternalWorkerAdapterError::InvalidRequest(leak_static(message))
        }
        None => ExternalWorkerAdapterError::InvalidRequest("idempotent mutation failed"),
    }
}

fn leak_static(message: &str) -> &'static str {
    // Durable receipts only store a small closed set of adapter labels plus
    // the original &'static InvalidRequest/InvalidResponse strings.
    match message {
        "Cursor workers must be isolated" => "Cursor workers must be isolated",
        "pull-request creation requires a separate approval action" => {
            "pull-request creation requires a separate approval action"
        }
        "Cursor repository allowlist is not configured" => {
            "Cursor repository allowlist is not configured"
        }
        "repository is not in the Cursor allowlist" => "repository is not in the Cursor allowlist",
        "repository is not in the host allowlist" => "repository is not in the host allowlist",
        "Cursor follow-up bounds are not supported by the provider API" => {
            "Cursor follow-up bounds are not supported by the provider API"
        }
        "Cursor worker is not eligible for a follow-up" => {
            "Cursor worker is not eligible for a follow-up"
        }
        "Cursor worker already has an active run" => "Cursor worker already has an active run",
        "Cursor run is not cancellable" => "Cursor run is not cancellable",
        "Cursor cancellation did not return a terminal cancelled run" => {
            "Cursor cancellation did not return a terminal cancelled run"
        }
        "idempotent mutation failed" => "idempotent mutation failed",
        _ => "idempotent mutation failed",
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// True when a terminal receipt has outlived the retention window.
///
/// An unreadable or unparseable stamp keeps the receipt: dropping one early
/// would turn an identical retry back into a fresh provider mutation, so the
/// safe direction is to retain.
fn terminal_receipt_is_expired(receipt: &LedgerReceipt, path: &Path) -> bool {
    let retention = chrono::Duration::seconds(TERMINAL_RECEIPT_RETENTION_SECS as i64);
    if let Some(finished_at) = receipt.finished_at.as_deref() {
        return chrono::DateTime::parse_from_rfc3339(finished_at).is_ok_and(|finished| {
            chrono::Utc::now().signed_duration_since(finished.with_timezone(&chrono::Utc))
                > retention
        });
    }
    // Written before receipts carried a stamp: fall back to file age.
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > Duration::from_secs(TERMINAL_RECEIPT_RETENTION_SECS))
}

/// Publish a receipt atomically, privately, and crash-durably.
///
/// Delegates to [`super::durable`]: an unpredictable private temp name, an
/// `O_NOFOLLOW` create, an `fsync` of the contents before the rename and of the
/// parent directory after it. The previous implementation staged under a
/// guessable `.json.tmp`, created it with `File::create` (following a symlink
/// at that name, world-readable by default), and never synced the parent, so a
/// crash could lose the rename even when the bytes had reached the disk.
fn atomic_write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ExternalWorkerAdapterError> {
    super::durable::write_private_json(path, value).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be written")
    })
}

/// Publish a receipt only if nothing has claimed this path yet.
///
/// This is the claim's compare-and-swap: exactly one writer may create the
/// pending receipt, which is what makes a concurrent identical request fail
/// closed on `Pending` instead of launching twice.
fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    super::durable::cas_private_json(path, None, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokptah_agent_sdk::{ExternalWorkerExecutionMode, ExternalWorkerProvider};

    fn launch(request_id: &str, prompt: &str) -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: request_id.into(),
            provider: ExternalWorkerProvider::CursorCloud,
            provider_id: None,
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "main".into(),
            prompt: prompt.into(),
            model: None,
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: None,
        }
    }

    #[test]
    fn identical_retries_replay_and_payload_drift_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-1", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        match ledger
            .claim(ExternalWorkerOperation::Launch, "req-1", &hash)
            .unwrap()
        {
            ExternalWorkerLedgerClaim::Perform => {}
            other => panic!("expected perform, got {other:?}"),
        }
        ledger
            .complete(
                ExternalWorkerOperation::Launch,
                "req-1",
                &hash,
                serde_json::json!({"ok": true}),
            )
            .unwrap();
        match ledger
            .claim(ExternalWorkerOperation::Launch, "req-1", &hash)
            .unwrap()
        {
            ExternalWorkerLedgerClaim::Replay(value) => assert_eq!(value["ok"], true),
            other => panic!("expected replay, got {other:?}"),
        }
        let drifted = launch("req-1", "different work");
        let drifted_hash = canonical_launch_payload_hash(&drifted).unwrap();
        assert!(matches!(
            ledger.claim(ExternalWorkerOperation::Launch, "req-1", &drifted_hash),
            Err(ExternalWorkerAdapterError::PayloadDrift)
        ));
    }

    #[test]
    fn pending_and_orphaned_uncertain_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-pending", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        ledger
            .claim(ExternalWorkerOperation::Launch, "req-pending", &hash)
            .unwrap();
        assert!(matches!(
            ledger.claim(ExternalWorkerOperation::Launch, "req-pending", &hash),
            Err(ExternalWorkerAdapterError::Pending)
        ));
        drop(ledger);
        let reopened = ExternalWorkerLedger::open(dir.path()).unwrap();
        assert!(matches!(
            reopened.claim(ExternalWorkerOperation::Launch, "req-pending", &hash),
            Err(ExternalWorkerAdapterError::Uncertain)
        ));
    }

    fn receipt_path(root: &Path, operation: ExternalWorkerOperation, request_id: &str) -> PathBuf {
        root.join("external-workers")
            .join("idempotency")
            .join(operation.as_str())
            .join(format!("{}.json", hex_sha256(request_id.as_bytes())))
    }

    /// Backdate a terminal receipt past the retention window.
    fn backdate(path: &Path, days: i64) {
        let mut receipt: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let stamp = chrono::Utc::now() - chrono::Duration::days(days);
        receipt["finishedAt"] = serde_json::json!(stamp.to_rfc3339());
        fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }

    /// The contrast with `pending_and_orphaned_uncertain_fail_closed`, which
    /// drops the owner first. While the owner is alive its claim is not an
    /// orphan, and adopting it would leave the owner unable to record a
    /// provider mutation that really happened.
    #[test]
    fn a_live_owners_pending_claim_is_not_adopted_by_another_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-live", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        owner
            .claim(ExternalWorkerOperation::Launch, "req-live", &hash)
            .unwrap();

        // A second host opens the same GrokPtah home while the owner runs.
        let other = ExternalWorkerLedger::open(dir.path()).unwrap();
        assert!(
            matches!(
                other.claim(ExternalWorkerOperation::Launch, "req-live", &hash),
                Err(ExternalWorkerAdapterError::Pending)
            ),
            "a live owner's claim must stay Pending, not be adopted as Uncertain"
        );

        // The owner can still record its result.
        owner
            .complete(
                ExternalWorkerOperation::Launch,
                "req-live",
                &hash,
                serde_json::json!({"ok": true}),
            )
            .expect("the owner must still be able to finish its own claim");
        match other
            .claim(ExternalWorkerOperation::Launch, "req-live", &hash)
            .unwrap()
        {
            ExternalWorkerLedgerClaim::Replay(value) => assert_eq!(value["ok"], true),
            other => panic!("expected replay, got {other:?}"),
        }
    }

    #[test]
    fn finish_is_refused_for_a_claim_owned_by_another_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let owner = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-owned", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        owner
            .claim(ExternalWorkerOperation::Launch, "req-owned", &hash)
            .unwrap();
        let other = ExternalWorkerLedger::open(dir.path()).unwrap();
        let error = other
            .complete(
                ExternalWorkerOperation::Launch,
                "req-owned",
                &hash,
                serde_json::json!({"ok": true}),
            )
            .expect_err("a foreign ledger must not finish this claim");
        assert!(
            matches!(
                error,
                ExternalWorkerAdapterError::InvalidRequest(
                    "external worker ledger claim belongs to another owner"
                )
            ),
            "got {error:?}"
        );
    }

    /// Receipts decide whether a provider mutation may be retried, so they
    /// must not be readable by another local user, must not be redirectable
    /// through a symlink planted at the receipt name, and must not be staged
    /// under a name an attacker can guess.
    #[test]
    fn receipts_are_written_privately_and_leave_no_guessable_staging() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-private", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        ledger
            .claim(ExternalWorkerOperation::Launch, "req-private", &hash)
            .unwrap();
        ledger
            .complete(
                ExternalWorkerOperation::Launch,
                "req-private",
                &hash,
                serde_json::json!({"ok": true}),
            )
            .unwrap();

        let path = receipt_path(dir.path(), ExternalWorkerOperation::Launch, "req-private");
        assert!(path.exists(), "receipt was published");
        assert!(
            !path.with_extension("json.tmp").exists(),
            "the guessable staging name must not be used",
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "receipt must not be group/other readable");
            let parent = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(parent & 0o077, 0, "receipt directory must be owner-only");
        }
    }

    #[test]
    fn terminal_receipts_are_pruned_after_the_retention_window() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-old", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        ledger
            .claim(ExternalWorkerOperation::Launch, "req-old", &hash)
            .unwrap();
        ledger
            .complete(
                ExternalWorkerOperation::Launch,
                "req-old",
                &hash,
                serde_json::json!({"ok": true}),
            )
            .unwrap();
        let path = receipt_path(dir.path(), ExternalWorkerOperation::Launch, "req-old");
        assert!(path.is_file());

        // Still inside the window: retained and replayable.
        drop(ledger);
        let fresh = ExternalWorkerLedger::open(dir.path()).unwrap();
        assert!(matches!(
            fresh
                .claim(ExternalWorkerOperation::Launch, "req-old", &hash)
                .unwrap(),
            ExternalWorkerLedgerClaim::Replay(_)
        ));
        drop(fresh);

        backdate(&path, (TERMINAL_RECEIPT_RETENTION_SECS / 86_400) as i64 + 1);
        let pruned = ExternalWorkerLedger::open(dir.path()).unwrap();
        assert!(!path.exists(), "an aged terminal receipt must be pruned");
        assert!(matches!(
            pruned
                .claim(ExternalWorkerOperation::Launch, "req-old", &hash)
                .unwrap(),
            ExternalWorkerLedgerClaim::Perform
        ));
    }

    /// Retention must not become a way for an unreconciled outcome to age into
    /// a fresh provider mutation.
    #[test]
    fn uncertain_receipts_are_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let request = launch("req-uncertain", "do the work");
        let hash = canonical_launch_payload_hash(&request).unwrap();
        ledger
            .claim(ExternalWorkerOperation::Launch, "req-uncertain", &hash)
            .unwrap();
        ledger
            .uncertain(ExternalWorkerOperation::Launch, "req-uncertain", &hash)
            .unwrap();
        let path = receipt_path(dir.path(), ExternalWorkerOperation::Launch, "req-uncertain");
        drop(ledger);
        backdate(
            &path,
            (TERMINAL_RECEIPT_RETENTION_SECS / 86_400) as i64 + 365,
        );

        let reopened = ExternalWorkerLedger::open(dir.path()).unwrap();
        assert!(path.is_file(), "an Uncertain receipt must never be pruned");
        assert!(matches!(
            reopened.claim(ExternalWorkerOperation::Launch, "req-uncertain", &hash),
            Err(ExternalWorkerAdapterError::Uncertain)
        ));
    }

    #[test]
    fn operations_are_namespaced_so_cancel_cannot_replay_a_launch() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ExternalWorkerLedger::open(dir.path()).unwrap();
        let launch = launch("shared-id", "do the work");
        let launch_hash = canonical_launch_payload_hash(&launch).unwrap();
        ledger
            .claim(ExternalWorkerOperation::Launch, "shared-id", &launch_hash)
            .unwrap();
        ledger
            .complete(
                ExternalWorkerOperation::Launch,
                "shared-id",
                &launch_hash,
                serde_json::json!({"kind": "launch"}),
            )
            .unwrap();
        let cancel_hash = canonical_cancel_payload_hash("agent-1", "run-1");
        match ledger
            .claim(ExternalWorkerOperation::Cancel, "shared-id", &cancel_hash)
            .unwrap()
        {
            ExternalWorkerLedgerClaim::Perform => {}
            other => panic!("cancel must not replay launch, got {other:?}"),
        }
    }
}
