//! Durable run records, idempotency receipts, audit log (#196).

use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use uuid::Uuid;

use super::types::{
    safe_id_filename, AgentRecord, AgentState, AuditEntry, ContinuationCheckpoint,
    IdempotencyReceipt, OrchError, OrchErrorCode, PromotionState, RunRecord, RunState,
};

#[derive(Clone)]
pub struct OrchStore {
    inner: Arc<OrchStoreInner>,
}

struct OrchStoreInner {
    root: PathBuf,
    _store_lock: fs::File,
    lock: Mutex<()>,
    last_run_error: Mutex<Option<String>>,
    last_audit_error: Arc<Mutex<Option<String>>>,
    audit_file_lock: Arc<Mutex<()>>,
    audit_writer: AuditWriter,
}

const MAX_AUDIT_BYTES: u64 = 4 * 1024 * 1024;

/// Conservative bounds for the durable orchestration ledger. Retention is
/// deliberately age- and count-bounded, but never trades away an active run,
/// a reviewable isolated run, or the source of a retry chain.
pub const DEFAULT_MAX_TERMINAL_RUNS: usize = 500;
pub const DEFAULT_MAX_IDEMPOTENCY_RECEIPTS: usize = 1_000;
pub const DEFAULT_TERMINAL_RUN_AGE: Duration = Duration::days(30);
pub const DEFAULT_IDEMPOTENCY_RECEIPT_AGE: Duration = Duration::days(7);
pub const MAX_FINALIZATION_RECOVERY_INTENTS: usize = 32;
pub const MAX_ACCEPTANCE_PROMPT_BYTES: usize = 1_000_000;

const ACTIVE_ATTEMPT_PHASES: &[&str] = &["claimed", "running", "finalizing"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcceptancePhase {
    Queued,
    Claimed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptanceIntent {
    pub admission_id: String,
    pub sequence: u64,
    pub run_id: String,
    pub request_id: String,
    pub payload_hash: String,
    pub tool: String,
    pub session_id: uuid::Uuid,
    pub workspace: String,
    pub execution_mode: super::types::RunExecutionMode,
    pub allow_queue: bool,
    #[serde(default)]
    pub attempt_id: Option<String>,
    /// Full input exists only in this private acceptance ledger. It is
    /// removed before a claimed model attempt is dispatched.
    pub prompt: Option<String>,
    pub prompt_hash: String,
    pub bounds: super::types::RunBounds,
    pub run: RunRecord,
    pub response: serde_json::Value,
    pub phase: AcceptancePhase,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
    pub integrity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Claimed,
    Running,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Reaped,
}

impl AttemptPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Running => "running",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Reaped => "reaped",
        }
    }

    pub(crate) fn is_active(self) -> bool {
        ACTIVE_ATTEMPT_PHASES.contains(&self.as_str())
    }
}

impl AcceptanceIntent {
    fn digest(&self) -> String {
        super::types::hash_payload(&serde_json::json!({
            "admissionId": self.admission_id,
            "sequence": self.sequence,
            "runId": self.run_id,
            "requestId": self.request_id,
            "payloadHash": self.payload_hash,
            "tool": self.tool,
            "sessionId": self.session_id,
            "workspace": self.workspace,
            "executionMode": self.execution_mode,
            "allowQueue": self.allow_queue,
            "attemptId": self.attempt_id,
            "prompt": self.prompt,
            "promptHash": self.prompt_hash,
            "bounds": self.bounds,
            "run": self.run,
            "response": self.response,
            "phase": self.phase,
            "createdAt": self.created_at,
        }))
    }

    fn seal(&mut self) {
        self.integrity = self.digest();
    }

    fn validate(&self) -> anyhow::Result<()> {
        safe_id_filename(&self.admission_id).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        safe_id_filename(&self.run_id).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        safe_id_filename(&self.request_id).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if self.sequence == 0
            || self.workspace.is_empty()
            || self.workspace.len() > 4 * 1024
            || self.workspace.chars().any(|character| character == '\0')
        {
            anyhow::bail!("acceptance workspace is outside its bound");
        }
        self.bounds
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        self.run
            .bounds
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if self.run.run_id != self.run_id
            || self.run.session_id != self.session_id
            || self.run.request_id != self.request_id
            || self.run.workspace != self.workspace
            || self.run.bounds.max_prompt_bytes != self.bounds.max_prompt_bytes
            || self.run.bounds.max_rounds != self.bounds.max_rounds
            || self.run.bounds.max_duration_ms != self.bounds.max_duration_ms
        {
            anyhow::bail!("acceptance identity does not match its run");
        }
        if self
            .prompt
            .as_deref()
            .is_some_and(|prompt| prompt.is_empty() || prompt.len() > self.bounds.max_prompt_bytes)
            || self
                .prompt
                .as_deref()
                .is_some_and(|prompt| prompt.len() > MAX_ACCEPTANCE_PROMPT_BYTES)
        {
            anyhow::bail!("acceptance prompt is outside its bound");
        }
        if let Some(prompt) = self.prompt.as_deref() {
            if self.prompt_hash != super::types::hash_payload(&serde_json::json!(prompt)) {
                anyhow::bail!("acceptance prompt hash is invalid");
            }
            let expected_payload = super::types::hash_payload(&serde_json::json!({
                "sessionId": self.session_id,
                "workspace": self.workspace,
                "prompt": prompt,
                "bounds": self.bounds,
                "executionMode": self.execution_mode,
                "allowQueue": self.allow_queue,
                "retryOf": self.run.retry_of,
            }));
            if self.payload_hash != expected_payload {
                anyhow::bail!("acceptance payload hash is invalid");
            }
        } else if self.prompt_hash.is_empty() {
            anyhow::bail!("consumed acceptance has no prompt hash");
        }
        match self.phase {
            AcceptancePhase::Queued => {
                if self.prompt.is_none()
                    || !matches!(self.run.state, RunState::Queued | RunState::Running)
                {
                    anyhow::bail!("queued acceptance has no recoverable input");
                }
            }
            AcceptancePhase::Claimed
            | AcceptancePhase::Cancelled
            | AcceptancePhase::Interrupted
                if self.prompt.is_some() =>
            {
                anyhow::bail!("consumed acceptance still contains prompt input");
            }
            _ => {}
        }
        if self.integrity.is_empty()
            || !super::authz::constant_time_eq(self.integrity.as_bytes(), self.digest().as_bytes())
        {
            anyhow::bail!("acceptance integrity check failed");
        }
        Ok(())
    }
}

/// Private lease record for one model attempt. The file name carries the
/// run_id so the public record never needs to grow a provider/task handle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub owner_instance_id: String,
    pub revision: u64,
    pub heartbeat_at: chrono::DateTime<Utc>,
    pub expires_at: chrono::DateTime<Utc>,
    pub phase: AttemptPhase,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DurabilityHealth {
    degraded: bool,
    detail: Option<String>,
    updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub max_terminal_runs: usize,
    pub max_idempotency_receipts: usize,
    pub terminal_run_age: Duration,
    pub idempotency_receipt_age: Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_terminal_runs: DEFAULT_MAX_TERMINAL_RUNS,
            max_idempotency_receipts: DEFAULT_MAX_IDEMPOTENCY_RECEIPTS,
            terminal_run_age: DEFAULT_TERMINAL_RUN_AGE,
            idempotency_receipt_age: DEFAULT_IDEMPOTENCY_RECEIPT_AGE,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub run_files_scanned: usize,
    pub run_files_removed: usize,
    pub idempotency_files_scanned: usize,
    pub idempotency_files_removed: usize,
    pub protected_runs: usize,
    pub skipped_files: usize,
}

struct AuditWriter {
    tx: Mutex<Option<SyncSender<AuditEntry>>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for AuditWriter {
    fn drop(&mut self) {
        self.tx.lock().take();
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
    }
}

impl OrchStore {
    /// Open store and convert unfinished runs to `interrupted` (crash recovery).
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("runs"))?;
        fs::create_dir_all(root.join("agents"))?;
        fs::create_dir_all(root.join("checkpoints"))?;
        fs::create_dir_all(root.join("idempotency"))?;
        fs::create_dir_all(root.join("audit"))?;
        fs::create_dir_all(root.join("finalization"))?;
        ensure_private_dir(&root.join("acceptance"))?;
        ensure_private_dir(&root.join("attempts"))?;
        fs::create_dir_all(root.join("health"))?;
        let root = dunce::canonicalize(root)?;
        let lock_path = root.join(".store.lock");
        let store_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        store_lock.try_lock_exclusive().map_err(|e| {
            anyhow::anyhow!(
                "orchestration store {} is already open ({e})",
                root.display()
            )
        })?;
        let last_audit_error = Arc::new(Mutex::new(None));
        let audit_file_lock = Arc::new(Mutex::new(()));
        let (audit_tx, audit_rx) = sync_channel::<AuditEntry>(256);
        let audit_root = root.clone();
        let writer_error = last_audit_error.clone();
        let writer_lock = audit_file_lock.clone();
        let audit_join = std::thread::Builder::new()
            .name("grokptah-orchestration-audit".into())
            .spawn(move || {
                while let Ok(entry) = audit_rx.recv() {
                    let _guard = writer_lock.lock();
                    let result = append_audit_entry(&audit_root, &entry);
                    if let Err(error) = result {
                        *writer_error.lock() = Some(error.to_string());
                    }
                }
            })?;
        let store = Self {
            inner: Arc::new(OrchStoreInner {
                root,
                _store_lock: store_lock,
                lock: Mutex::new(()),
                last_run_error: Mutex::new(None),
                last_audit_error,
                audit_file_lock,
                audit_writer: AuditWriter {
                    tx: Mutex::new(Some(audit_tx)),
                    join: Mutex::new(Some(audit_join)),
                },
            }),
        };
        // Acceptance is the admission authority. It is intentionally
        // reconstructed before receipts so a completed receipt can never
        // advertise a queued run whose private input was lost.
        let recovered_acceptances = match store.recover_acceptance_intents() {
            Ok(survivors) => survivors,
            Err(error) => {
                store.mark_health_degraded(&format!("acceptance recovery failed: {error}"));
                HashSet::new()
            }
        };
        if let Err(error) = store.retire_lost_queued_runs(&recovered_acceptances) {
            store.mark_health_degraded(&format!("lost admission retirement failed: {error}"));
        }
        if let Err(error) = store.recover_finalization_intents() {
            store.mark_health_degraded(&format!("finalization recovery failed: {error}"));
        }
        store.mark_unfinished_interrupted()?;
        if let Err(error) = store.recover_attempts() {
            store.mark_health_degraded(&format!("attempt recovery failed: {error}"));
        }
        store.fail_orphaned_idempotency_claims()?;
        // Cleanup is best-effort at the record level, but directory access
        // failures still surface so a broken ledger cannot look healthy.
        store.prune_retention(RetentionPolicy::default())?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn run_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self.inner.root.join("runs").join(format!("{safe}.json")))
    }

    fn idemp_path(&self, request_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(request_id)?;
        Ok(self
            .inner
            .root
            .join("idempotency")
            .join(format!("{safe}.json")))
    }

    fn finalization_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("finalization")
            .join(format!("{safe}.json")))
    }

    fn acceptance_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("acceptance")
            .join(format!("{safe}.json")))
    }

    fn attempt_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("attempts")
            .join(format!("{safe}.json")))
    }

    fn admission_sequence_path(&self) -> PathBuf {
        self.inner.root.join("acceptance").join("sequence.json")
    }

    fn health_path(&self) -> PathBuf {
        self.inner.root.join("health").join("orchestration.json")
    }

    fn agent_path(&self, agent_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(agent_id)?;
        Ok(self.inner.root.join("agents").join(format!("{safe}.json")))
    }

    fn checkpoint_path(&self, checkpoint_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(checkpoint_id)?;
        Ok(self
            .inner
            .root
            .join("checkpoints")
            .join(format!("{safe}.json")))
    }

    pub fn save_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        let result = self
            .run_path(&run.run_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .and_then(|path| atomic_write_json(&path, run));
        *self.inner.last_run_error.lock() = result.as_ref().err().map(ToString::to_string);
        result
    }

    pub fn load_run(&self, run_id: &str) -> anyhow::Result<Option<RunRecord>> {
        let _g = self.inner.lock.lock();
        self.load_run_unlocked(run_id)
    }

    fn load_run_unlocked(&self, run_id: &str) -> anyhow::Result<Option<RunRecord>> {
        let path = match self.run_path(run_id) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// Allocate a durable host-admission identity. Gaps are harmless after a
    /// crash; reusing an order number is not.
    pub(crate) fn allocate_admission_identity(&self) -> anyhow::Result<(String, u64)> {
        let _g = self.inner.lock.lock();
        let sequence_path = self.admission_sequence_path();
        let persisted = fs::read_to_string(&sequence_path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value["next"].as_u64())
            .unwrap_or(0);
        let mut highest = persisted;
        let dir = self.inner.root.join("acceptance");
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json")
                || path.file_name().and_then(|s| s.to_str()) == Some("sequence.json")
            {
                continue;
            }
            if let Ok(text) = fs::read_to_string(path) {
                if let Ok(intent) = serde_json::from_str::<AcceptanceIntent>(&text) {
                    highest = highest.max(intent.sequence);
                }
            }
        }
        let sequence = highest.saturating_add(1);
        private_atomic_write_json(&sequence_path, &serde_json::json!({ "next": sequence }))?;
        Ok((Uuid::new_v4().to_string(), sequence))
    }

    /// Persist the full acceptance intent before a public run or receipt can
    /// claim that admission succeeded.
    pub(crate) fn save_acceptance_intent(&self, intent: &AcceptanceIntent) -> anyhow::Result<()> {
        let mut sealed = intent.clone();
        sealed.seal();
        sealed.validate()?;
        let path = self
            .acceptance_path(&sealed.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let result = private_write_json_exclusive(&path, &sealed);
        if result
            .as_ref()
            .is_err_and(|error| error.kind() != std::io::ErrorKind::AlreadyExists)
        {
            let _ = fs::remove_file(&path);
        }
        result.map_err(anyhow::Error::from)
    }

    pub(crate) fn load_acceptance_intent(
        &self,
        run_id: &str,
    ) -> anyhow::Result<Option<AcceptanceIntent>> {
        let path = match self.acceptance_path(run_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let _g = self.inner.lock.lock();
        load_acceptance_intent_path(&path)
    }

    pub(crate) fn list_acceptance_intents(&self) -> anyhow::Result<Vec<AcceptanceIntent>> {
        let _g = self.inner.lock.lock();
        let mut intents = Vec::new();
        for entry in fs::read_dir(self.inner.root.join("acceptance"))? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json")
                || path.file_name().and_then(|s| s.to_str()) == Some("sequence.json")
            {
                continue;
            }
            if let Some(intent) = load_acceptance_intent_path(&path)? {
                intents.push(intent);
            }
        }
        intents.sort_by_key(|intent| intent.sequence);
        Ok(intents)
    }

    /// Update the private coupling after an admission becomes a claimed
    /// attempt. The prompt is removed from the serialized value before the
    /// intent is retained as a no-prompt receipt-recovery tombstone.
    pub(crate) fn claim_acceptance_input(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_instance_id: &str,
        revision: u64,
        run: &RunRecord,
        response: serde_json::Value,
    ) -> anyhow::Result<String> {
        let path = self
            .acceptance_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let attempt_path = self
            .attempt_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let attempt = self
            .read_attempt_path(&attempt_path)?
            .ok_or_else(|| anyhow::anyhow!("attempt is missing"))?;
        check_attempt_owner(
            &attempt,
            attempt_id,
            owner_instance_id,
            revision,
            Utc::now(),
        )?;
        if attempt.phase != AttemptPhase::Running {
            anyhow::bail!("attempt is not running");
        }
        let mut intent = load_acceptance_intent_path(&path)?
            .ok_or_else(|| anyhow::anyhow!("acceptance intent is missing"))?;
        if intent.run_id != run_id
            || intent.run.run_id != run_id
            || intent.phase != AcceptancePhase::Queued
        {
            anyhow::bail!("acceptance intent is no longer claimable");
        }
        let prompt = intent
            .prompt
            .take()
            .ok_or_else(|| anyhow::anyhow!("accepted prompt is missing"))?;
        intent.attempt_id = Some(attempt_id.to_string());
        intent.phase = AcceptancePhase::Claimed;
        intent.run = run.clone();
        intent.response = response;
        intent.updated_at = Utc::now();
        intent.seal();
        intent.validate()?;
        // The no-prompt intent is itself private and mode-checked. Keeping it
        // until receipt settlement closes the claim/receipt crash cut.
        private_atomic_write_json(&path, &intent)?;
        Ok(prompt)
    }

    pub(crate) fn settle_acceptance_intent(&self, run_id: &str) -> anyhow::Result<()> {
        self.tombstone_acceptance_intent(run_id)
    }

    pub(crate) fn update_acceptance_intent(
        &self,
        run_id: &str,
        run: RunRecord,
        response: serde_json::Value,
    ) -> anyhow::Result<()> {
        let path = self
            .acceptance_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let Some(mut intent) = load_acceptance_intent_path(&path)? else {
            return Ok(());
        };
        intent.run = run;
        intent.response = response;
        intent.updated_at = Utc::now();
        intent.seal();
        intent.validate()?;
        private_atomic_write_json(&path, &intent)
    }

    pub(crate) fn cancel_acceptance_input(&self, run_id: &str) -> anyhow::Result<()> {
        let path = self
            .acceptance_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let Some(mut intent) = load_acceptance_intent_path(&path)? else {
            return Ok(());
        };
        intent.prompt = None;
        intent.phase = AcceptancePhase::Cancelled;
        intent.updated_at = Utc::now();
        intent.seal();
        intent.validate()?;
        private_atomic_write_json(&path, &intent)?;
        fs::remove_file(&path)?;
        sync_parent_dir(&path)?;
        Ok(())
    }

    /// Fail a submission that has already created a private acceptance
    /// record. The prompt is removed before the record is unlinked; no
    /// recovery path may see this admission as executable work.
    pub(crate) fn fail_acceptance_intent(&self, run_id: &str) -> anyhow::Result<()> {
        let path = self
            .acceptance_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        if !path.is_file() {
            return Ok(());
        }
        match fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<AcceptanceIntent>(&text).ok())
        {
            Some(mut intent) => {
                intent.prompt = None;
                intent.phase = AcceptancePhase::Interrupted;
                intent.updated_at = Utc::now();
                intent.seal();
                let _ = private_atomic_write_json(&path, &intent);
            }
            None => {
                self.quarantine_acceptance_path(&path);
            }
        }
        if path.is_file() {
            fs::remove_file(&path)?;
            sync_parent_dir(&path)?;
        }
        Ok(())
    }

    pub fn load_attempt(&self, run_id: &str) -> anyhow::Result<Option<AttemptRecord>> {
        let path = match self.attempt_path(run_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        let _g = self.inner.lock.lock();
        self.read_attempt_path(&path)
    }

    /// CAS-create the one attempt allowed for a durable run. `None` is the
    /// compare value for an absent record; an existing record is never
    /// silently replaced, even when it is stale.
    pub(crate) fn claim_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_instance_id: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
        lease: StdDuration,
    ) -> anyhow::Result<AttemptRecord> {
        let path = self
            .attempt_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let current = self.read_attempt_path(&path)?;
        let next_revision = match (current, expected_revision) {
            (Some(current), Some(expected)) if current.revision != expected => {
                anyhow::bail!("stale attempt revision")
            }
            (Some(current), Some(_)) if !current.phase.is_active() => {
                current.revision.saturating_add(1)
            }
            (Some(_), _) => anyhow::bail!("attempt is already claimed"),
            (None, Some(_)) => anyhow::bail!("stale attempt revision"),
            (None, None) => 1,
        };
        let attempt = AttemptRecord {
            attempt_id: attempt_id.into(),
            owner_instance_id: owner_instance_id.into(),
            revision: next_revision,
            heartbeat_at: now,
            expires_at: now
                + chrono::Duration::from_std(lease)
                    .map_err(|error| anyhow::anyhow!("invalid attempt lease: {error}"))?,
            phase: AttemptPhase::Running,
        };
        private_atomic_write_json(&path, &attempt)?;
        Ok(attempt)
    }

    pub(crate) fn heartbeat_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_instance_id: &str,
        revision: u64,
        now: chrono::DateTime<Utc>,
        lease: StdDuration,
    ) -> anyhow::Result<AttemptRecord> {
        self.mutate_attempt(run_id, |current| {
            check_attempt_owner(current, attempt_id, owner_instance_id, revision, now)?;
            if current.phase != AttemptPhase::Running {
                anyhow::bail!("attempt is not running");
            }
            current.revision = current.revision.saturating_add(1);
            current.heartbeat_at = now;
            current.expires_at = now
                + chrono::Duration::from_std(lease)
                    .map_err(|error| anyhow::anyhow!("invalid attempt lease: {error}"))?;
            Ok(())
        })
    }

    pub(crate) fn finalize_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_instance_id: &str,
        revision: u64,
        phase: AttemptPhase,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<AttemptRecord> {
        if !matches!(
            phase,
            AttemptPhase::Completed
                | AttemptPhase::Failed
                | AttemptPhase::Cancelled
                | AttemptPhase::Interrupted
                | AttemptPhase::Reaped
        ) {
            anyhow::bail!("attempt finalization must be terminal");
        }
        self.mutate_attempt(run_id, |current| {
            check_attempt_owner(current, attempt_id, owner_instance_id, revision, now)?;
            if current.phase != AttemptPhase::Running && current.phase != AttemptPhase::Finalizing {
                anyhow::bail!("attempt is no longer finalizable");
            }
            current.revision = current.revision.saturating_add(1);
            current.phase = phase;
            current.heartbeat_at = now;
            Ok(())
        })
    }

    pub(crate) fn begin_attempt_finalization(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_instance_id: &str,
        revision: u64,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<AttemptRecord> {
        self.mutate_attempt(run_id, |current| {
            check_attempt_owner(current, attempt_id, owner_instance_id, revision, now)?;
            if current.phase != AttemptPhase::Running {
                anyhow::bail!("attempt is no longer running");
            }
            current.revision = current.revision.saturating_add(1);
            current.phase = AttemptPhase::Finalizing;
            current.heartbeat_at = now;
            Ok(())
        })
    }

    /// Reap only the exact expired revision. This is deliberately owner
    /// agnostic: the service that owns the in-memory task supplies the exact
    /// attempt id and abort handle, while stale/wrong revisions fail closed.
    pub(crate) fn reap_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        revision: u64,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<Option<AttemptRecord>> {
        let path = self
            .attempt_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let Some(mut current) = self.read_attempt_path(&path)? else {
            return Ok(None);
        };
        if current.attempt_id != attempt_id
            || current.revision != revision
            || current.phase != AttemptPhase::Running
            || now < current.expires_at
        {
            return Ok(None);
        }
        current.revision = current.revision.saturating_add(1);
        current.phase = AttemptPhase::Reaped;
        current.heartbeat_at = now;
        private_atomic_write_json(&path, &current)?;
        Ok(Some(current))
    }

    pub(crate) fn list_attempts(&self) -> anyhow::Result<Vec<(String, AttemptRecord)>> {
        let _g = self.inner.lock.lock();
        let mut out = Vec::new();
        let mut report = RetentionReport::default();
        for (_, run) in self.read_run_entries_unlocked(&mut report)? {
            let path = self
                .attempt_path(&run.run_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if let Some(attempt) = self.read_attempt_path(&path)? {
                out.push((run.run_id, attempt));
            }
        }
        Ok(out)
    }

    pub fn finalization_recovery_count(&self) -> usize {
        fs::read_dir(self.inner.root.join("finalization"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| {
                        entry.path().is_file()
                            && entry.path().extension().and_then(|s| s.to_str()) == Some("json")
                    })
                    .count()
            })
            .unwrap_or_default()
    }

    pub fn durability_health(&self) -> serde_json::Value {
        let degraded = fs::read_to_string(self.health_path())
            .ok()
            .and_then(|text| serde_json::from_str::<DurabilityHealth>(&text).ok())
            .unwrap_or(DurabilityHealth {
                degraded: false,
                detail: None,
                updated_at: Utc::now(),
            });
        serde_json::json!({
            "degraded": degraded.degraded || self.finalization_recovery_count() > 0,
            "detail": degraded.detail,
            "finalizationRecoveryPending": self.finalization_recovery_count(),
            "finalizationRecoveryLimit": MAX_FINALIZATION_RECOVERY_INTENTS,
        })
    }

    pub(crate) fn mark_health_degraded(&self, detail: &str) {
        let health = DurabilityHealth {
            degraded: true,
            detail: Some(detail.chars().take(500).collect()),
            updated_at: Utc::now(),
        };
        if let Err(error) = atomic_write_json(&self.health_path(), &health) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
        }
    }

    fn mutate_attempt<F>(&self, run_id: &str, update: F) -> anyhow::Result<AttemptRecord>
    where
        F: FnOnce(&mut AttemptRecord) -> anyhow::Result<()>,
    {
        let path = self
            .attempt_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let mut attempt = self
            .read_attempt_path(&path)?
            .ok_or_else(|| anyhow::anyhow!("attempt is missing"))?;
        update(&mut attempt)?;
        private_atomic_write_json(&path, &attempt)?;
        Ok(attempt)
    }

    fn read_attempt_path(&self, path: &Path) -> anyhow::Result<Option<AttemptRecord>> {
        if !path.is_file() {
            return Ok(None);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(path)?.permissions().mode() & 0o777 != 0o600 {
                anyhow::bail!("private attempt record has unsafe permissions");
            }
        }
        Ok(Some(serde_json::from_str(&fs::read_to_string(path)?)?))
    }

    /// Atomically read, mutate, and replace a run record.
    pub fn update_run<F>(&self, run_id: &str, update: F) -> anyhow::Result<Option<RunRecord>>
    where
        F: FnOnce(&mut RunRecord) -> anyhow::Result<()>,
    {
        let _g = self.inner.lock.lock();
        let Some(mut run) = self.load_run_unlocked(run_id)? else {
            return Ok(None);
        };
        update(&mut run)?;
        let path = self
            .run_path(run_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if let Err(error) = atomic_write_json(&path, &run) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
            return Err(error);
        }
        *self.inner.last_run_error.lock() = None;
        Ok(Some(run))
    }

    pub fn list_runs(&self) -> anyhow::Result<Vec<RunRecord>> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("runs");
        if !dir.is_dir() {
            return Ok(out);
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(e.path()) {
                if let Ok(r) = serde_json::from_str::<RunRecord>(&text) {
                    out.push(r);
                }
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Persist one transport-neutral durable agent identity.
    pub fn save_agent(&self, agent: &AgentRecord) -> anyhow::Result<()> {
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let path = self
            .agent_path(&agent.agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&path, agent)
    }

    pub fn load_agent(&self, agent_id: &str) -> anyhow::Result<Option<AgentRecord>> {
        let _g = self.inner.lock.lock();
        let path = match self.agent_path(agent_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let agent: AgentRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(agent))
    }

    pub fn list_agents(&self) -> anyhow::Result<Vec<AgentRecord>> {
        let _g = self.inner.lock.lock();
        let mut out = Vec::new();
        let dir = self.inner.root.join("agents");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let agent: AgentRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
            agent
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            out.push(agent);
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn update_agent<F>(&self, agent_id: &str, update: F) -> anyhow::Result<Option<AgentRecord>>
    where
        F: FnOnce(&mut AgentRecord) -> anyhow::Result<()>,
    {
        let _g = self.inner.lock.lock();
        let path = self
            .agent_path(agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !path.is_file() {
            return Ok(None);
        }
        let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&path)?)?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        update(&mut agent)?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent.updated_at = Utc::now();
        atomic_write_json(&path, &agent)?;
        Ok(Some(agent))
    }

    /// Persist a verified continuation point. Checkpoint records are append-
    /// only at the logical level; replacing an ID is allowed only for the
    /// atomic write/recovery path and must still pass hash validation.
    pub fn save_checkpoint(&self, checkpoint: &ContinuationCheckpoint) -> anyhow::Result<()> {
        checkpoint
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let path = self
            .checkpoint_path(&checkpoint.checkpoint_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&path, checkpoint)
    }

    pub fn load_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> anyhow::Result<Option<ContinuationCheckpoint>> {
        let _g = self.inner.lock.lock();
        let path = match self.checkpoint_path(checkpoint_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let checkpoint: ContinuationCheckpoint = serde_json::from_str(&fs::read_to_string(path)?)?;
        checkpoint
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(checkpoint))
    }

    pub fn list_checkpoints(
        &self,
        agent_id: Option<&str>,
    ) -> anyhow::Result<Vec<ContinuationCheckpoint>> {
        let _g = self.inner.lock.lock();
        let mut out = Vec::new();
        let dir = self.inner.root.join("checkpoints");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let checkpoint: ContinuationCheckpoint =
                serde_json::from_str(&fs::read_to_string(path)?)?;
            checkpoint
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if agent_id.is_none_or(|id| checkpoint.agent_id == id) {
                out.push(checkpoint);
            }
        }
        out.sort_by(|a, b| {
            b.ordinal
                .cmp(&a.ordinal)
                .then(b.created_at.cmp(&a.created_at))
        });
        Ok(out)
    }

    /// Apply conservative retention to the durable run and idempotency
    /// ledgers. This never recursively deletes anything and is safe to call
    /// while the store is live because it shares the store mutex.
    pub fn prune_retention(&self, policy: RetentionPolicy) -> anyhow::Result<RetentionReport> {
        anyhow::ensure!(
            policy.max_terminal_runs > 0,
            "retention must preserve at least one terminal run"
        );
        anyhow::ensure!(
            policy.max_idempotency_receipts > 0,
            "retention must preserve at least one idempotency receipt"
        );
        anyhow::ensure!(
            policy.terminal_run_age > Duration::zero()
                && policy.idempotency_receipt_age > Duration::zero(),
            "retention ages must be greater than zero"
        );

        let _guard = self.inner.lock.lock();
        let now = Utc::now();
        let mut report = RetentionReport::default();
        let runs = self.read_run_entries_unlocked(&mut report)?;
        let retry_sources: std::collections::HashSet<&str> = runs
            .iter()
            .filter_map(|(_, run)| run.retry_of.as_deref())
            .collect();

        let mut eligible_runs: Vec<(&Path, &RunRecord)> = Vec::new();
        for (path, run) in &runs {
            if !run.state.is_terminal() {
                report.protected_runs += 1;
                continue;
            }
            if retry_sources.contains(run.run_id.as_str()) || !safe_to_expire_run(run) {
                report.protected_runs += 1;
                continue;
            }
            eligible_runs.push((path.as_path(), run));
        }
        eligible_runs.sort_by(|(_, a), (_, b)| b.updated_at.cmp(&a.updated_at));
        for (index, (path, run)) in eligible_runs.iter().enumerate() {
            let over_count = index >= policy.max_terminal_runs;
            let over_age = now.signed_duration_since(run.updated_at) >= policy.terminal_run_age;
            if !over_count && !over_age {
                continue;
            }
            match fs::remove_file(path) {
                Ok(()) => report.run_files_removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => report.skipped_files += 1,
            }
        }

        let mut receipts = Vec::new();
        let dir = self.inner.root.join("idempotency");
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                report.skipped_files += 1;
                continue;
            };
            let Ok(receipt) = serde_json::from_str::<IdempotencyReceipt>(&text) else {
                report.skipped_files += 1;
                continue;
            };
            report.idempotency_files_scanned += 1;
            if !matches!(receipt.status.as_str(), "complete" | "failed") {
                report.skipped_files += 1;
                continue;
            }
            let linked_active = match receipt.run_id.as_deref() {
                Some(run_id) => match self.load_run_unlocked(run_id) {
                    Ok(Some(run)) => !run.state.is_terminal(),
                    Ok(None) => false,
                    Err(_) => {
                        // A receipt that cannot be reconciled with its run is
                        // retained rather than guessed to be safe to remove.
                        report.skipped_files += 1;
                        true
                    }
                },
                None => false,
            };
            if linked_active {
                continue;
            }
            receipts.push((path, receipt));
        }
        receipts.sort_by(|(_, a), (_, b)| b.created_at.cmp(&a.created_at));
        for (index, (path, receipt)) in receipts.iter().enumerate() {
            let over_count = index >= policy.max_idempotency_receipts;
            let over_age =
                now.signed_duration_since(receipt.created_at) >= policy.idempotency_receipt_age;
            if !over_count && !over_age {
                continue;
            }
            match fs::remove_file(path) {
                Ok(()) => report.idempotency_files_removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => report.skipped_files += 1,
            }
        }
        Ok(report)
    }

    fn read_run_entries_unlocked(
        &self,
        report: &mut RetentionReport,
    ) -> anyhow::Result<Vec<(PathBuf, RunRecord)>> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("runs");
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            report.run_files_scanned += 1;
            let Ok(text) = fs::read_to_string(&path) else {
                report.skipped_files += 1;
                continue;
            };
            let Ok(run) = serde_json::from_str::<RunRecord>(&text) else {
                report.skipped_files += 1;
                continue;
            };
            out.push((path, run));
        }
        Ok(out)
    }

    /// Install a terminal run through a durable recovery intent.
    pub fn persist_finalization(&self, candidate: &RunRecord) -> anyhow::Result<RunRecord> {
        if !candidate.state.is_terminal() {
            anyhow::bail!("finalization candidate must be terminal");
        }
        let _guard = self.inner.lock.lock();
        let mut final_run = candidate.clone();
        let run_path = self
            .run_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut corrupt_target = None;
        if run_path.is_file() {
            match fs::read_to_string(&run_path)
                .and_then(|text| serde_json::from_str::<RunRecord>(&text).map_err(Into::into))
            {
                Ok(current) => {
                    merge_run_observations(&mut final_run, &current);
                    if current.state.is_terminal() {
                        final_run.state = current.state;
                        final_run.terminal_result = current.terminal_result;
                        final_run.final_response = current.final_response;
                        final_run.error_code = current.error_code;
                    }
                }
                Err(_) => {
                    corrupt_target =
                        Some(run_path.with_extension(format!(
                            "json.corrupt-{}",
                            Utc::now().timestamp_millis()
                        )));
                }
            }
        }
        let intent_path = self
            .finalization_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !intent_path.is_file()
            && self.finalization_recovery_count() >= MAX_FINALIZATION_RECOVERY_INTENTS
        {
            anyhow::bail!("finalization recovery queue is full");
        }
        let result = (|| -> anyhow::Result<()> {
            atomic_write_json(&intent_path, &final_run)?;
            if let Some(corrupt) = &corrupt_target {
                fs::rename(&run_path, corrupt)?;
            }
            atomic_write_json(&run_path, &final_run)?;
            fs::remove_file(&intent_path)?;
            Ok(())
        })();
        *self.inner.last_run_error.lock() = result.as_ref().err().map(ToString::to_string);
        result.map(|_| final_run)
    }

    /// Keep a terminal candidate in the bounded recovery queue when the
    /// synchronous install deadline has elapsed. The candidate is already
    /// evidence-backed; this method never promotes a non-terminal run.
    pub(crate) fn ensure_finalization_intent(&self, candidate: &RunRecord) -> anyhow::Result<()> {
        if !candidate.state.is_terminal() {
            anyhow::bail!("finalization candidate must be terminal");
        }
        let path = self
            .finalization_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        if path.is_file() {
            return Ok(());
        }
        if self.finalization_recovery_count() >= MAX_FINALIZATION_RECOVERY_INTENTS {
            anyhow::bail!("finalization recovery queue is full");
        }
        atomic_write_json(&path, candidate)
    }

    pub fn save_idempotency(&self, receipt: &IdempotencyReceipt) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        let path = self
            .idemp_path(&receipt.request_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        atomic_write_json(&path, receipt)
    }

    pub fn load_idempotency(&self, request_id: &str) -> anyhow::Result<Option<IdempotencyReceipt>> {
        let path = match self.idemp_path(request_id) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// Atomically claim a request_id for mutation.
    /// - Existing complete receipt with same hash → `Replay(response)`
    /// - Existing complete receipt with different hash → Conflict
    /// - Existing pending → `Pending` (callers wait asynchronously)
    /// - None → create pending claim (exclusive)
    pub fn claim_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
    ) -> Result<IdempotencyClaim, OrchError> {
        let path = self.idemp_path(request_id)?;
        let _g = self.inner.lock.lock();
        if path.is_file() {
            let text = fs::read_to_string(&path)
                .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
            let prev: IdempotencyReceipt = serde_json::from_str(&text)
                .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
            if prev.request_id != request_id
                || prev.tool != tool
                || prev.payload_hash != payload_hash
            {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "request_id reused with different payload",
                ));
            }
            return match prev.status.as_str() {
                "complete" => Ok(IdempotencyClaim::Replay(Ok(prev.response))),
                "failed" => Ok(IdempotencyClaim::Replay(Err(prev.error.unwrap_or_else(
                    || OrchError::new(OrchErrorCode::Internal, "idempotent mutation failed"),
                )))),
                _ => Ok(IdempotencyClaim::Pending),
            };
        }

        let pending = IdempotencyReceipt {
            request_id: request_id.into(),
            payload_hash: payload_hash.into(),
            run_id: None,
            tool: tool.into(),
            response: serde_json::Value::Null,
            error: None,
            created_at: Utc::now(),
            status: "pending".into(),
        };
        match write_json_exclusive(&path, &pending) {
            Ok(()) => Ok(IdempotencyClaim::Perform),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(IdempotencyClaim::Pending)
            }
            Err(e) => Err(OrchError::new(OrchErrorCode::Internal, e.to_string())),
        }
    }

    pub fn complete_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        run_id: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        self.finish_idempotency(
            tool,
            request_id,
            payload_hash,
            run_id,
            response,
            None,
            "complete",
        )
    }

    pub fn fail_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        run_id: Option<String>,
        error: OrchError,
    ) -> Result<(), OrchError> {
        self.finish_idempotency(
            tool,
            request_id,
            payload_hash,
            run_id,
            serde_json::Value::Null,
            Some(error),
            "failed",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        run_id: Option<String>,
        response: serde_json::Value,
        error: Option<OrchError>,
        status: &str,
    ) -> Result<(), OrchError> {
        let path = self.idemp_path(request_id)?;
        let _g = self.inner.lock.lock();
        if !path.is_file() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "idempotency claim is missing",
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        let previous: IdempotencyReceipt = serde_json::from_str(&text)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        if previous.request_id != request_id
            || previous.tool != tool
            || previous.payload_hash != payload_hash
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "request_id reused with different payload",
            ));
        }
        if previous.status != "pending" {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("idempotency claim is already {}", previous.status),
            ));
        }
        let receipt = IdempotencyReceipt {
            request_id: request_id.into(),
            payload_hash: payload_hash.into(),
            run_id,
            tool: tool.into(),
            response,
            error,
            created_at: Utc::now(),
            status: status.into(),
        };
        atomic_write_json(&path, &receipt)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn append_audit(&self, entry: &AuditEntry) -> anyhow::Result<()> {
        let _guard = self.inner.audit_file_lock.lock();
        let result = append_audit_entry(&self.inner.root, entry);
        if let Err(error) = &result {
            *self.inner.last_audit_error.lock() = Some(error.to_string());
        }
        result
    }

    pub fn enqueue_audit(&self, entry: AuditEntry) -> anyhow::Result<()> {
        let sender = self.inner.audit_writer.tx.lock();
        let Some(sender) = sender.as_ref() else {
            let error = "audit writer is stopped".to_string();
            *self.inner.last_audit_error.lock() = Some(error.clone());
            return Err(anyhow::anyhow!(error));
        };
        sender.try_send(entry).map_err(|error| {
            let detail = match error {
                TrySendError::Full(_) => "audit writer queue is full",
                TrySendError::Disconnected(_) => "audit writer stopped",
            };
            *self.inner.last_audit_error.lock() = Some(detail.into());
            anyhow::anyhow!(detail)
        })
    }

    pub fn last_audit_error(&self) -> Option<String> {
        self.inner.last_audit_error.lock().clone()
    }

    pub fn last_run_error(&self) -> Option<String> {
        self.inner.last_run_error.lock().clone()
    }

    pub fn mark_unfinished_interrupted(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        let mut interrupted_agents = Vec::new();
        for mut run in self.list_runs()? {
            let accepted_queued = run.state == RunState::Queued
                && self
                    .load_acceptance_intent(&run.run_id)?
                    .is_some_and(|intent| {
                        intent.phase == AcceptancePhase::Queued && intent.prompt.is_some()
                    });
            if run.state == RunState::Running || (run.state == RunState::Queued && !accepted_queued)
            {
                run.state = RunState::Interrupted;
                run.queue_position = None;
                run.updated_at = Utc::now();
                run.terminal_result = Some("interrupted".into());
                run.error_code = Some("interrupted".into());
                if let Some(execution) = run.execution.as_mut() {
                    execution.promotion_state = PromotionState::Conflicted;
                }
                self.save_run(&run)?;
                if let Some(agent_id) = run.agent_id.clone() {
                    interrupted_agents.push((agent_id, run.run_id.clone()));
                }
                n += 1;
            }
        }
        for (agent_id, run_id) in interrupted_agents {
            let _ = self.update_agent(&agent_id, |agent| {
                if agent.current_run_id.as_deref() == Some(run_id.as_str()) {
                    agent.current_run_id = None;
                    agent.state = AgentState::Interrupted;
                }
                Ok(())
            })?;
        }
        // A crash can occur after a terminal run is durably installed but
        // before its checkpoint is attached to the agent. Never leave that
        // agent permanently active: terminal ownership is no longer live.
        for agent in self.list_agents()? {
            let Some(run_id) = agent.current_run_id.clone() else {
                continue;
            };
            let next_state = match self.load_run(&run_id)? {
                Some(run) if run.state.is_terminal() => AgentState::Waiting,
                Some(_) | None => AgentState::Interrupted,
            };
            let _ = self.update_agent(&agent.agent_id, |current| {
                if current.current_run_id.as_deref() == Some(run_id.as_str()) {
                    current.current_run_id = None;
                    current.state = next_state;
                }
                Ok(())
            })?;
        }
        Ok(n)
    }

    fn recover_acceptance_intents(&self) -> anyhow::Result<HashSet<String>> {
        let dir = self.inner.root.join("acceptance");
        let mut survivors = HashSet::new();
        let mut highest = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || path.file_name().and_then(|value| value.to_str()) == Some("sequence.json")
            {
                continue;
            }
            let parsed = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<AcceptanceIntent>(&text).ok());
            let valid = parsed.as_ref().is_some_and(|intent| {
                intent.validate().is_ok()
                    && fs::metadata(&path).is_ok_and(|metadata| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            metadata.permissions().mode() & 0o777 == 0o600
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = metadata;
                            true
                        }
                    })
            });
            let Some(intent) = parsed.filter(|_| valid) else {
                self.inner
                    .last_run_error
                    .lock()
                    .replace("acceptance intent integrity or mode check failed".into());
                self.quarantine_acceptance_path(&path);
                continue;
            };
            highest = highest.max(intent.sequence);
            if intent.phase != AcceptancePhase::Queued || intent.prompt.is_none() {
                let _ = fs::remove_file(&path);
                continue;
            }
            let run = self.load_run(&intent.run_id)?;
            if run.as_ref().is_some_and(|run| {
                run.state == RunState::Queued
                    && run.run_id == intent.run_id
                    && run.session_id == intent.session_id
                    && run.request_id == intent.request_id
                    && run.workspace == intent.workspace
                    && self.receipt_acknowledged(&intent)
            }) {
                survivors.insert(intent.run_id);
            } else {
                // A missing/pending/failed receipt means the caller was not
                // told this admission succeeded. Never execute it later.
                let _ = fs::remove_file(&path);
            }
        }
        let next = highest.saturating_add(1);
        let _ = private_atomic_write_json(
            &self.admission_sequence_path(),
            &serde_json::json!({ "next": next }),
        );
        Ok(survivors)
    }

    fn quarantine_acceptance_path(&self, path: &Path) {
        let quarantine = path.with_extension(format!("json.corrupt-{}", Uuid::new_v4()));
        if fs::rename(path, quarantine).is_err() {
            let _ = fs::remove_file(path);
        }
    }

    fn retire_lost_queued_runs(&self, survivors: &HashSet<String>) -> anyhow::Result<usize> {
        let mut retired = 0;
        for mut run in self.list_runs()? {
            if run.state != RunState::Queued || survivors.contains(&run.run_id) {
                continue;
            }
            run.state = RunState::Interrupted;
            run.queue_position = None;
            run.terminal_result = Some("interrupted".into());
            run.error_code = Some("admission_lost".into());
            run.updated_at = Utc::now();
            if let Some(execution) = run.execution.as_mut() {
                execution.promotion_state = PromotionState::Conflicted;
            }
            self.save_run(&run)?;
            retired += 1;
        }
        Ok(retired)
    }

    fn receipt_acknowledged(&self, intent: &AcceptanceIntent) -> bool {
        matches!(
            self.load_idempotency(&intent.request_id),
            Ok(Some(receipt)) if receipt.status == "complete"
                && receipt.request_id == intent.request_id
                && receipt.payload_hash == intent.payload_hash
                && receipt.tool == intent.tool
                && receipt.run_id.as_deref() == Some(intent.run_id.as_str())
                && receipt.response == intent.response
        )
    }

    fn tombstone_acceptance_intent(&self, run_id: &str) -> anyhow::Result<()> {
        let path = self
            .acceptance_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        if !path.is_file() {
            return Ok(());
        }
        // Replace the full DTO with a valid no-prompt intent first. This
        // avoids retaining prompt bytes in a failed delete path and makes the
        // delete itself idempotent across restart.
        let mut intent: AcceptanceIntent = serde_json::from_str(&fs::read_to_string(&path)?)?;
        intent.prompt = None;
        intent.phase = AcceptancePhase::Interrupted;
        intent.updated_at = Utc::now();
        intent.seal();
        intent.validate()?;
        private_atomic_write_json(&path, &intent)?;
        fs::remove_file(&path)?;
        sync_parent_dir(&path)?;
        Ok(())
    }

    fn recover_attempts(&self) -> anyhow::Result<usize> {
        let mut changed = 0;
        for (run_id, mut attempt) in self.list_attempts()? {
            if !attempt.phase.is_active() {
                continue;
            }
            let run = self.load_run(&run_id)?;
            attempt.revision = attempt.revision.saturating_add(1);
            attempt.phase = match run {
                Some(run) if run.state == RunState::Cancelled => AttemptPhase::Cancelled,
                Some(run) if run.state == RunState::Completed => AttemptPhase::Completed,
                Some(run) if run.state == RunState::Failed => AttemptPhase::Failed,
                _ => AttemptPhase::Interrupted,
            };
            attempt.heartbeat_at = Utc::now();
            let path = self
                .attempt_path(&run_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let _g = self.inner.lock.lock();
            private_atomic_write_json(&path, &attempt)?;
            changed += 1;
        }
        Ok(changed)
    }

    fn recover_finalization_intents(&self) -> anyhow::Result<usize> {
        let dir = self.inner.root.join("finalization");
        let mut recovered = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let candidate: RunRecord = serde_json::from_str(&text)?;
            self.persist_finalization(&candidate)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    fn fail_orphaned_idempotency_claims(&self) -> anyhow::Result<usize> {
        let dir = self.inner.root.join("idempotency");
        let mut changed = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut receipt) = serde_json::from_str::<IdempotencyReceipt>(&text) else {
                continue;
            };
            if receipt.status != "pending" {
                continue;
            }
            receipt.status = "failed".into();
            receipt.error = Some(OrchError::new(
                OrchErrorCode::Internal,
                "mutation was interrupted before its durable receipt completed; use a new request_id",
            ));
            receipt.created_at = Utc::now();
            atomic_write_json(&path, &receipt)?;
            changed += 1;
        }
        Ok(changed)
    }
}

/// A terminal run may be removed only when it no longer owns a live isolated
/// worktree. Reviewable and promotable records therefore remain durable until
/// their managed resource is explicitly discarded or otherwise disappears.
fn safe_to_expire_run(run: &RunRecord) -> bool {
    run.execution
        .as_ref()
        .map(|execution| !Path::new(&execution.execution_workspace).exists())
        .unwrap_or(true)
}

fn check_attempt_owner(
    current: &AttemptRecord,
    attempt_id: &str,
    owner_instance_id: &str,
    revision: u64,
    now: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    if current.attempt_id != attempt_id {
        anyhow::bail!("attempt identity mismatch");
    }
    if current.owner_instance_id != owner_instance_id {
        anyhow::bail!("attempt owner mismatch");
    }
    if current.revision != revision {
        anyhow::bail!("stale attempt revision");
    }
    if now >= current.expires_at {
        anyhow::bail!("attempt heartbeat expired");
    }
    Ok(())
}

fn load_acceptance_intent_path(path: &Path) -> anyhow::Result<Option<AcceptanceIntent>> {
    if !path.is_file() {
        return Ok(None);
    }
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            anyhow::bail!("private acceptance intent has unsafe permissions");
        }
    }
    let intent: AcceptanceIntent = serde_json::from_str(&fs::read_to_string(path)?)?;
    intent.validate()?;
    Ok(Some(intent))
}

fn ensure_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o700 {
            anyhow::bail!(
                "private directory {} has unsafe permissions",
                path.display()
            );
        }
    }
    Ok(())
}

fn sync_parent_dir(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Dedicated writer for the acceptance and attempt security boundary.
///
/// `File::create` inherits the process umask and can leave a readable temp
/// file. This writer creates a unique 0600 file, validates the mode before
/// writing, reapplies it after rename, and fsyncs the containing directory.
fn private_write_json_exclusive<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent).map_err(std::io::Error::other)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
                return Err(std::io::Error::other(
                    "private acceptance file has unsafe permissions",
                ));
            }
        }
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        sync_parent_dir(path).map_err(std::io::Error::other)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn private_atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> anyhow::Result<()> {
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            let mode = file.metadata()?.permissions().mode() & 0o777;
            if mode != 0o600 {
                anyhow::bail!("private temp file has unsafe permissions");
            }
        }
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            let mode = fs::metadata(path)?.permissions().mode() & 0o777;
            if mode != 0o600 {
                anyhow::bail!("private file has unsafe permissions");
            }
        }
        sync_parent_dir(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn merge_run_observations(target: &mut RunRecord, current: &RunRecord) {
    for change in &current.aggregates.changes {
        if !target
            .aggregates
            .changes
            .iter()
            .any(|existing| existing.path == change.path)
        {
            target.aggregates.changes.push(change.clone());
        }
    }
    for test in &current.aggregates.tests {
        if !target
            .aggregates
            .tests
            .iter()
            .any(|existing| existing.call_id == test.call_id)
        {
            target.aggregates.tests.push(test.clone());
        }
    }
    if current
        .progress
        .as_ref()
        .zip(target.progress.as_ref())
        .is_some_and(|(current, target)| current.updated_at > target.updated_at)
        || target.progress.is_none()
    {
        target.progress = current.progress.clone();
    }
}

fn append_audit_entry(root: &Path, entry: &AuditEntry) -> anyhow::Result<()> {
    use std::io::Write;

    let path = root.join("audit").join("audit.jsonl");
    if fs::metadata(&path)
        .map(|metadata| metadata.len() >= MAX_AUDIT_BYTES)
        .unwrap_or(false)
    {
        let rotated = path.with_extension("jsonl.1");
        if rotated.exists() {
            fs::remove_file(&rotated)?;
        }
        fs::rename(&path, rotated)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    file.sync_data()?;
    Ok(())
}

pub enum IdempotencyClaim {
    Perform,
    Pending,
    Replay(Result<serde_json::Value, OrchError>),
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    use std::io::Write;
    let result = (|| -> anyhow::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_json_exclusive<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        AgentRecord, ContinuationCheckpoint, ContinuationReason, RunBounds,
    };
    use tempfile::tempdir;
    use uuid::Uuid;

    fn terminal_run(run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: format!("req-{run_id}"),
            client_id: None,
            state: RunState::Completed,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: Some(2),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: Some("completed".into()),
            final_response: Some("done".into()),
            error_code: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    fn checkpoint(agent_id: &str, session_id: Uuid, run_id: &str) -> ContinuationCheckpoint {
        let mut checkpoint = ContinuationCheckpoint {
            checkpoint_id: format!("checkpoint-{run_id}"),
            agent_id: agent_id.into(),
            session_id,
            run_id: run_id.into(),
            parent_checkpoint_id: None,
            ordinal: 1,
            workspace: "/tmp/w".into(),
            context_summary: "verified context".into(),
            context_hash: String::new(),
            event_seq: 2,
            reason: ContinuationReason::TurnCompleted,
            created_at: Utc::now(),
        };
        checkpoint.context_hash = checkpoint.context_hash_for();
        checkpoint
    }

    #[test]
    fn restart_marks_running_interrupted() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let run = RunRecord {
            run_id: "r1".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: "req1".into(),
            client_id: None,
            state: RunState::Running,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store.save_run(&run).unwrap();
        drop(store);
        let store2 = OrchStore::open(d.path()).unwrap();
        let loaded = store2.load_run("r1").unwrap().unwrap();
        assert_eq!(loaded.state, RunState::Interrupted);
    }

    #[test]
    fn restart_marks_bound_agent_interrupted_but_preserves_checkpoint() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let session_id = Uuid::new_v4();
        let agent_id = "agent-restart";
        let cp = checkpoint(agent_id, session_id, "r-agent");
        store.save_checkpoint(&cp).unwrap();
        store
            .save_agent(&AgentRecord {
                agent_id: agent_id.into(),
                session_id,
                workspace: "/tmp/w".into(),
                model: "grok".into(),
                state: AgentState::Active,
                current_run_id: Some("r-agent".into()),
                latest_checkpoint_id: Some(cp.checkpoint_id.clone()),
                continuation_ordinal: 1,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        let mut run = terminal_run("r-agent");
        run.session_id = session_id;
        run.state = RunState::Running;
        run.terminal_result = None;
        run.final_response = None;
        run.agent_id = Some(agent_id.into());
        run.end_seq = None;
        store.save_run(&run).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let agent = reopened.load_agent(agent_id).unwrap().unwrap();
        assert_eq!(agent.state, AgentState::Interrupted);
        assert_eq!(agent.current_run_id, None);
        assert!(reopened
            .load_checkpoint(&cp.checkpoint_id)
            .unwrap()
            .is_some());
    }

    #[test]
    fn restart_clears_agent_after_terminal_run_before_checkpoint_attach() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let session_id = Uuid::new_v4();
        let agent_id = "agent-terminal-gap";
        store
            .save_agent(&AgentRecord {
                agent_id: agent_id.into(),
                session_id,
                workspace: "/tmp/w".into(),
                model: "grok".into(),
                state: AgentState::Active,
                current_run_id: Some("terminal-gap".into()),
                latest_checkpoint_id: None,
                continuation_ordinal: 0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .unwrap();
        let mut run = terminal_run("terminal-gap");
        run.session_id = session_id;
        run.agent_id = Some(agent_id.into());
        store.save_run(&run).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let agent = reopened.load_agent(agent_id).unwrap().unwrap();
        assert_eq!(agent.state, AgentState::Waiting);
        assert_eq!(agent.current_run_id, None);
    }

    #[test]
    fn tampered_checkpoint_fails_closed() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let cp = checkpoint("agent-tamper", Uuid::new_v4(), "run-tamper");
        store.save_checkpoint(&cp).unwrap();
        let path = store.checkpoint_path(&cp.checkpoint_id).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path.clone()).unwrap()).unwrap();
        value["contextSummary"] = serde_json::Value::String("tampered".into());
        fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        assert!(store.load_checkpoint(&cp.checkpoint_id).is_err());
    }

    #[test]
    fn clone_does_not_interrupt_running() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let run = RunRecord {
            run_id: "r2".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: "req2".into(),
            client_id: None,
            state: RunState::Running,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store.save_run(&run).unwrap();
        let clone = store.clone();
        let loaded = clone.load_run("r2").unwrap().unwrap();
        assert_eq!(loaded.state, RunState::Running);
        assert!(
            OrchStore::open(d.path()).is_err(),
            "a second live opener must not run crash recovery"
        );
    }

    #[test]
    fn idempotency_claim_exclusive() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        match store.claim_idempotency("t", "req", "h").unwrap() {
            IdempotencyClaim::Perform => {}
            _ => panic!("first claim should perform"),
        }
        store
            .complete_idempotency("t", "req", "h", None, serde_json::json!({"ok": true}))
            .unwrap();
        match store.claim_idempotency("t", "req", "h").unwrap() {
            IdempotencyClaim::Replay(Ok(v)) => assert_eq!(v["ok"], true),
            _ => panic!("replay"),
        }
        assert!(store.claim_idempotency("t", "req", "other").is_err());
    }

    #[test]
    fn terminal_receipt_cannot_be_overwritten() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        assert!(matches!(
            store.claim_idempotency("t", "failed", "h").unwrap(),
            IdempotencyClaim::Perform
        ));
        let error = OrchError::new(OrchErrorCode::Internal, "failed once");
        store
            .fail_idempotency("t", "failed", "h", None, error.clone())
            .unwrap();
        assert!(store
            .complete_idempotency("t", "failed", "h", None, serde_json::json!({"ok": true}))
            .is_err());
        match store.claim_idempotency("t", "failed", "h").unwrap() {
            IdempotencyClaim::Replay(Err(replayed)) => {
                assert_eq!(replayed.code.as_str(), error.code.as_str());
                assert_eq!(replayed.message, error.message);
            }
            _ => panic!("failed outcome must remain monotonic"),
        }
    }

    #[test]
    fn retention_prunes_only_expirable_records() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();

        let mut old = terminal_run("old-shared");
        old.updated_at = Utc::now() - Duration::days(10);
        store.save_run(&old).unwrap();

        let mut retry_source = terminal_run("retry-source");
        retry_source.state = RunState::Interrupted;
        retry_source.updated_at = Utc::now() - Duration::days(10);
        store.save_run(&retry_source).unwrap();
        let mut retry = terminal_run("retry-child");
        retry.retry_of = Some(retry_source.run_id.clone());
        retry.updated_at = Utc::now();
        store.save_run(&retry).unwrap();

        let mut active = terminal_run("active");
        active.state = RunState::Running;
        active.updated_at = Utc::now() - Duration::days(10);
        store.save_run(&active).unwrap();

        let live_worktree = d.path().join("live-worktree");
        fs::create_dir_all(&live_worktree).unwrap();
        let mut isolated = terminal_run("isolated-live");
        isolated.updated_at = Utc::now() - Duration::days(10);
        isolated.execution = Some(super::super::types::RunExecution {
            mode: super::super::types::RunExecutionMode::IsolatedWorktree,
            source_workspace: d.path().display().to_string(),
            execution_workspace: live_worktree.display().to_string(),
            base_revision: "base".into(),
            source_fingerprint: "source".into(),
            final_fingerprint: Some("final".into()),
            promotion_state: PromotionState::Ready,
            promoted_at: None,
        });
        store.save_run(&isolated).unwrap();

        store
            .save_idempotency(&IdempotencyReceipt {
                request_id: "old-receipt".into(),
                payload_hash: "hash".into(),
                run_id: None,
                tool: "ptah_submit_task".into(),
                response: serde_json::json!({"runId": "old-shared"}),
                error: None,
                created_at: Utc::now() - Duration::days(10),
                status: "complete".into(),
            })
            .unwrap();

        let report = store
            .prune_retention(RetentionPolicy {
                max_terminal_runs: 100,
                max_idempotency_receipts: 100,
                terminal_run_age: Duration::days(1),
                idempotency_receipt_age: Duration::days(1),
            })
            .unwrap();
        assert_eq!(report.run_files_removed, 1);
        assert_eq!(report.idempotency_files_removed, 1);
        assert!(store.load_run("old-shared").unwrap().is_none());
        assert!(store.load_run("retry-source").unwrap().is_some());
        assert!(store.load_run("retry-child").unwrap().is_some());
        assert!(store.load_run("active").unwrap().is_some());
        assert!(store.load_run("isolated-live").unwrap().is_some());
        assert!(store.load_idempotency("old-receipt").unwrap().is_none());
    }

    #[test]
    fn retention_enforces_terminal_run_count_without_deleting_recent_active() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        for (run_id, age_hours) in [("oldest", 3), ("middle", 2), ("newest", 1)] {
            let mut run = terminal_run(run_id);
            run.updated_at = Utc::now() - Duration::hours(age_hours);
            store.save_run(&run).unwrap();
        }
        let report = store
            .prune_retention(RetentionPolicy {
                max_terminal_runs: 2,
                max_idempotency_receipts: 100,
                terminal_run_age: Duration::days(365),
                idempotency_receipt_age: Duration::days(365),
            })
            .unwrap();
        assert_eq!(report.run_files_removed, 1);
        assert!(store.load_run("oldest").unwrap().is_none());
        assert!(store.load_run("middle").unwrap().is_some());
        assert!(store.load_run("newest").unwrap().is_some());
    }

    #[test]
    fn retention_fails_closed_on_invalid_policy_and_unknown_receipt_status() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        store
            .save_idempotency(&IdempotencyReceipt {
                request_id: "unknown-status".into(),
                payload_hash: "hash".into(),
                run_id: None,
                tool: "ptah_queue_prompt".into(),
                response: serde_json::Value::Null,
                error: None,
                created_at: Utc::now() - Duration::days(10),
                status: "future_status".into(),
            })
            .unwrap();

        let invalid = RetentionPolicy {
            terminal_run_age: Duration::days(-1),
            ..RetentionPolicy::default()
        };
        assert!(store.prune_retention(invalid).is_err());

        let report = store
            .prune_retention(RetentionPolicy {
                terminal_run_age: Duration::days(1),
                idempotency_receipt_age: Duration::days(1),
                ..RetentionPolicy::default()
            })
            .unwrap();
        assert_eq!(report.idempotency_files_removed, 0);
        assert!(report.skipped_files >= 1);
        assert!(store.load_idempotency("unknown-status").unwrap().is_some());
    }

    #[test]
    fn restart_fails_orphaned_idempotency_claim() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        assert!(matches!(
            store.claim_idempotency("t", "orphan", "h").unwrap(),
            IdempotencyClaim::Perform
        ));
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        match reopened.claim_idempotency("t", "orphan", "h").unwrap() {
            IdempotencyClaim::Replay(Err(e)) => {
                assert!(e.message.contains("interrupted"));
            }
            _ => panic!("orphan must become a durable failed receipt"),
        }
    }

    #[test]
    fn update_run_preserves_transactional_state() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let run = RunRecord {
            run_id: "tx-run".into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: "req-tx".into(),
            client_id: None,
            state: RunState::Running,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "hi".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        store.save_run(&run).unwrap();
        store
            .update_run("tx-run", |r| {
                r.state = RunState::Cancelled;
                r.terminal_result = Some("cancelled".into());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.load_run("tx-run").unwrap().unwrap().state,
            RunState::Cancelled
        );
    }

    #[test]
    fn traversal_id_rejected() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        assert!(store.claim_idempotency("t", "../x", "h").is_err());
    }

    #[test]
    fn audit_rotates_at_bound_and_reports_success() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let path = d.path().join("audit").join("audit.jsonl");
        fs::write(&path, vec![b'x'; MAX_AUDIT_BYTES as usize]).unwrap();
        let entry = AuditEntry {
            ts: Utc::now(),
            tool: "ptah_get_capacity".into(),
            request_id: None,
            session_id: None,
            workspace: None,
            outcome: "accepted".into(),
            error_code: None,
            detail: "test".into(),
        };
        store.append_audit(&entry).unwrap();
        assert!(path.with_extension("jsonl.1").is_file());
        assert!(path.is_file());
        assert!(store.last_audit_error().is_none());
    }

    #[test]
    fn queued_audit_flushes_before_store_shutdown() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let path = d.path().join("audit").join("audit.jsonl");
        store
            .enqueue_audit(AuditEntry {
                ts: Utc::now(),
                tool: "auth".into(),
                request_id: None,
                session_id: None,
                workspace: None,
                outcome: "rejected".into(),
                error_code: Some("unauthenticated".into()),
                detail: "test".into(),
            })
            .unwrap();
        drop(store);
        let body = fs::read_to_string(path).unwrap();
        assert!(body.contains("\"tool\":\"auth\""));
    }

    #[test]
    fn finalization_reconstructs_corrupt_run_and_restart_replays_intent() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let corrupt_candidate = terminal_run("corrupt-final");
        let corrupt_path = store.run_path(&corrupt_candidate.run_id).unwrap();
        fs::write(&corrupt_path, b"{broken").unwrap();
        store.persist_finalization(&corrupt_candidate).unwrap();
        assert_eq!(
            store
                .load_run(&corrupt_candidate.run_id)
                .unwrap()
                .unwrap()
                .state,
            RunState::Completed
        );

        let restart_candidate = terminal_run("restart-final");
        let intent = store.finalization_path(&restart_candidate.run_id).unwrap();
        atomic_write_json(&intent, &restart_candidate).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened
                .load_run(&restart_candidate.run_id)
                .unwrap()
                .unwrap()
                .state,
            RunState::Completed
        );
        assert!(!intent.exists());
    }

    fn queued_run(run_id: &str, sequence: u64) -> (RunRecord, AcceptanceIntent) {
        let mut run = terminal_run(run_id);
        run.state = RunState::Queued;
        run.start_seq = None;
        run.end_seq = None;
        run.terminal_result = None;
        run.final_response = None;
        run.error_code = None;
        run.prompt_preview = "redacted preview".into();
        run.request_id = format!("request-{run_id}");
        let response = serde_json::json!({
            "runId": run_id,
            "state": RunState::Queued,
        });
        let prompt = format!("private full prompt {run_id}");
        let payload_hash = super::super::types::hash_payload(&serde_json::json!({
            "sessionId": run.session_id,
            "workspace": &run.workspace,
            "prompt": &prompt,
            "bounds": &run.bounds,
            "executionMode": super::super::types::RunExecutionMode::Shared,
            "allowQueue": true,
            "retryOf": &run.retry_of,
        }));
        let intent = AcceptanceIntent {
            admission_id: format!("admission-{run_id}"),
            sequence,
            run_id: run_id.into(),
            request_id: run.request_id.clone(),
            payload_hash,
            tool: "ptah_submit_task".into(),
            session_id: run.session_id,
            workspace: run.workspace.clone(),
            execution_mode: super::super::types::RunExecutionMode::Shared,
            allow_queue: true,
            attempt_id: None,
            prompt: Some(prompt.clone()),
            prompt_hash: super::super::types::hash_payload(&serde_json::json!(prompt)),
            bounds: run.bounds.clone(),
            run: run.clone(),
            response,
            phase: AcceptancePhase::Queued,
            created_at: run.created_at,
            updated_at: run.updated_at,
            integrity: String::new(),
        };
        (run, intent)
    }

    #[test]
    fn acceptance_crash_cuts_fail_unacknowledged_admissions() {
        for cut in 0..=3 {
            let d = tempdir().unwrap();
            let store = OrchStore::open(d.path()).unwrap();
            let (run, intent) = queued_run("crash-cut", 1);
            if cut >= 1 {
                store.save_acceptance_intent(&intent).unwrap();
            }
            if cut >= 2 {
                store.save_run(&run).unwrap();
            }
            if cut >= 3 {
                store
                    .save_idempotency(&IdempotencyReceipt {
                        request_id: intent.request_id.clone(),
                        payload_hash: intent.payload_hash.clone(),
                        run_id: Some(intent.run_id.clone()),
                        tool: intent.tool.clone(),
                        response: serde_json::Value::Null,
                        error: None,
                        created_at: Utc::now(),
                        status: "pending".into(),
                    })
                    .unwrap();
            }
            drop(store);
            let reopened = OrchStore::open(d.path()).unwrap();
            if cut == 0 {
                assert!(reopened.load_run("crash-cut").unwrap().is_none());
                continue;
            }
            if cut == 1 {
                assert!(reopened.load_run("crash-cut").unwrap().is_none());
                assert!(reopened.list_acceptance_intents().unwrap().is_empty());
                continue;
            }
            assert_eq!(
                reopened.load_run("crash-cut").unwrap().unwrap().state,
                RunState::Interrupted
            );
            if cut == 3 {
                let receipt = reopened
                    .load_idempotency(&intent.request_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(receipt.status, "failed");
            } else {
                assert!(reopened
                    .load_idempotency(&intent.request_id)
                    .unwrap()
                    .is_none());
            }
            assert!(reopened.list_acceptance_intents().unwrap().is_empty());
        }
    }

    #[test]
    fn private_acceptance_input_is_0600_and_disappears_after_claim() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (run, intent) = queued_run("private-input", 1);
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        let path = store.acceptance_path(&run.run_id).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("private full prompt"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let attempt = store
            .claim_attempt(
                &run.run_id,
                "attempt-private",
                "owner-private",
                None,
                Utc::now(),
                StdDuration::from_secs(60),
            )
            .unwrap();
        let prompt = store
            .claim_acceptance_input(
                &run.run_id,
                &attempt.attempt_id,
                &attempt.owner_instance_id,
                attempt.revision,
                &RunRecord {
                    state: RunState::Running,
                    ..run.clone()
                },
                serde_json::json!({"runId": run.run_id, "state": "running"}),
            )
            .unwrap();
        assert_eq!(prompt, "private full prompt private-input");
        let body = fs::read_to_string(&path).unwrap();
        assert!(!body.contains("private full prompt"));
        assert!(body.contains("\"prompt\": null"));
        store.settle_acceptance_intent(&run.run_id).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn parseable_acceptance_tamper_is_quarantined_and_never_recovered() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (run, intent) = queued_run("tampered-acceptance", 1);
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        store
            .save_idempotency(&IdempotencyReceipt {
                request_id: intent.request_id.clone(),
                payload_hash: intent.payload_hash.clone(),
                run_id: Some(intent.run_id.clone()),
                tool: intent.tool.clone(),
                response: intent.response.clone(),
                error: None,
                created_at: Utc::now(),
                status: "complete".into(),
            })
            .unwrap();
        let path = store.acceptance_path(&run.run_id).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["prompt"] = serde_json::json!("tampered input");
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let run = reopened.load_run(&run.run_id).unwrap().unwrap();
        assert_eq!(run.state, RunState::Interrupted);
        assert_eq!(run.error_code.as_deref(), Some("admission_lost"));
        assert!(reopened.list_acceptance_intents().unwrap().is_empty());
        assert!(fs::read_dir(d.path().join("acceptance"))
            .unwrap()
            .flatten()
            .any(|entry| entry
                .path()
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| extension.starts_with("corrupt-"))));
    }

    #[test]
    fn attempt_cas_rejects_stale_owner_and_expired_mutations() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let now = Utc::now();
        let attempt = store
            .claim_attempt(
                "attempt-run",
                "attempt-1",
                "owner-1",
                None,
                now,
                StdDuration::from_secs(1),
            )
            .unwrap();
        assert!(store
            .heartbeat_attempt(
                "attempt-run",
                "attempt-1",
                "owner-2",
                attempt.revision,
                now,
                StdDuration::from_secs(1),
            )
            .is_err());
        assert!(store
            .heartbeat_attempt(
                "attempt-run",
                "attempt-1",
                "owner-1",
                attempt.revision.saturating_sub(1),
                now,
                StdDuration::from_secs(1),
            )
            .is_err());
        let expired = now + Duration::seconds(2);
        assert!(store
            .heartbeat_attempt(
                "attempt-run",
                "attempt-1",
                "owner-1",
                attempt.revision,
                expired,
                StdDuration::from_secs(1),
            )
            .is_err());
        assert!(store
            .reap_attempt("attempt-run", "attempt-1", attempt.revision, now)
            .unwrap()
            .is_none());
        let reaped = store
            .reap_attempt("attempt-run", "attempt-1", attempt.revision, expired)
            .unwrap()
            .unwrap();
        assert_eq!(reaped.phase, AttemptPhase::Reaped);
        assert!(store
            .reap_attempt("attempt-run", "attempt-1", attempt.revision, expired)
            .unwrap()
            .is_none());

        let final_attempt = store
            .claim_attempt(
                "finalize-run",
                "attempt-finalize",
                "owner-finalize",
                None,
                now,
                StdDuration::from_secs(10),
            )
            .unwrap();
        assert!(store
            .finalize_attempt(
                "finalize-run",
                "attempt-finalize",
                "wrong-owner",
                final_attempt.revision,
                AttemptPhase::Completed,
                now,
            )
            .is_err());
        let heartbeat = store
            .heartbeat_attempt(
                "finalize-run",
                "attempt-finalize",
                "owner-finalize",
                final_attempt.revision,
                now,
                StdDuration::from_secs(10),
            )
            .unwrap();
        assert!(store
            .finalize_attempt(
                "finalize-run",
                "attempt-finalize",
                "owner-finalize",
                final_attempt.revision,
                AttemptPhase::Completed,
                now,
            )
            .is_err());
        let finalizing = store
            .begin_attempt_finalization(
                "finalize-run",
                "attempt-finalize",
                "owner-finalize",
                heartbeat.revision,
                now,
            )
            .unwrap();
        let completed = store
            .finalize_attempt(
                "finalize-run",
                "attempt-finalize",
                "owner-finalize",
                finalizing.revision,
                AttemptPhase::Completed,
                now,
            )
            .unwrap();
        assert_eq!(completed.phase, AttemptPhase::Completed);
    }

    #[test]
    fn restart_rebuilds_thirty_two_acceptances_in_exact_fifo_order() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        for sequence in 1..=32 {
            let run_id = format!("fifo-{sequence}");
            let (run, intent) = queued_run(&run_id, sequence);
            store.save_run(&run).unwrap();
            store.save_acceptance_intent(&intent).unwrap();
            store
                .save_idempotency(&IdempotencyReceipt {
                    request_id: intent.request_id.clone(),
                    payload_hash: intent.payload_hash.clone(),
                    run_id: Some(intent.run_id.clone()),
                    tool: intent.tool.clone(),
                    response: intent.response.clone(),
                    error: None,
                    created_at: Utc::now(),
                    status: "complete".into(),
                })
                .unwrap();
        }
        drop(store);
        let reopened = OrchStore::open(d.path()).unwrap();
        let intents = reopened.list_acceptance_intents().unwrap();
        assert_eq!(intents.len(), 32);
        assert_eq!(
            intents
                .iter()
                .map(|intent| intent.run_id.as_str())
                .collect::<Vec<_>>(),
            (1..=32)
                .map(|sequence| format!("fifo-{sequence}"))
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            reopened
                .list_acceptance_intents()
                .unwrap()
                .iter()
                .filter(|intent| intent.prompt.is_some())
                .count(),
            32
        );
    }

    #[test]
    fn already_running_acceptance_is_interrupted_and_never_requeued() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (mut run, mut intent) = queued_run("running-restart", 1);
        run.state = RunState::Running;
        run.start_seq = Some(1);
        intent.run = run.clone();
        intent.phase = AcceptancePhase::Claimed;
        intent.prompt = None;
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        drop(store);
        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened.load_run("running-restart").unwrap().unwrap().state,
            RunState::Interrupted
        );
        assert!(reopened.list_acceptance_intents().unwrap().is_empty());
    }

    #[test]
    fn injected_acceptance_write_failure_leaves_no_prompt_temp_file() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (_run, intent) = queued_run("write-failure", 1);
        let target = store.acceptance_path(&intent.run_id).unwrap();
        fs::create_dir(&target).unwrap();
        assert!(store.save_acceptance_intent(&intent).is_err());
        fs::remove_dir(&target).unwrap();
        let leftovers = fs::read_dir(d.path().join("acceptance"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name() != "sequence.json")
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "failed private write left temporary input files: {leftovers:?}"
        );
    }

    #[test]
    fn receipt_write_failure_tombstones_acceptance_before_restart() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (run, intent) = queued_run("receipt-write-failure", 1);
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        store
            .claim_idempotency(&intent.tool, &intent.request_id, &intent.payload_hash)
            .unwrap();
        let receipt_path = store.idemp_path(&intent.request_id).unwrap();
        fs::remove_file(&receipt_path).unwrap();
        fs::create_dir(&receipt_path).unwrap();
        assert!(store
            .complete_idempotency(
                &intent.tool,
                &intent.request_id,
                &intent.payload_hash,
                Some(intent.run_id.clone()),
                intent.response.clone(),
            )
            .is_err());
        store.fail_acceptance_intent(&intent.run_id).unwrap();
        fs::remove_dir(&receipt_path).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let run = reopened.load_run(&run.run_id).unwrap().unwrap();
        assert_eq!(run.state, RunState::Interrupted);
        assert_eq!(run.error_code.as_deref(), Some("admission_lost"));
        assert!(reopened.list_acceptance_intents().unwrap().is_empty());
        assert!(reopened
            .load_idempotency(&intent.request_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn finalization_recovery_queue_is_bounded_and_projects_degraded_health() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        for index in 0..MAX_FINALIZATION_RECOVERY_INTENTS {
            store
                .ensure_finalization_intent(&terminal_run(&format!("recovery-{index}")))
                .unwrap();
        }
        assert_eq!(
            store.finalization_recovery_count(),
            MAX_FINALIZATION_RECOVERY_INTENTS
        );
        assert!(store
            .ensure_finalization_intent(&terminal_run("recovery-overflow"))
            .is_err());
        let health = store.durability_health();
        assert_eq!(health["degraded"], true);
        assert_eq!(
            health["finalizationRecoveryPending"],
            MAX_FINALIZATION_RECOVERY_INTENTS
        );
    }

    #[test]
    fn claim_receipt_restart_cut_never_resumes_running_work() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (mut run, mut intent) = queued_run("claimed-cut", 1);
        run.state = RunState::Running;
        run.start_seq = Some(1);
        intent.run = run.clone();
        intent.phase = AcceptancePhase::Claimed;
        intent.attempt_id = Some("attempt-claimed-cut".into());
        intent.prompt = None;
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        store
            .save_idempotency(&IdempotencyReceipt {
                request_id: intent.request_id.clone(),
                payload_hash: intent.payload_hash.clone(),
                run_id: Some(intent.run_id.clone()),
                tool: intent.tool.clone(),
                response: intent.response.clone(),
                error: None,
                created_at: Utc::now(),
                status: "pending".into(),
            })
            .unwrap();
        store
            .claim_attempt(
                &run.run_id,
                "attempt-claimed-cut",
                "owner-claimed-cut",
                None,
                Utc::now(),
                StdDuration::from_secs(30),
            )
            .unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened.load_run("claimed-cut").unwrap().unwrap().state,
            RunState::Interrupted
        );
        assert_eq!(
            reopened
                .load_idempotency(&intent.request_id)
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            reopened.load_attempt("claimed-cut").unwrap().unwrap().phase,
            AttemptPhase::Interrupted
        );
        assert!(reopened.list_acceptance_intents().unwrap().is_empty());
    }

    #[test]
    fn complete_receipt_before_attempt_lease_recovers_as_queued_work() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (run, intent) = queued_run("receipt-before-lease", 1);
        store.save_run(&run).unwrap();
        store.save_acceptance_intent(&intent).unwrap();
        assert!(store
            .complete_idempotency(
                &intent.tool,
                &intent.request_id,
                &intent.payload_hash,
                Some(intent.run_id.clone()),
                intent.response.clone(),
            )
            .is_err());
        store
            .claim_idempotency(&intent.tool, &intent.request_id, &intent.payload_hash)
            .unwrap();
        store
            .complete_idempotency(
                &intent.tool,
                &intent.request_id,
                &intent.payload_hash,
                Some(intent.run_id.clone()),
                intent.response.clone(),
            )
            .unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened.load_run(&run.run_id).unwrap().unwrap().state,
            RunState::Queued
        );
        assert_eq!(
            reopened
                .load_idempotency(&intent.request_id)
                .unwrap()
                .unwrap()
                .status,
            "complete"
        );
        assert!(reopened.load_attempt(&run.run_id).unwrap().is_none());
        assert_eq!(reopened.list_acceptance_intents().unwrap().len(), 1);
    }

    #[test]
    fn injected_finalization_failure_keeps_recovery_queue_bounded() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let candidate = terminal_run("finalization-write-failure");
        let target = store.finalization_path(&candidate.run_id).unwrap();
        fs::create_dir(&target).unwrap();
        assert!(store.persist_finalization(&candidate).is_err());
        assert_eq!(store.finalization_recovery_count(), 0);
        assert!(!store.durability_health()["degraded"].as_bool().unwrap());
        assert!(!target.with_extension("json.tmp").exists());
        fs::remove_dir(&target).unwrap();
    }
}
