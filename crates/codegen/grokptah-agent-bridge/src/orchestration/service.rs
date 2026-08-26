//! Orchestration service: reads + bounded mutations over AgentHostHandle (#196).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::event_bus::{CursorExpiredError, EventBus, EventReceiver, JournalPage};
use crate::host::AgentHostHandle;
use crate::prompt_queue::{PromptQueueEntry, SteeringDisposition};
use crate::session::{SessionKind, WorkspaceStatus};

use super::admission::{
    AcceptanceIntent, AttemptLease, AuthorizationSnapshot, GateOutcome, LiveWorker,
    ProviderRequestSink, ProviderRequestTicket, ProviderSendFailure, ProviderSendState,
    RequestPhase, SealedBounds, SpecBinding, SpecHolder, StartGate, WorkerLiveness,
    WorkerLivenessGuard, ACCEPTANCE_INTENT_VERSION, DEFAULT_ATTEMPT_LEASE_TTL_MS,
    DEFAULT_TEARDOWN_BUDGET,
};
use super::authz::{canonical_workspace, require_workspace_match, AuthContext, WorkspaceAllowlist};
use super::store::{IdempotencyClaim, OrchStore};
use super::types::*;

/// Admission is deliberately bounded so an untrusted coordinator cannot turn
/// queued submissions into an unbounded in-memory prompt store.
const MAX_PENDING_ADMISSIONS: usize = 32;

/// Execution spec revision sealed into every acceptance intent. Bump this
/// when the meaning of a sealed field changes, so intents accepted under the
/// old contract stop verifying instead of running under the new one.
const EXECUTION_SPEC_REVISION: &str = "grokptah-agent-bridge/orchestration/1";

/// How often a live attempt refreshes its durable lease heartbeat.
const ATTEMPT_HEARTBEAT_INTERVAL: Duration =
    Duration::from_millis(DEFAULT_ATTEMPT_LEASE_TTL_MS / 3);

/// Bounded number of finished attempts whose worker liveness stays queryable.
const MAX_REMEMBERED_LIVENESS: usize = 256;

/// How often the expired-lease reconciler sweeps. Comfortably shorter than a
/// lease lifetime so a dead holder is reclaimed within one TTL.
const LEASE_RECONCILE_INTERVAL: Duration = Duration::from_millis(DEFAULT_ATTEMPT_LEASE_TTL_MS / 4);

/// How often durable queued work is re-derived from the ledger.
const QUEUE_RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Default)]
struct AdmissionQueueState {
    pending: VecDeque<PendingRun>,
}

/// A queued admission. The prompt is deliberately **not** held here: the
/// sealed [`AcceptanceIntent`] on disk is the only source of execution input,
/// so a restart loses nothing and an untrusted coordinator cannot grow an
/// in-memory prompt store.
struct PendingRun {
    run_id: String,
    session_id: Uuid,
}

#[derive(Clone)]
pub struct OrchestrationConfig {
    pub bearer_token: String,
    pub allowlist: WorkspaceAllowlist,
    pub max_concurrent_runs: usize,
    pub bounds: RunBounds,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            bearer_token: String::new(),
            allowlist: WorkspaceAllowlist::default(),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        }
    }
}

pub struct OrchestrationService {
    host: AgentHostHandle,
    bus: EventBus,
    store: OrchStore,
    config: Mutex<OrchestrationConfig>,
    self_ref: Weak<OrchestrationService>,
    pending_admissions: Mutex<AdmissionQueueState>,
    scheduler_watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Authoritative registry of every dispatched attempt that can still run.
    ///
    /// An entry exists from the moment a worker is spawned until its capacity
    /// has been released, and it owns the cancel token plus the abort handles
    /// for the nested worker/aggregator tasks. Nothing may release a run's
    /// capacity, promote another attempt, or report the run terminal while its
    /// entry is still live.
    live_workers: Mutex<HashMap<String, Arc<LiveWorker>>>,
    /// Run ids claimed for dispatch but not yet published. A reservation
    /// blocks a second attempt without exposing an entry whose handles are
    /// still being attached.
    dispatch_reservations: Mutex<std::collections::HashSet<String>>,
    /// Bounded post-mortem liveness of recent attempts, so callers can ask
    /// whether the *worker future itself* is gone rather than trusting the
    /// ledger.
    remembered_liveness: Mutex<VecDeque<(String, Arc<WorkerLiveness>)>>,
    /// Identity of this process instance for attempt-lease ownership.
    owner_id: String,
    /// The one place teardown is performed. Every path that wants an attempt
    /// stopped sends a request here instead of tearing it down itself, so
    /// there is exactly one owner of the abort-then-prove-quiescence sequence
    /// and exactly one place that may release capacity.
    teardown_tx: tokio::sync::mpsc::UnboundedSender<TeardownRequest>,
    teardown_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Independent reconcilers: expired attempt leases, and durable queued
    /// runs that no live pump is tracking.
    reconcilers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// Why an attempt is being torn down. Carried through so the terminal record
/// and the audit trail can name the cause rather than inferring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TeardownReason {
    /// The supervisor finished normally and its terminal record is installed.
    Completed,
    /// An explicit cancel from the control plane.
    Cancelled,
    /// The attempt's wall-clock bound elapsed.
    Deadline,
    /// The durable lease expired or was taken by another attempt.
    LeaseLost,
    /// The supervisor exited without installing a terminal record.
    SupervisorExit,
    /// Authorization changed after acceptance.
    AuthorizationDrift,
}

impl TeardownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Deadline => "deadline",
            Self::LeaseLost => "lease_lost",
            Self::SupervisorExit => "supervisor_exit",
            Self::AuthorizationDrift => "authorization_drift",
        }
    }
}

/// A snapshot of one live attempt's teardown state, for callers that need to
/// reason about capacity rather than about run outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptStatus {
    pub attempt: u64,
    /// Every handle is in the registry and the start gate has opened.
    pub registered: bool,
    /// Cancellation is signalled and the nested futures are aborted. Says
    /// nothing about whether anything has stopped.
    pub fenced: bool,
    /// The terminal record has been installed or staged.
    pub finalized: bool,
    /// Host capacity has been given back. Only ever true after quiescence was
    /// proved.
    pub capacity_released: bool,
    /// A bounded teardown could not prove the work stopped. Lease and capacity
    /// are retained while this holds.
    pub escaped: bool,
    /// The worker future itself can no longer execute.
    pub worker_quiescent: bool,
    /// Work behind this attempt's start gate could still begin.
    ///
    /// Reported separately from `worker_quiescent` because the two answer
    /// different questions: one is "has it stopped?", the other is "can it
    /// start?", and an attempt in the registration gap is neither.
    pub may_still_start: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TeardownRequest {
    run_id: String,
    reason: TeardownReason,
}

/// Authorized bounds for a live run event stream.
#[derive(Debug, Clone)]
pub(crate) struct LiveRunScope {
    pub session_id: Uuid,
    pub run_id: String,
    pub start_seq: u64,
    pub end_seq: Option<u64>,
}

impl Drop for OrchestrationService {
    fn drop(&mut self) {
        if let Some(watcher) = self.scheduler_watcher.get_mut().take() {
            watcher.abort();
        }
        if let Some(task) = self.teardown_task.get_mut().take() {
            task.abort();
        }
        for task in self.reconcilers.get_mut().drain(..) {
            task.abort();
        }

        // `Drop` is synchronous, so it cannot await anything, which means it
        // can never prove a worker stopped. It therefore does exactly two
        // things: it **fences** every live attempt, and it **records that the
        // outcome is unknown**.
        //
        // What it must not do is release. Dropping a lease, dropping durable
        // input, or handing back capacity would each authorize a second
        // attempt on the strength of an abort *request*, and an abort request
        // is not evidence. Callers that can await should use
        // `shutdown().await`, which proves quiescence first; this is the last
        // resort for when nobody did.
        let live: Vec<Arc<LiveWorker>> = self
            .live_workers
            .get_mut()
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in live {
            entry.fence();
            if entry.claim_finalization() {
                stage_supervisor_exit(&self.store, &entry.run_id, "shutdown");
            }
            if entry.liveness.quiescent() {
                // Provably gone: the gate was abandoned before it opened, or
                // the worker had already finished. Nothing is uncertain, so
                // the conflict domain does not need fencing past this point.
                continue;
            }
            // Otherwise the run keeps its lease and its capacity until
            // something that can await reconciles it, and the ledger says so
            // out loud rather than leaving the gap to be inferred.
            if let Err(error) = self.store.record_teardown_uncertain(
                &entry.run_id,
                &entry.attempt_id,
                &self.owner_id,
                "process shut down while the attempt was live",
            ) {
                eprintln!(
                    "[grokptah] run {} teardown uncertainty could not be recorded: {}",
                    entry.run_id, error.message
                );
            }
        }

        let pending = self
            .pending_admissions
            .get_mut()
            .pending
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        for run_id in pending {
            // A queued run never started, so releasing its *queue* slot claims
            // nothing about whether a worker ran.
            self.host.release_orchestration_queue_slot(&run_id);
        }
    }
}

/// Record that one outer supervisor exited without installing a terminal
/// record, leaving a bounded recoverable finalization intent behind.
///
/// A run that is already terminal is left exactly as it is: an explicit
/// cancel that raced this teardown keeps its `cancelled` outcome. Everything
/// else becomes `interrupted`, which is the honest live-reaper outcome.
fn stage_supervisor_exit(store: &OrchStore, run_id: &str, reason: &str) {
    let Ok(Some(mut candidate)) = store.load_run(run_id) else {
        return;
    };
    if candidate.state.is_terminal() {
        return;
    }
    candidate.state = RunState::Interrupted;
    candidate.queue_position = None;
    candidate.terminal_result = Some("interrupted".into());
    candidate.error_code = Some(reason.into());
    candidate.updated_at = Utc::now();
    if let Some(execution) = candidate.execution.as_mut() {
        execution.promotion_state = PromotionState::Conflicted;
    }
    if store.persist_finalization(&candidate).is_err() {
        // Bounded, recoverable: replayed by the next `OrchStore::open`.
        if let Err(error) = store.stage_finalization_intent(&candidate) {
            eprintln!("[grokptah] run {run_id} supervisor exit could not be recorded: {error}");
        }
    }
}

/// The durable sink one attempt's physical provider requests report to.
///
/// Holds the store and the run identity, so the HTTP layer can report phases
/// without knowing anything about the ledger. `may_send` is the enforcement
/// point: it is consulted before *every* send, including retries, so a retry
/// across an unobserved outcome is refused rather than attempted and regretted.
pub struct LedgerRequestSink {
    store: OrchStore,
    run_id: String,
}

impl LedgerRequestSink {
    pub fn new(store: OrchStore, run_id: String) -> Self {
        Self { store, run_id }
    }
}

impl ProviderRequestSink for LedgerRequestSink {
    fn record(
        &self,
        ticket: &ProviderRequestTicket,
        phase: RequestPhase,
        detail: Option<&str>,
    ) -> Result<(), String> {
        self.store
            .record_provider_request(&self.run_id, ticket, phase, detail)
            .map_err(|error| error.message)
    }

    fn may_send(&self, ticket: &ProviderRequestTicket) -> Result<(), String> {
        // Every earlier physical request for this run must have reached a
        // state that makes another send safe. `Uncertain` never does: the
        // provider may already have run the work, and no local retry can
        // establish otherwise.
        let history = self
            .store
            .list_provider_requests(&self.run_id)
            .map_err(|error| error.message)?;
        for previous in history {
            if previous.request_ordinal >= ticket.request_ordinal {
                continue;
            }
            match previous.phase {
                RequestPhase::KnownNotSent | RequestPhase::Settled => {}
                RequestPhase::Uncertain => {
                    return Err(format!(
                        "request {} for this run has an unobserved outcome; \
                         resending would risk duplicate provider work. \
                         Reconcile it or record an explicit operator disposition first.",
                        previous.request_ordinal
                    ));
                }
                other => {
                    return Err(format!(
                        "request {} for this run is still {}; a second send is not authorized",
                        previous.request_ordinal,
                        other.as_str()
                    ));
                }
            }
        }
        // A run fenced by an unresolved teardown never sends at all.
        if matches!(
            self.store.load_teardown_uncertain(&self.run_id),
            Ok(Some(_))
        ) {
            return Err("run is fenced by an unresolved teardown".into());
        }
        Ok(())
    }
}

/// Why a worker future did not return a turn result.
///
/// The classification is durable evidence, not a log line: `send_failure`
/// decides what the provider-send ledger records, and `error_code` decides
/// what the run's terminal record says. Collapsing these into one opaque error
/// is how "the local task failed" gets mistaken for "the work never ran".
#[derive(Debug, Clone)]
pub(crate) struct WorkerOutcome {
    pub message: String,
    pub error_code: &'static str,
    pub send_failure: Option<ProviderSendFailure>,
}

impl WorkerOutcome {
    /// The attempt was cancelled during the registration gap, before its gate
    /// opened. Nothing was transmitted because nothing ever ran.
    fn abandoned() -> Self {
        Self {
            message: "attempt was cancelled before it started".into(),
            error_code: "cancelled_before_start",
            send_failure: Some(ProviderSendFailure::PreflightRejected),
        }
    }

    /// Refused before the turn began; nothing was transmitted.
    fn refused(error: OrchError) -> Self {
        Self {
            message: error.message,
            error_code: "authorization_drift",
            send_failure: Some(ProviderSendFailure::PreflightRejected),
        }
    }

    /// The durable send ledger could not be advanced.
    fn send_failed(failure: ProviderSendFailure, error: OrchError) -> Self {
        Self {
            message: error.message,
            error_code: match failure.resulting_state() {
                ProviderSendState::Uncertain => "provider_send_uncertain",
                _ => "provider_send_blocked",
            },
            send_failure: Some(failure),
        }
    }

    /// The turn itself returned an error after transmission began.
    fn turn_failed(error: anyhow::Error) -> Self {
        Self {
            message: error.to_string(),
            error_code: "internal",
            send_failure: None,
        }
    }

    /// The worker task did not complete: a panic or an abort.
    fn panicked(detail: String) -> Self {
        Self {
            message: format!("run worker did not complete: {detail}"),
            error_code: "worker_lost",
            send_failure: Some(ProviderSendFailure::AttemptTornDown),
        }
    }
}

/// Allocate a journal sequence through the service, when it is still alive.
fn bus_next_seq_for(service: &Weak<OrchestrationService>) -> Option<u64> {
    service.upgrade().map(|service| service.bus.next_seq())
}

/// Redact through the shared bus so worker detail can never carry a secret.
fn redact_for(service: &Weak<OrchestrationService>, text: &str) -> String {
    match service.upgrade() {
        Some(service) => service.bus.redact_text(text, 500),
        None => String::new(),
    }
}

/// The synchronous exit fence for one dispatched attempt.
///
/// This guard is uniquely owned by the outer supervisor, and its `Drop` runs
/// on **every** way that supervisor can end: a normal return, an early return,
/// a panic unwind, an abort landing on any await point, and runtime shutdown.
///
/// What it may do is deliberately narrow. `Drop` is synchronous, so it cannot
/// await anything, which means it can never prove that the worker future has
/// stopped. It therefore:
///
/// 1. **fences** the attempt — signals cancellation, aborts the nested futures;
/// 2. **stages** durable terminal evidence — the computed terminal record, or
///    a bounded recoverable finalization intent the next store open replays;
/// 3. **hands off** to the async teardown owner.
///
/// It does **not** release host capacity, and it does not release the durable
/// attempt lease. Both of those would authorize a second attempt, and an abort
/// request is not evidence that the first one stopped. Only the teardown
/// owner, which can await, is allowed to draw that conclusion.
struct SupervisorExitGuard {
    store: OrchStore,
    service: Weak<OrchestrationService>,
    entry: Arc<LiveWorker>,
    run_id: String,
    /// The terminal record the supervisor computed, when it got that far.
    candidate: Option<RunRecord>,
    reason: TeardownReason,
}

/// Bounded attempts to install a terminal record before falling back to a
/// staged intent. Unbounded retrying here would hold admission capacity for as
/// long as the disk stays full.
const FINALIZATION_ATTEMPTS: u32 = 3;

impl Drop for SupervisorExitGuard {
    fn drop(&mut self) {
        // Stop the attempt from progressing. Synchronous and immediate; it
        // claims nothing about whether anything has actually stopped yet.
        self.entry.fence();

        if self.entry.claim_finalization() {
            match self.candidate.take() {
                Some(candidate) => {
                    let mut installed = false;
                    let mut last_error = None;
                    for _ in 0..FINALIZATION_ATTEMPTS {
                        match self.store.persist_finalization(&candidate) {
                            Ok(_) => {
                                installed = true;
                                break;
                            }
                            Err(error) => last_error = Some(error.to_string()),
                        }
                    }
                    if !installed {
                        // Bounded recoverable finalization intent: replayed at
                        // the next store open, so the run is never left
                        // non-terminal with no record of why.
                        if let Err(error) = self.store.stage_finalization_intent(&candidate) {
                            eprintln!(
                                "[grokptah] run {} finalization could not be staged: {error}",
                                self.run_id
                            );
                        }
                        let detail = last_error.unwrap_or_else(|| "unknown".into());
                        let _ = self.store.enqueue_audit(AuditEntry {
                            ts: Utc::now(),
                            tool: "run_finalization".into(),
                            request_id: None,
                            session_id: Some(candidate.session_id),
                            workspace: None,
                            outcome: "deferred".into(),
                            error_code: Some("run_persistence_failed".into()),
                            detail,
                        });
                    }
                }
                // The supervisor never reached a terminal decision: it
                // panicked, was aborted, or the process is shutting down.
                None => stage_supervisor_exit(&self.store, &self.run_id, "supervisor_exit"),
            }
        }

        // Hand off to the one owner allowed to prove quiescence and release.
        if let Some(service) = self.service.upgrade() {
            service.request_teardown(&self.run_id, self.reason);
        }
    }
}

enum IdempotencyStart {
    Perform(IdempotencyLease),
    Replay(serde_json::Value),
}

struct IdempotencyLease {
    store: OrchStore,
    tool: String,
    request_id: String,
    payload_hash: String,
    /// The execution specification this receipt admits, once one exists.
    spec_key: Option<String>,
    settled: bool,
}

impl IdempotencyLease {
    fn complete(
        &mut self,
        run_id: Option<String>,
        response: serde_json::Value,
    ) -> Result<(), OrchError> {
        self.store.complete_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            self.spec_key.clone(),
            response,
        )?;
        self.settled = true;
        Ok(())
    }

    fn fail(&mut self, run_id: Option<String>, error: OrchError) -> OrchError {
        match self.store.fail_idempotency(
            &self.tool,
            &self.request_id,
            &self.payload_hash,
            run_id,
            error.clone(),
        ) {
            Ok(()) => {
                self.settled = true;
                error
            }
            Err(store_error) => store_error,
        }
    }
}

impl Drop for IdempotencyLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let error = OrchError::new(
            OrchErrorCode::Internal,
            "mutation abandoned before its durable outcome completed",
        );
        if self
            .store
            .fail_idempotency(
                &self.tool,
                &self.request_id,
                &self.payload_hash,
                None,
                error,
            )
            .is_ok()
        {
            self.settled = true;
        }
    }
}

impl OrchestrationService {
    pub fn new(
        host: AgentHostHandle,
        bus: EventBus,
        store: OrchStore,
        mut config: OrchestrationConfig,
    ) -> Arc<Self> {
        host.install_orchestration_store(store.clone());
        // The host owns the process-wide ledger. If desktop bootstrap opened
        // it first, use that same handle instead of creating a split history.
        let store = host.ensure_orchestration_store().unwrap_or(store);
        // Register control bearer (and any future secrets) on the *shared* host bus
        // so durable journal redaction covers the shipped desktop path.
        if !config.bearer_token.is_empty() {
            bus.add_control_secrets([config.bearer_token.clone()]);
        }
        config.max_concurrent_runs =
            host.configure_orchestration_capacity(config.max_concurrent_runs);
        let (teardown_tx, teardown_rx) = tokio::sync::mpsc::unbounded_channel();
        let service = Arc::new_cyclic(|self_ref| Self {
            host,
            bus,
            store,
            config: Mutex::new(config),
            self_ref: self_ref.clone(),
            pending_admissions: Mutex::new(AdmissionQueueState::default()),
            scheduler_watcher: Mutex::new(None),
            live_workers: Mutex::new(HashMap::new()),
            dispatch_reservations: Mutex::new(std::collections::HashSet::new()),
            remembered_liveness: Mutex::new(VecDeque::new()),
            owner_id: Uuid::new_v4().to_string(),
            teardown_tx,
            teardown_task: Mutex::new(None),
            reconcilers: Mutex::new(Vec::new()),
        });
        service.start_scheduler_watcher();
        service.start_teardown_owner(teardown_rx);
        service.start_reconcilers();
        // Re-admit work that was durably accepted before the last restart,
        // before any new submission can take its capacity.
        service.recover_admissions();
        service
    }

    /// Shut down every live attempt, proving quiescence before releasing.
    ///
    /// This is the path a caller that can await should always use. It fences
    /// each attempt, waits within a bound for its worker to actually stop, and
    /// only then releases the lease, the durable input, and the capacity.
    /// Anything that cannot be proved is recorded as teardown-uncertain and
    /// keeps its conflict domain fenced rather than being assumed finished.
    ///
    /// Returns the run ids whose outcome could not be established.
    pub async fn shutdown(&self) -> Vec<String> {
        let live: Vec<Arc<LiveWorker>> = self.live_workers.lock().values().cloned().collect();
        let mut uncertain = Vec::new();
        for entry in live {
            let outcome = entry.terminate(DEFAULT_TEARDOWN_BUDGET).await;
            if !outcome.may_release_capacity() {
                // Not provably stopped. Fence the conflict domain and say so.
                let _ = self.store.record_teardown_uncertain(
                    &entry.run_id,
                    &entry.attempt_id,
                    &self.owner_id,
                    &format!("bounded teardown ended as {outcome:?}"),
                );
                uncertain.push(entry.run_id.clone());
                continue;
            }
            // Quiescence is proved. Winning the release latch means this call
            // performs the release; losing it means the teardown owner already
            // did, which is the same success reached by another path — not a
            // reason to fence anything.
            if entry.claim_capacity_release() {
                let _ = self.store.release_attempt_lease(
                    &entry.run_id,
                    &entry.attempt_id,
                    &self.owner_id,
                );
                let _ = self.store.remove_acceptance_intent(&entry.run_id);
                self.host.release_orchestration_turn(&entry.run_id);
            }
            self.live_workers.lock().remove(&entry.run_id);
        }
        if let Some(task) = self.teardown_task.lock().take() {
            task.abort();
        }
        uncertain
    }

    /// Start the single async owner of teardown.
    ///
    /// Everything that wants an attempt stopped — an explicit cancel, a
    /// deadline, a lost lease, a supervisor exit, a shutdown — sends a request
    /// here. Concentrating it means the abort-then-bounded-await sequence
    /// happens once per attempt, in an async context that can actually await,
    /// and that capacity release has exactly one caller which can only reach
    /// it through proved quiescence.
    fn start_teardown_owner(&self, mut rx: tokio::sync::mpsc::UnboundedReceiver<TeardownRequest>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let service_ref = self.self_ref.clone();
        let task = runtime.spawn(async move {
            while let Some(request) = rx.recv().await {
                let Some(service) = service_ref.upgrade() else {
                    break;
                };
                service.perform_teardown(request).await;
            }
        });
        *self.teardown_task.lock() = Some(task);
    }

    /// Abort, prove quiescence, and only then release.
    ///
    /// The order here is the whole point. Capacity is what lets another
    /// attempt start, and the durable lease is what lets another *process*
    /// start one, so neither is given up until the worker's own liveness guard
    /// has confirmed the future is gone. When it cannot be confirmed, both are
    /// deliberately retained: an escaped worker holding a slot is a bounded
    /// capacity loss, while an escaped worker whose slot was reused is
    /// unbounded duplicate execution.
    async fn perform_teardown(&self, request: TeardownRequest) {
        let Some(entry) = self.live_workers.lock().get(&request.run_id).cloned() else {
            return;
        };
        let outcome = entry.terminate(DEFAULT_TEARDOWN_BUDGET).await;
        if !entry.claim_capacity_release_after(outcome) {
            if !outcome.may_release_capacity() {
                self.audit(
                    "run_teardown",
                    None,
                    Some(entry.session_id),
                    None,
                    "rejected",
                    Some("teardown_incomplete"),
                    &format!(
                        "run {} escaped teardown ({outcome:?}); lease and capacity retained",
                        request.run_id
                    ),
                );
            }
            return;
        }

        // Proved quiescent. The run is terminal by now (the supervisor's exit
        // guard installs or stages it), so its lease and its private input can
        // finally be dropped.
        let _ =
            self.store
                .release_attempt_lease(&request.run_id, &entry.attempt_id, &self.owner_id);
        let _ = self.store.remove_acceptance_intent(&request.run_id);
        self.live_workers.lock().remove(&request.run_id);
        self.remove_pending(&request.run_id);
        self.host.release_orchestration_turn(&request.run_id);
        self.audit(
            "run_teardown",
            None,
            Some(entry.session_id),
            None,
            "accepted",
            None,
            &format!(
                "run {} torn down ({})",
                request.run_id,
                request.reason.as_str()
            ),
        );
        self.pump_pending();
    }

    /// Ask the teardown owner to stop one attempt. Never blocks, never tears
    /// anything down itself, and is safe from a `Drop`.
    pub(crate) fn request_teardown(&self, run_id: &str, reason: TeardownReason) {
        let _ = self.teardown_tx.send(TeardownRequest {
            run_id: run_id.to_string(),
            reason,
        });
    }

    /// Start the two independent reconcilers.
    ///
    /// They exist because the fast paths can all be interrupted. A pump that
    /// never runs, a lease whose holder died mid-teardown, a queued run whose
    /// in-memory entry was lost — each is invisible to the code that normally
    /// would have handled it. The reconcilers re-derive both facts from the
    /// durable ledger alone, so recovery does not depend on any in-process
    /// state surviving.
    fn start_reconcilers(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut tasks = Vec::new();

        let lease_ref = self.self_ref.clone();
        tasks.push(runtime.spawn(async move {
            let mut ticker = tokio::time::interval(LEASE_RECONCILE_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(service) = lease_ref.upgrade() else {
                    break;
                };
                service.reconcile_expired_leases();
            }
        }));

        let queued_ref = self.self_ref.clone();
        tasks.push(runtime.spawn(async move {
            let mut ticker = tokio::time::interval(QUEUE_RECONCILE_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(service) = queued_ref.upgrade() else {
                    break;
                };
                service.reconcile_durable_queued();
            }
        }));

        *self.reconcilers.lock() = tasks;
    }

    /// Capture every authorization input, as fingerprints, right now.
    ///
    /// Called once at admission to seal into the specification, and again at
    /// action time to compare. Nothing here returns a secret: each field is a
    /// digest, so a drifting credential is detectable without the credential
    /// ever being stored.
    pub(crate) fn authorization_snapshot(
        &self,
        principal_token_id: &str,
        session: &crate::session::SessionSummary,
        agent_id: Option<&str>,
    ) -> AuthorizationSnapshot {
        let (allowlist, ceiling, token) = {
            let config = self.config.lock();
            (
                config.allowlist.clone(),
                config.bounds.clone(),
                config.bearer_token.clone(),
            )
        };
        // The principal is the *authenticated caller*, not whatever credential
        // the config happens to hold. Binding the config alone would let work
        // admitted by one caller be dispatched under another's authority as
        // soon as the config was reloaded, which is the drift this exists to
        // catch. `token_id` identifies the presented credential; the config
        // digest is included as well so rotating the accepted secret is also
        // drift.
        let principal_revision = hash_payload(&json!({
            "tokenId": principal_token_id,
            "acceptedCredential": hash_payload(&json!(token)),
            "capabilities": CONTROL_TOOLS,
            "authenticated": !principal_token_id.is_empty(),
        }));
        let policy_revision = hash_payload(&json!({
            "allowlist": allowlist.fingerprint(),
            "ceiling": {
                "maxPromptBytes": ceiling.max_prompt_bytes,
                "maxRounds": ceiling.max_rounds,
                "maxDurationMs": ceiling.max_duration_ms,
            },
            "maxConcurrentRuns": self.host.orchestration_capacity_limit(),
        }));
        // Provider, model, route, and credential material as one fingerprint.
        // Offline execution is a route in its own right, not an absence of one.
        let route_revision = provider_route_revision(session);
        // The continuation revision of a persistent agent, when the work
        // belongs to one. Work with no agent has no continuation lineage, and
        // must not borrow the session's message count as a stand-in: that
        // would advance on every unrelated turn.
        let agent_revision = self.agent_continuation_revision(agent_id).unwrap_or(0);
        AuthorizationSnapshot {
            principal_revision,
            policy_revision,
            session_revision: session_revision_of(session),
            workspace_revision: workspace_revision_of(session),
            agent_revision,
            route_revision,
        }
    }

    /// Continuation revision of a persistent agent, when the work belongs to
    /// one. A resumed agent whose lineage advanced is not the same work.
    fn agent_continuation_revision(&self, agent_id: Option<&str>) -> Option<u64> {
        let agent_id = agent_id?;
        self.store
            .load_agent(agent_id)
            .ok()
            .flatten()
            .map(|agent| agent.continuation_ordinal)
    }

    /// Re-answer "may this run?" at the moment of action.
    ///
    /// Admission can be arbitrarily far in the past — a queue wait, a restart,
    /// an operator revoking a scope in between. Every authorization input is
    /// recomputed and compared against what the specification sealed, and any
    /// drift refuses the dispatch rather than executing under authority that
    /// no longer exists.
    /// Re-answer "may this run?" at the moment of action.
    ///
    /// The principal is taken from the specification, not from whoever is
    /// configured now: a queued task must execute as the principal that was
    /// authorized to submit it, and if that principal's capabilities or
    /// credential have changed since, the recomputed fingerprint says so.
    pub(crate) fn reauthorize_for_action(&self, spec: &AcceptanceIntent) -> Result<(), OrchError> {
        // Session, project, and capability must still resolve at all.
        let session = self.require_build_session(spec.session_id)?;
        let allowlist = self.config.lock().allowlist.clone();
        let workspace = PathBuf::from(&spec.workspace);
        if !allowlist.contains(&workspace) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "workspace is no longer authorized",
            ));
        }
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        require_workspace_match(&allowlist, cwd.as_deref(), &workspace)?;
        // A persistent agent that has moved on is not this work's owner.
        if let Some(agent_id) = spec.agent_id.as_deref() {
            let agent = self
                .store
                .load_agent(agent_id)
                .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
                .ok_or_else(|| {
                    OrchError::new(
                        OrchErrorCode::ForbiddenScope,
                        "persistent agent no longer exists",
                    )
                })?;
            if agent.session_id != spec.session_id {
                return Err(OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "persistent agent is bound to a different session",
                ));
            }
        }
        self.authorization_snapshot(&spec.principal_token_id, &session, spec.agent_id.as_deref())
            .reauthorize(spec)
    }

    /// Reclaim leases whose holder stopped heartbeating.
    ///
    /// A lease still owned by a live worker in this process is a teardown
    /// problem, not a reclamation problem: reclaiming it would authorize a
    /// second attempt beside a future that is still running. Those are handed
    /// to the teardown owner instead, and only genuinely ownerless leases are
    /// released.
    /// Returns how many leases this sweep reclaimed. Public so a caller can
    /// drive one sweep deterministically instead of waiting on the ticker.
    pub fn reconcile_expired_leases(&self) -> usize {
        let Ok(leases) = self.store.list_attempt_leases() else {
            return 0;
        };
        let now = Utc::now();
        let mut reclaimed = 0;
        for (_, lease) in leases {
            let Some(lease) = lease else { continue };
            if !lease.is_expired(now) || lease.state != super::admission::AttemptLeaseState::Held {
                continue;
            }
            if self.live_workers.lock().contains_key(&lease.run_id) {
                self.request_teardown(&lease.run_id, TeardownReason::LeaseLost);
                continue;
            }
            match self.store.reclaim_expired_attempt_lease(&lease.run_id) {
                Ok(Some(_)) => {
                    reclaimed += 1;
                    self.audit(
                        "lease_reconciler",
                        None,
                        Some(lease.session_id),
                        None,
                        "accepted",
                        None,
                        &format!(
                            "reclaimed expired attempt {} for run {}",
                            lease.attempt, lease.run_id
                        ),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!(
                        "[grokptah] expired lease for run {} could not be reclaimed: {}",
                        lease.run_id, error.message
                    );
                }
            }
        }
        if reclaimed > 0 {
            self.pump_pending();
        }
        reclaimed
    }

    /// Re-admit durable queued work that nothing in this process is tracking.
    ///
    /// Derived purely from the ledger, so a queued run is executed even if the
    /// pump, the scheduler watcher, and the recovery pass all missed it.
    /// Returns how many runs this sweep re-admitted. Public so a caller can
    /// drive one sweep deterministically instead of waiting on the ticker.
    pub fn reconcile_durable_queued(&self) -> usize {
        let Ok(runs) = self.store.list_runs() else {
            return 0;
        };
        let mut queued: Vec<RunRecord> = runs
            .into_iter()
            .filter(|run| run.state == RunState::Queued)
            .collect();
        queued.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.run_id.cmp(&b.run_id))
        });

        let mut readmitted = 0;
        for run in queued {
            {
                let pending = self.pending_admissions.lock();
                if pending.pending.iter().any(|p| p.run_id == run.run_id) {
                    continue;
                }
            }
            if self.live_workers.lock().contains_key(&run.run_id) {
                continue;
            }
            // An active durable lease means some attempt already owns this
            // run, in this process or another. Re-admitting it would race a
            // dispatch that has not finished registering yet.
            if matches!(
                self.store.load_attempt_lease(&run.run_id),
                Ok(Some(ref lease)) if lease.is_active(Utc::now())
            ) {
                continue;
            }
            // Only input that verifies *and* is bound to this exact run may be
            // re-admitted.
            if self.load_bound_intent(&run).is_err() {
                self.tombstone_admission(&run.run_id, "admission_tampered");
                continue;
            }
            if self
                .enqueue_pending(PendingRun {
                    run_id: run.run_id.clone(),
                    session_id: run.session_id,
                })
                .is_ok()
            {
                readmitted += 1;
            }
        }
        if readmitted > 0 {
            self.audit(
                "queue_reconciler",
                None,
                None,
                None,
                "accepted",
                None,
                &format!("re-admitted {readmitted} durable queued run(s)"),
            );
            self.pump_pending();
        }
        readmitted
    }

    /// Re-admit every durably accepted, not-yet-executed task after a restart.
    ///
    /// This is deliberately narrow. It re-admits **only** runs the ledger
    /// already records as `Queued` — which `OrchStore::open` has already
    /// reduced to those with a verifying sealed input *and* a completed
    /// receipt naming that exact run. It never invents a run: an input whose
    /// run record is missing, failed, or terminal is reclaimed as garbage, so
    /// a submission that ended in an explicit `Err` can never come back as
    /// executable work.
    fn recover_admissions(&self) {
        let mut recovered: Vec<RunRecord> = match self.store.list_runs() {
            Ok(runs) => runs
                .into_iter()
                .filter(|run| run.state == RunState::Queued)
                .collect(),
            Err(error) => {
                eprintln!("[grokptah] admission recovery could not read the ledger: {error}");
                Vec::new()
            }
        };
        // Deterministic, arrival-ordered promotion across restarts.
        recovered.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.run_id.cmp(&b.run_id))
        });

        let mut readmitted = Vec::new();
        for run in recovered {
            // The input is re-verified *and re-bound* here as well as in the
            // store sweep, so neither a tamper nor a resealed forgery landed
            // between the two can execute.
            if let Err(error) = self.load_bound_intent(&run) {
                self.audit(
                    "admission_recovery",
                    Some(&run.request_id),
                    Some(run.session_id),
                    Some(&run.workspace),
                    "rejected",
                    Some(error.code.as_str()),
                    &error.message,
                );
                self.tombstone_admission(&run.run_id, "admission_tampered");
                continue;
            }
            match self.enqueue_pending(PendingRun {
                run_id: run.run_id.clone(),
                session_id: run.session_id,
            }) {
                Ok(_) => readmitted.push(run.run_id.clone()),
                Err(error) => {
                    // The bounded queue is full. The record stays `Queued`
                    // with its input intact, so a later restart or a freed
                    // slot re-admits it; it is never silently dropped.
                    eprintln!(
                        "[grokptah] queued run {} deferred during recovery: {}",
                        run.run_id, error.message
                    );
                }
            }
        }

        // Reclaim durable input that no longer belongs to an admitted run.
        self.reclaim_orphaned_inputs();

        if !readmitted.is_empty() {
            self.audit(
                "admission_recovery",
                None,
                None,
                None,
                "accepted",
                None,
                &format!("re-admitted {} queued task(s)", readmitted.len()),
            );
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            self.pump_pending();
        }
    }

    /// Permanently fail one admission. The run can never later recover or
    /// execute: its durable input is destroyed in the same step, so no
    /// recovery pass can find anything to run.
    fn tombstone_admission(&self, run_id: &str, error_code: &str) {
        // A fenced run keeps its lease and its input: the fence exists
        // precisely because we do not know whether a worker is still using
        // them. Tombstoning it would release both on a guess.
        if matches!(self.store.load_teardown_uncertain(run_id), Ok(Some(_))) {
            return;
        }
        let _ = self.store.update_run(run_id, |run| {
            if run.state.is_terminal() {
                return Ok(());
            }
            run.state = RunState::Failed;
            run.queue_position = None;
            run.terminal_result = Some("failed".into());
            run.error_code = Some(error_code.into());
            run.updated_at = Utc::now();
            Ok(())
        });
        let _ = self.store.remove_acceptance_intent(run_id);
        let _ = self.store.remove_attempt_lease(run_id);
        self.remove_pending(run_id);
    }

    /// Remove sealed input that is not backed by an admitted, non-terminal
    /// run. This is the only cleanup path that may touch input for a run that
    /// was never dispatched, and it only ever *removes*: it can promote
    /// nothing.
    fn reclaim_orphaned_inputs(&self) {
        let Ok(entries) = self.store.list_acceptance_intent_run_ids() else {
            return;
        };
        for (stem, run_id) in entries {
            let Some(run_id) = run_id else {
                // Unreadable or non-verifying input: garbage by definition.
                let _ = self.store.remove_acceptance_intent_file(&stem);
                continue;
            };
            match self.store.load_run(&run_id) {
                // Never synthesize a run from an input. A missing run record
                // means the admission never completed; the input is garbage.
                Ok(None) => {
                    let _ = self.store.remove_acceptance_intent(&run_id);
                    let _ = self.store.remove_attempt_lease(&run_id);
                }
                Ok(Some(run)) if run.state.is_terminal() => {
                    let _ = self.store.remove_acceptance_intent(&run_id);
                    let _ = self.store.remove_attempt_lease(&run_id);
                }
                Ok(Some(_)) | Err(_) => {}
            }
        }
    }

    fn start_scheduler_watcher(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut events = self.host.subscribe_events();
        let wakeup = self.host.orchestration_wakeup();
        let service_ref = self.self_ref.clone();
        let watcher = runtime.spawn(async move {
            loop {
                tokio::select! {
                    update = events.recv() => {
                        let Some(update) = update else {
                            break;
                        };
                        if matches!(
                            update,
                            crate::events::SessionUpdate::TurnComplete { .. }
                                | crate::events::SessionUpdate::Error { .. }
                        ) {
                            let Some(service) = service_ref.upgrade() else {
                                break;
                            };
                            service.pump_pending();
                        }
                    }
                    _ = wakeup.notified() => {
                        let Some(service) = service_ref.upgrade() else {
                            break;
                        };
                        service.pump_pending();
                    }
                }
            }
        });
        *self.scheduler_watcher.lock() = Some(watcher);
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn store(&self) -> &OrchStore {
        &self.store
    }

    pub fn set_token(&self, token: String) {
        if !token.is_empty() {
            self.bus.add_control_secrets([token.clone()]);
        }
        self.config.lock().bearer_token = token;
    }

    pub fn set_allowlist(&self, allowlist: WorkspaceAllowlist) {
        self.config.lock().allowlist = allowlist;
    }

    pub(crate) fn audit_transport_result(&self, tool: &str, error: Option<&OrchError>) {
        self.audit(
            tool,
            None,
            None,
            None,
            if error.is_some() {
                "rejected"
            } else {
                "accepted"
            },
            error.map(|e| e.code.as_str()),
            "mcp transport call",
        );
    }

    pub fn auth_header(&self, header: Option<&str>) -> Result<AuthContext, OrchError> {
        let tok = self.config.lock().bearer_token.clone();
        let res = super::authz::require_bearer(header, &tok);
        if let Err(ref e) = res {
            self.audit(
                "auth",
                None,
                None,
                None,
                "rejected",
                Some(e.code.as_str()),
                "auth failed",
            );
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn audit(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        outcome: &str,
        error_code: Option<&str>,
        detail: &str,
    ) {
        let entry = AuditEntry {
            ts: Utc::now(),
            tool: self.bus.redact_text(tool, 100),
            request_id: request_id.map(|value| self.bus.redact_text(value, 256)),
            session_id,
            workspace: workspace.map(|value| self.bus.redact_text(value, 1_000)),
            outcome: self.bus.redact_text(outcome, 100),
            error_code: error_code.map(|value| self.bus.redact_text(value, 100)),
            detail: self.bus.redact_text(detail, 500),
        };
        if let Err(e) = self.store.enqueue_audit(entry) {
            eprintln!("[grokptah] orchestration audit persistence failed: {e}");
        }
    }

    fn audit_err(
        &self,
        tool: &str,
        request_id: Option<&str>,
        session_id: Option<Uuid>,
        workspace: Option<&str>,
        e: &OrchError,
    ) {
        self.audit(
            tool,
            request_id,
            session_id,
            workspace,
            "rejected",
            Some(e.code.as_str()),
            &e.message,
        );
    }

    fn try_reserve_capacity(&self, run_id: &str, session_id: Uuid) -> Result<(), OrchError> {
        self.host
            .reserve_orchestration_turn(run_id, session_id)
            .map_err(|error| {
                let message = error.to_string();
                if message.contains("max concurrent") {
                    OrchError::new(OrchErrorCode::CapacityExhausted, message)
                } else {
                    OrchError::new(OrchErrorCode::SessionBusy, message)
                }
            })
    }

    fn release_capacity(&self, run_id: &str) {
        self.host.release_orchestration_turn(run_id);
        self.pump_pending();
    }

    /// Remember a finished attempt's worker liveness so callers can prove the
    /// worker future is actually gone. Bounded so it cannot grow without end.
    fn remember_liveness(&self, run_id: &str, liveness: Arc<WorkerLiveness>) {
        let mut remembered = self.remembered_liveness.lock();
        remembered.retain(|(id, _)| id != run_id);
        remembered.push_back((run_id.to_string(), liveness));
        while remembered.len() > MAX_REMEMBERED_LIVENESS {
            remembered.pop_front();
        }
    }

    /// Whether the worker future dispatched for `run_id` can still execute.
    ///
    /// `Some(false)` means a future for this run is live — capacity must not
    /// be reused and no other attempt may be promoted. `Some(true)` means the
    /// future is provably gone (it ended, or it was cancelled before it was
    /// ever polled). `None` means this process never dispatched the run, or
    /// the record has aged out of the bounded post-mortem window.
    ///
    /// This reads the worker's own liveness guard, not the run ledger. A run
    /// can be recorded `cancelled` while its future is still running; only
    /// this answers whether the work actually stopped.
    pub fn worker_future_finished(&self, run_id: &str) -> Option<bool> {
        if let Some(entry) = self.live_workers.lock().get(run_id) {
            return Some(entry.liveness.quiescent());
        }
        self.remembered_liveness
            .lock()
            .iter()
            .find(|(id, _)| id == run_id)
            .map(|(_, liveness)| liveness.quiescent())
    }

    /// Run ids with a dispatched attempt that has not yet settled.
    ///
    /// A run listed here still owns admission capacity, so no second attempt
    /// for it may be promoted and its slot may not be handed to another run.
    pub fn live_run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .live_workers
            .lock()
            .iter()
            .filter(|(_, entry)| !entry.is_settled())
            .map(|(run_id, _)| run_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// The durable attempt identity currently dispatched for `run_id`, as
    /// `(attempt_id, attempt_number)`. Exactly one attempt is ever live.
    pub fn live_attempt(&self, run_id: &str) -> Option<(String, u64)> {
        self.live_workers
            .lock()
            .get(run_id)
            .map(|entry| (entry.attempt_id.clone(), entry.attempt))
    }

    /// Wait, within a bound, for the teardown owner to prove one attempt is
    /// gone.
    ///
    /// Answers the question a caller actually has — "can this run's slot be
    /// reused yet?" — from the worker's own liveness guard rather than from a
    /// ledger write. Returns `false` when the attempt escaped its budget,
    /// which is the honest answer and the one that keeps its capacity held.
    async fn await_quiescence(&self, run_id: &str, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            if !self.live_workers.lock().contains_key(run_id) {
                return true;
            }
            if self.worker_future_finished(run_id) == Some(true) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Where one dispatched attempt currently stands.
    ///
    /// Reports registration, fencing, capacity, and escape as separate facts
    /// because they are separate: an attempt can be fenced without being gone,
    /// and gone without having released capacity.
    pub fn attempt_status(&self, run_id: &str) -> Option<AttemptStatus> {
        let entry = self.live_workers.lock().get(run_id).cloned()?;
        Some(AttemptStatus {
            attempt: entry.attempt,
            registered: entry.is_registered(),
            fenced: entry.is_fenced(),
            finalized: entry.is_finalized(),
            capacity_released: entry.capacity_released(),
            escaped: entry.has_escaped(),
            worker_quiescent: entry.liveness.quiescent(),
            may_still_start: entry.may_still_start(),
        })
    }

    /// Keep the durable records aligned with the host-global scheduler. The
    /// prompt itself remains in memory by design; this metadata is only for
    /// honest operator/coordinator visibility while the run is pending.
    fn sync_pending_positions(&self) {
        let run_ids: Vec<String> = {
            let queue = self.pending_admissions.lock();
            queue
                .pending
                .iter()
                .map(|pending| pending.run_id.clone())
                .collect()
        };
        for run_id in run_ids {
            let Some(position) = self.host.orchestration_pending_position(&run_id) else {
                continue;
            };
            if let Err(error) = self.store.update_run(&run_id, |run| {
                if run.state == RunState::Queued {
                    run.queue_position = Some(position);
                    run.updated_at = Utc::now();
                }
                Ok(())
            }) {
                eprintln!("[grokptah] queued run position persistence failed: {error}");
            }
        }
    }

    fn clear_queue_position(&self, run_id: &str) {
        if let Err(error) = self.store.update_run(run_id, |run| {
            if run.queue_position.take().is_some() {
                run.updated_at = Utc::now();
            }
            Ok(())
        }) {
            eprintln!("[grokptah] queued run position clear failed: {error}");
        }
    }

    fn enqueue_pending(&self, pending: PendingRun) -> Result<usize, OrchError> {
        let mut queue = self.pending_admissions.lock();
        if queue.pending.len() >= MAX_PENDING_ADMISSIONS {
            return Err(OrchError::new(
                OrchErrorCode::CapacityExhausted,
                format!("bounded admission queue is full ({MAX_PENDING_ADMISSIONS} pending runs)"),
            ));
        }
        let run_id = pending.run_id.clone();
        self.host
            .reserve_orchestration_queue_slot(&run_id, pending.session_id)
            .map_err(|error| OrchError::new(OrchErrorCode::CapacityExhausted, error.to_string()))?;
        queue.pending.push_back(pending);
        drop(queue);
        self.sync_pending_positions();
        Ok(self
            .host
            .orchestration_pending_position(&run_id)
            .unwrap_or(1))
    }

    fn remove_pending(&self, run_id: &str) -> bool {
        let mut queue = self.pending_admissions.lock();
        let before = queue.pending.len();
        queue.pending.retain(|pending| pending.run_id != run_id);
        let removed = before != queue.pending.len();
        drop(queue);
        if removed {
            self.host.release_orchestration_queue_slot(run_id);
            self.sync_pending_positions();
        }
        removed
    }

    /// Promote as many queued tasks as the shared host capacity allows. The
    /// host atomically chooses the globally fair run and reserves its active
    /// turn, so two embedded control services cannot both select conflicting
    /// queue heads.
    fn pump_pending(&self) {
        loop {
            if self.host.orchestration_active_count() >= self.host.orchestration_capacity_limit() {
                return;
            }
            let candidates: Vec<(String, Uuid)> = {
                let live = self.live_workers.lock();
                let queue = self.pending_admissions.lock();
                queue
                    .pending
                    .iter()
                    // A run that already has a live attempt is not a
                    // promotion candidate. Without this the loser of a race
                    // reaches the failure paths below and releases the
                    // *winner's* capacity.
                    .filter(|pending| !live.contains_key(&pending.run_id))
                    .map(|pending| (pending.run_id.clone(), pending.session_id))
                    .collect()
            };
            let Some((run_id, _session_id)) =
                candidates.into_iter().find(|(run_id, session_id)| {
                    self.host.claim_orchestration_pending(run_id, *session_id)
                })
            else {
                return;
            };
            let pending = {
                let mut queue = self.pending_admissions.lock();
                let Some(index) = queue.pending.iter().position(|p| p.run_id == run_id) else {
                    self.host.release_orchestration_turn(&run_id);
                    continue;
                };
                queue.pending.remove(index).expect("pending index exists")
            };
            self.clear_queue_position(&pending.run_id);
            self.sync_pending_positions();

            // Cancellation can win after the task left the queue but before
            // promotion. Treat terminal records as a normal, safe skip.
            let Some(current) = self.store.load_run(&pending.run_id).ok().flatten() else {
                self.release_turn_if_not_live(&pending.run_id);
                continue;
            };
            if current.state != RunState::Queued {
                self.release_turn_if_not_live(&pending.run_id);
                continue;
            }

            // The sealed input is the only source of execution material and
            // is re-verified at every promotion, so a tamper landed while the
            // task sat in the queue fails closed instead of running.
            let intent = match self.load_bound_intent(&current) {
                Ok(intent) => intent,
                Err(error) => {
                    self.release_turn_if_not_live(&pending.run_id);
                    self.audit(
                        "run_promotion",
                        Some(&current.request_id),
                        Some(current.session_id),
                        Some(&current.workspace),
                        "rejected",
                        Some(error.code.as_str()),
                        &error.message,
                    );
                    self.tombstone_admission(&pending.run_id, "admission_tampered");
                    continue;
                }
            };

            // Reauthorize *before* promotion, not only before the gate opens.
            // A task that sat in the queue while a scope was revoked must not
            // consume a capacity slot or take a lease it is no longer entitled
            // to; refusing here keeps the slot for work that is still allowed.
            if let Err(error) = self.reauthorize_for_action(&intent) {
                self.release_turn_if_not_live(&pending.run_id);
                self.audit(
                    "run_promotion",
                    Some(&current.request_id),
                    Some(current.session_id),
                    Some(&current.workspace),
                    "rejected",
                    Some(error.code.as_str()),
                    &error.message,
                );
                self.tombstone_admission(&pending.run_id, "authorization_drift");
                continue;
            }

            // Mandatory attempt lease. A run whose previous attempt is still
            // live in this process is never promoted a second time.
            let lease = match self.acquire_dispatch_lease(&current, &intent) {
                Ok(lease) => lease,
                Err(error) => {
                    self.release_turn_if_not_live(&pending.run_id);
                    eprintln!(
                        "[grokptah] queued run {} could not take an attempt lease: {}",
                        pending.run_id, error.message
                    );
                    // The record stays `Queued` with its input intact; a later
                    // pump or restart re-admits it. Nothing is lost, nothing
                    // runs twice.
                    continue;
                }
            };

            // The promotion hands the run to a dispatched attempt; the
            // worker itself performs `queued -> running` once it starts.
            self.dispatch_attempt(current, intent, lease);
        }
    }

    /// Release a promotion's turn reservation, unless a live attempt owns it.
    ///
    /// A failed promotion must undo only its own reservation. Releasing one
    /// that a registered attempt is using would hand its slot to another run
    /// while its worker is still executing — the exact overlap the capacity
    /// accounting exists to prevent.
    fn release_turn_if_not_live(&self, run_id: &str) {
        if self.live_workers.lock().contains_key(run_id) {
            return;
        }
        self.host.release_orchestration_turn(run_id);
    }

    /// Load the durable input for a run **and** prove it is the input that run
    /// was admitted with.
    ///
    /// Validating the sealed record on its own is not enough. A forgery can be
    /// resealed: change the prompt, recompute the digest, and the record
    /// verifies perfectly — as a *different* execution specification. What
    /// exposes it is that the run, the receipt, and the lease are each bound
    /// to the original key, and the forgery matches none of them.
    ///
    /// Every path that turns durable input into execution goes through here.
    fn load_bound_intent(&self, run: &RunRecord) -> Result<AcceptanceIntent, OrchError> {
        // A run whose previous teardown could not be established is fenced.
        // Its worker may still be executing somewhere, so authorizing another
        // attempt would be authorizing an overlap. Only an explicit
        // reconciliation or operator disposition lifts this.
        if let Some(uncertain) = self.store.load_teardown_uncertain(&run.run_id)? {
            return Err(OrchError::with_data(
                OrchErrorCode::Conflict,
                "run is fenced by an unresolved teardown and cannot be dispatched",
                json!({
                    "runId": run.run_id,
                    "attemptId": uncertain.attempt_id,
                    "reason": uncertain.reason,
                }),
            ));
        }
        let intent = self
            .store
            .load_acceptance_intent(&run.run_id)?
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "run has no durable execution input",
                )
            })?;
        let receipt = self
            .store
            .load_idempotency(&run.request_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        let receipt_key = receipt.as_ref().and_then(|r| r.spec_key.as_deref());
        let lease = self.store.load_attempt_lease(&run.run_id).ok().flatten();
        SpecBinding {
            run: run.spec_key.as_deref(),
            receipt: receipt_key,
            lease: lease.as_ref().map(|lease| lease.intent_digest.as_str()),
            ..SpecBinding::default()
        }
        .verify(&intent, self.store.seal_authority(), &[SpecHolder::Run])?;
        Ok(intent)
    }

    /// Acquire the mandatory compare-and-swap attempt lease that authorizes
    /// dispatch. Refuses while this process still has a live attempt for the
    /// run, so capacity freed by a partial teardown cannot start a second
    /// worker beside the first.
    fn acquire_dispatch_lease(
        &self,
        run: &RunRecord,
        intent: &AcceptanceIntent,
    ) -> Result<AttemptLease, OrchError> {
        if self.live_workers.lock().contains_key(&run.run_id) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run already has a live attempt in this process",
            ));
        }
        self.store.acquire_attempt_lease(
            &run.run_id,
            &self.owner_id,
            run.session_id,
            &intent.digest,
            DEFAULT_ATTEMPT_LEASE_TTL_MS,
        )
    }

    async fn begin_idempotency(
        &self,
        tool: &str,
        request_id: &str,
        payload_hash: &str,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<IdempotencyStart, OrchError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            match self.store.claim_idempotency(tool, request_id, payload_hash) {
                Ok(IdempotencyClaim::Perform) => {
                    return Ok(IdempotencyStart::Perform(IdempotencyLease {
                        store: self.store.clone(),
                        tool: tool.into(),
                        request_id: request_id.into(),
                        payload_hash: payload_hash.into(),
                        spec_key: None,
                        settled: false,
                    }));
                }
                Ok(IdempotencyClaim::Replay(Ok(value))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        None,
                        "replayed successful mutation outcome",
                    );
                    return Ok(IdempotencyStart::Replay(value));
                }
                Ok(IdempotencyClaim::Replay(Err(error))) => {
                    self.audit(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        "replayed",
                        Some(error.code.as_str()),
                        "replayed rejected mutation outcome",
                    );
                    return Err(error);
                }
                Ok(IdempotencyClaim::Pending) => {
                    if tokio::time::Instant::now() >= deadline {
                        let error = OrchError::new(
                            OrchErrorCode::Conflict,
                            "matching request_id is still in progress",
                        );
                        self.audit_err(
                            tool,
                            Some(request_id),
                            Some(session_id),
                            Some(&workspace.display().to_string()),
                            &error,
                        );
                        return Err(error);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => {
                    self.audit_err(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        &error,
                    );
                    return Err(error);
                }
            }
        }
    }

    fn fail_claim(
        &self,
        lease: &mut IdempotencyLease,
        run_id: Option<String>,
        session_id: Uuid,
        workspace: &Path,
        error: OrchError,
    ) -> OrchError {
        self.audit_err(
            &lease.tool,
            Some(&lease.request_id),
            Some(session_id),
            Some(&workspace.display().to_string()),
            &error,
        );
        lease.fail(run_id, error)
    }

    /// Load run and verify workspace ownership against allowlist + session.
    fn load_authorized_run(&self, run_id: &str) -> Result<RunRecord, OrchError> {
        if safe_id_filename(run_id).is_err() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "malformed run_id",
            ));
        }
        let run = self
            .store
            .load_run(run_id)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))?
            .ok_or_else(|| OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"))?;
        let allowlist = self.config.lock().allowlist.clone();
        let ws = PathBuf::from(&run.workspace);
        if !allowlist.contains(&ws) {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run workspace not authorized",
            ));
        }
        // Session must still match claimed workspace when present.
        if let Ok(session) = self.host.session_load(run.session_id) {
            if !session.cwd.is_empty() {
                let _ = require_workspace_match(&allowlist, Some(Path::new(&session.cwd)), &ws)
                    .map_err(|_| {
                        OrchError::new(
                            OrchErrorCode::ForbiddenScope,
                            "run session workspace mismatch",
                        )
                    })?;
            }
        }
        Ok(run)
    }

    // ── reads ──────────────────────────────────────────────────────────

    pub fn list_sessions(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let sessions = self.host.list_sessions_by_kind(SessionKind::Build, false);
        let rows: Vec<serde_json::Value> = sessions
            .into_iter()
            .filter(|s| {
                if s.cwd.is_empty() {
                    return false;
                }
                allowlist.contains(Path::new(&s.cwd))
            })
            .map(|s| {
                let busy = self.host.session_busy(s.id);
                json!({
                    "sessionId": s.id,
                    "title": s.title,
                    "kind": "build",
                    "cwd": s.cwd,
                    "workspaceStatus": s.workspace_status.as_str(),
                    "updatedAt": s.updated_at,
                    "busy": busy,
                })
            })
            .collect();
        Ok(json!({ "sessions": rows }))
    }

    /// List durable agent identities whose workspaces are visible to this
    /// authenticated control-plane instance. Checkpoint contents remain a
    /// scoped read so listing cannot become a transcript or workspace oracle.
    pub fn list_persistent_agents(
        &self,
        _auth: &AuthContext,
    ) -> Result<serde_json::Value, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let agents = self
            .host
            .list_persistent_agents()
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .into_iter()
            .filter(|agent| allowlist.contains(Path::new(&agent.workspace)))
            .collect::<Vec<_>>();
        Ok(json!({ "agents": agents }))
    }

    pub fn get_persistent_agent_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = self.authorize_persistent_agent_request(session_id, workspace, agent_id)?;
        let plan = self
            .host
            .prepare_agent_resume(session_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if plan.agent.agent_id != agent_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "persistent agent is not available in the requested scope",
            ));
        }
        serde_json::to_value(plan)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    /// Resume one verified persistent agent through the service adapter. The
    /// host owns the idempotency receipt and checkpoint validation; this layer
    /// adds workspace/session authorization and transport bounds.
    pub async fn resume_persistent_agent(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
        prompt: String,
        max_rounds: Option<u32>,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_resume_persistent_agent";
        let (agent, claimed) =
            match self.authorize_persistent_agent_request(session_id, workspace, agent_id) {
                Ok(value) => value,
                Err(error) => {
                    self.audit_err(
                        tool,
                        Some(request_id),
                        Some(session_id),
                        Some(&workspace.display().to_string()),
                        &error,
                    );
                    return Err(error);
                }
            };
        if let Err(error) = reject_control_prompt(&prompt) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                &error,
            );
            return Err(error);
        }
        let bounds = self.config.lock().bounds.clone();
        if prompt.len() > bounds.max_prompt_bytes {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&claimed.display().to_string()),
                &error,
            );
            return Err(error);
        }
        if let Some(rounds) = max_rounds {
            if rounds == 0 || rounds > bounds.max_rounds {
                let error = OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("max_rounds must be between 1 and {}", bounds.max_rounds),
                );
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        }
        let response = match self
            .host
            .resume_agent_with_request_id(session_id, prompt, max_rounds, Some(request_id.into()))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let error = OrchError::new(OrchErrorCode::Conflict, error.to_string());
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        let updated = self
            .host
            .get_persistent_agent(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .unwrap_or(agent);
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "persistent agent resumed",
        );
        Ok(json!({
            "agent": updated,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "response": response,
        }))
    }

    pub fn get_capacity(&self, _auth: &AuthContext) -> Result<serde_json::Value, OrchError> {
        let max = self.host.orchestration_capacity_limit();
        let active = self.host.orchestration_active_count();
        let queued = self.host.orchestration_pending_count();
        let event_error = self
            .bus
            .last_persistence_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let audit_error = self
            .store
            .last_audit_error()
            .map(|error| self.bus.redact_text(&error, 500));
        let run_error = self
            .store
            .last_run_error()
            .map(|error| self.bus.redact_text(&error, 500));
        Ok(json!({
            "maxConcurrentRuns": max,
            "activeRuns": active,
            "available": max.saturating_sub(active),
            "queuedRuns": queued,
            "queueLimit": MAX_PENDING_ADMISSIONS,
            "health": {
                "laggedLiveEvents": self.bus.lagged_event_count(),
                "eventJournalPersistenceError": event_error,
                "auditPersistenceError": audit_error,
                "runPersistenceError": run_error,
            },
        }))
    }

    pub fn get_run(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.run_value(self.load_authorized_run(run_id)?)
    }

    pub fn get_run_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.run_value(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn run_value(&self, mut run: RunRecord) -> Result<serde_json::Value, OrchError> {
        self.refresh_queue_position(&mut run);
        serde_json::to_value(run)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    pub fn get_progress(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.progress_value(self.load_authorized_run(run_id)?)
    }

    pub fn get_progress_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.progress_value(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn progress_value(&self, mut run: RunRecord) -> Result<serde_json::Value, OrchError> {
        self.refresh_queue_position(&mut run);
        let busy = self.host.session_busy(run.session_id);
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "queuePosition": run.queue_position,
            "busy": busy,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "promptPreview": run.prompt_preview,
            "progress": run.progress,
            "createdAt": run.created_at,
            "updatedAt": run.updated_at,
            "terminalResult": run.terminal_result,
            "errorCode": run.error_code,
        }))
    }

    fn refresh_queue_position(&self, run: &mut RunRecord) {
        run.queue_position = if run.state == RunState::Queued {
            self.host.orchestration_pending_position(&run.run_id)
        } else {
            None
        };
    }

    pub fn get_events(
        &self,
        _auth: &AuthContext,
        run_id: Option<&str>,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        // run_id is required — never fall back to the global journal.
        let rid = run_id.ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "run_id is required for get_events",
            )
        })?;
        let run = self.load_authorized_run(rid)?;
        self.events_for_run(run, after_seq, limit)
    }

    pub fn get_events_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        self.events_for_run(
            self.authorize_run_request(session_id, workspace, run_id)?,
            after_seq,
            limit,
        )
    }

    /// Authorize a run and return its current journal bounds plus an initial
    /// durable page for the optional Streamable HTTP live channel.
    pub(crate) fn live_run_page(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<(LiveRunScope, JournalPage), OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let Some(start_seq) = run.start_seq else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run has not started; use ptah_get_progress and open the live stream once running",
            ));
        };
        let scope = LiveRunScope {
            session_id: run.session_id,
            run_id: run.run_id.clone(),
            start_seq,
            end_seq: run.end_seq,
        };
        let page = self.events_page_for_run(run, after_seq, limit)?;
        Ok((scope, page))
    }

    pub(crate) fn subscribe_events(&self) -> EventReceiver {
        self.bus.subscribe()
    }

    // ── Computer Run reads (#271 slice 2) ──────────────────────────────
    //
    // Read-only projections of the durable Computer Run ledger. Mutations
    // deliberately remain absent from the control plane.

    /// Backend-free scoped reader over the host's shared Computer Run store.
    /// Availability is global and session-independent, so this failure leaks
    /// nothing about any run or session.
    fn computer_reads(&self) -> Result<crate::computer_use::ComputerRunReads, OrchError> {
        self.host
            .ensure_computer_store()
            .map(crate::computer_use::ComputerRunReads::new)
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::Unsupported,
                    "computer use is unavailable on this host",
                )
            })
    }

    /// Session + workspace gate shared by every Computer Run read. Computer
    /// Runs are owned by build and chat sessions alike, so this requires the
    /// session to exist and match the claimed allowlisted workspace — not to
    /// be a Build session.
    ///
    /// The claimed workspace is allowlisted first (session-independent).
    /// Unknown session, missing cwd, and cwd mismatch then collapse into the
    /// same `forbidden_scope` as an unauthorized run, so session existence
    /// is not distinguishable from cross-scope.
    fn authorize_computer_scope(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<String, OrchError> {
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = super::authz::canonical_workspace(workspace)?;
        if !allowlist.contains(&claimed) {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "workspace not in allowlist",
            ));
        }
        let session = self.host.session_load(session_id).ok();
        let cwd = session
            .as_ref()
            .and_then(|loaded| (!loaded.cwd.is_empty()).then(|| PathBuf::from(&loaded.cwd)));
        let Some(cwd) = cwd else {
            return Err(computer_scope_denied());
        };
        let session_cwd =
            super::authz::canonical_workspace(&cwd).map_err(|_| computer_scope_denied())?;
        if session_cwd != claimed {
            return Err(computer_scope_denied());
        }
        Ok(claimed.display().to_string())
    }

    pub fn list_computer_runs_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let runs = reads
            .list_run_projections(binding, Utc::now())
            .map_err(computer_read_error)?;
        Ok(json!({ "runs": runs }))
    }

    pub fn get_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let projection = reads
            .project_run(binding, run_id, Utc::now())
            .map_err(computer_read_error)?;
        serde_json::to_value(projection)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn get_computer_run_events_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let page = reads
            .run_events(binding, run_id, after_seq, limit)
            .map_err(computer_read_error)?;
        if page.cursor_expired {
            // Same 410 idiom as `ptah_get_events`, but the retained window
            // rides the error so recovery does not require a second get.
            return Err(OrchError::with_data(
                OrchErrorCode::CursorExpired,
                "computer run event cursor is below the retained window; resume from eventRange",
                json!({
                    "eventRange": page.range.map(|range| json!({
                        "startSeq": range.start_seq,
                        "endSeq": range.end_seq,
                    })),
                }),
            ));
        }
        serde_json::to_value(page)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    pub fn get_computer_capacity_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, &claimed);
        let capacity = reads.capacity(binding).map_err(computer_read_error)?;
        serde_json::to_value(capacity)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    // ── Computer Run mutations (#271 control slice) ───────────────────

    fn computer_controller(
        &self,
    ) -> Result<Arc<dyn crate::computer_use::ComputerRunController>, OrchError> {
        self.host.computer_run_controller().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Unsupported,
                "computer use control is unavailable on this host",
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn authorize_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        client: &crate::computer_use::ComputerClientIdentity,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        expected_version: u64,
        grant: crate::computer_use::ComputerGrantRequest,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let controller = self.computer_controller()?;
        controller
            .authorize(
                client,
                request_id,
                session_id,
                &claimed,
                run_id,
                expected_version,
                grant,
            )
            .await
            .map_err(computer_mutation_error)?;
        self.project_computer_run(session_id, &claimed, run_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn pause_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        client: &crate::computer_use::ComputerClientIdentity,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let controller = self.computer_controller()?;
        controller
            .pause(
                client,
                request_id,
                session_id,
                &claimed,
                run_id,
                expected_version,
            )
            .await
            .map_err(computer_mutation_error)?;
        self.project_computer_run(session_id, &claimed, run_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn take_over_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        client: &crate::computer_use::ComputerClientIdentity,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let controller = self.computer_controller()?;
        controller
            .take_over(
                client,
                request_id,
                session_id,
                &claimed,
                run_id,
                expected_version,
            )
            .await
            .map_err(computer_mutation_error)?;
        self.project_computer_run(session_id, &claimed, run_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn cancel_computer_run_scoped(
        &self,
        _auth: &AuthContext,
        client: &crate::computer_use::ComputerClientIdentity,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_computer_scope(session_id, workspace)?;
        let controller = self.computer_controller()?;
        controller
            .cancel(
                client,
                request_id,
                session_id,
                &claimed,
                run_id,
                expected_version,
            )
            .await
            .map_err(computer_mutation_error)?;
        self.project_computer_run(session_id, &claimed, run_id)
    }

    fn project_computer_run(
        &self,
        session_id: Uuid,
        claimed_workspace: &str,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let reads = self.computer_reads()?;
        let binding = crate::computer_use::ComputerReadBinding::new(session_id, claimed_workspace);
        serde_json::to_value(
            reads
                .project_run(binding, run_id, Utc::now())
                .map_err(computer_read_error)?,
        )
        .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
    }

    fn events_for_run(
        &self,
        run: RunRecord,
        after_seq: u64,
        limit: usize,
    ) -> Result<serde_json::Value, OrchError> {
        serde_json::to_value(self.events_page_for_run(run, after_seq, limit)?)
            .map_err(|e| OrchError::new(OrchErrorCode::Internal, e.to_string()))
    }

    fn events_page_for_run(
        &self,
        run: RunRecord,
        after_seq: u64,
        limit: usize,
    ) -> Result<JournalPage, OrchError> {
        // Read the bounded run range before applying the caller's page limit.
        // Applying `limit` to the global journal first can return a page made
        // entirely of other sessions and advance the cursor past this run's
        // events. `read_range_all` is bounded by the journal retention policy
        // and preserves cursor-expiry failures instead of silently skipping.
        let mut entries = self
            .bus
            .read_range_all(after_seq, run.end_seq, Some(run.session_id))
            .map_err(|_| {
                OrchError::new(
                    OrchErrorCode::CursorExpired,
                    "event cursor expired; restart from seq 0 or latest",
                )
            })?;
        entries.retain(|e| {
            run.start_seq.map(|s| e.seq >= s).unwrap_or(true)
                && run.end_seq.map(|s| e.seq <= s).unwrap_or(true)
        });
        entries.truncate(limit.clamp(1, 500));
        let next_cursor = entries.last().map(|e| e.seq);
        Ok(JournalPage {
            entries,
            next_cursor,
            cursor_expired: false,
        })
    }

    pub fn get_changes(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.changes_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_changes_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.changes_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn changes_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        // Prefer durable aggregates (survive journal rollover).
        let mut paths: Vec<serde_json::Value> = run
            .aggregates
            .changes
            .iter()
            .map(|c| json!({ "path": c.path, "summary": c.summary }))
            .collect();
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                if let crate::events::SessionUpdate::FileEdit { path, summary, .. } = e.update {
                    if !paths.iter().any(|p| p["path"] == path) {
                        paths.push(json!({ "path": path, "summary": summary }));
                    }
                }
            }
        }
        Ok(json!({ "runId": run.run_id, "changes": paths }))
    }

    pub fn get_test_results(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.test_results_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_test_results_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.test_results_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn test_results_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        let mut by_id: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        // Seed from durable aggregates.
        for t in &run.aggregates.tests {
            by_id.insert(
                t.call_id.clone(),
                json!({
                    "callId": t.call_id,
                    "command": t.command,
                    "status": t.status,
                    "exitCode": t.exit_code,
                    "cancelled": t.cancelled,
                }),
            );
        }
        if let Ok(entries) = self.scoped_events_complete(&run) {
            for e in entries {
                match e.update {
                    crate::events::SessionUpdate::ShellSessionStarted {
                        command, call_id, ..
                    } => {
                        if is_recognized_test_command(&command) {
                            by_id.insert(
                                call_id.clone(),
                                json!({
                                    "callId": call_id,
                                    "command": command,
                                    "status": "started",
                                }),
                            );
                        }
                    }
                    crate::events::SessionUpdate::ShellSessionEnded {
                        call_id,
                        exit_code,
                        cancelled,
                        ..
                    } => {
                        if let Some(prev) = by_id.get_mut(&call_id) {
                            prev["status"] = json!("ended");
                            prev["exitCode"] = json!(exit_code);
                            prev["cancelled"] = json!(cancelled);
                        }
                        // Do NOT record non-test shell ends.
                    }
                    _ => {}
                }
            }
        }
        let observed: Vec<_> = by_id.into_values().collect();
        if observed.is_empty() {
            Ok(json!({
                "runId": run.run_id,
                "status": "not_observed",
                "results": [],
            }))
        } else {
            Ok(json!({
                "runId": run.run_id,
                "status": "observed",
                "results": observed,
            }))
        }
    }

    pub fn get_handoff(
        &self,
        _auth: &AuthContext,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.handoff_for_run(self.load_authorized_run(run_id)?)
    }

    pub fn get_handoff_scoped(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        self.handoff_for_run(self.authorize_run_request(session_id, workspace, run_id)?)
    }

    fn handoff_for_run(&self, run: RunRecord) -> Result<serde_json::Value, OrchError> {
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "state": run.state,
            "finalResponse": run.final_response,
            "terminalResult": run.terminal_result,
            "startSeq": run.start_seq,
            "endSeq": run.end_seq,
            "changes": run.aggregates.changes,
            "tests": run.aggregates.tests,
            "verification": run.aggregates.verification,
            "usage": run.aggregates.usage,
        }))
    }

    fn scoped_events_complete(
        &self,
        run: &RunRecord,
    ) -> Result<Vec<crate::event_bus::JournalEntry>, OrchError> {
        let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
        match self
            .bus
            .read_range_all(after, run.end_seq, Some(run.session_id))
        {
            Ok(v) => Ok(v),
            Err(CursorExpiredError) => Err(OrchError::new(
                OrchErrorCode::CursorExpired,
                "event cursor expired for run range",
            )),
        }
    }

    fn require_build_session(
        &self,
        session_id: Uuid,
    ) -> Result<crate::session::SessionSummary, OrchError> {
        let session = self
            .host
            .session_load(session_id)
            .map_err(|_| OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"))?;
        if session.kind != SessionKind::Build {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "only Build sessions are controllable in this slice",
            ));
        }
        if session.workspace_status != WorkspaceStatus::Ready {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                format!(
                    "session workspace is {}: choose a working directory before controlling it",
                    session.workspace_status.as_str()
                ),
            ));
        }
        Ok(session)
    }

    fn authorize_run_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<RunRecord, OrchError> {
        let run = self.load_authorized_run(run_id)?;
        if run.session_id != session_id {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "run does not belong to the requested session",
            ));
        }
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        if claimed.display().to_string() != run.workspace {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "run workspace does not match the requested workspace",
            ));
        }
        Ok(run)
    }

    fn authorize_persistent_agent_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
        agent_id: &str,
    ) -> Result<(AgentRecord, PathBuf), OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = require_workspace_match(&allowlist, cwd.as_deref(), workspace)?;
        let agent = self
            .host
            .get_persistent_agent(agent_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?
            .ok_or_else(|| {
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "persistent agent is not available in the requested scope",
                )
            })?;
        let agent_workspace = canonical_workspace(Path::new(&agent.workspace))?;
        if agent.session_id != session_id || agent_workspace != claimed {
            return Err(OrchError::new(
                OrchErrorCode::ForbiddenScope,
                "persistent agent is not available in the requested scope",
            ));
        }
        Ok((agent, claimed))
    }

    fn authorize_queue_request(
        &self,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<PathBuf, OrchError> {
        let session = self.require_build_session(session_id)?;
        let cwd = (!session.cwd.is_empty()).then(|| PathBuf::from(&session.cwd));
        let allowlist = self.config.lock().allowlist.clone();
        require_workspace_match(&allowlist, cwd.as_deref(), workspace)
    }

    async fn begin_queue_mutation(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        payload: &serde_json::Value,
    ) -> Result<(PathBuf, IdempotencyStart), OrchError> {
        let claimed = match self.authorize_queue_request(session_id, workspace) {
            Ok(path) => path,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&workspace.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        let payload_hash = hash_payload(payload);
        let start = match self
            .begin_idempotency(tool, request_id, &payload_hash, session_id, &claimed)
            .await
        {
            Ok(start) => start,
            Err(error) => {
                self.audit_err(
                    tool,
                    Some(request_id),
                    Some(session_id),
                    Some(&claimed.display().to_string()),
                    &error,
                );
                return Err(error);
            }
        };
        Ok((claimed, start))
    }

    fn queue_error(error: anyhow::Error) -> OrchError {
        let message = error.to_string();
        let code = if message.contains("stale queued prompt version")
            || message.contains("stale prompt queue revision")
        {
            OrchErrorCode::StaleVersion
        } else if message.contains("unknown queued prompt")
            || message.contains("no prompt queue for session")
        {
            OrchErrorCode::InvalidRequest
        } else {
            OrchErrorCode::Internal
        };
        OrchError::new(code, message)
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_response(
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        action: &str,
        entries: Vec<PromptQueueEntry>,
        changed_entry: Option<PromptQueueEntry>,
        disposition: Option<SteeringDisposition>,
        revision: u64,
    ) -> serde_json::Value {
        json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "origin": "mcp",
            "action": action,
            "disposition": disposition,
            "actionVersion": changed_entry.as_ref().map(|entry| entry.version),
            // The queue revision this mutation produced. Reorder is fenced on
            // it, so a coordinator that had to re-read the queue after every
            // other verb could never chain a mutation into a reorder without a
            // window for someone else to move first. Every receipt now carries
            // the revision its own mutation stamped.
            "revision": revision,
            "entry": changed_entry,
            "entries": entries,
        })
    }

    pub fn get_queue(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let claimed = self.authorize_queue_request(session_id, workspace)?;
        let snapshot = self
            .host
            .session_queue_snapshot(session_id)
            .map_err(Self::queue_error)?;
        Ok(json!({
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "revision": snapshot.revision,
            "entries": snapshot.entries,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn edit_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        version: u64,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_edit_queue";
        if let Err(error) = reject_control_prompt(&text) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            return Err(error);
        }
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "version": version,
            "text": text,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, revision) = match self
            .host
            .session_queue_edit_with_origin(session_id, entry_id, version, text, "mcp")
        {
            Ok(entries) => entries,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let changed_entry = entries.iter().find(|entry| entry.id == entry_id).cloned();
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "edited",
            entries,
            changed_entry,
            None,
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue edited",
        );
        Ok(response)
    }

    pub async fn remove_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_remove_queue";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, changed_entry, revision) = match self
            .host
            .session_queue_remove_with_origin_receipt(session_id, entry_id, "mcp", expected_version)
        {
            Ok(entries) => entries,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "removed",
            entries,
            Some(changed_entry),
            None,
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue entry removed",
        );
        Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn reorder_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        to_index: usize,
        expected_version: u64,
        expected_revision: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_reorder_queue";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "toIndex": to_index,
            "expectedVersion": expected_version,
            "expectedRevision": expected_revision,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, revision) = match self.host.session_queue_move_with_origin_and_revision(
            session_id,
            entry_id,
            to_index,
            "mcp",
            expected_version,
            expected_revision,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let changed_entry = entries.iter().find(|entry| entry.id == entry_id).cloned();
        let mut response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "reordered",
            entries,
            changed_entry,
            None,
            revision,
        );
        response["revision"] = json!(revision);
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue reordered",
        );
        Ok(response)
    }

    pub async fn clear_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_clear_queue";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (entries, outcome, revision) = match self
            .host
            .session_queue_clear_with_origin_receipt(session_id, "mcp")
        {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let mut response = Self::queue_response(
            request_id, session_id, &claimed, "cleared", entries, None, None, revision,
        );
        // An empty `entries` list alone would be a fail-open receipt: steering
        // already handed to a model boundary cannot be retracted and will
        // still be injected. Report it rather than implying the session is
        // quiet. `stopped` is the field a coordinator should branch on.
        if let Some(object) = response.as_object_mut() {
            object.insert("clearedQueued".into(), json!(outcome.queued_cleared));
            object.insert(
                "steeringCancelled".into(),
                json!(outcome.steering_cancelled),
            );
            object.insert("steeringInFlight".into(), json!(outcome.steering_in_flight));
            object.insert("stopped".into(), json!(outcome.fully_stopped()));
        }
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue cleared",
        );
        Ok(response)
    }

    /// S7: the control plane must not be able to *schedule* a prompt it is
    /// forbidden from *creating*.
    ///
    /// `reject_control_prompt` blocks `!` and `/` prompts on every path that
    /// authors text, but selection verbs took an entry id and never looked at
    /// what they were selecting. A locally authored `/yolo` or `!rm ...` could
    /// therefore be promoted to the head of the queue, and `run_next` would
    /// cancel the active turn to make it run — the forbidden outcome reached
    /// by choosing instead of by writing. Selection is now held to the same
    /// policy as authorship, evaluated against the stored text.
    ///
    /// Reading the entry before claiming the mutation is safe against edits in
    /// the gap: changing the text bumps the entry version, so the caller's
    /// `expected_version` fails closed.
    fn reject_selecting_control_entry(
        &self,
        tool: &str,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
    ) -> Result<(), OrchError> {
        // Authorize before reading. This runs ahead of `begin_queue_mutation`,
        // which does its own authorization, so without this an unscoped caller
        // could learn something about another workspace's queue from whether
        // the policy rejected it.
        self.authorize_queue_request(session_id, workspace)?;
        let entries = self.host.session_queue_list(session_id).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("queue unavailable: {error}"),
            )
        })?;
        let Some(entry) = entries.into_iter().find(|entry| entry.id == entry_id) else {
            // Leave "unknown entry" to the mutator, so the not-found contract
            // stays in one place.
            return Ok(());
        };
        if let Err(error) = reject_control_prompt(&entry.text) {
            self.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            return Err(error);
        }
        Ok(())
    }

    pub async fn run_next_queue(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_run_next";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (result, revision) = match self.host.session_queue_run_next_with_origin(
            session_id,
            entry_id,
            "mcp",
            expected_version,
        ) {
            Ok(result) => result,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        // `run_next` removes the entry from the durable queue, so the host
        // returns the changed entry separately from the post-action snapshot.
        let changed_entry = Some(result.changed_entry.clone());
        let mut response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "run_next",
            result.entries,
            changed_entry,
            None,
            revision,
        );
        response["cancelledActive"] = json!(result.cancelled_active);
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queue entry promoted to run next",
        );
        Ok(response)
    }

    pub async fn steer_queued(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        entry_id: &str,
        expected_version: u64,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_steer_queued";
        self.reject_selecting_control_entry(tool, request_id, session_id, workspace, entry_id)?;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "entryId": entry_id,
            "expectedVersion": expected_version,
        });
        let (claimed, start) = self
            .begin_queue_mutation(tool, request_id, session_id, workspace, &payload)
            .await?;
        let mut lease = match start {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let (receipt, revision) = match self.host.session_queue_steer_entry_with_origin(
            session_id,
            entry_id,
            "mcp",
            expected_version,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    None,
                    session_id,
                    &claimed,
                    Self::queue_error(error),
                ));
            }
        };
        let response = Self::queue_response(
            request_id,
            session_id,
            &claimed,
            "steer_now",
            receipt.entries,
            Some(receipt.entry),
            Some(receipt.disposition),
            revision,
        );
        if let Err(error) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, error));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued entry steered without cancelling",
        );
        Ok(response)
    }

    fn isolated_review(
        &self,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<(RunRecord, crate::run_promotion::RunReview), OrchError> {
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if run.state != RunState::Completed {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only completed runs can be reviewed",
            ));
        }
        let Some(execution) = run.execution.as_ref() else {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run used shared execution and has no isolated diff",
            ));
        };
        if execution.mode != RunExecutionMode::IsolatedWorktree
            || execution.promotion_state != PromotionState::Ready
        {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated run is not ready for review",
            ));
        }
        let review = self
            .host
            .inspect_run(session_id, run_id)
            .map_err(|error| OrchError::new(OrchErrorCode::Conflict, error.to_string()))?;
        if execution.final_fingerprint.as_deref() != Some(review.fingerprint.as_str()) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "isolated worktree fingerprint changed; review is stale",
            ));
        }
        Ok((run, review))
    }

    pub fn review_run(
        &self,
        _auth: &AuthContext,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let (run, review) = self.isolated_review(session_id, workspace, run_id)?;
        Ok(json!({
            "runId": run.run_id,
            "sessionId": run.session_id,
            "sourceFingerprint": run.execution.as_ref().map(|e| e.source_fingerprint.clone()),
            "finalFingerprint": review.fingerprint,
            "changedFiles": review.changed_files,
            "diff": review.diff,
            "diffTruncated": review.diff_truncated,
            "promotionState": run.execution.as_ref().map(|e| e.promotion_state),
        }))
    }

    #[allow(clippy::too_many_arguments)] // Keeps the approval scope explicit at the control boundary.
    pub async fn approve_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        source_fingerprint: String,
        final_fingerprint: String,
        changed_files: Vec<ChangeRecord>,
        ttl_ms: Option<u64>,
    ) -> Result<serde_json::Value, OrchError> {
        const DEFAULT_TTL_MS: u64 = 5 * 60 * 1_000;
        const MAX_TTL_MS: u64 = 15 * 60 * 1_000;
        let tool = "ptah_approve_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "sourceFingerprint": source_fingerprint,
            "finalFingerprint": final_fingerprint,
            "changedFiles": changed_files,
            "ttlMs": ttl_ms,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };
        let ttl = ttl_ms.unwrap_or(DEFAULT_TTL_MS);
        if ttl == 0 || ttl > MAX_TTL_MS {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    "ttl_ms must be between 1 and 900000",
                ),
            ));
        }
        let run = match self.authorize_run_request(session_id, workspace, run_id) {
            Ok(run) => run,
            Err(error) => return Err(fail(self, error)),
        };
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await
        {
            Ok(IdempotencyStart::Replay(value)) => return Ok(value),
            Ok(IdempotencyStart::Perform(lease)) => lease,
            Err(error) => return Err(error),
        };
        let (run, review) = match self.isolated_review(session_id, workspace, run_id) {
            Ok(value) => value,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ))
            }
        };
        let Some(execution) = run.execution.as_ref() else {
            unreachable!("isolated_review guarantees execution");
        };
        if source_fingerprint != execution.source_fingerprint
            || final_fingerprint != review.fingerprint
            || changed_files != review.changed_files
        {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "approval scope does not match the current reviewed diff",
                ),
            ));
        }
        if let Some(existing) = run.approval.as_ref() {
            if existing.expires_at > Utc::now() {
                let error = OrchError::new(
                    OrchErrorCode::Conflict,
                    "an unexpired approval already exists for this run",
                );
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    error,
                ));
            }
        }
        let issued_at = Utc::now();
        let approval = RunApproval {
            approval_id: Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            session_id,
            workspace: run.workspace.clone(),
            source_fingerprint,
            final_fingerprint,
            changed_files,
            issued_at,
            expires_at: issued_at + chrono::Duration::milliseconds(ttl as i64),
        };
        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "approvalId": approval.approval_id,
            "expiresAt": approval.expires_at,
            "sourceFingerprint": approval.source_fingerprint,
            "finalFingerprint": approval.final_fingerprint,
            "changedFiles": approval.changed_files,
        });
        let updated = self.store.update_run(run_id, |current| {
            current.approval = Some(approval.clone());
            current.updated_at = Utc::now();
            Ok(())
        });
        let updated = match updated {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(anyhow::anyhow!("run disappeared while approving")),
            Err(error) => Err(error),
        };
        if let Err(error) = updated {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                OrchError::new(OrchErrorCode::Internal, error.to_string()),
            ));
        }
        if let Err(error) = lease.complete(Some(run_id.to_string()), response.clone()) {
            return Err(self.fail_claim(
                &mut lease,
                Some(run_id.to_string()),
                session_id,
                Path::new(&run.workspace),
                error,
            ));
        }
        Ok(response)
    }

    pub async fn promote_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
        approval_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_promote_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
            "approvalId": approval_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let promoted =
            match self
                .host
                .promote_run_with_approval(session_id, run_id, Some(approval_id))
            {
                Ok(run) => run,
                Err(error) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id.to_string()),
                        session_id,
                        Path::new(&run.workspace),
                        OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                    ))
                }
            };
        let response = serde_json::to_value(promoted)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    pub async fn discard_run(
        &self,
        _auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_discard_run";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let run = self.authorize_run_request(session_id, workspace, run_id)?;
        if !run.state.is_terminal() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "only terminal runs can be discarded",
            ));
        }
        let mut lease = match self
            .begin_idempotency(
                tool,
                request_id,
                &phash,
                session_id,
                Path::new(&run.workspace),
            )
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        let discarded = match self.host.discard_run(session_id, run_id) {
            Ok(run) => run,
            Err(error) => {
                return Err(self.fail_claim(
                    &mut lease,
                    Some(run_id.to_string()),
                    session_id,
                    Path::new(&run.workspace),
                    OrchError::new(OrchErrorCode::Conflict, error.to_string()),
                ))
            }
        };
        let response = serde_json::to_value(discarded)
            .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
        lease.complete(Some(run_id.to_string()), response.clone())?;
        Ok(response)
    }

    // ── mutations ──────────────────────────────────────────────────────

    pub async fn submit_task(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            RunExecutionMode::Shared,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // Keeps bounded submission policy explicit at the control boundary.
    pub async fn submit_task_with_execution_mode(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode_and_queue(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            execution_mode,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn submit_task_with_execution_mode_and_queue(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
    ) -> Result<serde_json::Value, OrchError> {
        self.submit_task_with_execution_mode_and_queue_parent(
            auth,
            request_id,
            session_id,
            workspace,
            prompt,
            bounds_json,
            execution_mode,
            allow_queue,
            None,
            "ptah_submit_task",
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn submit_task_with_execution_mode_and_queue_parent(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: RunExecutionMode,
        allow_queue: bool,
        retry_of: Option<&str>,
        idempotency_tool: &str,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = idempotency_tool;
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "bounds": bounds_json,
            "executionMode": execution_mode,
            "allowQueue": allow_queue,
            "retryOf": retry_of,
        });
        let phash = hash_payload(&payload);

        let finish_err = |svc: &Self, e: OrchError| -> OrchError {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(finish_err(self, e));
        }
        let ceiling = self.config.lock().bounds.clone();
        let bounds = match merge_bounds(&ceiling, bounds_json.as_ref()) {
            Ok(b) => b,
            Err(e) => return Err(finish_err(self, e)),
        };
        if prompt.len() > bounds.max_prompt_bytes {
            let e = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!(
                    "prompt exceeds max_prompt_bytes ({})",
                    bounds.max_prompt_bytes
                ),
            );
            return Err(finish_err(self, e));
        }

        let session = match self.require_build_session(session_id) {
            Ok(s) => s,
            Err(e) => return Err(finish_err(self, e)),
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(finish_err(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        // Give older queued work first claim on any newly available capacity.
        self.pump_pending();
        let run_id = Uuid::new_v4().to_string();
        let queue_ahead = self.host.orchestration_pending_count() > 0;
        let mut queued = false;
        if allow_queue && queue_ahead {
            queued = true;
        } else if let Err(e) = self.try_reserve_capacity(&run_id, session_id) {
            if allow_queue
                && matches!(
                    e.code,
                    OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted
                )
            {
                queued = true;
            } else {
                return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
            }
        }

        // The authorization that admits this work, captured as fingerprints
        // and sealed into the specification, so action time can re-answer the
        // same question instead of trusting this moment forever.
        let authorization =
            self.authorization_snapshot(&auth.token_id, &session, session.agent_id.as_deref());

        // ── crash-safe cut C2: private bounded input, sealed ───────────
        //
        // Every accepted task — immediate execution included — gets its
        // durable input written and fsynced here, *before* the receipt can
        // say "accepted". A crash from this point on can only produce
        // "tombstoned, never ran" or "ran exactly once".
        let intent = AcceptanceIntent {
            intent_version: ACCEPTANCE_INTENT_VERSION,
            run_id: run_id.clone(),
            request_id: request_id.into(),
            payload_hash: phash.clone(),
            tool: tool.into(),
            session_id,
            session_revision: authorization.session_revision.clone(),
            workspace: claimed.display().to_string(),
            workspace_revision: authorization.workspace_revision.clone(),
            agent_id: session.agent_id.clone(),
            agent_revision: authorization.agent_revision,
            spec_revision: EXECUTION_SPEC_REVISION.into(),
            principal_token_id: auth.token_id.clone(),
            principal_revision: authorization.principal_revision.clone(),
            policy_revision: authorization.policy_revision.clone(),
            route_revision: authorization.route_revision.clone(),
            prompt: prompt.clone(),
            bounds: SealedBounds::from(&bounds),
            execution_mode,
            allow_queue,
            retry_of: retry_of.map(str::to_string),
            parent_run_id: None,
            created_at: Utc::now(),
            digest: String::new(),
            seal: super::seal::SealStamp::unsealed(),
        }
        .seal_with(self.store.seal_authority())?;
        if let Err(e) = self.store.save_acceptance_intent(&intent) {
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            // Nothing durable claims this run yet; fail the admission for good.
            let _ = self.store.remove_acceptance_intent(&run_id);
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }

        // ── crash-safe cut C3: the run record ──────────────────────────
        //
        // Always `Queued`, for every accepted task — immediate execution
        // included. The receipt is issued from here, and at this point nothing
        // has started: no handle is registered, no worker has acknowledged,
        // and no byte has reached a provider. Reporting `running` would be a
        // claim about the future rather than a record of the present, and a
        // crash one instruction later would make it false forever.
        //
        // The run reaches `running` as the worker's own first durable act,
        // once the start gate has opened and every handle is registered.
        let reported_state = RunState::Queued;
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id,
            workspace: claimed.display().to_string(),
            request_id: request_id.into(),
            // Distinguish coordinator-owned work from desktop turns so the
            // desktop can surface external activity without guessing from
            // transport timing.
            client_id: Some("mcp".into()),
            state: RunState::Queued,
            agent_id: None,
            retry_of: retry_of.map(str::to_string),
            parent_run_id: None,
            queue_position: None,
            spec_key: Some(intent.spec_key().to_string()),
            bounds: bounds.clone(),
            prompt_preview: self.bus.redact_text(&prompt_preview(&prompt), 500),
            start_seq: None,
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        if let Err(e) = self.store.save_run(&run) {
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            let _ = self.store.remove_acceptance_intent(&run_id);
            let e = OrchError::new(OrchErrorCode::Internal, e.to_string());
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }

        let queued_position = if queued {
            match self.enqueue_pending(PendingRun {
                run_id: run_id.clone(),
                session_id,
            }) {
                Ok(position) => Some(position),
                Err(error) => {
                    self.tombstone_admission(&run_id, error.code.as_str());
                    return Err(self.fail_claim(
                        &mut lease,
                        Some(run_id),
                        session_id,
                        &claimed,
                        error,
                    ));
                }
            }
        } else {
            None
        };

        let response = json!({
            "runId": run_id,
            "sessionId": session_id,
            "state": reported_state,
            "requestId": request_id,
            "executionMode": execution_mode,
            "queuedPosition": queued_position,
        });

        // ── crash-safe cut C4: the receipt ─────────────────────────────
        //
        // The receipt records the same specification key the run and the input
        // already carry, so a receipt can never be replayed against different
        // work than the one it admitted.
        lease.spec_key = Some(intent.spec_key().to_string());
        //
        // Only now may the caller be told the work is accepted, because the
        // input behind that promise is already durable.
        if let Err(e) = lease.complete(Some(run_id.clone()), response.clone()) {
            // The promise was never made, so the admission is failed for good
            // and its input destroyed: it can never later recover or execute.
            self.remove_pending(&run_id);
            if !queued {
                self.host.release_turn_reservation(session_id, &run_id);
                self.release_capacity(&run_id);
            }
            self.tombstone_admission(&run_id, "receipt_persistence_failed");
            return Err(self.fail_claim(&mut lease, Some(run_id), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            // Never "started": at receipt time no lease is held and no worker
            // exists. Dispatch audits itself separately, after both are true.
            if queued {
                "run queued"
            } else {
                "run accepted for immediate dispatch"
            },
        );

        if queued {
            // A capacity release can race the enqueue; this also makes an
            // immediately available slot visible without requiring polling.
            self.pump_pending();
            return Ok(response);
        }

        // ── crash-safe cut C5: mandatory attempt lease before dispatch ──
        //
        // Cut C6 (`queued -> running`) now belongs to the worker, not to this
        // path: the transition *is* the acknowledgement, so it can only be
        // written by the thing that actually started.
        match self.acquire_dispatch_lease(&run, &intent) {
            Ok(attempt) => self.dispatch_attempt(run, intent, attempt),
            Err(error) => {
                // The receipt is already durable, so this admission must still
                // happen: fall back to the bounded queue instead of losing it.
                eprintln!(
                    "[grokptah] accepted run {run_id} deferred to the queue: {}",
                    error.message
                );
                self.defer_accepted_run(&run_id, session_id);
            }
        }

        Ok(response)
    }

    /// Hand an already-accepted run back to the bounded queue.
    ///
    /// Used when dispatch cannot proceed *after* the receipt is durable. The
    /// run keeps its sealed input and stays `Queued`, so it is executed later
    /// by the pump or by restart recovery — exactly once, never zero times.
    fn defer_accepted_run(&self, run_id: &str, session_id: Uuid) {
        let _ = self.store.update_run(run_id, |current| {
            if current.state == RunState::Running {
                current.state = RunState::Queued;
                current.start_seq = None;
                current.updated_at = Utc::now();
            }
            Ok(())
        });
        self.host.release_turn_reservation(session_id, run_id);
        self.host.release_orchestration_turn(run_id);
        if let Err(error) = self.enqueue_pending(PendingRun {
            run_id: run_id.to_string(),
            session_id,
        }) {
            eprintln!(
                "[grokptah] accepted run {run_id} could not be re-queued: {}",
                error.message
            );
        }
        self.pump_pending();
    }

    /// Explicitly create a bounded replacement for one interrupted run.
    /// Restart recovery never resumes a model turn implicitly; the caller
    /// supplies a fresh prompt and the new request is idempotent on its own.
    #[allow(clippy::too_many_arguments)]
    pub async fn retry_run(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        source_run_id: &str,
        prompt: String,
        bounds_json: Option<serde_json::Value>,
        execution_mode: Option<RunExecutionMode>,
        allow_queue: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let tool = "ptah_retry_run";
        let fail = |svc: &Self, error: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &error,
            );
            error
        };
        let source = match self.authorize_run_request(session_id, workspace, source_run_id) {
            Ok(run) => run,
            Err(error) => return Err(fail(self, error)),
        };
        if source.state != RunState::Interrupted {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "only interrupted runs can be explicitly retried",
                ),
            ));
        }

        let previous_mode = source
            .execution
            .as_ref()
            .map(|execution| execution.mode)
            .unwrap_or(RunExecutionMode::Shared);
        if let Some(requested_mode) = execution_mode {
            if requested_mode != previous_mode {
                return Err(fail(
                    self,
                    OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "a linked retry must preserve the interrupted run execution mode",
                    ),
                ));
            }
        }
        let server_bounds = self.config.lock().bounds.clone();
        if source.bounds.max_prompt_bytes > server_bounds.max_prompt_bytes
            || source.bounds.max_rounds > server_bounds.max_rounds
            || source.bounds.max_duration_ms > server_bounds.max_duration_ms
        {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Conflict,
                    "interrupted run exceeds the current retry policy ceiling",
                ),
            ));
        }
        let bounds = match bounds_json {
            Some(value) => {
                let retry_bounds = merge_bounds(&source.bounds, Some(&value))
                    .map_err(|error| fail(self, error))?;
                Some(serde_json::to_value(retry_bounds).map_err(|error| {
                    fail(
                        self,
                        OrchError::new(OrchErrorCode::Internal, error.to_string()),
                    )
                })?)
            }
            None => Some(serde_json::to_value(&source.bounds).map_err(|error| {
                fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, error.to_string()),
                )
            })?),
        };
        let response = self
            .submit_task_with_execution_mode_and_queue_parent(
                auth,
                request_id,
                session_id,
                workspace,
                prompt,
                bounds,
                previous_mode,
                allow_queue,
                Some(source_run_id),
                tool,
            )
            .await
            .map_err(|error| fail(self, error))?;
        if response["runId"].as_str().is_none() {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::Internal,
                    "retry submission returned no run_id",
                ),
            ));
        }
        let mut response = response;
        response["sourceRunId"] = json!(source_run_id);
        response["retryOf"] = json!(source_run_id);
        response["requestId"] = json!(request_id);
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&source.workspace),
            "accepted",
            None,
            "explicit replacement created for interrupted run",
        );
        Ok(response)
    }

    /// Start a run whose host admission has already been reserved.
    /// Dispatch one authorized attempt.
    ///
    /// Three tasks make up an attempt — the *worker* (the model turn), the
    /// *aggregator* (journal fold), and the *supervisor* (deadline, heartbeat,
    /// terminalization). All three are created behind one **closed start
    /// gate**, every handle is registered in a single registry mutation, and
    /// only then is the gate opened. Nothing can therefore run before teardown
    /// is able to find and abort it.
    ///
    /// The durable input is **not** removed here. It survives until the
    /// teardown owner has proved the attempt is quiescent, so a crash between
    /// "spawn confirmed" and "turn finished" still leaves exactly one
    /// recoverable copy of the accepted work.
    fn dispatch_attempt(&self, run: RunRecord, intent: AcceptanceIntent, lease: AttemptLease) {
        if tokio::runtime::Handle::try_current().is_err() {
            // No runtime to own the worker. Leave the record `Queued` with its
            // durable input intact rather than losing the admission.
            let _ =
                self.store
                    .release_attempt_lease(&run.run_id, &lease.attempt_id, &self.owner_id);
            self.host.release_orchestration_turn(&run.run_id);
            return;
        }

        let host = self.host.clone();
        let store = self.store.clone();
        let bus = self.bus.clone();
        let service_ref = self.self_ref.clone();
        let owner_id = self.owner_id.clone();
        let session_id = run.session_id;
        let rid = run.run_id.clone();
        let max_ms = run.bounds.max_duration_ms;
        let max_rounds = run.bounds.max_rounds;
        let prompt = intent.prompt.clone();
        let execution_mode = intent.execution_mode;

        let cancel = CancellationToken::new();
        let liveness = Arc::new(WorkerLiveness::default());
        let entry = Arc::new(LiveWorker::new(
            rid.clone(),
            lease.attempt_id.clone(),
            lease.attempt,
            session_id,
            cancel.clone(),
            liveness.clone(),
        ));

        // ── one active attempt ─────────────────────────────────────────
        //
        // Reserving is not publishing. A *reservation* claims the run id so a
        // second dispatch cannot interleave; the entry itself is published
        // only once every handle exists, because a published entry with
        // missing handles is one that teardown cannot fully abort.
        if !self.reserve_dispatch(&rid) {
            eprintln!("[grokptah] refusing a second live attempt for run {rid}");
            let _ = self
                .store
                .release_attempt_lease(&rid, &lease.attempt_id, &owner_id);
            return;
        }
        self.remember_liveness(&rid, liveness.clone());

        // ── the closed, cancel-aware start gate ────────────────────────
        let gate = StartGate::new();
        entry.attach_gate(gate.clone());

        // ── nested aggregator ──────────────────────────────────────────
        let mut agg_rx = bus.subscribe();
        let store_agg = store.clone();
        let rid_agg = rid.clone();
        let gate_agg = gate.clone();
        let agg_task = tokio::spawn(async move {
            if gate_agg.wait().await == GateOutcome::Abandoned {
                return;
            }
            while let Some(update) = agg_rx.recv().await {
                apply_run_aggregate(&store_agg, &rid_agg, session_id, &update);
            }
        });
        let agg_task_abort = agg_task.abort_handle();

        // ── nested worker ──────────────────────────────────────────────
        // The liveness guard lives *inside* the worker future, so it drops
        // when the future actually ends — including on abort. That is the only
        // signal that proves the work is gone; ledger state is not evidence.
        let host_worker = host.clone();
        let rid_worker = rid.clone();
        let liveness_worker = liveness.clone();
        let store_worker = store.clone();
        let service_worker = service_ref.clone();
        let gate_worker = gate.clone();
        let attempt_id_worker = lease.attempt_id.clone();
        let spec_worker = intent.clone();
        let worker = tokio::spawn(async move {
            if gate_worker.wait().await == GateOutcome::Abandoned {
                // Cancelled during the registration gap: this worker never
                // begins, and says so, so teardown can prove quiescence
                // without waiting for work that will never run.
                liveness_worker.mark_unstartable();
                return Err(WorkerOutcome::abandoned());
            }
            let _live = WorkerLivenessGuard::new(liveness_worker);

            // Action-time reauthorization. The worker re-checks for itself
            // rather than trusting the dispatcher's earlier answer, because
            // the gate, the queue, and a restart can all sit between them.
            if let Some(service) = service_worker.upgrade() {
                if let Err(error) = service.reauthorize_for_action(&spec_worker) {
                    let _ = store_worker.open_provider_send(
                        &rid_worker,
                        &attempt_id_worker,
                        spec_worker.spec_key(),
                    );
                    return Err(WorkerOutcome::refused(error));
                }
            }

            // Worker acknowledgement: the run becomes `Running` only now, as
            // the worker's own first durable act. Until this lands the receipt
            // honestly says queued, because nothing has started.
            let start_seq = bus_next_seq_for(&service_worker);
            let acknowledged = store_worker.update_run(&rid_worker, |current| {
                if current.state != RunState::Starting {
                    anyhow::bail!("run is not awaiting worker acknowledgement");
                }
                current.state = RunState::Running;
                current.queue_position = None;
                current.start_seq = start_seq;
                current.updated_at = Utc::now();
                Ok(())
            });
            if !matches!(acknowledged, Ok(Some(_))) {
                return Err(WorkerOutcome::refused(OrchError::new(
                    OrchErrorCode::Conflict,
                    "run was not awaiting acknowledgement when its worker started",
                )));
            }

            // ── durable provider send identity ─────────────────────────
            // Opened before anything is transmitted, so a crash from here on
            // can never be mistaken for "nothing happened".
            let send = match store_worker.open_provider_send(
                &rid_worker,
                &attempt_id_worker,
                spec_worker.spec_key(),
            ) {
                Ok(send) => send,
                Err(error) => {
                    return Err(WorkerOutcome::send_failed(
                        ProviderSendFailure::LedgerUnavailable,
                        error,
                    ));
                }
            };
            if let Err(error) = store_worker.advance_provider_send(
                &rid_worker,
                &send.send_id,
                &attempt_id_worker,
                ProviderSendState::Sending,
                None,
                None,
            ) {
                return Err(WorkerOutcome::send_failed(
                    ProviderSendFailure::LedgerUnavailable,
                    error,
                ));
            }

            let result = host_worker
                .session_prompt_reserved_with_max_rounds_for_run(
                    session_id,
                    prompt,
                    Some(max_rounds.max(1)),
                    &rid_worker,
                    &rid_worker,
                    execution_mode,
                )
                .await;

            // Classify the outcome into durable evidence. A returned error is
            // not proof that nothing was transmitted, so it settles as
            // `Uncertain` unless the turn provably never began.
            let (state, failure) = match &result {
                Ok(_) => (ProviderSendState::Sent, None),
                Err(_) => (
                    ProviderSendState::Uncertain,
                    Some(ProviderSendFailure::ResponseUnobserved),
                ),
            };
            let detail = result
                .as_ref()
                .err()
                .map(|error| redact_for(&service_worker, &error.to_string()));
            let _ = store_worker.advance_provider_send(
                &rid_worker,
                &send.send_id,
                &attempt_id_worker,
                state,
                failure,
                detail,
            );
            result.map_err(WorkerOutcome::turn_failed)
        });
        let worker_abort = worker.abort_handle();

        // ── outer supervisor ───────────────────────────────────────────
        let rid_publish = rid.clone();
        let entry_sup = entry.clone();
        let lease_sup = lease.clone();
        let gate_sup = gate.clone();
        let spec_sup = intent.clone();
        let supervisor = tokio::spawn(async move {
            if gate_sup.wait().await == GateOutcome::Abandoned {
                // The attempt was cancelled before it started. Terminalize it
                // honestly rather than leaving a `Starting` record behind.
                let mut exit = SupervisorExitGuard {
                    store: store.clone(),
                    service: service_ref.clone(),
                    entry: entry_sup.clone(),
                    run_id: rid.clone(),
                    candidate: None,
                    reason: TeardownReason::Cancelled,
                };
                exit.candidate = None;
                drop(exit);
                return;
            }
            // Armed for every exit: normal return, `?`, panic unwind, abort.
            let mut exit = SupervisorExitGuard {
                store: store.clone(),
                service: service_ref.clone(),
                entry: entry_sup.clone(),
                run_id: rid.clone(),
                candidate: None,
                reason: TeardownReason::SupervisorExit,
            };

            let deadline = tokio::time::sleep(Duration::from_millis(max_ms.max(1)));
            tokio::pin!(deadline);
            let mut heartbeat = tokio::time::interval(ATTEMPT_HEARTBEAT_INTERVAL);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            heartbeat.tick().await; // the first tick is immediate
            tokio::pin!(worker);

            enum Exit {
                Joined(Result<Result<String, WorkerOutcome>, tokio::task::JoinError>),
                TimedOut,
                Reaped,
            }

            // Cancellation and teardown are bounded. A backend that ignores
            // its token cannot hold admission capacity forever.
            let exit_reason = loop {
                tokio::select! {
                    biased;
                    _ = entry_sup.cancel.cancelled() => break Exit::Reaped,
                    _ = &mut deadline => break Exit::TimedOut,
                    joined = &mut worker => break Exit::Joined(joined),
                    _ = heartbeat.tick() => {
                        // A heartbeat that no longer belongs to this attempt
                        // means the lease was taken; stop rather than keep
                        // running beside the new owner.
                        if store
                            .renew_attempt_lease(&rid, &lease_sup.attempt_id, &owner_id)
                            .is_err()
                        {
                            exit.reason = TeardownReason::LeaseLost;
                            break Exit::Reaped;
                        }
                    }
                }
            };

            let (timed_out, reaped, result): (bool, bool, Result<String, WorkerOutcome>) =
                match exit_reason {
                    Exit::Joined(Ok(result)) => (false, false, result),
                    Exit::Joined(Err(join_error)) => (
                        false,
                        true,
                        Err(WorkerOutcome::panicked(join_error.to_string())),
                    ),
                    Exit::TimedOut | Exit::Reaped => {
                        let timed_out = matches!(exit_reason, Exit::TimedOut);
                        if timed_out {
                            exit.reason = TeardownReason::Deadline;
                        }
                        let _ = tokio::time::timeout(
                            DEFAULT_TEARDOWN_BUDGET,
                            host.cancel_turn_and_await(Some(session_id)),
                        )
                        .await;
                        // Bounded-await the real worker, then abort it and
                        // await the abort. Capacity is not released here: the
                        // teardown owner does that, and only after proving the
                        // future is gone.
                        let settled =
                            tokio::time::timeout(Duration::from_secs(1), &mut worker).await;
                        let result = match settled {
                            Ok(Ok(result)) => result,
                            Ok(Err(join_error)) => {
                                Err(WorkerOutcome::panicked(join_error.to_string()))
                            }
                            Err(_) => {
                                worker.abort();
                                let _ = tokio::time::timeout(DEFAULT_TEARDOWN_BUDGET, &mut worker)
                                    .await;
                                // Torn down mid-flight: whether the provider
                                // saw the request is unknown, and must be
                                // recorded as unknown.
                                Err(WorkerOutcome::send_failed(
                                    ProviderSendFailure::AttemptTornDown,
                                    OrchError::new(
                                        OrchErrorCode::Timeout,
                                        "turn did not stop within the teardown deadline",
                                    ),
                                ))
                            }
                        };
                        (timed_out, !timed_out, result)
                    }
                };

            // Stop aggregator; then reconcile aggregates from the journal range
            // so late FileEdit/test events are not lost if the task was aborted mid-drain.
            agg_task.abort();
            let _ = agg_task.await;

            // A torn-down send must leave `Uncertain` evidence even though the
            // worker future never got to write it.
            if let Err(outcome) = &result {
                if let Some(failure) = outcome.send_failure {
                    if let Ok(Some(send)) = store.load_provider_send(&rid) {
                        let _ = store.advance_provider_send(
                            &rid,
                            &send.send_id,
                            &lease_sup.attempt_id,
                            failure.resulting_state(),
                            Some(failure),
                            None,
                        );
                    }
                }
            }

            let end_seq = bus.current_seq();
            let reconciliation = collect_run_updates(&bus, &store, &rid, end_seq);
            let durable_result = match &result {
                Ok(text) => Ok(bus.redact_text(text, 8_000)),
                Err(outcome) => Err(bus.redact_text(&outcome.message, 2_000)),
            };
            let incomplete_stop = result
                .as_ref()
                .is_ok_and(|text| crate::host_helpers::is_incomplete_stop_message(text));
            let mut candidate = store.load_run(&rid).ok().flatten().unwrap_or(run);
            for update in &reconciliation {
                fold_run_update(&mut candidate, update);
            }
            candidate.end_seq = candidate.end_seq.or(Some(end_seq));
            candidate.updated_at = Utc::now();

            // Completion is gated on durable send evidence. A run whose work
            // is not known to have reached the provider can never be recorded
            // `Completed`, however cleanly the local future returned.
            let send_state = store
                .load_provider_send(&rid)
                .ok()
                .flatten()
                .map(|send| send.state);

            if !candidate.state.is_terminal() {
                if timed_out {
                    candidate.state = RunState::LimitReached;
                    candidate.terminal_result = Some("limit_reached".into());
                    candidate.error_code = Some("limit_reached".into());
                    if let Ok(text) = &durable_result {
                        candidate.final_response = Some(text.clone());
                    }
                } else if reaped {
                    // A live reaper (expiry, explicit teardown, lost lease)
                    // ends the run `Interrupted`. This is a real terminal
                    // outcome, not a placeholder: the model turn is gone and
                    // is never resumed implicitly.
                    candidate.state = RunState::Interrupted;
                    candidate.terminal_result = Some("interrupted".into());
                    candidate.error_code = Some(
                        result
                            .as_ref()
                            .err()
                            .map(|outcome| outcome.error_code)
                            .unwrap_or("interrupted")
                            .to_string(),
                    );
                    if let Err(text) = &durable_result {
                        candidate.final_response = Some(text.clone());
                    }
                } else {
                    match &durable_result {
                        Ok(text) => {
                            let provider_confirmed = send_state
                                .map(ProviderSendState::permits_completion)
                                .unwrap_or(false);
                            if !provider_confirmed {
                                // Refusing to fabricate a success: the turn
                                // returned, but nothing durably says the work
                                // reached the provider.
                                candidate.state = RunState::Failed;
                                candidate.terminal_result = Some("failed".into());
                                candidate.error_code = Some("provider_send_unconfirmed".into());
                            } else if incomplete_stop {
                                candidate.state = RunState::LimitReached;
                                candidate.terminal_result = Some("limit_reached".into());
                                candidate.error_code = Some("limit_reached".into());
                            } else {
                                candidate.state = RunState::Completed;
                                candidate.terminal_result = Some("completed".into());
                            }
                            candidate.final_response = Some(text.clone());
                        }
                        Err(error) => {
                            candidate.state = RunState::Failed;
                            candidate.terminal_result = Some("failed".into());
                            candidate.error_code = Some(
                                result
                                    .as_ref()
                                    .err()
                                    .map(|outcome| outcome.error_code)
                                    .unwrap_or("internal")
                                    .to_string(),
                            );
                            candidate.final_response = Some(error.clone());
                        }
                    }
                }
            }
            if candidate.aggregates.verification.is_none() {
                let observations = crate::completion::observations_from_run(
                    candidate.aggregates.changes.len(),
                    candidate
                        .aggregates
                        .tests
                        .iter()
                        .map(|t| (t.exit_code, t.cancelled)),
                    candidate.aggregates.permissions_requested,
                    candidate.aggregates.permissions_granted,
                    candidate.aggregates.permissions_denied,
                );
                let outcome = candidate.terminal_result.as_deref().unwrap_or("incomplete");
                candidate.aggregates.verification = Some(crate::completion::build_evidence(
                    outcome,
                    candidate.final_response.as_deref(),
                    observations,
                    candidate.aggregates.usage.clone(),
                    matches!(candidate.state, RunState::Cancelled | RunState::Interrupted),
                ));
            }
            // External isolated runs do not pass through the desktop finalizer.
            if let Some(execution) = candidate.execution.as_mut() {
                if execution.mode == RunExecutionMode::IsolatedWorktree {
                    if candidate.state == RunState::Completed {
                        match crate::run_promotion::snapshot(
                            Path::new(&execution.execution_workspace),
                            &execution.base_revision,
                        ) {
                            Ok(snapshot) => {
                                execution.final_fingerprint = Some(snapshot.fingerprint);
                                execution.promotion_state = PromotionState::Ready;
                                if !snapshot.changed_files.is_empty() {
                                    candidate.aggregates.changes = snapshot.changed_files;
                                }
                            }
                            Err(error) => {
                                execution.promotion_state = PromotionState::Conflicted;
                                candidate.error_code = Some("promotion_conflict".into());
                                let _ = store.enqueue_audit(AuditEntry {
                                    ts: Utc::now(),
                                    tool: "run_finalization".into(),
                                    request_id: None,
                                    session_id: Some(session_id),
                                    workspace: Some(candidate.workspace.clone()),
                                    outcome: "promotion_conflict".into(),
                                    error_code: Some("promotion_conflict".into()),
                                    detail: bus.redact_text(&error.to_string(), 500),
                                });
                            }
                        }
                    } else {
                        execution.promotion_state = PromotionState::Conflicted;
                    }
                }
            }

            // The specification must still be the one every holder agreed on.
            let binding = SpecBinding {
                run: candidate.spec_key.as_deref(),
                lease: Some(lease_sup.intent_digest.as_str()),
                worker: Some(spec_sup.spec_key()),
                ..SpecBinding::default()
            };
            if let Err(error) = binding.verify(
                &spec_sup,
                store.seal_authority(),
                &[SpecHolder::Run, SpecHolder::Lease, SpecHolder::Worker],
            ) {
                candidate.state = RunState::Failed;
                candidate.terminal_result = Some("failed".into());
                candidate.error_code = Some("spec_binding_mismatch".into());
                candidate.final_response = Some(bus.redact_text(&error.message, 500));
            }

            // Hand the terminal record to the exit guard. From here every exit
            // path — including an abort landing on the next line — installs it
            // or leaves a bounded recoverable finalization intent.
            if exit.reason == TeardownReason::SupervisorExit {
                exit.reason = if candidate.error_code.as_deref() == Some("authorization_drift") {
                    TeardownReason::AuthorizationDrift
                } else if candidate.state.is_terminal() {
                    TeardownReason::Completed
                } else {
                    TeardownReason::SupervisorExit
                };
            }
            exit.candidate = Some(candidate);
            drop(exit);
        });

        // ── publish, record the dispatch intent, then open ─────────────
        //
        // Order matters three times over. Handles are attached before the
        // entry is published, so nothing can observe a half-registered
        // attempt. The `Starting` cut is written before the gate opens, so the
        // ledger admits the attempt exists before any of it can run. And the
        // gate opens last, so no task begins until both are true.
        entry.attach_aggregator(agg_task_abort);
        entry.attach_worker(worker_abort);
        entry.attach_supervisor(supervisor);
        self.publish_dispatch(&rid_publish, entry.clone());
        entry.mark_registered();

        if let Err(error) = self.record_dispatch_intent(&rid_publish, &lease) {
            // The ledger could not admit that this attempt exists, so the
            // attempt does not happen: abandon the gate and let the supervisor
            // terminalize it. Opening the gate here would start work the
            // ledger has no record of.
            eprintln!(
                "[grokptah] run {rid_publish} could not record its dispatch intent: {}",
                error.message
            );
            gate.abandon();
            return;
        }

        gate.open();
        self.audit(
            "run_dispatch",
            None,
            Some(session_id),
            None,
            "accepted",
            None,
            &format!("run {rid_publish} dispatched as attempt {}", lease.attempt),
        );
    }

    /// Claim the right to dispatch one run, without publishing anything.
    ///
    /// Returns false when another attempt already holds the claim or is
    /// already published.
    fn reserve_dispatch(&self, run_id: &str) -> bool {
        let mut reserved = self.dispatch_reservations.lock();
        if self.live_workers.lock().contains_key(run_id) {
            return false;
        }
        reserved.insert(run_id.to_string())
    }

    /// Publish a fully-registered attempt and release its reservation.
    fn publish_dispatch(&self, run_id: &str, entry: Arc<LiveWorker>) {
        let mut live = self.live_workers.lock();
        live.insert(run_id.to_string(), entry);
        drop(live);
        self.dispatch_reservations.lock().remove(run_id);
    }

    /// Persist the honest `Starting` cut: an attempt holds this run's lease
    /// and is about to begin, but nothing has acknowledged yet.
    fn record_dispatch_intent(&self, run_id: &str, lease: &AttemptLease) -> Result<(), OrchError> {
        let attempt = lease.attempt;
        match self.store.update_run(run_id, |run| {
            if run.state != RunState::Queued {
                anyhow::bail!("run is no longer queued");
            }
            run.state = RunState::Starting;
            run.queue_position = None;
            run.updated_at = Utc::now();
            Ok(())
        }) {
            Ok(Some(_)) => {
                let _ = attempt;
                Ok(())
            }
            Ok(None) => Err(OrchError::new(
                OrchErrorCode::Conflict,
                "run record disappeared before dispatch",
            )),
            Err(error) => Err(OrchError::new(OrchErrorCode::Internal, error.to_string())),
        }
    }

    pub async fn queue_prompt(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        prompt: String,
        priority: bool,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_queue_prompt";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "prompt": prompt,
            "priority": priority,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&prompt) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_load(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let (entries, changed_entry, revision) =
            match self.host.session_queue_add_with_source_receipt(
                session_id,
                prompt,
                priority,
                "control",
                Some("mcp".into()),
            ) {
                Ok(e) => e,
                Err(e) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        None,
                        session_id,
                        &claimed,
                        OrchError::new(OrchErrorCode::Internal, e.to_string()),
                    ));
                }
            };
        let response = json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "origin": "mcp",
            "action": "queued",
            "disposition": "queued",
            "actionVersion": changed_entry.version,
            "revision": revision,
            "entry": changed_entry,
            "entries": entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "queued",
        );
        Ok(response)
    }

    pub async fn steer(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        text: String,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_steer";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "text": text,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };
        if let Err(e) = reject_control_prompt(&text) {
            return Err(fail(self, e));
        }
        if let Err(e) = self.require_build_session(session_id) {
            return Err(fail(self, e));
        }
        let session = self.host.session_load(session_id).unwrap();
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };

        let (receipt, revision) =
            match self
                .host
                .session_steer_with_owner(session_id, text, Some("mcp".into()))
            {
                Ok(r) => r,
                Err(e) => {
                    return Err(self.fail_claim(
                        &mut lease,
                        None,
                        session_id,
                        &claimed,
                        OrchError::new(OrchErrorCode::Internal, e.to_string()),
                    ));
                }
            };
        let response = json!({
            "requestId": request_id,
            "actionId": request_id,
            "sessionId": session_id,
            "workspace": claimed.display().to_string(),
            "origin": "mcp",
            "action": "steer_now",
            "disposition": receipt.disposition,
            "entry": receipt.entry,
            "actionVersion": receipt.entry.version,
            "revision": revision,
            "entries": receipt.entries,
        });
        if let Err(e) = lease.complete(None, response.clone()) {
            return Err(self.fail_claim(&mut lease, None, session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "steer",
        );
        Ok(response)
    }

    pub async fn cancel(
        &self,
        auth: &AuthContext,
        request_id: &str,
        session_id: Uuid,
        workspace: &Path,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value, OrchError> {
        let _ = auth;
        let tool = "ptah_cancel";
        let payload = json!({
            "sessionId": session_id,
            "workspace": workspace.display().to_string(),
            "runId": run_id,
        });
        let phash = hash_payload(&payload);
        let fail = |svc: &Self, e: OrchError| {
            svc.audit_err(
                tool,
                Some(request_id),
                Some(session_id),
                Some(&workspace.display().to_string()),
                &e,
            );
            e
        };

        let rid = match run_id {
            Some(r) if !r.is_empty() => r,
            _ => {
                return Err(fail(
                    self,
                    OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "run_id is required for cancel",
                    ),
                ));
            }
        };

        let run = match self.store.load_run(rid) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown run_id"),
                ));
            }
            Err(e) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::Internal, e.to_string()),
                ));
            }
        };

        if run.session_id != session_id {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::ForbiddenScope,
                    "run_id does not belong to session",
                ),
            ));
        }
        let session = match self.host.session_load(session_id) {
            Ok(s) => s,
            Err(_) => {
                return Err(fail(
                    self,
                    OrchError::new(OrchErrorCode::InvalidRequest, "unknown session"),
                ));
            }
        };
        let cwd = if session.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&session.cwd))
        };
        let allowlist = self.config.lock().allowlist.clone();
        let claimed = match require_workspace_match(&allowlist, cwd.as_deref(), workspace) {
            Ok(c) => c,
            Err(e) => return Err(fail(self, e)),
        };
        // Workspace must match the run record as well.
        if claimed.display().to_string() != run.workspace
            && canonical_cmp(&claimed, Path::new(&run.workspace)).is_err()
        {
            return Err(fail(
                self,
                OrchError::new(
                    OrchErrorCode::WorkspaceMismatch,
                    "workspace does not match run",
                ),
            ));
        }

        let mut lease = match self
            .begin_idempotency(tool, request_id, &phash, session_id, &claimed)
            .await?
        {
            IdempotencyStart::Replay(value) => return Ok(value),
            IdempotencyStart::Perform(lease) => lease,
        };
        if run.state.is_terminal() {
            let error = OrchError::new(
                OrchErrorCode::InvalidRequest,
                format!("run already terminal ({:?})", run.state),
            );
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, error));
        }

        // Persist cancelled transactionally before signalling. The closure
        // rechecks state so a concurrent completion can never be overwritten.
        let cancel_update = self.store.update_run(rid, |current| {
            if current.session_id != session_id {
                return Err(anyhow::anyhow!("run_id does not belong to session"));
            }
            if current.state.is_terminal() {
                return Err(anyhow::anyhow!(
                    "run already terminal ({:?})",
                    current.state
                ));
            }
            current.state = RunState::Cancelled;
            current.queue_position = None;
            current.updated_at = Utc::now();
            current.end_seq = None;
            current.terminal_result = Some("cancelled".into());
            Ok(())
        });
        if !matches!(cancel_update, Ok(Some(_))) {
            let message = match cancel_update {
                Ok(None) => "run record disappeared during cancel".into(),
                Err(error) => error.to_string(),
                Ok(Some(_)) => unreachable!(),
            };
            return Err(self.fail_claim(
                &mut lease,
                Some(rid.into()),
                session_id,
                &claimed,
                OrchError::new(OrchErrorCode::Internal, message),
            ));
        }

        let was_pending = self.remove_pending(rid);
        // A cancelled admission must not keep executable input around: it can
        // never legitimately run again.
        let _ = self.store.remove_acceptance_intent(rid);

        // Teardown must abort and bounded-await the *actual* worker and its
        // supervisor before this run's capacity can be reused. Reporting
        // `teardownComplete` from a ledger write alone would let a second
        // attempt start beside a future that can still execute.
        //
        // Cancellation does not tear anything down itself: it asks the single
        // teardown owner to, then waits for that owner's own proof. Two
        // callers racing the same abort-and-await sequence is exactly the
        // duplication this design removes.
        let live = self.live_workers.lock().contains_key(rid);
        let teardown_complete = if live {
            self.request_teardown(rid, TeardownReason::Cancelled);
            self.await_quiescence(rid, DEFAULT_TEARDOWN_BUDGET).await
        } else {
            let reservation_released = self.host.release_turn_reservation(session_id, rid);
            if was_pending || reservation_released {
                true
            } else {
                tokio::time::timeout(DEFAULT_TEARDOWN_BUDGET, async {
                    let _ = self.host.cancel_turn_and_await(Some(session_id)).await;
                    self.host.wait_turn_idle(session_id).await;
                })
                .await
                .is_ok()
            }
        };

        let response = json!({
            "requestId": request_id,
            "sessionId": session_id,
            "runId": rid,
            "cancelled": true,
            "wasQueued": was_pending,
            "teardownComplete": teardown_complete,
            "state": RunState::Cancelled,
        });
        if let Err(e) = lease.complete(Some(rid.into()), response.clone()) {
            return Err(self.fail_claim(&mut lease, Some(rid.into()), session_id, &claimed, e));
        }
        self.audit(
            tool,
            Some(request_id),
            Some(session_id),
            Some(&claimed.display().to_string()),
            "accepted",
            None,
            "cancelled",
        );
        if was_pending {
            self.pump_pending();
        }
        Ok(response)
    }
}

/// Map a Computer Run read failure into the control plane's error vocabulary.
/// `Unauthorized` covers unknown, cross-session, cross-workspace, unbound, and
/// traversal-shaped reads with one shared message, so this mapping must stay
/// single-valued to preserve that indistinguishability on the wire.
/// Whether usable provider credentials currently resolve for this session's
/// route.
///
/// Only presence is reported, never the material. Credentials being revoked
/// between admission and action is a route change, and must refuse the
/// dispatch rather than fail deep inside a turn.
fn provider_route_revision(session: &crate::session::SessionSummary) -> String {
    let offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE").is_some();
    let model = crate::models_catalog::resolve_default_model();
    // Resolve the concrete route the work would take: provider, wire model,
    // and endpoint. A credential rotation, a re-pointed base URL, or a model
    // swap each change this, and each is a different execution than the one
    // that was authorized.
    let (provider_id, wire_model, endpoint, credential) =
        match crate::auth_store::resolve_wire_credentials_for_model(&model) {
            Ok(Some(creds)) => {
                let target = crate::host_helpers::resolve_model_target(&creds, &model).ok();
                (
                    creds.provider_id.clone(),
                    target
                        .as_ref()
                        .map(|t| t.wire_model.clone())
                        .unwrap_or_default(),
                    target.map(|t| t.base_url).unwrap_or_default(),
                    // The credential enters by digest: presence and identity
                    // are what matter, and the material must never be sealed
                    // into a record.
                    hash_payload(&json!(creds.bearer)),
                )
            }
            _ => (String::new(), String::new(), String::new(), String::new()),
        };
    hash_payload(&json!({
        "offline": offline,
        "providerId": provider_id,
        "model": model,
        "wireModel": wire_model,
        "endpoint": endpoint,
        "credential": credential,
        "sessionKind": session.kind,
        "executionMode": session.execution_mode,
    }))
}

/// Session revision sealed into an acceptance intent.
///
/// This captures what *authorizes* the session, not what has happened in it.
/// Message count and last-updated deliberately do not appear: a queued task
/// exists precisely so other turns can run first, so treating ordinary
/// conversational progress as authorization drift would make queueing on a
/// busy session impossible. What does appear is everything that would change
/// where or under what policy the work executes — the session being re-pointed
/// at another directory, archived, switched out of Build, or switched between
/// shared and isolated execution.
fn session_revision_of(session: &crate::session::SessionSummary) -> String {
    hash_payload(&json!({
        "sessionId": session.id,
        "kind": session.kind,
        "cwd": session.cwd,
        "executionMode": session.execution_mode,
        "archived": session.archived,
    }))
}

/// Workspace revision sealed into an acceptance intent: the workspace the work
/// was admitted against and whether it was ready to be written to.
fn workspace_revision_of(session: &crate::session::SessionSummary) -> String {
    hash_payload(&json!({
        "cwd": session.cwd,
        "workspaceStatus": session.workspace_status.as_str(),
    }))
}

fn computer_scope_denied() -> OrchError {
    OrchError::new(
        OrchErrorCode::ForbiddenScope,
        "computer run is not available to this session",
    )
}

fn computer_read_error(error: crate::computer_use::ComputerError) -> OrchError {
    use crate::computer_use::ComputerErrorCode;
    let code = match error.code {
        ComputerErrorCode::Unauthorized => OrchErrorCode::ForbiddenScope,
        ComputerErrorCode::InvalidRequest => OrchErrorCode::InvalidRequest,
        _ => OrchErrorCode::Internal,
    };
    OrchError::new(code, error.message)
}

fn computer_mutation_error(error: crate::computer_use::ComputerError) -> OrchError {
    use crate::computer_use::ComputerErrorCode;
    let code = match error.code {
        ComputerErrorCode::Unauthorized
        | ComputerErrorCode::ForbiddenTarget
        | ComputerErrorCode::PermissionDenied
        | ComputerErrorCode::PermissionRevoked => OrchErrorCode::ForbiddenScope,
        ComputerErrorCode::InvalidRequest => OrchErrorCode::InvalidRequest,
        ComputerErrorCode::Conflict | ComputerErrorCode::StaleObservation => {
            OrchErrorCode::Conflict
        }
        ComputerErrorCode::InvalidState
        | ComputerErrorCode::TargetChanged
        | ComputerErrorCode::TargetClosed
        | ComputerErrorCode::Interrupted
        | ComputerErrorCode::UncertainOutcome => OrchErrorCode::Conflict,
        _ => OrchErrorCode::Internal,
    };
    OrchError::new(code, error.message)
}

fn canonical_cmp(a: &Path, b: &Path) -> Result<(), ()> {
    let ca = dunce::canonicalize(a).map_err(|_| ())?;
    let cb = dunce::canonicalize(b).map_err(|_| ())?;
    if ca == cb {
        Ok(())
    } else {
        Err(())
    }
}

/// Incrementally persist run-scoped aggregates so journal rollover cannot erase them.
pub(crate) fn apply_run_aggregate(
    store: &OrchStore,
    run_id: &str,
    session_id: Uuid,
    update: &crate::events::SessionUpdate,
) {
    if session_id_of(update) != Some(session_id) {
        return;
    }
    if !matches!(
        update,
        crate::events::SessionUpdate::FileEdit { .. }
            | crate::events::SessionUpdate::ShellSessionStarted { .. }
            | crate::events::SessionUpdate::ShellSessionEnded { .. }
            | crate::events::SessionUpdate::AgentProgress { .. }
            | crate::events::SessionUpdate::CompletionEvidence { .. }
    ) {
        return;
    }
    let _ = store.update_run(run_id, |r| {
        if fold_run_update(r, update) {
            r.updated_at = Utc::now();
        }
        Ok(())
    });
}

fn fold_run_update(run: &mut RunRecord, update: &crate::events::SessionUpdate) -> bool {
    match update {
        crate::events::SessionUpdate::FileEdit { path, summary, .. } => {
            if run.aggregates.changes.iter().any(|c| c.path == *path) {
                return false;
            }
            run.aggregates.changes.push(ChangeRecord {
                path: path.clone(),
                summary: summary.clone(),
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionStarted {
            command, call_id, ..
        } if is_recognized_test_command(command) => {
            if run.aggregates.tests.iter().any(|t| t.call_id == *call_id) {
                return false;
            }
            run.aggregates.tests.push(TestObservation {
                call_id: call_id.clone(),
                command: Some(command.clone()),
                status: "started".into(),
                exit_code: None,
                cancelled: None,
            });
            true
        }
        crate::events::SessionUpdate::ShellSessionEnded {
            call_id,
            exit_code,
            cancelled,
            ..
        } => {
            if let Some(t) = run
                .aggregates
                .tests
                .iter_mut()
                .find(|t| t.call_id == *call_id)
            {
                t.status = "ended".into();
                t.exit_code = *exit_code;
                t.cancelled = Some(*cancelled);
                true
            } else {
                false
            }
        }
        crate::events::SessionUpdate::AgentProgress {
            round,
            max_rounds,
            last_tool,
            detail,
            ..
        } => {
            run.progress = Some(RunProgress {
                round: *round,
                max_rounds: *max_rounds,
                last_tool: last_tool.clone(),
                detail: crate::textutil::truncate_at_char_boundary(detail, 2_000).to_string(),
                updated_at: Utc::now(),
            });
            true
        }
        crate::events::SessionUpdate::CompletionEvidence { evidence, .. } => {
            run.aggregates.usage = evidence.usage.clone();
            run.aggregates.permissions_requested = evidence.observations.permissions_requested;
            run.aggregates.permissions_granted = evidence.observations.permissions_granted;
            run.aggregates.permissions_denied = evidence.observations.permissions_denied;
            run.aggregates.verification = Some(evidence.clone());
            true
        }
        _ => false,
    }
}

fn collect_run_updates(
    bus: &EventBus,
    store: &OrchStore,
    run_id: &str,
    end_seq: u64,
) -> Vec<crate::events::SessionUpdate> {
    let Ok(Some(run)) = store.load_run(run_id) else {
        return Vec::new();
    };
    let after = run.start_seq.map(|s| s.saturating_sub(1)).unwrap_or(0);
    bus.read_range_all(after, Some(end_seq), Some(run.session_id))
        .map(|entries| entries.into_iter().map(|e| e.update).collect())
        .unwrap_or_default()
}

fn session_id_of(u: &crate::events::SessionUpdate) -> Option<Uuid> {
    use crate::events::SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | TurnStarted { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
        | CompletionEvidence { session_id, .. }
        | TurnComplete { session_id, .. }
        | Error { session_id, .. }
        | SubagentSpawned { session_id, .. }
        | SubagentUpdate { session_id, .. }
        | ShellSessionStarted { session_id, .. }
        | ShellOutput { session_id, .. }
        | ShellSessionEnded { session_id, .. }
        | FileEdit { session_id, .. }
        | AgentProgress { session_id, .. }
        | RateLimited { session_id, .. }
        | SteeringInjected { session_id, .. }
        | PromptQueueChanged { session_id, .. }
        | ComputerApprovalRequired { session_id, .. } => Some(*session_id),
        BackgroundTask { session_id, .. } => *session_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_run(run_id: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            session_id: Uuid::new_v4(),
            workspace: "/tmp/project".into(),
            request_id: format!("req-{run_id}"),
            client_id: Some("mcp".into()),
            state: RunState::Running,
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            queue_position: None,
            spec_key: None,
            bounds: RunBounds::default(),
            prompt_preview: "preview".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: None,
            final_response: None,
            error_code: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        }
    }

    fn guard_for(store: &OrchStore, run: &RunRecord) -> (Arc<LiveWorker>, SupervisorExitGuard) {
        let entry = Arc::new(LiveWorker::new(
            run.run_id.clone(),
            "attempt-1".into(),
            1,
            run.session_id,
            CancellationToken::new(),
            Arc::new(WorkerLiveness::default()),
        ));
        let guard = SupervisorExitGuard {
            store: store.clone(),
            service: Weak::new(),
            entry: entry.clone(),
            run_id: run.run_id.clone(),
            candidate: None,
            reason: TeardownReason::SupervisorExit,
        };
        (entry, guard)
    }

    /// A supervisor that unwinds must still leave the run durably terminal.
    /// The guard's `Drop` is the only terminalization path, so this is the
    /// panic case of "every outer-supervisor exit terminalizes".
    #[test]
    fn supervisor_panic_still_terminalizes_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = OrchStore::open(dir.path()).unwrap();
        let run = queued_run("panic-run");
        store.save_run(&run).unwrap();

        let (entry, guard) = guard_for(&store, &run);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard;
            panic!("supervisor exploded");
        }));
        assert!(result.is_err(), "the panic must actually propagate");

        let reloaded = store.load_run(&run.run_id).unwrap().expect("run record");
        assert_eq!(reloaded.state, RunState::Interrupted);
        assert_eq!(reloaded.error_code.as_deref(), Some("supervisor_exit"));
        assert!(entry.is_settled(), "the panic path must settle the attempt");
    }

    /// A supervisor that computed a terminal record installs exactly that
    /// record, and settles exactly once even if a second exit path races it.
    #[test]
    fn supervisor_exit_installs_its_candidate_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = OrchStore::open(dir.path()).unwrap();
        let run = queued_run("completed-run");
        store.save_run(&run).unwrap();

        let (entry, mut guard) = guard_for(&store, &run);
        let mut candidate = run.clone();
        candidate.state = RunState::Completed;
        candidate.terminal_result = Some("completed".into());
        candidate.final_response = Some("done".into());
        guard.candidate = Some(candidate);
        drop(guard);

        let reloaded = store.load_run(&run.run_id).unwrap().expect("run record");
        assert_eq!(reloaded.state, RunState::Completed);
        assert_eq!(reloaded.final_response.as_deref(), Some("done"));

        // A second exit path for the same attempt must be a no-op: it must
        // not re-terminalize the run or release capacity a second time.
        let (_, second) = SupervisorExitGuardPair::same_entry(&store, &run, entry.clone());
        drop(second);
        let after = store.load_run(&run.run_id).unwrap().expect("run record");
        assert_eq!(after.state, RunState::Completed);
        assert_eq!(after.final_response.as_deref(), Some("done"));
    }

    /// When the terminal record cannot be installed, the guard must still
    /// leave a bounded recoverable finalization intent for the next start.
    #[test]
    fn finalization_failure_leaves_a_recoverable_intent() {
        let dir = tempfile::tempdir().unwrap();
        let store = OrchStore::open(dir.path()).unwrap();
        let run = queued_run("undrainable-run");
        store.save_run(&run).unwrap();

        // Block the run record's atomic-write staging path so every install
        // attempt fails the way a full or read-only volume would.
        let safe = safe_id_filename(&run.run_id).unwrap();
        let blocked = dir.path().join("runs").join(format!("{safe}.json.tmp"));
        std::fs::create_dir_all(&blocked).unwrap();

        let (entry, mut guard) = guard_for(&store, &run);
        let mut candidate = run.clone();
        candidate.state = RunState::Failed;
        candidate.terminal_result = Some("failed".into());
        candidate.error_code = Some("internal".into());
        guard.candidate = Some(candidate);
        drop(guard);
        assert!(entry.is_settled());

        // The run record itself could not be rewritten...
        let stuck = store.load_run(&run.run_id).unwrap().expect("run record");
        assert_eq!(stuck.state, RunState::Running);
        // ...but a durable finalization intent is waiting for the next start.
        let intent = dir.path().join("finalization").join(format!("{safe}.json"));
        assert!(
            intent.is_file(),
            "a bounded recoverable finalization intent must exist"
        );

        // Unblock and reopen: the intent is replayed and the run terminalizes.
        std::fs::remove_dir_all(&blocked).unwrap();
        drop(store);
        let reopened = OrchStore::open(dir.path()).unwrap();
        let recovered = reopened.load_run(&run.run_id).unwrap().expect("run record");
        assert_eq!(recovered.state, RunState::Failed);
        assert!(!intent.is_file(), "the intent must be consumed");
    }

    /// Helper so the exactly-once assertion can build a second guard over the
    /// same live entry without duplicating the field list.
    struct SupervisorExitGuardPair;

    impl SupervisorExitGuardPair {
        fn same_entry(
            store: &OrchStore,
            run: &RunRecord,
            entry: Arc<LiveWorker>,
        ) -> (Arc<LiveWorker>, SupervisorExitGuard) {
            let guard = SupervisorExitGuard {
                store: store.clone(),
                service: Weak::new(),
                entry: entry.clone(),
                run_id: run.run_id.clone(),
                candidate: None,
                reason: TeardownReason::SupervisorExit,
            };
            (entry, guard)
        }
    }
}
