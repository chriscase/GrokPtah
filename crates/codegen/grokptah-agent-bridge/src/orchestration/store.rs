//! Durable run records, idempotency receipts, audit log (#196).

use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

use chrono::{Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;

use super::admission::{
    AcceptanceIntent, AttemptLease, AttemptLeaseState, ProviderSendFailure, ProviderSendRecord,
    ProviderSendState, ATTEMPT_LEASE_VERSION, PROVIDER_SEND_VERSION,
};
use super::ledger_io::LedgerDir;
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
    /// Open directory handles for every ledger that can hold private
    /// execution material. All reads and writes below go through these, so a
    /// name swapped for a link between check and use cannot be followed.
    inputs: LedgerDir,
    leases: LedgerDir,
    sends: LedgerDir,
    tombstones: LedgerDir,
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
        // Private execution input, attempt leases, provider-send evidence, and
        // idempotency tombstones. These hold the only durable copy of a queued
        // prompt and the only record of whether work reached a provider, so
        // each is opened as an owner-only directory handle and every access
        // below is handle-relative and refuses to follow links.
        create_private_dir(&root.join("inputs"))?;
        create_private_dir(&root.join("leases"))?;
        create_private_dir(&root.join("sends"))?;
        create_private_dir(&root.join("tombstones"))?;
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
        let open_ledger = |root: &Path, name: &str| -> anyhow::Result<LedgerDir> {
            LedgerDir::open(&root.join(name)).map_err(|error| anyhow::anyhow!(error.to_string()))
        };
        let inputs = open_ledger(&root, "inputs")?;
        let leases = open_ledger(&root, "leases")?;
        let sends = open_ledger(&root, "sends")?;
        let tombstones = open_ledger(&root, "tombstones")?;
        let store = Self {
            inner: Arc::new(OrchStoreInner {
                root,
                inputs,
                leases,
                sends,
                tombstones,
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
        store.recover_finalization_intents()?;
        // Opening the ledger takes an exclusive advisory lock, so reaching
        // this point proves no other process owns these runs. Every attempt
        // lease on disk therefore belongs to an instance that is gone, and is
        // released so recovery does not have to wait out its TTL.
        store.release_orphaned_attempt_leases()?;
        store.mark_unfinished_interrupted()?;
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

    // ── durable execution input (acceptance intents) ───────────────────

    /// Ledger file name for one record id. The name is a store-generated
    /// digest, so it is always a safe single path component.
    fn record_name(id: &str) -> Result<String, OrchError> {
        Ok(format!("{}.json", safe_id_filename(id)?))
    }

    /// Persist the sealed, private, bounded execution input for one accepted
    /// run. This must land (and fsync) **before** the idempotency receipt is
    /// completed: a receipt that says "accepted" always has a durable input
    /// behind it.
    pub fn save_acceptance_intent(&self, intent: &AcceptanceIntent) -> Result<(), OrchError> {
        intent.validate()?;
        let name = Self::record_name(&intent.run_id)?;
        let bytes = serde_json::to_vec_pretty(intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let _guard = self.inner.lock.lock();
        self.inner.inputs.write_private(&name, &bytes)
    }

    /// Load and re-verify a sealed input. The digest is recomputed on every
    /// load, so a parseable tamper fails closed instead of executing.
    pub fn load_acceptance_intent(
        &self,
        run_id: &str,
    ) -> Result<Option<AcceptanceIntent>, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.load_acceptance_intent_unlocked(&name, run_id)
    }

    fn load_acceptance_intent_unlocked(
        &self,
        name: &str,
        run_id: &str,
    ) -> Result<Option<AcceptanceIntent>, OrchError> {
        let Some(text) = self.inner.inputs.read_private(name)? else {
            return Ok(None);
        };
        let intent: AcceptanceIntent = serde_json::from_str(&text).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Conflict,
                format!("acceptance intent is unreadable: {error}"),
            )
        })?;
        intent.validate()?;
        if intent.run_id != run_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "acceptance intent does not belong to this run",
            ));
        }
        Ok(Some(intent))
    }

    /// Every input record on disk, as `(file name, verified run id)`. The run
    /// id is `None` for a record that no longer verifies; recovery needs those
    /// too, so it can remove them rather than leave them behind.
    pub fn list_acceptance_intent_run_ids(&self) -> anyhow::Result<Vec<(String, Option<String>)>> {
        let _guard = self.inner.lock.lock();
        let names = self
            .inner
            .inputs
            .list("json")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut out = Vec::new();
        for name in names {
            let run_id = self
                .inner
                .inputs
                .read_private(&name)
                .ok()
                .flatten()
                .and_then(|text| serde_json::from_str::<AcceptanceIntent>(&text).ok())
                .filter(|intent| intent.validate().is_ok())
                .map(|intent| intent.run_id);
            out.push((name, run_id));
        }
        out.sort();
        Ok(out)
    }

    /// Drop durable input. Callers must never do this for a run that is still
    /// admitted and not yet terminal: the input is the only copy.
    pub fn remove_acceptance_intent(&self, run_id: &str) -> Result<bool, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.inner.inputs.remove(&name)
    }

    /// Remove an input record by its on-disk name. Used only by recovery, for
    /// garbage whose body no longer names a run.
    pub fn remove_acceptance_intent_file(&self, name: &str) -> Result<bool, OrchError> {
        let _guard = self.inner.lock.lock();
        self.inner.inputs.remove(name)
    }

    // ── attempt leases ─────────────────────────────────────────────────

    pub fn load_attempt_lease(&self, run_id: &str) -> Result<Option<AttemptLease>, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.load_attempt_lease_unlocked(&name, run_id)
    }

    fn load_attempt_lease_unlocked(
        &self,
        name: &str,
        run_id: &str,
    ) -> Result<Option<AttemptLease>, OrchError> {
        let Some(text) = self.inner.leases.read_private(name)? else {
            return Ok(None);
        };
        let lease: AttemptLease = serde_json::from_str(&text).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Conflict,
                format!("attempt lease is unreadable: {error}"),
            )
        })?;
        lease.validate()?;
        if lease.run_id != run_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease does not belong to this run",
            ));
        }
        Ok(Some(lease))
    }

    fn write_attempt_lease_unlocked(
        &self,
        name: &str,
        lease: &AttemptLease,
    ) -> Result<(), OrchError> {
        let bytes = serde_json::to_vec_pretty(lease)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.inner.leases.write_private(name, &bytes)
    }

    /// Compare-and-swap the single attempt authorized to dispatch `run_id`.
    ///
    /// Succeeds only when there is no lease, when the current lease was
    /// released, or when it is expired against its own durable heartbeat.
    /// Each success mints a new attempt id and bumps the attempt number, so a
    /// previous holder that comes back can never renew, release, or be
    /// mistaken for the current attempt.
    pub fn acquire_attempt_lease(
        &self,
        run_id: &str,
        owner_id: &str,
        session_id: uuid::Uuid,
        intent_digest: &str,
        ttl_ms: u64,
    ) -> Result<AttemptLease, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let now = Utc::now();
        // A lease that no longer verifies is not a licence to run beside an
        // unknown holder; `?` refuses rather than overwriting it.
        let previous = self.load_attempt_lease_unlocked(&name, run_id)?;
        let next_attempt = match &previous {
            Some(lease) if lease.is_active(now) => {
                return Err(OrchError::with_data(
                    OrchErrorCode::Conflict,
                    "run already has an active attempt",
                    serde_json::json!({
                        "runId": run_id,
                        "attempt": lease.attempt,
                        "ownerId": lease.owner_id,
                    }),
                ));
            }
            Some(lease) => lease.attempt.saturating_add(1),
            None => 1,
        };
        let lease = AttemptLease {
            lease_version: ATTEMPT_LEASE_VERSION,
            run_id: run_id.to_string(),
            attempt: next_attempt,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            owner_id: owner_id.to_string(),
            session_id,
            intent_digest: intent_digest.to_string(),
            acquired_at: now,
            heartbeat_at: now,
            lease_ttl_ms: ttl_ms.max(1),
            state: AttemptLeaseState::Held,
            digest: String::new(),
        }
        .seal();
        lease.validate()?;
        self.write_attempt_lease_unlocked(&name, &lease)?;
        Ok(lease)
    }

    /// Extend a lease this exact attempt still holds.
    ///
    /// Refused for a different owner, for a stale attempt id, for a lease that
    /// is no longer held, and — critically — for one that has already expired.
    /// An expired holder must not be able to heartbeat its way back in: the
    /// reconciler may already have handed the run to a new attempt, and two
    /// live holders is exactly the state the lease exists to prevent.
    pub fn renew_attempt_lease(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_id: &str,
    ) -> Result<AttemptLease, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let Some(current) = self.load_attempt_lease_unlocked(&name, run_id)? else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease is missing",
            ));
        };
        if current.attempt_id != attempt_id || current.owner_id != owner_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease is held by another attempt",
            ));
        }
        if current.state != AttemptLeaseState::Held {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease is no longer held",
            ));
        }
        if current.is_expired(Utc::now()) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease has expired and cannot be renewed",
            ));
        }
        let renewed = AttemptLease {
            heartbeat_at: Utc::now(),
            digest: String::new(),
            ..current
        }
        .seal();
        self.write_attempt_lease_unlocked(&name, &renewed)?;
        Ok(renewed)
    }

    /// Release a lease this exact attempt holds. Idempotent for the holder,
    /// refused for anyone else.
    pub fn release_attempt_lease(
        &self,
        run_id: &str,
        attempt_id: &str,
        owner_id: &str,
    ) -> Result<bool, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let Some(current) = self.load_attempt_lease_unlocked(&name, run_id)? else {
            return Ok(false);
        };
        if current.attempt_id != attempt_id || current.owner_id != owner_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "attempt lease is held by another attempt",
            ));
        }
        if current.state == AttemptLeaseState::Released {
            return Ok(false);
        }
        let released = AttemptLease {
            state: AttemptLeaseState::Released,
            heartbeat_at: Utc::now(),
            digest: String::new(),
            ..current
        }
        .seal();
        self.write_attempt_lease_unlocked(&name, &released)?;
        Ok(true)
    }

    /// Reclaim one expired lease so a fresh attempt can take the run.
    ///
    /// Used only by the expired-lease reconciler, and only after it has
    /// established that no live worker in this process still owns the run.
    /// Returns the reclaimed lease, or `None` when nothing needed reclaiming.
    pub fn reclaim_expired_attempt_lease(
        &self,
        run_id: &str,
    ) -> Result<Option<AttemptLease>, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let Some(current) = self.load_attempt_lease_unlocked(&name, run_id)? else {
            return Ok(None);
        };
        if current.state != AttemptLeaseState::Held || !current.is_expired(Utc::now()) {
            return Ok(None);
        }
        let reclaimed = AttemptLease {
            state: AttemptLeaseState::Released,
            heartbeat_at: Utc::now(),
            digest: String::new(),
            ..current
        }
        .seal();
        self.write_attempt_lease_unlocked(&name, &reclaimed)?;
        Ok(Some(reclaimed))
    }

    /// Every lease on disk, verified. Records that no longer verify are
    /// returned by name only so the caller can remove them.
    pub fn list_attempt_leases(&self) -> Result<Vec<(String, Option<AttemptLease>)>, OrchError> {
        let _guard = self.inner.lock.lock();
        let names = self.inner.leases.list("json")?;
        let mut out = Vec::new();
        for name in names {
            let lease = self
                .inner
                .leases
                .read_private(&name)
                .ok()
                .flatten()
                .and_then(|text| serde_json::from_str::<AttemptLease>(&text).ok())
                .filter(|lease| lease.validate().is_ok());
            out.push((name, lease));
        }
        Ok(out)
    }

    /// Drop a lease record entirely. Only for a run that is already terminal.
    pub fn remove_attempt_lease(&self, run_id: &str) -> Result<bool, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.inner.leases.remove(&name)
    }

    // ── provider send evidence ─────────────────────────────────────────

    /// Mint a durable send identity in the `KnownNotSent` state.
    ///
    /// Written before anything is transmitted, so the very existence of this
    /// record marks the point after which "nothing happened" stops being a
    /// safe assumption.
    pub fn open_provider_send(
        &self,
        run_id: &str,
        attempt_id: &str,
        spec_key: &str,
    ) -> Result<ProviderSendRecord, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let now = Utc::now();
        let record = ProviderSendRecord {
            send_version: PROVIDER_SEND_VERSION,
            send_id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            attempt_id: attempt_id.to_string(),
            spec_key: spec_key.to_string(),
            state: ProviderSendState::KnownNotSent,
            failure: None,
            detail: None,
            created_at: now,
            updated_at: now,
            digest: String::new(),
        }
        .seal();
        record.validate()?;
        self.write_provider_send_unlocked(&name, &record)?;
        Ok(record)
    }

    pub fn load_provider_send(
        &self,
        run_id: &str,
    ) -> Result<Option<ProviderSendRecord>, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.load_provider_send_unlocked(&name, run_id)
    }

    fn load_provider_send_unlocked(
        &self,
        name: &str,
        run_id: &str,
    ) -> Result<Option<ProviderSendRecord>, OrchError> {
        let Some(text) = self.inner.sends.read_private(name)? else {
            return Ok(None);
        };
        let record: ProviderSendRecord = serde_json::from_str(&text).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Conflict,
                format!("provider send record is unreadable: {error}"),
            )
        })?;
        record.validate()?;
        if record.run_id != run_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "provider send record does not belong to this run",
            ));
        }
        Ok(Some(record))
    }

    fn write_provider_send_unlocked(
        &self,
        name: &str,
        record: &ProviderSendRecord,
    ) -> Result<(), OrchError> {
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.inner.sends.write_private(name, &bytes)
    }

    /// Advance one send record along its state machine.
    ///
    /// Only forward transitions are accepted, and only for the exact attempt
    /// and send identity that opened the record, so evidence can become more
    /// definite but never weaker and never reattributed.
    pub fn advance_provider_send(
        &self,
        run_id: &str,
        send_id: &str,
        attempt_id: &str,
        next: ProviderSendState,
        failure: Option<ProviderSendFailure>,
        detail: Option<String>,
    ) -> Result<ProviderSendRecord, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        let Some(current) = self.load_provider_send_unlocked(&name, run_id)? else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "provider send record is missing",
            ));
        };
        if current.send_id != send_id || current.attempt_id != attempt_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "provider send record belongs to another attempt",
            ));
        }
        if current.state == next && failure == current.failure {
            return Ok(current);
        }
        if !current.may_transition_to(next) {
            return Err(OrchError::with_data(
                OrchErrorCode::Conflict,
                "provider send state cannot move backwards",
                serde_json::json!({
                    "from": current.state.as_str(),
                    "to": next.as_str(),
                }),
            ));
        }
        let updated = ProviderSendRecord {
            state: next,
            failure,
            detail,
            updated_at: Utc::now(),
            digest: String::new(),
            ..current
        }
        .seal();
        updated.validate()?;
        self.write_provider_send_unlocked(&name, &updated)?;
        Ok(updated)
    }

    pub fn remove_provider_send(&self, run_id: &str) -> Result<bool, OrchError> {
        let name = Self::record_name(run_id)?;
        let _guard = self.inner.lock.lock();
        self.inner.sends.remove(&name)
    }

    // ── idempotency tombstones ─────────────────────────────────────────

    /// Record, durably and compactly, that one request identity has already
    /// been decided.
    ///
    /// Receipts are pruned by retention; tombstones are not. Without them,
    /// pruning a failed receipt would silently reopen the request for a fresh
    /// attempt — a failed submission would become executable simply by waiting
    /// out the retention horizon. The tombstone is tiny by design so keeping
    /// it far longer than the receipt costs almost nothing.
    pub fn write_idempotency_tombstone(
        &self,
        request_id: &str,
        tool: &str,
        payload_hash: &str,
        outcome: &str,
        run_id: Option<&str>,
    ) -> Result<(), OrchError> {
        let name = Self::record_name(request_id)?;
        let _guard = self.inner.lock.lock();
        let record = serde_json::json!({
            "tombstoneVersion": 1,
            "requestId": request_id,
            "tool": tool,
            "payloadHash": payload_hash,
            "outcome": outcome,
            "runId": run_id,
            "recordedAt": Utc::now().to_rfc3339(),
        });
        let bytes = serde_json::to_vec_pretty(&record)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.inner.tombstones.write_private(&name, &bytes)
    }

    /// The recorded decision for a request identity, if any.
    pub fn load_idempotency_tombstone(
        &self,
        request_id: &str,
    ) -> Result<Option<IdempotencyTombstone>, OrchError> {
        let name = Self::record_name(request_id)?;
        let _guard = self.inner.lock.lock();
        let Some(text) = self.inner.tombstones.read_private(&name)? else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Conflict,
                format!("idempotency tombstone is unreadable: {error}"),
            )
        })?;
        let field = |key: &str| -> Option<String> {
            value.get(key).and_then(|v| v.as_str()).map(str::to_string)
        };
        let (Some(recorded_request), Some(tool), Some(payload_hash), Some(outcome)) = (
            field("requestId"),
            field("tool"),
            field("payloadHash"),
            field("outcome"),
        ) else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "idempotency tombstone is missing required fields",
            ));
        };
        if recorded_request != request_id {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "idempotency tombstone does not belong to this request",
            ));
        }
        Ok(Some(IdempotencyTombstone {
            request_id: recorded_request,
            tool,
            payload_hash,
            outcome,
            run_id: field("runId"),
        }))
    }

    pub fn list_idempotency_tombstones(&self) -> Result<Vec<String>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.inner.tombstones.list("json")
    }

    /// Leave a bounded, durable, recoverable finalization intent without
    /// installing it.
    ///
    /// This is the last-resort path for an outer supervisor that is being torn
    /// down (panic, abort, shutdown) or that cannot install its terminal
    /// record. The intent is replayed by [`OrchStore::open`], so the run is
    /// terminalized on the next start even if this process never gets another
    /// chance to write.
    pub fn stage_finalization_intent(&self, candidate: &RunRecord) -> anyhow::Result<()> {
        if !candidate.state.is_terminal() {
            anyhow::bail!("finalization candidate must be terminal");
        }
        let _guard = self.inner.lock.lock();
        let intent_path = self
            .finalization_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let result = atomic_write_json(&intent_path, candidate);
        *self.inner.last_run_error.lock() = result.as_ref().err().map(ToString::to_string);
        result
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
        // A decision that outlived its receipt is still a decision. Checked
        // before the claim so a pruned request cannot be re-performed simply
        // because its receipt aged out.
        if let Some(tombstone) = self.load_idempotency_tombstone(request_id)? {
            if !path.is_file() {
                if tombstone.tool != tool || tombstone.payload_hash != payload_hash {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "request_id reused with different payload",
                    ));
                }
                return Err(OrchError::with_data(
                    OrchErrorCode::Conflict,
                    "request_id was already decided and its receipt has been retired",
                    serde_json::json!({
                        "requestId": tombstone.request_id,
                        "outcome": tombstone.outcome,
                        "runId": tombstone.run_id,
                    }),
                ));
            }
        }
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
            spec_key: None,
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

    #[allow(clippy::too_many_arguments)]
    pub fn complete_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        run_id: Option<String>,
        spec_key: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        self.finish_idempotency(
            tool,
            request_id,
            payload_hash,
            run_id,
            spec_key,
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
            None,
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
        spec_key: Option<String>,
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
        let run_id_for_tombstone = run_id.clone();
        let receipt = IdempotencyReceipt {
            request_id: request_id.into(),
            payload_hash: payload_hash.into(),
            run_id,
            spec_key,
            tool: tool.into(),
            response,
            error,
            created_at: Utc::now(),
            status: status.into(),
        };
        atomic_write_json(&path, &receipt)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?;
        // The tombstone outlives the receipt. Retention prunes receipts; if
        // the decision went with them, waiting out the horizon would turn a
        // refused request back into an executable one.
        drop(_g);
        self.write_idempotency_tombstone(
            request_id,
            tool,
            payload_hash,
            status,
            run_id_for_tombstone.as_deref(),
        )
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

    /// Recover the run ledger after a restart.
    ///
    /// * `Running` runs are always terminalized `Interrupted`. Model work is
    ///   never resumed implicitly: the transcript, the tool state, and the
    ///   provider stream are all gone, so continuing would be a guess. An
    ///   explicit `ptah_retry_run` is the only way to carry that work forward.
    /// * `Queued` runs survive **only** when their admission is provably
    ///   complete: a sealed, verifying [`AcceptanceIntent`] on disk *and* a
    ///   `complete` idempotency receipt for the same request. That is exactly
    ///   the state the accept path guarantees before it answers the caller,
    ///   so a queued task that was truly accepted is still executed, exactly
    ///   once, after any number of restarts.
    /// * Every other `Queued` run is tombstoned `Interrupted` with
    ///   `admission_lost`. A crash between the durable input and the receipt
    ///   therefore never executes, and a failed request is never resurrected.
    ///
    /// Returns the number of runs terminalized by this sweep.
    pub fn mark_unfinished_interrupted(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        let mut interrupted_agents = Vec::new();
        for mut run in self.list_runs()? {
            let tombstone_code = match run.state {
                RunState::Running => Some("interrupted"),
                RunState::Queued => match self.queued_admission_disposition(&run) {
                    QueuedAdmission::Recoverable => None,
                    QueuedAdmission::Lost => Some("admission_lost"),
                    QueuedAdmission::Tampered => Some("admission_tampered"),
                },
                _ => continue,
            };
            let Some(error_code) = tombstone_code else {
                // Keep the durable input; the service re-admits from it.
                continue;
            };
            run.state = RunState::Interrupted;
            run.queue_position = None;
            run.updated_at = Utc::now();
            run.terminal_result = Some("interrupted".into());
            run.error_code = Some(error_code.into());
            if let Some(execution) = run.execution.as_mut() {
                execution.promotion_state = PromotionState::Conflicted;
            }
            self.save_run(&run)?;
            // A tombstoned admission must never keep executable input around.
            let _ = self.remove_acceptance_intent(&run.run_id);
            if let Some(agent_id) = run.agent_id.clone() {
                interrupted_agents.push((agent_id, run.run_id.clone()));
            }
            n += 1;
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

    /// A queued admission may only survive a restart when both halves of the
    /// accept cut are durably present: the sealed input **and** the completed
    /// receipt that promised the caller it would run.
    fn queued_admission_disposition(&self, run: &RunRecord) -> QueuedAdmission {
        let intent = match self.load_acceptance_intent(&run.run_id) {
            Ok(Some(intent)) => intent,
            Ok(None) => return QueuedAdmission::Lost,
            // Present but not what was accepted: a parseable tamper, a widened
            // permission, a symlink. Never runs, and is named as such.
            Err(_) => return QueuedAdmission::Tampered,
        };
        if intent.request_id != run.request_id
            || intent.session_id != run.session_id
            || intent.workspace != run.workspace
        {
            return QueuedAdmission::Tampered;
        }
        // The run names one execution specification. Input that verifies as
        // some *other* specification — a resealed forgery — is not this run's
        // input, however internally consistent it is.
        if run.spec_key.as_deref() != Some(intent.spec_key()) {
            return QueuedAdmission::Tampered;
        }
        let receipt = match self.load_idempotency(&run.request_id) {
            Ok(Some(receipt)) => receipt,
            Ok(None) => return QueuedAdmission::Lost,
            Err(_) => return QueuedAdmission::Tampered,
        };
        // An explicit failure, or a claim that never settled, can never
        // become execution. Only a completed receipt admits work.
        if receipt.status != "complete" || receipt.error.is_some() {
            return QueuedAdmission::Lost;
        }
        if receipt.payload_hash != intent.payload_hash || receipt.tool != intent.tool {
            return QueuedAdmission::Tampered;
        }
        // The receipt must name this exact run; a receipt for other work can
        // never be used to justify running this record.
        if receipt.run_id.as_deref() != Some(run.run_id.as_str()) {
            return QueuedAdmission::Tampered;
        }
        QueuedAdmission::Recoverable
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

    /// Release every attempt lease left behind by a previous instance.
    ///
    /// Safe only because [`OrchStore::open`] holds the store's exclusive
    /// advisory lock: no other process can be executing these attempts. A
    /// lease that cannot be parsed or verified is removed outright — a
    /// tampered lease must not be able to block a run forever, and it cannot
    /// authorize one either, since dispatch always mints a fresh lease.
    fn release_orphaned_attempt_leases(&self) -> anyhow::Result<usize> {
        let leases = self
            .list_attempt_leases()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut released = 0;
        for (name, lease) in leases {
            let Some(lease) = lease else {
                // A lease that cannot be parsed or verified must not be able
                // to block a run forever, and cannot authorize one either:
                // dispatch always mints a fresh lease.
                let _ = self.inner.leases.remove(&name);
                continue;
            };
            if lease.state == AttemptLeaseState::Released {
                continue;
            }
            let released_lease = AttemptLease {
                state: AttemptLeaseState::Released,
                heartbeat_at: Utc::now(),
                digest: String::new(),
                ..lease
            }
            .seal();
            self.write_attempt_lease_unlocked(&name, &released_lease)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            released += 1;
        }
        Ok(released)
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

/// Why a queued admission may or may not survive a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedAdmission {
    /// Sealed input and a completed receipt naming this exact run both exist.
    Recoverable,
    /// The admission never completed, so it must never execute.
    Lost,
    /// Durable evidence exists but is not what was accepted.
    Tampered,
}

/// A durable record that one request identity has already been decided.
/// Survives receipt retention, so a pruned failure cannot become a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyTombstone {
    pub request_id: String,
    pub tool: String,
    pub payload_hash: String,
    /// `complete` or `failed` — the decision that was reached.
    pub outcome: String,
    pub run_id: Option<String>,
}

pub enum IdempotencyClaim {
    Perform,
    Pending,
    Replay(Result<serde_json::Value, OrchError>),
}

/// Owner-only permissions for the ledger directories. Records themselves are
/// written through `LedgerDir`, which owns the file-level guarantees.
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

fn create_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    }
    Ok(())
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
            spec_key: None,
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
            spec_key: None,
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
            spec_key: None,
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
            .complete_idempotency("t", "req", "h", None, None, serde_json::json!({"ok": true}))
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
            .complete_idempotency(
                "t",
                "failed",
                "h",
                None,
                None,
                serde_json::json!({"ok": true})
            )
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
                spec_key: None,
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
                spec_key: None,
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
            spec_key: None,
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
