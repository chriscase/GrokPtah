//! Durable idempotency ledger for external-worker mutations.
//!
//! Launch, follow-up, and cancel are keyed by `request_id` plus a canonical
//! payload hash. Identical retries replay the original result. Payload drift
//! is rejected. Pending and Uncertain outcomes are represented explicitly and
//! fail closed until an operator reconciles them. This ledger is namespaced
//! away from Computer Use and core-agent MCP receipts.

use super::{ExternalWorkerAdapterError, ProviderConflictCode};
use grokptah_agent_sdk::{ExternalWorkerFollowUpRequest, ExternalWorkerLaunchRequest};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
}

/// Filesystem-backed idempotency ledger for external workers.
#[derive(Clone)]
pub struct ExternalWorkerLedger {
    inner: Arc<LedgerInner>,
}

struct LedgerInner {
    root: PathBuf,
    lock: Mutex<()>,
}

impl ExternalWorkerLedger {
    /// Open (or create) a ledger under `root/external-workers/idempotency`.
    /// Orphaned pending claims become Uncertain and stay fail-closed.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ExternalWorkerAdapterError> {
        let root = root.as_ref().join("external-workers").join("idempotency");
        fs::create_dir_all(&root).map_err(|_| {
            ExternalWorkerAdapterError::InvalidRequest(
                "external worker ledger could not be created",
            )
        })?;
        let ledger = Self {
            inner: Arc::new(LedgerInner {
                root,
                lock: Mutex::new(()),
            }),
        };
        ledger.mark_orphaned_uncertain()?;
        Ok(ledger)
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

    fn mark_orphaned_uncertain(&self) -> Result<(), ExternalWorkerAdapterError> {
        let _g = self.inner.lock.lock();
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
                if receipt.status != ExternalWorkerLedgerStatus::Pending {
                    continue;
                }
                receipt.status = ExternalWorkerLedgerStatus::Uncertain;
                receipt.error = Some("interrupted before a durable result was recorded".into());
                atomic_write_json(&path, &receipt)?;
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
        let _g = self.inner.lock.lock();
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
            fs::create_dir_all(parent).map_err(|_| {
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
        let _g = self.inner.lock.lock();
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
        let receipt = LedgerReceipt {
            request_id: request_id.into(),
            operation,
            payload_hash: payload_hash.into(),
            status,
            response,
            error,
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

fn atomic_write_json<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), ExternalWorkerAdapterError> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be encoded")
    })?;
    let mut file = fs::File::create(&tmp).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be written")
    })?;
    file.write_all(&bytes).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be written")
    })?;
    file.sync_all().map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be written")
    })?;
    fs::rename(&tmp, path).map_err(|_| {
        ExternalWorkerAdapterError::InvalidRequest("external worker ledger could not be written")
    })?;
    Ok(())
}

fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    Ok(())
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
