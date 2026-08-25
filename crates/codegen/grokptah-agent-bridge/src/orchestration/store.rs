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

use super::managed::{
    ManagedExecutionIntent, ManagedExecutionPolicy, ManagedFinalizationOutcome,
    ManagedFinalizationRecord, ManagedFinalizationStage, ManagedIntentState, ManagedRetryCause,
    AGENT_CEILING_EXHAUSTED, MANAGED_FINALIZATION_SCHEMA_VERSION, PROVIDER_CEILING_EXHAUSTED,
};
use super::manager::{ManagerDecisionRecord, ManagerPlan};
use super::message::{MessagePage, WorkMessage, MAX_RETAINED_MESSAGES};
use super::provider_attempt::{ProviderAttemptRecord, ProviderAttemptState, ProviderSendCertainty};
use super::quota::{QuotaPoolUsage, QuotaReservation, QuotaReservationState};
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
    lease_duration, AssignmentStatus, AttemptState, WorkApproval, WorkAttempt, WorkClaim,
    WorkDecision, WorkDecisionAction, WorkItem, WorkProgress, WorkResult, WorkState,
    WorkloadReconciliationReport, WORKLOAD_SCHEMA_VERSION,
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
    #[cfg(test)]
    persist_cut: Mutex<Option<AdmissionPersistCut>>,
    #[cfg(test)]
    attempt_index_files_read: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    session_run_index_files_read: std::sync::atomic::AtomicUsize,
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
    /// Present only when the Run is being admitted with a new provider-quota
    /// reservation. Legacy activation intents omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quota_reservation: Option<QuotaReservation>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaAdmissionIntent {
    run: RunRecord,
    reservation: QuotaReservation,
}

/// Outcome of a durable Run/quota/Agent admission write.
///
/// Recovery may still commit after a crash. Callers must not treat
/// [`Self::Uncertain`] as a zero-effect failure, and must not start a
/// provider until [`Self::Committed`].
#[derive(Debug)]
pub enum DurableAdmission {
    Committed,
    DefinitelyNotCommitted(anyhow::Error),
    Uncertain(anyhow::Error),
}

impl DurableAdmission {
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Committed)
    }

    /// Convert a typed admission outcome into `Result`.
    ///
    /// Uncertain stays [`UncertainAdmission`], not a zero-effect error. Production
    /// persist paths must match [`DurableAdmission`] directly; this conversion is
    /// for tests that still want a `Result`.
    pub fn into_result(self) -> anyhow::Result<()> {
        match self {
            Self::Committed => Ok(()),
            Self::DefinitelyNotCommitted(error) => Err(error),
            Self::Uncertain(error) => Err(UncertainAdmission(error).into()),
        }
    }

    fn from_partial_write(error: anyhow::Error, recovery_intent_present: bool) -> Self {
        if recovery_intent_present {
            Self::Uncertain(error)
        } else {
            Self::DefinitelyNotCommitted(error)
        }
    }
}

/// Typed failure for [`DurableAdmission::Uncertain`]. Callers must not treat
/// this as a zero-effect rejection: recovery may still commit the write.
#[derive(Debug, thiserror::Error)]
#[error("durable admission is uncertain: {0}")]
pub struct UncertainAdmission(#[source] pub anyhow::Error);

impl UncertainAdmission {
    pub fn is(error: &anyhow::Error) -> bool {
        error.downcast_ref::<Self>().is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionPersistCut {
    AfterIntent,
    AfterQuota,
    AfterRun,
    AfterAgent,
    AfterIntentRemoval,
    AfterAbortJournal,
    AfterAbortRun,
    AfterAbortAgent,
    AfterAbortQuota,
    AfterAbortJournalRemoval,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdmissionAbortJournal {
    run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reservation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    error_code: String,
    message: String,
}

pub const MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE: usize = 128;
pub const MAX_PUBLIC_RUN_LIST: usize = 128;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderAttemptIndexEntry {
    attempt_id: String,
    run_id: String,
    ordinal: u64,
    #[serde(default)]
    state: Option<ProviderAttemptState>,
}

#[derive(Debug, Clone)]
pub struct ProviderAttemptPage {
    pub attempts: Vec<ProviderAttemptRecord>,
    pub total_count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRunIndexEntry {
    run_id: String,
    session_id: Uuid,
    workspace: String,
    created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RunRecordPage {
    pub runs: Vec<RunRecord>,
    pub total_count: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
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

fn validate_run_provider_route_for_spec(run: &RunRecord, spec: &AgentSpec) -> anyhow::Result<()> {
    if let Some(route) = &run.provider_route {
        route
            .validate()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            route.selection_key == spec.model.selection_key,
            "Run provider route does not match its captured Agent specification"
        );
    }
    Ok(())
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
        fs::create_dir_all(root.join("quota-reservations"))?;
        fs::create_dir_all(root.join("quota-admission-intents"))?;
        fs::create_dir_all(root.join("provider-attempts"))?;
        fs::create_dir_all(root.join("provider-attempt-index"))?;
        fs::create_dir_all(root.join("session-run-index"))?;
        fs::create_dir_all(root.join("admission-abort"))?;
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
                #[cfg(test)]
                persist_cut: Mutex::new(None),
                #[cfg(test)]
                attempt_index_files_read: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                session_run_index_files_read: std::sync::atomic::AtomicUsize::new(0),
            }),
        };
        store.recover_quota_admission_intents()?;
        store.recover_agent_activation_intents()?;
        store.rebuild_provider_attempt_index()?;
        store.recover_admission_abort_journals()?;
        store.recover_finalization_intents()?;
        store.recover_routine_intents()?;
        store.recover_managed_finalization_intents()?;
        store.recover_manager_creation_intents()?;
        store.reconcile_provider_attempt_ledger()?;
        store.mark_unfinished_interrupted()?;
        store.rebuild_session_run_index()?;
        store.reconcile_quota_ledger()?;
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

    fn quota_reservation_path(&self, reservation_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(reservation_id)?;
        Ok(self
            .inner
            .root
            .join("quota-reservations")
            .join(format!("{safe}.json")))
    }

    fn quota_admission_intent_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("quota-admission-intents")
            .join(format!("{safe}.json")))
    }

    fn provider_attempt_path(&self, attempt_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(attempt_id)?;
        Ok(self
            .inner
            .root
            .join("provider-attempts")
            .join(format!("{safe}.json")))
    }

    fn provider_attempt_index_dir(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self.inner.root.join("provider-attempt-index").join(safe))
    }

    fn provider_attempt_index_path(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<PathBuf, OrchError> {
        let safe_attempt = safe_id_filename(attempt_id)?;
        Ok(self
            .provider_attempt_index_dir(run_id)?
            .join(format!("{safe_attempt}.json")))
    }

    fn session_run_index_dir(&self, session_id: Uuid) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(&session_id.to_string())?;
        Ok(self.inner.root.join("session-run-index").join(safe))
    }

    fn session_run_index_path(&self, session_id: Uuid, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe_run = safe_id_filename(run_id)?;
        Ok(self
            .session_run_index_dir(session_id)?
            .join(format!("{safe_run}.json")))
    }

    fn admission_abort_path(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self
            .inner
            .root
            .join("admission-abort")
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

    fn load_quota_reservation_unlocked(
        &self,
        reservation_id: &str,
    ) -> anyhow::Result<Option<QuotaReservation>> {
        let path = match self.quota_reservation_path(reservation_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let mut reservation: QuotaReservation = serde_json::from_str(&fs::read_to_string(path)?)?;
        reservation.migrate_host_wide_pool();
        reservation
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(reservation))
    }

    pub fn load_quota_reservation(
        &self,
        reservation_id: &str,
    ) -> anyhow::Result<Option<QuotaReservation>> {
        let _guard = self.inner.lock.lock();
        self.load_quota_reservation_unlocked(reservation_id)
    }

    fn list_quota_reservations_unlocked(&self) -> anyhow::Result<Vec<QuotaReservation>> {
        let mut reservations = Vec::new();
        for entry in fs::read_dir(self.inner.root.join("quota-reservations"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let mut reservation: QuotaReservation =
                serde_json::from_str(&fs::read_to_string(path)?)?;
            reservation.migrate_host_wide_pool();
            reservation
                .validate()
                .map_err(|error| anyhow::anyhow!(error))?;
            reservations.push(reservation);
        }
        Ok(reservations)
    }

    pub fn list_quota_reservations(&self) -> anyhow::Result<Vec<QuotaReservation>> {
        let _guard = self.inner.lock.lock();
        self.list_quota_reservations_unlocked()
    }

    fn load_provider_attempt_unlocked(
        &self,
        attempt_id: &str,
    ) -> anyhow::Result<Option<ProviderAttemptRecord>> {
        let path = match self.provider_attempt_path(attempt_id) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if !path.is_file() {
            return Ok(None);
        }
        let attempt: ProviderAttemptRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
        attempt.validate().map_err(|error| anyhow::anyhow!(error))?;
        Ok(Some(attempt))
    }

    pub fn load_provider_attempt(
        &self,
        attempt_id: &str,
    ) -> anyhow::Result<Option<ProviderAttemptRecord>> {
        let _guard = self.inner.lock.lock();
        self.load_provider_attempt_unlocked(attempt_id)
    }

    fn list_provider_attempts_unlocked(&self) -> anyhow::Result<Vec<ProviderAttemptRecord>> {
        let mut attempts = Vec::new();
        for entry in fs::read_dir(self.inner.root.join("provider-attempts"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let attempt: ProviderAttemptRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
            attempt.validate().map_err(|error| anyhow::anyhow!(error))?;
            attempts.push(attempt);
        }
        attempts.sort_by(|left, right| {
            left.run_id
                .cmp(&right.run_id)
                .then(left.ordinal.cmp(&right.ordinal))
                .then(left.attempt_id.cmp(&right.attempt_id))
        });
        Ok(attempts)
    }

    pub fn list_provider_attempts(&self) -> anyhow::Result<Vec<ProviderAttemptRecord>> {
        let _guard = self.inner.lock.lock();
        self.list_provider_attempts_unlocked()
    }

    /// Indexed per-run query: total count plus the first 128 attempts.
    /// Does not scan foreign attempt files.
    pub fn list_provider_attempts_for_run(
        &self,
        run_id: &str,
    ) -> anyhow::Result<ProviderAttemptPage> {
        let _guard = self.inner.lock.lock();
        self.list_provider_attempts_for_run_unlocked(run_id)
    }

    #[cfg(test)]
    pub fn attempt_index_files_read(&self) -> usize {
        self.inner
            .attempt_index_files_read
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn reset_attempt_index_files_read(&self) {
        self.inner
            .attempt_index_files_read
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn session_run_index_files_read(&self) -> usize {
        self.inner
            .session_run_index_files_read
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn reset_session_run_index_files_read(&self) {
        self.inner
            .session_run_index_files_read
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn set_persist_cut(&self, cut: Option<AdmissionPersistCut>) {
        *self.inner.persist_cut.lock() = cut;
    }

    #[cfg(test)]
    pub fn test_put_provider_attempt(
        &self,
        attempt: &crate::orchestration::ProviderAttemptRecord,
    ) -> anyhow::Result<()> {
        let _guard = self.inner.lock.lock();
        self.save_provider_attempt_unlocked(attempt)
    }

    fn list_provider_attempt_index_unlocked(
        &self,
        run_id: &str,
    ) -> anyhow::Result<Vec<ProviderAttemptIndexEntry>> {
        let dir = match self.provider_attempt_index_dir(run_id) {
            Ok(dir) => dir,
            Err(_) => return Ok(Vec::new()),
        };
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            #[cfg(test)]
            self.inner
                .attempt_index_files_read
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let record: ProviderAttemptIndexEntry =
                serde_json::from_str(&fs::read_to_string(&path)?)?;
            if record.run_id == run_id {
                entries.push(record);
            }
        }
        entries.sort_by(|left, right| {
            left.ordinal
                .cmp(&right.ordinal)
                .then(left.attempt_id.cmp(&right.attempt_id))
        });
        Ok(entries)
    }

    fn list_provider_attempts_for_run_unlocked(
        &self,
        run_id: &str,
    ) -> anyhow::Result<ProviderAttemptPage> {
        let index_ids = self.list_provider_attempt_index_unlocked(run_id)?;
        let total_count = index_ids.len();
        let truncated = total_count > MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE;
        let mut attempts = Vec::new();
        for entry in index_ids
            .into_iter()
            .take(MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE)
        {
            #[cfg(test)]
            self.inner
                .attempt_index_files_read
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(attempt) = self.load_provider_attempt_unlocked(&entry.attempt_id)? {
                if attempt.run_id == run_id {
                    attempts.push(attempt);
                }
            }
        }
        Ok(ProviderAttemptPage {
            attempts,
            total_count,
            truncated,
        })
    }

    fn index_provider_attempt_unlocked(
        &self,
        attempt: &ProviderAttemptRecord,
    ) -> anyhow::Result<()> {
        let dir = self
            .provider_attempt_index_dir(&attempt.run_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        fs::create_dir_all(&dir)?;
        let path = self
            .provider_attempt_index_path(&attempt.run_id, &attempt.attempt_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        let entry = ProviderAttemptIndexEntry {
            attempt_id: attempt.attempt_id.clone(),
            run_id: attempt.run_id.clone(),
            ordinal: attempt.ordinal,
            state: Some(attempt.state),
        };
        atomic_write_json(&path, &entry)
    }

    fn rebuild_provider_attempt_index(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let mut rebuilt = 0;
        for attempt in self.list_provider_attempts_unlocked()? {
            let path = self
                .provider_attempt_index_path(&attempt.run_id, &attempt.attempt_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            if !path.is_file() {
                self.index_provider_attempt_unlocked(&attempt)?;
                rebuilt += 1;
            }
        }
        Ok(rebuilt)
    }

    fn write_run_record_at_unlocked(&self, path: &Path, run: &RunRecord) -> anyhow::Result<()> {
        atomic_write_json(path, run)?;
        self.index_session_run_unlocked(run)
    }

    fn index_session_run_unlocked(&self, run: &RunRecord) -> anyhow::Result<()> {
        let dir = self
            .session_run_index_dir(run.session_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        fs::create_dir_all(&dir)?;
        let path = self
            .session_run_index_path(run.session_id, &run.run_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        let entry = SessionRunIndexEntry {
            run_id: run.run_id.clone(),
            session_id: run.session_id,
            workspace: run.workspace.clone(),
            created_at: run.created_at,
        };
        atomic_write_json(&path, &entry)
    }

    fn remove_session_run_index_unlocked(&self, run: &RunRecord) -> anyhow::Result<()> {
        let path = match self.session_run_index_path(run.session_id, &run.run_id) {
            Ok(path) => path,
            Err(_) => return Ok(()),
        };
        if path.is_file() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn list_session_run_index_unlocked(
        &self,
        session_id: Uuid,
    ) -> anyhow::Result<Vec<SessionRunIndexEntry>> {
        let dir = match self.session_run_index_dir(session_id) {
            Ok(dir) => dir,
            Err(_) => return Ok(Vec::new()),
        };
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            #[cfg(test)]
            self.inner
                .session_run_index_files_read
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let record: SessionRunIndexEntry = serde_json::from_str(&fs::read_to_string(&path)?)?;
            if record.session_id == session_id {
                entries.push(record);
            }
        }
        entries.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then(right.run_id.cmp(&left.run_id))
        });
        Ok(entries)
    }

    fn rebuild_session_run_index(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let mut rebuilt = 0;
        let mut live = std::collections::HashSet::new();
        for run in self.list_runs_unlocked()? {
            live.insert((run.session_id, run.run_id.clone()));
            self.index_session_run_unlocked(&run)?;
            rebuilt += 1;
        }
        let index_root = self.inner.root.join("session-run-index");
        if index_root.is_dir() {
            for session_dir in fs::read_dir(&index_root)? {
                let session_dir = session_dir?.path();
                if !session_dir.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(&session_dir)? {
                    let path = entry?.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(text) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(record) = serde_json::from_str::<SessionRunIndexEntry>(&text) else {
                        continue;
                    };
                    if !live.contains(&(record.session_id, record.run_id)) {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
        Ok(rebuilt)
    }

    /// Session-scoped Run query: total matching count plus one bounded page.
    /// Does not scan Runs owned by another session.
    pub fn list_runs_for_session_page(
        &self,
        session_id: Uuid,
        workspace: Option<&str>,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> anyhow::Result<RunRecordPage> {
        let _guard = self.inner.lock.lock();
        self.list_runs_for_session_page_unlocked(session_id, workspace, cursor, limit)
    }

    /// Every Run for one session (and optional workspace), from the session index.
    pub fn list_runs_for_session(
        &self,
        session_id: Uuid,
        workspace: Option<&str>,
    ) -> anyhow::Result<Vec<RunRecord>> {
        let _guard = self.inner.lock.lock();
        let mut entries = self.list_session_run_index_unlocked(session_id)?;
        if let Some(workspace) = workspace {
            entries.retain(|entry| workspaces_match(&entry.workspace, workspace));
        }
        let mut runs = Vec::new();
        for entry in entries {
            #[cfg(test)]
            self.inner
                .session_run_index_files_read
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(run) = self.load_run_unlocked(&entry.run_id)? {
                if run.session_id == session_id
                    && workspace.is_none_or(|workspace| workspaces_match(&run.workspace, workspace))
                {
                    runs.push(run);
                }
            }
        }
        Ok(runs)
    }

    fn list_runs_for_session_page_unlocked(
        &self,
        session_id: Uuid,
        workspace: Option<&str>,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> anyhow::Result<RunRecordPage> {
        let mut entries = self.list_session_run_index_unlocked(session_id)?;
        if let Some(workspace) = workspace {
            entries.retain(|entry| workspaces_match(&entry.workspace, workspace));
        }
        let total_count = entries.len();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            if let Some(index) = entries.iter().position(|entry| entry.run_id == cursor) {
                entries = entries.split_off(index.saturating_add(1));
            } else {
                entries.clear();
            }
        }
        let limit = limit
            .unwrap_or(MAX_PUBLIC_RUN_LIST)
            .clamp(1, MAX_PUBLIC_RUN_LIST);
        let truncated = entries.len() > limit;
        let mut runs = Vec::new();
        for entry in entries.into_iter().take(limit) {
            #[cfg(test)]
            self.inner
                .session_run_index_files_read
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(run) = self.load_run_unlocked(&entry.run_id)? {
                if run.session_id == session_id
                    && workspace.is_none_or(|workspace| workspaces_match(&run.workspace, workspace))
                {
                    runs.push(run);
                }
            }
        }
        let next_cursor = truncated
            .then(|| runs.last().map(|run| run.run_id.clone()))
            .flatten();
        Ok(RunRecordPage {
            runs,
            total_count,
            truncated,
            next_cursor,
        })
    }

    fn list_runs_unlocked(&self) -> anyhow::Result<Vec<RunRecord>> {
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

    fn save_provider_attempt_unlocked(
        &self,
        attempt: &ProviderAttemptRecord,
    ) -> anyhow::Result<()> {
        attempt.validate().map_err(|error| anyhow::anyhow!(error))?;
        let path = self
            .provider_attempt_path(&attempt.attempt_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        atomic_write_json(&path, attempt)?;
        self.index_provider_attempt_unlocked(attempt)
    }

    fn validate_provider_attempt_binding(
        run: &RunRecord,
        attempt: &ProviderAttemptRecord,
    ) -> anyhow::Result<()> {
        attempt.validate().map_err(|error| anyhow::anyhow!(error))?;
        let route = run
            .provider_route
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("provider attempt Run has no route snapshot"))?;
        anyhow::ensure!(
            run.run_id == attempt.run_id
                && route.quota_reservation_id.as_deref() == Some(attempt.reservation_id.as_str())
                && route.snapshot_hash == attempt.route_snapshot_hash,
            "Run and provider attempt immutable identities do not match"
        );
        Ok(())
    }

    /// Install one durable provider attempt before transport dispatch. The
    /// attempt row is the recovery anchor; if the subsequent Run write fails,
    /// callers do not send and restart conservatively closes the row.
    pub fn begin_provider_attempt(&self, run_id: &str) -> anyhow::Result<ProviderAttemptRecord> {
        let _guard = self.inner.lock.lock();
        let mut run = self
            .load_run_unlocked(run_id)?
            .ok_or_else(|| anyhow::anyhow!("provider attempt Run is missing"))?;
        anyhow::ensure!(
            run.state == RunState::Running,
            "provider attempt Run is not active"
        );
        let reservation_id = run
            .provider_route
            .as_ref()
            .and_then(|route| route.quota_reservation_id.as_deref())
            .ok_or_else(|| anyhow::anyhow!("provider attempt Run has no quota reservation"))?;
        let reservation = self
            .load_quota_reservation_unlocked(reservation_id)?
            .ok_or_else(|| anyhow::anyhow!("provider attempt quota reservation is missing"))?;
        Self::validate_quota_binding(&run, &reservation)?;
        anyhow::ensure!(
            reservation.state == QuotaReservationState::Reserved,
            "provider attempt quota reservation is not active"
        );

        let index = self.list_provider_attempt_index_unlocked(run_id)?;
        anyhow::ensure!(
            !index
                .iter()
                .any(|entry| entry.state == Some(ProviderAttemptState::Admitted)),
            "provider attempt already admitted for this Run"
        );
        let ordinal = index
            .iter()
            .map(|entry| entry.ordinal)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("provider attempt ordinal overflowed"))?;
        let now = Utc::now();
        let attempt = ProviderAttemptRecord::admitted(
            &run,
            format!("provider-attempt-{}", Uuid::new_v4()),
            ordinal,
            now,
        )
        .map_err(|error| anyhow::anyhow!(error))?;
        self.save_provider_attempt_unlocked(&attempt)?;

        run.aggregates.usage_pending_requests = run
            .aggregates
            .usage_pending_requests
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("provider attempt counter overflowed"))?;
        run.updated_at = now;
        let run_path = self
            .run_path(run_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        if let Err(error) = self.write_run_record_at_unlocked(&run_path, &run) {
            let mut not_sent = attempt.clone();
            not_sent
                .finish(ProviderSendCertainty::KnownNotSent, None, None, Utc::now())
                .map_err(|finish_error| anyhow::anyhow!(finish_error))?;
            self.save_provider_attempt_unlocked(&not_sent)?;
            return Err(error.context("persist provider attempt on Run; transport was not entered"));
        }
        Ok(attempt)
    }

    fn apply_provider_attempt_to_run(
        &self,
        run: &mut RunRecord,
        attempt: &ProviderAttemptRecord,
    ) -> anyhow::Result<bool> {
        if attempt.state != ProviderAttemptState::Finished
            || run
                .aggregates
                .accounted_provider_attempt_ids
                .iter()
                .any(|attempt_id| attempt_id == &attempt.attempt_id)
        {
            return Ok(false);
        }
        Self::validate_provider_attempt_binding(run, attempt)?;
        run.aggregates.usage_pending_requests = u32::try_from(
            self.list_provider_attempt_index_unlocked(&run.run_id)?
                .into_iter()
                .filter(|entry| entry.state == Some(ProviderAttemptState::Admitted))
                .count(),
        )
        .map_err(|_| anyhow::anyhow!("provider attempt counter overflowed"))?;
        match attempt.send_certainty {
            Some(ProviderSendCertainty::KnownAccepted) => {
                if let Some(usage) = &attempt.usage {
                    run.aggregates.usage.prompt_tokens = run
                        .aggregates
                        .usage
                        .prompt_tokens
                        .checked_add(usage.prompt_tokens)
                        .ok_or_else(|| anyhow::anyhow!("provider token accounting overflowed"))?;
                    run.aggregates.usage.completion_tokens = run
                        .aggregates
                        .usage
                        .completion_tokens
                        .checked_add(usage.completion_tokens)
                        .ok_or_else(|| anyhow::anyhow!("provider token accounting overflowed"))?;
                    run.aggregates.usage.total_tokens = run
                        .aggregates
                        .usage
                        .total_tokens
                        .checked_add(usage.total_tokens)
                        .ok_or_else(|| anyhow::anyhow!("provider token accounting overflowed"))?;
                    run.aggregates.usage.requests = run
                        .aggregates
                        .usage
                        .requests
                        .checked_add(usage.requests)
                        .ok_or_else(|| anyhow::anyhow!("provider request accounting overflowed"))?;
                } else if attempt.http_status.is_none_or(|status| status < 400) {
                    run.aggregates.usage_complete = false;
                }
            }
            Some(ProviderSendCertainty::UncertainAccept) => {
                run.aggregates.usage_complete = false;
            }
            Some(ProviderSendCertainty::KnownNotSent) => {}
            None => anyhow::bail!("finished provider attempt has no send certainty"),
        }
        run.aggregates
            .accounted_provider_attempt_ids
            .push(attempt.attempt_id.clone());
        if let Some(verification) = run.aggregates.verification.as_mut() {
            verification.usage = run.aggregates.usage.clone();
        }
        if !run.aggregates.usage_complete && run.bounds.max_total_tokens.is_some() {
            let code = "max_total_tokens_usage_unavailable";
            run.error_code = Some(code.into());
            run.stop_cause = Some(RunStopCause::TokenAccountingUnavailable);
        } else if let Some(ceiling) = run.bounds.max_total_tokens {
            if run.aggregates.usage.total_tokens >= ceiling {
                run.error_code = Some("max_total_tokens_reached".into());
                run.stop_cause = Some(RunStopCause::TokenCeiling);
            }
        }
        run.updated_at = run.updated_at.max(attempt.updated_at);
        Ok(true)
    }

    /// Finish an attempt and fold its usage into the Run exactly once. The
    /// attempt is written first, so restart can complete the fold after a
    /// crash between the two durable records.
    pub fn finish_provider_attempt(
        &self,
        attempt_id: &str,
        certainty: ProviderSendCertainty,
        http_status: Option<u16>,
        usage: Option<crate::completion::CompletionUsage>,
    ) -> anyhow::Result<RunRecord> {
        let _guard = self.inner.lock.lock();
        let mut attempt = self
            .load_provider_attempt_unlocked(attempt_id)?
            .ok_or_else(|| anyhow::anyhow!("provider attempt is missing"))?;
        attempt
            .finish(certainty, http_status, usage, Utc::now())
            .map_err(|error| anyhow::anyhow!(error))?;
        self.save_provider_attempt_unlocked(&attempt)?;

        let mut run = self
            .load_run_unlocked(&attempt.run_id)?
            .ok_or_else(|| anyhow::anyhow!("provider attempt Run is missing"))?;
        if self.apply_provider_attempt_to_run(&mut run, &attempt)? {
            self.preflight_quota_for_run_unlocked(&run)?;
            let path = self
                .run_path(&run.run_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            self.write_run_record_at_unlocked(&path, &run)?;
        }
        // Keep settlement retryable even if an earlier call durably applied
        // the attempt to the Run and then failed before writing the quota row.
        self.sync_quota_for_run_unlocked(&run)?;
        Ok(run)
    }

    fn validate_quota_binding(
        run: &RunRecord,
        reservation: &QuotaReservation,
    ) -> anyhow::Result<()> {
        reservation
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        let route = run
            .provider_route
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("quota reservation Run has no provider route"))?;
        route.validate().map_err(|error| anyhow::anyhow!(error))?;
        anyhow::ensure!(
            route.quota_reservation_id.as_deref() == Some(reservation.reservation_id.as_str())
                && route.quota_class == Some(reservation.pool.class)
                && route.snapshot_hash == reservation.route_snapshot_hash
                && route.provider_id == reservation.pool.provider_id
                && route.credential_fingerprint == reservation.pool.credential_fingerprint
                && run.run_id == reservation.run_id
                && workspaces_match(&run.workspace, &reservation.pool.workspace),
            "Run and quota reservation immutable identities do not match"
        );
        Ok(())
    }

    fn ensure_quota_capacity_unlocked(&self, candidate: &QuotaReservation) -> anyhow::Result<()> {
        let mut usage = QuotaPoolUsage::default();
        for reservation in self.list_quota_reservations_unlocked()? {
            if reservation.reservation_id == candidate.reservation_id
                || reservation.run_id == candidate.run_id
            {
                anyhow::bail!(OrchError::new(
                    OrchErrorCode::Conflict,
                    "quota reservation or Run identity already exists",
                ));
            }
            usage
                .include(&reservation, &candidate.pool, candidate.window_started_at)
                .map_err(|error| anyhow::anyhow!(error))?;
        }
        usage
            .ensure_can_reserve(candidate)
            .map_err(|error| anyhow::anyhow!(error))
    }

    fn save_quota_reservation_unlocked(
        &self,
        reservation: &QuotaReservation,
    ) -> anyhow::Result<()> {
        reservation
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        let path = self
            .quota_reservation_path(&reservation.reservation_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        atomic_write_json(&path, reservation)
    }

    fn sync_quota_for_run_unlocked(&self, run: &RunRecord) -> anyhow::Result<()> {
        let Some(reservation_id) = run
            .provider_route
            .as_ref()
            .and_then(|route| route.quota_reservation_id.as_deref())
        else {
            return Ok(());
        };
        let mut reservation = self
            .load_quota_reservation_unlocked(reservation_id)?
            .ok_or_else(|| anyhow::anyhow!("Run quota reservation is missing"))?;
        Self::validate_quota_binding(run, &reservation)?;
        if run.state.is_terminal()
            && run.aggregates.usage_complete
            && run.aggregates.usage_pending_requests == 0
        {
            reservation
                .settle(
                    run.aggregates.usage.total_tokens,
                    run.aggregates.usage.requests,
                    run.updated_at,
                )
                .map_err(|error| anyhow::anyhow!(error))?;
            self.save_quota_reservation_unlocked(&reservation)?;
        }
        Ok(())
    }

    fn preflight_quota_for_run_unlocked(&self, run: &RunRecord) -> anyhow::Result<()> {
        let Some(reservation_id) = run
            .provider_route
            .as_ref()
            .and_then(|route| route.quota_reservation_id.as_deref())
        else {
            return Ok(());
        };
        let reservation = self
            .load_quota_reservation_unlocked(reservation_id)?
            .ok_or_else(|| anyhow::anyhow!("Run quota reservation is missing"))?;
        Self::validate_quota_binding(run, &reservation)?;
        if run.state.is_terminal()
            && run.aggregates.usage_complete
            && run.aggregates.usage_pending_requests == 0
        {
            let mut candidate = reservation;
            candidate
                .settle(
                    run.aggregates.usage.total_tokens,
                    run.aggregates.usage.requests,
                    run.updated_at,
                )
                .map_err(|error| anyhow::anyhow!(error))?;
        }
        Ok(())
    }

    /// Atomically install a queued Run and its provider-quota reservation.
    /// The intent is the recovery anchor for every crash cut point.
    pub fn admit_run_with_quota(
        &self,
        run: &RunRecord,
        reservation: &QuotaReservation,
    ) -> DurableAdmission {
        let _guard = self.inner.lock.lock();
        if let Err(error) = Self::validate_quota_binding(run, reservation) {
            return DurableAdmission::DefinitelyNotCommitted(error);
        }
        if let Err(error) = self.ensure_quota_capacity_unlocked(reservation) {
            return DurableAdmission::DefinitelyNotCommitted(error);
        }
        let run_path = match self.run_path(&run.run_id) {
            Ok(path) => path,
            Err(error) => {
                return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(error));
            }
        };
        if run_path.is_file() {
            return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(
                "Run ID already exists"
            ));
        }
        let reservation_path = match self.quota_reservation_path(&reservation.reservation_id) {
            Ok(path) => path,
            Err(error) => {
                return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(error));
            }
        };
        if reservation_path.is_file() {
            return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(
                "quota reservation ID already exists"
            ));
        }
        let intent_path = match self.quota_admission_intent_path(&run.run_id) {
            Ok(path) => path,
            Err(error) => {
                return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(error));
            }
        };
        let intent = QuotaAdmissionIntent {
            run: run.clone(),
            reservation: reservation.clone(),
        };
        if let Err(error) = atomic_write_json(&intent_path, &intent) {
            return DurableAdmission::DefinitelyNotCommitted(
                error.context("persist quota admission intent"),
            );
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterIntent, intent_path.is_file())
        {
            return outcome;
        }
        if let Err(error) = atomic_write_json(&reservation_path, reservation) {
            let intent_removed = remove_file_durable(&intent_path).is_ok();
            return DurableAdmission::from_partial_write(
                error.context("persist quota reservation"),
                !intent_removed,
            );
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterQuota, intent_path.is_file())
        {
            return outcome;
        }
        if let Err(error) = self.write_run_record_at_unlocked(&run_path, run) {
            let reservation_removed = remove_file_durable(&reservation_path).is_ok();
            let intent_removed = remove_file_durable(&intent_path).is_ok();
            return DurableAdmission::from_partial_write(
                error.context("persist quota-backed Run"),
                !(reservation_removed && intent_removed),
            );
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterRun, intent_path.is_file())
        {
            return outcome;
        }
        if let Some(outcome) = self.injected_persist_cut(
            AdmissionPersistCut::AfterIntentRemoval,
            intent_path.is_file(),
        ) {
            return outcome;
        }
        if let Err(error) = remove_file_durable(&intent_path) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
        }
        DurableAdmission::Committed
    }

    fn injected_persist_cut(
        &self,
        expected: AdmissionPersistCut,
        recovery_intent_present: bool,
    ) -> Option<DurableAdmission> {
        #[cfg(test)]
        {
            let mut cut = self.inner.persist_cut.lock();
            if *cut == Some(expected) {
                *cut = None;
                return Some(DurableAdmission::from_partial_write(
                    anyhow::anyhow!("injected persist cut {expected:?}"),
                    recovery_intent_present,
                ));
            }
        }
        #[cfg(not(test))]
        {
            let _ = (expected, recovery_intent_present);
        }
        None
    }

    fn hit_result_persist_cut(&self, expected: AdmissionPersistCut) -> bool {
        #[cfg(test)]
        {
            let mut cut = self.inner.persist_cut.lock();
            if *cut == Some(expected) {
                *cut = None;
                return true;
            }
        }
        #[cfg(not(test))]
        {
            let _ = expected;
        }
        false
    }

    /// Persist a new Run that is not quota-backed. Callers must match
    /// [`DurableAdmission`]. A written Run file with a later index failure is
    /// [`Self::Uncertain`], not a zero-effect rejection.
    pub fn admit_run(&self, run: &RunRecord) -> DurableAdmission {
        let _guard = self.inner.lock.lock();
        if run
            .provider_route
            .as_ref()
            .is_some_and(|route| route.quota_reservation_id.is_some())
        {
            return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(
                "quota-backed Run must be created atomically with its reservation"
            ));
        }
        if let Some(route) = &run.provider_route {
            if let Err(error) = route.validate() {
                return DurableAdmission::DefinitelyNotCommitted(
                    anyhow::anyhow!(error.to_string()),
                );
            }
        }
        if let Err(error) = self.preflight_quota_for_run_unlocked(run) {
            return DurableAdmission::DefinitelyNotCommitted(error);
        }
        let run_path = match self.run_path(&run.run_id) {
            Ok(path) => path,
            Err(error) => {
                return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(error));
            }
        };
        if run_path.is_file() {
            return DurableAdmission::DefinitelyNotCommitted(anyhow::anyhow!(
                "Run ID already exists"
            ));
        }
        if let Err(error) = self.write_run_record_at_unlocked(&run_path, run) {
            return DurableAdmission::from_partial_write(
                error.context("persist Run"),
                run_path.is_file(),
            );
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterRun, run_path.is_file())
        {
            return outcome;
        }
        DurableAdmission::Committed
    }

    pub fn save_run(&self, run: &RunRecord) -> anyhow::Result<()> {
        let _g = self.inner.lock.lock();
        if let Some(route) = &run.provider_route {
            route
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        if let Some(existing) = self.load_run_unlocked(&run.run_id)? {
            anyhow::ensure!(
                existing.provider_route == run.provider_route,
                "Run provider route snapshot is immutable"
            );
        } else if run
            .provider_route
            .as_ref()
            .is_some_and(|route| route.quota_reservation_id.is_some())
        {
            anyhow::bail!("quota-backed Run must be created atomically with its reservation");
        }
        self.preflight_quota_for_run_unlocked(run)?;
        let result = self
            .run_path(&run.run_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
            .and_then(|path| self.write_run_record_at_unlocked(&path, run));
        if result.is_ok() {
            if let Err(error) = self.sync_quota_for_run_unlocked(run) {
                *self.inner.last_run_error.lock() = Some(error.to_string());
                return Err(error);
            }
        }
        *self.inner.last_run_error.lock() = result.as_ref().err().map(ToString::to_string);
        result
    }

    /// Serialize Agent activation with durable Run creation so two Lanes
    /// cannot both pass the active-Run check and execute under one identity.
    pub fn admit_run_and_activate_agent(
        &self,
        run: &RunRecord,
        agent_id: &str,
        quota_reservation: Option<&QuotaReservation>,
    ) -> DurableAdmission {
        self.admit_run_and_activate_agent_with_candidate(run, agent_id, quota_reservation, None)
    }

    /// Same as [`Self::admit_run_and_activate_agent`], but first-use Agents may
    /// be supplied purely in memory and persisted only with Run+quota.
    pub fn admit_run_and_activate_agent_with_candidate(
        &self,
        run: &RunRecord,
        agent_id: &str,
        quota_reservation: Option<&QuotaReservation>,
        pending_agent: Option<&AgentRecord>,
    ) -> DurableAdmission {
        match self.save_run_and_activate_agent_inner(
            run,
            agent_id,
            quota_reservation,
            pending_agent,
        ) {
            Ok(()) => DurableAdmission::Committed,
            Err(error) => {
                let restart = error
                    .chain()
                    .any(|cause| cause.to_string().contains("requires restart"));
                DurableAdmission::from_partial_write(error, restart)
            }
        }
    }

    /// Compensate an unstarted admission. Writes a durable abort journal first,
    /// then terminalizes the Run, settles quota, and clears Agent activation.
    /// The Run identity is retained. Cleanup failure is never ignored.
    /// Callers must match [`DurableAdmission`]; Uncertain is not a zero-effect `Err`.
    pub fn abort_unstarted_run_admission(&self, run_id: &str) -> DurableAdmission {
        self.terminalize_unstarted_admission(
            run_id,
            "admission_aborted",
            "unstarted admission was compensated before provider start",
        )
    }

    pub fn terminalize_unstarted_admission(
        &self,
        run_id: &str,
        error_code: &str,
        message: &str,
    ) -> DurableAdmission {
        let _g = self.inner.lock.lock();
        self.terminalize_unstarted_admission_unlocked(run_id, error_code, message)
    }

    fn terminalize_unstarted_admission_unlocked(
        &self,
        run_id: &str,
        error_code: &str,
        message: &str,
    ) -> DurableAdmission {
        let run = match self.load_run_unlocked(run_id) {
            Ok(Some(run)) => run,
            Ok(None) => return DurableAdmission::Committed,
            Err(error) => {
                return DurableAdmission::Uncertain(error);
            }
        };
        let journal = AdmissionAbortJournal {
            run_id: run_id.to_string(),
            reservation_id: run
                .provider_route
                .as_ref()
                .and_then(|route| route.quota_reservation_id.clone()),
            agent_id: run.agent_id.clone(),
            error_code: error_code.to_string(),
            message: message.to_string(),
        };
        let journal_path = match self.admission_abort_path(run_id) {
            Ok(path) => path,
            Err(error) => {
                return DurableAdmission::Uncertain(anyhow::anyhow!(error));
            }
        };
        if let Err(error) = atomic_write_json(&journal_path, &journal) {
            return DurableAdmission::Uncertain(error.context("persist admission abort journal"));
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterAbortJournal, true)
        {
            return outcome;
        }
        if let Err(error) = self.apply_admission_abort_unlocked(&journal) {
            return DurableAdmission::Uncertain(error);
        }
        if let Some(outcome) =
            self.injected_persist_cut(AdmissionPersistCut::AfterAbortJournalRemoval, true)
        {
            return outcome;
        }
        if let Err(error) = remove_file_durable(&journal_path) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
            return DurableAdmission::Uncertain(
                error.context("remove admission abort journal; compensation requires restart"),
            );
        }
        DurableAdmission::Committed
    }

    fn apply_admission_abort_unlocked(
        &self,
        journal: &AdmissionAbortJournal,
    ) -> anyhow::Result<()> {
        let run_id = journal.run_id.as_str();
        let Some(mut run) = self.load_run_unlocked(run_id)? else {
            return Ok(());
        };
        let page = self.list_provider_attempts_for_run_unlocked(run_id)?;
        if run.state.is_terminal() && page.total_count == 0 {
            self.clear_agent_activation_unlocked(run_id, journal.agent_id.as_deref())?;
            return Ok(());
        }
        if !run.state.is_terminal() {
            run.state = RunState::Failed;
            run.terminal_result = Some("failed".into());
            run.error_code = Some(journal.error_code.clone());
            run.stop_cause = Some(RunStopCause::Failed);
            run.end_seq = run.end_seq.or(run.start_seq);
            run.updated_at = Utc::now();
            let run_path = self
                .run_path(run_id)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            self.write_run_record_at_unlocked(&run_path, &run)?;
        }
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterAbortRun) {
            anyhow::bail!(
                "injected persist cut AfterAbortRun; admission abort compensation requires restart"
            );
        }
        self.clear_agent_activation_unlocked(run_id, journal.agent_id.as_deref())?;
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterAbortAgent) {
            anyhow::bail!(
                "injected persist cut AfterAbortAgent; admission abort compensation requires restart"
            );
        }
        if let Some(reservation_id) = journal.reservation_id.as_deref() {
            if let Some(mut reservation) = self.load_quota_reservation_unlocked(reservation_id)? {
                if reservation.state == QuotaReservationState::Reserved {
                    let (tokens, requests) = if page.total_count == 0 {
                        (0, 0)
                    } else {
                        (
                            run.aggregates.usage.total_tokens,
                            run.aggregates.usage.requests,
                        )
                    };
                    reservation
                        .settle(tokens, requests, Utc::now())
                        .map_err(|error| anyhow::anyhow!(error))?;
                    self.save_quota_reservation_unlocked(&reservation)?;
                }
            }
        }
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterAbortQuota) {
            anyhow::bail!(
                "injected persist cut AfterAbortQuota; admission abort compensation requires restart"
            );
        }
        Ok(())
    }

    fn clear_agent_activation_unlocked(
        &self,
        run_id: &str,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(agent_id) = agent_id else {
            return Ok(());
        };
        if let Some(mut agent) = self.load_agent_unlocked(agent_id)? {
            if agent.current_run_id.as_deref() == Some(run_id) {
                agent.current_run_id = None;
                agent.last_run_id = Some(run_id.to_string());
                agent.state = crate::orchestration::AgentState::Waiting;
                agent.updated_at = Utc::now();
                let agent_path = self
                    .agent_path(agent_id)
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                atomic_write_json(&agent_path, &agent)?;
            }
        }
        Ok(())
    }

    fn recover_admission_abort_journals(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("admission-abort");
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut recovered = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let journal: AdmissionAbortJournal = serde_json::from_str(&fs::read_to_string(&path)?)?;
            self.apply_admission_abort_unlocked(&journal)?;
            remove_file_durable(&path)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    fn save_run_and_activate_agent_inner(
        &self,
        run: &RunRecord,
        agent_id: &str,
        quota_reservation: Option<&QuotaReservation>,
        pending_agent: Option<&AgentRecord>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            run.agent_id.as_deref() == Some(agent_id),
            "Run Agent identity does not match activation target"
        );
        let _g = self.inner.lock.lock();
        match quota_reservation {
            Some(reservation) => {
                Self::validate_quota_binding(run, reservation)?;
                self.ensure_quota_capacity_unlocked(reservation)?;
            }
            None if run
                .provider_route
                .as_ref()
                .is_some_and(|route| route.quota_reservation_id.is_some()) =>
            {
                anyhow::bail!("quota-backed Run must be activated atomically with its reservation");
            }
            None => {}
        }
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
        let mut agent = if agent_path.is_file() {
            let mut agent: AgentRecord = serde_json::from_str(&fs::read_to_string(&agent_path)?)?;
            agent
                .migrate_legacy_spec()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            agent
        } else {
            let pending = pending_agent
                .ok_or_else(|| anyhow::anyhow!("persistent Agent record is missing"))?;
            anyhow::ensure!(
                pending.agent_id == agent_id,
                "pending Agent identity does not match activation target"
            );
            let mut pending = pending.clone();
            pending
                .migrate_legacy_spec()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            pending
        };
        anyhow::ensure!(
            agent.current_run_id.is_none(),
            "persistent Agent already has an active Run"
        );
        anyhow::ensure!(
            agent.known_lane_ids().contains(&run.session_id),
            "Run Lane is not currently associated with the persistent Agent"
        );
        let current_spec = agent.current_spec()?;
        anyhow::ensure!(
            run.agent_spec_revision == Some(current_spec.revision),
            "Run Agent specification revision is stale"
        );
        validate_run_provider_route_for_spec(run, current_spec)?;
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
            quota_reservation: quota_reservation.cloned(),
        };
        atomic_write_json(&intent_path, &intent)?;
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterIntent) {
            anyhow::bail!(
                "injected persist cut AfterIntent; durable recovery intent requires restart"
            );
        }
        let quota_path = quota_reservation
            .map(|reservation| self.quota_reservation_path(&reservation.reservation_id))
            .transpose()
            .map_err(|error| anyhow::anyhow!(error))?;
        if let (Some(reservation), Some(quota_path)) = (quota_reservation, quota_path.as_ref()) {
            if let Err(error) = atomic_write_json(quota_path, reservation) {
                if remove_file_durable(&intent_path).is_err() {
                    return Err(error.context(
                        "persist activation quota; durable recovery intent requires restart",
                    ));
                }
                return Err(error.context("persist activation quota"));
            }
        }
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterQuota) {
            anyhow::bail!(
                "injected persist cut AfterQuota; durable recovery intent requires restart"
            );
        }
        if let Err(error) = self.write_run_record_at_unlocked(&run_path, run) {
            let run_rollback = match fs::symlink_metadata(&run_path) {
                Ok(_) => remove_file_durable(&run_path),
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(())
                }
                Err(metadata_error) => Err(metadata_error.into()),
            };
            let quota_rollback = quota_path
                .as_ref()
                .map_or(Ok(()), |path| remove_file_durable(path));
            if run_rollback.is_err()
                || quota_rollback.is_err()
                || remove_file_durable(&intent_path).is_err()
            {
                return Err(error.context(
                    "persist Agent activation Run; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent activation Run"));
        }
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterRun) {
            anyhow::bail!(
                "injected persist cut AfterRun; durable recovery intent requires restart"
            );
        }
        if let Err(error) = self.save_agent_spec_unlocked(&agent.agent_id, agent.current_spec()?) {
            let quota_rollback = quota_path
                .as_ref()
                .map_or(Ok(()), |path| remove_file_durable(path));
            if remove_file_durable(&run_path).is_err() || quota_rollback.is_err() {
                return Err(error.context(
                    "persist Agent specification; durable recovery intent requires restart",
                ));
            }
            if remove_file_durable(&intent_path).is_err() {
                return Err(error.context(
                    "persist Agent specification; durable recovery intent requires restart",
                ));
            }
            return Err(error.context("persist Agent specification"));
        }
        if let Err(error) = atomic_write_json(&agent_path, &agent) {
            let quota_rollback = quota_path
                .as_ref()
                .map_or(Ok(()), |path| remove_file_durable(path));
            if remove_file_durable(&run_path).is_err() || quota_rollback.is_err() {
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
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterAgent) {
            anyhow::bail!(
                "injected persist cut AfterAgent; durable recovery intent requires restart"
            );
        }
        if self.hit_result_persist_cut(AdmissionPersistCut::AfterIntentRemoval) {
            anyhow::bail!(
                "injected persist cut AfterIntentRemoval; durable recovery intent requires restart"
            );
        }
        if let Err(error) = remove_file_durable(&intent_path) {
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
        let current_spec = agent.current_spec()?;
        anyhow::ensure!(
            prior_run.agent_spec_revision == Some(current_spec.revision),
            "Run Agent specification revision is stale"
        );
        validate_run_provider_route_for_spec(&prior_run, current_spec)?;
        if let Some(reservation_id) = prior_run
            .provider_route
            .as_ref()
            .and_then(|route| route.quota_reservation_id.as_deref())
        {
            let reservation = self
                .load_quota_reservation_unlocked(reservation_id)?
                .ok_or_else(|| anyhow::anyhow!("queued Run quota reservation is missing"))?;
            Self::validate_quota_binding(&prior_run, &reservation)?;
        }

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
            quota_reservation: None,
        };
        atomic_write_json(&intent_path, &intent)?;
        if let Err(error) = self.write_run_record_at_unlocked(&run_path, &run) {
            if remove_file_durable(&intent_path).is_err() {
                return Err(
                    error.context("promote queued Run; durable recovery intent requires restart")
                );
            }
            return Err(error.context("promote queued Run"));
        }
        if let Err(error) = atomic_write_json(&agent_path, &agent) {
            if self
                .write_run_record_at_unlocked(&run_path, &prior_run)
                .is_err()
                || remove_file_durable(&intent_path).is_err()
            {
                return Err(error
                    .context("activate promoted Run; durable recovery intent requires restart"));
            }
            return Err(error.context("activate promoted Run"));
        }
        if let Err(error) = remove_file_durable(&intent_path) {
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
        let provider_route = run.provider_route.clone();
        update(&mut run)?;
        anyhow::ensure!(
            run.provider_route == provider_route,
            "Run provider route snapshot is immutable"
        );
        if let Some(route) = &run.provider_route {
            route
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        self.preflight_quota_for_run_unlocked(&run)?;
        let path = self
            .run_path(run_id)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if let Err(error) = self.write_run_record_at_unlocked(&path, &run) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
            return Err(error);
        }
        if let Err(error) = self.sync_quota_for_run_unlocked(&run) {
            *self.inner.last_run_error.lock() = Some(error.to_string());
            return Err(error);
        }
        *self.inner.last_run_error.lock() = None;
        Ok(Some(run))
    }

    pub fn list_runs(&self) -> anyhow::Result<Vec<RunRecord>> {
        self.list_runs_unlocked()
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

    pub fn load_work_attempt(&self, attempt_id: &str) -> anyhow::Result<Option<WorkAttempt>> {
        let _guard = self.inner.lock.lock();
        self.load_work_attempt_unlocked(attempt_id)
    }

    /// Linearize a Computer Use boundary against Work cancellation,
    /// expiration, reassignment, and Agent-spec revision. The callback runs
    /// while the workload ledger lock is held, so a caller may use it to
    /// commit a second durable fence before any physical side effect.
    pub(crate) fn with_active_computer_work_attempt<T, E>(
        &self,
        work_id: &str,
        attempt_id: &str,
        agent: (&str, u64),
        scope: (Uuid, &str),
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<Result<T, E>, OrchError> {
        let (agent_id, agent_spec_revision) = agent;
        let (owner_session_id, workspace) = scope;
        let denied = || {
            OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "Computer Use WorkAttempt authority is no longer active",
            )
        };
        let _guard = self.inner.lock.lock();
        let work = self
            .load_work_item_unlocked(work_id)
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        let attempt = self
            .load_work_attempt_unlocked(attempt_id)
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        let agent = self
            .load_agent_unlocked(agent_id)
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        let current_spec = agent.current_spec().map_err(|_| denied())?;
        let now = Utc::now();
        if work.work_id != work_id
            || work.session_id != owner_session_id
            || work.workspace != workspace
            || work.assigned_agent_id.as_deref() != Some(agent_id)
            || !matches!(work.state, WorkState::Leased | WorkState::Running)
            || attempt.attempt_id != attempt_id
            || attempt.work_id != work_id
            || attempt.claimant_id != agent_id
            || !attempt.state.is_active()
            || !attempt.lease_active_at(now)
            || agent.agent_id != agent_id
            || !agent.state.is_active_identity()
            || agent.workspace != workspace
            || current_spec.revision != agent_spec_revision
            || !current_spec.authority.computer_use_allowed
        {
            return Err(denied());
        }
        Ok(operation())
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
        atomic_write_json(&intent_path, &intent)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        self.commit_manager_creation_intent_unlocked(&intent)?;
        remove_file_durable(&intent_path)
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
            remove_file_durable(&path)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
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
        atomic_write_json(&path, plan)
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
        if item.state.is_terminal() || item.is_container {
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
                item.blocked_reason = Some(reason.to_string());
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
        self.save_work_decision_unlocked(&decision)?;
        self.save_work_item_unlocked(&item)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
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
        let path = self.work_decision_path(&decision.work_id, &decision.decision_id)?;
        atomic_write_json(&path, decision)
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
        atomic_write_json(&path, &presence)
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
        atomic_write_json(&path, &message)
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
            atomic_write_json(&self.message_path(message_id)?, &message)
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
            let _ = remove_file_durable(&self.message_path(&message.message_id)?);
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

    /// Reserve one live native admission while enforcing both durable
    /// capacity ceilings under the same store lock as the intent write.
    ///
    /// Callers may perform an earlier eligibility check for diagnostics, but
    /// this operation is the authority: concurrent executor drives cannot
    /// both observe the last provider or Agent slot and consume it.
    pub fn reserve_managed_intent(
        &self,
        intent: &ManagedExecutionIntent,
        max_concurrent_runs_for_agent: usize,
        max_concurrent_runs_for_provider: usize,
    ) -> Result<ManagedExecutionIntent, OrchError> {
        intent.validate()?;
        if !intent.state.is_live() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "managed execution reservation must start in a live state",
            ));
        }
        let provider_id = intent
            .provider_route
            .as_ref()
            .map(|route| route.provider_id.as_str())
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "managed execution reservation requires an exact provider route",
                )
            })?;
        let _guard = self.inner.lock.lock();
        let existing = self.list_managed_intents_unlocked()?;
        if existing
            .iter()
            .any(|current| current.state.is_live() && current.work_id == intent.work_id)
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "work already has a live managed execution intent",
            ));
        }
        let live_for_agent = existing
            .iter()
            .filter(|current| current.state.is_live() && current.agent_id == intent.agent_id)
            .count();
        if live_for_agent >= max_concurrent_runs_for_agent {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                AGENT_CEILING_EXHAUSTED,
            ));
        }
        let live_for_provider = existing
            .iter()
            .filter(|current| {
                current.state.is_live()
                    && current
                        .effective_provider_id()
                        .as_deref()
                        .is_none_or(|current_provider| current_provider == provider_id)
            })
            .count();
        if live_for_provider >= max_concurrent_runs_for_provider {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                PROVIDER_CEILING_EXHAUSTED,
            ));
        }
        let path = self.managed_intent_path(&intent.intent_id)?;
        if path.is_file() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "managed execution intent ID already exists",
            ));
        }
        self.save_managed_intent_unlocked(intent)?;
        Ok(intent.clone())
    }

    fn save_managed_intent_unlocked(
        &self,
        intent: &ManagedExecutionIntent,
    ) -> Result<(), OrchError> {
        let path = self.managed_intent_path(&intent.intent_id)?;
        atomic_write_json(&path, intent)
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

    /// Live native admissions that already route to `provider_id`.
    ///
    /// Counted from durable intents so duplicate supervisor ticks and process
    /// restarts re-derive the same provider capacity answer.
    pub fn live_managed_intents_for_provider(&self, provider_id: &str) -> Result<usize, OrchError> {
        Ok(self
            .list_managed_intents()?
            .into_iter()
            .filter(|intent| {
                intent.state.is_live()
                    && intent
                        .effective_provider_id()
                        .as_deref()
                        .is_none_or(|current_provider| current_provider == provider_id)
            })
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
        // Provider dispatch is a separate durable boundary from managed-work
        // retry policy. A completed or uncertain send may already have been
        // accepted by the gateway, so never let the generic managed retry
        // flag silently replay it. An admitted row is also unsafe: recovery
        // has not yet established that request bytes were not accepted.
        let provider_retry_safe = intent
            .run_id
            .as_deref()
            .map(|run_id| self.provider_retry_safe_for_run_unlocked(run_id))
            .transpose()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .unwrap_or(false);
        let retry = provider_retry_safe
            && policy.allows_auto_retry(&item, attempt_number.saturating_add(1), cause);
        let attempt_state = match cause {
            ManagedRetryCause::Expired | ManagedRetryCause::Interrupted => AttemptState::Expired,
            ManagedRetryCause::Failed => AttemptState::Failed,
        };
        let (outcome, work_state, result) = if item.state == WorkState::Cancelled {
            (
                ManagedFinalizationOutcome::Cancelled,
                WorkState::Cancelled,
                item.result.clone(),
            )
        } else if item.state == WorkState::Succeeded || item.state == WorkState::AwaitingApproval {
            (
                if item.state == WorkState::AwaitingApproval {
                    ManagedFinalizationOutcome::AwaitingApproval
                } else {
                    ManagedFinalizationOutcome::Completed
                },
                item.state,
                item.result.clone(),
            )
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
                    (AttemptState::Succeeded, WorkState::Succeeded, result)
                }
                ManagedFinalizationOutcome::AwaitingApproval => (
                    AttemptState::AwaitingApproval,
                    WorkState::AwaitingApproval,
                    result,
                ),
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
        atomic_write_json(&path, record)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        if stage == ManagedFinalizationStage::AfterJournal {
            return Ok(());
        }
        if let Some(attempt_id) = record.attempt_id.as_deref() {
            if let Ok(Some(mut attempt)) = self.load_work_attempt_unlocked(attempt_id) {
                if attempt.state.is_active() {
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
                || item.state == WorkState::AwaitingApproval;
            if !preserve_terminal {
                item.state = record.work_state;
                if record.work_state == WorkState::Queued {
                    item.blocked_reason = None;
                    item.result = None;
                } else if record.result.is_some() {
                    item.result = record.result.clone();
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
        let _ = fs::remove_file(path);
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

    fn provider_retry_safe_for_run_unlocked(&self, run_id: &str) -> anyhow::Result<bool> {
        let attempts = self.list_provider_attempts_for_run_unlocked(run_id)?;
        if attempts.truncated {
            return Ok(false);
        }
        Ok(attempts.attempts.iter().all(|attempt| {
            attempt.retry_class == Some(super::provider_attempt::ProviderRetryClass::SameRunSafe)
        }))
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

    /// Verify that a lease attempt belongs to a bound worker credential before
    /// accepting any lease-scoped mutation. The attempt token remains the
    /// second factor; this check prevents a worker bearer from reusing a
    /// different worker's token if it is ever exposed to that process.
    pub fn require_work_attempt_claimant(
        &self,
        work_id: &str,
        attempt_id: &str,
        claimant_id: &str,
    ) -> Result<(), OrchError> {
        let _guard = self.inner.lock.lock();
        let attempt = self
            .load_work_attempt_unlocked(attempt_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "work attempt is not owned by this credential",
                )
            })?;
        if attempt.work_id != work_id || attempt.claimant_id != claimant_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "work attempt is not owned by this credential",
            ));
        }
        Ok(())
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
        if item.is_container {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "coordination container Work is not executable",
            ));
        }
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
        item.assignment_status
            .is_claimable_by(item.assigned_agent_id.as_deref(), claimant_id)?;
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
        let provider_attempts = self.list_provider_attempts_unlocked()?;

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
                Ok(()) => {
                    report.run_files_removed += 1;
                    let _ = self.remove_session_run_index_unlocked(run);
                    for attempt in provider_attempts
                        .iter()
                        .filter(|attempt| attempt.run_id == run.run_id)
                    {
                        let attempt_path = self
                            .provider_attempt_path(&attempt.attempt_id)
                            .map_err(|error| anyhow::anyhow!(error))?;
                        match fs::remove_file(attempt_path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => report.skipped_files += 1,
                        }
                    }
                }
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
        if let Some(route) = &candidate.provider_route {
            route
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        let _guard = self.inner.lock.lock();
        let activation_path = self
            .agent_activation_path(&candidate.run_id)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if activation_path.is_file() {
            // A crash or cleanup failure may leave a fully applied activation
            // intent. Remove that Running snapshot before a terminal record
            // is installed so restart recovery can never resurrect the Run.
            remove_file_durable(&activation_path)
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
                    anyhow::ensure!(
                        current.provider_route == candidate.provider_route,
                        "Run provider route snapshot is immutable"
                    );
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
        self.preflight_quota_for_run_unlocked(&final_run)?;
        let result = (|| -> anyhow::Result<()> {
            atomic_write_json(&intent_path, &final_run)?;
            if let Some(corrupt) = &corrupt_target {
                fs::rename(&run_path, corrupt)?;
            }
            self.write_run_record_at_unlocked(&run_path, &final_run)?;
            fs::remove_file(&intent_path)?;
            Ok(())
        })();
        if result.is_ok() {
            if let Err(error) = self.sync_quota_for_run_unlocked(&final_run) {
                *self.inner.last_run_error.lock() = Some(error.to_string());
                return Err(error);
            }
        }
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

    fn recover_quota_admission_intents(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let dir = self.inner.root.join("quota-admission-intents");
        let mut recovered = 0;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: QuotaAdmissionIntent = serde_json::from_str(&fs::read_to_string(&path)?)?;
            Self::validate_quota_binding(&intent.run, &intent.reservation)?;
            let run_path = self
                .run_path(&intent.run.run_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            if run_path.is_file() {
                let existing: RunRecord = serde_json::from_str(&fs::read_to_string(&run_path)?)?;
                anyhow::ensure!(
                    serde_json::to_value(existing)? == serde_json::to_value(&intent.run)?,
                    "quota admission recovery Run conflicts with durable state"
                );
            }
            let reservation_path = self
                .quota_reservation_path(&intent.reservation.reservation_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            if reservation_path.is_file() {
                let existing: QuotaReservation =
                    serde_json::from_str(&fs::read_to_string(&reservation_path)?)?;
                anyhow::ensure!(
                    existing == intent.reservation,
                    "quota admission recovery reservation conflicts with durable state"
                );
            } else {
                self.ensure_quota_capacity_unlocked(&intent.reservation)?;
            }
            atomic_write_json(&reservation_path, &intent.reservation)?;
            self.write_run_record_at_unlocked(&run_path, &intent.run)?;
            remove_file_durable(&path)?;
            recovered += 1;
        }
        Ok(recovered)
    }

    fn reconcile_provider_attempt_ledger(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        let mut reconciled = 0;
        for mut attempt in self.list_provider_attempts_unlocked()? {
            if attempt.state == ProviderAttemptState::Admitted {
                // The row was durable before transport entry. After process
                // death there is no proof that request bytes were not
                // accepted, so recovery must never classify it retry-safe.
                attempt
                    .finish(
                        ProviderSendCertainty::UncertainAccept,
                        None,
                        None,
                        Utc::now(),
                    )
                    .map_err(|error| anyhow::anyhow!(error))?;
                self.save_provider_attempt_unlocked(&attempt)?;
                reconciled += 1;
            }
            let Some(mut run) = self.load_run_unlocked(&attempt.run_id)? else {
                let reservation = self
                    .load_quota_reservation_unlocked(&attempt.reservation_id)?
                    .ok_or_else(|| anyhow::anyhow!("provider attempt reservation is missing"))?;
                anyhow::ensure!(
                    attempt.state == ProviderAttemptState::Finished
                        && reservation.state != QuotaReservationState::Reserved,
                    "live or uncertain provider attempt Run is missing"
                );
                let path = self
                    .provider_attempt_path(&attempt.attempt_id)
                    .map_err(|error| anyhow::anyhow!(error))?;
                remove_file_durable(&path)?;
                reconciled += 1;
                continue;
            };
            if self.apply_provider_attempt_to_run(&mut run, &attempt)? {
                self.preflight_quota_for_run_unlocked(&run)?;
                let path = self
                    .run_path(&run.run_id)
                    .map_err(|error| anyhow::anyhow!(error))?;
                self.write_run_record_at_unlocked(&path, &run)?;
                self.sync_quota_for_run_unlocked(&run)?;
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }

    fn reconcile_quota_ledger(&self) -> anyhow::Result<usize> {
        let _guard = self.inner.lock.lock();
        for entry in fs::read_dir(self.inner.root.join("runs"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let run: RunRecord = serde_json::from_str(&fs::read_to_string(path)?)?;
            if let Some(route) = &run.provider_route {
                route.validate().map_err(|error| anyhow::anyhow!(error))?;
                if route.quota_reservation_id.is_some() {
                    self.sync_quota_for_run_unlocked(&run)?;
                }
            }
        }

        let mut expired = 0;
        for mut reservation in self.list_quota_reservations_unlocked()? {
            if reservation.state != QuotaReservationState::Reserved {
                continue;
            }
            let run_path = self
                .run_path(&reservation.run_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            let quota_intent_path = self
                .quota_admission_intent_path(&reservation.run_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            let activation_intent_path = self
                .agent_activation_path(&reservation.run_id)
                .map_err(|error| anyhow::anyhow!(error))?;
            if run_path.is_file() || quota_intent_path.is_file() || activation_intent_path.is_file()
            {
                continue;
            }
            reservation.state = QuotaReservationState::Expired;
            reservation.tokens_consumed = 0;
            reservation.requests_consumed = 0;
            reservation.updated_at = Utc::now();
            self.save_quota_reservation_unlocked(&reservation)?;
            expired += 1;
        }
        Ok(expired)
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
            let spec = intent.activated_agent.current_spec()?;
            // A route-less activation journal from before AgentSpec fencing
            // must still be installed so startup can mark it Interrupted. It
            // is never resumed. Any newer record that names either fence must
            // match the exact captured specification before recovery writes.
            if intent.run.provider_route.is_some() || intent.run.agent_spec_revision.is_some() {
                anyhow::ensure!(
                    intent.run.agent_spec_revision == Some(spec.revision),
                    "Agent activation recovery Run specification is stale"
                );
            }
            validate_run_provider_route_for_spec(&intent.run, spec)?;
            match (
                intent
                    .run
                    .provider_route
                    .as_ref()
                    .and_then(|route| route.quota_reservation_id.as_deref()),
                intent.quota_reservation.as_ref(),
            ) {
                (Some(reservation_id), Some(reservation)) => {
                    anyhow::ensure!(
                        reservation_id == reservation.reservation_id,
                        "Agent activation recovery quota identity is inconsistent"
                    );
                    Self::validate_quota_binding(&intent.run, reservation)?;
                    let reservation_path = self
                        .quota_reservation_path(reservation_id)
                        .map_err(|error| anyhow::anyhow!(error))?;
                    if reservation_path.is_file() {
                        let existing: QuotaReservation =
                            serde_json::from_str(&fs::read_to_string(&reservation_path)?)?;
                        anyhow::ensure!(
                            existing == *reservation,
                            "Agent activation recovery quota conflicts with durable state"
                        );
                    } else {
                        self.ensure_quota_capacity_unlocked(reservation)?;
                        atomic_write_json(&reservation_path, reservation)?;
                    }
                }
                (Some(reservation_id), None) => {
                    let reservation = self
                        .load_quota_reservation_unlocked(reservation_id)?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Agent activation recovery quota reservation is missing"
                            )
                        })?;
                    Self::validate_quota_binding(&intent.run, &reservation)?;
                }
                (None, Some(_)) => {
                    anyhow::bail!("Agent activation recovery has an unlinked quota reservation");
                }
                (None, None) => {}
            }
            if let Some(prior) = &intent.prior_run {
                anyhow::ensure!(
                    prior.provider_route == intent.run.provider_route,
                    "Agent activation recovery cannot replace the Run provider route"
                );
                if let Some(route) = &prior.provider_route {
                    route
                        .validate()
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                }
            }
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
            self.write_run_record_at_unlocked(&run_path, &intent.run)?;
            self.save_agent_spec_unlocked(
                &intent.activated_agent.agent_id,
                intent.activated_agent.current_spec()?,
            )?;
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
    for attempt_id in &current.aggregates.accounted_provider_attempt_ids {
        if !target
            .aggregates
            .accounted_provider_attempt_ids
            .contains(attempt_id)
        {
            target
                .aggregates
                .accounted_provider_attempt_ids
                .push(attempt_id.clone());
        }
    }
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
    use crate::orchestration::types::RunPurpose;
    use crate::orchestration::types::{
        AgentRecord, ContinuationCheckpoint, ContinuationReason, RunBounds,
    };
    use chrono::TimeZone;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn provider_route_snapshot(model_id: &str) -> super::super::types::ProviderRouteSnapshot {
        super::super::types::ProviderRouteSnapshot {
            schema_version: super::super::types::PROVIDER_ROUTE_SNAPSHOT_SCHEMA_VERSION,
            provider_id: "xai".into(),
            model_id: model_id.into(),
            wire_model_id: model_id.into(),
            selection_key: model_id.into(),
            kind: crate::gateway_config::ProviderKind::Xai,
            dialect: crate::gateway_config::ProviderDialect::XaiChatCompletions,
            base_url: "https://api.x.ai/v1".into(),
            endpoint_fingerprint: "endpoint-fingerprint".into(),
            credential_ref: "managed:xai:api-key".into(),
            credential_fingerprint: "credential-fingerprint".into(),
            capabilities: crate::gateway_config::ModelCapabilities::default(),
            deadline_class: crate::gateway_config::ProviderDeadlineClass::Standard,
            effort: crate::types::EffortLevel::Medium,
            qualification_record_id: None,
            quota_class: None,
            quota_reservation_id: None,
            snapshot_hash: String::new(),
        }
        .seal()
        .unwrap()
    }

    fn quota_backed_run(
        run_id: &str,
        state: RunState,
        now: chrono::DateTime<Utc>,
        limits: super::super::quota::QuotaLimits,
    ) -> (RunRecord, super::super::quota::QuotaReservation) {
        let mut run = terminal_run(run_id);
        run.state = state;
        run.created_at = now;
        run.updated_at = now;
        run.terminal_result = state.is_terminal().then(|| "terminal".into());
        run.final_response = None;
        run.end_seq = None;
        run.bounds.max_rounds = 8;
        run.bounds.max_total_tokens = Some(1_000);
        run.provider_route = Some(
            provider_route_snapshot("grok-code-1")
                .bind_quota(
                    super::super::quota::QuotaClass::CodingExecution,
                    format!("quota-{run_id}"),
                )
                .unwrap(),
        );
        let reservation =
            super::super::quota::QuotaReservation::for_run(&run, "owner-1", limits, now).unwrap();
        (run, reservation)
    }

    fn terminal_run(run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/w".into(),
            request_id: format!("req-{run_id}"),
            client_id: None,
            state: RunState::Completed,
            purpose: Default::default(),
            provider_route: None,
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
            purpose: Default::default(),
            provider_route: None,
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
    fn persisted_run_provider_route_is_immutable() {
        let directory = tempdir().unwrap();
        let store = OrchStore::open(directory.path()).unwrap();
        let mut run = terminal_run("immutable-route");
        run.provider_route = Some(provider_route_snapshot("grok-4"));
        store.save_run(&run).unwrap();

        let error = store
            .update_run(&run.run_id, |current| {
                current.provider_route = None;
                Ok(())
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("provider route snapshot is immutable"));
        assert_eq!(
            store.load_run(&run.run_id).unwrap().unwrap().provider_route,
            run.provider_route
        );

        let mut replacement = run.clone();
        replacement.provider_route = Some(provider_route_snapshot("grok-4-fast"));
        let error = store.save_run(&replacement).unwrap_err();
        assert!(error
            .to_string()
            .contains("provider route snapshot is immutable"));

        let error = store.persist_finalization(&replacement).unwrap_err();
        assert!(error
            .to_string()
            .contains("provider route snapshot is immutable"));
        assert_eq!(
            store.load_run(&run.run_id).unwrap().unwrap().provider_route,
            run.provider_route
        );
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
            provider_route: None,
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
            provider_route: None,
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
            principal_id: None,
            credential_id: None,
            authority_document_hash: None,
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
                principal_id: None,
                credential_id: None,
                authority_document_hash: None,
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
        first.provider_route = Some(provider_route_snapshot("wrong-model"));
        let error = store
            .admit_run_and_activate_agent(&first, "agent-activation-race", None)
            .into_result()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not match its captured Agent specification"),
            "unexpected error: {error}"
        );
        assert!(store.load_run(&first.run_id).unwrap().is_none());
        first.provider_route = Some(provider_route_snapshot("grok"));
        let mut second = first.clone();
        second.run_id = "activation-second".into();
        second.request_id = "req-activation-second".into();
        second.session_id = second_lane;

        store
            .admit_run_and_activate_agent(&first, "agent-activation-race", None)
            .into_result()
            .unwrap();
        let error = store
            .admit_run_and_activate_agent(&second, "agent-activation-race", None)
            .into_result()
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
                quota_reservation: None,
            };
            atomic_write_json(&store.agent_activation_path(run_id).unwrap(), &intent).unwrap();
            atomic_write_json(&store.run_path(run_id).unwrap(), &prior_run).unwrap();
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
    fn quota_admission_is_atomic_with_run() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let limits = super::super::quota::QuotaLimits {
            max_in_flight_reservations: 1,
            max_tokens_per_window: 2_000,
            max_requests_per_window: 16,
            ..Default::default()
        };
        let (first_run, first_reservation) =
            quota_backed_run("quota-run-1", RunState::Queued, now, limits);
        store
            .admit_run_with_quota(&first_run, &first_reservation)
            .into_result()
            .unwrap();
        assert_eq!(
            store.load_run(&first_run.run_id).unwrap().unwrap().run_id,
            first_run.run_id
        );
        assert_eq!(
            store
                .load_quota_reservation(&first_reservation.reservation_id)
                .unwrap()
                .unwrap(),
            first_reservation
        );

        let (second_run, second_reservation) =
            quota_backed_run("quota-run-2", RunState::Queued, now, limits);
        let error = store
            .admit_run_with_quota(&second_run, &second_reservation)
            .into_result()
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<OrchError>().map(|error| &error.code),
            Some(&OrchErrorCode::CapacityExhausted)
        );
        assert!(store.load_run(&second_run.run_id).unwrap().is_none());
        assert!(store
            .load_quota_reservation(&second_reservation.reservation_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn concurrent_last_quota_slot_has_one_winner() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let limits = super::super::quota::QuotaLimits {
            max_in_flight_reservations: 1,
            max_tokens_per_window: 2_000,
            max_requests_per_window: 16,
            ..Default::default()
        };
        let first = quota_backed_run("quota-race-1", RunState::Queued, now, limits);
        let second = quota_backed_run("quota-race-2", RunState::Queued, now, limits);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = [first, second].map(|(run, reservation)| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.admit_run_with_quota(&run, &reservation).into_result()
            })
        });
        barrier.wait();
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .filter(|error| {
                    error
                        .downcast_ref::<OrchError>()
                        .is_some_and(|error| error.code == OrchErrorCode::CapacityExhausted)
                })
                .count(),
            1
        );
        assert_eq!(store.list_runs().unwrap().len(), 1);
        assert_eq!(store.list_quota_reservations().unwrap().len(), 1);
    }

    #[test]
    fn quota_intent_recovers_without_orphan_reservation() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, reservation) = quota_backed_run(
            "quota-crash-run",
            RunState::Queued,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        {
            let store = OrchStore::open(root.path()).unwrap();
            let intent = QuotaAdmissionIntent {
                run: run.clone(),
                reservation: reservation.clone(),
            };
            atomic_write_json(
                &store.quota_admission_intent_path(&run.run_id).unwrap(),
                &intent,
            )
            .unwrap();
            atomic_write_json(
                &store
                    .quota_reservation_path(&reservation.reservation_id)
                    .unwrap(),
                &reservation,
            )
            .unwrap();
            // Crash after the reservation but before the Run write.
        }

        let reopened = OrchStore::open(root.path()).unwrap();
        let recovered_run = reopened.load_run(&run.run_id).unwrap().unwrap();
        let recovered_reservation = reopened
            .load_quota_reservation(&reservation.reservation_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_run.state, RunState::Interrupted);
        assert_eq!(
            recovered_reservation.state,
            super::super::quota::QuotaReservationState::Refunded
        );
        assert!(!reopened
            .quota_admission_intent_path(&run.run_id)
            .unwrap()
            .exists());
        assert_eq!(reopened.list_runs().unwrap().len(), 1);
        assert_eq!(reopened.list_quota_reservations().unwrap().len(), 1);
    }

    #[test]
    fn terminal_usage_settles_quota_idempotently_and_uncertain_usage_stays_reserved() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let limits = super::super::quota::QuotaLimits::default();
        let (run, reservation) = quota_backed_run("quota-consume", RunState::Queued, now, limits);
        store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap();
        for _ in 0..2 {
            store
                .update_run(&run.run_id, |current| {
                    current.state = RunState::Completed;
                    current.aggregates.usage.total_tokens = 400;
                    current.aggregates.usage.requests = 3;
                    current.aggregates.usage_complete = true;
                    current.aggregates.usage_pending_requests = 0;
                    current.updated_at = now;
                    Ok(())
                })
                .unwrap();
        }
        let consumed = store
            .load_quota_reservation(&reservation.reservation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            consumed.state,
            super::super::quota::QuotaReservationState::Consumed
        );
        assert_eq!(consumed.tokens_consumed, 400);
        assert_eq!(consumed.requests_consumed, 3);
        assert!(store
            .update_run(&run.run_id, |current| {
                current.aggregates.usage.requests = reservation.requests_reserved + 1;
                Ok(())
            })
            .is_err());
        assert_eq!(
            store
                .load_run(&run.run_id)
                .unwrap()
                .unwrap()
                .aggregates
                .usage
                .requests,
            3
        );

        let (uncertain_run, uncertain_reservation) =
            quota_backed_run("quota-uncertain", RunState::Queued, now, limits);
        store
            .admit_run_with_quota(&uncertain_run, &uncertain_reservation)
            .into_result()
            .unwrap();
        store
            .update_run(&uncertain_run.run_id, |current| {
                current.state = RunState::Interrupted;
                current.aggregates.usage_complete = false;
                current.aggregates.usage_pending_requests = 0;
                current.updated_at = now;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store
                .load_quota_reservation(&uncertain_reservation.reservation_id)
                .unwrap()
                .unwrap()
                .state,
            super::super::quota::QuotaReservationState::Reserved
        );
    }

    #[test]
    fn restart_expires_reservation_without_run_or_recovery_intent() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (_run, reservation) = quota_backed_run(
            "quota-orphan",
            RunState::Queued,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        {
            let store = OrchStore::open(root.path()).unwrap();
            atomic_write_json(
                &store
                    .quota_reservation_path(&reservation.reservation_id)
                    .unwrap(),
                &reservation,
            )
            .unwrap();
        }
        let reopened = OrchStore::open(root.path()).unwrap();
        assert_eq!(
            reopened
                .load_quota_reservation(&reservation.reservation_id)
                .unwrap()
                .unwrap()
                .state,
            super::super::quota::QuotaReservationState::Expired
        );
    }

    #[test]
    fn restart_settles_terminal_run_written_before_quota_update() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (mut run, reservation) = quota_backed_run(
            "quota-terminal-crash",
            RunState::Queued,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        {
            let store = OrchStore::open(root.path()).unwrap();
            store
                .admit_run_with_quota(&run, &reservation)
                .into_result()
                .unwrap();
            run.state = RunState::Completed;
            run.aggregates.usage.total_tokens = 333;
            run.aggregates.usage.requests = 2;
            run.aggregates.usage_complete = true;
            run.aggregates.usage_pending_requests = 0;
            atomic_write_json(&store.run_path(&run.run_id).unwrap(), &run).unwrap();
            // Crash after the terminal Run write but before quota settlement.
        }
        let reopened = OrchStore::open(root.path()).unwrap();
        let settled = reopened
            .load_quota_reservation(&reservation.reservation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            settled.state,
            super::super::quota::QuotaReservationState::Consumed
        );
        assert_eq!(settled.tokens_consumed, 333);
        assert_eq!(settled.requests_consumed, 2);
    }

    #[test]
    fn known_not_sent_is_retryable_and_completion_is_idempotent() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, reservation) = quota_backed_run(
            "attempt-not-sent",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap();

        let first = store.begin_provider_attempt(&run.run_id).unwrap();
        assert_eq!(first.ordinal, 1);
        assert_eq!(
            store
                .load_run(&run.run_id)
                .unwrap()
                .unwrap()
                .aggregates
                .usage_pending_requests,
            1
        );
        for _ in 0..2 {
            store
                .finish_provider_attempt(
                    &first.attempt_id,
                    ProviderSendCertainty::KnownNotSent,
                    None,
                    None,
                )
                .unwrap();
        }
        let finished = store
            .load_provider_attempt(&first.attempt_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            finished.retry_class,
            Some(super::super::provider_attempt::ProviderRetryClass::SameRunSafe)
        );
        assert!(store
            .provider_retry_safe_for_run_unlocked(&run.run_id)
            .unwrap());
        let after = store.load_run(&run.run_id).unwrap().unwrap();
        assert_eq!(after.aggregates.usage_pending_requests, 0);
        assert!(after.aggregates.usage_complete);
        assert_eq!(after.aggregates.accounted_provider_attempt_ids.len(), 1);

        let replacement = store.begin_provider_attempt(&run.run_id).unwrap();
        assert_eq!(replacement.ordinal, 2);
    }

    #[test]
    fn restart_marks_unresolved_attempt_uncertain_and_never_refunds_it() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, reservation) = quota_backed_run(
            "attempt-uncertain",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let attempt_id = {
            let store = OrchStore::open(root.path()).unwrap();
            store
                .admit_run_with_quota(&run, &reservation)
                .into_result()
                .unwrap();
            let attempt = store.begin_provider_attempt(&run.run_id).unwrap();
            assert!(!store
                .provider_retry_safe_for_run_unlocked(&run.run_id)
                .unwrap());
            attempt.attempt_id
            // Crash after the durable row and possible transport entry.
        };

        let reopened = OrchStore::open(root.path()).unwrap();
        let attempt = reopened
            .load_provider_attempt(&attempt_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.send_certainty,
            Some(ProviderSendCertainty::UncertainAccept)
        );
        assert_eq!(
            attempt.retry_class,
            Some(super::super::provider_attempt::ProviderRetryClass::ExplicitNewRunOnly)
        );
        assert!(!reopened
            .provider_retry_safe_for_run_unlocked(&run.run_id)
            .unwrap());
        let recovered_run = reopened.load_run(&run.run_id).unwrap().unwrap();
        assert_eq!(recovered_run.state, RunState::Interrupted);
        assert!(!recovered_run.aggregates.usage_complete);
        assert_eq!(recovered_run.aggregates.usage_pending_requests, 0);
        assert!(reopened.begin_provider_attempt(&run.run_id).is_err());
        assert_eq!(
            reopened
                .load_quota_reservation(&reservation.reservation_id)
                .unwrap()
                .unwrap()
                .state,
            QuotaReservationState::Reserved
        );
    }

    #[test]
    fn restart_applies_a_completed_attempt_exactly_once() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, reservation) = quota_backed_run(
            "attempt-complete-crash",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let attempt_id = {
            let store = OrchStore::open(root.path()).unwrap();
            store
                .admit_run_with_quota(&run, &reservation)
                .into_result()
                .unwrap();
            let mut attempt = store.begin_provider_attempt(&run.run_id).unwrap();
            attempt
                .finish(
                    ProviderSendCertainty::KnownAccepted,
                    Some(200),
                    Some(crate::completion::CompletionUsage {
                        prompt_tokens: 7,
                        completion_tokens: 5,
                        total_tokens: 12,
                        requests: 1,
                    }),
                    Utc::now(),
                )
                .unwrap();
            store.save_provider_attempt_unlocked(&attempt).unwrap();
            attempt.attempt_id
            // Crash after the response row but before applying it to Run.
        };

        for _ in 0..2 {
            let reopened = OrchStore::open(root.path()).unwrap();
            let recovered = reopened.load_run(&run.run_id).unwrap().unwrap();
            assert_eq!(recovered.aggregates.usage.total_tokens, 12);
            assert_eq!(recovered.aggregates.usage.requests, 1);
            assert_eq!(
                recovered.aggregates.accounted_provider_attempt_ids,
                vec![attempt_id.clone()]
            );
            assert_eq!(
                reopened
                    .load_quota_reservation(&reservation.reservation_id)
                    .unwrap()
                    .unwrap()
                    .state,
                QuotaReservationState::Consumed
            );
            drop(reopened);
        }
    }

    fn admission_kind(outcome: DurableAdmission) -> &'static str {
        match outcome {
            DurableAdmission::Committed => "committed",
            DurableAdmission::DefinitelyNotCommitted(_) => "not_committed",
            DurableAdmission::Uncertain(_) => "uncertain",
        }
    }

    fn waiting_agent(session_id: Uuid, agent_id: &str) -> AgentRecord {
        let now = Utc::now();
        let mut agent = AgentRecord {
            agent_id: agent_id.into(),
            owner_principal_id: None,
            session_id,
            lane_ids: vec![session_id],
            lane_associations: Vec::new(),
            workspace: "/tmp/w".into(),
            model: "grok-code-1".into(),
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
        agent.migrate_legacy_spec().unwrap();
        agent
    }

    fn assert_two_reopens_agree(root: &std::path::Path, run_id: &str, reservation_id: &str) {
        let mut previous: Option<(RunState, QuotaReservationState, Option<String>)> = None;
        for _ in 0..2 {
            let store = OrchStore::open(root).unwrap();
            let run = store.load_run(run_id).unwrap().expect("recovered run");
            let reservation = store
                .load_quota_reservation(reservation_id)
                .unwrap()
                .expect("recovered reservation");
            let snapshot = (
                run.state,
                reservation.state,
                run.agent_id
                    .as_ref()
                    .and_then(|agent_id| store.load_agent(agent_id).unwrap())
                    .and_then(|agent| agent.current_run_id),
            );
            if let Some(previous) = previous.as_ref() {
                assert_eq!(snapshot, *previous);
            }
            previous = Some(snapshot);
            drop(store);
        }
    }

    #[test]
    fn persist_cuts_are_uncertain_and_two_reopens_converge() {
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        for cut in [
            AdmissionPersistCut::AfterIntent,
            AdmissionPersistCut::AfterQuota,
            AdmissionPersistCut::AfterRun,
            AdmissionPersistCut::AfterIntentRemoval,
        ] {
            let root = tempdir().unwrap();
            let (run, reservation) = quota_backed_run(
                &format!("cut-{cut:?}"),
                RunState::Running,
                now,
                super::super::quota::QuotaLimits::default(),
            );
            {
                let store = OrchStore::open(root.path()).unwrap();
                store.set_persist_cut(Some(cut));
                assert_eq!(
                    admission_kind(store.admit_run_with_quota(&run, &reservation)),
                    "uncertain"
                );
            }
            assert_two_reopens_agree(root.path(), &run.run_id, &reservation.reservation_id);
        }
    }

    #[test]
    fn activate_persist_cuts_are_uncertain_and_two_reopens_converge() {
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        for cut in [
            AdmissionPersistCut::AfterIntent,
            AdmissionPersistCut::AfterQuota,
            AdmissionPersistCut::AfterRun,
            AdmissionPersistCut::AfterAgent,
            AdmissionPersistCut::AfterIntentRemoval,
        ] {
            let root = tempdir().unwrap();
            let (mut run, reservation) = quota_backed_run(
                &format!("activate-cut-{cut:?}"),
                RunState::Running,
                now,
                super::super::quota::QuotaLimits::default(),
            );
            let agent = waiting_agent(run.session_id, "agent-activate-cut");
            run.agent_id = Some(agent.agent_id.clone());
            run.agent_spec_revision = Some(agent.current_spec().unwrap().revision);
            {
                let store = OrchStore::open(root.path()).unwrap();
                store.save_agent(&agent).unwrap();
                store.set_persist_cut(Some(cut));
                assert_eq!(
                    admission_kind(store.admit_run_and_activate_agent(
                        &run,
                        &agent.agent_id,
                        Some(&reservation)
                    )),
                    "uncertain"
                );
            }
            assert_two_reopens_agree(root.path(), &run.run_id, &reservation.reservation_id);
        }
    }

    #[test]
    fn first_use_candidate_is_persisted_only_with_committed_activation() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (mut run, reservation) = quota_backed_run(
            "first-use-run",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let agent = waiting_agent(run.session_id, "agent-first-use");
        run.agent_id = Some(agent.agent_id.clone());
        run.agent_spec_revision = Some(agent.current_spec().unwrap().revision);
        let store = OrchStore::open(root.path()).unwrap();
        assert!(store.load_agent(&agent.agent_id).unwrap().is_none());
        store.set_persist_cut(Some(AdmissionPersistCut::AfterIntent));
        assert_eq!(
            admission_kind(store.admit_run_and_activate_agent_with_candidate(
                &run,
                &agent.agent_id,
                Some(&reservation),
                Some(&agent),
            )),
            "uncertain"
        );
        assert!(store.load_agent(&agent.agent_id).unwrap().is_none());
        drop(store);
        let reopened = OrchStore::open(root.path()).unwrap();
        assert!(reopened.load_agent(&agent.agent_id).unwrap().is_some());
        assert!(reopened.load_run(&run.run_id).unwrap().is_some());
    }

    #[test]
    fn abort_journal_cuts_converge_after_two_reopens() {
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        for cut in [
            AdmissionPersistCut::AfterAbortJournal,
            AdmissionPersistCut::AfterAbortRun,
            AdmissionPersistCut::AfterAbortAgent,
            AdmissionPersistCut::AfterAbortQuota,
            AdmissionPersistCut::AfterAbortJournalRemoval,
        ] {
            let root = tempdir().unwrap();
            let (mut run, reservation) = quota_backed_run(
                &format!("abort-{cut:?}"),
                RunState::Running,
                now,
                super::super::quota::QuotaLimits::default(),
            );
            let agent = waiting_agent(run.session_id, "agent-abort-cut");
            run.agent_id = Some(agent.agent_id.clone());
            run.agent_spec_revision = Some(agent.current_spec().unwrap().revision);
            {
                let store = OrchStore::open(root.path()).unwrap();
                store.save_agent(&agent).unwrap();
                store
                    .admit_run_and_activate_agent(&run, &agent.agent_id, Some(&reservation))
                    .into_result()
                    .unwrap();
                store.set_persist_cut(Some(cut));
                assert_eq!(
                    admission_kind(store.terminalize_unstarted_admission(
                        &run.run_id,
                        "admission_aborted",
                        "injected abort cut",
                    )),
                    "uncertain"
                );
            }
            for _ in 0..2 {
                let store = OrchStore::open(root.path()).unwrap();
                let recovered = store.load_run(&run.run_id).unwrap().unwrap();
                assert!(recovered.state.is_terminal());
                assert_eq!(recovered.error_code.as_deref(), Some("admission_aborted"));
                let agent = store.load_agent(&agent.agent_id).unwrap().unwrap();
                assert_eq!(agent.current_run_id, None);
                let reservation = store
                    .load_quota_reservation(&reservation.reservation_id)
                    .unwrap()
                    .unwrap();
                assert_ne!(reservation.state, QuotaReservationState::Reserved);
                drop(store);
            }
        }
    }

    #[test]
    fn host_wide_last_slot_admits_exactly_one_owner_including_after_restart() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let limits = super::super::quota::QuotaLimits {
            max_in_flight_reservations: 1,
            ..super::super::quota::QuotaLimits::default()
        };
        let (desktop_run, desktop_reservation) = {
            let (run, _) = quota_backed_run("desktop-last-slot", RunState::Running, now, limits);
            let reservation =
                super::super::quota::QuotaReservation::for_run(&run, "primary", limits, now)
                    .unwrap();
            (run, reservation)
        };
        let (native_run, native_reservation) = {
            let (run, _) = quota_backed_run("native-last-slot", RunState::Running, now, limits);
            let reservation =
                super::super::quota::QuotaReservation::for_run(&run, "native-owner", limits, now)
                    .unwrap();
            (run, reservation)
        };
        let store = std::sync::Arc::new(OrchStore::open(root.path()).unwrap());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let desktop = {
            let store = store.clone();
            let barrier = barrier.clone();
            let run = desktop_run.clone();
            let reservation = desktop_reservation.clone();
            std::thread::spawn(move || {
                barrier.wait();
                admission_kind(store.admit_run_with_quota(&run, &reservation))
            })
        };
        let native = {
            let store = store.clone();
            let barrier = barrier.clone();
            let run = native_run.clone();
            let reservation = native_reservation.clone();
            std::thread::spawn(move || {
                barrier.wait();
                admission_kind(store.admit_run_with_quota(&run, &reservation))
            })
        };
        let outcomes = [desktop.join().unwrap(), native.join().unwrap()];
        assert_eq!(
            outcomes.iter().filter(|kind| **kind == "committed").count(),
            1,
            "exactly one last-slot winner: {outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|kind| **kind == "not_committed")
                .count(),
            1,
            "the loser must be definitely not committed: {outcomes:?}"
        );
        drop(store);
        for _ in 0..2 {
            let reopened = OrchStore::open(root.path()).unwrap();
            let runs = reopened.list_runs().unwrap();
            assert_eq!(runs.len(), 1);
            let reservations = reopened.list_quota_reservations().unwrap();
            assert_eq!(reservations.len(), 1);
            assert_eq!(runs[0].run_id, reservations[0].run_id);
            drop(reopened);
        }
    }

    #[test]
    fn provider_attempt_index_is_bounded_for_foreign_high_cardinality() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (own, own_reservation) = quota_backed_run(
            "indexed-own",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let (foreign, foreign_reservation) = quota_backed_run(
            "indexed-foreign",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let store = OrchStore::open(root.path()).unwrap();
        store
            .admit_run_with_quota(&own, &own_reservation)
            .into_result()
            .unwrap();
        store
            .admit_run_with_quota(&foreign, &foreign_reservation)
            .into_result()
            .unwrap();
        for ordinal in 1..=200 {
            let attempt = super::super::provider_attempt::ProviderAttemptRecord::admitted(
                &own,
                format!("own-attempt-{ordinal}"),
                ordinal,
                now,
            )
            .unwrap();
            store.test_put_provider_attempt(&attempt).unwrap();
        }
        for ordinal in 1..=400 {
            let attempt = super::super::provider_attempt::ProviderAttemptRecord::admitted(
                &foreign,
                format!("foreign-attempt-{ordinal}"),
                ordinal,
                now,
            )
            .unwrap();
            store.test_put_provider_attempt(&attempt).unwrap();
        }
        store.reset_attempt_index_files_read();
        let page = store.list_provider_attempts_for_run(&own.run_id).unwrap();
        let files_read = store.attempt_index_files_read();
        assert_eq!(page.total_count, 200);
        assert!(page.truncated);
        assert_eq!(page.attempts.len(), MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE);
        assert_eq!(page.attempts[0].ordinal, 1);
        assert_eq!(
            page.attempts.last().map(|attempt| attempt.ordinal),
            Some(MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE as u64)
        );
        assert!(
            files_read <= 200 + MAX_PROVIDER_ATTEMPTS_PER_RUN_PAGE,
            "per-run index must not scan foreign attempts; files_read={files_read}"
        );
        store.reset_attempt_index_files_read();
        let _ = store.list_provider_attempts_for_run(&own.run_id).unwrap();
        let second_read = store.attempt_index_files_read();
        assert_eq!(second_read, files_read);
    }

    #[test]
    fn admit_run_with_quota_uncertain_into_result_is_typed_and_not_zero_effect() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, reservation) = quota_backed_run(
            "uncertain-result",
            RunState::Running,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let store = OrchStore::open(root.path()).unwrap();
        store.set_persist_cut(Some(AdmissionPersistCut::AfterQuota));
        let error = store
            .admit_run_with_quota(&run, &reservation)
            .into_result()
            .unwrap_err();
        assert!(
            UncertainAdmission::is(&error),
            "Uncertain must not collapse to an ordinary zero-effect error: {error}"
        );
        assert!(store
            .load_quota_reservation(&reservation.reservation_id)
            .unwrap()
            .is_some());
        assert!(store
            .quota_admission_intent_path(&run.run_id)
            .unwrap()
            .is_file());
    }

    #[test]
    fn admit_run_after_run_cut_is_uncertain_and_retains_run() {
        let root = tempdir().unwrap();
        let store = OrchStore::open(root.path()).unwrap();
        let mut run = terminal_run("offline-admit");
        run.state = RunState::Queued;
        run.terminal_result = None;
        store.set_persist_cut(Some(AdmissionPersistCut::AfterRun));
        let error = store.admit_run(&run).into_result().unwrap_err();
        assert!(
            UncertainAdmission::is(&error),
            "no-quota persist Uncertain must not collapse to a zero-effect error: {error}"
        );
        assert!(store.load_run(&run.run_id).unwrap().is_some());
        drop(store);
        let first = OrchStore::open(root.path()).unwrap();
        assert_eq!(
            first.load_run(&run.run_id).unwrap().unwrap().run_id,
            run.run_id
        );
        drop(first);
        let second = OrchStore::open(root.path()).unwrap();
        assert_eq!(
            second.load_run(&run.run_id).unwrap().unwrap().run_id,
            run.run_id
        );
    }

    #[test]
    fn admit_run_rejects_quota_backed_runs() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let (run, _reservation) = quota_backed_run(
            "must-use-quota-admit",
            RunState::Queued,
            now,
            super::super::quota::QuotaLimits::default(),
        );
        let store = OrchStore::open(root.path()).unwrap();
        assert_eq!(admission_kind(store.admit_run(&run)), "not_committed");
        assert!(store.load_run(&run.run_id).unwrap().is_none());
    }

    #[test]
    fn session_run_index_pages_without_scanning_foreign_sessions() {
        let root = tempdir().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
        let limits = super::super::quota::QuotaLimits {
            max_in_flight_reservations: 1024,
            ..super::super::quota::QuotaLimits::default()
        };
        let own_session = Uuid::from_u128(0x1111);
        let foreign_session = Uuid::from_u128(0x2222);
        let store = OrchStore::open(root.path()).unwrap();
        for index in 0..200 {
            let created = now + Duration::milliseconds(index as i64);
            let (mut run, _) = quota_backed_run(
                &format!("own-run-{index:03}"),
                RunState::Completed,
                created,
                limits,
            );
            run.session_id = own_session;
            run.workspace = "/tmp/own-session".into();
            run.created_at = created;
            run.updated_at = created;
            let reservation =
                super::super::quota::QuotaReservation::for_run(&run, "owner-1", limits, created)
                    .unwrap();
            store
                .admit_run_with_quota(&run, &reservation)
                .into_result()
                .unwrap();
        }
        for index in 0..400 {
            let created = now + Duration::milliseconds(index as i64);
            let (mut run, _) = quota_backed_run(
                &format!("foreign-run-{index:03}"),
                RunState::Completed,
                created,
                limits,
            );
            run.session_id = foreign_session;
            run.workspace = "/tmp/foreign-session".into();
            run.created_at = created;
            run.updated_at = created;
            let reservation =
                super::super::quota::QuotaReservation::for_run(&run, "owner-1", limits, created)
                    .unwrap();
            store
                .admit_run_with_quota(&run, &reservation)
                .into_result()
                .unwrap();
        }
        store.reset_session_run_index_files_read();
        let page = store
            .list_runs_for_session_page(own_session, Some("/tmp/own-session"), None, None)
            .unwrap();
        let files_read = store.session_run_index_files_read();
        assert_eq!(page.total_count, 200);
        assert!(page.truncated);
        assert_eq!(page.runs.len(), MAX_PUBLIC_RUN_LIST);
        assert_eq!(page.runs[0].run_id, "own-run-199");
        assert_eq!(
            page.runs.last().map(|run| run.run_id.as_str()),
            Some("own-run-072")
        );
        assert_eq!(page.next_cursor.as_deref(), Some("own-run-072"));
        assert!(
            files_read <= 200 + MAX_PUBLIC_RUN_LIST,
            "session index must not scan foreign Runs; files_read={files_read}"
        );
        store.reset_session_run_index_files_read();
        let _ = store
            .list_runs_for_session_page(own_session, Some("/tmp/own-session"), None, None)
            .unwrap();
        assert_eq!(store.session_run_index_files_read(), files_read);
    }

    #[test]
    fn persist_cut_subprocess_kill_then_two_reopens() {
        const ROOT: &str = "GROKPTAH_ADMISSION_KILL_ROOT";
        const MODE: &str = "GROKPTAH_ADMISSION_KILL_MODE";
        if let Ok(root) = std::env::var(ROOT) {
            let store = OrchStore::open(&root).unwrap();
            let now = Utc.with_ymd_and_hms(2026, 8, 22, 12, 0, 0).unwrap();
            let (mut run, reservation) = quota_backed_run(
                "kill-run",
                RunState::Running,
                now,
                super::super::quota::QuotaLimits::default(),
            );
            match std::env::var(MODE).unwrap().as_str() {
                "after-quota" => {
                    store.set_persist_cut(Some(AdmissionPersistCut::AfterQuota));
                    let _ = store.admit_run_with_quota(&run, &reservation);
                }
                "after-abort-journal" => {
                    let agent = waiting_agent(run.session_id, "agent-kill");
                    run.agent_id = Some(agent.agent_id.clone());
                    run.agent_spec_revision = Some(agent.current_spec().unwrap().revision);
                    store.save_agent(&agent).unwrap();
                    store
                        .admit_run_and_activate_agent(&run, &agent.agent_id, Some(&reservation))
                        .into_result()
                        .unwrap();
                    store.set_persist_cut(Some(AdmissionPersistCut::AfterAbortJournal));
                    let _ = store.terminalize_unstarted_admission(
                        &run.run_id,
                        "admission_aborted",
                        "subprocess kill after abort journal",
                    );
                }
                other => panic!("unknown kill mode {other}"),
            }
            #[cfg(unix)]
            unsafe {
                libc::raise(libc::SIGKILL);
            }
            #[cfg(not(unix))]
            std::process::abort();
        }

        for mode in ["after-quota", "after-abort-journal"] {
            let root = tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let status = std::process::Command::new(&exe)
                .arg("--exact")
                .arg("orchestration::store::tests::persist_cut_subprocess_kill_then_two_reopens")
                .env(ROOT, root.path())
                .env(MODE, mode)
                .env("RUST_TEST_THREADS", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(
                !status.success(),
                "{mode} helper must die before Drop cleanup"
            );
            assert_two_reopens_agree(root.path(), "kill-run", "quota-kill-run");
            if mode == "after-abort-journal" {
                let reopened = OrchStore::open(root.path()).unwrap();
                let recovered = reopened.load_run("kill-run").unwrap().unwrap();
                assert!(recovered.state.is_terminal());
                assert_eq!(recovered.error_code.as_deref(), Some("admission_aborted"));
            }
        }
    }
}
