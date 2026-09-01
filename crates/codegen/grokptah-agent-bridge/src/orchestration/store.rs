//! Durable run records, idempotency receipts, audit log (#196).

use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;

use anyhow::Context;
use chrono::{Duration, Utc};
use fs2::FileExt;
use parking_lot::{Condvar, Mutex};
use uuid::Uuid;

use super::managed::{
    ManagedExecutionIntent, ManagedExecutionPolicy, ManagedFinalizationOutcome,
    ManagedFinalizationRecord, ManagedFinalizationStage, ManagedIntentState, ManagedRetryCause,
    MANAGED_FINALIZATION_SCHEMA_VERSION,
};
use super::manager::{ManagerDecisionRecord, ManagerPlan};
use super::message::{MessagePage, WorkMessage, MAX_RETAINED_MESSAGES};
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
use super::worker::{WorkerHostKind, WorkerPresence, WorkerProjection};
use super::workload::{
    lease_duration, AssignmentStatus, AttemptState, BlockProvenance, WorkApproval, WorkAttempt,
    WorkClaim, WorkDecision, WorkDecisionAction, WorkItem, WorkProgress, WorkResult, WorkState,
    WorkloadReconciliationReport, WORKLOAD_SCHEMA_VERSION,
};
use super::{assemble_continuation_context, ContinuationContext, ContinuationInputSnapshot};
use crate::completion::{evidence_authorizes_success, CompletionEvidence};

#[derive(Clone)]
pub struct OrchStore {
    inner: Arc<OrchStoreInner>,
}

/// Atomic pre-claim fence for the one-attempt managed Grok executor. Every
/// field is derived from the already-eligible snapshot and is rechecked while
/// the store lock is held, before an attempt or lease is created.
pub(super) struct ManagedGrokClaimFence<'a> {
    pub expected_work_revision: u64,
    pub expected_decision_id: &'a str,
    pub expected_agent_spec_revision: u64,
    pub expected_allowed_files: &'a [String],
}

impl OrchStore {
    /// The authority every durable effect through this handle must pass.
    pub(crate) fn lease(&self) -> crate::host_runtime::WriteLease {
        self.inner.write_lease.lock().clone()
    }

    /// Bind this handle (and every clone of it) to the runtime that owns its
    /// home. Called by the host when it adopts a store it did not open.
    ///
    /// Returns `false` — leaving the handle's own authority untouched — when
    /// that runtime owns a *different* home, so a store can never borrow
    /// authority for a home the binder does not hold. A foreign-rooted ledger
    /// keeps the OS lock it took for its own home at open, which is a real
    /// authority for that home and no authority at all for this one.
    pub(crate) fn bind_lifecycle(
        &self,
        lifecycle: &Arc<crate::host_runtime::HostLifecycle>,
    ) -> bool {
        let Some(rebound) = crate::host_runtime::WriteLease::bound_to(&self.inner.root, lifecycle)
        else {
            return false;
        };
        *self.inner.write_lease.lock() = rebound;
        true
    }

    /// Whether this handle is bound to a live runtime rather than owning the
    /// home's OS lock itself.
    pub fn is_lease_bound(&self) -> bool {
        self.inner.write_lease.lock().is_bound()
    }

    /// Whether this handle holds its home's single-instance lock itself, for
    /// the whole lifetime of the handle.
    ///
    /// This is the negative control against a check-only probe: an offline
    /// ledger that can write must be able to say it *holds* the lock, not that
    /// it once observed the home to be free (#455).
    pub fn holds_home_lock_itself(&self) -> bool {
        self.inner.write_lease.lock().is_offline_owner()
    }

    /// The home whose single-instance lock governs this handle's writes.
    pub fn authority_home_lock(&self) -> PathBuf {
        self.inner.write_lease.lock().home_lock_path().to_path_buf()
    }
}

struct OrchStoreInner {
    /// Durable-write authority for this handle (#455).
    ///
    /// It lives in the shared inner, so every clone of this store — a service
    /// handle, a supervisor's copy, whatever a caller kept from `store()` —
    /// carries the same authority and fails closed together once the runtime
    /// that owns the home stops. Binding travels with the handle instead of
    /// living at a call site someone has to remember.
    write_lease: Arc<Mutex<crate::host_runtime::WriteLease>>,
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
    /// A queued Run replaced by `run` during atomic promotion. Legacy
    /// creation intents have no prior record.
    #[serde(default)]
    prior_run: Option<RunRecord>,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagerCreationIntent {
    plan: ManagerPlan,
    root_work: WorkItem,
}

/// Crash-recovery envelope for the paired WorkDecision + WorkItem mutation.
///
/// Each record is still an independently atomic JSON file, but the intent is
/// the durable commit marker: reopening the ledger can finish an interrupted
/// pair, or refuse a pair whose prior state was changed by an unknown writer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkMutationIntent {
    schema_version: u32,
    intent_id: String,
    work_id: String,
    expected_revision: u64,
    prior_item_digest: String,
    next_item_digest: String,
    decision_digest: String,
    item: WorkItem,
    decision: WorkDecision,
}

impl WorkMutationIntent {
    fn new(prior: &WorkItem, item: WorkItem, decision: WorkDecision) -> Result<Self, OrchError> {
        let prior_item_digest = super::hash_payload(
            &serde_json::to_value(prior)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        );
        let next_item_digest = super::hash_payload(
            &serde_json::to_value(&item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        );
        let decision_digest = super::hash_payload(
            &serde_json::to_value(&decision)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        );
        let intent = Self {
            schema_version: WORKLOAD_SCHEMA_VERSION,
            intent_id: decision.decision_id.clone(),
            work_id: item.work_id.clone(),
            expected_revision: prior.revision,
            prior_item_digest,
            next_item_digest,
            decision_digest,
            item,
            decision,
        };
        intent.validate()?;
        Ok(intent)
    }

    fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != WORKLOAD_SCHEMA_VERSION
            || self.intent_id.is_empty()
            || self.work_id.is_empty()
            || self.expected_revision == 0
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "work mutation intent schema or identity is invalid",
            ));
        }
        self.item.validate()?;
        self.decision.validate()?;
        if self.item.work_id != self.work_id
            || self.decision.work_id != self.work_id
            || self.intent_id != self.decision.decision_id
            || self.item.last_decision_id.as_deref() != Some(self.intent_id.as_str())
            || self.decision.work_revision != Some(self.expected_revision)
            // Work decisions describe the revision being changed; the item
            // carries the incremented revision after the mutation lands.
            || self
                .decision
                .work_revision
                .and_then(|revision| revision.checked_add(1))
                != Some(self.item.revision)
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "work mutation intent records do not agree",
            ));
        }
        let item_digest = super::hash_payload(
            &serde_json::to_value(&self.item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        );
        let decision_digest = super::hash_payload(
            &serde_json::to_value(&self.decision)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        );
        if item_digest != self.next_item_digest || decision_digest != self.decision_digest {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "work mutation intent digest is invalid",
            ));
        }
        Ok(())
    }
}

const MAX_AUDIT_BYTES: u64 = 4 * 1024 * 1024;

struct AssignmentMutation<'a> {
    work_id: &'a str,
    action: WorkDecisionAction,
    actor_id: &'a str,
    actor_agent_id: Option<&'a str>,
    assigned_agent_id: Option<&'a str>,
    reason: &'a str,
    expected_revision: Option<u64>,
    now: chrono::DateTime<Utc>,
}

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
    join_state: Arc<(Mutex<AuditWriterJoinState>, Condvar)>,
    last_audit_error: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use = "audit-writer stop evidence must be checked before releasing host authority"]
pub(crate) struct AuditWriterStopReport {
    pub fully_stopped: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
struct AuditWriterJoinState {
    started: bool,
    outcome: Option<AuditWriterStopReport>,
}

impl AuditWriter {
    /// Stop accepting audit entries and start a process-visible join monitor.
    /// The monitor and its outcome survive cancellation of a shutdown waiter,
    /// so an interrupted bounded wait cannot lose track of the writer.
    fn begin_join(&self) {
        self.tx.lock().take();
        let (state_lock, state_ready) = &*self.join_state;
        let mut state = state_lock.lock();
        if state.started {
            return;
        }
        state.started = true;
        let Some(join) = self.join.lock().take() else {
            state.outcome = Some(AuditWriterStopReport {
                fully_stopped: true,
                errors: Vec::new(),
            });
            state_ready.notify_all();
            return;
        };
        drop(state);

        let join_state = self.join_state.clone();
        let last_audit_error = self.last_audit_error.clone();
        let monitor = std::thread::Builder::new()
            .name("grokptah-orchestration-audit-join".into())
            .spawn(move || {
                let report = match join.join() {
                    Ok(()) => AuditWriterStopReport {
                        fully_stopped: true,
                        errors: Vec::new(),
                    },
                    Err(payload) => {
                        let error = payload
                            .downcast_ref::<&str>()
                            .map(|message| (*message).to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "non-string panic payload".to_string());
                        *last_audit_error.lock() = Some(error.clone());
                        AuditWriterStopReport {
                            fully_stopped: true,
                            errors: vec![error],
                        }
                    }
                };
                let (state_lock, state_ready) = &*join_state;
                state_lock.lock().outcome = Some(report);
                state_ready.notify_all();
            });
        if let Err(error) = monitor {
            let error = format!("failed to start the orchestration audit join monitor: {error}");
            *self.last_audit_error.lock() = Some(error.clone());
            let mut state = state_lock.lock();
            state.outcome = Some(AuditWriterStopReport {
                fully_stopped: false,
                errors: vec![error],
            });
            state_ready.notify_all();
        }
    }

    fn wait_bounded(&self, timeout: std::time::Duration) -> AuditWriterStopReport {
        self.begin_join();
        let (state_lock, state_ready) = &*self.join_state;
        let mut state = state_lock.lock();
        if state.outcome.is_none() {
            let deadline = std::time::Instant::now() + timeout;
            while state.outcome.is_none() {
                if state_ready.wait_until(&mut state, deadline).timed_out()
                    && state.outcome.is_none()
                {
                    return AuditWriterStopReport {
                        fully_stopped: false,
                        errors: vec![format!(
                            "durable orchestration audit writer did not stop within {timeout:?}"
                        )],
                    };
                }
            }
        }
        state
            .outcome
            .clone()
            .unwrap_or_else(|| AuditWriterStopReport {
                fully_stopped: false,
                errors: vec!["orchestration audit join outcome was unavailable".to_string()],
            })
    }
}

impl Drop for AuditWriter {
    fn drop(&mut self) {
        let _ = self.wait_bounded(std::time::Duration::from_secs(30));
    }
}

impl OrchStore {
    /// Open store and convert unfinished runs to `interrupted` (crash recovery).
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        // Authority *before* mutation (#455). Establishing the lease either
        // binds to the runtime that owns this home or takes the home's OS lock
        // and keeps it; either way nothing below — not one directory, not the
        // store lock — is created on a home this process may not write.
        let write_lease = crate::host_runtime::WriteLease::for_store_root(&root);
        // Bound to a name: the guard must be *held* across the layout creation
        // and the store lock below, not merely taken and dropped. A dropped
        // guard is a check, and a check can go stale between its answer and
        // the mutation it was supposed to authorize (#455).
        let _open_write = write_lease
            .begin("opening the durable orchestration ledger")
            .context("durable-write authority for the orchestration ledger")?;
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
        fs::create_dir_all(root.join("work-decisions"))?;
        fs::create_dir_all(root.join("messages"))?;
        fs::create_dir_all(root.join("worker-presence"))?;
        fs::create_dir_all(root.join("managed-intents"))?;
        fs::create_dir_all(root.join("managed-finalization"))?;
        fs::create_dir_all(root.join("manager-plans"))?;
        fs::create_dir_all(root.join("manager-decisions"))?;
        fs::create_dir_all(root.join("manager-intents"))?;
        fs::create_dir_all(root.join("work-intents"))?;
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
        let join_state = Arc::new((Mutex::new(AuditWriterJoinState::default()), Condvar::new()));
        // The audit writer runs on its own thread and outlives any single
        // call, so it carries the same lease every other durable effect does.
        // The lease is shared, so rebinding the store rebinds the writer.
        let write_lease = Arc::new(Mutex::new(write_lease));
        let writer_lease = write_lease.clone();
        let audit_join = std::thread::Builder::new()
            .name("grokptah-orchestration-audit".into())
            .spawn(move || {
                while let Ok(entry) = audit_rx.recv() {
                    let _guard = writer_lock.lock();
                    let lease = writer_lease.lock().clone();
                    let result = append_audit_entry(&lease, &audit_root, &entry);
                    if let Err(error) = result {
                        *writer_error.lock() = Some(error.to_string());
                    }
                }
            })?;
        let store = Self {
            inner: Arc::new(OrchStoreInner {
                // Bound to whichever runtime owns this home at open time. A
                // store opened outside any runtime stays unbound and may only
                // write while the home is genuinely unowned.
                write_lease,
                root,
                _store_lock: store_lock,
                lock: Mutex::new(()),
                last_run_error: Mutex::new(None),
                last_audit_error: last_audit_error.clone(),
                audit_file_lock,
                audit_writer: AuditWriter {
                    tx: Mutex::new(Some(audit_tx)),
                    join: Mutex::new(Some(audit_join)),
                    join_state,
                    last_audit_error: last_audit_error.clone(),
                },
            }),
        };
        store.recover_agent_activation_intents()?;
        store.recover_finalization_intents()?;
        store.recover_routine_intents()?;
        store.recover_managed_finalization_intents()?;
        store.recover_manager_creation_intents()?;
        store.recover_work_mutation_intents()?;
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

    fn manager_plan_path(&self, plan_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(plan_id)?;
        Ok(self
            .inner
            .root
            .join("manager-plans")
            .join(format!("{safe}.json")))
    }

    fn manager_decision_path(&self, decision_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("manager-decisions")
            .join(format!("{}.json", safe_id_filename(decision_id)?)))
    }

    fn manager_creation_intent_path(&self, plan_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("manager-intents")
            .join(format!("{}.json", safe_id_filename(plan_id)?)))
    }

    fn work_mutation_intent_path(&self, intent_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("work-intents")
            .join(format!("{}.json", safe_id_filename(intent_id)?)))
    }

    pub fn save_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        let result = self
            .run_path(&run.run_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .and_then(|path| atomic_write_json(&self.lease(), &path, run));
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
            agent.current_run_id.is_none(),
            "persistent Agent already has an active Run"
        );
        anyhow::ensure!(
            agent.known_lane_ids().contains(&run.session_id),
            "Run Lane is not currently associated with the persistent Agent"
        );
        anyhow::ensure!(
            run.agent_spec_revision == Some(agent.current_spec()?.revision),
            "Run Agent specification revision is stale"
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
            prior_run: None,
        };
        atomic_write_json(&self.lease(), &intent_path, &intent)?;
        if let Err(error) = atomic_write_json(&self.lease(), &run_path, run) {
            let run_rollback = match fs::symlink_metadata(&run_path) {
                Ok(_) => remove_file_durable(&self.lease(), &run_path),
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(())
                }
                Err(metadata_error) => Err(metadata_error.into()),
            };
            if run_rollback.is_err() || remove_file_durable(&self.lease(), &intent_path).is_err() {
                return Err(error.context(
                    "persist Agent activation Run; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent activation Run"));
        }
        if let Err(error) = atomic_write_json(&self.lease(), &agent_path, &agent) {
            if remove_file_durable(&self.lease(), &run_path).is_err() {
                return Err(error.context(
                    "persist Agent activation; durable recovery intent requires restart",
                ));
            }
            if remove_file_durable(&self.lease(), &intent_path).is_err() {
                return Err(error.context(
                    "persist Agent activation; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent activation"));
        }
        if let Err(error) = remove_file_durable(&self.lease(), &intent_path) {
            // Both authoritative records are installed. Keep the recovery
            // intent as a safe replay anchor; terminal finalization removes
            // it before replacing the Run.
            *self.inner.last_run_error.lock() = Some(error.to_string());
        }
        Ok(())
    }

    /// Atomically promote a durable queued Run and activate its Agent.
    ///
    /// The recovery intent records both sides of the queued-to-running
    /// replacement so a crash before either pointer write converges safely.
    pub fn promote_queued_run_and_activate_agent(
        &self,
        run_id: &str,
        agent_id: &str,
        start_seq: u64,
    ) -> anyhow::Result<Option<RunRecord>> {
        let _g = self.inner.lock.lock();
        let activation_dir = self.inner.root.join("agent-activation");
        if fs::read_dir(&activation_dir)?.any(|entry| {
            entry.ok().is_some_and(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            })
        }) {
            anyhow::bail!("a prior Agent activation requires restart recovery");
        }
        let Some(prior_run) = self.load_run_unlocked(run_id)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            prior_run.state == RunState::Queued,
            "Run is no longer queued"
        );
        anyhow::ensure!(
            prior_run.agent_id.as_deref() == Some(agent_id),
            "Run Agent identity does not match activation target"
        );
        let agent_path = self
            .agent_path(agent_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(agent_path.is_file(), "persistent Agent record is missing");
        let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&agent_path)?)?;
        agent
            .migrate_legacy_spec()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            agent.current_run_id.is_none(),
            "persistent Agent already has an active Run"
        );
        anyhow::ensure!(
            agent.known_lane_ids().contains(&prior_run.session_id),
            "Run Lane is not currently associated with the persistent Agent"
        );
        anyhow::ensure!(
            prior_run.agent_spec_revision == Some(agent.current_spec()?.revision),
            "Run Agent specification revision is stale"
        );

        let mut run = prior_run.clone();
        run.state = RunState::Running;
        run.queue_position = None;
        run.start_seq = Some(start_seq);
        run.updated_at = Utc::now();
        agent.state = AgentState::Active;
        agent.current_run_id = Some(run.run_id.clone());
        agent.last_lane_id = Some(run.session_id);
        agent.updated_at = Utc::now();
        agent
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let run_path = self
            .run_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let intent_path = self
            .agent_activation_path(run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let intent = AgentActivationIntent {
            run: run.clone(),
            activated_agent: agent.clone(),
            prior_run: Some(prior_run.clone()),
        };
        atomic_write_json(&self.lease(), &intent_path, &intent)?;
        if let Err(error) = atomic_write_json(&self.lease(), &run_path, &run) {
            if remove_file_durable(&self.lease(), &intent_path).is_err() {
                return Err(
                    error.context("promote queued Run; durable recovery intent requires restart")
                );
            }
            return Err(error.context("promote queued Run"));
        }
        if let Err(error) = atomic_write_json(&self.lease(), &agent_path, &agent) {
            if atomic_write_json(&self.lease(), &run_path, &prior_run).is_err()
                || remove_file_durable(&self.lease(), &intent_path).is_err()
            {
                return Err(error
                    .context("activate promoted Run; durable recovery intent requires restart"));
            }
            return Err(error.context("activate promoted Run"));
        }
        if let Err(error) = remove_file_durable(&self.lease(), &intent_path) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
        }
        Ok(Some(run))
    }

    /// Release a terminal Run's Agent pointer even when checkpoint creation
    /// fails. A different active Run is never disturbed.
    pub fn deactivate_agent_run(
        &self,
        agent_id: &str,
        run_id: &str,
        failed: bool,
    ) -> anyhow::Result<()> {
        self.update_agent(agent_id, |agent| {
            match agent.current_run_id.as_deref() {
                Some(current) if current == run_id => {
                    agent.current_run_id = None;
                    agent.last_run_id = Some(run_id.to_string());
                    agent.state = if failed {
                        AgentState::Failed
                    } else {
                        AgentState::Waiting
                    };
                }
                Some(_) => anyhow::bail!("persistent Agent is active on a different Run"),
                None => {}
            }
            Ok(())
        })?
        .ok_or_else(|| anyhow::anyhow!("persistent Agent disappeared during deactivation"))?;
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
        if let Err(error) = atomic_write_json(&self.lease(), &path, &run) {
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
        // Every durable writer must enforce the lane-local dependency graph,
        // not only the public create endpoint. Manager adoption, supervisor
        // reconciliation, and host recovery all converge here. Exclude the
        // current record when updating it so validation sees one candidate.
        let existing = self.load_work_item_unlocked(&item.work_id)?;
        let is_new = existing.is_none();
        let previous_dependencies = existing.map(|record| record.dependencies);
        // Dependency-free creates used to skip the graph ceiling. New items
        // count against it; an update of an item already at the ceiling must
        // still land.
        if is_new {
            let lane = self.scoped_work_items_unlocked(super::graph::GraphScope::of(item))?;
            if lane.len() >= super::graph::MAX_GRAPH_SCOPE_ITEMS {
                return Err(anyhow::Error::new(OrchError::new(
                    OrchErrorCode::CapacityExhausted,
                    format!(
                        "scope holds more than {} work items; a new item is refused rather than unbounded",
                        super::graph::MAX_GRAPH_SCOPE_ITEMS
                    ),
                )));
            }
        }
        if !item.dependencies.is_empty()
            && previous_dependencies.as_ref() != Some(&item.dependencies)
        {
            let scope = super::graph::GraphScope::of(item);
            let mut lane = self.scoped_work_items_unlocked(super::graph::GraphScope::of(item))?;
            lane.retain(|existing| existing.work_id != item.work_id && scope.contains(existing));
            super::graph::validate_scoped_dependency_graph(&lane, item, scope)
                .map_err(anyhow::Error::new)?;
        }
        let path = self
            .work_item_path(&item.work_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&self.lease(), &path, item)
    }

    fn save_work_item_unchecked_unlocked(&self, item: &WorkItem) -> anyhow::Result<()> {
        item.validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let path = self
            .work_item_path(&item.work_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        atomic_write_json(&self.lease(), &path, item)
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
        atomic_write_json(&self.lease(), &path, attempt)
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
        // Total order, not the previous partial one: two items created inside
        // the same clock tick used to fall back to `read_dir` order, which is
        // not reproducible across hosts or across restarts on one host.
        super::graph::order_work(&mut out);
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

    /// Import a pre-existing record for migration/fixture setup without
    /// re-validating edges against the rest of the ledger. Runtime mutation
    /// paths must use [`Self::save_work_item`].
    pub fn save_work_item_unchecked(&self, item: &WorkItem) -> anyhow::Result<()> {
        let _guard = self.inner.lock.lock();
        self.save_work_item_unchecked_unlocked(item)
    }

    pub fn load_work_item(&self, work_id: &str) -> anyhow::Result<Option<WorkItem>> {
        let _guard = self.inner.lock.lock();
        self.load_work_item_unlocked(work_id)
    }

    pub fn list_work_items(&self) -> anyhow::Result<Vec<WorkItem>> {
        let _guard = self.inner.lock.lock();
        self.list_work_items_unlocked()
    }

    /// The stored work of one lane, in the ledger's total order.
    ///
    /// This is what a writer validates a dependency graph against: it must see
    /// its own lane and nothing else, so a dependency declaration cannot be
    /// used to probe for work in another session or workspace.
    fn scoped_work_items_unlocked(
        &self,
        scope: super::graph::GraphScope<'_>,
    ) -> anyhow::Result<Vec<WorkItem>> {
        let mut items = Vec::new();
        let dir = self.inner.root.join("work-items");
        if !dir.is_dir() {
            return Ok(items);
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let item: WorkItem = serde_json::from_str(&fs::read_to_string(path)?)?;
            item.validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if scope.contains(&item) {
                items.push(item);
                if items.len() > super::graph::MAX_GRAPH_SCOPE_ITEMS {
                    return Err(anyhow::Error::new(OrchError::new(
                        OrchErrorCode::CapacityExhausted,
                        format!(
                            "lane contains more than {} work items; bounded read refused",
                            super::graph::MAX_GRAPH_SCOPE_ITEMS
                        ),
                    )));
                }
            }
        }
        super::graph::order_work(&mut items);
        Ok(items)
    }

    pub fn scoped_work_items(
        &self,
        session_id: Uuid,
        workspace: &str,
    ) -> anyhow::Result<Vec<WorkItem>> {
        let _guard = self.inner.lock.lock();
        self.scoped_work_items_unlocked(super::graph::GraphScope {
            session_id,
            workspace,
        })
    }

    /// The lane-scoped, redacted operator view of the durable work graph.
    ///
    /// Every node carries the lane's own coordinates and typed enumerations
    /// only; see [`super::graph::WorkGraphNode`] for what is withheld and why.
    pub fn work_graph_scoped(
        &self,
        session_id: Uuid,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<Vec<super::graph::WorkGraphNode>> {
        let _guard = self.inner.lock.lock();
        let scope = super::graph::GraphScope {
            session_id,
            workspace,
        };
        let items = self.scoped_work_items_unlocked(scope)?;
        Ok(super::graph::project_scoped_graph(&items, scope, now))
    }

    pub fn list_work_attempts(&self, work_id: Option<&str>) -> anyhow::Result<Vec<WorkAttempt>> {
        let _guard = self.inner.lock.lock();
        self.list_work_attempts_unlocked(work_id)
    }

    pub fn load_work_attempt(&self, attempt_id: &str) -> anyhow::Result<Option<WorkAttempt>> {
        let _guard = self.inner.lock.lock();
        self.load_work_attempt_unlocked(attempt_id)
    }

    /// Authoritative Work for a local executor Run, if this Run is Work-bound.
    ///
    /// Binding is the durable WorkAttempt `linked_run_ids` set and any
    /// managed-execution intent that recorded this Run. Missing Work after a
    /// binding is found fails closed rather than treating the Run as unbound.
    pub fn work_item_for_run(&self, run_id: &str) -> Result<Option<WorkItem>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.work_item_for_run_unlocked(run_id)
    }

    fn work_item_for_run_unlocked(&self, run_id: &str) -> Result<Option<WorkItem>, OrchError> {
        let mut matched: Option<String> = None;
        let attempts = self
            .list_work_attempts_unlocked(None)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        for attempt in attempts {
            if attempt.linked_run_ids.iter().any(|id| id == run_id) {
                if let Some(existing) = matched.as_ref() {
                    if existing != &attempt.work_id {
                        return Err(OrchError::new(
                            OrchErrorCode::Conflict,
                            "run is linked to more than one Work item",
                        ));
                    }
                } else {
                    matched = Some(attempt.work_id.clone());
                }
            }
        }
        for intent in self.list_managed_intents_unlocked()? {
            if intent.run_id.as_deref() == Some(run_id) {
                if let Some(existing) = matched.as_ref() {
                    if existing != &intent.work_id {
                        return Err(OrchError::new(
                            OrchErrorCode::Conflict,
                            "run is linked to more than one Work item",
                        ));
                    }
                } else {
                    matched = Some(intent.work_id.clone());
                }
            }
        }
        let Some(work_id) = matched else {
            return Ok(None);
        };
        match self
            .load_work_item_unlocked(&work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            Some(item) => Ok(Some(item)),
            None => Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("Work-bound run {run_id} is missing its Work item"),
            )),
        }
    }

    // --- Durable manager plans -------------------------------------------

    pub fn save_manager_plan_with_root(
        &self,
        plan: &ManagerPlan,
        root_work: &WorkItem,
    ) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        plan.validate()?;
        root_work
            .validate()
            .map_err(|error| OrchError::new(OrchErrorCode::InvalidRequest, error.message))?;
        if !root_work.is_container
            || root_work.work_id != plan.root_work_id
            || root_work.session_id != plan.session_id
            || root_work.workspace != plan.workspace
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "manager root Work does not match its plan",
            ));
        }
        let intent = ManagerCreationIntent {
            plan: plan.clone(),
            root_work: root_work.clone(),
        };
        let intent_path = self.manager_creation_intent_path(&plan.plan_id)?;
        atomic_write_json(&self.lease(), &intent_path, &intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.commit_manager_creation_intent_unlocked(&intent)?;
        remove_file_durable(&self.lease(), &intent_path)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn commit_manager_creation_intent_unlocked(
        &self,
        intent: &ManagerCreationIntent,
    ) -> Result<(), OrchError> {
        self.save_work_item_unlocked(&intent.root_work)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_manager_plan_unlocked(&intent.plan)
    }

    fn recover_manager_creation_intents(&self) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("manager-intents");
        for entry in fs::read_dir(&dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: ManagerCreationIntent = serde_json::from_str(
                &fs::read_to_string(&path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            intent.plan.validate()?;
            intent.root_work.validate()?;
            self.commit_manager_creation_intent_unlocked(&intent)?;
            remove_file_durable(&self.lease(), &path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        Ok(())
    }

    fn persist_work_mutation_intent_unlocked(
        &self,
        intent: &WorkMutationIntent,
    ) -> Result<(), OrchError> {
        intent.validate()?;
        let path = self.work_mutation_intent_path(&intent.intent_id)?;
        atomic_write_json(&self.lease(), &path, intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn clear_work_mutation_intent_unlocked(&self, intent_id: &str) -> Result<(), OrchError> {
        let path = self.work_mutation_intent_path(intent_id)?;
        remove_file_durable(&self.lease(), &path)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn load_work_decision_unlocked(
        &self,
        work_id: &str,
        decision_id: &str,
    ) -> Result<Option<WorkDecision>, OrchError> {
        let path = self.work_decision_path(work_id, decision_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let decision: WorkDecision = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        decision.validate()?;
        Ok(Some(decision))
    }

    fn commit_work_mutation_intent_unlocked(
        &self,
        intent: &WorkMutationIntent,
    ) -> Result<(), OrchError> {
        intent.validate()?;
        let current = self
            .load_work_item_unlocked(&intent.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let current_digest = current
            .as_ref()
            .map(|item| {
                serde_json::to_value(item)
                    .map(|value| super::hash_payload(&value))
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
            });
        let current_digest = current_digest.transpose()?;
        if current_digest.as_deref() != Some(intent.prior_item_digest.as_str())
            && current_digest.as_deref() != Some(intent.next_item_digest.as_str())
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work mutation recovery found an unexpected prior revision",
            ));
        }
        if current_digest.as_deref() == Some(intent.prior_item_digest.as_str())
            && current
                .as_ref()
                .is_none_or(|item| item.revision != intent.expected_revision)
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work mutation recovery found an unexpected expected_revision",
            ));
        }
        if let Some(decision) =
            self.load_work_decision_unlocked(&intent.work_id, &intent.decision.decision_id)?
        {
            let digest = super::hash_payload(
                &serde_json::to_value(&decision)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            );
            if digest != intent.decision_digest || decision != intent.decision {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "work mutation recovery found a conflicting decision receipt",
                ));
            }
        } else {
            self.save_work_decision_unlocked(&intent.decision)?;
        }
        if current_digest.as_deref() != Some(intent.next_item_digest.as_str()) {
            self.save_work_item_unlocked(&intent.item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        self.clear_work_mutation_intent_unlocked(&intent.intent_id)
    }

    fn recover_work_mutation_intents(&self) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("work-intents");
        for entry in fs::read_dir(&dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: WorkMutationIntent = serde_json::from_str(
                &fs::read_to_string(&path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            self.commit_work_mutation_intent_unlocked(&intent)?;
        }
        Ok(())
    }

    pub fn save_manager_plan(&self, plan: &ManagerPlan) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        self.save_manager_plan_unlocked(plan)
    }

    pub fn save_manager_plan_with_work(
        &self,
        plan: &ManagerPlan,
        work_items: &[WorkItem],
    ) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        for item in work_items {
            self.save_work_item_unlocked(item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        self.save_manager_plan_unlocked(plan)
    }

    /// Compare-and-swap a plan revision and its newly materialized Work under
    /// one process-store lock. The Work-first write order remains recoverable
    /// after a crash, while the CAS prevents concurrent ticks from producing
    /// two children for one logical step.
    pub fn save_manager_plan_with_work_cas(
        &self,
        plan: &ManagerPlan,
        expected_revision: u64,
        work_items: &[WorkItem],
    ) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        let current = self
            .load_manager_plan_unlocked(&plan.plan_id)?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown plan_id"))?;
        if current.revision != expected_revision {
            return Err(OrchError::new(
                OrchErrorCode::StaleVersion,
                "manager plan changed before the mutation could be committed",
            ));
        }
        for item in work_items {
            self.save_work_item_unlocked(item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        self.save_manager_plan_unlocked(plan)
    }

    pub fn save_manager_plan_cas(
        &self,
        plan: &ManagerPlan,
        expected_revision: u64,
    ) -> Result<(), OrchError> {
        self.save_manager_plan_with_work_cas(plan, expected_revision, &[])
    }

    pub fn load_manager_plan(&self, plan_id: &str) -> Result<Option<ManagerPlan>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_manager_plan_unlocked(plan_id)
    }

    pub fn list_manager_plans(&self) -> Result<Vec<ManagerPlan>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.list_manager_plans_unlocked()
    }

    fn save_manager_plan_unlocked(&self, plan: &ManagerPlan) -> Result<(), OrchError> {
        plan.validate()?;
        let path = self.manager_plan_path(&plan.plan_id)?;
        atomic_write_json(&self.lease(), &path, plan)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn load_manager_plan_unlocked(&self, plan_id: &str) -> Result<Option<ManagerPlan>, OrchError> {
        let path = match self.manager_plan_path(plan_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let plan: ManagerPlan = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        plan.validate()?;
        Ok(Some(plan))
    }

    fn list_manager_plans_unlocked(&self) -> Result<Vec<ManagerPlan>, OrchError> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("manager-plans");
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let plan: ManagerPlan = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            plan.validate()?;
            out.push(plan);
        }
        out.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(out)
    }

    pub fn save_manager_decision_with_work(
        &self,
        decision: &ManagerDecisionRecord,
        work: &WorkItem,
    ) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        decision.validate()?;
        if let Some(existing) = self
            .load_work_item_unlocked(&work.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            if existing.source_manager_plan_id != work.source_manager_plan_id
                || existing.kind != work.kind
            {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "manager decision Work ID is already owned by another occurrence",
                ));
            }
        } else {
            self.save_work_item_unlocked(work)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        self.save_manager_decision_unlocked(decision)
    }

    pub fn save_manager_decision(&self, decision: &ManagerDecisionRecord) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        self.save_manager_decision_unlocked(decision)
    }

    fn save_manager_decision_unlocked(
        &self,
        decision: &ManagerDecisionRecord,
    ) -> Result<(), OrchError> {
        decision.validate()?;
        atomic_write_json(
            &self.lease(),
            &self.manager_decision_path(&decision.decision_id)?,
            decision,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn load_manager_decision(
        &self,
        decision_id: &str,
    ) -> Result<Option<ManagerDecisionRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        let path = self.manager_decision_path(decision_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let decision: ManagerDecisionRecord = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        decision.validate()?;
        Ok(Some(decision))
    }

    pub fn list_manager_decisions(
        &self,
        plan_id: Option<&str>,
    ) -> Result<Vec<ManagerDecisionRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut out = Vec::new();
        let dir = self.inner.root.join("manager-decisions");
        for entry in fs::read_dir(dir)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            let path = entry
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let decision: ManagerDecisionRecord = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            decision.validate()?;
            if plan_id.is_none_or(|id| decision.plan_id == id) {
                out.push(decision);
            }
        }
        out.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(out)
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
            remove_file_durable(&self.lease(), &path)?;
        }
        Ok(())
    }

    fn save_routine_unlocked(&self, routine: &RoutineRecord) -> Result<(), OrchError> {
        routine.validate()?;
        let path = self.routine_path(&routine.routine_id)?;
        atomic_write_json(&self.lease(), &path, routine)
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
        work.assignment_status = AssignmentStatus::Accepted;
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
        atomic_write_json(&self.lease(), &path, intent)
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
        atomic_write_json(&self.lease(), &path, &intent.activation)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if let Some(dedupe) = &intent.dedupe {
            let dedupe_path =
                self.routine_dedupe_path(&intent.activation.routine_id, &dedupe.dedupe_key)?;
            atomic_write_json(&self.lease(), &dedupe_path, dedupe)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        Ok(())
    }

    fn clear_routine_intent_unlocked(&self, activation_id: &str) -> Result<(), OrchError> {
        let path = self.routine_intent_path(activation_id)?;
        remove_file_durable(&self.lease(), &path)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn prune_activations_unlocked(&self, routine_id: &str) -> Result<(), OrchError> {
        let activations = self.list_activations_unlocked(routine_id, usize::MAX)?;
        if activations.len() <= MAX_ACTIVATION_HISTORY {
            return Ok(());
        }
        for activation in activations.into_iter().skip(MAX_ACTIVATION_HISTORY) {
            let path = self.routine_activation_path(routine_id, &activation.activation_id)?;
            let _ = remove_file_durable(&self.lease(), &path);
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
            verification: None,
        }
    }

    /// Classify completion-oracle evidence bound to this work/run/attempt.
    /// Missing IDs are unbound (not success authority). Present IDs that do
    /// not match this exact work, attempt, and an allowed run are refused.
    fn bound_completion_evidence<'a>(
        evidence: Option<&'a CompletionEvidence>,
        work_id: &str,
        attempt_id: &str,
        linked_run_ids: &[String],
        extra_run_id: Option<&str>,
    ) -> Result<Option<&'a CompletionEvidence>, OrchError> {
        let Some(evidence) = evidence else {
            return Ok(None);
        };
        let has_any = evidence.work_id.is_some()
            || evidence.run_id.is_some()
            || evidence.attempt_id.is_some();
        let has_all = evidence.work_id.is_some()
            && evidence.run_id.is_some()
            && evidence.attempt_id.is_some();
        if has_any && !has_all {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "completion evidence must bind work, run, and attempt together",
            ));
        }
        if !has_all {
            return Ok(None);
        }
        let run_id = evidence.run_id.as_deref().unwrap_or_default();
        let run_allowed = linked_run_ids.iter().any(|id| id == run_id)
            || extra_run_id.is_some_and(|id| id == run_id);
        if evidence.work_id.as_deref() != Some(work_id)
            || evidence.attempt_id.as_deref() != Some(attempt_id)
            || !run_allowed
        {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "completion evidence is not bound to this work/run/attempt",
            ));
        }
        Ok(Some(evidence))
    }

    fn success_is_authorized_unlocked(
        &self,
        item: &WorkItem,
        attempt: Option<&WorkAttempt>,
        result: Option<&WorkResult>,
        extra_run_id: Option<&str>,
    ) -> Result<bool, OrchError> {
        if item.approval.is_some() {
            return Ok(true);
        }
        let Some(result) = result.or(item.result.as_ref()) else {
            return Ok(false);
        };
        if result.failure.is_some() || result.cancellation_reason.is_some() {
            return Ok(false);
        }
        let Some(attempt) = attempt else {
            return Ok(false);
        };
        if attempt.work_id != item.work_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "completion attempt does not belong to this work item",
            ));
        }
        let Some(evidence) = Self::bound_completion_evidence(
            result.verification.as_ref(),
            &item.work_id,
            &attempt.attempt_id,
            &attempt.linked_run_ids,
            extra_run_id,
        )?
        else {
            return Ok(false);
        };
        if !evidence_authorizes_success(evidence) {
            return Ok(false);
        }
        let run_id = evidence.run_id.as_deref().unwrap_or_default();
        match self.load_run_unlocked(run_id) {
            Ok(Some(run)) => {
                if run.session_id != item.session_id
                    || !workspaces_match(&run.workspace, &item.workspace)
                {
                    return Err(OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        "completion evidence run is outside this work lane",
                    ));
                }
                let Some(run_evidence) = run.aggregates.verification.as_ref() else {
                    return Ok(false);
                };
                if !evidence_authorizes_success(run_evidence) {
                    return Ok(false);
                }
                let same_observation = evidence.status == run_evidence.status
                    && evidence.stop_reason == run_evidence.stop_reason
                    && evidence.interrupted == run_evidence.interrupted
                    && evidence.claims == run_evidence.claims
                    && evidence.observations == run_evidence.observations
                    && evidence.usage == run_evidence.usage;
                if !same_observation {
                    return Err(OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        "completion evidence does not match the durable Run observation",
                    ));
                }
                Ok(true)
            }
            Ok(None) => Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "completion evidence run does not exist",
            )),
            Err(error) => Err(OrchError::new(OrchErrorCode::Internal, error.to_string())),
        }
    }

    fn ordinary_completion_states(item: &WorkItem) -> (AttemptState, WorkState) {
        if item.policy.requires_approval {
            (AttemptState::AwaitingApproval, WorkState::AwaitingApproval)
        } else {
            (AttemptState::Review, WorkState::Review)
        }
    }

    fn bind_managed_completion_result_unlocked(
        &self,
        intent: &ManagedExecutionIntent,
        result: Option<WorkResult>,
        attempt: Option<&WorkAttempt>,
    ) -> Result<Option<WorkResult>, OrchError> {
        let Some(mut result) = result else {
            return Ok(None);
        };
        if result.verification.is_some() {
            return Ok(Some(result));
        }
        let Some(run_id) = intent.run_id.as_deref() else {
            return Ok(Some(result));
        };
        let run = match self.load_run_unlocked(run_id) {
            Ok(run) => run,
            Err(error) => return Err(OrchError::new(OrchErrorCode::Internal, error.to_string())),
        };
        if let Some(mut evidence) = run.and_then(|run| run.aggregates.verification) {
            if evidence_authorizes_success(&evidence) {
                evidence.work_id = Some(intent.work_id.clone());
                evidence.run_id = Some(run_id.to_string());
                evidence.attempt_id = intent
                    .attempt_id
                    .clone()
                    .or_else(|| attempt.map(|attempt| attempt.attempt_id.clone()));
                result.verification = Some(evidence);
            }
        }
        Ok(Some(result))
    }

    fn completion_result_replays(stored: &WorkResult, incoming: &WorkResult) -> bool {
        stored.summary == incoming.summary
            && stored.evidence == incoming.evidence
            && stored.artifacts == incoming.artifacts
            && stored.failure == incoming.failure
            && stored.cancellation_reason == incoming.cancellation_reason
            && stored.verification == incoming.verification
    }

    /// The only production writer of `WorkState::Succeeded`.
    fn assign_work_succeeded_unlocked(
        &self,
        item: &mut WorkItem,
        attempt: Option<&mut WorkAttempt>,
        extra_run_id: Option<&str>,
    ) -> Result<(), OrchError> {
        if !self.success_is_authorized_unlocked(
            item,
            attempt.as_deref(),
            item.result.as_ref(),
            extra_run_id,
        )? {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item cannot enter succeeded without verified completion evidence or durable approval",
            ));
        }
        item.state = WorkState::Succeeded;
        if let Some(attempt) = attempt {
            attempt.state = AttemptState::Succeeded;
        }
        Ok(())
    }

    fn refresh_work_item_unlocked(&self, item: &mut WorkItem) -> anyhow::Result<()> {
        self.refresh_work_item_at_unlocked(item, Utc::now())
    }

    /// Resolve one item's declared dependencies inside its own lane.
    ///
    /// The previous implementation looked each dependency up by id across the
    /// whole ledger, so an item in one session could name work in another and
    /// then read that work's progress out of its own `Blocked`/`Queued`
    /// transitions. Resolution is scoped now, and an id outside the lane is
    /// reported exactly as one that does not exist.
    fn resolve_dependencies_unlocked(
        &self,
        item: &WorkItem,
    ) -> anyhow::Result<super::graph::DependencyStates> {
        if item.dependencies.is_empty() {
            return Ok(super::graph::DependencyStates::new());
        }
        let scope = super::graph::GraphScope::of(item);
        let mut states = super::graph::DependencyStates::new();
        for dependency in &item.dependencies {
            // Same per-id read the unscoped version did, plus the scope
            // predicate -- so this costs what it always cost, and an
            // unreadable record resolves to `None` rather than to a guess.
            let resolved = self
                .load_work_item_unlocked(&dependency.work_id)
                .ok()
                .flatten()
                .filter(|candidate| scope.contains(candidate))
                .map(|candidate| candidate.state);
            states.insert(dependency.work_id.clone(), resolved);
        }
        Ok(states)
    }

    fn refresh_work_item_at_unlocked(
        &self,
        item: &mut WorkItem,
        now: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if item.state.is_terminal() {
            return Ok(());
        }
        let states = self.resolve_dependencies_unlocked(item)?;
        let admission = super::graph::evaluate_admission(item, &states, now);
        // Admission is consulted before the container skip: a manual hold on a
        // container remains ManuallyBlocked, and the re-queue branch below
        // must not treat Container as "not waiting". The previous ordering
        // returned Container first, so a held container looked ready to lift.
        // The deadline below is not suspended by an operator hold: it is a
        // hard bound. Containers themselves are still not executed.
        let manually_held = admission == super::graph::AdmissionBlock::ManuallyBlocked;
        let waiting = !manually_held
            && matches!(
                admission,
                super::graph::AdmissionBlock::DependenciesPending
                    | super::graph::AdmissionBlock::DependencyUnsatisfiable
                    | super::graph::AdmissionBlock::DependencyUnresolved
            );
        if waiting && matches!(item.state, WorkState::Queued) {
            item.state = WorkState::Blocked;
            item.block_provenance = Some(BlockProvenance::Derived);
            item.blocked_reason = Some(admission.as_str().to_string());
            item.bump();
            self.save_work_item_unlocked(item)?;
        } else if !waiting && !manually_held && matches!(item.state, WorkState::Blocked) {
            item.state = WorkState::Queued;
            item.block_provenance = None;
            item.blocked_reason = None;
            item.bump();
            self.save_work_item_unlocked(item)?;
        } else if waiting && matches!(item.state, WorkState::Blocked) {
            // The hold stands but its cause may have changed -- a dependency
            // that was pending can become unsatisfiable. The same write stamps
            // a proven pre-upgrade derived wait with typed provenance so the
            // next tick does not have to recognize the legacy shape again.
            let reason = Some(admission.as_str().to_string());
            if item.blocked_reason != reason
                || item.block_provenance != Some(BlockProvenance::Derived)
            {
                item.block_provenance = Some(BlockProvenance::Derived);
                item.blocked_reason = reason;
                item.bump();
                self.save_work_item_unlocked(item)?;
            }
        }
        if item.is_container || admission == super::graph::AdmissionBlock::Container {
            return Ok(());
        }
        if item.deadline.is_some_and(|deadline| deadline <= now)
            && !item.state.is_terminal()
            && !item.state.is_review_gate()
        {
            item.state = WorkState::Failed;
            item.block_provenance = None;
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
                if !attempt.state.requires_lease_heartbeat() || now < attempt.lease_expires_at {
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
        item.assigned_agent_id = assigned_agent_id.clone();
        item.assignment_status = if assigned_agent_id.is_some() {
            AssignmentStatus::Accepted
        } else {
            AssignmentStatus::Unassigned
        };
        item.validate()?;
        item.bump();
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(item)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn offer_work(
        &self,
        work_id: &str,
        agent_id: &str,
        actor_id: &str,
        actor_agent_id: Option<&str>,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Offer,
                actor_id,
                actor_agent_id,
                assigned_agent_id: Some(agent_id),
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.state.is_terminal() {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "terminal work items cannot be offered",
                    ));
                }
                item.assigned_agent_id = Some(agent_id.to_string());
                item.assignment_status = AssignmentStatus::Offered;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn accept_work(
        &self,
        work_id: &str,
        agent_id: &str,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Accept,
                actor_id,
                actor_agent_id: Some(agent_id),
                assigned_agent_id: Some(agent_id),
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.assignment_status != AssignmentStatus::Offered
                    || item.assigned_agent_id.as_deref() != Some(agent_id)
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "work offer is not pending for this worker",
                    ));
                }
                item.assignment_status = AssignmentStatus::Accepted;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decline_work(
        &self,
        work_id: &str,
        agent_id: &str,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Decline,
                actor_id,
                actor_agent_id: Some(agent_id),
                assigned_agent_id: Some(agent_id),
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.assignment_status != AssignmentStatus::Offered
                    || item.assigned_agent_id.as_deref() != Some(agent_id)
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "work offer is not pending for this worker",
                    ));
                }
                item.assigned_agent_id = None;
                item.assignment_status = AssignmentStatus::Declined;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reassign_work(
        &self,
        work_id: &str,
        agent_id: &str,
        actor_id: &str,
        actor_agent_id: Option<&str>,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Reassign,
                actor_id,
                actor_agent_id,
                assigned_agent_id: Some(agent_id),
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.state.is_terminal()
                    || item.state == WorkState::Leased
                    || item.state == WorkState::Running
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "active or terminal work cannot be reassigned",
                    ));
                }
                item.assigned_agent_id = Some(agent_id.to_string());
                item.assignment_status = AssignmentStatus::Accepted;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reprioritize_work(
        &self,
        work_id: &str,
        priority: i32,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Reprioritize,
                actor_id,
                actor_agent_id: None,
                assigned_agent_id: None,
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.state.is_terminal() {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "terminal work cannot be reprioritized",
                    ));
                }
                item.priority = priority;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn block_work(
        &self,
        work_id: &str,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Block,
                actor_id,
                actor_agent_id: None,
                assigned_agent_id: None,
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.state.is_terminal() {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "terminal work cannot be blocked",
                    ));
                }
                if matches!(item.state, WorkState::Leased | WorkState::Running) {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "leased work cannot be blocked",
                    ));
                }
                item.state = WorkState::Blocked;
                // Typed provenance, so reconciliation can tell an operator
                // hold from its own dependency wait without parsing the
                // free-text reason.
                item.block_provenance = Some(BlockProvenance::Manual);
                item.blocked_reason = Some(reason.to_string());
                Ok(())
            },
        )
    }

    /// Release a block an operator placed, or an ambiguous legacy block whose
    /// provenance was never recorded.
    ///
    /// `WorkDecisionAction::Unblock` has been part of the decision vocabulary
    /// since the workload ledger landed but had no transition behind it, so a
    /// manual block was releasable only by the reconciliation bug this seam
    /// closes. Without this, making a manual hold durable would strand the
    /// work it holds.
    ///
    /// A derived block is deliberately not releasable here: it is
    /// reconciliation's own encoding of an unmet dependency, and clearing it
    /// by hand would be re-derived on the next tick. The proven pre-upgrade
    /// derived-wait shape is the same hold, just unstamped.
    pub fn unblock_work(
        &self,
        work_id: &str,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::Unblock,
                actor_id,
                actor_agent_id: None,
                assigned_agent_id: None,
                reason,
                expected_revision,
                now,
            },
            |item| {
                if item.state != WorkState::Blocked {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "work item is not blocked",
                    ));
                }
                if item.block_provenance == Some(BlockProvenance::Derived)
                    || super::graph::is_legacy_derived_wait(item)
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "a dependency block is released by reconciliation, not by an operator",
                    ));
                }
                item.state = WorkState::Queued;
                item.block_provenance = None;
                item.blocked_reason = None;
                Ok(())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_work_review(
        &self,
        work_id: &str,
        actor_id: &str,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::RequestReview,
                actor_id,
                actor_agent_id: None,
                assigned_agent_id: None,
                reason,
                expected_revision,
                now,
            },
            |item| {
                if !matches!(
                    item.state,
                    WorkState::Succeeded | WorkState::AwaitingApproval | WorkState::Review
                ) {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "review can be requested only for completed or approval-gated work",
                    ));
                }
                item.state = WorkState::Review;
                Ok(())
            },
        )
    }

    pub fn list_work_decisions(&self, work_id: &str) -> Result<Vec<WorkDecision>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.list_work_decisions_unlocked(work_id)
    }

    fn mutate_assignment(
        &self,
        request: AssignmentMutation<'_>,
        mutate: impl FnOnce(&mut WorkItem) -> Result<(), OrchError>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        if request.reason.trim().is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "decision reason is required",
            ));
        }
        let _guard = self.inner.lock.lock();
        let mut item = self
            .load_work_item_unlocked(request.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let prior_item = item.clone();
        Self::require_work_revision(&item, request.expected_revision)?;
        let (actor_agent_id, policy_revision) = self.resolve_assignment_agents_unlocked(
            &item,
            request.actor_agent_id,
            request.assigned_agent_id,
        )?;
        mutate(&mut item)?;
        let decision = WorkDecision {
            schema_version: WORKLOAD_SCHEMA_VERSION,
            decision_id: Uuid::new_v4().to_string(),
            work_id: request.work_id.to_string(),
            action: request.action,
            actor_id: request.actor_id.to_string(),
            actor_agent_id,
            assigned_agent_id: request
                .assigned_agent_id
                .map(str::to_string)
                .or_else(|| item.assigned_agent_id.clone()),
            policy_revision,
            work_revision: Some(item.revision),
            reason: request.reason.to_string(),
            created_at: request.now,
        };
        item.last_decision_id = Some(decision.decision_id.clone());
        item.bump_at(request.now);
        item.validate()?;
        // The intent is the durable commit marker for this pair. Either file
        // may be missing after a crash; reopening the store replays the exact
        // decision and item only when the observed prior/next digests agree.
        let intent = WorkMutationIntent::new(&prior_item, item.clone(), decision.clone())?;
        self.persist_work_mutation_intent_unlocked(&intent)?;
        self.commit_work_mutation_intent_unlocked(&intent)?;
        Ok((item, decision))
    }

    fn resolve_assignment_agents_unlocked(
        &self,
        item: &WorkItem,
        actor_agent_id: Option<&str>,
        assigned_agent_id: Option<&str>,
    ) -> Result<(Option<String>, Option<u64>), OrchError> {
        let actor = actor_agent_id
            .map(|agent_id| {
                self.require_agent_in_scope_unlocked(agent_id, item.session_id, &item.workspace)
            })
            .transpose()?;
        let assigned_agent_id = assigned_agent_id.or(item.assigned_agent_id.as_deref());
        let assigned = match assigned_agent_id {
            Some(agent_id)
                if actor
                    .as_ref()
                    .is_some_and(|agent| agent.agent_id == agent_id) =>
            {
                actor.clone()
            }
            Some(agent_id) => Some(self.require_agent_in_scope_unlocked(
                agent_id,
                item.session_id,
                &item.workspace,
            )?),
            None => None,
        };
        let policy_revision = actor
            .as_ref()
            .or(assigned.as_ref())
            .and_then(|agent| agent.spec.as_ref().map(|spec| spec.revision));
        Ok((actor.map(|agent| agent.agent_id), policy_revision))
    }

    /// Load an Agent and require it to belong to the requested session and
    /// workspace and still be an active identity.
    pub fn require_agent_in_scope(
        &self,
        agent_id: &str,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<AgentRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        self.require_agent_in_scope_unlocked(agent_id, session_id, workspace)
    }

    fn require_agent_in_scope_unlocked(
        &self,
        agent_id: &str,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<AgentRecord, OrchError> {
        let agent = self
            .load_agent_unlocked(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(OrchErrorCode::InvalidRequest, "unknown agent identity")
            })?;
        if !agent.known_lane_ids().contains(&session_id)
            || !workspaces_match(&agent.workspace, workspace)
        {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "agent is outside the requested session workspace",
            ));
        }
        if !agent.state.is_active_identity() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "agent identity is inactive",
            ));
        }
        Ok(agent)
    }

    fn save_work_decision_unlocked(&self, decision: &WorkDecision) -> Result<(), OrchError> {
        decision.validate()?;
        let path = self.work_decision_path(&decision.work_id, &decision.decision_id)?;
        atomic_write_json(&self.lease(), &path, decision)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn work_decision_path(&self, work_id: &str, decision_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("work-decisions")
            .join(safe_id_filename(work_id)?)
            .join(format!("{}.json", safe_id_filename(decision_id)?)))
    }

    fn list_work_decisions_unlocked(&self, work_id: &str) -> Result<Vec<WorkDecision>, OrchError> {
        let mut out = Vec::new();
        let dir = self
            .inner
            .root
            .join("work-decisions")
            .join(safe_id_filename(work_id)?);
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
            let decision: WorkDecision = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            decision.validate()?;
            out.push(decision);
        }
        out.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(out)
    }

    pub fn heartbeat_worker(
        &self,
        agent_id: &str,
        credential_id: &str,
        host_kind: WorkerHostKind,
        now: chrono::DateTime<Utc>,
    ) -> Result<WorkerPresence, OrchError> {
        let _guard = self.inner.lock.lock();
        self.write_worker_presence_unlocked(agent_id, credential_id, host_kind, now)
    }

    pub fn heartbeat_worker_scoped(
        &self,
        agent_id: &str,
        credential_id: &str,
        host_kind: WorkerHostKind,
        now: chrono::DateTime<Utc>,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<WorkerPresence, OrchError> {
        let _guard = self.inner.lock.lock();
        let agent = self.require_agent_in_scope_unlocked(agent_id, session_id, workspace)?;
        self.write_worker_presence_unlocked(&agent.agent_id, credential_id, host_kind, now)
    }

    fn write_worker_presence_unlocked(
        &self,
        agent_id: &str,
        credential_id: &str,
        host_kind: WorkerHostKind,
        now: chrono::DateTime<Utc>,
    ) -> Result<WorkerPresence, OrchError> {
        let presence = WorkerPresence::new(agent_id, credential_id, host_kind, now);
        let path = self.worker_presence_path(agent_id)?;
        atomic_write_json(&self.lease(), &path, &presence)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(presence)
    }

    pub fn list_workers(
        &self,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<WorkerProjection>, OrchError> {
        let _guard = self.inner.lock.lock();
        let agents = self
            .list_agents_unlocked()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let items = self
            .list_work_items_unlocked()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let attempts = self
            .list_work_attempts_unlocked(None)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let mut out = Vec::new();
        for agent in agents {
            if !workspaces_match(&agent.workspace, workspace) {
                continue;
            }
            let presence = self.load_worker_presence_unlocked(&agent.agent_id)?;
            out.push(WorkerProjection::project(
                &agent,
                agent.spec.as_ref(),
                presence.as_ref(),
                &items,
                &attempts,
                now,
                None,
            ));
        }
        Ok(out)
    }

    /// Durable work and attempts visible to a public scoped worker projection.
    /// Identity membership is fenced separately; this only bounds load counts
    /// to the requested lane and workspace.
    fn scoped_work_and_attempts_unlocked(
        &self,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<(Vec<WorkItem>, Vec<WorkAttempt>), OrchError> {
        let scope = super::graph::GraphScope {
            session_id,
            workspace,
        };
        let items = self
            .list_work_items_unlocked()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|item| scope.contains(item))
            .collect::<Vec<_>>();
        let scoped_ids = items
            .iter()
            .map(|item| item.work_id.clone())
            .collect::<HashSet<_>>();
        let attempts = self
            .list_work_attempts_unlocked(None)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|attempt| scoped_ids.contains(&attempt.work_id))
            .collect();
        Ok((items, attempts))
    }

    /// List only active workers attributable to the requested lane and
    /// workspace. This is the scoped read primitive used by public MCP
    /// observatory calls; the legacy workspace-only helper remains available
    /// to internal callers that already hold an equivalent scope.
    pub fn list_workers_scoped(
        &self,
        session_id: Uuid,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Vec<WorkerProjection>, OrchError> {
        let _guard = self.inner.lock.lock();
        let agents = self
            .list_agents_unlocked()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let (items, attempts) = self.scoped_work_and_attempts_unlocked(session_id, workspace)?;
        let mut out = Vec::new();
        for agent in agents {
            if !agent.state.is_active_identity()
                || !agent.known_lane_ids().contains(&session_id)
                || !workspaces_match(&agent.workspace, workspace)
            {
                continue;
            }
            let presence = self.load_worker_presence_unlocked(&agent.agent_id)?;
            out.push(WorkerProjection::project(
                &agent,
                agent.spec.as_ref(),
                presence.as_ref(),
                &items,
                &attempts,
                now,
                None,
            ));
        }
        out.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(out)
    }

    pub fn get_worker(
        &self,
        agent_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<WorkerProjection>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(agent) = self
            .load_agent_unlocked(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        else {
            return Ok(None);
        };
        let items = self
            .list_work_items_unlocked()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let attempts = self
            .list_work_attempts_unlocked(None)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let presence = self.load_worker_presence_unlocked(agent_id)?;
        Ok(Some(WorkerProjection::project(
            &agent,
            agent.spec.as_ref(),
            presence.as_ref(),
            &items,
            &attempts,
            now,
            None,
        )))
    }

    /// Scoped detail read for public observatories. Out-of-scope, inactive,
    /// and unknown identities intentionally collapse to `None` so the service
    /// can expose one indistinguishable public error.
    pub fn get_worker_scoped(
        &self,
        agent_id: &str,
        session_id: Uuid,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<WorkerProjection>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(agent) = self
            .load_agent_unlocked(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        else {
            return Ok(None);
        };
        if !agent.state.is_active_identity()
            || !agent.known_lane_ids().contains(&session_id)
            || !workspaces_match(&agent.workspace, workspace)
        {
            return Ok(None);
        }
        let (items, attempts) = self.scoped_work_and_attempts_unlocked(session_id, workspace)?;
        let presence = self.load_worker_presence_unlocked(agent_id)?;
        Ok(Some(WorkerProjection::project(
            &agent,
            agent.spec.as_ref(),
            presence.as_ref(),
            &items,
            &attempts,
            now,
            None,
        )))
    }

    fn worker_presence_path(&self, agent_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("worker-presence")
            .join(format!("{}.json", safe_id_filename(agent_id)?)))
    }

    fn load_worker_presence_unlocked(
        &self,
        agent_id: &str,
    ) -> Result<Option<WorkerPresence>, OrchError> {
        let path = self.worker_presence_path(agent_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let presence = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(Some(presence))
    }

    fn list_agents_unlocked(&self) -> anyhow::Result<Vec<AgentRecord>> {
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
            let _ = agent.migrate_legacy_spec();
            if agent.validate().is_ok() {
                out.push(agent);
            }
        }
        Ok(out)
    }

    pub fn send_message(&self, mut message: WorkMessage) -> Result<WorkMessage, OrchError> {
        let _guard = self.inner.lock.lock();
        self.send_message_unlocked(&mut message)
    }

    /// Persist a host-derived stable message identity exactly once. A replay
    /// with different content fails closed instead of overwriting the first
    /// durable observation.
    pub fn send_message_once(&self, mut message: WorkMessage) -> Result<WorkMessage, OrchError> {
        let _guard = self.inner.lock.lock();
        if let Some(existing) = self.load_message_unlocked(&message.message_id)? {
            if existing.kind != message.kind
                || existing.from_actor != message.from_actor
                || existing.from_agent_id != message.from_agent_id
                || existing.to_agent_id != message.to_agent_id
                || existing.session_id != message.session_id
                || existing.workspace != message.workspace
                || existing.work_id != message.work_id
                || existing.attempt_id != message.attempt_id
                || existing.run_id != message.run_id
                || existing.reply_to_id != message.reply_to_id
                || existing.thread_id != message.thread_id
                || existing.body != message.body
                || existing.payload != message.payload
                || existing.expires_at != message.expires_at
            {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "stable message identity was replayed with different content",
                ));
            }
            return Ok(existing);
        }
        self.send_message_unlocked(&mut message)
    }

    fn send_message_unlocked(&self, message: &mut WorkMessage) -> Result<WorkMessage, OrchError> {
        message.validate()?;
        self.require_optional_agent_in_scope_unlocked(
            message.from_agent_id.as_deref(),
            message.session_id,
            &message.workspace,
        )?;
        self.require_optional_agent_in_scope_unlocked(
            message.to_agent_id.as_deref(),
            message.session_id,
            &message.workspace,
        )?;
        let seq = self.next_message_seq_unlocked()?;
        message.seq = seq;
        let path = self.message_path(&message.message_id)?;
        atomic_write_json(&self.lease(), &path, &message)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.prune_messages_unlocked()?;
        Ok(message.clone())
    }

    fn require_optional_agent_in_scope_unlocked(
        &self,
        agent_id: Option<&str>,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<(), OrchError> {
        if let Some(agent_id) = agent_id {
            self.require_agent_in_scope_unlocked(agent_id, session_id, workspace)?;
        }
        Ok(())
    }

    pub fn ack_message(
        &self,
        message_id: &str,
        actor_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<WorkMessage, OrchError> {
        let _guard = self.inner.lock.lock();
        self.ack_message_unlocked(message_id, actor_id, now, None)
    }

    /// Lookup, session/workspace validation, and acknowledgement under one lock.
    /// Out-of-scope callers fail closed without writing `acked_at`/`acked_by`.
    pub fn ack_message_scoped(
        &self,
        message_id: &str,
        actor_id: &str,
        now: chrono::DateTime<Utc>,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<WorkMessage, OrchError> {
        let _guard = self.inner.lock.lock();
        self.ack_message_unlocked(message_id, actor_id, now, Some((session_id, workspace)))
    }

    fn ack_message_unlocked(
        &self,
        message_id: &str,
        actor_id: &str,
        now: chrono::DateTime<Utc>,
        scope: Option<(Uuid, &str)>,
    ) -> Result<WorkMessage, OrchError> {
        let mut message = self
            .load_message_unlocked(message_id)?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown message_id"))?;
        if let Some((session_id, workspace)) = scope {
            if message.session_id != session_id || !workspaces_match(&message.workspace, workspace)
            {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "message is out of scope",
                ));
            }
        }
        if message.acked_at.is_none() {
            message.acked_at = Some(now);
            message.acked_by = Some(actor_id.to_string());
            atomic_write_json(&self.lease(), &self.message_path(message_id)?, &message)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        }
        Ok(message)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn list_messages(
        &self,
        session_id: Uuid,
        workspace: &str,
        after_seq: u64,
        inbox_agent_id: Option<&str>,
        outbox_actor: Option<&str>,
        limit: usize,
    ) -> Result<MessagePage, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut messages = self.list_messages_unlocked()?;
        messages.retain(|message| {
            message.session_id == session_id
                && workspaces_match(&message.workspace, workspace)
                && match inbox_agent_id {
                    Some(agent) => message.to_agent_id.as_deref() == Some(agent),
                    None => true,
                }
                && match outbox_actor {
                    Some(actor) => {
                        message.from_actor == actor
                            || message.from_agent_id.as_deref() == Some(actor)
                    }
                    None => true,
                }
        });
        messages.sort_by_key(|message| message.seq);
        let retained_from_seq = messages.first().map(|message| message.seq).unwrap_or(1);
        if after_seq > 0 && after_seq < retained_from_seq.saturating_sub(1) && !messages.is_empty()
        {
            return Err(OrchError::new(
                OrchErrorCode::CursorExpired,
                "message cursor expired for the retained window",
            ));
        }
        let messages = messages
            .into_iter()
            .filter(|message| message.seq > after_seq)
            .take(limit.clamp(1, 200))
            .collect::<Vec<_>>();
        let next_seq = messages
            .last()
            .map(|message| message.seq)
            .unwrap_or(after_seq);
        Ok(MessagePage {
            messages,
            next_seq,
            retained_from_seq,
        })
    }

    /// Newest retained messages in deterministic `seq` order.
    ///
    /// `list_messages(after_seq=0)` returns the oldest page. Native input
    /// assembly needs the newest window so relevant instructions past seq 200
    /// are not dropped from a long-lived Lane.
    pub fn list_recent_messages(
        &self,
        session_id: Uuid,
        workspace: &str,
        inbox_agent_id: Option<&str>,
        outbox_actor: Option<&str>,
        limit: usize,
    ) -> Result<MessagePage, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut messages = self.list_messages_unlocked()?;
        messages.retain(|message| {
            message.session_id == session_id
                && workspaces_match(&message.workspace, workspace)
                && match inbox_agent_id {
                    Some(agent) => message.to_agent_id.as_deref() == Some(agent),
                    None => true,
                }
                && match outbox_actor {
                    Some(actor) => {
                        message.from_actor == actor
                            || message.from_agent_id.as_deref() == Some(actor)
                    }
                    None => true,
                }
        });
        messages.sort_by_key(|message| message.seq);
        let retained_from_seq = messages.first().map(|message| message.seq).unwrap_or(1);
        let limit = limit.clamp(1, MAX_RETAINED_MESSAGES);
        let skip = messages.len().saturating_sub(limit);
        let messages = messages.into_iter().skip(skip).collect::<Vec<_>>();
        let next_seq = messages.last().map(|message| message.seq).unwrap_or(0);
        Ok(MessagePage {
            messages,
            next_seq,
            retained_from_seq,
        })
    }

    pub fn load_message(&self, message_id: &str) -> Result<Option<WorkMessage>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_message_unlocked(message_id)
    }

    fn message_path(&self, message_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("messages")
            .join(format!("{}.json", safe_id_filename(message_id)?)))
    }

    fn next_message_seq_unlocked(&self) -> Result<u64, OrchError> {
        let path = self.inner.root.join("messages").join("seq");
        let current = if path.is_file() {
            fs::read_to_string(&path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .trim()
                .parse::<u64>()
                .unwrap_or(0)
        } else {
            0
        };
        let next = current + 1;
        let _write = self
            .lease()
            .begin("advancing the durable message sequence")
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        fs::write(&path, next.to_string())
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(next)
    }

    fn load_message_unlocked(&self, message_id: &str) -> Result<Option<WorkMessage>, OrchError> {
        let path = self.message_path(message_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let message = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(Some(message))
    }

    fn list_messages_unlocked(&self) -> Result<Vec<WorkMessage>, OrchError> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("messages");
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
            let message: WorkMessage = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            out.push(message);
        }
        Ok(out)
    }

    fn prune_messages_unlocked(&self) -> Result<(), OrchError> {
        let mut messages = self.list_messages_unlocked()?;
        if messages.len() <= MAX_RETAINED_MESSAGES {
            return Ok(());
        }
        messages.sort_by_key(|message| message.seq);
        let drop = messages.len() - MAX_RETAINED_MESSAGES;
        for message in messages.into_iter().take(drop) {
            let _ = remove_file_durable(&self.lease(), &self.message_path(&message.message_id)?);
        }
        Ok(())
    }

    pub fn save_managed_intent(
        &self,
        intent: &ManagedExecutionIntent,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        intent.validate()?;
        let _guard = self.inner.lock.lock();
        self.save_managed_intent_unlocked(intent)?;
        Ok(intent.clone())
    }

    fn save_managed_intent_unlocked(
        &self,
        intent: &ManagedExecutionIntent,
    ) -> Result<(), OrchError> {
        let path = self.managed_intent_path(&intent.intent_id)?;
        atomic_write_json(&self.lease(), &path, intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn load_managed_intent(
        &self,
        intent_id: &str,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_managed_intent_unlocked(intent_id)
    }

    fn load_managed_intent_unlocked(
        &self,
        intent_id: &str,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let path = self.managed_intent_path(intent_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let intent = serde_json::from_str(
            &fs::read_to_string(path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(Some(intent))
    }

    pub fn list_managed_intents(&self) -> Result<Vec<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.list_managed_intents_unlocked()
    }

    fn list_managed_intents_unlocked(&self) -> Result<Vec<ManagedExecutionIntent>, OrchError> {
        let mut out = Vec::new();
        let dir = self.inner.root.join("managed-intents");
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
            let intent: ManagedExecutionIntent = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            out.push(intent);
        }
        out.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(out)
    }

    pub fn live_managed_intents_for_agent(&self, agent_id: &str) -> Result<usize, OrchError> {
        Ok(self
            .list_managed_intents()?
            .into_iter()
            .filter(|intent| intent.agent_id == agent_id && intent.state.is_live())
            .count())
    }

    pub fn live_managed_intent_for_work(
        &self,
        work_id: &str,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        Ok(self
            .list_managed_intents()?
            .into_iter()
            .find(|intent| intent.work_id == work_id && intent.state.is_live()))
    }

    fn managed_intent_path(&self, intent_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("managed-intents")
            .join(format!("{}.json", safe_id_filename(intent_id)?)))
    }

    pub fn authorize_work_execution(
        &self,
        work_id: &str,
        actor_id: &str,
        actor_agent_id: Option<&str>,
        reason: &str,
        expected_revision: Option<u64>,
        now: chrono::DateTime<Utc>,
    ) -> Result<(WorkItem, WorkDecision), OrchError> {
        self.mutate_assignment(
            AssignmentMutation {
                work_id,
                action: WorkDecisionAction::AuthorizeExecution,
                actor_id,
                actor_agent_id,
                assigned_agent_id: None,
                reason,
                expected_revision,
                now,
            },
            |_| Ok(()),
        )
    }

    pub fn park_work_input(
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
        item.state = WorkState::AwaitingInput;
        item.blocked_reason = Some(reason.to_string());
        item.bump();
        attempt.state = AttemptState::AwaitingInput;
        attempt.last_heartbeat_at = now;
        attempt.updated_at = now;
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt))
    }

    pub fn abandon_managed_intent(
        &self,
        intent_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(mut intent) = self.load_managed_intent_unlocked(intent_id)? else {
            return Ok(None);
        };
        if intent.state == ManagedIntentState::Finalized {
            return Ok(Some(intent));
        }
        // Dispatching means physical send may already have occurred. It can
        // only converge through Review; abandoning it would release and
        // requeue uncertain work, creating a duplicate-send path.
        if intent.state == ManagedIntentState::Dispatching {
            return Ok(Some(intent));
        }
        if let Some(attempt_id) = &intent.attempt_id {
            if intent.run_id.is_none() {
                if let Ok(Some(mut attempt)) = self.load_work_attempt_unlocked(attempt_id) {
                    if attempt.state.is_active() {
                        if let Ok(Some(mut item)) = self.load_work_item_unlocked(&intent.work_id) {
                            attempt.state = AttemptState::Released;
                            attempt.terminal_reason = Some(
                                "managed admission abandoned before a Run was committed".into(),
                            );
                            attempt.updated_at = now;
                            item.state = WorkState::Queued;
                            item.bump_at(now);
                            let _ = self.save_work_attempt_unlocked(&attempt);
                            let _ = self.save_work_item_unlocked(&item);
                        }
                    }
                }
            }
        }
        intent.state = ManagedIntentState::Abandoned;
        intent.updated_at = now;
        self.save_managed_intent_unlocked(&intent)?;
        Ok(Some(intent))
    }

    pub fn find_run_by_request_id(&self, request_id: &str) -> Result<Option<RunRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        self.find_run_by_request_id_unlocked(request_id)
    }

    fn find_run_by_request_id_unlocked(
        &self,
        request_id: &str,
    ) -> Result<Option<RunRecord>, OrchError> {
        let dir = self.inner.root.join("runs");
        if !dir.is_dir() {
            return Ok(None);
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
            let run: RunRecord = serde_json::from_str(
                &fs::read_to_string(path)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            )
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            if run.request_id == request_id {
                return Ok(Some(run));
            }
        }
        Ok(None)
    }

    /// Recover a Claiming intent without creating a second Run.
    ///
    /// If `submit_task` already committed a Run for `intent_id` as request_id,
    /// that Run is adopted. Only a claim with no Run is released.
    pub fn reconcile_claiming_intent(
        &self,
        intent_id: &str,
        lease_secret: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(mut intent) = self.load_managed_intent_unlocked(intent_id)? else {
            return Ok(None);
        };
        if intent.state != ManagedIntentState::Claiming {
            return Ok(Some(intent));
        }
        let run = if let Some(run_id) = intent.run_id.as_deref() {
            self.load_run_unlocked(run_id)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        } else {
            let from_receipt = self
                .load_idempotency(&intent.intent_id)
                .ok()
                .flatten()
                .and_then(|receipt| receipt.run_id);
            match from_receipt {
                Some(run_id) => self
                    .load_run_unlocked(&run_id)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
                None => self.find_run_by_request_id_unlocked(&intent.intent_id)?,
            }
        };
        if let Some(run) = run {
            intent.run_id = Some(run.run_id.clone());
            if let Some(attempt_id) = intent.attempt_id.clone() {
                if let Ok(Some(attempt)) = self.load_work_attempt_unlocked(&attempt_id) {
                    let token = attempt.lease_token_for_secret(lease_secret);
                    if attempt.token_matches(&token) && attempt.lease_active_at(now) {
                        let mut linked = attempt;
                        if !linked.linked_run_ids.iter().any(|id| id == &run.run_id) {
                            linked.linked_run_ids.push(run.run_id.clone());
                        }
                        if linked.state == AttemptState::Leased {
                            linked.state = AttemptState::Running;
                        }
                        linked.updated_at = now;
                        let _ = self.save_work_attempt_unlocked(&linked);
                    }
                }
            }
            intent.state = ManagedIntentState::Admitted;
            intent.updated_at = now;
            self.save_managed_intent_unlocked(&intent)?;
            return Ok(Some(intent));
        }
        if intent.attempt_id.is_some() {
            drop(_guard);
            return self.abandon_managed_intent(intent_id, now);
        }
        intent.state = ManagedIntentState::Abandoned;
        intent.updated_at = now;
        self.save_managed_intent_unlocked(&intent)?;
        Ok(Some(intent))
    }

    pub fn close_managed_attempt(
        &self,
        intent_id: &str,
        retry_eligible: bool,
        cause: ManagedRetryCause,
        reason: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        self.close_managed_attempt_until(
            intent_id,
            retry_eligible,
            cause,
            reason,
            now,
            ManagedFinalizationStage::Complete,
        )
    }

    pub fn close_managed_attempt_until(
        &self,
        intent_id: &str,
        retry_eligible: bool,
        cause: ManagedRetryCause,
        reason: &str,
        now: chrono::DateTime<Utc>,
        stage: ManagedFinalizationStage,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(intent) = self.load_managed_intent_unlocked(intent_id)? else {
            return Ok(None);
        };
        if intent.state == ManagedIntentState::Finalized
            || intent.state == ManagedIntentState::Abandoned
        {
            return Ok(Some(intent));
        }
        let item = self
            .load_work_item_unlocked(&intent.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let attempt = match intent.attempt_id.as_deref() {
            Some(attempt_id) => self
                .load_work_attempt_unlocked(attempt_id)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            None => None,
        };
        let attempt_number = attempt
            .as_ref()
            .map(|value| value.attempt_number)
            .unwrap_or(item.attempt_count.max(1));
        let policy = ManagedExecutionPolicy {
            retry_eligible,
            ..ManagedExecutionPolicy::default()
        };
        let retry = policy.allows_auto_retry(&item, attempt_number.saturating_add(1), cause);
        let mut attempt_state = match cause {
            ManagedRetryCause::Expired | ManagedRetryCause::Interrupted => AttemptState::Expired,
            ManagedRetryCause::Failed => AttemptState::Failed,
        };
        let (outcome, work_state, result) = if item.state == WorkState::Cancelled {
            (
                ManagedFinalizationOutcome::Cancelled,
                WorkState::Cancelled,
                item.result.clone(),
            )
        } else if item.state.is_review_gate() || item.state == WorkState::Succeeded {
            let outcome = match item.state {
                WorkState::AwaitingApproval => {
                    attempt_state = AttemptState::AwaitingApproval;
                    ManagedFinalizationOutcome::AwaitingApproval
                }
                WorkState::Review => {
                    attempt_state = AttemptState::Review;
                    ManagedFinalizationOutcome::Review
                }
                WorkState::Succeeded => {
                    attempt_state = AttemptState::Succeeded;
                    ManagedFinalizationOutcome::Completed
                }
                _ => unreachable!("review gate or succeeded state matched"),
            };
            (outcome, item.state, item.result.clone())
        } else if retry {
            (
                ManagedFinalizationOutcome::RetryQueued,
                WorkState::Queued,
                None,
            )
        } else {
            (
                ManagedFinalizationOutcome::Failed,
                WorkState::Failed,
                Some(Self::work_failure_result(reason, now)),
            )
        };
        let record = ManagedFinalizationRecord {
            schema_version: MANAGED_FINALIZATION_SCHEMA_VERSION,
            intent_id: intent.intent_id.clone(),
            work_id: intent.work_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            outcome,
            attempt_state,
            work_state,
            reason: reason.to_string(),
            result,
            created_at: now,
        };
        self.apply_managed_finalization_unlocked(&record, stage, now)?;
        self.load_managed_intent_unlocked(intent_id)
    }

    pub fn finalize_managed_intent(
        &self,
        intent_id: &str,
        outcome: ManagedFinalizationOutcome,
        reason: &str,
        result: Option<WorkResult>,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        self.finalize_managed_intent_until(
            intent_id,
            outcome,
            reason,
            result,
            now,
            ManagedFinalizationStage::Complete,
        )
    }

    pub fn finalize_managed_intent_until(
        &self,
        intent_id: &str,
        outcome: ManagedFinalizationOutcome,
        reason: &str,
        result: Option<WorkResult>,
        now: chrono::DateTime<Utc>,
        stage: ManagedFinalizationStage,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(intent) = self.load_managed_intent_unlocked(intent_id)? else {
            return Ok(None);
        };
        if intent.state == ManagedIntentState::Finalized
            || intent.state == ManagedIntentState::Abandoned
        {
            return Ok(Some(intent));
        }
        let item = self
            .load_work_item_unlocked(&intent.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let attempt = match intent.attempt_id.as_deref() {
            Some(attempt_id) => self
                .load_work_attempt_unlocked(attempt_id)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            None => None,
        };
        let (attempt_state, work_state, result) = if item.state == WorkState::Cancelled {
            (
                AttemptState::Cancelled,
                WorkState::Cancelled,
                item.result.clone(),
            )
        } else if item.state == WorkState::Succeeded
            && outcome != ManagedFinalizationOutcome::Cancelled
        {
            (
                AttemptState::Succeeded,
                WorkState::Succeeded,
                item.result.clone(),
            )
        } else {
            match outcome {
                ManagedFinalizationOutcome::Completed => {
                    let result = self.bind_managed_completion_result_unlocked(
                        &intent,
                        result,
                        attempt.as_ref(),
                    )?;
                    let mut candidate = item.clone();
                    candidate.result = result.clone();
                    if item.policy.requires_approval {
                        (
                            AttemptState::AwaitingApproval,
                            WorkState::AwaitingApproval,
                            result,
                        )
                    } else if self.success_is_authorized_unlocked(
                        &candidate,
                        attempt.as_ref(),
                        result.as_ref(),
                        intent.run_id.as_deref(),
                    )? {
                        (AttemptState::Succeeded, WorkState::Succeeded, result)
                    } else {
                        (AttemptState::Review, WorkState::Review, result)
                    }
                }
                ManagedFinalizationOutcome::AwaitingApproval => (
                    AttemptState::AwaitingApproval,
                    WorkState::AwaitingApproval,
                    result,
                ),
                ManagedFinalizationOutcome::Review => {
                    (AttemptState::Review, WorkState::Review, result)
                }
                ManagedFinalizationOutcome::Failed => (
                    AttemptState::Failed,
                    WorkState::Failed,
                    result.or_else(|| Some(Self::work_failure_result(reason, now))),
                ),
                ManagedFinalizationOutcome::RetryQueued => {
                    (AttemptState::Failed, WorkState::Queued, None)
                }
                ManagedFinalizationOutcome::Cancelled => {
                    (AttemptState::Cancelled, WorkState::Cancelled, result)
                }
            }
        };
        let record = ManagedFinalizationRecord {
            schema_version: MANAGED_FINALIZATION_SCHEMA_VERSION,
            intent_id: intent.intent_id.clone(),
            work_id: intent.work_id.clone(),
            attempt_id: intent.attempt_id.clone(),
            outcome,
            attempt_state,
            work_state,
            reason: reason.to_string(),
            result,
            created_at: now,
        };
        self.apply_managed_finalization_unlocked(&record, stage, now)?;
        self.load_managed_intent_unlocked(intent_id)
    }

    pub fn recover_managed_finalization_intents(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("managed-finalization");
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                paths.push(path);
            }
        }
        let mut n = 0;
        for path in paths {
            let record: ManagedFinalizationRecord =
                serde_json::from_str(&fs::read_to_string(&path)?)?;
            self.apply_managed_finalization_unlocked(
                &record,
                ManagedFinalizationStage::Complete,
                Utc::now(),
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            n += 1;
        }
        Ok(n)
    }

    fn managed_finalization_path(&self, intent_id: &str) -> Result<PathBuf, OrchError> {
        Ok(self
            .inner
            .root
            .join("managed-finalization")
            .join(format!("{}.json", safe_id_filename(intent_id)?)))
    }

    fn apply_managed_finalization_unlocked(
        &self,
        record: &ManagedFinalizationRecord,
        stage: ManagedFinalizationStage,
        now: chrono::DateTime<Utc>,
    ) -> Result<(), OrchError> {
        let path = self.managed_finalization_path(&record.intent_id)?;
        atomic_write_json(&self.lease(), &path, record)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if stage == ManagedFinalizationStage::AfterJournal {
            return Ok(());
        }
        if let Some(attempt_id) = record.attempt_id.as_deref() {
            if let Ok(Some(mut attempt)) = self.load_work_attempt_unlocked(attempt_id) {
                if attempt.state.is_active() && !attempt.state.is_review_gate() {
                    attempt.state = record.attempt_state;
                    attempt.terminal_reason = Some(record.reason.clone());
                    if record.result.is_some() {
                        attempt.result = record.result.clone();
                    }
                    attempt.updated_at = now;
                    self.save_work_attempt_unlocked(&attempt).map_err(|error| {
                        OrchError::new(OrchErrorCode::Internal, error.to_string())
                    })?;
                }
            }
        }
        if stage == ManagedFinalizationStage::AfterAttempt {
            return Ok(());
        }
        if let Ok(Some(mut item)) = self.load_work_item_unlocked(&record.work_id) {
            let preserve_terminal = item.state == WorkState::Cancelled
                || item.state == WorkState::Succeeded
                || item.state == WorkState::AwaitingApproval
                || item.state == WorkState::Review;
            if !preserve_terminal {
                if record.result.is_some() && record.work_state != WorkState::Queued {
                    item.result = record.result.clone();
                }
                if record.work_state == WorkState::Succeeded {
                    let mut attempt = match record.attempt_id.as_deref() {
                        Some(attempt_id) => {
                            self.load_work_attempt_unlocked(attempt_id).ok().flatten()
                        }
                        None => None,
                    };
                    let extra_run_id = item
                        .result
                        .as_ref()
                        .and_then(|result| result.verification.as_ref())
                        .and_then(|evidence| evidence.run_id.clone());
                    self.assign_work_succeeded_unlocked(
                        &mut item,
                        attempt.as_mut(),
                        extra_run_id.as_deref(),
                    )?;
                    if let Some(attempt) = attempt.as_ref() {
                        self.save_work_attempt_unlocked(attempt).map_err(|error| {
                            OrchError::new(OrchErrorCode::Internal, error.to_string())
                        })?;
                    }
                } else if record.work_state == WorkState::Queued {
                    item.state = WorkState::Queued;
                    item.blocked_reason = None;
                    item.result = None;
                } else {
                    item.state = record.work_state;
                }
                item.bump_at(now);
                self.save_work_item_unlocked(&item)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            }
        }
        if stage == ManagedFinalizationStage::AfterWork {
            return Ok(());
        }
        if let Some(mut intent) = self.load_managed_intent_unlocked(&record.intent_id)? {
            if intent.state != ManagedIntentState::Finalized
                && intent.state != ManagedIntentState::Abandoned
            {
                intent.state = ManagedIntentState::Finalized;
                intent.updated_at = now;
                self.save_managed_intent_unlocked(&intent)?;
            }
        }
        if let Ok(_write) = self.lease().begin("clearing a managed finalization intent") {
            // `_write` is a binding, not a discarded temporary, so the removal
            // below happens *under* the authority rather than after it (#455).
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    pub fn seal_queued_managed_work(
        &self,
        work_id: &str,
        reason: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<WorkItem>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(mut item) = self
            .load_work_item_unlocked(work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        else {
            return Ok(None);
        };
        if item.state.is_terminal() {
            return Ok(Some(item));
        }
        if item.state != WorkState::Queued {
            return Ok(Some(item));
        }
        item.state = WorkState::Failed;
        item.result = Some(Self::work_failure_result(reason, now));
        item.bump_at(now);
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok(Some(item))
    }

    pub fn inspect_parked_managed_permission(
        &self,
        permission_id: &str,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        let _guard = self.inner.lock.lock();
        self.inspect_parked_managed_permission_unlocked(permission_id, session_id, workspace)
    }

    fn inspect_parked_managed_permission_unlocked(
        &self,
        permission_id: &str,
        session_id: Uuid,
        workspace: &str,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        let intents = self.list_managed_intents_unlocked()?;
        let Some(intent) = intents
            .into_iter()
            .find(|intent| intent.permission_request_id.as_deref() == Some(permission_id))
        else {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "unknown permission request",
            ));
        };
        if intent.session_id != session_id || !workspaces_match(&intent.workspace, workspace) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "permission is outside the requested session workspace",
            ));
        }
        match intent.state {
            ManagedIntentState::Parked | ManagedIntentState::Resolving => {}
            ManagedIntentState::Admitted | ManagedIntentState::Finalized
                if intent.permission_request_id.as_deref() == Some(permission_id) =>
            {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "permission is already resolved",
                ));
            }
            _ => {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "permission is not parked",
                ));
            }
        }
        let item = self
            .load_work_item_unlocked(&intent.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        if item.state.is_terminal() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work is no longer awaiting input",
            ));
        }
        if item.state != WorkState::AwaitingInput {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work is not awaiting input",
            ));
        }
        Ok(intent)
    }

    pub fn begin_managed_permission_resolve(
        &self,
        permission_id: &str,
        session_id: Uuid,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut intent =
            self.inspect_parked_managed_permission_unlocked(permission_id, session_id, workspace)?;
        intent.state = ManagedIntentState::Resolving;
        intent.updated_at = now;
        self.save_managed_intent_unlocked(&intent)?;
        Ok(intent)
    }

    pub fn abort_managed_permission_resolve(
        &self,
        intent_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Option<ManagedExecutionIntent>, OrchError> {
        let _guard = self.inner.lock.lock();
        let Some(mut intent) = self.load_managed_intent_unlocked(intent_id)? else {
            return Ok(None);
        };
        if intent.state == ManagedIntentState::Resolving {
            intent.state = ManagedIntentState::Parked;
            intent.updated_at = now;
            self.save_managed_intent_unlocked(&intent)?;
        }
        Ok(Some(intent))
    }

    pub fn resolve_parked_managed_permission(
        &self,
        permission_id: &str,
        session_id: Uuid,
        workspace: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        let _guard = self.inner.lock.lock();
        let mut intent =
            self.inspect_parked_managed_permission_unlocked(permission_id, session_id, workspace)?;
        let attempt_id = intent.attempt_id.clone().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Conflict,
                "parked intent is missing an attempt",
            )
        })?;
        let mut item = self
            .load_work_item_unlocked(&intent.work_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work item not found"))?;
        let mut attempt = self
            .load_work_attempt_unlocked(&attempt_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::Conflict, "work attempt not found"))?;
        if !attempt.state.is_active() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work attempt is no longer active",
            ));
        }
        item.state = WorkState::Running;
        item.blocked_reason = None;
        item.bump_at(now);
        attempt.state = AttemptState::Running;
        attempt.updated_at = now;
        intent.state = ManagedIntentState::Admitted;
        intent.updated_at = now;
        self.save_work_attempt_unlocked(&attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_managed_intent_unlocked(&intent)?;
        Ok(intent)
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
        if item.state == WorkState::Succeeded
            && item
                .approval
                .as_ref()
                .is_some_and(|existing| existing.reviewer_id == reviewer_id)
        {
            let attempts = self
                .list_work_attempts_unlocked(Some(work_id))
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
            let attempt = attempts
                .into_iter()
                .rev()
                .find(|attempt| {
                    attempt.state == AttemptState::Succeeded
                        || attempt.state == AttemptState::AwaitingApproval
                })
                .ok_or_else(|| {
                    OrchError::new(
                        OrchErrorCode::Conflict,
                        "approved work item has no succeeded attempt",
                    )
                })?;
            return Ok((item, attempt));
        }
        if item
            .approval
            .as_ref()
            .is_some_and(|existing| existing.reviewer_id != reviewer_id)
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item is already approved by a different actor",
            ));
        }
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
        attempt.terminal_reason = Some(format!("approved by {}", reviewer_id));
        attempt.updated_at = approval.approved_at;
        item.approval = Some(approval);
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.assign_work_succeeded_unlocked(&mut item, Some(attempt), None)?;
        item.bump();
        self.save_work_attempt_unlocked(attempt)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        Ok((item, attempt.clone()))
    }

    fn require_managed_grok_claim_fence_unlocked(
        &self,
        item: &WorkItem,
        claimant_id: &str,
        fence: &ManagedGrokClaimFence<'_>,
    ) -> Result<(), OrchError> {
        if item.revision != fence.expected_work_revision
            || item.last_decision_id.as_deref() != Some(fence.expected_decision_id)
            || item.assigned_agent_id.as_deref() != Some(claimant_id)
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed Grok Work authority changed before claim",
            ));
        }
        let current_allowed = super::workload::normalize_allowed_files(&item.policy.allowed_files)?;
        let expected_allowed =
            super::workload::normalize_allowed_files(fence.expected_allowed_files)?;
        if current_allowed != expected_allowed {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed Grok mutable-file authority changed before claim",
            ));
        }
        let agent =
            self.require_agent_in_scope_unlocked(claimant_id, item.session_id, &item.workspace)?;
        let spec = agent
            .current_spec()
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if spec.revision != fence.expected_agent_spec_revision {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed Grok Agent authority changed before claim",
            ));
        }
        let decision_path = self.work_decision_path(&item.work_id, fence.expected_decision_id)?;
        let decision: WorkDecision = serde_json::from_str(
            &fs::read_to_string(decision_path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let decision_is_current = decision.schema_version == WORKLOAD_SCHEMA_VERSION
            && decision.decision_id == fence.expected_decision_id
            && decision.work_id == item.work_id
            && decision.action == WorkDecisionAction::AuthorizeExecution
            && decision.assigned_agent_id.as_deref() == item.assigned_agent_id.as_deref()
            && decision.policy_revision == Some(fence.expected_agent_spec_revision)
            && decision
                .work_revision
                .and_then(|revision| revision.checked_add(1))
                == Some(item.revision);
        if !decision_is_current {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed Grok execution decision changed before claim",
            ));
        }
        Ok(())
    }

    pub fn claim_work(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
    ) -> Result<WorkClaim, OrchError> {
        self.claim_work_inner(work_id, claimant_id, lease_ms, None, None)
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
        self.claim_work_inner(work_id, claimant_id, lease_ms, Some(lease_secret), None)
    }

    pub(super) fn claim_managed_grok_work_with_lease_secret(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
        lease_secret: &str,
        fence: &ManagedGrokClaimFence<'_>,
    ) -> Result<WorkClaim, OrchError> {
        if lease_secret.is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "lease_secret is required",
            ));
        }
        self.claim_work_inner(
            work_id,
            claimant_id,
            lease_ms,
            Some(lease_secret),
            Some(fence),
        )
    }

    fn claim_work_inner(
        &self,
        work_id: &str,
        claimant_id: &str,
        lease_ms: Option<u64>,
        lease_secret: Option<&str>,
        managed_grok_fence: Option<&ManagedGrokClaimFence<'_>>,
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
        if item.is_container {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "coordination container Work is not executable",
            ));
        }
        self.refresh_work_item_unlocked(&mut item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if let Some(fence) = managed_grok_fence {
            self.require_managed_grok_claim_fence_unlocked(&item, claimant_id, fence)?;
        }
        let now = Utc::now();
        // The holder's identity is deliberately not echoed: a claim conflict is
        // answered with the fact of the conflict, not with an attribution of
        // the worker that won it.
        if self
            .active_attempt_unlocked(work_id, now)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .is_some()
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work item is already leased by an active attempt",
            ));
        }
        for mut attempt in self
            .list_work_attempts_unlocked(Some(work_id))
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
        {
            if attempt.state.requires_lease_heartbeat() && !attempt.lease_active_at(now) {
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
        // The one admission evaluator, not a second opinion. `Queued` alone is
        // not admission: reconciliation may not have run since a dependency
        // was declared, and an unresolvable or cyclic edge leaves an item
        // sitting in `Queued` that must never be handed to a worker.
        let admission = super::graph::evaluate_admission(
            &item,
            &self
                .resolve_dependencies_unlocked(&item)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?,
            now,
        );
        if !admission.is_admissible() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!("work item is not admissible: {}", admission.as_str()),
            ));
        }
        item.assignment_status
            .is_claimable_by(item.assigned_agent_id.as_deref(), claimant_id)?;
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
        let mut attempt = self
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
        let already_completed = matches!(
            attempt.state,
            AttemptState::Succeeded | AttemptState::Review | AttemptState::AwaitingApproval
        ) && matches!(
            item.state,
            WorkState::Succeeded | WorkState::Review | WorkState::AwaitingApproval
        );
        if already_completed {
            if !attempt
                .result
                .as_ref()
                .is_some_and(|stored| Self::completion_result_replays(stored, &result))
            {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "work attempt already completed",
                ));
            }
            if item.state != WorkState::Succeeded && now >= attempt.lease_expires_at {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "work lease is no longer active",
                ));
            }
            return Ok((item, attempt));
        }
        if !attempt.state.is_active() || now >= attempt.lease_expires_at {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work lease is no longer active",
            ));
        }
        attempt.result = Some(result.clone());
        attempt.updated_at = now;
        item.result = Some(result);
        if self.success_is_authorized_unlocked(&item, Some(&attempt), item.result.as_ref(), None)?
            && !item.policy.requires_approval
        {
            self.assign_work_succeeded_unlocked(&mut item, Some(&mut attempt), None)?;
        } else {
            let (attempt_state, work_state) = Self::ordinary_completion_states(&item);
            attempt.state = attempt_state;
            item.state = work_state;
        }
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
            verification: None,
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
        atomic_write_json(&self.lease(), &path, &agent)
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
            atomic_write_json(&self.lease(), &path, &agent)?;
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
                atomic_write_json(&self.lease(), &path, &agent)?;
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
        atomic_write_json(&self.lease(), &path, &agent)?;
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
        // No unguarded `create_dir_all` here: `atomic_write_json` creates the
        // parent under its own guard, so no directory appears on a home this
        // handle may not write (#455).
        atomic_write_json(&self.lease(), &path, spec)
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
        atomic_write_json(&self.lease(), &path, &agent)?;
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
        atomic_write_json(&self.lease(), &path, checkpoint)
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
        Ok(write_json_exclusive(&self.lease(), &path, snapshot)?)
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
        Ok(write_json_exclusive(&self.lease(), &path, context)?)
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
        // Retention deletes durable evidence, so it is held for the whole pass
        // rather than re-authorized per file (#455).
        let _write = self.lease().begin("pruning durable retention")?;
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
        let activation_path = self
            .agent_activation_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if activation_path.is_file() {
            // A crash or cleanup failure may leave a fully applied activation
            // intent. Remove that Running snapshot before a terminal record
            // is installed so restart recovery can never resurrect the Run.
            remove_file_durable(&self.lease(), &activation_path)
                .context("retire Agent activation before Run finalization")?;
        }
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
            atomic_write_json(&self.lease(), &intent_path, &final_run)?;
            let _write = self.lease().begin("committing run finalization")?;
            if let Some(corrupt) = &corrupt_target {
                fs::rename(&run_path, corrupt)?;
            }
            atomic_write_json(&self.lease(), &run_path, &final_run)?;
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
        atomic_write_json(&self.lease(), &path, receipt)
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
        match write_json_exclusive(&self.lease(), &path, &pending) {
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
        atomic_write_json(&self.lease(), &path, &receipt)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn append_audit(&self, entry: &AuditEntry) -> anyhow::Result<()> {
        let _guard = self.inner.audit_file_lock.lock();
        let result = append_audit_entry(&self.lease(), &self.inner.root, entry);
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

    /// Drain and stop the asynchronous audit writer. This is a shutdown-only
    /// seam: callers must invoke it before sealing the owning runtime's
    /// durable-write authority. The synchronous `append_audit` path remains
    /// available for the final host-shutdown record after the drain.
    pub(crate) fn close_audit_writer_bounded(
        &self,
        timeout: std::time::Duration,
    ) -> AuditWriterStopReport {
        let mut report = self.inner.audit_writer.wait_bounded(timeout);
        if let Some(error) = self.inner.last_audit_error.lock().clone() {
            if !report.errors.contains(&error) {
                report.errors.push(error);
            }
        }
        report
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
                    serde_json::to_value(&existing)? == serde_json::to_value(&intent.run)?
                        || intent.prior_run.as_ref().is_some_and(|prior| {
                            serde_json::to_value(&existing).ok() == serde_json::to_value(prior).ok()
                        }),
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
            atomic_write_json(&self.lease(), &run_path, &intent.run)?;
            atomic_write_json(&self.lease(), &agent_path, &intent.activated_agent)?;
            remove_file_durable(&self.lease(), &path)?;
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
            atomic_write_json(&self.lease(), &path, &receipt)?;
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

fn append_audit_entry(
    lease: &crate::host_runtime::WriteLease,
    root: &Path,
    entry: &AuditEntry,
) -> anyhow::Result<()> {
    use std::io::Write;

    // Covers the append *and* the rotation below: rotation renames and deletes
    // durable evidence, so it is authorized by the same guard rather than
    // slipping past a check that only guarded the append (#455).
    let _write = lease.begin("appending to the durable audit ledger")?;

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

fn remove_file_durable(lease: &crate::host_runtime::WriteLease, path: &Path) -> anyhow::Result<()> {
    let _write = lease.begin("removing a durable orchestration record")?;
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

/// Every durable effect in this store funnels through here, so the authority
/// check cannot be forgotten at a call site (#455).
fn atomic_write_json<T: serde::Serialize>(
    lease: &crate::host_runtime::WriteLease,
    path: &Path,
    value: &T,
) -> anyhow::Result<()> {
    let _write = lease.begin("writing the durable orchestration ledger")?;
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

fn write_json_exclusive<T: serde::Serialize>(
    lease: &crate::host_runtime::WriteLease,
    path: &Path,
    value: &T,
) -> std::io::Result<()> {
    let _write = lease
        .begin("creating a durable orchestration record")
        .map_err(std::io::Error::other)?;
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
    use crate::orchestration::types::RunPurpose;
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
            purpose: Default::default(),
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

    fn work_mutation_fixture(
        store: &OrchStore,
    ) -> (WorkItem, WorkItem, WorkDecision, WorkMutationIntent) {
        let now = Utc::now();
        let prior = WorkItem::new_at(
            "triage",
            "inspect the synthetic incident",
            Uuid::new_v4(),
            "/tmp/coordinator-workspace",
            "operator",
            Default::default(),
            now,
        )
        .unwrap();
        store.save_work_item(&prior).unwrap();

        let mut next = prior.clone();
        next.state = WorkState::Blocked;
        next.block_provenance = Some(BlockProvenance::Manual);
        next.blocked_reason = Some("waiting for human evidence approval".into());
        next.bump_at(now + chrono::Duration::seconds(1));
        let decision = WorkDecision {
            schema_version: WORKLOAD_SCHEMA_VERSION,
            decision_id: Uuid::new_v4().to_string(),
            work_id: prior.work_id.clone(),
            action: WorkDecisionAction::Block,
            actor_id: "operator".into(),
            actor_agent_id: None,
            assigned_agent_id: None,
            policy_revision: None,
            // A decision records the revision it authorizes; the item carries
            // the following revision after the mutation is applied.
            work_revision: Some(prior.revision),
            reason: "waiting for human evidence approval".into(),
            created_at: now,
        };
        next.last_decision_id = Some(decision.decision_id.clone());
        next.validate().unwrap();
        let intent = WorkMutationIntent::new(&prior, next.clone(), decision.clone()).unwrap();
        (prior, next, decision, intent)
    }

    #[test]
    fn work_mutation_intent_recovers_after_decision_only_crash() {
        let d = tempdir().unwrap();
        let intent_path;
        let expected;
        {
            let store = OrchStore::open(d.path()).unwrap();
            let (_, next, decision, intent) = work_mutation_fixture(&store);
            expected = next;
            intent_path = store.work_mutation_intent_path(&intent.intent_id).unwrap();
            store.persist_work_mutation_intent_unlocked(&intent).unwrap();
            store.save_work_decision_unlocked(&decision).unwrap();
        }

        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened.load_work_item(&expected.work_id).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(reopened.list_work_decisions(&expected.work_id).unwrap().len(), 1);
        assert!(!intent_path.exists());
    }

    #[test]
    fn work_mutation_intent_recovers_after_item_only_crash() {
        let d = tempdir().unwrap();
        let intent_path;
        let expected;
        {
            let store = OrchStore::open(d.path()).unwrap();
            let (_, next, _, intent) = work_mutation_fixture(&store);
            expected = next.clone();
            intent_path = store.work_mutation_intent_path(&intent.intent_id).unwrap();
            store.persist_work_mutation_intent_unlocked(&intent).unwrap();
            store.save_work_item_unlocked(&next).unwrap();
        }

        let reopened = OrchStore::open(d.path()).unwrap();
        assert_eq!(
            reopened.load_work_item(&expected.work_id).unwrap(),
            Some(expected.clone())
        );
        assert_eq!(reopened.list_work_decisions(&expected.work_id).unwrap().len(), 1);
        assert!(!intent_path.exists());
    }

    #[test]
    fn work_mutation_intent_refuses_an_unexpected_prior_revision() {
        let d = tempdir().unwrap();
        {
            let store = OrchStore::open(d.path()).unwrap();
            let (prior, _, _, intent) = work_mutation_fixture(&store);
            store.persist_work_mutation_intent_unlocked(&intent).unwrap();
            let mut conflicting = prior;
            conflicting.priority = 7;
            conflicting.bump();
            store.save_work_item_unchecked_unlocked(&conflicting).unwrap();
        }

        let result = OrchStore::open(d.path());
        let error = match result {
            Ok(_) => panic!("recovery must refuse an unexpected prior revision"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unexpected prior revision"));
    }

    #[test]
    fn work_mutation_intent_binds_decision_to_expected_prior_revision() {
        let d = tempdir().unwrap();
        let store = OrchStore::open(d.path()).unwrap();
        let (_, _, _, mut intent) = work_mutation_fixture(&store);
        intent.expected_revision += 1;
        let error = intent
            .validate()
            .expect_err("a decision must authorize the exact prior revision");
        assert!(error.to_string().contains("records do not agree"));
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
            purpose: Default::default(),
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
            purpose: Default::default(),
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
            purpose: Default::default(),
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
        let report = store.close_audit_writer_bounded(std::time::Duration::from_secs(1));
        assert!(
            report.fully_stopped,
            "audit writer did not drain: {report:?}"
        );
        assert!(report.errors.is_empty(), "audit writer errors: {report:?}");
        drop(store);
        let body = fs::read_to_string(path).unwrap();
        assert!(body.contains("\"tool\":\"auth\""));
    }

    #[test]
    fn audit_writer_timeout_is_reported_without_blocking_the_caller() {
        let join_state = Arc::new((Mutex::new(AuditWriterJoinState::default()), Condvar::new()));
        let last_audit_error = Arc::new(Mutex::new(None));
        let writer = AuditWriter {
            tx: Mutex::new(None),
            join: Mutex::new(Some(
                std::thread::Builder::new()
                    .name("grokptah-test-uncooperative-audit".into())
                    .spawn(|| std::thread::sleep(std::time::Duration::from_millis(100)))
                    .unwrap(),
            )),
            join_state,
            last_audit_error,
        };
        let started = std::time::Instant::now();
        let report = writer.wait_bounded(std::time::Duration::from_millis(5));
        assert!(!report.fully_stopped);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("did not stop within")));
        assert!(started.elapsed() < std::time::Duration::from_millis(80));
        drop(writer);
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
        atomic_write_json(&store.lease(), &intent, &restart_candidate).unwrap();
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
        first.agent_spec_revision = Some(
            store
                .load_agent("agent-activation-race")
                .unwrap()
                .unwrap()
                .current_spec()
                .unwrap()
                .revision,
        );
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

        store
            .deactivate_agent_run("agent-activation-race", &first.run_id, false)
            .unwrap();
        second.state = RunState::Queued;
        second.start_seq = None;
        store.save_run(&second).unwrap();
        let promoted = store
            .promote_queued_run_and_activate_agent(&second.run_id, "agent-activation-race", 42)
            .unwrap()
            .unwrap();
        assert_eq!(promoted.state, RunState::Running);
        assert_eq!(promoted.start_seq, Some(42));
        assert_eq!(
            store
                .load_agent("agent-activation-race")
                .unwrap()
                .unwrap()
                .current_run_id
                .as_deref(),
            Some(second.run_id.as_str())
        );
    }

    #[test]
    fn legacy_run_without_purpose_defaults_to_execution() {
        let mut value = serde_json::to_value(terminal_run("legacy-purpose")).unwrap();
        value.as_object_mut().unwrap().remove("purpose");
        let run: RunRecord = serde_json::from_value(value).unwrap();
        assert_eq!(run.purpose, RunPurpose::Execution);
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
            run.purpose = RunPurpose::ManagerProposal;
            run.agent_id = Some("agent-activation-crash".into());
            run.terminal_result = None;
            run.final_response = None;
            run.end_seq = None;
            let mut prior_run = run.clone();
            prior_run.state = RunState::Queued;
            prior_run.start_seq = None;
            let mut activated = store.load_agent("agent-activation-crash").unwrap().unwrap();
            activated.state = AgentState::Active;
            activated.current_run_id = Some(run_id.into());
            activated.last_lane_id = Some(lane_id);
            let intent = AgentActivationIntent {
                run: run.clone(),
                activated_agent: activated,
                prior_run: Some(prior_run.clone()),
            };
            atomic_write_json(
                &store.lease(),
                &store.agent_activation_path(run_id).unwrap(),
                &intent,
            )
            .unwrap();
            atomic_write_json(&store.lease(), &store.run_path(run_id).unwrap(), &prior_run)
                .unwrap();
            // Simulated crash after the promotion intent but before replacing
            // the queued Run or activating its Agent.
        }

        let reopened = OrchStore::open(root.path()).unwrap();
        let run = reopened.load_run(run_id).unwrap().unwrap();
        let agent = reopened
            .load_agent("agent-activation-crash")
            .unwrap()
            .unwrap();
        assert_eq!(run.state, RunState::Interrupted);
        assert_eq!(run.purpose, RunPurpose::ManagerProposal);
        assert_eq!(agent.state, AgentState::Interrupted);
        assert_eq!(agent.current_run_id, None);
        assert_eq!(agent.last_run_id.as_deref(), Some(run_id));
        assert!(!reopened.agent_activation_path(run_id).unwrap().exists());
    }

    #[test]
    fn managed_grok_claim_fence_rejects_snapshot_to_claim_authority_changes() {
        use crate::orchestration::workload::{WorkPolicy, WorkRetryPolicy};

        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let lane_id = Uuid::new_v4();
        let now = Utc::now();
        store
            .save_agent(&AgentRecord {
                agent_id: "grok-fenced-agent".into(),
                owner_principal_id: None,
                session_id: lane_id,
                lane_ids: vec![lane_id],
                lane_associations: Vec::new(),
                workspace: "/tmp/grok-fenced-workspace".into(),
                model: "grok".into(),
                spec: None,
                state: AgentState::Active,
                current_run_id: None,
                last_run_id: None,
                last_lane_id: Some(lane_id),
                latest_checkpoint_id: None,
                continuation_ordinal: 0,
                created_at: now,
                updated_at: now,
            })
            .unwrap();
        let mut work = WorkItem::new(
            "isolated-review",
            "bounded claim fence",
            lane_id,
            "/tmp/grok-fenced-workspace",
            "operator",
            WorkPolicy {
                retry: WorkRetryPolicy {
                    max_attempts: 1,
                    retry_failed: false,
                    retry_expired: false,
                    backoff_ms: 0,
                },
                allowed_files: vec!["allowed.txt".into()],
                ..WorkPolicy::default()
            },
        )
        .unwrap();
        work.assigned_agent_id = Some("grok-fenced-agent".into());
        work.assignment_status = AssignmentStatus::Accepted;
        work.source_manager_plan_id = Some("plan-fence".into());
        work.source_manager_step_id = Some("step-fence".into());
        store.save_work_item(&work).unwrap();
        let (authorized, decision) = store
            .authorize_work_execution(
                &work.work_id,
                "operator",
                None,
                "authorize exact snapshot",
                Some(work.revision),
                now,
            )
            .unwrap();
        let spec_revision = store
            .load_agent("grok-fenced-agent")
            .unwrap()
            .unwrap()
            .current_spec()
            .unwrap()
            .revision;
        let expected_allowed = authorized.policy.allowed_files.clone();
        let fence = ManagedGrokClaimFence {
            expected_work_revision: authorized.revision,
            expected_decision_id: &decision.decision_id,
            expected_agent_spec_revision: spec_revision,
            expected_allowed_files: &expected_allowed,
        };

        let mut narrowed = authorized.clone();
        narrowed.policy.allowed_files = vec!["narrower.txt".into()];
        narrowed.bump_at(now + Duration::milliseconds(1));
        store.save_work_item(&narrowed).unwrap();

        let error = store
            .claim_managed_grok_work_with_lease_secret(
                &narrowed.work_id,
                "grok-fenced-agent",
                None,
                "claim-secret",
                &fence,
            )
            .expect_err("stale authority snapshot must not claim");
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert!(store
            .list_work_attempts(Some(&narrowed.work_id))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .load_work_item(&narrowed.work_id)
                .unwrap()
                .unwrap()
                .state,
            WorkState::Queued
        );
    }
}
