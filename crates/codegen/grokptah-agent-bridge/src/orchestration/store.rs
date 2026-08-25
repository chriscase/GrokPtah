//! Durable run records, idempotency receipts, audit log (#196).

use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{self, AtomicU64};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use uuid::Uuid;

use super::types::{
    safe_id_filename, AdmissionRecord, AdmissionState, AgentRecord, AgentState, AuditEntry,
    ContinuationCheckpoint, IdempotencyReceipt, LeaseDenied, OrchError, OrchErrorCode,
    PromotionState, RunLease, RunRecord, RunState,
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
    /// Durable admission order, re-seeded from disk at every open.
    next_admission_sequence: AtomicU64,
    /// Queue reconstructed at open, handed to the first supervisor that asks.
    recovered_admissions: Mutex<Option<Vec<AdmissionRecord>>>,
    recovered_admissions_total: AtomicU64,
    admission_integrity_failures: AtomicU64,
    uncertain_admissions: AtomicU64,
    reaped_runs: AtomicU64,
    stuck_finalizations: AtomicU64,
}

/// One `Running` attempt the reaper retired because its lease expired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapedRun {
    pub run_id: String,
    pub session_id: Uuid,
    pub attempt: u32,
}

const MAX_AUDIT_BYTES: u64 = 4 * 1024 * 1024;

/// Conservative bounds for the durable orchestration ledger. Retention is
/// deliberately age- and count-bounded, but never trades away an active run,
/// a reviewable isolated run, or the source of a retry chain.
pub const DEFAULT_MAX_TERMINAL_RUNS: usize = 500;
pub const DEFAULT_MAX_IDEMPOTENCY_RECEIPTS: usize = 1_000;
pub const DEFAULT_TERMINAL_RUN_AGE: Duration = Duration::days(30);
pub const DEFAULT_IDEMPOTENCY_RECEIPT_AGE: Duration = Duration::days(7);

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
        // Private: the complete execution input for accepted work, and the
        // ownership of live attempts. Neither is ever projected publicly.
        fs::create_dir_all(root.join("admissions"))?;
        fs::create_dir_all(root.join("leases"))?;
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
                next_admission_sequence: AtomicU64::new(1),
                recovered_admissions: Mutex::new(None),
                recovered_admissions_total: AtomicU64::new(0),
                admission_integrity_failures: AtomicU64::new(0),
                uncertain_admissions: AtomicU64::new(0),
                reaped_runs: AtomicU64::new(0),
                stuck_finalizations: AtomicU64::new(0),
            }),
        };
        store.recover_finalization_intents()?;
        // Reconstruct the accepted queue before anything interrupts it, and
        // retire every attempt lease: the exclusive store lock means a lease
        // present at open cannot belong to a live attempt.
        store.clear_stale_leases()?;
        let recovered = store.recover_admissions()?;
        let survivors: HashSet<String> = recovered
            .iter()
            .map(|record| record.run_id.clone())
            .collect();
        *store.inner.recovered_admissions.lock() = Some(recovered);
        store.retire_lost_queued_runs(&survivors)?;
        store.mark_unfinished_interrupted_excluding(&survivors)?;
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
        self.mark_unfinished_interrupted_excluding(&HashSet::new())
    }

    /// Crash recovery, minus the queued runs whose durable admission record
    /// survived. Those keep their accepted position instead of being silently
    /// destroyed; everything else that was mid-flight becomes `interrupted`.
    fn mark_unfinished_interrupted_excluding(
        &self,
        keep_queued: &HashSet<String>,
    ) -> anyhow::Result<usize> {
        let mut n = 0;
        let mut interrupted_agents = Vec::new();
        for mut run in self.list_runs()? {
            if run.state == RunState::Queued && keep_queued.contains(&run.run_id) {
                continue;
            }
            if matches!(run.state, RunState::Queued | RunState::Running) {
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

    // ── durable admission queue ────────────────────────────────────────
    //
    // Accepted-but-not-started work is durable here, not in a process-local
    // queue. The two invariants that make the receipt honest:
    //   1. the record is fsync-safe (file + parent dir) before the caller's
    //      idempotency receipt is allowed to settle, and
    //   2. `Queued` is the only promotable state, and the transition out of
    //      it is a compare-and-set under this store's lock, so exactly one
    //      supervisor can ever consume it.

    fn admission_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("admissions")
            .join(format!("{safe}.json")))
    }

    fn lease_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self.inner.root.join("leases").join(format!("{safe}.json")))
    }

    /// Allocate the next durable admission order number. Monotonic for the
    /// life of this store handle, and re-seeded from disk at every open, so
    /// restart replays the accepted queue in arrival order rather than in
    /// directory order.
    pub fn next_admission_sequence(&self) -> u64 {
        self.inner
            .next_admission_sequence
            .fetch_add(1, atomic::Ordering::SeqCst)
    }

    /// Durably record one accepted admission. Exclusive-create: a second
    /// write for the same run is a conflict, never a silent overwrite.
    pub fn save_admission(&self, record: &AdmissionRecord) -> Result<(), OrchError> {
        record.validate()?;
        if record.state != AdmissionState::Queued {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "a new admission must be queued",
            ));
        }
        let mut sealed = record.clone();
        sealed.seal();
        let path = self.admission_path(&record.run_id)?;
        let _guard = self.inner.lock.lock();
        match write_private_json_exclusive(&path, &sealed) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(OrchError::new(
                OrchErrorCode::Conflict,
                "admission already exists for this run",
            )),
            Err(error) => {
                // A short write or a failed fsync can leave a partial record
                // on disk. The caller is about to be told the submission
                // failed, so remove it: a half-written admission must never
                // be something a later recovery has to reason about.
                let _ = fs::remove_file(&path);
                *self.inner.last_run_error.lock() = Some(error.to_string());
                Err(OrchError::new(OrchErrorCode::Internal, error.to_string()))
            }
        }
    }

    /// Read one admission, failing closed on a tampered or truncated record.
    pub fn load_admission(&self, run_id: &str) -> Result<Option<AdmissionRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_admission_unlocked(run_id)
    }

    fn load_admission_unlocked(&self, run_id: &str) -> Result<Option<AdmissionRecord>, OrchError> {
        let path = match self.admission_path(run_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let record: AdmissionRecord = serde_json::from_str(&text)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if !record.integrity_ok() || record.run_id != run_id {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "admission integrity check failed",
            ));
        }
        record.validate()?;
        Ok(Some(record))
    }

    /// Number of durably queued admissions still awaiting promotion.
    pub fn queued_admission_count(&self) -> usize {
        let _guard = self.inner.lock.lock();
        let Ok(entries) = fs::read_dir(self.inner.root.join("admissions")) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .filter(|entry| {
                fs::read_to_string(entry.path())
                    .ok()
                    .and_then(|text| serde_json::from_str::<AdmissionRecord>(&text).ok())
                    .is_some_and(|record| record.state == AdmissionState::Queued)
            })
            .count()
    }

    /// Durably retire queued work so nothing can resurrect it. The tombstone
    /// is written before the file is unlinked, so a crash mid-cancel still
    /// leaves a consumed marker rather than a promotable record.
    pub fn tombstone_admission(&self, run_id: &str) -> bool {
        let _guard = self.inner.lock.lock();
        let Ok(path) = self.admission_path(run_id) else {
            return false;
        };
        if !path.is_file() {
            return false;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(mut record) = serde_json::from_str::<AdmissionRecord>(&text) {
                record.state = AdmissionState::Tombstoned;
                record.updated_at = Utc::now();
                record.seal();
                let _ = atomic_write_private_json(&path, &record);
            }
        }
        fs::remove_file(&path).is_ok()
    }

    /// Consume one queued admission and start its run, exactly once.
    ///
    /// Everything below happens under the single store lock: the admission is
    /// compare-and-set out of `Queued`, the attempt lease is installed, and
    /// only then does the run become `Running`. A crash at any cut leaves the
    /// admission consumed and the run non-running, which recovery resolves to
    /// `interrupted` — never to a second dispatch.
    pub fn promote_admission(
        &self,
        run_id: &str,
        owner_id: &str,
        start_seq: u64,
        lease_ttl: Duration,
    ) -> Result<(RunRecord, AdmissionRecord), OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(mut admission) = self.load_admission_unlocked(run_id)? else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "admission is no longer queued",
            ));
        };
        if admission.state != AdmissionState::Queued {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "admission is already consumed",
            ));
        }
        let run = self
            .load_run_unlocked(run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "run record is missing"))?;
        if run.state != RunState::Queued {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("run is no longer queued ({:?})", run.state),
            ));
        }
        if run.session_id != admission.session_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "admission does not belong to the run's session",
            ));
        }

        let path = self.admission_path(run_id)?;
        let consumed = {
            let mut consumed = admission.clone();
            consumed.state = AdmissionState::Promoted;
            consumed.updated_at = Utc::now();
            consumed.seal();
            consumed
        };
        atomic_write_private_json(&path, &consumed)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;

        let attempt = self
            .load_lease_unlocked(run_id)
            .map(|lease| lease.attempt.saturating_add(1))
            .unwrap_or(1);
        let lease = self.write_lease_unlocked(run_id, run.session_id, owner_id, attempt, lease_ttl);
        let started = (|| -> anyhow::Result<RunRecord> {
            let mut started = run.clone();
            started.state = RunState::Running;
            started.queue_position = None;
            started.start_seq = Some(start_seq);
            started.updated_at = Utc::now();
            let run_path = self
                .run_path(run_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            atomic_write_json(&run_path, &started)?;
            Ok(started)
        })();

        match started {
            Ok(started) => {
                let _ = fs::remove_file(&path);
                admission.state = AdmissionState::Promoted;
                Ok((started, admission))
            }
            Err(error) => {
                // The admission is already consumed, so this work must never
                // be re-dispatched. Fail the run closed instead of leaving a
                // queued record nothing will ever start.
                if lease.is_some() {
                    let _ = self.clear_lease_unlocked(run_id);
                }
                let mut failed = run;
                failed.state = RunState::Failed;
                failed.queue_position = None;
                failed.terminal_result = Some("failed".into());
                failed.error_code = Some("admission_promotion_failed".into());
                failed.updated_at = Utc::now();
                if let Ok(run_path) = self.run_path(run_id) {
                    if atomic_write_json(&run_path, &failed).is_err() {
                        self.inner
                            .stuck_finalizations
                            .fetch_add(1, atomic::Ordering::Relaxed);
                    }
                }
                let _ = fs::remove_file(&path);
                Err(OrchError::new(OrchErrorCode::Internal, error.to_string()))
            }
        }
    }

    /// Adopt the queue reconstructed at open, exactly once per store handle.
    ///
    /// The store root is held under an exclusive advisory lock, so there is
    /// one store per ledger per process. Handing the recovered queue to the
    /// first caller keeps a second embedded control service from adopting the
    /// same work and dispatching it twice.
    pub fn take_recovered_admissions(&self) -> Vec<AdmissionRecord> {
        self.inner
            .recovered_admissions
            .lock()
            .take()
            .unwrap_or_default()
    }

    // ── attempt leases and the staleness reaper ────────────────────────

    fn load_lease_unlocked(&self, run_id: &str) -> Option<RunLease> {
        let path = self.lease_path(run_id).ok()?;
        let text = fs::read_to_string(path).ok()?;
        let lease: RunLease = serde_json::from_str(&text).ok()?;
        (lease.run_id == run_id).then_some(lease)
    }

    fn write_lease_unlocked(
        &self,
        run_id: &str,
        session_id: Uuid,
        owner_id: &str,
        attempt: u32,
        ttl: Duration,
    ) -> Option<RunLease> {
        let now = Utc::now();
        let lease = RunLease {
            run_id: run_id.to_string(),
            session_id,
            owner_id: owner_id.to_string(),
            attempt,
            acquired_at: now,
            heartbeat_at: now,
            expires_at: now + ttl,
        };
        let path = self.lease_path(run_id).ok()?;
        atomic_write_private_json(&path, &lease).ok()?;
        Some(lease)
    }

    fn clear_lease_unlocked(&self, run_id: &str) -> bool {
        let Ok(path) = self.lease_path(run_id) else {
            return false;
        };
        fs::remove_file(path).is_ok()
    }

    /// Install the owning lease for a run that starts without queueing.
    pub fn install_lease(
        &self,
        run_id: &str,
        session_id: Uuid,
        owner_id: &str,
        ttl: Duration,
    ) -> Option<RunLease> {
        let _guard = self.inner.lock.lock();
        let attempt = self
            .load_lease_unlocked(run_id)
            .map(|lease| lease.attempt.saturating_add(1))
            .unwrap_or(1);
        self.write_lease_unlocked(run_id, session_id, owner_id, attempt, ttl)
    }

    pub fn load_lease(&self, run_id: &str) -> Option<RunLease> {
        let _guard = self.inner.lock.lock();
        self.load_lease_unlocked(run_id)
    }

    /// Refresh one attempt's liveness. A heartbeat can only ever extend the
    /// exact live attempt: it never creates a lease, never adopts one owned
    /// by another owner or attempt, and never touches a terminal run.
    pub fn heartbeat_run(
        &self,
        run_id: &str,
        owner_id: &str,
        attempt: u32,
        ttl: Duration,
    ) -> Result<RunLease, LeaseDenied> {
        let _guard = self.inner.lock.lock();
        let run = match self.load_run_unlocked(run_id) {
            Ok(Some(run)) => run,
            Ok(None) => return Err(LeaseDenied::UnknownRun),
            Err(_) => return Err(LeaseDenied::UnknownRun),
        };
        if run.state.is_terminal() {
            return Err(LeaseDenied::Terminal);
        }
        let Some(existing) = self.load_lease_unlocked(run_id) else {
            return Err(LeaseDenied::Missing);
        };
        if !existing.matches(owner_id, attempt) {
            return Err(LeaseDenied::WrongOwner);
        }
        let now = Utc::now();
        let refreshed = RunLease {
            heartbeat_at: now,
            expires_at: now + ttl,
            ..existing
        };
        let path = self
            .lease_path(run_id)
            .map_err(|_| LeaseDenied::UnknownRun)?;
        atomic_write_private_json(&path, &refreshed).map_err(|_| LeaseDenied::Missing)?;
        Ok(refreshed)
    }

    /// Release the lease held by this exact attempt. A stale owner cannot
    /// release a newer attempt's lease.
    pub fn release_lease(&self, run_id: &str, owner_id: &str, attempt: u32) -> bool {
        let _guard = self.inner.lock.lock();
        match self.load_lease_unlocked(run_id) {
            Some(lease) if lease.matches(owner_id, attempt) => self.clear_lease_unlocked(run_id),
            _ => false,
        }
    }

    /// Move every `Running` run whose attempt lease has expired to
    /// `interrupted` with `lost_worker`, and report them so the caller can
    /// release the capacity they were holding.
    ///
    /// Deterministic by construction: the only input is the persisted
    /// expiry, so the same ledger and the same clock always reap the same
    /// set. Live attempts (fresh heartbeat) and terminal runs are never
    /// touched; a lease whose run is already terminal is simply cleared.
    pub fn reap_expired_leases(&self, now: DateTime<Utc>) -> Vec<ReapedRun> {
        let _guard = self.inner.lock.lock();
        let mut reaped = Vec::new();
        let Ok(entries) = fs::read_dir(self.inner.root.join("leases")) else {
            return reaped;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(lease) = serde_json::from_str::<RunLease>(&text) else {
                let _ = fs::remove_file(&path);
                continue;
            };
            let run = match self.load_run_unlocked(&lease.run_id) {
                Ok(Some(run)) => run,
                _ => {
                    let _ = fs::remove_file(&path);
                    continue;
                }
            };
            if run.state.is_terminal() {
                let _ = fs::remove_file(&path);
                continue;
            }
            if !lease.is_expired_at(now) {
                continue;
            }
            let mut lost = run;
            lost.state = RunState::Interrupted;
            lost.queue_position = None;
            lost.terminal_result = Some("interrupted".into());
            lost.error_code = Some("lost_worker".into());
            lost.updated_at = now;
            if let Some(execution) = lost.execution.as_mut() {
                execution.promotion_state = PromotionState::Conflicted;
            }
            let Ok(run_path) = self.run_path(&lease.run_id) else {
                continue;
            };
            if atomic_write_json(&run_path, &lost).is_err() {
                continue;
            }
            let _ = fs::remove_file(&path);
            self.inner
                .reaped_runs
                .fetch_add(1, atomic::Ordering::Relaxed);
            reaped.push(ReapedRun {
                run_id: lease.run_id,
                session_id: lease.session_id,
                attempt: lease.attempt,
            });
        }
        reaped
    }

    // ── bounded finalization ───────────────────────────────────────────

    /// Preserve a terminal candidate as a write-ahead intent without claiming
    /// the run was finalized. `recover_finalization_intents` replays it at the
    /// next open. This is the escape hatch that lets a bounded retry give the
    /// admission slot back instead of spinning on a full disk forever.
    pub fn write_finalization_intent(&self, candidate: &RunRecord) -> anyhow::Result<()> {
        anyhow::ensure!(
            candidate.state.is_terminal(),
            "finalization candidate must be terminal"
        );
        let _guard = self.inner.lock.lock();
        let intent_path = self
            .finalization_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&intent_path, candidate)
    }

    /// Count of finalizations that exhausted their bounded retry and were
    /// left to intent replay. Non-zero means a run is durably unresolved.
    pub fn note_stuck_finalization(&self) -> u64 {
        self.inner
            .stuck_finalizations
            .fetch_add(1, atomic::Ordering::Relaxed)
            .saturating_add(1)
    }

    pub fn stuck_finalizations(&self) -> u64 {
        self.inner
            .stuck_finalizations
            .load(atomic::Ordering::Relaxed)
    }

    pub fn reaped_runs(&self) -> u64 {
        self.inner.reaped_runs.load(atomic::Ordering::Relaxed)
    }

    pub fn admission_integrity_failures(&self) -> u64 {
        self.inner
            .admission_integrity_failures
            .load(atomic::Ordering::Relaxed)
    }

    /// Admissions retired at open because their acceptance was never
    /// acknowledged to the client. Each one is work that fails closed rather
    /// than running against a receipt the client was told had failed.
    pub fn uncertain_admissions(&self) -> u64 {
        self.inner
            .uncertain_admissions
            .load(atomic::Ordering::Relaxed)
    }

    pub fn recovered_admission_count(&self) -> u64 {
        self.inner
            .recovered_admissions_total
            .load(atomic::Ordering::Relaxed)
    }

    pub fn pending_finalization_intents(&self) -> usize {
        let Ok(entries) = fs::read_dir(self.inner.root.join("finalization")) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .count()
    }

    // ── restart reconstruction ─────────────────────────────────────────

    /// Rebuild the exact accepted admission queue and retire everything that
    /// cannot be honoured. Runs before `mark_unfinished_interrupted`, whose
    /// exception set it produces.
    fn recover_admissions(&self) -> anyhow::Result<Vec<AdmissionRecord>> {
        let dir = self.inner.root.join("admissions");
        let mut recovered: Vec<AdmissionRecord> = Vec::new();
        let mut highest = 0u64;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let parsed = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<AdmissionRecord>(&text).ok());
            let trusted = parsed
                .as_ref()
                .is_some_and(|record| record.integrity_ok() && record.validate().is_ok());
            let Some(record) = parsed.filter(|_| trusted) else {
                // Never execute a record whose bytes cannot be trusted, and
                // never delete the evidence either. When the record still
                // parses we know which run it belonged to, so that run can be
                // told what happened instead of failing anonymously.
                self.inner
                    .admission_integrity_failures
                    .fetch_add(1, atomic::Ordering::Relaxed);
                let quarantine =
                    path.with_extension(format!("json.corrupt-{}", Utc::now().timestamp_millis()));
                let _ = fs::rename(&path, &quarantine);
                continue;
            };
            highest = highest.max(record.sequence);
            if record.state != AdmissionState::Queued {
                // Already consumed: promoted or tombstoned before the crash.
                let _ = fs::remove_file(&path);
                continue;
            }
            match self.load_run_unlocked(&record.run_id) {
                Ok(Some(run)) if run.state == RunState::Queued => {
                    // Reconcile by request identity before re-admitting.
                    // A crash between the durable record and the settled
                    // receipt leaves the client holding a *failed* mutation
                    // (`fail_orphaned_idempotency_claims` below), so running
                    // this work anyway would execute a request its caller was
                    // told did not happen. Fail closed instead; the caller
                    // owns the retry, and it can never become a duplicate.
                    if self.receipt_acknowledged(&record.request_id) {
                        recovered.push(record);
                    } else {
                        self.inner
                            .uncertain_admissions
                            .fetch_add(1, atomic::Ordering::Relaxed);
                        let _ = fs::remove_file(&path);
                    }
                }
                // Terminal, running, or missing: the admission is either
                // already consumed or unresolvable. Fail closed and let the
                // run's own recovery state stand.
                _ => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        recovered.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        self.inner
            .next_admission_sequence
            .store(highest.saturating_add(1), atomic::Ordering::SeqCst);
        self.inner
            .recovered_admissions_total
            .store(recovered.len() as u64, atomic::Ordering::Relaxed);
        Ok(recovered)
    }

    /// Retire every queued run whose executable input did not survive.
    ///
    /// This is the complement of the recovered queue, so it covers both
    /// shapes at once: a record that recovery retired (uncertain, tampered,
    /// already consumed) and a record that was never written or is simply
    /// gone. `admission_lost` is deliberately distinct from a plain restart
    /// `interrupted`: it is the marker the replay path checks before it is
    /// allowed to hand a client back a `queued` result the ledger can no
    /// longer honour.
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

    /// Whether the caller was durably told this request was accepted. Only a
    /// settled `complete` receipt counts; a pending claim (about to be failed
    /// as orphaned) and a missing one are both uncertain.
    fn receipt_acknowledged(&self, request_id: &str) -> bool {
        matches!(
            self.load_idempotency(request_id),
            Ok(Some(receipt)) if receipt.status == "complete" && receipt.request_id == request_id
        )
    }

    /// Every attempt lease dies with the process that owned it. The store
    /// root is exclusively locked, so a lease found at open can never belong
    /// to a live attempt.
    fn clear_stale_leases(&self) -> anyhow::Result<usize> {
        let dir = self.inner.root.join("leases");
        let mut cleared = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                cleared += 1;
            }
        }
        Ok(cleared)
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
    let mut file = fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Atomic replace for a record that must not be world-readable. Same durable
/// shape as `atomic_write_json` (fsync on the file and the parent directory),
/// with `0600` established before any bytes are written.
fn atomic_write_private_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = fs::remove_file(&tmp);
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Exclusive private create: the durable proof that this admission is new.
fn write_private_json_exclusive<T: serde::Serialize>(
    path: &Path,
    value: &T,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
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
}
