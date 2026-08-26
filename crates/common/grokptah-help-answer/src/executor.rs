//! Supervised execution of bounded Help answers.
//!
//! What "supervised" means here, concretely:
//!
//! - **Bounded queue.** Admission to the queue is refused at its bound rather
//!   than growing. A queue that always accepts is a queue that turns a slow
//!   provider into unbounded memory.
//! - **A fixed pool.** Concurrency is the pool size. There is no path that
//!   spawns an extra thread under load.
//! - **Deadlines are enforced by the executor, not by the caller.** A
//!   supervisor thread cancels work whose deadline has passed. A design that
//!   relies on someone calling `join` in time is not supervised; it is
//!   supervised *if the caller remembers*.
//! - **Capacity is held until quiescence.** Cancelling sets a token and waits
//!   a bounded time for the worker to stop. A worker that ignores the token
//!   keeps its slot — because it is still running, and pretending otherwise
//!   is how a "cancelled" task quietly keeps talking to a provider while the
//!   executor reports capacity it does not have. The task reports
//!   [`ExecutionOutcome::Abandoned`], [`ExecutorStats::stuck`] counts it, and
//!   the slot returns only when the worker actually returns.
//! - **No caller-injected production transport.** An executor cannot be built
//!   without a [`GrantMintingKey`]. The key is what verifies admissions, so
//!   only code that holds host key material can construct one — a renderer
//!   cannot hand in its own provider, because it cannot build the executor
//!   that would call it.
//!
//! Nothing here touches the filesystem, spawns a process, or persists a byte.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use grokptah_help_authority::{
    AdmissionExpectation, AnswerAdmission, BoundCitation, GrantMintingKey, bind_outcome,
    citations_overlap, verify_admission,
};

use crate::dto::{AnswerReply, AnswerRequestCore, request_digest};
use crate::receipt::{
    ExecutionOutcome, ExecutionReceipt, FailureReason, ReceiptInputs, build_receipt,
};

/// A cooperative cancellation token.
///
/// Cooperative because it must be: a provider call cannot be interrupted from
/// outside without leaving whatever it was doing half-finished. A provider
/// that never reads this is exactly the case [`ExecutionOutcome::Abandoned`]
/// exists to report honestly rather than paper over.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// True once cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// What a provider may fail with.
///
/// Deliberately opaque. The executor never records or forwards a provider's
/// own message, which can carry a URL, a header, or a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError;

/// The one call a provider may make.
///
/// No tools, no history, no session, no follow-up. One request in, one reply
/// out, and a token to stop.
pub trait HelpAnswerProvider: Send + Sync + 'static {
    /// Phrase one bounded answer. Must observe `cancel` cooperatively.
    ///
    /// # Errors
    /// Returns [`ProviderError`] when the provider cannot answer.
    fn answer(
        &self,
        request: &AnswerRequestCore,
        cancel: &CancelToken,
    ) -> Result<AnswerReply, ProviderError>;
}

/// What the executor validated a reply against, beyond the wire contract.
///
/// The corpus lives on the retrieval side, so span and coverage verification
/// happen there and are reported back through this. The executor refuses to
/// bind an outcome without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyVerdict {
    /// True when the reply's claims were fully covered by verified spans.
    pub accepted: bool,
    /// Citations reduced to what the outcome binding covers.
    pub citations: Vec<BoundCitation>,
    /// Source anchor ids cited.
    pub cited_source_ids: Vec<String>,
    /// Claims the answer segmented into.
    pub claim_count: u32,
}

/// Decides whether a reply's claims are covered by the corpus it cites.
pub trait ReplyValidator: Send + Sync + 'static {
    /// Verify a reply against the corpus.
    fn verify(&self, request: &AnswerRequestCore, reply: &AnswerReply) -> ReplyVerdict;
}

/// Executor bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorConfig {
    /// Worker threads, and therefore maximum concurrency.
    pub capacity: usize,
    /// Tasks that may wait for a worker. Beyond this, submission is refused.
    pub queue_limit: usize,
    /// How long a task may run before the supervisor cancels it.
    pub deadline_ms: u64,
    /// How long the supervisor waits for a cancelled worker to stop before
    /// declaring the task abandoned and its slot still held.
    pub join_budget_ms: u64,
    /// How often the supervisor checks deadlines.
    pub tick_ms: u64,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            capacity: 2,
            queue_limit: 8,
            deadline_ms: 20_000,
            join_budget_ms: 2_000,
            tick_ms: 25,
        }
    }
}

/// A live view of what the executor is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutorStats {
    /// Worker threads the executor was built with.
    pub capacity: usize,
    /// Tasks currently inside a provider call.
    pub in_flight: usize,
    /// Tasks accepted and waiting for a worker.
    pub queued: usize,
    /// Tasks declared abandoned whose worker has still not returned.
    ///
    /// Effective capacity is `capacity - stuck`. This is reported rather than
    /// hidden: a caller deciding whether to submit deserves to know the pool
    /// is smaller than it was.
    pub stuck: usize,
    /// Tasks that have settled, for any reason.
    pub settled: u64,
}

/// Why a submission was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitRejection {
    /// The queue was at its bound.
    QueueFull,
    /// The executor is shutting down.
    ShuttingDown,
}

impl std::fmt::Display for SubmitRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::QueueFull => "help answer queue is full",
            Self::ShuttingDown => "help answer executor is shutting down",
        })
    }
}

impl std::error::Error for SubmitRejection {}

/// A submission that was not accepted, and the receipt recording it.
///
/// Boxed at the error position so the success path does not carry the receipt's
/// width on every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRefusal {
    /// Why the submission was not accepted.
    pub rejection: SubmitRejection,
    /// The receipt for the refusal. A refusal is still an audit event.
    pub receipt: ExecutionReceipt,
}

impl std::fmt::Display for SubmitRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.rejection.fmt(formatter)
    }
}

impl std::error::Error for SubmitRefusal {}

/// One submitted request, from admission through to settlement.
struct Task {
    core: AnswerRequestCore,
    admission: AnswerAdmission,
    cancel: CancelToken,
    queued_at: Instant,
    deadline: Instant,
    state: Arc<TaskState>,
}

struct TaskState {
    settled: Mutex<Option<ExecutionReceipt>>,
    done: Condvar,
    /// Set when the supervisor declared this task abandoned *and* counted the
    /// worker slot it is still holding. The worker clears the count when it
    /// finally returns. Distinct from "abandoned" because a task abandoned
    /// while still queued holds no slot to give back.
    holds_slot: AtomicBool,
    /// Set once a worker has picked the task up.
    started: AtomicBool,
}

impl TaskState {
    fn new() -> Self {
        Self {
            settled: Mutex::new(None),
            done: Condvar::new(),
            holds_slot: AtomicBool::new(false),
            started: AtomicBool::new(false),
        }
    }

    /// Record a receipt, once. Later settlements are discarded.
    ///
    /// Settle-once is what makes a late-returning worker harmless: it finds
    /// the task already settled by the supervisor and drops its own result
    /// rather than overwriting an abandonment with a success nobody waited for.
    fn settle(&self, receipt: ExecutionReceipt) -> bool {
        let mut slot = match self.settled.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.is_some() {
            return false;
        }
        *slot = Some(receipt);
        self.done.notify_all();
        true
    }
}

/// A handle to one submitted answer.
pub struct AnswerTask {
    state: Arc<TaskState>,
    cancel: CancelToken,
}

impl AnswerTask {
    /// Request cancellation. The task settles once the worker stops, or as
    /// abandoned once the join budget passes.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// True once the task has settled.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        let slot = match self.state.settled.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.is_some()
    }

    /// Wait for the receipt.
    ///
    /// Always returns: the supervisor settles every task, including one whose
    /// worker never stops.
    #[must_use]
    pub fn join(self) -> ExecutionReceipt {
        let mut slot = match self.state.settled.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        loop {
            if let Some(receipt) = slot.take() {
                return receipt;
            }
            slot = match self.state.done.wait(slot) {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Wait for the receipt, giving up after `budget`.
    ///
    /// # Errors
    /// Returns the handle back when the budget passes, so the caller can wait
    /// again rather than losing the task.
    pub fn join_within(self, budget: Duration) -> Result<ExecutionReceipt, Self> {
        let deadline = Instant::now() + budget;
        let mut slot = match self.state.settled.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        loop {
            if let Some(receipt) = slot.take() {
                return Ok(receipt);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(slot);
                return Err(self);
            }
            let (next, _) = match self.state.done.wait_timeout(slot, remaining) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            slot = next;
        }
    }
}

struct Shared {
    provider: Arc<dyn HelpAnswerProvider>,
    validator: Arc<dyn ReplyValidator>,
    config: ExecutorConfig,
    counts: Mutex<Counts>,
    /// Tasks the supervisor is watching, keyed by a monotonic id.
    watched: Mutex<BTreeMap<u64, Watched>>,
    shutting_down: AtomicBool,
    next_id: AtomicU64,
}

#[derive(Default)]
struct Counts {
    in_flight: usize,
    queued: usize,
    stuck: usize,
    settled: u64,
}

struct Watched {
    state: Arc<TaskState>,
    cancel: CancelToken,
    deadline: Instant,
    /// When the supervisor first asked this task to stop.
    cancel_requested_at: Option<Instant>,
    /// Why it was asked to stop, recorded at the moment it was asked rather
    /// than guessed later from a clock reading.
    cancel_reason: Option<FailureReason>,
    /// Inputs needed to seal a receipt without the worker's cooperation.
    receipt: ReceiptInputs,
    queued_at: Instant,
}

/// A supervised, tool-free, non-persistent answer executor.
pub struct HelpAnswerExecutor {
    shared: Arc<Shared>,
    sender: Mutex<Option<SyncSender<Task>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
    key: Arc<GrantMintingKey>,
}

impl HelpAnswerExecutor {
    /// Build an executor.
    ///
    /// Requires host key material. That is the structural part of "no
    /// caller-injected production transport": the key is what verifies
    /// admissions, so a caller that cannot verify admissions cannot construct
    /// the executor that would call its provider.
    #[must_use]
    pub fn new(
        key: GrantMintingKey,
        provider: Arc<dyn HelpAnswerProvider>,
        validator: Arc<dyn ReplyValidator>,
        config: ExecutorConfig,
    ) -> Self {
        let capacity = config.capacity.max(1);
        let queue_limit = config.queue_limit.max(1);
        let shared = Arc::new(Shared {
            provider,
            validator,
            config: ExecutorConfig {
                capacity,
                queue_limit,
                ..config
            },
            counts: Mutex::new(Counts::default()),
            watched: Mutex::new(BTreeMap::new()),
            shutting_down: AtomicBool::new(false),
            next_id: AtomicU64::new(0),
        });
        let (sender, receiver) = sync_channel::<Task>(queue_limit);
        let receiver = Arc::new(Mutex::new(receiver));

        let mut workers = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            let shared = Arc::clone(&shared);
            let receiver = Arc::clone(&receiver);
            workers.push(std::thread::spawn(move || worker_loop(&shared, &receiver)));
        }

        let supervisor = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || supervisor_loop(&shared))
        };

        Self {
            shared,
            sender: Mutex::new(Some(sender)),
            workers: Mutex::new(workers),
            supervisor: Mutex::new(Some(supervisor)),
            key: Arc::new(key),
        }
    }

    /// A live view of what the executor is doing.
    #[must_use]
    pub fn stats(&self) -> ExecutorStats {
        let counts = lock(&self.shared.counts);
        ExecutorStats {
            capacity: self.shared.config.capacity,
            in_flight: counts.in_flight,
            queued: counts.queued,
            stuck: counts.stuck,
            settled: counts.settled,
        }
    }

    /// Submit one admitted request.
    ///
    /// The admission is verified here, before the queue: work that would be
    /// refused should never occupy a slot. A refusal still produces a receipt,
    /// because "denied" is an outcome an audit needs to see.
    ///
    /// # Errors
    /// Returns a [`SubmitRefusal`] when the queue is at its bound or the
    /// executor is shutting down. Neither case produces a task.
    pub fn submit(
        &self,
        core: AnswerRequestCore,
        admission: AnswerAdmission,
        expectation: &AdmissionExpectation,
    ) -> Result<AnswerTask, Box<SubmitRefusal>> {
        let inputs = ReceiptInputs {
            admission_id: admission.admission_id.clone(),
            request_digest: admission.request_digest.clone(),
            corpus_digest: admission.corpus_digest.clone(),
            index_digest: admission.index_digest.clone(),
            manifest_digest: admission.manifest_digest.clone(),
            grant_revision: admission.grant_revision,
            outcome: ExecutionOutcome::Denied,
            failure: Some(FailureReason::AdmissionRefused),
            outcome_digest: None,
            cited_source_ids: Vec::new(),
            claim_count: 0,
            queued_ms: 0,
            ran_ms: 0,
        };

        if self.shared.shutting_down.load(Ordering::SeqCst) {
            let mut refused = inputs;
            refused.outcome = ExecutionOutcome::Refused;
            refused.failure = Some(FailureReason::ShuttingDown);
            return Err(Box::new(SubmitRefusal {
                rejection: SubmitRejection::ShuttingDown,
                receipt: build_receipt(refused),
            }));
        }

        // The digest the admission claims must be the digest of the body being
        // dispatched. Recomputed here rather than trusted from the caller.
        let mut expectation = expectation.clone();
        expectation.request_digest = request_digest(&core);

        let denied = |failure: FailureReason| -> Arc<TaskState> {
            let mut denial = ReceiptInputs {
                failure: Some(failure),
                ..inputs.clone()
            };
            denial.outcome = ExecutionOutcome::Denied;
            let state = Arc::new(TaskState::new());
            if state.settle(build_receipt(denial)) {
                lock(&self.shared.counts).settled += 1;
            }
            state
        };

        if verify_admission(&self.key, &admission, &expectation).is_err() {
            let state = denied(FailureReason::AdmissionRefused);
            return Ok(AnswerTask {
                state,
                cancel: CancelToken::new(),
            });
        }
        if core.enforce().is_err() {
            let state = denied(FailureReason::RequestRefused);
            return Ok(AnswerTask {
                state,
                cancel: CancelToken::new(),
            });
        }

        let cancel = CancelToken::new();
        let state = Arc::new(TaskState::new());
        let now = Instant::now();
        let task = Task {
            core,
            admission: admission.clone(),
            cancel: cancel.clone(),
            queued_at: now,
            deadline: now + Duration::from_millis(self.shared.config.deadline_ms),
            state: Arc::clone(&state),
        };

        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut watched = lock(&self.shared.watched);
            watched.insert(
                id,
                Watched {
                    state: Arc::clone(&state),
                    cancel: cancel.clone(),
                    deadline: task.deadline,
                    cancel_requested_at: None,
                    cancel_reason: None,
                    receipt: inputs.clone(),
                    queued_at: now,
                },
            );
        }
        {
            let mut counts = lock(&self.shared.counts);
            counts.queued += 1;
        }

        let sender = lock(&self.sender);
        let Some(sender) = sender.as_ref() else {
            self.forget(id);
            let mut refused = inputs;
            refused.outcome = ExecutionOutcome::Refused;
            refused.failure = Some(FailureReason::ShuttingDown);
            return Err(Box::new(SubmitRefusal {
                rejection: SubmitRejection::ShuttingDown,
                receipt: build_receipt(refused),
            }));
        };
        match sender.try_send(task) {
            Ok(()) => Ok(AnswerTask { state, cancel }),
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.forget(id);
                let mut refused = inputs;
                refused.outcome = ExecutionOutcome::Refused;
                refused.failure = Some(FailureReason::QueueFull);
                Err(Box::new(SubmitRefusal {
                    rejection: SubmitRejection::QueueFull,
                    receipt: build_receipt(refused),
                }))
            }
        }
    }

    fn forget(&self, id: u64) {
        lock(&self.shared.watched).remove(&id);
        let mut counts = lock(&self.shared.counts);
        counts.queued = counts.queued.saturating_sub(1);
    }

    /// Stop accepting work and wait for the pool, up to `budget`.
    ///
    /// Returns how many worker threads had not returned when the budget
    /// passed. A non-zero count is reported, not swallowed: those threads are
    /// still inside a provider call.
    pub fn shutdown(&self, budget: Duration) -> usize {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        // Cancel everything still watched, so cooperative providers stop.
        for watched in lock(&self.shared.watched).values() {
            watched.cancel.cancel();
        }
        drop(lock(&self.sender).take());

        let deadline = Instant::now() + budget;
        let mut handles = lock(&self.workers);
        let mut outstanding = 0usize;
        for handle in handles.drain(..) {
            if Instant::now() >= deadline && !handle.is_finished() {
                outstanding += 1;
                continue;
            }
            // A worker inside a provider call cannot be interrupted; joining is
            // the only honest option, and the budget above bounds how long the
            // caller spends on it.
            if handle.join().is_err() {
                outstanding += 1;
            }
        }
        if let Some(handle) = lock(&self.supervisor).take()
            && handle.join().is_err()
        {
            outstanding += 1;
        }
        outstanding
    }
}

impl Drop for HelpAnswerExecutor {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::SeqCst);
        drop(lock(&self.sender).take());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn worker_loop(shared: &Arc<Shared>, receiver: &Arc<Mutex<Receiver<Task>>>) {
    loop {
        let task = {
            let guard = lock(receiver);
            match guard.recv() {
                Ok(task) => task,
                Err(_) => return,
            }
        };

        {
            let mut counts = lock(&shared.counts);
            counts.queued = counts.queued.saturating_sub(1);
            counts.in_flight += 1;
        }
        task.state.started.store(true, Ordering::SeqCst);
        let queued_ms = task.queued_at.elapsed().as_millis() as u64;
        let started = Instant::now();

        // A task cancelled before a worker picked it up must not reach the
        // provider at all.
        let result = if task.cancel.is_cancelled() {
            None
        } else {
            Some(shared.provider.answer(&task.core, &task.cancel))
        };
        let ran_ms = started.elapsed().as_millis() as u64;

        let base = ReceiptInputs {
            admission_id: task.admission.admission_id.clone(),
            request_digest: task.admission.request_digest.clone(),
            corpus_digest: task.admission.corpus_digest.clone(),
            index_digest: task.admission.index_digest.clone(),
            manifest_digest: task.admission.manifest_digest.clone(),
            grant_revision: task.admission.grant_revision,
            outcome: ExecutionOutcome::Cancelled,
            failure: Some(FailureReason::CallerCancelled),
            outcome_digest: None,
            cited_source_ids: Vec::new(),
            claim_count: 0,
            queued_ms,
            ran_ms,
        };

        let receipt = match result {
            None => build_receipt(base),
            Some(Err(ProviderError)) => build_receipt(ReceiptInputs {
                outcome: ExecutionOutcome::ProviderError,
                failure: Some(FailureReason::ProviderFailed),
                ..base
            }),
            Some(Ok(reply)) => build_receipt(settle_reply(shared, &task, reply, base)),
        };

        // Settle-once: if the supervisor already abandoned this task, its
        // receipt stands and the work done here is discarded.
        let recorded = task.state.settle(receipt);
        {
            let mut counts = lock(&shared.counts);
            counts.in_flight = counts.in_flight.saturating_sub(1);
            if recorded {
                counts.settled += 1;
            }
            // The supervisor counted this slot as held while the worker ran
            // past its abandonment. The worker is free now, so give it back.
            if task.state.holds_slot.swap(false, Ordering::SeqCst) {
                counts.stuck = counts.stuck.saturating_sub(1);
            }
        }
    }
}

/// Turn a provider reply into receipt inputs.
fn settle_reply(
    shared: &Arc<Shared>,
    task: &Task,
    reply: AnswerReply,
    base: ReceiptInputs,
) -> ReceiptInputs {
    if reply
        .enforce(&task.core, &task.admission.admission_id)
        .is_err()
    {
        return ReceiptInputs {
            outcome: ExecutionOutcome::Rejected,
            failure: Some(FailureReason::ReplyRefused),
            ..base
        };
    }
    let verdict = shared.validator.verify(&task.core, &reply);
    if !verdict.accepted || citations_overlap(&verdict.citations) {
        return ReceiptInputs {
            outcome: ExecutionOutcome::Rejected,
            failure: Some(FailureReason::ReplyRefused),
            ..base
        };
    }
    ReceiptInputs {
        outcome: ExecutionOutcome::Answered,
        failure: None,
        outcome_digest: Some(bind_outcome(
            &task.admission,
            &reply.answer,
            &reply.uncertainty,
            &verdict.citations,
        )),
        cited_source_ids: verdict.cited_source_ids,
        claim_count: verdict.claim_count,
        ..base
    }
}

/// One task the supervisor has given up waiting for.
struct Abandonment {
    id: u64,
    inputs: ReceiptInputs,
    state: Arc<TaskState>,
    queued_ms: u64,
    reason: FailureReason,
}

fn supervisor_loop(shared: &Arc<Shared>) {
    let tick = Duration::from_millis(shared.config.tick_ms.max(1));
    let join_budget = Duration::from_millis(shared.config.join_budget_ms);
    loop {
        std::thread::sleep(tick);
        let now = Instant::now();
        let mut finished: Vec<u64> = Vec::new();
        let mut abandoned: Vec<Abandonment> = Vec::new();

        {
            let mut watched = lock(&shared.watched);
            for (id, entry) in watched.iter_mut() {
                let settled = {
                    let slot = lock(&entry.state.settled);
                    slot.is_some()
                };
                if settled {
                    finished.push(*id);
                    continue;
                }
                let overdue = now >= entry.deadline;
                let cancelled = entry.cancel.is_cancelled();
                if (overdue || cancelled) && entry.cancel_requested_at.is_none() {
                    // A caller's cancel is recorded as the caller's, even when
                    // the deadline has also passed by the time we look.
                    entry.cancel_reason = Some(if cancelled {
                        FailureReason::CallerCancelled
                    } else {
                        FailureReason::DeadlineExceeded
                    });
                    entry.cancel.cancel();
                    entry.cancel_requested_at = Some(now);
                    continue;
                }
                if let Some(asked) = entry.cancel_requested_at
                    && now.duration_since(asked) >= join_budget
                {
                    let queued_ms = entry.queued_at.elapsed().as_millis() as u64;
                    abandoned.push(Abandonment {
                        id: *id,
                        inputs: entry.receipt.clone(),
                        state: Arc::clone(&entry.state),
                        queued_ms,
                        reason: entry
                            .cancel_reason
                            .unwrap_or(FailureReason::DeadlineExceeded),
                    });
                }
            }
            for id in &finished {
                watched.remove(id);
            }
        }

        for entry in abandoned {
            // Only a task that actually reached a worker is holding a slot. One
            // abandoned while still queued is still in the channel; the worker
            // that eventually dequeues it finds it cancelled, skips the
            // provider, and returns the slot on the ordinary path.
            let holds_slot = entry.state.started.load(Ordering::SeqCst);
            let receipt = build_receipt(ReceiptInputs {
                outcome: ExecutionOutcome::Abandoned,
                failure: Some(entry.reason),
                queued_ms: entry.queued_ms,
                ..entry.inputs
            });
            // Marked before settling: a worker returning between these two
            // steps must never see a settled task whose slot is uncounted.
            if holds_slot {
                entry.state.holds_slot.store(true, Ordering::SeqCst);
            }
            if entry.state.settle(receipt) {
                let mut counts = lock(&shared.counts);
                counts.settled += 1;
                if holds_slot {
                    counts.stuck += 1;
                }
            } else if holds_slot {
                // The worker beat us to it; its own receipt stands and it has
                // already returned, so there is no held slot to count.
                entry.state.holds_slot.store(false, Ordering::SeqCst);
            }
            lock(&shared.watched).remove(&entry.id);
        }

        if shared.shutting_down.load(Ordering::SeqCst) && lock(&shared.watched).is_empty() {
            return;
        }
    }
}
