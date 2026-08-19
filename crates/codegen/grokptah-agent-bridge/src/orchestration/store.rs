//! Durable run records, idempotency receipts, audit log (#196).

use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

use anyhow::Context;
use chrono::{Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use uuid::Uuid;

use super::routine::{
    advance_next_fire, decide_lifecycle_skip, due_occurrences, in_flight_count,
    occurrence_dedupe_key, validate_activation_payload, ActivationCause, ActivationDisposition,
    ActivationRecord, ActivationRequest, CapturedActivationPolicy, RoutineFireReport,
    RoutineLifecycle, RoutineRecord, RoutineSnapshot, MAX_ACTIVATION_HISTORY,
    ROUTINE_SCHEMA_VERSION,
};
use super::types::{
    safe_id_filename, AgentRecord, AgentSpec, AgentState, AuditEntry, ContinuationCheckpoint,
    IdempotencyReceipt, OrchError, OrchErrorCode, PromotionState, RunBounds, RunRecord, RunState,
    RunStopCause,
};
use super::workload::{
    lease_duration, AttemptState, WorkApproval, WorkAttempt, WorkClaim, WorkItem, WorkProgress,
    WorkResult, WorkState, WorkloadReconciliationReport,
};
use super::{assemble_continuation_context, ContinuationContext, ContinuationInputSnapshot};

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentActivationIntent {
    run: RunRecord,
    activated_agent: AgentRecord,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutineDedupeRecord {
    dedupe_key: String,
    activation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    work_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disposition: Option<ActivationDisposition>,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoutineFireIntent {
    routine: RoutineRecord,
    activation: ActivationRecord,
    work: Option<WorkItem>,
    dedupe: Option<RoutineDedupeRecord>,
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
        fs::create_dir_all(root.join("agent-specs"))?;
        fs::create_dir_all(root.join("checkpoints"))?;
        fs::create_dir_all(root.join("continuation-inputs"))?;
        fs::create_dir_all(root.join("continuation-contexts"))?;
        fs::create_dir_all(root.join("agent-activation"))?;
        fs::create_dir_all(root.join("idempotency"))?;
        fs::create_dir_all(root.join("audit"))?;
        fs::create_dir_all(root.join("finalization"))?;
        fs::create_dir_all(root.join("work-items"))?;
        fs::create_dir_all(root.join("work-attempts"))?;
        fs::create_dir_all(root.join("routines"))?;
        fs::create_dir_all(root.join("routine-activations"))?;
        fs::create_dir_all(root.join("routine-dedupe"))?;
        fs::create_dir_all(root.join("routine-intents"))?;
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
        store.recover_agent_activation_intents()?;
        store.recover_finalization_intents()?;
        store.recover_routine_intents()?;
        store.mark_unfinished_interrupted()?;
        store.fail_orphaned_idempotency_claims()?;
        store.reconcile_workloads()?;
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

    fn agent_spec_path(&self, agent_id: &str, revision: u64) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(agent_id)?;
        if revision == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "agent specification revision must be greater than zero",
            ));
        }
        Ok(self
            .inner
            .root
            .join("agent-specs")
            .join(safe)
            .join(format!("{revision}.json")))
    }

    fn checkpoint_path(&self, checkpoint_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(checkpoint_id)?;
        Ok(self
            .inner
            .root
            .join("checkpoints")
            .join(format!("{safe}.json")))
    }

    fn continuation_input_path(&self, input_hash: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(input_hash)?;
        Ok(self
            .inner
            .root
            .join("continuation-inputs")
            .join(format!("{safe}.json")))
    }

    fn continuation_context_path(&self, context_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(context_id)?;
        Ok(self
            .inner
            .root
            .join("continuation-contexts")
            .join(format!("{safe}.json")))
    }

    fn agent_activation_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("agent-activation")
            .join(format!("{safe}.json")))
    }

    fn work_item_path(&self, work_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(work_id)?;
        Ok(self
            .inner
            .root
            .join("work-items")
            .join(format!("{safe}.json")))
    }

    fn work_attempt_path(&self, attempt_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(attempt_id)?;
        Ok(self
            .inner
            .root
            .join("work-attempts")
            .join(format!("{safe}.json")))
    }

    fn routine_path(&self, routine_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(routine_id)?;
        Ok(self
            .inner
            .root
            .join("routines")
            .join(format!("{safe}.json")))
    }

    fn routine_activation_path(
        &self,
        routine_id: &str,
        activation_id: &str,
    ) -> Result<PathBuf, OrchError> {
        let routine_safe = safe_id_filename(routine_id)?;
        let activation_safe = safe_id_filename(activation_id)?;
        Ok(self
            .inner
            .root
            .join("routine-activations")
            .join(routine_safe)
            .join(format!("{activation_safe}.json")))
    }

    fn routine_dedupe_path(
        &self,
        routine_id: &str,
        dedupe_key: &str,
    ) -> Result<PathBuf, OrchError> {
        let routine_safe = safe_id_filename(routine_id)?;
        let key_safe = safe_id_filename(dedupe_key)?;
        Ok(self
            .inner
            .root
            .join("routine-dedupe")
            .join(routine_safe)
            .join(format!("{key_safe}.json")))
    }

    fn routine_intent_path(&self, activation_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(activation_id)?;
        Ok(self
            .inner
            .root
            .join("routine-intents")
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

    /// Serialize Agent activation with durable Run creation so two Lanes
    /// cannot both pass the active-Run check and execute under one identity.
    pub fn save_run_and_activate_agent(
        &self,
        run: &RunRecord,
        agent_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            run.agent_id.as_deref() == Some(agent_id),
            "Run Agent identity does not match activation target"
        );
        let _g = self.inner.lock.lock();
        let activation_dir = self.inner.root.join("agent-activation");
        if fs::read_dir(&activation_dir)?.any(|entry| {
            entry.ok().is_some_and(|entry| {
                entry.path().extension().and_then(|v| v.to_str()) == Some("json")
            })
        }) {
            anyhow::bail!("a prior Agent activation requires restart recovery");
        }
        let agent_path = self
            .agent_path(agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(agent_path.is_file(), "persistent Agent record is missing");
        let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&agent_path)?)?;
        agent
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            !(agent.state == AgentState::Active && agent.current_run_id.is_some()),
            "persistent Agent already has an active Run"
        );
        anyhow::ensure!(
            agent.known_lane_ids().contains(&run.session_id),
            "Run Lane is not currently associated with the persistent Agent"
        );
        let run_path = self
            .run_path(&run.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(!run_path.is_file(), "Run ID already exists");
        agent.state = AgentState::Active;
        agent.current_run_id = Some(run.run_id.clone());
        agent.last_lane_id = Some(run.session_id);
        agent.updated_at = Utc::now();
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let intent_path = self
            .agent_activation_path(&run.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let intent = AgentActivationIntent {
            run: run.clone(),
            activated_agent: agent.clone(),
        };
        atomic_write_json(&intent_path, &intent)?;
        if let Err(error) = atomic_write_json(&run_path, run) {
            let run_rollback = match fs::symlink_metadata(&run_path) {
                Ok(_) => remove_file_durable(&run_path),
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(())
                }
                Err(metadata_error) => Err(metadata_error.into()),
            };
            if run_rollback.is_err() || remove_file_durable(&intent_path).is_err() {
                return Err(error.context(
                    "persist Agent activation Run; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent activation Run"));
        }
        if let Err(error) = atomic_write_json(&agent_path, &agent) {
            if remove_file_durable(&run_path).is_err() {
                return Err(error.context(
                    "persist Agent activation; durable recovery intent requires restart",
                ));
            }
            if remove_file_durable(&intent_path).is_err() {
                return Err(error.context(
                    "persist Agent activation; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent activation"));
        }
        remove_file_durable(&intent_path).context("commit Agent activation recovery intent")?;
        Ok(())
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

    // --- Durable workloads -------------------------------------------------

    fn load_work_item_unlocked(&self, work_id: &str) -> anyhow::Result<Option<WorkItem>> {
        let path = match self.work_item_path(work_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let item: WorkItem = serde_json::from_str(&fs::read_to_string(path)?)?;
        item.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(item))
    }

    fn save_work_item_unlocked(&self, item: &WorkItem) -> anyhow::Result<()> {
        item.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let path = self
            .work_item_path(&item.work_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&path, item)
    }

    fn load_work_attempt_unlocked(&self, attempt_id: &str) -> anyhow::Result<Option<WorkAttempt>> {
        let path = match self.work_attempt_path(attempt_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let attempt: WorkAttempt = serde_json::from_str(&fs::read_to_string(path)?)?;
        attempt
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(attempt))
    }

    fn save_work_attempt_unlocked(&self, attempt: &WorkAttempt) -> anyhow::Result<()> {
        attempt
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let path = self
            .work_attempt_path(&attempt.attempt_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&path, attempt)
    }

    fn list_work_items_unlocked(&self) -> anyhow::Result<Vec<WorkItem>> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("work-items");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let item: WorkItem = serde_json::from_str(&fs::read_to_string(path)?)?;
            item.validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            out.push(item);
        }
        out.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        Ok(out)
    }

    fn list_work_attempts_unlocked(
        &self,
        work_id: Option<&str>,
    ) -> anyhow::Result<Vec<WorkAttempt>> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("work-attempts");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let attempt: WorkAttempt = serde_json::from_str(&fs::read_to_string(path)?)?;
            attempt
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if work_id.is_none_or(|id| id == attempt.work_id) {
                out.push(attempt);
            }
        }
        out.sort_by(|a, b| a.attempt_number.cmp(&b.attempt_number));
        Ok(out)
    }

    pub fn save_work_item(&self, item: &WorkItem) -> anyhow::Result<()> {
        let _guard = self.inner.lock.lock();
        self.save_work_item_unlocked(item)
    }

    pub fn load_work_item(&self, work_id: &str) -> anyhow::Result<Option<WorkItem>> {
        let _guard = self.inner.lock.lock();
        self.load_work_item_unlocked(work_id)
    }

    pub fn list_work_items(&self) -> anyhow::Result<Vec<WorkItem>> {
        let _guard = self.inner.lock.lock();
        self.list_work_items_unlocked()
    }

    pub fn list_work_attempts(&self, work_id: Option<&str>) -> anyhow::Result<Vec<WorkAttempt>> {
        let _guard = self.inner.lock.lock();
        self.list_work_attempts_unlocked(work_id)
    }

    // --- Durable routines -------------------------------------------------

    pub fn save_routine(&self, routine: &RoutineRecord) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        self.save_routine_unlocked(routine)
    }

    pub fn load_routine(&self, routine_id: &str) -> Result<Option<RoutineRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_routine_unlocked(routine_id)
    }

    pub fn list_routines(&self) -> Result<Vec<RoutineRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.list_routines_unlocked()
    }

    pub fn list_activations(
        &self,
        routine_id: &str,
        limit: usize,
    ) -> Result<Vec<ActivationRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.list_activations_unlocked(routine_id, limit)
    }

    pub fn load_activation(
        &self,
        routine_id: &str,
        activation_id: &str,
    ) -> Result<Option<ActivationRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_activation_unlocked(routine_id, activation_id)
    }

    pub fn routine_snapshot(
        &self,
        routine_id: &str,
        history_limit: usize,
    ) -> Result<Option<RoutineSnapshot>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(routine) = self.load_routine_unlocked(routine_id)? else {
            return Ok(None);
        };
        let activations = self.list_activations_unlocked(routine_id, history_limit)?;
        Ok(Some(RoutineSnapshot {
            routine,
            activations,
        }))
    }

    pub fn set_routine_lifecycle(
        &self,
        routine_id: &str,
        lifecycle: RoutineLifecycle,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<RoutineRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut routine = self
            .load_routine_unlocked(routine_id)?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown routine_id"))?;
        if expected_revision.is_some_and(|expected| expected != routine.revision) {
            return Err(OrchError::new(
                OrchErrorCode::StaleVersion,
                "routine revision does not match expected_revision",
            ));
        }
        routine.lifecycle = lifecycle;
        if matches!(lifecycle, RoutineLifecycle::Enabled) {
            routine.circuit_open = false;
            routine.consecutive_failures = 0;
            if routine.next_fire_at.is_none() {
                routine.next_fire_at = routine.trigger.initial_next_fire(now)?;
            }
        }
        if matches!(lifecycle, RoutineLifecycle::Disabled) {
            routine.next_fire_at = None;
        }
        routine.bump_at(now);
        self.save_routine_unlocked(&routine)?;
        Ok(routine)
    }

    pub fn activate_routine(
        &self,
        routine_id: &str,
        request: ActivationRequest,
        server_ceiling: &RunBounds,
        now: chrono::DateTime<Utc>,
    ) -> Result<ActivationRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        self.activate_routine_unlocked(routine_id, request, server_ceiling, now)
    }

    pub fn fire_due_routines_at(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Result<RoutineFireReport, OrchError> {
        let _guard = self.inner.lock.lock();
        let routines = self.list_routines_unlocked()?;
        let ceiling = RunBounds::default();
        let mut report = RoutineFireReport {
            scanned_routines: routines.len(),
            ..RoutineFireReport::default()
        };
        for routine in routines {
            let due = match due_occurrences(&routine, now) {
                Ok(due) => due,
                Err(_) => {
                    report.rejected += 1;
                    continue;
                }
            };
            if due.is_empty() {
                if routine.next_fire_at.is_some_and(|next| next <= now) {
                    if let Ok(Some(mut current)) = self.load_routine_unlocked(&routine.routine_id) {
                        current.next_fire_at =
                            advance_next_fire(&current.trigger, now, &[]).unwrap_or(None);
                        current.updated_at = now;
                        let _ = self.save_routine_unlocked(&current);
                    }
                    report.skipped += 1;
                }
                continue;
            }
            for scheduled_at in &due {
                let request = ActivationRequest {
                    cause: ActivationCause::Scheduled,
                    dedupe_key: occurrence_dedupe_key(&routine.routine_id, *scheduled_at),
                    scheduled_at: *scheduled_at,
                    received_at: now,
                    payload: None,
                    created_by: format!("routine:{}", routine.routine_id),
                };
                match self.activate_routine_unlocked(&routine.routine_id, request, &ceiling, now) {
                    Ok(activation) => match activation.disposition {
                        ActivationDisposition::CreatedWork => report.created_work += 1,
                        ActivationDisposition::Deduplicated => report.deduplicated += 1,
                        ActivationDisposition::Rejected
                        | ActivationDisposition::Backoff
                        | ActivationDisposition::CircuitOpen => report.rejected += 1,
                        _ => report.skipped += 1,
                    },
                    Err(_) => report.rejected += 1,
                }
            }
            if let Ok(Some(mut current)) = self.load_routine_unlocked(&routine.routine_id) {
                if current.lifecycle.allows_scheduled_fire() && !current.circuit_open {
                    current.next_fire_at =
                        advance_next_fire(&current.trigger, now, &due).unwrap_or(None);
                    current.last_fire_at = Some(now);
                    current.updated_at = now;
                    let _ = self.save_routine_unlocked(&current);
                }
            }
        }
        Ok(report)
    }

    fn recover_routine_intents(&self) -> anyhow::Result<()> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("routine-intents");
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: RoutineFireIntent = serde_json::from_str(&fs::read_to_string(&path)?)?;
            self.commit_routine_intent_unlocked(&intent)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            remove_file_durable(&path)?;
        }
        Ok(())
    }

    fn save_routine_unlocked(&self, routine: &RoutineRecord) -> Result<(), OrchError> {
        routine.validate()?;
        let path = self.routine_path(&routine.routine_id)?;
        atomic_write_json(&path, routine)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn load_routine_unlocked(&self, routine_id: &str) -> Result<Option<RoutineRecord>, OrchError> {
        let path = match self.routine_path(routine_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let routine: RoutineRecord = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        routine.validate()?;
        Ok(Some(routine))
    }

    fn list_routines_unlocked(&self) -> Result<Vec<RoutineRecord>, OrchError> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("routines");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let routine: RoutineRecord = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            routine.validate()?;
            out.push(routine);
        }
        out.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(out)
    }

    fn load_activation_unlocked(
        &self,
        routine_id: &str,
        activation_id: &str,
    ) -> Result<Option<ActivationRecord>, OrchError> {
        let path = match self.routine_activation_path(routine_id, activation_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let activation: ActivationRecord = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        activation.validate()?;
        Ok(Some(activation))
    }

    fn list_activations_unlocked(
        &self,
        routine_id: &str,
        limit: usize,
    ) -> Result<Vec<ActivationRecord>, OrchError> {
        let mut out = Vec::new();
        let dir = self
            .inner
            .root
            .join("routine-activations")
            .join(safe_id_filename(routine_id)?);
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let activation: ActivationRecord = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            activation.validate()?;
            out.push(activation);
        }
        out.sort_by(|left, right| right.fired_at.cmp(&left.fired_at));
        if limit > 0 && out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    fn load_dedupe_unlocked(
        &self,
        routine_id: &str,
        dedupe_key: &str,
    ) -> Result<Option<RoutineDedupeRecord>, OrchError> {
        let path = self.routine_dedupe_path(routine_id, dedupe_key)?;
        if !path.is_file() {
            return Ok(None);
        }
        let record: RoutineDedupeRecord = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(Some(record))
    }

    fn replay_dedupe_unlocked(
        &self,
        routine: &RoutineRecord,
        request: &ActivationRequest,
        record: &RoutineDedupeRecord,
        now: chrono::DateTime<Utc>,
    ) -> Result<ActivationRecord, OrchError> {
        if let Some(existing) =
            self.load_activation_unlocked(&routine.routine_id, &record.activation_id)?
        {
            return Ok(existing);
        }
        // History may have been pruned; the dedupe receipt still forbids a
        // second Work item for this occurrence.
        Ok(ActivationRecord {
            schema_version: ROUTINE_SCHEMA_VERSION,
            activation_id: record.activation_id.clone(),
            routine_id: routine.routine_id.clone(),
            routine_revision: routine.revision,
            trigger_kind: request.cause.as_str().to_string(),
            dedupe_key: request.dedupe_key.clone(),
            scheduled_at: request.scheduled_at,
            received_at: request.received_at,
            fired_at: now,
            work_id: record.work_id.clone(),
            disposition: ActivationDisposition::Deduplicated,
            error: None,
            captured_policy: CapturedActivationPolicy::capture(
                routine.revision,
                Some(&routine.agent_id),
                None,
                &routine.work_template,
                &RunBounds::default(),
            )
            .unwrap_or_else(|_| CapturedActivationPolicy {
                routine_revision: routine.revision,
                agent_id: Some(routine.agent_id.clone()),
                agent_spec_revision: None,
                run_bounds: routine.work_template.policy.bounds.clone(),
                work_policy: routine.work_template.policy.clone(),
                sandbox_profile: "workspace-write".into(),
                computer_use_allowed: false,
                auto_approve_tools: false,
            }),
            payload: None,
            created_by: request.created_by.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn activate_routine_unlocked(
        &self,
        routine_id: &str,
        request: ActivationRequest,
        server_ceiling: &RunBounds,
        now: chrono::DateTime<Utc>,
    ) -> Result<ActivationRecord, OrchError> {
        let mut routine = self
            .load_routine_unlocked(routine_id)?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown routine_id"))?;
        if let Some(existing) = self.load_dedupe_unlocked(routine_id, &request.dedupe_key)? {
            return self.replay_dedupe_unlocked(&routine, &request, &existing, now);
        }
        let payload = validate_activation_payload(request.payload.as_ref())?;
        if matches!(
            request.cause,
            ActivationCause::External | ActivationCause::Scheduled
        ) {
            if let Some(expires) = match routine.trigger {
                super::routine::RoutineTrigger::OneShot { expires_at, .. } => expires_at,
                _ => None,
            } {
                if expires < now {
                    return self.record_non_work_activation(
                        &routine,
                        &request,
                        payload,
                        ActivationDisposition::SkippedExpired,
                        Some("activation expired before it could create work"),
                        server_ceiling,
                        now,
                    );
                }
            }
        }
        if request.cause == ActivationCause::External {
            return Err(OrchError::new(
                OrchErrorCode::Unsupported,
                "webhook, GitHub, and message adapters are reserved for a later slice",
            ));
        }
        if let Some(disposition) = decide_lifecycle_skip(&routine, request.cause) {
            return self.record_non_work_activation(
                &routine,
                &request,
                payload,
                disposition,
                None,
                server_ceiling,
                now,
            );
        }
        let inflight = in_flight_count(
            &self
                .list_work_items_unlocked()
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            routine_id,
        );
        if inflight >= routine.concurrency.max_in_flight as usize {
            return self.record_non_work_activation(
                &routine,
                &request,
                payload,
                ActivationDisposition::SkippedOverlap,
                Some("routine already has the maximum in-flight Work items"),
                server_ceiling,
                now,
            );
        }
        let agent = self
            .load_agent_unlocked(&routine.agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let spec = match &agent {
            Some(agent) => agent.spec.clone(),
            None => {
                return self.fail_activation_unlocked(
                    &mut routine,
                    &request,
                    payload,
                    "owning agent is unknown",
                    server_ceiling,
                    now,
                );
            }
        };
        if let Some(agent) = &agent {
            if !workspaces_match(&agent.workspace, &routine.workspace) {
                return self.fail_activation_unlocked(
                    &mut routine,
                    &request,
                    payload,
                    "agent source workspace does not match the routine workspace",
                    server_ceiling,
                    now,
                );
            }
        }
        let captured = match CapturedActivationPolicy::capture(
            routine.revision,
            Some(&routine.agent_id),
            spec.as_ref(),
            &routine.work_template,
            server_ceiling,
        ) {
            Ok(captured) => captured,
            Err(error) => {
                return self.fail_activation_unlocked(
                    &mut routine,
                    &request,
                    payload,
                    &error.message,
                    server_ceiling,
                    now,
                );
            }
        };
        let mut work = match WorkItem::new_at(
            routine.work_template.kind.clone(),
            routine.work_template.objective.clone(),
            routine.session_id,
            routine.workspace.clone(),
            format!("routine:{}", routine.routine_id),
            captured.work_policy.clone(),
            now,
        ) {
            Ok(work) => work,
            Err(error) => {
                return self.fail_activation_unlocked(
                    &mut routine,
                    &request,
                    payload,
                    &error.message,
                    server_ceiling,
                    now,
                );
            }
        };
        work.priority = routine.work_template.priority;
        work.assigned_agent_id = Some(routine.agent_id.clone());
        work.source_routine_id = Some(routine.routine_id.clone());
        let activation_id = Uuid::new_v4().to_string();
        work.source_activation_id = Some(activation_id.clone());
        work.validate()
            .map_err(|error| OrchError::new(OrchErrorCode::InvalidRequest, error.to_string()))?;
        let activation = ActivationRecord {
            schema_version: ROUTINE_SCHEMA_VERSION,
            activation_id: activation_id.clone(),
            routine_id: routine.routine_id.clone(),
            routine_revision: routine.revision,
            trigger_kind: request.cause.as_str().to_string(),
            dedupe_key: request.dedupe_key.clone(),
            scheduled_at: request.scheduled_at,
            received_at: request.received_at,
            fired_at: now,
            work_id: Some(work.work_id.clone()),
            disposition: ActivationDisposition::CreatedWork,
            error: None,
            captured_policy: captured,
            payload,
            created_by: request.created_by.clone(),
        };
        activation.validate()?;
        let intent = RoutineFireIntent {
            routine: routine.clone(),
            activation: activation.clone(),
            work: Some(work.clone()),
            dedupe: Some(RoutineDedupeRecord {
                dedupe_key: request.dedupe_key.clone(),
                activation_id: activation_id.clone(),
                work_id: Some(work.work_id.clone()),
                disposition: Some(ActivationDisposition::CreatedWork),
                created_at: now,
            }),
        };
        self.persist_routine_intent_unlocked(&intent)?;
        self.commit_routine_intent_unlocked(&intent)?;
        self.clear_routine_intent_unlocked(&activation_id)?;
        self.prune_activations_unlocked(routine_id)?;
        Ok(activation)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_non_work_activation(
        &self,
        routine: &RoutineRecord,
        request: &ActivationRequest,
        payload: Option<serde_json::Value>,
        disposition: ActivationDisposition,
        error: Option<&str>,
        server_ceiling: &RunBounds,
        now: chrono::DateTime<Utc>,
    ) -> Result<ActivationRecord, OrchError> {
        let captured = CapturedActivationPolicy::capture(
            routine.revision,
            Some(&routine.agent_id),
            None,
            &routine.work_template,
            server_ceiling,
        )
        .unwrap_or_else(|_| CapturedActivationPolicy {
            routine_revision: routine.revision,
            agent_id: Some(routine.agent_id.clone()),
            agent_spec_revision: None,
            run_bounds: routine.work_template.policy.bounds.clone(),
            work_policy: routine.work_template.policy.clone(),
            sandbox_profile: "workspace-write".into(),
            computer_use_allowed: false,
            auto_approve_tools: false,
        });
        let activation = ActivationRecord {
            schema_version: ROUTINE_SCHEMA_VERSION,
            activation_id: Uuid::new_v4().to_string(),
            routine_id: routine.routine_id.clone(),
            routine_revision: routine.revision,
            trigger_kind: request.cause.as_str().to_string(),
            dedupe_key: request.dedupe_key.clone(),
            scheduled_at: request.scheduled_at,
            received_at: request.received_at,
            fired_at: now,
            work_id: None,
            disposition,
            error: error.map(str::to_string),
            captured_policy: captured,
            payload,
            created_by: request.created_by.clone(),
        };
        activation.validate()?;
        let intent = RoutineFireIntent {
            routine: routine.clone(),
            activation: activation.clone(),
            work: None,
            dedupe: Some(RoutineDedupeRecord {
                dedupe_key: request.dedupe_key.clone(),
                activation_id: activation.activation_id.clone(),
                work_id: None,
                disposition: Some(disposition),
                created_at: now,
            }),
        };
        self.persist_routine_intent_unlocked(&intent)?;
        self.commit_routine_intent_unlocked(&intent)?;
        self.clear_routine_intent_unlocked(&activation.activation_id)?;
        self.prune_activations_unlocked(&routine.routine_id)?;
        Ok(activation)
    }

    #[allow(clippy::too_many_arguments)]
    fn fail_activation_unlocked(
        &self,
        routine: &mut RoutineRecord,
        request: &ActivationRequest,
        payload: Option<serde_json::Value>,
        message: &str,
        server_ceiling: &RunBounds,
        now: chrono::DateTime<Utc>,
    ) -> Result<ActivationRecord, OrchError> {
        routine.consecutive_failures = routine.consecutive_failures.saturating_add(1);
        if routine.consecutive_failures >= routine.retry.circuit_failures {
            routine.circuit_open = true;
            routine.lifecycle = RoutineLifecycle::Paused;
            routine.next_fire_at = None;
        } else {
            routine.next_fire_at =
                Some(now + routine.retry.delay_after(routine.consecutive_failures));
        }
        routine.updated_at = now;
        self.save_routine_unlocked(routine)?;
        self.record_non_work_activation(
            routine,
            request,
            payload,
            if routine.circuit_open {
                ActivationDisposition::CircuitOpen
            } else {
                ActivationDisposition::Backoff
            },
            Some(message),
            server_ceiling,
            now,
        )
    }

    fn persist_routine_intent_unlocked(&self, intent: &RoutineFireIntent) -> Result<(), OrchError> {
        let path = self.routine_intent_path(&intent.activation.activation_id)?;
        atomic_write_json(&path, intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn commit_routine_intent_unlocked(&self, intent: &RoutineFireIntent) -> Result<(), OrchError> {
        if let Some(work) = &intent.work {
            self.save_work_item_unlocked(work)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        let path = self.routine_activation_path(
            &intent.activation.routine_id,
            &intent.activation.activation_id,
        )?;
        atomic_write_json(&path, &intent.activation)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if let Some(dedupe) = &intent.dedupe {
            let dedupe_path =
                self.routine_dedupe_path(&intent.activation.routine_id, &dedupe.dedupe_key)?;
            atomic_write_json(&dedupe_path, dedupe)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        Ok(())
    }

    fn clear_routine_intent_unlocked(&self, activation_id: &str) -> Result<(), OrchError> {
        let path = self.routine_intent_path(activation_id)?;
        remove_file_durable(&path)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn prune_activations_unlocked(&self, routine_id: &str) -> Result<(), OrchError> {
        let activations = self.list_activations_unlocked(routine_id, usize::MAX)?;
        if activations.len() <= MAX_ACTIVATION_HISTORY {
            return Ok(());
        }
        for activation in activations.into_iter().skip(MAX_ACTIVATION_HISTORY) {
            let path = self.routine_activation_path(routine_id, &activation.activation_id)?;
            let _ = remove_file_durable(&path);
        }
        // Dedupe receipts outlive activation history so a late replay cannot
        // mint a second Work item after the inspectable history window.
        Ok(())
    }

    fn work_failure_result(summary: &str, now: chrono::DateTime<Utc>) -> WorkResult {
        WorkResult {
            summary: summary.into(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            failure: Some(summary.into()),
            cancellation_reason: None,
            completed_at: now,
        }
    }

    fn refresh_work_item_unlocked(&self, item: &mut WorkItem) -> anyhow::Result<()> {
        self.refresh_work_item_at_unlocked(item, Utc::now())
    }

    fn refresh_work_item_at_unlocked(
        &self,
        item: &mut WorkItem,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if item.state.is_terminal() {
            return Ok(());
        }
        let dependencies_ready = item.dependencies.iter().all(|dependency| {
            self.load_work_item_unlocked(&dependency.work_id)
                .ok()
                .flatten()
                .is_some_and(|dependency_item| dependency_item.state == dependency.required_state)
        });
        if !dependencies_ready && matches!(item.state, WorkState::Queued) {
            item.state = WorkState::Blocked;
            item.bump();
            self.save_work_item_unlocked(item)?;
        } else if dependencies_ready && matches!(item.state, WorkState::Blocked) {
            item.state = WorkState::Queued;
            item.bump();
            self.save_work_item_unlocked(item)?;
        }
        if item.deadline.is_some_and(|deadline| deadline <= now) && !item.state.is_terminal() {
            item.state = WorkState::Failed;
            item.result = Some(Self::work_failure_result(
                "work item deadline exceeded",
                now,
            ));
            item.bump();
            self.save_work_item_unlocked(item)?;
        }
        Ok(())
    }

    /// Reconcile every durable workload at a caller-supplied instant.
    ///
    /// Supplying the instant makes restart, lease-expiry, and deadline
    /// behavior deterministic in conformance tests while the no-argument
    /// wrapper remains the normal local/hosted service entry point.
    pub fn reconcile_workloads(&self) -> anyhow::Result<WorkloadReconciliationReport> {
        self.reconcile_workloads_at(Utc::now())
    }

    pub fn reconcile_workloads_at(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<WorkloadReconciliationReport> {
        let _guard = self.inner.lock.lock();
        let mut report = WorkloadReconciliationReport::default();
        for mut item in self.list_work_items_unlocked()? {
            report.scanned_items += 1;
            let previous_state = item.state;
            self.refresh_work_item_at_unlocked(&mut item, now)?;
            if previous_state == WorkState::Blocked && item.state == WorkState::Queued {
                report.unblocked_items += 1;
            } else if previous_state == WorkState::Queued && item.state == WorkState::Blocked {
                report.blocked_items += 1;
            }
            if previous_state != WorkState::Failed
                && item.state == WorkState::Failed
                && item.deadline.is_some_and(|deadline| deadline <= now)
            {
                report.failed_items += 1;
                report.deadline_failed_items += 1;
            }

            for mut attempt in self.list_work_attempts_unlocked(Some(&item.work_id))? {
                if !attempt.state.is_active() || now < attempt.lease_expires_at {
                    continue;
                }
                report.expired_attempts += 1;
                if item.state.is_terminal() {
                    // A deadline or explicit terminal transition may have
                    // happened before the supervisor saw the stale attempt.
                    // Preserve that terminal WorkItem state while closing the
                    // attempt so no active lease survives reconciliation.
                    attempt.state = AttemptState::Expired;
                    attempt.terminal_reason = Some("lease expired".into());
                    attempt.updated_at = now;
                    self.save_work_attempt_unlocked(&attempt)?;
                    continue;
                }
                let previous_state = item.state;
                self.expire_attempt_unlocked(&mut item, &mut attempt, now)?;
                if previous_state != WorkState::Queued && item.state == WorkState::Queued {
                    report.retried_items += 1;
                }
                if previous_state != WorkState::Failed && item.state == WorkState::Failed {
                    report.failed_items += 1;
                }
            }
        }
        Ok(report)
    }

    fn expire_attempt_unlocked(
        &self,
        item: &mut WorkItem,
        attempt: &mut WorkAttempt,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        attempt.state = AttemptState::Expired;
        attempt.terminal_reason = Some("lease expired".into());
        attempt.updated_at = now;
        self.save_work_attempt_unlocked(attempt)?;
        if item.policy.retry.retry_expired
            && attempt.attempt_number < item.policy.retry.max_attempts
        {
            item.state = WorkState::Queued;
        } else {
            item.state = WorkState::Failed;
            item.result = Some(Self::work_failure_result(
                "work item lease expired and retry budget was exhausted",
                now,
            ));
        }
        item.bump();
        self.save_work_item_unlocked(item)
    }

    fn active_attempt_unlocked(
        &self,
        work_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<Option<WorkAttempt>> {
        Ok(self
            .list_work_attempts_unlocked(Some(work_id))?
            .into_iter()
            .find(|attempt| attempt.lease_active_at(now)))
    }

    fn require_work_revision(
        item: &WorkItem,
        expected_revision: Option<u64>,
    ) -> Result<(), OrchError> {
        if let Some(expected) = expected_revision {
            if expected != item.revision {
                return Err(OrchError::new(
                    OrchErrorCode::StaleVersion,
                    format!(
                        "work item revision is {}, expected {}",
                        item.revision, expected
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Change the durable owner without claiming a lease. Assignment is a
    /// human/coordinator decision and never starts execution by itself.
    pub fn assign_work(
        &self,
        work_id: &str,
        assigned_agent_id: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        Self::require_work_revision(&item, expected_revision)?;
        if item.state.is_terminal() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "terminal work items cannot be reassigned",
            ));
        }
        item.assigned_agent_id = assigned_agent_id;
        item.validate()?;
        item.bump();
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(item)
    }

    /// Manually reopen a terminal failed Work Item when its declared retry
    /// budget still has capacity. Attempts remain durable and the next claim
    /// receives the next attempt number.
    pub fn retry_work(
        &self,
        work_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
    ) -> Result<WorkItem, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        Self::require_work_revision(&item, expected_revision)?;
        if item.state != WorkState::Failed {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only failed work items can be retried",
            ));
        }
        if item.attempt_count >= item.policy.retry.max_attempts {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item retry budget is exhausted",
            ));
        }
        if reason.trim().is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "retry reason is required",
            ));
        }
        item.state = WorkState::Queued;
        item.result = None;
        item.approval = None;
        item.bump();
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(item)
    }

    /// Release an approval-gated completion into a terminal succeeded state.
    /// No worker lease credential is accepted here: approval is an explicit
    /// operator action authenticated by the service boundary.
    pub fn approve_work(
        &self,
        work_id: &str,
        reviewer_id: &str,
        note: Option<String>,
        expected_revision: Option<u64>,
    ) -> Result<(WorkItem, WorkAttempt), OrchError> {
        let approval = WorkApproval {
            reviewer_id: reviewer_id.to_string(),
            note,
            approved_at: Utc::now(),
        };
        approval.validate()?;
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        Self::require_work_revision(&item, expected_revision)?;
        if item.state != WorkState::AwaitingApproval {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item is not awaiting approval",
            ));
        }
        let mut attempts = self
            .list_work_attempts_unlocked(Some(work_id))
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let attempt = attempts
            .iter_mut()
            .rev()
            .find(|attempt| attempt.state == AttemptState::AwaitingApproval)
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "approval-gated work item has no awaiting attempt",
                )
            })?;
        attempt.state = AttemptState::Succeeded;
        attempt.terminal_reason = Some(format!("approved by {}", reviewer_id));
        attempt.updated_at = approval.approved_at;
        item.state = WorkState::Succeeded;
        item.approval = Some(approval);
        item.bump();
        self.save_work_attempt_unlocked(attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt.clone()))
    }

    pub fn claim_work(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
    ) -> Result<WorkClaim, OrchError> {
        self.claim_work_inner(work_id, claimant_id, lease_ms, None)
    }

    pub fn claim_work_with_lease_secret(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
        lease_secret: &str,
    ) -> Result<WorkClaim, OrchError> {
        if lease_secret.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "lease_secret is required",
            ));
        }
        self.claim_work_inner(work_id, claimant_id, lease_ms, Some(lease_secret))
    }

    fn claim_work_inner(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
        lease_secret: Option<&str>,
    ) -> Result<WorkClaim, OrchError> {
        let lease = lease_duration(lease_ms)?;
        if claimant_id.trim().is_empty() || claimant_id.len() > 256 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "claimant_id is empty or exceeds its bound",
            ));
        }
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        self.refresh_work_item_unlocked(&mut item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let now = Utc::now();
        if let Some(active) = self
            .active_attempt_unlocked(work_id, now)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("work item already leased by {}", active.claimant_id),
            ));
        }
        for mut attempt in self
            .list_work_attempts_unlocked(Some(work_id))
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            if attempt.state.is_active() && !attempt.lease_active_at(now) {
                self.expire_attempt_unlocked(&mut item, &mut attempt, now)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            }
        }
        if item.state != WorkState::Queued {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("work item is not claimable in state {:?}", item.state),
            ));
        }
        if item.attempt_count >= item.policy.retry.max_attempts {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item retry budget is exhausted",
            ));
        }
        let (mut attempt, lease_token) = match lease_secret {
            Some(secret) => {
                let attempt = WorkAttempt::new_with_lease_secret(
                    &item.work_id,
                    item.attempt_count + 1,
                    claimant_id,
                    secret,
                );
                let lease_token = attempt.lease_token_for_secret(secret);
                (attempt, lease_token)
            }
            None => {
                let lease_token = Uuid::new_v4().to_string();
                let attempt = WorkAttempt::new(
                    &item.work_id,
                    item.attempt_count + 1,
                    claimant_id,
                    &lease_token,
                );
                (attempt, lease_token)
            }
        };
        attempt.acquired_at = now;
        attempt.last_heartbeat_at = now;
        attempt.lease_expires_at = now + lease;
        attempt.updated_at = now;
        item.attempt_count = attempt.attempt_number;
        item.state = WorkState::Leased;
        item.bump();
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(WorkClaim {
            work: item,
            attempt,
            lease_token,
        })
    }

    fn load_active_attempt_for_token_unlocked(
        &self,
        item: &WorkItem,
        attempt_id: &str,
        lease_token: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<WorkAttempt, OrchError> {
        let attempt = self
            .load_work_attempt_unlocked(attempt_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work attempt not found"))?;
        if attempt.work_id != item.work_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "work attempt does not belong to the work item",
            ));
        }
        if !attempt.token_matches(lease_token) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "invalid work lease token",
            ));
        }
        if !attempt.state.is_active() || now >= attempt.lease_expires_at {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work lease is no longer active",
            ));
        }
        Ok(attempt)
    }

    pub fn renew_work_lease(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        lease_ms: Option<u64>,
    ) -> Result<WorkAttempt, OrchError> {
        let lease = lease_duration(lease_ms)?;
        let _guard = self.inner.lock.lock();
        let item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        attempt.last_heartbeat_at = now;
        attempt.lease_expires_at = now + lease;
        attempt.updated_at = now;
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(attempt)
    }

    pub fn link_work_run(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        run_id: &str,
    ) -> Result<WorkAttempt, OrchError> {
        let _guard = self.inner.lock.lock();
        let item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        if !attempt.linked_run_ids.iter().any(|id| id == run_id) {
            if attempt.linked_run_ids.len() >= 16 {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "work attempt cannot link more than 16 runs",
                ));
            }
            attempt.linked_run_ids.push(run_id.to_string());
        }
        if attempt.state == AttemptState::Leased {
            attempt.state = AttemptState::Running;
        }
        attempt.updated_at = now;
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(attempt)
    }

    pub fn report_work_progress(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        progress: WorkProgress,
    ) -> Result<(WorkItem, WorkAttempt), OrchError> {
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        item.progress = Some(progress.clone());
        item.state = WorkState::Running;
        item.bump();
        attempt.progress = Some(progress);
        attempt.state = AttemptState::Running;
        attempt.last_heartbeat_at = now;
        attempt.updated_at = now;
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt))
    }

    pub fn release_work(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        reason: &str,
    ) -> Result<(WorkItem, WorkAttempt), OrchError> {
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        attempt.state = AttemptState::Released;
        attempt.terminal_reason = Some(reason.to_string());
        attempt.updated_at = now;
        item.state = WorkState::Queued;
        item.bump();
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt))
    }

    pub fn complete_work(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        result: WorkResult,
    ) -> Result<(WorkItem, WorkAttempt), OrchError> {
        result.validate()?;
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        attempt.result = Some(result.clone());
        attempt.state = if item.policy.requires_approval {
            AttemptState::AwaitingApproval
        } else {
            AttemptState::Succeeded
        };
        attempt.updated_at = now;
        item.result = Some(result);
        item.state = if item.policy.requires_approval {
            WorkState::AwaitingApproval
        } else {
            WorkState::Succeeded
        };
        item.bump();
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt))
    }

    pub fn fail_work(
        &self,
        work_id: &str,
        attempt_id: &str,
        lease_token: &str,
        result: WorkResult,
    ) -> Result<(WorkItem, WorkAttempt), OrchError> {
        result.validate()?;
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let now = Utc::now();
        let mut attempt =
            self.load_active_attempt_for_token_unlocked(&item, attempt_id, lease_token, now)?;
        attempt.result = Some(result.clone());
        attempt.state = AttemptState::Failed;
        attempt.terminal_reason = result.failure.clone();
        attempt.updated_at = now;
        item.result = Some(result);
        item.state = if item.policy.retry.retry_failed
            && attempt.attempt_number < item.policy.retry.max_attempts
        {
            WorkState::Queued
        } else {
            WorkState::Failed
        };
        item.bump();
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt))
    }

    pub fn cancel_work(
        &self,
        work_id: &str,
        reason: &str,
    ) -> Result<(WorkItem, Vec<WorkAttempt>), OrchError> {
        self.cancel_work_checked(work_id, reason, None)
    }

    pub fn cancel_work_checked(
        &self,
        work_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
    ) -> Result<(WorkItem, Vec<WorkAttempt>), OrchError> {
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        Self::require_work_revision(&item, expected_revision)?;
        if item.state.is_terminal() {
            return Ok((
                item,
                self.list_work_attempts_unlocked(Some(work_id))
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            ));
        }
        let now = Utc::now();
        let mut attempts = self
            .list_work_attempts_unlocked(Some(work_id))
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        for attempt in &mut attempts {
            if attempt.state.is_active() {
                attempt.state = AttemptState::Cancelled;
                attempt.terminal_reason = Some(reason.to_string());
                attempt.updated_at = now;
                self.save_work_attempt_unlocked(attempt)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            }
        }
        item.state = WorkState::Cancelled;
        item.result = Some(WorkResult {
            summary: "work item cancelled".into(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            failure: None,
            cancellation_reason: Some(reason.to_string()),
            completed_at: now,
        });
        item.bump();
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempts))
    }

    /// Persist one transport-neutral durable agent identity.
    pub fn save_agent(&self, agent: &AgentRecord) -> anyhow::Result<()> {
        let mut agent = agent.clone();
        agent
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let path = self
            .agent_path(&agent.agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if path.is_file() {
            let mut existing: AgentRecord = serde_json::from_str(&fs::read_to_string(&path)?)?;
            existing
                .migrate_legacy_spec()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if existing.current_spec()?.revision != agent.current_spec()?.revision {
                anyhow::bail!(
                    "Agent specification changes must use the attributable revision operation"
                );
            }
        }
        self.save_agent_spec_unlocked(&agent.agent_id, agent.current_spec()?)?;
        atomic_write_json(&path, &agent)
    }

    pub fn load_agent(&self, agent_id: &str) -> anyhow::Result<Option<AgentRecord>> {
        let _g = self.inner.lock.lock();
        self.load_agent_unlocked(agent_id)
    }

    fn load_agent_unlocked(&self, agent_id: &str) -> anyhow::Result<Option<AgentRecord>> {
        let path = match self.agent_path(agent_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let migrated = agent
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        self.save_agent_spec_unlocked(&agent.agent_id, agent.current_spec()?)?;
        if migrated {
            atomic_write_json(&path, &agent)?;
        }
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
            let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&path)?)?;
            let migrated = agent
                .migrate_legacy_spec()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            agent
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            self.save_agent_spec_unlocked(&agent.agent_id, agent.current_spec()?)?;
            if migrated {
                atomic_write_json(&path, &agent)?;
            }
            out.push(agent);
        }
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    /// Claim an unowned legacy Agent for the authenticated service account,
    /// or verify that an already-owned Agent belongs to that same account.
    /// Device credentials intentionally share this account owner while their
    /// individual IDs remain attributable at the transport layer.
    pub fn claim_agent_owner(
        &self,
        agent_id: &str,
        owner_principal_id: &str,
    ) -> anyhow::Result<Option<AgentRecord>> {
        let owner_principal_id = owner_principal_id.trim().to_string();
        anyhow::ensure!(
            !owner_principal_id.is_empty(),
            "Agent owner principal id must not be empty"
        );
        self.update_agent(agent_id, |agent| {
            match agent.owner_principal_id.as_deref() {
                None => agent.owner_principal_id = Some(owner_principal_id.clone()),
                Some(existing) if existing == owner_principal_id => {}
                Some(existing) => {
                    anyhow::bail!("Agent is owned by a different service account ({existing})")
                }
            }
            Ok(())
        })
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
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let original_spec = agent.current_spec()?.clone();
        update(&mut agent)?;
        if agent.current_spec()? != &original_spec {
            anyhow::bail!(
                "Agent specification changes must use the attributable revision operation"
            );
        }
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        agent.updated_at = Utc::now();
        self.save_agent_spec_unlocked(&agent.agent_id, agent.current_spec()?)?;
        atomic_write_json(&path, &agent)?;
        Ok(Some(agent))
    }

    fn save_agent_spec_unlocked(&self, agent_id: &str, spec: &AgentSpec) -> anyhow::Result<()> {
        spec.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let path = self
            .agent_spec_path(agent_id, spec.revision)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if path.is_file() {
            let existing: AgentSpec = serde_json::from_str(&fs::read_to_string(&path)?)?;
            if existing != *spec {
                anyhow::bail!(
                    "agent specification revision {} is immutable",
                    spec.revision
                );
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write_json(&path, spec)
    }

    pub fn load_agent_spec(
        &self,
        agent_id: &str,
        revision: u64,
    ) -> anyhow::Result<Option<AgentSpec>> {
        let _g = self.inner.lock.lock();
        let path = match self.agent_spec_path(agent_id, revision) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let spec: AgentSpec = serde_json::from_str(&fs::read_to_string(path)?)?;
        spec.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(spec))
    }

    pub fn list_agent_specs(&self, agent_id: &str) -> anyhow::Result<Vec<AgentSpec>> {
        let _g = self.inner.lock.lock();
        let dir = self
            .agent_spec_path(agent_id, 1)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?
            .parent()
            .expect("agent spec path has a parent")
            .to_path_buf();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut specs = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let spec: AgentSpec = serde_json::from_str(&fs::read_to_string(path)?)?;
            spec.validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            specs.push(spec);
        }
        specs.sort_by_key(|spec| spec.revision);
        Ok(specs)
    }

    /// Install an attributable replacement specification. The immutable
    /// revision is written before the Agent pointer, so a crash cannot leave a
    /// record referring to a missing revision.
    pub fn revise_agent_spec<F>(
        &self,
        agent_id: &str,
        actor: &str,
        revise: F,
    ) -> anyhow::Result<Option<AgentRecord>>
    where
        F: FnOnce(&mut AgentSpec) -> anyhow::Result<()>,
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
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let current = agent.current_spec()?.clone();
        let mut next = current.clone();
        revise(&mut next)?;
        let mut revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("agent specification revision counter overflowed"))?;
        if next.source_workspace != current.source_workspace {
            anyhow::bail!("an Agent source workspace cannot change through a spec revision");
        }
        loop {
            next.revision = revision;
            next.previous_revision = Some(current.revision);
            next.created_by = actor.to_string();
            let revision_path = self
                .agent_spec_path(agent_id, revision)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if revision_path.is_file() {
                let existing: AgentSpec =
                    serde_json::from_str(&fs::read_to_string(&revision_path)?)?;
                existing
                    .validate()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                next.created_at = existing.created_at;
                next.validate()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                if existing == next {
                    next = existing;
                    break;
                }
                revision = revision.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!("agent specification revision counter overflowed")
                })?;
                continue;
            }
            next.created_at = Utc::now();
            next.validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            self.save_agent_spec_unlocked(agent_id, &next)?;
            break;
        }
        agent.workspace = next.source_workspace.clone();
        agent.model = next.model.selection_key.clone();
        agent.spec = Some(next);
        agent.updated_at = Utc::now();
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
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

    /// Persist a sealed input snapshot by content hash. Existing content may
    /// be replayed, but the same hash can never be rebound to different bytes.
    pub fn save_continuation_input(
        &self,
        snapshot: &ContinuationInputSnapshot,
    ) -> anyhow::Result<()> {
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let path = self
            .continuation_input_path(&snapshot.input_hash)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if path.is_file() {
            let existing: ContinuationInputSnapshot =
                serde_json::from_str(&fs::read_to_string(&path)?)?;
            if existing != *snapshot {
                anyhow::bail!("continuation input hash is already bound to different content");
            }
            return Ok(());
        }
        Ok(write_json_exclusive(&path, snapshot)?)
    }

    pub fn load_continuation_input(
        &self,
        input_hash: &str,
    ) -> anyhow::Result<Option<ContinuationInputSnapshot>> {
        let _g = self.inner.lock.lock();
        let path = match self.continuation_input_path(input_hash) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let snapshot: ContinuationInputSnapshot = serde_json::from_str(&fs::read_to_string(path)?)?;
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(Some(snapshot))
    }

    /// Persist exact model-facing continuation bytes by content-derived ID.
    pub fn save_continuation_context(&self, context: &ContinuationContext) -> anyhow::Result<()> {
        context
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let _g = self.inner.lock.lock();
        let path = self
            .continuation_context_path(&context.context_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let input_path = self
            .continuation_input_path(&context.input_hash)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !input_path.is_file() {
            anyhow::bail!("continuation context input snapshot is missing");
        }
        let input: ContinuationInputSnapshot =
            serde_json::from_str(&fs::read_to_string(&input_path)?)?;
        input
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if input.input_hash != context.input_hash {
            anyhow::bail!("continuation context input snapshot does not match");
        }
        let assembled = assemble_continuation_context(&input)
            .map_err(|failure| anyhow::anyhow!(failure.detail))?;
        if assembled != *context {
            anyhow::bail!("continuation context was not assembled from its referenced input");
        }
        if path.is_file() {
            let existing: ContinuationContext = serde_json::from_str(&fs::read_to_string(&path)?)?;
            if existing != *context {
                anyhow::bail!("continuation context ID is already bound to different content");
            }
            return Ok(());
        }
        Ok(write_json_exclusive(&path, context)?)
    }

    pub fn load_continuation_context(
        &self,
        context_id: &str,
    ) -> anyhow::Result<Option<ContinuationContext>> {
        let _g = self.inner.lock.lock();
        let path = match self.continuation_context_path(context_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let context: ContinuationContext = serde_json::from_str(&fs::read_to_string(path)?)?;
        context
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let input_path = self
            .continuation_input_path(&context.input_hash)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !input_path.is_file() {
            anyhow::bail!("continuation context input snapshot is missing");
        }
        let input: ContinuationInputSnapshot =
            serde_json::from_str(&fs::read_to_string(input_path)?)?;
        input
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if input.input_hash != context.input_hash {
            anyhow::bail!("continuation context input snapshot does not match");
        }
        let assembled = assemble_continuation_context(&input)
            .map_err(|failure| anyhow::anyhow!(failure.detail))?;
        if assembled != context {
            anyhow::bail!("continuation context was not assembled from its referenced input");
        }
        Ok(Some(context))
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
                        final_run.final_response = current.final_response;
                        if final_run.stop_cause == Some(RunStopCause::TokenAccountingUnavailable) {
                            let code = "max_total_tokens_usage_unavailable";
                            final_run.terminal_result = Some(code.into());
                            final_run.error_code = Some(code.into());
                        } else {
                            final_run.terminal_result = current.terminal_result;
                            final_run.error_code = current.error_code;
                        }
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
        let mut n = 0;
        let mut interrupted_agents = Vec::new();
        for mut run in self.list_runs()? {
            let unfinished = matches!(run.state, RunState::Queued | RunState::Running);
            if unfinished {
                run.state = RunState::Interrupted;
                run.queue_position = None;
                run.terminal_result = Some("interrupted".into());
                run.error_code = Some("interrupted".into());
                run.stop_cause = Some(RunStopCause::Interrupted);
                if let Some(execution) = run.execution.as_mut() {
                    execution.promotion_state = PromotionState::Conflicted;
                }
                if let Some(agent_id) = run.agent_id.clone() {
                    interrupted_agents.push((agent_id, run.run_id.clone()));
                }
                n += 1;
            }
            let unresolved_provider_attempt = run.fail_closed_unresolved_provider_attempts();
            if unfinished || unresolved_provider_attempt {
                run.updated_at = Utc::now();
                self.save_run(&run)?;
            }
        }
        for (agent_id, run_id) in interrupted_agents {
            let _ = self.update_agent(&agent_id, |agent| {
                if agent.current_run_id.as_deref() == Some(run_id.as_str()) {
                    agent.current_run_id = None;
                    agent.last_run_id = Some(run_id.clone());
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
                    current.last_run_id = Some(run_id.clone());
                    current.state = next_state;
                }
                Ok(())
            })?;
        }
        Ok(n)
    }

    fn recover_agent_activation_intents(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("agent-activation");
        let mut recovered = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: AgentActivationIntent = serde_json::from_str(&fs::read_to_string(&path)?)?;
            anyhow::ensure!(
                intent.run.agent_id.as_deref() == Some(intent.activated_agent.agent_id.as_str())
                    && intent.activated_agent.current_run_id.as_deref()
                        == Some(intent.run.run_id.as_str()),
                "Agent activation recovery intent is inconsistent"
            );
            let run_path = self
                .run_path(&intent.run.run_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if run_path.is_file() {
                let existing: RunRecord = serde_json::from_str(&fs::read_to_string(&run_path)?)?;
                anyhow::ensure!(
                    serde_json::to_value(existing)? == serde_json::to_value(&intent.run)?,
                    "Agent activation recovery Run conflicts with durable state"
                );
            }
            let agent_path = self
                .agent_path(&intent.activated_agent.agent_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if agent_path.is_file() {
                let existing: AgentRecord =
                    serde_json::from_str(&fs::read_to_string(&agent_path)?)?;
                anyhow::ensure!(
                    existing.agent_id == intent.activated_agent.agent_id
                        && existing
                            .current_run_id
                            .as_deref()
                            .is_none_or(|run_id| { run_id == intent.run.run_id }),
                    "Agent activation recovery conflicts with another active Run"
                );
            }
            atomic_write_json(&run_path, &intent.run)?;
            atomic_write_json(&agent_path, &intent.activated_agent)?;
            remove_file_durable(&path)?;
            recovered += 1;
        }
        Ok(recovered)
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
    // Usage updates are cumulative snapshots, not deltas. A finalization
    // candidate must never overwrite a newer durable snapshot with stale
    // zeros, and missing usage is sticky across every observation.
    target.aggregates.usage.prompt_tokens = target
        .aggregates
        .usage
        .prompt_tokens
        .max(current.aggregates.usage.prompt_tokens);
    target.aggregates.usage.completion_tokens = target
        .aggregates
        .usage
        .completion_tokens
        .max(current.aggregates.usage.completion_tokens);
    target.aggregates.usage.total_tokens = target
        .aggregates
        .usage
        .total_tokens
        .max(current.aggregates.usage.total_tokens);
    target.aggregates.usage.requests = target
        .aggregates
        .usage
        .requests
        .max(current.aggregates.usage.requests);
    target.aggregates.usage_complete &= current.aggregates.usage_complete;
    // A terminal candidate may have deliberately closed an unresolved marker
    // as accounting-unavailable. Do not resurrect the stale durable pending
    // count while installing that fail-closed decision.
    if target.stop_cause != Some(RunStopCause::TokenAccountingUnavailable) {
        target.aggregates.usage_pending_requests = current.aggregates.usage_pending_requests;
    }
    if let Some(verification) = target.aggregates.verification.as_mut() {
        verification.usage = target.aggregates.usage.clone();
    }
    // The current durable record may have been updated by the provider usage
    // tracker after the finalization candidate was cloned. Its host-decided
    // cause is therefore authoritative over a stale candidate cause.
    if current.stop_cause.is_some()
        && target.stop_cause != Some(RunStopCause::TokenAccountingUnavailable)
    {
        target.stop_cause = current.stop_cause;
    }
    if current
        .error_code
        .as_deref()
        .is_some_and(|code| code.starts_with("max_total_tokens_"))
    {
        target.error_code = current.error_code.clone();
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

fn remove_file_durable(path: &Path) -> anyhow::Result<()> {
    if path.is_file() {
        fs::remove_file(path)?;
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(crate) fn workspaces_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
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
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| std::io::Error::other("exclusive JSON path has no filename"))?;
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
    use std::io::Write;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        // A hard-link install is atomic and never replaces an existing
        // content-addressed record. The temporary inode is removed only after
        // the final name is durable.
        fs::hard_link(&tmp, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&tmp);
    result
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
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
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
            stop_cause: None,
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
            agent_spec_revision: None,
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
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
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
            stop_cause: None,
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
    fn restart_fails_closed_when_a_bounded_provider_attempt_was_unresolved() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let mut run = terminal_run("pending-provider");
        run.state = RunState::Running;
        run.terminal_result = None;
        run.final_response = None;
        run.bounds.max_total_tokens = Some(1_000);
        run.aggregates.usage_pending_requests = 1;
        store.save_run(&run).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let recovered = reopened.load_run("pending-provider").unwrap().unwrap();
        assert_eq!(recovered.state, RunState::Interrupted);
        assert_eq!(recovered.aggregates.usage_pending_requests, 0);
        assert!(!recovered.aggregates.usage_complete);
        assert_eq!(
            recovered.error_code.as_deref(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(
            recovered.stop_cause,
            Some(RunStopCause::TokenAccountingUnavailable)
        );
    }

    #[test]
    fn restart_reconciles_a_terminal_run_with_an_unresolved_provider_attempt() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let mut run = terminal_run("terminal-pending-provider");
        run.state = RunState::Cancelled;
        run.terminal_result = Some("cancelled".into());
        run.error_code = Some("cancelled".into());
        run.stop_cause = Some(RunStopCause::Cancelled);
        run.bounds.max_total_tokens = Some(1_000);
        run.aggregates.usage_pending_requests = 1;
        store.save_run(&run).unwrap();
        drop(store);

        let reopened = OrchStore::open(d.path()).unwrap();
        let recovered = reopened
            .load_run("terminal-pending-provider")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, RunState::Cancelled);
        assert_eq!(recovered.aggregates.usage_pending_requests, 0);
        assert!(!recovered.aggregates.usage_complete);
        assert_eq!(
            recovered.error_code.as_deref(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(
            recovered.stop_cause,
            Some(RunStopCause::TokenAccountingUnavailable)
        );
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
                owner_principal_id: None,
                session_id,
                lane_ids: vec![session_id],
                lane_associations: Vec::new(),
                workspace: "/tmp/w".into(),
                model: "grok".into(),
                spec: None,
                state: AgentState::Active,
                current_run_id: Some("r-agent".into()),
                last_run_id: None,
                last_lane_id: Some(session_id),
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
                owner_principal_id: None,
                session_id,
                lane_ids: vec![session_id],
                lane_associations: Vec::new(),
                workspace: "/tmp/w".into(),
                model: "grok".into(),
                spec: None,
                state: AgentState::Active,
                current_run_id: Some("terminal-gap".into()),
                last_run_id: None,
                last_lane_id: Some(session_id),
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
    fn legacy_agent_is_migrated_to_an_attributable_revision_and_lane_association() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let session_id = Uuid::new_v4();
        let agent_id = "agent-legacy-spec";
        let path = store.agent_path(agent_id).unwrap();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agentId": agent_id,
                "sessionId": session_id,
                "workspace": "/tmp/project",
                "model": "grok",
                "state": "waiting",
                "createdAt": Utc::now(),
                "updatedAt": Utc::now()
            }))
            .unwrap(),
        )
        .unwrap();

        let migrated = store.load_agent(agent_id).unwrap().unwrap();
        let spec = migrated.current_spec().unwrap();
        assert_eq!(spec.revision, 1);
        assert_eq!(spec.created_by, "legacy_migration");
        assert_eq!(spec.model.provider_id, "xai");
        assert_eq!(spec.authority.allowed_mcp_servers, vec!["*"]);
        assert_eq!(migrated.known_lane_ids(), vec![session_id]);
        assert_eq!(migrated.lane_associations.len(), 1);
        assert_eq!(
            store.list_agent_specs(agent_id).unwrap(),
            vec![spec.clone()]
        );
        let rewritten: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(rewritten["spec"]["revision"], 1);
        assert_eq!(
            rewritten["laneAssociations"][0]["laneId"],
            session_id.to_string()
        );
    }

    #[test]
    fn agent_spec_revisions_are_append_only_and_attributable() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let mut agent = AgentRecord {
            agent_id: "agent-revisions".into(),
            owner_principal_id: None,
            session_id,
            lane_ids: vec![session_id],
            lane_associations: Vec::new(),
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            spec: None,
            state: AgentState::Waiting,
            current_run_id: None,
            last_run_id: None,
            last_lane_id: Some(session_id),
            latest_checkpoint_id: None,
            continuation_ordinal: 0,
            created_at: now,
            updated_at: now,
        };
        store.save_agent(&agent).unwrap();
        let revised = store
            .revise_agent_spec("agent-revisions", "operator:test", |spec| {
                spec.role = "Release steward".into();
                Ok(())
            })
            .unwrap()
            .unwrap();
        let current = revised.current_spec().unwrap();
        assert_eq!(current.revision, 2);
        assert_eq!(current.previous_revision, Some(1));
        assert_eq!(current.created_by, "operator:test");
        assert_eq!(current.role, "Release steward");
        let specs = store.list_agent_specs("agent-revisions").unwrap();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].role, "Software development agent");

        agent.migrate_legacy_spec().unwrap();
        assert!(store.save_agent(&agent).is_err());
        agent.spec.as_mut().unwrap().role = "tampered".into();
        assert!(store.save_agent(&agent).is_err());
    }

    #[test]
    fn orphaned_agent_spec_revision_is_reused_or_skipped_without_wedging() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let agent = AgentRecord {
            agent_id: "agent-orphan-revision".into(),
            owner_principal_id: None,
            session_id,
            lane_ids: vec![session_id],
            lane_associations: Vec::new(),
            workspace: "/tmp/project".into(),
            model: "grok".into(),
            spec: None,
            state: AgentState::Waiting,
            current_run_id: None,
            last_run_id: None,
            last_lane_id: Some(session_id),
            latest_checkpoint_id: None,
            continuation_ordinal: 0,
            created_at: now,
            updated_at: now,
        };
        store.save_agent(&agent).unwrap();
        let current = store
            .load_agent("agent-orphan-revision")
            .unwrap()
            .unwrap()
            .current_spec()
            .unwrap()
            .clone();
        let mut orphan = current.clone();
        orphan.revision = 2;
        orphan.previous_revision = Some(1);
        orphan.role = "First attempted role".into();
        orphan.created_by = "operator:test".into();
        orphan.created_at = Utc::now();
        store
            .save_agent_spec_unlocked("agent-orphan-revision", &orphan)
            .unwrap();

        let reused = store
            .revise_agent_spec("agent-orphan-revision", "operator:test", |spec| {
                spec.role = "First attempted role".into();
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(reused.current_spec().unwrap().revision, 2);

        // Simulate another orphan after the pointer remains on revision 2.
        let current = reused.current_spec().unwrap().clone();
        let mut conflicting = current.clone();
        conflicting.revision = 3;
        conflicting.previous_revision = Some(2);
        conflicting.role = "Abandoned role".into();
        conflicting.created_by = "operator:abandoned".into();
        conflicting.created_at = Utc::now();
        store
            .save_agent_spec_unlocked("agent-orphan-revision", &conflicting)
            .unwrap();
        let advanced = store
            .revise_agent_spec("agent-orphan-revision", "operator:test", |spec| {
                spec.role = "Final role".into();
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(advanced.current_spec().unwrap().revision, 4);
        assert_eq!(advanced.current_spec().unwrap().previous_revision, Some(2));
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
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
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
            stop_cause: None,
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
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
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
            stop_cause: None,
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

    #[test]
    fn finalization_preserves_newer_durable_usage_and_typed_stop() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let mut durable = terminal_run("usage-final");
        durable.state = RunState::Running;
        durable.terminal_result = None;
        durable.final_response = None;
        durable.aggregates.usage.prompt_tokens = 7;
        durable.aggregates.usage.completion_tokens = 5;
        durable.aggregates.usage.total_tokens = 12;
        durable.aggregates.usage.requests = 2;
        durable.aggregates.usage_complete = false;
        durable.error_code = Some("max_total_tokens_usage_unavailable".into());
        durable.stop_cause = Some(RunStopCause::TokenAccountingUnavailable);
        store.save_run(&durable).unwrap();

        let mut stale_candidate = terminal_run("usage-final");
        stale_candidate.error_code = Some("limit_reached".into());
        stale_candidate.stop_cause = Some(RunStopCause::Completed);
        let finalized = store.persist_finalization(&stale_candidate).unwrap();

        assert_eq!(finalized.aggregates.usage.prompt_tokens, 7);
        assert_eq!(finalized.aggregates.usage.completion_tokens, 5);
        assert_eq!(finalized.aggregates.usage.total_tokens, 12);
        assert_eq!(finalized.aggregates.usage.requests, 2);
        assert!(!finalized.aggregates.usage_complete);
        assert_eq!(
            finalized.error_code.as_deref(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(
            finalized.stop_cause,
            Some(RunStopCause::TokenAccountingUnavailable)
        );
    }

    #[test]
    fn finalization_keeps_accounting_fields_consistent_across_a_cancel_race() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let mut cancelled = terminal_run("cancel-accounting-race");
        cancelled.state = RunState::Cancelled;
        cancelled.terminal_result = Some("cancelled".into());
        cancelled.error_code = Some("cancelled".into());
        cancelled.stop_cause = Some(RunStopCause::Cancelled);
        cancelled.bounds.max_total_tokens = Some(100);
        cancelled.aggregates.usage_pending_requests = 1;
        store.save_run(&cancelled).unwrap();

        let mut accounting = terminal_run("cancel-accounting-race");
        accounting.state = RunState::LimitReached;
        accounting.bounds.max_total_tokens = Some(100);
        accounting.terminal_result = Some("max_total_tokens_usage_unavailable".into());
        accounting.error_code = Some("max_total_tokens_usage_unavailable".into());
        accounting.stop_cause = Some(RunStopCause::TokenAccountingUnavailable);
        accounting.aggregates.usage_complete = false;
        accounting.aggregates.usage_pending_requests = 0;

        let finalized = store.persist_finalization(&accounting).unwrap();
        assert_eq!(finalized.state, RunState::Cancelled);
        assert_eq!(
            finalized.stop_cause,
            Some(RunStopCause::TokenAccountingUnavailable)
        );
        assert_eq!(
            finalized.terminal_result.as_deref(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(
            finalized.error_code.as_deref(),
            Some("max_total_tokens_usage_unavailable")
        );
        assert_eq!(finalized.aggregates.usage_pending_requests, 0);
        assert!(!finalized.aggregates.usage_complete);
    }

    #[test]
    fn agent_activation_serializes_competing_lane_runs() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let first_lane = Uuid::new_v4();
        let second_lane = Uuid::new_v4();
        let now = Utc::now();
        store
            .save_agent(&AgentRecord {
                agent_id: "agent-activation-race".into(),
                owner_principal_id: None,
                session_id: first_lane,
                lane_ids: vec![first_lane, second_lane],
                lane_associations: Vec::new(),
                workspace: "/tmp/w".into(),
                model: "grok".into(),
                spec: None,
                state: AgentState::Waiting,
                current_run_id: None,
                last_run_id: None,
                last_lane_id: None,
                latest_checkpoint_id: None,
                continuation_ordinal: 0,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let mut first = terminal_run("activation-first");
        first.session_id = first_lane;
        first.state = RunState::Running;
        first.agent_id = Some("agent-activation-race".into());
        first.terminal_result = None;
        first.final_response = None;
        first.end_seq = None;
        let mut second = first.clone();
        second.run_id = "activation-second".into();
        second.request_id = "req-activation-second".into();
        second.session_id = second_lane;

        store
            .save_run_and_activate_agent(&first, "agent-activation-race")
            .unwrap();
        let error = store
            .save_run_and_activate_agent(&second, "agent-activation-race")
            .unwrap_err()
            .to_string();
        assert!(error.contains("active Run"), "unexpected error: {error}");
        assert!(store.load_run(&second.run_id).unwrap().is_none());
        assert_eq!(
            store
                .load_agent("agent-activation-race")
                .unwrap()
                .unwrap()
                .current_run_id
                .as_deref(),
            Some(first.run_id.as_str())
        );
    }

    #[test]
    fn restart_recovers_partial_agent_activation_then_interrupts_it() {
        let root = tempdir().unwrap();
        let lane_id = Uuid::new_v4();
        let run_id = "activation-crash-run";
        {
            let store = OrchStore::open(root.path()).unwrap();
            let now = Utc::now();
            store
                .save_agent(&AgentRecord {
                    agent_id: "agent-activation-crash".into(),
                    owner_principal_id: None,
                    session_id: lane_id,
                    lane_ids: vec![lane_id],
                    lane_associations: Vec::new(),
                    workspace: "/tmp/w".into(),
                    model: "grok".into(),
                    spec: None,
                    state: AgentState::Waiting,
                    current_run_id: None,
                    last_run_id: None,
                    last_lane_id: None,
                    latest_checkpoint_id: None,
                    continuation_ordinal: 0,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
            let mut run = terminal_run(run_id);
            run.session_id = lane_id;
            run.state = RunState::Running;
            run.agent_id = Some("agent-activation-crash".into());
            run.terminal_result = None;
            run.final_response = None;
            run.end_seq = None;
            let mut activated = store.load_agent("agent-activation-crash").unwrap().unwrap();
            activated.state = AgentState::Active;
            activated.current_run_id = Some(run_id.into());
            activated.last_lane_id = Some(lane_id);
            let intent = AgentActivationIntent {
                run: run.clone(),
                activated_agent: activated,
            };
            atomic_write_json(&store.agent_activation_path(run_id).unwrap(), &intent).unwrap();
            atomic_write_json(&store.run_path(run_id).unwrap(), &run).unwrap();
            // Simulated crash before the Agent pointer write.
        }

        let reopened = OrchStore::open(root.path()).unwrap();
        let run = reopened.load_run(run_id).unwrap().unwrap();
        let agent = reopened
            .load_agent("agent-activation-crash")
            .unwrap()
            .unwrap();
        assert_eq!(run.state, RunState::Interrupted);
        assert_eq!(agent.state, AgentState::Interrupted);
        assert_eq!(agent.current_run_id, None);
        assert_eq!(agent.last_run_id.as_deref(), Some(run_id));
        assert!(!reopened.agent_activation_path(run_id).unwrap().exists());
    }
}
